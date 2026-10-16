//! Where corvo-wan-p2p keeps its files, and how it writes the private ones.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tracing::warn;

const APP_DIR: &str = "corvo-wan-p2p";

/// The data folder: `$XDG_CONFIG_HOME/corvo-wan-p2p`, else
/// `~/.config/corvo-wan-p2p` (Linux and macOS), or `%APPDATA%\corvo-wan-p2p`
/// on Windows. `None` if the environment doesn't say where home is.
pub fn data_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var(name).ok();
    if cfg!(windows) {
        windows_data_dir(var("APPDATA").as_deref())
    } else {
        unix_data_dir(var("XDG_CONFIG_HOME").as_deref(), var("HOME").as_deref())
    }
}

/// Empty or relative values are ignored, as the XDG spec says.
pub fn unix_data_dir(xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = absolute(xdg).or_else(|| absolute(home).map(|h| h.join(".config")))?;
    Some(base.join(APP_DIR))
}

/// Empty or relative values are ignored.
pub fn windows_data_dir(appdata: Option<&str>) -> Option<PathBuf> {
    absolute(appdata).map(|p| p.join(APP_DIR))
}

/// The user's home folder (`HOME`, or `USERPROFILE` on Windows), where the
/// recovery file goes.
pub fn home_dir() -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    absolute(std::env::var(name).ok().as_deref())
}

fn absolute(value: Option<&str>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// Creates `dir` readable only by us: mode 0700 on Unix. On Windows the data
/// folder sits in the user's profile, which is already private.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Options for writing a file only we can read: mode 0600 on Unix (the mode
/// only applies when the file is created).
pub(crate) fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
}

/// Writes `bytes` beside `path` and renames, so a crash never leaves a
/// half-written file. The result is private (see [`private_file`]).
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_private_atomic_with(path, bytes, |_| Ok(()))
}

/// Like [`write_private_atomic`], but runs `prepare` on the temp file while
/// it's still empty, before `bytes` go in. If `prepare` fails, nothing is
/// written and the temp file is removed.
pub(crate) fn write_private_atomic_with(
    path: &Path,
    bytes: &[u8],
    prepare: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A leftover temp file would keep its old mode, so always start fresh.
    remove_if_present(&tmp)?;
    // Written through the handle that created it, so the bytes go into the
    // very file `prepare` saw, whatever happens to the path meanwhile.
    let mut file = private_file().create_new(true).open(&tmp)?;
    let written = prepare(&tmp).and_then(|()| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, path)?;
    // The file is in place. A folder that won't sync only makes the rename
    // less sure to survive a power cut, so it's noted, not reported.
    if let Err(e) = sync_parent(path) {
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        warn!(%file, error = %e, "couldn't sync the folder after saving");
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// The folders [`sync_parent`] synced, so tests can see it ran.
    static SYNCED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Makes [`sync_parent`] fail, as a full or failing disk might.
    static FAIL_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes a rename in `path`'s folder survive a power cut. Unix only: there
/// the rename lives in the folder, which needs its own sync; NTFS journals it.
fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        #[cfg(test)]
        if FAIL_SYNC.with(|f| f.get()) {
            return Err(std::io::Error::other("sync failed"));
        }
        fs::File::open(parent)?.sync_all()?;
        #[cfg(test)]
        SYNCED.with(|s| s.borrow_mut().push(parent.to_path_buf()));
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unix paths: "/x" isn't absolute on Windows, which uses windows_data_dir instead.
    #[test]
    #[cfg(unix)]
    fn unix_data_dir_follows_xdg_rules() {
        assert_eq!(unix_data_dir(Some("/x"), Some("/h")), Some(PathBuf::from("/x/corvo-wan-p2p")));
        assert_eq!(unix_data_dir(None, Some("/h")), Some(PathBuf::from("/h/.config/corvo-wan-p2p")));
        assert_eq!(unix_data_dir(Some(""), Some("/h")), Some(PathBuf::from("/h/.config/corvo-wan-p2p")));
        assert_eq!(unix_data_dir(Some("rel"), Some("/h")), Some(PathBuf::from("/h/.config/corvo-wan-p2p")));
        assert_eq!(unix_data_dir(None, None), None);
        assert_eq!(unix_data_dir(None, Some("")), None);
    }

    #[test]
    #[cfg(windows)]
    fn windows_data_dir_uses_appdata() {
        assert_eq!(
            windows_data_dir(Some(r"C:\Users\a\AppData\Roaming")),
            Some(PathBuf::from(r"C:\Users\a\AppData\Roaming\corvo-wan-p2p"))
        );
        assert_eq!(windows_data_dir(Some("")), None);
        assert_eq!(windows_data_dir(None), None);
    }

    #[test]
    fn atomic_write_replaces_the_file_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.json");
        fs::write(dir.path().join("key.json.tmp"), "left by a crash").unwrap();
        write_private_atomic(&path, b"one").unwrap();
        write_private_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!dir.path().join("key.json.tmp").exists());
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_syncs_the_folder_after_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        SYNCED.with(|s| s.borrow_mut().clear());
        write_private_atomic(&dir.path().join("key.json"), b"x").unwrap();
        assert_eq!(SYNCED.with(|s| s.borrow().clone()), [dir.path().to_path_buf()]);
    }

    #[test]
    #[cfg(unix)]
    fn a_failed_folder_sync_after_the_rename_still_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.json");
        FAIL_SYNC.with(|f| f.set(true));
        let written = write_private_atomic(&path, b"new");
        FAIL_SYNC.with(|f| f.set(false));
        written.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn sync_parent_handles_a_bare_file_name() {
        sync_parent(Path::new("key.json")).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn private_files_and_dirs_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("data");
        private_dir(&sub).unwrap();
        assert_eq!(fs::metadata(&sub).unwrap().permissions().mode() & 0o777, 0o700);
        let path = sub.join("key.json");
        write_private_atomic(&path, b"x").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
