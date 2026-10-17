//! The commands about people: `/code`, `/add`, `/chat`, `/contacts`,
//! `/requests`, `/accept`, `/block`, `/unblock`, `/blocked`, `/remove`,
//! `/info`, `/name`, and `/update` for now.

use std::path::PathBuf;

use corvo_wan_p2p_core::identity::Code;
use corvo_wan_p2p_core::names;
use corvo_wan_p2p_core::store::{Mark, Person, Standing};

use super::account::Pending;
use super::chat::{day_and_time, local};
use super::commands::Cmd;
use super::state::{State, Tone};
use super::table::{cut, table};
use super::update::Effect;
use super::view::areas;

/// The widest a name gets in a table.
const NAME_WIDTH: usize = 16;

/// What `/add`, `/accept`, `/block`, `/unblock` and `/remove` ask the engine to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Add,
    Accept,
    Block,
    Unblock,
    Remove { history: bool },
}

/// What to show once everyone has been read again (for last-seen times).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Contacts,
    Info(Code),
}

/// Runs one of the people commands.
pub fn command(state: &mut State, cmd: Cmd, effects: &mut Vec<Effect>) {
    match cmd {
        Cmd::Code => {
            let Some(code) = state.code else { return };
            state.select_tab(0);
            state.say(Tone::Dim, "Your code");
            for row in code_box(&code.to_string(), room(state)) {
                state.say(Tone::Plain, row);
            }
            state.say(Tone::Plain, "Send it to a friend so they can /add you.");
            effects.push(Effect::ShowCode);
        }
        Cmd::Add(code) if Some(code) == state.code => {
            state.say(Tone::Bad, "that's your own code: send it to friends, and /add theirs")
        }
        Cmd::Add(code) => match state.standing(&code) {
            Some(Standing::Contact) => {
                state.open_chat(code, true);
                state.say(Tone::Plain, format!("{} is already a contact", state.label(&code)));
            }
            Some(Standing::Blocked) => {
                state.say(Tone::Bad, format!("you've blocked {}: /unblock them first", state.label(&code)))
            }
            _ => effects.push(Effect::Act { code, act: Act::Add }),
        },
        Cmd::Chat(name) => {
            if let Some(code) = find(state, &name, &[Standing::Contact, Standing::Request]) {
                state.open_chat(code, true);
            }
        }
        Cmd::Contacts => effects.push(Effect::Refresh(Show::Contacts)),
        Cmd::Requests => {
            state.select_tab(0);
            let rows = requests(state);
            if rows.is_empty() {
                state.say(Tone::Plain, "no requests");
            } else {
                say_table(state, &["NAME", "UNREAD", "CODE"], &rows);
                state.say(Tone::Plain, "/chat <name> to read · /accept <name> to add");
            }
        }
        Cmd::Blocked => {
            state.select_tab(0);
            let mut blocked: Vec<&Person> = state.known.iter().filter(|p| p.standing == Standing::Blocked).collect();
            blocked.sort_by_key(|p| state.label(&p.code).to_lowercase());
            let rows: Vec<Vec<String>> =
                blocked.iter().map(|p| vec![cut(&state.label(&p.code), NAME_WIDTH), p.code.short()]).collect();
            if rows.is_empty() {
                state.say(Tone::Plain, "you haven't blocked anyone");
            } else {
                say_table(state, &["NAME", "CODE"], &rows);
                state.say(Tone::Plain, "/unblock <name> to let them back");
            }
        }
        Cmd::Accept(name) => {
            let code = match name {
                Some(name) => find(state, &name, &[Standing::Request]),
                None => this_tab(state, "which request? /accept <name>, or open their chat first"),
            };
            match code.map(|c| (c, state.standing(&c))) {
                Some((code, Some(Standing::Request))) => effects.push(Effect::Act { code, act: Act::Accept }),
                Some((code, _)) => refuse(state, code, Standing::Request),
                None => {}
            }
        }
        Cmd::Block(name) => {
            let code = match name {
                // Anyone can be blocked, even someone you don't know yet.
                Some(name) => match name.parse::<Code>() {
                    Ok(code) if state.standing(&code).is_none() && Some(code) != state.code => Some(code),
                    _ => find(state, &name, &[Standing::Contact, Standing::Request]),
                },
                None => this_tab(state, "who? /block <name>, or open their chat first"),
            };
            if let Some(code) = code {
                effects.push(Effect::Act { code, act: Act::Block });
            }
        }
        Cmd::Unblock(name) => {
            if let Some(code) = find(state, &name, &[Standing::Blocked]) {
                effects.push(Effect::Act { code, act: Act::Unblock });
            }
        }
        Cmd::Remove(name) => {
            if let Some(code) = find(state, &name, &[Standing::Contact, Standing::Request]) {
                let label = state.label(&code);
                state.select_tab(0);
                if state.standing(&code) == Some(Standing::Request) {
                    state.say(
                        Tone::Plain,
                        format!(
                            "a request that used up its messages comes back when their app sends the rest; \
                             /block {label} stops them for good"
                        ),
                    );
                }
                state.say(Tone::Plain, format!("removing {label}: delete your history with them too? (Esc cancels)"));
                state.pending = Some(Pending::Remove { code });
            }
        }
        Cmd::Info(name) => {
            if let Some(code) = find(state, &name, &[Standing::Contact, Standing::Request, Standing::Blocked]) {
                effects.push(Effect::Refresh(Show::Info(code)));
            }
        }
        Cmd::Name(name) => effects.push(Effect::SetName(name)),
        _ => {}
    }
}

/// How wide the corvo tab is at the terminal's size now, if it's drawn.
fn room(state: &State) -> Option<usize> {
    areas(state.size, 1).map(|a| usize::from(a.chat.width))
}

/// `code` in a box, split over as many rows as it takes to fit in `room`
/// columns (one if `room` isn't known).
fn code_box(code: &str, room: Option<usize>) -> Vec<String> {
    const FRAME: usize = 6;
    let chars: Vec<char> = code.chars().collect();
    let fits = room.map_or(chars.len(), |room| room.saturating_sub(FRAME).max(8));
    let parts = chars.len().div_ceil(fits).max(1);
    let wide = chars.len().div_ceil(parts);
    let rule = "─".repeat(wide + 4);
    let mut rows = vec![format!("┌{rule}┐")];
    for part in chars.chunks(wide) {
        let part: String = part.iter().collect();
        rows.push(format!("│  {part:<wide$}  │"));
    }
    rows.push(format!("└{rule}┘"));
    rows
}

/// Requests A to Z: name, unread messages, short code.
fn requests(state: &State) -> Vec<Vec<String>> {
    let mut requests: Vec<_> = state.people.iter().filter(|p| p.request).collect();
    requests.sort_by_key(|p| p.label.to_lowercase());
    requests.iter().map(|p| vec![cut(&p.label, NAME_WIDTH), p.unread.to_string(), p.code.short()]).collect()
}

/// A table in the corvo tab, its heading dim.
fn say_table(state: &mut State, head: &[&str], rows: &[Vec<String>]) {
    let mut lines = table(head, rows).into_iter();
    if let Some(heading) = lines.next() {
        state.say(Tone::Dim, heading);
    }
    for line in lines {
        state.say(Tone::Plain, line);
    }
}

/// What came of [`Effect::ShowCode`]: where the code file is, or else
/// whether to save it.
pub fn code_shown(state: &mut State, saved: Option<PathBuf>) {
    match saved {
        Some(path) => state.say(Tone::Plain, format!("it's saved in {}", path.display())),
        None => state.pending = Some(Pending::SaveCode),
    }
}

pub fn code_written(state: &mut State, result: Result<PathBuf, String>) {
    match result {
        Ok(path) => state.say(Tone::Good, format!("saved in {}: it holds only your code", path.display())),
        Err(why) => state.say(Tone::Bad, format!("couldn't save it: {why}")),
    }
}

/// The person `typed` means, if they stand one of the `allowed` ways;
/// otherwise says why not.
fn find(state: &mut State, typed: &str, allowed: &[Standing]) -> Option<Code> {
    match names::find(&state.known, typed) {
        Ok(code) if state.standing(&code).is_some_and(|s| allowed.contains(&s)) => Some(code),
        Ok(code) => {
            refuse(state, code, allowed[0]);
            None
        }
        Err(e) => {
            state.say(Tone::Bad, e.to_string());
            None
        }
    }
}

/// Why someone can't be used where a `wanted` person was expected.
fn refuse(state: &mut State, code: Code, wanted: Standing) {
    let label = state.label(&code);
    let why = match (state.standing(&code), wanted) {
        (Some(Standing::Blocked), _) => format!("you've blocked {label}: /unblock them first"),
        (Some(Standing::Contact), Standing::Request) => format!("{label} is already a contact"),
        (_, Standing::Blocked) => format!("{label} isn't blocked"),
        _ => format!("{label} isn't a request"),
    };
    state.say(Tone::Bad, why);
}

/// The person in the open chat tab, or else says `ask`.
fn this_tab(state: &mut State, ask: &str) -> Option<Code> {
    let code = state.open_peer();
    if code.is_none() {
        state.say(Tone::Bad, ask);
    }
    code
}

/// What came of [`Effect::Act`]. `people` is everyone, read again afterwards.
pub fn acted(state: &mut State, code: Code, act: Act, result: Result<(), String>, people: Option<Vec<Person>>) {
    if let Err(why) = result {
        state.say(Tone::Bad, why);
        return;
    }
    let before = state.label(&code);
    // The engine closes the connection without reporting them offline.
    if matches!(act, Act::Block | Act::Unblock | Act::Remove { .. }) {
        state.connected.remove(&code);
    }
    if let Some(people) = people {
        state.set_people(people);
    }
    let label = state.label(&code);
    match act {
        Act::Add => {
            state.open_chat(code, true);
            state.say(Tone::Good, format!("added {label}. Say hi: they see it as a request until they accept"));
        }
        Act::Accept => {
            state.open_chat(code, true);
            state.say(Tone::Good, format!("{label} is now a contact"));
        }
        Act::Block => {
            state.close_chat(code);
            drop_waiting(state, code);
            state.say(
                Tone::Good,
                format!("blocked {before}: they can't reach you, and you look offline to them. /unblock undoes it"),
            );
        }
        Act::Unblock => {
            state.say(Tone::Good, format!("unblocked {before}. If they message you, they come back as a request"))
        }
        Act::Remove { history } => {
            state.close_chat(code);
            if history {
                state.chats.remove(&code);
                state.say(Tone::Good, format!("removed {before} and your history with them"));
            } else {
                drop_waiting(state, code);
                state.say(Tone::Good, format!("removed {before}; your history with them is kept"));
            }
            state.refresh_sidebar();
        }
    }
}

pub fn refreshed(state: &mut State, show: Show, result: Result<Vec<Person>, String>) {
    let people = match result {
        Ok(people) => people,
        Err(why) => return state.say(Tone::Bad, why),
    };
    state.set_people(people);
    state.select_tab(0);
    match show {
        Show::Contacts => {
            let now = chrono::Utc::now().timestamp_millis();
            let mut contacts: Vec<&Person> = state.known.iter().filter(|p| p.standing == Standing::Contact).collect();
            contacts.sort_by_key(|p| state.label(&p.code).to_lowercase());
            let rows: Vec<Vec<String>> = contacts
                .iter()
                .map(|p| vec![cut(&state.label(&p.code), NAME_WIDTH), status(state, p, now), p.code.short()])
                .collect();
            let requests = requests(state);
            if rows.is_empty() && requests.is_empty() {
                return state.say(Tone::Plain, "no contacts yet: /add <code> adds a friend");
            }
            if rows.is_empty() {
                state.say(Tone::Plain, "no contacts yet");
            } else {
                say_table(state, &["NAME", "STATUS", "CODE"], &rows);
            }
            if requests.is_empty() {
                state.say(Tone::Plain, "/chat <name> to open a chat · /info <name> for more");
            } else {
                state.say(Tone::Plain, "");
                say_table(state, &["REQUESTS", "UNREAD", "CODE"], &requests);
                state.say(Tone::Plain, "/chat <name> to open a chat · /accept <name> to add");
            }
        }
        Show::Info(code) => {
            let Some(p) = state.known.iter().find(|p| p.code == code).cloned() else {
                return state.say(Tone::Bad, "they're not someone you know any more");
            };
            let standing = match p.standing {
                Standing::Contact => "a contact",
                Standing::Request => "a request (not a contact yet)",
                Standing::Blocked => "blocked",
            };
            let name = if p.name.is_empty() { "(their app hasn't said)".to_string() } else { p.name.clone() };
            let label = state.label(&code);
            state.say(Tone::Plain, format!("{label}: {standing}, {}", seen(state, &p)));
            state.say(Tone::Plain, format!("  name: {name}"));
            state.say(Tone::Plain, format!("  code: {code}"));
        }
    }
}

/// "● online", "○ seen …" or "○ not seen yet", as of `now`.
fn status(state: &State, person: &Person, now: i64) -> String {
    match (state.connected.contains(&person.code), person.last_seen) {
        (true, _) => "● online".into(),
        (false, Some(at)) => format!("○ seen {}", ago(at, now)),
        (false, None) => "○ not seen yet".into(),
    }
}

/// How long before `now` `at` was: `2h ago` within a day, else the day,
/// like `14 Oct`. Both are Unix milliseconds.
fn ago(at: i64, now: i64) -> String {
    match (now - at).max(0) / 60_000 {
        0 => "just now".into(),
        minutes @ 1..60 => format!("{minutes}m ago"),
        minutes @ 60..1440 => format!("{}h ago", minutes / 60),
        _ => local(at).map_or_else(|| "--".into(), |t| t.format("%-d %b").to_string()),
    }
}

/// "online", "last seen …" or "not seen yet".
fn seen(state: &State, person: &Person) -> String {
    match (state.connected.contains(&person.code), person.last_seen) {
        (true, _) => "online".into(),
        (false, Some(at)) => format!("last seen {}", day_and_time(at)),
        (false, None) => "not seen yet".into(),
    }
}

pub fn named(state: &mut State, result: Result<String, String>) {
    match result {
        Ok(name) => {
            state.say(Tone::Good, format!("you're now called {name}"));
            state.name = name;
        }
        Err(why) => state.say(Tone::Bad, why),
    }
}

/// Blocking or removing someone deletes what was waiting to go to them;
/// the chat on screen drops it too.
fn drop_waiting(state: &mut State, code: Code) {
    if let Some(chat) = state.chats.get_mut(&code) {
        chat.messages.retain(|m| m.mark != Mark::Waiting);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::APP;
    use crate::ui::update::{Msg, Outcome, update};
    use corvo_wan_p2p_core::identity::Seed;
    use corvo_wan_p2p_core::store::{Mark, StoredMessage};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use unicode_width::UnicodeWidthStr;

    struct People {
        rahul: Code,
        bob: Code,
        eve: Code,
    }

    fn person(code: Code, name: &str, standing: Standing) -> Person {
        Person { code, name: name.into(), last_seen: None, standing }
    }

    /// Ana knows rahul (a contact), bob (a request) and eve (blocked).
    fn state() -> (State, People) {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        state.code = Some(Seed::generate().code());
        state.title = APP.into();
        let p = People { rahul: Seed::generate().code(), bob: Seed::generate().code(), eve: Seed::generate().code() };
        state.set_people(vec![
            person(p.rahul, "rahul", Standing::Contact),
            person(p.bob, "bob", Standing::Request),
            person(p.eve, "eve", Standing::Blocked),
        ]);
        (state, p)
    }

    fn send(state: &mut State, text: &str) -> Vec<Effect> {
        for c in text.chars() {
            update(state, Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        update(state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
    }

    fn done(state: &mut State, outcome: Outcome) -> Vec<Effect> {
        update(state, Msg::Done(outcome))
    }

    fn said(state: &State) -> Vec<String> {
        state.tabs[0].lines.iter().map(|l| l.text.clone()).collect()
    }

    fn last(state: &State) -> String {
        said(state).last().cloned().unwrap_or_default()
    }

    /// Everyone after `act` on `code`, as the engine would have it.
    fn after(state: &State, code: Code, standing: Option<Standing>) -> Vec<Person> {
        let mut people: Vec<Person> = state.known.iter().filter(|p| p.code != code).cloned().collect();
        if let Some(standing) = standing {
            let name = state.known.iter().find(|p| p.code == code).map_or(String::new(), |p| p.name.clone());
            people.push(person(code, &name, standing));
        }
        people
    }

    fn act(code: Code, act: Act) -> Effect {
        Effect::Act { code, act }
    }

    /// What the box around the code holds, joined.
    fn boxed(lines: &[String]) -> String {
        lines.iter().filter_map(|l| l.strip_prefix("│")).map(|l| l.trim_end_matches('│').trim()).collect()
    }

    #[test]
    fn code_shows_a_box_and_asks_to_save_when_there_is_no_file() {
        for size in [(0, 0), (80, 24), (40, 12)] {
            let (mut s, _) = state();
            s.size = size;
            let mine = s.code.unwrap().to_string();
            assert_eq!(send(&mut s, "/code"), vec![Effect::ShowCode]);
            let lines = said(&s);
            assert_eq!(lines[0], "Your code");
            assert!(lines[1].starts_with('┌') && lines[1].ends_with('┐'));
            assert_eq!(boxed(&lines), mine, "{size:?}: {lines:#?}");
            let widths: Vec<usize> = lines[1..lines.len() - 1].iter().map(|l| l.width()).collect();
            assert!(widths.windows(2).all(|w| w[0] == w[1]), "{size:?}: the box is square: {lines:#?}");
            if let Some(room) = room(&s) {
                assert!(widths[0] <= room, "{size:?}: fits in {room}");
            }
            assert_eq!(last(&s), "Send it to a friend so they can /add you.");
            assert_eq!(s.ask(), None, "nothing asked until the runner says whether the file is there");
            done(&mut s, Outcome::CodeShown { saved: None });
            assert_eq!(s.ask().unwrap().label, "save it to ~/corvo-wan-p2p-code.txt? y/n: ");
        }
    }

    #[test]
    fn code_says_where_the_file_is_when_it_exists() {
        let (mut s, _) = state();
        send(&mut s, "/code");
        done(&mut s, Outcome::CodeShown { saved: Some(PathBuf::from("/home/ana/corvo-wan-p2p-code.txt")) });
        assert_eq!(s.ask(), None);
        assert_eq!(last(&s), "it's saved in /home/ana/corvo-wan-p2p-code.txt");
    }

    #[test]
    fn saving_the_code_writes_the_file() {
        let (mut s, _) = state();
        send(&mut s, "/code");
        done(&mut s, Outcome::CodeShown { saved: None });
        assert!(send(&mut s, "maybe").is_empty());
        assert_eq!(last(&s), "type y or n");
        assert_eq!(send(&mut s, "y"), vec![Effect::WriteCode]);
        assert_eq!(s.ask(), None);
        done(&mut s, Outcome::CodeWritten(Ok(PathBuf::from("/home/ana/corvo-wan-p2p-code.txt"))));
        assert!(last(&s).starts_with("saved in /home/ana/corvo-wan-p2p-code.txt"));
        assert_eq!(last_line_tone(&s), Tone::Good);

        send(&mut s, "/code");
        done(&mut s, Outcome::CodeShown { saved: None });
        assert!(send(&mut s, "n").is_empty());
        assert_eq!(last(&s), "not saved; /code asks again");
    }

    #[test]
    fn add_refuses_a_typo_and_your_own_code() {
        let (mut s, _) = state();
        let mut typo = Seed::generate().code().to_string();
        let end = typo.pop().unwrap();
        typo.push(if end == 'q' { 'p' } else { 'q' });
        let effects = send(&mut s, &format!("/add {typo}"));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Act { .. })));
        assert_eq!(last_line_tone(&s), Tone::Bad);
        assert!(last(&s).contains("typo"));
        let mine = s.code.unwrap();
        assert!(!send(&mut s, &format!("/add {mine}")).iter().any(|e| matches!(e, Effect::Act { .. })));
        assert!(last(&s).contains("your own code"));
    }

    fn last_line_tone(state: &State) -> Tone {
        state.tabs[0].lines.last().unwrap().tone
    }

    #[test]
    fn add_a_new_friend_and_open_the_chat() {
        let (mut s, _) = state();
        let friend = Seed::generate().code();
        assert!(send(&mut s, &format!("/add {friend}")).contains(&act(friend, Act::Add)));
        let people = after(&s, friend, Some(Standing::Contact));
        done(&mut s, Outcome::Acted { code: friend, act: Act::Add, result: Ok(()), people: Some(people) });
        assert_eq!(s.open_peer(), Some(friend));
        assert_eq!(s.standing(&friend), Some(Standing::Contact));
        assert_eq!(last_line_tone(&s), Tone::Good);
    }

    #[test]
    fn add_someone_already_a_contact_just_opens_the_chat() {
        let (mut s, p) = state();
        assert!(!send(&mut s, &format!("/add {}", p.rahul)).iter().any(|e| matches!(e, Effect::Act { .. })));
        assert_eq!(s.open_peer(), Some(p.rahul));
    }

    #[test]
    fn chat_opens_a_contact_or_a_request_by_name() {
        let (mut s, p) = state();
        send(&mut s, "/chat Rahul");
        assert_eq!(s.open_peer(), Some(p.rahul));
        send(&mut s, "/chat bob");
        assert_eq!(s.open_peer(), Some(p.bob));
        send(&mut s, "/chat zed");
        assert_eq!(last(&s), "there's no one called zed");
        send(&mut s, "/chat eve");
        assert!(last(&s).contains("/unblock"));
        assert_eq!(s.open_peer(), Some(p.bob), "nothing else opened");
    }

    /// The lines said since `from`, with their tones.
    fn said_since(state: &State, from: usize) -> Vec<(String, Tone)> {
        state.tabs[0].lines[from..].iter().map(|l| (l.text.clone(), l.tone)).collect()
    }

    #[test]
    fn contacts_print_a_table_sorted_a_to_z_with_requests_after() {
        let (mut s, p) = state();
        assert!(send(&mut s, "/contacts").contains(&Effect::Refresh(Show::Contacts)));
        let mut people = s.known.clone();
        let maria = Seed::generate().code();
        let long = Seed::generate().code();
        let an_hour_ago = chrono::Utc::now().timestamp_millis() - 2 * 3_600_000 - 60_000;
        people.push(Person {
            code: maria,
            name: "Maria".into(),
            last_seen: Some(an_hour_ago),
            standing: Standing::Contact,
        });
        people.push(person(long, "zelda the very long named one", Standing::Contact));
        update(&mut s, Msg::Core(corvo_wan_p2p_core::chat::Event::Online(p.rahul)));
        let from = s.tabs[0].lines.len();
        done(&mut s, Outcome::Refreshed { show: Show::Contacts, result: Ok(people) });
        let lines = said_since(&s, from);
        let text: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
        let short = |c: Code| c.short();
        let expected = [
            "NAME             STATUS          CODE".to_string(),
            format!("Maria            ○ seen 2h ago   {}", short(maria)),
            format!("rahul            ● online        {}", short(p.rahul)),
            format!("zelda the very…  ○ not seen yet  {}", short(long)),
        ];
        assert_eq!(text[..4], expected.iter().map(String::as_str).collect::<Vec<_>>()[..], "{text:#?}");
        assert_eq!(lines[0].1, Tone::Dim, "the heading is dim");
        assert!(lines[1..4].iter().all(|(_, tone)| *tone == Tone::Plain));
        assert_eq!(text[4], "");
        assert_eq!(text[5], "REQUESTS  UNREAD  CODE");
        assert_eq!(text[6], format!("bob       0       {}", short(p.bob)));
        assert_eq!(text[7], "/chat <name> to open a chat · /accept <name> to add");
        assert_eq!(text.len(), 8, "eve is blocked, not listed: {text:#?}");
    }

    #[test]
    fn requests_and_blocked_print_tables_with_a_hint() {
        let (mut s, p) = state();
        s.chats.entry(p.bob).or_default().unread = 2;
        s.refresh_sidebar();
        let from = s.tabs[0].lines.len();
        send(&mut s, "/requests");
        let text: Vec<String> = said_since(&s, from).into_iter().map(|(t, _)| t).collect();
        assert_eq!(
            text,
            [
                "NAME  UNREAD  CODE".to_string(),
                format!("bob   2       {}", p.bob.short()),
                "/chat <name> to read · /accept <name> to add".into()
            ]
        );
        let from = s.tabs[0].lines.len();
        send(&mut s, "/blocked");
        let text: Vec<String> = said_since(&s, from).into_iter().map(|(t, _)| t).collect();
        assert_eq!(
            text,
            ["NAME  CODE".to_string(), format!("eve   {}", p.eve.short()), "/unblock <name> to let them back".into()]
        );
    }

    #[test]
    fn an_empty_list_prints_one_line() {
        let mut s = State::new("ana".into(), "abcdefgh".into());
        s.code = Some(Seed::generate().code());
        for (line, says) in [("/requests", "no requests"), ("/blocked", "you haven't blocked anyone")] {
            let from = s.tabs[0].lines.len();
            send(&mut s, line);
            assert_eq!(said_since(&s, from), [(says.to_string(), Tone::Plain)], "{line}");
        }
        let from = s.tabs[0].lines.len();
        send(&mut s, "/contacts");
        done(&mut s, Outcome::Refreshed { show: Show::Contacts, result: Ok(Vec::new()) });
        assert_eq!(said_since(&s, from).len(), 1);
        assert_eq!(last(&s), "no contacts yet: /add <code> adds a friend");
    }

    #[test]
    fn seen_says_how_long_ago_within_a_day() {
        let now = 1_760_000_000_000;
        assert_eq!(ago(now - 10_000, now), "just now");
        assert_eq!(ago(now - 5 * 60_000, now), "5m ago");
        assert_eq!(ago(now - 2 * 3_600_000, now), "2h ago");
        assert_eq!(ago(now - 23 * 3_600_000, now), "23h ago");
        let days_ago = now - 3 * 86_400_000;
        assert_eq!(ago(days_ago, now), local(days_ago).unwrap().format("%-d %b").to_string());
    }

    #[test]
    fn accept_with_no_name_takes_the_request_in_this_tab() {
        let (mut s, p) = state();
        assert!(!send(&mut s, "/accept").iter().any(|e| matches!(e, Effect::Act { .. })));
        assert!(last(&s).contains("/accept <name>"), "on the corvo tab it asks which");
        s.open_chat(p.bob, true);
        assert!(send(&mut s, "/accept").contains(&act(p.bob, Act::Accept)));
        let people = after(&s, p.bob, Some(Standing::Contact));
        done(&mut s, Outcome::Acted { code: p.bob, act: Act::Accept, result: Ok(()), people: Some(people) });
        assert_eq!(s.standing(&p.bob), Some(Standing::Contact));
        assert!(last(&s).contains("bob is now a contact"));
        send(&mut s, "/accept rahul");
        assert_eq!(last(&s), "rahul is already a contact");
    }

    #[test]
    fn block_closes_their_chat() {
        let (mut s, p) = state();
        assert!(!send(&mut s, "/block").iter().any(|e| matches!(e, Effect::Act { .. })));
        assert!(last(&s).contains("/block <name>"));
        s.open_chat(p.rahul, true);
        assert!(send(&mut s, "/block").contains(&act(p.rahul, Act::Block)));
        let people = after(&s, p.rahul, Some(Standing::Blocked));
        done(&mut s, Outcome::Acted { code: p.rahul, act: Act::Block, result: Ok(()), people: Some(people) });
        assert!(!s.tabs.iter().any(|t| t.peer == Some(p.rahul)));
        assert!(s.on_corvo());
        assert!(last(&s).contains("blocked rahul"));
    }

    #[test]
    fn blocking_or_removing_drops_their_waiting_messages() {
        use corvo_wan_p2p_core::store::StoredMessage;
        for (act, standing) in [(Act::Block, Some(Standing::Blocked)), (Act::Remove { history: false }, None)] {
            let (mut s, p) = state();
            let message = |id: &str, mark| StoredMessage { id: id.into(), sent_at: 1, text: "x".into(), mark };
            let chat = s.chats.entry(p.rahul).or_default();
            chat.messages = vec![message("in", Mark::Received), message("out", Mark::Waiting)];
            let people = after(&s, p.rahul, standing);
            done(&mut s, Outcome::Acted { code: p.rahul, act, result: Ok(()), people: Some(people) });
            // The store deleted what was waiting to go to them.
            let left: Vec<&str> = s.chats[&p.rahul].messages.iter().map(|m| m.id.as_str()).collect();
            assert_eq!(left, ["in"], "{act:?}");
        }
    }

    #[test]
    fn unblock_only_the_blocked() {
        let (mut s, p) = state();
        assert!(send(&mut s, "/unblock eve").contains(&act(p.eve, Act::Unblock)));
        let people = after(&s, p.eve, None);
        done(&mut s, Outcome::Acted { code: p.eve, act: Act::Unblock, result: Ok(()), people: Some(people) });
        assert!(last(&s).contains("unblocked eve"));
        send(&mut s, "/unblock rahul");
        assert_eq!(last(&s), "rahul isn't blocked");
    }

    #[test]
    fn remove_asks_whether_to_delete_the_history() {
        let (mut s, p) = state();
        s.chats.entry(p.rahul).or_default().messages.push(StoredMessage {
            id: "01".into(),
            sent_at: 0,
            text: "hi".into(),
            mark: Mark::Received,
        });
        s.open_chat(p.rahul, true);
        assert!(send(&mut s, "/remove rahul").is_empty());
        assert_eq!(s.ask().unwrap().label, "delete your history too? y/n: ");
        assert!(send(&mut s, "maybe").is_empty());
        assert!(send(&mut s, "y").contains(&act(p.rahul, Act::Remove { history: true })));
        let people = after(&s, p.rahul, None);
        let removed = Act::Remove { history: true };
        done(&mut s, Outcome::Acted { code: p.rahul, act: removed, result: Ok(()), people: Some(people) });
        assert!(!s.tabs.iter().any(|t| t.peer == Some(p.rahul)));
        assert!(!s.chats.contains_key(&p.rahul), "the history is gone");
        assert!(last(&s).contains("removed rahul"));
    }

    #[test]
    fn remove_keeps_the_history_when_asked_to() {
        let (mut s, p) = state();
        s.chats.entry(p.rahul).or_default();
        send(&mut s, "/remove rahul");
        assert!(send(&mut s, "n").contains(&act(p.rahul, Act::Remove { history: false })));
        let people = after(&s, p.rahul, None);
        let removed = Act::Remove { history: false };
        done(&mut s, Outcome::Acted { code: p.rahul, act: removed, result: Ok(()), people: Some(people) });
        assert!(s.chats.contains_key(&p.rahul));
    }

    #[test]
    fn removing_a_request_suggests_block() {
        let (mut s, _) = state();
        send(&mut s, "/remove bob");
        assert!(said(&s).iter().any(|l| l.contains("/block bob")));
        assert!(s.ask().is_some());
    }

    #[test]
    fn info_shows_their_code_and_standing() {
        let (mut s, p) = state();
        assert!(send(&mut s, "/info bob").contains(&Effect::Refresh(Show::Info(p.bob))));
        let people = s.known.clone();
        done(&mut s, Outcome::Refreshed { show: Show::Info(p.bob), result: Ok(people) });
        let lines = said(&s);
        assert!(lines.iter().any(|l| l.contains(&p.bob.to_string())));
        assert!(lines.iter().any(|l| l.contains("request")));
    }

    #[test]
    fn name_changes_yours() {
        let (mut s, _) = state();
        assert!(send(&mut s, "/name Ana Maria").contains(&Effect::SetName("Ana Maria".into())));
        done(&mut s, Outcome::Named(Ok("Ana Maria".into())));
        assert_eq!(s.name, "Ana Maria");
        assert!(last(&s).contains("Ana Maria"));
    }

    #[test]
    fn a_failed_act_says_why() {
        let (mut s, p) = state();
        done(
            &mut s,
            Outcome::Acted {
                code: p.bob,
                act: Act::Accept,
                result: Err("they haven't sent you a request".into()),
                people: None,
            },
        );
        assert_eq!(last(&s), "they haven't sent you a request");
        assert_eq!(last_line_tone(&s), Tone::Bad);
    }

    #[test]
    fn blocking_or_removing_someone_online_clears_their_dot() {
        for (act_, standing) in [(Act::Block, Some(Standing::Blocked)), (Act::Remove { history: false }, None)] {
            let (mut s, p) = state();
            update(&mut s, Msg::Core(corvo_wan_p2p_core::chat::Event::Online(p.rahul)));
            let people = after(&s, p.rahul, standing);
            done(&mut s, Outcome::Acted { code: p.rahul, act: act_, result: Ok(()), people: Some(people) });
            assert!(!s.connected.contains(&p.rahul), "{act_:?}");
            // Added back later while they're offline, they mustn't show online.
            let people = after(&s, p.rahul, Some(Standing::Contact));
            done(&mut s, Outcome::Acted { code: p.rahul, act: Act::Add, result: Ok(()), people: Some(people) });
            assert!(!s.people.iter().find(|x| x.code == p.rahul).unwrap().online, "{act_:?}");
        }
    }
}
