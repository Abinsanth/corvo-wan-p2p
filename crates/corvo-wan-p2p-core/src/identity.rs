//! Who you are: the 12 recovery words, the key they produce, and the code
//! people use to reach you.
//!
//! The words encode a 128-bit [`Seed`] (BIP39). The seed is stretched with
//! HKDF into the iroh secret key, so the same words always give the same key
//! and therefore the same [`Code`].

use std::fmt;
use std::str::FromStr;

use bip39::Mnemonic;
use data_encoding::BASE32_NOPAD;
use hkdf::Hkdf;
use iroh_base::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// How many recovery words there are.
pub const PHRASE_WORDS: usize = 12;

/// Every code starts with this, so it's recognisable when pasted.
pub const CODE_PREFIX: &str = "corvo:";

/// The 32-byte key as lowercase base32 without padding.
const KEY_CHARS: usize = 52;
/// Characters of checksum after the key.
const CHECK_CHARS: usize = 4;

/// HKDF label for turning the seed into the secret key. Changing it changes every code.
const KEY_LABEL: &[u8] = b"corvo-wan-p2p key v1";
/// Domain-separation label for the code's checksum.
const CHECK_LABEL: &[u8] = b"corvo-wan-p2p code v1";

/// The 128 bits behind the 12 recovery words. Wiped from memory when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Seed([u8; 16]);

impl Seed {
    /// A new random seed, for a new account.
    pub fn generate() -> Seed {
        Seed(rand::random())
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Seed {
        Seed(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Reads typed recovery words. Case and extra spaces don't matter.
    pub fn from_phrase(phrase: &str) -> Result<Seed, PhraseError> {
        let words: Vec<String> = phrase.split_whitespace().map(str::to_lowercase).collect();
        if words.len() != PHRASE_WORDS {
            return Err(PhraseError::WrongWordCount(words.len()));
        }
        let mnemonic = Mnemonic::parse_normalized(&words.join(" ")).map_err(|e| match e {
            bip39::Error::UnknownWord(i) => PhraseError::UnknownWord { position: i + 1, word: words[i].clone() },
            _ => PhraseError::Checksum,
        })?;
        let mut entropy = mnemonic.to_entropy();
        let seed = Seed(entropy.as_slice().try_into().expect("12 words are 16 bytes"));
        entropy.zeroize();
        Ok(seed)
    }

    /// The 12 recovery words, in order.
    pub fn words(&self) -> Vec<&'static str> {
        Mnemonic::from_entropy(&self.0).expect("16 bytes is valid BIP39 entropy").words().collect()
    }

    /// The words joined by single spaces.
    pub fn phrase(&self) -> String {
        self.words().join(" ")
    }

    /// The iroh secret key these words produce.
    pub fn secret_key(&self) -> SecretKey {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.0).expand(KEY_LABEL, &mut out).expect("32 bytes is a valid HKDF length");
        let key = SecretKey::from_bytes(&out);
        out.zeroize();
        key
    }

    /// The code these words produce.
    pub fn code(&self) -> Code {
        Code::from_key(self.secret_key().public())
    }
}

impl Drop for Seed {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed(..)")
    }
}

/// Why typed recovery words were refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PhraseError {
    #[error("that's {0} words; the recovery phrase has 12")]
    WrongWordCount(usize),
    #[error("word {position} (\"{word}\") isn't one of the recovery words; check its spelling")]
    UnknownWord { position: usize, word: String },
    #[error("those words don't fit together; one is probably wrong or out of order")]
    Checksum,
}

/// Someone's address: their public key, written as `corvo:` + base32 + checksum.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Code(PublicKey);

impl Code {
    pub fn from_key(key: PublicKey) -> Code {
        Code(key)
    }

    pub fn key(&self) -> PublicKey {
        self.0
    }

    /// The first 8 characters after `corvo:`, for the status line.
    pub fn short(&self) -> String {
        body(&self.0)[..8].to_string()
    }
}

/// Lowercase base32 of the key, 52 characters.
fn body(key: &PublicKey) -> String {
    BASE32_NOPAD.encode(key.as_bytes()).to_lowercase()
}

/// 4 lowercase base32 characters from SHA-256 of the key.
fn checksum(key_bytes: &[u8; 32]) -> String {
    let hash = Sha256::new().chain_update(CHECK_LABEL).chain_update(key_bytes).finalize();
    BASE32_NOPAD.encode(&hash[..3])[..CHECK_CHARS].to_lowercase()
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{CODE_PREFIX}{}{}", body(&self.0), checksum(self.0.as_bytes()))
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code({self})")
    }
}

impl FromStr for Code {
    type Err = CodeError;

    /// Accepts what people paste: any case, spaces or line breaks inside, and
    /// with or without the `corvo:` prefix.
    fn from_str(s: &str) -> Result<Code, CodeError> {
        let joined: String = s.split_whitespace().collect::<String>().to_lowercase();
        let text = joined.strip_prefix(CODE_PREFIX).unwrap_or(&joined);
        if text.is_empty() {
            return Err(CodeError::Empty);
        }
        if let Some(bad) = text.chars().find(|c| !matches!(c, 'a'..='z' | '2'..='7')) {
            return Err(CodeError::BadCharacter(bad));
        }
        if text.len() != KEY_CHARS + CHECK_CHARS {
            return Err(CodeError::WrongLength(text.len()));
        }
        let (key_text, check_text) = text.split_at(KEY_CHARS);
        let bytes: [u8; 32] = BASE32_NOPAD
            .decode(key_text.to_uppercase().as_bytes())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(CodeError::Typo)?;
        if checksum(&bytes) != check_text {
            return Err(CodeError::Typo);
        }
        PublicKey::from_bytes(&bytes).map(Code).map_err(|_| CodeError::Typo)
    }
}

/// Why a pasted code was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("that's empty; paste the whole code, starting with corvo:")]
    Empty,
    #[error("a code can't contain '{0}'; paste it again")]
    BadCharacter(char),
    #[error("that code is {0} characters long; it should be 56 after corvo:. Part of it is probably missing")]
    WrongLength(usize),
    #[error("this code has a typo; ask for it again or copy it once more")]
    Typo,
}

/// The longest display name, in characters.
pub const NAME_MAX_CHARS: usize = 32;

/// Tidies a display name: trims it and turns runs of spaces into one.
/// It must end up 1 to 32 characters, with no control characters.
pub fn clean_name(raw: &str) -> Result<String, NameError> {
    if raw.chars().any(char::is_control) {
        return Err(NameError::ControlCharacter);
    }
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    match name.chars().count() {
        0 => Err(NameError::Empty),
        n if n > NAME_MAX_CHARS => Err(NameError::TooLong(n)),
        _ => Ok(name),
    }
}

/// Why a display name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("a name can't be empty")]
    Empty,
    #[error("that name is {0} characters; the most is 32")]
    TooLong(usize),
    #[error("a name can't contain tabs, line breaks or other control characters")]
    ControlCharacter,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard all-zero BIP39 test phrase.
    const VECTOR_PHRASE: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    /// What `VECTOR_PHRASE` must always produce. If these change, every user's code changes.
    const VECTOR_KEY: &str = "407ac8e2d82b601d521b40b834a5c2b63f0c4f8746fd99a2f6430fe21f5355ba";
    const VECTOR_CODE: &str = "corvo:ib5mrywyfnqb2uq3ic4djjocwy7qyt4hi36ztixwimh6eh2tkw5ageu6";

    fn vector() -> Seed {
        Seed::from_phrase(VECTOR_PHRASE).unwrap()
    }

    #[test]
    fn fixed_words_give_a_fixed_key_and_code() {
        let seed = vector();
        assert_eq!(seed.as_bytes(), &[0u8; 16]);
        assert_eq!(seed.secret_key().public().to_string(), VECTOR_KEY);
        assert_eq!(seed.code().to_string(), VECTOR_CODE);
    }

    #[test]
    fn words_round_trip_through_the_seed() {
        let seed = Seed::generate();
        assert_eq!(seed.words().len(), PHRASE_WORDS);
        assert_eq!(Seed::from_phrase(&seed.phrase()).unwrap(), seed);
        assert_eq!(Seed::from_bytes(*seed.as_bytes()).code(), seed.code());
    }

    #[test]
    fn typed_words_ignore_case_and_extra_spaces() {
        let messy = format!("  {}\n", VECTOR_PHRASE.to_uppercase().replace(' ', "   "));
        assert_eq!(Seed::from_phrase(&messy).unwrap(), vector());
    }

    #[test]
    fn wrong_words_are_explained() {
        assert_eq!(Seed::from_phrase("abandon about"), Err(PhraseError::WrongWordCount(2)));
        assert_eq!(Seed::from_phrase(""), Err(PhraseError::WrongWordCount(0)));
        let misspelt = VECTOR_PHRASE.replacen("abandon", "abandn", 1);
        assert_eq!(Seed::from_phrase(&misspelt), Err(PhraseError::UnknownWord { position: 1, word: "abandn".into() }));
        let swapped = VECTOR_PHRASE.replace("about", "abandon");
        assert_eq!(Seed::from_phrase(&swapped), Err(PhraseError::Checksum));
    }

    #[test]
    fn seed_debug_never_shows_the_secret() {
        assert_eq!(format!("{:?}", vector()), "Seed(..)");
    }

    #[test]
    fn code_round_trips_and_shows_a_short_form() {
        let code = vector().code();
        assert_eq!(VECTOR_CODE.parse::<Code>().unwrap(), code);
        assert_eq!(code.short(), "ib5mrywy");
        assert_eq!(code.to_string().len(), CODE_PREFIX.len() + 56);
    }

    #[test]
    fn pasted_codes_may_have_spaces_capitals_or_no_prefix() {
        let code = vector().code();
        let body = &VECTOR_CODE[CODE_PREFIX.len()..];
        assert_eq!(body.parse::<Code>().unwrap(), code);
        assert_eq!(format!("  {}  ", VECTOR_CODE.to_uppercase()).parse::<Code>().unwrap(), code);
        let wrapped = format!("{}\n{}", &VECTOR_CODE[..30], &VECTOR_CODE[30..]);
        assert_eq!(wrapped.parse::<Code>().unwrap(), code);
    }

    #[test]
    fn every_single_character_typo_is_caught() {
        let alphabet = "abcdefghijklmnopqrstuvwxyz234567";
        let body: Vec<char> = VECTOR_CODE[CODE_PREFIX.len()..].chars().collect();
        for i in 0..body.len() {
            for c in alphabet.chars().filter(|&c| c != body[i]) {
                let mut typo = body.clone();
                typo[i] = c;
                let typo: String = typo.into_iter().collect();
                assert!(typo.parse::<Code>().is_err(), "typo at {i} ({c}) was accepted: {typo}");
            }
        }
    }

    #[test]
    fn broken_codes_are_explained() {
        assert_eq!("".parse::<Code>(), Err(CodeError::Empty));
        assert_eq!("corvo:".parse::<Code>(), Err(CodeError::Empty));
        assert_eq!("corvo:abc!".parse::<Code>(), Err(CodeError::BadCharacter('!')));
        assert_eq!(VECTOR_CODE[..40].parse::<Code>(), Err(CodeError::WrongLength(34)));
        let mut extra = VECTOR_CODE.to_string();
        extra.push('a');
        assert_eq!(extra.parse::<Code>(), Err(CodeError::WrongLength(57)));
        // 0, 1, 8 and 9 aren't base32: a classic misread of o, l, b and g.
        assert_eq!(VECTOR_CODE.replacen('i', "1", 1).parse::<Code>(), Err(CodeError::BadCharacter('1')));
    }

    #[test]
    fn names_are_tidied_and_limited() {
        assert_eq!(clean_name("  abin   p  ").unwrap(), "abin p");
        assert_eq!(clean_name("ராகுல் 🚀").unwrap(), "ராகுல் 🚀");
        assert_eq!(clean_name("   "), Err(NameError::Empty));
        assert_eq!(clean_name(&"a".repeat(32)).unwrap().chars().count(), 32);
        assert_eq!(clean_name(&"a".repeat(33)), Err(NameError::TooLong(33)));
        assert_eq!(clean_name(&"é".repeat(33)), Err(NameError::TooLong(33)));
        assert_eq!(clean_name("bad\tname"), Err(NameError::ControlCharacter));
        assert_eq!(clean_name("two\nlines"), Err(NameError::ControlCharacter));
    }
}
