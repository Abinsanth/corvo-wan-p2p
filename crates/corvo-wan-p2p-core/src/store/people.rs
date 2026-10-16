//! Everyone the app knows of: contacts, requests and blocked people. Each row
//! is found by a keyed tag of their public key. (The table is still called
//! `contacts`, from before requests existed.)

use iroh_base::PublicKey;
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError, crypt, readable};
use crate::identity::Code;

/// Where someone stands with you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// You added them or accepted them: they're dialed and get read receipts.
    Contact,
    /// They messaged you first and you haven't answered yet.
    Request,
    /// You blocked them: their connections are closed before hello.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub code: Code,
    /// Their display name; empty until their app has told us.
    pub name: String,
    /// When the connection to them last ended, in Unix milliseconds. Only kept for contacts.
    pub last_seen: Option<i64>,
    pub standing: Standing,
}

/// What `contacts.standing` holds, sealed. A request also keeps the newest
/// message `seq` from before it began, so its own messages can be counted
/// even when older history with them is still kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    Contact,
    Request { since: i64 },
    Blocked,
}

impl Stored {
    fn bytes(self) -> Vec<u8> {
        match self {
            Stored::Contact => vec![0],
            Stored::Request { since } => [&[1u8][..], &since.to_be_bytes()].concat(),
            Stored::Blocked => vec![2],
        }
    }

    fn from_bytes(b: &[u8]) -> Option<Stored> {
        match b {
            [0] => Some(Stored::Contact),
            [1, since @ ..] => Some(Stored::Request { since: i64::from_be_bytes(since.try_into().ok()?) }),
            [2] => Some(Stored::Blocked),
            _ => None,
        }
    }

    fn standing(self) -> Standing {
        match self {
            Stored::Contact => Standing::Contact,
            Stored::Request { .. } => Standing::Request,
            Stored::Blocked => Standing::Blocked,
        }
    }
}

const COLUMNS: &str = "peer, key, name, last_seen, standing";

type Row = (Vec<u8>, Vec<u8>, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

impl Store {
    pub(super) fn peer_tag(&self, code: &Code) -> [u8; 32] {
        crypt::tag(&self.tag_key, b"peer", code.key().as_bytes())
    }

    /// Adds someone as a contact. `false` if you already knew of them (in any standing).
    pub fn add_contact(&self, code: &Code) -> Result<bool, StoreError> {
        self.insert_person(code, "", Stored::Contact)
    }

    /// Saves someone who messaged you first, with the name their app sent
    /// (may be empty). `false` if you already knew of them.
    pub fn add_request(&self, code: &Code, name: &str) -> Result<bool, StoreError> {
        let since = self.last_seq()?;
        self.insert_person(code, name, Stored::Request { since })
    }

    fn insert_person(&self, code: &Code, name: &str, standing: Stored) -> Result<bool, StoreError> {
        let peer = self.peer_tag(code);
        let added = self.db.execute(
            "INSERT OR IGNORE INTO contacts (peer, key, name, standing) VALUES (?1, ?2, ?3, ?4)",
            params![
                peer,
                self.seal_at(b"contacts.key", &[&peer], code.key().as_bytes()),
                self.seal_at(b"contacts.name", &[&peer], name.as_bytes()),
                self.seal_standing(&peer, standing),
            ],
        )?;
        Ok(added == 1)
    }

    fn seal_standing(&self, peer: &[u8], standing: Stored) -> Vec<u8> {
        self.seal_at(b"contacts.standing", &[peer], &standing.bytes())
    }

    fn set_stored(&self, peer: &[u8], standing: Stored) -> Result<(), StoreError> {
        self.db.execute(
            "UPDATE contacts SET standing = ?1 WHERE peer = ?2",
            params![self.seal_standing(peer, standing), peer],
        )?;
        Ok(())
    }

    fn stored(&self, peer: &[u8]) -> Result<Option<Stored>, StoreError> {
        let sealed: Option<Option<Vec<u8>>> =
            self.db.query_row("SELECT standing FROM contacts WHERE peer = ?1", [peer], |r| r.get(0)).optional()?;
        sealed.map(|sealed| self.open_standing(peer, sealed.as_deref())).transpose()
    }

    /// Schema step 4: rows from before requests have no standing; they're contacts.
    pub(super) fn seal_missing_standings(&self) -> Result<(), StoreError> {
        let mut query = self.db.prepare("SELECT peer FROM contacts WHERE standing IS NULL")?;
        let peers: Vec<Vec<u8>> = query.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for peer in peers {
            self.set_stored(&peer, Stored::Contact)?;
        }
        Ok(())
    }

    fn open_standing(&self, peer: &[u8], sealed: Option<&[u8]>) -> Result<Stored, StoreError> {
        // Schema step 4 sealed every standing, so a missing one was removed since.
        let sealed = sealed.ok_or_else(|| self.damaged("a standing is missing"))?;
        let bytes = self.open_at(b"contacts.standing", &[peer], sealed)?;
        Stored::from_bytes(&bytes).ok_or_else(|| self.damaged("unknown standing"))
    }

    /// Where `code` stands with you, or `None` if you don't know them.
    pub fn standing(&self, code: &Code) -> Result<Option<Standing>, StoreError> {
        Ok(self.stored(&self.peer_tag(code))?.map(Stored::standing))
    }

    pub fn is_contact(&self, code: &Code) -> Result<bool, StoreError> {
        Ok(self.standing(code)? == Some(Standing::Contact))
    }

    /// Makes a request a contact. `false` if they hadn't sent a request.
    pub fn accept(&self, code: &Code) -> Result<bool, StoreError> {
        let peer = self.peer_tag(code);
        if !matches!(self.stored(&peer)?, Some(Stored::Request { .. })) {
            return Ok(false);
        }
        self.set_stored(&peer, Stored::Contact)?;
        Ok(true)
    }

    /// Blocks anyone, known or not, and drops what you had queued to them.
    /// Their history and name are kept.
    pub fn block(&self, code: &Code) -> Result<(), StoreError> {
        let tx = self.db.unchecked_transaction()?;
        if !self.insert_person(code, "", Stored::Blocked)? {
            self.set_stored(&self.peer_tag(code), Stored::Blocked)?;
        }
        self.delete_waiting(code)?;
        tx.commit()?;
        Ok(())
    }

    /// Forgets that you blocked them: they're a stranger again, and come back
    /// as a request if they message you. `false` if they weren't blocked.
    pub fn unblock(&self, code: &Code) -> Result<bool, StoreError> {
        let peer = self.peer_tag(code);
        if self.stored(&peer)? != Some(Stored::Blocked) {
            return Ok(false);
        }
        self.db.execute("DELETE FROM contacts WHERE peer = ?1", [peer])?;
        Ok(true)
    }

    /// Forgets a contact or a request, and drops what you had queued to them.
    /// With `history`, deletes your messages with them too. `false` if they
    /// weren't a contact or a request (blocked people stay blocked).
    pub fn remove(&self, code: &Code, history: bool) -> Result<bool, StoreError> {
        let peer = self.peer_tag(code);
        if !matches!(self.stored(&peer)?, Some(Stored::Contact | Stored::Request { .. })) {
            return Ok(false);
        }
        let tx = self.db.unchecked_transaction()?;
        self.db.execute("DELETE FROM contacts WHERE peer = ?1", [peer])?;
        if history {
            self.delete_history(code)?;
        } else {
            self.delete_waiting(code)?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// How many messages a request has sent since it began (0 if they aren't one).
    pub fn request_messages(&self, code: &Code) -> Result<usize, StoreError> {
        let peer = self.peer_tag(code);
        let Some(Stored::Request { since }) = self.stored(&peer)? else { return Ok(0) };
        let n: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM messages WHERE peer = ?1 AND seq > ?2",
            params![peer, since],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Everyone you know of who can be read, in the order you first knew of them.
    pub fn people(&self) -> Result<Vec<Person>, StoreError> {
        Ok(self.people_checked()?.0)
    }

    /// [`Store::people`], and how many people couldn't be read (damaged).
    pub fn people_checked(&self) -> Result<(Vec<Person>, usize), StoreError> {
        let mut query = self.db.prepare(&format!("SELECT {COLUMNS} FROM contacts ORDER BY rowid"))?;
        let rows = query.query_map([], row)?;
        let (mut people, mut damaged) = (Vec::new(), 0);
        for r in rows {
            match readable(self.person_from_row(r?))? {
                Some(person) => people.push(person),
                None => damaged += 1,
            }
        }
        Ok((people, damaged))
    }

    /// Your contacts, in the order you added them.
    pub fn contacts(&self) -> Result<Vec<Person>, StoreError> {
        Ok(self.people()?.into_iter().filter(|p| p.standing == Standing::Contact).collect())
    }

    pub fn person(&self, code: &Code) -> Result<Option<Person>, StoreError> {
        let found = self
            .db
            .query_row(&format!("SELECT {COLUMNS} FROM contacts WHERE peer = ?1"), [self.peer_tag(code)], row)
            .optional()?;
        found.map(|r| self.person_from_row(r)).transpose()
    }

    fn person_from_row(&self, (peer, key, name, last_seen, standing): Row) -> Result<Person, StoreError> {
        let key = self.open_at(b"contacts.key", &[&peer], &key)?;
        let key: [u8; 32] = key.as_slice().try_into().map_err(|_| self.damaged("a contact key is the wrong size"))?;
        let key = PublicKey::from_bytes(&key).map_err(|_| self.damaged("a contact key isn't valid"))?;
        let name = String::from_utf8(self.open_at(b"contacts.name", &[&peer], &name)?.to_vec())
            .map_err(|_| self.damaged("a contact name isn't text"))?;
        let last_seen = match last_seen {
            Some(sealed) => Some(self.open_i64(b"contacts.last_seen", &[&peer], &sealed)?),
            None => None,
        };
        let standing = self.open_standing(&peer, standing.as_deref())?.standing();
        Ok(Person { code: Code::from_key(key), name, last_seen, standing })
    }

    /// Saves the name their app sent.
    pub fn set_contact_name(&self, code: &Code, name: &str) -> Result<(), StoreError> {
        let peer = self.peer_tag(code);
        self.db.execute(
            "UPDATE contacts SET name = ?1 WHERE peer = ?2",
            params![self.seal_at(b"contacts.name", &[&peer], name.as_bytes()), peer],
        )?;
        Ok(())
    }

    pub fn set_last_seen(&self, code: &Code, at: i64) -> Result<(), StoreError> {
        let peer = self.peer_tag(code);
        self.db.execute(
            "UPDATE contacts SET last_seen = ?1 WHERE peer = ?2",
            params![self.seal_at(b"contacts.last_seen", &[&peer], &at.to_be_bytes()), peer],
        )?;
        Ok(())
    }

    /// Remembers that you've read their messages up to `up_to`. The app sends
    /// this read receipt now if connected, and again on every new connection,
    /// so it can't get lost.
    pub fn note_read(&self, code: &Code, up_to: &str) -> Result<(), StoreError> {
        let peer = self.peer_tag(code);
        self.db.execute(
            "UPDATE contacts SET read_upto = ?1 WHERE peer = ?2",
            params![self.seal_at(b"contacts.read_upto", &[&peer], up_to.as_bytes()), peer],
        )?;
        Ok(())
    }

    /// The newest message of theirs you've read, if any.
    pub fn read_upto(&self, code: &Code) -> Result<Option<String>, StoreError> {
        let peer = self.peer_tag(code);
        let sealed: Option<Option<Vec<u8>>> =
            self.db.query_row("SELECT read_upto FROM contacts WHERE peer = ?1", [peer], |r| r.get(0)).optional()?;
        match sealed.flatten() {
            Some(sealed) => {
                let id = self.open_at(b"contacts.read_upto", &[&peer], &sealed)?;
                Ok(Some(String::from_utf8(id.to_vec()).map_err(|_| self.damaged("a read receipt isn't text"))?))
            }
            None => Ok(None),
        }
    }

    pub(super) fn open_i64(&self, column: &[u8], row: &[&[u8]], sealed: &[u8]) -> Result<i64, StoreError> {
        let bytes = self.open_at(column, row, sealed)?;
        let bytes: [u8; 8] = bytes.as_slice().try_into().map_err(|_| self.damaged("a number is the wrong size"))?;
        Ok(i64::from_be_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standings_round_trip_and_junk_is_refused() {
        for s in [Stored::Contact, Stored::Request { since: 0 }, Stored::Request { since: 1 << 40 }, Stored::Blocked] {
            assert_eq!(Stored::from_bytes(&s.bytes()), Some(s));
        }
        assert_eq!(Stored::from_bytes(&[]), None);
        assert_eq!(Stored::from_bytes(&[1, 2, 3]), None);
        assert_eq!(Stored::from_bytes(&[9]), None);
    }
}
