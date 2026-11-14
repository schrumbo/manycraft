//! Instances: one game version, one loader, one mod list and one config, played alone or by any number of batches.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::batch::ModRef;
use super::{paths, store};

/// File holding an instance in its folder.
pub const FILE: &str = "instance.json";
/// Memory a new instance gets.
pub const DEFAULT_MEMORY_MB: u32 = 2048;
const GAMES: &str = "games";
const COPIED: [&str; 6] = ["mods", "config", "resourcepacks", "shaderpacks", "options.txt", "servers.dat"];

/// The mod loader of an instance, with its pinned version; `None` follows the newest one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Loader {
    Vanilla,
    Fabric(Option<String>),
    Quilt(Option<String>),
    Forge(Option<String>),
    #[serde(rename = "neoforge")]
    NeoForge(Option<String>),
}

impl Default for Loader {
    fn default() -> Self {
        Self::Fabric(None)
    }
}

/// The loader kinds in the order the create sheet offers them.
pub const LOADERS: [Loader; 5] = [Loader::Vanilla, Loader::Fabric(None), Loader::Quilt(None), Loader::Forge(None), Loader::NeoForge(None)];

impl Loader {
    /// The loader's name as people know it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Vanilla => "Vanilla",
            Self::Fabric(_) => "Fabric",
            Self::Quilt(_) => "Quilt",
            Self::Forge(_) => "Forge",
            Self::NeoForge(_) => "NeoForge",
        }
    }

    /// The pinned loader version; always `None` for Vanilla.
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Vanilla => None,
            Self::Fabric(v) | Self::Quilt(v) | Self::Forge(v) | Self::NeoForge(v) => v.as_deref(),
        }
    }

    /// The same kind pinned to `version`.
    pub fn with_version(&self, version: Option<String>) -> Self {
        match self {
            Self::Vanilla => Self::Vanilla,
            Self::Fabric(_) => Self::Fabric(version),
            Self::Quilt(_) => Self::Quilt(version),
            Self::Forge(_) => Self::Forge(version),
            Self::NeoForge(_) => Self::NeoForge(version),
        }
    }

    /// Whether both are the same loader, whatever their versions.
    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// Whether the loader runs mods at all.
    pub fn takes_mods(&self) -> bool {
        !matches!(self, Self::Vanilla)
    }

    /// Whether Manycraft's companion mod exists for this loader, which previews, background saving and chat need.
    pub fn has_companion(&self) -> bool {
        matches!(self, Self::Fabric(_))
    }

    /// Whether Manycraft can install and start this loader yet.
    pub fn launchable(&self) -> bool {
        matches!(self, Self::Fabric(_))
    }
}

/// What a game is played with: version, loader, mods and memory. Its folder holds the shared `mods/`, `config/`,
/// `resourcepacks/` and `shaderpacks/`, the `options.txt` and `servers.dat` templates, and one game folder per account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub game_version: String,
    #[serde(default)]
    pub loader: Loader,
    #[serde(default)]
    pub mods: Vec<ModRef>,
    /// File names in the shared mods folder that are not on Modrinth; syncing never deletes them.
    #[serde(default)]
    pub local_mods: Vec<String>,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    /// The account that last played this instance alone.
    #[serde(default)]
    pub last_account: Option<Uuid>,
}

fn default_memory() -> u32 {
    DEFAULT_MEMORY_MB
}

impl Instance {
    /// A new instance for `game_version` with `loader`, a fresh id and no mods.
    pub fn new(name: &str, game_version: &str, loader: Loader) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string(),
            name: name.to_owned(),
            icon: None,
            game_version: game_version.to_owned(),
            loader,
            mods: Vec::new(),
            local_mods: Vec::new(),
            memory_mb: DEFAULT_MEMORY_MB,
            jvm_args: Vec::new(),
            last_account: None,
        }
    }

    /// Folder holding `instance.json`, the shared folders, the templates and the game folders.
    pub fn dir(&self) -> PathBuf {
        instances_dir().join(&self.id)
    }

    /// Shared mod folder every game of the instance links to.
    pub fn mods_dir(&self) -> PathBuf {
        self.dir().join("mods")
    }

    /// Shared config folder every game of the instance links to.
    pub fn config_dir(&self) -> PathBuf {
        self.dir().join("config")
    }

    /// Shared resource pack folder every game of the instance links to.
    pub fn resourcepacks_dir(&self) -> PathBuf {
        self.dir().join("resourcepacks")
    }

    /// Shared shader pack folder every game of the instance links to.
    pub fn shaderpacks_dir(&self) -> PathBuf {
        self.dir().join("shaderpacks")
    }

    /// Game options every game starts from; a game that saves its options updates the keys it changed.
    pub fn options_template(&self) -> PathBuf {
        self.dir().join("options.txt")
    }

    /// Server list every game starts from; a game that saves its list replaces it.
    pub fn servers_template(&self) -> PathBuf {
        self.dir().join("servers.dat")
    }

    /// Folder holding one game folder per account.
    pub fn games_dir(&self) -> PathBuf {
        games_dir(&self.dir())
    }

    /// Game folder of `account` in this instance, shared by every batch that plays it and by solo play.
    pub fn game_dir(&self, account: Uuid) -> PathBuf {
        self.games_dir().join(account.simple().to_string())
    }

    /// Game folders that exist, other than the one of `account`.
    pub fn other_games(&self, account: Uuid) -> Vec<PathBuf> {
        let own = account.simple().to_string();
        let Ok(entries) = std::fs::read_dir(self.games_dir()) else { return Vec::new() };
        entries.filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy() != own && e.path().is_dir()).map(|e| e.path()).collect()
    }

    /// "Minecraft 26.3 · Fabric 0.16.5", the version line of the instance page.
    pub fn version_line(&self) -> String {
        match (&self.loader, self.loader.version()) {
            (Loader::Vanilla, _) => format!("Minecraft {}", self.game_version),
            (loader, Some(v)) => format!("Minecraft {} · {} {v}", self.game_version, loader.name()),
            (loader, None) => format!("Minecraft {} · {}", self.game_version, loader.name()),
        }
    }
}

/// `games/` inside the instance folder `dir`.
pub fn games_dir(dir: &Path) -> PathBuf {
    dir.join(GAMES)
}

/// Folder holding one subfolder per instance.
pub fn instances_dir() -> PathBuf {
    paths::instances_dir()
}

/// All saved instances, sorted by name. An instance whose file cannot be read is an error, never skipped silently.
pub fn load_all() -> Result<Vec<Instance>> {
    load_all_in(&instances_dir())
}

/// Saves `instance` atomically under a lock, refusing to overwrite a change another Manycraft made since this thread loaded it.
pub fn save(instance: &Instance) -> Result<()> {
    save_in(&instances_dir(), instance)
}

/// A name for a new instance from `wanted` that no instance in `taken` has: "Farm", "Farm 2", ….
pub fn free_name(wanted: &str, taken: &[String]) -> String {
    let wanted = wanted.trim();
    let base = if wanted.is_empty() { "Minecraft" } else { wanted };
    let used = |n: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if !used(base) {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base} {n}")).find(|n| !used(n)).unwrap_or_else(|| base.to_owned())
}

pub(super) fn load_all_in(root: &Path) -> Result<Vec<Instance>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let mut instances = Vec::new();
    for entry in entries {
        let file = entry?.path().join(FILE);
        if !file.is_file() {
            continue;
        }
        let bytes = store::read(&file)?.with_context(|| format!("cannot read {}", file.display()))?;
        let instance: Instance = serde_json::from_slice(&bytes).with_context(|| format!("{} is damaged", file.display()))?;
        instances.push(instance);
    }
    instances.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Ok(instances)
}

pub(super) fn save_in(root: &Path, instance: &Instance) -> Result<()> {
    if !valid_id(&instance.id) {
        bail!("invalid instance id {:?}", instance.id);
    }
    store::write(&root.join(&instance.id).join(FILE), &serde_json::to_vec_pretty(instance)?, |_| Ok(()))
}

/// Whether `id` can name a folder without escaping it.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Deletes the instance with its mods, configs, worlds and game folders. Callers check first that no batch uses
/// it and no game of it runs.
pub fn delete(instance: &Instance) -> Result<()> {
    delete_in(&instances_dir(), instance)
}

pub(super) fn delete_in(root: &Path, instance: &Instance) -> Result<()> {
    if !valid_id(&instance.id) {
        bail!("invalid instance id {:?}", instance.id);
    }
    let dir = root.join(&instance.id);
    match std::fs::remove_file(dir.join(FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("cannot delete {}", dir.display())),
        _ => {}
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))?;
    }
    Ok(())
}

/// A copy of `source` named `name` with a fresh id: its settings, mods, configs, packs and templates. Game folders
/// and worlds stay with `source`; links are not followed. A copy that fails halfway is removed again.
pub fn clone(source: &Instance, name: &str) -> Result<Instance> {
    clone_in(&instances_dir(), source, name)
}

pub(super) fn clone_in(root: &Path, source: &Instance, name: &str) -> Result<Instance> {
    if !valid_id(&source.id) {
        bail!("invalid instance id {:?}", source.id);
    }
    let made = Instance { id: Uuid::new_v4().simple().to_string(), name: name.to_owned(), last_account: None, ..source.clone() };
    let (from, to) = (root.join(&source.id), root.join(&made.id));
    let copied = (|| -> Result<()> {
        std::fs::create_dir_all(&to).with_context(|| format!("cannot create {}", to.display()))?;
        for item in COPIED {
            copy_tree(&from.join(item), &to.join(item))?;
        }
        save_in(root, &made)
    })();
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&to);
        return Err(e);
    }
    Ok(made)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(from) else { return Ok(()) };
    if meta.is_dir() {
        std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
        for entry in std::fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else if meta.is_file() {
        std::fs::copy(from, to).with_context(|| format!("cannot copy {}", from.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Instance::new("zeta", "26.3", Loader::Fabric(Some("0.16.5".into())));
        b.jvm_args = vec!["-XX:+UseG1GC".into()];
        b.last_account = Some(Uuid::from_u128(7));
        let a = Instance::new("Alpha", "26.3", Loader::Vanilla);
        save_in(dir.path(), &b).unwrap();
        save_in(dir.path(), &a).unwrap();
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![a, b]);
    }

    #[test]
    fn loaders_serialize_compactly_and_old_files_get_defaults() {
        let json = |l: &Loader| serde_json::to_string(l).unwrap();
        assert_eq!(json(&Loader::Vanilla), r#""vanilla""#);
        assert_eq!(json(&Loader::Fabric(Some("0.16.5".into()))), r#"{"fabric":"0.16.5"}"#);
        assert_eq!(json(&Loader::NeoForge(None)), r#"{"neoforge":null}"#);
        for l in LOADERS {
            assert_eq!(serde_json::from_str::<Loader>(&json(&l)).unwrap(), l);
        }
        let i: Instance = serde_json::from_str(r#"{"id":"a1","name":"A","game_version":"26.3"}"#).unwrap();
        assert_eq!(i.loader, Loader::Fabric(None));
        assert_eq!(i.memory_mb, DEFAULT_MEMORY_MB);
        assert!(i.mods.is_empty() && i.local_mods.is_empty() && i.jvm_args.is_empty() && i.icon.is_none() && i.last_account.is_none());
    }

    #[test]
    fn loader_kinds_versions_and_lines() {
        let f = Loader::Fabric(Some("0.16".into()));
        assert_eq!(f.version(), Some("0.16"));
        assert!(f.same_kind(&Loader::Fabric(None)) && !f.same_kind(&Loader::Quilt(None)));
        assert_eq!(Loader::Vanilla.with_version(Some("1".into())), Loader::Vanilla);
        assert_eq!(Loader::Forge(None).with_version(Some("47".into())), Loader::Forge(Some("47".into())));
        assert!(f.launchable() && f.has_companion() && f.takes_mods());
        assert!(!Loader::Quilt(None).launchable() && !Loader::Vanilla.takes_mods());
        let mut i = Instance::new("A", "26.3", f);
        assert_eq!(i.version_line(), "Minecraft 26.3 · Fabric 0.16");
        i.loader = Loader::Fabric(None);
        assert_eq!(i.version_line(), "Minecraft 26.3 · Fabric");
        i.loader = Loader::Vanilla;
        assert_eq!(i.version_line(), "Minecraft 26.3");
    }

    #[test]
    fn free_names_count_up_ignoring_case() {
        let taken = vec!["Farm".to_owned(), "farm 2".to_owned()];
        assert_eq!(free_name("Farm", &taken), "Farm 3");
        assert_eq!(free_name(" Spawn ", &taken), "Spawn");
        assert_eq!(free_name("", &[]), "Minecraft");
    }

    #[test]
    fn ids_that_could_escape_the_folder_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut i = Instance::new("x", "26.3", Loader::Vanilla);
        i.id = "../evil".into();
        assert!(save_in(dir.path(), &i).is_err());
        assert!(delete_in(dir.path(), &i).is_err());
    }

    #[test]
    fn delete_removes_only_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let a = Instance::new("a", "26.3", Loader::Vanilla);
        let b = Instance::new("b", "26.3", Loader::Vanilla);
        save_in(dir.path(), &a).unwrap();
        save_in(dir.path(), &b).unwrap();
        std::fs::create_dir_all(dir.path().join(&a.id).join("games").join("x").join("saves")).unwrap();
        delete_in(dir.path(), &a).unwrap();
        assert!(!dir.path().join(&a.id).exists());
        assert_eq!(load_all_in(dir.path()).unwrap(), vec![b]);
    }

    #[test]
    fn a_clone_copies_settings_and_shared_files_but_no_games() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = Instance::new("Farm", "26.3", Loader::Fabric(Some("0.16.5".into())));
        a.memory_mb = 3072;
        a.last_account = Some(Uuid::from_u128(1));
        save_in(dir.path(), &a).unwrap();
        let src = dir.path().join(&a.id);
        std::fs::create_dir_all(src.join("mods")).unwrap();
        std::fs::create_dir_all(src.join("config").join("deep")).unwrap();
        std::fs::write(src.join("mods").join("a.jar"), b"jar").unwrap();
        std::fs::write(src.join("config").join("deep").join("c.toml"), b"c").unwrap();
        std::fs::write(src.join("options.txt"), b"fov:1
").unwrap();
        std::fs::create_dir_all(src.join("games").join("x").join("saves")).unwrap();
        let b = clone_in(dir.path(), &a, "Farm 2").unwrap();
        let dst = dir.path().join(&b.id);
        assert_ne!(a.id, b.id);
        assert_eq!((b.name.as_str(), b.memory_mb, &b.loader, b.last_account), ("Farm 2", 3072, &a.loader, None));
        assert_eq!(std::fs::read(dst.join("mods").join("a.jar")).unwrap(), b"jar");
        assert_eq!(std::fs::read(dst.join("config").join("deep").join("c.toml")).unwrap(), b"c");
        assert_eq!(std::fs::read(dst.join("options.txt")).unwrap(), b"fov:1
");
        assert!(!dst.join("games").exists());
        assert!(src.join("games").join("x").join("saves").is_dir());
        assert_eq!(load_all_in(dir.path()).unwrap().len(), 2);
    }
}
