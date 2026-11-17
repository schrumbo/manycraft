//! The Papierkorb across every instance: deleted instances, what a reset replaced, deleted mods, and worlds, screenshots
//! and folders put away, each kept for [`KEPT`] and then deleted at the next start.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};

use super::content::{TRASHED, Trashed};
use super::instance::{self, Instance};
use super::origin::{KEEP, MODS_LOCK, RESET_META, ResetMeta};
use super::{batch, lock, store, worlds};

/// How long anything stays in the Papierkorb.
pub const KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const TRASH: &str = ".trash";
const ORIGIN: &str = "origin";
const SUFFIX: &str = ".reset";

/// What a Papierkorb entry holds.
#[derive(Clone, Debug)]
pub enum What {
    /// A deleted instance.
    Instance,
    /// What a reset replaced, with the instance before and right after it when they were recorded.
    Reset(Option<Box<ResetMeta>>),
    /// Deleted mods, with what puts them back when it was recorded.
    Mods(Option<Trashed>),
    /// A world, screenshot or folder put away from `origin`, a `/` path inside the instance; `taken` when something is there now.
    File { origin: String, taken: bool },
    /// A deleted batch, named by `name`; its instance is `instance`.
    Batch,
}

/// One Papierkorb entry.
#[derive(Clone, Debug)]
pub struct Item {
    pub what: What,
    /// The instance it belongs to, by id; for a deleted instance its own.
    pub instance: String,
    /// That instance's name, empty when it could not be read.
    pub name: String,
    /// The entry's folder name.
    pub entry: String,
    /// Unix milliseconds when it was put there.
    pub at: u64,
    /// Bytes on disk.
    pub size: u64,
}

/// Everything in the Papierkorb, newest first; an error when the instances or batches folder cannot be read.
pub fn list() -> Result<Vec<Item>> {
    let mut out = list_in(&instance::instances_dir())?;
    out.extend(batch::trashed()?.into_iter().map(|t| Item { what: What::Batch, instance: t.batch.instance_id, name: t.batch.name, entry: t.entry, at: t.at, size: t.size }));
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.entry.cmp(&b.entry)));
    Ok(out)
}

/// Deletes `item` for good.
pub fn delete(item: &Item) -> Result<()> {
    match item.what {
        What::Batch => batch::purge(&item.entry),
        _ => delete_in(&instance::instances_dir(), item),
    }
}

fn read(dir: &Path) -> Result<Option<std::fs::ReadDir>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(Some(entries)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("cannot read {}", dir.display()))),
    }
}

pub(super) fn list_in(root: &Path) -> Result<Vec<Item>> {
    let mut out = Vec::new();
    let names = |id: &str| name_of(&instance::dir_in(root, id).join(instance::FILE));
    if let Some(entries) = read(&root.join(TRASH))? {
        for e in entries.filter_map(Result::ok) {
            let Some(entry) = e.file_name().to_str().map(str::to_owned) else { continue };
            let path = e.path();
            if path.join(KEEP).exists() {
                continue;
            }
            if let Some(stem) = entry.strip_suffix(SUFFIX)
                && let Some((id, at)) = instance::parse_entry(stem)
            {
                let meta = std::fs::read(path.join(RESET_META)).ok().and_then(|b| serde_json::from_slice::<ResetMeta>(&b).ok()).map(Box::new);
                out.push(Item { what: What::Reset(meta), instance: id.to_owned(), name: names(id), entry: entry.clone(), at, size: worlds::size(&path) });
            } else if let Some((id, at)) = instance::parse_entry(&entry) {
                out.push(Item { what: What::Instance, instance: id.to_owned(), name: name_of(&path.join(instance::FILE)), entry: entry.clone(), at, size: worlds::size(&path) });
            }
        }
    }
    if let Some(folders) = read(root)? {
        for folder in folders.filter_map(Result::ok) {
            if folder.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(folder.path().join(TRASH)) else { continue };
            let Some(made) = store::load::<Instance>(&folder.path().join(instance::FILE)).ok().flatten() else { continue };
            if instance::dir_in(root, &made.id) != folder.path() {
                continue;
            }
            let (id, name) = (made.id, made.name);
            for e in entries.filter_map(Result::ok) {
                let Some(entry) = e.file_name().to_str().map(str::to_owned) else { continue };
                let path = e.path();
                let what = if digits(&entry) {
                    What::Mods(std::fs::read(path.join(TRASHED)).ok().and_then(|b| serde_json::from_slice::<Trashed>(&b).ok()).map(|mut t| {
                        t.entry = entry.clone();
                        t
                    }))
                } else if let Some((a, b)) = entry.split_once('-')
                    && digits(a)
                    && digits(b)
                    && let Ok(origin) = std::fs::read_to_string(path.join(ORIGIN))
                {
                    let taken = origin.split('/').all(|part| !part.is_empty() && part != "." && part != "..") && std::fs::symlink_metadata(folder.path().join(&origin)).is_ok();
                    What::File { origin, taken }
                } else {
                    continue;
                };
                let Some(at) = entry.split('-').next().and_then(|m| m.parse().ok()) else { continue };
                out.push(Item { what, instance: id.clone(), name: name.clone(), entry, at, size: worlds::size(&path) });
            }
        }
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.entry.cmp(&b.entry)));
    Ok(out)
}

pub(super) fn delete_in(root: &Path, item: &Item) -> Result<()> {
    let path = path_in(root, item)?;
    let dir = instance::dir_in(root, &item.instance);
    let _held = if matches!(item.what, What::Instance) || !dir.is_dir() { None } else { Some(lock::exclusive(&dir.join(MODS_LOCK))?) };
    if path.join(KEEP).exists() {
        bail!("{} is kept", path.display());
    }
    instance::remove_dir(&path)
}

/// Deletes everything in the Papierkorb; returns how many entries went and the first failure, if any.
pub fn empty() -> (usize, Result<()>) {
    let mut first = Ok(());
    let mut gone = 0;
    let items = match list() {
        Ok(items) => items,
        Err(e) => return (0, Err(e)),
    };
    for item in items {
        match delete(&item) {
            Ok(()) => gone += 1,
            Err(e) if first.is_ok() => first = Err(e),
            Err(_) => {}
        }
    }
    (gone, first)
}

fn path_in(root: &Path, item: &Item) -> Result<PathBuf> {
    let valid = match &item.what {
        What::Instance => instance::parse_entry(&item.entry).is_some_and(|(id, _)| id == item.instance),
        What::Reset(_) => item.entry.strip_suffix(SUFFIX).and_then(instance::parse_entry).is_some_and(|(id, _)| id == item.instance),
        What::Mods(_) => digits(&item.entry),
        What::File { .. } => item.entry.split_once('-').is_some_and(|(a, b)| digits(a) && digits(b)),
        What::Batch => false,
    };
    if !valid || !instance::valid_id(&item.instance) {
        bail!("invalid trash entry {:?}", item.entry);
    }
    Ok(match item.what {
        What::Instance | What::Reset(_) => root.join(TRASH).join(&item.entry),
        What::Mods(_) | What::File { .. } => instance::existing_in(root, &item.instance)?.join(TRASH).join(&item.entry),
        What::Batch => root.join(&item.instance).join(TRASH).join(&item.entry),
    })
}

fn name_of(file: &Path) -> String {
    store::load::<Instance>(file).ok().flatten().map(|i| i.name).unwrap_or_default()
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instance::{Loader, save_in};

    #[test]
    fn every_kind_is_listed_newest_first_with_its_size_and_deleted_one_by_one() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let made = Instance::new("Farm", "26.3", Loader::Fabric(None));
        save_in(root, &made).unwrap();
        let gone = Instance::new("Alt", "26.3", Loader::Vanilla);
        save_in(root, &gone).unwrap();
        let entry = instance::trash_in(root, &gone, 1_000).unwrap();
        let dir = crate::core::instance::dir_in(root, &made.id);
        std::fs::create_dir_all(dir.join(TRASH).join("2000")).unwrap();
        std::fs::write(dir.join(TRASH).join("2000").join("a.jar"), b"jar").unwrap();
        std::fs::create_dir_all(dir.join(TRASH).join("3000-0").join("item")).unwrap();
        std::fs::write(dir.join(TRASH).join("3000-0").join(ORIGIN), "games/a/saves/Welt").unwrap();
        std::fs::write(dir.join(TRASH).join("3000-0").join("item").join("level.dat"), b"level").unwrap();
        std::fs::create_dir_all(root.join(TRASH).join(format!("{}-4000{SUFFIX}", made.id)).join("mods")).unwrap();
        std::fs::create_dir_all(root.join(TRASH).join(format!("{}-5000{SUFFIX}", made.id)).join("mods")).unwrap();
        std::fs::write(root.join(TRASH).join(format!("{}-5000{SUFFIX}", made.id)).join(KEEP), b"").unwrap();
        std::fs::create_dir_all(dir.join(TRASH).join("not-an-entry")).unwrap();
        let items = list_in(root).unwrap();
        let ats: Vec<u64> = items.iter().map(|i| i.at).collect();
        assert_eq!(ats, [4000, 3000, 2000, 1000]);
        assert!(matches!(items[0].what, What::Reset(None)) && items[0].name == "Farm");
        assert!(matches!(&items[1].what, What::File { origin, taken: false } if origin == "games/a/saves/Welt"));
        assert!(matches!(items[2].what, What::Mods(None)) && items[2].size == 3);
        assert!(matches!(items[3].what, What::Instance) && items[3].name == "Alt" && items[3].entry == entry);
        std::fs::create_dir_all(dir.join("games/a/saves/Welt")).unwrap();
        assert!(list_in(root).unwrap().iter().any(|i| matches!(&i.what, What::File { taken: true, .. })));
        delete_in(root, &items[1]).unwrap();
        assert!(!dir.join(TRASH).join("3000-0").exists());
        assert_eq!(list_in(root).unwrap().len(), 3);
    }

    #[test]
    fn deleting_refuses_a_foreign_path() {
        let root = tempfile::tempdir().unwrap();
        let item = Item { what: What::File { origin: String::new(), taken: false }, instance: "..".into(), name: String::new(), entry: "1-0".into(), at: 1, size: 0 };
        assert!(delete_in(root.path(), &item).is_err());
        let item = Item { what: What::Mods(None), instance: "abc".into(), name: String::new(), entry: "..".into(), at: 1, size: 0 };
        assert!(delete_in(root.path(), &item).is_err());
    }
}
