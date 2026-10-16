//! Every change to the screen's state, one message at a time. It never calls
//! the engine and draws nothing: it returns [`Effect`]s for the loop to carry
//! out, and their results come back as messages.

use std::path::PathBuf;

use corvo_wan_p2p_core::chat::Event;
use corvo_wan_p2p_core::identity::{Code, Seed};
use corvo_wan_p2p_core::store::{Person, StoreError, StoredMessage};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;
use zeroize::Zeroizing;

use super::account::{self, PhraseDone, PhraseUse};
use super::chat::{self, Loaded};
use super::commands::{Cmd, HELP, Input, KEYS, may_hold_a_secret, parse};
use super::complete;
use super::people::{self, Act, Show};
use super::state::{Screen, Secret, State, Tone, WHEEL_STEP};
use super::view::{areas, chat_page, input_rows, popup_area, sidebar_rows, sidebar_scrolled, sidebar_showing, tab_at};

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseEvent),
    Resize(u16, u16),
    /// Four times a second, so flashes go away on time.
    Tick,
    Core(Event),
    /// What came of an [`Effect`].
    Done(Outcome),
    /// The window gained or lost focus.
    Focus(bool),
}

/// Work for the loop: anything that touches the store, the engine, files or
/// the terminal. Its result comes back as [`Msg::Done`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Quit,
    /// Leave, and print this once the terminal is back to normal.
    Exit(String),
    SetTitle(String),
    /// Put this on the clipboard (OSC 52).
    Copy(String),
    /// Unlock the account and start the engine.
    Unlock(Secret),
    /// Make the account. The engine starts later, with [`Effect::Start`].
    Create {
        seed: Seed,
        name: String,
        passphrase: Secret,
    },
    /// Start the engine on the account just made.
    Start,
    /// Delete the account on this computer (`--recover`).
    Wipe,
    /// Whether the recovery file is there.
    CheckRecovery,
    WriteRecovery {
        seed: Seed,
        overwrite: bool,
    },
    ChangePassphrase {
        old: Secret,
        new: Secret,
    },
    /// Check the passphrase, then do `then` with the words.
    Phrase {
        passphrase: Secret,
        then: PhraseUse,
    },
    /// Read everyone and every chat, at start.
    Load,
    Send {
        to: Code,
        text: String,
    },
    /// You've seen everything they sent.
    MarkRead(Code),
    /// Read the `n` messages with `peer` from before the one with id `before`.
    LoadOlder {
        peer: Code,
        before: String,
        n: usize,
    },
    Act {
        code: Code,
        act: Act,
    },
    SetName(String),
    /// Read everyone again, then show `Show`.
    Refresh(Show),
    /// Look for a newer release; with `yes`, install it without asking.
    CheckUpdate {
        yes: bool,
    },
    /// Install the release the last check found.
    InstallUpdate,
}

impl Effect {
    /// What to show while it runs, for the ones that take a moment.
    pub fn busy(&self) -> Option<&'static str> {
        Some(match self {
            Effect::Unlock(_) => "unlocking…",
            Effect::Create { .. } => "making your account…",
            Effect::Start => "starting…",
            Effect::Wipe => "deleting…",
            Effect::ChangePassphrase { .. } => "changing your passphrase…",
            Effect::Phrase { .. } => "checking your passphrase…",
            Effect::CheckUpdate { .. } => "checking for updates…",
            Effect::InstallUpdate => "updating…",
            _ => return None,
        })
    }
}

/// What came of an [`Effect`].
#[derive(Debug)]
pub enum Outcome {
    /// The account is unlocked and the engine is running. `recovery_file` is
    /// where the recovery file is, if it's still there.
    Started {
        name: String,
        code: Code,
        recovery_file: Option<PathBuf>,
    },
    /// Unlocking didn't work.
    Unlock(StoreError),
    Created(Result<(), String>),
    Wiped(Result<(), String>),
    /// Where the recovery file is, if it's there.
    RecoveryFile(Option<PathBuf>),
    RecoveryWritten(Result<PathBuf, String>),
    PassphraseChanged(Result<(), String>),
    Phrase(Result<PhraseDone, String>),
    Loaded(Result<Loaded, String>),
    /// Older messages, and how many of them couldn't be read.
    LoadedOlder {
        peer: Code,
        result: Result<(Vec<StoredMessage>, usize), String>,
    },
    /// The message as saved, waiting to be sent; `text` is what was typed.
    Sent {
        to: Code,
        text: String,
        result: Result<StoredMessage, String>,
    },
    /// Something went wrong that the user should hear about.
    Failed(String),
    /// `people` is everyone, read again after acting (if that worked).
    Acted {
        code: Code,
        act: Act,
        result: Result<(), String>,
        people: Option<Vec<Person>>,
    },
    Named(Result<String, String>),
    Refreshed {
        show: Show,
        result: Result<Vec<Person>, String>,
    },
    /// The newer version found, if any.
    UpdateChecked {
        yes: bool,
        result: Result<Option<String>, String>,
    },
    /// The version now installed.
    Updated(Result<String, String>),
    /// `/update` after an update: this version is installed and waits for a restart.
    UpdatedAlready(String),
    /// Something that leaves the app unusable, like the engine not starting.
    Fatal(String),
}

/// What a key press means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Send,
    Newline,
    Insert(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    NextTab,
    PrevTab,
    GoTab(usize),
    PageUp,
    PageDown,
    Esc,
    KillRow,
    KillWord,
    Clear,
    Quit,
    Complete,
}

impl Action {
    /// Changes the text being typed.
    fn edits(self) -> bool {
        matches!(self, Action::Insert(_) | Action::Backspace | Action::Delete | Action::KillRow | Action::KillWord)
    }
}

/// Shift+Enter needs a terminal that reports it; Alt+Enter and Ctrl-J work everywhere.
fn action_for(key: KeyEvent) -> Option<Action> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    Some(match key.code {
        KeyCode::Enter if shift || alt => Action::Newline,
        KeyCode::Enter => Action::Send,
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Char('l') if ctrl => Action::Clear,
        KeyCode::Char('u') if ctrl => Action::KillRow,
        KeyCode::Char('w') if ctrl => Action::KillWord,
        KeyCode::Char('j') if ctrl => Action::Newline,
        KeyCode::Char(c @ '1'..='9') if alt => Action::GoTab(c as usize - '1' as usize),
        // Windows reports AltGr chords, like @ on a German keyboard, as Ctrl+Alt.
        KeyCode::Char(c) if ctrl && alt => Action::Insert(c),
        KeyCode::Char(_) if ctrl || alt => return None,
        KeyCode::Char(c) => Action::Insert(c),
        KeyCode::Backspace => Action::Backspace,
        KeyCode::Delete => Action::Delete,
        KeyCode::Left if alt => Action::PrevTab,
        KeyCode::Right if alt => Action::NextTab,
        KeyCode::Left => Action::Left,
        KeyCode::Right => Action::Right,
        KeyCode::Home => Action::Home,
        KeyCode::End => Action::End,
        KeyCode::Up => Action::Up,
        KeyCode::Down => Action::Down,
        KeyCode::BackTab => Action::NextTab,
        KeyCode::Tab => Action::Complete,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Esc => Action::Esc,
        _ => return None,
    })
}

pub fn update(state: &mut State, msg: Msg) -> Vec<Effect> {
    let mut effects = Vec::new();
    let resized = matches!(msg, Msg::Resize(..));
    if !matches!(msg, Msg::Tick) {
        state.changes += 1;
    }
    // Typing means the window has focus, even where the terminal doesn't say.
    if matches!(msg, Msg::Key(_) | Msg::Paste(_)) {
        state.focused = true;
    }
    match msg {
        // A key while the words are up only hides them.
        Msg::Key(key) if state.reveal.is_some() && key.kind != KeyEventKind::Release => state.reveal = None,
        Msg::Key(key) => {
            if state.ask().is_some() {
                state.editor.make_room_for_answer();
            }
            if let Some(action) = action_for(key) {
                on_action(state, action, &mut effects);
            }
        }
        Msg::Paste(text) if state.reveal.is_some() => {
            drop(Zeroizing::new(text));
            state.reveal = None;
        }
        Msg::Paste(text) if state.ask().is_some() => {
            state.editor.make_room_for_answer();
            state.editor.insert_str(&one_line_answer(text));
        }
        Msg::Paste(text) => {
            state.popup = None;
            state.editor.insert_str(&text);
        }
        Msg::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => wheel(state, mouse, -(WHEEL_STEP as isize)),
            MouseEventKind::ScrollDown => wheel(state, mouse, WHEEL_STEP as isize),
            MouseEventKind::Down(MouseButton::Left) => {
                state.focused = true;
                click(state, mouse.column, mouse.row);
            }
            _ => {}
        },
        Msg::Resize(w, h) => {
            state.size = (w, h);
            drop_hidden_popup(state);
        }
        Msg::Focus(focused) => state.focused = focused,
        Msg::Tick => {
            if let Some(flash) = &mut state.flash {
                flash.ticks_left = flash.ticks_left.saturating_sub(1);
                if flash.ticks_left == 0 {
                    state.flash = None;
                }
            }
        }
        Msg::Core(event) => on_core(state, event),
        Msg::Done(outcome) => {
            state.busy = None;
            on_done(state, outcome, &mut effects);
        }
    }
    if let Some(busy) = effects.iter().find_map(Effect::busy) {
        state.busy = Some(busy);
    }
    chat::trim(state);
    chat::load_older(state, &mut effects);
    chat::read_open_chat(state, &mut effects);
    if state.open_peer() != state.sidebar_for || resized {
        state.sidebar_for = state.open_peer();
        if let Some(code) = state.sidebar_for {
            state.sidebar_top = sidebar_showing(state, code);
        }
    }
    let title = chat::title(state);
    if state.title != title {
        state.title = title.clone();
        effects.push(Effect::SetTitle(title));
    }
    effects
}

/// A pasted answer as one line: a passphrase copied with its line break still
/// works, and words pasted one per line become one line. Wiped after use.
pub fn one_line_answer(text: String) -> Zeroizing<String> {
    let text = Zeroizing::new(text);
    let trimmed = text.trim_end_matches(['\r', '\n']);
    let mut line = Zeroizing::new(String::with_capacity(trimmed.len()));
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                line.push(' ');
            }
            '\n' => line.push(' '),
            c => line.push(c),
        }
    }
    line
}

/// A left click: on a tab selects it, on a name in the sidebar opens the chat.
fn click(state: &mut State, x: u16, y: u16) {
    let Some(areas) = areas(state.size, input_rows(state)) else { return };
    if !matches!(state.screen, Screen::Main) {
        return;
    }
    // The completion list sits over the sidebar and chat: a click on it is for it.
    if let Some((list, first, rows)) = popup_area(state, areas.input)
        && list.contains(Position::new(x, y))
    {
        if (list.y + 1..list.y + 1 + rows as u16).contains(&y)
            && let Some(popup) = &mut state.popup
        {
            popup.selected = first + usize::from(y - list.y - 1);
            complete::accept(state);
        }
        return;
    }
    if y == areas.tabs.y {
        if let Some(tab) = tab_at(state, x) {
            state.select_tab(tab);
        }
    } else if areas.sidebar.contains(Position::new(x, y))
        && let Some(Some(code)) = sidebar_rows(state).get(usize::from(y - areas.sidebar.y))
    {
        state.open_chat(*code, true);
    }
}

/// The wheel scrolls the sidebar when it's over it, and the chat otherwise.
fn wheel(state: &mut State, mouse: MouseEvent, delta: isize) {
    let over_sidebar = areas(state.size, input_rows(state))
        .is_some_and(|a| a.sidebar.contains(Position::new(mouse.column, mouse.row)));
    if over_sidebar && matches!(state.screen, Screen::Main) {
        state.sidebar_top = sidebar_scrolled(state, delta);
    } else {
        scroll_by(state, delta);
    }
}

fn scroll_by(state: &mut State, delta: isize) {
    let (total, height) = chat_page(state);
    state.scroll.by(delta, total, height);
}

/// A completion list that isn't drawn closes, so keys don't go to it unseen.
fn drop_hidden_popup(state: &mut State) {
    let drawn = areas(state.size, input_rows(state)).is_some_and(|a| popup_area(state, a.input).is_some());
    if !drawn {
        state.popup = None;
    }
}

fn on_action(state: &mut State, action: Action, effects: &mut Vec<Effect>) {
    drop_hidden_popup(state);
    if state.popup.is_some() {
        match action {
            Action::Complete | Action::Down => return complete::step(state, 1),
            Action::Up => return complete::step(state, -1),
            Action::Send => return complete::accept(state),
            Action::Esc => {
                state.popup = None;
                return;
            }
            a if a.edits() => {}
            _ => state.popup = None,
        }
    }
    on_key(state, action, effects);
    if action.edits() {
        complete::refresh(state);
    }
}

fn on_key(state: &mut State, action: Action, effects: &mut Vec<Effect>) {
    let asking = state.ask().is_some();
    let editor = &mut state.editor;
    match action {
        Action::Send => {
            let line = editor.take();
            if let Screen::Onboarding(onboarding) = &mut state.screen {
                effects.extend(onboarding.answer(Secret::new(line), editor));
            } else if asking {
                effects.extend(account::answer(state, Secret::new(line)));
            } else {
                let line = Zeroizing::new(line);
                if !may_hold_a_secret(&line) {
                    state.history.push(&line);
                }
                on_line(state, &line, effects);
            }
        }
        // An answer is one line, earlier lines aren't answers, and nothing in it completes.
        Action::Newline | Action::Up | Action::Down | Action::Complete if asking => {}
        Action::Complete => complete::tab(state),
        Action::Quit => effects.push(Effect::Quit),
        Action::Clear => state.clear_tab(),
        Action::Newline => editor.newline(),
        Action::Insert(c) => editor.insert(c),
        Action::Backspace => editor.backspace(),
        Action::Delete => editor.delete(),
        Action::Left => editor.left(),
        Action::Right => editor.right(),
        Action::Home => editor.home(),
        Action::End => editor.end(),
        Action::KillRow => editor.kill_to_row_start(),
        Action::KillWord => editor.kill_word(),
        Action::Up => {
            if !editor.up()
                && let Some(entry) = state.history.prev(editor.text())
            {
                editor.set(entry);
            }
        }
        Action::Down => {
            if !editor.down()
                && let Some(entry) = state.history.next()
            {
                editor.set(entry);
            }
        }
        Action::NextTab => state.step_tab(1),
        Action::PrevTab => state.step_tab(-1),
        Action::GoTab(i) => state.select_tab(i),
        Action::PageUp => {
            let (total, height) = chat_page(state);
            state.scroll.page_up(total, height);
        }
        Action::PageDown => {
            let (total, height) = chat_page(state);
            state.scroll.page_down(total, height);
        }
        Action::Esc if editor.is_empty() && state.pending.is_some() => account::cancel(state),
        Action::Esc if editor.is_empty() => state.scroll.follow_end(),
        Action::Esc => editor.clear(),
    }
}

/// A line sent with Enter.
fn on_line(state: &mut State, line: &str, effects: &mut Vec<Effect>) {
    match parse(line) {
        Input::Empty => {}
        Input::Text(_) if state.on_corvo() => {
            state.say(Tone::Bad, "this is the corvo tab: switch to a chat (Shift-Tab) to send messages")
        }
        Input::Text(text) => {
            if let Some(to) = state.open_peer() {
                chat::send(state, to, text, effects);
            }
        }
        Input::Wrong(why) => state.say(Tone::Bad, why),
        Input::Cmd(cmd) => on_command(state, cmd, effects),
    }
}

fn on_command(state: &mut State, cmd: Cmd, effects: &mut Vec<Effect>) {
    match cmd {
        Cmd::Help => {
            for (usage, what) in HELP {
                state.say(Tone::Plain, format!("{usage:<18} {what}"));
            }
            state.say(Tone::Plain, KEYS);
            state.select_tab(0);
        }
        Cmd::Clear => state.clear_tab(),
        cmd @ (Cmd::Passphrase | Cmd::Phrase | Cmd::PhraseSave | Cmd::PhraseCopy) => account::begin(state, cmd),
        Cmd::Quit => effects.push(Effect::Quit),
        Cmd::Update { yes } => effects.push(Effect::CheckUpdate { yes }),
        Cmd::Close => {
            if !state.close_tab() {
                state.say(Tone::Bad, "the corvo tab stays open; /quit leaves the app");
            }
        }
        other => people::command(state, other, effects),
    }
}

fn on_done(state: &mut State, outcome: Outcome, effects: &mut Vec<Effect>) {
    match outcome {
        Outcome::Started { name, code, recovery_file } => {
            state.screen = Screen::Main;
            state.short_code = code.short();
            state.code = Some(code);
            state.say(Tone::Good, format!("welcome, {name}. Your code is {code}"));
            state.name = name;
            // "connecting" until the engine says.
            state.online = None;
            effects.push(Effect::Load);
            if let Some(path) = recovery_file {
                state.say(
                    Tone::Warn,
                    format!("your recovery file is still at {}. Move it somewhere safe and delete it.", path.display()),
                );
            }
        }
        Outcome::Fatal(why) => effects.push(Effect::Exit(why)),
        Outcome::Failed(why) => state.say(Tone::Bad, why),
        Outcome::Loaded(result) => chat::loaded(state, result),
        Outcome::LoadedOlder { peer, result } => chat::loaded_older(state, peer, result),
        Outcome::Sent { to, text, result } => chat::sent(state, to, text, result),
        Outcome::Acted { code, act, result, people } => people::acted(state, code, act, result, people),
        Outcome::Named(result) => people::named(state, result),
        Outcome::Refreshed { show, result } => people::refreshed(state, show, result),
        other => match &mut state.screen {
            Screen::Onboarding(onboarding) => effects.extend(onboarding.done(other)),
            Screen::Main => effects.extend(account::done(state, other)),
        },
    }
}

fn on_core(state: &mut State, event: Event) {
    chat::on_event(state, event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::{APP, FLASH_TICKS, Line, Tab};
    use corvo_wan_p2p_core::identity::Seed;
    use ratatui::crossterm::event::KeyEventState;

    fn state() -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        for name in ["rahul", "maria"] {
            state.tabs.push(Tab::chat(Seed::generate().code(), name.into()));
        }
        state
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Msg {
        Msg::Key(KeyEvent::new(code, modifiers))
    }

    fn press(state: &mut State, code: KeyCode) -> Vec<Effect> {
        update(state, key(code, KeyModifiers::NONE))
    }

    fn alt(state: &mut State, code: KeyCode) -> Vec<Effect> {
        update(state, key(code, KeyModifiers::ALT))
    }

    fn ctrl(state: &mut State, c: char) -> Vec<Effect> {
        update(state, key(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn type_text(state: &mut State, text: &str) {
        for c in text.chars() {
            press(state, KeyCode::Char(c));
        }
    }

    fn send(state: &mut State, text: &str) -> Vec<Effect> {
        type_text(state, text);
        press(state, KeyCode::Enter)
    }

    fn last_corvo(state: &State) -> &Line {
        state.tabs[0].lines.last().expect("the corvo tab has lines")
    }

    #[test]
    fn typing_editing_and_alt_enter() {
        let mut s = state();
        type_text(&mut s, "helo");
        press(&mut s, KeyCode::Left);
        type_text(&mut s, "l");
        press(&mut s, KeyCode::End);
        alt(&mut s, KeyCode::Enter);
        type_text(&mut s, "world");
        assert_eq!(s.editor.text(), "hello\nworld");
        update(&mut s, key(KeyCode::Enter, KeyModifiers::SHIFT));
        ctrl(&mut s, 'j');
        assert_eq!(s.editor.text(), "hello\nworld\n\n");
        press(&mut s, KeyCode::Backspace);
        press(&mut s, KeyCode::Backspace);
        ctrl(&mut s, 'w');
        assert_eq!(s.editor.text(), "hello\n");
        press(&mut s, KeyCode::Esc);
        assert!(s.editor.is_empty(), "Esc clears the input");
    }

    #[test]
    fn key_releases_and_unknown_shortcuts_do_nothing() {
        let mut s = state();
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        release.state = KeyEventState::NONE;
        update(&mut s, Msg::Key(release));
        ctrl(&mut s, 'x');
        alt(&mut s, KeyCode::Char('x'));
        assert!(s.editor.is_empty());
    }

    #[test]
    fn up_and_down_browse_sent_lines_after_the_input_rows() {
        let mut s = state();
        send(&mut s, "/help");
        send(&mut s, "/clear");
        type_text(&mut s, "draft");
        press(&mut s, KeyCode::Up);
        assert_eq!(s.editor.text(), "/clear");
        press(&mut s, KeyCode::Up);
        assert_eq!(s.editor.text(), "/help");
        press(&mut s, KeyCode::Down);
        press(&mut s, KeyCode::Down);
        assert_eq!(s.editor.text(), "draft");

        s.editor.set("one\ntwo".into());
        press(&mut s, KeyCode::Up);
        assert_eq!(s.editor.text(), "one\ntwo", "first ↑ moves within the input");
        assert_eq!(s.editor.cursor_pos().0, 0);
    }

    #[test]
    fn every_tab_key() {
        let mut s = state();
        update(&mut s, key(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(s.current, 1, "Shift-Tab goes to the next tab");
        update(&mut s, key(KeyCode::BackTab, KeyModifiers::SHIFT));
        update(&mut s, key(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(s.current, 0, "and wraps around");
        alt(&mut s, KeyCode::Left);
        assert_eq!(s.current, 2, "Alt-← wraps backwards");
        alt(&mut s, KeyCode::Right);
        assert_eq!(s.current, 0);
        alt(&mut s, KeyCode::Char('2'));
        assert_eq!(s.current, 1);
        alt(&mut s, KeyCode::Char('3'));
        assert_eq!(s.current, 2);
        alt(&mut s, KeyCode::Char('9'));
        assert_eq!(s.current, 2, "no ninth tab: nothing changes");
        alt(&mut s, KeyCode::Char('1'));
        assert_eq!(s.current, 0);
    }

    #[test]
    fn switching_tabs_returns_to_the_newest_lines() {
        let mut s = state();
        for i in 0..100 {
            s.say(Tone::Plain, format!("line {i}"));
        }
        press(&mut s, KeyCode::PageUp);
        assert!(s.scroll.is_scrolled());
        alt(&mut s, KeyCode::Right);
        alt(&mut s, KeyCode::Left);
        assert!(!s.scroll.is_scrolled());
    }

    #[test]
    fn page_keys_and_the_wheel_scroll() {
        let mut s = state();
        for i in 0..100 {
            s.say(Tone::Plain, format!("line {i}"));
        }
        let (total, height) = chat_page(&s);
        press(&mut s, KeyCode::PageUp);
        assert_eq!(s.scroll.top(total, height), total - 2 * height);
        press(&mut s, KeyCode::PageDown);
        assert!(!s.scroll.is_scrolled());
        let wheel = |kind| Msg::Mouse(MouseEvent { kind, column: 30, row: 5, modifiers: KeyModifiers::NONE });
        update(&mut s, wheel(MouseEventKind::ScrollUp));
        assert_eq!(s.scroll.top(total, height), total - height - WHEEL_STEP);
        update(&mut s, wheel(MouseEventKind::ScrollDown));
        assert!(!s.scroll.is_scrolled());
        update(&mut s, wheel(MouseEventKind::ScrollUp));
        press(&mut s, KeyCode::Esc);
        assert!(!s.scroll.is_scrolled(), "Esc on an empty input goes back to the bottom");
    }

    #[test]
    fn clear_and_ctrl_l_empty_the_open_tab() {
        let mut s = state();
        s.say(Tone::Plain, "old");
        send(&mut s, "/clear");
        assert_eq!(s.tabs[0].cleared, 1);
        alt(&mut s, KeyCode::Char('2'));
        let peer = s.tabs[1].peer.unwrap();
        s.chats.entry(peer).or_default().messages.push(corvo_wan_p2p_core::store::StoredMessage {
            id: "01".into(),
            sent_at: 0,
            text: "hi".into(),
            mark: corvo_wan_p2p_core::store::Mark::Received,
        });
        ctrl(&mut s, 'l');
        assert_eq!(s.tabs[1].cleared, 1);
        assert_eq!(s.tabs[0].lines.len(), 1, "nothing is deleted");
    }

    #[test]
    fn quit_and_ctrl_c() {
        let mut s = state();
        assert!(send(&mut s, "/quit").contains(&Effect::Quit));
        assert!(send(&mut s, "/q").contains(&Effect::Quit));
        assert!(ctrl(&mut s, 'c').contains(&Effect::Quit));
        assert!(!send(&mut s, "/help").contains(&Effect::Quit));
    }

    #[test]
    fn pasting_keeps_line_breaks_as_newlines() {
        let mut s = state();
        type_text(&mut s, "> ");
        update(&mut s, Msg::Paste("one\r\ntwo\rthree\n".into()));
        assert_eq!(s.editor.text(), "> one\ntwo\nthree\n");
        type_text(&mut s, "!");
        assert!(s.editor.text().ends_with("\n!"), "the cursor is after the paste");
    }

    #[test]
    fn help_lists_every_command_in_the_corvo_tab() {
        let mut s = state();
        alt(&mut s, KeyCode::Char('2'));
        send(&mut s, "/h");
        assert_eq!(s.current, 0, "help opens the corvo tab");
        let text: Vec<&str> = s.tabs[0].lines.iter().map(|l| l.text.as_str()).collect();
        for usage in ["/add <code>", "/accept [name]", "/phrase copy", "/quit  /q"] {
            assert!(text.iter().any(|l| l.starts_with(usage)), "{usage} is listed");
        }
        assert!(text.last().unwrap().contains("Alt+Enter"));
    }

    #[test]
    fn close_closes_a_chat_but_not_corvo() {
        let mut s = state();
        alt(&mut s, KeyCode::Char('3'));
        send(&mut s, "/close");
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.current, 1, "the tab before it opens");
        send(&mut s, "/close");
        send(&mut s, "/close");
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(last_corvo(&s).tone, Tone::Bad);
        assert!(last_corvo(&s).text.contains("corvo tab stays open"));
    }

    #[test]
    fn mistakes_are_said_and_flashed() {
        let mut s = state();
        send(&mut s, "/acept");
        assert_eq!(last_corvo(&s).text, "unknown command /acept: did you mean /accept?");
        assert_eq!(s.flash.as_ref().unwrap().line, *last_corvo(&s));
        s.select_tab(0);
        send(&mut s, "hello");
        assert!(last_corvo(&s).text.starts_with("this is the corvo tab"));
        let before = s.tabs[0].lines.len();
        send(&mut s, "   ");
        assert_eq!(s.tabs[0].lines.len(), before, "an empty line does nothing");
    }

    #[test]
    fn a_flash_goes_away_after_its_ticks() {
        let mut s = state();
        send(&mut s, "/nope");
        for _ in 1..FLASH_TICKS {
            update(&mut s, Msg::Tick);
        }
        assert!(s.flash.is_some());
        update(&mut s, Msg::Tick);
        assert!(s.flash.is_none());
    }

    #[test]
    fn the_title_is_set_once() {
        let mut s = state();
        assert_eq!(update(&mut s, Msg::Tick), vec![Effect::SetTitle(APP.into())]);
        assert_eq!(update(&mut s, Msg::Tick), vec![]);
    }

    #[test]
    fn network_and_problems_from_the_engine() {
        let mut s = state();
        assert_eq!(s.online, None);
        update(&mut s, Msg::Core(Event::Network { online: true }));
        assert_eq!(s.online, Some(true));
        update(&mut s, Msg::Core(Event::Network { online: false }));
        assert_eq!(s.online, Some(false));
        update(&mut s, Msg::Core(Event::Problem("the store is full".into())));
        assert_eq!(last_corvo(&s).text, "the store is full");
        assert_eq!(last_corvo(&s).tone, Tone::Bad);
    }

    #[test]
    fn a_passphrase_typed_after_the_command_is_not_kept() {
        let mut s = state();
        send(&mut s, "/help");
        for line in
            ["/passphrase old-secret new-secret", "/PASSPHRASE old-secret", "  /phrase hunter2", "/phrase  copy now"]
        {
            send(&mut s, line);
            assert!(!last_corvo(&s).text.contains("secret") && !last_corvo(&s).text.contains("hunter2"));
        }
        assert_eq!(s.ask(), None, "nothing is asked");
        assert!(s.tabs[0].lines.iter().any(|l| l.text == "type /passphrase alone; it asks for them hidden"));
        assert_eq!(
            last_corvo(&s).text,
            "type /phrase alone, or /phrase save or /phrase copy; it asks for your passphrase hidden"
        );
        let mut kept = Vec::new();
        while let Some(entry) = s.history.prev("") {
            kept.push(entry);
        }
        assert_eq!(kept, vec!["/help"], "only the line without a passphrase in it");
    }

    #[test]
    fn an_unsent_passphrase_line_is_not_kept_as_a_draft() {
        let mut s = state();
        send(&mut s, "/help");
        type_text(&mut s, "/passphrase old-secret new-secret");
        press(&mut s, KeyCode::Up);
        assert_eq!(s.editor.text(), "/help");
        press(&mut s, KeyCode::Down);
        assert_eq!(s.editor.text(), "", "the line with a passphrase in it wasn't kept");
    }

    #[test]
    fn editor_does_not_reallocate_while_typing_an_answer() {
        let mut s = state();
        send(&mut s, "/passphrase");
        for question in 0..3 {
            press(&mut s, KeyCode::Char('x'));
            let (at, room) = (s.editor.text().as_ptr(), s.editor.capacity());
            type_text(&mut s, &"y".repeat(300));
            update(&mut s, Msg::Paste("pasted-part ".repeat(20)));
            assert_eq!(s.editor.text().as_ptr(), at, "question {question}: the buffer never moved");
            assert_eq!(s.editor.capacity(), room);
            press(&mut s, KeyCode::Enter);
        }
    }

    #[test]
    fn secret_text_is_wiped_after_use() {
        use zeroize::Zeroizing;
        let pasted: Zeroizing<String> = one_line_answer("my pass\r\nphrase\n".into());
        assert_eq!(pasted.as_str(), "my pass phrase");
        let _: Zeroizing<String> = crate::ui::clip::osc52("words");
        let _: Vec<Zeroizing<String>> = crate::ui::view::word_grid(&["abandon", "ability"], 80);
        let _: Zeroizing<String> = crate::ui::onboarding::lowered(&Secret::new(" Pass ".into()));
        assert_eq!(crate::ui::onboarding::lowered(&Secret::new(" Pass ".into())).as_str(), "pass");
    }

    #[test]
    fn a_list_that_is_not_drawn_takes_no_keys() {
        let mut s = state();
        s.set_people(vec![
            corvo_wan_p2p_core::store::Person {
                code: Seed::generate().code(),
                name: "rahul".into(),
                last_seen: None,
                standing: corvo_wan_p2p_core::store::Standing::Contact,
            },
            corvo_wan_p2p_core::store::Person {
                code: Seed::generate().code(),
                name: "rani".into(),
                last_seen: None,
                standing: corvo_wan_p2p_core::store::Standing::Contact,
            },
        ]);
        send(&mut s, "/help");
        type_text(&mut s, "/chat r");
        press(&mut s, KeyCode::Tab);
        assert!(s.popup.is_some());
        update(&mut s, Msg::Resize(80, 8));
        assert_eq!(s.popup, None, "a resize to too small clears it");

        update(&mut s, Msg::Resize(80, 24));
        press(&mut s, KeyCode::Tab);
        assert!(s.popup.is_some());
        // Drawn nowhere, as if the window shrank without saying.
        s.size = (80, 8);
        press(&mut s, KeyCode::Up);
        assert_eq!(s.popup, None);
        assert_eq!(s.editor.text(), "/help", "↑ browses the history as if there were no list");
    }

    #[test]
    fn resize_changes_the_page_size() {
        let mut s = state();
        update(&mut s, Msg::Resize(100, 40));
        assert_eq!(s.size, (100, 40));
        assert_eq!(chat_page(&s).1, 40 - 5);
    }
}
