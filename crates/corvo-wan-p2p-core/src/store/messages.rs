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
    fn select(&self, peer: &Code, filter: &str, limit: &str) -> Result<(Vec<StoredMessage>, Vec<Vec<u8>>), StoreError> {
        let peer = self.peer_tag(peer);
        let sql = format!("SELECT msg, body, mark FROM messages WHERE peer = ?1 {filter} ORDER BY seq {limit}");
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
        let body = self.open_at(b"messages.body", &[peer, msg], body)?;
        let body: Body = serde_json::from_slice(&body).map_err(|_| self.damaged("a message isn't readable"))?;
        let mark = self.open_mark(peer, msg, mark)?;
        Ok(StoredMessage { id: body.id, sent_at: body.sent_at, text: body.text, mark })
    }

    fn open_mark(&self, peer: &[u8], msg: &[u8], sealed: &[u8]) -> Result<Mark, StoreError> {
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
        let mut query = self.db.prepare("SELECT msg, mark FROM messages WHERE peer = ?1 AND seq <= ?2 ORDER BY seq")?;
        let rows: Vec<(Vec<u8>, Vec<u8>)> =
            query.query_map(params![peer, target], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        let mut changed = false;
        for (msg, mark) in rows {
            if matches!(readable(self.open_mark(&peer, &msg, &mark))?, Some(Mark::Waiting | Mark::Delivered)) {
                self.set_mark(&peer, &msg, Mark::Read)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// The id of the newest message `from` sent you.
    pub fn last_received(&self, from: &Code) -> Result<Option<String>, StoreError> {
        Ok(self.history(from)?.into_iter().rev().find(|m| m.mark == Mark::Received).map(|m| m.id))
    }
}
