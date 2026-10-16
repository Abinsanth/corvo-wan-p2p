//! Your encrypted data on disk.
//!
//! Two files in the data folder:
//! - `key.json` holds the data key, locked with your passphrase (see `keyfile`).
//! - `store.db` is SQLite. Every value in it is sealed with a key derived from
//!   the data key, so the file shows nothing readable.
//!
//! A `lock` file stops two copies of the app opening the same data at once.

mod crypt;
mod keyfile;
mod messages;
mod people;
mod schema;

use std::fs::{self, File, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use zeroize::Zeroizing;

pub use crypt::KdfParams;
use keyfile::KeyFile;
pub use messages::{Mark, StoredMessage};
pub use people::{Person, Standing};

use crate::files::{private_dir, private_file};
use crate::identity::{NameError, Seed, clean_name};

pub const KEY_FILE: &str = "key.json";
pub const DB_FILE: &str = "store.db";
const LOCK_FILE: &str = "lock";
/// How long a held lock is tried again before saying another copy has it.
const LOCK_PATIENCE: Duration = Duration::from_millis(300);
const LOCK_RETRY: Duration = Duration::from_millis(10);
/// SQLite's side files, which belong to `store.db`.
const DB_SIDE_FILES: [&str; 3] = ["store.db-journal", "store.db-wal", "store.db-shm"];

/// Label for the key that seals values in `store.db`.
const FIELD_KEY_LABEL: &[u8] = b"corvo-wan-p2p store fields v1";
/// Label for the key that makes row tags (see `crypt::tag`).
const TAG_KEY_LABEL: &[u8] = b"corvo-wan-p2p store tags v1";

/// Who you are, as saved in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub seed: Seed,
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("there's already an account in {0}")]
    AlreadyExists(PathBuf),
    #[error("there's no account in {0} yet")]
    NotFound(PathBuf),
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("the passphrase can't be empty")]
    EmptyPassphrase,
    #[error("corvo-wan-p2p is already open with this account; close the other window first")]
    AlreadyRunning,
    #[error("{0} was saved by a newer version of corvo-wan-p2p; update to open it")]
    TooNew(PathBuf),
    #[error("{file} is damaged: {why}")]
    Damaged { file: PathBuf, why: String },
    #[error("those passphrase settings aren't usable")]
    BadKdfParams,
    #[error(transparent)]
    Name(#[from] NameError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
}

/// An unlocked store. Holds the lock until dropped.
pub struct Store {
    dir: PathBuf,
    db: Connection,
    data_key: Zeroizing<[u8; 32]>,
    field_key: Zeroizing<[u8; 32]>,
    tag_key: Zeroizing<[u8; 32]>,
    _lock: File,
}

impl Store {
    /// Whether `dir` holds an account (its `key.json` exists).
    pub fn exists(dir: &Path) -> bool {
        dir.join(KEY_FILE).is_file()
    }

    /// Makes a new account in `dir`, locked with `passphrase`. `key.json` is
    /// written last, so a crash part-way leaves no account rather than a broken one.
    pub fn create(
        dir: &Path,
        passphrase: &str,
        params: KdfParams,
        seed: &Seed,
        name: &str,
    ) -> Result<Store, StoreError> {
        let name = clean_name(name)?;
        if passphrase.is_empty() {
            return Err(StoreError::EmptyPassphrase);
        }
        private_dir(dir)?;
        let lock = take_lock(dir)?;
        if Store::exists(dir) {
            return Err(StoreError::AlreadyExists(dir.to_path_buf()));
        }
        remove_db_files(dir)?; // left by a create that crashed before key.json
        let data_key = Zeroizing::new(rand::random::<[u8; 32]>());
        let store = Store::assemble(dir, data_key, lock)?;
        store.db.execute(
            "INSERT INTO profile (id, seed, name) VALUES (1, ?1, ?2)",
            params![store.seal(b"profile.seed", seed.as_bytes()), store.seal(b"profile.name", name.as_bytes())],
        )?;
        KeyFile::lock(&store.data_key, passphrase, params)?.write(&dir.join(KEY_FILE))?;
        Ok(store)
    }

    /// Unlocks the account in `dir`.
    pub fn open(dir: &Path, passphrase: &str) -> Result<Store, StoreError> {
        if !Store::exists(dir) {
            return Err(StoreError::NotFound(dir.to_path_buf()));
        }
        let lock = take_lock(dir)?;
        let key_path = dir.join(KEY_FILE);
        let data_key = KeyFile::read(&key_path)?.unlock(passphrase, &key_path)?;
        let store = Store::assemble(dir, data_key, lock)?;
        store.profile()?; // fail now, not later, if store.db doesn't match key.json
        Ok(store)
    }

    /// Opens `store.db` (creating it privately if missing) and brings its tables up to date.
    fn assemble(dir: &Path, data_key: Zeroizing<[u8; 32]>, lock: File) -> Result<Store, StoreError> {
        let db_path = dir.join(DB_FILE);
        private_file().create(true).truncate(false).open(&db_path)?;
        let db = Connection::open(&db_path)?;
        let field_key = crypt::subkey(&data_key, FIELD_KEY_LABEL);
        let tag_key = crypt::subkey(&data_key, TAG_KEY_LABEL);
        let store = Store { dir: dir.to_path_buf(), db, data_key, field_key, tag_key, _lock: lock };
        let upgraded = schema::upgrade(&store.db, &mut |step| store.data_step(step)).map_err(|e| match e {
            StoreError::Db(e)
                if matches!(e.sqlite_error_code(), Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)) =>
            {
                StoreError::Damaged { file: db_path.clone(), why: "it isn't a corvo-wan-p2p database".into() }
            }
            e => e,
        })?;
        if upgraded == schema::Upgrade::TooNew {
            return Err(StoreError::TooNew(db_path));
        }
        Ok(store)
    }

    fn data_step(&self, step: schema::DataStep) -> Result<(), StoreError> {
        match step {
            schema::DataStep::SealMissingStandings => self.seal_missing_standings(),
        }
    }

    /// Your seed and name.
    pub fn profile(&self) -> Result<Profile, StoreError> {
        let row: Option<(Vec<u8>, Vec<u8>)> = self
            .db
            .query_row("SELECT seed, name FROM profile WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let (seed, name) = row.ok_or_else(|| self.damaged("no profile"))?;
        let seed = self.open_value(b"profile.seed", &seed)?;
        let seed: [u8; 16] = seed.as_slice().try_into().map_err(|_| self.damaged("seed is the wrong size"))?;
        let name = String::from_utf8(self.open_value(b"profile.name", &name)?.to_vec())
            .map_err(|_| self.damaged("name isn't text"))?;
        Ok(Profile { seed: Seed::from_bytes(seed), name })
    }

    /// Changes your display name; returns it as saved (tidied, see `clean_name`).
    pub fn set_name(&self, name: &str) -> Result<String, StoreError> {
        let name = clean_name(name)?;
        self.db.execute("UPDATE profile SET name = ?1 WHERE id = 1", [self.seal(b"profile.name", name.as_bytes())])?;
        Ok(name)
    }

    /// Locks the data key with a new passphrase. Only `key.json` changes.
    pub fn change_passphrase(&self, new_passphrase: &str, params: KdfParams) -> Result<(), StoreError> {
        if new_passphrase.is_empty() {
            return Err(StoreError::EmptyPassphrase);
        }
        KeyFile::lock(&self.data_key, new_passphrase, params)?.write(&self.dir.join(KEY_FILE))
    }

    /// Whether `passphrase` unlocks this account (for commands that ask for it
    /// again). Another account's `key.json` never passes, whatever it's locked with.
    pub fn check_passphrase(&self, passphrase: &str) -> Result<bool, StoreError> {
        let key_path = self.dir.join(KEY_FILE);
        match KeyFile::read(&key_path)?.unlock(passphrase, &key_path) {
            Ok(key) => Ok(*key == *self.data_key),
            Err(StoreError::WrongPassphrase) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Deletes the account in `dir` (for `--recover` after a forgotten passphrase).
    /// `key.json` goes first, so a wipe cut short leaves no account.
    pub fn wipe(dir: &Path) -> Result<(), StoreError> {
        if !dir.exists() {
            return Ok(());
        }
        let _lock = take_lock(dir)?;
        remove_if_present(&dir.join(KEY_FILE))?;
        remove_db_files(dir)
    }

    fn seal(&self, place: &[u8], plain: &[u8]) -> Vec<u8> {
        crypt::seal(&self.field_key, place, plain)
    }

    fn open_value(&self, place: &[u8], sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        crypt::open(&self.field_key, place, sealed).ok_or_else(|| self.damaged("a value doesn't decrypt"))
    }

    /// Like `seal`, but also bound to one row, so a value can't be moved to another row.
    fn seal_at(&self, column: &[u8], row: &[&[u8]], plain: &[u8]) -> Vec<u8> {
        crypt::seal(&self.field_key, &row_aad(column, row), plain)
    }

    fn open_at(&self, column: &[u8], row: &[&[u8]], sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
        self.open_value(&row_aad(column, row), sealed)
    }

    fn damaged(&self, why: &str) -> StoreError {
        StoreError::Damaged { file: self.dir.join(DB_FILE), why: why.into() }
    }
}

/// Associated data for a value in one row: the column name, then each part of
/// the row's key, each preceded by its length.
fn row_aad(column: &[u8], row: &[&[u8]]) -> Vec<u8> {
    let mut aad = column.to_vec();
    for part in row {
        aad.extend_from_slice(&(part.len() as u32).to_be_bytes());
        aad.extend_from_slice(part);
    }
    aad
}

/// Takes the lock file in `dir`, or reports that another copy holds it. A
/// lock still held is tried again for a moment first: a process started just
/// before (curl or tar, say) shares the lock's file until it runs its own
/// program, so a lock can outlive the store that let it go.
fn take_lock(dir: &Path) -> Result<File, StoreError> {
    let file = private_file().create(true).truncate(false).open(dir.join(LOCK_FILE))?;
    let give_up = Instant::now() + LOCK_PATIENCE;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < give_up => std::thread::sleep(LOCK_RETRY),
            Err(TryLockError::WouldBlock) => return Err(StoreError::AlreadyRunning),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

fn remove_db_files(dir: &Path) -> Result<(), StoreError> {
    remove_if_present(&dir.join(DB_FILE))?;
    for side in DB_SIDE_FILES {
        remove_if_present(&dir.join(side))?;
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
