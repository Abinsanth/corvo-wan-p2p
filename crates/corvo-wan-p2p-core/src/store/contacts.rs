//! People you've added. Each row is found by a keyed tag of their public key.

use iroh_base::PublicKey;
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError, crypt};
use crate::identity::Code;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub code: Code,
    /// Their display name; empty until their app has told us.
    pub name: String,
    /// When the connection to them last ended, in Unix milliseconds.
    pub last_seen: Option<i64>,
}

impl Store {
    pub(super) fn peer_tag(&self, code: &Code) -> [u8; 32] {
        crypt::tag(&self.tag_key, b"peer", code.key().as_bytes())
    }

    /// Adds someone. `false` if they were already a contact.
    pub fn add_contact(&self, code: &Code) -> Result<bool, StoreError> {
        let peer = self.peer_tag(code);
        let added = self.db.execute(
            "INSERT OR IGNORE INTO contacts (peer, key, name) VALUES (?1, ?2, ?3)",
            params![
                peer,
                self.seal_at(b"contacts.key", &[&peer], code.key().as_bytes()),
                self.seal_at(b"contacts.name", &[&peer], b"")
            ],
        )?;
        Ok(added == 1)
    }

    pub fn is_contact(&self, code: &Code) -> Result<bool, StoreError> {
        let found = self
            .db
            .query_row("SELECT 1 FROM contacts WHERE peer = ?1", [self.peer_tag(code)], |_| Ok(()))
            .optional()?;
        Ok(found.is_some())
    }

    /// Everyone you've added, in the order you added them.
    pub fn contacts(&self) -> Result<Vec<Contact>, StoreError> {
        let mut query = self.db.prepare("SELECT peer, key, name, last_seen FROM contacts ORDER BY rowid")?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
            ))
        })?;
        let mut contacts = Vec::new();
        for row in rows {
            let (peer, key, name, last_seen) = row?;
            contacts.push(self.contact_from_row(&peer, &key, &name, last_seen.as_deref())?);
        }
        Ok(contacts)
    }

    pub fn contact(&self, code: &Code) -> Result<Option<Contact>, StoreError> {
        Ok(self.contacts()?.into_iter().find(|c| c.code == *code))
    }

    fn contact_from_row(
        &self,
        peer: &[u8],
        key: &[u8],
        name: &[u8],
        last_seen: Option<&[u8]>,
    ) -> Result<Contact, StoreError> {
        let key = self.open_at(b"contacts.key", &[peer], key)?;
        let key: [u8; 32] = key.as_slice().try_into().map_err(|_| self.damaged("a contact key is the wrong size"))?;
        let key = PublicKey::from_bytes(&key).map_err(|_| self.damaged("a contact key isn't valid"))?;
        let name = String::from_utf8(self.open_at(b"contacts.name", &[peer], name)?.to_vec())
            .map_err(|_| self.damaged("a contact name isn't text"))?;
        let last_seen = match last_seen {
            Some(sealed) => Some(self.open_i64(b"contacts.last_seen", &[peer], sealed)?),
            None => None,
        };
        Ok(Contact { code: Code::from_key(key), name, last_seen })
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
