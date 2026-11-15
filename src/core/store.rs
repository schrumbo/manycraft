//! Writing the app's JSON files under a lock, refusing to overwrite what another window saved since this thread last read it.
//! Every write keeps the previous good version as `<file>.bak`; a load falls back to it when the file is missing, empty or damaged.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use super::{lock, net};

const LOCK_WAIT: Duration = Duration::from_secs(15);

thread_local! {
    static SEEN: RefCell<HashMap<PathBuf, Option<u64>>> = RefCell::default();
}

static RECOVERED: Mutex<Vec<(PathBuf, Recovery)>> = Mutex::new(Vec::new());

/// What [`load`] did with a file that was missing, empty or damaged while a backup existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// The backup was good and replaced the file.
    Restored,
    /// Nothing was usable; the damaged copies were set aside as `<file>.damaged` and `<file>.bak.damaged`.
    Lost,
}

fn fingerprint(bytes: Option<&[u8]>) -> Option<u64> {
    bytes.map(|b| {
        let mut hasher = DefaultHasher::new();
        b.hash(&mut hasher);
        hasher.finish()
    })
}

fn remember(path: &Path, bytes: Option<&[u8]>) {
    SEEN.with(|seen| seen.borrow_mut().insert(path.to_path_buf(), fingerprint(bytes)));
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(std::ffi::OsString::from).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

fn lock_path(path: &Path) -> PathBuf {
    suffixed(path, ".lock")
}

/// The previous good version of `path`.
pub fn backup_path(path: &Path) -> PathBuf {
    suffixed(path, ".bak")
}

/// Where a damaged `path` is set aside.
pub fn damaged_path(path: &Path) -> PathBuf {
    suffixed(path, ".damaged")
}

/// Whether `path` or its backup exists, so a folder whose file is gone but whose backup is left still counts.
pub fn exists(path: &Path) -> bool {
    path.is_file() || backup_path(path).is_file()
}

/// Files [`load`] restored or gave up on since the last call, each reported once.
pub fn take_recovered() -> Vec<(PathBuf, Recovery)> {
    std::mem::take(&mut *RECOVERED.lock().unwrap_or_else(|e| e.into_inner()))
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    serde_json::from_slice(bytes).ok()
}

/// Reads `path` as `T`, `None` when neither it nor a backup exists. A missing, empty or damaged file is replaced by its
/// backup when that parses; when nothing parses, both are set aside and `None` is returned. Either case is reported once
/// through [`take_recovered`]. This thread's next [`write`] to the file expects it unchanged since.
pub fn load<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = read_raw(path)?;
    if let Some(value) = bytes.as_deref().and_then(parse::<T>) {
        remember(path, bytes.as_deref());
        return Ok(Some(value));
    }
    let backup = backup_path(path);
    if bytes.is_none() && !backup.exists() {
        remember(path, None);
        return Ok(None);
    }
    let _held = lock::exclusive_within(&lock_path(path), LOCK_WAIT)?;
    let bytes = read_raw(path)?;
    if let Some(value) = bytes.as_deref().and_then(parse::<T>) {
        remember(path, bytes.as_deref());
        return Ok(Some(value));
    }
    let saved = read_raw(&backup)?;
    if let Some(saved) = saved.as_deref()
        && let Some(value) = parse::<T>(saved)
    {
        if bytes.is_some() {
            set_aside(path, &damaged_path(path))?;
        }
        net::write_atomic(path, saved)?;
        remember(path, Some(saved));
        report(path, Recovery::Restored);
        return Ok(Some(value));
    }
    if bytes.is_some() {
        set_aside(path, &damaged_path(path))?;
    }
    if saved.is_some() {
        set_aside(&backup, &damaged_path(&backup))?;
    }
    remember(path, None);
    if bytes.is_some() || saved.is_some() {
        report(path, Recovery::Lost);
    }
    Ok(None)
}

fn set_aside(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).with_context(|| format!("cannot move {} aside", from.display()))
}

fn report(path: &Path, recovery: Recovery) {
    RECOVERED.lock().unwrap_or_else(|e| e.into_inner()).push((path.to_path_buf(), recovery));
}

fn read_raw(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn replace_keeping_backup<T: DeserializeOwned>(path: &Path, current: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
    if let Some(current) = current
        && current != bytes
        && parse::<T>(current).is_some()
    {
        net::write_atomic(&backup_path(path), current)?;
    }
    net::write_atomic(path, bytes)
}

/// Replaces `path` with `bytes` atomically while holding its lock. `check` sees the current bytes first and may refuse.
/// Fails when another window changed the file since this thread read it, so no save silently undoes another one.
/// The current file becomes the backup when it parses as `T`; a damaged one never replaces the backup.
pub fn write<T: DeserializeOwned>(path: &Path, bytes: &[u8], check: impl FnOnce(Option<&[u8]>) -> Result<()>) -> Result<()> {
    let _held = lock::exclusive_within(&lock_path(path), LOCK_WAIT)?;
    let current = read_raw(path)?;
    let expected = SEEN.with(|seen| seen.borrow().get(path).copied());
    if let Some(expected) = expected
        && expected != fingerprint(current.as_deref())
    {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        bail!("{name} was changed by another Manycraft since it was read; reload and try again");
    }
    check(current.as_deref())?;
    replace_keeping_backup::<T>(path, current.as_deref(), bytes)?;
    remember(path, Some(bytes));
    Ok(())
}

/// Replaces `path` with `bytes` atomically under its lock, without checking for other windows' saves, keeping the backup like [`write`].
pub fn replace<T: DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<()> {
    let _held = lock::exclusive_within(&lock_path(path), LOCK_WAIT)?;
    let current = read_raw(path)?;
    replace_keeping_backup::<T>(path, current.as_deref(), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Doc = Vec<u32>;

    fn accept(_: Option<&[u8]>) -> Result<()> {
        Ok(())
    }

    fn put(path: &Path, bytes: &[u8]) -> Result<()> {
        write::<Doc>(path, bytes, accept)
    }

    fn recovered(path: &Path) -> Vec<Recovery> {
        let mut all = RECOVERED.lock().unwrap();
        let (mine, rest): (Vec<_>, Vec<_>) = all.drain(..).partition(|(p, _)| p == path);
        *all = rest;
        mine.into_iter().map(|(_, r)| r).collect()
    }

    #[test]
    fn a_save_after_a_read_succeeds_and_can_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        assert_eq!(load::<Doc>(&path).unwrap(), None);
        put(&path, b"[1]").unwrap();
        put(&path, b"[2]").unwrap();
        assert_eq!(load::<Doc>(&path).unwrap(), Some(vec![2]));
    }

    #[test]
    fn a_save_over_another_windows_save_is_refused_and_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        put(&path, b"[1]").unwrap();
        load::<Doc>(&path).unwrap();
        let other = {
            let path = path.clone();
            std::thread::spawn(move || put(&path, b"[2]")).join().unwrap()
        };
        other.unwrap();
        let error = put(&path, b"[3]").unwrap_err().to_string();
        assert!(error.contains("changed by another Manycraft"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"[2]");
        load::<Doc>(&path).unwrap();
        put(&path, b"[4]").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"[4]");
    }

    #[test]
    fn a_file_that_appeared_after_the_read_counts_as_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        assert_eq!(load::<Doc>(&path).unwrap(), None);
        std::fs::write(&path, b"[9]").unwrap();
        assert!(put(&path, b"[1]").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"[9]");
    }

    #[test]
    fn a_refusing_check_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        std::fs::write(&path, b"[1]").unwrap();
        assert!(write::<Doc>(&path, b"[2]", |_| bail!("no")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"[1]");
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn each_save_keeps_the_previous_version_as_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        put(&path, b"[1]").unwrap();
        assert!(!backup_path(&path).exists());
        put(&path, b"[2]").unwrap();
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"[1]");
        put(&path, b"[2]").unwrap();
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"[1]");
        replace::<Doc>(&path, b"[3]").unwrap();
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"[2]");
    }

    #[test]
    fn a_damaged_file_is_restored_from_the_backup_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        put(&path, b"[1]").unwrap();
        put(&path, b"[1,2]").unwrap();
        std::fs::write(&path, b"[1,2").unwrap();
        assert_eq!(load::<Doc>(&path).unwrap(), Some(vec![1]));
        assert_eq!(std::fs::read(&path).unwrap(), b"[1]");
        assert_eq!(std::fs::read(damaged_path(&path)).unwrap(), b"[1,2");
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"[1]");
        assert_eq!(recovered(&path), vec![Recovery::Restored]);
        assert_eq!(load::<Doc>(&path).unwrap(), Some(vec![1]));
        assert!(recovered(&path).is_empty());
        put(&path, b"[5]").unwrap();
    }

    #[test]
    fn a_missing_or_empty_or_wrong_shaped_file_falls_back_to_the_backup() {
        for broken in [None, Some(&b""[..]), Some(&br#"{"a":1}"#[..])] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("a.json");
            std::fs::write(backup_path(&path), b"[7]").unwrap();
            if let Some(bytes) = broken {
                std::fs::write(&path, bytes).unwrap();
            }
            assert_eq!(load::<Doc>(&path).unwrap(), Some(vec![7]));
            assert_eq!(std::fs::read(&path).unwrap(), b"[7]");
            assert_eq!(recovered(&path), vec![Recovery::Restored]);
        }
    }

    #[test]
    fn when_both_are_damaged_nothing_loads_both_are_kept_and_it_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        std::fs::write(&path, b"[1,").unwrap();
        std::fs::write(backup_path(&path), b"").unwrap();
        assert_eq!(load::<Doc>(&path).unwrap(), None);
        assert_eq!(recovered(&path), vec![Recovery::Lost]);
        assert_eq!(std::fs::read(damaged_path(&path)).unwrap(), b"[1,");
        assert!(damaged_path(&backup_path(&path)).exists());
        assert!(!path.exists() && !backup_path(&path).exists());
        assert_eq!(load::<Doc>(&path).unwrap(), None);
        assert!(recovered(&path).is_empty());
        put(&path, b"[2]").unwrap();
        assert_eq!(load::<Doc>(&path).unwrap(), Some(vec![2]));
    }

    #[test]
    fn a_damaged_file_never_replaces_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        put(&path, b"[1]").unwrap();
        put(&path, b"[2]").unwrap();
        std::fs::write(&path, b"{oops").unwrap();
        SEEN.with(|seen| seen.borrow_mut().remove(&path));
        put(&path, b"[3]").unwrap();
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"[1]");
        std::fs::write(&path, br#"{"a":1}"#).unwrap();
        replace::<Doc>(&path, b"[4]").unwrap();
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"[1]");
        assert_eq!(load::<Doc>(&path).unwrap(), Some(vec![4]));
    }

    #[test]
    fn exists_counts_a_lone_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        assert!(!exists(&path));
        std::fs::write(backup_path(&path), b"[1]").unwrap();
        assert!(exists(&path));
    }

    #[test]
    fn writers_in_many_threads_never_tear_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..20 {
                        let text = format!("{{\"writer\":{n},\"round\":{i},\"pad\":\"{}\"}}", "x".repeat(4000));
                        while {
                            load::<serde_json::Value>(&path).unwrap();
                            write::<serde_json::Value>(&path, text.as_bytes(), accept).is_err()
                        } {}
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let last: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(last["round"], 19);
        assert_eq!(last["pad"].as_str().unwrap().len(), 4000);
        let backup: serde_json::Value = serde_json::from_slice(&std::fs::read(backup_path(&path)).unwrap()).unwrap();
        assert_eq!(backup["pad"].as_str().unwrap().len(), 4000);
    }

    const STRESS_CHILD: &str = "MANYCRAFT_STRESS_CHILD";
    const STRESS_ACCOUNTS: u128 = 20;
    const STRESS_BATCHES: usize = 5;

    fn stress_accounts(generation: u64) -> Vec<crate::core::account::Account> {
        (1..=STRESS_ACCOUNTS)
            .map(|n| crate::core::account::Account {
                id: uuid::Uuid::from_u128(n),
                name: format!("Alt{n}"),
                kind: crate::core::account::AccountKind::Offline,
                proxy: Some(format!("socks5://g{generation}@127.0.0.1:1080")),
                color: Some((n % 8) as u8),
            })
            .collect()
    }

    fn stress_child(home: &Path) -> ! {
        use crate::core::{account, batch, settings};
        let mut generation = 0u64;
        loop {
            generation += 1;
            account::load().unwrap();
            account::save(&stress_accounts(generation)).unwrap();
            let mut batches = batch::load_all().unwrap();
            for n in batches.len()..STRESS_BATCHES {
                let mut made = batch::Batch::new(&format!("Batch {n}"), "26.3");
                made.id = format!("b{n}");
                batches.push(made);
            }
            for b in &mut batches {
                b.join_commands = vec![format!("/gen {generation}"); 30];
                batch::save(b).unwrap();
            }
            let mut s = settings::active().clone();
            s.reconnect.max_attempts = (generation % 50) as u32 + 1;
            settings::set(s);
            settings::persist().unwrap();
            if generation == 1 {
                std::fs::write(home.join("ready"), b"").unwrap();
            }
        }
    }

    #[test]
    #[ignore = "stress: kills a child process 100 times during saves; run with --ignored"]
    fn hard_kill_during_saves_loses_nothing() {
        if let Some(home) = std::env::var_os(STRESS_CHILD) {
            stress_child(Path::new(&home));
        }
        let home = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 | 1;
        let mut restored = 0;
        for run in 0..100 {
            let ready = home.path().join("ready");
            let _ = std::fs::remove_file(&ready);
            let mut child = std::process::Command::new(&exe)
                .args(["--exact", "core::store::tests::hard_kill_during_saves_loses_nothing", "--ignored", "--nocapture", "--test-threads=1"])
                .env(STRESS_CHILD, home.path())
                .env("MANYCRAFT_HOME", home.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let until = std::time::Instant::now() + Duration::from_secs(60);
            while !ready.exists() {
                assert!(child.try_wait().unwrap().is_none(), "run {run}: the child ended before it was ready");
                assert!(std::time::Instant::now() < until, "run {run}: the child never got ready");
                std::thread::sleep(Duration::from_millis(5));
            }
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            std::thread::sleep(Duration::from_millis(seed % 60));
            child.kill().unwrap();
            child.wait().unwrap();

            let accounts = load::<Vec<crate::core::account::Account>>(&home.path().join("accounts.json")).unwrap().unwrap_or_else(|| panic!("run {run}: accounts lost"));
            assert_eq!(accounts.iter().map(|a| a.id).collect::<Vec<_>>(), (1..=STRESS_ACCOUNTS).map(uuid::Uuid::from_u128).collect::<Vec<_>>(), "run {run}");
            let generation = accounts[0].proxy.clone().unwrap();
            assert!(accounts.iter().all(|a| a.proxy.as_ref() == Some(&generation)), "run {run}: accounts torn");
            for n in 0..STRESS_BATCHES {
                let file = home.path().join("batches").join(format!("b{n}")).join("batch.json");
                assert!(exists(&file), "run {run}: batch b{n} missing");
                let b = load::<crate::core::batch::Batch>(&file).unwrap().unwrap_or_else(|| panic!("run {run}: batch b{n} lost"));
                assert_eq!(b.join_commands.len(), 30, "run {run}: batch b{n} torn");
            }
            load::<crate::core::settings::Settings>(&home.path().join("settings.json")).unwrap().unwrap_or_else(|| panic!("run {run}: settings lost"));
            for (path, recovery) in take_recovered() {
                assert_eq!(recovery, Recovery::Restored, "run {run}: {} lost", path.display());
                restored += 1;
            }
        }
        let files = walk(home.path());
        let damaged = files.iter().filter(|p| p.extension().is_some_and(|e| e == "damaged")).count();
        assert_eq!(damaged, 0, "{restored} files needed their backup");
        let torn = files.iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".tmp"))).count();
        println!("100 hard kills, nothing lost, {restored} restores, {torn} writes cut off mid-file");
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir).unwrap().filter_map(Result::ok).flat_map(|e| if e.path().is_dir() { walk(&e.path()) } else { vec![e.path()] }).collect()
    }
}
