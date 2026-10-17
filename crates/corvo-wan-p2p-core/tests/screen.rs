//! What the screen asks of the chat engine beyond chatting: your passphrase
//! and words, where you stopped reading, whether the internet is reachable,
//! and a limit on how often a request can rename itself.

mod common;

use std::time::Duration;

use common::{Fake, Peer, friends, say};
use corvo_wan_p2p_core::chat::{ChatError, Event, MAX_REQUEST_RENAMES};
use corvo_wan_p2p_core::proto::{Frame, PROTO_VERSION, write_frame};
use corvo_wan_p2p_core::store::{Store, StoreError};
use iroh::address_lookup::MemoryLookup;
use zeroize::Zeroizing;

const QUIET: Duration = Duration::from_millis(500);

#[tokio::test(flavor = "multi_thread")]
async fn the_passphrase_changes_only_when_the_old_one_is_right() {
    let lookup = MemoryLookup::new();
    let a = Peer::start(&lookup, "abin").await;
    assert!(matches!(a.client.change_passphrase("wrong", "new pass").await, Err(ChatError::WrongPassphrase)));
    assert!(matches!(a.client.change_passphrase("pass", "").await, Err(ChatError::Store(StoreError::EmptyPassphrase))));
    let (dir, seed) = a.stop().await;
    drop(Store::open(dir.path(), "pass").unwrap());

    let a = Peer::resume(&lookup, (dir, seed)).await;
    a.client.change_passphrase("pass", "new pass").await.unwrap();
    let (dir, _) = a.stop().await;
    assert!(matches!(Store::open(dir.path(), "pass"), Err(StoreError::WrongPassphrase)));
    drop(Store::open(dir.path(), "new pass").unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_words_are_given_only_for_the_right_passphrase() {
    let lookup = MemoryLookup::new();
    let a = Peer::start(&lookup, "abin").await;
    assert!(matches!(a.client.phrase("wrong").await, Err(ChatError::WrongPassphrase)));
    assert_eq!(a.client.phrase("pass").await.unwrap().code(), a.code);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_passphrase_check_says_whether_it_is_yours() {
    let lookup = MemoryLookup::new();
    let a = Peer::start(&lookup, "abin").await;
    assert!(!a.client.check_passphrase(Zeroizing::new("wrong".into())).await.unwrap());
    assert!(a.client.check_passphrase(Zeroizing::new("pass".into())).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn where_you_stopped_reading_survives_a_restart() {
    let lookup = MemoryLookup::new();
    let (mut a, b) = friends(&lookup).await;
    let bc = b.code;
    assert_eq!(a.client.read_upto(bc).await.unwrap(), None);
    b.client.send(a.code, "one").await.unwrap();
    let Event::Received { id, .. } = a.expect(|e| matches!(e, Event::Received { .. })).await else { unreachable!() };
    assert_eq!(a.client.read_upto(bc).await.unwrap(), None);
    a.client.mark_read(bc).await.unwrap();
    assert_eq!(a.client.read_upto(bc).await.unwrap(), Some(id.clone()));
    let a = a.restart().await;
    assert_eq!(a.client.read_upto(bc).await.unwrap(), Some(id));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_relays_the_network_is_reported_offline_and_chat_still_works() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    a.expect(|e| *e == Event::Network { online: false }).await;
    a.expect_none(QUIET, |e| matches!(e, Event::Network { .. })).await;

    let mut b = Peer::start(&lookup, "rahul").await;
    a.client.add_contact(b.code).await.unwrap();
    b.client.add_contact(a.code).await.unwrap();
    a.client.send(b.code, "local only").await.unwrap();
    b.expect(|e| matches!(e, Event::Received { text, .. } if text == "local only")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_can_only_rename_itself_so_often() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    let (mut send, _recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    write_frame(&mut send, &Frame::Name { name: "x0".into() }).await.unwrap();
    say(&mut send, "m1", "hi").await;
    a.expect(|e| matches!(e, Event::Request { .. })).await;
    for i in 1..=10 {
        write_frame(&mut send, &Frame::Name { name: format!("x{i}") }).await.unwrap();
    }
    for i in 1..=MAX_REQUEST_RENAMES {
        let want = Event::Name { of: fake.code, name: format!("x{i}") };
        a.expect(|e| *e == want).await;
    }
    a.expect_none(QUIET, |e| matches!(e, Event::Name { .. })).await;
    assert_eq!(a.client.people().await.unwrap()[0].name, format!("x{MAX_REQUEST_RENAMES}"));
}

/// Apps from before 0.1.0-rc.1 sent their name after their messages, so a
/// request's first name came as a "change" from no name at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_naming_itself_after_its_first_message_is_not_a_rename() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    let (mut send, _recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    say(&mut send, "m1", "hi").await;
    let fc = fake.code;
    a.expect(|e| *e == Event::Request { from: fc, name: String::new() }).await;
    for i in 0..=MAX_REQUEST_RENAMES {
        write_frame(&mut send, &Frame::Name { name: format!("x{i}") }).await.unwrap();
    }
    for i in 0..=MAX_REQUEST_RENAMES {
        let want = Event::Name { of: fc, name: format!("x{i}") };
        a.expect(|e| *e == want).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_contact_can_rename_as_often_as_they_like() {
    let lookup = MemoryLookup::new();
    let (mut a, b) = friends(&lookup).await;
    let bc = b.code;
    for i in 1..=MAX_REQUEST_RENAMES + 3 {
        b.client.set_name(&format!("rahul {i}")).await.unwrap();
        let want = Event::Name { of: bc, name: format!("rahul {i}") };
        a.expect(|e| *e == want).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_does_not_give_a_request_more_renames() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let fake = Fake::new(&lookup).await;
    let (mut send, _recv, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    write_frame(&mut send, &Frame::Name { name: "x0".into() }).await.unwrap();
    say(&mut send, "m1", "hi").await;
    a.expect(|e| matches!(e, Event::Request { .. })).await;
    for i in 1..=MAX_REQUEST_RENAMES {
        write_frame(&mut send, &Frame::Name { name: format!("x{i}") }).await.unwrap();
        let want = Event::Name { of: fake.code, name: format!("x{i}") };
        a.expect(|e| *e == want).await;
    }
    let (mut again, _recv2, _) = fake.hello(a.code, PROTO_VERSION).await.unwrap();
    write_frame(&mut again, &Frame::Name { name: "renamed again".into() }).await.unwrap();
    a.expect_none(QUIET, |e| matches!(e, Event::Name { .. })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_a_request_is_remembered_without_telling_them() {
    let lookup = MemoryLookup::new();
    let mut a = Peer::start(&lookup, "abin").await;
    let mut b = Peer::start(&lookup, "rahul").await;
    b.client.add_contact(a.code).await.unwrap();
    b.client.send(a.code, "hi").await.unwrap();
    let Event::Received { id, .. } = a.expect(|e| matches!(e, Event::Received { .. })).await else { unreachable!() };
    a.client.mark_read(b.code).await.unwrap();
    assert_eq!(a.client.read_upto(b.code).await.unwrap(), Some(id));
    b.expect_none(QUIET, |e| matches!(e, Event::Read { .. })).await;
}

/// While Argon2 works through `/passphrase` or `/phrase`, the engine still
/// answers everything else: `people()` comes back while that work is still
/// going on, whatever the machine's speed.
#[tokio::test(flavor = "multi_thread")]
async fn passphrase_work_does_not_hold_up_the_engine() {
    let lookup = MemoryLookup::new();
    let a = Peer::start_with_slow_kdf(&lookup, "abin").await;
    let change = a.client.change_passphrase("pass", "new pass");
    assert_answers_meanwhile(&a, change).await.unwrap();
    let phrase = a.client.phrase("new pass");
    assert_answers_meanwhile(&a, phrase).await.unwrap();
}

/// Quitting while `/passphrase` is still saving waits for it: the next start
/// opens with the new passphrase and finds the store free.
#[tokio::test(flavor = "multi_thread")]
async fn quitting_waits_for_a_passphrase_change() {
    let lookup = MemoryLookup::new();
    let a = Peer::start_with_slow_kdf(&lookup, "abin").await;
    {
        let change = a.client.change_passphrase("pass", "new pass");
        // Long enough for the change to reach the engine, not to finish.
        assert!(tokio::time::timeout(Duration::from_millis(20), change).await.is_err(), "finished too fast to tell");
    }
    let (dir, _) = a.stop().await;
    drop(Store::open(dir.path(), "new pass").unwrap());
}

/// Starts `slow`, then checks that `people()` answers while it's still running.
async fn assert_answers_meanwhile<T>(a: &Peer, slow: impl Future<Output = T>) -> T {
    let mut slow = std::pin::pin!(slow);
    // Long enough for the slow command to reach the engine first.
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut slow).await.is_err(), "finished too fast to tell");
    a.client.people().await.unwrap();
    assert!(tokio::time::timeout(Duration::ZERO, &mut slow).await.is_err(), "people() waited for it to finish");
    slow.await
}
