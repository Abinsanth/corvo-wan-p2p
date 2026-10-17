//! The code file: your code alone in a file in your home folder, so it's easy
//! to copy and send to a friend. It isn't secret.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::identity::Code;

pub const FILE_NAME: &str = "corvo-wan-p2p-code.txt";

/// Where the code file goes: `<home>/corvo-wan-p2p-code.txt`.
pub fn path(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

pub fn exists(home: &Path) -> bool {
    path(home).exists()
}

/// Writes the code file, holding only `code` and a line break. One that's
/// there already is kept, and gives `ErrorKind::AlreadyExists`.
pub fn write(home: &Path, code: &Code) -> std::io::Result<PathBuf> {
    let path = path(home);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    file.write_all(format!("{code}\n").as_bytes())?;
    file.sync_all()?;
    Ok(path)
}

/// Deletes the code file if it's there.
pub fn remove(home: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path(home)) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Seed;

    #[test]
    fn the_code_file_holds_only_the_code() {
        let home = tempfile::tempdir().unwrap();
        let code = Seed::generate().code();
        assert!(!exists(home.path()));
        let written = write(home.path(), &code).unwrap();
        assert_eq!(written, home.path().join("corvo-wan-p2p-code.txt"));
        assert_eq!(std::fs::read_to_string(&written).unwrap(), format!("{code}\n"));
        assert!(exists(home.path()));
    }

    #[test]
    fn an_existing_code_file_is_not_replaced() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(path(home.path()), "mine").unwrap();
        let err = write(home.path(), &Seed::generate().code()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(path(home.path())).unwrap(), "mine");
    }

    #[test]
    fn removing_a_missing_code_file_is_fine() {
        let home = tempfile::tempdir().unwrap();
        remove(home.path()).unwrap();
        write(home.path(), &Seed::generate().code()).unwrap();
        remove(home.path()).unwrap();
        assert!(!exists(home.path()));
    }
}
