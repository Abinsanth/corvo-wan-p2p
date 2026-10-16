//! Chatting: what the engine says (messages, marks, people coming and going,
//! the network), sending, and unread counts.

use chrono::{DateTime, Local};
use corvo_wan_p2p_core::chat::Event;
use corvo_wan_p2p_core::identity::Code;
use corvo_wan_p2p_core::proto::PROTO_VERSION;
use corvo_wan_p2p_core::store::{Mark, Person, Standing, StoredMessage};

use super::state::{APP, Chat, Screen, State, Tone};
use super::update::Effect;
use super::view::{areas, input_rows};

/// Wait this long (10 seconds of ticks) for the first connection before
/// saying "offline": the engine reports offline at start, before its relay
/// has had a chance to answer.
pub const CONNECTING_TICKS: u32 = 40;

/// Everything the chat screens need at start.
#[derive(Debug)]
pub struct Loaded {
    pub people: Vec<Person>,
    pub chats: Vec<History>,
    /// People whose saved details couldn't be read (a damaged store).
    pub unreadable_people: usize,
}

/// One conversation as stored, and the newest of their messages you've read.
#[derive(Debug)]
pub struct History {
    pub code: Code,
    pub messages: Vec<StoredMessage>,
    pub read_upto: Option<String>,
    /// Messages that couldn't be read (a damaged store); the rest are here.
    pub unreadable: usize,
}

pub fn on_event(state: &mut State, event: Event) {
    match event {
        Event::Online(code) => {
            state.connected.insert(code);
            state.refresh_sidebar();
        }
        Event::Offline(code) => {
            state.connected.remove(&code);
            state.refresh_sidebar();
        }
        Event::Request { from, name } => {
            know(state, from, &name);
            let label = state.label(&from);
            state.say(
                Tone::Warn,
                format!("{label} wants to chat: /chat {label} to read, /accept to add them, /block to stop them"),
            );
        }
        Event::Received { from, id, sent_at, text } => {
            know(state, from, "");
            let chat = state.chats.entry(from).or_default();
            // Already read from the store at start (it arrived while loading).
            if chat.messages.iter().any(|m| m.id == id) {
                return;
            }
            chat.messages.push(StoredMessage { id, sent_at, text, mark: Mark::Received });
            chat.unread += 1;
            if state.standing(&from) == Some(Standing::Contact) {
                state.open_chat(from, false);
            }
            state.refresh_sidebar();
        }
        Event::Delivered { to, id } => {
            if let Some(m) = state.chats.get_mut(&to).and_then(|c| c.messages.iter_mut().find(|m| m.id == id))
                && m.mark == Mark::Waiting
            {
                m.mark = Mark::Delivered;
            }
        }
        Event::Read { by, up_to } => {
            if let Some(chat) = state.chats.get_mut(&by)
                && let Some(at) = chat.messages.iter().position(|m| m.id == up_to)
            {
                for m in chat.messages[..=at].iter_mut().filter(|m| m.mark.is_outgoing()) {
                    m.mark = Mark::Read;
                }
            }
        }
        Event::Name { of, name } => {
            let before = state.label(&of);
            if let Some(person) = state.known.iter_mut().find(|p| p.code == of) {
                person.name = name;
                state.relabel();
                let after = state.label(&of);
                if after != before {
                    state.say(Tone::Plain, format!("{before} is now called {after}"));
                }
            }
        }
        Event::VersionMismatch { peer, theirs } => {
            let label = state.label(&peer);
            state.say(
                Tone::Warn,
                format!(
                    "{label}'s app speaks version {theirs} of the protocol and yours speaks {PROTO_VERSION}: \
                     whoever has the older app should /update. Messages wait until then."
                ),
            );
        }
        Event::Network { online: true } => state.online = Some(true),
        // Before the first connection, `tick` decides when it's really offline.
        Event::Network { online: false } => {
            if state.online.is_some() {
                state.online = Some(false);
            }
        }
        Event::Problem(text) => state.say(Tone::Bad, text),
    }
}

/// Adds someone the engine just told us about as a request, if they're new.
fn know(state: &mut State, code: Code, name: &str) {
    if state.standing(&code).is_none() {
        state.known.push(Person { code, name: name.to_string(), last_seen: None, standing: Standing::Request });
        state.relabel();
    }
}

/// A line typed in a chat tab.
pub fn send(state: &mut State, to: Code, text: String, effects: &mut Vec<Effect>) {
    let label = state.label(&to);
    match state.standing(&to) {
        Some(Standing::Request) => {
            state.say(Tone::Bad, format!("{label} isn't a contact yet: /accept them first to reply"))
        }
        Some(Standing::Blocked) => state.say(Tone::Bad, format!("you've blocked {label}: /unblock them first")),
        _ => effects.push(Effect::Send { to, text }),
    }
}

pub fn sent(state: &mut State, to: Code, text: String, result: Result<StoredMessage, String>) {
    match result {
        Ok(message) => state.chats.entry(to).or_default().messages.push(message),
        Err(why) => {
            state.say(Tone::Bad, why);
            if state.editor.is_empty() {
                state.editor.set(text);
            }
        }
    }
}

pub fn loaded(state: &mut State, result: Result<Loaded, String>) {
    match result {
        Ok(Loaded { people, chats, unreadable_people }) => {
            let mut damaged = Vec::new();
            for History { code, messages, read_upto, unreadable } in chats {
                let unread = unread_after(&messages, read_upto.as_deref());
                state.chats.insert(code, Chat { messages, unread });
                if unreadable > 0 {
                    damaged.push((unreadable, code));
                }
            }
            state.set_people(people);
            const DAMAGED: &str = "couldn't be read: that part of your saved chats is damaged. The rest is fine.";
            for (n, code) in damaged {
                let messages = if n == 1 { "message" } else { "messages" };
                let label = state.label(&code);
                state.say(Tone::Warn, format!("{n} {messages} with {label} {DAMAGED}"));
            }
            if unreadable_people > 0 {
                let n = unreadable_people;
                let who = if n == 1 { "person you know" } else { "people you know" };
                state.say(Tone::Warn, format!("{n} {who} {DAMAGED}"));
            }
        }
        Err(why) => state.say(Tone::Bad, format!("couldn't read your chats: {why}")),
    }
}

/// Their messages after `read_upto`; all of them if nothing was read.
fn unread_after(messages: &[StoredMessage], read_upto: Option<&str>) -> usize {
    let after = read_upto.and_then(|id| messages.iter().position(|m| m.id == id)).map_or(0, |at| at + 1);
    messages[after..].iter().filter(|m| m.mark == Mark::Received).count()
}

/// A time in Unix milliseconds in the local time zone.
pub fn local(ms: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp_millis(ms).map(|t| t.with_timezone(&Local))
}

/// A day and time, like `Thu 15 Oct 14:02`.
pub fn day_and_time(ms: i64) -> String {
    local(ms).map_or_else(|| "--".into(), |t| t.format("%a %-d %b %H:%M").to_string())
}

/// Four times a second.
pub fn tick(state: &mut State) {
    state.since_start = state.since_start.saturating_add(1);
    if state.online.is_none() && state.since_start >= CONNECTING_TICKS {
        state.online = Some(false);
    }
}

/// Marks the open chat read when its newest messages are on screen: the
/// window has focus, it's big enough to draw, the chat is scrolled to its end
/// and nothing covers it.
pub fn read_open_chat(state: &mut State, effects: &mut Vec<Effect>) {
    let visible = state.focused
        && matches!(state.screen, Screen::Main)
        && areas(state.size, input_rows(state)).is_some()
        && !state.scroll.is_scrolled()
        && state.reveal.is_none();
    if !visible {
        return;
    }
    if let Some(peer) = state.open_peer()
        && let Some(chat) = state.chats.get_mut(&peer)
        && chat.unread > 0
    {
        chat.unread = 0;
        state.refresh_sidebar();
        effects.push(Effect::MarkRead(peer));
    }
}

/// The window's title: the unread count, then the app's name.
pub fn title(state: &State) -> String {
    match state.unread_total() {
        0 => APP.to_string(),
        n => format!("({n}) {APP}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::update::{Msg, Outcome, update};
    use corvo_wan_p2p_core::identity::Seed;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn person(name: &str, standing: Standing) -> Person {
        Person { code: Seed::generate().code(), name: name.into(), last_seen: None, standing }
    }

    /// Ana, with rahul as a contact and bob as a request, on the corvo tab.
    fn state() -> (State, Code, Code) {
        let mut state = State::new("ana".into(), "abcdefgh".into());
        let (rahul, bob) = (person("rahul", Standing::Contact), person("bob", Standing::Request));
        let codes = (rahul.code, bob.code);
        state.set_people(vec![rahul, bob]);
        state.title = APP.into();
        (state, codes.0, codes.1)
    }

    fn message(id: &str, text: &str, mark: Mark) -> StoredMessage {
        StoredMessage { id: id.into(), sent_at: 1_760_000_000_000, text: text.into(), mark }
    }

    fn received(from: Code, id: &str) -> Msg {
        Msg::Core(Event::Received { from, id: id.into(), sent_at: 1_760_000_000_000, text: format!("text {id}") })
    }

    fn key(state: &mut State, code: KeyCode, modifiers: KeyModifiers) -> Vec<Effect> {
        update(state, Msg::Key(KeyEvent::new(code, modifiers)))
    }

    fn send_line(state: &mut State, text: &str) -> Vec<Effect> {
        for c in text.chars() {
            key(state, KeyCode::Char(c), KeyModifiers::NONE);
        }
        key(state, KeyCode::Enter, KeyModifiers::NONE)
    }

    fn marks(state: &State, who: Code) -> Vec<Mark> {
        state.chats[&who].messages.iter().map(|m| m.mark).collect()
    }

    fn last_corvo(state: &State) -> String {
        state.tabs[0].lines.last().unwrap().text.clone()
    }

    #[test]
    fn a_sent_message_waits_then_is_delivered_then_read() {
        let (mut s, rahul, _) = state();
        s.open_chat(rahul, true);
        let effects = send_line(&mut s, "hi there");
        assert_eq!(effects, vec![Effect::Send { to: rahul, text: "hi there".into() }]);
        let sent =
            |id: &str| Outcome::Sent { to: rahul, text: "x".into(), result: Ok(message(id, "x", Mark::Waiting)) };
        update(&mut s, Msg::Done(sent("01")));
        update(&mut s, Msg::Done(sent("02")));
        assert_eq!(marks(&s, rahul), vec![Mark::Waiting, Mark::Waiting], "· at once");
        update(&mut s, Msg::Core(Event::Delivered { to: rahul, id: "01".into() }));
        assert_eq!(marks(&s, rahul), vec![Mark::Delivered, Mark::Waiting]);
        update(&mut s, Msg::Core(Event::Read { by: rahul, up_to: "02".into() }));
        assert_eq!(marks(&s, rahul), vec![Mark::Read, Mark::Read], "read up to and including");
        update(&mut s, Msg::Core(Event::Delivered { to: rahul, id: "02".into() }));
        assert_eq!(marks(&s, rahul), vec![Mark::Read, Mark::Read], "a late ✓ doesn't undo ✓✓");
    }

    #[test]
    fn a_failed_send_flashes_and_gives_the_text_back() {
        let (mut s, rahul, _) = state();
        s.open_chat(rahul, true);
        send_line(&mut s, "long");
        let why = "that message is too long; the most is 16 KB".to_string();
        update(&mut s, Msg::Done(Outcome::Sent { to: rahul, text: "long".into(), result: Err(why.clone()) }));
        assert_eq!(s.flash.as_ref().unwrap().line.text, why);
        assert_eq!(s.editor.text(), "long", "nothing typed is lost");
        assert!(s.chats.get(&rahul).is_none_or(|c| c.messages.is_empty()));
    }

    #[test]
    fn a_request_must_be_accepted_before_replying() {
        let (mut s, _, bob) = state();
        s.open_chat(bob, true);
        assert!(send_line(&mut s, "hello?").is_empty());
        assert!(last_corvo(&s).contains("/accept"));
        assert_eq!(s.flash.as_ref().unwrap().line.tone, Tone::Bad);
    }

    #[test]
    fn a_message_elsewhere_raises_unread_and_the_title() {
        let (mut s, rahul, _) = state();
        let effects = update(&mut s, received(rahul, "01"));
        assert_eq!(effects, vec![Effect::SetTitle(format!("(1) {APP}"))]);
        update(&mut s, received(rahul, "02"));
        assert_eq!(s.chats[&rahul].unread, 2);
        assert_eq!(s.title, format!("(2) {APP}"));
        assert_eq!(s.people.iter().find(|p| p.code == rahul).unwrap().unread, 2, "the sidebar shows it");
        assert!(s.tabs.iter().any(|t| t.peer == Some(rahul)), "a contact's chat opens");
        assert!(s.on_corvo(), "without switching to it");
    }

    #[test]
    fn opening_the_tab_marks_it_read() {
        let (mut s, rahul, _) = state();
        update(&mut s, received(rahul, "01"));
        let effects = key(&mut s, KeyCode::Char('2'), KeyModifiers::ALT);
        assert!(effects.contains(&Effect::MarkRead(rahul)));
        assert!(effects.contains(&Effect::SetTitle(APP.into())));
        assert_eq!(s.chats[&rahul].unread, 0);
        assert!(!key(&mut s, KeyCode::Char('2'), KeyModifiers::ALT).contains(&Effect::MarkRead(rahul)), "only once");
    }

    #[test]
    fn the_open_chat_is_read_only_while_the_window_has_focus() {
        let (mut s, rahul, _) = state();
        s.open_chat(rahul, true);
        assert!(update(&mut s, received(rahul, "01")).contains(&Effect::MarkRead(rahul)));
        update(&mut s, Msg::Focus(false));
        assert!(!update(&mut s, received(rahul, "02")).contains(&Effect::MarkRead(rahul)));
        assert_eq!(s.chats[&rahul].unread, 1);
        assert!(update(&mut s, Msg::Focus(true)).contains(&Effect::MarkRead(rahul)));
        assert_eq!(s.chats[&rahul].unread, 0);
    }

    #[test]
    fn online_dots_follow_the_engine() {
        let (mut s, rahul, _) = state();
        let online = |s: &State| s.people.iter().find(|p| p.code == rahul).unwrap().online;
        assert!(!online(&s));
        update(&mut s, Msg::Core(Event::Online(rahul)));
        assert!(online(&s));
        update(&mut s, Msg::Core(Event::Offline(rahul)));
        assert!(!online(&s));
    }

    #[test]
    fn offline_is_only_said_after_waiting_for_a_first_connection() {
        let (mut s, _, _) = state();
        update(&mut s, Msg::Core(Event::Network { online: false }));
        for _ in 1..CONNECTING_TICKS {
            update(&mut s, Msg::Tick);
        }
        assert_eq!(s.online, None, "still connecting");
        update(&mut s, Msg::Tick);
        assert_eq!(s.online, Some(false));
        update(&mut s, Msg::Core(Event::Network { online: true }));
        assert_eq!(s.online, Some(true));
        update(&mut s, Msg::Core(Event::Network { online: false }));
        assert_eq!(s.online, Some(false), "once online, offline shows at once");
    }

    #[test]
    fn a_new_request_is_listed_and_said() {
        let (mut s, _, _) = state();
        let stranger = Seed::generate().code();
        update(&mut s, Msg::Core(Event::Request { from: stranger, name: "carol".into() }));
        update(&mut s, received(stranger, "01"));
        let requests: Vec<_> = s.people.iter().filter(|p| p.request).map(|p| p.label.as_str()).collect();
        assert_eq!(requests, vec!["bob", "carol"]);
        assert!(last_corvo(&s).contains("carol"));
        assert!(last_corvo(&s).contains("/accept"));
        assert_eq!(s.chats[&stranger].unread, 1);
        assert!(!s.tabs.iter().any(|t| t.peer == Some(stranger)), "a request's chat waits in the sidebar");
    }

    #[test]
    fn renames_and_version_mismatches_are_said() {
        let (mut s, rahul, _) = state();
        s.open_chat(rahul, false);
        update(&mut s, Msg::Core(Event::Name { of: rahul, name: "rahul k".into() }));
        assert_eq!(s.label(&rahul), "rahul k");
        assert_eq!(s.tabs[1].title, "rahul k");
        assert_eq!(last_corvo(&s), "rahul is now called rahul k");
        update(&mut s, Msg::Core(Event::VersionMismatch { peer: rahul, theirs: 2 }));
        assert!(last_corvo(&s).contains("rahul k"));
        assert!(last_corvo(&s).contains("/update"));
    }

    #[test]
    fn loading_counts_what_came_after_the_last_read() {
        let (mut s, rahul, bob) = state();
        let people = s.known.clone();
        let history = vec![
            message("01", "a", Mark::Received),
            message("02", "b", Mark::Received),
            message("03", "mine", Mark::Read),
            message("04", "c", Mark::Received),
        ];
        let chats = vec![
            History { code: rahul, messages: history.clone(), read_upto: Some("02".into()), unreadable: 0 },
            History { code: bob, messages: history, read_upto: None, unreadable: 0 },
        ];
        update(&mut s, Msg::Done(Outcome::Loaded(Ok(Loaded { people, chats, unreadable_people: 0 }))));
        assert_eq!(s.chats[&rahul].unread, 1);
        assert_eq!(s.chats[&bob].unread, 3);
        assert_eq!(s.title, format!("(4) {APP}"));
        assert_eq!(s.chats[&rahul].messages.len(), 4);
    }

    #[test]
    fn starting_loads_the_chats() {
        let mut s = State::onboarding(crate::ui::onboarding::Onboarding::start(true, false));
        let code = Seed::generate().code();
        let effects = update(&mut s, Msg::Done(Outcome::Started { name: "ana".into(), code, recovery_file: None }));
        assert!(effects.contains(&Effect::Load));
    }

    #[test]
    fn a_slow_unlock_still_starts_as_connecting() {
        let mut s = State::onboarding(crate::ui::onboarding::Onboarding::start(true, false));
        for _ in 0..CONNECTING_TICKS + 5 {
            update(&mut s, Msg::Tick);
        }
        let code = Seed::generate().code();
        update(&mut s, Msg::Done(Outcome::Started { name: "ana".into(), code, recovery_file: None }));
        assert_eq!(s.online, None, "the wait starts once the engine does");
    }

    #[test]
    fn messages_not_on_screen_are_not_marked_read() {
        let (mut s, rahul, _) = state();
        s.size = (80, 24);
        s.open_chat(rahul, true);
        for i in 0..60 {
            update(&mut s, received(rahul, &format!("{i:02}")));
        }
        key(&mut s, KeyCode::PageUp, KeyModifiers::NONE);
        assert!(s.scroll.is_scrolled());
        assert!(!update(&mut s, received(rahul, "x1")).contains(&Effect::MarkRead(rahul)), "scrolled up");
        assert!(key(&mut s, KeyCode::Esc, KeyModifiers::NONE).contains(&Effect::MarkRead(rahul)), "back at the end");

        s.reveal = Some(crate::ui::state::Secret::new("words".into()));
        assert!(!update(&mut s, received(rahul, "x2")).contains(&Effect::MarkRead(rahul)), "the words cover it");
        s.reveal = None;
        update(&mut s, Msg::Resize(30, 8));
        assert!(!update(&mut s, received(rahul, "x3")).contains(&Effect::MarkRead(rahul)), "too small to draw");
        assert!(update(&mut s, Msg::Resize(80, 24)).contains(&Effect::MarkRead(rahul)));
    }

    #[test]
    fn a_message_already_loaded_is_not_counted_twice() {
        let (mut s, rahul, _) = state();
        let people = s.known.clone();
        let chats = vec![History {
            code: rahul,
            messages: vec![message("01", "a", Mark::Received)],
            read_upto: None,
            unreadable: 0,
        }];
        update(&mut s, Msg::Done(Outcome::Loaded(Ok(Loaded { people, chats, unreadable_people: 0 }))));
        update(&mut s, received(rahul, "01"));
        assert_eq!(s.chats[&rahul].messages.len(), 1);
        assert_eq!(s.chats[&rahul].unread, 1);
    }

    #[test]
    fn unreadable_rows_are_mentioned_and_the_rest_is_shown() {
        let (mut s, rahul, _) = state();
        let people = s.known.clone();
        let messages = vec![message("01", "a", Mark::Received)];
        let chats = vec![History { code: rahul, messages, read_upto: None, unreadable: 2 }];
        update(&mut s, Msg::Done(Outcome::Loaded(Ok(Loaded { people, chats, unreadable_people: 1 }))));
        assert_eq!(s.chats[&rahul].messages.len(), 1);
        assert_eq!(s.known.len(), 2);
        let said: Vec<&str> = s.tabs[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert!(said.iter().any(|l| l.starts_with("2 messages with rahul couldn't be read")), "{said:?}");
        assert!(said.iter().any(|l| l.starts_with("1 person you know couldn't be read")), "{said:?}");
    }
}
