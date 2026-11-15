//! Updates: a daily look for a newer release, a verified download in the background, and the install once the app has quit.
//! Everything lives in `updates/` in the data folder; `state.json` there remembers what was found, downloaded and installed.

pub mod download;
pub mod install;
pub mod release;
pub mod verify;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub use release::{Kind, Release};

use super::{net, paths};

const LIST: &str = "https://api.github.com/repos/schrumbo/manycraft/releases?per_page=20";
const STATE: &str = "state.json";
const DOWNLOADING: &str = "download.lock";
const TIMEOUT: Duration = Duration::from_secs(10);
const DAY: u64 = 24 * 3600;
const MAX_SIZE: u64 = 1 << 30;

static EDIT: Mutex<()> = Mutex::new(());

/// What went wrong last, for the quiet line in Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trouble {
    /// The download stopped or the server could not be reached.
    Download,
    /// The file did not match its checksum or signature and was deleted.
    Damaged,
    /// The installer or the switch back failed; the running version stayed.
    Install,
}

/// A downloaded and verified installer, waiting for the app to quit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ready {
    pub version: String,
    pub notes: String,
    pub page: String,
    /// File name inside the updates folder.
    pub file: String,
}

/// A version this app installed itself, until it has proven it starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fresh {
    pub version: String,
    pub notes: String,
    /// Starts that ended before the window was up for a while, in a row.
    pub failed_starts: u32,
    pub proven: bool,
    /// Whether the user dismissed the "new in" line.
    pub seen: bool,
}

/// The version before the last update, kept for going back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Previous {
    pub version: String,
    pub path: PathBuf,
}

/// Everything the updater remembers between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub checked_at: u64,
    pub checked_prereleases: bool,
    pub newest: Option<Release>,
    pub ready: Option<Ready>,
    pub failed_at: u64,
    pub trouble: Option<Trouble>,
    /// A version the user or a crash went back from; it is never offered again.
    pub skipped: Option<String>,
    /// A version whose download the user cancelled; it is not fetched by itself again, only on request.
    pub declined: Option<String>,
    pub fresh: Option<Fresh>,
    pub previous: Option<Previous>,
}

/// The folder holding downloads, the kept previous version and the state.
pub fn dir() -> PathBuf {
    paths::data_dir().join("updates")
}

/// Where the release list comes from; `MANYCRAFT_UPDATE_URL` replaces GitHub for tests.
fn list_url() -> String {
    override_url().unwrap_or_else(|| LIST.to_owned())
}

/// The release list `MANYCRAFT_UPDATE_URL` names instead of GitHub's, for tests.
pub fn override_url() -> Option<String> {
    std::env::var("MANYCRAFT_UPDATE_URL").ok().filter(|u| !u.trim().is_empty())
}

/// What `state.json` in `dir` says; a missing or damaged file reads as empty.
pub fn load(dir: &Path) -> State {
    std::fs::read(dir.join(STATE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Changes `state.json` in `dir` under a lock, so two changes never undo each other.
pub fn edit<T>(dir: &Path, change: impl FnOnce(&mut State) -> T) -> Result<T> {
    let _guard = EDIT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut state = load(dir);
    let out = change(&mut state);
    net::write_atomic(&dir.join(STATE), &serde_json::to_vec_pretty(&state)?)?;
    Ok(out)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn within_a_day(then: u64, now: u64) -> bool {
    then > 0 && then <= now && now - then < DAY
}

/// The newest release newer than `current`. Asks at most once a day and answers from the state in between, unless `force`.
pub fn check(current: &str, prereleases: bool, force: bool) -> Result<Option<Release>> {
    let agent = net::agent_within(TIMEOUT);
    check_in(&dir(), current, prereleases, force, now(), || net::get_json(&agent, &list_url()))
}

fn check_in(dir: &Path, current: &str, prereleases: bool, force: bool, now: u64, fetch: impl FnOnce() -> Result<serde_json::Value>) -> Result<Option<Release>> {
    let state = load(dir);
    if !force && state.checked_prereleases == prereleases && within_a_day(state.checked_at, now) {
        let skipped = state.skipped.as_deref();
        return Ok(state.newest.filter(|r| release::is_newer(&r.version, current) && skipped.is_none_or(|s| !release::same(&r.version, s))));
    }
    let newest = release::newest(fetch()?, current, prereleases, state.skipped.as_deref())?;
    let saved = edit(dir, |s| {
        s.checked_at = now;
        s.checked_prereleases = prereleases;
        s.newest = newest.clone();
    });
    if let Err(e) = saved {
        eprintln!("update: cannot remember the check: {e:#}");
    }
    Ok(newest)
}

/// Why [`prepare`] did not produce an installer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// The release has no signed file for this kind of install; the release page is the way.
    NoInstaller,
    /// The last attempt failed less than a day ago.
    Waiting,
    /// The user stopped the download; what was loaded stays for the next try.
    Cancelled,
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Skip::NoInstaller => "no signed installer for this system",
            Skip::Waiting => "the last attempt failed less than a day ago",
            Skip::Cancelled => "the download was cancelled",
        })
    }
}

impl std::error::Error for Skip {}

/// Downloads the installer of `release` that fits `kind`, checks it against its SHA-256 and the release signature, and
/// remembers it as ready. A verified copy already there is reused; a failure waits a day unless `force`.
/// Setting `cancel` stops it with [`Skip::Cancelled`], which counts as no failure.
pub fn prepare(release: &Release, kind: Kind, force: bool, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Ready> {
    let agent = net::agent(None)?;
    prepare_in(&dir(), release, kind, std::env::consts::ARCH, &verify::release_key(), force, now(), &agent, cancel, progress)
}

#[allow(clippy::too_many_arguments)]
fn prepare_in(dir: &Path, release: &Release, kind: Kind, arch: &str, key: &str, force: bool, now: u64, agent: &ureq::Agent, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Ready> {
    let state = load(dir);
    if let Some(ready) = state.ready.filter(|r| release::same(&r.version, &release.version) && dir.join(&r.file).is_file()) {
        return Ok(ready);
    }
    let asset = release::pick(&release.assets, kind, arch).ok_or(Skip::NoInstaller)?;
    if !plain_name(&asset.name) {
        bail!(Skip::NoInstaller);
    }
    if !force && within_a_day(state.failed_at, now) {
        bail!(Skip::Waiting);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let Some(_downloading) = crate::core::lock::try_exclusive(&dir.join(DOWNLOADING))? else { bail!(Skip::Waiting) };
    let fail = |trouble: Trouble, e: anyhow::Error| {
        let _ = edit(dir, |s| {
            s.failed_at = now;
            s.trouble = Some(trouble);
        });
        e
    };
    let sidecars = (|| -> Result<(String, String)> {
        let sha = release::sidecar(&release.assets, &asset.name, "sha256").context("no checksum")?;
        let sig = release::sidecar(&release.assets, &asset.name, "minisig").context("no signature")?;
        Ok((download::text(agent, &sha.url)?, download::text(agent, &sig.url)?))
    })();
    let (sha, sig) = sidecars.map_err(|e| if cancel.load(Ordering::SeqCst) { Skip::Cancelled.into() } else { fail(Trouble::Download, e) })?;
    let part = dir.join(format!("{}.part", asset.name));
    let max = if asset.size > 0 { asset.size.min(MAX_SIZE) } else { MAX_SIZE };
    download::resumable(agent, &asset.url, &part, max, cancel, progress).map_err(|e| if e.downcast_ref::<Skip>() == Some(&Skip::Cancelled) { e } else { fail(Trouble::Download, e) })?;
    if let Err(e) = verify::file(&part, &sha, &sig, key, &verify::comment(&release.version, &asset.name)) {
        let _ = std::fs::remove_file(&part);
        return Err(fail(Trouble::Damaged, e));
    }
    if cancel.load(Ordering::SeqCst) {
        bail!(Skip::Cancelled);
    }
    let file = dir.join(&asset.name);
    std::fs::write(sidecar_path(&file, "sha256"), &sha)?;
    std::fs::write(sidecar_path(&file, "minisig"), &sig)?;
    std::fs::rename(&part, &file).with_context(|| format!("cannot move {}", part.display()))?;
    forget_downloads(dir, &asset.name);
    let ready = Ready { version: release.version.clone(), notes: release.notes.clone(), page: release.page.clone(), file: asset.name.clone() };
    edit(dir, |s| {
        s.ready = Some(ready.clone());
        s.failed_at = 0;
        s.trouble = None;
    })?;
    Ok(ready)
}

fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', ':']) && !name.contains("..")
}

/// `<file>.<ext>` next to `file`.
pub fn sidecar_path(file: &Path, ext: &str) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(ext);
    file.with_file_name(name)
}

fn forget_downloads(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = name.starts_with("Manycraft") && !name.starts_with(keep);
        if ours && entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Drops the downloaded installer and what the state says about it.
pub fn discard_ready(dir: &Path) {
    let ready = edit(dir, |s| s.ready.take()).ok().flatten();
    if let Some(ready) = ready {
        let file = dir.join(&ready.file);
        for path in [sidecar_path(&file, "sha256"), sidecar_path(&file, "minisig"), file] {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The ready installer, if it is newer than `current` and still on disk.
pub fn ready_for(state: &State, dir: &Path, current: &str) -> Option<Ready> {
    state.ready.clone().filter(|r| release::is_newer(&r.version, current) && dir.join(&r.file).is_file())
}

/// The "new in" line for `current`: its version and the first line of its notes, until the user dismissed it.
pub fn news(state: &State, current: &str) -> Option<(String, String)> {
    state.fresh.as_ref().filter(|f| release::same(&f.version, current) && !f.seen).map(|f| (f.version.clone(), f.notes.clone()))
}

/// The version kept for going back, while it is still on disk.
pub fn previous(state: &State) -> Option<Previous> {
    state.previous.clone().filter(|p| p.path.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/testdata/update");
    const NO: AtomicBool = AtomicBool::new(false);

    fn list(tag: &str) -> serde_json::Value {
        json!([{ "tag_name": tag, "html_url": format!("https://github.com/schrumbo/manycraft/releases/tag/{tag}"), "draft": false, "prerelease": false, "body": "Faster." }])
    }

    #[test]
    fn a_newer_release_is_reported_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let found = check_in(dir.path(), "0.2.0", false, false, 1000, || Ok(list("v0.3.0"))).unwrap().unwrap();
        assert_eq!(found.version, "0.3.0");
        assert!(found.page.ends_with("v0.3.0"));
        assert_eq!(load(dir.path()).newest, Some(found));
    }

    #[test]
    fn within_a_day_the_state_answers_without_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let first = check_in(dir.path(), "0.2.0", false, false, 1000, || Ok(list("v0.3.0"))).unwrap();
        let second = check_in(dir.path(), "0.2.0", false, false, 1000 + DAY - 1, || panic!("asked the network")).unwrap();
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn force_after_a_day_or_a_changed_channel_asks_again() {
        let dir = tempfile::tempdir().unwrap();
        check_in(dir.path(), "0.2.0", false, false, 1000, || Ok(list("v0.3.0"))).unwrap();
        assert_eq!(check_in(dir.path(), "0.2.0", false, false, 1000 + DAY, || Ok(list("v0.4.0"))).unwrap().unwrap().version, "0.4.0");
        assert_eq!(check_in(dir.path(), "0.2.0", false, true, 1001 + DAY, || Ok(list("v0.5.0"))).unwrap().unwrap().version, "0.5.0");
        assert_eq!(check_in(dir.path(), "0.2.0", true, false, 1002 + DAY, || Ok(list("v0.6.0"))).unwrap().unwrap().version, "0.6.0");
    }

    #[test]
    fn a_cached_release_the_app_caught_up_with_or_skipped_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        check_in(dir.path(), "0.2.0", false, false, 1000, || Ok(list("v0.3.0"))).unwrap();
        assert_eq!(check_in(dir.path(), "0.3.0", false, false, 2000, || panic!("asked the network")).unwrap(), None);
        edit(dir.path(), |s| s.skipped = Some("0.3.0".into())).unwrap();
        assert_eq!(check_in(dir.path(), "0.2.0", false, false, 2000, || panic!("asked the network")).unwrap(), None);
        assert_eq!(check_in(dir.path(), "0.2.0", false, true, 2001, || Ok(list("v0.3.0"))).unwrap(), None);
    }

    #[test]
    fn a_failed_fetch_is_an_error_and_is_not_remembered() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_in(dir.path(), "0.2.0", false, false, 1000, || anyhow::bail!("offline")).is_err());
        assert!(!dir.path().join(STATE).exists());
        assert!(check_in(dir.path(), "0.2.0", false, false, 1001, || Ok(list("v0.3.0"))).unwrap().is_some());
    }

    #[test]
    fn a_damaged_state_or_odd_answer_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE), b"{ oops").unwrap();
        assert!(check_in(dir.path(), "0.2.0", false, false, 5, || Ok(list("v0.3.0"))).unwrap().is_some());
        assert!(check_in(dir.path(), "0.2.0", false, false, 5 + DAY, || Ok(json!({ "message": "Not Found" }))).is_err());
    }

    #[test]
    fn a_clock_set_back_does_not_freeze_the_state() {
        let dir = tempfile::tempdir().unwrap();
        check_in(dir.path(), "0.2.0", false, false, 10 * DAY, || Ok(list("v0.3.0"))).unwrap();
        assert_eq!(check_in(dir.path(), "0.2.0", false, false, 1000, || Ok(list("v0.4.0"))).unwrap().unwrap().version, "0.4.0");
    }

    struct Hosted {
        _servers: Vec<download::tests::Server>,
        release: Release,
    }

    fn host(body: Vec<u8>, sha: String, sig: String) -> Hosted {
        let name = "Manycraft-Setup-0.9.0-windows-x64.exe";
        let servers = vec![download::tests::serve(body, true, None), download::tests::serve(sha.into_bytes(), true, None), download::tests::serve(sig.into_bytes(), true, None)];
        let asset = |n: String, s: &download::tests::Server| release::Asset { name: n, url: s.url.clone(), size: 0 };
        let assets = vec![asset(name.into(), &servers[0]), asset(format!("{name}.sha256"), &servers[1]), asset(format!("{name}.minisig"), &servers[2])];
        Hosted { release: Release { version: "0.9.0".into(), page: "https://github.com/schrumbo/manycraft/releases/tag/v0.9.0".into(), notes: "Faster.".into(), assets }, _servers: servers }
    }

    fn fixture() -> (Vec<u8>, String, String, String) {
        let body = std::fs::read(format!("{FIXTURES}/asset.bin")).unwrap();
        let sig = std::fs::read_to_string(format!("{FIXTURES}/asset.bin.minisig")).unwrap();
        let key = std::fs::read_to_string(format!("{FIXTURES}/fixture.pub")).unwrap().lines().nth(1).unwrap().to_owned();
        (body, "bb0cabcbff009359250bafb84c7be5a7ac81f84340ffc6db268645997b17c308  asset.bin".into(), sig, key)
    }

    fn agent() -> ureq::Agent {
        net::agent_within(Duration::from_secs(10))
    }

    #[test]
    fn a_verified_download_becomes_ready_and_is_reused() {
        let (body, sha, sig, key) = fixture();
        let hosted = host(body.clone(), sha, sig);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Manycraft-Setup-0.8.0-windows-x64.exe"), b"old").unwrap();
        let ready = prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, false, 100, &agent(), &NO, &mut |_, _| {}).unwrap();
        assert_eq!(ready.version, "0.9.0");
        assert_eq!(std::fs::read(dir.path().join(&ready.file)).unwrap(), body);
        assert!(sidecar_path(&dir.path().join(&ready.file), "minisig").is_file());
        assert!(!dir.path().join("Manycraft-Setup-0.8.0-windows-x64.exe").exists());
        let state = load(dir.path());
        assert_eq!(state.ready.as_ref(), Some(&ready));
        assert_eq!(ready_for(&state, dir.path(), "0.5.0"), Some(ready.clone()));
        assert_eq!(ready_for(&state, dir.path(), "0.9.0"), None);
        let again = prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", "unused", false, 101, &agent(), &NO, &mut |_, _| panic!("downloaded twice")).unwrap();
        assert_eq!(again, ready);
    }

    #[test]
    fn a_tampered_download_is_deleted_and_waits_a_day() {
        let (mut body, sha, sig, key) = fixture();
        body[3] ^= 0x20;
        let hosted = host(body, sha, sig);
        let dir = tempfile::tempdir().unwrap();
        assert!(prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, false, 100, &agent(), &NO, &mut |_, _| {}).is_err());
        let state = load(dir.path());
        assert_eq!(state.trouble, Some(Trouble::Damaged));
        assert_eq!(state.ready, None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().flatten().filter(|e| e.file_name() != STATE && e.file_name() != DOWNLOADING).count(), 0);
        let waiting = prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, false, 100 + DAY - 1, &agent(), &NO, &mut |_, _| {}).unwrap_err();
        assert_eq!(waiting.downcast_ref::<Skip>(), Some(&Skip::Waiting));
        assert!(prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, true, 100 + DAY - 1, &agent(), &NO, &mut |_, _| {}).unwrap_err().downcast_ref::<Skip>().is_none());
    }

    #[test]
    fn a_second_manycraft_waits_while_one_downloads() {
        let (body, sha, sig, key) = fixture();
        let hosted = host(body, sha, sig);
        let dir = tempfile::tempdir().unwrap();
        let held = crate::core::lock::try_exclusive(&dir.path().join(DOWNLOADING)).unwrap().unwrap();
        let e = prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, true, 100, &agent(), &NO, &mut |_, _| {}).unwrap_err();
        assert_eq!(e.downcast_ref::<Skip>(), Some(&Skip::Waiting));
        drop(held);
        prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, true, 100, &agent(), &NO, &mut |_, _| {}).unwrap();
    }

    #[test]
    fn a_cancelled_download_is_no_failure_and_resumes() {
        let (body, sha, sig, key) = fixture();
        let hosted = host(body.clone(), sha, sig);
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let e = prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, false, 100, &agent(), &cancel, &mut |_, _| {}).unwrap_err();
        assert_eq!(e.downcast_ref::<Skip>(), Some(&Skip::Cancelled));
        let state = load(dir.path());
        assert_eq!((state.trouble, state.failed_at, state.ready), (None, 0, None));
        let ready = prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", &key, false, 101, &agent(), &NO, &mut |_, _| {}).unwrap();
        assert_eq!(std::fs::read(dir.path().join(&ready.file)).unwrap(), body);
    }

    #[test]
    fn the_wrong_key_counts_as_damaged() {
        let (body, sha, sig, _) = fixture();
        let hosted = host(body, sha, sig);
        let dir = tempfile::tempdir().unwrap();
        assert!(prepare_in(dir.path(), &hosted.release, Kind::Installer, "x86_64", verify::RELEASE_KEY, false, 100, &agent(), &NO, &mut |_, _| {}).is_err());
        assert_eq!(load(dir.path()).trouble, Some(Trouble::Damaged));
    }

    #[test]
    fn installs_that_cannot_update_themselves_get_no_download() {
        let (body, sha, sig, key) = fixture();
        let hosted = host(body, sha, sig);
        let dir = tempfile::tempdir().unwrap();
        for kind in [Kind::Package, Kind::Unknown, Kind::Bundle] {
            let e = prepare_in(dir.path(), &hosted.release, kind, "x86_64", &key, true, 100, &agent(), &NO, &mut |_, _| {}).unwrap_err();
            assert_eq!(e.downcast_ref::<Skip>(), Some(&Skip::NoInstaller));
        }
    }

    #[test]
    fn asset_names_from_the_network_stay_inside_the_folder() {
        assert!(plain_name("Manycraft-Setup-0.9.0-windows-x64.exe"));
        for bad in ["", "../x.exe", "a/b", "a\\b", ".hidden", "C:x", "Manycraft..exe"] {
            assert!(!plain_name(bad), "{bad}");
        }
    }

    #[test]
    fn news_shows_once_for_the_fresh_version() {
        let mut state = State { fresh: Some(Fresh { version: "0.6.0".into(), notes: "Faster.".into(), ..Fresh::default() }), ..State::default() };
        assert_eq!(news(&state, "0.6.0"), Some(("0.6.0".into(), "Faster.".into())));
        assert_eq!(news(&state, "0.5.0"), None);
        if let Some(f) = state.fresh.as_mut() {
            f.seen = true;
        }
        assert_eq!(news(&state, "0.6.0"), None);
    }
}
