//! The mods of an instance as the Mods tab shows them: what the shared folder holds, which mods are on or off, what they
//! need from each other, duplicates, the mod trash and the troubleshooter that finds a faulty mod by switching halves off.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::batch::ModRef;
use super::fleet::COMPANION_FILE;
use super::i18n::{tr, trf};
use super::instance::{self, Final, Instance};
use super::modmeta::{self, Meta};
use super::modrinth::{self, DISABLED, Found, jar_name, list_jars, safe_file_name, with_mods_lock};
use super::{modicons, net, paths, store};

const ICONS: &str = "jar-icons";
const TRASH: &str = ".trash";
/// What a mod trash entry took away, kept inside it so it can be put back after a restart.
pub(super) const TRASHED: &str = "trashed.json";
const STATE: &str = "bisect.json";
const PRE_RELEASE: [&str; 4] = ["-alpha", "-beta", "-rc", "-pre"];
const MANYCRAFT: &str = "Manycraft";

type Key = (PathBuf, u64, SystemTime);

static METAS: LazyLock<Mutex<HashMap<Key, Option<Meta>>>> = LazyLock::new(Mutex::default);
static NO_ICON: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn key(name: &str) -> String {
    name.to_lowercase()
}

/// Where a mod of an instance comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Modrinth { project_id: String, title: String },
    Own,
}

/// One mod of an instance, from its list and its shared mods folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub file: String,
    pub on_disk: bool,
    pub enabled: bool,
    pub source: Source,
    pub meta: Option<Meta>,
    pub icon: Option<PathBuf>,
    pub added: Option<SystemTime>,
    pub updated: Option<SystemTime>,
    pub size: u64,
}

impl Entry {
    /// The name to show: from the jar, else the Modrinth title, else the file name without `.jar`.
    pub fn name(&self) -> String {
        if let Some(meta) = self.meta.as_ref().filter(|m| !m.name.is_empty()) {
            return meta.name.clone();
        }
        if let Source::Modrinth { title, .. } = &self.source
            && !title.is_empty()
        {
            return title.clone();
        }
        match jar_name(&self.file) {
            Some((base, _)) => base.get(..base.len().saturating_sub(4)).unwrap_or(base).to_owned(),
            None => self.file.clone(),
        }
    }

    /// The version the jar states, when it states one.
    pub fn version(&self) -> Option<&str> {
        self.meta.as_ref().map(|m| m.version.as_str()).filter(|v| !v.is_empty())
    }

    /// The mod id the jar states.
    pub fn id(&self) -> Option<&str> {
        self.meta.as_ref().map(|m| m.id.as_str())
    }
}

/// The mods of an instance, what Manycraft's own mod needs and the enabled jars the sync keeps in the folder that are not in the instance's lists.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub entries: Vec<Entry>,
    pub companion: Option<Meta>,
    pub managed: Vec<Meta>,
}

const FIXED: usize = usize::MAX;

fn existing(root: &Path, id: &str) -> Result<PathBuf> {
    instance::existing_in(root, id)
}

fn saved_in(root: &Path, id: &str) -> Result<Instance> {
    store::load::<Instance>(&instance::dir_in(root, id).join(instance::FILE))?.ok_or_else(|| anyhow!(tr("Diese Instanz gibt es nicht mehr.")))
}

fn shared<T>(root: &Path, id: &str, work: impl FnOnce(&Path, Instance) -> Result<T>) -> Result<T> {
    existing(root, id)?;
    let _hold = instance::hold_in(root, id).with_context(|| tr("Die Instanz wird gerade umbenannt oder zurückgesetzt. Versuch es gleich noch einmal."))?;
    let dir = existing(root, id)?;
    with_mods_lock(&dir, || work(&dir, saved_in(root, id)?))
}

fn exclusive<T>(root: &Path, id: &str, work: impl FnOnce(&Path, Instance) -> Result<T>) -> Result<T> {
    let _quiet = instance::quiet_in(root, id)?;
    let dir = existing(root, id)?;
    with_mods_lock(&dir, || work(&dir, saved_in(root, id)?))
}

/// Reads the shared mods folder of the saved instance `id` under the mods lock, takes jars that were put there by hand into `local_mods`, drops local names whose file is gone, takes the on/off state from the folder (a file that exists only as `.disabled` is off), saves the instance when that changed, and returns it with the scan.
pub fn scan(id: &str) -> Result<(Instance, Scan)> {
    scan_in(&instance::instances_dir(), &paths::data_dir().join("cache").join("modicons").join(ICONS), id)
}

fn scan_in(root: &Path, icons: &Path, id: &str) -> Result<(Instance, Scan)> {
    shared(root, id, |dir, made| scan_locked(root, dir, made, Some(icons)))
}

struct Disk {
    key: Key,
    actual: PathBuf,
    added: Option<SystemTime>,
    updated: Option<SystemTime>,
}

/// The jar of `file` in `mods`, wherever it is now: switched on, or off as `.disabled`.
pub fn jar_path(mods: &Path, file: &str) -> PathBuf {
    let found = list_jars(mods).ok().and_then(|mut jars| jars.remove(&file.to_lowercase()));
    found.and_then(|f| f.on.or(f.off)).unwrap_or_else(|| mods.join(file))
}

fn stat(found: &Found, mods: &Path) -> Option<Disk> {
    let actual = found.on.as_ref().or_else(|| found.off.as_ref())?;
    let meta = std::fs::metadata(actual).ok()?;
    let updated = meta.modified().ok();
    Some(Disk { key: (mods.join(&found.name), meta.len(), updated.unwrap_or(UNIX_EPOCH)), actual: actual.clone(), added: meta.created().ok().or(updated), updated })
}

fn metas_of(disks: &[&Disk]) -> Vec<Option<Meta>> {
    let todo: Vec<&Disk> = {
        let known = guard(&METAS);
        disks.iter().copied().filter(|d| !known.contains_key(&d.key)).collect()
    };
    let parsed: Vec<(Key, Option<Meta>)> = todo.par_iter().filter_map(|d| modmeta::read(&d.actual).ok().map(|meta| (d.key.clone(), meta))).collect();
    let mut known = guard(&METAS);
    known.extend(parsed);
    disks.iter().map(|d| known.get(&d.key).cloned().flatten()).collect()
}

fn icon_file(dir: &Path, disk: &Disk, entry: &str) -> Option<PathBuf> {
    let mut hash = Sha1::new();
    hash.update(disk.key.0.to_string_lossy().as_bytes());
    hash.update(disk.key.1.to_le_bytes());
    hash.update(disk.key.2.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos()).to_le_bytes());
    let name: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("{name}.png"));
    if path.is_file() {
        return Some(path);
    }
    if guard(&NO_ICON).contains(&path) {
        return None;
    }
    let made = modmeta::entry_bytes(&disk.actual, entry).and_then(|bytes| modicons::shrink(&bytes)).and_then(|small| net::write_atomic(&path, &small));
    if made.is_err() {
        guard(&NO_ICON).insert(path);
        return None;
    }
    Some(path)
}

fn scan_locked(root: &Path, dir: &Path, made: Instance, icons: Option<&Path>) -> Result<(Instance, Scan)> {
    let mods = dir.join("mods");
    let folder = list_jars(&mods)?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut rows: Vec<(String, Source, String)> = Vec::new();
    for m in &made.mods {
        let k = key(&m.file_name);
        if seen.insert(k.clone()) {
            rows.push((m.file_name.clone(), Source::Modrinth { project_id: m.project_id.clone(), title: m.title.clone() }, k));
        }
    }
    let mut local: Vec<String> = Vec::new();
    for name in &made.local_mods {
        let k = key(name);
        if folder.contains_key(&k) && seen.insert(k.clone()) {
            local.push(name.clone());
            rows.push((name.clone(), Source::Own, k));
        }
    }
    let ledger = modrinth::managed_files(dir);
    let mut adopted: Vec<(&String, &Found)> = Vec::new();
    let mut kept_by_sync: Vec<&Found> = Vec::new();
    for (k, found) in folder.iter().filter(|(k, _)| !seen.contains(*k) && k.as_str() != COMPANION_FILE) {
        if ledger.contains_key(k) && found.on.iter().chain(&found.off).all(|path| modrinth::is_managed(&ledger, k, path)) {
            kept_by_sync.push(found);
        } else {
            adopted.push((k, found));
        }
    }
    adopted.sort_by(|a, b| a.0.cmp(b.0));
    for (k, found) in adopted {
        local.push(found.name.clone());
        rows.push((found.name.clone(), Source::Own, k.clone()));
    }
    let off: HashSet<String> = made.disabled.iter().map(|name| key(name)).collect();
    let disks: Vec<Option<Disk>> = rows.iter().map(|(_, _, k)| folder.get(k).and_then(|found| stat(found, &mods))).collect();
    let companion_disk = folder.get(COMPANION_FILE).and_then(|found| stat(found, &mods));
    let managed_disks: Vec<Disk> = kept_by_sync.iter().filter(|found| found.on.is_some()).filter_map(|found| stat(found, &mods)).collect();
    let mut wanted: Vec<&Disk> = disks.iter().flatten().collect();
    wanted.extend(companion_disk.as_ref());
    wanted.extend(&managed_disks);
    let mut metas = metas_of(&wanted).into_iter();
    let row_metas: Vec<Option<Meta>> = disks.iter().map(|disk| if disk.is_some() { metas.next().flatten() } else { None }).collect();
    let companion = companion_disk.as_ref().and_then(|_| metas.next().flatten());
    let managed: Vec<Meta> = managed_disks.iter().filter_map(|_| metas.next().flatten()).collect();
    let pictures: Vec<Option<PathBuf>> = disks
        .par_iter()
        .zip(&row_metas)
        .map(|(disk, meta)| match (disk, meta.as_ref().and_then(|m| m.icon.as_deref()), icons) {
            (Some(disk), Some(entry), Some(dir)) => icon_file(dir, disk, entry),
            _ => None,
        })
        .collect();
    let mut entries = Vec::with_capacity(rows.len());
    for (((file, source, k), disk), (meta, icon)) in rows.into_iter().zip(disks).zip(row_metas.into_iter().zip(pictures)) {
        let on_disk = disk.is_some();
        let enabled = match folder.get(&k) {
            Some(found) if on_disk => found.on.is_some(),
            _ => !off.contains(&k),
        };
        entries.push(Entry {
            file,
            on_disk,
            enabled,
            source,
            meta,
            icon,
            added: disk.as_ref().and_then(|d| d.added),
            updated: disk.as_ref().and_then(|d| d.updated),
            size: disk.as_ref().map_or(0, |d| d.key.1),
        });
    }
    let switched_off: HashSet<String> = entries.iter().filter(|e| !e.enabled).map(|e| key(&e.file)).collect();
    let mut disabled: Vec<String> = Vec::new();
    for name in &made.disabled {
        if switched_off.contains(&key(name)) && !disabled.iter().any(|d| key(d) == key(name)) {
            disabled.push(name.clone());
        }
    }
    for entry in entries.iter().filter(|e| !e.enabled) {
        if !disabled.iter().any(|d| key(d) == key(&entry.file)) {
            disabled.push(entry.file.clone());
        }
    }
    let next = Instance { local_mods: local, disabled, ..made.clone() };
    if next != made {
        instance::save_in(root, &next)?;
    }
    Ok((next, Scan { entries, companion, managed }))
}

fn providers(scan: &Scan) -> HashMap<&str, Vec<usize>> {
    let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, entry) in scan.entries.iter().enumerate() {
        if let Some(meta) = &entry.meta {
            for id in std::iter::once(&meta.id).chain(&meta.provides) {
                index.entry(id.as_str()).or_default().push(i);
            }
        }
    }
    for meta in &scan.managed {
        for id in std::iter::once(&meta.id).chain(&meta.provides) {
            index.entry(id.as_str()).or_default().push(FIXED);
        }
    }
    index
}

fn loses(index: &HashMap<&str, Vec<usize>>, on: &dyn Fn(usize) -> bool, depends: &[String], off: &HashSet<usize>) -> bool {
    depends.iter().any(|dep| {
        index.get(dep.as_str()).is_some_and(|all| {
            let live: Vec<usize> = all.iter().copied().filter(|j| on(*j)).collect();
            !live.is_empty() && live.iter().all(|j| off.contains(j))
        })
    })
}

fn spread(entries: &[Entry], index: &HashMap<&str, Vec<usize>>, on: &dyn Fn(usize) -> bool, off: &mut HashSet<usize>) -> Vec<usize> {
    let mut added = Vec::new();
    loop {
        let next: Vec<usize> = (0..entries.len()).filter(|i| on(*i) && !off.contains(i) && entries[*i].meta.as_ref().is_some_and(|m| loses(index, on, &m.depends, off))).collect();
        if next.is_empty() {
            return added;
        }
        off.extend(next.iter().copied());
        added.extend(next);
    }
}

fn indices(entries: &[Entry], files: &[String]) -> HashSet<usize> {
    let wanted: HashSet<String> = files.iter().map(|f| key(f)).collect();
    (0..entries.len()).filter(|i| wanted.contains(&key(&entries[*i].file))).collect()
}

fn sorted_names(mut names: Vec<String>) -> Vec<String> {
    names.sort_by_key(|n| (key(n), n.clone()));
    names.dedup();
    names
}

/// Names of mods that stop working when `files` are switched off: enabled mods outside `files` with a required dependency that only mods in `files` satisfy, repeated until nothing more breaks; "Manycraft" when its own mod is among them. Sorted, deduplicated.
pub fn needed_by(scan: &Scan, files: &[String]) -> Vec<String> {
    let entries = &scan.entries;
    let index = providers(scan);
    let on = |i: usize| entries.get(i).is_none_or(|e| e.enabled);
    let mut off = indices(entries, files);
    let broken = spread(entries, &index, &on, &mut off);
    let mut names: Vec<String> = broken.into_iter().map(|i| entries[i].name()).collect();
    if scan.companion.as_ref().is_some_and(|c| loses(&index, &on, &c.depends, &off)) {
        names.push(MANYCRAFT.to_owned());
    }
    sorted_names(names)
}

/// Files of switched-off mods that `files` need, directly or through each other, to work once switched on.
pub fn missing_for(scan: &Scan, files: &[String]) -> Vec<String> {
    let entries = &scan.entries;
    let index = providers(scan);
    let mut on: HashSet<usize> = (0..entries.len()).filter(|i| entries[*i].enabled).collect();
    on.insert(FIXED);
    let wanted = indices(entries, files);
    on.extend(wanted.iter().copied());
    let mut queue: Vec<usize> = wanted.into_iter().collect();
    let mut found: Vec<usize> = Vec::new();
    while let Some(i) = queue.pop() {
        let Some(meta) = &entries[i].meta else { continue };
        for dep in &meta.depends {
            let Some(all) = index.get(dep.as_str()) else { continue };
            if all.iter().any(|j| on.contains(j)) {
                continue;
            }
            if let Some(j) = all.first().copied() {
                on.insert(j);
                found.push(j);
                queue.push(j);
            }
        }
    }
    sorted_names(found.into_iter().map(|i| entries[i].file.clone()).collect())
}

/// For every switched-on mod on disk, the files of switched-off mods it needs, as [`missing_for`] names them for that one
/// file; mods that need nothing are left out. Builds the provider index once, so it stays linear in the mods.
pub fn missing_for_enabled(scan: &Scan) -> HashMap<String, Vec<String>> {
    let entries = &scan.entries;
    let index = providers(scan);
    let mut base: HashSet<usize> = (0..entries.len()).filter(|i| entries[*i].enabled).collect();
    base.insert(FIXED);
    let mut out = HashMap::new();
    for (start, e) in entries.iter().enumerate().filter(|(_, e)| e.enabled && e.on_disk) {
        let mut added: HashSet<usize> = HashSet::new();
        let mut queue = vec![start];
        let mut found: Vec<usize> = Vec::new();
        while let Some(i) = queue.pop() {
            let Some(meta) = &entries[i].meta else { continue };
            for dep in &meta.depends {
                let Some(all) = index.get(dep.as_str()) else { continue };
                if all.iter().any(|j| base.contains(j) || added.contains(j)) {
                    continue;
                }
                if let Some(j) = all.first().copied() {
                    added.insert(j);
                    found.push(j);
                    queue.push(j);
                }
            }
        }
        if !found.is_empty() {
            out.insert(e.file.clone(), sorted_names(found.into_iter().map(|i| entries[i].file.clone()).collect()));
        }
    }
    out
}

/// Mods whose jar has the same mod id: the files, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate {
    pub id: String,
    pub files: Vec<String>,
}

/// The groups of on-disk mods that share a mod id, sorted by id.
pub fn duplicates(entries: &[Entry]) -> Vec<Duplicate> {
    let mut groups: HashMap<&str, Vec<&Entry>> = HashMap::new();
    for entry in entries.iter().filter(|e| e.on_disk) {
        if let Some(meta) = &entry.meta {
            groups.entry(meta.id.as_str()).or_default().push(entry);
        }
    }
    let mut found: Vec<Duplicate> = groups
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .map(|(id, mut group)| {
            group.sort_by(|a, b| compare_versions(b.version().unwrap_or_default(), a.version().unwrap_or_default()).then_with(|| b.updated.cmp(&a.updated)).then_with(|| a.file.cmp(&b.file)));
            Duplicate { id: id.to_owned(), files: group.iter().map(|e| e.file.clone()).collect() }
        })
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

#[derive(Debug, PartialEq, Eq)]
enum Part {
    Number(String),
    Text(String),
}

fn parts(version: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut numeric = false;
    let flush = |run: &mut String, numeric: bool, out: &mut Vec<Part>| {
        if run.is_empty() {
            return;
        }
        let text = std::mem::take(run);
        out.push(if numeric { Part::Number(text.trim_start_matches('0').to_owned()) } else { Part::Text(text) });
    };
    for c in version.chars() {
        if c.is_ascii_digit() || c.is_alphabetic() {
            let digit = c.is_ascii_digit();
            if digit != numeric {
                flush(&mut run, numeric, &mut out);
                numeric = digit;
            }
            run.extend(c.to_lowercase());
        } else {
            flush(&mut run, numeric, &mut out);
        }
    }
    flush(&mut run, numeric, &mut out);
    out
}

fn compare_parts(a: &[Part], b: &[Part]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let order = match (x, y) {
            (Part::Number(p), Part::Number(q)) => p.len().cmp(&q.len()).then_with(|| p.cmp(q)),
            (Part::Text(p), Part::Text(q)) => p.cmp(q),
            (Part::Number(_), Part::Text(_)) => Ordering::Greater,
            (Part::Text(_), Part::Number(_)) => Ordering::Less,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

fn split_pre(version: &str) -> (&str, Option<&str>) {
    let lower = version.to_ascii_lowercase();
    match PRE_RELEASE.iter().filter_map(|marker| lower.find(marker)).min() {
        Some(at) => {
            let (release, pre) = version.split_at(at);
            (release, Some(pre))
        }
        None => (version, None),
    }
}

/// Compares two version strings: numeric runs numerically, the rest as text, a pre-release (`-beta`, `-alpha`, `-rc`, `-pre`) below its release.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let ((release_a, pre_a), (release_b, pre_b)) = (split_pre(a), split_pre(b));
    compare_parts(&parts(release_a), &parts(release_b)).then_with(|| match (pre_a, pre_b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => compare_parts(&parts(x), &parts(y)),
    })
}

struct Moves(Vec<(PathBuf, PathBuf)>);

impl Moves {
    fn undo(self) {
        for (from, to) in self.0.into_iter().rev() {
            let _ = std::fs::rename(&to, &from);
        }
    }
}

fn apply(plan: Vec<(PathBuf, PathBuf)>) -> Result<Moves> {
    let mut moves = Moves(Vec::new());
    for (from, to) in plan {
        if let Err(e) = modrinth::rename(&from, &to) {
            moves.undo();
            return Err(e);
        }
        moves.0.push((from, to));
    }
    Ok(moves)
}

fn file_name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn resolve(made: &Instance, files: &[String]) -> Result<Vec<String>> {
    let known: HashMap<String, &str> = made.mods.iter().map(|m| m.file_name.as_str()).chain(made.local_mods.iter().map(String::as_str)).map(|n| (key(n), n)).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for file in files {
        let name = known.get(&key(file)).ok_or_else(|| anyhow!(tr("Diesen Mod gibt es nicht mehr.")))?;
        if seen.insert(key(name)) {
            out.push((*name).to_owned());
        }
    }
    Ok(out)
}

fn retarget(dir: &Path, made: &Instance, target: &[String]) -> Result<(Instance, Moves)> {
    let mods = dir.join("mods");
    let folder = list_jars(&mods)?;
    let want_off: HashSet<String> = target.iter().map(|n| key(n)).collect();
    let listed: Vec<&str> = made.mods.iter().map(|m| m.file_name.as_str()).chain(made.local_mods.iter().map(String::as_str)).collect();
    let listed_keys: HashSet<String> = listed.iter().map(|n| key(n)).collect();
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for name in &listed {
        let k = key(name);
        if !seen.insert(k.clone()) {
            continue;
        }
        let off = want_off.contains(&k);
        let (from, to) = match folder.get(&k) {
            Some(Found { on: Some(on), name: base, .. }) if off => (on.clone(), mods.join(format!("{base}{DISABLED}"))),
            Some(Found { on: None, off: Some(old), name: base, .. }) if !off => (old.clone(), mods.join(base)),
            _ => continue,
        };
        if std::fs::symlink_metadata(&to).is_ok() {
            bail!(trf("Die Datei %@ gibt es schon.", &[&file_name_of(&to)]));
        }
        plan.push((from, to));
    }
    let mut disabled: Vec<String> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    for name in made.disabled.iter().map(String::as_str).chain(listed.iter().copied()) {
        let k = key(name);
        if want_off.contains(&k) && listed_keys.contains(&k) && taken.insert(k) {
            disabled.push(name.to_owned());
        }
    }
    let moves = apply(plan)?;
    Ok((Instance { disabled, ..made.clone() }, moves))
}

fn commit(root: &Path, dir: &Path, made: &Instance, target: &[String]) -> Result<Instance> {
    let (next, moves) = retarget(dir, made, target)?;
    if next != *made
        && let Err(e) = instance::save_in(root, &next)
    {
        moves.undo();
        return Err(e);
    }
    Ok(next)
}

/// Switches the mods `files` of instance `id` on or off: renames `x.jar` <-> `x.jar.disabled` in the shared folder and saves `disabled`. Refused while a game of the instance runs. On failure every rename done so far is undone.
pub fn set_enabled(id: &str, files: &[String], on: bool) -> Result<Instance> {
    set_enabled_in(&instance::instances_dir(), id, files, on)
}

fn set_enabled_in(root: &Path, id: &str, files: &[String], on: bool) -> Result<Instance> {
    exclusive(root, id, |dir, made| {
        if searching(dir) {
            bail!(tr("Beende zuerst die Fehlersuche."));
        }
        let (made, _) = scan_locked(root, dir, made, None)?;
        let listed = resolve(&made, files)?;
        let touched: HashSet<String> = listed.iter().map(|n| key(n)).collect();
        let mut target: Vec<String> = made.disabled.clone();
        if on {
            target.retain(|n| !touched.contains(&key(n)));
        } else {
            for name in listed {
                if !target.iter().any(|n| key(n) == key(&name)) {
                    target.push(name);
                }
            }
        }
        commit(root, dir, &made, &target)
    })
}

/// Copies the `.jar` files `paths` into the shared mods folder of instance `id` and lists them in `local_mods`. A file whose name is already in the folder (on or off) or in the instance's lists is skipped and its name returned; other files are ignored. Refused while a game of the instance runs. Nothing stays copied when it fails.
pub fn add_files(id: &str, paths: &[PathBuf]) -> Result<(Instance, Vec<String>)> {
    add_files_in(&instance::instances_dir(), id, paths)
}

fn copy_into(from: &Path, mods: &Path, name: &str) -> Result<PathBuf> {
    let tmp = mods.join(format!("{name}.manycraft-tmp"));
    let target = mods.join(name);
    let placed = (|| -> Result<()> {
        std::fs::copy(from, &tmp).with_context(|| format!("cannot copy {}", from.display()))?;
        if std::fs::symlink_metadata(&target).is_ok() {
            bail!(trf("Die Datei %@ gibt es schon.", &[&name]));
        }
        modrinth::rename(&tmp, &target)
    })();
    if let Err(e) = placed {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(target)
}

fn add_files_in(root: &Path, id: &str, paths: &[PathBuf]) -> Result<(Instance, Vec<String>)> {
    exclusive(root, id, |dir, made| {
        let mods = dir.join("mods");
        std::fs::create_dir_all(&mods).with_context(|| format!("cannot create {}", mods.display()))?;
        let mut taken: HashSet<String> = list_jars(&mods)?.into_keys().collect();
        taken.extend(made.mods.iter().map(|m| key(&m.file_name)));
        taken.extend(made.local_mods.iter().map(|n| key(n)));
        let mut skipped: Vec<String> = Vec::new();
        let mut added: Vec<String> = Vec::new();
        let mut copied: Vec<PathBuf> = Vec::new();
        let result = (|| -> Result<()> {
            for path in paths {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
                if !matches!(jar_name(name), Some((_, false))) || safe_file_name(name).is_err() || !path.is_file() {
                    continue;
                }
                let lower = key(name);
                if taken.contains(&lower) {
                    skipped.push(name.to_owned());
                    continue;
                }
                copied.push(copy_into(path, &mods, name)?);
                taken.insert(lower);
                added.push(name.to_owned());
            }
            Ok(())
        })();
        let finished = result.and_then(|()| {
            if added.is_empty() {
                return Ok(made.clone());
            }
            let mut next = made.clone();
            next.local_mods.extend(added.iter().cloned());
            instance::save_in(root, &next)?;
            Ok(next)
        });
        match finished {
            Ok(next) => Ok((next, skipped)),
            Err(e) => {
                for path in &copied {
                    let _ = std::fs::remove_file(path);
                }
                Err(e)
            }
        }
    })
}

/// What `trash` took away, for `untrash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trashed {
    #[serde(skip)]
    pub entry: String,
    pub files: Vec<String>,
    pub refs: Vec<(usize, ModRef)>,
    pub local: Vec<String>,
    pub disabled: Vec<String>,
}

/// Moves the mods `files` of instance `id` to its trash folder `<instance>/.trash/<unix_millis>`, removes them from `mods`, `local_mods` and `disabled`, and saves. Refused while a game runs.
pub fn trash(id: &str, files: &[String]) -> Result<(Instance, Trashed)> {
    trash_in(&instance::instances_dir(), id, files, instance::now_millis())
}

fn trash_in(root: &Path, id: &str, files: &[String], millis: u64) -> Result<(Instance, Trashed)> {
    exclusive(root, id, |dir, made| {
        if searching(dir) {
            bail!(tr("Beende zuerst die Fehlersuche."));
        }
        let (made, _) = scan_locked(root, dir, made, None)?;
        let listed = resolve(&made, files)?;
        let wanted: HashSet<String> = listed.iter().map(|n| key(n)).collect();
        let folder = list_jars(&dir.join("mods"))?;
        let trash = dir.join(TRASH);
        let stamp = (millis..).find(|n| std::fs::symlink_metadata(trash.join(n.to_string())).is_err()).unwrap_or(millis);
        let entry = trash.join(stamp.to_string());
        std::fs::create_dir_all(&entry).with_context(|| format!("cannot create {}", entry.display()))?;
        let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut moved: Vec<String> = Vec::new();
        for name in &listed {
            if let Some(found) = folder.get(&key(name)) {
                for path in found.on.iter().chain(&found.off) {
                    let file = file_name_of(path);
                    plan.push((path.clone(), entry.join(&file)));
                    moved.push(file);
                }
            }
        }
        let moves = match apply(plan) {
            Ok(moves) => moves,
            Err(e) => {
                let _ = std::fs::remove_dir(&entry);
                return Err(e);
            }
        };
        let taken = |name: &str| wanted.contains(&key(name));
        let refs: Vec<(usize, ModRef)> = made.mods.iter().enumerate().filter(|(_, m)| taken(&m.file_name)).map(|(i, m)| (i, m.clone())).collect();
        let next = Instance {
            mods: made.mods.iter().filter(|m| !taken(&m.file_name)).cloned().collect(),
            local_mods: made.local_mods.iter().filter(|n| !taken(n)).cloned().collect(),
            disabled: made.disabled.iter().filter(|n| !taken(n)).cloned().collect(),
            ..made.clone()
        };
        if let Err(e) = instance::save_in(root, &next) {
            moves.undo();
            let _ = std::fs::remove_dir(&entry);
            return Err(e);
        }
        let trashed = Trashed {
            entry: stamp.to_string(),
            files: moved,
            refs,
            local: made.local_mods.iter().filter(|n| taken(n)).cloned().collect(),
            disabled: made.disabled.iter().filter(|n| taken(n)).cloned().collect(),
        };
        if let Err(e) = serde_json::to_vec(&trashed).map_err(anyhow::Error::from).and_then(|bytes| std::fs::write(entry.join(TRASHED), bytes).map_err(Into::into)) {
            eprintln!("mod trash {}: {e:#}", entry.display());
        }
        Ok((next, trashed))
    })
}

/// Puts back what `trash` took away. Refused for good (`instance::Final`) when a file of that name is there again or the entry is gone.
pub fn untrash(id: &str, trashed: &Trashed) -> Result<Instance> {
    untrash_in(&instance::instances_dir(), id, trashed)
}

fn untrash_in(root: &Path, id: &str, trashed: &Trashed) -> Result<Instance> {
    exclusive(root, id, |dir, made| {
        if searching(dir) {
            bail!(tr("Beende zuerst die Fehlersuche."));
        }
        let gone = || Final(tr("Diese Mods liegen nicht mehr im Papierkorb."));
        let valid = !trashed.entry.is_empty() && trashed.entry.bytes().all(|b| b.is_ascii_digit());
        let entry = dir.join(TRASH).join(&trashed.entry);
        if !valid || !entry.is_dir() {
            bail!(gone());
        }
        let mods = dir.join("mods");
        let folder = list_jars(&mods)?;
        let again = |name: &str| Final(trf("Die Datei %@ gibt es wieder in der Instanz.", &[&name]));
        let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
        for name in &trashed.files {
            safe_file_name(name)?;
            let from = entry.join(name);
            if std::fs::symlink_metadata(&from).is_err() {
                bail!(gone());
            }
            let base = jar_name(name).map_or(name.as_str(), |(base, _)| base);
            if folder.contains_key(&key(base)) || std::fs::symlink_metadata(mods.join(name)).is_ok() {
                bail!(again(base));
            }
            plan.push((from, mods.join(name)));
        }
        let listed = |name: &str| made.mods.iter().any(|m| m.file_name.eq_ignore_ascii_case(name)) || made.local_mods.iter().any(|n| n.eq_ignore_ascii_case(name));
        for name in trashed.refs.iter().map(|(_, m)| &m.file_name).chain(&trashed.local) {
            if listed(name) {
                bail!(again(name));
            }
        }
        std::fs::create_dir_all(&mods).with_context(|| format!("cannot create {}", mods.display()))?;
        let moves = apply(plan)?;
        let mut next = made.clone();
        let mut refs: Vec<&(usize, ModRef)> = trashed.refs.iter().collect();
        refs.sort_by_key(|(at, _)| *at);
        for (at, m) in refs {
            next.mods.insert((*at).min(next.mods.len()), m.clone());
        }
        for name in &trashed.local {
            if !next.local_mods.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                next.local_mods.push(name.clone());
            }
        }
        for name in &trashed.disabled {
            if !next.disabled.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                next.disabled.push(name.clone());
            }
        }
        if let Err(e) = instance::save_in(root, &next) {
            moves.undo();
            return Err(e);
        }
        let _ = std::fs::remove_file(entry.join(TRASHED));
        let _ = std::fs::remove_dir(&entry);
        Ok(next)
    })
}

/// Deletes mod trash entries older than `older_than` in every instance.
pub fn purge_stale(older_than: Duration) -> Result<()> {
    purge_stale_in(&instance::instances_dir(), instance::now_millis(), older_than)
}

fn purge_stale_in(root: &Path, now: u64, older_than: Duration) -> Result<()> {
    let instances = match std::fs::read_dir(root) {
        Ok(instances) => instances,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let cutoff = now.saturating_sub(u64::try_from(older_than.as_millis()).unwrap_or(u64::MAX));
    let ahead = now.saturating_add(instance::FUTURE_MILLIS);
    let mut first = Ok(());
    for folder in instances.filter_map(Result::ok) {
        if folder.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(folder.path().join(TRASH)) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let Some(millis) = name.to_str().filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).and_then(|n| n.parse::<u64>().ok()) else { continue };
            if (millis < cutoff || millis > ahead)
                && let Err(e) = instance::remove_dir(&entry.path())
                && first.is_ok()
            {
                first = Err(e);
            }
        }
    }
    first
}

/// The troubleshooter's state, kept in `<instance>/bisect.json` so it survives a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bisect {
    pub before: Vec<String>,
    pub suspects: Vec<String>,
    pub off: Vec<String>,
    /// Answers given so far.
    pub round: u32,
    pub rounds: u32,
    pub outcome: Option<Outcome>,
}

/// How the troubleshooter ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Found(String),
    Several(Vec<String>),
    Nothing,
}

/// What the user saw with this round's mods switched off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Still,
    Gone,
}

fn ceil_log2(n: usize) -> u32 {
    if n <= 1 { 0 } else { usize::BITS - (n - 1).leading_zeros() }
}

struct Graph<'a> {
    entries: &'a [Entry],
    index: HashMap<&'a str, Vec<usize>>,
    active: HashSet<usize>,
}

impl<'a> Graph<'a> {
    fn new(scan: &'a Scan, before: &[String]) -> Self {
        let off: HashSet<String> = before.iter().map(|n| key(n)).collect();
        let active = (0..scan.entries.len()).filter(|i| scan.entries[*i].on_disk && !off.contains(&key(&scan.entries[*i].file))).collect();
        Self { entries: &scan.entries, index: providers(scan), active }
    }

    fn on(&self) -> impl Fn(usize) -> bool + '_ {
        |i| i == FIXED || self.active.contains(&i)
    }

    fn protected(&self, companion: Option<&Meta>) -> HashSet<usize> {
        let mut safe: HashSet<usize> = HashSet::new();
        let mut queue: Vec<&[String]> = companion.map(|c| c.depends.as_slice()).into_iter().collect();
        while let Some(depends) = queue.pop() {
            for dep in depends {
                for j in self.index.get(dep.as_str()).into_iter().flatten().copied().filter(|j| self.active.contains(j)) {
                    if safe.insert(j)
                        && let Some(meta) = &self.entries[j].meta
                    {
                        queue.push(&meta.depends);
                    }
                }
            }
        }
        safe
    }

    fn files(&self, set: &HashSet<usize>) -> Vec<String> {
        sorted_names(set.iter().map(|i| self.entries[*i].file.clone()).collect())
    }
}

fn plan(scan: &Scan, before: &[String], suspects: &[String]) -> Option<Vec<String>> {
    let graph = Graph::new(scan, before);
    let wanted: HashSet<String> = suspects.iter().map(|n| key(n)).collect();
    let mut ids: Vec<usize> = graph.active.iter().copied().filter(|i| wanted.contains(&key(&graph.entries[*i].file))).collect();
    ids.sort_by_key(|i| key(&graph.entries[*i].file));
    let n = ids.len();
    let (front, back) = ids.split_at(n.div_ceil(2));
    let mut tries: Vec<Vec<usize>> = vec![front.to_vec(), back.to_vec()];
    tries.extend(ids.iter().map(|i| vec![*i]));
    let on = graph.on();
    for seed in tries.into_iter().filter(|s| !s.is_empty()) {
        let mut off: HashSet<usize> = seed.into_iter().collect();
        spread(graph.entries, &graph.index, &on, &mut off);
        let hits = ids.iter().filter(|i| off.contains(i)).count();
        if hits > 0 && hits < n {
            return Some(graph.files(&off));
        }
    }
    None
}

fn advance(mut state: Bisect, scan: &Scan) -> Bisect {
    match plan(scan, &state.before, &state.suspects) {
        Some(off) => {
            state.off = off;
            state.rounds = state.round + ceil_log2(state.suspects.len());
        }
        None => {
            state.off = Vec::new();
            state.rounds = state.round;
            state.outcome = Some(Outcome::Several(state.suspects.clone()));
        }
    }
    state
}

pub(super) fn first(scan: &Scan) -> Bisect {
    let before: Vec<String> = scan.entries.iter().filter(|e| !e.enabled).map(|e| e.file.clone()).collect();
    let graph = Graph::new(scan, &before);
    let protected = graph.protected(scan.companion.as_ref());
    let open: HashSet<usize> = graph.active.difference(&protected).copied().collect();
    let suspects = graph.files(&open);
    advance(Bisect { before, suspects, off: Vec::new(), round: 0, rounds: 0, outcome: None }, scan)
}

pub(super) fn next(state: &Bisect, scan: &Scan, answer: Answer) -> Bisect {
    let mut state = state.clone();
    if state.outcome.is_some() {
        return state;
    }
    let off: HashSet<String> = state.off.iter().map(|n| key(n)).collect();
    let present: HashSet<String> = scan.entries.iter().filter(|e| e.on_disk).map(|e| key(&e.file)).collect();
    let gone = answer == Answer::Gone;
    state.suspects.retain(|s| present.contains(&key(s)) && off.contains(&key(s)) == gone);
    state.round += 1;
    state.off = Vec::new();
    state.rounds = state.round;
    match state.suspects.as_slice() {
        [] => state.outcome = Some(Outcome::Nothing),
        [one] => state.outcome = Some(Outcome::Found(one.clone())),
        _ => return advance(state, scan),
    }
    state
}

fn applied(state: &Bisect) -> Vec<String> {
    let mut all = state.before.clone();
    if state.outcome.is_none() {
        for name in &state.off {
            if !all.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                all.push(name.clone());
            }
        }
    }
    all
}

fn read_state(dir: &Path) -> Option<Bisect> {
    std::fs::read(dir.join(STATE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn write_state(dir: &Path, state: &Bisect) -> Result<()> {
    net::write_atomic(&dir.join(STATE), &serde_json::to_vec_pretty(state)?)
}

pub(super) fn drop_state(dir: &Path) -> Result<()> {
    match std::fs::remove_file(dir.join(STATE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).with_context(|| format!("cannot delete {}", dir.join(STATE).display())),
        _ => Ok(()),
    }
}

/// Starts the troubleshooter: switches off the first half and saves the state. Refused while a game runs or when fewer than two mods are on.
pub fn start(id: &str) -> Result<(Instance, Bisect)> {
    start_in(&instance::instances_dir(), id)
}

fn start_in(root: &Path, id: &str) -> Result<(Instance, Bisect)> {
    exclusive(root, id, |dir, made| {
        if read_state(dir).is_some_and(|s| s.outcome.is_none()) {
            bail!(tr("Die Fehlersuche läuft schon."));
        }
        let (made, scan) = scan_locked(root, dir, made, None)?;
        if scan.entries.iter().filter(|e| e.on_disk && e.enabled).count() < 2 {
            bail!(tr("Zum Eingrenzen müssen mindestens zwei Mods an sein."));
        }
        let state = first(&scan);
        if state.suspects.len() < 2 {
            bail!(tr("Zum Eingrenzen sind mindestens zwei Mods nötig, die sich abschalten lassen."));
        }
        let made = commit(root, dir, &made, &applied(&state))?;
        if let Err(e) = write_state(dir, &state) {
            let _ = commit(root, dir, &made, &state.before);
            return Err(e);
        }
        Ok((made, state))
    })
}

/// Takes the user's answer for the current round and switches to the next one; when it is over, puts every mod back as it was and keeps the outcome.
pub fn answer(id: &str, answer: Answer) -> Result<(Instance, Bisect)> {
    answer_in(&instance::instances_dir(), id, answer)
}

fn answer_in(root: &Path, id: &str, answer: Answer) -> Result<(Instance, Bisect)> {
    exclusive(root, id, |dir, made| {
        let state = read_state(dir).filter(|s| s.outcome.is_none()).ok_or_else(|| anyhow!(tr("Die Fehlersuche läuft nicht.")))?;
        let (made, scan) = scan_locked(root, dir, made, None)?;
        let following = next(&state, &scan, answer);
        let made = commit(root, dir, &made, &applied(&following))?;
        if let Err(e) = write_state(dir, &following) {
            let _ = commit(root, dir, &made, &state.before);
            return Err(e);
        }
        Ok((made, following))
    })
}

/// Ends the troubleshooter: every mod back as before it started, state file removed.
pub fn stop(id: &str) -> Result<Instance> {
    stop_in(&instance::instances_dir(), id)
}

fn stop_in(root: &Path, id: &str) -> Result<Instance> {
    let finished = shared(root, id, |dir, made| {
        if read_state(dir).is_some_and(|s| s.outcome.is_none()) {
            return Ok(None);
        }
        drop_state(dir)?;
        Ok(Some(made))
    })?;
    if let Some(made) = finished {
        return Ok(made);
    }
    exclusive(root, id, |dir, made| {
        let made = match read_state(dir) {
            Some(state) => commit(root, dir, &made, &state.before)?,
            None => made,
        };
        drop_state(dir)?;
        Ok(made)
    })
}

/// The troubleshooter state of instance `id`, if one runs or has an outcome to show.
pub fn load(id: &str) -> Option<Bisect> {
    load_in(&instance::instances_dir(), id)
}

fn load_in(root: &Path, id: &str) -> Option<Bisect> {
    instance::valid_id(id).then(|| read_state(&instance::dir_in(root, id))).flatten()
}

/// Forgets a finished troubleshooter's outcome.
pub fn dismiss(id: &str) -> Result<()> {
    dismiss_in(&instance::instances_dir(), id)
}

fn dismiss_in(root: &Path, id: &str) -> Result<()> {
    if !instance::valid_id(id) {
        bail!("invalid instance id {id:?}");
    }
    let Some(dir) = super::folders::find_fresh(root, id) else { return Ok(()) };
    with_mods_lock(&dir, || {
        if read_state(&dir).is_some_and(|s| s.outcome.is_some()) {
            drop_state(&dir)?;
        }
        Ok(())
    })
}

pub(super) fn searching(dir: &Path) -> bool {
    read_state(dir).is_some_and(|s| s.outcome.is_none())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::core::instance::Loader;

    fn setup() -> (tempfile::TempDir, PathBuf, Instance) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("instances");
        let made = Instance::new("Farm", "26.3", Loader::Fabric(None));
        instance::save_in(&root, &made).unwrap();
        std::fs::create_dir_all(crate::core::instance::dir_in(&root, &made.id).join("mods")).unwrap();
        (dir, root, made)
    }

    fn put(root: &Path, made: &Instance, name: &str, bytes: &[u8]) {
        std::fs::write(crate::core::instance::dir_in(root, &made.id).join("mods").join(name), bytes).unwrap();
    }

    fn mods_of(root: &Path, made: &Instance) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(crate::core::instance::dir_in(root, &made.id).join("mods")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    fn saved(root: &Path, made: &Instance) -> Instance {
        saved_in(root, &made.id).unwrap()
    }

    fn mod_ref(file: &str) -> ModRef {
        ModRef { project_id: format!("p-{file}"), version_id: format!("v-{file}"), title: format!("T {file}"), file_name: file.into(), url: "http://127.0.0.1:1/x".into(), sha1: "00".into() }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn save_with(root: &Path, made: &mut Instance, mods: &[&str], local: &[&str], disabled: &[&str]) {
        made.mods = mods.iter().map(|f| mod_ref(f)).collect();
        made.local_mods = names(local);
        made.disabled = names(disabled);
        instance::save_in(root, made).unwrap();
    }

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(256, 256, image::Rgba([10, 200, 30, 255])).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn jar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn switching_mods_off_and_on_renames_their_files_and_restores_them_exactly() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &["r.jar", "later.jar"], &["a.jar", "B.jar"], &[]);
        for name in ["r.jar", "a.jar", "B.jar"] {
            put(&root, &made, name, name.as_bytes());
        }
        let off = set_enabled_in(&root, &made.id, &names(&["R.JAR", "a.jar", "later.jar"]), false).unwrap();
        assert_eq!(off.disabled, ["r.jar", "later.jar", "a.jar"]);
        assert_eq!(mods_of(&root, &made), ["B.jar", "a.jar.disabled", "r.jar.disabled"]);
        assert_eq!(saved(&root, &made), off);
        let again = set_enabled_in(&root, &made.id, &names(&["a.jar"]), false).unwrap();
        assert_eq!(again.disabled, off.disabled);
        let back = set_enabled_in(&root, &made.id, &names(&["r.jar", "a.jar", "later.jar"]), true).unwrap();
        assert!(back.disabled.is_empty());
        assert_eq!(mods_of(&root, &made), ["B.jar", "a.jar", "r.jar"]);
        assert_eq!(std::fs::read(crate::core::instance::dir_in(&root, &made.id).join("mods").join("r.jar")).unwrap(), b"r.jar");
        assert_eq!(saved(&root, &made), back);
        assert!(set_enabled_in(&root, &made.id, &names(&["unknown.jar"]), false).is_err());
        assert!(set_enabled_in(&root, "../x", &[], false).is_err());
    }

    #[test]
    fn a_name_that_is_taken_stops_the_switch_before_any_file_moves() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &[], &["a.jar", "b.jar"], &[]);
        for name in ["a.jar", "b.jar", "b.jar.disabled"] {
            put(&root, &made, name, b"x");
        }
        let err = set_enabled_in(&root, &made.id, &names(&["a.jar", "b.jar"]), false).unwrap_err();
        assert!(err.to_string().contains("b.jar.disabled"), "{err:#}");
        assert_eq!(mods_of(&root, &made), ["a.jar", "b.jar", "b.jar.disabled"]);
        assert!(saved(&root, &made).disabled.is_empty());
    }

    #[test]
    fn a_rename_that_fails_undoes_the_ones_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        std::fs::write(p("a"), "a").unwrap();
        std::fs::write(p("b"), "b").unwrap();
        let err = apply(vec![(p("a"), p("a2")), (p("b"), p("b2")), (p("missing"), p("c2"))]);
        assert!(err.is_err());
        assert!(p("a").is_file() && p("b").is_file() && !p("a2").exists() && !p("b2").exists());
        let moves = apply(vec![(p("a"), p("a2")), (p("b"), p("b2"))]).unwrap();
        assert!(p("a2").is_file() && p("b2").is_file());
        moves.undo();
        assert!(p("a").is_file() && p("b").is_file());
    }

    #[test]
    fn nothing_changes_while_a_game_of_the_instance_runs() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &[], &["a.jar", "b.jar"], &[]);
        put(&root, &made, "a.jar", b"x");
        put(&root, &made, "b.jar", b"x");
        let running = instance::hold_in(&root, &made.id).unwrap();
        assert!(set_enabled_in(&root, &made.id, &names(&["a.jar"]), false).is_err());
        assert!(trash_in(&root, &made.id, &names(&["a.jar"]), 5).is_err());
        assert!(start_in(&root, &made.id).is_err());
        assert_eq!(mods_of(&root, &made), ["a.jar", "b.jar"]);
        drop(running);
        assert!(set_enabled_in(&root, &made.id, &names(&["a.jar"]), false).is_ok());
    }

    #[test]
    fn trashed_mods_come_back_with_their_files_refs_in_place_and_switches() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &["a.jar", "b.jar", "c.jar", "d.jar"], &["keep.jar", "mine.jar"], &["keep.jar", "b.jar", "mine.jar"]);
        for name in ["a.jar", "b.jar.disabled", "c.jar", "mine.jar.disabled", "keep.jar.disabled"] {
            put(&root, &made, name, name.as_bytes());
        }
        let before = saved(&root, &made);
        let (after, trashed) = trash_in(&root, &made.id, &names(&["A.jar", "c.jar", "mine.jar", "b.jar"]), 1000).unwrap();
        assert_eq!(after.mods.iter().map(|m| m.file_name.as_str()).collect::<Vec<_>>(), ["d.jar"]);
        assert_eq!((after.local_mods.clone(), after.disabled.clone()), (names(&["keep.jar"]), names(&["keep.jar"])));
        assert_eq!(saved(&root, &made), after);
        assert_eq!(trashed.entry, "1000");
        assert_eq!(trashed.refs.iter().map(|(i, m)| (*i, m.file_name.as_str())).collect::<Vec<_>>(), [(0, "a.jar"), (1, "b.jar"), (2, "c.jar")]);
        assert_eq!((trashed.local.clone(), trashed.disabled.clone()), (names(&["mine.jar"]), names(&["b.jar", "mine.jar"])));
        assert_eq!(mods_of(&root, &made), ["keep.jar.disabled"]);
        let held = crate::core::instance::dir_in(&root, &made.id).join(".trash").join("1000");
        assert_eq!(std::fs::read(held.join("b.jar.disabled")).unwrap(), b"b.jar.disabled");
        let mut kept: Trashed = serde_json::from_slice(&std::fs::read(held.join(TRASHED)).unwrap()).unwrap();
        kept.entry = "1000".into();
        assert_eq!(kept, trashed);
        let back = untrash_in(&root, &made.id, &kept).unwrap();
        assert_eq!(back, before);
        assert_eq!(saved(&root, &made), before);
        assert_eq!(mods_of(&root, &made), ["a.jar", "b.jar.disabled", "c.jar", "keep.jar.disabled", "mine.jar.disabled"]);
        assert!(!held.exists());
    }

    #[test]
    fn a_trash_entry_is_refused_for_good_when_its_files_are_back_or_it_is_gone() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &["a.jar"], &["mine.jar"], &[]);
        put(&root, &made, "a.jar", b"a");
        put(&root, &made, "mine.jar", b"m");
        let (_, trashed) = trash_in(&root, &made.id, &names(&["a.jar", "mine.jar"]), 7).unwrap();
        let is_final = |e: anyhow::Error| e.chain().any(|c| c.downcast_ref::<Final>().is_some());
        put(&root, &made, "A.JAR.disabled", b"new");
        assert!(is_final(untrash_in(&root, &made.id, &trashed).unwrap_err()));
        std::fs::remove_file(crate::core::instance::dir_in(&root, &made.id).join("mods").join("A.JAR.disabled")).unwrap();
        let held = crate::core::instance::dir_in(&root, &made.id).join(".trash").join("7");
        std::fs::remove_file(held.join("mine.jar")).unwrap();
        assert!(is_final(untrash_in(&root, &made.id, &trashed).unwrap_err()));
        assert!(mods_of(&root, &made).is_empty() && held.join("a.jar").is_file());
        std::fs::remove_dir_all(&held).unwrap();
        assert!(is_final(untrash_in(&root, &made.id, &trashed).unwrap_err()));
        let forged = Trashed { entry: "../..".into(), ..trashed };
        assert!(is_final(untrash_in(&root, &made.id, &forged).unwrap_err()));
    }

    #[test]
    fn a_mod_listed_again_blocks_its_own_untrash_and_two_trashes_in_one_millisecond_get_two_entries() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &["a.jar", "b.jar"], &[], &[]);
        put(&root, &made, "a.jar", b"a");
        put(&root, &made, "b.jar", b"b");
        let (_, first) = trash_in(&root, &made.id, &names(&["a.jar"]), 50).unwrap();
        let (after, second) = trash_in(&root, &made.id, &names(&["b.jar"]), 50).unwrap();
        assert_ne!(first.entry, second.entry);
        let mut readded = after.clone();
        readded.mods.push(mod_ref("a.jar"));
        instance::save_in(&root, &readded).unwrap();
        assert!(untrash_in(&root, &made.id, &first).unwrap_err().chain().any(|c| c.downcast_ref::<Final>().is_some()));
        let restored = untrash_in(&root, &made.id, &second).unwrap();
        assert_eq!(restored.mods.iter().map(|m| m.file_name.as_str()).collect::<Vec<_>>(), ["b.jar", "a.jar"]);
    }

    #[test]
    fn stale_mod_trash_goes_and_fresh_trash_and_other_names_stay() {
        let (_dir, root, made) = setup();
        let trash = crate::core::instance::dir_in(&root, &made.id).join(".trash");
        for name in ["1000", "700000", "notes", "9x"] {
            std::fs::create_dir_all(trash.join(name)).unwrap();
        }
        std::fs::create_dir_all(root.join(".trash").join(format!("{}-5", made.id))).unwrap();
        purge_stale_in(&root, 800_000, Duration::from_secs(600)).unwrap();
        assert!(!trash.join("1000").exists());
        for name in ["700000", "notes", "9x"] {
            assert!(trash.join(name).exists(), "{name}");
        }
        assert!(root.join(".trash").join(format!("{}-5", made.id)).exists());
        let far = (800_000 + instance::FUTURE_MILLIS + 1).to_string();
        std::fs::create_dir_all(trash.join(&far)).unwrap();
        purge_stale_in(&root, 800_000, Duration::from_secs(600)).unwrap();
        assert!(!trash.join(&far).exists() && trash.join("700000").exists());
        purge_stale_in(&root.join("none"), 1, Duration::ZERO).unwrap();
    }

    #[test]
    fn a_scan_adopts_hand_dropped_jars_reads_the_folder_for_on_and_off_and_drops_vanished_local_names() {
        let (dir, root, mut made) = setup();
        save_with(&root, &mut made, &["r.jar", "pending.jar", "both.jar"], &["gone.jar", "kept.jar", "R.jar"], &["pending.jar", "kept.jar"]);
        put(&root, &made, "r.jar.disabled", b"r");
        put(&root, &made, "kept.jar", b"k");
        put(&root, &made, "both.jar", b"b");
        put(&root, &made, "both.jar.disabled", b"b");
        put(&root, &made, "Dropped.jar", b"d");
        put(&root, &made, "zeta.jar.disabled", b"z");
        put(&root, &made, "readme.txt", b"t");
        std::fs::create_dir_all(crate::core::instance::dir_in(&root, &made.id).join("mods").join("sub.jar")).unwrap();
        let companion = jar_bytes(&[("fabric.mod.json", br#"{"id":"manycraft","depends":{"fabricloader":"*","fabric-api":"*"}}"#)]);
        put(&root, &made, COMPANION_FILE, &companion);
        let icons = dir.path().join("icons");
        let (now, scan) = scan_in(&root, &icons, &made.id).unwrap();
        let rows: Vec<(&str, bool, bool, bool)> = scan.entries.iter().map(|e| (e.file.as_str(), e.on_disk, e.enabled, e.source == Source::Own)).collect();
        assert_eq!(
            rows,
            [("r.jar", true, false, false), ("pending.jar", false, false, false), ("both.jar", true, true, false), ("kept.jar", true, true, true), ("Dropped.jar", true, true, true), ("zeta.jar", true, false, true)]
        );
        assert_eq!(now.local_mods, ["kept.jar", "Dropped.jar", "zeta.jar"]);
        assert_eq!(now.disabled, ["pending.jar", "r.jar", "zeta.jar"]);
        assert_eq!(saved(&root, &made), now);
        assert_eq!(scan.companion.as_ref().map(|c| c.depends.clone()), Some(names(&["fabric-api"])));
        assert_eq!(scan.entries[0].size, 1);
        assert!(scan.entries[0].added.is_some() && scan.entries[0].updated.is_some() && scan.entries[1].updated.is_none());
        assert_eq!(scan_in(&root, &icons, &made.id).unwrap().0, now);
        assert_eq!(scan.entries[0].name(), "T r.jar");
        assert_eq!(scan.entries[3].name(), "kept");
    }

    #[test]
    fn a_scan_reads_a_jars_metadata_and_makes_its_icon_once() {
        let (dir, root, mut made) = setup();
        save_with(&root, &mut made, &[], &[], &[]);
        let json = r#"{"id":"Sodium","name":"Sodium","version":"0.6.0","icon":"icon.png","depends":{"fabric-api":"*","minecraft":"*"}}"#;
        put(&root, &made, "sodium.jar", &jar_bytes(&[("fabric.mod.json", json.as_bytes()), ("icon.png", &png())]));
        put(&root, &made, "broken.jar", &jar_bytes(&[("fabric.mod.json", br#"{"id":"b","icon":"missing.png"}"#)]));
        put(&root, &made, "plain.jar", b"not a zip");
        let icons = dir.path().join("icons");
        let (_, scan) = scan_in(&root, &icons, &made.id).unwrap();
        let by = |file: &str| scan.entries.iter().find(|e| e.file == file).unwrap();
        let sodium = by("sodium.jar");
        assert_eq!((sodium.id(), sodium.version(), sodium.name().as_str()), (Some("sodium"), Some("0.6.0"), "Sodium"));
        assert_eq!(sodium.meta.as_ref().unwrap().depends, ["fabric-api"]);
        let icon = sodium.icon.clone().unwrap();
        assert_eq!(image::image_dimensions(&icon).unwrap(), (72, 72));
        assert!(icon.starts_with(&icons));
        assert!(by("broken.jar").meta.is_some() && by("broken.jar").icon.is_none());
        assert!(by("plain.jar").meta.is_none() && by("plain.jar").icon.is_none() && by("plain.jar").name() == "plain");
        std::fs::write(&icon, b"cached").unwrap();
        let (_, again) = scan_in(&root, &icons, &made.id).unwrap();
        assert_eq!(again.entries.iter().find(|e| e.file == "sodium.jar").unwrap().icon.as_ref(), Some(&icon));
        assert_eq!(std::fs::read(&icon).unwrap(), b"cached");
        set_enabled_in(&root, &made.id, &names(&["sodium.jar"]), false).unwrap();
        let (_, off) = scan_in(&root, &icons, &made.id).unwrap();
        let moved = off.entries.iter().find(|e| e.file == "sodium.jar").unwrap();
        assert!(!moved.enabled && moved.icon.as_ref() == Some(&icon) && moved.id() == Some("sodium"));
    }

    #[test]
    fn a_scan_of_an_unknown_instance_is_an_error_and_leaves_no_folder_behind() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("instances");
        std::fs::create_dir_all(&root).unwrap();
        assert!(scan_in(&root, &dir.path().join("icons"), "nothere").is_err());
        assert!(scan_in(&root, &dir.path().join("icons"), "../x").is_err());
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    }

    fn entry(file: &str, id: &str, depends: &[&str], provides: &[&str], enabled: bool) -> Entry {
        let meta = Meta { id: id.into(), name: id.to_uppercase(), version: "1.0".into(), icon: None, depends: names(depends), provides: names(provides) };
        Entry { file: file.into(), on_disk: true, enabled, source: Source::Own, meta: Some(meta), icon: None, added: None, updated: None, size: 0 }
    }

    fn scan_of(entries: Vec<Entry>, companion: Option<&[&str]>) -> Scan {
        Scan { entries, companion: companion.map(|d| Meta { id: "manycraft".into(), name: "Manycraft".into(), depends: names(d), ..Meta::default() }), managed: Vec::new() }
    }

    #[test]
    fn switching_off_a_mod_names_the_mods_that_need_it_directly_through_each_other_through_provides_and_the_companion() {
        let scan = scan_of(
            vec![
                entry("lib.jar", "lib", &[], &[], true),
                entry("mid.jar", "mid", &["lib"], &[], true),
                entry("top.jar", "top", &["mid", "minecraft-extra"], &[], true),
                entry("api.jar", "api-impl", &[], &["api"], true),
                entry("user.jar", "user", &["api"], &[], true),
                entry("twin1.jar", "twin1", &[], &["twin"], true),
                entry("twin2.jar", "twin2", &[], &["twin"], true),
                entry("pair.jar", "pair", &["twin"], &[], true),
                entry("off.jar", "offmod", &["lib"], &[], false),
                entry("free.jar", "free", &["nobody-has-this"], &[], true),
                entry("fabric-api.jar", "fabric-api", &[], &[], true),
            ],
            Some(&["fabric-api"]),
        );
        assert_eq!(needed_by(&scan, &names(&["lib.jar"])), ["MID", "TOP"]);
        assert_eq!(needed_by(&scan, &names(&["LIB.jar", "api.jar"])), ["MID", "TOP", "USER"]);
        assert_eq!(needed_by(&scan, &names(&["mid.jar"])), ["TOP"]);
        assert_eq!(needed_by(&scan, &names(&["twin1.jar"])), Vec::<String>::new());
        assert_eq!(needed_by(&scan, &names(&["twin1.jar", "twin2.jar"])), ["PAIR"]);
        assert_eq!(needed_by(&scan, &names(&["fabric-api.jar"])), ["Manycraft"]);
        assert_eq!(needed_by(&scan, &names(&["top.jar", "free.jar", "off.jar", "unknown.jar"])), Vec::<String>::new());
        let chained = scan_of(vec![entry("fabric-api.jar", "fabric-api", &["base"], &[], true), entry("base.jar", "base", &[], &[], true), entry("x.jar", "x", &["base"], &[], true)], Some(&["fabric-api"]));
        assert_eq!(needed_by(&chained, &names(&["base.jar"])), ["FABRIC-API", "Manycraft", "X"]);
    }

    #[test]
    fn switching_on_a_mod_names_the_switched_off_mods_it_needs_through_each_other() {
        let scan = scan_of(
            vec![
                entry("top.jar", "top", &["mid", "api"], &[], false),
                entry("mid.jar", "mid", &["lib", "nobody"], &[], false),
                entry("lib.jar", "lib", &[], &[], false),
                entry("impl.jar", "impl", &[], &["api"], false),
                entry("on.jar", "on", &[], &[], true),
                entry("needs-on.jar", "needs-on", &["on", "lib"], &[], false),
                entry("loop-a.jar", "loop-a", &["loop-b"], &[], false),
                entry("loop-b.jar", "loop-b", &["loop-a"], &[], false),
            ],
            None,
        );
        assert_eq!(missing_for(&scan, &names(&["top.jar"])), ["impl.jar", "lib.jar", "mid.jar"]);
        assert_eq!(missing_for(&scan, &names(&["TOP.jar", "mid.jar"])), ["impl.jar", "lib.jar"]);
        assert_eq!(missing_for(&scan, &names(&["needs-on.jar"])), ["lib.jar"]);
        assert_eq!(missing_for(&scan, &names(&["loop-a.jar"])), ["loop-b.jar"]);
        assert_eq!(missing_for(&scan, &names(&["lib.jar", "on.jar"])), Vec::<String>::new());
    }

    #[test]
    fn what_every_switched_on_mod_needs_matches_asking_for_each_alone() {
        let scan = scan_of(
            vec![
                entry("top.jar", "top", &["mid", "api"], &[], true),
                entry("mid.jar", "mid", &["lib", "nobody"], &[], false),
                entry("lib.jar", "lib", &[], &[], false),
                entry("impl.jar", "impl", &[], &["api"], false),
                entry("on.jar", "on", &[], &[], true),
                entry("needs-on.jar", "needs-on", &["on", "lib"], &[], true),
                entry("loop-a.jar", "loop-a", &["loop-b"], &[], true),
                entry("loop-b.jar", "loop-b", &["loop-a"], &[], false),
            ],
            None,
        );
        let all = missing_for_enabled(&scan);
        for e in scan.entries.iter().filter(|e| e.enabled && e.on_disk) {
            let alone = missing_for(&scan, std::slice::from_ref(&e.file));
            assert_eq!(all.get(&e.file).cloned().unwrap_or_default(), alone, "{}", e.file);
        }
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn versions_compare_by_number_not_text_with_a_pre_release_below_its_release() {
        let less = |a: &str, b: &str| compare_versions(a, b) == Ordering::Less && compare_versions(b, a) == Ordering::Greater;
        assert!(less("1.9", "1.10"));
        assert!(less("1.0-beta", "1.0"));
        assert!(less("1.0.0-alpha", "1.0.0-beta"));
        assert!(less("1.0.0-beta.2", "1.0.0-beta.10"));
        assert!(less("1.0.0-rc.1", "1.0.0"));
        assert!(less("1.0-pre1", "1.0"));
        assert!(less("0.5.0-beta+mc1.20", "0.5.0+mc1.20"));
        assert!(less("1.0", "1.0.1"));
        assert!(less("0.6.0+mc1.21.2", "0.6.0+mc1.21.4"));
        assert!(less("1.0", "1.0.0+build"));
        assert!(less("1.2.3", "1.10.0"));
        assert!(less("2.0.0-beta", "10.0.0-alpha"));
        assert_eq!(compare_versions("1.0", "1.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.00", "1.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0-BETA", "1.0-beta"), Ordering::Equal);
        assert_eq!(compare_versions("", ""), Ordering::Equal);
        assert_eq!(compare_versions("日本-beta", "日本"), Ordering::Less);
    }

    fn versioned(file: &str, id: &str, version: &str, updated: u64) -> Entry {
        let mut e = entry(file, id, &[], &[], true);
        e.meta.as_mut().unwrap().version = version.into();
        e.updated = Some(UNIX_EPOCH + Duration::from_secs(updated));
        e
    }

    #[test]
    fn duplicates_list_the_newest_file_first_by_version_then_by_modified_time() {
        let mut gone = versioned("gone.jar", "sodium", "9.9", 9);
        gone.on_disk = false;
        let mut plain = entry("plain.jar", "sodium", &[], &[], true);
        plain.meta = None;
        let entries = vec![
            versioned("sodium-1.9.jar", "sodium", "1.9", 5),
            versioned("sodium-1.10.jar", "sodium", "1.10", 1),
            versioned("sodium-1.0-beta.jar", "sodium", "1.0-beta", 9),
            versioned("lithium-a.jar", "lithium", "2.0", 10),
            versioned("lithium-b.jar", "lithium", "2.0", 20),
            versioned("solo.jar", "solo", "1", 1),
            gone,
            plain,
        ];
        assert_eq!(
            duplicates(&entries),
            [
                Duplicate { id: "lithium".into(), files: names(&["lithium-b.jar", "lithium-a.jar"]) },
                Duplicate { id: "sodium".into(), files: names(&["sodium-1.10.jar", "sodium-1.9.jar", "sodium-1.0-beta.jar"]) },
            ]
        );
        assert!(duplicates(&[]).is_empty());
    }

    fn eight() -> Scan {
        scan_of((0..8).map(|i| entry(&format!("m{i}.jar"), &format!("m{i}"), &[], &[], true)).collect(), None)
    }

    fn solve(scan: &Scan, culprit: &str) -> (Bisect, u32) {
        let mut state = first(scan);
        let mut answers = 0;
        while state.outcome.is_none() {
            let gone = state.off.iter().any(|f| f == culprit);
            state = next(&state, scan, if gone { Answer::Gone } else { Answer::Still });
            answers += 1;
        }
        (state, answers)
    }

    #[test]
    fn one_culprit_among_eight_is_found_in_three_answers_wherever_it_sits() {
        let scan = eight();
        let start = first(&scan);
        assert_eq!((start.suspects.len(), start.off.len(), start.round, start.rounds, start.outcome.clone()), (8, 4, 0, 3, None));
        assert!(start.before.is_empty());
        for i in 0..8 {
            let culprit = format!("m{i}.jar");
            let (done, answers) = solve(&scan, &culprit);
            assert_eq!(done.outcome, Some(Outcome::Found(culprit.clone())), "{culprit}");
            assert_eq!((answers, done.round, done.rounds), (3, 3, 3), "{culprit}");
            assert!(done.off.is_empty());
        }
    }

    #[test]
    fn the_estimate_of_rounds_follows_the_suspects_left() {
        let scan = scan_of((0..5).map(|i| entry(&format!("m{i}.jar"), &format!("m{i}"), &[], &[], true)).collect(), None);
        let mut state = first(&scan);
        assert_eq!((state.suspects.len(), state.rounds), (5, 3));
        state = next(&state, &scan, Answer::Still);
        assert_eq!((state.round, state.suspects.len(), state.rounds), (1, 2, 2));
        let one = scan_of(vec![entry("a.jar", "a", &[], &[], true), entry("b.jar", "b", &[], &[], true)], None);
        assert_eq!(first(&one).rounds, 1);
        assert_eq!((ceil_log2(0), ceil_log2(1), ceil_log2(2), ceil_log2(3), ceil_log2(4), ceil_log2(5), ceil_log2(8), ceil_log2(9)), (0, 0, 1, 2, 2, 3, 3, 4));
    }

    fn assert_closed(scan: &Scan, state: &Bisect) {
        let off: HashSet<String> = state.before.iter().chain(&state.off).map(|n| key(n)).collect();
        let index = providers(scan);
        let on = |i: usize| !off.contains(&key(&scan.entries[i].file));
        for (i, e) in scan.entries.iter().enumerate().filter(|(i, _)| on(*i)) {
            for dep in &e.meta.as_ref().unwrap().depends {
                if let Some(all) = index.get(dep.as_str()) {
                    let was_on = all.iter().any(|j| scan.entries[*j].enabled);
                    assert!(!was_on || all.iter().any(|j| on(*j)), "{} is on while {dep} is off, round {}", scan.entries[i].file, state.round);
                }
            }
        }
    }

    #[test]
    fn a_mod_is_never_on_while_a_mod_it_needs_is_off_and_every_culprit_is_still_found() {
        let scan = scan_of(
            vec![
                entry("lib.jar", "lib", &[], &[], true),
                entry("a.jar", "a", &["lib"], &[], true),
                entry("b.jar", "b", &["lib"], &[], true),
                entry("c.jar", "c", &[], &["c-api"], true),
                entry("d.jar", "d", &["c-api"], &[], true),
                entry("e.jar", "e", &["d"], &[], true),
                entry("f.jar", "f", &[], &[], true),
                entry("g.jar", "g", &["a"], &[], true),
            ],
            None,
        );
        for culprit in ["lib.jar", "a.jar", "b.jar", "c.jar", "d.jar", "e.jar", "f.jar", "g.jar"] {
            let mut state = first(&scan);
            while state.outcome.is_none() {
                assert_closed(&scan, &state);
                let gone = state.off.iter().any(|f| f == culprit);
                state = next(&state, &scan, if gone { Answer::Gone } else { Answer::Still });
            }
            assert_eq!(state.outcome, Some(Outcome::Found(culprit.into())), "{culprit}");
        }
    }

    #[test]
    fn the_mods_the_companion_needs_are_never_suspects_and_never_switched_off() {
        let scan = scan_of(
            vec![
                entry("fabric-api.jar", "fabric-api", &["fabric-base"], &["fabric-x"], true),
                entry("fabric-base.jar", "fabric-base", &[], &[], true),
                entry("m0.jar", "m0", &["fabric-api"], &[], true),
                entry("m1.jar", "m1", &[], &[], true),
                entry("m2.jar", "m2", &[], &[], true),
                entry("m3.jar", "m3", &["m2"], &[], true),
            ],
            Some(&["fabric-api"]),
        );
        let start = first(&scan);
        assert_eq!(start.suspects, ["m0.jar", "m1.jar", "m2.jar", "m3.jar"]);
        for culprit in ["m0.jar", "m1.jar", "m2.jar", "m3.jar"] {
            let mut state = first(&scan);
            while state.outcome.is_none() {
                assert!(!state.off.iter().any(|f| f.starts_with("fabric")), "{state:?}");
                let gone = state.off.iter().any(|f| f == culprit);
                state = next(&state, &scan, if gone { Answer::Gone } else { Answer::Still });
            }
            assert_eq!(state.outcome, Some(Outcome::Found(culprit.into())));
        }
    }

    #[test]
    fn mods_that_depend_on_each_other_cannot_be_split_and_end_as_several() {
        let scan = scan_of(vec![entry("x.jar", "x", &["y"], &[], true), entry("y.jar", "y", &["x"], &[], true)], None);
        let start = first(&scan);
        assert_eq!(start.outcome, Some(Outcome::Several(names(&["x.jar", "y.jar"]))));
        assert!(start.off.is_empty());
        let mixed = scan_of(vec![entry("x.jar", "x", &["y"], &[], true), entry("y.jar", "y", &["x"], &[], true), entry("z.jar", "z", &[], &[], true)], None);
        let (done, _) = solve(&mixed, "x.jar");
        assert_eq!(done.outcome, Some(Outcome::Several(names(&["x.jar", "y.jar"]))));
        let (clean, _) = solve(&mixed, "z.jar");
        assert_eq!(clean.outcome, Some(Outcome::Found("z.jar".into())));
    }

    #[test]
    fn mods_already_switched_off_stay_off_and_are_no_suspects() {
        let mut scan = eight();
        scan.entries[3].enabled = false;
        scan.entries[6].enabled = false;
        let start = first(&scan);
        assert_eq!(start.before, ["m3.jar", "m6.jar"]);
        assert_eq!(start.suspects.len(), 6);
        assert_eq!(applied(&start), ["m3.jar", "m6.jar", start.off[0].as_str(), start.off[1].as_str(), start.off[2].as_str()]);
        let (done, answers) = solve(&scan, "m5.jar");
        assert_eq!(done.outcome, Some(Outcome::Found("m5.jar".into())));
        assert_eq!(answers, 3);
        assert_eq!(applied(&done), ["m3.jar", "m6.jar"]);
        assert_eq!(next(&done, &scan, Answer::Still), done);
    }

    #[test]
    fn a_suspect_that_vanished_between_answers_is_dropped() {
        let mut scan = eight();
        let state = first(&scan);
        scan.entries.retain(|e| e.file != "m7.jar");
        let after = next(&state, &scan, Answer::Still);
        assert!(!after.suspects.contains(&"m7.jar".to_owned()));
        assert_eq!(after.suspects.len(), 3);
    }

    #[test]
    fn the_state_survives_a_restart_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = first(&eight());
        write_state(dir.path(), &state).unwrap();
        assert_eq!(read_state(dir.path()), Some(state.clone()));
        for outcome in [Outcome::Found("a.jar".into()), Outcome::Several(names(&["a.jar", "b.jar"])), Outcome::Nothing] {
            state.outcome = Some(outcome);
            write_state(dir.path(), &state).unwrap();
            assert_eq!(read_state(dir.path()), Some(state.clone()));
        }
        drop_state(dir.path()).unwrap();
        drop_state(dir.path()).unwrap();
        assert_eq!(read_state(dir.path()), None);
        std::fs::write(dir.path().join(STATE), "{").unwrap();
        assert_eq!(read_state(dir.path()), None);
    }

    fn four(root: &Path, made: &mut Instance) {
        save_with(root, made, &[], &[], &[]);
        for i in 0..4 {
            put(root, made, &format!("m{i}.jar"), b"x");
        }
        shared(root, &made.id, |dir, saved| scan_locked(root, dir, saved, None)).unwrap();
    }

    #[test]
    fn nothing_switches_trashes_or_comes_back_from_the_trash_while_a_search_runs() {
        let (_dir, root, mut made) = setup();
        four(&root, &mut made);
        let (_, trashed) = trash_in(&root, &made.id, &names(&["m3.jar"]), 7).unwrap();
        start_in(&root, &made.id).unwrap();
        let before = mods_of(&root, &made);
        assert!(set_enabled_in(&root, &made.id, &names(&["m0.jar"]), true).is_err());
        assert!(trash_in(&root, &made.id, &names(&["m2.jar"]), 8).is_err());
        assert!(untrash_in(&root, &made.id, &trashed).is_err());
        assert_eq!(mods_of(&root, &made), before);
    }

    #[test]
    fn the_troubleshooter_switches_halves_on_disk_finds_the_culprit_and_puts_everything_back() {
        let (_dir, root, mut made) = setup();
        four(&root, &mut made);
        let (now, state) = start_in(&root, &made.id).unwrap();
        assert_eq!((state.suspects.len(), state.off.clone()), (4, names(&["m0.jar", "m1.jar"])));
        assert_eq!(mods_of(&root, &made), ["m0.jar.disabled", "m1.jar.disabled", "m2.jar", "m3.jar"]);
        assert_eq!(now.disabled, ["m0.jar", "m1.jar"]);
        assert_eq!(load_in(&root, &made.id), Some(state.clone()));
        assert!(start_in(&root, &made.id).is_err());
        let (now, state) = answer_in(&root, &made.id, Answer::Still).unwrap();
        assert_eq!((state.round, state.suspects.clone(), state.off.clone()), (1, names(&["m2.jar", "m3.jar"]), names(&["m2.jar"])));
        assert_eq!(mods_of(&root, &made), ["m0.jar", "m1.jar", "m2.jar.disabled", "m3.jar"]);
        assert_eq!(now.disabled, ["m2.jar"]);
        let (now, state) = answer_in(&root, &made.id, Answer::Gone).unwrap();
        assert_eq!(state.outcome, Some(Outcome::Found("m2.jar".into())));
        assert!(now.disabled.is_empty());
        assert_eq!(mods_of(&root, &made), ["m0.jar", "m1.jar", "m2.jar", "m3.jar"]);
        assert_eq!(load_in(&root, &made.id), Some(state));
        assert!(answer_in(&root, &made.id, Answer::Gone).is_err());
        dismiss_in(&root, &made.id).unwrap();
        assert_eq!(load_in(&root, &made.id), None);
        assert_eq!(saved(&root, &made).disabled, Vec::<String>::new());
    }

    #[test]
    fn stopping_the_troubleshooter_restores_what_was_off_before_it_started() {
        let (_dir, root, mut made) = setup();
        four(&root, &mut made);
        set_enabled_in(&root, &made.id, &names(&["m3.jar"]), false).unwrap();
        let (_, state) = start_in(&root, &made.id).unwrap();
        assert_eq!(state.before, ["m3.jar"]);
        assert_eq!(state.suspects, ["m0.jar", "m1.jar", "m2.jar"]);
        assert!(mods_of(&root, &made).iter().filter(|n| n.ends_with(".disabled")).count() >= 2);
        let restored = stop_in(&root, &made.id).unwrap();
        assert_eq!(restored.disabled, ["m3.jar"]);
        assert_eq!(mods_of(&root, &made), ["m0.jar", "m1.jar", "m2.jar", "m3.jar.disabled"]);
        assert_eq!(load_in(&root, &made.id), None);
        assert_eq!(saved(&root, &made), restored);
        assert_eq!(stop_in(&root, &made.id).unwrap(), restored);
        let (_, again) = start_in(&root, &made.id).unwrap();
        assert_eq!(again.before, ["m3.jar"]);
        let running = instance::hold_in(&root, &made.id).unwrap();
        assert!(stop_in(&root, &made.id).is_err() && answer_in(&root, &made.id, Answer::Still).is_err());
        assert!(load_in(&root, &made.id).is_some());
        drop(running);
        stop_in(&root, &made.id).unwrap();
    }

    #[test]
    fn the_troubleshooter_needs_two_mods_that_are_on_and_can_be_switched_off() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &[], &[], &[]);
        assert!(start_in(&root, &made.id).is_err());
        put(&root, &made, "only.jar", b"x");
        assert!(start_in(&root, &made.id).is_err());
        put(&root, &made, "other.jar.disabled", b"x");
        assert!(start_in(&root, &made.id).is_err());
        assert_eq!(mods_of(&root, &made), ["only.jar", "other.jar.disabled"]);
        assert_eq!(load_in(&root, &made.id), None);
        assert_eq!(load_in(&root, "../x"), None);
        assert!(dismiss_in(&root, "../x").is_err());
    }

    #[test]
    fn a_finished_outcome_can_be_stopped_without_stopping_games_and_a_running_search_is_not_dismissed() {
        let (_dir, root, mut made) = setup();
        four(&root, &mut made);
        start_in(&root, &made.id).unwrap();
        dismiss_in(&root, &made.id).unwrap();
        assert!(load_in(&root, &made.id).is_some());
        answer_in(&root, &made.id, Answer::Still).unwrap();
        answer_in(&root, &made.id, Answer::Still).unwrap();
        assert!(matches!(load_in(&root, &made.id).unwrap().outcome, Some(Outcome::Found(_))));
        let running = instance::hold_in(&root, &made.id).unwrap();
        stop_in(&root, &made.id).unwrap();
        assert_eq!(load_in(&root, &made.id), None);
        drop(running);
    }

    fn sha_of(bytes: &[u8]) -> String {
        Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn write_ledger(root: &Path, made: &Instance, lines: &[(&str, &[u8])]) {
        let text: String = lines.iter().map(|(name, bytes)| format!("{} {}\n", name.to_lowercase(), sha_of(bytes))).collect();
        std::fs::write(crate::core::instance::dir_in(root, &made.id).join("mods.managed"), text).unwrap();
    }

    #[test]
    fn a_jar_the_sync_placed_is_not_adopted_and_counts_as_an_always_on_provider() {
        let (dir, root, mut made) = setup();
        save_with(&root, &mut made, &["x.jar"], &["alt.jar"], &[]);
        let api = jar_bytes(&[("fabric.mod.json", br#"{"id":"fabric-api","version":"1"}"#)]);
        let alt = jar_bytes(&[("fabric.mod.json", br#"{"id":"alt","provides":["fabric-api"]}"#)]);
        put(&root, &made, "x.jar", &jar_bytes(&[("fabric.mod.json", br#"{"id":"x","name":"X","depends":{"fabric-api":"*"}}"#)]));
        put(&root, &made, "alt.jar", &alt);
        put(&root, &made, "Fabric-API-1.jar", &api);
        write_ledger(&root, &made, &[("fabric-api-1.jar", &api[..])]);
        let (now, scan) = scan_in(&root, &dir.path().join("icons"), &made.id).unwrap();
        assert_eq!(scan.entries.iter().map(|e| e.file.as_str()).collect::<Vec<_>>(), ["x.jar", "alt.jar"]);
        assert_eq!(now.local_mods, ["alt.jar"]);
        assert_eq!(scan.managed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["fabric-api"]);
        assert!(needed_by(&scan, &names(&["alt.jar"])).is_empty());
        let mut bare = scan.clone();
        bare.managed.clear();
        assert_eq!(needed_by(&bare, &names(&["alt.jar"])), ["X"]);
        let unmanaged = scan_of(vec![entry("top.jar", "top", &["api"], &[], false), entry("impl.jar", "impl", &[], &["api"], false)], None);
        let managed = Scan { managed: vec![Meta { id: "api".into(), ..Meta::default() }], ..unmanaged.clone() };
        assert!(missing_for(&managed, &names(&["top.jar"])).is_empty());
        assert_eq!(missing_for(&unmanaged, &names(&["top.jar"])), ["impl.jar"]);
    }

    #[test]
    fn a_jar_with_a_ledger_name_but_another_hash_is_adopted_and_an_old_file_after_an_update_is_not() {
        let (dir, root, mut made) = setup();
        save_with(&root, &mut made, &["new.jar"], &[], &[]);
        put(&root, &made, "new.jar", b"new");
        put(&root, &made, "mine.jar", b"mine");
        put(&root, &made, "old.jar", b"old");
        write_ledger(&root, &made, &[("mine.jar", &b"theirs"[..]), ("old.jar", &b"old"[..])]);
        let (now, scan) = scan_in(&root, &dir.path().join("icons"), &made.id).unwrap();
        assert_eq!(scan.entries.iter().map(|e| e.file.as_str()).collect::<Vec<_>>(), ["new.jar", "mine.jar"]);
        assert_eq!(now.local_mods, ["mine.jar"]);
        assert!(scan.managed.is_empty());
        assert_eq!(mods_of(&root, &made), ["mine.jar", "new.jar", "old.jar"]);
    }

    #[test]
    fn a_hand_renamed_disabled_file_stays_off_when_other_mods_are_switched_or_trashed() {
        let (_dir, root, mut made) = setup();
        save_with(&root, &mut made, &[], &["a.jar", "b.jar", "c.jar"], &[]);
        put(&root, &made, "a.jar.disabled", b"a");
        put(&root, &made, "b.jar", b"b");
        put(&root, &made, "c.jar", b"c");
        let now = set_enabled_in(&root, &made.id, &names(&["b.jar"]), false).unwrap();
        assert_eq!(now.disabled, ["a.jar", "b.jar"]);
        assert_eq!(mods_of(&root, &made), ["a.jar.disabled", "b.jar.disabled", "c.jar"]);
        let (after, _) = trash_in(&root, &made.id, &names(&["c.jar"]), 9).unwrap();
        assert_eq!(after.disabled, ["a.jar", "b.jar"]);
        assert_eq!(mods_of(&root, &made), ["a.jar.disabled", "b.jar.disabled"]);
    }

    #[test]
    fn ending_or_dismissing_a_finished_search_waits_for_the_mods_lock() {
        let (_dir, root, mut made) = setup();
        four(&root, &mut made);
        let state = Bisect { before: Vec::new(), suspects: Vec::new(), off: Vec::new(), round: 1, rounds: 1, outcome: Some(Outcome::Nothing) };
        let dir = crate::core::instance::dir_in(&root, &made.id);
        for stop in [true, false] {
            write_state(&dir, &state).unwrap();
            let held = crate::core::lock::exclusive(&dir.join("mods.lock")).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let (root2, id) = (root.clone(), made.id.clone());
            let worker = std::thread::spawn(move || {
                let done = if stop { stop_in(&root2, &id).map(|_| ()) } else { dismiss_in(&root2, &id) };
                let _ = tx.send(done.is_ok());
            });
            assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
            assert!(read_state(&dir).is_some());
            drop(held);
            assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(true));
            worker.join().unwrap();
            assert!(read_state(&dir).is_none());
        }
    }

    fn outside(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(dir.join("picked")).unwrap();
        let path = dir.join("picked").join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn added_files_are_copied_whole_and_listed_as_own_mods() {
        let (dir, root, mut made) = setup();
        save_with(&root, &mut made, &["listed.jar"], &[], &[]);
        let one = outside(dir.path(), "One.JAR", b"one");
        let two = outside(dir.path(), "two.jar", b"two");
        let (now, skipped) = add_files_in(&root, &made.id, &[one, two]).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(now.local_mods, ["One.JAR", "two.jar"]);
        assert_eq!(saved(&root, &made), now);
        assert_eq!(mods_of(&root, &made), ["One.JAR", "two.jar"]);
        assert_eq!(std::fs::read(crate::core::instance::dir_in(&root, &made.id).join("mods").join("two.jar")).unwrap(), b"two");
        let (_, scan) = scan_in(&root, &dir.path().join("icons"), &made.id).unwrap();
        assert!(scan.entries.iter().any(|e| e.file == "two.jar" && e.source == Source::Own));
    }

    #[test]
    fn an_added_file_whose_name_is_taken_is_skipped_and_named_and_non_jars_are_ignored() {
        let (dir, root, mut made) = setup();
        save_with(&root, &mut made, &["listed.jar"], &["own.jar"], &[]);
        put(&root, &made, "a.jar", b"mine");
        put(&root, &made, "b.jar.disabled", b"mine");
        let picked: Vec<PathBuf> = [("A.jar", &b"x"[..]), ("b.JAR", b"x"), ("Listed.jar", b"x"), ("OWN.jar", b"x"), ("notes.txt", b"x"), ("c.jar.disabled", b"x"), ("fresh.jar", b"x"), ("fresh.jar", b"y")]
            .iter()
            .enumerate()
            .map(|(i, (name, bytes))| outside(&dir.path().join(i.to_string()), name, bytes))
            .chain([dir.path().join("missing.jar")])
            .collect();
        let (now, skipped) = add_files_in(&root, &made.id, &picked).unwrap();
        assert_eq!(skipped, ["A.jar", "b.JAR", "Listed.jar", "OWN.jar", "fresh.jar"]);
        assert_eq!(now.local_mods, ["own.jar", "fresh.jar"]);
        assert_eq!(mods_of(&root, &made), ["a.jar", "b.jar.disabled", "fresh.jar"]);
        assert_eq!(std::fs::read(crate::core::instance::dir_in(&root, &made.id).join("mods").join("a.jar")).unwrap(), b"mine");
        assert_eq!(std::fs::read(crate::core::instance::dir_in(&root, &made.id).join("mods").join("fresh.jar")).unwrap(), b"x");
        let (same, none) = add_files_in(&root, &made.id, &[]).unwrap();
        assert!(none.is_empty() && same == now);
    }

    #[test]
    fn adding_files_is_refused_while_a_game_runs_and_a_failure_leaves_no_copy_behind() {
        let (dir, root, mut made) = setup();
        save_with(&root, &mut made, &[], &[], &[]);
        let jar = outside(dir.path(), "a.jar", b"a");
        let running = instance::hold_in(&root, &made.id).unwrap();
        assert!(add_files_in(&root, &made.id, std::slice::from_ref(&jar)).is_err());
        assert!(mods_of(&root, &made).is_empty());
        drop(running);
        std::fs::create_dir_all(crate::core::instance::dir_in(&root, &made.id).join("mods").join("blocked.jar")).unwrap();
        let blocked = outside(dir.path(), "blocked.jar", b"b");
        assert!(add_files_in(&root, &made.id, &[jar, blocked]).is_err());
        assert_eq!(mods_of(&root, &made), ["blocked.jar"]);
        assert_eq!(saved(&root, &made), made);
    }
}
