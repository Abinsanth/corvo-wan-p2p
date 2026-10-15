//! The recovery file: a copy of the 12 words in your home folder, for you to
//! move somewhere safe and then delete. The app reminds you while it exists.

use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::files::write_private_atomic;
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
    let path = path(home);
    if !overwrite && path.exists() {
        return Err(RecoveryError::AlreadyExists(path));
    }
    write_private_atomic(&path, contents(seed, made).as_bytes())?;
    if let Err(why) = restrict_to_owner(&path) {
        let _ = std::fs::remove_file(&path);
        return Err(RecoveryError::NotPrivate(path, why));
    }
    Ok(path)
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

/// Windows: drop inherited permissions and grant only the current user, using
/// the built-in `icacls`.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) -> Result<(), String> {
    let user = std::env::var("USERNAME").map_err(|_| "USERNAME isn't set".to_string())?;
    let out = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:F"))
        .output()
        .map_err(|e| format!("couldn't run icacls: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
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
        let out = std::process::Command::new("icacls").arg(&path).output().unwrap();
        let acl = String::from_utf8_lossy(&out.stdout);
        let user = std::env::var("USERNAME").unwrap();
        assert!(acl.contains(&format!("{user}:(F)")), "{acl}");
        for other in ["Everyone", "BUILTIN\\Users", "Administrators", "SYSTEM"] {
            assert!(!acl.contains(other), "{other} can still read it: {acl}");
        }
    }
}
