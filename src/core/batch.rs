//! Batches: groups of instances that share one game version, one mod list and one config.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use super::{paths, store};

const FILE: &str = "batch.json";
const DEFAULT_MEMORY_MB: u32 = 2048;
const DEFAULT_JOIN_DELAY_MS: u32 = 2000;
const MAX_JOIN_LINES: usize = 20;
/// Longest line a chat message can hold.
pub const MAX_CHAT_CHARS: usize = 256;

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

/// Frames per second a custom background level allows.
pub const BACKGROUND_FPS: std::ops::RangeInclusive<u8> = 1..=60;
/// Sight in chunks a custom background level allows.
pub const BACKGROUND_SIGHT: std::ops::RangeInclusive<u8> = 2..=32;
const SAVING_FPS: u8 = 3;
const SAVING_SIGHT: u8 = 2;
const BALANCED_FPS: u8 = 30;
const GAME_SIGHT: u8 = 12;

/// How much a hidden instance may cost. Every level mutes the game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Background {
    /// A few frames a second, short sight, no particles or clouds.
    #[default]
    Saving,
    /// At most 30 frames a second and few particles; sight stays.
    Balanced,
    /// Nothing reduced.
    Full,
    /// At most these frames per second and this sight in chunks, never above the player's own; particles and clouds as in `Saving`.
    Custom {
        #[serde(default = "default_fps")]
        fps: u8,
        #[serde(default = "default_sight")]
        sight: u8,
    },
}

fn default_fps() -> u8 {
    5
}

fn default_sight() -> u8 {
    4
}

impl Background {
    /// The presets, cheapest first; `Custom` comes after them.
    pub const PRESETS: [Self; 3] = [Self::Saving, Self::Balanced, Self::Full];

    /// The custom level starting from the frames and sight this level allows.
    pub fn to_custom(self) -> Self {
        let (fps, sight) = match self {
            Self::Saving => (SAVING_FPS, SAVING_SIGHT),
            Self::Balanced => (BALANCED_FPS, GAME_SIGHT),
            Self::Full => (*BACKGROUND_FPS.end(), GAME_SIGHT),
            Self::Custom { fps, sight } => (fps, sight),
        };
        Self::Custom { fps, sight }.clamped()
    }

    /// The same level with custom values pulled into their ranges.
    pub fn clamped(self) -> Self {
        match self {
            Self::Custom { fps, sight } => Self::Custom {
                fps: fps.clamp(*BACKGROUND_FPS.start(), *BACKGROUND_FPS.end()),
                sight: sight.clamp(*BACKGROUND_SIGHT.start(), *BACKGROUND_SIGHT.end()),
            },
            level => level,
        }
    }
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
    /// File names in the shared mods folder that are not on Modrinth; syncing never deletes them.
    #[serde(default)]
    pub local_mods: Vec<String>,
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
    /// What hidden instances may cost; a value from a newer version reads as the default.
    #[serde(default, deserialize_with = "lenient")]
    pub background: Background,
}

fn lenient<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Background, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value::<Background>(value).map(Background::clamped).unwrap_or_default())
}

fn default_memory() -> u32 {
    DEFAULT_MEMORY_MB
}

fn default_join_delay() -> u32 {
    DEFAULT_JOIN_DELAY_MS
}

impl Batch {
    /// Puts account `id` back at `index` (or last); false when it already belongs to the batch.
    pub fn rejoin(&mut self, id: Uuid, index: usize) -> bool {
        if self.accounts.contains(&id) {
            return false;
        }
        self.accounts.insert(index.min(self.accounts.len()), id);
        true
    }

    /// A new, empty batch for `game_version` with a fresh id.
    pub fn new(name: &str, game_version: &str) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string(),
            name: name.to_owned(),
            game_version: game_version.to_owned(),
            loader_version: None,
            mods: Vec::new(),
            local_mods: Vec::new(),
            server: None,
            memory_mb: DEFAULT_MEMORY_MB,
            accounts: Vec::new(),
            jvm_args: Vec::new(),
            join_commands: Vec::new(),
            join_delay_ms: DEFAULT_JOIN_DELAY_MS,
            background: Background::Saving,
        }
    }

    /// Folder holding `batch.json`, the shared `mods/`, `config/`, `resourcepacks/` and `shaderpacks/`, and the
    /// `options.txt` and `servers.dat` templates.
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

    /// Shared resource pack folder every instance of the batch links to.
    pub fn resourcepacks_dir(&self) -> PathBuf {
        self.dir().join("resourcepacks")
    }

    /// Shared shader pack folder every instance of the batch links to.
    pub fn shaderpacks_dir(&self) -> PathBuf {
        self.dir().join("shaderpacks")
    }

    /// Game options every instance starts from; a game that saves its options updates the keys it changed.
    pub fn options_template(&self) -> PathBuf {
        self.dir().join("options.txt")
    }

    /// Server list every instance starts from; a game that saves its list replaces it.
    pub fn servers_template(&self) -> PathBuf {
        self.dir().join("servers.dat")
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

/// All saved batches, sorted by name. A damaged batch file falls back to its backup; one without a good backup is left out and reported through `store::take_recovered`.
pub fn load_all() -> Result<Vec<Batch>> {
    load_all_in(&batches_dir())
}

/// Saves `batch` atomically under a lock, refusing to overwrite a change another Manycraft made since this thread loaded it.
pub fn save(batch: &Batch) -> Result<()> {
    save_in(&batches_dir(), batch)
}

/// Deletes the batch's folder including its shared mods and config. Use `fleet::delete_batch` to keep those.
pub fn delete(batch: &Batch) -> Result<()> {
    let dir = batch.dir();
    for file in [store::backup_path(&dir.join(FILE)), dir.join(FILE)] {
        match std::fs::remove_file(file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("cannot delete {}", dir.display())),
            _ => {}
        }
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))?;
    }
    Ok(())
}

pub(super) fn load_all_in(root: &Path) -> Result<Vec<Batch>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let mut batches = Vec::new();
    for entry in entries {
        let file = entry?.path().join(FILE);
        if !store::exists(&file) {
            continue;
        }
        if let Some(batch) = store::load::<Batch>(&file)? {
            batches.push(batch);
        }
    }
    batches.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Ok(batches)
}

pub(super) fn save_in(root: &Path, batch: &Batch) -> Result<()> {
    if batch.id.is_empty() || !batch.id.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("invalid batch id {:?}", batch.id);
    }
    store::write::<Batch>(&root.join(&batch.id).join(FILE), &serde_json::to_vec_pretty(batch)?, |_| Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejoin_keeps_the_place_and_refuses_twice() {
        let mut b = Batch::new("A", "26.3");
        b.accounts = vec![Uuid::from_u128(1), Uuid::from_u128(3)];
        assert!(b.rejoin(Uuid::from_u128(2), 1));
        assert!(!b.rejoin(Uuid::from_u128(2), 0));
        assert!(b.rejoin(Uuid::from_u128(4), 9));
        assert_eq!(b.accounts, (1..=4).map(Uuid::from_u128).collect::<Vec<_>>());
    }

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
    fn missing_folder_is_empty_and_damaged_file_without_backup_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_all_in(&dir.path().join("none")).unwrap().is_empty());
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("x").join(FILE), b"{").unwrap();
        assert!(load_all_in(dir.path()).unwrap().is_empty());
        assert!(store::damaged_path(&dir.path().join("x").join(FILE)).exists());
    }

    #[test]
    fn damaged_or_missing_file_comes_back_from_the_previous_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("A", "26.3");
        save_in(dir.path(), &b).unwrap();
        let first = b.clone();
        b.server = Some("example.org".into());
        save_in(dir.path(), &b).unwrap();
        let file = dir.path().join(&b.id).join(FILE);
        std::fs::write(&file, b"").unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![first.clone()]);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![first]);
    }

    #[test]
    fn old_files_get_defaults() {
        let batch: Batch = serde_json::from_str(r#"{"id":"a1","name":"A","game_version":"26.3"}"#).unwrap();
        assert_eq!(batch.memory_mb, DEFAULT_MEMORY_MB);
        assert!(batch.mods.is_empty() && batch.accounts.is_empty() && batch.server.is_none());
        assert!(batch.join_commands.is_empty());
        assert_eq!(batch.join_delay_ms, DEFAULT_JOIN_DELAY_MS);
        assert_eq!(batch.background, Background::Saving);
    }

    #[test]
    fn background_round_trips_and_unknown_levels_read_as_saving() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("a", "26.3");
        b.background = Background::Full;
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
        let json = |level: &str| format!(r#"{{"id":"a1","name":"A","game_version":"26.3","background":{level}}}"#);
        assert_eq!(serde_json::from_str::<Batch>(&json(r#""balanced""#)).unwrap().background, Background::Balanced);
        assert_eq!(serde_json::from_str::<Batch>(&json(r#""turbo""#)).unwrap().background, Background::Saving);
        assert_eq!(serde_json::from_str::<Batch>(&json("7")).unwrap().background, Background::Saving);
    }

    #[test]
    fn custom_background_round_trips_with_defaults_and_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("a", "26.3");
        b.background = Background::Custom { fps: 10, sight: 6 };
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
        let json = |level: &str| format!(r#"{{"id":"a1","name":"A","game_version":"26.3","background":{level}}}"#);
        let read = |level: &str| serde_json::from_str::<Batch>(&json(level)).unwrap().background;
        assert_eq!(read(r#"{"custom":{"fps":10,"sight":6}}"#), Background::Custom { fps: 10, sight: 6 });
        assert_eq!(read(r#"{"custom":{}}"#), Background::Custom { fps: 5, sight: 4 });
        assert_eq!(read(r#"{"custom":{"fps":0,"sight":200}}"#), Background::Custom { fps: 1, sight: 32 });
        assert_eq!(read(r#"{"custom":{"fps":-3}}"#), Background::Saving);
        assert_eq!(read(r#"{"custom":"x"}"#), Background::Saving);
        assert_eq!(read(r#""custom""#), Background::Saving);
    }

    #[test]
    fn custom_starts_from_the_previous_preset() {
        assert_eq!(Background::Saving.to_custom(), Background::Custom { fps: 3, sight: 2 });
        assert_eq!(Background::Balanced.to_custom(), Background::Custom { fps: 30, sight: 12 });
        assert_eq!(Background::Full.to_custom(), Background::Custom { fps: 60, sight: 12 });
        assert_eq!(Background::Custom { fps: 7, sight: 9 }.to_custom(), Background::Custom { fps: 7, sight: 9 });
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
