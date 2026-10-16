//! The encrypted store, used the way the app will use it.

use std::fs;
use std::path::Path;

use corvo_wan_p2p_core::identity::{NameError, Seed};
use corvo_wan_p2p_core::store::{DB_FILE, KEY_FILE, KdfParams, Store, StoreError};

/// Real Argon2 settings would make every test take a second.
const FAST: KdfParams = KdfParams::FAST_INSECURE;

fn new_account(dir: &Path) -> (Store, Seed) {
    let seed = Seed::generate();
    let store = Store::create(dir, "first pass", FAST, &seed, "abin").unwrap();
    (store, seed)
}

#[test]
fn a_new_account_reopens_with_its_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let (store, seed) = new_account(dir.path());
    drop(store);
    assert!(Store::exists(dir.path()));
    let profile = Store::open(dir.path(), "first pass").unwrap().profile().unwrap();
    assert_eq!(profile.seed, seed);
    assert_eq!(profile.name, "abin");
}

#[test]
fn a_wrong_passphrase_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    drop(new_account(dir.path()));
    assert!(matches!(Store::open(dir.path(), "first pas"), Err(StoreError::WrongPassphrase)));
    assert!(matches!(Store::open(dir.path(), ""), Err(StoreError::WrongPassphrase)));
}

#[test]
fn opening_where_there_is_no_account_says_so() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!Store::exists(dir.path()));
    assert!(matches!(Store::open(dir.path(), "x"), Err(StoreError::NotFound(_))));
    assert!(matches!(Store::open(&dir.path().join("missing"), "x"), Err(StoreError::NotFound(_))));
}

#[test]
fn creating_twice_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    drop(new_account(dir.path()));
    let again = Store::create(dir.path(), "other", FAST, &Seed::generate(), "x");
    assert!(matches!(again, Err(StoreError::AlreadyExists(_))));
    assert_eq!(Store::open(dir.path(), "first pass").unwrap().profile().unwrap().name, "abin");
}

#[test]
fn empty_passphrases_and_bad_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let seed = Seed::generate();
    assert!(matches!(Store::create(dir.path(), "", FAST, &seed, "abin"), Err(StoreError::EmptyPassphrase)));
    assert!(matches!(Store::create(dir.path(), "p", FAST, &seed, "  "), Err(StoreError::Name(NameError::Empty))));
    assert!(!Store::exists(dir.path()));
    let store = Store::create(dir.path(), "p", FAST, &seed, "abin").unwrap();
    assert!(matches!(store.change_passphrase("", FAST), Err(StoreError::EmptyPassphrase)));
    assert!(matches!(store.set_name(&"x".repeat(33)), Err(StoreError::Name(NameError::TooLong(33)))));
}

#[test]
fn a_second_copy_of_the_app_cannot_open_the_same_account() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = new_account(dir.path());
    assert!(matches!(Store::open(dir.path(), "first pass"), Err(StoreError::AlreadyRunning)));
    assert!(matches!(Store::wipe(dir.path()), Err(StoreError::AlreadyRunning)));
    drop(store);
    Store::open(dir.path(), "first pass").unwrap();
}

#[test]
fn a_lock_let_go_a_moment_later_is_taken() {
    // A process started just before (curl, tar) briefly shares the lock's file
    // until it runs its own program, so the lock can outlive its store by a moment.
    let dir = tempfile::tempdir().unwrap();
    let (first, _) = new_account(dir.path());
    let releasing = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(first);
    });
    assert!(Store::open(dir.path(), "first pass").is_ok());
    releasing.join().unwrap();
}

#[test]
fn a_new_name_is_tidied_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = new_account(dir.path());
    assert_eq!(store.set_name("  rahul   k ").unwrap(), "rahul k");
    drop(store);
    assert_eq!(Store::open(dir.path(), "first pass").unwrap().profile().unwrap().name, "rahul k");
}

#[test]
fn changing_the_passphrase_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let (store, seed) = new_account(dir.path());
    store.change_passphrase("second pass", FAST).unwrap();
    drop(store);
    assert!(matches!(Store::open(dir.path(), "first pass"), Err(StoreError::WrongPassphrase)));
    assert_eq!(Store::open(dir.path(), "second pass").unwrap().profile().unwrap().seed, seed);
}

#[test]
fn wipe_removes_the_account() {
    let dir = tempfile::tempdir().unwrap();
    drop(new_account(dir.path()));
    Store::wipe(dir.path()).unwrap();
    assert!(!Store::exists(dir.path()));
    assert!(!dir.path().join(DB_FILE).exists());
    assert!(matches!(Store::open(dir.path(), "first pass"), Err(StoreError::NotFound(_))));
    Store::wipe(dir.path()).unwrap(); // nothing left: still fine
    Store::wipe(&dir.path().join("never-made")).unwrap();
    new_account(dir.path()); // and a fresh account can be made there
}

#[test]
fn a_create_that_crashed_before_key_json_can_be_redone() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(DB_FILE), b"half-made").unwrap();
    assert!(!Store::exists(dir.path()));
    drop(new_account(dir.path()));
    Store::open(dir.path(), "first pass").unwrap();
}

#[test]
fn a_store_from_a_newer_version_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    drop(new_account(dir.path()));
    rusqlite::Connection::open(dir.path().join(DB_FILE)).unwrap().pragma_update(None, "user_version", 99).unwrap();
    assert!(matches!(Store::open(dir.path(), "first pass"), Err(StoreError::TooNew(_))));
}

#[test]
fn damaged_files_are_reported_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    drop(new_account(dir.path()));
    fs::write(dir.path().join(KEY_FILE), "{ not json").unwrap();
    assert!(matches!(Store::open(dir.path(), "first pass"), Err(StoreError::Damaged { .. })));
}

#[test]
fn another_accounts_store_db_does_not_open_with_this_key_json() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    drop(new_account(a.path()));
    drop(new_account(b.path()));
    fs::copy(b.path().join(DB_FILE), a.path().join(DB_FILE)).unwrap();
    assert!(matches!(Store::open(a.path(), "first pass"), Err(StoreError::Damaged { .. })));
}

#[test]
fn nothing_readable_is_left_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let seed = Seed::generate();
    let name = "zebra-name-4417";
    drop(Store::create(dir.path(), "first pass", FAST, &seed, name).unwrap());
    for file in [DB_FILE, KEY_FILE] {
        let bytes = fs::read(dir.path().join(file)).unwrap();
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(!has(name.as_bytes()), "{file} contains the name");
        assert!(!has(seed.as_bytes()), "{file} contains the seed");
        assert!(!has(seed.phrase().as_bytes()), "{file} contains the words");
        assert!(!has(b"first pass"), "{file} contains the passphrase");
    }
}

#[test]
#[cfg(unix)]
fn the_files_are_readable_only_by_you() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("corvo-wan-p2p");
    drop(new_account(&data));
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&data.join(KEY_FILE)), 0o600);
    assert_eq!(mode(&data.join(DB_FILE)), 0o600);
}

#[test]
fn real_kdf_settings_work() {
    let dir = tempfile::tempdir().unwrap();
    let seed = Seed::generate();
    drop(Store::create(dir.path(), "p", KdfParams::DEFAULT, &seed, "abin").unwrap());
    assert_eq!(Store::open(dir.path(), "p").unwrap().profile().unwrap().seed, seed);
}

#[test]
fn any_language_and_emoji_work_in_a_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let seed = Seed::generate();
    drop(Store::create(dir.path(), "പാസ്‌വേഡ് 🔑 ok", FAST, &seed, "abin").unwrap());
    assert!(matches!(Store::open(dir.path(), "പാസ്‌വേഡ് 🔑 o"), Err(StoreError::WrongPassphrase)));
    assert_eq!(Store::open(dir.path(), "പാസ്‌വേഡ് 🔑 ok").unwrap().profile().unwrap().seed, seed);
}

#[test]
fn the_same_passphrase_typed_differently_still_opens() {
    // "café" with é as one character (NFC) and as e plus a combining accent (NFD):
    // the same text on screen, different bytes, depending on the keyboard or password manager.
    let dir = tempfile::tempdir().unwrap();
    let seed = Seed::generate();
    drop(Store::create(dir.path(), "caf\u{e9}", FAST, &seed, "abin").unwrap());
    assert_eq!(Store::open(dir.path(), "cafe\u{301}").unwrap().profile().unwrap().seed, seed);
}

#[test]
fn the_passphrase_can_be_checked_while_the_store_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = new_account(dir.path());
    assert!(store.check_passphrase("first pass").unwrap());
    assert!(!store.check_passphrase("first pas").unwrap());
    assert!(!store.check_passphrase("").unwrap());
    store.change_passphrase("second pass", FAST).unwrap();
    assert!(store.check_passphrase("second pass").unwrap());
    assert!(!store.check_passphrase("first pass").unwrap());
}

#[test]
fn another_accounts_key_json_does_not_pass_the_check() {
    let (mine, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (store, _) = new_account(mine.path());
    drop(new_account(theirs.path()));
    fs::copy(theirs.path().join(KEY_FILE), mine.path().join(KEY_FILE)).unwrap();
    assert!(!store.check_passphrase("first pass").unwrap());
}

#[test]
fn weak_or_huge_settings_are_refused_when_locking() {
    for params in [
        KdfParams { m_kib: 8, t: 2, p: 1 },
        KdfParams { m_kib: KdfParams::MIN.m_kib, t: 1, p: 1 },
        KdfParams { m_kib: KdfParams::MAX.m_kib + 1, ..KdfParams::DEFAULT },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let made = Store::create(dir.path(), "pass", params, &Seed::generate(), "abin");
        assert!(matches!(made, Err(StoreError::BadKdfParams)), "{params:?}");
        assert!(!Store::exists(dir.path()));
    }
}

#[test]
fn absurd_settings_in_key_json_are_refused_quickly() {
    for params in ["{\"m_kib\":4294967295,\"t\":4294967295,\"p\":1}", "{\"m_kib\":1048577,\"t\":3,\"p\":1}"] {
        let dir = tempfile::tempdir().unwrap();
        drop(new_account(dir.path()));
        let path = dir.path().join(KEY_FILE);
        let mut json: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        json["params"] = serde_json::from_str(params).unwrap();
        fs::write(&path, json.to_string()).unwrap();
        let started = std::time::Instant::now();
        let opened = Store::open(dir.path(), "first pass");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let Err(StoreError::Damaged { why, .. }) = opened else { panic!("expected damaged for {params}") };
        assert!(why.contains("out of range"), "{why}");
    }
}

#[test]
fn a_garbage_store_db_is_reported_as_damaged() {
    let dir = tempfile::tempdir().unwrap();
    drop(new_account(dir.path()));
    fs::write(dir.path().join(DB_FILE), "not a database at all, just some text that is long enough to look like one".repeat(4))
        .unwrap();
    let Err(StoreError::Damaged { file, why }) = Store::open(dir.path(), "first pass") else { panic!("expected damaged") };
    assert!(file.ends_with(DB_FILE));
    assert!(why.contains("isn't a corvo-wan-p2p database"), "{why}");
}
