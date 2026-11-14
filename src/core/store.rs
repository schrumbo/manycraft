//! Writing the app's JSON files under a lock, refusing to overwrite what another window saved since this thread last read it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::{lock, net};

const LOCK_WAIT: Duration = Duration::from_secs(15);

thread_local! {
    static SEEN: RefCell<HashMap<PathBuf, Option<u64>>> = RefCell::default();
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

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(std::ffi::OsString::from).unwrap_or_default();
    name.push(".lock");
    path.with_file_name(name)
}

/// The bytes of `path`, `None` when it does not exist. This thread's next [`write`] to the file expects it unchanged since.
pub fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    let bytes = read_raw(path)?;
    remember(path, bytes.as_deref());
    Ok(bytes)
}

fn read_raw(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Replaces `path` with `bytes` atomically while holding its lock. `check` sees the current bytes first and may refuse.
/// Fails when another window changed the file since this thread read it, so no save silently undoes another one.
pub fn write(path: &Path, bytes: &[u8], check: impl FnOnce(Option<&[u8]>) -> Result<()>) -> Result<()> {
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
    net::write_atomic(path, bytes)?;
    remember(path, Some(bytes));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(_: Option<&[u8]>) -> Result<()> {
        Ok(())
    }

    #[test]
    fn a_save_after_a_read_succeeds_and_can_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        assert_eq!(read(&path).unwrap(), None);
        write(&path, b"1", accept).unwrap();
        write(&path, b"2", accept).unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some(&b"2"[..]));
    }

    #[test]
    fn a_save_over_another_windows_save_is_refused_and_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write(&path, b"mine", accept).unwrap();
        read(&path).unwrap();
        let other = {
            let path = path.clone();
            std::thread::spawn(move || write(&path, b"theirs", accept)).join().unwrap()
        };
        other.unwrap();
        let error = write(&path, b"stale", accept).unwrap_err().to_string();
        assert!(error.contains("changed by another Manycraft"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
        read(&path).unwrap();
        write(&path, b"fresh", accept).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"fresh");
    }

    #[test]
    fn a_file_that_appeared_after_the_read_counts_as_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        assert_eq!(read(&path).unwrap(), None);
        std::fs::write(&path, b"theirs").unwrap();
        assert!(write(&path, b"mine", accept).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
    }

    #[test]
    fn a_refusing_check_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        std::fs::write(&path, b"keep").unwrap();
        assert!(write(&path, b"new", |_| bail!("no")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
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
                            read(&path).unwrap();
                            write(&path, text.as_bytes(), accept).is_err()
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
    }
}
