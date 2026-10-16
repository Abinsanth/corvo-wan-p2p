//! Windows over the main screen: the welcome after a new or recovered account.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::state::State;
use super::view::wrap;

/// A window over the main screen, and how far down it's scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Welcome { scroll: u16 },
}

/// The widest a window gets: its text, the margins and the border.
pub const WIDTH: u16 = 56;
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
        }
    }

    pub fn scroll(&self) -> u16 {
        match self {
            Overlay::Welcome { scroll } => *scroll,
        }
    }

    fn scrolled_to(self, scroll: u16) -> Overlay {
        match self {
            Overlay::Welcome { .. } => Overlay::Welcome { scroll },
        }
    }

    fn text(&self) -> &'static [&'static str] {
        match self {
            Overlay::Welcome { .. } => WELCOME,
        }
    }
}

/// The window's text as drawn on a terminal of `size`: wrapped to fit
/// when the terminal is narrower than [`WIDTH`].
pub fn rows(overlay: &Overlay, size: (u16, u16)) -> Vec<String> {
    let inner = usize::from(size.0.min(WIDTH).saturating_sub(FRAME));
    overlay.text().iter().flat_map(|line| wrap(line, inner)).collect()
}

/// How many of `total` rows the window shows at once: all of them if there's
/// room, leaving a row above and below it.
pub fn height(size: (u16, u16), total: usize) -> u16 {
    u16::try_from(total).unwrap_or(u16::MAX).min(size.1.saturating_sub(4)).max(1)
}

/// A key while a window is open: Enter or Esc closes it, the arrows and
/// PgUp/PgDn scroll it, and anything else does nothing.
pub fn key(state: &mut State, key: KeyEvent) {
    let Some(overlay) = state.overlay else { return };
    let total = rows(&overlay, state.size).len();
    let page = height(state.size, total);
    let last = u16::try_from(total).unwrap_or(u16::MAX).saturating_sub(page);
    let scroll = overlay.scroll();
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
}
