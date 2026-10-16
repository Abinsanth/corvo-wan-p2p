//! Carries out [`Effect`]s: the store, the recovery file, the chat engine and
//! the terminal. What came of each goes back to `update` as [`Msg::Done`].

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use corvo_wan_p2p_core::chat::{ChatError, Client, Config, Event};
use corvo_wan_p2p_core::identity::{Code, Seed};
use corvo_wan_p2p_core::net::internet_endpoint;
use corvo_wan_p2p_core::recovery;
use corvo_wan_p2p_core::store::{Mark, Standing, Store, StoredMessage};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::SetTitle;

use super::account::{PhraseDone, PhraseUse};
use super::chat::{History, Loaded};
use super::clip::osc52;
use super::people::Act;
use super::state::{CHAT_LEN, Secret};
use super::update::{Effect, Msg, Outcome};
use crate::selfupdate::{self, Release, Urls};
use semver::Version;

/// For commands that need the engine, before it has started.
const NOT_STARTED: &str = "the chat engine hasn't started";

/// How the engine reaches other people.
pub enum Net {
    /// n0's relays and discovery: the real thing.
    Internet,
    /// This machine only, for tests: apps sharing a lookup find each other.
    #[cfg(test)]
    Local(iroh::address_lookup::MemoryLookup),
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

/// Where `/update` looks, and what it found.
struct Updates {
    urls: Urls,
    /// The binary to replace: the running one, found at start (on Linux its
    /// path reads as deleted once it's been replaced).
    target: Result<PathBuf, String>,
    /// The asset to install: this system's.
    asset: Result<String, String>,
    /// Run the new binary before the swap, to see it works here.
    verify: bool,
    /// The release the last check found.
    found: Option<Release>,
    /// The version installed since this app started, which a restart runs.
    installed: Option<Version>,
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
    updates: Updates,
}

impl<W: Write> Runner<W> {
    /// `dir` is the data folder; `home` is where the recovery file goes.
    pub fn new(dir: PathBuf, home: Option<PathBuf>, config: Config, net: Net, out: W) -> Runner<W> {
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let updates = Updates {
            urls: Urls::default(),
            target: selfupdate::running_exe().map_err(|e| format!("couldn't find the running app: {e}")),
            asset: selfupdate::asset_name(os, arch).ok_or_else(|| selfupdate::no_build(os, arch)),
            verify: true,
            found: None,
            installed: None,
        };
        Runner { dir, home, config, net, out, store: None, client: None, updates }
    }

    /// Points `/update` at `urls`, installing `asset` over `target`.
    #[cfg(test)]
    fn with_updates(mut self, urls: Urls, asset: String, target: PathBuf) -> Self {
        let verify = selfupdate::testing::VERIFY;
        self.updates = Updates { urls, target: Ok(target), asset: Ok(asset), verify, found: None, installed: None };
        self
    }

    /// The newer version with a build for this system, if there is one.
    async fn check_update(&mut self) -> Result<Option<String>, String> {
        let target = self.updates.target.clone()?;
        if selfupdate::is_built_from_source(&target) {
            return Err(selfupdate::FROM_SOURCE.into());
        }
        let asset = self.updates.asset.clone()?;
        let found = selfupdate::check_with(&self.updates.urls, &selfupdate::current(), &asset).await?;
        let version = found.as_ref().map(|r| r.version.to_string());
        self.updates.found = found;
        Ok(version)
    }

    async fn install_update(&mut self, release: &Release) -> Result<(), String> {
        let (target, asset) = (self.updates.target.clone()?, self.updates.asset.clone()?);
        selfupdate::install_with(&self.updates.urls, release, &asset, &target, self.updates.verify).await?;
        self.updates.installed = Some(release.version.clone());
        Ok(())
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
            Effect::Load => Outcome::Loaded(self.load().await.map_err(|e| e.to_string())),
            Effect::Send { to, text } => {
                let result = match self.client() {
                    Ok(client) => client.send(to, &text).await.map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                let result =
                    result.map(|id| StoredMessage { id, sent_at: now_ms(), text: text.clone(), mark: Mark::Waiting });
                Outcome::Sent { to, text, result }
            }
            Effect::LoadOlder { peer, before, n } => Outcome::LoadedOlder {
                peer,
                result: match self.client() {
                    Ok(client) => client.history_before(peer, Some(before), n).await.map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                },
            },
            Effect::MarkRead(peer) => match self.client() {
                Ok(client) => match client.mark_read(peer).await {
                    Ok(()) => return Ok(Flow::Next(None)),
                    Err(e) => Outcome::Failed(e.to_string()),
                },
                Err(e) => Outcome::Failed(e),
            },
            Effect::Act { code, act } => {
                let result = self.act(code, act).await;
                // Everyone again, so the screen matches what the engine now has.
                let people = match (&result, self.client()) {
                    (Ok(()), Ok(client)) => client.people().await.ok(),
                    _ => None,
                };
                Outcome::Acted { code, act, result, people }
            }
            Effect::SetName(name) => Outcome::Named(match self.client() {
                Ok(client) => client.set_name(&name).await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            }),
            Effect::CheckUpdate { yes } => match &self.updates.installed {
                Some(version) => Outcome::UpdatedAlready(version.to_string()),
                None => Outcome::UpdateChecked { yes, result: self.check_update().await },
            },
            Effect::InstallUpdate => Outcome::Updated(match self.updates.found.take() {
                Some(release) => self.install_update(&release).await.map(|()| release.version.to_string()),
                None => Err("nothing to install: /update looks for a newer version first".into()),
            }),
            Effect::Refresh(show) => Outcome::Refreshed {
                show,
                result: match self.client() {
                    Ok(client) => client.people().await.map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                },
            },
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

    fn client(&self) -> Result<&Client, String> {
        self.client.as_ref().ok_or_else(|| NOT_STARTED.to_string())
    }

    /// Everyone, and the chats with contacts and requests.
    async fn load(&self) -> Result<Loaded, ChatError> {
        let client = self.client.as_ref().ok_or(ChatError::Stopped)?;
        let (people, unreadable_people) = client.people_checked().await?;
        let mut chats = Vec::new();
        for p in people.iter().filter(|p| p.standing != Standing::Blocked) {
            let (messages, unreadable) = client.history_before(p.code, None, CHAT_LEN).await?;
            let read_upto = client.read_upto(p.code).await?;
            chats.push(History { code: p.code, messages, read_upto, unreadable });
        }
        Ok(Loaded { people, chats, unreadable_people })
    }

    async fn act(&self, code: Code, act: Act) -> Result<(), String> {
        let client = self.client()?;
        let done = match act {
            Act::Add => client.add_contact(code).await,
            Act::Accept => client.accept(code).await,
            Act::Block => client.block(code).await,
            Act::Unblock => client.unblock(code).await,
            Act::Remove { history } => client.remove(code, history).await,
        };
        done.map_err(|e| e.to_string())
    }

    fn copy(&mut self, text: &str) -> std::io::Result<()> {
        let sequence = osc52(text);
        self.out.write_all(sequence.as_bytes())?;
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
        let endpoint = match &self.net {
            Net::Internet => internet_endpoint(secret).await?,
            #[cfg(test)]
            Net::Local(lookup) => {
                let endpoint = corvo_wan_p2p_core::net::local_endpoint(secret, lookup.clone()).await?;
                lookup.set_endpoint_info(corvo_wan_p2p_core::net::local_addr(&endpoint));
                endpoint
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
            PhraseUse::Save { overwrite } => match self.recovery_file() {
                Some(path) if !overwrite => Ok(PhraseDone::Exists(path)),
                _ => self.write_recovery(&seed, overwrite).map(PhraseDone::Saved),
            },
        }
    }
}

/// Now, in Unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
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
    use crate::ui::window::Overlay;
    use corvo_wan_p2p_core::identity::Seed;
    use corvo_wan_p2p_core::recovery;
    use corvo_wan_p2p_core::store::KdfParams;
    use iroh::address_lookup::MemoryLookup;
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
            App::open_on(data, home, recover, &MemoryLookup::new())
        }

        /// Like `open`, finding other apps through `lookup`.
        fn open_on(data: &TempDir, home: &TempDir, recover: bool, lookup: &MemoryLookup) -> App {
            let dir = data.path().join("corvo-wan-p2p");
            let state = State::onboarding(Onboarding::start(Store::exists(&dir), recover));
            let net = Net::Local(lookup.clone());
            let runner = Runner::new(dir, Some(home.path().to_path_buf()), FAST, net, Vec::new());
            App { state, runner }
        }

        /// An account called `name` with passphrase `pw`, made straight in the
        /// store, then unlocked as at a start.
        async fn person(data: &TempDir, home: &TempDir, lookup: &MemoryLookup, name: &str) -> App {
            Store::create(&data.path().join("corvo-wan-p2p"), "pw", KdfParams::FAST_INSECURE, &Seed::generate(), name)
                .unwrap();
            App::restart(data, home, lookup).await
        }

        /// Starts the app on an existing account and unlocks it.
        async fn restart(data: &TempDir, home: &TempDir, lookup: &MemoryLookup) -> App {
            let mut app = App::open_on(data, home, false, lookup);
            assert_eq!(app.send("pw").await, None);
            assert!(matches!(app.state.screen, Screen::Main));
            app
        }

        fn code(&self) -> Code {
            self.state.code.unwrap()
        }

        /// Handles engine events until `done` holds.
        async fn until(&mut self, what: &str, done: impl Fn(&State) -> bool) {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            while !done(&self.state) {
                let event = tokio::time::timeout_at(deadline, self.runner.next_event())
                    .await
                    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                    .expect("the engine is running");
                self.handle(Msg::Core(event)).await;
            }
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

        /// Enter at the code starts the app; the welcome shows, and Enter closes it.
        async fn start(&mut self) {
            self.send("").await;
            assert!(matches!(self.state.overlay, Some(Overlay::Welcome { .. })), "the welcome shows");
            self.send("").await;
            assert_eq!(self.state.overlay, None);
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
        assert!(app.written().contains(osc52(&seed.code().to_string()).as_str()), "the code is copied");
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
        app.start().await;
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
        app.start().await;
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
        assert!(app.written().contains(osc52(&seed.phrase()).as_str()));

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
    async fn phrase_save_checks_the_passphrase_before_asking_to_replace() {
        let (data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut app = App::open(&data, &home, false);
        app.make_account().await;
        app.start().await;
        let file = recovery::path(home.path());
        let before = std::fs::read_to_string(&file).unwrap();
        app.send("/phrase save").await;
        app.send("nope").await;
        assert_eq!(app.state.ask(), None, "no question after a wrong passphrase");
        assert_eq!(app.said().last().unwrap(), "wrong passphrase");
        app.send("/phrase save").await;
        app.send("pw").await;
        assert_eq!(app.state.ask().unwrap().label, "replace it? y/n: ");
        app.send("n").await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "kept");
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

    fn marks(app: &App, with: Code) -> Vec<Mark> {
        app.state.chats.get(&with).map(|c| c.messages.iter().map(|m| m.mark).collect()).unwrap_or_default()
    }

    /// What the app logs says who and what happened, never what was said or
    /// who people are. (Logging at its most detailed here.)
    #[tokio::test]
    async fn the_log_holds_no_message_text_names_or_codes() {
        let log = crate::logging::start_for_tests();
        let lookup = MemoryLookup::new();
        let (zorba_data, quixote_data, home) =
            (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut zorba = App::person(&zorba_data, &home, &lookup, "zorba").await;
        let mut quixote = App::person(&quixote_data, &home, &lookup, "quixote").await;
        let (zorba_code, quixote_code) = (zorba.code(), quixote.code());
        zorba.send(&format!("/add {quixote_code}")).await;
        zorba.send("marker-7f3a is the secret").await;
        quixote.until("zorba's message", |s| s.chats.get(&zorba_code).is_some_and(|c| !c.messages.is_empty())).await;
        quixote.send("/chat zorba").await;
        quixote.send("/accept").await;
        quixote.send("marker-7f3a back").await;
        zorba.until("the reply", |s| s.chats[&quixote_code].messages.len() == 2).await;
        let text = std::fs::read_to_string(log).unwrap();
        assert!(text.contains("link up"), "something was logged:\n{text}");
        for secret in ["marker-7f3a", "zorba", "quixote", &zorba_code.to_string(), &quixote_code.to_string()] {
            assert!(!text.contains(secret), "the log holds {secret:?}:\n{text}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(log).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn two_people_chat_with_marks_unread_and_presence() {
        let lookup = MemoryLookup::new();
        let (ana_data, rahul_data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut ana = App::person(&ana_data, &home, &lookup, "ana").await;
        let mut rahul = App::person(&rahul_data, &home, &lookup, "rahul").await;
        let (ana_code, rahul_code) = (ana.code(), rahul.code());

        ana.send(&format!("/add {rahul_code}")).await;
        assert_eq!(ana.state.open_peer(), Some(rahul_code), "adding opens the chat");
        ana.send("hi rahul").await;
        assert_eq!(marks(&ana, rahul_code), vec![Mark::Waiting]);

        rahul.until("ana's request", |s| s.chats.get(&ana_code).is_some_and(|c| c.unread == 1)).await;
        assert_eq!(rahul.state.standing(&ana_code), Some(Standing::Request));
        assert_eq!(rahul.state.label(&ana_code), "ana");
        assert_eq!(rahul.state.title, format!("(1) {}", crate::ui::state::APP));
        ana.until("✓", |s| marks_in(s, rahul_code) == vec![Mark::Delivered]).await;

        rahul.send("/chat ana").await;
        rahul.send("/accept").await;
        assert_eq!(rahul.state.standing(&ana_code), Some(Standing::Contact));
        assert_eq!(rahul.state.chats[&ana_code].unread, 0, "the open chat is read");
        ana.until("✓✓", |s| marks_in(s, rahul_code) == vec![Mark::Read]).await;
        ana.until("rahul online", |s| s.people.iter().any(|p| p.code == rahul_code && p.online)).await;

        rahul.send("hello ana").await;
        ana.until("rahul's reply", |s| s.chats[&rahul_code].messages.len() == 2).await;
        rahul.until("✓ on the reply", |s| marks_in(s, ana_code).last() == Some(&Mark::Delivered)).await;

        // Unread counts survive a restart.
        ana.send("/close").await;
        rahul.send("one more").await;
        ana.until("the next message", |s| s.chats[&rahul_code].unread == 1).await;
        ana.runner.shutdown().await;
        let mut ana = App::restart(&ana_data, &home, &lookup).await;
        assert_eq!(ana.state.chats[&rahul_code].unread, 1);
        assert_eq!(ana.state.chats[&rahul_code].messages.len(), 3);
        assert_eq!(ana.state.title, format!("(1) {}", crate::ui::state::APP));
        ana.send("/chat rahul").await;
        assert_eq!(ana.state.chats[&rahul_code].unread, 0);
        ana.runner.shutdown().await;
        rahul.runner.shutdown().await;
    }

    fn marks_in(state: &State, with: Code) -> Vec<Mark> {
        state.chats.get(&with).map(|c| c.messages.iter().map(|m| m.mark).collect()).unwrap_or_default()
    }

    #[tokio::test]
    async fn people_commands_reach_the_engine() {
        let lookup = MemoryLookup::new();
        let (data, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut ana = App::person(&data, &home, &lookup, "ana").await;
        let friend = Seed::generate().code();
        ana.send(&format!("/add {friend}")).await;
        assert_eq!(ana.state.standing(&friend), Some(Standing::Contact));
        ana.send(&format!("/block {}", friend.short())).await;
        assert_eq!(ana.state.standing(&friend), Some(Standing::Blocked));
        ana.send(&format!("/unblock {}", friend.short())).await;
        assert_eq!(ana.state.standing(&friend), None, "a stranger again");
        ana.send(&format!("/add {friend}")).await;
        ana.send(&format!("/remove {}", friend.short())).await;
        ana.send("y").await;
        assert_eq!(ana.state.standing(&friend), None);
        assert!(!ana.state.chats.contains_key(&friend));
        ana.send("/name Ana M").await;
        assert_eq!(ana.state.name, "Ana M");
        ana.send("/contacts").await;
        assert_eq!(ana.said().last().unwrap(), "no contacts yet: /add <code> adds a friend");
        ana.send(&format!("/add {}", ana.code())).await;
        assert!(ana.said().last().unwrap().contains("your own code"));
        ana.runner.shutdown().await;
        let ana = App::restart(&data, &home, &lookup).await;
        assert_eq!(ana.state.name, "Ana M", "the new name was saved");
    }

    #[tokio::test]
    async fn update_installs_a_newer_release_after_yes() {
        let (data, home, site, bin) =
            (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
        let lookup = MemoryLookup::new();
        let mut app = App::person(&data, &home, &lookup, "ana").await;
        let (urls, asset) = crate::selfupdate::testing::publish(site.path(), true);
        let target = bin.path().join("corvo-wan-p2p");
        std::fs::write(&target, "old").unwrap();
        app.runner = app.runner.with_updates(urls, asset, target.clone());
        app.send("/update").await;
        assert!(app.said().last().unwrap().starts_with("v9.0.0 is available"));
        app.send("y").await;
        assert_eq!(app.said().last().unwrap(), "updated to v9.0.0: /quit and start corvo-wan-p2p again to use it");
        assert_ne!(std::fs::read_to_string(&target).unwrap(), "old");
        app.send("/update").await;
        assert_eq!(
            app.said().last().unwrap(),
            "v9.0.0 is installed already: /quit and start corvo-wan-p2p again to use it"
        );
        app.runner.shutdown().await;
    }
}
