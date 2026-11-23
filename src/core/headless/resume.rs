//! The connections to bring back after the restart that installs an update. A game outlives the manager and is found again
//! through the launch record; a connection lives inside the manager and ends with it, so the restart writes down which
//! ones were up and the next start reconnects them, the way any start of those accounts does.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::{net, paths};

const FILE: &str = "resume.json";
/// How long after the restart the connections are still wanted back; a longer wait means the update never came up.
const FRESH: Duration = Duration::from_secs(600);

#[derive(Serialize, Deserialize)]
struct Record {
    at: u64,
    keys: Vec<String>,
}

/// Writes down the connections `keys` for the next start; none removes an older note.
pub fn remember(keys: &[String]) {
    write(&paths::data_dir().join(FILE), now(), keys);
}

/// The connections the last manager wrote down, once: the note is gone after reading, and a stale one reads as none.
pub fn take() -> Vec<String> {
    read(&paths::data_dir().join(FILE), now())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn write(path: &Path, at: u64, keys: &[String]) {
    if keys.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let bytes = serde_json::to_vec(&Record { at, keys: keys.to_vec() }).map_err(anyhow::Error::from);
    if let Err(e) = bytes.and_then(|b| net::write_atomic(path, &b)) {
        eprintln!("resume note: {e:#}");
    }
}

fn read(path: &Path, at: u64) -> Vec<String> {
    let record = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok());
    let _ = std::fs::remove_file(path);
    record.filter(|r| at.saturating_sub(r.at) <= FRESH.as_secs()).map(|r| r.keys).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_read_once_and_only_while_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let keys = vec!["a".to_owned(), "b".to_owned()];
        write(&path, 1000, &keys);
        assert_eq!(read(&path, 1060), keys);
        assert!(read(&path, 1060).is_empty(), "taken once");
        write(&path, 1000, &keys);
        assert!(read(&path, 1000 + FRESH.as_secs() + 1).is_empty(), "an update that never came up");
        assert!(!path.exists(), "a stale note is gone too");
    }

    #[test]
    fn nothing_to_resume_removes_the_note_and_a_broken_one_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        write(&path, 1, &["a".to_owned()]);
        write(&path, 2, &[]);
        assert!(!path.exists());
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(read(&path, 1).is_empty());
    }
}
