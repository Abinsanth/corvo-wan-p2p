//! Messages, in the order they were saved. Each row is found by keyed tags of
//! the peer and the message id; its text and state are sealed.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{Store, StoreError, crypt, readable};
use crate::identity::Code;

/// Where a message stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Yours, not yet received by them (`·`).
    Waiting,
    /// Yours, saved by their app (`✓`).
    Delivered,
    /// Yours, read by them (`✓✓`).
    Read,
    /// Theirs.
    Received,
}

impl Mark {
    pub fn is_outgoing(self) -> bool {
        self != Mark::Received
    }

    fn byte(self) -> u8 {
        match self {
            Mark::Waiting => 0,
            Mark::Delivered => 1,
            Mark::Read => 2,
            Mark::Received => 3,
        }
    }

    fn from_byte(b: u8) -> Option<Mark> {
        Some(match b {
            0 => Mark::Waiting,
            1 => Mark::Delivered,
            2 => Mark::Read,
            3 => Mark::Received,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: String,
    /// Unix milliseconds, by the sender's clock.
    pub sent_at: i64,
    pub text: String,
    pub mark: Mark,
}

/// What `messages.body` holds, sealed.
#[derive(Serialize, Deserialize)]
struct Body {
    id: String,
    sent_at: i64,
    text: String,
}

#[cfg(test)]
thread_local! {
    /// How many marks and bodies were opened, so tests can see what a call reads.
    static OPENED: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

impl Store {
    fn msg_tag(&self, id: &str) -> [u8; 32] {
        crypt::tag(&self.tag_key, b"msg", id.as_bytes())
    }

    /// Saves a message you're sending, as waiting.
    pub fn add_outgoing(&self, to: &Code, id: &str, sent_at: i64, text: &str) -> Result<(), StoreError> {
        self.insert(to, id, sent_at, text, Mark::Waiting)?;
        Ok(())
    }

    /// Saves a message you've received. `false` if it was already saved (it was
    /// sent again because our ack got lost).
    pub fn add_incoming(&self, from: &Code, id: &str, sent_at: i64, text: &str) -> Result<bool, StoreError> {
        self.insert(from, id, sent_at, text, Mark::Received)
    }

    fn insert(&self, peer: &Code, id: &str, sent_at: i64, text: &str, mark: Mark) -> Result<bool, StoreError> {
        let peer = self.peer_tag(peer);
        let msg = self.msg_tag(id);
        let body = serde_json::to_vec(&Body { id: id.into(), sent_at, text: text.into() }).expect("a Body serializes");
        let added = self.db.execute(
            "INSERT OR IGNORE INTO messages (peer, msg, body, mark, waiting) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                peer,
                msg,
                self.seal_at(b"messages.body", &[&peer, &msg], &body),
                self.seal_at(b"messages.mark", &[&peer, &msg], &[mark.byte()]),
                mark == Mark::Waiting
            ],
        )?;
        Ok(added == 1)
    }

    /// Your messages to `to` that their app hasn't confirmed yet, oldest first.
    pub fn waiting(&self, to: &Code) -> Result<Vec<StoredMessage>, StoreError> {
        Ok(self.select(to, "AND waiting = 1", "")?.0)
    }

    /// The oldest `n` of [`Store::waiting`], without reading the rest. A
    /// waiting message that can't be read is taken out of the outbox, so it
    /// doesn't hold up the ones after it.
    pub fn oldest_waiting(&self, to: &Code, n: usize) -> Result<Vec<StoredMessage>, StoreError> {
        let (waiting, damaged) = self.select(to, "AND waiting = 1", &format!("LIMIT {n}"))?;
        if damaged.is_empty() {
            return Ok(waiting);
        }
        let peer = self.peer_tag(to);
        for msg in damaged {
            self.db.execute("UPDATE messages SET waiting = 0 WHERE peer = ?1 AND msg = ?2", params![peer, msg])?;
        }
        self.oldest_waiting(to, n)
    }

    /// Everything between you and `peer` that can be read, oldest first.
    pub fn history(&self, peer: &Code) -> Result<Vec<StoredMessage>, StoreError> {
        Ok(self.history_checked(peer)?.0)
    }

    /// [`Store::history`], and how many messages couldn't be read (damaged).
    pub fn history_checked(&self, peer: &Code) -> Result<(Vec<StoredMessage>, usize), StoreError> {
        let (messages, damaged) = self.select(peer, "", "")?;
        Ok((messages, damaged.len()))
    }

    /// The newest `n` messages with `peer` from before the one with id
    /// `before` (or the newest of all with `None`), oldest first, and how many
    /// of them couldn't be read. Only those `n` are opened. An id that isn't
    /// saved gives nothing.
    pub fn history_before(
        &self,
        peer: &Code,
        before: Option<&str>,
        n: usize,
    ) -> Result<(Vec<StoredMessage>, usize), StoreError> {
        let below: i64 = match before {
            None => i64::MAX,
            Some(id) => {
                let seq = self
                    .db
                    .query_row(
                        "SELECT seq FROM messages WHERE peer = ?1 AND msg = ?2",
                        params![self.peer_tag(peer), self.msg_tag(id)],
                        |r| r.get(0),
                    )
                    .optional()?;
                match seq {
                    Some(seq) => seq,
                    None => return Ok((Vec::new(), 0)),
                }
            }
        };
        let (mut messages, damaged) = self.select(peer, &format!("AND seq < {below}"), &format!("DESC LIMIT {n}"))?;
        messages.reverse();
        Ok((messages, damaged.len()))
    }

    /// Whether a message with this id from or to `peer` is saved, or was
    /// until its history was deleted.
    pub fn has_message(&self, peer: &Code, id: &str) -> Result<bool, StoreError> {
        let found = self
            .db
            .query_row(
                "SELECT 1 FROM messages WHERE peer = ?1 AND msg = ?2
                 UNION ALL SELECT 1 FROM forgotten WHERE peer = ?1 AND msg = ?2 LIMIT 1",
                params![self.peer_tag(peer), self.msg_tag(id)],
                |_| Ok(()),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// The `seq` of the newest message saved with anyone (0 if none).
    pub(super) fn last_seq(&self) -> Result<i64, StoreError> {
        Ok(self.db.query_row("SELECT COALESCE(MAX(seq), 0) FROM messages", [], |r| r.get(0))?)
    }

    /// Drops your messages to `to` that haven't gone out yet.
    pub(super) fn delete_waiting(&self, to: &Code) -> Result<(), StoreError> {
        self.db.execute("DELETE FROM messages WHERE peer = ?1 AND waiting = 1", [self.peer_tag(to)])?;
        Ok(())
    }

    /// Deletes every message between you and `peer`, keeping only their ids'
    /// tags (see [`Store::has_message`]). Call it inside a transaction.
    pub(super) fn delete_history(&self, peer: &Code) -> Result<(), StoreError> {
        let peer = self.peer_tag(peer);
        self.db.execute(
            "INSERT OR IGNORE INTO forgotten (peer, msg) SELECT peer, msg FROM messages WHERE peer = ?1",
            [peer],
        )?;
        self.db.execute("DELETE FROM messages WHERE peer = ?1", [peer])?;
        Ok(())
    }

    /// The messages that open, and the `msg` tags of those that don't.
    /// `order` follows `ORDER BY seq`: a direction and a limit, or nothing.
    fn select(&self, peer: &Code, filter: &str, order: &str) -> Result<(Vec<StoredMessage>, Vec<Vec<u8>>), StoreError> {
        let peer = self.peer_tag(peer);
        let sql = format!("SELECT msg, body, mark FROM messages WHERE peer = ?1 {filter} ORDER BY seq {order}");
        let mut query = self.db.prepare(&sql)?;
        let rows = query
            .query_map([peer], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, Vec<u8>>(2)?)))?;
        let (mut out, mut damaged) = (Vec::new(), Vec::new());
        for row in rows {
            let (msg, body, mark) = row?;
            match readable(self.message_from_row(&peer, &msg, &body, &mark))? {
                Some(message) => out.push(message),
                None => damaged.push(msg),
            }
        }
        Ok((out, damaged))
    }

    fn message_from_row(&self, peer: &[u8], msg: &[u8], body: &[u8], mark: &[u8]) -> Result<StoredMessage, StoreError> {
        let body = self.open_body(peer, msg, body)?;
        let mark = self.open_mark(peer, msg, mark)?;
        Ok(StoredMessage { id: body.id, sent_at: body.sent_at, text: body.text, mark })
    }

    fn open_body(&self, peer: &[u8], msg: &[u8], sealed: &[u8]) -> Result<Body, StoreError> {
        #[cfg(test)]
        OPENED.with(|o| o.set((o.get().0, o.get().1 + 1)));
        let body = self.open_at(b"messages.body", &[peer, msg], sealed)?;
        serde_json::from_slice(&body).map_err(|_| self.damaged("a message isn't readable"))
    }

    fn open_mark(&self, peer: &[u8], msg: &[u8], sealed: &[u8]) -> Result<Mark, StoreError> {
        #[cfg(test)]
        OPENED.with(|o| o.set((o.get().0 + 1, o.get().1)));
        let byte = self.open_at(b"messages.mark", &[peer, msg], sealed)?;
        match byte.as_slice() {
            [b] => Mark::from_byte(*b).ok_or_else(|| self.damaged("unknown message mark")),
            _ => Err(self.damaged("a message mark is the wrong size")),
        }
    }

    fn set_mark(&self, peer: &[u8], msg: &[u8], mark: Mark) -> Result<(), StoreError> {
        self.db.execute(
            "UPDATE messages SET mark = ?1, waiting = 0 WHERE peer = ?2 AND msg = ?3",
            params![self.seal_at(b"messages.mark", &[peer, msg], &[mark.byte()]), peer, msg],
        )?;
        Ok(())
    }

    /// Their app saved your message `id`. `false` if there's no such waiting
    /// message (an unknown id, or already confirmed).
    pub fn mark_delivered(&self, to: &Code, id: &str) -> Result<bool, StoreError> {
        let peer = self.peer_tag(to);
        let msg = self.msg_tag(id);
        let sealed: Option<Vec<u8>> = self
            .db
            .query_row("SELECT mark FROM messages WHERE peer = ?1 AND msg = ?2", params![peer, msg], |r| r.get(0))
            .optional()?;
        match sealed {
            Some(sealed) if self.open_mark(&peer, &msg, &sealed)? == Mark::Waiting => {
                self.set_mark(&peer, &msg, Mark::Delivered)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// They've read your messages up to and including `up_to`. `false` if that
    /// isn't one of your messages or nothing changed.
    pub fn mark_read(&self, to: &Code, up_to: &str) -> Result<bool, StoreError> {
        let peer = self.peer_tag(to);
        let target: Option<i64> = self
            .db
            .query_row(
                "SELECT seq FROM messages WHERE peer = ?1 AND msg = ?2",
                params![peer, self.msg_tag(up_to)],
                |r| r.get(0),
            )
            .optional()?;
        let Some(target) = target else { return Ok(false) };
        // Marks only move forward, so everything before a read message is read
        // too: walk back from `up_to` and stop at the first one.
        let mut newly_read = Vec::new();
        {
            let mut query =
                self.db.prepare("SELECT msg, mark FROM messages WHERE peer = ?1 AND seq <= ?2 ORDER BY seq DESC")?;
            let mut rows = query.query(params![peer, target])?;
            while let Some(row) = rows.next()? {
                let (msg, mark): (Vec<u8>, Vec<u8>) = (row.get(0)?, row.get(1)?);
                match readable(self.open_mark(&peer, &msg, &mark))? {
                    Some(Mark::Read) => break,
                    Some(Mark::Waiting | Mark::Delivered) => newly_read.push(msg),
                    Some(Mark::Received) | None => {}
                }
            }
        }
        let tx = self.db.unchecked_transaction()?;
        for msg in &newly_read {
            self.set_mark(&peer, msg, Mark::Read)?;
        }
        tx.commit()?;
        Ok(!newly_read.is_empty())
    }

    /// The id of the newest message `from` sent you, found from the end of the chat.
    pub fn last_received(&self, from: &Code) -> Result<Option<String>, StoreError> {
        let peer = self.peer_tag(from);
        let mut query = self.db.prepare("SELECT msg, body, mark FROM messages WHERE peer = ?1 ORDER BY seq DESC")?;
        let mut rows = query.query([&peer])?;
        while let Some(row) = rows.next()? {
            let (msg, body, mark): (Vec<u8>, Vec<u8>, Vec<u8>) = (row.get(0)?, row.get(1)?, row.get(2)?);
            if readable(self.open_mark(&peer, &msg, &mark))? == Some(Mark::Received)
                && let Some(body) = readable(self.open_body(&peer, &msg, &body))?
            {
                return Ok(Some(body.id));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Seed;
    use crate::store::KdfParams;

    fn store(dir: &std::path::Path) -> Store {
        Store::create(dir, "pass", KdfParams::FAST_INSECURE, &Seed::generate(), "abin").unwrap()
    }

    fn opened() -> (usize, usize) {
        OPENED.with(|o| o.get())
    }

    #[test]
    fn a_read_receipt_only_opens_the_marks_not_yet_read() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let rahul = Seed::generate().code();
        for i in 0..100 {
            s.add_outgoing(&rahul, &format!("m{i:03}"), i, "x").unwrap();
            s.mark_delivered(&rahul, &format!("m{i:03}")).unwrap();
        }
        assert!(s.mark_read(&rahul, "m097").unwrap());
        OPENED.with(|o| o.set((0, 0)));
        assert!(s.mark_read(&rahul, "m099").unwrap());
        assert!(opened().0 <= 3, "opened {} marks", opened().0);
        let marks: Vec<Mark> = s.history(&rahul).unwrap().iter().map(|m| m.mark).collect();
        assert!(marks.iter().all(|&m| m == Mark::Read));
        assert!(!s.mark_read(&rahul, "m099").unwrap(), "nothing new");
    }

    #[test]
    fn history_before_pages_back_from_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let (rahul, maria) = (Seed::generate().code(), Seed::generate().code());
        for i in 0..25 {
            s.add_incoming(&rahul, &format!("m{i:02}"), i, "x").unwrap();
            s.add_outgoing(&maria, &format!("other{i:02}"), i, "x").unwrap();
        }
        let ids = |page: &(Vec<StoredMessage>, usize)| page.0.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
        let newest = s.history_before(&rahul, None, 10).unwrap();
        assert_eq!(ids(&newest), (15..25).map(|i| format!("m{i:02}")).collect::<Vec<_>>(), "the newest, oldest first");
        assert_eq!(newest.1, 0);
        OPENED.with(|o| o.set((0, 0)));
        let older = s.history_before(&rahul, Some("m15"), 10).unwrap();
        assert_eq!(ids(&older), (5..15).map(|i| format!("m{i:02}")).collect::<Vec<_>>());
        assert!(opened().1 <= 10, "opened {} bodies", opened().1);
        let oldest = s.history_before(&rahul, Some("m05"), 10).unwrap();
        assert_eq!(ids(&oldest), (0..5).map(|i| format!("m{i:02}")).collect::<Vec<_>>(), "fewer at the start");
        assert!(s.history_before(&rahul, Some("m00"), 10).unwrap().0.is_empty());
        assert!(s.history_before(&rahul, Some("not-here"), 10).unwrap().0.is_empty());
    }

    #[test]
    fn the_last_received_message_is_found_from_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let rahul = Seed::generate().code();
        for i in 0..100 {
            s.add_incoming(&rahul, &format!("in{i:03}"), i, "x").unwrap();
        }
        s.add_outgoing(&rahul, "out", 100, "x").unwrap();
        OPENED.with(|o| o.set((0, 0)));
        assert_eq!(s.last_received(&rahul).unwrap().as_deref(), Some("in099"));
        assert!(opened().0 <= 2 && opened().1 <= 1, "opened {:?} (marks, bodies)", opened());
        assert_eq!(s.last_received(&Seed::generate().code()).unwrap(), None);
    }
}
