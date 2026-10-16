//! Copying to the clipboard through the terminal (OSC 52), so it works over
//! SSH too. Terminals that don't support it ignore the sequence.

/// Said wherever something is sent to the clipboard: it may not have worked.
pub const MAYBE: &str = "if your terminal allows it";
/// Said instead where the terminal is known not to copy.
pub const NO_COPY: &str = "Terminal.app can't copy from here";

/// Whether the terminal may copy: not Terminal.app, which ignores OSC 52.
/// `term_program` is `TERM_PROGRAM`.
pub fn can_copy(term_program: Option<&str>) -> bool {
    term_program != Some("Apple_Terminal")
}

/// What puts `text` on the clipboard, if the terminal allows it.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::account::PhraseDone;
    use crate::ui::commands::HELP;
    use crate::ui::onboarding::Onboarding;
    use crate::ui::state::State;
    use crate::ui::update::{Msg, Outcome, update};
    use corvo_wan_p2p_core::identity::Seed;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn send(state: &mut State, text: &str) {
        for c in text.chars() {
            update(state, Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        update(state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    fn state(can_copy: bool) -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.code = Some(Seed::generate().code());
        state.can_copy = can_copy;
        state
    }

    fn said(state: &State) -> Vec<String> {
        state.tabs[0].lines.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn clipboard_texts_never_claim_success() {
        let mut texts: Vec<String> = HELP.iter().map(|(usage, what)| format!("{usage} {what}")).collect();
        let mut s = state(true);
        send(&mut s, "/code");
        update(&mut s, Msg::Done(Outcome::Phrase(Ok(PhraseDone::Copied))));
        texts.extend(said(&s));
        texts.extend(Onboarding::at_code(Seed::generate().code(), false).guidance(true));
        texts.extend(Onboarding::at_code(Seed::generate().code(), true).guidance(true));
        let about_copying: Vec<&String> =
            texts.iter().filter(|t| t.contains("clipboard") || t.to_lowercase().contains("cop")).collect();
        assert!(about_copying.len() >= 5, "{about_copying:#?}");
        for text in about_copying {
            assert!(!text.contains("copied"), "{text:?} claims a copy");
            if text.contains("clipboard") {
                assert!(text.contains(MAYBE), "{text:?} doesn't say it may not have worked");
            }
        }
        let readme = include_str!("../../../../README.md");
        for line in readme.lines().filter(|l| l.contains("clipboard")) {
            assert!(line.contains(MAYBE) || line.contains("history apps"), "README: {line:?}");
        }
    }

    #[test]
    fn terminal_app_is_told_it_cannot_copy() {
        assert!(!can_copy(Some("Apple_Terminal")));
        assert!(can_copy(Some("iTerm.app")) && can_copy(Some("WezTerm")) && can_copy(None));

        let mut s = state(false);
        send(&mut s, "/code");
        assert!(said(&s).iter().any(|l| l.contains(NO_COPY)), "{:?}", said(&s));
        send(&mut s, "/phrase copy");
        assert_eq!(s.ask(), None, "no passphrase asked for a copy that can't happen");
        let last = said(&s).last().unwrap().clone();
        assert!(last.contains(NO_COPY) && last.contains("/phrase"), "{last}");
        let guidance = Onboarding::at_code(Seed::generate().code(), false).guidance(false).join(" ");
        assert!(guidance.contains(NO_COPY) && !guidance.contains("clipboard"), "{guidance}");
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (plain, encoded) in
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn osc52_wraps_the_text_for_the_clipboard() {
        assert_eq!(osc52("corvo:abc"), "\x1b]52;c;Y29ydm86YWJj\x07");
    }
}
