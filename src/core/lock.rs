//! Advisory file locks, so two Manycraft windows never write the same file or folder at once.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const POLL: Duration = Duration::from_millis(50);

/// A held lock; it is released when this is dropped.
#[derive(Debug)]
pub struct Guard {
    _file: File,
}

fn open(path: &Path) -> Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    OpenOptions::new().create(true).truncate(false).write(true).open(path).with_context(|| format!("cannot open {}", path.display()))
}

/// Waits until the exclusive lock on `path` (created when missing) is free and takes it.
pub fn exclusive(path: &Path) -> Result<Guard> {
    let file = open(path)?;
    file.lock().with_context(|| format!("cannot lock {}", path.display()))?;
    Ok(Guard { _file: file })
}

/// Like [`exclusive`], but gives up with an error after `timeout`.
pub fn exclusive_within(path: &Path, timeout: Duration) -> Result<Guard> {
    let until = Instant::now() + timeout;
    loop {
        if let Some(guard) = try_exclusive(path)? {
            return Ok(guard);
        }
        if Instant::now() >= until {
            bail!("{} is in use by another Manycraft", path.display());
        }
        std::thread::sleep(POLL);
    }
}

/// Takes the exclusive lock on `path` when it is free right now.
pub fn try_exclusive(path: &Path) -> Result<Option<Guard>> {
    let file = open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Guard { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
}

/// Takes a shared lock on `path`, which any number of holders may have but no exclusive one can, when no exclusive lock is held right now.
pub fn try_shared(path: &Path) -> Result<Option<Guard>> {
    let file = open(path)?;
    match file.try_lock_shared() {
        Ok(()) => Ok(Some(Guard { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_exclusive_lock_waits_for_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let first = exclusive(&path).unwrap();
        assert!(try_exclusive(&path).unwrap().is_none());
        assert!(exclusive_within(&path, Duration::from_millis(120)).is_err());
        drop(first);
        assert!(try_exclusive(&path).unwrap().is_some());
    }

    #[test]
    fn exclusive_within_takes_a_free_lock_and_waits_for_a_busy_one_until_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let held = exclusive_within(&path, Duration::from_secs(1)).unwrap();
        assert!(try_exclusive(&path).unwrap().is_none());
        let start = Instant::now();
        assert!(exclusive_within(&path, Duration::from_millis(300)).is_err());
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(300), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        assert!(exclusive_within(&path, Duration::from_secs(10)).is_ok());
        release.join().unwrap();
    }

    #[test]
    fn shared_locks_coexist_and_keep_exclusive_ones_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let a = try_shared(&path).unwrap().unwrap();
        let b = try_shared(&path).unwrap().unwrap();
        assert!(try_exclusive(&path).unwrap().is_none());
        drop((a, b));
        assert!(try_exclusive(&path).unwrap().is_some());
    }

    #[test]
    fn a_blocked_thread_gets_the_lock_when_it_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("x.lock");
        let first = exclusive(&path).unwrap();
        let waiting = {
            let path = path.clone();
            std::thread::spawn(move || exclusive(&path).map(|_| ()))
        };
        std::thread::sleep(Duration::from_millis(150));
        assert!(!waiting.is_finished());
        drop(first);
        waiting.join().unwrap().unwrap();
    }
}
