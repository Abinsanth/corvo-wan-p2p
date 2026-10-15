//! People who aren't your contacts: requests, blocking, removing, and the
//! limits on what strangers and misbehaving peers can make the app hold.

mod common;

use std::time::Duration;

use common::{Fake, Peer, eventually, frames_until_quiet, friends, is_cut_off, next_frame, next_frames, say};
use corvo_wan_p2p_core::chat::{ChatError, Event, MAX_GUESTS, MAX_REQUEST_MESSAGES, MAX_REQUESTS, WINDOW};
use corvo_wan_p2p_core::identity::Seed;
use corvo_wan_p2p_core::proto::{Frame, PROTO_VERSION, write_frame};
use corvo_wan_p2p_core::store::{Mark, Standing};
use iroh::address_lookup::MemoryLookup;

const QUIET: Duration = Duration::from_millis(500);

#[tokio::test(flavor = "multi_thread")]
async fn a_first_message_from_a_stranger_becomes_a_request_until_accepted() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let mut b = Peer::start(&lookup, "rahul").await;
    let (ac, bc) = (a.code, b.code);
    b.client.add_contact(ac).await.unwrap();
    let id = b.client.send(ac, "hi, it's rahul").await.unwrap();

    a.expect(|e| *e == Event::Request { from: bc, name: "rahul".into() }).await;
    a.expect(|e| matches!(e, Event::Received { from, text, .. } if *from == bc && text == "hi, it's rahul")).await;
    b.expect(|e| *e == Event::Delivered { to: ac, id: id.clone() }).await;
    let people = a.client.people().await.unwrap();
    assert_eq!((people.len(), people[0].standing, people[0].name.as_str()), (1, Standing::Request, "rahul"));
    assert!(a.client.contacts().await.unwrap().is_empty());
    assert!(matches!(a.client.send(bc, "can't yet").await, Err(ChatError::NotAContact)));

    // No read receipt and no name for a request.
    a.client.mark_read(bc).await.unwrap();
    b.expect_none(QUIET, |e| matches!(e, Event::Read { .. } | Event::Name { .. })).await;

    a.client.accept(bc).await.unwrap();
    a.expect(|e| *e == Event::Online(bc)).await;
    b.expect(|e| *e == Event::Name { of: ac, name: "abin".into() }).await;
    a.client.mark_read(bc).await.unwrap();
    b.expect(|e| *e == Event::Read { by: ac, up_to: id.clone() }).await;
    a.client.send(bc, "welcome").await.unwrap();
    b.expect(|e| matches!(e, Event::Received { text, .. } if text == "welcome")).await;
    assert!(matches!(a.client.accept(bc).await, Err(ChatError::NotARequest)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_renames_is_reported() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let b = Peer::start(&lookup, "rahul").await;
    let bc = b.code;
    b.client.add_contact(a.code).await.unwrap();
    b.client.send(a.code, "hi").await.unwrap();
    a.expect(|e| matches!(e, Event::Request { .. })).await;
    b.client.set_name("Rahul K").await.unwrap();
    a.expect(|e| *e == Event::Name { of: bc, name: "Rahul K".into() }).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stranger_who_says_nothing_is_not_a_request() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    let (mut send, _recv, theirs) = fake.hello(a.code, PROTO_VERSION).await.expect("strangers get a hello");
    assert!(matches!(theirs, Frame::Hello { .. }));
    write_frame(&mut send, &Frame::Name { name: "lurker".into() }).await.unwrap();
    a.expect_none(QUIET, |e| !matches!(e, Event::Problem(_))).await;
    assert!(a.client.people().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_someone_who_sent_a_request_accepts_it() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let b = Peer::start(&lookup, "rahul").await;
    let bc = b.code;
    b.client.add_contact(a.code).await.unwrap();
    b.client.send(a.code, "hi").await.unwrap();
    a.expect(|e| matches!(e, Event::Request { .. })).await;
    a.client.add_contact(bc).await.unwrap();
    a.expect(|e| *e == Event::Online(bc)).await;
    assert_eq!(a.client.people().await.unwrap()[0].standing, Standing::Contact);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_contact_sees_you_offline_and_nothing_queued_reaches_them() {
    let lookup = MemoryLookup::new();
    let (mut a, b) = friends(&lookup).await;
    let (ac, bc) = (a.code, b.code);
    let stopped = b.stop().await;
    a.expect(|e| *e == Event::Offline(bc)).await;
    a.client.send(bc, "never sent").await.unwrap();
    a.client.block(bc).await.unwrap();
    a.skip_pending().await; // a leftover from the start-up double dial may still be there
    assert!(a.client.history(bc).await.unwrap().is_empty(), "the queued message is dropped");
    assert!(matches!(a.client.add_contact(bc).await, Err(ChatError::Blocked)));
    assert!(matches!(a.client.send(bc, "no").await, Err(ChatError::NotAContact)));

    let mut b = Peer::resume(&lookup, stopped).await;
    let id = b.client.send(ac, "are you there?").await.unwrap();
    b.expect_none(Duration::from_millis(1500), |e| *e == Event::Online(ac)).await;
    a.expect_none(QUIET, |e| !matches!(e, Event::Problem(_))).await;
    assert_eq!(b.client.history(ac).await.unwrap()[0].mark, Mark::Waiting);

    // Unblocked, they're a stranger again: their waiting message makes a request.
    a.client.unblock(bc).await.unwrap();
    a.expect(|e| matches!(e, Event::Request { from, .. } if *from == bc)).await;
    b.expect(|e| *e == Event::Delivered { to: ac, id: id.clone() }).await;
    assert!(matches!(a.client.unblock(bc).await, Err(ChatError::NotBlocked)));
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_a_connected_contact_cuts_them_off() {
    let lookup = MemoryLookup::new();
    let (a, mut b) = friends(&lookup).await;
    let ac = a.code;
    a.client.block(b.code).await.unwrap();
    b.expect(|e| *e == Event::Offline(ac)).await;
    // A connection whose hellos were exchanged before the block may still come
    // up for a moment; let those settle. Nothing after the block says hello.
    tokio::time::sleep(QUIET).await;
    b.skip_pending().await;
    b.expect_none(Duration::from_millis(1500), |e| *e == Event::Online(ac)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_stranger_is_refused_before_hello() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    say(&mut send, "m1", "buy my stuff").await;
    a.expect(|e| matches!(e, Event::Request { .. })).await;
    a.client.block(fake.code).await.unwrap();
    assert!(is_cut_off(&mut recv).await);
    assert!(fake.hello(a.code, PROTO_VERSION).await.is_none(), "no hello for someone blocked");
    assert!(matches!(a.client.block(a.code).await, Err(ChatError::Yourself)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_contact_comes_back_as_a_request_when_they_write() {
    let lookup = MemoryLookup::new();
    let (mut a, mut b) = friends(&lookup).await;
    let (ac, bc) = (a.code, b.code);
    let before = b.client.send(ac, "before").await.unwrap();
    b.expect(|e| *e == Event::Delivered { to: ac, id: before.clone() }).await;
    a.client.remove(bc, false).await.unwrap();
    assert!(a.client.people().await.unwrap().is_empty());
    // Their app still has you, reconnects, and is let in as a guest.
    b.expect(|e| *e == Event::Offline(ac)).await;
    b.expect(|e| *e == Event::Online(ac)).await;
    a.expect_none(QUIET, |e| *e == Event::Online(bc)).await;

    b.client.send(ac, "still there?").await.unwrap();
    a.expect(|e| matches!(e, Event::Request { from, name } if *from == bc && name == "rahul")).await;
    let texts: Vec<String> = a.client.history(bc).await.unwrap().into_iter().map(|m| m.text).collect();
    assert_eq!(texts, ["before", "still there?"], "history was kept");

    a.client.remove(bc, true).await.unwrap();
    assert!(a.client.history(bc).await.unwrap().is_empty());
    assert!(matches!(a.client.remove(bc, true).await, Err(ChatError::NotAContact)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_resent_after_removal_is_acked_but_is_not_a_request() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    a.client.add_contact(fake.code).await.unwrap();
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    say(&mut send, "m1", "hello").await;
    next_frame(&mut recv, |f| matches!(f, Frame::Ack { .. })).await;
    a.client.remove(fake.code, false).await.unwrap();
    a.skip_pending().await;
    assert!(is_cut_off(&mut recv).await);
    // Their app never saw the ack, so it sends m1 again on the next connection.
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    say(&mut send, "m1", "hello").await;
    assert_eq!(next_frame(&mut recv, |f| matches!(f, Frame::Ack { .. })).await, Frame::Ack { id: "m1".into() });
    a.expect_none(QUIET, |e| matches!(e, Event::Request { .. } | Event::Received { .. })).await;
    assert!(a.client.people().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_saves_only_so_many_messages() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    for i in 0..MAX_REQUEST_MESSAGES + 5 {
        say(&mut send, &format!("m{i}"), "spam").await;
    }
    say(&mut send, "m0", "spam").await; // a repeat is still acked
    let acks = next_frames(&mut recv, MAX_REQUEST_MESSAGES + 1).await;
    assert!(acks.iter().all(|f| matches!(f, Frame::Ack { .. })), "a request gets acks only: {acks:?}");
    assert_eq!(acks.last(), Some(&Frame::Ack { id: "m0".into() }));
    let more = frames_until_quiet(&mut recv, QUIET).await;
    assert!(more.is_empty(), "messages past the limit were acked: {more:?}");
    assert_eq!(a.client.history(fake.code).await.unwrap().len(), MAX_REQUEST_MESSAGES);
    a.expect(|e| matches!(e, Event::Request { .. })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn once_requests_are_full_new_strangers_are_refused() {
    let lookup = MemoryLookup::new();
    let a = Peer::start(&lookup, "abin").await;
    let mut askers = Vec::new();
    for i in 0..MAX_REQUESTS {
        let fake = Fake::new(&lookup).await;
        let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
        say(&mut send, "m", &format!("request {i}")).await;
        next_frame(&mut recv, |f| matches!(f, Frame::Ack { .. })).await;
        askers.push(fake); // hanging up frees the guest slot; the request stays
    }
    eventually(async || a.client.people().await.unwrap().len() == MAX_REQUESTS).await;
    // Someone who already has a request isn't new: they get in once the
    // hung-up guests' slots are free...
    let mut back = None;
    for _ in 0..50 {
        back = askers[0].hello(a.code, PROTO_VERSION).await;
        if back.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(back.is_some(), "a known requester couldn't reconnect");
    // ...so a new stranger is refused because the requests are full, not the guests.
    let latecomer = Fake::new(&lookup).await;
    assert!(latecomer.hello(a.code, PROTO_VERSION).await.is_none(), "a 21st stranger got in");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_so_many_guests_may_be_connected_at_once() {
    let lookup = MemoryLookup::new();
    let a = Peer::start(&lookup, "abin").await;
    let mut guests = Vec::new();
    for _ in 0..MAX_GUESTS {
        let fake = Fake::new(&lookup).await;
        let streams = fake.hello(a.code, PROTO_VERSION).await.expect("room for this guest");
        guests.push((fake, streams));
    }
    let extra = Fake::new(&lookup).await;
    assert!(extra.hello(a.code, PROTO_VERSION).await.is_none(), "one guest too many got in");
    let (_still_here, streams) = guests.pop().unwrap();
    drop(streams); // one hangs up
    let mut got_in = false;
    for _ in 0..50 {
        if extra.hello(a.code, PROTO_VERSION).await.is_some() {
            got_in = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(got_in, "the freed slot was never reused");
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_a_window_of_messages_is_unacked_at_once() {
    let lookup = MemoryLookup::new();
    let a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    a.client.add_contact(fake.code).await.unwrap();
    for i in 0..WINDOW + 8 {
        a.client.send(fake.code, &format!("message {i}")).await.unwrap();
    }
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    let mut msgs = Vec::new();
    for _ in 0..WINDOW {
        let Frame::Msg { id, .. } = next_frame(&mut recv, |f| matches!(f, Frame::Msg { .. })).await else {
            unreachable!()
        };
        msgs.push(id);
    }
    let more = frames_until_quiet(&mut recv, QUIET).await;
    assert!(!more.iter().any(|f| matches!(f, Frame::Msg { .. })), "more than a window went out before any ack");
    write_frame(&mut send, &Frame::Ack { id: msgs[0].clone() }).await.unwrap();
    let next = next_frame(&mut recv, |f| matches!(f, Frame::Msg { .. })).await;
    assert!(matches!(next, Frame::Msg { text, .. } if text == format!("message {WINDOW}")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_outbox_goes_out_in_full() {
    let lookup = MemoryLookup::new();
    let rahul = Seed::generate();
    let rc = rahul.code();
    let mut a = Peer::start(&lookup, "abin").await;
    a.client.add_contact(rc).await.unwrap();
    for i in 0..100 {
        a.client.send(rc, &format!("note {i}")).await.unwrap();
    }
    let b = Peer::start_with(&lookup, rahul, "rahul").await;
    b.client.add_contact(a.code).await.unwrap();
    for _ in 0..100 {
        a.expect(|e| matches!(e, Event::Delivered { .. })).await;
    }
    eventually(async || b.client.history(a.code).await.unwrap().len() == 100).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_stops_reading_is_cut_off_and_others_carry_on() {
    let lookup = MemoryLookup::new();
    let (mut a, mut b) = friends(&lookup).await;
    let fake = Fake::with_small_window(&lookup, 1024).await;
    a.client.add_contact(fake.code).await.unwrap();
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    let fc = fake.code;
    a.expect(|e| *e == Event::Online(fc)).await;
    // Acks pile up unread on a's side until its queue for the fake is full.
    // The same message over and over: each repeat is acked without a disk
    // write, so this is quick even where saving is slow.
    let frame = Frame::Msg { id: "f".into(), sent_at: 1, text: "flood".into() };
    for _ in 0..5000 {
        if write_frame(&mut send, &frame).await.is_err() {
            break;
        }
    }
    a.expect(|e| *e == Event::Offline(fc)).await;
    assert!(is_cut_off(&mut recv).await);
    a.client.send(b.code, "still working").await.unwrap();
    b.expect(|e| matches!(e, Event::Received { text, .. } if text == "still working")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_hello_closes_the_link_and_nothing_after_it_counts() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    a.client.add_contact(fake.code).await.unwrap();
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    let fc = fake.code;
    a.expect(|e| *e == Event::Online(fc)).await;
    write_frame(&mut send, &Frame::Hello { proto: PROTO_VERSION, app_version: "again".into() }).await.unwrap();
    let _ = write_frame(&mut send, &Frame::Msg { id: "late".into(), sent_at: 1, text: "after".into() }).await;
    assert!(is_cut_off(&mut recv).await);
    a.expect(|e| *e == Event::Offline(fc)).await;
    a.expect_none(QUIET, |e| matches!(e, Event::Received { .. })).await;
    assert!(a.client.history(fc).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turned_down_link_delivers_nothing() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let contact = Fake::new(&lookup).await;
    let stranger = Fake::new(&lookup).await;
    a.client.add_contact(contact.code).await.unwrap();
    let cc = contact.code;
    // The contact is connected properly, then opens a second connection with
    // another version: that one is turned down, and what it carries is ignored
    // even though the contact is online.
    let _good = contact.hello(a.code, PROTO_VERSION).await.unwrap();
    a.expect(|e| *e == Event::Online(cc)).await;
    let sneaky = [Frame::Msg { id: "x".into(), sent_at: 1, text: "sneaky".into() }];
    for fake in [&contact, &stranger] {
        let _ = fake.hello_with(a.code, PROTO_VERSION + 1, &sneaky).await;
    }
    a.expect(|e| *e == Event::VersionMismatch { peer: cc, theirs: PROTO_VERSION + 1 }).await;
    // Strangers with another version are just closed: no notice to spam the screen with.
    a.expect_none(QUIET, |e| {
        matches!(e, Event::Received { .. } | Event::Request { .. } | Event::VersionMismatch { .. })
    })
    .await;
    assert!(a.client.history(cc).await.unwrap().is_empty());
    assert!(a.client.people().await.unwrap().iter().all(|p| p.code == cc));
}
