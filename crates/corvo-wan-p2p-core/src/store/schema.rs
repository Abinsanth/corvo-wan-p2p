//! The store's tables, created and upgraded by numbered steps. The number of
//! steps done is kept in SQLite's `user_version`.

use rusqlite::Connection;

/// One entry per schema version, applied in order. Never edit a released step:
/// add a new one.
const STEPS: &[&str] = &[
    // 1: who you are. One row; both columns are sealed (see `crypt::seal`).
    "CREATE TABLE profile (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        seed BLOB NOT NULL,
        name BLOB NOT NULL
    );",
    // 2: contacts and messages. Rows are found by keyed tags (see `crypt::tag`)
    // of the peer's key and the message id; every value is sealed and bound to
    // its row. `waiting` (an unacked outgoing message) stays plain so the
    // outbox can be found without decrypting everything.
    "CREATE TABLE contacts (
        peer      BLOB PRIMARY KEY,
        key       BLOB NOT NULL,
        name      BLOB NOT NULL,
        last_seen BLOB,
        read_upto BLOB
    );
    CREATE TABLE messages (
        seq     INTEGER PRIMARY KEY AUTOINCREMENT,
        peer    BLOB NOT NULL,
        msg     BLOB NOT NULL,
        body    BLOB NOT NULL,
        mark    BLOB NOT NULL,
        waiting INTEGER NOT NULL,
        UNIQUE (peer, msg)
    );
    CREATE INDEX messages_waiting ON messages (peer, waiting);",
    // 3: requests and blocking. The contacts table now holds everyone you know
    // of; `standing` (sealed) says whether each is a contact, a request or
    // blocked. Rows from before this step have none: they're contacts.
    "ALTER TABLE contacts ADD COLUMN standing BLOB;",
];

/// What [`upgrade`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Upgrade {
    Done,
    /// A newer version of the app wrote this file; leave it alone.
    TooNew,
}

/// Brings `db` up to the newest schema.
pub(crate) fn upgrade(db: &mut Connection) -> rusqlite::Result<Upgrade> {
    let current: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let target = STEPS.len() as i64;
    if current > target {
        return Ok(Upgrade::TooNew);
    }
    for (i, step) in STEPS.iter().enumerate().skip(current as usize) {
        let tx = db.transaction()?;
        tx.execute_batch(step)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    Ok(Upgrade::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(db: &Connection) -> i64 {
        db.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap()
    }

    #[test]
    fn upgrade_creates_the_tables_once() {
        let mut db = Connection::open_in_memory().unwrap();
        assert_eq!(upgrade(&mut db).unwrap(), Upgrade::Done);
        assert_eq!(version(&db), STEPS.len() as i64);
        assert_eq!(upgrade(&mut db).unwrap(), Upgrade::Done);
        db.execute("INSERT INTO profile (id, seed, name) VALUES (1, x'00', x'00')", []).unwrap();
        db.execute("INSERT INTO contacts (peer, key, name, standing) VALUES (x'01', x'00', x'00', x'00')", []).unwrap();
        db.execute("INSERT INTO messages (peer, msg, body, mark, waiting) VALUES (x'01', x'02', x'00', x'00', 1)", [])
            .unwrap();
    }

    #[test]
    fn a_newer_schema_is_left_alone() {
        let mut db = Connection::open_in_memory().unwrap();
        db.pragma_update(None, "user_version", 99).unwrap();
        assert_eq!(upgrade(&mut db).unwrap(), Upgrade::TooNew);
        assert_eq!(version(&db), 99);
    }
}
