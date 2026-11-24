//! Settings instances share through the sync folder: `options.txt` key by key, `config/` file by file and `servers.dat`
//! whole. What the sync folder holds wins whenever an instance starts; a game's changes flow back only after it loaded,
//! so a file a game resets or recreates while starting never becomes the shared state. Every replaced version is kept.

mod history;
mod keys;
mod notes;
mod setup;
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
pub use notes::Note;
pub use setup::{Setup, Standing, Summary, for_new, last_setup, setup, standing, undo_setup};
#[cfg(test)]
use setup::{last_in, setup_in, standing_in, undo_in};
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
    fn key(self) -> &'static str {
        match self {
            Self::Options => "options",
            Self::Config => "config",
            Self::Servers => "servers",
        }
    }

    fn of(rel: &str) -> Option<Self> {
        ITEMS.into_iter().find(|i| i.holds(rel))
    }

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

    /// The file or folder `rel`, with `/` between parts, in the sync folder.
    pub(crate) fn path(&self, rel: &str) -> PathBuf {
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

/// The options the games of the instance in folder `dir` start from: its own file, and for an instance that shares
/// them the shared keys of the sync folder over the keys only the instance keeps.
pub fn game_options(dir: &Path) -> Result<Option<String>> {
    options_for(&Mother::current(), dir)
}

pub(crate) fn options_for(mother: &Mother, dir: &Path) -> Result<Option<String>> {
    compose(&dir.join(OPTIONS_FILE), flags_in(dir).options.then(|| mother.path(OPTIONS_FILE)).as_deref())
}

/// The options in `own` with the shared keys of the file `shared` over them, when there is one that reads as options.
pub(crate) fn compose(own: &Path, shared: Option<&Path>) -> Result<Option<String>> {
    let mine = read_text(own)?;
    let Some(shared) = shared else { return Ok(mine) };
    let Some(theirs) = read_text(shared)?.filter(|t| keys::readable(t)) else { return Ok(mine) };
    Ok(Some(keys::merge(&mine.unwrap_or_default(), &theirs)))
}

/// The server list the games of the instance in folder `dir` start from: the shared one when the instance shares it.
pub fn game_servers(dir: &Path) -> PathBuf {
    servers_for(&Mother::current(), dir)
}

pub(crate) fn servers_for(mother: &Mother, dir: &Path) -> PathBuf {
    if flags_in(dir).servers { mother.path(SERVERS_FILE) } else { dir.join(SERVERS_FILE) }
}

/// Whether the options key `key` stays with each instance even when it shares its options.
pub(crate) fn kept_apart(key: &str) -> bool {
    keys::excluded(key)
}

/// Before a game of `instance` starts: its folder holds no copy of what it shares. Its own options keep only the keys
/// that stay with the instance, its own server list goes, its `config` folder becomes a link to the shared one, and an
/// item it stopped sharing gets back a real folder. Every own copy is kept in the history first and fills what the sync
/// folder lacks. With `joining`, a game of the instance already runs and its `config` folder stays as it is.
pub fn inbound(instance: &Instance, joining: bool) -> Result<()> {
    inbound_in(&Mother::current(), &Own::of(instance), joining)
}

pub(crate) fn inbound_in(mother: &Mother, own: &Own, joining: bool) -> Result<()> {
    join_in(mother, own, joining, false)
}

/// As [`inbound`]; with `replace`, what the instance had never adds to an item the sync folder already holds, so the
/// shared state wins whole and the own copy only stays in the history.
pub(crate) fn join_in(mother: &Mother, own: &Own, joining: bool, replace: bool) -> Result<()> {
    if !own.dir.is_dir() {
        return Ok(());
    }
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        let flags = flags_in(&own.dir);
        let plan = kept(mother, |history| {
            let mut plan = Plan::default();
            if flags.options {
                plan.options = options_join(mother, own, history, replace)?;
            }
            if flags.servers {
                plan.servers = servers_join(mother, own, history)?;
            }
            if flags.config {
                if !joining {
                    config_join(mother, own, history, replace)?;
                }
                snapshot(mother, history)?;
            }
            Ok(plan)
        })?;
        plan.apply(mother, own, flags.config, joining)
    })
}

/// What a join changes in the instance folder once the history kept every own copy.
#[derive(Default)]
struct Plan {
    options: Option<Option<String>>,
    servers: bool,
}

impl Plan {
    fn apply(self, mother: &Mother, own: &Own, config: bool, joining: bool) -> Result<()> {
        if let Some(rest) = self.options {
            let path = own.file(OPTIONS_FILE);
            match rest {
                Some(text) => net::write_atomic(&path, text.as_bytes())?,
                None => remove(&path)?,
            }
        }
        if self.servers {
            remove(&own.file(SERVERS_FILE))?;
        }
        if joining {
            return Ok(());
        }
        let path = own.file(CONFIG);
        let shared = mother.path(CONFIG);
        let linked = is_link(&path) && points_at(&path, &shared);
        match (config, linked) {
            (true, false) => link_config(&path, &shared),
            (false, true) => unlink_config(&path, &shared),
            (false, false) => {
                let fresh = path.with_file_name(format!("{CONFIG}.own-new"));
                if fs::symlink_metadata(&path).is_err() && fresh.is_dir() {
                    fs::rename(&fresh, &path).with_context(|| format!("cannot move {} back", fresh.display()))?;
                }
                Ok(())
            }
            (true, true) => Ok(()),
        }
    }
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            writable(path);
            fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Keeps the own options of `own` when they hold shared keys and fills the sync folder from them when it has none;
/// returns what the own file keeps: the keys that stay with the instance, or nothing.
fn options_join(mother: &Mother, own: &Own, history: &mut History, replace: bool) -> Result<Option<Option<String>>> {
    let Some(raw) = read_file(&own.file(OPTIONS_FILE))? else { return Ok(None) };
    let text = String::from_utf8_lossy(&raw).into_owned();
    let usable = keys::readable(&text) && !keys::legacy(&text);
    let shared = if usable { keys::shared(&text) } else { String::new() };
    if usable && shared.is_empty() {
        return Ok(None);
    }
    history.backup(own, OPTIONS_FILE, &raw)?;
    if usable {
        let theirs = read_text(&mother.path(OPTIONS_FILE))?.filter(|t| keys::readable(t));
        let filled = theirs.as_deref().map_or_else(|| shared.clone(), |t| keys::merge(&shared, t));
        if theirs.is_none() || (!replace && theirs.as_deref().is_some_and(|t| keys::map(t) != keys::map(&filled))) {
            put_locked(mother, history, own, vec![(OPTIONS_FILE.to_owned(), filled.into_bytes())])?;
        }
    }
    let rest = if usable { keys::apart(&text) } else { String::new() };
    Ok(Some((!rest.is_empty()).then_some(rest)))
}

fn servers_join(mother: &Mother, own: &Own, history: &mut History) -> Result<bool> {
    let Some(mine) = read_file(&own.file(SERVERS_FILE))? else { return Ok(false) };
    history.backup(own, SERVERS_FILE, &mine)?;
    if !mine.is_empty() && read_file(&mother.path(SERVERS_FILE))?.is_none() {
        put_locked(mother, history, own, vec![(SERVERS_FILE.to_owned(), mine)])?;
    }
    Ok(true)
}

/// Keeps every file of a real `config` folder of `own` and fills the empty shared one from it.
fn config_join(mother: &Mother, own: &Own, history: &mut History, replace: bool) -> Result<()> {
    let path = own.file(CONFIG);
    if is_link(&path) || fs::symlink_metadata(&path).is_err() {
        return Ok(());
    }
    let mine = own_files(own, Item::Config)?;
    for (rel, bytes) in &mine {
        history.backup(own, rel, bytes)?;
    }
    let shared = mother.path(CONFIG);
    let holds_any = replace && !files_in(&shared)?.is_empty();
    let missing: Vec<(String, Vec<u8>)> = mine
        .into_iter()
        .filter(|_| !holds_any)
        .filter(|(rel, _)| {
            let path = mother.path(rel);
            fs::symlink_metadata(&path).is_err() && path.ancestors().skip(1).take_while(|p| p.starts_with(&shared)).all(|p| fs::metadata(p).map_or(true, |m| m.is_dir()))
        })
        .collect();
    if !missing.is_empty() {
        put_locked(mother, history, own, missing)?;
    }
    Ok(())
}

/// Keeps each shared mod setting that changed since the history kept it last, so the state before a start stays
/// restorable even though games write the shared folder directly.
fn snapshot(mother: &Mother, history: &mut History) -> Result<()> {
    for (rel, path) in files_in(&mother.path(CONFIG))? {
        let Ok(bytes) = fs::read(&path) else { continue };
        history.snapshot(&format!("{CONFIG}/{rel}"), &bytes)?;
    }
    Ok(())
}

/// Makes the `config` folder at `path` a link to `shared`. A real folder moves aside first and comes back when the link
/// cannot be made; once linked it goes, since the history keeps its files.
fn link_config(path: &Path, shared: &Path) -> Result<()> {
    fs::create_dir_all(shared).with_context(|| format!("cannot create {}", shared.display()))?;
    let aside = if is_link(path) {
        super::fleet::remove_link(path)?;
        None
    } else if fs::symlink_metadata(path).is_ok() {
        let free = (1..).map(|n| path.with_file_name(format!("{CONFIG}.own-{n}"))).find(|p| fs::symlink_metadata(p).is_err()).context("no free name")?;
        fs::rename(path, &free).with_context(|| tr("Der Ordner „config“ der Instanz ist gerade in Benutzung. Schließ das Spiel und versuch es noch einmal."))?;
        Some(free)
    } else {
        None
    };
    if let Err(e) = super::fleet::create_link(shared, path) {
        if let Some(free) = &aside {
            let _ = fs::rename(free, path);
        }
        return Err(e).with_context(|| format!("cannot link {} to {}", path.display(), shared.display()));
    }
    if let Some(free) = aside
        && let Err(e) = fs::remove_dir_all(&free)
    {
        eprintln!("the old config folder {} stays: {e}", free.display());
    }
    Ok(())
}

/// Turns the link at `path` into a real folder holding a copy of `shared`; the link stays when the copy fails.
fn unlink_config(path: &Path, shared: &Path) -> Result<()> {
    let fresh = path.with_file_name(format!("{CONFIG}.own-new"));
    let _ = fs::remove_dir_all(&fresh);
    let copied = (|| -> Result<()> {
        for (rel, from) in files_in(shared)? {
            let to = rel.split('/').fold(fresh.clone(), |p, part| p.join(part));
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
            }
            fs::copy(&from, &to).with_context(|| format!("cannot copy {}", from.display()))?;
        }
        fs::create_dir_all(&fresh).with_context(|| format!("cannot create {}", fresh.display()))
    })();
    if let Err(e) = copied {
        let _ = fs::remove_dir_all(&fresh);
        return Err(e);
    }
    super::fleet::remove_link(path)?;
    fs::rename(&fresh, path).with_context(|| format!("cannot move {} to {}", fresh.display(), path.display()))
}

fn points_at(link: &Path, target: &Path) -> bool {
    matches!((fs::canonicalize(link), fs::canonicalize(target)), (Ok(a), Ok(b)) if a == b)
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

/// Right before a game starts: makes sure its `options.txt` at `game` holds every key of the options `template` but the
/// per-game ones and a data version, rebuilding a missing or unreadable file from the template with `fallback` as the
/// version when nothing else names one. Tries again writable once, then refuses the start.
pub fn verify_game(template: Option<&str>, game: &Path, fallback: Option<u32>) -> Result<()> {
    let Some(ours) = template else { return Ok(()) };
    let wanted: BTreeMap<String, String> = keys::map(ours).into_iter().filter(|(k, _)| k != "version" && !super::sync::PER_INSTANCE_KEYS.contains(&k.as_str())).collect();
    let existing = read_text(game).ok().flatten();
    let versioned = existing.as_deref().and_then(keys::version).or_else(|| keys::version(ours)).or(fallback).is_some();
    let holds = || read_text(game).ok().flatten().is_some_and(|t| keys::readable(&t) && keys::covers(&t, &wanted) && (!versioned || keys::version(&t).is_some()));
    if holds() {
        return Ok(());
    }
    let text = keys::game_file(ours, existing.as_deref(), fallback);
    settle(game, text.as_bytes(), holds, tr("Die Spiel-Optionen dieses Accounts ließen sich nicht vorbereiten. Prüf, ob die Datei „options.txt“ im Spielordner beschreibbar ist."))
}

/// Saves which items instance `id` shares and returns it. An item turned off first gets a real copy of the shared
/// state in the instance folder; an item turned on keeps the own copy in the history and then uses the shared one.
/// While a game of the instance runs (`running`), its `config` folder changes at the next start.
pub fn set(id: &str, sharing: Sharing, running: bool) -> Result<Instance> {
    set_in(&Mother::current(), &instance::instances_dir(), id, sharing, running)
}

pub(crate) fn set_in(mother: &Mother, root: &Path, id: &str, sharing: Sharing, running: bool) -> Result<Instance> {
    let mut made = find(root, id)?;
    let own = Own { id: made.id.clone(), name: made.name.clone(), dir: instance::dir_in(root, id) };
    let off: Vec<Item> = ITEMS.into_iter().filter(|i| !sharing.get(*i) && made.sync.get(*i) && travels(&made, *i)).collect();
    if !off.is_empty() {
        super::sync::with_templates(|| {
            let _held = mother.lock()?;
            leave(mother, &own, &off)
        })?;
    }
    made.sync = sharing;
    let made = save(root, made)?;
    if let Err(e) = inbound_in(mother, &own, running) {
        eprintln!("{} switches its shared files at the next start: {e:#}", own.name);
    }
    Ok(made)
}

/// Gives `own` real copies of the shared `items` it stops sharing; the `config` folder follows at the next join.
fn leave(mother: &Mother, own: &Own, items: &[Item]) -> Result<()> {
    for item in items {
        match item {
            Item::Options => {
                if let Some(text) = compose(&own.file(OPTIONS_FILE), Some(&mother.path(OPTIONS_FILE)))? {
                    net::write_atomic(&own.file(OPTIONS_FILE), text.as_bytes())?;
                }
            }
            Item::Servers => {
                if let Some(bytes) = read_file(&mother.path(SERVERS_FILE))? {
                    net::write_atomic(&own.file(SERVERS_FILE), &bytes)?;
                }
            }
            Item::Config => {}
        }
    }
    Ok(())
}

fn travels(made: &Instance, item: Item) -> bool {
    item != Item::Options || options_travel(&made.game_version)
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

fn own_files(own: &Own, item: Item) -> Result<Vec<(String, Vec<u8>)>> {
    match item {
        Item::Config => files_in(&own.file(CONFIG))?.into_iter().map(|(rel, path)| Ok((format!("{CONFIG}/{rel}"), fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?))).collect(),
        _ => Ok(read_file(&own.file(item.rel()))?.map(|b| (item.rel().to_owned(), b)).into_iter().collect()),
    }
}

/// What sharing restored on its own lately, newest first.
pub fn notes() -> Vec<Note> {
    notes::read(&Mother::current())
}

/// Forgets what sharing restored on its own once the person saw it.
pub fn clear_notes() -> Result<()> {
    notes::clear(&Mother::current())
}

/// The folder `name` of the instance folder `dir` to read when copying the instance out, `None` without one. A shared
/// `config` link reads as the shared folder it points at.
pub fn folder_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    if fs::symlink_metadata(&path).ok()?.is_dir() {
        return Some(path);
    }
    linked(dir, name).then(|| fs::canonicalize(&path).ok()).flatten().filter(|p| p.is_dir())
}

/// Whether `name` in the instance folder `dir` is a link to a shared folder, which belongs to every sharing instance.
pub fn linked(dir: &Path, name: &str) -> bool {
    name == CONFIG && is_link(&dir.join(name))
}

/// The sync folder, where the shared files live.
pub fn folder() -> PathBuf {
    Mother::current().root
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
        } else if !source.id.is_empty()
            && let Some(item) = Item::of(&rel)
        {
            history.based(item, source, true);
        }
        net::write_atomic(&path, &bytes)?;
        history.wrote(&rel, source);
        changed += 1;
    }
    Ok(changed)
}

/// Puts each mod setting in `back` a game broke back to its good state, unless something wrote it again since it was
/// seen broken, and keeps the ones it changed, `seen`, in the history. Returns the paths it put back.
fn mend_config(mother: &Mother, back: &[(String, Vec<u8>, Option<Vec<u8>>)], seen: &[(String, Vec<u8>)]) -> Result<Vec<String>> {
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        kept(mother, |history| {
            for (rel, bytes) in seen {
                history.snapshot(rel, bytes)?;
            }
            let mut put = Vec::new();
            for (rel, good, broke) in back {
                let path = mother.path(rel);
                if read_file(&path)? != *broke {
                    continue;
                }
                if let Some(broke) = broke {
                    history.snapshot(rel, broke)?;
                }
                net::write_atomic(&path, good)?;
                put.push(rel.clone());
            }
            Ok(put)
        })
    })
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