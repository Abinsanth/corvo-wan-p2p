//! Who you are: the 12 recovery words, the key they produce, and the code
//! people use to reach you.
//!
//! The words encode a 128-bit [`Seed`] (BIP39). The seed is stretched with
//! HKDF into the iroh secret key, so the same words always give the same key
//! and therefore the same [`Code`].

use std::fmt;
use std::str::FromStr;

use bip39::Mnemonic;
use hkdf::Hkdf;
use iroh_base::{PublicKey, SecretKey};
use sha2::Sha256;
use zeroize::Zeroize;

/// How many recovery words there are.
pub const PHRASE_WORDS: usize = 12;

/// Every code starts with this, so it's recognisable when pasted: the
/// bech32m human-readable part `corvo` and its separator `1`.
pub const CODE_PREFIX: &str = "corvo1";
/// The bech32m human-readable part. It's covered by the checksum.
const HRP: &str = "corvo";
/// The 32-byte key in code characters, 5 bits each (the last one padded).
/// Two different keys always differ within these.
pub const KEY_CHARS: usize = 52;
/// Characters of bech32m checksum after the key.
const CHECK_CHARS: usize = 6;
/// The 32 characters codes are written in (BIP-173): no b, i, o or 1, which
/// are easy to misread.
const ALPHABET: &str = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
/// What a valid bech32m string's checksum works out to (BIP-350).
const BECH32M: u32 = 0x2bc8_30a3;
/// How codes started in the test versions 0.1.0-rc.1 and rc.2.
const OLD_PREFIX: &str = "corvo:";

/// HKDF label for turning the seed into the secret key. Changing it changes every code.
const KEY_LABEL: &[u8] = b"corvo-wan-p2p key v1";

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

/// Someone's address: their public key, written as a bech32m string (BIP-350):
/// `corvo1` + 52 key characters + 6 checksum characters. The checksum catches
/// any one wrong character, and any four.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Code(PublicKey);

impl Code {
    pub fn from_key(key: PublicKey) -> Code {
        Code(key)
    }

    pub fn key(&self) -> PublicKey {
        self.0
    }

    /// The first 8 characters after `corvo1`, for the status line.
    pub fn short(&self) -> String {
        spelled(&groups(self.0.as_bytes()))[..8].to_string()
    }
}

/// The key's bits in 5-bit groups, the last one padded with zeros.
fn groups(bytes: &[u8; 32]) -> Vec<u8> {
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::with_capacity(KEY_CHARS));
    for &byte in bytes {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(((acc >> bits) & 31) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(((acc << (5 - bits)) & 31) as u8);
    }
    out
}

/// The key back from its 5-bit groups; `None` if the padding isn't zeros.
fn key_bytes(groups: &[u8]) -> Option<[u8; 32]> {
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::with_capacity(32));
    for &group in groups {
        acc = (acc << 5) | u32::from(group);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if acc != 0 {
        return None;
    }
    out.try_into().ok()
}

fn spelled(groups: &[u8]) -> String {
    groups.iter().map(|&g| char::from(ALPHABET.as_bytes()[usize::from(g)])).collect()
}

/// Whether `c` is one of the 32 characters codes are written in.
pub fn is_code_char(c: char) -> bool {
    char_value(c).is_some()
}

fn char_value(c: char) -> Option<u8> {
    ALPHABET.find(c).and_then(|i| u8::try_from(i).ok())
}

/// The bech32 checksum function (BIP-173).
fn polymod(values: &[u8]) -> u32 {
    const GENERATOR: [u32; 5] = [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3];
    let mut check = 1u32;
    for &value in values {
        let top = check >> 25;
        check = ((check & 0x1ff_ffff) << 5) ^ u32::from(value);
        for (i, g) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                check ^= g;
            }
        }
    }
    check
}

/// The human-readable part as the checksum sees it.
fn hrp_expand(hrp: &str) -> Vec<u8> {
    let bytes = hrp.as_bytes();
    bytes.iter().map(|b| b >> 5).chain([0]).chain(bytes.iter().map(|b| b & 31)).collect()
}

/// The 6 checksum groups for `data`.
fn checksum(data: &[u8]) -> Vec<u8> {
    let sum = polymod(&[hrp_expand(HRP), data.to_vec(), vec![0; CHECK_CHARS]].concat()) ^ BECH32M;
    (0..CHECK_CHARS).map(|i| ((sum >> (5 * (CHECK_CHARS - 1 - i))) & 31) as u8).collect()
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let data = groups(self.0.as_bytes());
        write!(f, "{CODE_PREFIX}{}{}", spelled(&data), spelled(&checksum(&data)))
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code({self})")
    }
}

/// Dropped from pasted codes: spaces, line breaks and characters that show nothing.
fn pasted_noise(c: char) -> bool {
    c.is_whitespace() || is_invisible(c) || matches!(c, '\u{200C}' | '\u{200D}')
}

impl FromStr for Code {
    type Err = CodeError;

    /// Accepts what people paste: any case, spaces, line breaks or invisible
    /// characters inside, and with or without the `corvo1` prefix.
    fn from_str(s: &str) -> Result<Code, CodeError> {
        let joined: String = s.chars().filter(|&c| !pasted_noise(c)).collect::<String>().to_lowercase();
        if joined.starts_with(OLD_PREFIX) {
            return Err(CodeError::OldFormat);
        }
        let text = joined.strip_prefix(CODE_PREFIX).unwrap_or(&joined);
        if text.is_empty() {
            return Err(CodeError::Empty);
        }
        // An old code without its prefix: base32 with a letter codes never have.
        let base32 = |c: char| matches!(c, 'a'..='z' | '2'..='7');
        if text.len() == 56 && text.chars().all(base32) && text.contains(['b', 'i', 'o']) {
            return Err(CodeError::OldFormat);
        }
        if let Some(bad) = text.chars().find(|&c| char_value(c).is_none()) {
            return Err(if matches!(bad, 'b' | 'i' | 'o' | '1') {
                CodeError::NeverInCodes(bad)
            } else {
                CodeError::BadCharacter(bad)
            });
        }
        if text.len() != KEY_CHARS + CHECK_CHARS {
            return Err(CodeError::WrongLength(text.len()));
        }
        let values: Vec<u8> = text.chars().filter_map(char_value).collect();
        if polymod(&[hrp_expand(HRP), values.clone()].concat()) != BECH32M {
            return Err(CodeError::Typo);
        }
        let bytes = key_bytes(&values[..KEY_CHARS]).ok_or(CodeError::Typo)?;
        PublicKey::from_bytes(&bytes).map(Code).map_err(|_| CodeError::Typo)
    }
}

/// How a refused character is named: as itself when it can be seen, and
/// always by its code point.
fn shown(c: char) -> String {
    if c.is_alphanumeric() || c.is_ascii_graphic() {
        format!("'{c}' (U+{:04X})", u32::from(c))
    } else {
        format!("U+{:04X}", u32::from(c))
    }
}

/// Why a pasted code was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("that's empty; paste the whole code, starting with corvo1")]
    Empty,
    #[error("a code can't contain {}; paste it again", shown(*.0))]
    BadCharacter(char),
    #[error("codes never contain b, i, o or 1, but this has '{0}'; check that character, or copy the code once more")]
    NeverInCodes(char),
    #[error("that code is {0} characters long; it should be 58 after corvo1. Part of it is probably missing")]
    WrongLength(usize),
    #[error("this code has a typo; ask for it again or copy it once more")]
    Typo,
    #[error("that's an old-style code from a test version; ask them for their new one, which starts with corvo1")]
    OldFormat,
}

/// The longest display name, in characters.
pub const NAME_MAX_CHARS: usize = 32;

/// Tidies a display name: trims it and turns runs of spaces into one.
/// It must end up 1 to 32 characters, with no control characters and nothing
/// invisible (see [`is_invisible`]).
pub fn clean_name(raw: &str) -> Result<String, NameError> {
    if raw.chars().any(char::is_control) {
        return Err(NameError::ControlCharacter);
    }
    if let Some(c) = raw.chars().find(|&c| is_invisible(c)) {
        return Err(NameError::Invisible(c));
    }
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    match name.chars().count() {
        0 => Err(NameError::Empty),
        n if n > NAME_MAX_CHARS => Err(NameError::TooLong(n)),
        _ => Ok(name),
    }
}

/// Characters that never belong in a name: they show nothing, or they turn
/// the text around them backwards, so a name can pass for someone else's.
/// The zero-width joiners U+200C and U+200D, variation selectors and the tag
/// characters of subdivision flags are allowed, because emoji and Indic and Persian
/// writing need them; `names` ignores them when comparing.
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' // soft hyphen
            | '\u{034F}' // combining grapheme joiner
            | '\u{061C}' // Arabic letter mark
            | '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}' // Hangul fillers
            | '\u{17B4}' | '\u{17B5}' // Khmer inherent vowels
            | '\u{180B}'..='\u{180F}' // Mongolian variation selectors and vowel separator
            | '\u{200B}' // zero-width space
            | '\u{200E}' | '\u{200F}' // left-to-right and right-to-left marks
            | '\u{202A}'..='\u{202E}' // direction embeddings and overrides
            | '\u{2060}'..='\u{206F}' // word joiner, invisible operators, direction isolates
            | '\u{FEFF}' // byte-order mark
            | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation
            | '\u{2800}' // braille blank
            | '\u{1BCA0}'..='\u{1BCA3}' // shorthand format controls
            | '\u{1D173}'..='\u{1D17A}' // musical formatting
            | '\u{E0001}' // language tag
    )
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
    #[error("a name can't contain invisible or text-direction characters (U+{:04X})", u32::from(*.0))]
    Invisible(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard all-zero BIP39 test phrase.
    const VECTOR_PHRASE: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    /// What `VECTOR_PHRASE` must always produce. If these change, every user's code changes.
    const VECTOR_KEY: &str = "407ac8e2d82b601d521b40b834a5c2b63f0c4f8746fd99a2f6430fe21f5355ba";
    const VECTOR_CODE: &str = "corvo1gpav3ckc9dsp65smgzurffwzkclscnu8gm7enghkgv87y86n2kaq5jnjm8";

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

    /// The same key in the code format of `0.1.0-rc.1` and `rc.2`.
    const OLD_VECTOR_CODE: &str = "corvo:ib5mrywyfnqb2uq3ic4djjocwy7qyt4hi36ztixwimh6eh2tkw5ageu6";

    #[test]
    fn code_round_trips_and_shows_a_short_form() {
        let code = vector().code();
        assert_eq!(VECTOR_CODE.parse::<Code>().unwrap(), code);
        assert_eq!(code.short(), "gpav3ckc");
        assert_eq!(code.to_string().len(), 64);
        assert_eq!(code.to_string().len(), CODE_PREFIX.len() + KEY_CHARS + CHECK_CHARS);
    }

    #[test]
    fn pasted_codes_may_have_spaces_capitals_or_no_prefix() {
        let code = vector().code();
        let body = &VECTOR_CODE[CODE_PREFIX.len()..];
        assert_eq!(body.parse::<Code>().unwrap(), code);
        assert_eq!(format!("  {}  ", VECTOR_CODE.to_uppercase()).parse::<Code>().unwrap(), code);
        assert_eq!(format!(" {} ", body.to_uppercase()).parse::<Code>().unwrap(), code);
        let wrapped = format!("{}\n{}", &VECTOR_CODE[..30], &VECTOR_CODE[30..]);
        assert_eq!(wrapped.parse::<Code>().unwrap(), code);
    }

    #[test]
    fn pasted_codes_ignore_zero_width_characters() {
        let code = vector().code();
        for mark in ['\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}'] {
            for at in [0, 10, VECTOR_CODE.len()] {
                let mut pasted = VECTOR_CODE.to_string();
                pasted.insert(at, mark);
                assert_eq!(pasted.parse::<Code>(), Ok(code), "U+{:04X} at {at}", u32::from(mark));
            }
        }
    }

    /// Checks a whole bech32m string the way BIP-350 does, for the reference vectors.
    fn bech32m_valid(s: &str) -> bool {
        let s = s.to_lowercase();
        let Some(at) = s.rfind('1').filter(|&at| at >= 1 && at + 7 <= s.len()) else { return false };
        let Some(data) = s[at + 1..].chars().map(char_value).collect::<Option<Vec<u8>>>() else { return false };
        polymod(&[hrp_expand(&s[..at]), data].concat()) == BECH32M
    }

    #[test]
    fn bech32m_reference_vectors_pass() {
        let valid = [
            "A1LQFN3A",
            "a1lqfn3a",
            "an83characterlonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11sg7hg6",
            "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx",
            "11llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllludsr8",
            "split1checkupstagehandshakeupstreamerranterredcaperredlc445v",
            "?1v759aa",
        ];
        for v in valid {
            assert!(bech32m_valid(v), "{v} should pass");
        }
        let invalid = [
            "M1VUXWEZ",
            "in1muywd",
            "y1b0jsk6g",
            "lt1igcx5c0",
            "mm1crxm3i",
            "au1s5cgom",
            "qyrz8wqd2c9m",
            "1qyrz8wqd2c9m",
        ];
        for v in invalid {
            assert!(!bech32m_valid(v), "{v} should fail");
        }
    }

    /// Every way to change one character of `code`, swap two neighbours, or
    /// drop or add one character.
    fn mistakes(code: &str) -> Vec<String> {
        let body: Vec<char> = code[CODE_PREFIX.len()..].chars().collect();
        let text = |chars: &[char]| format!("{CODE_PREFIX}{}", chars.iter().collect::<String>());
        let mut out = Vec::new();
        for i in 0..body.len() {
            for c in ALPHABET.chars().filter(|&c| c != body[i]) {
                let mut typo = body.clone();
                typo[i] = c;
                out.push(text(&typo));
            }
            if i + 1 < body.len() && body[i] != body[i + 1] {
                let mut swapped = body.clone();
                swapped.swap(i, i + 1);
                out.push(text(&swapped));
            }
            let mut dropped = body.clone();
            dropped.remove(i);
            out.push(text(&dropped));
            let mut added = body.clone();
            added.insert(i, 'q');
            out.push(text(&added));
        }
        out
    }

    #[test]
    fn every_single_typo_swap_and_missing_or_extra_character_is_caught() {
        for _ in 0..200 {
            let code = Seed::generate().code().to_string();
            for wrong in mistakes(&code) {
                assert!(wrong.parse::<Code>().is_err(), "{wrong} was accepted for {code}");
            }
        }
    }

    #[test]
    fn broken_codes_are_explained() {
        assert_eq!("".parse::<Code>(), Err(CodeError::Empty));
        assert_eq!("corvo1".parse::<Code>(), Err(CodeError::Empty));
        assert_eq!("corvo1acd!".parse::<Code>(), Err(CodeError::BadCharacter('!')));
        assert_eq!(VECTOR_CODE[..40].parse::<Code>(), Err(CodeError::WrongLength(34)));
        let mut extra = VECTOR_CODE.to_string();
        extra.push('q');
        assert_eq!(extra.parse::<Code>(), Err(CodeError::WrongLength(59)));
        for letter in ['b', 'i', 'o', '1'] {
            let typo = format!("{}{letter}{}", &VECTOR_CODE[..10], &VECTOR_CODE[11..]);
            assert_eq!(typo.parse::<Code>(), Err(CodeError::NeverInCodes(letter)), "{typo}");
        }
        assert!(CodeError::NeverInCodes('b').to_string().contains("never contain b, i, o or 1"));
        assert!(CodeError::WrongLength(34).to_string().contains("58 after corvo1"));
    }

    #[test]
    fn a_refused_character_is_named_by_its_code_point() {
        assert_eq!(CodeError::BadCharacter('!').to_string(), "a code can't contain '!' (U+0021); paste it again");
        assert_eq!(CodeError::BadCharacter('é').to_string(), "a code can't contain 'é' (U+00E9); paste it again");
        let accent = "corvo1gpav\u{0301}3ckc".parse::<Code>().unwrap_err();
        assert_eq!(accent, CodeError::BadCharacter('\u{0301}'));
        assert_eq!(accent.to_string(), "a code can't contain U+0301; paste it again");
    }

    #[test]
    fn an_old_style_code_is_named_as_such() {
        assert_eq!(OLD_VECTOR_CODE.parse::<Code>(), Err(CodeError::OldFormat));
        assert_eq!(OLD_VECTOR_CODE[OLD_PREFIX.len()..].parse::<Code>(), Err(CodeError::OldFormat), "no prefix");
        assert_eq!(OLD_VECTOR_CODE.to_uppercase().parse::<Code>(), Err(CodeError::OldFormat));
        assert!(CodeError::OldFormat.to_string().contains("ask them for their new one"));
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

    #[test]
    fn invisible_and_backwards_names_are_refused() {
        assert_eq!(clean_name("rahu\u{200B}l"), Err(NameError::Invisible('\u{200B}')));
        assert_eq!(clean_name("\u{202E}luhar"), Err(NameError::Invisible('\u{202E}')));
        assert_eq!(clean_name("\u{2067}abin\u{2069}"), Err(NameError::Invisible('\u{2067}')));
        assert_eq!(clean_name("\u{3164}"), Err(NameError::Invisible('\u{3164}')));
        for blank in ['\u{2800}', '\u{1D173}', '\u{1BCA0}', '\u{E0001}'] {
            assert_eq!(
                clean_name(&format!("rahul{blank}")),
                Err(NameError::Invisible(blank)),
                "U+{:04X}",
                u32::from(blank)
            );
        }
        assert_eq!(
            NameError::Invisible('\u{200B}').to_string(),
            "a name can't contain invisible or text-direction characters (U+200B)"
        );
        // Joiners are part of real writing and emoji.
        assert_eq!(clean_name("👩\u{200D}💻").unwrap(), "👩\u{200D}💻");
        assert_eq!(clean_name("می\u{200C}خواهم").unwrap(), "می\u{200C}خواهم");
    }
}
