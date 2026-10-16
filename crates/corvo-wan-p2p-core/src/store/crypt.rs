//! The store's encryption: passphrase stretching, and sealing single values.

use argon2::{Algorithm, Argon2, Block, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

const NONCE_LEN: usize = 24;

/// How hard Argon2id works to turn a passphrase into a key. Saved in
/// `key.json`, so opening always uses the settings the store was locked with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory, in KiB.
    pub m_kib: u32,
    /// Passes.
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

impl KdfParams {
    /// 64 MiB, 3 passes: about half a second on a laptop, which is fine once per start.
    pub const DEFAULT: KdfParams = KdfParams { m_kib: 64 * 1024, t: 3, p: 1 };
    /// The weakest settings a passphrase is locked with (OWASP's floor for Argon2id).
    pub const MIN: KdfParams = KdfParams { m_kib: 19 * 1024, t: 2, p: 1 };
    /// The most a `key.json` may ask for, so a crafted one can't make opening
    /// use gigabytes of memory or run for hours.
    pub const MAX: KdfParams = KdfParams { m_kib: 1024 * 1024, t: 16, p: 8 };
    /// The smallest Argon2 allows. Only for tests, which would crawl otherwise;
    /// release builds don't have it.
    #[cfg(any(test, feature = "insecure-test-kdf"))]
    pub const FAST_INSECURE: KdfParams = KdfParams { m_kib: 8, t: 1, p: 1 };

    /// Whether each setting is between [`KdfParams::MIN`] and [`KdfParams::MAX`].
    pub fn within_limits(self) -> bool {
        let (min, max) = (KdfParams::MIN, KdfParams::MAX);
        (min.m_kib..=max.m_kib).contains(&self.m_kib)
            && (min.t..=max.t).contains(&self.t)
            && (min.p..=max.p).contains(&self.p)
    }

    /// Whether a passphrase may be locked or unlocked with these settings.
    pub(crate) fn usable(self) -> bool {
        #[cfg(any(test, feature = "insecure-test-kdf"))]
        if self == KdfParams::FAST_INSECURE {
            return true;
        }
        self.within_limits()
    }
}

/// Stretches a passphrase into a 32-byte key. `None` if `params` are outside
/// what Argon2 accepts (a damaged `key.json`).
///
/// The passphrase is NFC-normalized first: the same text on screen can arrive
/// as different bytes (é as one character, or e plus an accent) depending on
/// the keyboard or password manager.
pub(crate) fn passphrase_key(passphrase: &str, salt: &[u8], params: KdfParams) -> Option<Zeroizing<[u8; 32]>> {
    let params = Params::new(params.m_kib, params.t, params.p, Some(32)).ok()?;
    let normalized = Zeroizing::new(passphrase.nfc().collect::<String>());
    let mut key = Zeroizing::new([0u8; 32]);
    // Argon2's working memory holds what the key is made from: ours, so it's wiped.
    let mut memory = Zeroizing::new(vec![Block::default(); params.block_count()]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into_with_memory(normalized.as_bytes(), salt, key.as_mut(), memory.as_mut_slice())
        .ok()?;
    Some(key)
}

/// A purpose-specific key from the data key, so one key is never used for two jobs.
pub(crate) fn subkey(data_key: &[u8; 32], label: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, data_key).expand(label, out.as_mut()).expect("32 bytes is a valid HKDF length");
    out
}

/// A stable, secret-keyed name for a value, so rows can be found by it without
/// storing the value itself: HMAC-SHA256(key, kind, 0, value). `kind` keeps
/// different sorts of value (peers, message ids) from ever sharing a tag.
pub(crate) fn tag(key: &[u8; 32], kind: &[u8], value: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(kind);
    mac.update(&[0]);
    mac.update(value);
    mac.finalize().into_bytes().into()
}

/// Encrypts `plain` with a fresh random nonce. `aad` says where the value
/// belongs (for example `b"profile.name"`), so a value copied into another
/// place won't open there. Output: nonce, then ciphertext.
pub(crate) fn seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let nonce: [u8; NONCE_LEN] = rand::random();
    let ct = XChaCha20Poly1305::new(&(*key).into())
        .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad })
        .expect("encrypting in memory can't fail");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    out
}

/// Reverses [`seal`]. `None` if the key or `aad` is wrong or the bytes were changed.
pub(crate) fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if sealed.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    XChaCha20Poly1305::new(&(*key).into())
        .decrypt(&XNonce::from(nonce), Payload { msg: ct, aad })
        .ok()
        .map(Zeroizing::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn sealed_values_open_only_with_the_same_key_and_place() {
        let sealed = seal(&KEY, b"profile.name", b"abin");
        assert_eq!(open(&KEY, b"profile.name", &sealed).unwrap().as_slice(), b"abin");
        assert!(open(&[8; 32], b"profile.name", &sealed).is_none());
        assert!(open(&KEY, b"profile.seed", &sealed).is_none());
    }

    #[test]
    fn sealing_twice_gives_different_bytes() {
        assert_ne!(seal(&KEY, b"a", b"same"), seal(&KEY, b"a", b"same"));
    }

    #[test]
    fn changed_or_short_bytes_do_not_open() {
        let mut sealed = seal(&KEY, b"a", b"abin");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(open(&KEY, b"a", &sealed).is_none());
        assert!(open(&KEY, b"a", &[0; 10]).is_none());
        assert!(open(&KEY, b"a", &[]).is_none());
    }

    #[test]
    fn passphrase_key_depends_on_passphrase_and_salt() {
        let p = KdfParams::FAST_INSECURE;
        let a = passphrase_key("correct horse", &[1; 16], p).unwrap();
        assert_eq!(a, passphrase_key("correct horse", &[1; 16], p).unwrap());
        assert_ne!(a, passphrase_key("correct horsf", &[1; 16], p).unwrap());
        assert_ne!(a, passphrase_key("correct horse", &[2; 16], p).unwrap());
    }

    #[test]
    fn impossible_kdf_params_are_refused_not_a_panic() {
        assert!(passphrase_key("x", &[1; 16], KdfParams { m_kib: 0, t: 0, p: 0 }).is_none());
    }

    #[test]
    fn ciphers_and_argon2_memory_wipe_themselves() {
        fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        fn wipeable<T: zeroize::Zeroize>() {}
        wiped_on_drop::<XChaCha20Poly1305>();
        wipeable::<argon2::Block>();
    }

    #[test]
    fn the_default_settings_are_within_limits_and_the_test_ones_are_not() {
        assert!(KdfParams::DEFAULT.within_limits());
        assert!(KdfParams::MIN.within_limits() && KdfParams::MAX.within_limits());
        assert!(!KdfParams::FAST_INSECURE.within_limits());
        for over in [
            KdfParams { m_kib: KdfParams::MAX.m_kib + 1, ..KdfParams::DEFAULT },
            KdfParams { t: KdfParams::MAX.t + 1, ..KdfParams::DEFAULT },
            KdfParams { p: KdfParams::MAX.p + 1, ..KdfParams::DEFAULT },
            KdfParams { m_kib: KdfParams::MIN.m_kib - 1, ..KdfParams::DEFAULT },
            KdfParams { t: KdfParams::MIN.t - 1, ..KdfParams::DEFAULT },
        ] {
            assert!(!over.within_limits(), "{over:?}");
        }
    }

    #[test]
    fn tags_are_stable_and_separated() {
        assert_eq!(tag(&KEY, b"peer", b"abc"), tag(&KEY, b"peer", b"abc"));
        assert_ne!(tag(&KEY, b"peer", b"abc"), tag(&KEY, b"msg", b"abc"));
        assert_ne!(tag(&KEY, b"peer", b"abc"), tag(&[8; 32], b"peer", b"abc"));
        assert_ne!(tag(&KEY, b"pe", b"erabc"), tag(&KEY, b"peer", b"abc"));
    }

    #[test]
    fn subkeys_differ_by_label() {
        assert_ne!(subkey(&KEY, b"one"), subkey(&KEY, b"two"));
    }
}
