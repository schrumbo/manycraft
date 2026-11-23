//! Settings instances share through the sync folder: `options.txt` key by key, `config/` file by file and `servers.dat`
//! whole. What the sync folder holds wins whenever an instance starts; a game's changes flow back only after it loaded,
//! so a file a game resets or recreates while starting never becomes the shared state. Every replaced version is kept.

mod history;
mod keys;
mod track;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::i18n::{tr, trf};
use super::instance::{self, Instance};
use super::sync::{OPTIONS_FILE, SERVERS_FILE};
use super::{lock, net, paths};

use history::History;
pub use history::Version;
pub(crate) use track::{Start, Tracker};

/// Folder in the data folder that holds what sharing instances share.
pub const DIR: &str = "sync";
const LOCK: &str = ".lock";
const CONFIG: &str = "config";
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// Which items an instance shares with the other sharing instances. An instance file without it shares nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharing {
    pub options: bool,
    pub config: bool,
    pub servers: bool,
}

impl Sharing {
    /// Every item: what a new instance shares.
    pub const ALL: Self = Self { options: true, config: true, servers: true };
    /// Nothing: what an instance from before sharing existed shares.
    pub const NONE: Self = Self { options: false, config: false, servers: false };

    /// Whether `item` is shared.
    pub fn get(self, item: Item) -> bool {
        match item {
            Item::Options => self.options,
            Item::Config => self.config,
            Item::Servers => self.servers,
        }
    }

    /// These choices with `item` shared or not.
    pub fn with(mut self, item: Item, on: bool) -> Self {
        match item {
            Item::Options => self.options = on,
            Item::Config => self.config = on,
            Item::Servers => self.servers = on,
        }
        self
    }

    /// Whether anything is shared.
    pub fn any(self) -> bool {
        self.options || self.config || self.servers
    }

    /// Whether nothing is shared, which an instance file leaves out.
    pub fn is_none(&self) -> bool {
        !self.any()
    }
}

/// What an instance can share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Item {
    Options,
    Config,
    Servers,
}

/// Every item, in the order the settings list them.
pub const ITEMS: [Item; 3] = [Item::Options, Item::Config, Item::Servers];

impl Item {
    fn rel(self) -> &'static str {
        match self {
            Self::Options => OPTIONS_FILE,
            Self::Config => CONFIG,
            Self::Servers => SERVERS_FILE,
        }
    }

    fn holds(self, rel: &str) -> bool {
        match self {
            Self::Config => rel.starts_with("config/"),
            _ => rel == self.rel(),
        }
    }
}

/// The sync folder.
#[derive(Clone, Debug)]
pub struct Mother {
    root: PathBuf,
}

impl Mother {
    /// The sync folder in the data folder.
    pub fn current() -> Self {
        Self::at(paths::data_dir().join(DIR))
    }

    pub(crate) fn at(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, part| p.join(part))
    }

    fn lock(&self) -> Result<lock::Guard> {
        lock::exclusive_within(&self.root.join(LOCK), LOCK_WAIT)
    }
}

/// An instance as sharing sees it: who it is for the history and where its own copies live.
#[derive(Clone, Debug)]
pub(crate) struct Own {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
}

impl Own {
    pub(crate) fn of(instance: &Instance) -> Self {
        Self { id: instance.id.clone(), name: instance.name.clone(), dir: instance.dir() }
    }

    fn file(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.dir.clone(), |p, part| p.join(part))
    }
}

/// What the instance in folder `dir` shares now, read from its file; nothing when it cannot be read. Game options only
/// count for games whose options can travel, see [`options_travel`].
pub(crate) fn flags_in(dir: &Path) -> Sharing {
    let Ok(text) = fs::read_to_string(dir.join(instance::FILE)) else { return Sharing::NONE };
    let Ok(file) = serde_json::from_str::<serde_json::Value>(&text) else { return Sharing::NONE };
    let mut flags: Sharing = file.get("sync").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or(Sharing::NONE);
    flags.options &= file.get("game_version").and_then(serde_json::Value::as_str).is_some_and(options_travel);
    flags
}

/// Whether the game options of Minecraft `game` can be shared. Key names and values changed with 1.13 (snapshot 17w43a),
/// so older games keep their own options: nothing of theirs reaches the shared ones and nothing shared reaches them.
pub fn options_travel(game: &str) -> bool {
    let number = |s: &str| s.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u32>().ok();
    if !game.contains('.')
        && let Some((year, week)) = game.split_once('w')
    {
        return year.parse::<u32>().ok().zip(number(week)).is_some_and(|w| w >= (17, 43));
    }
    let Some((major, rest)) = game.split_once('.') else { return false };
    match (major.parse::<u32>(), number(rest)) {
        (Ok(1), Some(minor)) => minor >= 13,
        (Ok(major), Some(_)) => major > 1,
        _ => false,
    }
}

/// Before a game of `instance` starts: what the sync folder holds wins over the instance's own copy of every item the
/// instance shares, and an item the sync folder lacks is taken from the instance. With `joining`, a game of the
/// instance already runs and its `config/` folder stays as it is. Fails when the instance's copy cannot be made to match.
pub fn inbound(instance: &Instance, joining: bool) -> Result<()> {
    inbound_in(&Mother::current(), &Own::of(instance), joining)
}

pub(crate) fn inbound_in(mother: &Mother, own: &Own, joining: bool) -> Result<()> {
    let flags = flags_in(&own.dir);
    if !flags.any() || !own.dir.is_dir() {
        return Ok(());
    }
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        kept(mother, |history| {
            if flags.options {
                options_in(mother, own, history)?;
            }
            if flags.servers {
                servers_in(mother, own, history)?;
            }
            if flags.config && !joining {
                config_in(mother, own, history)?;
            }
            Ok(())
        })
    })
}

/// Runs `work` on the history and records what it kept even when `work` fails halfway, so no kept version is lost.
fn kept<T>(mother: &Mother, work: impl FnOnce(&mut History) -> Result<T>) -> Result<T> {
    let mut history = History::open(mother)?;
    let done = work(&mut history);
    let saved = history.save();
    let value = done?;
    saved?;
    Ok(value)
}

fn options_in(mother: &Mother, own: &Own, history: &mut History) -> Result<()> {
    let template = own.file(OPTIONS_FILE);
    let raw = read_file(&template)?;
    let mine = raw.as_deref().map(|b| String::from_utf8_lossy(b).into_owned());
    let Some(theirs) = read_text(&mother.path(OPTIONS_FILE))? else {
        if let Some(mine) = mine.filter(|t| keys::readable(t)) {
            put_locked(mother, history, own, vec![(OPTIONS_FILE.to_owned(), keys::shared(&mine).into_bytes())])?;
        }
        return Ok(());
    };
    let wanted: BTreeMap<String, String> = keys::map(&theirs).into_iter().filter(|(k, _)| !keys::excluded(k)).collect();
    let base = mine.clone().filter(|t| keys::readable(t)).unwrap_or_default();
    if keys::covers(&base, &wanted) && mine.is_some() {
        return Ok(());
    }
    if let Some(raw) = &raw {
        history.backup(own, OPTIONS_FILE, raw)?;
    }
    let merged = keys::merge(&base, &theirs);
    settle(&template, merged.as_bytes(), || read_text(&template).ok().flatten().is_some_and(|t| keys::covers(&t, &wanted)), tr("Die geteilten Spiel-Optionen ließen sich nicht in die Instanz schreiben."))
}

fn servers_in(mother: &Mother, own: &Own, history: &mut History) -> Result<()> {
    let template = own.file(SERVERS_FILE);
    let mine = read_file(&template)?;
    let Some(theirs) = read_file(&mother.path(SERVERS_FILE))? else {
        if let Some(mine) = mine.filter(|b| !b.is_empty()) {
            put_locked(mother, history, own, vec![(SERVERS_FILE.to_owned(), mine)])?;
        }
        return Ok(());
    };
    if mine.as_deref() == Some(theirs.as_slice()) {
        return Ok(());
    }
    if let Some(mine) = &mine {
        history.backup(own, SERVERS_FILE, mine)?;
    }
    settle(&template, &theirs, || read_file(&template).ok().flatten().as_deref() == Some(theirs.as_slice()), tr("Die geteilte Serverliste ließ sich nicht in die Instanz schreiben."))
}

fn config_in(mother: &Mother, own: &Own, history: &mut History) -> Result<()> {
    let shared = files_in(&mother.path(CONFIG))?;
    if shared.is_empty() {
        let mine = files_in(&own.file(CONFIG))?;
        if !mine.is_empty() {
            let files = mine.into_iter().map(|(rel, path)| Ok((format!("{CONFIG}/{rel}"), fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?))).collect::<Result<Vec<_>>>()?;
            put_locked(mother, history, own, files)?;
        }
        return Ok(());
    }
    for (rel, path) in shared {
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let full = format!("{CONFIG}/{rel}");
        if through_link(&own.file(CONFIG), &rel) {
            continue;
        }
        let target = own.file(&full);
        let mine = read_file(&target)?;
        if mine.as_deref() == Some(bytes.as_slice()) {
            continue;
        }
        if let Some(mine) = &mine {
            history.backup(own, &full, mine)?;
        }
        settle(&target, &bytes, || read_file(&target).ok().flatten().as_deref() == Some(bytes.as_slice()), trf("Die geteilte Mod-Einstellung „%@“ ließ sich nicht in die Instanz schreiben.", &[&rel]))?;
    }
    Ok(())
}

/// Writes `bytes` to `path` until `holds` says it arrived: once, then once more after making the file writable.
fn settle(path: &Path, bytes: &[u8], holds: impl Fn() -> bool, why: String) -> Result<()> {
    if net::write_atomic(path, bytes).is_ok() && holds() {
        return Ok(());
    }
    writable(path);
    match net::write_atomic(path, bytes) {
        Ok(()) if holds() => Ok(()),
        Ok(()) => bail!("{why}"),
        Err(e) => Err(e.context(why)),
    }
}

/// Whether writing `rel` under `base` would go through a linked folder, out of the instance.
fn through_link(base: &Path, rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut at = base.to_path_buf();
    is_link(&at)
        || parts[..parts.len().saturating_sub(1)].iter().any(|part| {
            at.push(part);
            is_link(&at)
        })
}

/// Makes the file at `path` one a write can replace: a link is removed and a read-only file made writable.
pub(crate) fn writable(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else { return };
    if meta.file_type().is_symlink() {
        let _ = fs::remove_file(path);
        return;
    }
    let mut perms = meta.permissions();
    if perms.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(path, perms);
    }
}

/// Right before a game starts: makes sure its `options.txt` at `game` holds every key of the instance `template` but the
/// per-game ones and a data version, rebuilding a missing or unreadable file from the template with `fallback` as the
/// version when nothing else names one. Tries again writable once, then refuses the start.
pub fn verify_game(template: &Path, game: &Path, fallback: Option<u32>) -> Result<()> {
    let Some(ours) = read_text(template)? else { return Ok(()) };
    let wanted: BTreeMap<String, String> = keys::map(&ours).into_iter().filter(|(k, _)| k != "version" && !super::sync::PER_INSTANCE_KEYS.contains(&k.as_str())).collect();
    let existing = read_text(game).ok().flatten();
    let versioned = existing.as_deref().and_then(keys::version).or_else(|| keys::version(&ours)).or(fallback).is_some();
    let holds = || read_text(game).ok().flatten().is_some_and(|t| keys::readable(&t) && keys::covers(&t, &wanted) && (!versioned || keys::version(&t).is_some()));
    if holds() {
        return Ok(());
    }
    let text = keys::game_file(&ours, existing.as_deref(), fallback);
    settle(game, text.as_bytes(), holds, tr("Die Spiel-Optionen dieses Accounts ließen sich nicht vorbereiten. Prüf, ob die Datei „options.txt“ im Spielordner beschreibbar ist."))
}

/// Saves which items instance `id` shares and returns it. An item turned on first keeps the instance's own copy in the
/// history and, when the sync folder lacks it, becomes it; otherwise the sync folder wins at the next start.
pub fn set(id: &str, sharing: Sharing) -> Result<Instance> {
    set_in(&Mother::current(), &instance::instances_dir(), id, sharing)
}

pub(crate) fn set_in(mother: &Mother, root: &Path, id: &str, sharing: Sharing) -> Result<Instance> {
    let mut made = find(root, id)?;
    let on: Vec<Item> = ITEMS.into_iter().filter(|i| sharing.get(*i) && !made.sync.get(*i) && travels(&made, *i)).collect();
    join(mother, &Own { id: made.id.clone(), name: made.name.clone(), dir: instance::dir_in(root, id) }, &on)?;
    made.sync = sharing;
    save(root, made)
}

/// Turns sharing of every item on for the instances `ids`, the most recently played first, so that it fills what the
/// sync folder lacks and the others keep their own copies in the history.
pub fn turn_on(ids: &[String]) -> Result<Vec<Instance>> {
    turn_on_in(&Mother::current(), &instance::instances_dir(), ids)
}

pub(crate) fn turn_on_in(mother: &Mother, root: &Path, ids: &[String]) -> Result<Vec<Instance>> {
    let mut chosen: Vec<(Instance, PathBuf, Option<std::time::SystemTime>)> = instance::load_all_in(root)?
        .into_iter()
        .filter(|i| ids.contains(&i.id))
        .map(|i| {
            let dir = instance::dir_in(root, &i.id);
            let played = played(&dir);
            (i, dir, played)
        })
        .collect();
    chosen.sort_by(|a, b| b.2.cmp(&a.2));
    let mut done = Vec::new();
    for (mut made, dir, _) in chosen {
        let on: Vec<Item> = ITEMS.into_iter().filter(|i| !made.sync.get(*i) && travels(&made, *i)).collect();
        join(mother, &Own { id: made.id.clone(), name: made.name.clone(), dir }, &on)?;
        made.sync = Sharing::ALL;
        done.push(save(root, made)?);
    }
    Ok(done)
}

fn travels(made: &Instance, item: Item) -> bool {
    item != Item::Options || options_travel(&made.game_version)
}

fn played(dir: &Path) -> Option<std::time::SystemTime> {
    fs::read_dir(instance::games_dir(dir)).ok()?.filter_map(Result::ok).filter_map(|e| fs::metadata(e.path().join("logs").join("latest.log")).and_then(|m| m.modified()).ok()).max()
}

fn find(root: &Path, id: &str) -> Result<Instance> {
    instance::load_all_in(root)?.into_iter().find(|i| i.id == id).with_context(|| tr("Diese Instanz gibt es nicht mehr."))
}

fn save(root: &Path, made: Instance) -> Result<Instance> {
    match instance::save_in(root, &made) {
        Ok(()) => Ok(made),
        Err(_) => {
            let mut fresh = find(root, &made.id)?;
            fresh.sync = made.sync;
            instance::save_in(root, &fresh)?;
            Ok(fresh)
        }
    }
}

/// Keeps the own copy of each of `items` of `own` and fills what the sync folder lacks from it.
fn join(mother: &Mother, own: &Own, items: &[Item]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        kept(mother, |history| {
            for item in items {
                let mine = own_files(own, *item)?;
                for (rel, bytes) in &mine {
                    history.backup(own, rel, bytes)?;
                }
                if !present(mother, *item)? && !mine.is_empty() {
                    let up = match item {
                        Item::Options => mine.into_iter().map(|(rel, b)| (rel, keys::shared(&String::from_utf8_lossy(&b)).into_bytes())).collect(),
                        _ => mine,
                    };
                    put_locked(mother, history, own, up)?;
                }
            }
            Ok(())
        })
    })
}

fn present(mother: &Mother, item: Item) -> Result<bool> {
    Ok(match item {
        Item::Config => !files_in(&mother.path(CONFIG))?.is_empty(),
        _ => fs::symlink_metadata(mother.path(item.rel())).is_ok(),
    })
}

fn own_files(own: &Own, item: Item) -> Result<Vec<(String, Vec<u8>)>> {
    match item {
        Item::Config => files_in(&own.file(CONFIG))?.into_iter().map(|(rel, path)| Ok((format!("{CONFIG}/{rel}"), fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?))).collect(),
        _ => Ok(read_file(&own.file(item.rel()))?.map(|b| (item.rel().to_owned(), b)).into_iter().collect()),
    }
}

/// The earlier versions of `item`, newest first.
pub fn versions(item: Item) -> Result<Vec<Version>> {
    history::versions(&Mother::current(), item)
}

/// Makes `version` the shared state again; what it replaces is kept like any other version.
pub fn restore(version: &Version) -> Result<()> {
    restore_in(&Mother::current(), version)
}

pub(crate) fn restore_in(mother: &Mother, version: &Version) -> Result<()> {
    let files = history::files(mother, version)?;
    let source = Own { id: String::new(), name: version.source.clone().map_or_else(|| tr("Früherer Stand"), |s| trf("Früherer Stand von %@", &[&s])), dir: PathBuf::new() };
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        kept(mother, |history| put_locked(mother, history, &source, files).map(|_| ()))
    })
}

/// Sets the keys of `changes` the sync folder may hold in its `options.txt` and returns the ones that differed.
pub(crate) fn put_options(mother: &Mother, source: &Own, changes: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        let now = read_text(&mother.path(OPTIONS_FILE))?.unwrap_or_default();
        let up = keys::upward(changes, &keys::map(&now));
        if up.is_empty() {
            return Ok(up);
        }
        kept(mother, |history| put_locked(mother, history, source, vec![(OPTIONS_FILE.to_owned(), keys::apply(&now, &up).into_bytes())]))?;
        Ok(up)
    })
}

/// Writes `files`, by path relative to the sync folder, keeping each version they replace; returns how many changed.
pub(crate) fn put_files(mother: &Mother, source: &Own, files: Vec<(String, Vec<u8>)>) -> Result<usize> {
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        kept(mother, |history| put_locked(mother, history, source, files))
    })
}

fn put_locked(mother: &Mother, history: &mut History, source: &Own, files: Vec<(String, Vec<u8>)>) -> Result<usize> {
    let mut changed = 0;
    for (rel, bytes) in files {
        let path = mother.path(&rel);
        let old = read_file(&path)?;
        if old.as_deref() == Some(bytes.as_slice()) {
            continue;
        }
        if let Some(old) = &old {
            history.keep(&rel, old)?;
        }
        net::write_atomic(&path, &bytes)?;
        history.wrote(&rel, source);
        changed += 1;
    }
    Ok(changed)
}

fn read_file(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(None),
        Err(e) if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) => {
            let _ = e;
            Ok(None)
        }
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// The text at `path`; `None` when it is missing or not text.
fn read_text(path: &Path) -> Result<Option<String>> {
    Ok(read_file(path)?.map(|b| String::from_utf8_lossy(&b).into_owned()))
}

/// Every regular file under `dir` by its path relative to it with `/` between parts; links are not followed.
fn files_in(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut todo = vec![(dir.to_path_buf(), String::new())];
    while let Some((at, prefix)) = todo.pop() {
        let entries = match fs::read_dir(&at) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound && at == dir => return Ok(out),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", at.display())),
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read {}", at.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let kind = entry.file_type().with_context(|| format!("cannot read {}", entry.path().display()))?;
            if kind.is_dir() && !is_link(&entry.path()) {
                todo.push((entry.path(), rel));
            } else if kind.is_file() {
                out.push((rel, entry.path()));
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(windows)]
fn is_link(path: &Path) -> bool {
    junction::exists(path).unwrap_or(false) || fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(not(windows))]
fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}
/// The data version of the game in `classpath`, read from `version.json` in the client jar named `names`; kept per jar.
pub fn data_version(classpath: &[PathBuf], names: &[&str]) -> Option<u32> {
    static KNOWN: std::sync::Mutex<BTreeMap<PathBuf, Option<u32>>> = std::sync::Mutex::new(BTreeMap::new());
    let jar = classpath.iter().find(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| names.contains(&s)))?;
    let mut known = KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = known.get(jar) {
        return *found;
    }
    let found = (|| {
        let mut zip = zip::ZipArchive::new(fs::File::open(jar).ok()?).ok()?;
        let entry = zip.by_name("version.json").ok()?;
        let value: serde_json::Value = serde_json::from_reader(entry).ok()?;
        value.get("world_version")?.as_u64().and_then(|v| u32::try_from(v).ok())
    })();
    known.insert(jar.clone(), found);
    found
}

/// The name in the `instance.json` in `dir`, or its folder name.
pub(crate) fn name_in(dir: &Path) -> String {
    fs::read_to_string(dir.join(instance::FILE)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v.get("name")?.as_str().map(str::to_owned)).unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

#[cfg(test)]
mod tests;