//! Questions asked in the input box (`/remove`, `/update`, `/code`), and
//! `/update` itself. Your words go through the Profile tab's window (see
//! `form`), using `PhraseUse` and `PhraseDone` from here.

use std::path::PathBuf;

use corvo_wan_p2p_core::identity::Code;

use super::people::Act;
use super::state::{Ask, Secret, State, Tone};
use super::update::{Effect, Outcome};
use super::window::{Overlay, UpdateView};
use crate::selfupdate::{self, Stage};

/// What to do with your words once your passphrase checks out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseUse {
    /// Show them in the words window.
    Show,
    /// Write the recovery file, replacing one that's there only with `overwrite`.
    Save { overwrite: bool },
}

/// What was done with your words.
#[derive(Debug)]
pub enum PhraseDone {
    Shown(Secret),
    Saved(PathBuf),
    /// The passphrase was right, but the recovery file is there and wasn't
    /// to be replaced; nothing was written.
    Exists(PathBuf),
}

/// A command in the middle of asking its questions.
#[derive(Debug)]
pub enum Pending {
    /// `/remove`: delete the history with them too?
    Remove { code: Code },
    /// `/update`: install the newer `version` found?
    Update { version: String },
    /// `/code`: save it to the code file?
    SaveCode,
}

impl Pending {
    pub fn ask(&self) -> Ask {
        let (label, masked) = match self {
            Pending::Remove { .. } => ("delete your history too? y/n: ", false),
            Pending::Update { .. } => ("update now? y/n: ", false),
            Pending::SaveCode => ("save it to ~/corvo-wan-p2p-code.txt? y/n: ", false),
        };
        Ask { label: label.into(), masked }
    }
}

/// An answer to the pending question.
pub fn answer(state: &mut State, text: Secret) -> Vec<Effect> {
    let Some(pending) = state.pending.take() else {
        return Vec::new();
    };
    let mut effects = Vec::new();
    state.pending = match pending {
        Pending::Remove { code } => match text.expose().trim().to_lowercase().as_str() {
            "y" | "yes" => {
                effects.push(Effect::Act { code, act: Act::Remove { history: true } });
                None
            }
            "n" | "no" => {
                effects.push(Effect::Act { code, act: Act::Remove { history: false } });
                None
            }
            _ => {
                state.say(Tone::Bad, "type y or n");
                Some(Pending::Remove { code })
            }
        },
        Pending::Update { version } => match text.expose().trim().to_lowercase().as_str() {
            "y" | "yes" => {
                install(state, version, &mut effects);
                None
            }
            "n" | "no" => {
                state.say(Tone::Plain, "not updating; /update asks again");
                None
            }
            _ => {
                state.say(Tone::Bad, "type y or n");
                Some(Pending::Update { version })
            }
        },
        Pending::SaveCode => match text.expose().trim().to_lowercase().as_str() {
            "y" | "yes" => {
                effects.push(Effect::WriteCode);
                None
            }
            "n" | "no" => {
                state.say(Tone::Plain, "not saved; /code asks again");
                None
            }
            _ => {
                state.say(Tone::Bad, "type y or n");
                Some(Pending::SaveCode)
            }
        },
    };
    effects
}

/// Starts installing `version`: its window shows the stages as they go.
fn install(state: &mut State, version: String, effects: &mut Vec<Effect>) {
    state.overlay =
        Some(Overlay::Update(UpdateView { version: version.clone(), stage: Stage::Fetch, failed: None, scroll: 0 }));
    state.installing = Some(version);
    effects.push(Effect::Install(Stage::Fetch));
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
        Outcome::UpdateChecked { result: Ok(None), .. } => {
            state.say(Tone::Plain, format!("you're up to date ({})", selfupdate::current()))
        }
        Outcome::UpdateChecked { yes: true, result: Ok(Some(version)) } => install(state, version, &mut effects),
        Outcome::UpdateChecked { yes: false, result: Ok(Some(version)) } => {
            state.select_tab(0);
            state.say(
                Tone::Plain,
                format!("v{version} is available (you have {}). Update now? (Esc cancels)", selfupdate::current()),
            );
            state.pending = Some(Pending::Update { version });
        }
        Outcome::UpdateChecked { result: Err(why), .. } => state.say(Tone::Bad, why),
        Outcome::InstallStep { stage, result: Ok(()) } => match stage.next() {
            Some(next) => {
                if let Some(Overlay::Update(view)) = &mut state.overlay {
                    view.stage = next;
                }
                effects.push(Effect::Install(next));
            }
            None => {
                if matches!(state.overlay, Some(Overlay::Update(_))) {
                    state.overlay = None;
                }
                let version = state.installing.take().unwrap_or_default();
                state.say(Tone::Good, format!("updated to v{version}: /quit and start corvo-wan-p2p again to use it"));
            }
        },
        Outcome::InstallStep { result: Err(why), .. } => {
            state.installing = None;
            if let Some(Overlay::Update(view)) = &mut state.overlay {
                view.failed = Some(why.clone());
            }
            state.say(Tone::Bad, why);
        }
        Outcome::UpdatedAlready(version) => state.say(
            Tone::Plain,
            format!("v{version} is installed already: /quit and start corvo-wan-p2p again to use it"),
        ),
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

    /// `/update` asking whether to install 9.0.0.
    fn asking() -> State {
        let mut s = state();
        send(&mut s, "/update");
        done(&mut s, Outcome::UpdateChecked { yes: false, result: Ok(Some("9.0.0".into())) });
        s
    }

    #[test]
    fn esc_cancels_the_questions() {
        let mut s = asking();
        press(&mut s, KeyCode::Char('x'));
        press(&mut s, KeyCode::Esc);
        assert!(s.ask().is_some(), "the first Esc clears what's typed");
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.ask(), None);
        assert_eq!(last(&s).text, "cancelled");
    }

    #[test]
    fn answers_are_not_kept_in_the_input_history() {
        let mut s = asking();
        send(&mut s, "maybe");
        send(&mut s, "n");
        let mut seen = Vec::new();
        while let Some(entry) = s.history.prev("") {
            seen.push(entry);
        }
        assert_eq!(seen, vec!["/update"]);
    }

    fn current() -> String {
        crate::selfupdate::current().to_string()
    }

    #[test]
    fn update_says_when_you_are_up_to_date() {
        let mut s = state();
        assert_eq!(send(&mut s, "/update"), vec![Effect::CheckUpdate { yes: false }]);
        assert_eq!(s.busy, Some("checking for updates…"));
        done(&mut s, Outcome::UpdateChecked { yes: false, result: Ok(None) });
        assert_eq!(last(&s).text, format!("you're up to date ({})", current()));
        assert_eq!(s.ask(), None);
    }

    #[test]
    fn update_asks_before_installing_a_newer_version() {
        let mut s = state();
        send(&mut s, "/update");
        assert!(done(&mut s, Outcome::UpdateChecked { yes: false, result: Ok(Some("9.0.0".into())) }).is_empty());
        assert!(last(&s).text.starts_with(&format!("v9.0.0 is available (you have {})", current())));
        assert_eq!(label(&s), "update now? y/n: ");
        assert!(!s.ask().unwrap().masked);
        assert!(send(&mut s, "maybe").is_empty());
        assert_eq!(label(&s), "update now? y/n: ");
        assert!(send(&mut s, "n").is_empty());
        assert_eq!(s.ask(), None);
        assert_eq!(last(&s).text, "not updating; /update asks again");

        send(&mut s, "/update");
        done(&mut s, Outcome::UpdateChecked { yes: false, result: Ok(Some("9.0.0".into())) });
        assert_eq!(send(&mut s, "y"), vec![Effect::Install(Stage::Fetch)]);
        assert_eq!(s.busy, Some("updating…"));
        assert!(matches!(&s.overlay, Some(Overlay::Update(v)) if v.version == "9.0.0" && v.stage == Stage::Fetch));
    }

    /// An update to 9.0.0 under way, its window open.
    fn installing() -> State {
        let mut s = state();
        send(&mut s, "/update yes");
        done(&mut s, Outcome::UpdateChecked { yes: true, result: Ok(Some("9.0.0".into())) });
        s
    }

    fn step(stage: Stage, result: Result<(), String>) -> Outcome {
        Outcome::InstallStep { stage, result }
    }

    #[test]
    fn the_update_window_walks_fetch_check_swap() {
        let mut s = installing();
        assert_eq!(done(&mut s, step(Stage::Fetch, Ok(()))), vec![Effect::Install(Stage::Check)]);
        assert!(matches!(&s.overlay, Some(Overlay::Update(v)) if v.stage == Stage::Check));
        assert_eq!(done(&mut s, step(Stage::Check, Ok(()))), vec![Effect::Install(Stage::Swap)]);
        assert!(matches!(&s.overlay, Some(Overlay::Update(v)) if v.stage == Stage::Swap));
        assert!(done(&mut s, step(Stage::Swap, Ok(()))).is_empty());
        assert_eq!(s.overlay, None, "it closes itself");
        assert_eq!(
            *last(&s),
            Line { text: "updated to v9.0.0: /quit and start corvo-wan-p2p again to use it".into(), tone: Tone::Good }
        );
        assert_eq!(s.installing, None);
    }

    #[test]
    fn a_hidden_update_goes_on_and_says_when_it_is_done() {
        let mut s = installing();
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.overlay, None, "Esc hides it");
        assert_eq!(done(&mut s, step(Stage::Fetch, Ok(()))), vec![Effect::Install(Stage::Check)]);
        send(&mut s, "/update");
        assert_eq!(last(&s).text, "already updating: wait for it to finish");
        done(&mut s, step(Stage::Check, Ok(())));
        done(&mut s, step(Stage::Swap, Ok(())));
        assert!(last(&s).text.starts_with("updated to v9.0.0"));
    }

    #[test]
    fn a_failed_step_shows_in_the_window_then_in_the_corvo_tab() {
        let mut s = installing();
        done(&mut s, step(Stage::Fetch, Ok(())));
        let why = "the download didn't match its checksum; nothing changed";
        assert!(done(&mut s, step(Stage::Check, Err(why.into()))).is_empty(), "no further stage");
        assert!(matches!(&s.overlay, Some(Overlay::Update(v)) if v.failed.as_deref() == Some(why)));
        assert_eq!(*last(&s), Line { text: why.into(), tone: Tone::Bad });
        assert_eq!(s.installing, None, "/update can try again");
        press(&mut s, KeyCode::Char('x'));
        assert_eq!(s.overlay, None, "any key closes it");
    }

    #[test]
    fn update_yes_installs_without_asking() {
        let mut s = state();
        assert_eq!(send(&mut s, "/update yes"), vec![Effect::CheckUpdate { yes: true }]);
        let effects = done(&mut s, Outcome::UpdateChecked { yes: true, result: Ok(Some("9.0.0".into())) });
        assert_eq!(effects, vec![Effect::Install(Stage::Fetch)]);
        assert_eq!(s.ask(), None);
    }

    #[test]
    fn update_failures_say_why() {
        let mut s = state();
        done(&mut s, Outcome::UpdateChecked { yes: false, result: Err("no connection to GitHub".into()) });
        assert_eq!(*last(&s), Line { text: "no connection to GitHub".into(), tone: Tone::Bad });
        done(&mut s, Outcome::UpdateChecked { yes: false, result: Err(selfupdate::FROM_SOURCE.into()) });
        assert_eq!(last(&s).tone, Tone::Bad);
        assert!(last(&s).text.contains("built from source"));
    }
}
