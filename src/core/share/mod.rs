//! One sync for every instance: `options.txt` key by key, and the `config`, `saves` and `screenshots` folders as one
//! linked copy. What the sync folder holds wins whenever an instance starts; a game's option changes flow back only after
//! it loaded, so a file a game resets while starting never becomes the shared state.

mod folders;
mod keys;
mod migrate;
mod state;
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
use super::sync::OPTIONS_FILE;
use super::{lock, net, paths};

use folders::{game_dirs, is_link};
pub use migrate::migrate;
pub use state::{Flags, KINDS, Kind, State, state};
pub(crate) use state::flags_in;
pub(crate) use track::{Start, Tracker};

/// Folder in the data folder that holds what sharing instances share.
pub const DIR: &str = "sync";
const LOCK: &str = ".lock";
const CONFIG: &str = "config";
const LOCK_WAIT: Duration = Duration::from_secs(30);

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
        fs::create_dir_all(&self.root).with_context(|| format!("cannot create {}", self.root.display()))?;
        lock::exclusive_within(&self.root.join(LOCK), LOCK_WAIT)
    }
}

/// An instance as sharing sees it: who it is and where its own copies live.
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

/// What the instance in folder `dir` takes part in now.
pub fn flags(dir: &Path) -> Flags {
    flags_in(&Mother::current(), dir)
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
    compose(&dir.join(OPTIONS_FILE), flags_in(mother, dir).options.then(|| mother.path(OPTIONS_FILE)).as_deref())
}

/// The options in `own` with the shared keys of the file `shared` over them, when there is one that reads as options.
pub(crate) fn compose(own: &Path, shared: Option<&Path>) -> Result<Option<String>> {
    let mine = read_text(own)?;
    let Some(shared) = shared else { return Ok(mine) };
    let Some(theirs) = read_text(shared)?.filter(|t| keys::readable(t)) else { return Ok(mine) };
    Ok(Some(keys::merge(&mine.unwrap_or_default(), &theirs)))
}

/// Whether the options key `key` stays with each instance even when it shares its options.
pub(crate) fn kept_apart(key: &str) -> bool {
    keys::excluded(key)
}

/// Before a game of `instance` starts: its options keep only the keys that stay with it and its `config` folder
/// becomes a link to the shared one, when it takes part; what it stopped taking part in is a real copy again. With
/// `joining`, a game of the instance already runs and its `config` folder stays as it is.
pub fn inbound(instance: &Instance, joining: bool) -> Result<()> {
    inbound_in(&Mother::current(), &Own::of(instance), joining)
}

pub(crate) fn inbound_in(mother: &Mother, own: &Own, joining: bool) -> Result<()> {
    if !own.dir.is_dir() {
        return Ok(());
    }
    settle_instance(mother, own, Flags::default(), flags_in(mother, &own.dir), !joining)
}

/// Readies the game folder `game` of `instance` before its game starts: `saves` and `screenshots` are links to the
/// shared folders when the instance takes part in them and real folders when not.
pub fn settle_game(instance: &Instance, game: &Path) -> Result<()> {
    settle_game_in(&Mother::current(), &instance.dir(), game)
}

pub(crate) fn settle_game_in(mother: &Mother, dir: &Path, game: &Path) -> Result<()> {
    let flags = flags_in(mother, dir);
    for kind in [Kind::Saves, Kind::Screenshots] {
        folders::settle(&game.join(kind.rel()), &mother.path(kind.rel()), flags.get(kind))?;
    }
    Ok(())
}

/// Brings the instance `own` from what it took part in (`old`) to what it takes part in now (`new`). Options change
/// at once; folders only when `idle`, else at the next start.
fn settle_instance(mother: &Mother, own: &Own, old: Flags, new: Flags, idle: bool) -> Result<()> {
    super::sync::with_templates(|| {
        if old.options && !new.options {
            leave_options(mother, own)?;
        }
        if new.options {
            join_options(own)?;
        }
        Ok::<(), anyhow::Error>(())
    })?;
    if !idle {
        return Ok(());
    }
    folders::settle(&own.file(CONFIG), &mother.path(CONFIG), new.config)?;
    for game in game_dirs(&own.dir) {
        for kind in [Kind::Saves, Kind::Screenshots] {
            folders::settle(&game.join(kind.rel()), &mother.path(kind.rel()), new.get(kind))?;
        }
    }
    Ok(())
}

/// Keeps what the own options of `own` hold beyond the keys that stay with the instance: a copy next to the file, once,
/// then the file keeps only those keys.
fn join_options(own: &Own) -> Result<()> {
    let path = own.file(OPTIONS_FILE);
    let Some(raw) = read_file(&path)? else { return Ok(()) };
    let text = String::from_utf8_lossy(&raw).into_owned();
    let usable = keys::readable(&text) && !keys::legacy(&text);
    if usable && keys::shared(&text).is_empty() {
        return Ok(());
    }
    let aside = folders::first_free(&path, "before-sync")?;
    fs::write(&aside, &raw).with_context(|| format!("cannot write {}", aside.display()))?;
    let rest = if usable { keys::apart(&text) } else { String::new() };
    if rest.is_empty() { remove(&path) } else { net::write_atomic(&path, rest.as_bytes()) }
}

/// Gives `own` a real options file holding the shared keys.
fn leave_options(mother: &Mother, own: &Own) -> Result<()> {
    if let Some(text) = compose(&own.file(OPTIONS_FILE), Some(&mother.path(OPTIONS_FILE)))? {
        net::write_atomic(&own.file(OPTIONS_FILE), text.as_bytes())?;
    }
    Ok(())
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

/// Makes what the instance \`own\` holds of \`kinds\` the shared state. Whatever the sync folder held of them moves aside
/// to \`<name>.before-setup\`.
fn seed(mother: &Mother, own: &Own, game_version: &str, kinds: &[Kind]) -> Result<()> {
    for kind in kinds {
        let shared = mother.path(kind.rel());
        if fs::symlink_metadata(&shared).is_ok() {
            let free = folders::first_free(&shared, "before-setup")?;
            fs::rename(&shared, &free).with_context(|| format!("cannot move {} aside", shared.display()))?;
        }
        match kind {
            Kind::Options => {
                if options_travel(game_version)
                    && let Some(text) = read_text(&own.file(OPTIONS_FILE))?.filter(|t| keys::readable(t) && !keys::legacy(t))
                {
                    let held = keys::shared(&text);
                    if !held.is_empty() {
                        net::write_atomic(&shared, held.as_bytes())?;
                    }
                }
            }
            Kind::Config => {
                let path = own.file(CONFIG);
                if !is_link(&path) && path.is_dir() {
                    folders::copy_tree(&path, &shared)?;
                }
            }
            Kind::Saves | Kind::Screenshots => {
                for game in game_dirs(&own.dir) {
                    folders::move_children(&game.join(kind.rel()), &shared)?;
                }
            }
        }
    }
    Ok(())
}

/// Every instance with what it takes part in now, to compare after a change.
fn before(mother: &Mother, root: &Path) -> Result<Vec<(Instance, Flags)>> {
    Ok(instance::load_all_in(root)?.into_iter().map(|i| (instance::dir_in(root, &i.id), i)).map(|(dir, i)| (i, flags_in(mother, &dir))).collect())
}

/// Brings each instance of `before` to what it takes part in now. A failing instance is retried at its next start.
/// Returns whether every instance made it.
fn transition(mother: &Mother, root: &Path, before: Vec<(Instance, Flags)>, running: &[String]) -> bool {
    let mut all = true;
    for (made, old) in before {
        let own = Own { id: made.id.clone(), name: made.name.clone(), dir: instance::dir_in(root, &made.id) };
        if !own.dir.is_dir() {
            continue;
        }
        let new = flags_in(mother, &own.dir);
        if let Err(e) = settle_instance(mother, &own, old, new, !running.contains(&own.id)) {
            eprintln!("{} switches its shared files at the next start: {e:#}", own.name);
            all = false;
        }
    }
    all
}

/// Deletes what the sync folder holds of every kind that is off, once every instance has its own copy.
fn clear_unused(mother: &Mother, state: &State) {
    for kind in KINDS.into_iter().filter(|k| !(state.on && state.get(*k))) {
        let path = mother.path(kind.rel());
        let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
    }
}

/// Turns the sync on with the files of instance `mother_id` as the first shared state of every kind, and brings every
/// instance in. Instances with a game running follow at their next start. Fails when the instance itself runs.
pub fn setup(mother_id: &str, running: &[String]) -> Result<()> {
    setup_in(&Mother::current(), &instance::instances_dir(), mother_id, running)
}

pub(crate) fn setup_in(mother: &Mother, root: &Path, mother_id: &str, running: &[String]) -> Result<()> {
    let base = find(root, mother_id)?;
    if running.contains(&base.id) {
        bail!(trf("Schließ zuerst die Instanz „%@“, ihr Stand wird der gemeinsame.", &[&base.name]));
    }
    let before = before(mother, root)?;
    let own = Own { id: base.id.clone(), name: base.name.clone(), dir: instance::dir_in(root, &base.id) };
    inbound_in(mother, &own, false)?;
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        seed(mother, &own, &base.game_version, &KINDS)
    })?;
    state::write(mother, &State { on: true, options: true, config: true, saves: true, screenshots: true, mother: Some(base.id) })?;
    transition(mother, root, before, running);
    Ok(())
}

/// Turns the sync off: every instance gets its own copy of the shared state, and the sync folder is emptied once all
/// of them have it.
pub fn turn_off(running: &[String]) -> Result<()> {
    turn_off_in(&Mother::current(), &instance::instances_dir(), running)
}

pub(crate) fn turn_off_in(mother: &Mother, root: &Path, running: &[String]) -> Result<()> {
    let mut now = state::read(mother);
    if !now.on {
        return Ok(());
    }
    let before = before(mother, root)?;
    now.on = false;
    state::write(mother, &now)?;
    let all = transition(mother, root, before, running);
    if all && running.is_empty() {
        clear_unused(mother, &now);
    }
    Ok(())
}

/// Shares `kind` between the instances that take part, or gives each of them its own copy again. A kind switched on
/// later takes the files of the instance the sync started from.
pub fn set_kind(kind: Kind, on: bool, running: &[String]) -> Result<()> {
    set_kind_in(&Mother::current(), &instance::instances_dir(), kind, on, running)
}

pub(crate) fn set_kind_in(mother: &Mother, root: &Path, kind: Kind, on: bool, running: &[String]) -> Result<()> {
    let mut now = state::read(mother);
    if !now.on || now.get(kind) == on {
        return Ok(());
    }
    let before = before(mother, root)?;
    if on {
        let all = instance::load_all_in(root)?;
        let base = now.mother.as_ref().and_then(|id| all.iter().find(|i| &i.id == id)).or(all.first()).cloned().with_context(|| tr("Diese Instanz gibt es nicht mehr."))?;
        if running.contains(&base.id) && matches!(kind, Kind::Saves | Kind::Screenshots) {
            bail!(trf("Schließ zuerst die Instanz „%@“, ihr Stand wird der gemeinsame.", &[&base.name]));
        }
        let own = Own { id: base.id.clone(), name: base.name.clone(), dir: instance::dir_in(root, &base.id) };
        inbound_in(mother, &own, false)?;
        super::sync::with_templates(|| {
            let _held = mother.lock()?;
            seed(mother, &own, &base.game_version, &[kind])
        })?;
        now.mother = Some(base.id);
    }
    now = now.with(kind, on);
    state::write(mother, &now)?;
    let all = transition(mother, root, before, running);
    if all && running.is_empty() {
        clear_unused(mother, &now);
    }
    Ok(())
}

/// Lets the instance `id` stay out of the sync with its own files, or take part again. Returns it as saved.
pub fn set_opt_out(id: &str, out: bool, running: bool) -> Result<Instance> {
    set_opt_out_in(&Mother::current(), &instance::instances_dir(), id, out, running)
}

pub(crate) fn set_opt_out_in(mother: &Mother, root: &Path, id: &str, out: bool, running: bool) -> Result<Instance> {
    let mut made = find(root, id)?;
    let old = flags_in(mother, &instance::dir_in(root, id));
    made.sync_out = out;
    let made = save(root, made)?;
    let own = Own { id: made.id.clone(), name: made.name.clone(), dir: instance::dir_in(root, &made.id) };
    if let Err(e) = settle_instance(mother, &own, old, flags_in(mother, &own.dir), !running) {
        eprintln!("{} switches its shared files at the next start: {e:#}", own.name);
    }
    Ok(made)
}

fn find(root: &Path, id: &str) -> Result<Instance> {
    instance::load_all_in(root)?.into_iter().find(|i| i.id == id).with_context(|| tr("Diese Instanz gibt es nicht mehr."))
}

fn save(root: &Path, made: Instance) -> Result<Instance> {
    match instance::save_in(root, &made) {
        Ok(()) => Ok(made),
        Err(_) => {
            let mut fresh = find(root, &made.id)?;
            fresh.sync_out = made.sync_out;
            instance::save_in(root, &fresh)?;
            Ok(fresh)
        }
    }
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

/// Sets the keys of `changes` the sync folder may hold in its `options.txt` and returns the ones that differed.
pub(crate) fn put_options(mother: &Mother, changes: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    super::sync::with_templates(|| {
        let _held = mother.lock()?;
        let now = read_text(&mother.path(OPTIONS_FILE))?.unwrap_or_default();
        let up = keys::upward(changes, &keys::map(&now));
        if !up.is_empty() {
            net::write_atomic(&mother.path(OPTIONS_FILE), keys::apply(&now, &up).as_bytes())?;
        }
        Ok(up)
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
