//! Packs in an instance: where each kind of project goes, which pack files a catalog knows by hash, and installing,
//! updating and removing them. [`Catalog`] is the seam for a second source next to Modrinth.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};

use super::batch::ModRef;
use super::instance::Loader;
use super::modrinth::{self, Details, Filter, Kind, Page, Search, Update, Version};
use super::{i18n, net, paths, trash};

const DATAPACKS: &str = "datapacks";
const GLOBAL_PACK_MODS: [&str; 2] = [GLOBAL_PACKS, "gEUERjxK"];
const GLOBAL_PACK_NAMES: [&str; 4] = ["global packs", "globalpacks", "global_packs", "data loader"];
const SHADER_MODS: [&str; 2] = ["YL57xq9U", "GchcoXML"];
const SHADER_NAMES: [&str; 2] = ["iris", "oculus"];
const SHADER_SETTINGS: [&str; 2] = ["iris.properties", "oculus.properties"];
const OPTION_KEYS: [&str; 2] = ["resourcePacks:", "incompatibleResourcePacks:"];
const CDN: &str = "https://cdn.modrinth.com/";
const SHOT_WIDTH: u32 = 960;
const SHOT_LARGE_WIDTH: u32 = 2560;
const KEPT_LARGE: usize = 40;
const MAX_SIDE: u32 = 8192;
const MAX_DECODED: u64 = 256 << 20;
const MAX_ICON: u64 = 4 << 20;

static HASHES: LazyLock<Mutex<HashMap<PathBuf, (u64, SystemTime, String)>>> = LazyLock::new(Mutex::default);

/// A source of projects: search, project pages, versions, files and lookups by hash.
pub trait Catalog: Sync {
    fn search(&self, search: &Search, offset: usize) -> Result<Page>;
    fn details(&self, project_id: &str) -> Result<(Details, Option<SystemTime>)>;
    fn versions(&self, project_id: &str) -> Result<(Vec<Version>, Option<SystemTime>)>;
    fn titles(&self, ids: &[String]) -> Result<HashMap<String, String>>;
    fn file_for(&self, project_id: &str, title: &str, filter: &Filter) -> Result<ModRef>;
    fn updates(&self, filter: &Filter, current: &[ModRef]) -> Result<Vec<Update>>;
    fn by_hash(&self, hashes: &[String]) -> Result<HashMap<String, ModRef>>;
}

/// Modrinth as a [`Catalog`].
pub struct Modrinth;

impl Catalog for Modrinth {
    fn search(&self, search: &Search, offset: usize) -> Result<Page> {
        modrinth::search_for(search, offset)
    }

    fn details(&self, project_id: &str) -> Result<(Details, Option<SystemTime>)> {
        modrinth::details(project_id)
    }

    fn versions(&self, project_id: &str) -> Result<(Vec<Version>, Option<SystemTime>)> {
        modrinth::versions(project_id)
    }

    fn titles(&self, ids: &[String]) -> Result<HashMap<String, String>> {
        modrinth::titles(ids)
    }

    fn file_for(&self, project_id: &str, title: &str, filter: &Filter) -> Result<ModRef> {
        modrinth::file_for(project_id, title, filter)
    }

    fn updates(&self, filter: &Filter, current: &[ModRef]) -> Result<Vec<Update>> {
        modrinth::updates_for(filter, current)
    }

    fn by_hash(&self, hashes: &[String]) -> Result<HashMap<String, ModRef>> {
        modrinth::match_hashes(hashes)
    }
}

/// Where packs of `kind` go in the instance folder `instance_dir`: the shared resource or shader pack folder, the
/// `datapacks` folder of the world folder `world`, or without a world the shared data pack folder that a mod like
/// Global Packs loads into every new world. Mods and modpacks have no pack folder.
pub fn pack_dir(instance_dir: &Path, kind: Kind, world: Option<&Path>) -> Option<PathBuf> {
    match kind {
        Kind::ResourcePack => Some(instance_dir.join("resourcepacks")),
        Kind::Shader => Some(instance_dir.join("shaderpacks")),
        Kind::DataPack => Some(world.map_or_else(|| instance_dir.join(DATAPACKS), |w| w.join(DATAPACKS))),
        Kind::Mod | Kind::Modpack => None,
    }
}

/// The Modrinth id of Global Packs, which loads the shared data pack folder into every new world.
pub const GLOBAL_PACKS: &str = "NRLPy2mk";

/// Whether `mods` or the instance's own files `local_mods` have a mod that loads the shared data pack folder into
/// every new world.
pub fn loads_global_packs(mods: &[ModRef], local_mods: &[String]) -> bool {
    has_mod(mods, local_mods, &GLOBAL_PACK_MODS, &GLOBAL_PACK_NAMES)
}

/// The shader loader among `mods` and `local_mods`, as Modrinth names the shader packs for it: Iris and Oculus
/// both load `iris` packs.
pub fn shader_loader(mods: &[ModRef], local_mods: &[String]) -> Option<&'static str> {
    has_mod(mods, local_mods, &SHADER_MODS, &SHADER_NAMES).then_some("iris")
}

/// The Modrinth id and name of the mod that loads shader packs on `loader`: Oculus on Forge, Iris on the others,
/// none on Vanilla.
pub fn shader_mod(loader: &Loader) -> Option<(&'static str, &'static str)> {
    match loader {
        Loader::Vanilla => None,
        Loader::Forge(_) => Some((SHADER_MODS[1], "Oculus")),
        Loader::Fabric(_) | Loader::Quilt(_) | Loader::NeoForge(_) => Some((SHADER_MODS[0], "Iris")),
    }
}

fn has_mod(mods: &[ModRef], local_mods: &[String], ids: &[&str], names: &[&str]) -> bool {
    let named = |name: &str| {
        let lower = name.to_lowercase();
        names.iter().any(|n| lower.starts_with(n))
    };
    mods.iter().any(|m| ids.contains(&m.project_id.as_str()) || named(&m.title)) || local_mods.iter().any(|f| named(f))
}

/// One pack file that a catalog knows: its name in the folder and the catalog's file it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub file: String,
    pub found: ModRef,
}

/// The `.zip` files directly in `dir` with their SHA-1, by name. A hash is computed once per path, size and time.
pub fn pack_files(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            if !name.to_lowercase().ends_with(".zip") {
                return None;
            }
            Some((name, sha1_of(&e.path())?))
        })
        .collect();
    files.sort_by_cached_key(|(name, _)| name.to_lowercase());
    files
}

fn sha1_of(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let stamp = (meta.len(), meta.modified().ok()?);
    let hashes = HASHES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((len, at, hash)) = hashes.get(path)
        && (*len, *at) == stamp
    {
        return Some(hash.clone());
    }
    drop(hashes);
    let hash = net::file_sha1(path).ok()?;
    HASHES.lock().unwrap_or_else(PoisonError::into_inner).insert(path.to_owned(), (stamp.0, stamp.1, hash.clone()));
    Some(hash)
}

/// Which of `files` (name and SHA-1) are in `known`, a lookup of lowercase SHA-1 to the catalog's file.
/// One entry per project: of two files of one project the first by name wins.
pub fn match_installed(files: &[(String, String)], known: &HashMap<String, ModRef>) -> Vec<Installed> {
    let mut out: Vec<Installed> = Vec::new();
    for (file, hash) in files {
        let Some(found) = known.get(&hash.to_ascii_lowercase()) else { continue };
        if out.iter().any(|i| i.found.project_id == found.project_id) {
            continue;
        }
        out.push(Installed { file: file.clone(), found: found.clone() });
    }
    out
}

/// The pack files in `dir` that `catalog` knows.
pub fn installed(catalog: &dyn Catalog, dir: &Path) -> Result<Vec<Installed>> {
    let files = pack_files(dir);
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let hashes: Vec<String> = files.iter().map(|(_, h)| h.clone()).collect();
    Ok(match_installed(&files, &catalog.by_hash(&hashes)?))
}

/// Puts `found` into `dir`, verified by hash, and returns its file name. A different file of the same name stays and
/// is refused, unless it is `replacing`, the file this one updates; that one is removed once the new one is in place.
pub fn place(found: &ModRef, dir: &Path, replacing: Option<&str>, download: impl FnOnce(&ModRef, &Path) -> Result<()>) -> Result<String> {
    modrinth::safe_file_name(&found.file_name)?;
    if let Some(old) = replacing {
        modrinth::safe_file_name(old)?;
    }
    let path = dir.join(&found.file_name);
    let same_name = replacing.is_some_and(|old| old.eq_ignore_ascii_case(&found.file_name));
    if path.exists() && !same_name && !net::file_sha1(&path).is_ok_and(|h| h.eq_ignore_ascii_case(&found.sha1)) {
        bail!(super::instance::Final(i18n::trf("Dort liegt schon eine andere Datei „%@“.", &[&found.file_name])));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let fresh = !path.exists();
    download(found, &path)?;
    if let Some(name) = replacing.filter(|_| !same_name) {
        let old = dir.join(name);
        if old.exists() && std::fs::remove_file(&old).is_err() {
            if fresh {
                let _ = std::fs::remove_file(&path);
            }
            bail!(super::instance::Final(i18n::trf("„%@“ ist gerade in Benutzung. Schließe das Spiel und versuche es noch einmal.", &[&name])));
        }
    }
    Ok(found.file_name.clone())
}

/// Downloads `found` to `path` and checks its SHA-1.
pub fn download(found: &ModRef, path: &Path) -> Result<()> {
    download_counted(found, path, &|_, _| {})
}

/// Like [`download`], and tells `progress` how many bytes arrived of how many the server announced.
pub fn download_counted(found: &ModRef, path: &Path, progress: &(dyn Fn(u64, Option<u64>) + Sync)) -> Result<()> {
    net::fetch_counted(&net::agent(None)?, &found.url, path, Some(&found.sha1), progress).with_context(|| format!("cannot download {}", found.title))
}

/// Moves the pack `file` in `dir`, a folder inside `instance_dir`, to the instance's trash and returns the entry
/// [`put_back`] takes.
pub fn remove(instance_dir: &Path, dir: &Path, file: &str) -> Result<String> {
    modrinth::safe_file_name(file)?;
    trash::put(instance_dir, &dir.join(file))
}

/// What [`add_files`] did: the names it copied and the names it left alone because they were taken.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Added {
    pub added: Vec<String>,
    pub taken: Vec<String>,
}

/// Copies the `.zip` files `paths` into `dir`, a folder inside `instance_dir`; other files are ignored. A name that is
/// already there (in any case) is left alone and returned in `taken`; with `replace` the old pack goes to the instance's
/// trash instead and the new one takes its place. Nothing stays changed when it fails.
pub fn add_files(instance_dir: &Path, dir: &Path, paths: &[PathBuf], replace: bool) -> Result<Added> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut present: HashMap<String, String> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .map(|n| (n.to_lowercase(), n))
        .collect();
    let mut done = Added::default();
    let mut copied: Vec<PathBuf> = Vec::new();
    let mut trashed: Vec<String> = Vec::new();
    let result = (|| -> Result<()> {
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if !name.to_lowercase().ends_with(".zip") || modrinth::safe_file_name(name).is_err() || !path.is_file() {
                continue;
            }
            let lower = name.to_lowercase();
            if let Some(old) = present.get(&lower) {
                if done.added.iter().any(|n| n.to_lowercase() == lower) {
                    continue;
                }
                if !replace {
                    if !done.taken.iter().any(|n| n.to_lowercase() == lower) {
                        done.taken.push(name.to_owned());
                    }
                    continue;
                }
                trashed.push(trash::put(instance_dir, &dir.join(old))?);
            }
            let target = dir.join(name);
            let tmp = dir.join(format!("{name}.manycraft-tmp"));
            let placed = std::fs::copy(path, &tmp).with_context(|| format!("cannot copy {}", path.display())).and_then(|_| modrinth::rename(&tmp, &target));
            if let Err(e) = placed {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            copied.push(target);
            present.insert(lower, name.to_owned());
            done.added.push(name.to_owned());
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(done),
        Err(e) => {
            for path in &copied {
                let _ = std::fs::remove_file(path);
            }
            for entry in trashed.iter().rev() {
                let _ = trash::take_back(instance_dir, entry);
            }
            Err(e)
        }
    }
}

/// Puts a pack [`remove`] took back where it was.
pub fn put_back(instance_dir: &Path, entry: &str) -> Result<PathBuf> {
    trash::take_back(instance_dir, entry)
}

/// Points what selects the pack `old` at `new` after an update renamed its file: the resource pack lists of the
/// instance's options template, or the shader pack setting of Iris and Oculus in its shared config.
pub fn rename_selected(instance_dir: &Path, kind: Kind, old: &str, new: &str) -> Result<()> {
    if old == new {
        return Ok(());
    }
    match kind {
        Kind::ResourcePack => super::sync::with_templates(|| rewrite(&instance_dir.join(super::sync::OPTIONS_FILE), |text| renamed_packs(text, old, new))),
        Kind::Shader => {
            for name in SHADER_SETTINGS {
                rewrite(&instance_dir.join("config").join(name), |text| renamed_shader(text, old, new))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The files that say which packs of `kind` are selected, as they read now, for [`restore_selection`].
pub fn saved_selection(instance_dir: &Path, kind: Kind) -> Vec<(PathBuf, String)> {
    let read = || selection_files(instance_dir, kind).into_iter().filter_map(|path| std::fs::read_to_string(&path).ok().map(|text| (path, text))).collect();
    if kind == Kind::ResourcePack { super::sync::with_templates(read) } else { read() }
}

/// Writes back what [`saved_selection`] read wherever it changed since.
pub fn restore_selection(kind: Kind, saved: &[(PathBuf, String)]) -> Result<()> {
    let write = || -> Result<()> {
        for (path, text) in saved {
            if std::fs::read_to_string(path).ok().as_deref() != Some(text.as_str()) {
                net::write_atomic(path, text.as_bytes())?;
            }
        }
        Ok(())
    };
    if kind == Kind::ResourcePack { super::sync::with_templates(write) } else { write() }
}

/// The pack files of `kind` the instance has switched on: its resource packs from the options template, or the shader
/// pack that Iris or Oculus load.
pub fn selected(instance_dir: &Path, kind: Kind) -> Vec<String> {
    match kind {
        Kind::ResourcePack => super::sync::with_templates(|| std::fs::read_to_string(instance_dir.join(super::sync::OPTIONS_FILE)).map(|text| packs_on(&text)).unwrap_or_default()),
        Kind::Shader => SHADER_SETTINGS.iter().find_map(|name| std::fs::read_to_string(instance_dir.join("config").join(name)).ok().and_then(|text| shader_on(&text))).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Switches the pack `file` of `kind` on or off for the instance. A resource pack goes last in the options template,
/// where it wins over the others; a shader pack replaces the one Iris or Oculus had.
pub fn switch(instance_dir: &Path, kind: Kind, file: &str, on: bool, loader: &Loader) -> Result<()> {
    modrinth::safe_file_name(file)?;
    match kind {
        Kind::ResourcePack => super::sync::with_templates(|| {
            let path = instance_dir.join(super::sync::OPTIONS_FILE);
            let text = read_or_empty(&path)?;
            let next = switched_packs(&text, file, on)?;
            if next == text { Ok(()) } else { net::write_atomic(&path, next.as_bytes()) }
        }),
        Kind::Shader => {
            let config = instance_dir.join("config");
            let own = if matches!(loader, Loader::Forge(_)) { SHADER_SETTINGS[1] } else { SHADER_SETTINGS[0] };
            let name = std::iter::once(own).chain(SHADER_SETTINGS).find(|name| config.join(name).is_file()).unwrap_or(own);
            let path = config.join(name);
            let text = read_or_empty(&path)?;
            let next = switched_shader(&text, file, on);
            if next == text {
                return Ok(());
            }
            std::fs::create_dir_all(&config).with_context(|| format!("cannot create {}", config.display()))?;
            net::write_atomic(&path, next.as_bytes())
        }
        _ => Ok(()),
    }
}

fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn pack_list(text: &str) -> Option<(usize, Vec<String>)> {
    text.split('\n').enumerate().find_map(|(at, line)| {
        let body = line.trim_end_matches('\r').strip_prefix(OPTION_KEYS[0])?;
        Some((at, serde_json::from_str::<Vec<String>>(body).ok()?))
    })
}

fn packs_on(text: &str) -> Vec<String> {
    pack_list(text).map(|(_, list)| list.into_iter().filter_map(|p| p.strip_prefix("file/").map(str::to_owned)).collect()).unwrap_or_default()
}

fn switched_packs(text: &str, file: &str, on: bool) -> Result<String> {
    let entry = format!("file/{file}");
    let line_of = |list: &[String], tail: &str| format!("{}{}{tail}", OPTION_KEYS[0], serde_json::to_string(list).unwrap_or_default());
    let Some((at, mut list)) = pack_list(text) else {
        if text.split('\n').any(|l| l.starts_with(OPTION_KEYS[0])) {
            bail!("the resource pack list in options.txt cannot be read");
        }
        if !on {
            return Ok(text.to_owned());
        }
        let line = line_of(&["vanilla".to_owned(), entry], "");
        let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
        return Ok(format!("{text}{sep}{line}\n"));
    };
    list.retain(|p| *p != entry);
    if on {
        list.push(entry);
    }
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let tail = if lines[at].ends_with('\r') { "\r" } else { "" };
    lines[at] = line_of(&list, tail);
    Ok(lines.join("\n"))
}

fn shader_on(text: &str) -> Option<String> {
    text.split('\n').find_map(|line| {
        let (key, value) = line.trim_end_matches('\r').split_once('=')?;
        (key.trim() == "shaderPack").then(|| unescaped(value.trim_start())).filter(|v| !v.is_empty())
    })
}

fn switched_shader(text: &str, file: &str, on: bool) -> String {
    let mut seen = false;
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let body = line.trim_end_matches('\r');
        match body.split_once('=') {
            Some((key, value)) if key.trim() == "shaderPack" => {
                seen = true;
                if on {
                    lines.push(format!("shaderPack={}{}", escaped(file), &line[body.len()..]));
                } else if unescaped(value.trim_start()) != file {
                    lines.push(line.to_owned());
                }
            }
            _ => lines.push(line.to_owned()),
        }
    }
    if on && !seen {
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.push(format!("shaderPack={}", escaped(file)));
        lines.push(String::new());
    }
    lines.join("\n")
}

fn selection_files(instance_dir: &Path, kind: Kind) -> Vec<PathBuf> {
    match kind {
        Kind::ResourcePack => vec![instance_dir.join(super::sync::OPTIONS_FILE)],
        Kind::Shader => SHADER_SETTINGS.iter().map(|name| instance_dir.join("config").join(name)).collect(),
        _ => Vec::new(),
    }
}

fn rewrite(path: &Path, change: impl Fn(&str) -> Option<String>) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    match change(&text) {
        Some(next) => net::write_atomic(path, next.as_bytes()),
        None => Ok(()),
    }
}

fn renamed_packs(text: &str, old: &str, new: &str) -> Option<String> {
    let (from, to) = (format!("file/{old}"), format!("file/{new}"));
    let mut changed = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let body = line.trim_end_matches('\r');
            let Some(key) = OPTION_KEYS.iter().find(|k| body.starts_with(*k)) else { return line.to_owned() };
            let Ok(mut packs) = serde_json::from_str::<Vec<String>>(&body[key.len()..]) else { return line.to_owned() };
            if !packs.contains(&from) {
                return line.to_owned();
            }
            changed = true;
            for pack in packs.iter_mut().filter(|p| **p == from) {
                pack.clone_from(&to);
            }
            format!("{key}{}{}", serde_json::to_string(&packs).unwrap_or_default(), &line[body.len()..])
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

fn renamed_shader(text: &str, old: &str, new: &str) -> Option<String> {
    let mut changed = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| match line.trim_end_matches('\r').split_once('=') {
            Some((key, value)) if key.trim() == "shaderPack" && unescaped(value.trim_start()) == old => {
                changed = true;
                format!("shaderPack={}{}", escaped(new), if line.ends_with('\r') { "\r" } else { "" })
            }
            _ => line.to_owned(),
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

fn unescaped(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut units: Vec<u16> = Vec::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.clone().next() == Some('u') {
            chars.next();
            let hex: String = chars.by_ref().take(4).collect();
            units.extend(u16::from_str_radix(&hex, 16).ok());
            continue;
        }
        out.extend(char::decode_utf16(units.drain(..)).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)));
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{c}'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out.extend(char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)));
    out
}

fn escaped(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (i, c) in value.chars().enumerate() {
        match c {
            ':' | '=' | '#' | '!' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            ' ' if i == 0 => out.push_str("\\ "),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04X}"));
                }
            }
        }
    }
    out
}

/// Downloads the `.mrpack` of a modpack to the cache folder for an import and returns its path.
pub fn fetch_modpack(found: &ModRef, progress: &(dyn Fn(u64, Option<u64>) + Sync)) -> Result<PathBuf> {
    modrinth::safe_file_name(&found.file_name)?;
    let path = paths::data_dir().join("cache").join("modpacks").join(&found.file_name);
    download_counted(found, &path, progress)?;
    Ok(path)
}

/// A project description reduced to the Markdown the browser shows: no pictures, no HTML tags (their text stays),
/// common entities decoded, at most one empty line in a row. Code blocks stay as they are.
pub fn readable(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fenced = false;
    for line in body.replace("\r\n", "\n").split('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if fenced {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        out.push_str(&entities(&strip_tags(&strip_images(line))));
        out.push('\n');
    }
    let mut squeezed = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.split('\n') {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
            squeezed.push('\n');
        } else {
            blank = 0;
            squeezed.push_str(line.trim_end());
            squeezed.push('\n');
        }
    }
    squeezed.trim().to_owned()
}

fn strip_images(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("![") {
        let linked = rest[..at].ends_with('[');
        out.push_str(&rest[..if linked { at - 1 } else { at }]);
        let after = &rest[at..];
        let Some(close) = after.find("](").and_then(|open| after[open..].find(')').map(|c| open + c + 1)) else {
            out.push_str(after);
            return out;
        };
        rest = &after[close..];
        if linked && rest.starts_with("](") {
            rest = rest.find(')').map_or("", |c| &rest[c + 1..]);
        }
        if out.trim().is_empty() {
            out.clear();
            rest = rest.trim_start();
        }
    }
    out.push_str(rest);
    out
}

fn strip_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[at..]);
            return out;
        };
        let tag = &after[..end];
        let opens = tag.starts_with('/') || tag.starts_with('!') || tag.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
        if tag.starts_with("http://") || tag.starts_with("https://") {
            out.push_str(tag);
        } else if opens && !tag.contains('<') {
            let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
            if (name == "br" || name == "p" || name == "div") && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push('<');
            rest = after;
            continue;
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn entities(text: &str) -> String {
    text.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// A gallery picture as a file in the cache folder, at most `SHOT_WIDTH` wide. Blocking: run it off the UI thread.
pub fn shot_file(url: &str) -> Result<PathBuf> {
    if !url.starts_with(CDN) {
        bail!("picture is not on Modrinth's CDN");
    }
    cached_picture("gallery", url, SHOT_WIDTH, |url| net::get_bytes(&net::agent(None)?, url))
}

/// A gallery picture as a file in the cache folder, at most `SHOT_LARGE_WIDTH` wide; the newest `KEPT_LARGE` stay.
/// Blocking: run it off the UI thread.
pub fn shot_file_large(url: &str) -> Result<PathBuf> {
    if !url.starts_with(CDN) {
        bail!("picture is not on Modrinth's CDN");
    }
    let path = cached_picture("gallery-large", url, SHOT_LARGE_WIDTH, |url| net::get_bytes(&net::agent(None)?, url))?;
    if let Ok(file) = std::fs::File::options().write(true).open(&path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
    if let Some(dir) = path.parent() {
        super::modrinth::prune(dir, KEPT_LARGE);
    }
    Ok(path)
}

/// A picture of a project description as a file in the cache folder, at most `SHOT_WIDTH` wide.
/// Only public web hosts are fetched. Blocking: run it off the UI thread.
pub fn readme_picture(url: &str) -> Result<PathBuf> {
    if !public_https(url) {
        bail!("picture is not on a public https host");
    }
    cached_picture("readme", url, SHOT_WIDTH, |url| net::get_bytes_vetted(url, public_https))
}

/// The picture of a pack: its `pack.png`, in a folder as it is, in a zip copied to the cache folder.
/// Blocking: run it off the UI thread.
pub fn icon_file(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        return Some(path.join("pack.png")).filter(|p| p.is_file());
    }
    let meta = std::fs::metadata(path).ok()?;
    let stamp = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let out = paths::data_dir().join("cache").join("packicons").join(format!("{}-{}-{stamp}.png", sha1_hex(&path.to_string_lossy()), meta.len()));
    if out.is_file() {
        return Some(out);
    }
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let entry = archive.by_name("pack.png").ok()?;
    if entry.size() > MAX_ICON {
        return None;
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(entry, MAX_ICON), &mut bytes).ok()?;
    net::write_atomic(&out, &bytes).ok()?;
    Some(out)
}

fn cached_picture(folder: &str, url: &str, width: u32, fetch: impl FnOnce(&str) -> Result<Vec<u8>>) -> Result<PathBuf> {
    let path = paths::data_dir().join("cache").join(folder).join(format!("{}-{width}.png", sha1_hex(url)));
    if path.is_file() {
        return Ok(path);
    }
    let bytes = fetch(url)?;
    net::write_atomic(&path, &shrink(&bytes, width)?)?;
    Ok(path)
}

fn public_https(url: &str) -> bool {
    let Some(rest) = url.get(..8).filter(|s| s.eq_ignore_ascii_case("https://")).map(|_| &url[8..]) else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| if port.chars().all(|c| c.is_ascii_digit()) { h } else { authority }).to_ascii_lowercase();
    let private_suffix = [".local", ".localhost", ".internal", ".lan", ".home", ".corp"].iter().any(|s| host.ends_with(s));
    host.contains('.') && !host.starts_with('[') && !private_suffix && host.parse::<std::net::IpAddr>().is_err() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn shrink(bytes: &[u8], width: u32) -> Result<Vec<u8>> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED);
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits);
    let full = reader.decode().context("not a supported picture")?;
    let small = if full.width() > width { full.resize(width, MAX_SIDE, FilterType::Triangle) } else { full };
    let mut out = std::io::Cursor::new(Vec::new());
    small.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn sha1_hex(text: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(project: &str, file: &str, sha1: &str) -> ModRef {
        ModRef { project_id: project.into(), version_id: format!("v-{project}"), title: project.into(), file_name: file.into(), url: String::new(), sha1: sha1.into() }
    }

    #[test]
    fn readme_pictures_come_only_from_public_https_hosts() {
        assert!(public_https("https://raw.githubusercontent.com/a/b/main/x.png"));
        assert!(public_https("HTTPS://cdn.modrinth.com:443/x.png?v=1"));
        assert!(!public_https("http://example.org/x.png"));
        assert!(!public_https("https://localhost/x.png"));
        assert!(!public_https("https://printer.local/x.png"));
        assert!(!public_https("https://127.0.0.1/x.png"));
        assert!(!public_https("https://192.168.0.2:8080/x.png"));
        assert!(!public_https("https://[::1]/x.png"));
        assert!(!public_https("https://user@example.org/x.png"));
        assert!(!public_https("https://intranet/x.png"));
        assert!(!public_https("file:///etc/passwd"));
        assert!(!public_https("https://"));
    }

    #[test]
    fn each_kind_goes_to_its_folder() {
        let dir = Path::new("inst");
        assert_eq!(pack_dir(dir, Kind::ResourcePack, None), Some(dir.join("resourcepacks")));
        assert_eq!(pack_dir(dir, Kind::Shader, Some(Path::new("w"))), Some(dir.join("shaderpacks")));
        let world = dir.join("games").join("a").join("saves").join("World");
        assert_eq!(pack_dir(dir, Kind::DataPack, Some(&world)), Some(world.join("datapacks")));
        assert_eq!(pack_dir(dir, Kind::DataPack, None), Some(dir.join("datapacks")));
        assert_eq!(pack_dir(dir, Kind::Mod, None), None);
        assert_eq!(pack_dir(dir, Kind::Modpack, None), None);
    }

    #[test]
    fn installed_packs_are_matched_by_hash_once_per_project() {
        let known: HashMap<String, ModRef> = [("aa".to_owned(), found("fresh", "Fresh.zip", "aa")), ("bb".to_owned(), found("fresh", "Fresh-old.zip", "bb")), ("cc".to_owned(), found("bsl", "BSL.zip", "cc"))].into();
        let files = vec![("A renamed.zip".to_owned(), "AA".to_owned()), ("B.zip".to_owned(), "bb".to_owned()), ("mine.zip".to_owned(), "dd".to_owned()), ("BSL.zip".to_owned(), "cc".to_owned())];
        let got = match_installed(&files, &known);
        assert_eq!(got.iter().map(|i| (i.file.as_str(), i.found.project_id.as_str())).collect::<Vec<_>>(), [("A renamed.zip", "fresh"), ("BSL.zip", "bsl")]);
        assert!(match_installed(&files, &HashMap::new()).is_empty());
    }

    #[test]
    fn pack_files_lists_zips_with_their_hashes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.zip"), b"two").unwrap();
        std::fs::write(dir.path().join("A.ZIP"), b"one").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("folder.zip")).unwrap();
        let files = pack_files(dir.path());
        assert_eq!(files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["A.ZIP", "b.zip"]);
        assert_eq!(files[0].1, net::file_sha1(&dir.path().join("A.ZIP")).unwrap());
        assert!(pack_files(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn placing_refuses_a_foreign_file_and_replaces_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let write = |body: &'static [u8]| move |_: &ModRef, path: &Path| -> Result<()> { Ok(std::fs::write(path, body)?) };
        std::fs::write(dir.path().join("Pack.zip"), b"mine").unwrap();
        assert!(place(&found("p", "Pack.zip", "00"), dir.path(), None, write(b"new")).is_err());
        assert_eq!(std::fs::read(dir.path().join("Pack.zip")).unwrap(), b"mine");
        std::fs::write(dir.path().join("Old.zip"), b"old").unwrap();
        assert_eq!(place(&found("p", "New.zip", "00"), dir.path(), Some("Old.zip"), write(b"new")).unwrap(), "New.zip");
        assert!(!dir.path().join("Old.zip").exists() && dir.path().join("New.zip").is_file());
        assert!(place(&found("p", "../x.zip", "00"), dir.path(), None, write(b"x")).is_err());
        let failing = |_: &ModRef, _: &Path| -> Result<()> { bail!("offline") };
        std::fs::write(dir.path().join("Keep.zip"), b"keep").unwrap();
        assert!(place(&found("p", "Next.zip", "00"), dir.path(), Some("Keep.zip"), failing).is_err());
        assert!(dir.path().join("Keep.zip").is_file());
        std::fs::create_dir_all(dir.path().join("Busy.zip")).unwrap();
        assert!(place(&found("p", "Later.zip", "00"), dir.path(), Some("Busy.zip"), write(b"new")).is_err());
        assert!(dir.path().join("Busy.zip").is_dir() && !dir.path().join("Later.zip").exists());
    }

    #[test]
    fn escaped_selections_are_renamed_too() {
        let inst = tempfile::tempdir().unwrap();
        std::fs::write(inst.path().join("options.txt"), "resourcePacks:[\"vanilla\",\"file/Xali\\u0027s Books.zip\"]\r\nincompatibleResourcePacks:[\"file/Xali\\u0027s Books.zip\"]\r\n").unwrap();
        rename_selected(inst.path(), Kind::ResourcePack, "Xali's Books.zip", "Xali's Books 2.zip").unwrap();
        assert_eq!(std::fs::read_to_string(inst.path().join("options.txt")).unwrap(), "resourcePacks:[\"vanilla\",\"file/Xali's Books 2.zip\"]\r\nincompatibleResourcePacks:[\"file/Xali's Books 2.zip\"]\r\n");
        std::fs::create_dir_all(inst.path().join("config")).unwrap();
        std::fs::write(inst.path().join("config").join("iris.properties"), "shaderPack=Sch\\u00F6ne\\: Shader.zip\n").unwrap();
        rename_selected(inst.path(), Kind::Shader, "Schöne: Shader.zip", "Schöne: Shader 2.zip").unwrap();
        assert_eq!(std::fs::read_to_string(inst.path().join("config").join("iris.properties")).unwrap(), "shaderPack=Sch\\u00F6ne\\: Shader 2.zip\n");
        assert_eq!(unescaped(&escaped(" a=b#c!d\\é😀")), " a=b#c!d\\é😀");
    }

    #[test]
    fn a_resource_pack_goes_on_last_and_off_again() {
        let text = "lang:de_de\r\nresourcePacks:[\"vanilla\",\"file/A.zip\"]\r\nfov:0.5\r\n";
        assert_eq!(packs_on(text), ["A.zip"]);
        let on = switched_packs(text, "B's.zip", true).unwrap();
        assert_eq!(on, "lang:de_de\r\nresourcePacks:[\"vanilla\",\"file/A.zip\",\"file/B's.zip\"]\r\nfov:0.5\r\n");
        assert_eq!(packs_on(&on), ["A.zip", "B's.zip"]);
        assert_eq!(switched_packs(&on, "A.zip", true).unwrap(), "lang:de_de\r\nresourcePacks:[\"vanilla\",\"file/B's.zip\",\"file/A.zip\"]\r\nfov:0.5\r\n");
        assert_eq!(switched_packs(&on, "B's.zip", false).unwrap(), text);
        assert_eq!(switched_packs(text, "Other.zip", false).unwrap(), text);
    }

    #[test]
    fn a_resource_pack_list_that_is_missing_is_made_and_one_that_is_broken_is_left_alone() {
        assert_eq!(switched_packs("", "A.zip", true).unwrap(), "resourcePacks:[\"vanilla\",\"file/A.zip\"]\n");
        assert_eq!(switched_packs("fov:1", "A.zip", true).unwrap(), "fov:1\nresourcePacks:[\"vanilla\",\"file/A.zip\"]\n");
        assert_eq!(switched_packs("fov:1\n", "A.zip", false).unwrap(), "fov:1\n");
        assert!(switched_packs("resourcePacks:[oops\n", "A.zip", true).is_err());
        assert!(packs_on("resourcePacks:[oops\n").is_empty());
    }

    #[test]
    fn one_shader_pack_is_on_at_a_time() {
        let text = "enableShaders=true\nshaderPack=Old\\: One.zip\n";
        assert_eq!(shader_on(text).as_deref(), Some("Old: One.zip"));
        assert_eq!(switched_shader(text, "New.zip", true), "enableShaders=true\nshaderPack=New.zip\n");
        assert_eq!(switched_shader(text, "Old: One.zip", false), "enableShaders=true\n");
        assert_eq!(switched_shader(text, "Other.zip", false), text);
        assert_eq!(switched_shader("a=b", "N.zip", true), "a=b\nshaderPack=N.zip\n");
        assert_eq!(switched_shader("", "N.zip", true), "shaderPack=N.zip\n");
        assert_eq!(switched_shader("shaderPack=\r\n", "N.zip", true), "shaderPack=N.zip\r\n");
        assert!(shader_on("shaderPack=\n").is_none());
    }

    #[test]
    fn switching_writes_the_files_the_game_reads() {
        let inst = tempfile::tempdir().unwrap();
        switch(inst.path(), Kind::ResourcePack, "A.zip", true, &Loader::Vanilla).unwrap();
        assert_eq!(selected(inst.path(), Kind::ResourcePack), ["A.zip"]);
        switch(inst.path(), Kind::ResourcePack, "A.zip", false, &Loader::Vanilla).unwrap();
        assert!(selected(inst.path(), Kind::ResourcePack).is_empty());
        switch(inst.path(), Kind::Shader, "S.zip", true, &Loader::Forge(None)).unwrap();
        assert!(inst.path().join("config").join("oculus.properties").is_file());
        assert_eq!(selected(inst.path(), Kind::Shader), ["S.zip"]);
        switch(inst.path(), Kind::Shader, "T.zip", true, &Loader::Fabric(None)).unwrap();
        assert_eq!(selected(inst.path(), Kind::Shader), ["T.zip"]);
        assert!(!inst.path().join("config").join("iris.properties").exists());
        assert!(switch(inst.path(), Kind::Shader, "..\\x.zip", true, &Loader::Vanilla).is_err());
        assert!(selected(inst.path(), Kind::Mod).is_empty());
    }

    #[test]
    fn a_settings_file_that_cannot_be_read_is_not_overwritten() {
        let inst = tempfile::tempdir().unwrap();
        std::fs::create_dir(inst.path().join(crate::core::sync::OPTIONS_FILE)).unwrap();
        assert!(switch(inst.path(), Kind::ResourcePack, "A.zip", true, &Loader::Vanilla).is_err());
        assert!(inst.path().join(crate::core::sync::OPTIONS_FILE).is_dir());
    }

    #[test]
    fn a_removed_pack_comes_back() {
        let inst = tempfile::tempdir().unwrap();
        let packs = inst.path().join("resourcepacks");
        std::fs::create_dir_all(&packs).unwrap();
        std::fs::write(packs.join("P.zip"), b"x").unwrap();
        let entry = remove(inst.path(), &packs, "P.zip").unwrap();
        assert!(!packs.join("P.zip").exists());
        put_back(inst.path(), &entry).unwrap();
        assert!(packs.join("P.zip").is_file());
        assert!(remove(inst.path(), &packs, "..\\P.zip").is_err());
    }

    #[test]
    fn an_update_keeps_the_pack_selected() {
        let inst = tempfile::tempdir().unwrap();
        std::fs::write(inst.path().join("options.txt"), "lang:de_de\nresourcePacks:[\"vanilla\",\"file/Old 1.zip\"]\nincompatibleResourcePacks:[]\n").unwrap();
        rename_selected(inst.path(), Kind::ResourcePack, "Old 1.zip", "New 2.zip").unwrap();
        assert_eq!(std::fs::read_to_string(inst.path().join("options.txt")).unwrap(), "lang:de_de\nresourcePacks:[\"vanilla\",\"file/New 2.zip\"]\nincompatibleResourcePacks:[]\n");
        std::fs::create_dir_all(inst.path().join("config")).unwrap();
        std::fs::write(inst.path().join("config").join("iris.properties"), "enableShaders=true\r\nshaderPack=BSL v1.zip\r\n").unwrap();
        rename_selected(inst.path(), Kind::Shader, "BSL v1.zip", "BSL v2.zip").unwrap();
        assert_eq!(std::fs::read_to_string(inst.path().join("config").join("iris.properties")).unwrap(), "enableShaders=true\r\nshaderPack=BSL v2.zip\r\n");
        rename_selected(inst.path(), Kind::Shader, "Other.zip", "X.zip").unwrap();
        assert!(!inst.path().join("config").join("oculus.properties").exists());
    }

    #[test]
    fn shader_and_global_pack_mods_are_recognised() {
        let (mut mods, mut local) = (vec![found("sodium", "sodium.jar", "0")], Vec::new());
        assert_eq!(shader_loader(&mods, &local), None);
        assert!(!loads_global_packs(&mods, &local));
        mods.push(found("YL57xq9U", "iris.jar", "0"));
        assert_eq!(shader_loader(&mods, &local), Some("iris"));
        local.push("GlobalPacks-1.2.jar".to_owned());
        assert!(loads_global_packs(&mods, &local));
        assert!(loads_global_packs(&[found(GLOBAL_PACKS, "x.jar", "0")], &[]));
        assert_eq!(shader_loader(&[ModRef { title: "Oculus".into(), ..found("other", "o.jar", "0") }], &[]), Some("iris"));
        assert_eq!(shader_mod(&Loader::Forge(None)).map(|m| m.1), Some("Oculus"));
        assert_eq!(shader_mod(&Loader::NeoForge(None)).map(|m| m.1), Some("Iris"));
        assert_eq!(shader_mod(&Loader::Vanilla), None);
    }

    #[test]
    fn descriptions_lose_pictures_and_tags_but_keep_text_and_code() {
        let body = "<center>\n\n[![Banner](https://cdn.x/b.png)](https://x.org)\n\n# Fresh &amp; new</center>\n\n![shot](https://cdn.x/s.png) Moves <b>mobs</b> 3 < 4<br>more\n\n\n\nSee <https://x.org>.\n```\n<keep> ![this](x)\n```";
        assert_eq!(readable(body), "# Fresh & new\n\nMoves mobs 3 < 4 more\n\nSee https://x.org.\n```\n<keep> ![this](x)\n```");
        assert_eq!(readable("![unclosed](x"), "![unclosed](x");
        assert_eq!(readable("a < b and c > d"), "a < b and c > d");
    }

    fn zip_at(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn added_packs_are_copied_and_a_taken_name_is_left_alone_until_replace_is_asked_for() {
        let tmp = tempfile::tempdir().unwrap();
        let instance = tmp.path().join("inst");
        let dir = instance.join("resourcepacks");
        zip_at(&dir, "Old.zip", b"old");
        let picked = [zip_at(&tmp.path().join("0"), "old.ZIP", b"new"), zip_at(&tmp.path().join("1"), "fresh.zip", b"fresh"), zip_at(&tmp.path().join("2"), "notes.txt", b"x"), tmp.path().join("missing.zip")];
        let first = add_files(&instance, &dir, &picked, false).unwrap();
        assert_eq!(first, Added { added: vec!["fresh.zip".into()], taken: vec!["old.ZIP".into()] });
        assert_eq!(std::fs::read(dir.join("Old.zip")).unwrap(), b"old");
        assert_eq!(std::fs::read(dir.join("fresh.zip")).unwrap(), b"fresh");
        assert!(!dir.join("notes.txt").exists());
        let second = add_files(&instance, &dir, &picked[..1], true).unwrap();
        assert_eq!(second, Added { added: vec!["old.ZIP".into()], taken: Vec::new() });
        assert_eq!(std::fs::read(dir.join("old.ZIP")).unwrap(), b"new");
        let held: Vec<_> = std::fs::read_dir(instance.join(".trash")).unwrap().collect();
        assert_eq!(held.len(), 1);
        assert!(!std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".manycraft-tmp")));
    }

    #[test]
    fn a_failed_pack_copy_leaves_the_folder_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let instance = tmp.path().join("inst");
        let dir = instance.join("shaderpacks");
        zip_at(&dir, "a.zip", b"old-a");
        zip_at(&dir, "b.zip", b"old-b");
        std::fs::create_dir(dir.join("b.zip.manycraft-tmp")).unwrap();
        let picked = [zip_at(&tmp.path().join("0"), "a.zip", b"new-a"), zip_at(&tmp.path().join("1"), "b.zip", b"new-b")];
        assert!(add_files(&instance, &dir, &picked, true).is_err());
        assert_eq!(std::fs::read(dir.join("a.zip")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(dir.join("b.zip")).unwrap(), b"old-b");
    }
}
