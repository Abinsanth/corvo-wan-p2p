//! Your account from the chat screen: `/passphrase` and `/phrase`. They ask
//! for passphrases in the input box, masked, and the loop does the work.

use std::path::PathBuf;

use super::commands::Cmd;
use super::state::{Ask, Secret, State, Tone};
use super::update::{Effect, Outcome};

/// What `/phrase` does with your words once your passphrase checks out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseUse {
    /// Show them until a key is pressed.
    Show,
    /// Write the recovery file, replacing one that's there only with `overwrite`.
    Save { overwrite: bool },
    /// Copy them to the clipboard.
    Copy,
}

/// What `/phrase` did.
#[derive(Debug)]
pub enum PhraseDone {
    Shown(Secret),
    Saved(PathBuf),
    Copied,
}

/// A command in the middle of asking its questions.
#[derive(Debug)]
pub enum Pending {
    /// `/passphrase`: the current one…
    Current,
    /// …the new one…
    New { old: Secret },
    /// …and the new one again.
    Repeat { old: Secret, new: Secret },
    /// `/phrase`: your passphrase, then `PhraseUse`.
    Passphrase(PhraseUse),
    /// `/phrase save`: finding out whether the recovery file is there.
    Checking { passphrase: Secret },
    /// `/phrase save`: it's there; replace it?
    Replace { passphrase: Secret },
}

impl Pending {
    pub fn ask(&self) -> Ask {
        let (label, masked) = match self {
            Pending::Current => ("current passphrase: ", true),
            Pending::New { .. } => ("new passphrase: ", true),
            Pending::Repeat { .. } => ("again: ", true),
            Pending::Passphrase(_) | Pending::Checking { .. } => ("your passphrase: ", true),
            Pending::Replace { .. } => ("replace it? y/n: ", false),
        };
        Ask { label: label.into(), masked }
    }
}

/// Starts `/passphrase` or one of the `/phrase` commands.
pub fn begin(state: &mut State, cmd: Cmd) {
    let (pending, why) = match cmd {
        Cmd::Passphrase => (Pending::Current, "changing your passphrase: type the current one"),
        Cmd::Phrase => (Pending::Passphrase(PhraseUse::Show), "type your passphrase to see your 12 words"),
        Cmd::PhraseSave => (
            Pending::Passphrase(PhraseUse::Save { overwrite: false }),
            "type your passphrase to save your 12 words to the recovery file",
        ),
        Cmd::PhraseCopy => (Pending::Passphrase(PhraseUse::Copy), "type your passphrase to copy your 12 words"),
        _ => return,
    };
    state.select_tab(0);
    state.say(Tone::Plain, format!("{why} (Esc cancels)"));
    state.pending = Some(pending);
}

/// An answer to the pending question.
pub fn answer(state: &mut State, text: Secret) -> Vec<Effect> {
    let Some(pending) = state.pending.take() else {
        return Vec::new();
    };
    let mut effects = Vec::new();
    let empty = text.expose().is_empty();
    state.pending = match pending {
        Pending::Current if empty => {
            state.say(Tone::Bad, "type your current passphrase, or Esc to cancel");
            Some(Pending::Current)
        }
        Pending::Current => Some(Pending::New { old: text }),
        Pending::New { old } if empty => {
            state.say(Tone::Bad, "the passphrase can't be empty");
            Some(Pending::New { old })
        }
        Pending::New { old } => Some(Pending::Repeat { old, new: text }),
        Pending::Repeat { new, .. } if text != new => {
            state.say(Tone::Bad, "those didn't match; nothing changed");
            None
        }
        Pending::Repeat { old, new } => {
            effects.push(Effect::ChangePassphrase { old, new });
            None
        }
        Pending::Passphrase(then) if empty => {
            state.say(Tone::Bad, "type your passphrase, or Esc to cancel");
            Some(Pending::Passphrase(then))
        }
        Pending::Passphrase(PhraseUse::Save { .. }) => {
            effects.push(Effect::CheckRecovery);
            Some(Pending::Checking { passphrase: text })
        }
        Pending::Passphrase(then) => {
            effects.push(Effect::Phrase { passphrase: text, then });
            None
        }
        Pending::Replace { passphrase } => match text.expose().trim().to_lowercase().as_str() {
            "y" | "yes" => {
                effects.push(Effect::Phrase { passphrase, then: PhraseUse::Save { overwrite: true } });
                None
            }
            "n" | "no" => {
                state.say(Tone::Plain, "kept the file that's there; nothing was saved");
                None
            }
            _ => {
                state.say(Tone::Bad, "type y or n");
                Some(Pending::Replace { passphrase })
            }
        },
        // Nothing is asked while waiting.
        checking @ Pending::Checking { .. } => Some(checking),
    };
    effects
}

/// Esc: stops asking.
pub fn cancel(state: &mut State) {
    if state.pending.take().is_some() {
        state.say(Tone::Plain, "cancelled");
    }
}

/// What came of an effect these commands asked for.
pub fn done(state: &mut State, outcome: Outcome) -> Vec<Effect> {
    let mut effects = Vec::new();
    match outcome {
        Outcome::PassphraseChanged(Ok(())) => state.say(Tone::Good, "passphrase changed: use the new one from now on"),
        Outcome::PassphraseChanged(Err(why)) => state.say(Tone::Bad, format!("{why}; nothing changed")),
        Outcome::Phrase(Ok(PhraseDone::Shown(words))) => state.reveal = Some(words),
        Outcome::Phrase(Ok(PhraseDone::Copied)) => state.say(
            Tone::Good,
            "copied your 12 words: paste them into a password manager, then copy something else over them",
        ),
        Outcome::Phrase(Ok(PhraseDone::Saved(path))) => state.say(
            Tone::Good,
            format!(
                "your words are saved in {}, readable only by you. Copy them somewhere safe, then delete the file.",
                path.display()
            ),
        ),
        Outcome::Phrase(Err(why)) => state.say(Tone::Bad, why),
        Outcome::RecoveryFile(found) => {
            if let Some(Pending::Checking { passphrase }) = state.pending.take() {
                match found {
                    Some(path) => {
                        state.say(Tone::Plain, format!("there's already a recovery file at {}", path.display()));
                        state.pending = Some(Pending::Replace { passphrase });
                    }
                    None => effects.push(Effect::Phrase { passphrase, then: PhraseUse::Save { overwrite: false } }),
                }
            }
        }
        _ => {}
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::{Line, Tone};
    use crate::ui::update::{Msg, update};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn state() -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.title = crate::ui::state::APP.into();
        state
    }

    fn press(state: &mut State, code: KeyCode) -> Vec<Effect> {
        update(state, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn send(state: &mut State, text: &str) -> Vec<Effect> {
        for c in text.chars() {
            press(state, KeyCode::Char(c));
        }
        press(state, KeyCode::Enter)
    }

    fn done(state: &mut State, outcome: Outcome) -> Vec<Effect> {
        update(state, Msg::Done(outcome))
    }

    fn last(state: &State) -> &Line {
        state.tabs[0].lines.last().expect("the corvo tab has lines")
    }

    fn label(state: &State) -> String {
        state.ask().expect("a question").label
    }

    fn secret(text: &str) -> Secret {
        Secret::new(text.into())
    }

    #[test]
    fn changing_the_passphrase_asks_three_times_masked() {
        let mut s = state();
        assert!(send(&mut s, "/passphrase").is_empty());
        assert_eq!(label(&s), "current passphrase: ");
        assert!(s.ask().unwrap().masked);
        send(&mut s, "old one");
        assert_eq!(label(&s), "new passphrase: ");
        assert!(s.ask().unwrap().masked);
        send(&mut s, "new one");
        assert_eq!(label(&s), "again: ");
        assert!(s.ask().unwrap().masked);
        assert_eq!(
            send(&mut s, "new one"),
            vec![Effect::ChangePassphrase { old: secret("old one"), new: secret("new one") }]
        );
        assert_eq!(s.busy, Some("changing your passphrase…"));
        assert_eq!(s.ask(), None);
        assert!(done(&mut s, Outcome::PassphraseChanged(Ok(()))).is_empty());
        assert_eq!(s.busy, None);
        assert_eq!(last(&s).tone, Tone::Good);
        assert!(last(&s).text.contains("passphrase changed"));
    }

    #[test]
    fn a_wrong_current_passphrase_changes_nothing() {
        let mut s = state();
        send(&mut s, "/passphrase");
        send(&mut s, "wrong");
        send(&mut s, "new");
        send(&mut s, "new");
        done(&mut s, Outcome::PassphraseChanged(Err("wrong passphrase".into())));
        assert_eq!(*last(&s), Line { text: "wrong passphrase; nothing changed".into(), tone: Tone::Bad });
    }

    #[test]
    fn mismatched_new_passphrases_are_refused() {
        let mut s = state();
        send(&mut s, "/passphrase");
        send(&mut s, "old");
        send(&mut s, "one");
        assert!(send(&mut s, "two").is_empty());
        assert_eq!(s.ask(), None);
        assert_eq!(*last(&s), Line { text: "those didn't match; nothing changed".into(), tone: Tone::Bad });
    }

    #[test]
    fn an_empty_new_passphrase_is_asked_again() {
        let mut s = state();
        send(&mut s, "/passphrase");
        send(&mut s, "old");
        assert!(send(&mut s, "").is_empty());
        assert_eq!(label(&s), "new passphrase: ");
        assert_eq!(last(&s).text, "the passphrase can't be empty");
    }

    #[test]
    fn esc_cancels_the_questions() {
        let mut s = state();
        send(&mut s, "/phrase");
        press(&mut s, KeyCode::Char('x'));
        press(&mut s, KeyCode::Esc);
        assert!(s.ask().is_some(), "the first Esc clears what's typed");
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.ask(), None);
        assert_eq!(last(&s).text, "cancelled");
    }

    #[test]
    fn phrase_asks_the_passphrase_and_shows_the_words_until_a_key() {
        let mut s = state();
        send(&mut s, "/phrase");
        assert_eq!(label(&s), "your passphrase: ");
        assert!(s.ask().unwrap().masked);
        assert_eq!(send(&mut s, "pw"), vec![Effect::Phrase { passphrase: secret("pw"), then: PhraseUse::Show }]);
        assert_eq!(s.busy, Some("checking your passphrase…"));
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Shown(secret("abandon ability able")))));
        assert_eq!(s.reveal, Some(secret("abandon ability able")));
        let lines = s.tabs[0].lines.len();
        press(&mut s, KeyCode::Char('x'));
        assert_eq!(s.reveal, None, "gone after a key");
        assert!(s.editor.is_empty(), "the key only hides them");
        assert_eq!(s.tabs[0].lines.len(), lines, "the words were never a line");
    }

    #[test]
    fn phrase_with_a_wrong_passphrase_shows_nothing() {
        let mut s = state();
        send(&mut s, "/phrase");
        send(&mut s, "nope");
        done(&mut s, Outcome::Phrase(Err("wrong passphrase".into())));
        assert_eq!(s.reveal, None);
        assert_eq!(*last(&s), Line { text: "wrong passphrase".into(), tone: Tone::Bad });
    }

    #[test]
    fn phrase_copy_copies_after_the_passphrase() {
        let mut s = state();
        send(&mut s, "/phrase copy");
        assert_eq!(send(&mut s, "pw"), vec![Effect::Phrase { passphrase: secret("pw"), then: PhraseUse::Copy }]);
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Copied)));
        assert_eq!(last(&s).tone, Tone::Good);
        assert!(last(&s).text.contains("copied"));
    }

    #[test]
    fn phrase_save_writes_the_file_when_there_is_none() {
        let mut s = state();
        send(&mut s, "/phrase save");
        assert_eq!(send(&mut s, "pw"), vec![Effect::CheckRecovery]);
        let effects = done(&mut s, Outcome::RecoveryFile(None));
        assert_eq!(
            effects,
            vec![Effect::Phrase { passphrase: secret("pw"), then: PhraseUse::Save { overwrite: false } }]
        );
        assert_eq!(s.ask(), None);
        done(&mut s, Outcome::Phrase(Ok(PhraseDone::Saved(PathBuf::from("/h/corvo-wan-p2p-recovery.txt")))));
        assert_eq!(last(&s).tone, Tone::Good);
        assert!(last(&s).text.contains("/h/corvo-wan-p2p-recovery.txt"));
    }

    #[test]
    fn phrase_save_asks_before_replacing_the_file() {
        let mut s = state();
        let path = PathBuf::from("/h/corvo-wan-p2p-recovery.txt");
        send(&mut s, "/phrase save");
        send(&mut s, "pw");
        assert!(done(&mut s, Outcome::RecoveryFile(Some(path.clone()))).is_empty());
        assert_eq!(label(&s), "replace it? y/n: ");
        assert!(last(&s).text.contains("/h/corvo-wan-p2p-recovery.txt"));
        assert!(send(&mut s, "maybe").is_empty());
        assert_eq!(label(&s), "replace it? y/n: ");
        assert!(send(&mut s, "n").is_empty());
        assert_eq!(s.ask(), None);
        assert_eq!(last(&s).text, "kept the file that's there; nothing was saved");

        send(&mut s, "/phrase save");
        send(&mut s, "pw");
        done(&mut s, Outcome::RecoveryFile(Some(path)));
        let effects = send(&mut s, "y");
        assert_eq!(
            effects,
            vec![Effect::Phrase { passphrase: secret("pw"), then: PhraseUse::Save { overwrite: true } }]
        );
    }

    #[test]
    fn answers_are_not_kept_in_the_input_history() {
        let mut s = state();
        send(&mut s, "/passphrase");
        send(&mut s, "secret-old");
        send(&mut s, "secret-new");
        send(&mut s, "secret-new");
        send(&mut s, "/phrase");
        send(&mut s, "secret-pw");
        let mut seen = Vec::new();
        while let Some(entry) = s.history.prev("") {
            seen.push(entry);
        }
        assert_eq!(seen, vec!["/phrase", "/passphrase"]);
    }

    #[test]
    fn a_paste_while_the_words_are_up_only_hides_them() {
        let mut s = state();
        s.reveal = Some(secret("abandon ability able"));
        update(&mut s, Msg::Paste("hello".into()));
        assert_eq!(s.reveal, None);
        assert!(s.editor.is_empty());
    }
}
