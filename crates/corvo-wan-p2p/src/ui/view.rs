//! Drawing. Reads the state and changes nothing.
//!
//! ```text
//!  corvo │ rahul │ ana                      tabs
//! ● rahul        │ hi!                      sidebar │ chat area
//! ○ ana          │
//!                │
//! Requests (1)   │
//!   bob·m4x8     │
//! ! that code has a typo                    flash
//! ────────────────────────────────────────
//! > hello_                                  input (1 to 5 rows)
//!  ana · abcdefgh · online                  status line
//! ```

use std::iter::Peekable;
use std::str::Chars;

use corvo_wan_p2p_core::VERSION;
use corvo_wan_p2p_core::identity::is_invisible;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::onboarding::Onboarding;
use super::state::{APP, MAX_INPUT_ROWS, Screen, Secret, State, Tab, Tone};

/// Smaller than this, only [`TOO_SMALL`] is drawn.
pub const MIN_WIDTH: u16 = 40;
pub const MIN_HEIGHT: u16 = 12;
pub const TOO_SMALL: &str = "make the window bigger";
/// Instead of the 12 words, when they don't all fit.
pub const BIGGER_FOR_WORDS: &str = "make the window bigger for your words";
/// Rows that aren't the chat area or the input: tabs, flash, the input's border, status.
const FIXED_ROWS: u16 = 4;
/// Before the input's first row, and the space before its others.
const PROMPT: &str = "> ";
/// How a masked answer's characters show.
const MASK: char = '•';
/// Columns for one of the 12 words: `12. ` and the longest word, 8 letters, then a gap.
const WORD_CELL: usize = 14;

/// Where each part of the screen goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    pub tabs: Rect,
    pub sidebar: Rect,
    pub chat: Rect,
    pub flash: Rect,
    pub border: Rect,
    pub input: Rect,
    pub status: Rect,
}

/// The layout for a terminal of `size`; `None` when it's too small.
pub fn areas(size: (u16, u16), input_rows: usize) -> Option<Areas> {
    let (w, h) = size;
    if w < MIN_WIDTH || h < MIN_HEIGHT {
        return None;
    }
    let input_h = input_rows.clamp(1, MAX_INPUT_ROWS) as u16;
    let body_h = h - FIXED_ROWS - input_h;
    let side_w = (w / 4).clamp(14, 24);
    let row = |y, height| Rect::new(0, y, w, height);
    Some(Areas {
        tabs: row(0, 1),
        sidebar: Rect::new(0, 1, side_w, body_h),
        chat: Rect::new(side_w + 1, 1, w - side_w - 1, body_h),
        flash: row(1 + body_h, 1),
        border: row(2 + body_h, 1),
        input: row(3 + body_h, input_h),
        status: row(h - 1, 1),
    })
}

/// How many rows the input box shows.
pub fn input_rows(state: &State) -> usize {
    state.editor.rows().len().clamp(1, MAX_INPUT_ROWS)
}

/// The open tab's rows at the chat area's width, and the row its last `/clear`
/// was at.
pub fn chat_rows(tab: &Tab, width: u16) -> (Vec<(String, Tone)>, usize) {
    let mut rows = Vec::new();
    let mut clear_row = None;
    for (i, line) in tab.lines.iter().enumerate() {
        if i == tab.cleared {
            clear_row = Some(rows.len());
        }
        for piece in line.text.split('\n') {
            rows.extend(wrap(&clean(piece), width.into()).into_iter().map(|r| (r, line.tone)));
        }
    }
    let clear_row = clear_row.unwrap_or(rows.len());
    (rows, clear_row)
}

/// (rows to scroll through, rows visible) in the chat area. After a `/clear`
/// there are always enough rows below the mark to fill the area, so it starts
/// out empty.
pub fn chat_page(state: &State) -> (usize, usize) {
    let Some(areas) = areas(state.size, input_rows(state)) else {
        return (0, 0);
    };
    let (rows, clear_row) = chat_rows(state.tab(), areas.chat.width);
    let height = areas.chat.height as usize;
    (rows.len().max(clear_row + height), height)
}

/// Text from someone else, made safe to draw: escape sequences, control
/// characters and invisible or text-direction characters are removed, tabs
/// become spaces. Line breaks go too, so split lines before cleaning them.
/// Zero-width joiners and variation selectors stay, since emoji need them.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
            '\t' => out.push(' '),
            c if c.is_control() || is_invisible(c) => {}
            c => out.push(c),
        }
    }
    out
}

/// Skips the rest of an escape sequence whose ESC was just read.
fn skip_escape(chars: &mut Peekable<Chars>) {
    match chars.next() {
        // CSI: parameter and intermediate bytes, then one final byte.
        Some('[') => {
            for c in chars.by_ref() {
                if !('\x20'..='\x3f').contains(&c) {
                    break;
                }
            }
        }
        // OSC and the other string sequences: up to BEL or ESC \.
        Some(']' | 'P' | 'X' | '^' | '_') => {
            while let Some(c) = chars.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' {
                    chars.next_if_eq(&'\\');
                    break;
                }
            }
        }
        // Everything else is ESC and one more character.
        _ => {}
    }
}

/// Splits `text` into rows at most `width` columns wide, at spaces when it
/// can. Wide characters count as two columns.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_w = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if row_w + w > width && !row.is_empty() {
            match row.rfind(' ') {
                // A space that doesn't fit is where the row ends.
                _ if c == ' ' => rows.push(std::mem::take(&mut row)),
                Some(i) if i > 0 => {
                    let rest = row[i + 1..].to_string();
                    rows.push(row[..i].trim_end().to_string());
                    row = rest;
                }
                _ => rows.push(std::mem::take(&mut row)),
            }
            row_w = row.width();
            if row_w + w > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                row_w = 0;
            }
        }
        if c == ' ' && row.is_empty() && !rows.is_empty() {
            continue;
        }
        row.push(c);
        row_w += w;
    }
    rows.push(row);
    rows
}

/// The 12 words in numbered columns, as many as fit in `width` (2 to 4),
/// numbered down each column.
pub fn word_grid(words: &[&str], width: u16) -> Vec<String> {
    let cols = (usize::from(width) / WORD_CELL).clamp(2, 4);
    let rows = words.len().div_ceil(cols);
    (0..rows)
        .map(|r| {
            let cells = (0..cols).filter_map(|c| words.get(c * rows + r).map(|w| (c * rows + r + 1, w)));
            let row: String = cells.map(|(n, w)| format!("{n:>2}. {w:<width$}", width = WORD_CELL - 4)).collect();
            row.trim_end().to_string()
        })
        .collect()
}

pub fn draw(frame: &mut Frame, state: &State) {
    let area = frame.area();
    if let Screen::Onboarding(onboarding) = &state.screen {
        draw_onboarding(frame, state, onboarding, area);
        return;
    }
    let Some(areas) = areas((area.width, area.height), input_rows(state)) else {
        frame.render_widget(Paragraph::new(TOO_SMALL).wrap(Wrap { trim: true }), area);
        return;
    };
    draw_tabs(frame, state, areas.tabs);
    draw_sidebar(frame, state, areas.sidebar);
    draw_chat(frame, state, areas.chat);
    if let Some(words) = &state.reveal {
        let body = Rect::new(0, areas.sidebar.y, area.width, areas.sidebar.height);
        draw_reveal(frame, words, body);
    }
    draw_flash(frame, state, areas.flash);
    frame.render_widget(Block::new().borders(Borders::TOP).border_style(dim()), areas.border);
    draw_input(frame, state, areas.input);
    draw_status(frame, state, areas.status);
}

/// What the app is busy with, or else the flash.
fn draw_flash(frame: &mut Frame, state: &State, area: Rect) {
    if let Some(busy) = state.busy {
        frame.render_widget(Paragraph::new(busy).style(tone(Tone::Warn)), area);
    } else if let Some(flash) = &state.flash {
        frame.render_widget(Paragraph::new(clean(&flash.line.text)).style(tone(flash.line.tone)), area);
    }
}

/// ```text
///  corvo-wan-p2p 0.1.0                      title
///                                           
///  Choose a passphrase. It locks…           what this step says
///   1. abandon      7. about                the words, when shown
///  those didn't match                       notes
/// unlocking…                                busy
/// ────────────────────────────────────────
/// passphrase: ••••_                         the question
/// ```
fn draw_onboarding(frame: &mut Frame, state: &State, onboarding: &Onboarding, area: Rect) {
    let (w, h) = (area.width, area.height);
    if w < MIN_WIDTH || h < MIN_HEIGHT {
        frame.render_widget(Paragraph::new(TOO_SMALL).wrap(Wrap { trim: true }), area);
        return;
    }
    let title = format!(" {APP} {VERSION}");
    frame.render_widget(Paragraph::new(title).style(Style::new().add_modifier(Modifier::BOLD)), Rect::new(0, 0, w, 1));
    let body = Rect::new(1, 2, w - 2, h - 5);
    let plain = |text: &String| (text.clone(), Style::new());
    let guidance: Vec<(String, Style)> = onboarding.guidance().iter().map(plain).collect();
    let mut notes: Vec<(String, Style)> = onboarding.notes.iter().map(|n| (n.text.clone(), tone(n.tone))).collect();
    if !notes.is_empty() {
        notes.insert(0, (String::new(), Style::new()));
    }
    let lines = match onboarding.words() {
        None => [guidance, notes].concat(),
        Some(words) => {
            let grid: Vec<(String, Style)> = word_grid(&words, body.width).iter().map(plain).collect();
            let blank = vec![(String::new(), Style::new())];
            let all = [guidance.clone(), blank, grid.clone(), notes.clone()].concat();
            if text_rows(&all, body.width) <= body.height.into() {
                all
            } else if grid.len() <= body.height.into() {
                // The words matter most: they go first, and the rest as fits.
                [grid, notes, guidance].concat()
            } else {
                [vec![(BIGGER_FOR_WORDS.to_string(), tone(Tone::Warn))], notes, guidance].concat()
            }
        }
    };
    let rows: Vec<TextLine> = lines
        .iter()
        .flat_map(|(text, style)| {
            wrap(&one_line(text), body.width.into()).into_iter().map(|r| TextLine::styled(r, *style))
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), body);
    draw_flash(frame, state, Rect::new(0, h - 3, w, 1));
    frame.render_widget(Block::new().borders(Borders::TOP).border_style(dim()), Rect::new(0, h - 2, w, 1));
    draw_input(frame, state, Rect::new(0, h - 1, w, 1));
}

/// How many rows `lines` take at `width`, wrapped.
fn text_rows(lines: &[(String, Style)], width: u16) -> usize {
    lines.iter().map(|(text, _)| wrap(&one_line(text), width.into()).len()).sum()
}

/// Your 12 words over the chat, from `/phrase`.
fn draw_reveal(frame: &mut Frame, words: &Secret, area: Rect) {
    let words: Vec<&str> = words.expose().split_whitespace().collect();
    let block = Block::bordered().title(" your recovery words ");
    let inner = block.inner(area);
    let grid: Vec<TextLine> = word_grid(&words, inner.width).into_iter().map(TextLine::from).collect();
    let hint = TextLine::styled("press any key to hide them", dim());
    let fits = |rows: usize| rows <= inner.height.into();
    let lines = if fits(grid.len() + 4) {
        [vec![TextLine::from("Your 12 recovery words:"), TextLine::default()], grid, vec![TextLine::default(), hint]]
            .concat()
    } else if fits(grid.len() + 1) {
        [grid, vec![hint]].concat()
    } else {
        vec![TextLine::styled(BIGGER_FOR_WORDS, tone(Tone::Warn)), hint]
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn tone(tone: Tone) -> Style {
    match tone {
        Tone::Plain => Style::new(),
        Tone::Good => Style::new().fg(Color::Green),
        Tone::Warn => Style::new().fg(Color::Yellow),
        Tone::Bad => Style::new().fg(Color::Red),
    }
}

/// A name for one row: cleaned, and line breaks become spaces.
fn one_line(text: &str) -> String {
    clean(&text.replace('\n', " "))
}

fn draw_tabs(frame: &mut Frame, state: &State, area: Rect) {
    let mut spans = Vec::new();
    for (i, tab) in state.tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", dim()));
        }
        let style = if i == state.current {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        spans.push(Span::styled(format!(" {} ", one_line(&tab.title)), style));
    }
    frame.render_widget(Paragraph::new(TextLine::from(spans)), area);
}

fn draw_sidebar(frame: &mut Frame, state: &State, area: Rect) {
    let open = state.tab().peer;
    let mut lines = Vec::new();
    for peer in state.people.iter().filter(|p| !p.request) {
        let (dot, dot_style) = if peer.online { ("● ", Style::new().fg(Color::Green)) } else { ("○ ", dim()) };
        let name = if open == Some(peer.code) { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
        lines.push(TextLine::from(vec![Span::styled(dot, dot_style), Span::styled(one_line(&peer.label), name)]));
    }
    let requests: Vec<_> = state.people.iter().filter(|p| p.request).collect();
    if !requests.is_empty() {
        if !lines.is_empty() {
            lines.push(TextLine::default());
        }
        lines.push(TextLine::styled(format!("Requests ({})", requests.len()), tone(Tone::Warn)));
        for peer in requests {
            lines.push(TextLine::from(format!("  {}", one_line(&peer.label))));
        }
    }
    if lines.is_empty() {
        lines = vec![TextLine::styled("no one yet", dim()), TextLine::styled("/add <code>", dim())];
    }
    frame.render_widget(Paragraph::new(lines).block(Block::new().borders(Borders::RIGHT).border_style(dim())), area);
}

fn draw_chat(frame: &mut Frame, state: &State, area: Rect) {
    let (rows, _) = chat_rows(state.tab(), area.width);
    let (total, height) = chat_page(state);
    let top = state.scroll.top(total, height);
    let shown: Vec<TextLine> =
        rows.into_iter().skip(top).take(height).map(|(text, t)| TextLine::styled(text, tone(t))).collect();
    frame.render_widget(Paragraph::new(shown), area);
    if state.scroll.is_scrolled() && area.height > 0 {
        let more = " ↓ more ";
        let w = (more.width() as u16).min(area.width);
        let at = Rect::new(area.right() - w, area.bottom() - 1, w, 1);
        frame.render_widget(Paragraph::new(more).style(Style::new().add_modifier(Modifier::REVERSED)), at);
    }
}

/// Your own typing, drawn one character per character so the cursor lines up:
/// anything that isn't safe to draw shows as �, and all of a masked answer as •.
fn shown_char(c: char, masked: bool) -> char {
    if masked {
        MASK
    } else if c.is_control() || is_invisible(c) {
        '\u{FFFD}'
    } else {
        c
    }
}

fn draw_input(frame: &mut Frame, state: &State, area: Rect) {
    let ask = state.ask();
    let (prompt, masked) = ask.as_ref().map_or((PROMPT, false), |a| (a.label.as_str(), a.masked));
    let prompt_w = prompt.width();
    let editor = &state.editor;
    if editor.is_empty() {
        let hint = match ask {
            Some(_) => "",
            None if state.on_corvo() => "/help lists commands",
            None => "type a message",
        };
        let line = TextLine::from(vec![Span::raw(prompt), Span::styled(hint, dim())]);
        frame.render_widget(Paragraph::new(line), area);
        let x = (area.x as usize + prompt_w).min(area.right().saturating_sub(1) as usize);
        frame.set_cursor_position(Position::new(x as u16, area.y));
        return;
    }
    let visible = area.height as usize;
    let (cursor_row, cursor_col) = editor.cursor_pos();
    let first = cursor_row.saturating_sub(visible.saturating_sub(1));
    let room = (area.width as usize).saturating_sub(prompt_w + 1).max(1);
    let indent = " ".repeat(prompt_w);
    let mut lines = Vec::new();
    for (i, row) in editor.rows().into_iter().enumerate().skip(first).take(visible) {
        let chars: Vec<char> = row.chars().map(|c| shown_char(c, masked)).collect();
        let lead = if i == 0 { prompt } else { indent.as_str() };
        let mut skip = 0;
        if i == cursor_row {
            // Keeps the cursor in view on a row wider than the box.
            while chars[skip..cursor_col].iter().map(|c| c.width().unwrap_or(0)).sum::<usize>() > room {
                skip += 1;
            }
            let x: usize = chars[skip..cursor_col].iter().map(|c| c.width().unwrap_or(0)).sum();
            let x = (area.x as usize + prompt_w + x).min(area.right().saturating_sub(1) as usize);
            frame.set_cursor_position(Position::new(x as u16, area.y + (i - first) as u16));
        }
        lines.push(TextLine::from(format!("{lead}{}", chars[skip..].iter().collect::<String>())));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_status(frame: &mut Frame, state: &State, area: Rect) {
    let (word, color) = match state.online {
        None => ("connecting", Color::Yellow),
        Some(true) => ("online", Color::Green),
        Some(false) => ("offline", Color::Red),
    };
    let line = TextLine::from(vec![
        Span::raw(format!(" {} · {} · ", one_line(&state.name), state.short_code)),
        Span::styled(word, Style::new().fg(color)),
    ]);
    frame.render_widget(Paragraph::new(line).style(Style::new().add_modifier(Modifier::REVERSED)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::onboarding::Onboarding;
    use crate::ui::state::{Line, Peer, Screen, Secret};
    use crate::ui::update::{Msg, update};
    use corvo_wan_p2p_core::identity::Seed;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn clean_removes_escapes_controls_and_direction_marks() {
        assert_eq!(clean("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(clean("a\x1b[2Jb"), "ab");
        assert_eq!(clean("\x1b]0;evil title\x07ok"), "ok");
        assert_eq!(clean("\x1b]52;c;aGk=\x1b\\ok"), "ok");
        assert_eq!(clean("over\rwrite"), "overwrite");
        assert_eq!(clean("\u{202E}txt.exe"), "txt.exe");
        assert_eq!(clean("zero\u{200B}width"), "zerowidth");
        assert_eq!(clean("bell\x07 nul\0 c1\u{9b}"), "bell nul c1");
        assert_eq!(clean("a\tb"), "a b");
        assert_eq!(clean("two\nlines"), "twolines");
    }

    #[test]
    fn clean_leaves_plain_text_and_emoji_alone() {
        for text in ["hello, world!", "नमस्ते", "שלום", "日本語", "👋🏽 👨‍👩‍👧 🏴󠁧󠁢󠁳󠁣󠁴󠁿 ❤️", "a·b ✓✓"]
        {
            assert_eq!(clean(text), text);
        }
    }

    #[test]
    fn wrap_breaks_at_spaces_then_anywhere() {
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("hello world", 20), vec!["hello world"]);
        assert_eq!(wrap("hello there world", 11), vec!["hello there", "world"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab cdefghij", 4), vec!["ab", "cdef", "ghij"]);
        assert_eq!(wrap("日本語です", 4), vec!["日本", "語で", "す"], "wide characters take two columns");
        let long = "x".repeat(300);
        let rows = wrap(&long, 50);
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().all(|r| r.width() <= 50));
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn render(state: &State, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn busy_state() -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        let rahul = Seed::generate().code();
        state.people = vec![
            Peer { code: rahul, label: "rahul".into(), online: true, request: false },
            Peer { code: Seed::generate().code(), label: "maria".into(), online: false, request: false },
            Peer { code: Seed::generate().code(), label: "bob·m4x8".into(), online: false, request: true },
        ];
        let mut chat = Tab::chat(rahul, "rahul".into());
        for i in 0..40 {
            chat.lines
                .push(Line { text: format!("rahul: message {i} with some words to wrap around"), tone: Tone::Plain });
        }
        chat.lines.push(Line { text: "two\nlines and 日本語 and \x1b[2J\u{202E}".into(), tone: Tone::Good });
        state.tabs.push(chat);
        state.say(Tone::Plain, "welcome");
        state.say(Tone::Bad, "something went wrong");
        state
    }

    #[test]
    fn the_layout_at_80_by_24() {
        let mut state = busy_state();
        state.online = Some(true);
        state.editor.set("hello".into());
        let buf = render(&state, 80, 24);
        assert!(row(&buf, 0).starts_with(" corvo │ rahul "), "{:?}", row(&buf, 0));
        assert!(row(&buf, 1).starts_with("● rahul"), "{:?}", row(&buf, 1));
        assert!(row(&buf, 2).starts_with("○ maria"));
        assert!(row(&buf, 4).starts_with("Requests (1)"));
        assert!(row(&buf, 5).starts_with("  bob·m4x8"));
        assert_eq!(row(&buf, 1).chars().nth(19), Some('│'), "the sidebar is 20 columns");
        assert!(row(&buf, 1).contains("welcome"));
        assert!(row(&buf, 20).starts_with("something went wrong"), "the flash");
        assert!(row(&buf, 21).starts_with("────"));
        assert!(row(&buf, 22).starts_with("> hello"));
        assert!(row(&buf, 23).starts_with(" ana · abcdefgh · online"));
    }

    #[test]
    fn a_chat_tab_follows_its_newest_lines_cleaned() {
        let mut state = busy_state();
        state.select_tab(1);
        let buf = render(&state, 80, 24);
        let screen: Vec<String> = (0..24).map(|y| row(&buf, y)).collect();
        let last = screen.iter().rposition(|r| r.contains("lines and")).expect("the newest line is shown");
        assert!(screen[last - 1].contains("two"));
        assert!(!screen.iter().any(|r| r.contains('\u{202E}') || r.contains('\x1b')));
        assert!(screen[1].contains("rahul"), "the open chat's name is in the sidebar");
    }

    #[test]
    fn after_clear_the_tab_is_empty_until_something_new() {
        let mut state = busy_state();
        state.clear_tab();
        let buf = render(&state, 80, 24);
        assert!(!(1..20).any(|y| row(&buf, y).contains("welcome") || row(&buf, y).contains("went wrong")));
        state.say(Tone::Plain, "fresh");
        let buf = render(&state, 80, 24);
        assert!(row(&buf, 1).contains("fresh"), "new lines start at the top");
        let (total, height) = chat_page(&state);
        assert_eq!(total, 2 + height, "what came before is still there to scroll up to");
    }

    #[test]
    fn a_long_input_row_keeps_the_cursor_in_view() {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.editor.set(format!("{}END", "x".repeat(200)));
        let buf = render(&state, 80, 24);
        assert!(row(&buf, 22).trim_end().ends_with("END"));
    }

    #[test]
    fn too_small_only_asks_for_a_bigger_window() {
        let state = busy_state();
        let buf = render(&state, 39, 24);
        assert!(row(&buf, 0).starts_with(TOO_SMALL));
        let buf = render(&state, 80, 11);
        assert!(row(&buf, 0).starts_with(TOO_SMALL));
    }

    #[test]
    fn no_panic_at_any_size() {
        let mut state = busy_state();
        state.editor.set("one\ntwo\nthree\nfour\nfive\nsix 日本語 \x1b \u{202E}".into());
        for scrolled in [false, true] {
            if scrolled {
                state.scroll.by(-3, 100, 10);
            }
            for w in 1..=120 {
                for h in 1..=40 {
                    for tab in 0..2 {
                        state.select_tab(tab);
                        render(&state, w, h);
                    }
                }
            }
        }
    }

    fn screen(buf: &Buffer) -> String {
        (0..buf.area.height).map(|y| row(buf, y)).collect::<Vec<_>>().join("\n")
    }

    fn typed(state: &mut State, text: &str) {
        for c in text.chars() {
            update(state, Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
    }

    fn send(state: &mut State, text: &str) {
        typed(state, text);
        update(state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    /// True if the screen shows any of `seed`'s words numbered, as the word list does.
    fn shows_words(buf: &Buffer, seed: &Seed) -> bool {
        let screen = screen(buf);
        seed.words().iter().enumerate().any(|(i, w)| screen.contains(&format!("{}. {w}", i + 1)))
    }

    fn shows_all_words(buf: &Buffer, seed: &Seed) -> bool {
        let screen = screen(buf);
        seed.words().iter().enumerate().all(|(i, w)| screen.contains(&format!("{}. {w}", i + 1)))
    }

    #[test]
    fn the_onboarding_screen_shows_guidance_notes_and_the_question() {
        let mut state = State::onboarding(Onboarding::start(false, false));
        send(&mut state, "what");
        let buf = render(&state, 80, 24);
        assert!(row(&buf, 0).contains("corvo-wan-p2p"));
        let all = screen(&buf);
        assert!(all.contains("There's no account on this computer yet"));
        assert!(all.contains("type n for a new account"), "the note about the wrong answer");
        assert!(row(&buf, 23).starts_with("n or r: "), "{:?}", row(&buf, 23));
    }

    #[test]
    fn busy_shows_above_the_input() {
        let mut state = State::onboarding(Onboarding::start(true, false));
        state.busy = Some("unlocking…");
        assert!(row(&render(&state, 80, 24), 21).starts_with("unlocking…"));
        let mut state = busy_state();
        state.busy = Some("checking your passphrase…");
        assert!(row(&render(&state, 80, 24), 20).starts_with("checking your passphrase…"));
    }

    #[test]
    fn passphrases_never_appear_on_screen() {
        let secret = "hunter2-secret";
        let mut state = State::onboarding(Onboarding::start(false, false));
        send(&mut state, "n");
        send(&mut state, "ana");
        typed(&mut state, secret);
        let buf = render(&state, 80, 24);
        assert!(!screen(&buf).contains("hunter2"));
        assert!(row(&buf, 23).starts_with(&format!("passphrase: {}", "•".repeat(secret.len()))));
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        typed(&mut state, secret);
        assert!(!screen(&render(&state, 80, 24)).contains("hunter2"));

        let mut state = State::onboarding(Onboarding::start(true, false));
        typed(&mut state, secret);
        assert!(!screen(&render(&state, 80, 24)).contains("hunter2"));
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(!screen(&render(&state, 80, 24)).contains("hunter2"));

        let mut state = busy_state();
        for (command, questions) in [("/passphrase", 3), ("/phrase", 1), ("/phrase copy", 1)] {
            send(&mut state, command);
            for _ in 0..questions {
                typed(&mut state, secret);
                assert!(!screen(&render(&state, 80, 24)).contains("hunter2"), "{command}");
                update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
                assert!(!screen(&render(&state, 80, 24)).contains("hunter2"), "{command}");
            }
        }
    }

    #[test]
    fn the_words_show_only_while_they_are_meant_to() {
        let mut state = State::onboarding(Onboarding::start(false, false));
        send(&mut state, "n");
        send(&mut state, "ana");
        send(&mut state, "pw");
        assert!(!shows_words(&render(&state, 80, 24), &Seed::generate()));
        send(&mut state, "pw");
        let Screen::Onboarding(onboarding) = &state.screen else { panic!() };
        let seed = Seed::from_phrase(&onboarding.words().unwrap().join(" ")).unwrap();
        for (w, h) in [(80, 24), (40, 24), (120, 40)] {
            assert!(shows_all_words(&render(&state, w, h), &seed), "all 12 at {w}×{h}");
        }
        send(&mut state, "");
        assert!(!shows_words(&render(&state, 80, 24), &seed), "hidden while typing them back");

        let mut state = busy_state();
        state.reveal = Some(Secret::new(seed.phrase()));
        assert!(shows_all_words(&render(&state, 80, 24), &seed));
        assert!(screen(&render(&state, 80, 24)).contains("press any key"));
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)));
        assert!(!shows_words(&render(&state, 80, 24), &seed), "gone after a key");
    }

    #[test]
    fn the_words_are_never_cut_off() {
        let mut onboarding = State::onboarding(Onboarding::start(false, false));
        for answer in ["n", "ana", "pw", "pw"] {
            send(&mut onboarding, answer);
        }
        let Screen::Onboarding(o) = &onboarding.screen else { panic!() };
        let seed = Seed::from_phrase(&o.words().unwrap().join(" ")).unwrap();
        let mut main = busy_state();
        main.reveal = Some(Secret::new(seed.phrase()));
        for w in MIN_WIDTH..=120 {
            for h in MIN_HEIGHT..=40 {
                for state in [&onboarding, &main] {
                    let buf = render(state, w, h);
                    assert!(
                        shows_all_words(&buf, &seed) || screen(&buf).contains(BIGGER_FOR_WORDS),
                        "{w}×{h}:\n{}",
                        screen(&buf)
                    );
                }
            }
        }
    }

    #[test]
    fn onboarding_and_the_words_never_panic_at_any_size() {
        let mut onboarding = State::onboarding(Onboarding::start(false, false));
        send(&mut onboarding, "n");
        send(&mut onboarding, "ana");
        send(&mut onboarding, "pw");
        send(&mut onboarding, "pw");
        let mut main = busy_state();
        main.reveal = Some(Secret::new(Seed::generate().phrase()));
        send(&mut main, "/phrase");
        typed(&mut main, "日本語");
        for w in 1..=120 {
            for h in 1..=40 {
                render(&onboarding, w, h);
                render(&main, w, h);
            }
        }
    }
}
