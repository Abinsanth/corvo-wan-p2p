//! The screens before the chat: unlocking your account, or making a new one or
//! recovering yours. Each answer typed moves it on a step. The store work is
//! done by the loop as [`Effect`]s, and what came of it comes back to [`Onboarding::done`].

use std::path::PathBuf;

use corvo_wan_p2p_core::identity::{Code, PHRASE_WORDS, Seed, clean_name};
use corvo_wan_p2p_core::store::StoreError;

use zeroize::Zeroizing;

use super::clip::{MAYBE, NO_COPY};
use super::state::{Ask, Editor, Line, Secret, Tone};
use super::update::{Effect, Outcome};

pub struct Onboarding {
    step: Step,
    /// A new or recovered account was made (not just unlocked).
    made: bool,
    /// What happened since the last answer: mistakes, and what was done.
    pub notes: Vec<Line>,
}

/// Where onboarding is, and what it has gathered so far.
enum Step {
    /// There's an account: its passphrase.
    Unlock,
    /// `--recover` with an account here: type `yes` to delete it.
    ConfirmWipe,
    /// No account: (n)ew or (r)ecover.
    Choose,
    /// Recover: the 12 words.
    Words,
    Name {
        seed: Seed,
        recovered: bool,
    },
    Passphrase {
        seed: Seed,
        name: String,
        recovered: bool,
    },
    Repeat {
        seed: Seed,
        name: String,
        recovered: bool,
        first: Secret,
    },
    /// New: the words, to write down.
    ShowWords {
        seed: Seed,
        name: String,
        passphrase: Secret,
    },
    /// New: type two of them back. `asked` counts from 0; `done` of them are typed.
    TypeBack {
        seed: Seed,
        name: String,
        passphrase: Secret,
        asked: [usize; 2],
        done: usize,
    },
    /// Waiting while the account is made and, for a new one, the recovery file saved.
    Creating {
        seed: Seed,
        recovered: bool,
    },
    /// The recovery file is there already: replace it?
    Replace {
        seed: Seed,
        path: PathBuf,
    },
    /// Your code, then Enter to start.
    Code {
        code: Code,
        recovered: bool,
    },
    /// Waiting for the engine.
    Starting,
}

impl Onboarding {
    /// `account`: there's an account on this computer. `recover`: `--recover` was given.
    pub fn start(account: bool, recover: bool) -> Onboarding {
        let step = match (account, recover) {
            (true, false) => Step::Unlock,
            (true, true) => Step::ConfirmWipe,
            (false, true) => Step::Words,
            (false, false) => Step::Choose,
        };
        Onboarding { step, made: false, notes: Vec::new() }
    }

    /// Whether this ends with a new or recovered account, rather than an unlock.
    pub fn made_account(&self) -> bool {
        self.made
    }

    pub fn ask(&self) -> Ask {
        let (label, masked) = match &self.step {
            Step::Unlock | Step::Passphrase { .. } => ("passphrase: ", true),
            Step::Repeat { .. } => ("again: ", true),
            Step::ConfirmWipe => ("type yes: ", false),
            Step::Choose => ("n or r: ", false),
            Step::Words => ("words: ", false),
            Step::Name { .. } => ("name: ", false),
            Step::ShowWords { .. } | Step::Code { .. } => ("press Enter ", false),
            Step::TypeBack { asked, done, .. } => {
                return Ask { label: format!("word {}: ", asked[*done] + 1), masked: false };
            }
            Step::Replace { .. } => ("replace it? y/n: ", false),
            Step::Creating { .. } | Step::Starting => ("", false),
        };
        Ask { label: label.into(), masked }
    }

    /// At the code step, for tests.
    #[cfg(test)]
    pub fn at_code(code: Code, recovered: bool) -> Onboarding {
        Onboarding { step: Step::Code { code, recovered }, made: false, notes: Vec::new() }
    }

    /// What this step says above the input; `can_copy` as in `State`.
    pub fn guidance(&self, can_copy: bool) -> Vec<String> {
        let lines: &[&str] = match &self.step {
            Step::Unlock => &["Type your passphrase to unlock your account."],
            Step::ConfirmWipe => &[
                "--recover deletes the account on this computer, with its contacts and history.",
                "Your 12 recovery words bring back the same code, with no history.",
                "Type yes to delete it and go on. Anything else stops here.",
            ],
            Step::Choose => &[
                "Welcome to corvo-wan-p2p. There's no account on this computer yet.",
                "Type n to make a new one, or r to recover yours with your 12 words.",
            ],
            Step::Words => &["Type your 12 recovery words, in order, with spaces between them."],
            Step::Name { .. } => &["Choose a display name. Friends see it, and /name changes it later."],
            Step::Passphrase { .. } => &[
                "Choose a passphrase. It locks your history on this computer,",
                "and you type it each time corvo-wan-p2p starts.",
            ],
            Step::Repeat { .. } => &["Type the passphrase again."],
            Step::ShowWords { .. } => &[
                "These 12 words are your identity: anyone who has them can pretend to be you.",
                "They're also the only way back if you lose this computer or forget your passphrase.",
                "Write them down, then press Enter.",
            ],
            Step::TypeBack { asked, done, .. } => {
                return vec![format!("To check you have them, type word {}.", asked[*done] + 1)];
            }
            Step::Replace { path, .. } => {
                return vec![
                    format!("There's already a recovery file at {}, maybe from another account.", path.display()),
                    "Replace it with this account's words? Type y or n.".into(),
                ];
            }
            Step::Code { code, recovered } => {
                let then = if *recovered {
                    "It's the same as before. Friends who message you show up as requests: /accept them."
                } else {
                    "Send it to friends any way you like: they add you with /add and your code."
                };
                let copy = if can_copy {
                    format!("Your code, sent to your clipboard {MAYBE}:")
                } else {
                    format!("Your code ({NO_COPY}: select it to copy it):")
                };
                return vec![copy, code.to_string(), then.into(), "Press Enter to start.".into()];
            }
            Step::Creating { .. } | Step::Starting => &[],
        };
        lines.iter().map(|l| l.to_string()).collect()
    }

    /// The 12 words, only while they're shown to be written down.
    pub fn words(&self) -> Option<Vec<&'static str>> {
        match &self.step {
            Step::ShowWords { seed, .. } => Some(seed.words()),
            _ => None,
        }
    }

    /// Which word (counting from 1) the person is asked to type back.
    #[cfg(test)]
    pub fn asked_word(&self) -> Option<usize> {
        match &self.step {
            Step::TypeBack { asked, done, .. } => Some(asked[*done] + 1),
            _ => None,
        }
    }

    fn note(&mut self, tone: Tone, text: impl Into<String>) {
        self.notes.push(Line { text: text.into(), tone });
    }

    /// An answer typed and sent with Enter. `editor` gets back what needs fixing,
    /// so a typo in the 12 words doesn't mean typing them all again.
    pub fn answer(&mut self, text: Secret, editor: &mut Editor) -> Vec<Effect> {
        self.notes.clear();
        let mut effects = Vec::new();
        // Only answers that are compared: never a passphrase.
        let compared = matches!(
            self.step,
            Step::ConfirmWipe | Step::Choose | Step::Words | Step::TypeBack { .. } | Step::Replace { .. }
        );
        let typed = if compared { lowered(&text) } else { Zeroizing::default() };
        let typed = typed.as_str();
        let step = std::mem::replace(&mut self.step, Step::Starting);
        self.step = match step {
            Step::Unlock if text.expose().is_empty() => {
                self.note(Tone::Bad, "type your passphrase");
                Step::Unlock
            }
            Step::Unlock => {
                effects.push(Effect::Unlock(text));
                Step::Unlock
            }
            Step::ConfirmWipe if typed == "yes" => {
                effects.push(Effect::Wipe);
                Step::ConfirmWipe
            }
            Step::ConfirmWipe => {
                effects.push(Effect::Exit("nothing was deleted.".into()));
                Step::ConfirmWipe
            }
            Step::Choose => match typed {
                "n" | "new" => Step::Name { seed: Seed::generate(), recovered: false },
                "r" | "recover" => Step::Words,
                _ => {
                    self.note(Tone::Bad, "type n for a new account, or r to recover yours");
                    Step::Choose
                }
            },
            Step::Words => match Seed::from_phrase(typed) {
                Ok(seed) => Step::Name { seed, recovered: true },
                Err(e) => {
                    self.note(Tone::Bad, e.to_string());
                    editor.set(text.expose().to_string());
                    Step::Words
                }
            },
            Step::Name { seed, recovered } => match clean_name(text.expose()) {
                Ok(name) => Step::Passphrase { seed, name, recovered },
                Err(e) => {
                    self.note(Tone::Bad, e.to_string());
                    Step::Name { seed, recovered }
                }
            },
            Step::Passphrase { seed, name, recovered } if text.expose().is_empty() => {
                self.note(Tone::Bad, "the passphrase can't be empty");
                Step::Passphrase { seed, name, recovered }
            }
            Step::Passphrase { seed, name, recovered } => Step::Repeat { seed, name, recovered, first: text },
            Step::Repeat { seed, name, recovered, first } if text != first => {
                self.note(Tone::Bad, "those didn't match; choose your passphrase again");
                Step::Passphrase { seed, name, recovered }
            }
            Step::Repeat { seed, name, recovered: true, first } => {
                effects.push(Effect::Create { seed: seed.clone(), name, passphrase: first });
                Step::Creating { seed, recovered: true }
            }
            Step::Repeat { seed, name, recovered: false, first } => Step::ShowWords { seed, name, passphrase: first },
            Step::ShowWords { seed, name, passphrase } => {
                Step::TypeBack { seed, name, passphrase, asked: pick_two(), done: 0 }
            }
            Step::TypeBack { seed, name, passphrase, asked, done } => {
                let n = asked[done];
                if typed != seed.words()[n] {
                    self.note(Tone::Bad, format!("that isn't word {}. Look at the words again.", n + 1));
                    Step::ShowWords { seed, name, passphrase }
                } else if done + 1 < asked.len() {
                    Step::TypeBack { seed, name, passphrase, asked, done: done + 1 }
                } else {
                    effects.push(Effect::Create { seed: seed.clone(), name, passphrase });
                    Step::Creating { seed, recovered: false }
                }
            }
            Step::Replace { seed, path } => match typed {
                "y" | "yes" => {
                    effects.push(Effect::WriteRecovery { seed: seed.clone(), overwrite: true });
                    Step::Creating { seed, recovered: false }
                }
                "n" | "no" => {
                    self.note(
                        Tone::Warn,
                        "your words weren't saved to a file. Write them down, or use /phrase save once you're in.",
                    );
                    show_code(&seed, false, &mut effects)
                }
                _ => {
                    self.note(Tone::Bad, "type y or n");
                    Step::Replace { seed, path }
                }
            },
            Step::Code { .. } => {
                effects.push(Effect::Start);
                self.made = true;
                Step::Starting
            }
            // Nothing is asked while waiting.
            step @ (Step::Creating { .. } | Step::Starting) => step,
        };
        effects
    }

    /// What came of an effect onboarding asked for.
    pub fn done(&mut self, outcome: Outcome) -> Vec<Effect> {
        self.notes.clear();
        let mut effects = Vec::new();
        let step = std::mem::replace(&mut self.step, Step::Starting);
        self.step = match (step, outcome) {
            (step, Outcome::Unlock(StoreError::WrongPassphrase)) => {
                self.note(Tone::Bad, "wrong passphrase, try again");
                self.note(
                    Tone::Plain,
                    "forgotten it? Quit (Ctrl-C) and run corvo-wan-p2p --recover to start again from your \
                     12 words. The history on this computer is lost then.",
                );
                step
            }
            (_, Outcome::Unlock(error)) => {
                effects.push(Effect::Exit(error.to_string()));
                Step::Starting
            }
            (_, Outcome::Wiped(Ok(()))) => {
                self.note(Tone::Good, "the account on this computer is deleted.");
                Step::Words
            }
            (_, Outcome::Wiped(Err(why))) => {
                effects.push(Effect::Exit(format!("couldn't delete the account: {why}")));
                Step::Starting
            }
            (_, Outcome::Created(Err(why))) => {
                effects.push(Effect::Exit(format!("couldn't make your account: {why}")));
                Step::Starting
            }
            (Step::Creating { seed, recovered: true }, Outcome::Created(Ok(()))) => {
                show_code(&seed, true, &mut effects)
            }
            (Step::Creating { seed, recovered: false }, Outcome::Created(Ok(()))) => {
                effects.push(Effect::CheckRecovery);
                Step::Creating { seed, recovered: false }
            }
            (Step::Creating { seed, .. }, Outcome::RecoveryFile(Some(path))) => Step::Replace { seed, path },
            (Step::Creating { seed, .. }, Outcome::RecoveryFile(None)) => {
                effects.push(Effect::WriteRecovery { seed: seed.clone(), overwrite: false });
                Step::Creating { seed, recovered: false }
            }
            (Step::Creating { seed, .. }, Outcome::RecoveryWritten(Ok(path))) => {
                self.note(
                    Tone::Good,
                    format!(
                        "your words are saved in {}, readable only by you. Copy them somewhere safe \
                         (a password manager, a USB stick, paper), then delete the file.",
                        path.display()
                    ),
                );
                show_code(&seed, false, &mut effects)
            }
            (Step::Creating { seed, .. }, Outcome::RecoveryWritten(Err(why))) => {
                self.note(
                    Tone::Bad,
                    format!(
                        "couldn't save the recovery file: {why}. Write the words down instead: \
                         /phrase shows them again once you're in."
                    ),
                );
                show_code(&seed, false, &mut effects)
            }
            (step, _) => step,
        };
        effects
    }
}

#[cfg(test)]
thread_local! {
    /// How many answers were lower-cased, for tests.
    static LOWERED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// An answer trimmed and in lower case, for comparing; wiped after use,
/// since it may be a passphrase.
pub fn lowered(text: &Secret) -> Zeroizing<String> {
    #[cfg(test)]
    LOWERED.with(|l| l.set(l.get() + 1));
    Zeroizing::new(text.expose().trim().to_lowercase())
}

/// The code step, which copies the code as it opens.
fn show_code(seed: &Seed, recovered: bool, effects: &mut Vec<Effect>) -> Step {
    let code = seed.code();
    effects.push(Effect::Copy(code.to_string()));
    Step::Code { code, recovered }
}

/// Two different word positions, smaller first.
fn pick_two() -> [usize; 2] {
    let a = rand::random_range(0..PHRASE_WORDS);
    let b = (a + rand::random_range(1..PHRASE_WORDS)) % PHRASE_WORDS;
    [a.min(b), a.max(b)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::{Screen, State, Tone};
    use crate::ui::update::{Msg, update};
    use corvo_wan_p2p_core::identity::Seed;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn begin(account: bool, recover: bool) -> State {
        State::onboarding(Onboarding::start(account, recover))
    }

    fn onboarding(state: &State) -> &Onboarding {
        match &state.screen {
            Screen::Onboarding(o) => o,
            Screen::Main => panic!("onboarding is over"),
        }
    }

    /// `update`'s effects, without the window title it sets on the first message.
    fn run(state: &mut State, msg: Msg) -> Vec<Effect> {
        update(state, msg).into_iter().filter(|e| !matches!(e, Effect::SetTitle(_))).collect()
    }

    /// Types `text` and presses Enter; returns the effects.
    fn send(state: &mut State, text: &str) -> Vec<Effect> {
        for c in text.chars() {
            run(state, Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        run(state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
    }

    fn done(state: &mut State, outcome: Outcome) -> Vec<Effect> {
        run(state, Msg::Done(outcome))
    }

    fn last_note(state: &State) -> &Line {
        onboarding(state).notes.last().expect("a note")
    }

    fn label(state: &State) -> String {
        state.ask().expect("a question").label
    }

    /// Through New up to the words being shown; returns the seed.
    fn new_until_words(state: &mut State) -> Seed {
        send(state, "n");
        send(state, "  ana  ");
        send(state, "hunter2");
        send(state, "hunter2");
        let words = onboarding(state).words().expect("the words are shown");
        Seed::from_phrase(&words.join(" ")).unwrap()
    }

    /// Types back both asked words correctly; returns the effects of the last.
    fn type_back(state: &mut State, seed: &Seed) -> Vec<Effect> {
        send(state, "");
        let first = onboarding(state).asked_word().expect("a word is asked");
        send(state, seed.words()[first - 1]);
        let second = onboarding(state).asked_word().expect("a second word is asked");
        assert!(second > first, "two different words, in order");
        send(state, &seed.words()[second - 1].to_uppercase())
    }

    #[test]
    fn a_passphrase_is_never_lowercased() {
        let mut s = State::onboarding(Onboarding::start(false, false));
        send(&mut s, "n");
        send(&mut s, "ana");
        let before = LOWERED.with(|l| l.get());
        send(&mut s, "Pass-Word");
        send(&mut s, "Pass-Word");
        let mut s = State::onboarding(Onboarding::start(true, false));
        send(&mut s, "Pass-Word");
        assert_eq!(LOWERED.with(|l| l.get()), before, "no lower-case copy of a passphrase");
    }

    #[test]
    fn a_new_account_from_start_to_the_code() {
        let mut s = begin(false, false);
        assert_eq!(label(&s), "n or r: ");
        assert!(onboarding(&s).guidance(true).iter().any(|l| l.contains("no account")));
        let seed = new_until_words(&mut s);
        assert_eq!(onboarding(&s).words().unwrap().len(), 12);
        let effects = type_back(&mut s, &seed);
        assert_eq!(
            effects,
            vec![Effect::Create { seed: seed.clone(), name: "ana".into(), passphrase: Secret::new("hunter2".into()) }]
        );
        assert_eq!(s.busy, Some("making your account…"));
        assert_eq!(onboarding(&s).words(), None, "the words are hidden once typed back");

        assert_eq!(done(&mut s, Outcome::Created(Ok(()))), vec![Effect::CheckRecovery]);
        assert_eq!(s.busy, None);
        assert_eq!(
            done(&mut s, Outcome::RecoveryFile(None)),
            vec![Effect::WriteRecovery { seed: seed.clone(), overwrite: false }]
        );
        let path = PathBuf::from("/home/ana/corvo-wan-p2p-recovery.txt");
        let effects = done(&mut s, Outcome::RecoveryWritten(Ok(path)));
        assert_eq!(effects, vec![Effect::Copy(seed.code().to_string())], "the code is copied");
        assert!(last_note(&s).text.contains("/home/ana/corvo-wan-p2p-recovery.txt"));
        assert!(last_note(&s).text.contains("delete"));
        assert!(onboarding(&s).guidance(true).iter().any(|l| l.contains(&seed.code().to_string())));
        assert_eq!(send(&mut s, ""), vec![Effect::Start]);
        assert_eq!(s.busy, Some("starting…"));

        done(&mut s, Outcome::Started { name: "ana".into(), code: seed.code(), recovery_file: None });
        assert!(matches!(s.screen, Screen::Main));
        assert_eq!(s.name, "ana");
        assert_eq!(s.short_code, seed.code().short());
        assert_eq!(s.code, Some(seed.code()));
        assert!(s.history.prev("").is_none(), "nothing typed during onboarding is in the history");
    }

    #[test]
    fn choosing_takes_n_or_r_and_nothing_else() {
        let mut s = begin(false, false);
        send(&mut s, "maybe");
        assert_eq!(label(&s), "n or r: ");
        assert_eq!(last_note(&s).tone, Tone::Bad);
        send(&mut s, " R ");
        assert_eq!(label(&s), "words: ");
    }

    #[test]
    fn an_invalid_name_is_refused_with_the_reason() {
        let mut s = begin(false, false);
        send(&mut s, "n");
        send(&mut s, "   ");
        assert_eq!(label(&s), "name: ");
        assert_eq!(last_note(&s).text, "a name can't be empty");
        send(&mut s, &"x".repeat(33));
        assert!(last_note(&s).text.contains("33 characters"));
        send(&mut s, "evil\u{202E}name");
        assert!(last_note(&s).text.contains("invisible"));
        send(&mut s, "ana");
        assert_eq!(label(&s), "passphrase: ");
        assert!(s.ask().unwrap().masked);
    }

    #[test]
    fn an_empty_or_mismatched_passphrase_is_asked_again() {
        let mut s = begin(false, false);
        send(&mut s, "n");
        send(&mut s, "ana");
        send(&mut s, "");
        assert_eq!(label(&s), "passphrase: ");
        assert_eq!(last_note(&s).text, "the passphrase can't be empty");
        send(&mut s, "one");
        assert_eq!(label(&s), "again: ");
        assert!(s.ask().unwrap().masked);
        send(&mut s, "two");
        assert_eq!(label(&s), "passphrase: ", "back to choosing it");
        assert!(last_note(&s).text.contains("didn't match"));
        send(&mut s, "same");
        send(&mut s, "same");
        assert!(onboarding(&s).words().is_some());
    }

    #[test]
    fn a_wrong_typed_back_word_shows_the_words_again() {
        let mut s = begin(false, false);
        let seed = new_until_words(&mut s);
        send(&mut s, "");
        let asked = onboarding(&s).asked_word().unwrap();
        let wrong = seed.words()[asked % 12];
        assert!(send(&mut s, wrong).is_empty());
        assert!(last_note(&s).text.contains(&format!("isn't word {asked}")));
        assert!(onboarding(&s).words().is_some(), "the words are shown again");
        let effects = type_back(&mut s, &seed);
        assert!(matches!(effects[..], [Effect::Create { .. }]));
    }

    #[test]
    fn replacing_the_recovery_file_is_asked_first() {
        let mut s = begin(false, false);
        let seed = new_until_words(&mut s);
        type_back(&mut s, &seed);
        done(&mut s, Outcome::Created(Ok(())));
        let path = PathBuf::from("/home/ana/corvo-wan-p2p-recovery.txt");
        assert!(done(&mut s, Outcome::RecoveryFile(Some(path))).is_empty());
        assert!(onboarding(&s).guidance(true)[0].contains("/home/ana/corvo-wan-p2p-recovery.txt"));
        assert_eq!(label(&s), "replace it? y/n: ");
        assert!(send(&mut s, "what").is_empty());
        assert_eq!(label(&s), "replace it? y/n: ");
        assert_eq!(send(&mut s, "y"), vec![Effect::WriteRecovery { seed: seed.clone(), overwrite: true }]);
    }

    #[test]
    fn not_replacing_the_recovery_file_goes_on_without_one() {
        let mut s = begin(false, false);
        let seed = new_until_words(&mut s);
        type_back(&mut s, &seed);
        done(&mut s, Outcome::Created(Ok(())));
        done(&mut s, Outcome::RecoveryFile(Some(PathBuf::from("/h/corvo-wan-p2p-recovery.txt"))));
        assert_eq!(send(&mut s, "no"), vec![Effect::Copy(seed.code().to_string())]);
        assert_eq!(last_note(&s).tone, Tone::Warn);
        assert!(last_note(&s).text.contains("/phrase save"));
        assert_eq!(send(&mut s, ""), vec![Effect::Start]);
    }

    #[test]
    fn a_recovery_file_that_fails_to_save_says_so_and_goes_on() {
        let mut s = begin(false, false);
        let seed = new_until_words(&mut s);
        type_back(&mut s, &seed);
        done(&mut s, Outcome::Created(Ok(())));
        done(&mut s, Outcome::RecoveryFile(None));
        let effects = done(&mut s, Outcome::RecoveryWritten(Err("disk full".into())));
        assert_eq!(effects, vec![Effect::Copy(seed.code().to_string())]);
        assert_eq!(last_note(&s).tone, Tone::Bad);
        assert!(last_note(&s).text.contains("disk full"));
    }

    #[test]
    fn failing_to_make_the_account_exits_with_the_reason() {
        let mut s = begin(false, false);
        let seed = new_until_words(&mut s);
        type_back(&mut s, &seed);
        let effects = done(&mut s, Outcome::Created(Err("no space left".into())));
        assert_eq!(effects, vec![Effect::Exit("couldn't make your account: no space left".into())]);
    }

    #[test]
    fn the_engine_not_starting_exits_with_the_reason() {
        let mut s = begin(true, false);
        send(&mut s, "pw");
        assert_eq!(
            done(&mut s, Outcome::Fatal("no network card".into())),
            vec![Effect::Exit("no network card".into())]
        );
    }

    #[test]
    fn a_recovery_file_left_behind_is_a_reminder_on_start() {
        let mut s = begin(true, false);
        send(&mut s, "pw");
        let code = Seed::generate().code();
        let path = PathBuf::from("/home/ana/corvo-wan-p2p-recovery.txt");
        done(&mut s, Outcome::Started { name: "ana".into(), code, recovery_file: Some(path) });
        let last = s.tabs[0].lines.last().unwrap();
        assert_eq!(
            last.text,
            "your recovery file is still at /home/ana/corvo-wan-p2p-recovery.txt. Move it somewhere safe and delete it."
        );
        assert_eq!(last.tone, Tone::Warn);
    }

    #[test]
    fn recovering_brings_back_the_same_code() {
        let seed = Seed::generate();
        let mut s = begin(false, false);
        send(&mut s, "r");
        let typed = format!("  {}  ", seed.phrase().to_uppercase().replace(' ', "   "));
        send(&mut s, &typed);
        assert_eq!(label(&s), "name: ");
        send(&mut s, "ana");
        send(&mut s, "pw");
        let effects = send(&mut s, "pw");
        assert_eq!(
            effects,
            vec![Effect::Create { seed: seed.clone(), name: "ana".into(), passphrase: Secret::new("pw".into()) }]
        );
        let effects = done(&mut s, Outcome::Created(Ok(())));
        assert_eq!(effects, vec![Effect::Copy(seed.code().to_string())], "no recovery file: you have the words");
        assert!(onboarding(&s).guidance(true).iter().any(|l| l.contains("same as before")));
        assert_eq!(send(&mut s, ""), vec![Effect::Start]);
    }

    #[test]
    fn a_bad_recovery_word_is_named_and_left_to_fix() {
        let seed = Seed::generate();
        let mut words = seed.words();
        words[4] = "abandn";
        let typed = words.join(" ");
        let mut s = begin(false, false);
        send(&mut s, "r");
        assert!(send(&mut s, &typed).is_empty());
        assert_eq!(label(&s), "words: ");
        assert_eq!(last_note(&s).text, "word 5 (\"abandn\") isn't one of the recovery words; check its spelling");
        assert_eq!(s.editor.text(), typed, "what was typed is back to fix");
        s.editor.clear();
        send(&mut s, &words[..11].join(" "));
        assert!(last_note(&s).text.starts_with("that's 11 words"));
    }

    #[test]
    fn recover_without_an_account_starts_at_the_words() {
        let s = begin(false, true);
        assert_eq!(label(&s), "words: ");
    }

    #[test]
    fn recover_with_an_account_deletes_it_after_yes() {
        let mut s = begin(true, true);
        assert_eq!(label(&s), "type yes: ");
        assert!(onboarding(&s).guidance(true)[0].contains("deletes"));
        assert_eq!(send(&mut s, " YES "), vec![Effect::Wipe]);
        assert_eq!(s.busy, Some("deleting…"));
        assert!(done(&mut s, Outcome::Wiped(Ok(()))).is_empty());
        assert_eq!(label(&s), "words: ");
        assert_eq!(last_note(&s).tone, Tone::Good);
    }

    #[test]
    fn recover_without_yes_deletes_nothing() {
        let mut s = begin(true, true);
        assert_eq!(send(&mut s, "y"), vec![Effect::Exit("nothing was deleted.".into())]);
    }

    #[test]
    fn a_failed_delete_exits_with_the_reason() {
        let mut s = begin(true, true);
        send(&mut s, "yes");
        let effects = done(&mut s, Outcome::Wiped(Err("it's open in another window".into())));
        assert_eq!(effects, vec![Effect::Exit("couldn't delete the account: it's open in another window".into())]);
    }

    #[test]
    fn unlocking_asks_for_the_passphrase_masked() {
        let mut s = begin(true, false);
        assert_eq!(label(&s), "passphrase: ");
        assert!(s.ask().unwrap().masked);
        assert!(send(&mut s, "").is_empty());
        assert_eq!(last_note(&s).text, "type your passphrase");
        assert_eq!(send(&mut s, "my pass "), vec![Effect::Unlock(Secret::new("my pass ".into()))]);
        assert_eq!(s.busy, Some("unlocking…"));
    }

    #[test]
    fn a_wrong_passphrase_says_so_hints_at_recover_and_asks_again() {
        let mut s = begin(true, false);
        send(&mut s, "nope");
        assert!(done(&mut s, Outcome::Unlock(StoreError::WrongPassphrase)).is_empty());
        let notes = &onboarding(&s).notes;
        assert_eq!(notes[0].text, "wrong passphrase, try again");
        assert_eq!(notes[0].tone, Tone::Bad);
        assert!(notes[1].text.contains("corvo-wan-p2p --recover"));
        assert_eq!(label(&s), "passphrase: ");
        assert_eq!(send(&mut s, "yes"), vec![Effect::Unlock(Secret::new("yes".into()))]);
    }

    #[test]
    fn an_account_that_cannot_be_opened_exits_with_its_message() {
        let errors = [
            StoreError::AlreadyRunning,
            StoreError::TooNew(PathBuf::from("/d/store.db")),
            StoreError::Damaged { file: PathBuf::from("/d/store.db"), why: "a value doesn't decrypt".into() },
        ];
        for error in errors {
            let message = error.to_string();
            let mut s = begin(true, false);
            send(&mut s, "pw");
            assert_eq!(done(&mut s, Outcome::Unlock(error)), vec![Effect::Exit(message)]);
        }
    }

    #[test]
    fn a_pasted_answer_loses_its_line_breaks() {
        let mut s = begin(true, false);
        run(&mut s, Msg::Paste("hunter2\r\n".into()));
        assert_eq!(s.editor.text(), "hunter2");
        assert_eq!(send(&mut s, ""), vec![Effect::Unlock(Secret::new("hunter2".into()))]);

        let seed = Seed::generate();
        let mut s = begin(false, true);
        run(&mut s, Msg::Paste(format!("{}\n", seed.words().join("\n"))));
        assert_eq!(s.editor.text(), seed.phrase(), "words pasted one per line become one line");
        send(&mut s, "");
        assert_eq!(label(&s), "name: ");
    }

    #[test]
    fn altgr_characters_reach_a_masked_answer() {
        // Windows reports AltGr chords (like @ on a German keyboard) as Ctrl+Alt.
        let mut s = begin(true, false);
        run(&mut s, Msg::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)));
        run(&mut s, Msg::Key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT)));
        assert_eq!(s.editor.text(), "p@");
    }
}
