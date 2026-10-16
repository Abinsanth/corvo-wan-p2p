//! What to call people on screen, and which person a typed name means.
//!
//! Names aren't unique, and anyone can pick any name. So each person gets a
//! **label**: their name, or, when it looks like someone else's, their name
//! plus the start of their code (`rahul·m4x8`). "Looks like" is judged on a
//! [`skeleton`] that ignores case, joiners, full-width forms and the Cyrillic
//! and Greek letters that pass for Latin ones, so a request from "Rаhul" (with
//! a Cyrillic а) can't pass for your contact rahul.

use std::collections::{HashMap, HashSet};

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::identity::{CODE_PREFIX, Code, KEY_CHARS};
use crate::store::Person;

/// Between a name and the start of the code in a label.
pub const SEPARATOR: char = '·';
/// How much of the code a label shows at first.
const TAIL: usize = 4;
/// What the screen calls you. Someone whose name looks like it always shows a
/// tail, so their lines can't pass for yours.
pub const YOU: &str = "you";

/// A label for each person, all different from each other however they're
/// compared. Someone whose app hasn't sent a name yet is shown by the start of
/// their code.
pub fn labels(people: &[Person]) -> HashMap<Code, String> {
    // How much of each person's code their label shows; 0 means just the name.
    let names = name_skeletons(people);
    let mut tails: Vec<usize> =
        people.iter().map(|p| if looks_like_a_label(&p.name, &names) || is_you(&p.name) { TAIL } else { 0 }).collect();
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

/// Everyone's names as skeletons, and [`YOU`], since labels can't pass for it either.
fn name_skeletons(people: &[Person]) -> HashSet<String> {
    let names = people.iter().filter(|p| !p.name.is_empty()).map(|p| skeleton(&p.name));
    names.chain([YOU.to_string()]).collect()
}

fn is_you(name: &str) -> bool {
    skeleton(name) == YOU
}

/// A name split the way a label is: what comes before its last separator (any
/// mark that isn't a letter, digit, space or accent) and the letters and
/// digits after it. `None` when it doesn't end like that.
fn split_tail(name: &str) -> Option<(&str, &str)> {
    let separator = |c: char| !c.is_alphanumeric() && !c.is_whitespace() && !is_combining_mark(c);
    let (at, c) = name.char_indices().rfind(|&(_, c)| separator(c))?;
    let (before, tail) = (name[..at].trim(), &name[at + c.len_utf8()..]);
    (!before.is_empty() && !tail.is_empty() && tail.chars().all(char::is_alphanumeric)).then_some((before, tail))
}

/// Whether a name looks like another person's label would: their name, a
/// separator and something after it (`rahul·m4x8`, `rahul.m4x8`). Such a name
/// always shows a tail of its own, so it can't pass for the label that person
/// gets once a namesake of theirs appears.
fn looks_like_a_label(name: &str, names: &HashSet<String>) -> bool {
    split_tail(name).is_some_and(|(before, _)| names.contains(&skeleton(before)))
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
    // A label from before (when they had a namesake) still means the person
    // whose code it shows, never someone who took it as their name.
    if let Some((name, tail)) = split_tail(typed) {
        let (name, tail) = (skeleton(name), tail.to_lowercase());
        let owners: Vec<&Person> = people
            .iter()
            .filter(|p| !p.name.is_empty() && skeleton(&p.name) == name && key_text(&p.code).starts_with(&tail))
            .collect();
        if let [owner] = owners.as_slice() {
            return Ok(owner.code);
        }
    }
    let names = name_skeletons(people);
    let named: Vec<&Person> = people
        .iter()
        .filter(|p| !p.name.is_empty() && skeleton(&p.name) == wanted && !looks_like_a_label(&p.name, &names))
        .collect();
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
            out.push(if c == SEPARATOR || is_dot_lookalike(c) { '.' } else { latin_lookalike(c).unwrap_or(c) });
        }
    }
    out
}

/// Dots drawn like the `·` in labels.
fn is_dot_lookalike(c: char) -> bool {
    matches!(
        c,
        '\u{0387}' // Greek ano teleia
            | '\u{1427}' // Canadian syllabics final middle dot
            | '\u{2022}' // bullet
            | '\u{2027}' // hyphenation point
            | '\u{2219}' // bullet operator
            | '\u{22C5}' // dot operator
            | '\u{2E31}' // word separator middle dot
            | '\u{30FB}' // katakana middle dot
            | '\u{A78F}' // Latin letter sinological dot
    )
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
    fn a_peer_called_you_gets_a_tail() {
        // Cyrillic у and о, and full-width letters, all pass for "you".
        for name in ["you", "You", "YOU", "\u{0443}\u{043E}u", "ｙｏｕ"] {
            let (them, abin) = (person(name), person("abin"));
            let labels = labels(&[them.clone(), abin.clone()]);
            assert_eq!(labels[&them.code], format!("{name}·{}", tail(&them, TAIL)), "{name:?}");
            assert_eq!(labels[&abin.code], "abin");
            assert_eq!(find(&[them.clone(), abin], &labels[&them.code]), Ok(them.code));
        }
        let fake = person("you.m4x8");
        assert!(labels(&[fake.clone()])[&fake.code].starts_with("you.m4x8·"), "can't pass for a you label");
        for name in ["yours", "youssef"] {
            let p = person(name);
            assert_eq!(labels(&[p.clone()])[&p.code], name);
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
    fn a_name_shaped_like_someone_elses_label_gets_its_own_tail() {
        let rahul = person("rahul");
        for copy in [format!("rahul·{}", tail(&rahul, TAIL)), format!("rahul.{}", tail(&rahul, TAIL))] {
            let copy = person(&copy);
            let two = labels(&[rahul.clone(), copy.clone()]);
            assert_eq!(two[&rahul.code], "rahul");
            assert_ne!(skeleton(&two[&copy.code]), skeleton(&format!("rahul·{}", tail(&rahul, TAIL))));
            assert!(two[&copy.code].starts_with(&format!("{}·", copy.name)), "{}", two[&copy.code]);
            // Once a second rahul comes along, everyone still looks different.
            let other = person("rahul");
            let three = labels(&[rahul.clone(), copy.clone(), other]);
            let mut all: Vec<String> = three.values().map(|l| skeleton(l)).collect();
            all.sort();
            all.dedup();
            assert_eq!(all.len(), 3, "labels clash: {three:?}");
        }
    }

    #[test]
    fn lookalike_dots_and_letters_cannot_pass_for_a_label() {
        let rahul = person("rahul");
        let t = tail(&rahul, TAIL);
        let armenian = format!("rahul·{}\u{0570}", &t[..3]);
        for fake in ['\u{22C5}', '\u{2022}', '\u{2219}', '\u{30FB}', '\u{FF65}', '\u{A78F}']
            .map(|dot| format!("rahul{dot}{t}"))
            .into_iter()
            .chain([armenian])
        {
            let fake = person(&fake);
            let other = person("rahul");
            let labels = labels(&[rahul.clone(), fake.clone(), other]);
            assert!(
                labels[&fake.code].starts_with(&format!("{}·", fake.name)),
                "{:?} got {}",
                fake.name,
                labels[&fake.code]
            );
        }
    }

    #[test]
    fn dotted_names_get_a_tail_only_next_to_the_name_before_the_dot() {
        for plain in ["mary.jane", "dr.mehta", "st.jude", "a.b. smith", "dr. rahul", "rahul.", "v1.0", "ராகுல்"]
        {
            let p = person(plain);
            assert_eq!(labels(std::slice::from_ref(&p))[&p.code], plain);
        }
        let (mary, mary_jane) = (person("mary"), person("mary.jane"));
        let labels = labels(&[mary.clone(), mary_jane.clone()]);
        assert_eq!(labels[&mary.code], "mary");
        assert!(labels[&mary_jane.code].starts_with("mary.jane·"));
    }

    #[test]
    fn a_typed_label_finds_the_person_whose_code_it_is() {
        let rahul = person("rahul");
        let t = tail(&rahul, TAIL);
        let impostor = person(&format!("rahul.{t}"));
        let people = [rahul.clone(), impostor.clone()];
        assert_eq!(find(&people, &format!("rahul·{t}")), Ok(rahul.code));
        assert_eq!(find(&people, &format!("rahul.{t}")), Ok(rahul.code));
        assert_eq!(find(&people, &labels(&people)[&impostor.code]), Ok(impostor.code));
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
