//! What to call people on screen, and which person a typed name means.
//!
//! Names aren't unique, and anyone can pick any name. So each person gets a
//! **label**: their name, or, when it looks like someone else's, their name
//! plus the start of their code (`rahul·m4x8`). "Looks like" is judged on a
//! [`skeleton`] that ignores case, joiners, full-width forms and the Cyrillic
//! and Greek letters that pass for Latin ones, so a request from "Rаhul" (with
//! a Cyrillic а) can't pass for your contact rahul.

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;

use crate::identity::{CODE_PREFIX, Code};
use crate::store::Person;

/// Between a name and the start of the code in a label.
pub const SEPARATOR: char = '·';
/// How much of the code a label shows at first.
const TAIL: usize = 4;
/// The code is 52 characters before its checksum; two codes always differ by then.
const KEY_CHARS: usize = 52;

/// A label for each person, all different from each other however they're
/// compared. Someone whose app hasn't sent a name yet is shown by the start of
/// their code.
pub fn labels(people: &[Person]) -> HashMap<Code, String> {
    // How much of each person's code their label shows; 0 means just the name.
    let mut tails = vec![0; people.len()];
    loop {
        let mut seen: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in people.iter().enumerate() {
            seen.entry(skeleton(&label(p, tails[i]))).or_default().push(i);
        }
        let mut changed = false;
        for clash in seen.values().filter(|c| c.len() > 1) {
            for &i in clash {
                if !people[i].name.is_empty() && tails[i] < KEY_CHARS {
                    tails[i] = if tails[i] == 0 { TAIL } else { tails[i] + 1 };
                    changed = true;
                }
            }
        }
        if !changed {
            return people.iter().zip(tails).map(|(p, n)| (p.code, label(p, n))).collect();
        }
    }
}

fn label(p: &Person, tail: usize) -> String {
    match (p.name.is_empty(), tail) {
        (true, _) => p.code.short(),
        (false, 0) => p.name.clone(),
        (false, n) => format!("{}{SEPARATOR}{}", p.name, &key_text(&p.code)[..n]),
    }
}

/// The code without its prefix.
fn key_text(code: &Code) -> String {
    code.to_string()[CODE_PREFIX.len()..].to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FindError {
    #[error("there's no one called {0}")]
    NoOne(String),
    #[error("more than one person is called {typed}: {}", choices.join(", "))]
    Several { typed: String, choices: Vec<String> },
}

/// The person `typed` means, among `people`: their code, their label (with
/// `.` accepted for `·`), or their name if only one of them has it.
pub fn find(people: &[Person], typed: &str) -> Result<Code, FindError> {
    let typed = typed.trim();
    if let Ok(code) = typed.parse::<Code>()
        && people.iter().any(|p| p.code == code)
    {
        return Ok(code);
    }
    let labels = labels(people);
    let wanted = skeleton(typed);
    if let Some(p) = people.iter().find(|p| skeleton(&labels[&p.code]) == wanted) {
        return Ok(p.code);
    }
    let named: Vec<&Person> = people.iter().filter(|p| !p.name.is_empty() && skeleton(&p.name) == wanted).collect();
    match named.as_slice() {
        [] => Err(FindError::NoOne(typed.to_string())),
        [one] => Ok(one.code),
        several => {
            let mut choices: Vec<String> = several.iter().map(|p| labels[&p.code].clone()).collect();
            choices.sort();
            Err(FindError::Several { typed: typed.to_string(), choices })
        }
    }
}

/// What a name looks like, for comparing: compatibility forms folded (NFKC),
/// lower case, joiners, variation selectors and tags dropped, `·` read as
/// `.`, and Latin lookalikes from Cyrillic and Greek read as Latin. Capitals
/// are matched before lowercasing, since many (Т, Н, Ν…) lowercase to
/// letters that no longer look Latin.
pub fn skeleton(name: &str) -> String {
    let mut out = String::new();
    for c in name.nfkc() {
        if let Some(latin) = latin_capital_lookalike(c) {
            out.push(latin);
            continue;
        }
        for c in c.to_lowercase().filter(|&c| !is_ignorable(c)) {
            out.push(if c == SEPARATOR { '.' } else { latin_lookalike(c).unwrap_or(c) });
        }
    }
    out
}

/// Marks that change nothing visible in a name.
fn is_ignorable(c: char) -> bool {
    matches!(c, '\u{200C}' | '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0000}'..='\u{E0FFF}')
}

/// The lower-case Latin letter a Cyrillic or Greek capital is drawn like, if any.
fn latin_capital_lookalike(c: char) -> Option<char> {
    Some(match c {
        'А' | 'Α' => 'a',
        'В' | 'Β' => 'b',
        'С' => 'c',
        'Е' | 'Ε' => 'e',
        'Н' | 'Η' => 'h',
        'І' | 'Ι' => 'i',
        'Ј' => 'j',
        'К' | 'Κ' => 'k',
        'М' | 'Μ' => 'm',
        'Ν' => 'n',
        'О' | 'Ο' => 'o',
        'Р' | 'Ρ' => 'p',
        'Ԛ' => 'q',
        'Ѕ' => 's',
        'Т' | 'Τ' => 't',
        'Ԝ' => 'w',
        'Х' | 'Χ' => 'x',
        'Ү' | 'Υ' => 'y',
        'Ζ' => 'z',
        _ => return None,
    })
}

/// The Latin letter a lower-case Cyrillic or Greek letter is drawn like, if any.
fn latin_lookalike(c: char) -> Option<char> {
    Some(match c {
        'а' | 'α' => 'a',
        'с' | 'ϲ' => 'c',
        'ԁ' => 'd',
        'е' => 'e',
        'һ' => 'h',
        'і' | 'ι' => 'i',
        'ј' => 'j',
        'к' | 'κ' => 'k',
        'о' | 'ο' => 'o',
        'р' | 'ρ' => 'p',
        'ԛ' => 'q',
        'ѕ' => 's',
        'ν' => 'v',
        'ԝ' => 'w',
        'х' | 'χ' => 'x',
        'у' => 'y',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Seed;
    use crate::store::Standing;

    fn person(name: &str) -> Person {
        Person { code: Seed::generate().code(), name: name.into(), last_seen: None, standing: Standing::Contact }
    }

    fn tail(p: &Person, n: usize) -> String {
        key_text(&p.code)[..n].to_string()
    }

    #[test]
    fn a_unique_name_is_its_own_label() {
        let (abin, rahul) = (person("abin"), person("rahul"));
        let labels = labels(&[abin.clone(), rahul.clone()]);
        assert_eq!((labels[&abin.code].as_str(), labels[&rahul.code].as_str()), ("abin", "rahul"));
    }

    #[test]
    fn a_shared_name_shows_the_start_of_each_code() {
        let (one, two, meera) = (person("rahul"), person("rahul"), person("meera"));
        let labels = labels(&[one.clone(), two.clone(), meera.clone()]);
        assert!(labels[&one.code].starts_with("rahul·"), "{}", labels[&one.code]);
        assert!(labels[&two.code].starts_with("rahul·"));
        assert_ne!(labels[&one.code], labels[&two.code]);
        assert_eq!(labels[&meera.code], "meera");
        if tail(&one, TAIL) != tail(&two, TAIL) {
            assert_eq!(labels[&one.code], format!("rahul·{}", tail(&one, TAIL)));
        }
    }

    #[test]
    fn lookalike_names_count_as_shared() {
        let rahul = person("rahul");
        for fake in ["Rahul", "RAHUL", "rаhul", "ｒａｈｕｌ", "rahu\u{200D}l", "rαhul"] {
            let fake = person(fake);
            let labels = labels(&[rahul.clone(), fake.clone()]);
            assert!(labels[&rahul.code].starts_with("rahul·"), "{:?} wasn't caught", fake.name);
            assert!(labels[&fake.code].contains('·'));
        }
    }

    #[test]
    fn capital_lookalikes_and_ignorable_marks_count_as_shared() {
        let pairs = [
            ("Tom", "Тom"),
            ("Hari", "Нari"),
            ("Mike", "Мike"),
            ("Ben", "Вen"),
            ("Nina", "ΝINA"),
            ("Zoe", "Ζoe"),
            ("rahul", "rahul\u{E0100}"),
            ("rahul", "rahul\u{E0020}"),
        ];
        for (real, fake) in pairs {
            let (real, fake) = (person(real), person(fake));
            let labels = labels(&[real.clone(), fake.clone()]);
            assert!(labels[&real.code].contains('·'), "{:?} passed for {:?}", fake.name, real.name);
            assert!(labels[&fake.code].contains('·'));
        }
    }

    #[test]
    fn a_name_copying_a_label_gets_a_label_too() {
        let (one, two) = (person("rahul"), person("rahul"));
        let first = labels(&[one.clone(), two.clone()]);
        let copycat = person(&first[&one.code]);
        let after = labels(&[one.clone(), two.clone(), copycat.clone()]);
        let all: Vec<String> = after.values().map(|l| skeleton(l)).collect();
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(all.len(), unique.len(), "labels clash: {after:?}");
        assert_ne!(after[&copycat.code], first[&one.code]);
    }

    #[test]
    fn someone_without_a_name_is_shown_by_their_code() {
        let p = person("");
        assert_eq!(labels(std::slice::from_ref(&p))[&p.code], p.code.short());
    }

    #[test]
    fn find_takes_a_name_a_label_or_a_code() {
        let (one, two, meera) = (person("rahul"), person("rahul"), person("Meera"));
        let people = [one.clone(), two.clone(), meera.clone()];
        let labels = labels(&people);
        assert_eq!(find(&people, "meera"), Ok(meera.code));
        assert_eq!(find(&people, " Meera "), Ok(meera.code));
        assert_eq!(find(&people, &labels[&one.code]), Ok(one.code));
        assert_eq!(find(&people, &labels[&two.code].replace('·', ".")), Ok(two.code));
        assert_eq!(find(&people, &one.code.to_string()), Ok(one.code));
        let Err(FindError::Several { choices, .. }) = find(&people, "rahul") else { panic!("expected several") };
        assert_eq!(choices.len(), 2);
        assert_eq!(find(&people, "ravi"), Err(FindError::NoOne("ravi".into())));
        let stranger = Seed::generate().code().to_string();
        assert_eq!(find(&people, &stranger), Err(FindError::NoOne(stranger.clone())));
    }
}
