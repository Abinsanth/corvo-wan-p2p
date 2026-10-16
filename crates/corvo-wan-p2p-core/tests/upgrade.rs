//! A store saved by `0.1.0-rc.2` (in `tests/fixtures/rc2`) opens with the
//! current code, upgrades, and keeps everything.

use std::fs;
use std::path::Path;

use corvo_wan_p2p_core::identity::{Code, Seed};
use corvo_wan_p2p_core::store::{DB_FILE, KEY_FILE, KdfParams, Mark, Standing, Store};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rc2");
const PASSPHRASE: &str = "rc2 pass";
const LAST_SEEN: i64 = 1_760_000_000_000;

fn code(n: u8) -> Code {
    Seed::from_bytes([n; 16]).code()
}

/// Who's in the fixture.
fn me() -> Seed {
    Seed::from_bytes([1; 16])
}
fn rahul() -> Code {
    code(2)
}
fn meera() -> Code {
    code(3)
}
fn stranger() -> Code {
    code(4)
}
fn pest() -> Code {
    code(5)
}

/// Writes the fixture. Run once, with the code of `0.1.0-rc.2`:
/// `cargo test -p corvo-wan-p2p-core --test upgrade -- --ignored`.
#[test]
#[ignore]
fn write_rc2_fixture() {
    let dir = Path::new(FIXTURE);
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
    let s = Store::create(dir, PASSPHRASE, KdfParams::FAST_INSECURE, &me(), "abin").unwrap();
    s.add_contact(&rahul()).unwrap();
    s.set_contact_name(&rahul(), "rahul").unwrap();
    s.set_last_seen(&rahul(), LAST_SEEN).unwrap();
    s.add_contact(&meera()).unwrap();
    s.set_contact_name(&meera(), "meera").unwrap();
    s.add_incoming(&meera(), "m1", 1, "from before requests").unwrap();
    s.add_outgoing(&rahul(), "r1", 2, "read by rahul").unwrap();
    s.mark_delivered(&rahul(), "r1").unwrap();
    s.mark_read(&rahul(), "r1").unwrap();
    s.add_outgoing(&rahul(), "r2", 3, "delivered").unwrap();
    s.mark_delivered(&rahul(), "r2").unwrap();
    s.add_incoming(&rahul(), "i1", 4, "hi").unwrap();
    s.add_incoming(&rahul(), "i2", 5, "unread").unwrap();
    s.note_read(&rahul(), "i1").unwrap();
    s.add_outgoing(&rahul(), "r3", 6, "still waiting").unwrap();
    s.add_request(&stranger(), "stranger").unwrap();
    s.add_incoming(&stranger(), "s1", 7, "hello?").unwrap();
    s.block(&pest()).unwrap();
    drop(s);
    // meera's row as an app from before requests left it: no standing.
    let db = rusqlite::Connection::open(dir.join(DB_FILE)).unwrap();
    let meera_tag: Vec<u8> = {
        let s = Store::open(dir, PASSPHRASE).unwrap();
        let first = s.people().unwrap().iter().position(|p| p.code == meera()).unwrap();
        db.query_row("SELECT peer FROM contacts ORDER BY rowid LIMIT 1 OFFSET ?1", [first as i64], |r| r.get(0)).unwrap()
    };
    db.execute("UPDATE contacts SET standing = NULL WHERE peer = ?1", [meera_tag]).unwrap();
    fs::remove_file(dir.join("lock")).unwrap();
}

fn texts(s: &Store, who: &Code) -> Vec<(String, Mark)> {
    s.history(who).unwrap().into_iter().map(|m| (m.text, m.mark)).collect()
}

#[test]
fn an_rc2_store_upgrades_with_nothing_lost() {
    let dir = tempfile::tempdir().unwrap();
    for file in [KEY_FILE, DB_FILE] {
        fs::copy(Path::new(FIXTURE).join(file), dir.path().join(file)).unwrap();
    }
    for _ in 0..2 {
        let s = Store::open(dir.path(), PASSPHRASE).unwrap();
        let profile = s.profile().unwrap();
        assert_eq!((profile.seed, profile.name.as_str()), (me(), "abin"));
        let people: Vec<(Code, String, Standing)> =
            s.people().unwrap().into_iter().map(|p| (p.code, p.name, p.standing)).collect();
        assert_eq!(
            people,
            [
                (rahul(), "rahul".into(), Standing::Contact),
                (meera(), "meera".into(), Standing::Contact),
                (stranger(), "stranger".into(), Standing::Request),
                (pest(), String::new(), Standing::Blocked),
            ]
        );
        assert_eq!(s.person(&rahul()).unwrap().unwrap().last_seen, Some(LAST_SEEN));
        assert_eq!(
            texts(&s, &rahul()),
            [
                ("read by rahul".into(), Mark::Read),
                ("delivered".into(), Mark::Delivered),
                ("hi".into(), Mark::Received),
                ("unread".into(), Mark::Received),
                ("still waiting".into(), Mark::Waiting),
            ]
        );
        assert_eq!(s.read_upto(&rahul()).unwrap().as_deref(), Some("i1"));
        assert_eq!(s.waiting(&rahul()).unwrap().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["r3"]);
        assert_eq!(texts(&s, &meera()), [("from before requests".into(), Mark::Received)]);
        assert_eq!(texts(&s, &stranger()), [("hello?".into(), Mark::Received)]);
        assert_eq!(s.request_messages(&stranger()).unwrap(), 1);
        drop(s);
        let db = rusqlite::Connection::open(dir.path().join(DB_FILE)).unwrap();
        let missing: i64 = db.query_row("SELECT COUNT(*) FROM contacts WHERE standing IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(missing, 0, "every standing is sealed now");
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(version, 5);
    }
}
