//! The recovery file: a copy of the 12 words in your home folder, for you to
//! move somewhere safe and then delete. The app reminds you while it exists.

use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::files::write_private_atomic_with;
use crate::identity::Seed;

pub const FILE_NAME: &str = "corvo-wan-p2p-recovery.txt";

/// Where the recovery file goes: `<home>/corvo-wan-p2p-recovery.txt`.
pub fn path(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

/// Whether the recovery file is still there (the app shows a reminder while it is).
pub fn exists(home: &Path) -> bool {
    path(home).exists()
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
    #[error("couldn't make {0} private, so it was removed: {1}")]
    NotPrivate(PathBuf, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// What the file says. `made` is the date shown, for example `2026-10-15`.
pub fn contents(seed: &Seed, made: &str) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "corvo-wan-p2p recovery words");
    let _ = writeln!(text, "Made: {made}");
    let _ = writeln!(text);
    let _ = writeln!(text, "These 12 words are your identity. Anyone who has them can pretend to be you.");
    let _ = writeln!(text, "Copy them somewhere safe (a password manager, a USB stick, or paper),");
    let _ = writeln!(text, "then delete this file.");
    let _ = writeln!(text);
    for (i, word) in seed.words().iter().enumerate() {
        let _ = writeln!(text, "{:>2}. {word}", i + 1);
    }
    let _ = writeln!(text);
    let _ = writeln!(text, "Your code: {}", seed.code());
    let _ = writeln!(text);
    let _ = writeln!(text, "On a new computer, start corvo-wan-p2p, choose (r)ecover and type these words.");
    let _ = writeln!(text, "You get the same code back. Chat history stays on the old computer.");
    text
}

/// Writes the recovery file, readable only by you. An existing file is
/// replaced only if `overwrite` is true (the app asks first).
pub fn write(home: &Path, seed: &Seed, made: &str, overwrite: bool) -> Result<PathBuf, RecoveryError> {
    write_with(home, seed, made, overwrite, restrict_to_owner)
}

/// [`write`], with `restrict` making the still-empty temp file private before
/// the words go in. If it can't, nothing is left behind and an existing file
/// stays as it was.
fn write_with(
    home: &Path,
    seed: &Seed,
    made: &str,
    overwrite: bool,
    restrict: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<PathBuf, RecoveryError> {
    let path = path(home);
    if !overwrite && path.exists() {
        return Err(RecoveryError::AlreadyExists(path));
    }
    let mut refused = None;
    let result = write_private_atomic_with(&path, contents(seed, made).as_bytes(), |tmp| {
        restrict(tmp).map_err(|why| {
            refused = Some(why);
            std::io::Error::other("not private")
        })
    });
    match (result, refused) {
        (Ok(()), _) => Ok(path),
        (Err(_), Some(why)) => Err(RecoveryError::NotPrivate(path, why)),
        (Err(e), None) => Err(e.into()),
    }
}

/// Deletes the recovery file if it's there.
pub fn remove(home: &Path) -> Result<(), RecoveryError> {
    match std::fs::remove_file(path(home)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Unix: the file was created with mode 0600 already.
#[cfg(not(windows))]
fn restrict_to_owner(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Windows: drop inherited permissions and grant only the current user, by
/// SID (a name may not resolve on work laptops), then remove SYSTEM
/// (`S-1-5-18`) and Administrators (`S-1-5-32-544`), which a new file can also
/// get as explicit entries. Then read the permissions back to be sure.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) -> Result<(), String> {
    let sid = current_sid()?;
    icacls(path, &["/inheritance:r".into(), "/grant:r".into(), format!("*{sid}:F")])?;
    icacls(path, &["/remove:g".into(), "*S-1-5-18".into(), "*S-1-5-32-544".into()])?;
    let acl = saved_acl(path)?;
    if only_for(&acl, &sid) {
        Ok(())
    } else {
        let entries: Vec<String> = acl.iter().map(|a| format!("{}:{}:{}", a.kind, a.rights, a.who)).collect();
        Err(format!("it should be open only to {sid}, but its permissions are {}", entries.join(", ")))
    }
}

/// A tool from `System32`, by full path, so nothing else on `PATH` runs instead.
#[cfg(windows)]
fn system_tool(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).filter(|p| p.is_absolute());
    root.unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("System32").join(name)
}

#[cfg(windows)]
fn run_tool(name: &str, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>, String> {
    let out = std::process::Command::new(system_tool(name))
        .args(args)
        .output()
        .map_err(|e| format!("couldn't run {name}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        // These tools report some failures on stdout and some on stderr.
        let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        Err(format!("{name} failed: {}", said.trim()))
    }
}

/// The current user's SID, from `whoami`.
#[cfg(windows)]
fn current_sid() -> Result<String, String> {
    let out = run_tool("whoami.exe", &["/user".as_ref(), "/fo".as_ref(), "csv".as_ref(), "/nh".as_ref()])?;
    parse_whoami(&String::from_utf8_lossy(&out)).ok_or_else(|| "whoami didn't say who you are".into())
}

#[cfg(windows)]
fn icacls(path: &Path, args: &[String]) -> Result<(), String> {
    let mut all: Vec<&std::ffi::OsStr> = vec![path.as_os_str()];
    all.extend(args.iter().map(std::ffi::OsStr::new));
    run_tool("icacls.exe", &all).map(drop)
}

/// The permissions on `path`, read from `icacls /save`, which writes them as
/// SDDL with SIDs (`icacls` itself prints names, which may not resolve).
#[cfg(windows)]
fn saved_acl(path: &Path) -> Result<Vec<Ace>, String> {
    let saved = std::env::temp_dir().join(format!("corvo-wan-p2p-acl-{:016x}.txt", rand::random::<u64>()));
    let result = icacls(path, &["/save".into(), saved.display().to_string()]).and_then(|()| {
        let bytes = std::fs::read(&saved).map_err(|e| format!("couldn't read the saved permissions: {e}"))?;
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        let text = String::from_utf16_lossy(&units);
        parse_saved_acl(text.trim_start_matches('\u{feff}')).ok_or_else(|| "couldn't read the saved permissions".into())
    });
    let _ = std::fs::remove_file(&saved);
    result
}

/// The SID in `whoami /user /fo csv /nh` output: `"domain\user","S-1-5-…"`.
#[cfg(any(windows, test))]
fn parse_whoami(csv: &str) -> Option<String> {
    let sid = csv.trim().rsplit(',').next()?.trim().trim_matches('"');
    let digits = sid.strip_prefix("S-1-")?;
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '-')).then(|| sid.to_string())
}

/// One entry of a file's permissions, as SDDL writes it: `kind` is `A` for
/// allow or `D` for deny, `rights` is like `FA` (full access), and `who` is a
/// SID or a short alias for a well-known one (`SY`, `BA`, `LA`…).
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct Ace {
    kind: String,
    rights: String,
    who: String,
}

/// The entries in an `icacls /save` file: a line with the file name, then
/// the SDDL, like `D:PAI(A;;FA;;;S-1-5-…)(A;;FA;;;SY)`. `None` if there's no
/// DACL line.
#[cfg(any(windows, test))]
fn parse_saved_acl(saved: &str) -> Option<Vec<Ace>> {
    let dacl = saved.lines().skip(1).map(str::trim).find(|l| l.starts_with("D:"))?;
    let aces = dacl.split('(').skip(1).map(|ace| ace.trim_end_matches(')').split(';').collect::<Vec<_>>());
    Some(
        aces.filter(|fields| fields.len() >= 6)
            .map(|f| Ace { kind: f[0].into(), rights: f[2].into(), who: f[5].into() })
            .collect(),
    )
}

/// Whether `acl` is exactly one entry: full access for `sid`. SDDL writes a
/// machine's built-in Administrator (`…-500`) and Guest (`…-501`) as `LA` and
/// `LG`, so those count as the same.
#[cfg(any(windows, test))]
fn only_for(acl: &[Ace], sid: &str) -> bool {
    let is_sid = |who: &str| {
        who == sid
            || (sid.starts_with("S-1-5-21-")
                && ((who == "LA" && sid.ends_with("-500")) || (who == "LG" && sid.ends_with("-501"))))
    };
    matches!(acl, [only] if only.kind == "A" && only.rights == "FA" && is_sid(&only.who))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector() -> Seed {
        Seed::from_bytes([0; 16])
    }

    #[test]
    fn the_file_lists_the_words_the_code_and_the_warning() {
        let text = contents(&vector(), "2026-10-15");
        assert!(text.contains("Made: 2026-10-15"));
        assert!(text.contains(" 1. abandon\n"));
        assert!(text.contains("12. about\n"));
        assert!(text.contains(&format!("Your code: {}", vector().code())));
        assert!(text.contains("Anyone who has them can pretend to be you"));
        assert!(text.contains("then delete this file"));
    }

    #[test]
    fn the_words_in_the_file_recover_the_same_seed() {
        let seed = Seed::generate();
        let text = contents(&seed, "2026-10-15");
        let words: Vec<&str> = text
            .lines()
            .filter_map(|l| l.trim_start().split_once(". "))
            .filter(|(n, _)| n.parse::<u8>().is_ok())
            .map(|(_, w)| w)
            .collect();
        assert_eq!(Seed::from_phrase(&words.join(" ")).unwrap(), seed);
    }

    #[test]
    fn writing_needs_permission_to_replace_and_removing_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        assert!(!exists(home.path()));
        let written = write(home.path(), &vector(), "2026-10-15", false).unwrap();
        assert_eq!(written, home.path().join(FILE_NAME));
        assert!(exists(home.path()));
        assert!(matches!(
            write(home.path(), &Seed::generate(), "2026-10-16", false),
            Err(RecoveryError::AlreadyExists(_))
        ));
        assert!(std::fs::read_to_string(&written).unwrap().contains("Made: 2026-10-15"));
        write(home.path(), &vector(), "2026-10-16", true).unwrap();
        assert!(std::fs::read_to_string(&written).unwrap().contains("Made: 2026-10-16"));
        remove(home.path()).unwrap();
        assert!(!exists(home.path()));
        remove(home.path()).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn the_file_is_readable_only_by_you() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let path = write(home.path(), &vector(), "2026-10-15", false).unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    #[cfg(windows)]
    fn the_file_is_readable_only_by_you() {
        let home = tempfile::tempdir().unwrap();
        let path = write(home.path(), &vector(), "2026-10-15", false).unwrap();
        let acl = saved_acl(&path).unwrap();
        assert!(only_for(&acl, &current_sid().unwrap()), "{acl:?}");
    }

    #[test]
    fn a_file_that_cannot_be_made_private_is_not_left_behind() {
        let home = tempfile::tempdir().unwrap();
        let refuse = |_: &Path| Err("no SID".to_string());
        let err = write_with(home.path(), &vector(), "2026-10-15", false, refuse).unwrap_err();
        assert!(
            matches!(&err, RecoveryError::NotPrivate(p, why) if *p == path(home.path()) && why == "no SID"),
            "{err}"
        );
        assert!(!exists(home.path()));
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0, "a temp file was left behind");
    }

    #[test]
    fn a_failed_replacement_keeps_the_old_file() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), &vector(), "2026-10-15", false).unwrap();
        let refuse = |_: &Path| Err("no SID".to_string());
        assert!(write_with(home.path(), &Seed::generate(), "2026-10-16", true, refuse).is_err());
        assert!(std::fs::read_to_string(path(home.path())).unwrap().contains("Made: 2026-10-15"));
    }

    #[test]
    fn the_temp_file_is_made_private_before_the_words_go_in() {
        let home = tempfile::tempdir().unwrap();
        let check = |tmp: &Path| {
            assert_eq!(std::fs::metadata(tmp).unwrap().len(), 0, "the words were written before restricting");
            assert_ne!(tmp, path(home.path()));
            #[cfg(windows)]
            {
                restrict_to_owner(tmp)?;
                assert!(only_for(&saved_acl(tmp).unwrap(), &current_sid().unwrap()));
            }
            Ok(())
        };
        write_with(home.path(), &vector(), "2026-10-15", false, check).unwrap();
        assert!(std::fs::read_to_string(path(home.path())).unwrap().contains(" 1. abandon\n"));
    }

    #[test]
    fn the_sid_is_read_from_whoami() {
        let sid = "S-1-5-21-3623811015-3361044348-30300820-1013";
        assert_eq!(parse_whoami(&format!("\"desktop-x\\abin\",\"{sid}\"\r\n")), Some(sid.into()));
        assert_eq!(parse_whoami(&format!("\"CORP\\a, b\",\"{sid}\"")), Some(sid.into()));
        assert_eq!(parse_whoami(""), None);
        assert_eq!(parse_whoami("\"x\",\"not a sid\""), None);
        assert_eq!(parse_whoami("\"x\",\"S-1-5-21-12a\""), None);
    }

    #[test]
    fn only_one_full_access_entry_for_you_counts_as_private() {
        let me = "S-1-5-21-1-2-3-1001";
        let acl = |dacl: &str| parse_saved_acl(&format!("x.txt\r\n{dacl}\r\n")).unwrap();
        assert!(only_for(&acl(&format!("D:PAI(A;;FA;;;{me})")), me));
        assert!(!only_for(&acl(&format!("D:PAI(A;;FA;;;{me})(A;;FA;;;SY)")), me));
        assert!(!only_for(&acl(&format!("D:PAI(A;;FR;;;{me})")), me), "read only isn't full access");
        assert!(!only_for(&acl(&format!("D:PAI(A;;FA;;;{me})(D;;FA;;;WD)")), me), "exactly one entry");
        assert!(!only_for(&acl(&format!("D:PAI(XA;;FA;;;{me})")), me));
        assert!(!only_for(&acl("D:P"), me));
    }

    #[test]
    fn the_built_in_administrator_and_guest_are_written_as_aliases() {
        let admin = "S-1-5-21-1-2-3-500";
        let acl = parse_saved_acl("x.txt\r\nD:PAI(A;;FA;;;LA)\r\n").unwrap();
        assert!(only_for(&acl, admin));
        assert!(!only_for(&acl, "S-1-5-21-1-2-3-1500"));
        let acl = parse_saved_acl("x.txt\r\nD:PAI(A;;FA;;;LG)\r\n").unwrap();
        assert!(only_for(&acl, "S-1-5-21-1-2-3-501"));
    }

    #[test]
    fn allowed_sids_are_read_from_saved_acls() {
        let me = "S-1-5-21-1-2-3-1001";
        let saved = format!("corvo-wan-p2p-recovery.txt\r\nD:PAI(A;;FA;;;{me})(D;;FA;;;WD)\r\n");
        let ace =
            |kind: &str, rights: &str, who: &str| Ace { kind: kind.into(), rights: rights.into(), who: who.into() };
        assert_eq!(parse_saved_acl(&saved), Some(vec![ace("A", "FA", me), ace("D", "FA", "WD")]));
        let shared = format!("x.txt\r\nD:AI(A;;FA;;;{me})(A;ID;FA;;;SY)(A;ID;0x1200a9;;;BU)\r\n");
        let want = vec![ace("A", "FA", me), ace("A", "FA", "SY"), ace("A", "0x1200a9", "BU")];
        assert_eq!(parse_saved_acl(&shared), Some(want));
        assert_eq!(parse_saved_acl("D:\\folder\\x.txt\r\nD:P\r\n"), Some(vec![]));
        assert_eq!(parse_saved_acl("x.txt\r\n"), None);
    }
}
