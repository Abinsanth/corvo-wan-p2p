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

use std::borrow::Cow;
use std::iter::Peekable;
use std::str::Chars;
use std::sync::Arc;

use corvo_wan_p2p_core::VERSION;
use corvo_wan_p2p_core::identity::{Code, is_invisible};
use corvo_wan_p2p_core::names::YOU;
use corvo_wan_p2p_core::store::{Mark, Standing};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use zeroize::Zeroizing;

use super::chat::local;
use super::onboarding::Onboarding;
use super::state::{APP, Line, MAX_INPUT_ROWS, RowKey, Rows, Screen, Secret, State, Tab, Tone};
use super::window::{self, Overlay};

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
/// Most completion choices shown at once; the list scrolls with the selection.
pub const POPUP_ROWS: usize = 6;
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

/// One line of a tab and how long its head is: the bytes of `text` before
/// a message's own text (its time and who sent it), 0 for other lines.
pub type Shown = (Line, usize);

/// What a tab shows, and the line its last `/clear` was at. A chat tab's
/// lines come from its messages: a line for each new day, the time, who
/// sent it and, for yours, its mark.
pub fn tab_lines(state: &State, tab: &Tab) -> (Vec<Shown>, usize) {
    let Some(peer) = tab.peer else {
        return (tab.lines.iter().map(|l| (l.clone(), 0)).collect(), tab.cleared);
    };
    let label = state.label(&peer);
    let mut lines = Vec::new();
    if state.standing(&peer) == Some(Standing::Request) {
        let text = format!("{label} isn't a contact yet: /accept adds them, /block stops them");
        lines.push((Line { text, tone: Tone::Warn }, 0));
    }
    let messages = state.chats.get(&peer).map_or(&[][..], |c| &c.messages[..]);
    let mut clear_line = None;
    let mut last_day = None;
    for (i, m) in messages.iter().enumerate() {
        if i == tab.cleared {
            clear_line = Some(lines.len());
        }
        let at = local(m.sent_at);
        let day = at.map(|t| t.date_naive());
        if day != last_day {
            let text = at.map_or_else(|| "── ? ──".into(), |t| format!("── {} ──", t.format("%a %-d %b")));
            lines.push((Line { text, tone: Tone::Dim }, 0));
            last_day = day;
        }
        let time = at.map_or_else(|| "--:--".into(), |t| t.format("%H:%M").to_string());
        let (who, line_tone) = if m.mark.is_outgoing() { (YOU, Tone::Own) } else { (label.as_str(), Tone::Plain) };
        let mark = match m.mark {
            Mark::Waiting => " ·",
            Mark::Delivered => " ✓",
            Mark::Read => " ✓✓",
            Mark::Received => "",
        };
        let head = format!("{time} {}: ", one_line(who));
        let head_len = head.len();
        lines.push((Line { text: format!("{head}{}{mark}", m.text), tone: line_tone }, head_len));
    }
    // Never cleared: everything shows, the request line too.
    let clear_line = if tab.cleared == 0 { 0 } else { clear_line.unwrap_or(lines.len()) };
    (lines, clear_line)
}

#[cfg(test)]
thread_local! {
    /// How many times a tab's rows were built, for tests.
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The open tab's rows at `width`, and the row its last `/clear` was at.
/// Built once per change and width, then reused.
pub fn chat_rows(state: &State, width: u16) -> (Arc<Rows>, usize) {
    let key = RowKey { tab: state.current, peer: state.tab().peer, width, changes: state.changes };
    if let Some(hit) = state.rows.get(&key) {
        return hit;
    }
    let (rows, clear_row) = build_rows(state, state.tab(), width);
    let rows = Arc::new(rows);
    state.rows.put(key, rows.clone(), clear_row);
    (rows, clear_row)
}

fn build_rows(state: &State, tab: &Tab, width: u16) -> (Rows, usize) {
    #[cfg(test)]
    BUILDS.with(|b| b.set(b.get() + 1));
    let (lines, cleared) = tab_lines(state, tab);
    let mut rows = Vec::new();
    let mut clear_row = None;
    for (i, (line, head)) in lines.iter().enumerate() {
        if i == cleared {
            clear_row = Some(rows.len());
        }
        rows.extend(line_rows(&line.text, *head, width.into()).into_iter().map(|r| (r, line.tone)));
    }
    let clear_row = clear_row.unwrap_or(rows.len());
    (rows, clear_row)
}

/// One line's rows at `width`. After its first `head` bytes, the text wraps
/// with a hanging indent, so every row of it starts under its first column;
/// not when the head takes more than half the width.
fn line_rows(text: &str, head: usize, width: usize) -> Vec<String> {
    let indent = text.get(..head).map_or(0, |h| h.width());
    let (lead, body, indent) =
        if head > 0 && indent * 2 <= width { (&text[..head], &text[head..], indent) } else { ("", text, 0) };
    let mut rows = Vec::new();
    for piece in body.split('\n') {
        for row in wrap(&clean(piece), width - indent) {
            let before = if rows.is_empty() { lead.to_string() } else { " ".repeat(indent) };
            rows.push(format!("{before}{row}"));
        }
    }
    rows
}

/// (rows to scroll through, rows visible) in the chat area. After a `/clear`
/// there are always enough rows below the mark to fill the area, so it starts
/// out empty.
pub fn chat_page(state: &State) -> (usize, usize) {
    let Some(areas) = areas(state.size, input_rows(state)) else {
        return (0, 0);
    };
    let (rows, clear_row) = chat_rows(state, areas.chat.width);
    let height = areas.chat.height as usize;
    (scroll_total(rows.len(), clear_row, height), height)
}

/// Rows to scroll through: after a `/clear`, enough below its mark to fill the area.
fn scroll_total(rows: usize, clear_row: usize, height: usize) -> usize {
    rows.max(clear_row + height)
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
/// numbered down each column. Each row is wiped after use.
pub fn word_grid(words: &[&str], width: u16) -> Vec<Zeroizing<String>> {
    use std::fmt::Write;
    let cols = (usize::from(width) / WORD_CELL).clamp(2, 4);
    let rows = words.len().div_ceil(cols);
    (0..rows)
        .map(|r| {
            // Written straight into a buffer that's big enough, so no copy is left behind.
            let mut row = Zeroizing::new(String::with_capacity(cols * WORD_CELL));
            for (n, w) in (0..cols).filter_map(|c| words.get(c * rows + r).map(|w| (c * rows + r + 1, w))) {
                let _ = write!(row, "{n:>2}. {w:<width$}", width = WORD_CELL - 4);
            }
            let end = row.trim_end().len();
            row.truncate(end);
            row
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
    draw_popup(frame, state, areas.input);
    draw_status(frame, state, areas.status);
    if let Some(overlay) = &state.overlay {
        draw_overlay(frame, overlay, area);
    }
}

/// A window centred over the screen, scrolled as `update` left it.
fn draw_overlay(frame: &mut Frame, overlay: &Overlay, area: Rect) {
    let rows = window::rows(overlay, (area.width, area.height));
    let height = window::height((area.width, area.height), rows.len()) + 2;
    let width = window::WIDTH.min(area.width);
    let at =
        Rect::new(area.x + (area.width - width) / 2, area.y + (area.height.saturating_sub(height)) / 2, width, height);
    let lines: Vec<TextLine> = rows.into_iter().map(|row| TextLine::from(format!("  {row}"))).collect();
    frame.render_widget(Clear, at);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(overlay.title())).scroll((overlay.scroll(), 0)),
        at,
    );
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
    let guidance_text = onboarding.guidance(state.can_copy);
    let guidance: Vec<(Cow<str>, Style)> = guidance_text.iter().map(|t| plain(t)).collect();
    let mut notes: Vec<(Cow<str>, Style)> =
        onboarding.notes.iter().map(|n| (Cow::Borrowed(n.text.as_str()), tone(n.tone))).collect();
    if !notes.is_empty() {
        notes.insert(0, plain(""));
    }
    // Borrowed from here on, so the words aren't copied.
    let grid_rows = onboarding.words().map(|words| word_grid(&words, body.width));
    let lines = match &grid_rows {
        None => [guidance, notes].concat(),
        Some(grid_rows) => {
            let grid: Vec<(Cow<str>, Style)> = grid_rows.iter().map(|r| plain(r)).collect();
            let all = [guidance.clone(), vec![plain("")], grid.clone(), notes.clone()].concat();
            if text_rows(&all, body.width) <= body.height.into() {
                all
            } else if grid.len() <= body.height.into() {
                // The words matter most: they go first, and the rest as fits.
                [grid, notes, guidance].concat()
            } else {
                [vec![(Cow::Borrowed(BIGGER_FOR_WORDS), tone(Tone::Warn))], notes, guidance].concat()
            }
        }
    };
    let rows: Vec<TextLine> = lines
        .iter()
        .flat_map(|(text, style)| rows_of(text, body.width).into_iter().map(|r| TextLine::styled(r, *style)))
        .collect();
    frame.render_widget(Paragraph::new(rows), body);
    draw_flash(frame, state, Rect::new(0, h - 3, w, 1));
    frame.render_widget(Block::new().borders(Borders::TOP).border_style(dim()), Rect::new(0, h - 2, w, 1));
    draw_input(frame, state, Rect::new(0, h - 1, w, 1));
}

/// A line without a style.
fn plain(text: &str) -> (Cow<'_, str>, Style) {
    (Cow::Borrowed(text), Style::new())
}

/// How many rows `lines` take at `width`, wrapped.
fn text_rows(lines: &[(Cow<str>, Style)], width: u16) -> usize {
    lines.iter().map(|(text, _)| rows_of(text, width).len()).sum()
}

/// `text` as rows at `width`: itself, not a copy, when it's safe to draw and
/// fits (like a row of the 12 words); otherwise cleaned and wrapped.
fn rows_of(text: &str, width: u16) -> Vec<Cow<'_, str>> {
    let safe = !text.chars().any(|c| c.is_control() || is_invisible(c));
    if safe && text.width() <= width.into() {
        vec![Cow::Borrowed(text)]
    } else {
        wrap(&one_line(text), width.into()).into_iter().map(Cow::Owned).collect()
    }
}

/// Your 12 words over the chat, from `/phrase`.
fn draw_reveal(frame: &mut Frame, words: &Secret, area: Rect) {
    let words: Vec<&str> = words.expose().split_whitespace().collect();
    let block = Block::bordered().title(" your recovery words ");
    let inner = block.inner(area);
    let grid_rows = word_grid(&words, inner.width);
    let grid: Vec<TextLine> = grid_rows.iter().map(|r| TextLine::from(r.as_str())).collect();
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
        Tone::Dim => dim(),
        Tone::Good => Style::new().fg(Color::Green),
        Tone::Warn => Style::new().fg(Color::Yellow),
        Tone::Bad => Style::new().fg(Color::Red),
        Tone::Own => Style::new().fg(Color::Cyan),
    }
}

/// A name for one row: cleaned, and line breaks become spaces.
fn one_line(text: &str) -> String {
    clean(&text.replace('\n', " "))
}

/// Each tab's title as drawn, padded with a space each side.
fn tab_titles(state: &State) -> Vec<String> {
    state.tabs.iter().map(|tab| format!(" {} ", one_line(&tab.title))).collect()
}

/// The tab drawn at column `x`, for clicks.
pub fn tab_at(state: &State, x: u16) -> Option<usize> {
    let mut start = 0;
    for (i, title) in tab_titles(state).iter().enumerate() {
        let end = start + title.width();
        if (start..end).contains(&usize::from(x)) {
            return Some(i);
        }
        start = end + 1; // the │ between tabs
    }
    None
}

fn draw_tabs(frame: &mut Frame, state: &State, area: Rect) {
    let mut spans = Vec::new();
    for (i, title) in tab_titles(state).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", dim()));
        }
        let style = if i == state.current {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        spans.push(Span::styled(title, style));
    }
    frame.render_widget(Paragraph::new(TextLine::from(spans)), area);
}

/// The sidebar's lines, each with the person a click on it opens.
fn sidebar(state: &State) -> Vec<(TextLine<'static>, Option<Code>)> {
    let open = state.tab().peer;
    let unread = |n: usize| if n > 0 { Span::styled(format!(" ({n})"), tone(Tone::Warn)) } else { Span::raw("") };
    let mut lines = Vec::new();
    for peer in state.people.iter().filter(|p| !p.request) {
        let (dot, dot_style) = if peer.online { ("● ", Style::new().fg(Color::Green)) } else { ("○ ", dim()) };
        let name = if open == Some(peer.code) { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
        let spans = vec![Span::styled(dot, dot_style), Span::styled(one_line(&peer.label), name), unread(peer.unread)];
        lines.push((TextLine::from(spans), Some(peer.code)));
    }
    let requests: Vec<_> = state.people.iter().filter(|p| p.request).collect();
    if !requests.is_empty() {
        if !lines.is_empty() {
            lines.push((TextLine::default(), None));
        }
        lines.push((TextLine::styled(format!("Requests ({})", requests.len()), tone(Tone::Warn)), None));
        for peer in requests {
            let spans = vec![Span::raw(format!("  {}", one_line(&peer.label))), unread(peer.unread)];
            lines.push((TextLine::from(spans), Some(peer.code)));
        }
    }
    if lines.is_empty() {
        lines = vec![(TextLine::styled("no one yet", dim()), None), (TextLine::styled("/add <code>", dim()), None)];
    }
    lines
}

/// The first sidebar row shown when it's scrolled to `top`, kept in range.
fn sidebar_first(total: usize, height: usize, top: usize) -> usize {
    top.min(total.saturating_sub(height))
}

/// Whether row `i` of `total` is drawn in `height` rows from `first`: the
/// top and bottom rows give way to "↑ N more" and "↓ N more" when there is.
fn sidebar_shows(i: usize, total: usize, height: usize, first: usize) -> bool {
    let above = first > 0;
    let below = first + height < total;
    i >= first + usize::from(above) && i + usize::from(below) < first + height
}

/// The sidebar's rows from the top it's scrolled to, as drawn in `height`
/// rows, each with the person a click on it opens.
fn sidebar_page(state: &State, height: usize) -> Vec<(TextLine<'static>, Option<Code>)> {
    let lines = sidebar(state);
    let total = lines.len();
    let first = sidebar_first(total, height, state.sidebar_top);
    let more = |hidden: &[(TextLine<'static>, Option<Code>)], arrow: &str| {
        let people: Vec<Code> = hidden.iter().filter_map(|(_, code)| *code).collect();
        let requests = people.iter().filter(|c| state.people.iter().any(|p| p.code == **c && p.request)).count();
        // Too narrow to say more: hidden requests make it stand out instead.
        let style = if requests > 0 { tone(Tone::Warn) } else { dim() };
        (TextLine::styled(format!("{arrow} {} more", people.len()), style), None)
    };
    let mut page = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(first).take(height) {
        if sidebar_shows(i, total, height, first) {
            page.push(line.clone());
        } else if i == first {
            page.push(more(&lines[..=first], "↑"));
        } else {
            page.push(more(&lines[i..], "↓"));
        }
    }
    page
}

/// Rows in the sidebar: (all of them, how many fit).
fn sidebar_size(state: &State) -> (usize, usize) {
    let height = areas(state.size, input_rows(state)).map_or(0, |a| usize::from(a.sidebar.height));
    (sidebar(state).len(), height)
}

/// Where the sidebar is scrolled after the wheel moves it by `delta` rows.
pub fn sidebar_scrolled(state: &State, delta: isize) -> usize {
    let (total, height) = sidebar_size(state);
    let first = sidebar_first(total, height, state.sidebar_top) as isize;
    sidebar_first(total, height, (first + delta).max(0) as usize)
}

/// Where the sidebar is scrolled so `code`'s row is in view; where it is if it already is.
pub fn sidebar_showing(state: &State, code: Code) -> usize {
    let (total, height) = sidebar_size(state);
    let mut first = sidebar_first(total, height, state.sidebar_top);
    let Some(i) = sidebar(state).iter().position(|(_, c)| *c == Some(code)) else { return first };
    while height > 2 && !sidebar_shows(i, total, height, first) {
        if i < first + 1 { first -= 1 } else { first += 1 }
    }
    first
}

/// Who each sidebar row on screen opens, top to bottom, for clicks.
pub fn sidebar_rows(state: &State) -> Vec<Option<Code>> {
    let (_, height) = sidebar_size(state);
    sidebar_page(state, height).into_iter().map(|(_, code)| code).collect()
}

fn draw_sidebar(frame: &mut Frame, state: &State, area: Rect) {
    let lines: Vec<TextLine> = sidebar_page(state, area.height.into()).into_iter().map(|(line, _)| line).collect();
    frame.render_widget(Paragraph::new(lines).block(Block::new().borders(Borders::RIGHT).border_style(dim())), area);
}

/// The completion list, just above the input and lined up with the word.
/// Shrinks to the rows that fit below the tabs.
/// Where the completion list goes above `input`: its area (border included),
/// the first match shown and how many show. `None` when it isn't drawn.
pub fn popup_area(state: &State, input: Rect) -> Option<(Rect, usize, usize)> {
    let p = state.popup.as_ref()?;
    let room = usize::from(input.y).saturating_sub(3);
    let rows = p.matches.len().min(POPUP_ROWS).min(room);
    if rows == 0 || p.start > state.editor.before_cursor().len() {
        return None;
    }
    let height = rows as u16 + 2;
    let longest = p.matches.iter().map(|m| one_line(m).width()).max().unwrap_or(0) as u16;
    let width = (longest + 2).min(input.width);
    let before = &state.editor.before_cursor()[..p.start];
    let col = before[before.rfind('\n').map_or(0, |i| i + 1)..].width() as u16;
    let x = (input.x + PROMPT.len() as u16 + col).min(input.x + input.width - width);
    Some((Rect::new(x, input.y - height, width, height), p.selected.saturating_sub(rows - 1), rows))
}

fn draw_popup(frame: &mut Frame, state: &State, input: Rect) {
    let (Some(p), Some((area, first, rows))) = (&state.popup, popup_area(state, input)) else {
        return;
    };
    let lines: Vec<TextLine> = p
        .matches
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, m)| {
            let style = if i == p.selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
            TextLine::styled(one_line(m), style)
        })
        .collect();
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(Block::bordered().border_style(dim())), area);
}

fn draw_chat(frame: &mut Frame, state: &State, area: Rect) {
    let (rows, clear_row) = chat_rows(state, area.width);
    let height = usize::from(area.height);
    let total = scroll_total(rows.len(), clear_row, height);
    let top = state.scroll.top(total, height);
    let shown: Vec<TextLine> =
        rows.iter().skip(top).take(height).map(|(text, t)| TextLine::styled(text.as_str(), tone(*t))).collect();
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
    use crate::ui::state::{Screen, Secret};
    use crate::ui::update::{Msg, update};
    use corvo_wan_p2p_core::identity::Seed;
    use corvo_wan_p2p_core::store::{Person, StoredMessage};
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

    #[test]
    fn the_welcome_window_reads_on_80_by_24_and_scrolls_on_less() {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.overlay = Some(Overlay::Welcome { scroll: 0 });
        let all = |buf: &Buffer| (0..buf.area.height).map(|y| row(buf, y)).collect::<Vec<_>>().join("\n");
        let screen = all(&render(&state, 80, 24));
        // As approved in #41.
        for part in [
            "┌─ Welcome to corvo ───",
            "│  Chat with friends directly, computer to computer.   │",
            "│  • Your 12 words are your account. Never share       │",
            "│              /help any time · Enter to start         │",
        ] {
            assert!(screen.contains(part), "{part:?} missing:\n{screen}");
        }
        let short = all(&render(&state, 40, 12));
        assert!(short.contains("Welcome to corvo") && !short.contains("/help any time"), "{short}");
        let total = window::rows(&Overlay::Welcome { scroll: 0 }, (40, 12)).len();
        state.overlay =
            Some(Overlay::Welcome { scroll: (total - usize::from(window::height((40, 12), total))) as u16 });
        assert!(all(&render(&state, 40, 12)).contains("/help any time"), "scrolled to the end");
    }

    fn render(state: &State, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn person(code: Code, name: &str, standing: Standing) -> Person {
        Person { code, name: name.into(), last_seen: None, standing }
    }

    fn received(id: usize, text: &str) -> StoredMessage {
        StoredMessage {
            id: format!("{id:04}"),
            sent_at: 1_760_000_000_000 + id as i64,
            text: text.into(),
            mark: Mark::Received,
        }
    }

    /// rahul (online, with a chat open), maria, and two bobs who look alike:
    /// one a request, one blocked.
    fn busy_state() -> State {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        let rahul = Seed::generate().code();
        state.set_people(vec![
            person(rahul, "rahul", Standing::Contact),
            person(Seed::generate().code(), "maria", Standing::Contact),
            person(Seed::generate().code(), "bob", Standing::Request),
            person(Seed::generate().code(), "bob", Standing::Blocked),
        ]);
        state.connected.insert(rahul);
        let chat = state.chats.entry(rahul).or_default();
        for i in 0..40 {
            chat.messages.push(received(i, &format!("message {i} with some words to wrap around")));
        }
        chat.messages.push(received(40, "two\nlines and 日本語 and \x1b[2J\u{202E}"));
        state.refresh_sidebar();
        state.open_chat(rahul, false);
        state.say(Tone::Plain, "welcome");
        state.say(Tone::Bad, "something went wrong");
        state
    }

    fn rahul(state: &State) -> Code {
        state.tabs[1].peer.unwrap()
    }

    #[test]
    fn the_layout_at_80_by_24() {
        let mut state = busy_state();
        state.online = Some(true);
        state.editor.set("hello".into());
        let buf = render(&state, 80, 24);
        assert!(row(&buf, 0).starts_with(" corvo │ rahul "), "{:?}", row(&buf, 0));
        assert!(row(&buf, 1).starts_with("○ maria"), "{:?}", row(&buf, 1));
        assert!(row(&buf, 2).starts_with("● rahul"), "contacts by name");
        assert!(row(&buf, 4).starts_with("Requests (1)"));
        assert!(row(&buf, 5).starts_with("  bob·"));
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

    fn hhmm(ms: i64) -> String {
        crate::ui::chat::local(ms).unwrap().format("%H:%M").to_string()
    }

    fn chat_screen(state: &mut State) -> String {
        state.select_tab(1);
        screen(&render(state, 80, 24))
    }

    #[test]
    fn messages_show_time_sender_and_marks() {
        let mut state = busy_state();
        let rahul = rahul(&state);
        let t = 1_760_000_100_000;
        let mine = |id: &str, text: &str, mark| StoredMessage { id: id.into(), sent_at: t, text: text.into(), mark };
        let chat = state.chats.get_mut(&rahul).unwrap();
        chat.messages = vec![
            StoredMessage { id: "a".into(), sent_at: t, text: "hi".into(), mark: Mark::Received },
            mine("b", "waiting", Mark::Waiting),
            mine("c", "delivered", Mark::Delivered),
            mine("d", "read", Mark::Read),
        ];
        let screen = chat_screen(&mut state);
        let at = hhmm(t);
        assert!(screen.contains(&format!("{at} rahul: hi")), "{screen}");
        assert!(screen.contains(&format!("{at} you: waiting ·")));
        assert!(screen.contains(&format!("{at} you: delivered ✓")));
        assert!(screen.contains(&format!("{at} you: read ✓✓")));
        let day = crate::ui::chat::local(t).unwrap().format("%a %-d %b").to_string();
        assert!(screen.contains(&format!("── {day} ──")), "a line for the day");
    }

    #[test]
    fn a_peer_called_you_is_not_drawn_as_you() {
        let mut state = busy_state();
        let you = Seed::generate().code();
        let mut known = state.known.clone();
        known.push(person(you, "You", Standing::Contact));
        state.set_people(known);
        let t = 1_760_000_100_000;
        state.chats.entry(you).or_default().messages = vec![
            StoredMessage { id: "a".into(), sent_at: t, text: "theirs".into(), mark: Mark::Received },
            StoredMessage { id: "b".into(), sent_at: t, text: "mine".into(), mark: Mark::Delivered },
        ];
        state.open_chat(you, true);
        let label = state.label(&you);
        assert!(label.starts_with("You·"), "{label}");
        let buf = render(&state, 80, 24);
        let all = screen(&buf);
        assert!(all.contains(&format!("{label}: theirs")), "{all}");
        let y_of = |text: &str| (0..24).find(|&y| row(&buf, y).contains(text)).unwrap();
        let chat_x = areas((80, 24), 1).unwrap().chat.x;
        let (mine, theirs) = (buf[(chat_x, y_of("you: mine"))].style(), buf[(chat_x, y_of("theirs"))].style());
        assert_eq!(mine.fg, tone(Tone::Own).fg, "your own lines have their own style");
        assert_ne!(theirs.fg, mine.fg);
    }

    #[test]
    fn rows_are_built_once_per_change() {
        let mut state = busy_state();
        state.size = (80, 24);
        state.select_tab(1);
        let start = BUILDS.with(|b| b.get());
        let built = || BUILDS.with(|b| b.get()) - start;
        render(&state, 80, 24);
        render(&state, 80, 24);
        update(&mut state, Msg::Tick);
        render(&state, 80, 24);
        assert_eq!(built(), 1, "a frame that changes nothing reuses the rows");
        let rahul = rahul(&state);
        update(&mut state, Msg::Core(corvo_wan_p2p_core::chat::Event::Online(rahul)));
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)));
        render(&state, 80, 24);
        assert_eq!(built(), 2, "once after a change, scrolling included");
        render(&state, 100, 30);
        assert_eq!(built(), 3, "and again at a new width");
    }

    #[test]
    fn continuation_rows_line_up_under_the_text() {
        let mut state = busy_state();
        let rahul = rahul(&state);
        let long = "word ".repeat(30);
        state.chats.get_mut(&rahul).unwrap().messages =
            vec![received(1, "first line\nsecond line"), received(2, long.trim_end())];
        state.select_tab(1);
        let buf = render(&state, 80, 24);
        let rows: Vec<String> = (0..24).map(|y| row(&buf, y)).collect();
        let column = |r: &str, text: &str| r.find(text).map(|at| r[..at].chars().count());
        let first = rows.iter().find(|r| r.contains("first line")).unwrap();
        let text_at = column(first, "first line").unwrap();
        assert!(first.contains("rahul: first line"));
        let second = rows.iter().find(|r| r.contains("second line")).unwrap();
        assert_eq!(column(second, "second line"), Some(text_at), "after a line break:\n{}", rows.join("\n"));
        let wrapped: Vec<&String> = rows.iter().filter(|r| r.contains("word")).collect();
        assert!(wrapped.len() >= 3, "the long one wraps");
        for r in &wrapped[1..] {
            assert_eq!(column(r, "word"), Some(text_at), "after a wrap:\n{}", rows.join("\n"));
        }
        let words: usize = wrapped.iter().map(|r| r.matches("word").count()).sum();
        assert_eq!(words, 30, "nothing is lost");
    }

    #[test]
    fn peer_text_is_drawn_cleaned() {
        let mut state = busy_state();
        let rahul = rahul(&state);
        state.chats.get_mut(&rahul).unwrap().messages = vec![received(1, "\x1b[2Jboom \u{202E}evil\x1b]0;title\x07")];
        let screen = chat_screen(&mut state);
        assert!(screen.contains("rahul: boom evil"), "{screen}");
        assert!(!screen.contains('\x1b') && !screen.contains('\u{202E}'));
    }

    #[test]
    fn a_long_line_wraps_within_the_chat() {
        let mut state = busy_state();
        let rahul = rahul(&state);
        state.chats.get_mut(&rahul).unwrap().messages = vec![received(1, &"x".repeat(300))];
        state.select_tab(1);
        let buf = render(&state, 80, 24);
        let chat = areas((80, 24), 1).unwrap().chat;
        // Only the chat's columns: a random code in the sidebar can hold an x too.
        let in_chat = |y| (chat.x..chat.right()).filter(|&x| buf[(x, y)].symbol() == "x").count();
        let xs: usize = (chat.y..chat.bottom()).map(in_chat).sum();
        assert_eq!(xs, 300, "every character is drawn");
        assert!((chat.y..chat.bottom()).filter(|&y| row(&buf, y).contains("xxxx")).count() >= 6);
    }

    #[test]
    fn look_alike_labels_show_in_the_sidebar_and_on_messages() {
        let mut state = busy_state();
        let bob = state.people.iter().find(|p| p.request).unwrap().code;
        state.chats.entry(bob).or_default().messages.push(received(1, "hello"));
        state.open_chat(bob, true);
        let label = state.label(&bob);
        assert!(label.starts_with("bob·"));
        let screen = screen(&render(&state, 80, 24));
        assert!(screen.contains(&format!("{label}: hello")));
        assert!(screen.contains("/accept"), "a request's chat says how to answer:\n{screen}");
    }

    #[test]
    fn the_sidebar_counts_unread_messages() {
        let mut state = busy_state();
        let rahul = rahul(&state);
        state.chats.get_mut(&rahul).unwrap().unread = 3;
        state.refresh_sidebar();
        let buf = render(&state, 80, 24);
        assert!((0..24).any(|y| row(&buf, y).starts_with("● rahul (3)")));
    }

    #[test]
    fn the_completion_list_sits_above_the_input_lined_up_with_the_word() {
        let mut state = busy_state();
        typed(&mut state, "/chat ");
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        let buf = render(&state, 80, 24);
        let names: Vec<String> = state.popup.as_ref().unwrap().matches.clone();
        assert_eq!(names.len(), 3, "rahul, maria and the bob who's a request");
        let input = areas((80, 24), 1).unwrap().input;
        let first = input.y as usize - names.len() - 1;
        for (i, name) in names.iter().enumerate() {
            let line = row(&buf, (first + i) as u16);
            let column = line.find(name.as_str()).map(|at| line[..at].chars().count());
            assert_eq!(column, Some("> /chat ".len() + 1), "{line:?}");
        }
        let selected = buf[(("> /chat ".len() + 1) as u16, first as u16)].modifier;
        assert!(selected.contains(Modifier::REVERSED));
    }

    #[test]
    fn clicking_a_name_opens_the_chat_and_clicking_a_tab_selects_it() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = busy_state();
        state.size = (80, 24);
        let click = |column, row| {
            Msg::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        let maria = state.people.iter().find(|p| p.label == "maria").unwrap().code;
        let y = (0..24).find(|&y| row(&render(&state, 80, 24), y).starts_with("○ maria")).unwrap();
        update(&mut state, click(3, y));
        assert_eq!(state.open_peer(), Some(maria));
        let bob = state.people.iter().find(|p| p.request).unwrap().code;
        let y = (0..24).find(|&y| row(&render(&state, 80, 24), y).starts_with("  bob")).unwrap();
        update(&mut state, click(4, y));
        assert_eq!(state.open_peer(), Some(bob));
        update(&mut state, click(2, 0));
        assert!(state.on_corvo(), "the first tab");
        let x = row(&render(&state, 80, 24), 0).find("rahul").unwrap() as u16;
        update(&mut state, click(x, 0));
        assert_eq!(state.open_peer(), Some(rahul(&state)));
        update(&mut state, click(3, 4));
        update(&mut state, click(40, 10));
        assert_eq!(state.open_peer(), Some(rahul(&state)), "clicking elsewhere changes nothing");
    }

    #[test]
    fn the_sidebar_scrolls_to_the_open_chat_and_shows_requests() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.size = (80, 24);
        let mut people: Vec<Person> =
            (0..30).map(|i| person(Seed::generate().code(), &format!("p{i:02}"), Standing::Contact)).collect();
        people.push(person(Seed::generate().code(), "zed", Standing::Request));
        state.set_people(people.clone());
        let mouse = |kind, column, row| Msg::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
        let sidebar = areas((80, 24), 1).unwrap().sidebar;
        let shown = |state: &State| {
            let buf = render(state, 80, 24);
            (sidebar.y..sidebar.bottom())
                .map(|y| row(&buf, y).chars().take(sidebar.width as usize - 1).collect::<String>())
                .collect::<Vec<_>>()
        };

        let rows = shown(&state);
        assert!(rows.last().unwrap().contains("↓ 13 more"), "{rows:#?}");
        let more = render(&state, 80, 24)[(2, sidebar.bottom() - 1)].fg;
        assert_eq!(more, Color::Yellow, "hidden requests make it stand out");

        send(&mut state, "/chat p29");
        let rows = shown(&state);
        assert!(rows.iter().any(|r| r.contains("p29")), "the open chat is in view: {rows:#?}");
        assert!(rows[0].contains("↑"), "{rows:#?}");

        for _ in 0..10 {
            update(&mut state, mouse(MouseEventKind::ScrollDown, 3, 5));
        }
        let rows = shown(&state);
        let y = rows.iter().position(|r| r.contains("zed")).expect("the request is reachable") as u16;
        assert!(rows.iter().any(|r| r.starts_with("Requests (1)")));
        update(&mut state, mouse(MouseEventKind::Down(MouseButton::Left), 4, sidebar.y + y));
        assert_eq!(state.open_peer(), Some(people[30].code), "the click lands on the request");
        assert!(shown(&state).iter().any(|r| r.contains("zed")), "opening it doesn't move it out of view");

        for _ in 0..20 {
            update(&mut state, mouse(MouseEventKind::ScrollUp, 3, 5));
        }
        let rows = shown(&state);
        assert!(rows[0].starts_with("○ p00"), "back at the top: {rows:#?}");
        let y = rows.iter().position(|r| r.contains("p05")).unwrap() as u16;
        update(&mut state, mouse(MouseEventKind::Down(MouseButton::Left), 3, sidebar.y + y));
        assert_eq!(state.open_peer(), Some(people[5].code));
        assert!(!state.scroll.is_scrolled(), "the wheel over the sidebar doesn't scroll the chat");
        let last = sidebar.bottom() - 1;
        update(&mut state, mouse(MouseEventKind::Down(MouseButton::Left), 3, last));
        assert_eq!(state.open_peer(), Some(people[5].code), "the ↓ row opens no one");
    }

    #[test]
    fn clicking_an_entry_in_the_completion_list_takes_it() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = busy_state();
        state.size = (80, 24);
        typed(&mut state, "/chat ");
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        let names = state.popup.as_ref().unwrap().matches.clone();
        let buf = render(&state, 80, 24);
        let y = (0..24).find(|&y| row(&buf, y).contains(&format!("│{}", names[1]))).unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let before = state.open_peer();
        update(&mut state, Msg::Mouse(click));
        assert_eq!(state.editor.text(), format!("/chat {} ", names[1]));
        assert_eq!(state.popup, None);
        assert_eq!(state.open_peer(), before, "the sidebar under it isn't clicked");
    }
}
