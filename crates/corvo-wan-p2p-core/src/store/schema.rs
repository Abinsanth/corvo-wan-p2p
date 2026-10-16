//! The store's tables, created and upgraded by numbered steps. The number of
//! steps done is kept in SQLite's `user_version`.

use rusqlite::Connection;

use super::StoreError;

/// One schema version: SQL, or work on sealed values that needs the store's keys.
pub(crate) enum Step {
    Sql(&'static str),
    Data(DataStep),
}

/// The steps that need the keys; the store carries them out (see `Store::data_step`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataStep {
    /// Seal `Contact` into the rows saved before requests existed, which have
    /// no standing. After it, a missing standing is damage.
    SealMissingStandings,
}

/// One entry per schema version, applied in order. Never edit a released step:
/// add a new one.
const STEPS: &[Step] = &[
    // 1: who you are. One row; both columns are sealed (see `crypt::seal`).
    Step::Sql(
        "CREATE TABLE profile (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        seed BLOB NOT NULL,
        name BLOB NOT NULL
    );",
    ),
    // 2: contacts and messages. Rows are found by keyed tags (see `crypt::tag`)
    // of the peer's key and the message id; every value is sealed and bound to
    // its row. `waiting` (an unacked outgoing message) stays plain so the
    // outbox can be found without decrypting everything.
    Step::Sql(
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
    ),
    // 3: requests and blocking. The contacts table now holds everyone you know
    // of; `standing` (sealed) says whether each is a contact, a request or
    // blocked. Rows from before this step have none: they're contacts.
    Step::Sql("ALTER TABLE contacts ADD COLUMN standing BLOB;"),
    // 4: see `DataStep::SealMissingStandings`.
    Step::Data(DataStep::SealMissingStandings),
    // 5: the tags of messages deleted with their history, so a message sent
    // again (its ack got lost) is known and not taken for a new request.
    // Tags only: nothing about the content.
    Step::Sql(
        "CREATE TABLE forgotten (
        peer BLOB NOT NULL,
        msg  BLOB NOT NULL,
        PRIMARY KEY (peer, msg)
    ) WITHOUT ROWID;",
    ),
];

/// What [`upgrade`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Upgrade {
    Done,
    /// A newer version of the app wrote this file; leave it alone.
    TooNew,
}

/// Brings `db` up to the newest schema. `data` carries out the data steps on
/// the same connection. Each step and its version bump are one transaction.
pub(crate) fn upgrade(
    db: &Connection,
    data: &mut dyn FnMut(DataStep) -> Result<(), StoreError>,
) -> Result<Upgrade, StoreError> {
    let current: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let target = STEPS.len() as i64;
    if current > target {
        return Ok(Upgrade::TooNew);
    }
    for (i, step) in STEPS.iter().enumerate().skip(current as usize) {
        let tx = db.unchecked_transaction()?;
        match step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::Data(step) => data(*step)?,
        }
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

    fn no_data(_: DataStep) -> Result<(), StoreError> {
        Ok(())
    }

    #[test]
    fn upgrade_creates_the_tables_once() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(upgrade(&db, &mut no_data).unwrap(), Upgrade::Done);
        assert_eq!(version(&db), STEPS.len() as i64);
        assert_eq!(upgrade(&db, &mut no_data).unwrap(), Upgrade::Done);
        db.execute("INSERT INTO profile (id, seed, name) VALUES (1, x'00', x'00')", []).unwrap();
        db.execute("INSERT INTO contacts (peer, key, name, standing) VALUES (x'01', x'00', x'00', x'00')", []).unwrap();
        db.execute("INSERT INTO messages (peer, msg, body, mark, waiting) VALUES (x'01', x'02', x'00', x'00', 1)", [])
            .unwrap();
    }

    #[test]
    fn a_newer_schema_is_left_alone() {
        let db = Connection::open_in_memory().unwrap();
        db.pragma_update(None, "user_version", 99).unwrap();
        assert_eq!(upgrade(&db, &mut no_data).unwrap(), Upgrade::TooNew);
        assert_eq!(version(&db), 99);
    }

    #[test]
    fn data_steps_run_once_in_their_place() {
        let db = Connection::open_in_memory().unwrap();
        let mut ran = Vec::new();
        upgrade(&db, &mut |step| {
            ran.push((step, version(&db)));
            Ok(())
        })
        .unwrap();
        assert_eq!(ran, [(DataStep::SealMissingStandings, 3)]);
        upgrade(&db, &mut |_| panic!("ran again")).unwrap();
    }

    #[test]
    fn a_failed_data_step_leaves_the_version_where_it_was() {
        let db = Connection::open_in_memory().unwrap();
        let failed = upgrade(&db, &mut |_| Err(StoreError::BadKdfParams));
        assert!(matches!(failed, Err(StoreError::BadKdfParams)));
        assert_eq!(version(&db), 3);
    }
}
