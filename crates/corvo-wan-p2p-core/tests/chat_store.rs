//! Contacts and messages in the encrypted store.

use std::fs;
use std::path::Path;

use corvo_wan_p2p_core::identity::{Code, Seed};
use corvo_wan_p2p_core::store::{DB_FILE, KdfParams, Mark, Standing, Store, StoreError};

fn store(dir: &Path) -> Store {
    Store::create(dir, "pass", KdfParams::FAST_INSECURE, &Seed::generate(), "abin").unwrap()
}

fn someone() -> Code {
    Seed::generate().code()
}

#[test]
fn contacts_are_added_once_and_kept_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let (rahul, meera) = (someone(), someone());
    assert!(!s.is_contact(&rahul).unwrap());
    assert!(s.add_contact(&rahul).unwrap());
    assert!(!s.add_contact(&rahul).unwrap());
    assert!(s.add_contact(&meera).unwrap());
    let all = s.contacts().unwrap();
    assert_eq!(all.iter().map(|c| c.code).collect::<Vec<_>>(), vec![rahul, meera]);
    assert_eq!(all[0].name, "");
    assert_eq!(all[0].last_seen, None);
    assert!(s.is_contact(&rahul).unwrap());
}

#[test]
fn contact_names_and_last_seen_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let rahul = someone();
    {
        let s = store(dir.path());
        s.add_contact(&rahul).unwrap();
        s.set_contact_name(&rahul, "rahul").unwrap();
        s.set_last_seen(&rahul, 1_760_000_000_123).unwrap();
    }
    let s = Store::open(dir.path(), "pass").unwrap();
    let c = s.person(&rahul).unwrap().unwrap();
    assert_eq!((c.name.as_str(), c.last_seen), ("rahul", Some(1_760_000_000_123)));
    assert_eq!(s.person(&someone()).unwrap(), None);
}

#[test]
fn an_outgoing_message_goes_waiting_delivered_read() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let rahul = someone();
    s.add_contact(&rahul).unwrap();
    s.add_outgoing(&rahul, "m1", 10, "hi").unwrap();
    s.add_outgoing(&rahul, "m2", 20, "there").unwrap();
    assert_eq!(s.waiting(&rahul).unwrap().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["m1", "m2"]);

    assert!(s.mark_delivered(&rahul, "m1").unwrap());
    assert!(!s.mark_delivered(&rahul, "m1").unwrap(), "a second ack changes nothing");
    assert!(!s.mark_delivered(&rahul, "nope").unwrap());
    assert_eq!(s.waiting(&rahul).unwrap().len(), 1);

    assert!(s.mark_read(&rahul, "m2").unwrap(), "read up to m2 covers m1 and m2");
    assert!(!s.mark_read(&rahul, "m2").unwrap());
    assert!(!s.mark_read(&rahul, "nope").unwrap());
    assert!(s.waiting(&rahul).unwrap().is_empty(), "a read message isn't waiting any more");
    let marks: Vec<Mark> = s.history(&rahul).unwrap().iter().map(|m| m.mark).collect();
    assert_eq!(marks, [Mark::Read, Mark::Read]);
}

#[test]
fn a_resent_message_is_saved_once() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let rahul = someone();
    assert!(s.add_incoming(&rahul, "x1", 5, "hello").unwrap());
    assert!(!s.add_incoming(&rahul, "x1", 5, "hello").unwrap());
    let history = s.history(&rahul).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].text.as_str(), history[0].sent_at, history[0].mark), ("hello", 5, Mark::Received));
    assert_eq!(s.last_received(&rahul).unwrap().as_deref(), Some("x1"));
    assert_eq!(s.last_received(&someone()).unwrap(), None);
}

#[test]
fn history_keeps_both_sides_in_order_and_per_person() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let (rahul, meera) = (someone(), someone());
    s.add_outgoing(&rahul, "a", 1, "one").unwrap();
    s.add_incoming(&rahul, "b", 2, "two").unwrap();
    s.add_outgoing(&meera, "a", 3, "same id, other person").unwrap();
    s.add_outgoing(&rahul, "c", 4, "three").unwrap();
    let texts: Vec<String> = s.history(&rahul).unwrap().into_iter().map(|m| m.text).collect();
    assert_eq!(texts, ["one", "two", "three"]);
    assert_eq!(s.history(&meera).unwrap().len(), 1);
    assert_eq!(s.last_received(&rahul).unwrap().as_deref(), Some("b"));
}

#[test]
fn the_latest_read_receipt_is_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let rahul = someone();
    {
        let s = store(dir.path());
        s.add_contact(&rahul).unwrap();
        assert_eq!(s.read_upto(&rahul).unwrap(), None);
        s.note_read(&rahul, "x9").unwrap();
        s.note_read(&rahul, "x10").unwrap();
    }
    let s = Store::open(dir.path(), "pass").unwrap();
    assert_eq!(s.read_upto(&rahul).unwrap().as_deref(), Some("x10"));
    assert_eq!(s.read_upto(&someone()).unwrap(), None);
}

#[test]
fn messages_and_contacts_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let rahul = someone();
    {
        let s = store(dir.path());
        s.add_contact(&rahul).unwrap();
        s.add_outgoing(&rahul, "m1", 10, "still here").unwrap();
    }
    let s = Store::open(dir.path(), "pass").unwrap();
    assert_eq!(s.waiting(&rahul).unwrap()[0].text, "still here");
}

#[test]
fn nothing_about_contacts_or_messages_is_readable_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let rahul = someone();
    {
        let s = store(dir.path());
        s.add_contact(&rahul).unwrap();
        s.set_contact_name(&rahul, "zebra-contact-77").unwrap();
        s.add_outgoing(&rahul, "01JA-ID-SECRET", 10, "zebra-text-42").unwrap();
    }
    let bytes = fs::read(dir.path().join(DB_FILE)).unwrap();
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(!has(b"zebra-contact-77"));
    assert!(!has(b"zebra-text-42"));
    assert!(!has(b"01JA-ID-SECRET"));
    assert!(!has(rahul.key().as_bytes()));
    assert!(!has(rahul.to_string().as_bytes()));
}

#[test]
fn a_value_moved_to_another_row_does_not_open() {
    let dir = tempfile::tempdir().unwrap();
    let rahul = someone();
    {
        let s = store(dir.path());
        s.add_outgoing(&rahul, "a", 1, "for a").unwrap();
        s.add_outgoing(&rahul, "b", 2, "for b").unwrap();
    }
    let db = rusqlite::Connection::open(dir.path().join(DB_FILE)).unwrap();
    db.execute("UPDATE messages SET body = (SELECT body FROM messages WHERE seq = 1) WHERE seq = 2", []).unwrap();
    drop(db);
    let s = Store::open(dir.path(), "pass").unwrap();
    assert!(matches!(s.history(&rahul), Err(StoreError::Damaged { .. })));
}

#[test]
fn a_request_can_be_accepted_and_its_messages_are_counted() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let rahul = someone();
    s.add_incoming(&rahul, "old", 1, "from before you removed them").unwrap();
    assert!(s.add_request(&rahul, "rahul").unwrap());
    assert!(!s.add_request(&rahul, "again").unwrap());
    assert_eq!(s.standing(&rahul).unwrap(), Some(Standing::Request));
    assert!(!s.is_contact(&rahul).unwrap());
    assert_eq!(s.request_messages(&rahul).unwrap(), 0, "older history doesn't count");
    s.add_incoming(&rahul, "new", 2, "hi").unwrap();
    assert_eq!(s.request_messages(&rahul).unwrap(), 1);
    assert!(s.has_message(&rahul, "new").unwrap());
    assert!(!s.has_message(&rahul, "nope").unwrap());
    let p = s.person(&rahul).unwrap().unwrap();
    assert_eq!((p.name.as_str(), p.standing), ("rahul", Standing::Request));

    assert!(s.accept(&rahul).unwrap());
    assert!(!s.accept(&rahul).unwrap(), "already a contact");
    assert!(s.is_contact(&rahul).unwrap());
    assert_eq!(s.request_messages(&rahul).unwrap(), 0);
    assert!(!s.accept(&someone()).unwrap());
}

#[test]
fn blocking_drops_the_outbox_and_keeps_history() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let (rahul, stranger) = (someone(), someone());
    s.add_contact(&rahul).unwrap();
    s.set_contact_name(&rahul, "rahul").unwrap();
    s.add_outgoing(&rahul, "sent", 1, "delivered one").unwrap();
    s.mark_delivered(&rahul, "sent").unwrap();
    s.add_outgoing(&rahul, "queued", 2, "never sent").unwrap();
    s.block(&rahul).unwrap();
    s.block(&rahul).unwrap(); // twice is fine
    s.block(&stranger).unwrap(); // someone you never knew
    assert_eq!(s.standing(&rahul).unwrap(), Some(Standing::Blocked));
    assert_eq!(s.standing(&stranger).unwrap(), Some(Standing::Blocked));
    assert!(s.waiting(&rahul).unwrap().is_empty());
    assert_eq!(s.history(&rahul).unwrap().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["sent"]);
    assert_eq!(s.person(&rahul).unwrap().unwrap().name, "rahul", "the name stays for the blocked list");
    assert!(s.contacts().unwrap().is_empty());
    assert!(!s.remove(&rahul, true).unwrap(), "blocked people stay blocked");

    assert!(s.unblock(&rahul).unwrap());
    assert!(!s.unblock(&rahul).unwrap());
    assert_eq!(s.standing(&rahul).unwrap(), None);
    assert_eq!(s.history(&rahul).unwrap().len(), 1);
}

#[test]
fn removing_forgets_them_and_optionally_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let (rahul, meera) = (someone(), someone());
    for (who, id) in [(&rahul, "r"), (&meera, "m")] {
        s.add_contact(who).unwrap();
        s.add_incoming(who, &format!("{id}-in"), 1, "theirs").unwrap();
        s.add_outgoing(who, &format!("{id}-out"), 2, "waiting").unwrap();
    }
    assert!(s.remove(&rahul, false).unwrap());
    assert!(s.remove(&meera, true).unwrap());
    assert!(!s.remove(&meera, true).unwrap());
    assert_eq!((s.standing(&rahul).unwrap(), s.standing(&meera).unwrap()), (None, None));
    assert_eq!(s.history(&rahul).unwrap().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["r-in"]);
    assert!(s.history(&meera).unwrap().is_empty());
}

#[test]
fn people_lists_everyone_and_contacts_only_contacts() {
    let dir = tempfile::tempdir().unwrap();
    let (friend, asker, pest) = (someone(), someone(), someone());
    {
        let s = store(dir.path());
        s.add_contact(&friend).unwrap();
        s.add_request(&asker, "asker").unwrap();
        s.block(&pest).unwrap();
    }
    let s = Store::open(dir.path(), "pass").unwrap();
    let people: Vec<(Code, Standing)> = s.people().unwrap().iter().map(|p| (p.code, p.standing)).collect();
    assert_eq!(people, [(friend, Standing::Contact), (asker, Standing::Request), (pest, Standing::Blocked)]);
    assert_eq!(s.contacts().unwrap().iter().map(|p| p.code).collect::<Vec<_>>(), [friend]);
}

#[test]
fn the_outbox_can_be_read_a_few_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let rahul = someone();
    for i in 0..5 {
        s.add_outgoing(&rahul, &format!("m{i}"), i, "x").unwrap();
    }
    s.mark_delivered(&rahul, "m0").unwrap();
    let first: Vec<String> = s.oldest_waiting(&rahul, 2).unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(first, ["m1", "m2"]);
}

#[test]
fn a_contact_saved_before_requests_existed_is_still_a_contact() {
    let dir = tempfile::tempdir().unwrap();
    let rahul = someone();
    drop(store(dir.path()));
    {
        let s = Store::open(dir.path(), "pass").unwrap();
        s.add_contact(&rahul).unwrap();
    }
    let db = rusqlite::Connection::open(dir.path().join(DB_FILE)).unwrap();
    db.execute("UPDATE contacts SET standing = NULL", []).unwrap();
    drop(db);
    let s = Store::open(dir.path(), "pass").unwrap();
    assert_eq!(s.standing(&rahul).unwrap(), Some(Standing::Contact));
}

#[test]
fn a_standing_moved_to_another_row_does_not_open() {
    let dir = tempfile::tempdir().unwrap();
    let (rahul, pest) = (someone(), someone());
    {
        let s = store(dir.path());
        s.add_contact(&rahul).unwrap();
        s.block(&pest).unwrap();
    }
    let db = rusqlite::Connection::open(dir.path().join(DB_FILE)).unwrap();
    db.execute("UPDATE contacts SET standing = (SELECT standing FROM contacts WHERE rowid = 2) WHERE rowid = 1", [])
        .unwrap();
    drop(db);
    let s = Store::open(dir.path(), "pass").unwrap();
    assert!(matches!(s.standing(&rahul), Err(StoreError::Damaged { .. })));
}
