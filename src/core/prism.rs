//! Import from Prism Launcher: its Fabric instances become batches, its offline accounts become accounts.
//! Prism's files are only read, never moved or changed, and its sign-in tokens are never kept, logged or stored.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::account::{self, Account, AccountKind};
use super::batch::{self, Batch, ModRef};
use super::{fleet, modrinth, net};

const CONFIG_FILE: &str = "prismlauncher.cfg";
const INSTANCE_FILE: &str = "instance.cfg";
const PACK_FILE: &str = "mmc-pack.json";
const ACCOUNTS_FILE: &str = "accounts.json";
const OPTIONS_FILE: &str = "options.txt";
const DEFAULT_INSTANCE_DIR: &str = "instances";
const GAME_DIRS: [&str; 2] = [".minecraft", "minecraft"];
const FLATPAK: &str = ".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher";
const MINECRAFT: &str = "net.minecraft";
const FABRIC_LOADER: &str = "net.fabricmc.fabric-loader";
const OTHER_LOADERS: [(&str, &str); 4] = [
    ("net.minecraftforge", "Forge"),
    ("net.neoforged", "NeoForge"),
    ("org.quiltmc.quilt-loader", "Quilt"),
    ("com.mumfrey.liteloader", "LiteLoader"),
];
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
    pub loader_version: Option<String>,
    /// Enabled mods: `.jar` files directly in the mods folder.
    pub mods: usize,
    pub icon: String,
    /// Why the instance cannot be imported; `None` for a Fabric instance.
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

/// Every instance in every Prism data folder, sorted by name. Instances that are not Fabric are listed with a reason.
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
pub fn import_instance(instance: &Instance, name: &str) -> Result<Batch> {
    let name = name.trim();
    if batch::load_all()?.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        bail!("a batch named {name} exists already");
    }
    import_to(instance, name, &batch::batches_dir(), &modrinth::match_hashes)
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
                    saved.push(new.clone());
                    done.added.push(new);
                }
                Err(e) => done.skipped.push((found.name.clone(), e.to_string())),
            },
        }
    }
    done
}

type Lookup<'a> = &'a dyn Fn(&[String]) -> Result<HashMap<String, ModRef>>;

struct Jar {
    path: PathBuf,
    name: String,
    sha1: String,
}

fn import_to(instance: &Instance, name: &str, root: &Path, lookup: Lookup) -> Result<Batch> {
    if name.is_empty() {
        bail!("a batch needs a name");
    }
    if let Some(reason) = &instance.unsupported {
        bail!("{} cannot be imported: {reason}", instance.name);
    }
    let game = instance.game_version.as_deref().with_context(|| format!("{} has no Minecraft version", instance.name))?;
    let jars = jars_in(&instance.game_dir.join("mods"))?;
    let hashes: Vec<String> = jars.iter().map(|j| j.sha1.clone()).collect();
    let matched = lookup(&hashes).context("cannot look the mods up on Modrinth")?;

    let mut batch = Batch::new(name, game);
    batch.loader_version.clone_from(&instance.loader_version);
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
    let copied = copy_into(instance, &dest, &copies).and_then(|()| batch::save_in(root, &batch));
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(batch)
}

fn copy_into(instance: &Instance, dest: &Path, copies: &[(&Jar, String)]) -> Result<()> {
    let mods = dest.join("mods");
    std::fs::create_dir_all(&mods).with_context(|| format!("cannot create {}", mods.display()))?;
    for (jar, name) in copies {
        copy_file(&jar.path, &mods.join(name))?;
    }
    let config = dest.join("config");
    let source = instance.game_dir.join("config");
    if source.is_dir() {
        copy_tree(&source, &config, 0)?;
    } else {
        std::fs::create_dir_all(&config).with_context(|| format!("cannot create {}", config.display()))?;
    }
    let options = instance.game_dir.join(OPTIONS_FILE);
    if options.is_file() {
        copy_file(&options, &dest.join(OPTIONS_FILE))?;
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to).with_context(|| format!("cannot copy {}", from.display()))?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("{} is nested too deeply", from.display());
    }
    std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, depth + 1)?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn is_jar(name: &str) -> bool {
    name.len() > 4 && name.get(name.len() - 4..).is_some_and(|ext| ext.eq_ignore_ascii_case(".jar"))
}

fn jar_names(mods: &Path) -> Result<Vec<(String, PathBuf)>> {
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

fn jars_in(mods: &Path) -> Result<Vec<Jar>> {
    jar_names(mods)?
        .into_iter()
        .map(|(name, path)| {
            let sha1 = net::file_sha1(&path).with_context(|| format!("cannot read {}", path.display()))?;
            Ok(Jar { path, name, sha1 })
        })
        .collect()
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
            loader_version: None,
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
    let (game_version, loader_version, unsupported) = match std::fs::read(dir.join(PACK_FILE)) {
        Err(_) => (None, None, Some(format!("no {PACK_FILE}"))),
        Ok(bytes) => match serde_json::from_slice::<Pack>(&bytes) {
            Err(_) => (None, None, Some(format!("{PACK_FILE} is unreadable"))),
            Ok(pack) => versions_of(&pack),
        },
    };
    Ok(Instance { id, name, dir, game_dir, game_version, loader_version, mods, icon, unsupported })
}

fn versions_of(pack: &Pack) -> (Option<String>, Option<String>, Option<String>) {
    let find = |uid: &str| pack.components.iter().find(|c| c.uid == uid);
    let game = find(MINECRAFT).and_then(|c| c.version.clone());
    let fabric = find(FABRIC_LOADER);
    let loader = fabric.and_then(|c| c.version.clone());
    let other = OTHER_LOADERS.iter().find(|(uid, _)| find(uid).is_some()).map(|(_, name)| *name);
    let reason = if game.is_none() {
        Some("no Minecraft version".to_owned())
    } else if let Some(other) = other {
        Some(format!("uses {other}, only Fabric is supported"))
    } else if fabric.is_none() {
        Some("no mod loader, only Fabric instances can be imported".to_owned())
    } else {
        None
    };
    (game, loader, reason)
}

fn ini_value(text: &str, key: &str) -> Option<String> {
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
            None => println!("{}  {}  Minecraft {game}  Fabric {}  {} mods", i.id, i.name, i.loader_version.as_deref().unwrap_or("?"), i.mods),
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
    println!("Batch {} ({}) for Minecraft {}", batch.name, batch.id, batch.game_version);
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
        assert_eq!((i.game_version.as_deref(), i.loader_version.as_deref()), (Some("1.21.1"), Some("0.16.5")));
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
    fn other_loaders_and_broken_packs_are_unsupported_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let cases: [(&str, Option<&str>, &str); 6] = [
            ("forge", Some(r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.minecraftforge","version":"47.2"}]}"#), "Forge"),
            ("neo", Some(r#"{"components":[{"uid":"net.minecraft","version":"1.21"},{"uid":"net.neoforged","version":"21"}]}"#), "NeoForge"),
            ("quilt", Some(r#"{"components":[{"uid":"net.minecraft","version":"1.21"},{"uid":"org.quiltmc.quilt-loader","version":"0.2"}]}"#), "Quilt"),
            ("vanilla", Some(r#"{"components":[{"uid":"net.minecraft","version":"1.21"}]}"#), "no mod loader"),
            ("nopack", None, "mmc-pack.json"),
            ("broken", Some("{"), "unreadable"),
        ];
        for (id, pack, reason) in cases {
            let (mut s, cfg) = spec(id, id);
            s.pack = pack;
            make(dir.path(), &s, &cfg);
        }
        let (mut s, cfg) = spec("nogame", "nogame");
        s.pack = Some(r#"{"components":[{"uid":"net.fabricmc.fabric-loader","version":"0.16"}]}"#);
        make(dir.path(), &s, &cfg);
        let found = instances_in(dir.path()).unwrap();
        assert_eq!(found.len(), 7);
        for (id, _, reason) in cases {
            let i = found.iter().find(|i| i.id == id).unwrap();
            assert!(i.unsupported.as_deref().is_some_and(|r| r.contains(reason)), "{id}: {:?}", i.unsupported);
        }
        let nogame = found.iter().find(|i| i.id == "nogame").unwrap();
        assert!(nogame.unsupported.as_deref().is_some_and(|r| r.contains("Minecraft version")));
        assert_eq!(nogame.game_version, None);
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
        assert_eq!((batch.name.as_str(), batch.game_version.as_str(), batch.loader_version.as_deref()), ("Imported", "1.21.1", Some("0.16.5")));
        assert_eq!(batch.mods, known);
        assert_eq!(batch.local_mods, ["mine.jar"]);
        let dest = out.path().join(&batch.id);
        let mut mods: Vec<String> = fs::read_dir(dest.join("mods")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        mods.sort();
        assert_eq!(mods, ["mine.jar", "sodium-fabric-0.6.jar"]);
        assert_eq!(fs::read(dest.join("mods").join("sodium-fabric-0.6.jar")).unwrap(), b"sodium bytes");
        assert_eq!(fs::read(dest.join("mods").join("mine.jar")).unwrap(), b"my own mod");
        assert_eq!(fs::read(dest.join("config").join("sodium.json")).unwrap(), b"{}");
        assert_eq!(fs::read(dest.join("config").join("deep").join("er").join("x.toml")).unwrap(), b"x=1");
        assert_eq!(fs::read_to_string(dest.join(OPTIONS_FILE)).unwrap(), "fov:1.0\nsoundCategory_master:0.7\n");
        assert!(!dest.join("servers.dat").exists());
        let saved: Batch = serde_json::from_slice(&fs::read(dest.join("batch.json")).unwrap()).unwrap();
        assert_eq!(saved, batch);
    }

    #[test]
    fn an_instance_without_mods_config_or_options_still_imports() {
        let prism = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let inst = fabric(prism.path(), "bare", "Bare");
        let batch = import_to(&inst, "Bare", out.path(), &lookup_of(Vec::new())).unwrap();
        let dest = out.path().join(&batch.id);
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
        let forge = Instance { unsupported: Some("uses Forge".into()), ..inst.clone() };
        let err = import_to(&forge, "X", out.path(), &none).unwrap_err().to_string();
        assert!(err.contains("Farm") && err.contains("Forge"), "{err}");
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
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn nesting_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().join("from");
        for _ in 0..=MAX_DEPTH + 1 {
            deep = deep.join("d");
        }
        fs::create_dir_all(&deep).unwrap();
        assert!(copy_tree(&dir.path().join("from"), &dir.path().join("to"), 0).is_err());
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
        assert_eq!(done.added, [Account::offline("Steve").unwrap()]);
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
