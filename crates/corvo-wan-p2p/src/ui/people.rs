//! The commands about people: `/code`, `/add`, `/chat`, `/contacts`,
//! `/requests`, `/accept`, `/block`, `/unblock`, `/blocked`, `/remove`,
//! `/info`, `/name`, and `/update` for now.

use corvo_wan_p2p_core::identity::Code;
use corvo_wan_p2p_core::names;
use corvo_wan_p2p_core::store::{Person, Standing};

use super::account::Pending;
use super::chat::day_and_time;
use super::clip::{MAYBE, NO_COPY};
use super::commands::Cmd;
use super::state::{State, Tone};
use super::update::Effect;

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
            state.say(Tone::Plain, format!("your code: {code}"));
            let copy = if state.can_copy {
                format!("sent to your clipboard, {MAYBE}.")
            } else {
                format!("{NO_COPY}: select it above to copy it.")
            };
            state.say(
                Tone::Plain,
                format!("{copy} Send it to friends any way you like; they add you with /add and your code."),
            );
            effects.push(Effect::Copy(code.to_string()));
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
            let requests: Vec<String> = state
                .people
                .iter()
                .filter(|p| p.request)
                .map(|p| if p.unread > 0 { format!("{} ({} unread)", p.label, p.unread) } else { p.label.clone() })
                .collect();
            if requests.is_empty() {
                state.say(Tone::Plain, "no requests");
            } else {
                state.say(
                    Tone::Plain,
                    format!("requests: {}. /chat <name> to read, /accept to add", requests.join(", ")),
                );
            }
        }
        Cmd::Blocked => {
            state.select_tab(0);
            let mut blocked: Vec<String> =
                state.known.iter().filter(|p| p.standing == Standing::Blocked).map(|p| state.label(&p.code)).collect();
            blocked.sort_by_key(|l| l.to_lowercase());
            if blocked.is_empty() {
                state.say(Tone::Plain, "you haven't blocked anyone");
            } else {
                state.say(Tone::Plain, format!("blocked: {}", blocked.join(", ")));
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
            let mut contacts: Vec<&Person> = state.known.iter().filter(|p| p.standing == Standing::Contact).collect();
            contacts.sort_by_key(|p| state.label(&p.code).to_lowercase());
            let lines: Vec<String> =
                contacts.iter().map(|p| format!("{} {}", state.label(&p.code), seen(state, p))).collect();
            if lines.is_empty() {
                state.say(Tone::Plain, "no contacts yet: /add <code> adds a friend");
            }
            for line in lines {
                state.say(Tone::Plain, line);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::APP;
    use crate::ui::update::{Msg, Outcome, update};
    use corvo_wan_p2p_core::identity::Seed;
    use corvo_wan_p2p_core::store::{Mark, StoredMessage};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

    #[test]
    fn code_shows_and_copies_your_code() {
        let (mut s, _) = state();
        let mine = s.code.unwrap().to_string();
        let effects = send(&mut s, "/code");
        assert!(effects.contains(&Effect::Copy(mine.clone())));
        assert!(said(&s).iter().any(|l| l.contains(&mine)));
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

    #[test]
    fn contacts_lists_them_with_online_or_last_seen() {
        let (mut s, p) = state();
        assert!(send(&mut s, "/contacts").contains(&Effect::Refresh(Show::Contacts)));
        let mut people = s.known.clone();
        people.push(Person {
            code: Seed::generate().code(),
            name: "maria".into(),
            last_seen: Some(1),
            standing: Standing::Contact,
        });
        update(&mut s, Msg::Core(corvo_wan_p2p_core::chat::Event::Online(p.rahul)));
        done(&mut s, Outcome::Refreshed { show: Show::Contacts, result: Ok(people) });
        let lines = said(&s);
        assert!(lines.iter().any(|l| l.contains("rahul") && l.contains("online")));
        assert!(lines.iter().any(|l| l.contains("maria") && l.contains("last seen")));
        assert!(!lines.iter().any(|l| l.contains("bob") || l.contains("eve")), "only contacts");
    }

    #[test]
    fn requests_and_blocked_are_listed() {
        let (mut s, p) = state();
        s.chats.entry(p.bob).or_default().unread = 2;
        s.refresh_sidebar();
        send(&mut s, "/requests");
        assert!(last(&s).contains("bob") && last(&s).contains('2'));
        send(&mut s, "/blocked");
        assert!(last(&s).contains("eve"));
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
