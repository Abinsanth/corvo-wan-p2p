//! Windows over the main screen: the welcome after a new or recovered
//! account, and the help.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::commands::{HELP, KEYS};
use super::state::State;
use super::view::wrap;
use unicode_width::UnicodeWidthStr;

/// A window over the main screen, and how far down it's scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Welcome { scroll: u16 },
    Help { scroll: u16 },
}

/// The help's columns: the group, then the command (or key), then what it
/// does. Narrower than `GROUP + COMMAND + DESCRIPTION`, each group's name
/// goes on a row of its own instead.
const GROUP: usize = 11;
const COMMAND: usize = 18;
const DESCRIPTION: usize = 12;
/// Columns a window's text leaves for its border and margins: two at the
/// left, at least two at the right.
const FRAME: u16 = 6;

const WELCOME: &[&str] = &[
    "Chat with friends directly, computer to computer.",
    "",
    "• Your code is your address. Share it freely:",
    "  /code shows it and can save it to a file.",
    "• Your 12 words are your account. Never share",
    "  them, and keep a copy off this computer.",
    "• To add a friend: /add <their code>, then say hi.",
    "• Messages travel when you're both online. Until",
    "  then they wait here:  · waiting  ✓ delivered",
    "  ✓✓ read",
    "• Encrypted end to end, and no server keeps your",
    "  chats. Friends you connect with can see your IP.",
    "",
    "            /help any time · Enter to start",
];

impl Overlay {
    pub fn title(&self) -> &'static str {
        match self {
            Overlay::Welcome { .. } => "─ Welcome to corvo ",
            Overlay::Help { .. } => "─ Help ",
        }
    }

    /// What the top border says at its right end, if anything.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Overlay::Welcome { .. } => None,
            Overlay::Help { .. } => Some(" Esc closes ─"),
        }
    }

    /// The widest it gets: its text, the margins and the border.
    pub fn width(&self) -> u16 {
        match self {
            Overlay::Welcome { .. } => 56,
            Overlay::Help { .. } => 80,
        }
    }

    pub fn scroll(&self) -> u16 {
        match self {
            Overlay::Welcome { scroll } | Overlay::Help { scroll } => *scroll,
        }
    }

    fn scrolled_to(self, scroll: u16) -> Overlay {
        match self {
            Overlay::Welcome { .. } => Overlay::Welcome { scroll },
            Overlay::Help { .. } => Overlay::Help { scroll },
        }
    }
}

/// The window's text as drawn on a terminal of `size`: wrapped to fit
/// when the terminal is narrower than the window.
pub fn rows(overlay: &Overlay, size: (u16, u16)) -> Vec<String> {
    let inner = usize::from(size.0.min(overlay.width()).saturating_sub(FRAME));
    match overlay {
        Overlay::Welcome { .. } => WELCOME.iter().flat_map(|line| wrap(line, inner)).collect(),
        Overlay::Help { .. } => help_rows(inner),
    }
}

/// The help in `inner` columns: each group with its commands and what they
/// do, which wraps under itself; then the keys.
fn help_rows(inner: usize) -> Vec<String> {
    let wide = inner >= GROUP + COMMAND + DESCRIPTION;
    let indent = if wide { GROUP } else { 2 };
    let mut rows = Vec::new();
    for (group, items) in HELP.iter().chain([("Keys", KEYS)].iter()) {
        if !wide {
            rows.push(group.to_string());
        }
        for (i, (name, what)) in items.iter().enumerate() {
            let lead = if wide && i == 0 { format!("{group:<GROUP$}") } else { " ".repeat(indent) };
            // A long key gets two spaces after it, past the column.
            let column = COMMAND.max(name.width() + 2);
            let width = inner.saturating_sub(indent + column).max(DESCRIPTION);
            for (j, part) in wrap(what, width).into_iter().enumerate() {
                let head = if j == 0 {
                    format!("{lead}{name}{}", " ".repeat(column - name.width()))
                } else {
                    " ".repeat(indent + column)
                };
                rows.push(format!("{head}{part}"));
            }
        }
    }
    rows
}

/// How many of `total` rows the window shows at once: all of them if there's
/// room, leaving a row above and below it.
pub fn height(size: (u16, u16), total: usize) -> u16 {
    u16::try_from(total).unwrap_or(u16::MAX).min(size.1.saturating_sub(4)).max(1)
}

/// How far down the window shows at `size`: as scrolled, but never past its
/// end, which moves when the terminal is resized.
pub fn scroll(overlay: &Overlay, size: (u16, u16)) -> u16 {
    overlay.scroll().min(last(size, rows(overlay, size).len()))
}

/// The furthest `total` rows can scroll at `size`.
fn last(size: (u16, u16), total: usize) -> u16 {
    u16::try_from(total).unwrap_or(u16::MAX).saturating_sub(height(size, total))
}

/// Whether the window has rows below what it shows at `size`.
pub fn more_below(overlay: &Overlay, size: (u16, u16)) -> bool {
    scroll(overlay, size) < last(size, rows(overlay, size).len())
}

/// A key while a window is open: Enter or Esc closes it, the arrows and
/// PgUp/PgDn scroll it, and anything else does nothing.
pub fn key(state: &mut State, key: KeyEvent) {
    let Some(overlay) = state.overlay else { return };
    let page = height(state.size, rows(&overlay, state.size).len()) as isize;
    match key.code {
        KeyCode::Enter | KeyCode::Esc => state.overlay = None,
        KeyCode::Up => scroll_by(state, -1),
        KeyCode::Down => scroll_by(state, 1),
        KeyCode::PageUp => scroll_by(state, -page),
        KeyCode::PageDown => scroll_by(state, page),
        _ => {}
    }
}

/// Scrolls the open window by `delta` rows, within its text.
pub fn scroll_by(state: &mut State, delta: isize) {
    let Some(overlay) = state.overlay else { return };
    let last = last(state.size, rows(&overlay, state.size).len());
    let to = (scroll(&overlay, state.size) as isize).saturating_add(delta).clamp(0, last as isize);
    state.overlay = Some(overlay.scrolled_to(u16::try_from(to).unwrap_or(last)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::onboarding::Onboarding;
    use crate::ui::state::State;
    use crate::ui::update::{Effect, Msg, Outcome, update};
    use corvo_wan_p2p_core::identity::Seed;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(state: &mut State, code: KeyCode) -> Vec<Effect> {
        update(state, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn send_line(state: &mut State, text: &str) {
        for c in text.chars() {
            key(state, KeyCode::Char(c));
        }
        key(state, KeyCode::Enter);
    }

    fn started(state: &mut State) {
        let code = Seed::generate().code();
        update(state, Msg::Done(Outcome::Started { name: "ana".into(), code, recovery_file: None }));
    }

    fn main_screen() -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.size = (80, 24);
        state
    }

    #[test]
    fn the_welcome_shows_after_a_new_or_recovered_account() {
        for recovered in [false, true] {
            let mut s = State::onboarding(Onboarding::at_code(Seed::generate().code(), recovered));
            assert!(key(&mut s, KeyCode::Enter).contains(&Effect::Start));
            started(&mut s);
            assert_eq!(s.overlay, Some(Overlay::Welcome { scroll: 0 }));
        }
    }

    #[test]
    fn the_welcome_does_not_show_after_an_unlock() {
        let mut s = State::onboarding(Onboarding::start(true, false));
        started(&mut s);
        assert_eq!(s.overlay, None);
    }

    #[test]
    fn slash_welcome_opens_it_and_enter_or_esc_closes_it() {
        for close in [KeyCode::Esc, KeyCode::Enter] {
            let mut s = main_screen();
            send_line(&mut s, "/welcome");
            assert_eq!(s.overlay, Some(Overlay::Welcome { scroll: 0 }));
            key(&mut s, close);
            assert_eq!(s.overlay, None);
        }
    }

    #[test]
    fn keys_typed_while_the_welcome_is_open_do_not_reach_the_input() {
        let mut s = main_screen();
        s.overlay = Some(Overlay::Welcome { scroll: 0 });
        for c in "hi".chars() {
            key(&mut s, KeyCode::Char(c));
        }
        assert_eq!(s.editor.text(), "");
        assert!(s.overlay.is_some());
    }

    #[test]
    fn the_welcome_scrolls_only_as_far_as_it_needs() {
        let mut s = main_screen();
        s.overlay = Some(Overlay::Welcome { scroll: 0 });
        key(&mut s, KeyCode::Down);
        assert_eq!(s.overlay, Some(Overlay::Welcome { scroll: 0 }), "it fits on 80×24");

        s.size = (80, 10);
        for _ in 0..3 {
            key(&mut s, KeyCode::PageDown);
        }
        let Some(overlay @ Overlay::Welcome { scroll }) = s.overlay else { panic!("closed") };
        let total = rows(&overlay, s.size).len();
        assert_eq!(usize::from(scroll), total - usize::from(height(s.size, total)), "at the end, no further");
        key(&mut s, KeyCode::Up);
        assert_eq!(s.overlay, Some(Overlay::Welcome { scroll: scroll - 1 }));
    }

    #[test]
    fn help_opens_a_window_and_prints_nothing() {
        for line in ["/help", "/h"] {
            let mut s = main_screen();
            let said = s.tabs[0].lines.len();
            send_line(&mut s, line);
            assert_eq!(s.overlay, Some(Overlay::Help { scroll: 0 }));
            assert_eq!(s.tabs[0].lines.len(), said, "nothing printed into the corvo tab");
            key(&mut s, KeyCode::Esc);
            assert_eq!(s.overlay, None);
        }
    }

    #[test]
    fn the_help_scrolls_on_a_short_terminal() {
        let mut s = main_screen();
        s.size = (80, 12);
        s.overlay = Some(Overlay::Help { scroll: 0 });
        for _ in 0..20 {
            key(&mut s, KeyCode::PageDown);
        }
        let Some(overlay @ Overlay::Help { scroll }) = s.overlay else { panic!("closed") };
        let total = rows(&overlay, s.size).len();
        assert!(scroll > 0);
        assert_eq!(usize::from(scroll), total - usize::from(height(s.size, total)));
    }

    #[test]
    fn a_long_help_line_wraps_under_its_description() {
        let help = help_rows(GROUP + COMMAND + DESCRIPTION);
        let at = help.iter().position(|r| r.contains("/contacts")).expect("listed");
        assert!(help[at].ends_with("  contacts and"), "{:?}", &help[at..at + 2]);
        let indent = help[at].find("contacts and").expect("described");
        assert_eq!(help[at + 1], format!("{}requests", " ".repeat(indent)));
    }

    #[test]
    fn the_help_reads_as_decided() {
        let help = help_rows(74);
        for row in [
            "You        /name <new name>  your display name",
            "Friends    /code             your code",
            "           /add <code>       add a friend",
            "Chatting   /chat <name>      open a chat",
            "App        /update           get the newest version",
            "           /quit  /q         quit (Ctrl-C)",
            "Keys       Tab               complete",
            "           Shift-Tab, Alt-1..9  switch tabs",
            "           ↑/↓               what you typed before",
        ] {
            assert!(help.iter().any(|r| r == row), "{row:?} missing: {help:#?}");
        }
    }

    #[test]
    fn a_narrow_help_puts_each_group_on_its_own_row() {
        let help = help_rows(34);
        assert_eq!(help[0], "You");
        assert!(help[1].starts_with("  /name <new name>"), "{help:#?}");
        assert!(help.iter().any(|r| r == "Keys"));
        assert!(help.iter().all(|r| r.width() <= 34), "{help:#?}");
    }

    #[test]
    fn the_wheel_scrolls_a_window_and_clicks_do_nothing() {
        use crate::ui::state::WHEEL_STEP;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mouse = |kind| Msg::Mouse(MouseEvent { kind, column: 30, row: 10, modifiers: KeyModifiers::NONE });
        let mut s = main_screen();
        s.overlay = Some(Overlay::Help { scroll: 0 });
        update(&mut s, mouse(MouseEventKind::ScrollDown));
        assert_eq!(s.overlay, Some(Overlay::Help { scroll: WHEEL_STEP as u16 }));
        update(&mut s, mouse(MouseEventKind::ScrollUp));
        assert_eq!(s.overlay, Some(Overlay::Help { scroll: 0 }));
        let tab = s.current;
        update(&mut s, mouse(MouseEventKind::Down(MouseButton::Left)));
        assert_eq!(s.overlay, Some(Overlay::Help { scroll: 0 }));
        assert_eq!(s.current, tab);
    }

    #[test]
    fn a_bigger_terminal_brings_a_scrolled_window_back_into_view() {
        let mut s = main_screen();
        s.overlay = Some(Overlay::Welcome { scroll: 13 });
        key(&mut s, KeyCode::Up);
        assert_eq!(s.overlay, Some(Overlay::Welcome { scroll: 0 }), "it all fits on 80×24 now");
        s.overlay = Some(Overlay::Welcome { scroll: 13 });
        key(&mut s, KeyCode::Down);
        assert_eq!(s.overlay, Some(Overlay::Welcome { scroll: 0 }));
    }
}
