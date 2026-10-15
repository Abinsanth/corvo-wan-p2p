//! Tab completion: command names as the first word, and people's labels
//! after the commands that take a name. Pure.

use corvo_wan_p2p_core::store::Standing;

use super::commands::COMMANDS;
use super::state::{Popup, State};
use super::view::{areas, input_rows};

/// What can be completed right now: labels, each list sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidates {
    pub contacts: Vec<String>,
    pub requests: Vec<String>,
    pub blocked: Vec<String>,
}

impl Candidates {
    pub fn from_state(state: &State) -> Candidates {
        let of = |standing: Standing| {
            let mut labels: Vec<String> =
                state.known.iter().filter(|p| p.standing == standing).map(|p| state.label(&p.code)).collect();
            sort(&mut labels);
            labels
        };
        Candidates { contacts: of(Standing::Contact), requests: of(Standing::Request), blocked: of(Standing::Blocked) }
    }

    /// Who the command `name` can take.
    fn pool(&self, name: &str) -> Option<Vec<String>> {
        let mut pool = match name {
            "chat" | "block" | "remove" => [&self.contacts[..], &self.requests[..]].concat(),
            "accept" => self.requests.clone(),
            "unblock" => self.blocked.clone(),
            "info" => [&self.contacts[..], &self.requests[..], &self.blocked[..]].concat(),
            _ => return None,
        };
        sort(&mut pool);
        Some(pool)
    }
}

fn sort(labels: &mut [String]) {
    labels.sort_by_key(|l| l.to_lowercase());
}

/// The word being completed and what it could become.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Byte offset in the input where the word starts.
    pub start: usize,
    /// Full replacements for the word.
    pub matches: Vec<String>,
}

/// Completes what's before the cursor (`before`): a command name as the
/// first word (never after `//`), or after a command that takes a name,
/// everything after it (names can have spaces).
pub fn complete(before: &str, cands: &Candidates) -> Option<Completion> {
    if before.contains('\n') {
        return None;
    }
    let lead = before.len() - before.trim_start().len();
    let command = before[lead..].strip_prefix('/')?;
    if command.starts_with('/') {
        return None;
    }
    let (start, matches): (usize, Vec<String>) = match command.split_once(char::is_whitespace) {
        None => {
            let mut names = COMMANDS.to_vec();
            names.sort_unstable();
            let part = command.to_lowercase();
            (lead, names.into_iter().filter(|n| n.starts_with(&part)).map(|n| format!("/{n}")).collect())
        }
        Some((name, args)) => {
            let pool = cands.pool(&name.to_lowercase())?;
            let word = args.trim_start();
            let part = word.to_lowercase();
            (before.len() - word.len(), pool.into_iter().filter(|l| l.to_lowercase().starts_with(&part)).collect())
        }
    };
    (!matches.is_empty()).then_some(Completion { start, matches })
}

/// The longest start all `matches` share, compared char by char.
pub fn common_prefix(matches: &[String]) -> String {
    let Some(first) = matches.first() else {
        return String::new();
    };
    let mut len = first.chars().count();
    for m in &matches[1..] {
        len = len.min(first.chars().zip(m.chars()).take_while(|(a, b)| a == b).count());
    }
    first.chars().take(len).collect()
}

/// Tab: one match goes straight in, with a space; several fill their shared
/// start and open the list. Not when the screen is too small to show it.
pub fn tab(state: &mut State) {
    if areas(state.size, input_rows(state)).is_none() {
        return;
    }
    let Some(c) = complete(state.editor.before_cursor(), &Candidates::from_state(state)) else {
        return;
    };
    if let [only] = c.matches.as_slice() {
        state.editor.replace_before_cursor(c.start, &format!("{only} "));
        return;
    }
    let prefix = common_prefix(&c.matches);
    if prefix.chars().count() > state.editor.before_cursor()[c.start..].chars().count() {
        state.editor.replace_before_cursor(c.start, &prefix);
    }
    state.popup = Some(Popup { matches: c.matches, selected: 0, start: c.start });
}

/// ↑/↓ (and Tab) in the open list; wraps.
pub fn step(state: &mut State, delta: isize) {
    if let Some(p) = &mut state.popup {
        p.selected = (p.selected as isize + delta).rem_euclid(p.matches.len() as isize) as usize;
    }
}

/// Enter in the open list: puts the choice in the input and closes it.
pub fn accept(state: &mut State) {
    if let Some(p) = state.popup.take() {
        state.editor.replace_before_cursor(p.start, &format!("{} ", p.matches[p.selected]));
    }
}

/// After an edit with the list open: completes again, closing the list when
/// nothing matches.
pub fn refresh(state: &mut State) {
    if state.popup.is_some() {
        let found = complete(state.editor.before_cursor(), &Candidates::from_state(state));
        state.popup = found.map(|c| Popup { matches: c.matches, selected: 0, start: c.start });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::APP;
    use crate::ui::update::{Msg, update};
    use corvo_wan_p2p_core::identity::Seed;
    use corvo_wan_p2p_core::store::Person;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn cands() -> Candidates {
        Candidates {
            contacts: vec!["ana maria".into(), "rahul".into(), "rani".into()],
            requests: vec!["bob".into()],
            blocked: vec!["eve".into()],
        }
    }

    fn matches(before: &str) -> Vec<String> {
        complete(before, &cands()).map(|c| c.matches).unwrap_or_default()
    }

    #[test]
    fn commands_complete_as_the_first_word_only() {
        assert_eq!(matches("/bl"), vec!["/block", "/blocked"]);
        assert_eq!(
            complete("  /q", &cands()),
            Some(Completion { start: 2, matches: vec!["/q".into(), "/quit".into()] })
        );
        assert_eq!(complete("hi /bl", &cands()), None);
        assert_eq!(complete("//bl", &cands()), None);
        assert_eq!(complete("one\n/bl", &cands()), None);
        assert_eq!(complete("hello", &cands()), None);
        assert_eq!(complete("", &cands()), None);
    }

    #[test]
    fn names_complete_after_commands_that_take_one() {
        assert_eq!(
            complete("/chat RA", &cands()),
            Some(Completion { start: 6, matches: vec!["rahul".into(), "rani".into()] })
        );
        assert_eq!(matches("/chat ana m"), vec!["ana maria"], "names can have spaces");
        assert_eq!(matches("/chat "), vec!["ana maria", "bob", "rahul", "rani"], "contacts and requests");
        assert_eq!(matches("/accept "), vec!["bob"], "only requests");
        assert_eq!(matches("/unblock "), vec!["eve"], "only the blocked");
        assert_eq!(matches("/info "), vec!["ana maria", "bob", "eve", "rahul", "rani"]);
        assert_eq!(matches("/remove r"), vec!["rahul", "rani"]);
        assert_eq!(matches("/block b"), vec!["bob"]);
        assert_eq!(matches("/add r"), Vec::<String>::new(), "/add takes a code");
        assert_eq!(matches("/chat zed"), Vec::<String>::new());
    }

    #[test]
    fn common_prefix_is_char_wise() {
        assert_eq!(common_prefix(&["rahul·m4x8".into(), "rahul·pnoh".into()]), "rahul·");
        assert_eq!(common_prefix(&["/q".into()]), "/q");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn candidates_are_labels_by_standing() {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        let person = |name: &str, standing| Person {
            code: Seed::generate().code(),
            name: name.into(),
            last_seen: None,
            standing,
        };
        state.set_people(vec![
            person("rahul", Standing::Contact),
            person("rahul", Standing::Request),
            person("Bob", Standing::Contact),
            person("eve", Standing::Blocked),
        ]);
        let c = Candidates::from_state(&state);
        assert_eq!(c.contacts.len(), 2);
        assert_eq!(c.contacts[0], "Bob", "sorted ignoring case");
        assert!(c.contacts[1].starts_with("rahul·"), "look-alikes complete to their labels");
        assert!(c.requests[0].starts_with("rahul·"));
        assert_eq!(c.blocked, vec!["eve"]);
    }

    fn state() -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.title = APP.into();
        let person = |name: &str| Person {
            code: Seed::generate().code(),
            name: name.into(),
            last_seen: None,
            standing: Standing::Contact,
        };
        state.set_people(vec![person("rahul"), person("rani"), person("maria")]);
        state
    }

    fn press(state: &mut State, code: KeyCode) {
        update(state, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn typed(state: &mut State, text: &str) {
        for c in text.chars() {
            press(state, KeyCode::Char(c));
        }
    }

    fn popup(state: &State) -> Option<(Vec<String>, usize)> {
        state.popup.as_ref().map(|Popup { matches, selected, .. }| (matches.clone(), *selected))
    }

    #[test]
    fn tab_with_one_match_completes_it() {
        let mut s = state();
        typed(&mut s, "/cha");
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.editor.text(), "/chat ");
        typed(&mut s, "m");
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.editor.text(), "/chat maria ");
        assert_eq!(s.popup, None);
    }

    #[test]
    fn tab_with_several_fills_their_start_and_opens_the_list() {
        let mut s = state();
        typed(&mut s, "/chat r");
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.editor.text(), "/chat ra");
        assert_eq!(popup(&s), Some((vec!["rahul".into(), "rani".into()], 0)));
        press(&mut s, KeyCode::Tab);
        assert_eq!(popup(&s).unwrap().1, 1, "Tab moves down");
        press(&mut s, KeyCode::Down);
        assert_eq!(popup(&s).unwrap().1, 0, "and wraps");
        press(&mut s, KeyCode::Up);
        assert_eq!(popup(&s).unwrap().1, 1);
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.editor.text(), "/chat rani ", "Enter takes the choice, and sends nothing");
        assert_eq!(s.popup, None);
    }

    #[test]
    fn typing_narrows_the_list_and_other_keys_close_it() {
        let mut s = state();
        typed(&mut s, "/chat r");
        press(&mut s, KeyCode::Tab);
        typed(&mut s, "h");
        assert_eq!(popup(&s), Some((vec!["rahul".into()], 0)), "typing refreshes the list");
        typed(&mut s, "z");
        assert_eq!(s.popup, None, "nothing matches");
        press(&mut s, KeyCode::Backspace);
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.editor.text(), "/chat rahul ");

        s.editor.clear();
        typed(&mut s, "/chat r");
        press(&mut s, KeyCode::Tab);
        press(&mut s, KeyCode::Left);
        assert_eq!(s.popup, None, "moving the cursor closes it");
        press(&mut s, KeyCode::End);
        press(&mut s, KeyCode::Tab);
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.popup, None);
        assert_eq!(s.editor.text(), "/chat ra", "Esc only closes the list");
    }

    #[test]
    fn tab_does_nothing_in_a_question_or_with_nothing_to_complete() {
        let mut s = state();
        typed(&mut s, "hello");
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.editor.text(), "hello");
        s.editor.clear();
        typed(&mut s, "/passphrase");
        press(&mut s, KeyCode::Enter);
        typed(&mut s, "/q");
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.editor.text(), "/q", "a passphrase isn't completed");
    }
}
