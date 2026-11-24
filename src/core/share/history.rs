//! Every version the sync folder replaced, and each instance's own copy from before it joined, under `sync/history/`:
//! the newest 50 of each file within 200 MB, the oldest go first.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Item, Mother, Own};
use crate::core::net;

const DIR: &str = "history";
const INDEX: &str = "index.json";
const INSTANCES: &str = "instances";
/// Versions kept per file.
pub(super) const KEEP: usize = 50;
/// Bytes the history may take.
pub(super) const CAP: u64 = 200 * 1024 * 1024;

/// Who wrote a version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Source {
    id: String,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    at: i64,
    rel: String,
    sha: String,
    size: u64,
    file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    written: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    source: Source,
    written: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    entries: Vec<Entry>,
    #[serde(default)]
    current: BTreeMap<String, Stamp>,
    #[serde(default)]
    bases: BTreeMap<String, Stamp>,
}

/// An earlier state of one shared item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub item: Item,
    /// When it was kept, in milliseconds since 1970 (UTC).
    pub at: i64,
    /// When it became the shared state, when known.
    pub written: Option<i64>,
    /// The name of the instance that wrote it, or whose own copy it is.
    pub source: Option<String>,
    /// Whether it is an instance's own copy from before that instance shared the item.
    pub own: bool,
    /// How many files it holds.
    pub files: usize,
    instance: Option<String>,
}

/// The history open for one write: what it keeps is recorded on [`History::save`].
pub(super) struct History {
    root: PathBuf,
    index: Index,
    at: i64,
    pub(super) keep: usize,
    pub(super) cap: u64,
}

impl History {
    pub(super) fn open(mother: &Mother) -> Result<Self> {
        let root = mother.root.join(DIR);
        let index = read_index(&root)?;
        let last = index.entries.iter().map(|e| e.at).max().unwrap_or(0);
        let at = chrono::Utc::now().timestamp_millis().max(last + 1);
        Ok(Self { root, index, at, keep: KEEP, cap: CAP })
    }

    /// Keeps `bytes`, the version of `rel` about to be replaced.
    pub(super) fn keep(&mut self, rel: &str, bytes: &[u8]) -> Result<()> {
        let stamp = self.index.current.get(rel).cloned();
        let folder = stamp_name(self.at);
        self.store(None, &folder, rel, bytes, stamp.as_ref().map(|s| s.source.clone()), stamp.map(|s| s.written))
    }

    /// Keeps `bytes`, the current version of `rel`, unless the history kept that version of it last.
    pub(super) fn snapshot(&mut self, rel: &str, bytes: &[u8]) -> Result<()> {
        let sha = hash(bytes);
        let last = self.index.entries.iter().filter(|e| e.instance.is_none() && e.rel == rel).max_by_key(|e| e.at);
        if last.is_some_and(|e| e.sha == sha) {
            return Ok(());
        }
        self.keep(rel, bytes)
    }

    /// Records that `source` wrote the version of `rel` that is now shared.
    pub(super) fn wrote(&mut self, rel: &str, source: &Own) {
        self.index.current.insert(rel.to_owned(), Stamp { source: Source { id: source.id.clone(), name: source.name.clone() }, written: self.at });
    }

    /// The moment this history was opened, which names every version it keeps.
    pub(super) fn stamp(&self) -> i64 {
        self.at
    }

    /// Records that the shared state of `item` comes from `source`; with `only_first` an earlier base stays.
    pub(super) fn based(&mut self, item: Item, source: &Own, only_first: bool) {
        let key = item.key().to_owned();
        if only_first && self.index.bases.contains_key(&key) {
            return;
        }
        self.index.bases.insert(key, Stamp { source: Source { id: source.id.clone(), name: source.name.clone() }, written: self.at });
    }

    /// Keeps `bytes`, the own copy of `rel` of `own`, unless it is the copy kept last.
    pub(super) fn backup(&mut self, own: &Own, rel: &str, bytes: &[u8]) -> Result<()> {
        let sha = hash(bytes);
        let last = self.index.entries.iter().filter(|e| e.instance.as_deref() == Some(own.id.as_str()) && e.rel == rel).max_by_key(|e| e.at);
        if last.is_some_and(|e| e.sha == sha) {
            return Ok(());
        }
        let folder = format!("{INSTANCES}/{}/{}", own.id, stamp_name(self.at));
        self.store(Some(own.id.clone()), &folder, rel, bytes, Some(Source { id: own.id.clone(), name: own.name.clone() }), None)
    }

    fn store(&mut self, instance: Option<String>, folder: &str, rel: &str, bytes: &[u8], source: Option<Source>, written: Option<i64>) -> Result<()> {
        let sha = hash(bytes);
        let file = match self.index.entries.iter().find(|e| e.sha == sha && self.path(&e.file).is_file()) {
            Some(same) => same.file.clone(),
            None => {
                let file = format!("{folder}/{rel}");
                net::write_atomic(&self.path(&file), bytes)?;
                file
            }
        };
        self.index.entries.push(Entry { at: self.at, rel: rel.to_owned(), sha, size: bytes.len() as u64, file, source, written, instance });
        Ok(())
    }

    fn path(&self, file: &str) -> PathBuf {
        file.split('/').fold(self.root.clone(), |p, part| p.join(part))
    }

    /// Drops the oldest versions beyond the limits and writes the index.
    pub(super) fn save(mut self) -> Result<()> {
        self.index.entries.sort_by(|a, b| b.at.cmp(&a.at));
        let mut seen: BTreeMap<(Option<String>, String), usize> = BTreeMap::new();
        self.index.entries.retain(|e| {
            let n = seen.entry((e.instance.clone(), e.rel.clone())).or_default();
            *n += 1;
            *n <= self.keep
        });
        let mut uses: BTreeMap<String, usize> = BTreeMap::new();
        let mut total = 0u64;
        for e in &self.index.entries {
            let n = uses.entry(e.file.clone()).or_default();
            if *n == 0 {
                total += e.size;
            }
            *n += 1;
        }
        let mut order: Vec<usize> = (0..self.index.entries.len()).filter(|&i| self.index.entries[i].at != self.at).collect();
        order.sort_by_key(|&i| (self.index.entries[i].instance.is_some(), self.index.entries[i].at));
        let mut out = HashSet::new();
        for i in order {
            if total <= self.cap {
                break;
            }
            let e = &self.index.entries[i];
            out.insert(i);
            if let Some(n) = uses.get_mut(&e.file) {
                *n -= 1;
                if *n == 0 {
                    total = total.saturating_sub(e.size);
                }
            }
        }
        let (dropped, left): (Vec<(usize, Entry)>, Vec<(usize, Entry)>) = std::mem::take(&mut self.index.entries).into_iter().enumerate().partition(|(i, _)| out.contains(i));
        self.index.entries = left.into_iter().map(|(_, e)| e).collect();
        let dropped: Vec<Entry> = dropped.into_iter().map(|(_, e)| e).collect();
        let kept: HashSet<&str> = self.index.entries.iter().map(|e| e.file.as_str()).collect();
        let mut gone: Vec<PathBuf> = Vec::new();
        for e in &dropped {
            if !kept.contains(e.file.as_str()) {
                gone.push(self.path(&e.file));
            }
        }
        net::write_atomic(&self.root.join(INDEX), &serde_json::to_vec_pretty(&self.index)?)?;
        for path in gone {
            let _ = fs::remove_file(&path);
            let mut dir = path.parent();
            while let Some(d) = dir
                && d.starts_with(&self.root)
                && d != self.root
                && fs::remove_dir(d).is_ok()
            {
                dir = d.parent();
            }
        }
        Ok(())
    }
}

/// The index under `root`, without entries that point outside it. An index that cannot be read moves aside with its
/// folder, which keeps every file for a person to look at, and the history starts empty.
fn read_index(root: &Path) -> Result<Index> {
    let path = root.join(INDEX);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Index::default()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    match serde_json::from_slice::<Index>(&bytes) {
        Ok(mut index) => {
            index.entries.retain(|e| inside(&e.rel) && inside(&e.file));
            index.current.retain(|rel, _| inside(rel));
            Ok(index)
        }
        Err(e) => {
            let aside = root.with_file_name(format!("{DIR}.broken-{}", chrono::Utc::now().timestamp_millis()));
            fs::rename(root, &aside).with_context(|| format!("cannot read {}: {e}", path.display()))?;
            eprintln!("the history index could not be read ({e}); it moved to {}", aside.display());
            Ok(Index::default())
        }
    }
}

/// Whether `rel`, a path with `/` between parts, stays below the folder it is relative to.
fn inside(rel: &str) -> bool {
    !rel.is_empty() && rel.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !(cfg!(windows) && part.contains(['\\', ':'])))
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn stamp_name(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at).map_or_else(|| at.to_string(), |t| t.format("%Y-%m-%dT%H-%M-%S-%3fZ").to_string())
}

pub(super) fn versions(mother: &Mother, item: Item) -> Result<Vec<Version>> {
    let index = read_index(&mother.root.join(DIR))?;
    let mut grouped: BTreeMap<(i64, Option<String>), Version> = BTreeMap::new();
    for e in index.entries.iter().filter(|e| item.holds(&e.rel)) {
        let v = grouped.entry((e.at, e.instance.clone())).or_insert_with(|| Version { item, at: e.at, written: e.written, source: e.source.as_ref().map(|s| s.name.clone()), own: e.instance.is_some(), files: 0, instance: e.instance.clone() });
        v.files += 1;
    }
    let mut out: Vec<Version> = grouped.into_values().collect();
    out.sort_by(|a, b| b.at.cmp(&a.at));
    Ok(out)
}

/// The files of `version`, by path relative to the sync folder.
pub(super) fn files(mother: &Mother, version: &Version) -> Result<Vec<(String, Vec<u8>)>> {
    let root = mother.root.join(DIR);
    let index = read_index(&root)?;
    let mut out = Vec::new();
    for e in index.entries.iter().filter(|e| e.at == version.at && e.instance == version.instance && version.item.holds(&e.rel)) {
        let path = e.file.split('/').fold(root.clone(), |p, part| p.join(part));
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        out.push((e.rel.clone(), bytes));
    }
    out.sort();
    Ok(out)
}
/// Where the shared state of an item stands: the instance it was taken from and who wrote it last.
pub(super) fn origin(mother: &Mother, item: Item) -> Result<(Option<(String, i64)>, Option<(String, i64)>)> {
    let index = read_index(&mother.root.join(DIR))?;
    let base = index.bases.get(item.key()).map(|s| (s.source.name.clone(), s.written));
    let last = index.current.iter().filter(|(rel, _)| item.holds(rel)).map(|(_, s)| s).max_by_key(|s| s.written).map(|s| (s.source.name.clone(), s.written));
    Ok((base, last))
}

/// The newest own copy of each file of `item` that the instance `id` kept after `since`, by path relative to the instance folder.
pub(super) fn own_latest(mother: &Mother, id: &str, item: Item, since: i64) -> Result<Vec<(String, Vec<u8>)>> {
    let root = mother.root.join(DIR);
    let index = read_index(&root)?;
    let mut newest: BTreeMap<&str, &Entry> = BTreeMap::new();
    for e in index.entries.iter().filter(|e| e.instance.as_deref() == Some(id) && e.at > since && item.holds(&e.rel)) {
        if newest.get(e.rel.as_str()).is_none_or(|n| n.at < e.at) {
            newest.insert(e.rel.as_str(), e);
        }
    }
    let mut out = Vec::new();
    for (rel, e) in newest {
        let path = e.file.split('/').fold(root.clone(), |p, part| p.join(part));
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        out.push((rel.to_owned(), bytes));
    }
    Ok(out)
}

/// The versions the history kept in the open at `at`, which are the shared states one write replaced.
pub(super) fn replaced_at(mother: &Mother, item: Item, at: i64) -> Result<Vec<Version>> {
    Ok(versions(mother, item)?.into_iter().filter(|v| v.at == at && !v.own).collect())
}
