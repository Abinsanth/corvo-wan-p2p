//! `key.json`: the data key, locked with the passphrase.
//!
//! The data key encrypts everything in `store.db`. Only this small file depends
//! on the passphrase, so changing the passphrase rewrites just this file.

use std::fs;
use std::path::Path;

use data_encoding::HEXLOWER;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::StoreError;
use super::crypt::{self, KdfParams};
use crate::files::write_private_atomic;

const FORMAT: u32 = 1;
const KDF: &str = "argon2id";
/// Binds the sealed data key to this file's purpose and format.
const AAD: &[u8] = b"corvo-wan-p2p key.json v1";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct KeyFile {
    format: u32,
    kdf: String,
    params: KdfParams,
    /// Hex.
    salt: String,
    /// Hex: the data key, sealed with the passphrase key.
    data_key: String,
}

impl KeyFile {
    /// Locks `data_key` with `passphrase`, using a fresh salt.
    pub(crate) fn lock(data_key: &[u8; 32], passphrase: &str, params: KdfParams) -> Result<KeyFile, StoreError> {
        let salt: [u8; 16] = rand::random();
        let key = crypt::passphrase_key(passphrase, &salt, params).ok_or(StoreError::BadKdfParams)?;
        Ok(KeyFile {
            format: FORMAT,
            kdf: KDF.into(),
            params,
            salt: HEXLOWER.encode(&salt),
            data_key: HEXLOWER.encode(&crypt::seal(&key, AAD, data_key)),
        })
    }

    /// Opens the data key. A wrong passphrase and a damaged file look the same
    /// to the cipher, so a file that parses is assumed intact.
    pub(crate) fn unlock(&self, passphrase: &str, path: &Path) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        let damaged = |why: &str| StoreError::Damaged { file: path.to_path_buf(), why: why.into() };
        if self.format != FORMAT || self.kdf != KDF {
            return Err(damaged("unknown format"));
        }
        let salt = HEXLOWER.decode(self.salt.as_bytes()).map_err(|_| damaged("bad salt"))?;
        let sealed = HEXLOWER.decode(self.data_key.as_bytes()).map_err(|_| damaged("bad data key"))?;
        let key = crypt::passphrase_key(passphrase, &salt, self.params).ok_or_else(|| damaged("bad settings"))?;
        let plain = crypt::open(&key, AAD, &sealed).ok_or(StoreError::WrongPassphrase)?;
        let data_key: [u8; 32] = plain.as_slice().try_into().map_err(|_| damaged("data key is the wrong size"))?;
        Ok(Zeroizing::new(data_key))
    }

    pub(crate) fn read(path: &Path) -> Result<KeyFile, StoreError> {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| StoreError::Damaged { file: path.to_path_buf(), why: e.to_string() })
    }

    pub(crate) fn write(&self, path: &Path) -> Result<(), StoreError> {
        let text = serde_json::to_string_pretty(self).expect("a KeyFile always serializes");
        write_private_atomic(path, text.as_bytes())?;
        Ok(())
    }
}
