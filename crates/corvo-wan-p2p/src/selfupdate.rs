//! `/update`: finds a newer GitHub release and puts it in place of the running
//! binary. Talks to GitHub through the system `curl` and unpacks with the
//! system `tar`, as install.sh does (Windows 10+ ships both), so the app needs
//! no HTTP or TLS crates.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const REPO: &str = "Abinsanth/corvo-wan-p2p";
const INSTALL_URL: &str = "https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.sh";
const INSTALL_PS1_URL: &str = "https://raw.githubusercontent.com/Abinsanth/corvo-wan-p2p/release/install.ps1";
/// The binary inside a release archive.
#[cfg(windows)]
const BIN_NAME: &str = "corvo-wan-p2p.exe";
#[cfg(not(windows))]
const BIN_NAME: &str = "corvo-wan-p2p";
/// Names the temp folder an update works in, beside the binary it replaces.
const TMP_PREFIX: &str = ".corvo-wan-p2p-update-";
/// Where Windows moves the running exe, which can be renamed but not replaced:
/// `corvo-wan-p2p.old-<n>.exe`.
const OLD_EXE_PREFIX: &str = "corvo-wan-p2p.old";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The downloadable files.
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    /// Bytes.
    pub size: u64,
}

/// The fields used from `GET /repos/{REPO}/releases`.
#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// The version of this binary.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is a semver version")
}

/// Published releases with a version tag, newest first. Pre-releases count:
/// the people using this are on them.
pub fn parse_releases(json: &str) -> Result<Vec<Release>, String> {
    let api: Vec<ApiRelease> = serde_json::from_str(json).map_err(|_| "unexpected reply from GitHub".to_string())?;
    let mut releases: Vec<Release> = api
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = Version::parse(r.tag_name.strip_prefix('v').unwrap_or(&r.tag_name)).ok()?;
            Some(Release { version, assets: r.assets })
        })
        .collect();
    releases.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(releases)
}

/// The newest of `releases` above `current`.
pub fn newest_above(current: &Version, releases: Vec<Release>) -> Option<Release> {
    releases.into_iter().filter(|r| r.version > *current).max_by(|a, b| a.version.cmp(&b.version))
}

/// The release asset for `std::env::consts::{OS, ARCH}`; named as install.sh and install.ps1 name them.
pub fn asset_name(os: &str, arch: &str) -> Option<String> {
    match (os, arch) {
        ("macos", "x86_64" | "aarch64") => Some(format!("corvo-wan-p2p-{arch}-apple-darwin.tar.gz")),
        ("linux", "x86_64" | "aarch64") => Some(format!("corvo-wan-p2p-{arch}-unknown-linux-musl.tar.gz")),
        ("windows", "x86_64") => Some("corvo-wan-p2p-x86_64-pc-windows-msvc.zip".into()),
        _ => None,
    }
}

/// Where releases are listed and downloaded from. Tests point these at `file://`.
pub struct Urls {
    pub api: String,
    pub download: String,
}

impl Default for Urls {
    fn default() -> Self {
        Urls {
            api: format!("https://api.github.com/repos/{REPO}/releases?per_page=20"),
            download: format!("https://github.com/{REPO}/releases/download"),
        }
    }
}

/// The newest release above `current`, if there is one.
pub async fn check_with(urls: &Urls, current: &Version) -> Result<Option<Release>, String> {
    let agent = format!("User-Agent: corvo-wan-p2p/{current}");
    let accept = "Accept: application/vnd.github+json";
    let body = curl(["-fsSL", "--max-time", "10", "-H", accept, "-H", &agent, &urls.api])
        .await
        .map_err(|e| format!("couldn't check for updates: {e}"))?;
    let json = String::from_utf8(body).map_err(|_| "unexpected reply from GitHub".to_string())?;
    Ok(newest_above(current, parse_releases(&json)?))
}

/// Puts `release`'s `asset` in place of `target`: downloads into a temp folder
/// beside it (so the last rename is on one filesystem), checks the SHA-256,
/// unpacks, then swaps the new binary in. Any failure leaves `target` as it was.
pub async fn install_with(urls: &Urls, release: &Release, asset: &str, target: &Path) -> Result<(), String> {
    let dir = target.parent().ok_or_else(|| cant_replace(target, "it has no parent folder"))?;
    sweep_leftovers(dir);
    let tmp = dir.join(format!("{TMP_PREFIX}{}", std::process::id()));
    if let Err(e) = std::fs::create_dir(&tmp) {
        let reason = if e.kind() == ErrorKind::PermissionDenied { "permission denied".into() } else { e.to_string() };
        return Err(cant_replace(target, &reason));
    }
    let result = install_in(&tmp, urls, release, asset, target).await;
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

async fn install_in(tmp: &Path, urls: &Urls, release: &Release, asset: &str, target: &Path) -> Result<(), String> {
    let version = &release.version;
    let url = format!("{}/v{version}/{asset}", urls.download);
    let archive = tmp.join(asset);
    let sums = tmp.join(format!("{asset}.sha256"));
    let failed = |e: String| format!("couldn't download v{version}: {e}");
    download(&url, &archive).await.map_err(failed)?;
    download(&format!("{url}.sha256"), &sums).await.map_err(failed)?;

    let expected = std::fs::read_to_string(&sums).unwrap_or_default();
    let expected = expected.split_whitespace().next().unwrap_or_default().to_lowercase();
    let bytes = std::fs::read(&archive).map_err(|e| e.to_string())?;
    let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if expected != actual {
        return Err("the download didn't match its checksum; nothing changed".into());
    }

    let status = tokio::process::Command::new(tar_program())
        .kill_on_drop(true)
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(tmp)
        .status()
        .await;
    match status {
        Ok(s) if s.success() => {}
        Ok(_) => return Err("couldn't unpack the download; nothing changed".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => return Err("/update needs tar, which isn't installed".into()),
        Err(e) => return Err(format!("couldn't run tar: {e}")),
    }
    let new = tmp.join(BIN_NAME);
    if !new.is_file() {
        return Err("the download didn't contain corvo-wan-p2p; nothing changed".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    swap_in(&new, target).map_err(|e| cant_replace(target, &e.to_string()))
}

/// Windows' own bsdtar reads zip; a GNU tar from Git for Windows on PATH doesn't.
fn tar_program() -> PathBuf {
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        return Path::new(&root).join("System32").join("tar.exe");
    }
    PathBuf::from("tar")
}

/// Puts `new` where `target` is. The running app keeps its open copy of the
/// old file; starting again picks up the new one.
#[cfg(not(windows))]
fn swap_in(new: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(new, target)
}

/// Windows won't replace a running exe but will rename it, so move it aside
/// first and put it back if the new one can't go in.
#[cfg(windows)]
fn swap_in(new: &Path, target: &Path) -> std::io::Result<()> {
    let old = aside_path(target.parent().unwrap_or(Path::new(".")));
    std::fs::rename(target, &old)?;
    if let Err(e) = std::fs::rename(new, target) {
        return match std::fs::rename(&old, target) {
            Ok(()) => Err(e),
            Err(_) => Err(std::io::Error::other(format!(
                "{e}; the old version is at {}, rename it back to {}",
                old.display(),
                target.display()
            ))),
        };
    }
    Ok(())
}

/// The first `corvo-wan-p2p.old-<n>.exe` in `dir` that's free, or can be freed.
#[cfg(windows)]
fn aside_path(dir: &Path) -> PathBuf {
    (0..)
        .map(|n| dir.join(format!("{OLD_EXE_PREFIX}-{n}.exe")))
        .find(|path| {
            let _ = std::fs::remove_file(path);
            !path.exists()
        })
        .expect("some name is free")
}

/// The running binary's path. Unix resolves symlinks; Windows keeps the plain
/// path, since `canonicalize` there gives a `\\?\` path that curl and tar
/// and the messages would then show.
pub fn running_exe() -> std::io::Result<PathBuf> {
    if cfg!(windows) { std::env::current_exe() } else { std::env::current_exe()?.canonicalize() }
}

/// Removes temp folders left by updates that were cut short, and exes an
/// earlier Windows update moved aside (one still running can't go yet).
fn sweep_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(TMP_PREFIX) {
            let path = entry.path();
            let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
        }
        if name.starts_with(OLD_EXE_PREFIX) && name.ends_with(".exe") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How to reinstall by hand on this system.
fn reinstall_hint() -> String {
    if cfg!(windows) { format!("irm {INSTALL_PS1_URL} | iex") } else { format!("curl -fsSL {INSTALL_URL} | sh") }
}

fn cant_replace(target: &Path, reason: &str) -> String {
    format!("can't replace {}: {reason}. Reinstall with: {}", target.display(), reinstall_hint())
}

async fn download(url: &str, out: &Path) -> Result<(), String> {
    let args = [OsStr::new("-fsSL"), OsStr::new("--max-time"), OsStr::new("120"), OsStr::new("-o"), out.as_os_str()];
    curl(args.into_iter().chain([OsStr::new(url)])).await.map(|_| ())
}

/// Runs curl and returns what it wrote.
async fn curl<I, S>(args: I) -> Result<Vec<u8>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = tokio::process::Command::new("curl").kill_on_drop(true).args(args).output().await.map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            "/update needs curl, which isn't installed".to_string()
        } else {
            format!("couldn't run curl: {e}")
        }
    })?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.trim().trim_start_matches("curl: ").trim();
    Err(match out.status.code() {
        Some(6 | 7 | 28) => "no connection to GitHub; check your internet and try again".into(),
        _ if said.is_empty() => "the download failed".into(),
        _ => said.to_string(),
    })
}

/// A fake release in a temp folder, served through `file://`.
#[cfg(test)]
pub mod testing {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;

    /// A `file://` URL for `path`, which curl reads like a download.
    pub fn file_url(path: &Path) -> String {
        let path = path.display().to_string().replace('\\', "/");
        if path.starts_with('/') { format!("file://{path}") } else { format!("file:///{path}") }
    }

    /// A release v9.0.0 in `site`, its archive holding a "binary" that says
    /// `new`. Returns the URLs and the asset's name.
    pub fn publish(site: &Path, checksum_ok: bool) -> (Urls, String) {
        let asset = "corvo-wan-p2p-test.tar.gz".to_string();
        let staging = site.join("staging");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join(BIN_NAME), "new").unwrap();
        let dir = site.join("download").join("v9.0.0");
        fs::create_dir_all(&dir).unwrap();
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(dir.join(&asset))
            .arg("-C")
            .arg(&staging)
            .arg(BIN_NAME)
            .status()
            .unwrap();
        assert!(status.success());
        let hash = Sha256::digest(fs::read(dir.join(&asset)).unwrap());
        let mut hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        if !checksum_ok {
            hex = "0".repeat(64);
        }
        fs::write(dir.join(format!("{asset}.sha256")), format!("{hex}  {asset}\n")).unwrap();
        let json = format!(
            r#"[{{"tag_name": "v9.0.0", "draft": false, "prerelease": false, "assets": [{{"name": "{asset}", "size": 1}}]}}]"#
        );
        fs::write(site.join("releases.json"), json).unwrap();
        let urls = Urls { api: file_url(&site.join("releases.json")), download: file_url(&site.join("download")) };
        (urls, asset)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{file_url, publish};
    use super::*;
    use std::fs;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn pre_releases_come_before_their_release() {
        let order = ["0.0.9", "0.1.0-rc.1", "0.1.0-rc.2", "0.1.0-rc.10", "0.1.0", "0.1.1", "0.10.0"];
        for pair in order.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
    }

    const RELEASES: &str = r#"[
        {"tag_name": "v0.1.0-rc.2", "draft": false, "prerelease": true,
         "assets": [{"name": "corvo-wan-p2p-aarch64-apple-darwin.tar.gz", "size": 10}]},
        {"tag_name": "v0.2.0", "draft": true, "prerelease": false, "assets": []},
        {"tag_name": "nightly", "draft": false, "prerelease": true, "assets": []},
        {"tag_name": "v0.1.0-rc.1", "draft": false, "prerelease": true, "assets": []}
    ]"#;

    #[test]
    fn releases_are_read_newest_first_without_drafts() {
        let releases = parse_releases(RELEASES).unwrap();
        let versions: Vec<String> = releases.iter().map(|r| r.version.to_string()).collect();
        assert_eq!(versions, vec!["0.1.0-rc.2", "0.1.0-rc.1"], "drafts and odd tags are skipped");
        assert_eq!(releases[0].assets[0].size, 10);
        assert!(parse_releases("{\"message\": \"rate limited\"}").is_err());
    }

    #[test]
    fn only_a_newer_release_is_offered() {
        let releases = parse_releases(RELEASES).unwrap();
        assert_eq!(newest_above(&v("0.1.0-rc.1"), releases.clone()).map(|r| r.version), Some(v("0.1.0-rc.2")));
        assert_eq!(newest_above(&v("0.1.0-rc.2"), releases.clone()), None);
        assert_eq!(newest_above(&v("0.1.0"), releases), None, "a full release is newer than its candidates");
    }

    #[test]
    fn each_system_gets_its_build() {
        assert_eq!(asset_name("macos", "aarch64").as_deref(), Some("corvo-wan-p2p-aarch64-apple-darwin.tar.gz"));
        assert_eq!(asset_name("macos", "x86_64").as_deref(), Some("corvo-wan-p2p-x86_64-apple-darwin.tar.gz"));
        assert_eq!(asset_name("linux", "aarch64").as_deref(), Some("corvo-wan-p2p-aarch64-unknown-linux-musl.tar.gz"));
        assert_eq!(asset_name("windows", "x86_64").as_deref(), Some("corvo-wan-p2p-x86_64-pc-windows-msvc.zip"));
        assert_eq!(asset_name("windows", "aarch64"), None);
        assert_eq!(asset_name("freebsd", "x86_64"), None);
    }

    #[tokio::test]
    async fn a_newer_release_is_found_and_put_in_place() {
        let (site, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (urls, asset) = publish(site.path(), true);
        let target = bin.path().join(BIN_NAME);
        fs::write(&target, "old").unwrap();
        let found = check_with(&urls, &v("0.1.0-rc.1")).await.unwrap().expect("v9.0.0 is newer");
        assert_eq!(found.version, v("9.0.0"));
        assert_eq!(check_with(&urls, &v("9.0.0")).await.unwrap(), None);
        install_with(&urls, &found, &asset, &target).await.unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        let mut left: Vec<String> =
            fs::read_dir(bin.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        // Windows can't delete a running exe, so the old one waits beside it
        // until the next update clears it.
        let expected = if cfg!(windows) { vec![BIN_NAME, "corvo-wan-p2p.old-0.exe"] } else { vec![BIN_NAME] };
        assert_eq!(left, expected, "no temp files left beside it");
        if cfg!(windows) {
            assert_eq!(fs::read_to_string(bin.path().join("corvo-wan-p2p.old-0.exe")).unwrap(), "old");
        }
    }

    #[tokio::test]
    async fn a_bad_checksum_changes_nothing() {
        let (site, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (urls, asset) = publish(site.path(), false);
        let target = bin.path().join(BIN_NAME);
        fs::write(&target, "old").unwrap();
        let found = check_with(&urls, &v("0.1.0")).await.unwrap().unwrap();
        let err = install_with(&urls, &found, &asset, &target).await.unwrap_err();
        assert!(err.contains("nothing changed"), "{err}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    }

    #[tokio::test]
    async fn an_unreachable_server_says_so() {
        let site = tempfile::tempdir().unwrap();
        let urls = Urls { api: file_url(&site.path().join("missing.json")), download: String::new() };
        assert!(check_with(&urls, &v("0.1.0")).await.unwrap_err().contains("couldn't"));
    }
}
