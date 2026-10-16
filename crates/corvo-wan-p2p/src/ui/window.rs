//! Windows over the main screen: the welcome after a new or recovered
//! account, and the help.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::commands::{HELP, KEYS, MARKS};
use super::state::State;
use super::view::wrap;

/// A window over the main screen, and how far down it's scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Welcome { scroll: u16 },
    Help { scroll: u16 },
}

/// Where a help line's description starts: after a two-space indent and
/// the command (or key), padded.
const DESCRIBED_AT: usize = 23;
/// Columns a window's text leaves for its border and margins: two at the
/// left, at least two at the right.
const FRAME: u16 = 6;

const WELCOME: &[&str] = &[
    "Chat with friends directly, computer to computer.",
    "",
    "• Your code is your address. Share it freely:",
    "  /code shows it and copies it.",
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

/// The help in `inner` columns: each group's heading, then its commands with
/// their descriptions, which wrap under themselves; then the keys and marks.
fn help_rows(inner: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut section = |title: &str, items: &[(&str, &str)]| {
        rows.push(title.to_string());
        for (name, what) in items {
            let width = inner.saturating_sub(DESCRIBED_AT).max(12);
            for (i, part) in wrap(what, width).into_iter().enumerate() {
                let head =
                    if i == 0 { format!("  {name:<w$}", w = DESCRIBED_AT - 2) } else { " ".repeat(DESCRIBED_AT) };
                rows.push(format!("{head}{part}"));
            }
        }
    };
    for (group, commands) in HELP {
        section(group, commands);
    }
    section("", &[]);
    section("Keys", KEYS);
    section("Marks", MARKS);
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

/// A key while a window is open: Enter or Esc closes it, the arrows and
/// PgUp/PgDn scroll it, and anything else does nothing.
pub fn key(state: &mut State, key: KeyEvent) {
    let Some(overlay) = state.overlay else { return };
    let total = rows(&overlay, state.size).len();
    let page = height(state.size, total);
    let last = last(state.size, total);
    let scroll = scroll(&overlay, state.size);
    state.overlay = match key.code {
        KeyCode::Enter | KeyCode::Esc => None,
        KeyCode::Up => Some(overlay.scrolled_to(scroll.saturating_sub(1))),
        KeyCode::Down => Some(overlay.scrolled_to(scroll.saturating_add(1).min(last))),
        KeyCode::PageUp => Some(overlay.scrolled_to(scroll.saturating_sub(page))),
        KeyCode::PageDown => Some(overlay.scrolled_to(scroll.saturating_add(page).min(last))),
        _ => Some(overlay),
    };
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
        let help = rows(&Overlay::Help { scroll: 0 }, (80, 24));
        let at = help.iter().position(|r| r.contains("/code")).expect("listed");
        let indent = help[at].find("show").expect("described");
        assert!(help[at + 1].starts_with(&" ".repeat(indent)), "{:?}", &help[at..at + 2]);
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
