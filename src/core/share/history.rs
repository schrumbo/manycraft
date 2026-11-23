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

    /// Records that `source` wrote the version of `rel` that is now shared.
    pub(super) fn wrote(&mut self, rel: &str, source: &Own) {
        self.index.current.insert(rel.to_owned(), Stamp { source: Source { id: source.id.clone(), name: source.name.clone() }, written: self.at });
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
        let mut total = 0u64;
        let mut counted = HashSet::new();
        let mut cut = self.index.entries.len();
        for (i, e) in self.index.entries.iter().enumerate() {
            if counted.insert(e.file.clone()) {
                total += e.size;
            }
            if total > self.cap {
                cut = i;
                break;
            }
        }
        let dropped: Vec<Entry> = self.index.entries.split_off(cut);
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
    !rel.is_empty() && !rel.contains(['\\', ':']) && rel.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
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