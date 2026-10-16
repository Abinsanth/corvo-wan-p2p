//! Two or three real peers chatting over iroh on this machine.

mod common;

use std::time::Duration;

use common::{Fake, Peer, eventually, friends, is_cut_off, next_frame, say};
use corvo_wan_p2p_core::chat::{ChatError, Event};
use corvo_wan_p2p_core::identity::Seed;
use corvo_wan_p2p_core::proto::{Frame, MAX_FRAME, MAX_TEXT, PROTO_VERSION, write_frame};
use corvo_wan_p2p_core::store::Mark;
use iroh::address_lookup::MemoryLookup;

#[tokio::test(flavor = "multi_thread")]
async fn a_message_is_delivered_then_read() {
    let lookup = MemoryLookup::new();
    let (mut a, mut b) = friends(&lookup).await;
    let id = a.client.send(b.code, "hi rahul 👋").await.unwrap();

    let got = b.expect(|e| matches!(e, Event::Received { .. })).await;
    let Event::Received { from, id: got_id, text, .. } = got else { unreachable!() };
    assert_eq!((from, got_id.as_str(), text.as_str()), (a.code, id.as_str(), "hi rahul 👋"));
    a.expect(|e| *e == Event::Delivered { to: b.code, id: id.clone() }).await;

    b.client.mark_read(a.code).await.unwrap();
    a.expect(|e| *e == Event::Read { by: b.code, up_to: id.clone() }).await;
    let history = a.client.history(b.code).await.unwrap();
    assert_eq!((history.len(), history[0].mark), (1, Mark::Read));
    assert_eq!(b.client.history(a.code).await.unwrap()[0].mark, Mark::Received);
}

#[tokio::test(flavor = "multi_thread")]
async fn names_arrive_with_the_connection_and_when_changed() {
    let lookup = MemoryLookup::new();
    let (mut a, b) = friends(&lookup).await;
    let bc = b.code;
    a.expect(|e| *e == Event::Name { of: bc, name: "rahul".into() }).await;
    assert_eq!(b.client.set_name("  Rahul   K ").await.unwrap(), "Rahul K");
    a.expect(|e| *e == Event::Name { of: bc, name: "Rahul K".into() }).await;
    eventually(async || a.client.contacts().await.unwrap()[0].name == "Rahul K").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_waits_until_they_come_online() {
    let lookup = MemoryLookup::new();
    let rahul = Seed::generate();
    let rahul_code = rahul.code();
    let mut a = Peer::start(&lookup, "abin").await;
    a.client.add_contact(rahul_code).await.unwrap();
    let id = a.client.send(rahul_code, "are you there?").await.unwrap();
    assert_eq!(a.client.history(rahul_code).await.unwrap()[0].mark, Mark::Waiting);

    let mut b = Peer::start_with(&lookup, rahul, "rahul").await;
    b.client.add_contact(a.code).await.unwrap();
    a.expect(|e| *e == Event::Delivered { to: rahul_code, id: id.clone() }).await;
    let got = b.expect(|e| matches!(e, Event::Received { .. })).await;
    assert!(matches!(got, Event::Received { text, .. } if text == "are you there?"));
}

#[tokio::test(flavor = "multi_thread")]
async fn going_offline_and_coming_back() {
    let lookup = MemoryLookup::new();
    let (mut a, b) = friends(&lookup).await;
    let bc = b.code;
    let stopped = b.stop().await;
    a.expect(|e| *e == Event::Offline(bc)).await;
    assert!(a.client.contacts().await.unwrap()[0].last_seen.is_some());

    let id = a.client.send(bc, "sent while you were away").await.unwrap();
    let mut b = Peer::resume(&lookup, stopped).await;
    a.expect(|e| *e == Event::Online(bc)).await;
    a.expect(|e| *e == Event::Delivered { to: bc, id: id.clone() }).await;
    b.expect(|e| matches!(e, Event::Received { text, .. } if text == "sent while you were away")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unsent_messages_survive_a_restart() {
    let lookup = MemoryLookup::new();
    let rahul = Seed::generate();
    let rahul_code = rahul.code();
    let a = Peer::start(&lookup, "abin").await;
    a.client.add_contact(rahul_code).await.unwrap();
    let id = a.client.send(rahul_code, "kept on disk").await.unwrap();
    let mut a = a.restart().await;

    let mut b = Peer::start_with(&lookup, rahul, "rahul").await;
    b.client.add_contact(a.code).await.unwrap();
    a.expect(|e| *e == Event::Delivered { to: rahul_code, id: id.clone() }).await;
    b.expect(|e| matches!(e, Event::Received { text, .. } if text == "kept on disk")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_receipt_made_offline_goes_out_later() {
    let lookup = MemoryLookup::new();
    let (mut a, mut b) = friends(&lookup).await;
    let (ac, bc) = (a.code, b.code);
    let id = a.client.send(bc, "read me later").await.unwrap();
    b.expect(|e| matches!(e, Event::Received { .. })).await;
    a.expect(|e| matches!(e, Event::Delivered { .. })).await;

    let stopped = a.stop().await;
    b.expect(|e| *e == Event::Offline(ac)).await;
    b.client.mark_read(ac).await.unwrap(); // a is offline: the receipt waits
    let mut a = Peer::resume(&lookup, stopped).await;
    a.expect(|e| *e == Event::Read { by: bc, up_to: id.clone() }).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn both_dialing_at_once_ends_with_one_connection() {
    let lookup = MemoryLookup::new();
    for _ in 0..3 {
        let (mut a, mut b) = friends(&lookup).await;
        let (ac, bc) = (a.code, b.code);
        a.client.send(bc, "from a").await.unwrap();
        b.client.send(ac, "from b").await.unwrap();
        b.expect(|e| matches!(e, Event::Received { text, .. } if text == "from a")).await;
        a.expect(|e| matches!(e, Event::Received { text, .. } if text == "from b")).await;
        // A dropped duplicate connection must not look like going offline.
        a.expect_none(Duration::from_millis(500), |e| *e == Event::Offline(bc)).await;
        b.expect_none(Duration::from_millis(500), |e| *e == Event::Offline(ac)).await;
        assert_eq!(a.client.history(bc).await.unwrap().len(), 2);
        assert_eq!(b.client.history(ac).await.unwrap().len(), 2);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeated_message_is_shown_once_and_acked_twice() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    a.client.add_contact(fake.code).await.unwrap();
    let (mut send, mut recv, theirs) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    assert!(matches!(theirs, Frame::Hello { proto: PROTO_VERSION, .. }));
    let msg = Frame::Msg { id: "dup-1".into(), sent_at: 1, text: "only once".into() };
    write_frame(&mut send, &msg).await.unwrap();
    write_frame(&mut send, &msg).await.unwrap();
    for _ in 0..2 {
        let ack = next_frame(&mut recv, |f| matches!(f, Frame::Ack { .. })).await;
        assert_eq!(ack, Frame::Ack { id: "dup-1".into() });
    }
    a.expect(|e| matches!(e, Event::Received { .. })).await;
    a.expect_none(Duration::from_millis(500), |e| matches!(e, Event::Received { .. })).await;
    assert_eq!(a.client.history(fake.code).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_different_protocol_version_is_reported_and_messages_wait() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    a.client.add_contact(fake.code).await.unwrap();
    a.client.send(fake.code, "waiting for an update").await.unwrap();
    let _connection = fake.hello(a.code, PROTO_VERSION + 1).await;
    let fc = fake.code;
    a.expect(|e| *e == Event::VersionMismatch { peer: fc, theirs: PROTO_VERSION + 1 }).await;
    assert_eq!(a.client.history(fake.code).await.unwrap()[0].mark, Mark::Waiting);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_contact_answering_another_version_is_reported_once() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    let dials = fake.answer_with(fake.hello_frame(PROTO_VERSION + 1));
    a.client.add_contact(fake.code).await.unwrap();
    a.client.send(fake.code, "waits").await.unwrap();
    let fc = fake.code;
    a.expect(|e| *e == Event::VersionMismatch { peer: fc, theirs: PROTO_VERSION + 1 }).await;
    // Dialed again after retry_max (1 s here), but said only once.
    a.expect_none(Duration::from_millis(2500), |e| matches!(e, Event::VersionMismatch { .. })).await;
    assert!(dials.load(std::sync::atomic::Ordering::SeqCst) >= 2, "dialed again");
    assert_eq!(a.client.history(fc).await.unwrap()[0].mark, Mark::Waiting);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_contact_dialing_on_another_version_is_reported_once() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    fake.answer_with(fake.hello_frame(PROTO_VERSION + 1));
    a.client.add_contact(fake.code).await.unwrap();
    let fc = fake.code;
    a.expect(|e| *e == Event::VersionMismatch { peer: fc, theirs: PROTO_VERSION + 1 }).await;
    for _ in 0..2 {
        let _ = fake.hello(a.code, PROTO_VERSION + 1).await;
    }
    a.expect_none(Duration::from_millis(500), |e| matches!(e, Event::VersionMismatch { .. })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_on_another_version_is_told_ours() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let (contact, stranger) = (Fake::new(&lookup).await, Fake::new(&lookup).await);
    a.client.add_contact(contact.code).await.unwrap();
    for fake in [&contact, &stranger] {
        let (_, mut recv, theirs) = fake.hello(a.code, PROTO_VERSION + 1).await.expect("an answer");
        assert!(matches!(theirs, Frame::Hello { proto: PROTO_VERSION, .. }), "{theirs:?}");
        assert!(is_cut_off(&mut recv).await);
    }
    let sc = stranger.code;
    a.expect_none(Duration::from_millis(500), |e| matches!(e, Event::VersionMismatch { peer, .. } if *peer == sc))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_naming_another_version_is_reported() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    fake.refuse_with(5, b"7");
    a.client.add_contact(fake.code).await.unwrap();
    let fc = fake.code;
    a.expect(|e| *e == Event::VersionMismatch { peer: fc, theirs: 7 }).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_peer_replaces_its_stale_link_at_once() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    // The key order decides which of two links both sides keep; this is the
    // case where it used to keep the dead one.
    let seed = loop {
        let seed = Seed::generate();
        if seed.code().key().as_bytes() > a.code.key().as_bytes() {
            break seed;
        }
    };
    let before = Fake::with_seed(&lookup, Seed::from_bytes(*seed.as_bytes())).await;
    before.answer_with(before.hello_frame(PROTO_VERSION));
    a.client.add_contact(before.code).await.unwrap();
    let fc = before.code;
    a.expect(|e| *e == Event::Online(fc)).await;
    // Their app dies without a word and starts again.
    let after = Fake::with_seed(&lookup, seed).await;
    let started = tokio::time::Instant::now();
    let (mut send, mut recv, _) = after.hello(a.code, PROTO_VERSION).await.expect("the new link is taken");
    say(&mut send, "m1", "back again").await;
    assert_eq!(next_frame(&mut recv, |f| matches!(f, Frame::Ack { .. })).await, Frame::Ack { id: "m1".into() });
    assert!(started.elapsed() < Duration::from_secs(3));
    drop(before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_contact_that_answers_wrongly_is_retried_with_back_off() {
    let lookup = MemoryLookup::new();
    let fake = Fake::new(&lookup).await;
    let dials = fake.answer_with(Frame::Ack { id: "not a hello".into() });
    let a = Peer::start(&lookup, "abin").await;
    a.client.add_contact(fake.code).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // Back-off from 200 ms doubling to 1 s allows about 3 tries in 1.5 s.
    let n = dials.load(std::sync::atomic::Ordering::SeqCst);
    assert!((1..=6).contains(&n), "{n} dials in 1.5 s");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_sending_an_oversized_frame_is_cut_off_and_others_carry_on() {
    let lookup = MemoryLookup::new();
    let (mut a, mut b) = friends(&lookup).await;
    let fake = Fake::new(&lookup).await;
    a.client.add_contact(fake.code).await.unwrap();
    let (mut send, mut recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    let fc = fake.code;
    a.expect(|e| *e == Event::Online(fc)).await;
    send.write_all(&((MAX_FRAME as u32) + 1).to_be_bytes()).await.unwrap();
    a.expect(|e| *e == Event::Offline(fc)).await;
    assert!(is_cut_off(&mut recv).await, "the connection was closed");

    a.client.send(b.code, "still working").await.unwrap();
    b.expect(|e| matches!(e, Event::Received { text, .. } if text == "still working")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_are_refused() {
    let lookup = MemoryLookup::new();
    let a = Peer::start(&lookup, "abin").await;
    let stranger = Seed::generate().code();
    assert!(matches!(a.client.add_contact(a.code).await, Err(ChatError::Yourself)));
    assert!(matches!(a.client.send(stranger, "hi").await, Err(ChatError::NotAContact)));
    a.client.add_contact(stranger).await.unwrap();
    assert!(matches!(a.client.send(stranger, "  \n ").await, Err(ChatError::Empty)));
    assert!(matches!(a.client.send(stranger, &"a".repeat(MAX_TEXT + 1)).await, Err(ChatError::TooLong)));
    // Within 16 KB of text, but control characters escape to six bytes each in JSON.
    assert!(matches!(a.client.send(stranger, &"\u{1}".repeat(MAX_TEXT)).await, Err(ChatError::TooLong)));
    a.client.send(stranger, &"a".repeat(MAX_TEXT)).await.unwrap();
    assert!(a.client.set_name("").await.is_err());
}
