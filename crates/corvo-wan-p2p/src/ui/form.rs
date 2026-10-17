//! Windows with fields, opened from the Profile tab: change your name, change
//! your passphrase, show your 12 words. What's typed goes into the window,
//! never the input box or its history, and a window's secrets are wiped when
//! it closes (`state.overlay = None` drops them).

use std::fmt;

use corvo_wan_p2p_core::identity::clean_name;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_width::UnicodeWidthStr;
use zeroize::Zeroizing;

use super::account::{PhraseDone, PhraseUse};
use super::people;
use super::state::{Line, Secret, State, Tone};
use super::update::{Effect, Outcome, one_line_answer};
use super::view::{BIGGER_FOR_WORDS, word_grid, wrap};
use super::window::Overlay;

/// The most a field holds, in bytes. Its buffer is this big from the start,
/// so typing never moves it and leaves a copy behind.
const MAX_FIELD: usize = 1024;

/// Where the words are saved, as the words window says.
const RECOVERY_FILE: &str = "~/corvo-wan-p2p-recovery.txt";

#[derive(Clone, PartialEq, Eq)]
pub struct Field {
    pub label: &'static str,
    pub value: Zeroizing<String>,
    /// Each character shows as `•`.
    pub masked: bool,
    /// Shown, not typed in.
    pub read_only: bool,
}

impl fmt::Debug for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Field").field("label", &self.label).field("masked", &self.masked).finish_non_exhaustive()
    }
}

impl Field {
    fn typed(label: &'static str, masked: bool) -> Field {
        Field { label, value: Zeroizing::new(String::with_capacity(MAX_FIELD)), masked, read_only: false }
    }

    fn shown(label: &'static str, value: &str) -> Field {
        Field { label, value: Zeroizing::new(value.to_string()), masked: false, read_only: true }
    }

    /// What was typed, moved out (not copied), leaving the field empty.
    fn take(&mut self) -> Secret {
        let empty = String::with_capacity(MAX_FIELD);
        Secret::new(std::mem::replace(&mut *self.value, empty))
    }

    fn push(&mut self, c: char) {
        if !self.read_only && self.value.len() + c.len_utf8() <= MAX_FIELD {
            self.value.push(c);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormKind {
    Name,
    Passphrase,
    /// Your passphrase first; then the words, which `S` saves with the
    /// passphrase kept from before.
    Words {
        passphrase: Option<Secret>,
        words: Option<Secret>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub kind: FormKind,
    pub fields: Vec<Field>,
    /// The field being typed in.
    pub focus: usize,
    /// What happened: a mistake, "changing…", or the result.
    pub note: Option<Line>,
    /// Waiting for the engine: keys do nothing but Esc.
    pub waiting: bool,
    /// The result is shown: any key closes it.
    pub done: bool,
}

impl Form {
    fn new(kind: FormKind, fields: Vec<Field>) -> Form {
        let focus = fields.iter().position(|f| !f.read_only).unwrap_or(0);
        Form { kind, fields, focus, note: None, waiting: false, done: false }
    }

    pub fn name(now: &str) -> Form {
        Form::new(FormKind::Name, vec![Field::shown("Now", now), Field::typed("New", false)])
    }

    pub fn passphrase() -> Form {
        let fields = vec![Field::typed("Current", true), Field::typed("New", true), Field::typed("Again", true)];
        Form::new(FormKind::Passphrase, fields)
    }

    pub fn words() -> Form {
        Form::new(FormKind::Words { passphrase: None, words: None }, vec![Field::typed("Passphrase", true)])
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            FormKind::Name => "─ Change name ",
            FormKind::Passphrase => "─ Change passphrase ",
            FormKind::Words { .. } => "─ Your 12 words ",
        }
    }

    /// The words, once your passphrase checked out.
    pub fn words_shown(&self) -> Option<&Secret> {
        match &self.kind {
            FormKind::Words { words, .. } => words.as_ref(),
            _ => None,
        }
    }

    /// The last row: what the keys do now.
    fn hint(&self) -> &'static str {
        match (&self.kind, self.done) {
            (_, true) => "any key closes",
            (FormKind::Name, _) => "Enter saves · Esc cancels",
            (FormKind::Passphrase, _) => "Tab next · Enter changes it · Esc cancels",
            (FormKind::Words { words: Some(_), .. }, _) => "S saves them · Esc closes",
            (FormKind::Words { .. }, _) => "Enter shows them · Esc cancels",
        }
    }

    fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.note = Some(Line { text: text.into(), tone });
    }

    fn step_focus(&mut self, delta: isize) {
        let editable: Vec<usize> = (0..self.fields.len()).filter(|&i| !self.fields[i].read_only).collect();
        if let Some(at) = editable.iter().position(|&i| i == self.focus) {
            let to = (at as isize + delta).rem_euclid(editable.len() as isize) as usize;
            self.focus = editable[to];
        }
    }

    /// Enter: checks what was typed and asks the engine to do it.
    fn submit(&mut self) -> Vec<Effect> {
        match &mut self.kind {
            FormKind::Name => match clean_name(&self.fields[1].value) {
                Ok(name) => {
                    self.waiting = true;
                    self.say(Tone::Dim, "saving…");
                    vec![Effect::SetName(name)]
                }
                Err(e) => {
                    self.say(Tone::Bad, e.to_string());
                    Vec::new()
                }
            },
            FormKind::Passphrase => {
                if self.fields.iter().any(|f| f.value.is_empty()) {
                    self.say(Tone::Bad, "type all three");
                    return Vec::new();
                }
                if self.fields[1].value != self.fields[2].value {
                    self.fields[2].take();
                    self.focus = 2;
                    self.say(Tone::Bad, "the new ones don't match; type it again");
                    return Vec::new();
                }
                let (old, new) = (self.fields[0].take(), self.fields[1].take());
                self.fields[2].take();
                self.waiting = true;
                self.say(Tone::Dim, "changing…");
                vec![Effect::ChangePassphrase { old, new }]
            }
            FormKind::Words { words: Some(_), .. } => Vec::new(),
            FormKind::Words { passphrase, .. } => {
                if self.fields[0].value.is_empty() {
                    self.say(Tone::Bad, "type your passphrase");
                    return Vec::new();
                }
                let typed = self.fields[0].take();
                *passphrase = Some(typed.clone());
                self.waiting = true;
                self.say(Tone::Dim, "checking…");
                vec![Effect::Phrase { passphrase: typed, then: PhraseUse::Show }]
            }
        }
    }

    /// `S` while the words show: saves them with the passphrase kept.
    fn save(&mut self) -> Vec<Effect> {
        let FormKind::Words { passphrase: Some(passphrase), words: Some(_) } = &self.kind else { return Vec::new() };
        let passphrase = passphrase.clone();
        self.waiting = true;
        self.say(Tone::Dim, "saving…");
        vec![Effect::Phrase { passphrase, then: PhraseUse::Save { overwrite: false } }]
    }
}

/// A key while a form is open. Ctrl-C never gets here: it quits.
pub fn key(state: &mut State, key: KeyEvent) -> Vec<Effect> {
    let Some(Overlay::Form(form)) = &mut state.overlay else { return Vec::new() };
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let words_shown = form.words_shown().is_some();
    match key.code {
        _ if form.done => state.overlay = None,
        KeyCode::Esc => state.overlay = None,
        _ if form.waiting => {}
        KeyCode::Char('s' | 'S') if words_shown && !ctrl && !alt => return form.save(),
        KeyCode::Enter if words_shown => state.overlay = None,
        _ if words_shown => {}
        KeyCode::Enter => return form.submit(),
        KeyCode::Tab | KeyCode::Down => form.step_focus(1),
        KeyCode::BackTab | KeyCode::Up => form.step_focus(-1),
        KeyCode::Backspace => {
            let field = &mut form.fields[form.focus];
            if !field.read_only {
                field.value.pop();
            }
        }
        // Windows reports AltGr chords, like @ on a German keyboard, as Ctrl+Alt.
        KeyCode::Char(c) if !(ctrl ^ alt) => form.fields[form.focus].push(c),
        _ => {}
    }
    Vec::new()
}

/// A paste while a form is open goes into the field being typed in, on one
/// line and without a line ending, as an answer to the unlock prompt would:
/// a passphrase pasted here is the one that unlocks. It's wiped after.
pub fn paste(state: &mut State, text: String) {
    let text = one_line_answer(text);
    let Some(Overlay::Form(form)) = &mut state.overlay else { return };
    if form.waiting || form.done || form.words_shown().is_some() {
        return;
    }
    let field = &mut form.fields[form.focus];
    for c in text.chars() {
        field.push(c);
    }
}

/// What came of a form's effect. It goes to the window waiting for it; if
/// that window was closed meanwhile, a name or passphrase change is still
/// said in the corvo tab, and the words are dropped.
pub fn done(state: &mut State, outcome: Outcome) -> Vec<Effect> {
    match outcome {
        Outcome::Named(result) => match (waiting(&mut state.overlay, |k| *k == FormKind::Name), result) {
            (Some(form), Err(why)) => {
                form.waiting = false;
                form.say(Tone::Bad, why);
            }
            (Some(_), Ok(name)) => {
                state.overlay = None;
                people::named(state, Ok(name));
            }
            (None, result) => people::named(state, result),
        },
        Outcome::PassphraseChanged(result) => match waiting(&mut state.overlay, |k| *k == FormKind::Passphrase) {
            Some(form) => {
                form.waiting = false;
                form.done = true;
                match result {
                    Ok(()) => form.say(Tone::Good, "done: use the new one from now on"),
                    Err(why) => form.say(Tone::Bad, format!("{why}; nothing changed")),
                }
            }
            None => match result {
                Ok(()) => state.say(Tone::Good, "passphrase changed: use the new one from now on"),
                Err(why) => state.say(Tone::Bad, format!("{why}; nothing changed")),
            },
        },
        Outcome::Phrase(result) => match waiting(&mut state.overlay, |k| matches!(k, FormKind::Words { .. })) {
            Some(form) => {
                form.waiting = false;
                match (&mut form.kind, result) {
                    (FormKind::Words { words, .. }, Ok(PhraseDone::Shown(shown))) => {
                        *words = Some(shown);
                        form.fields.clear();
                        form.note = None;
                    }
                    (_, Ok(PhraseDone::Shown(_))) => {}
                    (_, Ok(PhraseDone::Saved(path))) => form.say(Tone::Good, format!("saved in {}", path.display())),
                    (_, Ok(PhraseDone::Exists(path))) => {
                        form.say(Tone::Plain, format!("already saved in {}", path.display()))
                    }
                    // A wrong passphrase before the words show: type it again.
                    (FormKind::Words { passphrase, words: None }, Err(why)) => {
                        *passphrase = None;
                        form.say(Tone::Bad, why);
                    }
                    (_, Err(why)) => form.say(Tone::Bad, why),
                }
            }
            None => {
                if let Ok(PhraseDone::Saved(path)) = result {
                    state.say(Tone::Good, format!("your words are saved in {}", path.display()));
                }
                // Anything else, the words with it, is dropped here.
            }
        },
        _ => {}
    }
    Vec::new()
}

/// The open form, if it's waiting for a result and is of the `kind` asked.
fn waiting(overlay: &mut Option<Overlay>, kind: fn(&FormKind) -> bool) -> Option<&mut Form> {
    match overlay {
        Some(Overlay::Form(form)) if form.waiting && kind(&form.kind) => Some(form),
        _ => None,
    }
}

/// The form's rows in `inner` columns, at most `room` of them, each with
/// its tone. Wiped after use: they may hold your words.
///
/// The fields or the words come first, and the words are never cut off: if
/// they don't fit, it says to make the window bigger instead. Below them,
/// wrapped, what happened and the keys; a blank row and the warning go
/// first when there's no room for everything.
pub fn rows(form: &Form, inner: usize, room: usize) -> Vec<(Zeroizing<String>, Tone)> {
    let mut rows = Vec::new();
    if let Some(words) = form.words_shown() {
        let words: Vec<&str> = words.expose().split_whitespace().collect();
        rows.extend(word_grid(&words, u16::try_from(inner).unwrap_or(u16::MAX)).into_iter().map(|r| (r, Tone::Plain)));
        if rows.len() > room {
            return vec![(Zeroizing::new(BIGGER_FOR_WORDS.into()), Tone::Warn)];
        }
    } else {
        let width = form.fields.iter().map(|f| f.label.width()).max().unwrap_or(0);
        for (i, field) in form.fields.iter().enumerate() {
            rows.push((field_row(field, i == form.focus, width, inner), Tone::Plain));
        }
    }
    let wrapped = |text: &str, tone: Tone| -> Vec<(Zeroizing<String>, Tone)> {
        wrap(text, inner).into_iter().map(|r| (Zeroizing::new(r), tone)).collect()
    };
    let blank = vec![(Zeroizing::new(String::new()), Tone::Plain)];
    let warning = match form.words_shown() {
        Some(_) => wrapped(&format!("Never share these. S saves them to {RECOVERY_FILE}"), Tone::Warn),
        None => Vec::new(),
    };
    let note = form.note.as_ref().map_or_else(Vec::new, |n| wrapped(&n.text, n.tone));
    let hint = wrapped(form.hint(), Tone::Dim);
    // In the order they're dropped when there's no room.
    let mut extras = [blank, warning, hint, note];
    let mut drop = 0;
    while rows.len() + extras.iter().map(Vec::len).sum::<usize>() > room && drop < extras.len() {
        extras[drop].clear();
        drop += 1;
    }
    let [blank, warning, hint, note] = extras;
    rows.extend(blank.into_iter().chain(warning).chain(note).chain(hint));
    rows.truncate(room);
    rows
}

/// `› New   ana maria`: the field being typed in is marked, a masked one
/// shows `•`s, and a long value shows its end.
fn field_row(field: &Field, focused: bool, label_width: usize, inner: usize) -> Zeroizing<String> {
    let mut row = Zeroizing::new(String::with_capacity(inner * 4 + 8));
    row.push(if focused { '›' } else { ' ' });
    row.push(' ');
    row.push_str(field.label);
    for _ in field.label.width()..label_width + 2 {
        row.push(' ');
    }
    let room = inner.saturating_sub(row.width()).max(1);
    if field.masked {
        for _ in 0..field.value.chars().count().min(room) {
            row.push('•');
        }
    } else {
        let mut start = field.value.len();
        let mut used = 0;
        for (i, c) in field.value.char_indices().rev() {
            used += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if used > room {
                break;
            }
            start = i;
        }
        row.push_str(&field.value[start..]);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::update::{Msg, update};
    use std::path::PathBuf;

    fn state() -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.code = Some(corvo_wan_p2p_core::identity::Seed::generate().code());
        state.size = (80, 24);
        state
    }

    fn press(state: &mut State, code: KeyCode) -> Vec<Effect> {
        update(state, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn type_text(state: &mut State, text: &str) {
        for c in text.chars() {
            press(state, KeyCode::Char(c));
        }
    }

    fn done(state: &mut State, outcome: Outcome) -> Vec<Effect> {
        update(state, Msg::Done(outcome))
    }

    fn form(state: &State) -> &Form {
        match &state.overlay {
            Some(Overlay::Form(form)) => form,
            other => panic!("no form: {other:?}"),
        }
    }

    fn note(state: &State) -> String {
        form(state).note.as_ref().map(|n| n.text.clone()).unwrap_or_default()
    }

    fn shown(state: &State) -> Vec<String> {
        rows(form(state), 46, 100).iter().map(|(r, _)| r.to_string()).collect()
    }

    fn secret(text: &str) -> Secret {
        Secret::new(text.into())
    }

    /// The Profile tab with its window for `item` open.
    fn open(item: usize) -> State {
        let mut s = state();
        s.open_profile();
        for _ in 0..item {
            press(&mut s, KeyCode::Down);
        }
        press(&mut s, KeyCode::Enter);
        s
    }

    #[test]
    fn the_name_window_saves_on_enter() {
        let mut s = open(0);
        assert_eq!(shown(&s)[..2], ["  Now  ana".to_string(), "› New  ".into()]);
        type_text(&mut s, "Ana  Maria ");
        assert_eq!(press(&mut s, KeyCode::Enter), vec![Effect::SetName("Ana Maria".into())]);
        assert_eq!(note(&s), "saving…");
        done(&mut s, Outcome::Named(Ok("Ana Maria".into())));
        assert_eq!(s.overlay, None, "it closes");
        assert_eq!(s.name, "Ana Maria");
        assert!(s.tabs[0].lines.last().unwrap().text.contains("Ana Maria"), "and says so in the corvo tab");
        assert!(crate::ui::profile::lines(&s)[0].text.contains("Ana Maria"), "the tab shows it");

        let mut s = open(0);
        type_text(&mut s, "a\u{202E}b");
        assert!(press(&mut s, KeyCode::Enter).is_empty());
        assert!(note(&s).contains("can't"), "{}", note(&s));
    }

    #[test]
    fn passphrase_fields_are_masked_and_a_mismatch_is_noted() {
        let mut s = open(1);
        type_text(&mut s, "old-pw");
        press(&mut s, KeyCode::Tab);
        type_text(&mut s, "new-pw");
        press(&mut s, KeyCode::Tab);
        type_text(&mut s, "new-px");
        let rows = shown(&s);
        assert!(rows.iter().all(|r| !r.contains("pw") && !r.contains("px")), "{rows:?}");
        assert_eq!(rows[0], format!("  Current  {}", "•".repeat(6)));
        assert!(press(&mut s, KeyCode::Enter).is_empty());
        assert_eq!(note(&s), "the new ones don't match; type it again");
        assert_eq!(form(&s).focus, 2);
        type_text(&mut s, "new-pw");
        let effects = press(&mut s, KeyCode::Enter);
        assert_eq!(effects, vec![Effect::ChangePassphrase { old: secret("old-pw"), new: secret("new-pw") }]);
        assert_eq!(note(&s), "changing…");
        assert!(form(&s).fields.iter().all(|f| f.value.is_empty()), "nothing kept in the window");
        done(&mut s, Outcome::PassphraseChanged(Ok(())));
        assert_eq!(note(&s), "done: use the new one from now on");
        press(&mut s, KeyCode::Char('x'));
        assert_eq!(s.overlay, None, "any key closes it");

        let mut s = open(1);
        assert!(press(&mut s, KeyCode::Enter).is_empty());
        assert_eq!(note(&s), "type all three");
    }

    #[test]
    fn the_words_window_asks_then_shows_then_saves_with_s() {
        let mut s = open(2);
        assert!(press(&mut s, KeyCode::Char('S')).is_empty(), "S is a letter of the passphrase here");
        press(&mut s, KeyCode::Backspace);
        type_text(&mut s, "pw");
        assert_eq!(
            press(&mut s, KeyCode::Enter),
            vec![Effect::Phrase { passphrase: secret("pw"), then: PhraseUse::Show }]
        );
        done(&mut s, Outcome::Phrase(Err("wrong passphrase".into())));
        assert_eq!(note(&s), "wrong passphrase");
        assert!(form(&s).words_shown().is_none());
        type_text(&mut s, "pw");
        press(&mut s, KeyCode::Enter);
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Shown(secret("abandon ability able")))));
        let rows = shown(&s);
        assert!(rows[0].starts_with(" 1. abandon"), "{rows:?}");
        assert!(rows.join(" ").contains("Never share these. S saves them to ~/corvo-wan-p2p-recovery.txt"));
        let effects = press(&mut s, KeyCode::Char('s'));
        assert_eq!(
            effects,
            vec![Effect::Phrase { passphrase: secret("pw"), then: PhraseUse::Save { overwrite: false } }]
        );
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Saved(PathBuf::from("/h/corvo-wan-p2p-recovery.txt")))));
        assert_eq!(note(&s), "saved in /h/corvo-wan-p2p-recovery.txt");
        assert!(form(&s).words_shown().is_some(), "the words stay up");
    }

    #[test]
    fn already_saved_words_say_where() {
        let mut s = open(2);
        type_text(&mut s, "pw");
        press(&mut s, KeyCode::Enter);
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Shown(secret("abandon ability able")))));
        press(&mut s, KeyCode::Char('S'));
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Exists(PathBuf::from("/h/corvo-wan-p2p-recovery.txt")))));
        assert_eq!(note(&s), "already saved in /h/corvo-wan-p2p-recovery.txt");
        assert_eq!(s.ask(), None, "nothing asks to replace it");
    }

    #[test]
    fn closing_the_words_window_forgets_the_words() {
        for close in [KeyCode::Esc, KeyCode::Enter] {
            let mut s = open(2);
            type_text(&mut s, "pw");
            press(&mut s, KeyCode::Enter);
            done(&mut s, Outcome::Phrase(Ok(PhraseDone::Shown(secret("abandon ability able")))));
            press(&mut s, close);
            assert_eq!(s.overlay, None);
            assert!(s.on_profile(), "back on the Profile tab");
            let all: Vec<&str> = s.tabs.iter().flat_map(|t| t.lines.iter().map(|l| l.text.as_str())).collect();
            assert!(!all.iter().any(|l| l.contains("abandon")), "never drawn into a tab");
            // Arriving after the window closed, they go nowhere.
            done(&mut s, Outcome::Phrase(Ok(PhraseDone::Shown(secret("abandon ability able")))));
            assert_eq!(s.overlay, None);
        }
    }

    #[test]
    fn keys_in_a_form_never_reach_the_input_or_history() {
        let mut s = open(1);
        type_text(&mut s, "hunter2");
        update(&mut s, Msg::Paste("pasted\nsecret".into()));
        press(&mut s, KeyCode::Up);
        assert_eq!(s.editor.text(), "");
        assert_eq!(*form(&s).fields[0].value, "hunter2pasted secret");
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.overlay, None);
        assert_eq!(s.history.prev(""), None, "nothing in the history");
        assert!(!format!("{:?}", form_debug()).contains("hunter2"));
    }

    #[test]
    fn a_pasted_line_ending_is_not_part_of_the_passphrase() {
        for pasted in ["new-pw\n", "new-pw\r\n"] {
            let mut s = open(1);
            type_text(&mut s, "pw");
            press(&mut s, KeyCode::Tab);
            update(&mut s, Msg::Paste(pasted.into()));
            press(&mut s, KeyCode::Tab);
            update(&mut s, Msg::Paste(pasted.into()));
            let effects = press(&mut s, KeyCode::Enter);
            assert_eq!(
                effects,
                vec![Effect::ChangePassphrase { old: secret("pw"), new: secret("new-pw") }],
                "{pasted:?}"
            );
        }
    }

    #[test]
    fn a_late_result_does_not_land_in_a_window_opened_again() {
        let mut s = open(2);
        type_text(&mut s, "pw");
        press(&mut s, KeyCode::Enter);
        press(&mut s, KeyCode::Esc);
        press(&mut s, KeyCode::Enter);
        assert!(matches!(&s.overlay, Some(Overlay::Form(_))), "opened again");
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Shown(secret("abandon ability able")))));
        assert!(form(&s).words_shown().is_none(), "no words where no passphrase was typed");

        let mut s = open(1);
        done(&mut s, Outcome::PassphraseChanged(Ok(())));
        assert!(!form(&s).done, "a window being typed in isn't finished by an old result");
        assert!(s.tabs[0].lines.last().unwrap().text.starts_with("passphrase changed"), "said in the corvo tab");

        let mut s = open(0);
        done(&mut s, Outcome::Named(Ok("Old".into())));
        assert!(matches!(&s.overlay, Some(Overlay::Form(_))), "the new window stays");
        assert_eq!(s.name, "Old", "the name did change");
    }

    #[test]
    fn long_notes_wrap_inside_the_window() {
        let mut s = open(2);
        type_text(&mut s, "pw");
        press(&mut s, KeyCode::Enter);
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Shown(secret("abandon ability able")))));
        press(&mut s, KeyCode::Char('s'));
        let path = "/Users/someone-with-a-long-name/corvo-wan-p2p-recovery.txt";
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Exists(PathBuf::from(path)))));
        for inner in [34, 50] {
            let rows: Vec<String> = rows(form(&s), inner, 100).iter().map(|(r, _)| r.to_string()).collect();
            assert!(rows.iter().all(|r| r.width() <= inner), "{inner}: {rows:#?}");
            assert!(rows.concat().contains("corvo-wan-p2p-recovery.txt"), "the whole path shows: {rows:#?}");
        }
    }

    fn form_debug() -> Form {
        let mut form = Form::passphrase();
        "hunter2".chars().for_each(|c| form.fields[0].push(c));
        form
    }
}
