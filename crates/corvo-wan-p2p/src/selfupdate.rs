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
/// Held locked in an update's temp folder while it works, so another
/// update's sweep leaves the folder alone.
const LOCK_FILE: &str = ".lock";
/// How long the new binary gets to print its version before the swap.
const VERSION_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Said instead of updating a binary built from a checkout.
pub const FROM_SOURCE: &str = "built from source; update with git pull && cargo build";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release's tag, as its downloads are found under (`v0.1.0`, or a tag without the `v`).
    pub tag: String,
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
            Some(Release { version, tag: r.tag_name, assets: r.assets })
        })
        .collect();
    releases.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(releases)
}

/// The newest of `releases` above `current` that has `asset`, this system's build.
pub fn newest_above(current: &Version, releases: Vec<Release>, asset: &str) -> Option<Release> {
    releases
        .into_iter()
        .filter(|r| r.version > *current && r.assets.iter().any(|a| a.name == asset))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// What `/update` says on a system no release is built for.
pub fn no_build(os: &str, arch: &str) -> String {
    format!("no release build for {os}/{arch}; build from source")
}

/// Whether `exe` is in a cargo build folder (`target/debug`, `target/release`,
/// or the same under `target/<triple>`), so it came from a checkout.
pub fn is_built_from_source(exe: &Path) -> bool {
    let dirs: Vec<&OsStr> = exe.parent().map(|p| p.iter().collect()).unwrap_or_default();
    let Some((last, before)) = dirs.split_last() else { return false };
    let profile = matches!(last.to_str(), Some("debug" | "release"));
    let target_at = |back: usize| before.len() >= back && before[before.len() - back] == "target";
    profile && (target_at(1) || target_at(2))
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

/// The newest release above `current` with `asset`, this system's build, if there is one.
pub async fn check_with(urls: &Urls, current: &Version, asset: &str) -> Result<Option<Release>, String> {
    let agent = format!("User-Agent: corvo-wan-p2p/{current}");
    let accept = "Accept: application/vnd.github+json";
    let body = curl(["-fsSL", "--max-time", "10", "-H", accept, "-H", &agent, &urls.api])
        .await
        .map_err(|e| format!("couldn't check for updates: {e}"))?;
    let json = String::from_utf8(body).map_err(|_| "unexpected reply from GitHub".to_string())?;
    Ok(newest_above(current, parse_releases(&json)?, asset))
}

/// Puts `release`'s `asset` in place of `target`: downloads into a temp folder
/// beside it (so the last rename is on one filesystem), checks the SHA-256,
/// unpacks, runs the new binary (with `verify`) to see it works here, then
/// swaps it in. Any failure leaves `target` as it was.
pub async fn install_with(
    urls: &Urls,
    release: &Release,
    asset: &str,
    target: &Path,
    verify: bool,
) -> Result<(), String> {
    let dir = target.parent().ok_or_else(|| cant_replace(target, "it has no parent folder"))?;
    sweep_leftovers(dir);
    let tmp = dir.join(format!("{TMP_PREFIX}{}", std::process::id()));
    let lock = std::fs::create_dir(&tmp).and_then(|()| {
        let lock = std::fs::File::create(tmp.join(LOCK_FILE))?;
        lock.try_lock()?;
        Ok(lock)
    });
    let lock = match lock {
        Ok(lock) => lock,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            let reason =
                if e.kind() == ErrorKind::PermissionDenied { "permission denied".into() } else { e.to_string() };
            return Err(cant_replace(target, &reason));
        }
    };
    let result = install_in(&tmp, urls, release, asset, target, verify).await;
    // Closed first: Windows can't delete a file that's open.
    drop(lock);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

async fn install_in(
    tmp: &Path,
    urls: &Urls,
    release: &Release,
    asset: &str,
    target: &Path,
    verify: bool,
) -> Result<(), String> {
    let version = &release.version;
    let url = format!("{}/{}/{asset}", urls.download, release.tag);
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
    if verify && !prints_version(&new, version).await {
        return Err("the new version doesn't run here; nothing changed".into());
    }
    // On disk before the swap, and the swap itself after it, in case the power goes.
    std::fs::File::open(&new).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
    swap_in(&new, target).map_err(|e| cant_replace(target, &e.to_string()))?;
    #[cfg(unix)]
    if let Some(dir) = target.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

/// Whether `exe --version` prints `corvo-wan-p2p <version>` within [`VERSION_WAIT`].
async fn prints_version(exe: &Path, version: &Version) -> bool {
    let run = tokio::process::Command::new(exe).arg("--version").kill_on_drop(true).output();
    match tokio::time::timeout(VERSION_WAIT, run).await {
        Ok(Ok(out)) => {
            out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == format!("corvo-wan-p2p {version}")
        }
        _ => false,
    }
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

/// Removes temp folders left by updates that were cut short (not one another
/// update holds locked), and exes an earlier Windows update moved aside (one
/// still running can't go yet).
fn sweep_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.starts_with(TMP_PREFIX) && !in_use(&path) {
            let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
        }
        if name.starts_with(OLD_EXE_PREFIX) && name.ends_with(".exe") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether an update is working in the temp folder `tmp`: its lock is held.
fn in_use(tmp: &Path) -> bool {
    match std::fs::File::open(tmp.join(LOCK_FILE)) {
        Ok(lock) => matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        Err(_) => false,
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
    Err(curl_failure(out.status.code(), &String::from_utf8_lossy(&out.stderr)))
}

/// What a failed curl means, from its exit `code` and what it wrote to stderr.
/// With `-f`, any HTTP error is exit 22 and the status is in the message.
pub fn curl_failure(code: Option<i32>, stderr: &str) -> String {
    let said = stderr.trim().trim_start_matches("curl: ").trim();
    let http = |status: &str| code == Some(22) && said.ends_with(&format!("error: {status}"));
    match code {
        Some(6 | 7) => "no connection to GitHub; check your internet and try again".into(),
        Some(28) => "GitHub didn't answer in time; try again".into(),
        _ if http("403") || http("429") => "GitHub is limiting requests from here; try again in an hour".into(),
        _ if said.is_empty() => "the download failed".into(),
        _ => said.to_string(),
    }
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

    /// What the "binary" in a fake release does when run.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Binary {
        /// Prints `corvo-wan-p2p 9.0.0`, like the real one (a shell script, so Unix only).
        Works,
        /// Fails when run.
        Broken,
        /// The text `new`: for systems where it isn't run.
        Text,
    }

    /// A release v9.0.0 in `site` that works where it's run, tagged `v9.0.0`.
    /// Returns the URLs and the asset's name.
    pub fn publish(site: &Path, checksum_ok: bool) -> (Urls, String) {
        let binary = if cfg!(unix) { Binary::Works } else { Binary::Text };
        publish_as(site, "v9.0.0", binary, checksum_ok)
    }

    /// A release v9.0.0 in `site` under `tag`, holding `binary`.
    pub fn publish_as(site: &Path, tag: &str, binary: Binary, checksum_ok: bool) -> (Urls, String) {
        let asset = "corvo-wan-p2p-test.tar.gz".to_string();
        let staging = site.join("staging");
        fs::create_dir_all(&staging).unwrap();
        let contents = match binary {
            Binary::Works => "#!/bin/sh\necho 'corvo-wan-p2p 9.0.0'\n",
            Binary::Broken => "#!/bin/sh\nexit 1\n",
            Binary::Text => "new",
        };
        fs::write(staging.join(BIN_NAME), contents).unwrap();
        let dir = site.join("download").join(tag);
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
            r#"[{{"tag_name": "{tag}", "draft": false, "prerelease": false, "assets": [{{"name": "{asset}", "size": 1}}]}}]"#
        );
        fs::write(site.join("releases.json"), json).unwrap();
        let urls = Urls { api: file_url(&site.join("releases.json")), download: file_url(&site.join("download")) };
        (urls, asset)
    }

    /// Whether the new binary is run before the swap: where the fake one can run.
    pub const VERIFY: bool = cfg!(unix);
}

#[cfg(test)]
mod tests {
    use super::testing::{Binary, VERIFY, file_url, publish, publish_as};
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
        let mac = "corvo-wan-p2p-aarch64-apple-darwin.tar.gz";
        assert_eq!(newest_above(&v("0.1.0-rc.1"), releases.clone(), mac).map(|r| r.version), Some(v("0.1.0-rc.2")));
        assert_eq!(newest_above(&v("0.1.0-rc.2"), releases.clone(), mac), None);
        assert_eq!(newest_above(&v("0.1.0"), releases, mac), None, "a full release is newer than its candidates");
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
        let found = check_with(&urls, &v("0.1.0-rc.1"), &asset).await.unwrap().expect("v9.0.0 is newer");
        assert_eq!(found.version, v("9.0.0"));
        assert_eq!(check_with(&urls, &v("9.0.0"), &asset).await.unwrap(), None);
        let new = fs::read_to_string(site.path().join("staging").join(BIN_NAME)).unwrap();
        install_with(&urls, &found, &asset, &target, VERIFY).await.unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), new);
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
        let found = check_with(&urls, &v("0.1.0"), &asset).await.unwrap().unwrap();
        let err = install_with(&urls, &found, &asset, &target, VERIFY).await.unwrap_err();
        assert!(err.contains("nothing changed"), "{err}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    }

    #[tokio::test]
    async fn an_unreachable_server_says_so() {
        let site = tempfile::tempdir().unwrap();
        let urls = Urls { api: file_url(&site.path().join("missing.json")), download: String::new() };
        assert!(check_with(&urls, &v("0.1.0"), "any").await.unwrap_err().contains("couldn't"));
    }

    #[test]
    fn only_releases_with_this_build_are_offered() {
        let json = r#"[
            {"tag_name": "v0.3.0", "assets": [{"name": "corvo-wan-p2p-x86_64-pc-windows-msvc.zip", "size": 1}]},
            {"tag_name": "v0.2.0", "assets": [{"name": "corvo-wan-p2p-aarch64-apple-darwin.tar.gz", "size": 1}]}
        ]"#;
        let releases = parse_releases(json).unwrap();
        let mac = "corvo-wan-p2p-aarch64-apple-darwin.tar.gz";
        assert_eq!(newest_above(&v("0.1.0"), releases.clone(), mac).map(|r| r.version), Some(v("0.2.0")));
        assert_eq!(newest_above(&v("0.2.0"), releases, mac), None, "0.3.0 has no build for this system");
        assert_eq!(no_build("freebsd", "x86_64"), "no release build for freebsd/x86_64; build from source");
    }

    #[test]
    fn curl_failures_are_explained() {
        let limited = "GitHub is limiting requests from here; try again in an hour";
        assert_eq!(curl_failure(Some(22), "curl: (22) The requested URL returned error: 403"), limited);
        assert_eq!(curl_failure(Some(22), "curl: (22) The requested URL returned error: 429"), limited);
        assert_eq!(curl_failure(Some(28), "curl: (28) Operation timed out"), "GitHub didn't answer in time; try again");
        assert_eq!(curl_failure(Some(6), ""), "no connection to GitHub; check your internet and try again");
        assert_eq!(
            curl_failure(Some(22), "curl: (22) The requested URL returned error: 404"),
            "(22) The requested URL returned error: 404"
        );
        assert_eq!(curl_failure(None, ""), "the download failed");
    }

    #[tokio::test]
    async fn downloads_use_the_tag() {
        let (site, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let binary = if VERIFY { Binary::Works } else { Binary::Text };
        let (urls, asset) = publish_as(site.path(), "9.0.0", binary, true);
        let target = bin.path().join(BIN_NAME);
        fs::write(&target, "old").unwrap();
        let found = check_with(&urls, &v("0.1.0"), &asset).await.unwrap().unwrap();
        assert_eq!(found.tag, "9.0.0");
        install_with(&urls, &found, &asset, &target, VERIFY).await.unwrap();
        assert_ne!(fs::read_to_string(&target).unwrap(), "old", "found under 9.0.0, not v9.0.0");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_binary_that_does_not_run_changes_nothing() {
        let (site, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (urls, asset) = publish_as(site.path(), "v9.0.0", Binary::Broken, true);
        let target = bin.path().join(BIN_NAME);
        fs::write(&target, "old").unwrap();
        let found = check_with(&urls, &v("0.1.0"), &asset).await.unwrap().unwrap();
        let err = install_with(&urls, &found, &asset, &target, true).await.unwrap_err();
        assert_eq!(err, "the new version doesn't run here; nothing changed");
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    }

    #[test]
    fn a_locked_temp_folder_is_left_alone() {
        let bin = tempfile::tempdir().unwrap();
        let (busy, done, old) = (
            bin.path().join(format!("{TMP_PREFIX}1")),
            bin.path().join(format!("{TMP_PREFIX}2")),
            bin.path().join(format!("{TMP_PREFIX}3")),
        );
        for dir in [&busy, &done, &old] {
            fs::create_dir(dir).unwrap();
        }
        let lock = fs::File::create(busy.join(LOCK_FILE)).unwrap();
        lock.try_lock().unwrap();
        fs::File::create(done.join(LOCK_FILE)).unwrap();
        sweep_leftovers(bin.path());
        assert!(busy.exists(), "another update is using it");
        assert!(!done.exists() && !old.exists(), "left by updates that were cut short");
    }

    #[test]
    fn built_from_source() {
        for path in [
            "/src/corvo/target/debug/corvo-wan-p2p",
            "/src/corvo/target/release/corvo-wan-p2p",
            "/src/corvo/target/x86_64-unknown-linux-musl/release/corvo-wan-p2p",
            "C:/src/corvo/target/debug/corvo-wan-p2p.exe",
        ] {
            assert!(is_built_from_source(Path::new(path)), "{path}");
        }
        for path in [
            "/home/ana/.local/bin/corvo-wan-p2p",
            "/opt/target/corvo-wan-p2p",
            "/home/ana/release/corvo-wan-p2p",
            "/usr/local/bin/corvo-wan-p2p",
        ] {
            assert!(!is_built_from_source(Path::new(path)), "{path}");
        }
        assert_eq!(FROM_SOURCE, "built from source; update with git pull && cargo build");
    }
}
