//! Batches: groups of instances that share one game version, one mod list and one config.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{net, paths};

const FILE: &str = "batch.json";
const DEFAULT_MEMORY_MB: u32 = 2048;
const DEFAULT_JOIN_DELAY_MS: u32 = 2000;
const MAX_JOIN_LINES: usize = 20;
const MAX_CHAT_CHARS: usize = 256;

/// One mod pinned to an exact file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModRef {
    pub project_id: String,
    pub version_id: String,
    pub title: String,
    pub file_name: String,
    pub url: String,
    pub sha1: String,
}

/// A group of instances: every account in it runs the same version, mods and config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub id: String,
    pub name: String,
    pub game_version: String,
    #[serde(default)]
    pub loader_version: Option<String>,
    #[serde(default)]
    pub mods: Vec<ModRef>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default)]
    pub accounts: Vec<Uuid>,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    /// Chat lines or commands every instance sends once after each successful join, in order.
    #[serde(default)]
    pub join_commands: Vec<String>,
    /// Pause before the first join command and between two of them.
    #[serde(default = "default_join_delay")]
    pub join_delay_ms: u32,
}

fn default_memory() -> u32 {
    DEFAULT_MEMORY_MB
}

fn default_join_delay() -> u32 {
    DEFAULT_JOIN_DELAY_MS
}

impl Batch {
    /// A new, empty batch for `game_version` with a fresh id.
    pub fn new(name: &str, game_version: &str) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string(),
            name: name.to_owned(),
            game_version: game_version.to_owned(),
            loader_version: None,
            mods: Vec::new(),
            server: None,
            memory_mb: DEFAULT_MEMORY_MB,
            accounts: Vec::new(),
            jvm_args: Vec::new(),
            join_commands: Vec::new(),
            join_delay_ms: DEFAULT_JOIN_DELAY_MS,
        }
    }

    /// Folder holding `batch.json`, the shared `mods/` and `config/` and the `options.txt` template.
    pub fn dir(&self) -> PathBuf {
        batches_dir().join(&self.id)
    }

    /// Shared mod folder every instance of the batch links to.
    pub fn mods_dir(&self) -> PathBuf {
        self.dir().join("mods")
    }

    /// Shared config folder every instance of the batch links to.
    pub fn config_dir(&self) -> PathBuf {
        self.dir().join("config")
    }

    /// Game options every instance starts from.
    pub fn options_template(&self) -> PathBuf {
        self.dir().join("options.txt")
    }

    /// The join commands as they are sent: trimmed, without empty lines or lines longer than a chat message, at most 20.
    pub fn join_lines(&self) -> Vec<String> {
        self.join_commands.iter().map(|l| l.trim()).filter(|l| !l.is_empty() && l.chars().count() <= MAX_CHAT_CHARS).take(MAX_JOIN_LINES).map(str::to_owned).collect()
    }

    /// Game folder of `account` in this batch.
    pub fn instance_dir(&self, account: Uuid) -> PathBuf {
        paths::instances_dir().join(&self.id).join(account.simple().to_string())
    }
}

/// Folder holding one subfolder per batch.
pub fn batches_dir() -> PathBuf {
    paths::data_dir().join("batches")
}

/// All saved batches, sorted by name. A batch whose file cannot be read is an error, never skipped silently.
pub fn load_all() -> Result<Vec<Batch>> {
    load_all_in(&batches_dir())
}

/// Saves `batch` atomically.
pub fn save(batch: &Batch) -> Result<()> {
    save_in(&batches_dir(), batch)
}

/// Deletes the batch's folder including its shared mods and config. Use `fleet::delete_batch` to keep those.
pub fn delete(batch: &Batch) -> Result<()> {
    let dir = batch.dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))?;
    }
    Ok(())
}

fn load_all_in(root: &Path) -> Result<Vec<Batch>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let mut batches = Vec::new();
    for entry in entries {
        let file = entry?.path().join(FILE);
        if !file.is_file() {
            continue;
        }
        let bytes = std::fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?;
        let batch: Batch = serde_json::from_slice(&bytes).with_context(|| format!("{} is damaged", file.display()))?;
        batches.push(batch);
    }
    batches.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Ok(batches)
}

fn save_in(root: &Path, batch: &Batch) -> Result<()> {
    if batch.id.is_empty() || !batch.id.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("invalid batch id {:?}", batch.id);
    }
    net::write_atomic(&root.join(&batch.id).join(FILE), &serde_json::to_vec_pretty(batch)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("zeta", "26.3");
        b.accounts.push(Uuid::nil());
        let a = Batch::new("Alpha", "26.3");
        save_in(dir.path(), &b).unwrap();
        save_in(dir.path(), &a).unwrap();
        let loaded = load_all_in(dir.path()).unwrap();
        assert_eq!(loaded, vec![a, b]);
    }

    #[test]
    fn missing_folder_is_empty_and_damaged_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_all_in(&dir.path().join("none")).unwrap().is_empty());
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("x").join(FILE), b"{").unwrap();
        assert!(load_all_in(dir.path()).is_err());
    }

    #[test]
    fn old_files_get_defaults() {
        let batch: Batch = serde_json::from_str(r#"{"id":"a1","name":"A","game_version":"26.3"}"#).unwrap();
        assert_eq!(batch.memory_mb, DEFAULT_MEMORY_MB);
        assert!(batch.mods.is_empty() && batch.accounts.is_empty() && batch.server.is_none());
        assert!(batch.join_commands.is_empty());
        assert_eq!(batch.join_delay_ms, DEFAULT_JOIN_DELAY_MS);
    }

    #[test]
    fn join_commands_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("a", "26.3");
        b.join_commands = vec!["/login x".into(), "/server survival".into()];
        b.join_delay_ms = 500;
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
    }

    #[test]
    fn join_lines_are_trimmed_and_bounded() {
        let mut b = Batch::new("a", "26.3");
        b.join_commands = vec!["  /login x ".into(), String::new(), "   ".into(), "y".repeat(257), "hello".into()];
        assert_eq!(b.join_lines(), vec!["/login x".to_owned(), "hello".to_owned()]);
        b.join_commands = (0..50).map(|n| format!("/c{n}")).collect();
        assert_eq!(b.join_lines().len(), MAX_JOIN_LINES);
    }

    #[test]
    fn ids_that_could_escape_the_folder_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("x", "26.3");
        b.id = "../evil".into();
        assert!(save_in(dir.path(), &b).is_err());
    }
}
