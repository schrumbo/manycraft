//! Batches: groups of accounts that play one instance together on one server.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use super::fleet::server_name;
use super::{paths, store};

const FILE: &str = "batch.json";
const TRASH: &str = ".trash";
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

/// What the games of a batch answer when a server offers a resource pack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Packs {
    /// A hidden game holds the offer until the user answers it in the batch; a shown game asks in its own window.
    #[default]
    Ask,
    Accept,
    Decline,
}

impl Packs {
    pub const ALL: [Self; 3] = [Self::Ask, Self::Accept, Self::Decline];

    /// What a game in `mode` answers: a headless game has no window to ask in, so asking becomes accepting.
    pub fn for_mode(self, mode: RunMode) -> Self {
        if mode == RunMode::Connection && self == Self::Ask { Self::Accept } else { self }
    }

    fn is_ask(&self) -> bool {
        *self == Self::Ask
    }
}

/// Minutes between automatic screenshots a headless batch may choose.
pub const SCREENSHOT_MINUTES: [u32; 5] = [1, 5, 15, 30, 60];

/// How the games of a batch run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Real game windows, hidden or shown by the manager.
    #[default]
    Windows,
    /// The real game without any window; previewed by screenshots.
    Hidden,
    /// A small protocol client without the game: no window, no picture.
    Connection,
}

impl RunMode {
    fn is_windows(&self) -> bool {
        *self == Self::Windows
    }

    /// Whether the games run without any window.
    pub fn windowless(self) -> bool {
        self != Self::Windows
    }
}

/// The Minecraft version a headless batch speaks when it plays no instance.
pub const HEADLESS_VERSION: &str = "26.3";

/// Whether the headless mode is offered at all. The release flips it when the mode is not ready to ship.
pub const CONNECTION_ONLY_OFFERED: bool = true;

/// A server's code of conduct a batch's user accepted, by the hash of its text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conduct {
    pub server: String,
    pub hash: String,
}

/// How a batch runs unattended: how its games run, screenshots, and how it answers resource packs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unattended {
    pub mode: RunMode,
    pub screenshots: Option<u32>,
    pub packs: Packs,
}

/// A group of accounts that all play the same instance with the same run settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub id: String,
    pub name: String,
    /// The instance every account plays; empty in a file the migration has not reached yet.
    #[serde(default)]
    pub instance_id: String,
    /// The Minecraft version a headless batch speaks when it plays no instance; empty for a batch that plays one. Named apart
    /// from the `game_version` of the files before instances, so an older version does not mistake it for one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub headless_version: String,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Uuid>,
    /// Chat lines or commands every instance sends once after each successful join, in order.
    #[serde(default)]
    pub join_commands: Vec<String>,
    /// Pause before the first join command and between two of them.
    #[serde(default = "default_join_delay")]
    pub join_delay_ms: u32,
    /// What hidden instances may cost; a value from a newer version reads as the default.
    #[serde(default, deserialize_with = "lenient")]
    pub background: Background,
    /// Memory each game of the batch gets instead of the instance's.
    #[serde(default)]
    pub memory_override: Option<u32>,
    /// How many games of a start may load at once; 0 lets the machine's cores and free memory decide.
    #[serde(default)]
    pub launch_parallel: u32,
    /// Games keep the full compiler, so a played game runs as fast as a solo one at the cost of more memory per game.
    #[serde(default)]
    pub full_speed: bool,
    /// How the games run: with windows, as the real game without any, or headless, as a protocol client. A `headless: true` from an
    /// early build reads as the hidden game.
    #[serde(default, alias = "headless", deserialize_with = "lenient_mode", skip_serializing_if = "RunMode::is_windows")]
    pub mode: RunMode,
    /// The codes of conduct the user accepted for this batch, one per server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conduct: Vec<Conduct>,
    /// Minutes between automatic screenshots of a headless game; `None` takes them only when asked.
    #[serde(default, deserialize_with = "lenient_minutes", skip_serializing_if = "Option::is_none")]
    pub screenshots: Option<u32>,
    /// What the games answer when a server offers a resource pack; a value from a newer version reads as the default.
    #[serde(default, deserialize_with = "lenient_packs", skip_serializing_if = "Packs::is_ask")]
    pub packs: Packs,
}

fn lenient_mode<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<RunMode, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Bool(true) => RunMode::Hidden,
        other => serde_json::from_value::<RunMode>(other).unwrap_or_default(),
    })
}

fn lenient_minutes<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<u32>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value::<Option<u32>>(value).ok().flatten().filter(|m| SCREENSHOT_MINUTES.contains(m)))
}

fn lenient_packs<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Packs, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value::<Packs>(value).unwrap_or_default())
}

fn lenient<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Background, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value::<Background>(value).map(Background::clamped).unwrap_or_default())
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

    /// A new, empty batch playing `instance_id` with a fresh id.
    pub fn new(name: &str, instance_id: &str) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string(),
            name: name.to_owned(),
            instance_id: instance_id.to_owned(),
            headless_version: String::new(),
            server: None,
            accounts: Vec::new(),
            join_commands: Vec::new(),
            join_delay_ms: DEFAULT_JOIN_DELAY_MS,
            background: Background::Saving,
            memory_override: None,
            launch_parallel: 0,
            full_speed: false,
            mode: RunMode::Windows,
            conduct: Vec::new(),
            screenshots: None,
            packs: Packs::Ask,
        }
    }

    /// A new, empty headless batch playing no instance: it speaks [`HEADLESS_VERSION`].
    pub fn headless(name: &str) -> Self {
        Self { headless_version: HEADLESS_VERSION.to_owned(), mode: RunMode::Connection, packs: Packs::Accept, ..Self::new(name, "") }
    }

    /// The instance every account plays; `None` for a headless batch that plays none.
    pub fn instance(&self) -> Option<&str> {
        Some(self.instance_id.as_str()).filter(|id| !id.is_empty())
    }

    /// The Minecraft version its protocol client speaks.
    pub fn connection_version(&self) -> &str {
        if self.headless_version.is_empty() { HEADLESS_VERSION } else { &self.headless_version }
    }

    /// Plays `instance_id` from now on.
    pub fn play(&mut self, instance_id: &str) {
        self.instance_id = instance_id.to_owned();
        self.headless_version.clear();
    }

    /// Plays no instance from now on and speaks [`HEADLESS_VERSION`]; only a headless batch can.
    pub fn play_none(&mut self) {
        self.instance_id.clear();
        self.headless_version = HEADLESS_VERSION.to_owned();
    }

    fn playable(&self) -> bool {
        match self.instance() {
            Some(id) => super::instance::valid_id(id),
            None => self.mode == RunMode::Connection && !self.headless_version.is_empty(),
        }
    }

    /// Its unattended settings.
    pub fn unattended(&self) -> Unattended {
        Unattended { mode: self.mode, screenshots: self.screenshots.filter(|m| SCREENSHOT_MINUTES.contains(m)), packs: self.packs.for_mode(self.mode) }
    }

    /// Takes over `u`; an interval it does not offer reads as none.
    pub fn set_unattended(&mut self, u: Unattended) {
        self.mode = u.mode;
        self.screenshots = u.screenshots.filter(|m| SCREENSHOT_MINUTES.contains(m));
        self.packs = u.packs.for_mode(u.mode);
    }

    /// Whether the user accepted the code of conduct with `hash` on `server` for this batch.
    pub fn conduct_accepted(&self, server: &str, hash: &str) -> bool {
        let server = server_name(server);
        self.conduct.iter().any(|c| server_name(&c.server) == server && c.hash == hash)
    }

    /// The hashes of the codes of conduct accepted on `server`.
    pub fn conduct_hashes(&self, server: &str) -> Vec<String> {
        let server = server_name(server);
        self.conduct.iter().filter(|c| server_name(&c.server) == server).map(|c| c.hash.clone()).collect()
    }

    /// Keeps the acceptance of the code of conduct with `hash` on `server`, replacing an older text of that server.
    pub fn accept_conduct(&mut self, server: &str, hash: &str) {
        let server = server_name(server);
        self.conduct.retain(|c| server_name(&c.server) != server);
        self.conduct.push(Conduct { server, hash: hash.to_owned() });
    }

    /// The name its measured memory per game is kept under: games with the full compiler hold more, so each choice has a
    /// figure of its own.
    pub fn measure_key(&self) -> String {
        if self.full_speed { format!("{}+full", self.id) } else { self.id.clone() }
    }

    /// Folder holding `batch.json`.
    pub fn dir(&self) -> PathBuf {
        batches_dir().join(&self.id)
    }

    /// Memory each game gets: the batch's own value, else `instance_mb`.
    pub fn memory(&self, instance_mb: u32) -> u32 {
        self.memory_override.unwrap_or(instance_mb)
    }

    /// The join commands as they are sent: trimmed, without empty lines or lines longer than a chat message, at most 20.
    pub fn join_lines(&self) -> Vec<String> {
        self.join_commands.iter().map(|l| l.trim()).filter(|l| !l.is_empty() && l.chars().count() <= MAX_CHAT_CHARS).take(MAX_JOIN_LINES).map(str::to_owned).collect()
    }
}

/// Folder holding one subfolder per batch.
pub fn batches_dir() -> PathBuf {
    paths::data_dir().join("batches")
}

/// All saved batches, sorted by name. A damaged batch file falls back to its backup; one without a good backup is left out and
/// reported through `store::take_recovered`. One the migration has not reached yet is left out.
pub fn load_all() -> Result<Vec<Batch>> {
    load_all_in(&batches_dir())
}

/// Saves `batch` atomically under a lock, refusing to overwrite a change another Manycraft made since this thread loaded it.
pub fn save(batch: &Batch) -> Result<()> {
    save_in(&batches_dir(), batch)
}

/// Keeps the user's acceptance of a code of conduct on `server` in the saved batch `id`, as it is on disk now.
pub fn remember_conduct(id: &str, server: &str, hash: &str) -> Result<()> {
    let mut batch = load_all()?.into_iter().find(|b| b.id == id).context("the batch is gone")?;
    batch.accept_conduct(server, hash);
    save(&batch)
}

/// Deletes the batch's file. Its instance, mods, configs and worlds stay; anything else in its folder stays too.
pub fn delete(batch: &Batch) -> Result<()> {
    delete_in(&batches_dir(), batch)
}

/// Saves a deleted `batch` again as it was; leaves a batch of the same id that exists by now alone.
pub fn restore(batch: &Batch) -> Result<()> {
    restore_in(&batches_dir(), batch)
}

fn restore_in(root: &Path, batch: &Batch) -> Result<()> {
    if store::load::<Batch>(&root.join(&batch.id).join(FILE))?.is_some() {
        return Ok(());
    }
    save_in(root, batch)
}

pub(super) fn delete_in(root: &Path, batch: &Batch) -> Result<()> {
    if !super::instance::valid_id(&batch.id) {
        bail!("invalid batch id {:?}", batch.id);
    }
    let dir = root.join(&batch.id);
    for file in [store::backup_path(&dir.join(FILE)), dir.join(FILE)] {
        match std::fs::remove_file(file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("cannot delete {}", dir.display())),
            _ => {}
        }
    }
    let _ = std::fs::remove_file(dir.join(format!("{FILE}.lock")));
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

/// A batch in the Papierkorb.
#[derive(Clone, Debug)]
pub struct Trashed {
    pub batch: Batch,
    /// Its folder name in the Papierkorb.
    pub entry: String,
    /// Unix milliseconds when it went there.
    pub at: u64,
    /// Bytes on disk.
    pub size: u64,
}

/// Moves the batch's folder, with its file and the options its games share, to the Papierkorb; returns its entry there.
pub fn trash(batch: &Batch) -> Result<String> {
    trash_in(&batches_dir(), batch, super::instance::now_millis())
}

/// Puts the batch of Papierkorb entry `entry` back and returns it; refuses while a batch of its id exists.
pub fn untrash(entry: &str) -> Result<Batch> {
    untrash_in(&batches_dir(), entry)
}

/// The batches in the Papierkorb, newest first.
pub fn trashed() -> Result<Vec<Trashed>> {
    trashed_in(&batches_dir())
}

/// Deletes the batch of Papierkorb entry `entry` for good.
pub fn purge(entry: &str) -> Result<()> {
    purge_in(&batches_dir(), entry)
}

/// Deletes the batches in the Papierkorb older than `older_than` or stamped more than a day ahead of now.
pub fn purge_stale(older_than: std::time::Duration) -> Result<()> {
    purge_stale_in(&batches_dir(), super::instance::now_millis(), older_than)
}

fn entry_of(entry: &str) -> Result<&str> {
    super::instance::parse_entry(entry).map(|(id, _)| id).with_context(|| format!("invalid trash entry {entry:?}"))
}

pub(super) fn trash_in(root: &Path, batch: &Batch, millis: u64) -> Result<String> {
    if !super::instance::valid_id(&batch.id) {
        bail!("invalid batch id {:?}", batch.id);
    }
    let entry = format!("{}-{millis}", batch.id);
    let trash = root.join(TRASH);
    let (from, to) = (root.join(&batch.id), trash.join(&entry));
    if !store::exists(&from.join(FILE)) {
        bail!(super::instance::Final(super::i18n::tr("Diesen Batch gibt es nicht mehr.")));
    }
    if std::fs::symlink_metadata(&to).is_ok() {
        bail!("trash entry {entry} exists already");
    }
    std::fs::create_dir_all(&trash).with_context(|| format!("cannot create {}", trash.display()))?;
    let _ = std::fs::remove_file(from.join(format!("{FILE}.lock")));
    std::fs::rename(&from, &to).with_context(|| format!("cannot move {} to the trash", from.display()))?;
    let _ = store::load::<Batch>(&from.join(FILE));
    Ok(entry)
}

pub(super) fn untrash_in(root: &Path, entry: &str) -> Result<Batch> {
    let id = entry_of(entry)?;
    let from = root.join(TRASH).join(entry);
    let Some(batch) = store::load::<Batch>(&from.join(FILE))?.filter(|b| b.id == id) else {
        bail!(super::instance::Final(super::i18n::tr("Das liegt nicht mehr im Papierkorb.")));
    };
    let to = root.join(id);
    if store::exists(&to.join(FILE)) {
        bail!(super::instance::Final(super::i18n::tr("Diesen Batch gibt es schon wieder.")));
    }
    if std::fs::symlink_metadata(&to).is_ok() {
        std::fs::remove_dir(&to).with_context(|| format!("{} is in the way", to.display()))?;
    }
    std::fs::rename(&from, &to).with_context(|| format!("cannot move {} back", from.display()))?;
    Ok(store::load::<Batch>(&to.join(FILE)).ok().flatten().unwrap_or(batch))
}

pub(super) fn trashed_in(root: &Path) -> Result<Vec<Trashed>> {
    let trash = root.join(TRASH);
    let entries = match std::fs::read_dir(&trash) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", trash.display())),
    };
    let mut out = Vec::new();
    for e in entries.filter_map(Result::ok) {
        let Some(entry) = e.file_name().to_str().map(str::to_owned) else { continue };
        let Some((id, at)) = super::instance::parse_entry(&entry) else { continue };
        let path = e.path();
        let Ok(Some(batch)) = store::load::<Batch>(&path.join(FILE)) else { continue };
        if batch.id != id {
            continue;
        }
        out.push(Trashed { batch, at, size: super::worlds::size(&path), entry });
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.entry.cmp(&b.entry)));
    Ok(out)
}

pub(super) fn purge_in(root: &Path, entry: &str) -> Result<()> {
    entry_of(entry)?;
    super::instance::remove_dir(&root.join(TRASH).join(entry))
}

pub(super) fn purge_stale_in(root: &Path, now: u64, older_than: std::time::Duration) -> Result<()> {
    let trash = root.join(TRASH);
    let entries = match std::fs::read_dir(&trash) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", trash.display())),
    };
    let cutoff = now.saturating_sub(u64::try_from(older_than.as_millis()).unwrap_or(u64::MAX));
    let ahead = now.saturating_add(super::instance::FUTURE_MILLIS);
    let mut first = Ok(());
    for e in entries.filter_map(Result::ok) {
        let Some(at) = e.file_name().to_str().and_then(super::instance::parse_entry).map(|(_, at)| at) else { continue };
        if (at < cutoff || at > ahead)
            && let Err(err) = super::instance::remove_dir(&e.path())
            && first.is_ok()
        {
            first = Err(err);
        }
    }
    first
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
        if let Some(batch) = store::load::<Batch>(&file)?
            && (batch.instance().is_some() || !batch.headless_version.is_empty())
        {
            batches.push(batch);
        }
    }
    batches.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Ok(batches)
}

pub(super) fn save_in(root: &Path, batch: &Batch) -> Result<()> {
    if !super::instance::valid_id(&batch.id) {
        bail!("invalid batch id {:?}", batch.id);
    }
    if !batch.playable() {
        bail!("batch {} has no instance", batch.id);
    }
    store::write::<Batch>(&root.join(&batch.id).join(FILE), &serde_json::to_vec_pretty(batch)?, |_| Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_headless_batch_without_an_instance_round_trips_and_a_game_batch_needs_one() {
        let dir = tempfile::tempdir().unwrap();
        let b = Batch::headless("Farm");
        assert_eq!((b.instance(), b.connection_version()), (None, HEADLESS_VERSION));
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b.clone()]);
        let text = std::fs::read_to_string(dir.path().join(&b.id).join(FILE)).unwrap();
        assert!(text.contains("\"headless_version\": \"26.3\"") && !text.contains("game_version"));
        let windows = Batch { mode: RunMode::Windows, ..b.clone() };
        assert!(save_in(dir.path(), &windows).is_err());
        let mut played = windows;
        played.play("i1");
        save_in(dir.path(), &played).unwrap();
        assert!(!std::fs::read_to_string(dir.path().join(&b.id).join(FILE)).unwrap().contains("headless_version"));
    }

    #[test]
    fn a_batch_without_an_instance_that_is_not_headless_stays_out() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b1").join(FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, br#"{"id":"b1","name":"N"}"#).unwrap();
        assert!(load_all_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_deleted_batch_comes_back_as_it_was_and_never_over_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("A", "i1");
        b.accounts = vec![Uuid::from_u128(1)];
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b.clone()]);
        delete_in(dir.path(), &b).unwrap();
        restore_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b.clone()]);
        let mut newer = b.clone();
        newer.name = "B".into();
        save_in(dir.path(), &newer).unwrap();
        restore_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![newer]);
    }

    #[test]
    fn a_trashed_batch_keeps_its_folder_comes_back_whole_and_goes_after_a_week() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut b = Batch::new("Farm", "i1");
        b.accounts = vec![Uuid::from_u128(1)];
        save_in(root, &b).unwrap();
        std::fs::write(root.join(&b.id).join("options.txt"), b"soundCategory_master:0.0").unwrap();
        let entry = trash_in(root, &b, 1_000).unwrap();
        assert!(load_all_in(root).unwrap().is_empty(), "the Papierkorb is no batch");
        assert!(!root.join(&b.id).exists());
        let listed = trashed_in(root).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].entry == entry && listed[0].at == 1_000 && listed[0].batch == b && listed[0].size > 0);
        assert!(trash_in(root, &b, 2_000).is_err(), "gone already");
        assert_eq!(untrash_in(root, &entry).unwrap(), b);
        assert_eq!(load_all_in(root).unwrap(), vec![b.clone()]);
        assert_eq!(std::fs::read(root.join(&b.id).join("options.txt")).unwrap(), b"soundCategory_master:0.0");
        assert!(trashed_in(root).unwrap().is_empty());
        assert!(untrash_in(root, &entry).is_err());

        let entry = trash_in(root, &b, 3_000).unwrap();
        save_in(root, &Batch { name: "Neu".into(), ..b.clone() }).unwrap();
        assert!(untrash_in(root, &entry).is_err(), "never over a batch of the same id");
        assert_eq!(load_all_in(root).unwrap()[0].name, "Neu");
        assert_eq!(trashed_in(root).unwrap().len(), 1, "the refused one stays in the Papierkorb");

        let week = std::time::Duration::from_secs(7 * 24 * 60 * 60);
        purge_stale_in(root, 3_000 + 1_000, week).unwrap();
        assert_eq!(trashed_in(root).unwrap().len(), 1);
        purge_stale_in(root, 3_000 + 8 * 24 * 60 * 60 * 1000, week).unwrap();
        assert!(trashed_in(root).unwrap().is_empty());
        assert_eq!(load_all_in(root).unwrap().len(), 1, "purging leaves live batches alone");
    }

    #[test]
    fn purging_one_entry_refuses_a_foreign_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(purge_in(dir.path(), "..").is_err());
        assert!(purge_in(dir.path(), "../x-1").is_err());
        let b = Batch::new("A", "i1");
        save_in(dir.path(), &b).unwrap();
        let entry = trash_in(dir.path(), &b, 5).unwrap();
        purge_in(dir.path(), &entry).unwrap();
        assert!(trashed_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn rejoin_keeps_the_place_and_refuses_twice() {
        let mut b = Batch::new("A", "i1");
        b.accounts = vec![Uuid::from_u128(1), Uuid::from_u128(3)];
        assert!(b.rejoin(Uuid::from_u128(2), 1));
        assert!(!b.rejoin(Uuid::from_u128(2), 0));
        assert!(b.rejoin(Uuid::from_u128(4), 9));
        assert_eq!(b.accounts, (1..=4).map(Uuid::from_u128).collect::<Vec<_>>());
    }

    #[test]
    fn round_trip_and_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("zeta", "i1");
        b.accounts.push(Uuid::nil());
        let a = Batch::new("Alpha", "i1");
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
    fn a_deleted_batch_stays_deleted_with_or_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let once = Batch::new("Once", "i1");
        let twice = Batch::new("Twice", "i1");
        save_in(dir.path(), &once).unwrap();
        save_in(dir.path(), &twice).unwrap();
        save_in(dir.path(), &Batch { server: Some("example.org".into()), ..twice.clone() }).unwrap();
        assert!(store::backup_path(&dir.path().join(&twice.id).join(FILE)).exists());
        delete_in(dir.path(), &once).unwrap();
        delete_in(dir.path(), &twice).unwrap();
        assert!(!dir.path().join(&once.id).exists() && !dir.path().join(&twice.id).exists());
        assert!(load_all_in(dir.path()).unwrap().is_empty());
        delete_in(dir.path(), &once).unwrap();
    }

    #[test]
    fn damaged_or_missing_file_comes_back_from_the_previous_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("A", "i1");
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
        let batch: Batch = serde_json::from_str(r#"{"id":"a1","name":"A","instance_id":"i1"}"#).unwrap();
        assert!(batch.memory_override.is_none() && batch.accounts.is_empty() && batch.server.is_none());
        assert!(batch.join_commands.is_empty());
        assert_eq!(batch.join_delay_ms, DEFAULT_JOIN_DELAY_MS);
        assert_eq!(batch.background, Background::Saving);
        assert_eq!(batch.launch_parallel, 0);
        assert!(!batch.full_speed);
    }

    #[test]
    fn launch_parallel_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("a", "i1");
        b.launch_parallel = 4;
        b.full_speed = true;
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
    }

    #[test]
    fn each_compiler_choice_keeps_its_own_measured_memory() {
        let mut b = Batch::new("a", "i1");
        let lean = b.measure_key();
        assert_eq!(lean, b.id);
        b.full_speed = true;
        assert_ne!(b.measure_key(), lean);
    }

    #[test]
    fn background_round_trips_and_unknown_levels_read_as_saving() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("a", "i1");
        b.background = Background::Full;
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
        let json = |level: &str| format!(r#"{{"id":"a1","name":"A","instance_id":"i1","background":{level}}}"#);
        assert_eq!(serde_json::from_str::<Batch>(&json(r#""balanced""#)).unwrap().background, Background::Balanced);
        assert_eq!(serde_json::from_str::<Batch>(&json(r#""turbo""#)).unwrap().background, Background::Saving);
        assert_eq!(serde_json::from_str::<Batch>(&json("7")).unwrap().background, Background::Saving);
    }

    #[test]
    fn custom_background_round_trips_with_defaults_and_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("a", "i1");
        b.background = Background::Custom { fps: 10, sight: 6 };
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
        let json = |level: &str| format!(r#"{{"id":"a1","name":"A","instance_id":"i1","background":{level}}}"#);
        let read = |level: &str| serde_json::from_str::<Batch>(&json(level)).unwrap().background;
        assert_eq!(read(r#"{"custom":{"fps":10,"sight":6}}"#), Background::Custom { fps: 10, sight: 6 });
        assert_eq!(read(r#"{"custom":{}}"#), Background::Custom { fps: 5, sight: 4 });
        assert_eq!(read(r#"{"custom":{"fps":0,"sight":200}}"#), Background::Custom { fps: 1, sight: 32 });
        assert_eq!(read(r#"{"custom":{"fps":-3}}"#), Background::Saving);
        assert_eq!(read(r#"{"custom":"x"}"#), Background::Saving);
        assert_eq!(read(r#""custom""#), Background::Saving);
    }

    #[test]
    fn unattended_settings_round_trip_and_read_old_or_odd_files_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("a", "i1");
        b.set_unattended(Unattended { mode: RunMode::Hidden, screenshots: Some(15), packs: Packs::Decline });
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b.clone()]);
        assert_eq!(b.unattended(), Unattended { mode: RunMode::Hidden, screenshots: Some(15), packs: Packs::Decline });
        let read = |extra: &str| serde_json::from_str::<Batch>(&format!(r#"{{"id":"a1","name":"A","instance_id":"i1"{extra}}}"#)).unwrap().unattended();
        assert_eq!(read(""), Unattended::default(), "a batch from before headless runs with windows and asks");
        assert_eq!(read(r#","screenshots":7,"packs":"maybe""#), Unattended::default(), "an interval it does not offer and an unknown answer read as off and ask");
        assert_eq!(read(r#","headless":true,"screenshots":60,"packs":"accept""#), Unattended { mode: RunMode::Hidden, screenshots: Some(60), packs: Packs::Accept }, "an early build's headless reads as the hidden game");
        assert_eq!(read(r#","mode":"connection""#).mode, RunMode::Connection);
        assert_eq!(read(r#","mode":"warp""#).mode, RunMode::Windows, "a mode from a newer version reads as windows");
        assert_eq!(read(r#","headless":false"#).mode, RunMode::Windows);
        let mut odd = Batch::new("b", "i1");
        odd.set_unattended(Unattended { mode: RunMode::Windows, screenshots: Some(2), packs: Packs::Ask });
        assert_eq!(odd.screenshots, None);
        let json = serde_json::to_string(&odd).unwrap();
        assert!(!json.contains("mode") && !json.contains("conduct") && !json.contains("headless"), "defaults stay out of the file: {json}");
    }

    #[test]
    fn a_code_of_conduct_is_remembered_per_server_by_its_text() {
        let mut b = Batch::new("a", "i1");
        let (old, new) = ("aa11".to_owned(), "bb22".to_owned());
        assert!(!b.conduct_accepted("play.example.org", &old));
        b.accept_conduct("play.example.org", &old);
        assert!(b.conduct_accepted("PLAY.example.org", &old) && !b.conduct_accepted("play.example.org", &new) && !b.conduct_accepted("other.org", &old));
        b.accept_conduct("play.example.org", &new);
        assert_eq!(b.conduct.len(), 1, "a new text replaces the old one");
        let back: Batch = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert!(back.conduct_accepted("play.example.org", &new));
    }

    #[test]
    fn a_remembered_code_of_conduct_matches_the_server_with_or_without_the_default_port() {
        let mut b = Batch::new("a", "i1");
        b.accept_conduct("mc.example:25565", "aa11");
        assert_eq!(b.conduct_hashes("MC.example"), ["aa11"]);
        assert!(b.conduct_accepted("mc.example", "aa11") && !b.conduct_accepted("mc.example:25566", "aa11"));
        b.accept_conduct("mc.example", "bb22");
        assert_eq!(b.conduct.len(), 1);
    }

    #[test]
    fn accepted_servers_are_stored_by_their_one_name_and_old_entries_still_match() {
        let mut b = Batch::new("a", "i1");
        b.conduct = serde_json::from_str(r#"[{"server":"Mc.Example","hash":"old"},{"server":"[::1]:25570","hash":"six"}]"#).unwrap();
        assert!(b.conduct_accepted("mc.example:25565", "old") && b.conduct_accepted("MC.EXAMPLE", "old") && b.conduct_accepted("[::1]:25570", "six"));
        b.accept_conduct("MC.Example", "new");
        assert_eq!(b.conduct.iter().map(|c| (c.server.as_str(), c.hash.as_str())).collect::<Vec<_>>(), [("[::1]:25570", "six"), ("mc.example:25565", "new")]);
        assert_eq!(b.conduct_hashes("mc.example"), ["new"]);
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
        let mut b = Batch::new("a", "i1");
        b.join_commands = vec!["/login x".into(), "/server survival".into()];
        b.join_delay_ms = 500;
        save_in(dir.path(), &b).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
    }

    #[test]
    fn join_lines_are_trimmed_and_bounded() {
        let mut b = Batch::new("a", "i1");
        b.join_commands = vec!["  /login x ".into(), String::new(), "   ".into(), "y".repeat(257), "hello".into()];
        assert_eq!(b.join_lines(), vec!["/login x".to_owned(), "hello".to_owned()]);
        b.join_commands = (0..50).map(|n| format!("/c{n}")).collect();
        assert_eq!(b.join_lines().len(), MAX_JOIN_LINES);
    }

    #[test]
    fn unmigrated_batches_are_left_out_and_cannot_be_saved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("old")).unwrap();
        std::fs::write(dir.path().join("old").join(FILE), br#"{"id":"old","name":"Old","game_version":"26.3"}"#).unwrap();
        assert!(load_all_in(dir.path()).unwrap().is_empty());
        assert!(save_in(dir.path(), &Batch::new("x", "")).is_err());
        assert_eq!(Batch::new("x", "i1").memory(3000), 3000);
        assert_eq!(Batch { memory_override: Some(1024), ..Batch::new("x", "i1") }.memory(3000), 1024);
    }

    #[test]
    fn delete_keeps_anything_it_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let a = Batch::new("a", "i1");
        let b = Batch::new("b", "i1");
        save_in(dir.path(), &a).unwrap();
        save_in(dir.path(), &b).unwrap();
        std::fs::write(dir.path().join(&b.id).join("stray.txt"), b"x").unwrap();
        delete_in(dir.path(), &a).unwrap();
        delete_in(dir.path(), &b).unwrap();
        assert!(!dir.path().join(&a.id).exists());
        assert!(dir.path().join(&b.id).join("stray.txt").is_file());
        assert!(load_all_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn ids_that_could_escape_the_folder_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Batch::new("x", "i1");
        b.id = "../evil".into();
        assert!(save_in(dir.path(), &b).is_err());
    }

    #[test]
    fn a_headless_batch_never_asks_about_resource_packs() {
        let mut b = Batch::headless("h");
        assert_eq!(b.unattended().packs, Packs::Accept);
        b.packs = Packs::Ask;
        assert_eq!(b.unattended().packs, Packs::Accept);
        let mut u = b.unattended();
        u.packs = Packs::Ask;
        b.set_unattended(u);
        assert_eq!(b.packs, Packs::Accept);
        let mut g = Batch::new("g", "i1");
        let mut u = g.unattended();
        u.mode = RunMode::Windows;
        u.packs = Packs::Ask;
        g.set_unattended(u);
        assert_eq!(g.packs, Packs::Ask);
        u.packs = Packs::Decline;
        u.mode = RunMode::Connection;
        g.set_unattended(u);
        assert_eq!(g.packs, Packs::Decline);
    }
}
