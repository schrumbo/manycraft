//! Import from Prism Launcher and MultiMC: their instances become instances, its offline accounts become accounts.
//! Prism's files are only read, never moved or changed, and its sign-in tokens are never kept, logged or stored.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::account::{self, Account, AccountKind};
use super::batch::ModRef;
use super::instance::{self as made, Loader};
use super::{modrinth, mrpack, launchers};

const CONFIG_FILE: &str = "prismlauncher.cfg";
const INSTANCE_FILE: &str = "instance.cfg";
const PACK_FILE: &str = "mmc-pack.json";
const ACCOUNTS_FILE: &str = "accounts.json";
pub(super) const OPTIONS_FILE: &str = "options.txt";
const DEFAULT_INSTANCE_DIR: &str = "instances";
const GAME_DIRS: [&str; 2] = [".minecraft", "minecraft"];
const FLATPAK: &str = ".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher";
const MINECRAFT: &str = "net.minecraft";
type MakeLoader = fn(Option<String>) -> Loader;
const LOADERS: [(&str, MakeLoader); 4] = [
    ("net.fabricmc.fabric-loader", Loader::Fabric),
    ("org.quiltmc.quilt-loader", Loader::Quilt),
    ("net.minecraftforge", Loader::Forge),
    ("net.neoforged", Loader::NeoForge),
];
const OTHER_LOADERS: [(&str, &str); 1] = [("com.mumfrey.liteloader", "LiteLoader")];
const MAX_DEPTH: usize = 32;
const USAGE: &str = "usage: manycraft prism list
       manycraft prism import <instance> [--name <name>]
       manycraft prism accounts [--import]";

/// One Prism instance, read from its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// Folder name inside Prism's instance folder.
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    /// The `.minecraft` (or older `minecraft`) folder holding mods, config and options.
    pub game_dir: PathBuf,
    pub game_version: Option<String>,
    pub loader: Loader,
    /// Enabled mods: `.jar` files directly in the mods folder.
    pub mods: usize,
    pub icon: String,
    /// Why the instance cannot be imported; `None` when its game version and loader are known.
    pub unsupported: Option<String>,
}

/// An account listed in Prism's `accounts.json`: only its name and kind, never a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrismAccount {
    pub name: String,
    pub kind: AccountKind,
}

/// What `import_accounts` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountImport {
    /// Offline accounts that were saved.
    pub added: Vec<Account>,
    /// Microsoft accounts that must sign in again; nothing was saved for them.
    pub needs_sign_in: Vec<String>,
    /// Accounts left out, with the reason.
    pub skipped: Vec<(String, String)>,
}

/// Prism's data folders found on this machine: `MANYCRAFT_PRISM` when set, otherwise the standard
/// folder of the OS and, on Linux, the Flatpak one.
pub fn data_dirs() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("MANYCRAFT_PRISM").filter(|d| !d.is_empty()) {
        return vec![PathBuf::from(dir)];
    }
    candidates(dirs::data_dir(), dirs::home_dir()).into_iter().filter(|d| d.is_dir()).collect()
}

fn candidates(data: Option<PathBuf>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(data) = data {
        found.push(data.join("PrismLauncher"));
    }
    if cfg!(target_os = "linux") {
        if let Some(home) = home {
            found.push(home.join(FLATPAK));
        }
    }
    found
}

/// Every instance in every Prism data folder, sorted by name. Instances without a game version are listed with a reason.
pub fn instances() -> Result<Vec<Instance>> {
    let mut all = Vec::new();
    for data in data_dirs() {
        all.extend(instances_in(&instance_root(&data))?);
    }
    all.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Ok(all)
}

/// Accounts of every Prism data folder, once per name.
pub fn accounts() -> Result<Vec<PrismAccount>> {
    let mut all: Vec<PrismAccount> = Vec::new();
    for data in data_dirs() {
        for found in accounts_in(&data)? {
            if !all.iter().any(|a| a.name.eq_ignore_ascii_case(&found.name)) {
                all.push(found);
            }
        }
    }
    Ok(all)
}

/// Copies `instance` into a new batch called `name`: same game and loader version, its mods, config and
/// `options.txt`. Mods found on Modrinth by SHA-1 become pinned `ModRef`s; the others stay local jars,
/// listed in `Batch::local_mods`. Saved before it returns; Prism's files are untouched.
pub fn import_instance(instance: &Instance, name: &str) -> Result<made::Instance> {
    let name = name.trim();
    if made::load_all()?.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        bail!("an instance named {name} exists already");
    }
    import_to(instance, name, &made::instances_dir(), &modrinth::match_hashes)
}

/// Adds Prism's offline accounts to the saved accounts. Microsoft accounts are not saved, they need a sign-in.
/// Names already saved (any case) are skipped.
pub fn import_accounts() -> Result<AccountImport> {
    let prism = accounts()?;
    let mut saved = account::load()?;
    let done = import_accounts_into(&mut saved, &prism);
    if !done.added.is_empty() {
        account::save(&saved)?;
    }
    Ok(done)
}

/// Adds the offline accounts of `prism` to `saved` without writing anything; see `import_accounts`.
pub fn import_accounts_into(saved: &mut Vec<Account>, prism: &[PrismAccount]) -> AccountImport {
    let mut done = AccountImport::default();
    for found in prism {
        let listed = done.needs_sign_in.iter().any(|n| n.eq_ignore_ascii_case(&found.name));
        if listed || saved.iter().any(|a| a.name.eq_ignore_ascii_case(&found.name)) {
            done.skipped.push((found.name.clone(), "already added".to_owned()));
            continue;
        }
        match found.kind {
            AccountKind::Microsoft => done.needs_sign_in.push(found.name.clone()),
            AccountKind::Offline => match Account::offline(&found.name) {
                Ok(new) => {
                    done.added.push(account::upsert(saved, new));
                }
                Err(e) => done.skipped.push((found.name.clone(), e.to_string())),
            },
        }
    }
    done
}

type Lookup<'a> = &'a dyn Fn(&[String]) -> Result<HashMap<String, ModRef>>;

/// Whether `dir` is a Prism instance folder: it holds an `instance.cfg`.
pub fn is_instance(dir: &Path) -> bool {
    dir.join(INSTANCE_FILE).is_file()
}

/// Reads the Prism instance in `dir`, named after the folder when its `instance.cfg` has no name.
pub fn instance_at(dir: &Path) -> Result<Instance> {
    let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or("instance").to_owned();
    read_instance(id, dir.to_path_buf())
}

/// Copies `instance` into a new batch `name` under `root`; see `import_instance`. Does not check that the name is free.
pub(super) fn import_into(instance: &Instance, name: &str, root: &Path, lookup: Lookup) -> Result<made::Instance> {
    import_to(instance, name, root, lookup)
}

/// Where the instance sits in a Prism export zip: `""` when `instance.cfg` and `mmc-pack.json` are at the top,
/// `"folder/"` when they are one folder down; `None` when the zip is no Prism export.
pub fn export_prefix<R: Read + Seek>(zip: &zip::ZipArchive<R>) -> Option<String> {
    let names: HashSet<String> = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    let mut prefixes: Vec<&str> = names.iter().filter_map(|n| n.strip_suffix(INSTANCE_FILE)).filter(|p| p.is_empty() || (p.ends_with('/') && p.matches('/').count() == 1)).collect();
    prefixes.sort_unstable();
    prefixes.into_iter().find(|p| names.contains(&format!("{p}{PACK_FILE}"))).map(str::to_owned)
}

/// Unpacks what an import reads from a Prism export zip into `staging`: `instance.cfg`, `mmc-pack.json` and the
/// game folder's jars, config and `options.txt`. Worlds, logs and everything else stay in the zip. Returns the instance folder.
pub fn unpack_export<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, prefix: &str, staging: &Path, cancel: &AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<PathBuf> {
    let dir = staging.join("instance");
    let mut total = 0;
    for index in 0..zip.len() {
        if cancel.load(Ordering::SeqCst) {
            bail!(mrpack::Refused::Cancelled);
        }
        progress(index, zip.len());
        let entry = zip.by_index(index).context("cannot read the zip")?;
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let Some(rest) = entry.name().context("cannot read the zip")?.strip_prefix(prefix).map(str::to_owned) else { continue };
        if !wanted_in_export(&rest) {
            continue;
        }
        let path = dir.join(mrpack::safe_rel(&rest)?);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let mut out = std::fs::File::create(&path).with_context(|| format!("cannot write {}", path.display()))?;
        mrpack::copy_limited(entry, &mut out, &mut total)?;
    }
    Ok(dir)
}

fn wanted_in_export(rest: &str) -> bool {
    if rest == INSTANCE_FILE || rest == PACK_FILE {
        return true;
    }
    let Some(inner) = GAME_DIRS.iter().find_map(|d| rest.strip_prefix(d).and_then(|r| r.strip_prefix('/'))) else { return false };
    inner == OPTIONS_FILE || inner.starts_with("config/") || inner.strip_prefix("mods/").is_some_and(|jar| !jar.contains('/') && is_jar(jar))
}

pub(super) struct Jar {
    pub path: PathBuf,
    pub name: String,
    pub sha1: String,
}

fn import_to(instance: &Instance, name: &str, root: &Path, lookup: Lookup) -> Result<made::Instance> {
    if let Some(reason) = &instance.unsupported {
        bail!("{} cannot be imported: {reason}", instance.name);
    }
    let spec = launchers::Spec { name, label: &instance.name, game_version: instance.game_version.as_deref(), loader: instance.loader.clone(), game_dir: &instance.game_dir, take: launchers::Take { settings: true, worlds: false } };
    launchers::copy_instance(&spec, root, lookup, &AtomicBool::new(false), &|_, _| {})
}

pub(super) fn copy_file(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to).with_context(|| format!("cannot copy {}", from.display()))?;
    Ok(())
}

pub(super) fn copy_tree(from: &Path, to: &Path, depth: usize, check: &dyn Fn() -> Result<()>) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("{} is nested too deeply", from.display());
    }
    std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))? {
        let entry = entry?;
        check()?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, depth + 1, check)?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

pub(super) fn is_jar(name: &str) -> bool {
    name.len() > 4 && name.get(name.len() - 4..).is_some_and(|ext| ext.eq_ignore_ascii_case(".jar"))
}

pub(super) fn jar_names(mods: &Path) -> Result<Vec<(String, PathBuf)>> {
    let entries = match std::fs::read_dir(mods) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", mods.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_jar(&name) && entry.file_type()?.is_file() {
            found.push((name, entry.path()));
        }
    }
    found.sort();
    Ok(found)
}

fn instance_root(data: &Path) -> PathBuf {
    let configured = std::fs::read(data.join(CONFIG_FILE)).ok().and_then(|b| ini_value(&String::from_utf8_lossy(&b), "InstanceDir"));
    match configured {
        Some(dir) => data.join(dir),
        None => data.join(DEFAULT_INSTANCE_DIR),
    }
}

fn instances_in(root: &Path) -> Result<Vec<Instance>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let dir = entry.path();
        if id.starts_with(['.', '_']) || !dir.join(INSTANCE_FILE).is_file() {
            continue;
        }
        let game_dir = dir.join(GAME_DIRS[0]);
        found.push(read_instance(id.clone(), dir.clone()).unwrap_or_else(|e| Instance {
            name: id.clone(),
            id,
            dir,
            game_dir,
            game_version: None,
            loader: Loader::Vanilla,
            mods: 0,
            icon: "default".to_owned(),
            unsupported: Some(format!("{e:#}")),
        }));
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Ok(found)
}

#[derive(Deserialize)]
struct Pack {
    components: Vec<Component>,
}

#[derive(Deserialize)]
struct Component {
    uid: String,
    #[serde(default)]
    version: Option<String>,
}

fn read_instance(id: String, dir: PathBuf) -> Result<Instance> {
    let cfg = std::fs::read(dir.join(INSTANCE_FILE)).with_context(|| format!("cannot read {}", dir.join(INSTANCE_FILE).display()))?;
    let cfg = String::from_utf8_lossy(&cfg);
    let name = ini_value(&cfg, "name").unwrap_or_else(|| id.clone());
    let icon = ini_value(&cfg, "iconKey").unwrap_or_else(|| "default".to_owned());
    let game_dir = GAME_DIRS.iter().map(|d| dir.join(d)).find(|d| d.is_dir()).unwrap_or_else(|| dir.join(GAME_DIRS[0]));
    let mods = jar_names(&game_dir.join("mods"))?.len();
    let (game_version, loader, unsupported) = match std::fs::read(dir.join(PACK_FILE)) {
        Err(_) => (None, Loader::Vanilla, Some(format!("no {PACK_FILE}"))),
        Ok(bytes) => match serde_json::from_slice::<Pack>(&bytes) {
            Err(_) => (None, Loader::Vanilla, Some(format!("{PACK_FILE} is unreadable"))),
            Ok(pack) => versions_of(&pack),
        },
    };
    Ok(Instance { id, name, dir, game_dir, game_version, loader, mods, icon, unsupported })
}

fn versions_of(pack: &Pack) -> (Option<String>, Loader, Option<String>) {
    let find = |uid: &str| pack.components.iter().find(|c| c.uid == uid);
    let game = find(MINECRAFT).and_then(|c| c.version.clone());
    let loader = LOADERS.iter().find_map(|(uid, make)| find(uid).map(|c| make(c.version.clone())));
    let other = OTHER_LOADERS.iter().find(|(uid, _)| find(uid).is_some()).map(|(_, name)| *name);
    let reason = match (&game, other) {
        (None, _) => Some("no Minecraft version".to_owned()),
        (_, Some(other)) if loader.is_none() => Some(format!("uses {other}, which is not supported")),
        _ => None,
    };
    (game, loader.unwrap_or(Loader::Vanilla), reason)
}

pub(super) fn ini_value(text: &str, key: &str) -> Option<String> {
    let mut general = true;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            general = section.eq_ignore_ascii_case("General");
        } else if let Some((k, value)) = line.split_once('=') {
            let value = unescape(value.trim());
            if general && k.trim() == key && !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn unescape(value: &str) -> String {
    let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[derive(Deserialize)]
struct AccountsFile {
    #[serde(default)]
    accounts: Vec<RawAccount>,
}

#[derive(Deserialize)]
struct RawAccount {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    profile: Option<RawProfile>,
}

#[derive(Deserialize)]
struct RawProfile {
    #[serde(default)]
    name: String,
}

fn accounts_in(data: &Path) -> Result<Vec<PrismAccount>> {
    let path = data.join(ACCOUNTS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let file: AccountsFile = serde_json::from_slice(&bytes).with_context(|| format!("{} is unreadable", path.display()))?;
    let mut found = Vec::new();
    for raw in file.accounts {
        let kind = match raw.kind.to_ascii_lowercase().as_str() {
            "msa" => AccountKind::Microsoft,
            "offline" => AccountKind::Offline,
            _ => continue,
        };
        let Some(name) = raw.profile.map(|p| p.name.trim().to_owned()).filter(|n| !n.is_empty()) else {
            continue;
        };
        found.push(PrismAccount { name, kind });
    }
    Ok(found)
}

/// `manycraft prism`: lists and imports Prism Launcher instances and accounts from the terminal.
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
        ["list"] => list(),
        ["import", who, flags @ ..] => import(who, flags),
        ["accounts"] => show_accounts(false),
        ["accounts", "--import"] => show_accounts(true),
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn list() -> Result<i32> {
    let found = instances()?;
    if found.is_empty() {
        println!("no Prism Launcher instances found");
    }
    for i in &found {
        let game = i.game_version.as_deref().unwrap_or("?");
        match &i.unsupported {
            Some(reason) => println!("{}  {}  Minecraft {game}  unsupported: {reason}", i.id, i.name),
            None => println!("{}  {}  Minecraft {game}  {}  {} mods", i.id, i.name, i.loader.name(), i.mods),
        }
    }
    Ok(0)
}

fn find_instance(found: Vec<Instance>, who: &str) -> Result<Instance> {
    let mut hits: Vec<Instance> = found.into_iter().filter(|i| i.id.eq_ignore_ascii_case(who) || i.name.eq_ignore_ascii_case(who)).collect();
    if hits.len() > 1 {
        let exact: Vec<Instance> = hits.iter().filter(|i| i.id.eq_ignore_ascii_case(who)).cloned().collect();
        if exact.len() != 1 {
            bail!("several instances are named {who}, use the folder name");
        }
        hits = exact;
    }
    hits.pop().with_context(|| format!("no Prism instance {who}"))
}

fn import(who: &str, args: &[&str]) -> Result<i32> {
    let name = match args {
        [] => None,
        ["--name", name] => Some(*name),
        _ => {
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    let instance = find_instance(instances()?, who)?;
    let batch = import_instance(&instance, name.unwrap_or(&instance.name))?;
    println!("Instance {} ({}): {}", batch.name, batch.id, batch.version_line());
    println!("{} mods from Modrinth, {} local", batch.mods.len(), batch.local_mods.len());
    for local in &batch.local_mods {
        println!("  local: {local}");
    }
    Ok(0)
}

fn show_accounts(save: bool) -> Result<i32> {
    if save {
        let done = import_accounts()?;
        for a in &done.added {
            println!("added {} (offline)", a.name);
        }
        for name in &done.needs_sign_in {
            println!("{name}  needs sign-in");
        }
        for (name, reason) in &done.skipped {
            println!("{name}  skipped: {reason}");
        }
        return Ok(0);
    }
    let found = accounts()?;
    if found.is_empty() {
        println!("no Prism Launcher accounts found");
    }
    for a in &found {
        let kind = match a.kind {
            AccountKind::Microsoft => "microsoft",
            AccountKind::Offline => "offline",
        };
        println!("{}  {kind}", a.name);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::fleet;
    use super::*;

    const PACK: &str = r#"{"components":[{"uid":"net.minecraft","version":"1.21.1","important":true},{"uid":"net.fabricmc.intermediary","version":"1.21.1"},{"uid":"net.fabricmc.fabric-loader","version":"0.16.5"}],"formatVersion":1}"#;

    struct Spec<'a> {
        id: &'a str,
        cfg: &'a str,
        pack: Option<&'a str>,
        game_dir: &'a str,
    }

    fn spec<'a>(id: &'a str, name: &str) -> (Spec<'a>, String) {
        (Spec { id, cfg: "", pack: Some(PACK), game_dir: ".minecraft" }, format!("[General]\nInstanceType=OneSix\niconKey=grass\nname={name}\n"))
    }

    fn make(root: &Path, spec: &Spec, cfg: &str) -> PathBuf {
        let dir = root.join(spec.id);
        fs::create_dir_all(dir.join(spec.game_dir)).unwrap();
        fs::write(dir.join(INSTANCE_FILE), if spec.cfg.is_empty() { cfg } else { spec.cfg }).unwrap();
        if let Some(pack) = spec.pack {
            fs::write(dir.join(PACK_FILE), pack).unwrap();
        }
        dir.join(spec.game_dir)
    }

    fn fabric(root: &Path, id: &str, name: &str) -> Instance {
        let (s, cfg) = spec(id, name);
        make(root, &s, &cfg);
        instances_in(root).unwrap().into_iter().find(|i| i.id == id).unwrap()
    }

    fn sha1_of(bytes: &[u8]) -> String {
        use sha1::{Digest, Sha1};
        Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn modref(bytes: &[u8], file: &str, title: &str) -> ModRef {
        ModRef { project_id: format!("p-{title}"), version_id: format!("v-{title}"), title: title.into(), file_name: file.into(), url: "https://cdn.example/x.jar".into(), sha1: sha1_of(bytes) }
    }

    fn lookup_of(known: Vec<ModRef>) -> impl Fn(&[String]) -> Result<HashMap<String, ModRef>> {
        move |hashes: &[String]| Ok(known.iter().filter(|m| hashes.contains(&m.sha1)).map(|m| (m.sha1.clone(), m.clone())).collect())
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

    #[test]
    fn data_folders_follow_the_os_and_flatpak() {
        let found = candidates(Some(PathBuf::from("/d")), Some(PathBuf::from("/h")));
        assert_eq!(found[0], Path::new("/d").join("PrismLauncher"));
        if cfg!(target_os = "linux") {
            assert_eq!(found[1], Path::new("/h").join(FLATPAK));
        } else {
            assert_eq!(found.len(), 1);
        }
        assert!(candidates(None, None).is_empty());
    }

    #[test]
    fn the_instance_folder_follows_prismlauncher_cfg() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        assert_eq!(instance_root(data), data.join("instances"));
        fs::write(data.join(CONFIG_FILE), "\u{feff}[General]\nLastUsedGroups=x\nInstanceDir=mine\n").unwrap();
        assert_eq!(instance_root(data), data.join("mine"));
        let elsewhere = dir.path().join("else").join("where");
        fs::write(data.join(CONFIG_FILE), format!("[General]\nInstanceDir={}\n", elsewhere.to_string_lossy().replace('\\', "\\\\"))).unwrap();
        assert_eq!(instance_root(data), elsewhere);
        fs::write(data.join(CONFIG_FILE), "[General]\nInstanceDir=\n").unwrap();
        assert_eq!(instance_root(data), data.join("instances"));
        fs::write(data.join(CONFIG_FILE), "[Other]\nInstanceDir=nope\n").unwrap();
        assert_eq!(instance_root(data), data.join("instances"));
    }

    #[test]
    fn ini_values_are_scoped_trimmed_quoted_and_unescaped() {
        let text = "[Misc]\nname=wrong\n[General]\n name = Mein Server \niconKey=\"a\\tb\"\npath=C:\\\\x\\\\y\nempty=\n";
        assert_eq!(ini_value(text, "name").as_deref(), Some("Mein Server"));
        assert_eq!(ini_value(text, "iconKey").as_deref(), Some("a\tb"));
        assert_eq!(ini_value(text, "path").as_deref(), Some("C:\\x\\y"));
        assert_eq!(ini_value(text, "empty"), None);
        assert_eq!(ini_value(text, "missing"), None);
        assert_eq!(unescape("end\\"), "end\\");
        assert_eq!(unescape("a\\nb"), "a\nb");
    }

    #[test]
    fn a_fabric_instance_is_listed_with_versions_mods_and_icon() {
        let dir = tempfile::tempdir().unwrap();
        let (s, cfg) = spec("fabric1", "Farm");
        let game = make(dir.path(), &s, &cfg);
        fs::create_dir_all(game.join("mods").join("sub")).unwrap();
        fs::write(game.join("mods").join("a.jar"), b"a").unwrap();
        fs::write(game.join("mods").join("B.JAR"), b"b").unwrap();
        fs::write(game.join("mods").join("c.jar.disabled"), b"c").unwrap();
        fs::write(game.join("mods").join("notes.txt"), b"n").unwrap();
        fs::write(game.join("mods").join("sub").join("d.jar"), b"d").unwrap();
        fs::create_dir(game.join("mods").join("e.jar")).unwrap();
        let found = instances_in(dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        let i = &found[0];
        assert_eq!((i.id.as_str(), i.name.as_str(), i.icon.as_str()), ("fabric1", "Farm", "grass"));
        assert_eq!((i.game_version.as_deref(), &i.loader), (Some("1.21.1"), &Loader::Fabric(Some("0.16.5".into()))));
        assert_eq!(i.mods, 2);
        assert_eq!(i.unsupported, None);
        assert_eq!(i.game_dir, dir.path().join("fabric1").join(".minecraft"));
    }

    #[test]
    fn an_unreadable_instance_is_listed_as_unsupported_and_does_not_hide_the_others() {
        let dir = tempfile::tempdir().unwrap();
        fabric(dir.path(), "good", "Good");
        let bad = fabric(dir.path(), "bad", "Bad");
        fs::write(bad.game_dir.join("mods"), b"a file, not a folder").unwrap();
        let found = instances_in(dir.path()).unwrap();
        assert_eq!(found.len(), 2);
        let bad = found.iter().find(|i| i.id == "bad").unwrap();
        assert!(bad.unsupported.as_deref().is_some_and(|r| r.contains("cannot read")), "{:?}", bad.unsupported);
        assert_eq!(found.iter().find(|i| i.id == "good").unwrap().unsupported, None);
    }

    #[test]
    fn every_loader_is_read_and_broken_packs_are_unsupported_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let loaders: [(&str, &str, Loader); 4] = [
            ("forge", r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.minecraftforge","version":"47.2.0"}]}"#, Loader::Forge(Some("47.2.0".into()))),
            ("neo", r#"{"components":[{"uid":"net.minecraft","version":"1.21.1"},{"uid":"net.neoforged","version":"21.1.77"}]}"#, Loader::NeoForge(Some("21.1.77".into()))),
            ("quilt", r#"{"components":[{"uid":"net.minecraft","version":"1.21"},{"uid":"org.quiltmc.quilt-loader","version":"0.26.0"}]}"#, Loader::Quilt(Some("0.26.0".into()))),
            ("vanilla", r#"{"components":[{"uid":"net.minecraft","version":"1.21"}]}"#, Loader::Vanilla),
        ];
        for (id, pack, _) in &loaders {
            let (mut s, cfg) = spec(id, id);
            s.pack = Some(pack);
            make(dir.path(), &s, &cfg);
        }
        let cases: [(&str, Option<&str>, &str); 4] = [
            ("lite", Some(r#"{"components":[{"uid":"net.minecraft","version":"1.7.10"},{"uid":"com.mumfrey.liteloader","version":"1"}]}"#), "LiteLoader"),
            ("nopack", None, "mmc-pack.json"),
            ("broken", Some("{"), "unreadable"),
            ("nogame", Some(r#"{"components":[{"uid":"net.fabricmc.fabric-loader","version":"0.16"}]}"#), "Minecraft version"),
        ];
        for (id, pack, _) in cases {
            let (mut s, cfg) = spec(id, id);
            s.pack = pack;
            make(dir.path(), &s, &cfg);
        }
        let found = instances_in(dir.path()).unwrap();
        assert_eq!(found.len(), 8);
        for (id, _, loader) in loaders.iter() {
            let i = found.iter().find(|i| i.id == *id).unwrap();
            assert_eq!((&i.loader, &i.unsupported), (loader, &None), "{id}");
        }
        for (id, _, reason) in cases {
            let i = found.iter().find(|i| i.id == id).unwrap();
            assert!(i.unsupported.as_deref().is_some_and(|r| r.contains(reason)), "{id}: {:?}", i.unsupported);
        }
        assert_eq!(found.iter().find(|i| i.id == "nogame").unwrap().game_version, None);
    }

    #[test]
    fn listing_skips_other_folders_and_falls_back_on_names_and_the_old_game_folder() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("_LAUNCHER_TEMP")).unwrap();
        fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        fs::create_dir_all(dir.path().join("empty")).unwrap();
        fs::write(dir.path().join("instgroups.json"), b"{}").unwrap();
        let (mut s, _) = spec("zeta", "");
        s.game_dir = "minecraft";
        make(dir.path(), &s, "[General]\nInstanceType=OneSix\n");
        fabric(dir.path(), "alpha", "alpha farm");
        fabric(dir.path(), "beta", "Beta farm");
        let found = instances_in(dir.path()).unwrap();
        let names: Vec<&str> = found.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["alpha farm", "Beta farm", "zeta"]);
        let zeta = found.iter().find(|i| i.id == "zeta").unwrap();
        assert_eq!((zeta.icon.as_str(), zeta.game_dir.ends_with("minecraft")), ("default", true));
        assert!(instances_in(&dir.path().join("none")).unwrap().is_empty());
    }

    #[test]
    fn import_copies_mods_config_and_options_and_leaves_prism_untouched() {
        let prism = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let inst = fabric(prism.path(), "farm", "Farm");
        let game = &inst.game_dir;
        fs::create_dir_all(game.join("mods")).unwrap();
        fs::write(game.join("mods").join("renamed-by-user.jar"), b"sodium bytes").unwrap();
        fs::write(game.join("mods").join("mine.jar"), b"my own mod").unwrap();
        fs::write(game.join("mods").join("off.jar.disabled"), b"disabled").unwrap();
        fs::write(game.join("mods").join(fleet::COMPANION_FILE), b"old companion").unwrap();
        fs::create_dir_all(game.join("config").join("deep").join("er")).unwrap();
        fs::write(game.join("config").join("sodium.json"), b"{}").unwrap();
        fs::write(game.join("config").join("deep").join("er").join("x.toml"), b"x=1").unwrap();
        fs::write(game.join("options.txt"), "fov:1.0\nsoundCategory_master:0.7\n").unwrap();
        fs::write(game.join("servers.dat"), b"servers").unwrap();
        let before = snapshot(prism.path());
        let known = vec![modref(b"sodium bytes", "sodium-fabric-0.6.jar", "Sodium")];

        let batch = import_to(&inst, "Imported", out.path(), &lookup_of(known.clone())).unwrap();

        assert_eq!(snapshot(prism.path()), before);
        assert_eq!((batch.name.as_str(), batch.game_version.as_str(), batch.loader.version()), ("Imported", "1.21.1", Some("0.16.5")));
        assert_eq!(batch.mods, known);
        assert_eq!(batch.local_mods, ["mine.jar"]);
        let dest = crate::core::instance::dir_in(out.path(), &batch.id);
        let mut mods: Vec<String> = fs::read_dir(dest.join("mods")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        mods.sort();
        assert_eq!(mods, ["mine.jar", "sodium-fabric-0.6.jar"]);
        assert_eq!(fs::read(dest.join("mods").join("sodium-fabric-0.6.jar")).unwrap(), b"sodium bytes");
        assert_eq!(fs::read(dest.join("mods").join("mine.jar")).unwrap(), b"my own mod");
        assert_eq!(fs::read(dest.join("config").join("sodium.json")).unwrap(), b"{}");
        assert_eq!(fs::read(dest.join("config").join("deep").join("er").join("x.toml")).unwrap(), b"x=1");
        assert_eq!(fs::read_to_string(dest.join(OPTIONS_FILE)).unwrap(), "fov:1.0\nsoundCategory_master:0.7\n");
        assert!(!dest.join("servers.dat").exists());
        let saved: made::Instance = serde_json::from_slice(&fs::read(dest.join("instance.json")).unwrap()).unwrap();
        assert_eq!(saved, batch);
    }

    #[test]
    fn an_instance_without_mods_config_or_options_still_imports() {
        let prism = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let inst = fabric(prism.path(), "bare", "Bare");
        let batch = import_to(&inst, "Bare", out.path(), &lookup_of(Vec::new())).unwrap();
        let dest = crate::core::instance::dir_in(out.path(), &batch.id);
        assert!(dest.join("mods").is_dir() && dest.join("config").is_dir());
        assert!(!dest.join(OPTIONS_FILE).exists());
        assert!(batch.mods.is_empty() && batch.local_mods.is_empty());
    }

    #[test]
    fn identical_jars_import_once() {
        let prism = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let inst = fabric(prism.path(), "dup", "Dup");
        fs::create_dir_all(inst.game_dir.join("mods")).unwrap();
        fs::write(inst.game_dir.join("mods").join("a.jar"), b"same").unwrap();
        fs::write(inst.game_dir.join("mods").join("b.jar"), b"same").unwrap();
        let batch = import_to(&inst, "Dup", out.path(), &lookup_of(Vec::new())).unwrap();
        assert_eq!(batch.local_mods, ["a.jar"]);
    }

    #[test]
    fn the_lookup_gets_every_hash_once_and_nothing_is_created_when_it_fails() {
        let prism = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let inst = fabric(prism.path(), "farm", "Farm");
        fs::create_dir_all(inst.game_dir.join("mods")).unwrap();
        fs::write(inst.game_dir.join("mods").join("a.jar"), b"a").unwrap();
        fs::write(inst.game_dir.join("mods").join("b.jar"), b"b").unwrap();
        let asked = std::cell::RefCell::new(Vec::new());
        let spy = |hashes: &[String]| -> Result<HashMap<String, ModRef>> {
            asked.borrow_mut().extend_from_slice(hashes);
            Ok(HashMap::new())
        };
        import_to(&inst, "One", out.path(), &spy).unwrap();
        let mut asked = asked.into_inner();
        asked.sort();
        assert_eq!(asked, [sha1_of(b"a"), sha1_of(b"b")]);
        let before = fs::read_dir(out.path()).unwrap().count();
        let failing = |_: &[String]| -> Result<HashMap<String, ModRef>> { bail!("offline") };
        let err = format!("{:#}", import_to(&inst, "Two", out.path(), &failing).unwrap_err());
        assert!(err.contains("Modrinth") && err.contains("offline"), "{err}");
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), before);
    }

    #[test]
    fn unusable_imports_are_refused_before_anything_is_written() {
        let prism = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let inst = fabric(prism.path(), "farm", "Farm");
        let none = lookup_of(Vec::new());
        assert!(import_to(&inst, "", out.path(), &none).is_err());
        let lite = Instance { unsupported: Some("uses LiteLoader".into()), ..inst.clone() };
        let err = import_to(&lite, "X", out.path(), &none).unwrap_err().to_string();
        assert!(err.contains("Farm") && err.contains("LiteLoader"), "{err}");
        let nogame = Instance { game_version: None, ..inst.clone() };
        assert!(import_to(&nogame, "X", out.path(), &none).is_err());
        fs::create_dir_all(inst.game_dir.join("mods")).unwrap();
        fs::write(inst.game_dir.join("mods").join("a.jar"), b"a").unwrap();
        fs::write(inst.game_dir.join("mods").join("b.jar"), b"b").unwrap();
        let evil = lookup_of(vec![modref(b"a", "../evil.jar", "Evil")]);
        assert!(import_to(&inst, "X", out.path(), &evil).is_err());
        let clash = lookup_of(vec![modref(b"a", "B.jar", "A")]);
        let err = import_to(&inst, "X", out.path(), &clash).unwrap_err().to_string();
        assert!(err.to_lowercase().contains("b.jar"), "{err}");
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_failed_copy_removes_the_half_made_batch() {
        let prism = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let inst = fabric(prism.path(), "farm", "Farm");
        fs::create_dir_all(inst.game_dir.join("mods")).unwrap();
        let jar = inst.game_dir.join("mods").join("a.jar");
        fs::write(&jar, b"a").unwrap();
        let vanishing = |_: &[String]| -> Result<HashMap<String, ModRef>> {
            fs::remove_file(&jar).unwrap();
            Ok(HashMap::new())
        };
        assert!(import_to(&inst, "Broken", out.path(), &vanishing).is_err());
        assert!(fs::read_dir(out.path()).unwrap().all(|e| e.unwrap().file_name().to_string_lossy().starts_with('.')));
    }

    #[test]
    fn nesting_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().join("from");
        for _ in 0..=MAX_DEPTH + 1 {
            deep = deep.join("d");
        }
        fs::create_dir_all(&deep).unwrap();
        assert!(copy_tree(&dir.path().join("from"), &dir.path().join("to"), 0, &|| Ok(())).is_err());
    }

    #[test]
    fn a_copy_stops_between_entries_when_asked_to() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..6 {
            fs::create_dir_all(dir.path().join("from")).unwrap();
            fs::write(dir.path().join("from").join(format!("f{n}")), "x").unwrap();
        }
        let calls = std::cell::Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if calls.get() > 2 { bail!("stop") } else { Ok(()) }
        };
        assert!(copy_tree(&dir.path().join("from"), &dir.path().join("to"), 0, &check).is_err());
        assert!(fs::read_dir(dir.path().join("to")).unwrap().count() < 6);
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> zip::ZipArchive<std::io::Cursor<Vec<u8>>> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip::ZipArchive::new(std::io::Cursor::new(zip.finish().unwrap().into_inner())).unwrap()
    }

    #[test]
    fn prism_exports_are_found_at_the_top_or_one_folder_down() {
        let cfg = b"[General]
name=Farm
";
        assert_eq!(export_prefix(&zip_of(&[("instance.cfg", cfg), ("mmc-pack.json", PACK.as_bytes())])).as_deref(), Some(""));
        assert_eq!(export_prefix(&zip_of(&[("Farm/instance.cfg", cfg), ("Farm/mmc-pack.json", PACK.as_bytes())])).as_deref(), Some("Farm/"));
        assert_eq!(export_prefix(&zip_of(&[("instance.cfg", cfg)])), None);
        assert_eq!(export_prefix(&zip_of(&[("a/b/instance.cfg", cfg), ("a/b/mmc-pack.json", PACK.as_bytes())])), None);
        assert_eq!(export_prefix(&zip_of(&[("modrinth.index.json", b"{}")])), None);
    }

    #[test]
    fn a_prism_export_unpacks_only_what_an_import_reads_and_imports() {
        let staging = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let cfg = "[General]
name=Farm
".to_owned();
        let mut zip = zip_of(&[
            ("Farm/instance.cfg", cfg.as_bytes()),
            ("Farm/mmc-pack.json", PACK.as_bytes()),
            ("Farm/.minecraft/mods/a.jar", b"a"),
            ("Farm/.minecraft/mods/sub/b.jar", b"b"),
            ("Farm/.minecraft/config/x.json", b"{}"),
            ("Farm/.minecraft/options.txt", b"fov:1.0
"),
            ("Farm/.minecraft/saves/w/level.dat", b"world"),
            ("Farm/.minecraft/logs/latest.log", b"log"),
        ]);
        let dir = unpack_export(&mut zip, "Farm/", staging.path(), &AtomicBool::new(false), &|_, _| {}).unwrap();
        assert!(is_instance(&dir));
        assert!(dir.join(".minecraft").join("mods").join("a.jar").is_file());
        assert!(!dir.join(".minecraft").join("mods").join("sub").exists());
        assert!(!dir.join(".minecraft").join("saves").exists() && !dir.join(".minecraft").join("logs").exists());
        let inst = instance_at(&dir).unwrap();
        assert_eq!((inst.name.as_str(), inst.unsupported.as_deref()), ("Farm", None));
        let batch = import_into(&inst, "Farm", out.path(), &lookup_of(Vec::new())).unwrap();
        assert_eq!(batch.local_mods, ["a.jar"]);
        assert_eq!(fs::read(crate::core::instance::dir_in(out.path(), &batch.id).join(OPTIONS_FILE)).unwrap(), b"fov:1.0
");
    }

    #[test]
    fn unpacking_a_prism_export_reports_progress_and_stops_on_cancel() {
        let staging = tempfile::tempdir().unwrap();
        let entries: &[(&str, &[u8])] = &[("instance.cfg", b"x"), ("mmc-pack.json", PACK.as_bytes()), (".minecraft/options.txt", b"fov:1.0")];
        let steps = std::sync::Mutex::new(Vec::new());
        unpack_export(&mut zip_of(entries), "", staging.path(), &AtomicBool::new(false), &|done, total| steps.lock().unwrap().push((done, total))).unwrap();
        assert_eq!(steps.into_inner().unwrap(), [(0, 3), (1, 3), (2, 3)]);
        let err = unpack_export(&mut zip_of(entries), "", staging.path(), &AtomicBool::new(true), &|_, _| {}).unwrap_err();
        assert!(matches!(err.downcast_ref::<mrpack::Refused>(), Some(mrpack::Refused::Cancelled)));
    }

    #[test]
    fn zip_slip_in_a_prism_export_is_refused() {
        let staging = tempfile::tempdir().unwrap();
        let mut zip = zip_of(&[("instance.cfg", b"x"), ("mmc-pack.json", PACK.as_bytes()), (".minecraft/config/../../../evil.txt", b"x")]);
        assert!(unpack_export(&mut zip, "", staging.path(), &AtomicBool::new(false), &|_, _| {}).is_err());
        assert!(!staging.path().parent().unwrap().join("evil.txt").exists());
    }

    const ACCOUNTS: &str = r#"{"accounts":[
        {"type":"MSA","msa-client-id":"x","ygg":{"token":"SECRET-YGG","exp":1},"msa":{"token":"SECRET-MSA","refresh_token":"SECRET-REFRESH"},"profile":{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch","skin":{"url":"x"}}},
        {"type":"Offline","profile":{"id":"abcdef","name":"Steve"}},
        {"type":"Mojang","profile":{"id":"1","name":"Legacy"}},
        {"type":"MSA","profile":{"id":"2","name":""}},
        {"type":"MSA"},
        {"type":"offline","profile":{"name":"  Alex  "}}
    ],"formatVersion":3}"#;

    #[test]
    fn accounts_read_names_and_kinds_only() {
        let dir = tempfile::tempdir().unwrap();
        assert!(accounts_in(dir.path()).unwrap().is_empty());
        fs::write(dir.path().join(ACCOUNTS_FILE), ACCOUNTS).unwrap();
        let found = accounts_in(dir.path()).unwrap();
        let pairs: Vec<(&str, AccountKind)> = found.iter().map(|a| (a.name.as_str(), a.kind)).collect();
        assert_eq!(pairs, [("Notch", AccountKind::Microsoft), ("Steve", AccountKind::Offline), ("Alex", AccountKind::Offline)]);
        assert!(!format!("{found:?}").contains("SECRET"));
        fs::write(dir.path().join(ACCOUNTS_FILE), b"{").unwrap();
        let err = format!("{:#}", accounts_in(dir.path()).unwrap_err());
        assert!(err.contains("unreadable") && !err.contains("SECRET"), "{err}");
        fs::write(dir.path().join(ACCOUNTS_FILE), br#"{"formatVersion":3}"#).unwrap();
        assert!(accounts_in(dir.path()).unwrap().is_empty());
    }

    fn prism_account(name: &str, kind: AccountKind) -> PrismAccount {
        PrismAccount { name: name.into(), kind }
    }

    #[test]
    fn offline_accounts_are_added_microsoft_ones_need_a_sign_in_and_names_are_unique() {
        let mut saved = vec![Account::offline("Existing").unwrap(), Account { id: uuid::Uuid::from_u128(5), name: "Signed".into(), kind: AccountKind::Microsoft, proxy: None, color: None }];
        let prism = [
            prism_account("Steve", AccountKind::Offline),
            prism_account("Notch", AccountKind::Microsoft),
            prism_account("existing", AccountKind::Offline),
            prism_account("SIGNED", AccountKind::Microsoft),
            prism_account("steve", AccountKind::Offline),
            prism_account("x", AccountKind::Offline),
            prism_account("notch", AccountKind::Microsoft),
        ];
        let done = import_accounts_into(&mut saved, &prism);
        let used = account::least_used(&saved[..2]);
        assert_eq!(done.added, [Account { color: Some(used), ..Account::offline("Steve").unwrap() }], "an imported account gets the least used colour");
        assert_eq!(done.needs_sign_in, ["Notch"]);
        let skipped: Vec<&str> = done.skipped.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(skipped, ["existing", "SIGNED", "steve", "x", "notch"]);
        assert_eq!(done.skipped[0].1, "already added");
        assert!(done.skipped[3].1.contains("3 to 16"), "{:?}", done.skipped[3]);
        let names: Vec<&str> = saved.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Existing", "Signed", "Steve"]);
        assert!(saved.iter().all(|a| a.kind != AccountKind::Microsoft || a.name == "Signed"));
    }

    #[test]
    fn instances_are_found_by_folder_or_name() {
        let prism = tempfile::tempdir().unwrap();
        fabric(prism.path(), "one", "Shared");
        fabric(prism.path(), "two", "Shared");
        fabric(prism.path(), "three", "Third");
        let all = instances_in(prism.path()).unwrap();
        assert_eq!(find_instance(all.clone(), "THIRD").unwrap().id, "three");
        assert_eq!(find_instance(all.clone(), "two").unwrap().id, "two");
        assert_eq!(find_instance(all.clone(), "TWO").unwrap().id, "two");
        assert!(find_instance(all.clone(), "Shared").unwrap_err().to_string().contains("several"));
        assert!(find_instance(all, "nope").is_err());
    }

    #[test]
    fn the_command_line_is_checked() {
        let args = |s: &[&str]| s.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        assert_eq!(cli(&args(&[])), 2);
        assert_eq!(cli(&args(&["nope"])), 2);
        assert_eq!(cli(&args(&["list", "x"])), 2);
        assert_eq!(cli(&args(&["import"])), 2);
        assert_eq!(cli(&args(&["accounts", "--nope"])), 2);
        assert_eq!(import("x", &["--name"]).unwrap(), 2);
        assert_eq!(import("x", &["--bogus", "y"]).unwrap(), 2);
    }
}
