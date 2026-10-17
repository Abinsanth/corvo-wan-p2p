//! The Profile tab: your name and code, and a menu for your account. Each
//! item opens a window with its own fields (see `form`).

use super::form::Form;
use super::state::{Line, State, Tone};
use super::window::Overlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Name,
    Passphrase,
    Words,
    DeleteAccount,
}

/// The menu, in order.
pub const MENU: &[(&str, MenuItem)] = &[
    ("Change name", MenuItem::Name),
    ("Change passphrase", MenuItem::Passphrase),
    ("Show 12 words", MenuItem::Words),
    ("Delete account", MenuItem::DeleteAccount),
];

const HINT: &str = "↑/↓ choose · Enter open · Esc close";

/// What the Profile tab shows: your name and code, then the menu with the
/// chosen item marked `›` (Delete account in red), then how to use it.
pub fn lines(state: &State) -> Vec<Line> {
    let line = |text: String, tone| Line { text, tone };
    let code = state.code.map_or_else(String::new, |c| c.to_string());
    // The code on a line of its own: it's longer than most chats are wide.
    let mut lines = vec![
        line(format!("Name: {}", state.name), Tone::Plain),
        line("Code:".into(), Tone::Plain),
        line(code, Tone::Plain),
    ];
    lines.push(line(String::new(), Tone::Plain));
    for (i, (label, item)) in MENU.iter().enumerate() {
        let mark = if i == state.profile_at { '›' } else { ' ' };
        let tone = if *item == MenuItem::DeleteAccount { Tone::Bad } else { Tone::Plain };
        lines.push(line(format!("{mark} {label}"), tone));
    }
    lines.push(line(String::new(), Tone::Plain));
    lines.push(line(HINT.into(), Tone::Dim));
    lines
}

/// Moves the `›` by `delta`, staying on the menu.
pub fn step(state: &mut State, delta: isize) {
    let last = MENU.len() as isize - 1;
    state.profile_at = (state.profile_at as isize + delta).clamp(0, last) as usize;
}

/// Opens the window for the item marked `›`.
pub fn open(state: &mut State) {
    let form = match MENU[state.profile_at].1 {
        MenuItem::Name => Form::name(&state.name),
        MenuItem::Passphrase => Form::passphrase(),
        MenuItem::Words => Form::words(),
        MenuItem::DeleteAccount => Form::delete_account(),
    };
    state.overlay = Some(Overlay::Form(form));
}
