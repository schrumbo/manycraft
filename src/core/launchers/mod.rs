//! Import from other launchers: Prism, MultiMC, Modrinth App, CurseForge, ATLauncher, GDLauncher and the vanilla launcher.
//! Their folders are only read, never moved or changed, and no account, token or sign-in is ever taken from them.

mod read;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

use super::batch::ModRef;
use super::instance::{self as made, Loader};
use super::prism::{self, Jar};
use super::{fleet, modrinth, mrpack};

/// Folder inside an instance that holds imported worlds until the first game of the instance moves them into its game folder.
pub const SAVES_SEED: &str = "saves";
const USAGE: &str = "usage: manycraft launchers list [<folder>]
       manycraft launchers import <launcher:id> [--worlds] [--name <name>]";

/// The launchers an import can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Launcher {
    Prism,
    MultiMc,
    Modrinth,
    CurseForge,
    AtLauncher,
    GdLauncher,
    Vanilla,
}

impl Launcher {
    /// The launcher's name as people know it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Prism => "Prism Launcher",
            Self::MultiMc => "MultiMC",
            Self::Modrinth => "Modrinth App",
            Self::CurseForge => "CurseForge",
            Self::AtLauncher => "ATLauncher",
            Self::GdLauncher => "GDLauncher",
            Self::Vanilla => "Minecraft Launcher",
        }
    }

    /// Short lowercase key used on the command line and in ids.
    pub fn key(self) -> &'static str {
        match self {
            Self::Prism => "prism",
            Self::MultiMc => "multimc",
            Self::Modrinth => "modrinth",
            Self::CurseForge => "curseforge",
            Self::AtLauncher => "atlauncher",
            Self::GdLauncher => "gdlauncher",
            Self::Vanilla => "vanilla",
        }
    }

    /// The launcher whose key is `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::Prism, Self::MultiMc, Self::Modrinth, Self::CurseForge, Self::AtLauncher, Self::GdLauncher, Self::Vanilla].into_iter().find(|l| l.key() == key)
    }
}

/// Why an instance cannot be imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    NoVersion,
    Loader(String),
    Unreadable,
}

/// One instance of another launcher, read from its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub launcher: Launcher,
    /// Folder name, profile key or database path inside the launcher.
    pub id: String,
    pub name: String,
    /// The game folder holding mods, config, options and saves.
    pub game_dir: PathBuf,
    pub game_version: Option<String>,
    pub loader: Loader,
    /// Enabled mods: `.jar` files directly in the mods folder.
    pub mods: usize,
    pub unsupported: Option<Unsupported>,
}

impl Found {
    /// `launcher:id`, unique among what one scan returns.
    pub fn key(&self) -> String {
        format!("{}:{}", self.launcher.key(), self.id)
    }

    /// Whether the instance can be imported.
    pub fn importable(&self) -> bool {
        self.unsupported.is_none()
    }

    /// "Minecraft 1.21.1 · Fabric 0.16.5", or why it cannot be imported.
    pub fn version_line(&self) -> String {
        let Some(game) = &self.game_version else { return String::new() };
        made::Instance::new("", game, self.loader.clone()).version_line()
    }
}

/// A launcher found on this machine with the instances in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    pub launcher: Launcher,
    pub root: PathBuf,
    pub instances: Vec<Found>,
}

/// What an import takes along besides the mods, which are always matched and copied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Take {
    /// `options.txt`, resource packs and shader packs. The server list stays behind.
    pub settings: bool,
    /// The worlds in `saves`.
    pub worlds: bool,
}

/// The launchers on this machine: `MANYCRAFT_LAUNCHERS` (folders separated like `PATH`) when set, otherwise the standard
/// places of each OS. Launchers without an instance are left out.
pub fn scan() -> Vec<Library> {
    let roots = match std::env::var_os("MANYCRAFT_LAUNCHERS").filter(|v| !v.is_empty()) {
        Some(value) => std::env::split_paths(&value).collect(),
        None => read::default_roots(dirs::data_dir(), dirs::config_dir(), dirs::home_dir()),
    };
    scan_roots(&roots)
}

/// The launchers in `folder`: a launcher's data folder, its instances folder or one instance.
pub fn scan_folder(folder: &Path) -> Vec<Library> {
    scan_roots(&[folder.to_path_buf()])
}

fn scan_roots(roots: &[PathBuf]) -> Vec<Library> {
    let mut seen = HashSet::new();
    let mut all: Vec<Library> = Vec::new();
    for root in roots {
        let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if !seen.insert(canon) || !root.is_dir() {
            continue;
        }
        for lib in read::libraries(root) {
            match all.iter_mut().find(|l| l.launcher == lib.launcher && l.root == lib.root) {
                Some(have) => have.instances.extend(lib.instances),
                None => all.push(lib),
            }
        }
    }
    for lib in &mut all {
        lib.instances.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
        lib.instances.dedup_by(|a, b| a.key() == b.key());
    }
    all.retain(|l| !l.instances.is_empty());
    all
}

/// Copies `found` into a new instance named after it with a free name: same game and loader version, its mods, config
/// and, as asked, settings and worlds. Mods found on Modrinth by SHA-1 become pinned `ModRef`s; the others stay local
/// jars. Saved before it returns; the other launcher's files are untouched.
pub fn import(found: &Found, name: &str, take: Take, cancel: &AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<made::Instance> {
    let taken: Vec<String> = made::load_all()?.into_iter().map(|b| b.name).collect();
    let root = made::instances_dir();
    let name = mrpack::free_name(name, &taken);
    let made = import_into(found, &name, take, &root, &modrinth::match_hashes, cancel, progress)?;
    Ok(mrpack::claim_name(&root, made))
}

type Lookup<'a> = &'a dyn Fn(&[String]) -> Result<HashMap<String, ModRef>>;

pub(super) struct Spec<'a> {
    pub name: &'a str,
    pub label: &'a str,
    pub game_version: Option<&'a str>,
    pub loader: Loader,
    pub game_dir: &'a Path,
    pub take: Take,
}

pub(super) fn import_into(found: &Found, name: &str, take: Take, root: &Path, lookup: Lookup, cancel: &AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<made::Instance> {
    if let Some(why) = &found.unsupported {
        bail!("{} cannot be imported: {why:?}", found.name);
    }
    let spec = Spec { name, label: &found.name, game_version: found.game_version.as_deref(), loader: found.loader.clone(), game_dir: &found.game_dir, take };
    copy_instance(&spec, root, lookup, cancel, progress)
}

pub(super) fn copy_instance(spec: &Spec, root: &Path, lookup: Lookup, cancel: &AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<made::Instance> {
    let name = spec.name.trim();
    if name.is_empty() {
        bail!("an instance needs a name");
    }
    let game = spec.game_version.with_context(|| format!("{} has no Minecraft version", spec.label))?;
    let check = || if cancel.load(Ordering::SeqCst) { bail!(mrpack::Refused::Cancelled) } else { Ok(()) };
    let names = prism::jar_names(&spec.game_dir.join("mods"))?;
    let steps = names.len() * 2 + 4;
    let mut done = 0;
    let mut step = || {
        done += 1;
        progress(done, steps);
    };
    let mut jars = Vec::with_capacity(names.len());
    for (jar_name, path) in names {
        check()?;
        let sha1 = super::net::file_sha1(&path).with_context(|| format!("cannot read {}", path.display()))?;
        jars.push(Jar { path, name: jar_name, sha1 });
        step();
    }
    check()?;
    let hashes: Vec<String> = jars.iter().map(|j| j.sha1.clone()).collect();
    let matched = lookup(&hashes).context("cannot look the mods up on Modrinth")?;
    step();

    let mut batch = made::Instance::new(name, game, spec.loader.clone());
    let mut copies: Vec<(&Jar, String)> = Vec::new();
    let mut taken = HashSet::new();
    let mut seen = HashSet::new();
    for jar in &jars {
        if jar.name.eq_ignore_ascii_case(fleet::COMPANION_FILE) || !seen.insert(jar.sha1.clone()) {
            continue;
        }
        let dest = match matched.get(&jar.sha1) {
            Some(found) => {
                batch.mods.push(found.clone());
                found.file_name.clone()
            }
            None => {
                batch.local_mods.push(jar.name.clone());
                jar.name.clone()
            }
        };
        modrinth::safe_file_name(&dest)?;
        if !taken.insert(dest.to_lowercase()) {
            bail!("two mods would both be called {dest}");
        }
        copies.push((jar, dest));
    }

    let dest = root.join(&batch.id);
    let copied = copy_files(spec, &dest, &copies, &check, &mut step).and_then(|()| made::save_in(root, &batch));
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(batch)
}

fn copy_files(spec: &Spec, dest: &Path, copies: &[(&Jar, String)], check: &dyn Fn() -> Result<()>, step: &mut dyn FnMut()) -> Result<()> {
    let mods = dest.join("mods");
    std::fs::create_dir_all(&mods).with_context(|| format!("cannot create {}", mods.display()))?;
    for (jar, name) in copies {
        check()?;
        prism::copy_file(&jar.path, &mods.join(name))?;
        step();
    }
    check()?;
    copy_folder(&spec.game_dir.join("config"), &dest.join("config"), check)?;
    step();
    if spec.take.settings {
        let options = spec.game_dir.join(prism::OPTIONS_FILE);
        if options.is_file() {
            prism::copy_file(&options, &dest.join(prism::OPTIONS_FILE))?;
        }
        for folder in ["resourcepacks", "shaderpacks"] {
            check()?;
            copy_folder(&spec.game_dir.join(folder), &dest.join(folder), check)?;
        }
    }
    step();
    if spec.take.worlds {
        check()?;
        let saves = spec.game_dir.join("saves");
        if saves.is_dir() {
            prism::copy_tree(&saves, &dest.join(SAVES_SEED), 0, check)?;
        }
    }
    step();
    Ok(())
}

fn copy_folder(from: &Path, to: &Path, check: &dyn Fn() -> Result<()>) -> Result<()> {
    if from.is_dir() {
        prism::copy_tree(from, to, 0, check)
    } else {
        std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))
    }
}

/// `manycraft launchers`: lists and imports instances of other launchers from the terminal.
pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["list"] => list(scan()),
        ["list", folder] => list(scan_folder(Path::new(folder))),
        ["import", key, flags @ ..] => import_cli(key, flags),
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn list(libraries: Vec<Library>) -> Result<i32> {
    if libraries.is_empty() {
        println!("no launcher instances found");
    }
    for lib in &libraries {
        println!("{}  {}", lib.launcher.name(), lib.root.display());
        for i in &lib.instances {
            match &i.unsupported {
                Some(why) => println!("  {}  {}  unsupported: {why:?}", i.key(), i.name),
                None => println!("  {}  {}  {}  {} mods", i.key(), i.name, i.version_line(), i.mods),
            }
        }
    }
    Ok(0)
}

fn import_cli(key: &str, flags: &[&str]) -> Result<i32> {
    let mut take = Take { settings: true, worlds: false };
    let mut name = None;
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        match *flag {
            "--worlds" => take.worlds = true,
            "--name" => name = rest.next().map(|n| (*n).to_owned()),
            _ => {
                eprintln!("{USAGE}");
                return Ok(2);
            }
        }
    }
    let found = scan().into_iter().flat_map(|l| l.instances).find(|i| i.key().eq_ignore_ascii_case(key)).with_context(|| format!("no instance {key}"))?;
    let made = import(&found, name.as_deref().unwrap_or(&found.name), take, &AtomicBool::new(false), &|_, _| {})?;
    println!("Instance {} ({}): {}", made.name, made.id, made.version_line());
    println!("{} mods from Modrinth, {} local", made.mods.len(), made.local_mods.len());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use sha1::{Digest, Sha1};

    use super::*;

    fn sha1_of(bytes: &[u8]) -> String {
        Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn modref(bytes: &[u8], file: &str) -> ModRef {
        ModRef { project_id: "p".into(), version_id: "v".into(), title: "Sodium".into(), file_name: file.into(), url: "https://cdn.example/x.jar".into(), sha1: sha1_of(bytes) }
    }

    fn known(list: Vec<ModRef>) -> impl Fn(&[String]) -> Result<HashMap<String, ModRef>> {
        move |hashes: &[String]| Ok(list.iter().filter(|m| hashes.contains(&m.sha1)).map(|m| (m.sha1.clone(), m.clone())).collect())
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, base, out);
                } else {
                    out.push((path.strip_prefix(base).unwrap().to_string_lossy().into_owned(), fs::read(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    fn farm(root: &Path) -> Found {
        let dir = root.join("Instances").join("Farm Pack");
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::create_dir_all(dir.join("resourcepacks")).unwrap();
        fs::create_dir_all(dir.join("saves").join("World One")).unwrap();
        fs::write(dir.join("minecraftinstance.json"), r#"{"name":"Farm Pack","gameVersion":"1.20.1","baseModLoader":{"name":"forge-47.2.0","forgeVersion":"47.2.0","minecraftVersion":"1.20.1"}}"#).unwrap();
        fs::write(dir.join("mods").join("renamed.jar"), b"sodium bytes").unwrap();
        fs::write(dir.join("mods").join("mine.jar"), b"my own mod").unwrap();
        fs::write(dir.join("config").join("a.toml"), b"a=1").unwrap();
        fs::write(dir.join("resourcepacks").join("pack.zip"), b"zip").unwrap();
        fs::write(dir.join("options.txt"), "fov:1.0\n").unwrap();
        fs::write(dir.join("servers.dat"), b"servers").unwrap();
        fs::write(dir.join("saves").join("World One").join("level.dat"), b"level").unwrap();
        scan_folder(root).into_iter().flat_map(|l| l.instances).next().unwrap()
    }

    fn run(found: &Found, take: Take, out: &Path) -> Result<made::Instance> {
        let list = known(vec![modref(b"sodium bytes", "sodium-0.6.jar")]);
        import_into(found, "Imported", take, out, &list, &AtomicBool::new(false), &|_, _| {})
    }

    #[test]
    fn an_import_copies_mods_config_and_settings_and_leaves_the_source_untouched() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let found = farm(src.path());
        let before = snapshot(src.path());
        let made = run(&found, Take { settings: true, worlds: false }, out.path()).unwrap();
        assert_eq!(snapshot(src.path()), before);
        assert_eq!((made.game_version.as_str(), &made.loader), ("1.20.1", &Loader::Forge(Some("47.2.0".into()))));
        assert_eq!((made.mods.len(), made.local_mods.clone()), (1, vec!["mine.jar".to_owned()]));
        let dest = out.path().join(&made.id);
        assert_eq!(fs::read(dest.join("mods").join("sodium-0.6.jar")).unwrap(), b"sodium bytes");
        assert_eq!(fs::read(dest.join("config").join("a.toml")).unwrap(), b"a=1");
        assert_eq!(fs::read(dest.join("resourcepacks").join("pack.zip")).unwrap(), b"zip");
        assert_eq!(fs::read_to_string(dest.join("options.txt")).unwrap(), "fov:1.0\n");
        assert!(!dest.join("servers.dat").exists() && !dest.join(SAVES_SEED).exists());
    }

    #[test]
    fn worlds_and_settings_come_along_only_when_asked() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let found = farm(src.path());
        let bare = run(&found, Take::default(), out.path()).unwrap();
        let dest = out.path().join(&bare.id);
        assert!(!dest.join("options.txt").exists() && !dest.join("resourcepacks").exists() && !dest.join(SAVES_SEED).exists());
        assert!(dest.join("mods").is_dir() && dest.join("config").is_dir());
        let full = run(&found, Take { settings: true, worlds: true }, out.path()).unwrap();
        assert_eq!(fs::read(out.path().join(&full.id).join(SAVES_SEED).join("World One").join("level.dat")).unwrap(), b"level");
    }

    #[test]
    fn a_cancelled_import_leaves_nothing_behind() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let found = farm(src.path());
        let stop = AtomicBool::new(true);
        let err = import_into(&found, "X", Take::default(), out.path(), &known(Vec::new()), &stop, &|_, _| {}).unwrap_err();
        assert!(err.chain().any(|c| c.downcast_ref::<mrpack::Refused>() == Some(&mrpack::Refused::Cancelled)));
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn progress_counts_up_to_the_total() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let found = farm(src.path());
        let seen = std::sync::Mutex::new(Vec::new());
        let list = known(Vec::new());
        import_into(&found, "X", Take::default(), out.path(), &list, &AtomicBool::new(false), &|d, t| seen.lock().unwrap().push((d, t))).unwrap();
        let seen = seen.into_inner().unwrap();
        assert!(seen.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(seen.iter().all(|(d, t)| d <= t));
    }

    #[test]
    fn unusable_instances_are_refused_before_anything_is_written() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let mut found = farm(src.path());
        found.unsupported = Some(Unsupported::Loader("liteloader".into()));
        assert!(run(&found, Take::default(), out.path()).is_err());
        found.unsupported = None;
        found.game_version = None;
        assert!(run(&found, Take::default(), out.path()).is_err());
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn scanning_several_roots_groups_by_launcher_and_skips_empty_ones() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        farm(a.path());
        let found = scan_roots(&[a.path().to_path_buf(), a.path().to_path_buf(), b.path().to_path_buf(), PathBuf::from("missing-folder")]);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].launcher, found[0].instances.len()), (Launcher::CurseForge, 1));
        assert_eq!(Launcher::from_key(found[0].launcher.key()), Some(Launcher::CurseForge));
    }
}
