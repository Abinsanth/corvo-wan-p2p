//! Carries out [`Effect`]s: the store, the recovery file, the chat engine and
//! the terminal. What came of each goes back to `update` as [`Msg::Done`].

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use corvo_wan_p2p_core::chat::{Client, Config, Event};
use corvo_wan_p2p_core::identity::Seed;
use corvo_wan_p2p_core::net::internet_endpoint;
use corvo_wan_p2p_core::recovery;
use corvo_wan_p2p_core::store::Store;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::SetTitle;

use super::account::{PhraseDone, PhraseUse};
use super::clip::osc52;
use super::state::Secret;
use super::update::{Effect, Msg, Outcome};

/// For commands that need the engine, before it has started.
const NOT_STARTED: &str = "the chat engine hasn't started";

/// How the engine reaches other people.
pub enum Net {
    /// n0's relays and discovery: the real thing.
    Internet,
    /// This machine only, for tests.
    #[cfg(test)]
    Local,
}

/// What the loop does after an effect.
#[derive(Debug)]
pub enum Flow {
    /// Carry on, feeding this to `update` if there is one.
    Next(Option<Msg>),
    Quit,
    /// Leave, printing this.
    Exit(String),
}

pub struct Runner<W: Write> {
    dir: PathBuf,
    home: Option<PathBuf>,
    config: Config,
    net: Net,
    /// The terminal: window titles and the clipboard are written here.
    out: W,
    /// An account just made, until the engine starts on it.
    store: Option<Store>,
    client: Option<Client>,
}

impl<W: Write> Runner<W> {
    /// `dir` is the data folder; `home` is where the recovery file goes.
    pub fn new(dir: PathBuf, home: Option<PathBuf>, config: Config, net: Net, out: W) -> Runner<W> {
        Runner { dir, home, config, net, out, store: None, client: None }
    }

    /// Does `effect`. An error here is the terminal failing; everything else
    /// comes back as an [`Outcome`].
    pub async fn apply(&mut self, effect: Effect) -> anyhow::Result<Flow> {
        let outcome = match effect {
            Effect::Quit => return Ok(Flow::Quit),
            Effect::Exit(why) => return Ok(Flow::Exit(why)),
            Effect::SetTitle(title) => {
                execute!(self.out, SetTitle(title))?;
                return Ok(Flow::Next(None));
            }
            Effect::Copy(text) => {
                self.copy(&text)?;
                return Ok(Flow::Next(None));
            }
            Effect::Unlock(passphrase) => {
                let dir = self.dir.clone();
                match blocking(move || Store::open(&dir, passphrase.expose())).await? {
                    Ok(store) => self.start(store).await,
                    Err(e) => Outcome::Unlock(e),
                }
            }
            Effect::Create { seed, name, passphrase } => {
                let (dir, kdf) = (self.dir.clone(), self.config.kdf);
                let made = blocking(move || Store::create(&dir, passphrase.expose(), kdf, &seed, &name)).await?;
                Outcome::Created(made.map(|store| self.store = Some(store)).map_err(|e| e.to_string()))
            }
            Effect::Start => match self.store.take() {
                Some(store) => self.start(store).await,
                None => Outcome::Fatal("there's no account to start".into()),
            },
            Effect::Wipe => {
                let dir = self.dir.clone();
                Outcome::Wiped(blocking(move || Store::wipe(&dir)).await?.map_err(|e| e.to_string()))
            }
            Effect::CheckRecovery => Outcome::RecoveryFile(self.recovery_file()),
            Effect::WriteRecovery { seed, overwrite } => {
                Outcome::RecoveryWritten(self.write_recovery(&seed, overwrite))
            }
            Effect::ChangePassphrase { old, new } => Outcome::PassphraseChanged(match &self.client {
                Some(client) => client.change_passphrase(old.expose(), new.expose()).await.map_err(|e| e.to_string()),
                None => Err(NOT_STARTED.into()),
            }),
            Effect::Phrase { passphrase, then } => Outcome::Phrase(self.phrase(&passphrase, then).await),
            Effect::Load
            | Effect::Send { .. }
            | Effect::MarkRead(_)
            | Effect::Act { .. }
            | Effect::SetName(_)
            | Effect::Refresh(_) => return Ok(Flow::Next(None)),
        };
        Ok(Flow::Next(Some(Msg::Done(outcome))))
    }

    /// The engine's next event; waits forever until it's started.
    pub async fn next_event(&mut self) -> Option<Event> {
        match &mut self.client {
            Some(client) => client.next_event().await,
            None => std::future::pending().await,
        }
    }

    /// Stops the engine, if it's running, and lets go of the store.
    pub async fn shutdown(&mut self) {
        self.store = None;
        if let Some(client) = self.client.take() {
            client.shutdown().await;
        }
    }

    fn copy(&mut self, text: &str) -> std::io::Result<()> {
        let sequence = Secret::new(osc52(text));
        self.out.write_all(sequence.expose().as_bytes())?;
        self.out.flush()
    }

    /// Starts the engine on `store`; anything that stops it is fatal.
    async fn start(&mut self, store: Store) -> Outcome {
        match self.try_start(store).await {
            Ok(outcome) => outcome,
            Err(e) => Outcome::Fatal(format!("couldn't start: {e:#}")),
        }
    }

    async fn try_start(&mut self, store: Store) -> anyhow::Result<Outcome> {
        let profile = store.profile()?;
        let secret = profile.seed.secret_key();
        let endpoint = match self.net {
            Net::Internet => internet_endpoint(secret).await?,
            #[cfg(test)]
            Net::Local => {
                let lookup = iroh::address_lookup::MemoryLookup::new();
                corvo_wan_p2p_core::net::local_endpoint(secret, lookup).await?
            }
        };
        let client = Client::start(store, endpoint, self.config)?;
        let code = client.me();
        self.client = Some(client);
        Ok(Outcome::Started { name: profile.name, code, recovery_file: self.recovery_file() })
    }

    /// Where the recovery file is, if it's there.
    fn recovery_file(&self) -> Option<PathBuf> {
        self.home.as_deref().map(recovery::path).filter(|p| p.exists())
    }

    fn write_recovery(&self, seed: &Seed, overwrite: bool) -> Result<PathBuf, String> {
        let home = self.home.as_deref().ok_or("couldn't find your home folder")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        recovery::write(home, seed, &date(now), overwrite).map_err(|e| e.to_string())
    }

    async fn phrase(&mut self, passphrase: &Secret, then: PhraseUse) -> Result<PhraseDone, String> {
        let client = self.client.as_ref().ok_or(NOT_STARTED)?;
        let seed = client.phrase(passphrase.expose()).await.map_err(|e| e.to_string())?;
        let words = Secret::new(seed.phrase());
        match then {
            PhraseUse::Show => Ok(PhraseDone::Shown(words)),
            PhraseUse::Copy => {
                self.copy(words.expose()).map_err(|e| format!("couldn't copy them: {e}"))?;
                Ok(PhraseDone::Copied)
            }
            PhraseUse::Save { overwrite } => self.write_recovery(&seed, overwrite).map(PhraseDone::Saved),
        }
    }
}

/// Runs slow work (Argon2, file system) off the async threads.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(work).await.context("a background task failed")
}

/// `YYYY-MM-DD` for a time in seconds since 1970, in UTC.
pub fn date(unix_secs: u64) -> String {
    // Howard Hinnant's days-to-civil, for days since 1970-01-01.
    let days = (unix_secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::clip::osc52;
    use crate::ui::onboarding::Onboarding;
    use crate::ui::state::{Screen, State};
    use crate::ui::update::update;
    use corvo_wan_p2p_core::identity::Seed;
    use corvo_wan_p2p_core::recovery;
    use corvo_wan_p2p_core::store::{KdfParams, Store};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::collections::VecDeque;
    use tempfile::TempDir;

    const FAST: Config = Config { kdf: KdfParams::FAST_INSECURE, ..Config::DEFAULT };

    /// The screen and the runner, joined the way the loop joins them, minus the terminal.
    struct App {
        state: State,
        runner: Runner<Vec<u8>>,
    }

    impl App {
        /// Starts as `main` does, on the account in `data` (or none).
        fn open(data: &TempDir, home: &TempDir, recover: bool) -> App {
            let dir = data.path().join("corvo-wan-p2p");
            let state = State::onboarding(Onboarding::start(Store::exists(&dir), recover));
            let runner = Runner::new(dir, Some(home.path().to_path_buf()), FAST, Net::Local, Vec::new());
            App { state, runner }
        }

        /// Feeds `msg` to `update` and carries out every effect that follows.
        /// `Some` with the message if the app exits.
        async fn handle(&mut self, msg: Msg) -> Option<String> {
            let mut queue: VecDeque<Effect> = update(&mut self.state, msg).into();
            while let Some(effect) = queue.pop_front() {
                match self.runner.apply(effect).await.unwrap() {
                    Flow::Next(Some(msg)) => queue.extend(update(&mut self.state, msg)),
                    Flow::Next(None) => {}
                    Flow::Quit => return None,
                    Flow::Exit(why) => return Some(why),
                }
            }
            None
        }

        async fn send(&mut self, text: &str) -> Option<String> {
            for c in text.chars() {
                self.handle(Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))).await;
            }
            self.handle(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))).await
        }

        fn onboarding(&self) -> &Onboarding {
            match &self.state.screen {
                Screen::Onboarding(o) => o,
                Screen::Main => panic!("onboarding is over"),
            }
        }

        fn written(&self) -> String {
            String::from_utf8_lossy(&self.runner.out).into_owned()
        }

        fn said(&self) -> Vec<String> {
            self.state.tabs[0].lines.iter().map(|l| l.text.clone()).collect()
        }

        /// Makes a new account named ana with passphrase `pw`; returns its seed.
        async fn make_account(&mut self) -> Seed {
            self.send("n").await;
            self.send("ana").await;
            self.send("pw").await;
            self.send("pw").await;
            let seed = Seed::from_phrase(&self.onboarding().words().unwrap().join(" ")).unwrap();
            self.send("").await;
            for _ in 0..2 {
                let n = self.onboarding().asked_word().unwrap();
                self.send(seed.words()[n - 1]).await;
            }
            seed
        }
    }

    #[tokio::test]
    async fn an_account_made_through_onboarding_unlocks_to_the_same_code() {
        let (data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut app = App::open(&data, &home, false);
        let seed = app.make_account().await;
        let file = std::fs::read_to_string(recovery::path(home.path())).expect("the recovery file is written");
        for (i, word) in seed.words().iter().enumerate() {
            assert!(file.contains(&format!("{:>2}. {word}", i + 1)));
        }
        assert!(file.contains(&seed.code().to_string()));
        assert!(app.written().contains(&osc52(&seed.code().to_string())), "the code is copied");
        assert_eq!(app.send("").await, None);
        assert!(matches!(app.state.screen, Screen::Main));
        assert_eq!(app.state.code, Some(seed.code()));
        app.runner.shutdown().await;

        let mut app = App::open(&data, &home, false);
        assert_eq!(app.state.ask().unwrap().label, "passphrase: ");
        app.send("pw").await;
        assert!(matches!(app.state.screen, Screen::Main));
        assert_eq!(app.state.code, Some(seed.code()), "the same code");
        assert_eq!(app.state.name, "ana");
        assert!(app.said().iter().any(|l| l.starts_with("your recovery file is still at")));
        app.runner.shutdown().await;
    }

    #[tokio::test]
    async fn a_changed_passphrase_is_the_one_that_unlocks() {
        let (data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut app = App::open(&data, &home, false);
        app.make_account().await;
        app.send("").await;
        for answer in ["/passphrase", "wrong", "new", "new"] {
            app.send(answer).await;
        }
        assert_eq!(app.said().last().unwrap(), "wrong passphrase; nothing changed");
        for answer in ["/passphrase", "pw", "new", "new"] {
            app.send(answer).await;
        }
        assert!(app.said().last().unwrap().starts_with("passphrase changed"));
        app.runner.shutdown().await;

        let mut app = App::open(&data, &home, false);
        app.send("pw").await;
        assert_eq!(app.onboarding().notes[0].text, "wrong passphrase, try again");
        app.send("new").await;
        assert!(matches!(app.state.screen, Screen::Main));
        app.runner.shutdown().await;
    }

    #[tokio::test]
    async fn phrase_shows_saves_and_copies_the_words() {
        let (data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut app = App::open(&data, &home, false);
        let seed = app.make_account().await;
        app.send("").await;
        app.send("/phrase").await;
        app.send("nope").await;
        assert_eq!(app.state.reveal, None);
        assert_eq!(app.said().last().unwrap(), "wrong passphrase");
        app.send("/phrase").await;
        app.send("pw").await;
        assert_eq!(app.state.reveal.as_ref().map(|w| w.expose().to_string()), Some(seed.phrase()));
        app.handle(Msg::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))).await;
        assert_eq!(app.state.reveal, None);

        app.send("/phrase copy").await;
        app.send("pw").await;
        assert!(app.written().contains(&osc52(&seed.phrase())));

        recovery::remove(home.path()).unwrap();
        app.send("/phrase save").await;
        app.send("pw").await;
        assert!(recovery::exists(home.path()));
        app.send("/phrase save").await;
        app.send("pw").await;
        assert_eq!(app.state.ask().unwrap().label, "replace it? y/n: ");
        app.send("y").await;
        assert!(app.said().last().unwrap().starts_with("your words are saved in"));
        app.runner.shutdown().await;
    }

    #[tokio::test]
    async fn a_second_window_on_the_same_account_exits() {
        let (data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut first = App::open(&data, &home, false);
        first.make_account().await;
        first.send("").await;
        let mut second = App::open(&data, &home, false);
        let exit = second.send("pw").await;
        assert_eq!(
            exit.as_deref(),
            Some("corvo-wan-p2p is already open with this account; close the other window first")
        );
        first.runner.shutdown().await;
    }

    #[tokio::test]
    async fn recover_deletes_the_account_and_brings_back_the_same_code() {
        let (data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut app = App::open(&data, &home, false);
        let seed = app.make_account().await;
        app.send("").await;
        app.runner.shutdown().await;

        let mut app = App::open(&data, &home, true);
        app.send("yes").await;
        assert_eq!(app.state.ask().unwrap().label, "words: ");
        app.send(&seed.phrase()).await;
        app.send("ana again").await;
        app.send("other").await;
        app.send("other").await;
        app.send("").await;
        assert_eq!(app.state.code, Some(seed.code()));
        app.runner.shutdown().await;

        let mut app = App::open(&data, &home, false);
        app.send("pw").await;
        assert_eq!(app.onboarding().notes[0].text, "wrong passphrase, try again", "the old account is gone");
        app.send("other").await;
        assert_eq!(app.state.name, "ana again");
        app.runner.shutdown().await;
    }

    #[test]
    fn dates_are_utc_days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_792_022_400), "2026-10-15");
        assert_eq!(date(1_792_108_799), "2026-10-15");
    }
}
