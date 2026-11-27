//! Per-account event history: the last connects, drops, kicks and crashes of one instance, kept in its game folder.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::instance::Instance;
use super::net;

/// Entries kept per instance; older ones fall off.
pub const KEPT: usize = 50;
const FILE: &str = "manycraft-history.json";
const MAX_FILE: u64 = 256 * 1024;
const MAX_REASON: usize = 300;

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Connected,
    Disconnected,
    Kicked,
    Banned,
    Crashed,
    StartFailed,
    /// A connection-only account died.
    Died,
    /// A connection-only account respawned on its own.
    Respawned,
}

/// One event with its time in seconds since 1970 and, when there is one, the reason the server or the game gave.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at: u64,
    pub kind: Kind,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub reason: String,
}

impl Entry {
    /// An event that happens now; the reason is cut to a sane length.
    pub fn now(kind: Kind, server: Option<String>, reason: &str) -> Self {
        Self::at(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()), kind, server, reason)
    }

    /// An event at `at` seconds since 1970; the reason is cut to a sane length.
    pub fn at(at: u64, kind: Kind, server: Option<String>, reason: &str) -> Self {
        Self { at, kind, server, reason: reason.chars().take(MAX_REASON).collect() }
    }
}

/// The newest [`KEPT`] events of an instance, oldest first.
pub type Log = VecDeque<Entry>;

/// Adds `entry` to `log`, dropping the oldest events beyond [`KEPT`].
pub fn push(log: &mut Log, entry: Entry) {
    log.push_back(entry);
    while log.len() > KEPT {
        log.pop_front();
    }
}

/// File holding the history of the game of `account` in `instance`.
pub fn path(instance: &Instance, account: Uuid) -> PathBuf {
    instance.game_dir(account).join(FILE)
}

/// Reads a history file. An absent file is an empty history, a damaged or oversized one too: it only records, so it never blocks anything.
pub fn load(path: &Path) -> Log {
    read(path).unwrap_or_default()
}

fn read(path: &Path) -> Result<Log> {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len == 0 || len > MAX_FILE {
        return Ok(Log::new());
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut log: Log = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)?.into_iter().filter_map(|v| serde_json::from_value::<Entry>(v).ok()).collect();
    while log.len() > KEPT {
        log.pop_front();
    }
    Ok(log)
}

fn write(path: &Path, log: &Log) -> Result<()> {
    net::write_atomic(path, &serde_json::to_vec(log)?)
}

/// Writes histories on a thread of its own, so the UI thread never waits for the disk. Writes queued for one file collapse into the newest.
#[derive(Clone)]
pub struct Writer {
    tx: mpsc::Sender<(PathBuf, Log)>,
}

impl Writer {
    /// Starts the writer thread; it ends when the last `Writer` is dropped.
    pub fn start() -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<(PathBuf, Log)>();
        std::thread::Builder::new().name("history".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut pending: Vec<(PathBuf, Log)> = vec![first];
                pending.extend(rx.try_iter());
                let mut written: Vec<&PathBuf> = Vec::new();
                for (path, log) in pending.iter().rev() {
                    if !written.contains(&path) {
                        written.push(path);
                        let _ = write(path, log);
                    }
                }
            }
        })?;
        Ok(Self { tx })
    }

    /// Queues `log` to be saved at `path`.
    pub fn save(&self, path: PathBuf, log: Log) {
        let _ = self.tx.send((path, log));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_entry_of_a_kind_from_a_newer_version_is_skipped_and_the_rest_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(&path, br#"[{"at":1,"kind":"banned","reason":"x"},{"at":2,"kind":"teleported"},{"at":3,"kind":"connected"}]"#).unwrap();
        let log = super::load(&path);
        assert_eq!(log.iter().map(|e| e.at).collect::<Vec<_>>(), [1, 3]);
    }

    use super::*;

    fn entry(n: u64) -> Entry {
        Entry { at: n, kind: Kind::Disconnected, server: Some("s".into()), reason: format!("r{n}") }
    }

    #[test]
    fn only_the_newest_entries_are_kept() {
        let mut log = Log::new();
        for n in 0..(KEPT as u64 + 10) {
            push(&mut log, entry(n));
        }
        assert_eq!(log.len(), KEPT);
        assert_eq!(log.front().unwrap().at, 10);
        assert_eq!(log.back().unwrap().at, KEPT as u64 + 9);
    }

    #[test]
    fn round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x").join(FILE);
        let mut log = Log::new();
        push(&mut log, entry(1));
        push(&mut log, Entry::now(Kind::Banned, None, "You are banned"));
        write(&path, &log).unwrap();
        assert_eq!(load(&path), log);
    }

    #[test]
    fn missing_damaged_and_oversized_files_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert!(load(&path).is_empty());
        std::fs::write(&path, b"[{").unwrap();
        assert!(load(&path).is_empty());
        std::fs::write(&path, vec![b' '; MAX_FILE as usize + 1]).unwrap();
        assert!(load(&path).is_empty());
    }

    #[test]
    fn a_long_file_is_cut_to_the_newest_entries_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let all: Vec<Entry> = (0..80).map(entry).collect();
        std::fs::write(&path, serde_json::to_vec(&all).unwrap()).unwrap();
        let log = load(&path);
        assert_eq!((log.len(), log.front().unwrap().at), (KEPT, 30));
    }

    #[test]
    fn reasons_are_cut() {
        assert_eq!(Entry::now(Kind::Kicked, None, &"x".repeat(1000)).reason.chars().count(), MAX_REASON);
    }

    #[test]
    fn the_writer_saves_off_thread_and_keeps_the_newest_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let writer = Writer::start().unwrap();
        for n in 0..20 {
            let mut log = Log::new();
            push(&mut log, entry(n));
            writer.save(path.clone(), log);
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while load(&path).back().is_none_or(|e| e.at != 19) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(load(&path).back().unwrap().at, 19);
    }
}
