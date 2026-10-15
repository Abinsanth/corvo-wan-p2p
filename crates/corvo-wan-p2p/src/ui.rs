//! The full-screen UI: owns the terminal while it runs and gives it back
//! cleanly on every exit, including panics. One loop merges terminal input, a
//! tick and messages from elsewhere, feeds them to `update` and draws.

pub mod account;
pub mod commands;
pub mod onboarding;
pub mod state;
pub mod update;
pub mod view;

use std::io::{Write, stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Context;
use corvo_wan_p2p_core::chat::Event;
use corvo_wan_p2p_core::identity::Seed;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, Event as TermEvent, KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::crossterm::{Command, execute};
use tokio::sync::mpsc;

use self::state::{Line, Peer, State, Tab, Tone};
use self::update::{Effect, Msg, update};

/// How long the input thread waits for a key before checking whether to stop.
const POLL: Duration = Duration::from_millis(100);
const TICK: Duration = Duration::from_millis(250);

/// Disambiguated keys (so Shift/Alt+Enter are distinguishable) with shifted
/// characters reported as typed. Not REPORT_ALL_KEYS: it would send unshifted keys.
const KEYBOARD_FLAGS: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES.union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS);

/// xterm's title stack: save the window title on entry and put it back on exit.
/// Terminals without one ignore both.
const SAVE_TITLE: &str = "\x1b[22;0t";
const RESTORE_TITLE: &str = "\x1b[23;0t";

/// What puts the terminal back the way the shell expects, as escape codes.
/// Written as plain bytes so it's the same on every system and can be tested.
fn restore_sequence() -> String {
    let mut out = String::new();
    let commands: [&dyn Fn(&mut String) -> std::fmt::Result; 6] = [
        &|s| PopKeyboardEnhancementFlags.write_ansi(s),
        &|s| DisableBracketedPaste.write_ansi(s),
        &|s| DisableMouseCapture.write_ansi(s),
        &|s| DisableFocusChange.write_ansi(s),
        &|s| LeaveAlternateScreen.write_ansi(s),
        // The title we set mustn't outlive us where there's no title stack.
        &|s| SetTitle("").write_ansi(s),
    ];
    for write in commands {
        let _ = write(&mut out);
    }
    out.push_str(RESTORE_TITLE);
    out
}

/// Best-effort and safe to repeat.
fn restore_terminal() {
    let mut out = stdout();
    let _ = out.write_all(restore_sequence().as_bytes()).and_then(|()| out.flush());
    let _ = disable_raw_mode();
}

/// Restores the terminal before a panic message prints, so it stays readable.
/// Only for a panic on the UI thread: one in a tokio task is caught there and
/// the UI keeps running.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    let ui_thread = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == ui_thread {
            restore_terminal();
        }
        previous(info);
    }));
}

/// Owns the terminal setup. Dropping it, also while a panic unwinds, writes
/// the restore sequence and leaves raw mode.
struct Guard<W: Write> {
    out: W,
    raw: bool,
}

impl Guard<std::io::Stdout> {
    fn enter() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut guard = Guard { out: stdout(), raw: true };
        guard.out.write_all(SAVE_TITLE.as_bytes())?;
        execute!(guard.out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
        if supports_keyboard_enhancement().unwrap_or(false) {
            execute!(guard.out, PushKeyboardEnhancementFlags(KEYBOARD_FLAGS))?;
        }
        Ok(guard)
    }
}

impl<W: Write> Drop for Guard<W> {
    fn drop(&mut self) {
        let _ = self.out.write_all(restore_sequence().as_bytes()).and_then(|()| self.out.flush());
        if self.raw {
            let _ = disable_raw_mode();
        }
    }
}

/// Reads terminal events on its own thread. Stopped and joined before the
/// terminal is given back, so nothing reads it afterwards.
struct InputThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl InputThread {
    fn start(tx: mpsc::UnboundedSender<TermEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match event::poll(POLL) {
                    Ok(true) => {
                        let Ok(ev) = event::read() else { return };
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        });
        InputThread { stop, handle: Some(handle) }
    }
}

impl Drop for InputThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The terminal events `update` cares about. Mouse moves are dropped so they
/// don't cause redraws.
fn to_msg(ev: TermEvent) -> Option<Msg> {
    match ev {
        TermEvent::Key(key) => Some(Msg::Key(key)),
        TermEvent::Paste(text) => Some(Msg::Paste(text)),
        TermEvent::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) => {
            Some(Msg::Mouse(m))
        }
        TermEvent::Resize(w, h) => Some(Msg::Resize(w, h)),
        _ => None,
    }
}

/// Runs the UI until it quits. `msgs` brings everything that isn't the
/// terminal: engine events now, and effect results later.
pub async fn run(mut state: State, mut msgs: mpsc::UnboundedReceiver<Msg>) -> anyhow::Result<()> {
    // Declared in this order so they drop in reverse: the input thread stops
    // before the guard gives the terminal back.
    let _guard = Guard::enter().context("couldn't set up the terminal")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let (term_tx, mut term_rx) = mpsc::unbounded_channel();
    let _input = InputThread::start(term_tx);
    let size = terminal.size()?;
    let mut next = Some(Msg::Resize(size.width, size.height));
    let mut tick = tokio::time::interval(TICK);
    loop {
        if let Some(msg) = next.take() {
            for effect in update(&mut state, msg) {
                match effect {
                    Effect::Quit => return Ok(()),
                    Effect::SetTitle(title) => execute!(stdout(), SetTitle(title))?,
                    _ => {}
                }
            }
        }
        terminal.draw(|frame| view::draw(frame, &state))?;
        next = tokio::select! {
            ev = term_rx.recv() => match ev {
                Some(ev) => to_msg(ev),
                None => anyhow::bail!("the terminal stopped sending input"),
            },
            Some(msg) = msgs.recv() => Some(msg),
            _ = tick.tick() => Some(Msg::Tick),
        };
    }
}

/// `--demo-ui`: the screen with made-up people and chats, to try it by hand
/// before there's an account to unlock.
pub async fn demo() -> anyhow::Result<()> {
    let me = Seed::generate().code();
    let mut state = State::new("you".into(), me.short());
    let people = [("rahul", true, false), ("ana maria", false, false), ("bob·m4x8", false, true)];
    for (label, online, request) in people {
        state.people.push(Peer { code: Seed::generate().code(), label: label.into(), online, request });
    }
    let chat = |peer: &Peer, lines: &[&str]| {
        let mut tab = Tab::chat(peer.code, peer.label.clone());
        tab.lines = lines.iter().map(|text| Line { text: text.to_string(), tone: Tone::Plain }).collect();
        tab
    };
    let rahul = chat(&state.people[0], &["rahul: hi!", "you: hey, this is the demo", "rahul: two\nlines"]);
    let bob = chat(&state.people[2], &["bob: \x1b[2Jthis tried to clear your screen \u{202E}and flip text"]);
    state.tabs.extend([rahul, bob]);
    state.say(Tone::Good, "demo: nothing here is sent anywhere. /help lists commands, /quit or Ctrl-C leaves.");

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = tx.send(Msg::Core(Event::Network { online: true }));
    });
    run(state, rx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A writer the test can read back after the guard is gone.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_terminal_is_restored_after_a_panic() {
        let out = Shared::default();
        let written = out.clone();
        let result = std::panic::catch_unwind(move || {
            let _guard = Guard { out, raw: false };
            panic!("something broke while drawing");
        });
        assert!(result.is_err());
        let written = String::from_utf8(written.0.lock().unwrap().clone()).unwrap();
        assert_eq!(written, restore_sequence());
        for part in ["\x1b[?1049l", "\x1b[?2004l", "\x1b[?1000l", "\x1b[?1004l", RESTORE_TITLE] {
            assert!(written.contains(part), "{part:?} is written");
        }
    }

    #[test]
    fn keyboard_flags_report_shifted_characters() {
        use KeyboardEnhancementFlags as F;
        assert!(KEYBOARD_FLAGS.contains(F::DISAMBIGUATE_ESCAPE_CODES));
        assert!(KEYBOARD_FLAGS.contains(F::REPORT_ALTERNATE_KEYS));
        assert!(!KEYBOARD_FLAGS.contains(F::REPORT_ALL_KEYS_AS_ESCAPE_CODES));
    }

    #[test]
    fn mouse_moves_are_dropped_but_the_wheel_is_kept() {
        use ratatui::crossterm::event::{KeyModifiers, MouseEvent};
        let mouse = |kind| TermEvent::Mouse(MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE });
        assert!(to_msg(mouse(MouseEventKind::Moved)).is_none());
        assert!(matches!(to_msg(mouse(MouseEventKind::ScrollUp)), Some(Msg::Mouse(_))));
        assert!(matches!(to_msg(TermEvent::Resize(10, 5)), Some(Msg::Resize(10, 5))));
        assert!(to_msg(TermEvent::FocusLost).is_none());
    }
}
