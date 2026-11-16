//! Snapshots taken before a change that may hurt an instance: its version, loader, mods, config, packs and every world, the newest three kept.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::i18n::{tr, trf};
use super::instance::{self, Final, Instance};
use super::worlds::{self, Progress};
use super::{lock, store, trash};

const DIR: &str = "snapshots";
const META: &str = "snapshot.json";
const ZIP: &str = "data.zip";
const COPY: &str = "data";
const LARGE_CHOICE: &str = "large.json";
const SAVES: &str = "saves";
const PARTS: [&str; 4] = ["mods", "config", "resourcepacks", "shaderpacks"];
/// Snapshots kept per instance.
pub const KEPT: usize = 3;
/// World bytes above which a snapshot is a plain copy instead of a zip, and the owner is asked once whether to take it at all.
pub const LARGE: u64 = 2 << 30;

/// Why a snapshot was taken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Before moving to this game version.
    Version(String),
    /// Before moving to this loader, like "Quilt".
    Loader(String),
    /// Before updating the loader to this, like "Fabric 0.18.1".
    LoaderUpdate(String),
    Reset,
    /// Before updating this many mods.
    ModUpdates(usize),
}

impl Reason {
    /// "Vor dem Wechsel auf Minecraft 26.4", the line a snapshot is listed under.
    pub fn label(&self) -> String {
        match self {
            Self::Version(v) => trf("Vor dem Wechsel auf Minecraft %@", &[v]),
            Self::Loader(l) => trf("Vor dem Wechsel zu %@", &[l]),
            Self::LoaderUpdate(l) => trf("Vor dem Update auf %@", &[l]),
            Self::Reset => tr("Vor dem Zurücksetzen"),
            Self::ModUpdates(1) => tr("Vor dem Update eines Mods"),
            Self::ModUpdates(n) => trf("Vor dem Update von %lld Mods", &[n]),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Meta {
    reason: Reason,
    at: i64,
    version_line: String,
    size: u64,
    copy: bool,
}

/// One finished snapshot of an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Folder name, unique within the instance.
    pub id: String,
    pub reason: Reason,
    /// Unix seconds.
    pub at: i64,
    /// "Minecraft 26.3 · Fabric", what the instance was.
    pub version_line: String,
    /// Bytes on disk.
    pub size: u64,
}

/// Folder holding the snapshots of the instance in `instance_dir`.
pub fn dir(instance_dir: &Path) -> PathBuf {
    instance_dir.join(DIR)
}

/// Bytes in the worlds of every game folder of the instance in `instance_dir`.
pub fn worlds_size(instance_dir: &Path) -> u64 {
    worlds::game_dirs(instance_dir).iter().map(|(_, game)| worlds::size(&game.join(SAVES))).sum()
}

/// Whether snapshots are wanted for an instance whose worlds are above [`LARGE`]; `None` until the owner was asked.
pub fn large_choice(instance_dir: &Path) -> Option<bool> {
    serde_json::from_slice(&std::fs::read(dir(instance_dir).join(LARGE_CHOICE)).ok()?).ok()
}

/// Remembers whether snapshots are wanted for an instance with large worlds.
pub fn set_large_choice(instance_dir: &Path, wanted: bool) -> Result<()> {
    let dir = dir(instance_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    store::write::<bool>(&dir.join(LARGE_CHOICE), &serde_json::to_vec(&wanted)?, |_| Ok(()))
}

/// The world size to ask about when the instance in `instance_dir` is large and the owner was not asked yet.
pub fn needs_choice(instance_dir: &Path) -> Option<u64> {
    if large_choice(instance_dir).is_some() {
        return None;
    }
    Some(worlds_size(instance_dir)).filter(|size| *size > LARGE)
}

/// Every finished snapshot of the instance in `instance_dir`, newest first.
pub fn list(instance_dir: &Path) -> Vec<Snapshot> {
    let Ok(entries) = std::fs::read_dir(dir(instance_dir)) else { return Vec::new() };
    let mut found: Vec<Snapshot> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let id = e.file_name().to_str().filter(|n| valid_id(n))?.to_owned();
            let meta: Meta = serde_json::from_slice(&std::fs::read(e.path().join(META)).ok()?).ok()?;
            Some(Snapshot { id, reason: meta.reason, at: meta.at, version_line: meta.version_line, size: meta.size })
        })
        .collect();
    found.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| millis_of(&b.id).cmp(&millis_of(&a.id))));
    found
}

/// Takes a snapshot of `made`, the instance in `instance_dir`, then deletes all but the newest [`KEPT`]. Returns `None` when
/// the owner turned snapshots off for its large worlds. A snapshot that fails leaves nothing behind and every older one in place.
pub fn take(instance_dir: &Path, made: &Instance, reason: Reason, progress: &Progress) -> Result<Option<Snapshot>> {
    take_at(instance_dir, made, reason, chrono::Local::now().timestamp(), instance::now_millis(), progress)
}

fn take_at(instance_dir: &Path, made: &Instance, reason: Reason, at: i64, millis: u64, progress: &Progress) -> Result<Option<Snapshot>> {
    let worlds = worlds_size(instance_dir);
    let copy = worlds > LARGE;
    if copy && large_choice(instance_dir) == Some(false) {
        return Ok(None);
    }
    let root = dir(instance_dir);
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
    sweep_parts(&root);
    let id = (millis..).find(|n| std::fs::symlink_metadata(root.join(n.to_string())).is_err() && std::fs::symlink_metadata(root.join(format!(".{n}.part"))).is_err()).context("no free snapshot name")?.to_string();
    let part = root.join(format!(".{id}.part"));
    std::fs::create_dir(&part).with_context(|| format!("cannot create {}", part.display()))?;
    let sources = sources(instance_dir);
    progress.total.store(sources.iter().map(|(_, from)| worlds::size(from)).sum(), Ordering::Relaxed);
    let written = (|| -> Result<Snapshot> {
        if copy {
            let data = part.join(COPY);
            for (rel, from) in &sources {
                copy_into(from, &data.join(rel), progress)?;
            }
        } else {
            let file = std::fs::File::create(part.join(ZIP)).with_context(|| format!("cannot create {}", part.display()))?;
            let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
            for (rel, from) in &sources {
                let name = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                if from.is_file() {
                    worlds::add_file(&mut zip, from, &name, progress)?;
                } else {
                    worlds::add_tree(&mut zip, from, &name, progress)?;
                }
            }
            zip.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        }
        let size = worlds::size(&part);
        let meta = Meta { reason: reason.clone(), at, version_line: made.version_line(), size, copy };
        let mut file = std::fs::File::create(part.join(META)).with_context(|| format!("cannot write {}", part.display()))?;
        file.write_all(&serde_json::to_vec_pretty(&meta)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part, root.join(&id)).with_context(|| format!("cannot finish {}", root.join(&id).display()))?;
        Ok(Snapshot { id: id.clone(), reason, at, version_line: meta.version_line, size })
    })();
    match written {
        Ok(snapshot) => {
            rotate(instance_dir, &snapshot.id);
            Ok(Some(snapshot))
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&part);
            Err(e)
        }
    }
}

fn sources(instance_dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = vec![(PathBuf::from(instance::FILE), instance_dir.join(instance::FILE))];
    for name in PARTS {
        let from = instance_dir.join(name);
        if std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_dir()) {
            out.push((PathBuf::from(name), from));
        }
    }
    for (_, game) in worlds::game_dirs(instance_dir) {
        let Ok(entries) = std::fs::read_dir(game.join(SAVES)) else { continue };
        for world in entries.filter_map(Result::ok).map(|e| e.path()) {
            if std::fs::symlink_metadata(&world).is_ok_and(|m| m.is_dir())
                && !worlds::is_open(&world)
                && let Ok(rel) = world.strip_prefix(instance_dir)
            {
                out.push((rel.to_path_buf(), world));
            }
        }
    }
    out
}

fn copy_into(from: &Path, to: &Path, progress: &Progress) -> Result<()> {
    let meta = std::fs::symlink_metadata(from).with_context(|| format!("cannot read {}", from.display()))?;
    if meta.is_file() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::copy(from, to).with_context(|| format!("cannot copy {}", from.display()))?;
        progress.done.fetch_add(meta.len(), Ordering::Relaxed);
        return Ok(());
    }
    worlds::copy_tree(from, to, progress)
}

fn rotate(instance_dir: &Path, newest: &str) {
    for old in list(instance_dir).into_iter().filter(|s| s.id != newest).skip(KEPT - 1) {
        let path = dir(instance_dir).join(&old.id);
        if let Err(e) = std::fs::remove_dir_all(&path) {
            eprintln!("snapshot {} stays: {e}", path.display());
        }
    }
}

fn sweep_parts(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_prefix('.')).and_then(|n| n.strip_suffix(".part")) else { continue };
        if valid_id(id) && millis_of(id).is_some_and(|m| instance::now_millis().saturating_sub(m) > 24 * 60 * 60 * 1000) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// What a restore did: the saved instance, the instance as it was before, and the trash entries holding what was there.
#[derive(Debug)]
pub struct Restored {
    pub instance: Instance,
    pub before: Instance,
    pub trashed: Vec<String>,
}

/// Takes a restore back: the version, loader and mods of `before` again, and every entry of `trashed` swapped into its place,
/// what the restore put there going to the trash. Refused while a game of the instance runs.
pub fn unrestore(before: &Instance, trashed: &[String]) -> Result<Instance> {
    unrestore_in(&instance::instances_dir(), before, trashed)
}

pub(super) fn unrestore_in(root: &Path, before: &Instance, trashed: &[String]) -> Result<Instance> {
    if !instance::valid_id(&before.id) {
        bail!("invalid instance {:?}", before.id);
    }
    let _quiet = instance::quiet_in(root, &before.id)?;
    let dir = root.join(&before.id);
    let _held = lock::exclusive(&dir.join(super::origin::MODS_LOCK))?;
    let now: Instance = store::load(&dir.join(instance::FILE))?.ok_or_else(|| anyhow::Error::new(Final(tr("Diese Instanz gibt es nicht mehr."))))?;
    if let Some(gone) = trashed.iter().find(|e| !trash::holds(&dir, e)) {
        bail!(Final(trf("„%@“ liegt nicht mehr im Papierkorb.", &[gone])));
    }
    for entry in trashed {
        trash::swap_back(&dir, entry)?;
    }
    let next = Instance { game_version: before.game_version.clone(), loader: before.loader.clone(), mods: before.mods.clone(), local_mods: before.local_mods.clone(), disabled: before.disabled.clone(), ..latest(&dir, now) };
    instance::save_in(root, &next)?;
    let _ = super::content::drop_state(&dir);
    Ok(next)
}

/// Puts snapshot `id` of instance `instance_id` back: its version, loader and mods, its config and packs, and the worlds of every
/// game folder it holds. What is there now goes to the instance's trash. Refused while a game of it runs; whatever fails,
/// the instance is left as it was.
pub fn restore(instance_id: &str, id: &str) -> Result<Restored> {
    restore_in(&instance::instances_dir(), instance_id, id, instance::now_millis())
}

pub(super) fn restore_in(root: &Path, instance_id: &str, id: &str, millis: u64) -> Result<Restored> {
    if !instance::valid_id(instance_id) || !valid_id(id) {
        bail!("invalid snapshot {instance_id:?} {id:?}");
    }
    let _quiet = instance::quiet_in(root, instance_id)?;
    let dir = root.join(instance_id);
    let _held = lock::exclusive(&dir.join(super::origin::MODS_LOCK))?;
    let now: Instance = store::load(&dir.join(instance::FILE))?.ok_or_else(|| anyhow::Error::new(Final(tr("Diese Instanz gibt es nicht mehr."))))?;
    let from = self::dir(&dir).join(id);
    let gone = || Final(tr("Diese Sicherung gibt es nicht mehr."));
    let meta: Meta = serde_json::from_slice(&std::fs::read(from.join(META)).map_err(|e| anyhow::Error::new(gone()).context(e))?).map_err(|e| anyhow::Error::new(gone()).context(e))?;
    let staging = dir.join(format!(".restore-{millis}"));
    let _ = std::fs::remove_dir_all(&staging);
    let unpacked = if meta.copy { worlds::copy_tree(&from.join(COPY), &staging, &Progress::default()) } else { unpack(&from.join(ZIP), &staging) };
    let fail = |e: anyhow::Error| {
        let _ = std::fs::remove_dir_all(&staging);
        e
    };
    unpacked.map_err(|e| fail(e.context(tr("Die Sicherung ist beschädigt."))))?;
    let then: Instance = std::fs::read(staging.join(instance::FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or_else(|| fail(anyhow::Error::new(Final(tr("Die Sicherung ist beschädigt.")))))?;
    if then.id != now.id {
        return Err(fail(anyhow::Error::new(Final(tr("Die Sicherung gehört zu einer anderen Instanz.")))));
    }
    let mut swapped: Vec<PathBuf> = PARTS.iter().map(PathBuf::from).collect();
    swapped.extend(staged_worlds(&staging));
    let mut trashed: Vec<(PathBuf, String)> = Vec::new();
    let mut placed: Vec<PathBuf> = Vec::new();
    let undo = |trashed: &[(PathBuf, String)], placed: &[PathBuf]| {
        for rel in placed {
            let _ = std::fs::rename(dir.join(rel), staging.join(rel));
        }
        for (_, entry) in trashed {
            let _ = trash::take_back(&dir, entry);
        }
    };
    let moved = (|| -> Result<()> {
        for rel in &swapped {
            if std::fs::symlink_metadata(dir.join(rel)).is_ok() {
                trashed.push((rel.clone(), trash::put(&dir, &dir.join(rel))?));
            }
        }
        for rel in &swapped {
            let staged = staging.join(rel);
            if std::fs::symlink_metadata(&staged).is_ok() {
                let to = dir.join(rel);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
                }
                std::fs::rename(&staged, &to).with_context(|| format!("cannot move {} into place", to.display()))?;
                placed.push(rel.clone());
            }
        }
        Ok(())
    })();
    if let Err(e) = moved {
        undo(&trashed, &placed);
        return Err(fail(e));
    }
    let before = now.clone();
    let next = Instance { game_version: then.game_version, loader: then.loader, mods: then.mods, local_mods: then.local_mods, disabled: then.disabled, ..latest(&dir, now) };
    if let Err(e) = instance::save_in(root, &next) {
        undo(&trashed, &placed);
        return Err(fail(e));
    }
    let _ = std::fs::remove_dir_all(&staging);
    let _ = super::content::drop_state(&dir);
    Ok(Restored { instance: next, before, trashed: trashed.into_iter().map(|(_, entry)| entry).collect() })
}

fn latest(dir: &Path, read: Instance) -> Instance {
    match store::load::<Instance>(&dir.join(instance::FILE)) {
        Ok(Some(now)) if now.id == read.id => now,
        _ => read,
    }
}

fn staged_worlds(staging: &Path) -> Vec<PathBuf> {
    let Ok(games) = std::fs::read_dir(instance::games_dir(staging)) else { return Vec::new() };
    games
        .filter_map(Result::ok)
        .filter_map(|game| std::fs::read_dir(game.path().join(SAVES)).ok())
        .flat_map(|worlds| worlds.filter_map(Result::ok).map(|w| w.path()))
        .filter(|w| w.is_dir())
        .filter_map(|w| w.strip_prefix(staging).ok().map(Path::to_path_buf))
        .collect()
}

fn unpack(zip_path: &Path, into: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path).with_context(|| format!("cannot read {}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    std::fs::create_dir_all(into).with_context(|| format!("cannot create {}", into.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let Some(name) = entry.enclosed_name() else { bail!("unsafe path in {}", zip_path.display()) };
        if !name.components().all(|c| matches!(c, Component::Normal(_))) {
            bail!("unsafe path in {}", zip_path.display());
        }
        let out = into.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&out).with_context(|| format!("cannot write {}", out.display()))?;
        let mut buf = vec![0; 64 << 10];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}

fn millis_of(id: &str) -> Option<u64> {
    id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instance::{Loader, save_in};

    fn setup(root: &Path) -> (Instance, PathBuf, PathBuf) {
        let made = Instance::new("Farm", "26.3", Loader::Fabric(None));
        save_in(root, &made).unwrap();
        let dir = root.join(&made.id);
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::write(dir.join("mods").join("a.jar"), b"a1").unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("a.toml"), b"old").unwrap();
        let world = instance::games_dir(&dir).join(uuid::Uuid::from_u128(1).simple().to_string()).join(SAVES).join("Welt");
        std::fs::create_dir_all(world.join("region")).unwrap();
        std::fs::write(world.join("level.dat"), b"level").unwrap();
        std::fs::write(world.join("region").join("r.0.0.mca"), b"chunks").unwrap();
        (made, dir, world)
    }

    #[test]
    fn a_restore_brings_back_version_mods_config_and_worlds_and_trashes_the_present() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, world) = setup(root.path());
        let snap = take_at(&dir, &made, Reason::Version("26.4".into()), 10, 10, &Progress::default()).unwrap().unwrap();
        assert_eq!(snap.reason.label(), "Vor dem Wechsel auf Minecraft 26.4");
        let moved = Instance { game_version: "26.4".into(), name: "Farm neu".into(), ..made.clone() };
        save_in(root.path(), &moved).unwrap();
        std::fs::write(dir.join("mods").join("a.jar"), b"a2").unwrap();
        std::fs::write(dir.join("config").join("a.toml"), b"new").unwrap();
        std::fs::write(world.join("region").join("r.0.0.mca"), b"upgraded").unwrap();
        let later = world.with_file_name("Neu");
        std::fs::create_dir_all(&later).unwrap();
        std::fs::write(later.join("level.dat"), b"new world").unwrap();
        let done = restore_in(root.path(), &made.id, &snap.id, 20).unwrap();
        assert_eq!(std::fs::read(later.join("level.dat")).unwrap(), b"new world", "a world made after the snapshot stays where it is");
        assert_eq!(done.instance.game_version, "26.3");
        assert_eq!(done.instance.name, "Farm neu");
        assert_eq!(std::fs::read(dir.join("mods").join("a.jar")).unwrap(), b"a1");
        assert_eq!(std::fs::read(dir.join("config").join("a.toml")).unwrap(), b"old");
        assert_eq!(std::fs::read(world.join("region").join("r.0.0.mca")).unwrap(), b"chunks");
        assert!(!done.trashed.is_empty() && done.trashed.iter().all(|e| trash::holds(&dir, e)));
        assert!(!dir.join(".restore-20").exists());
        assert_eq!(list(&dir).len(), 1);
        let back = unrestore_in(root.path(), &done.before, &done.trashed).unwrap();
        assert_eq!(back.game_version, "26.4");
        assert_eq!(std::fs::read(dir.join("mods").join("a.jar")).unwrap(), b"a2");
        assert_eq!(std::fs::read(dir.join("config").join("a.toml")).unwrap(), b"new");
        assert_eq!(std::fs::read(world.join("region").join("r.0.0.mca")).unwrap(), b"upgraded");
        assert_eq!(std::fs::read(later.join("level.dat")).unwrap(), b"new world");
        assert!(unrestore_in(root.path(), &done.before, &done.trashed).is_err(), "a second undo finds nothing to put back");
    }

    #[test]
    fn a_restore_keeps_what_was_saved_to_the_instance_while_it_ran() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, _) = setup(root.path());
        let chosen = Instance { default_account: Some(uuid::Uuid::from_u128(7)), name: "Farm neu".into(), ..made.clone() };
        save_in(root.path(), &chosen).unwrap();
        let kept = latest(&dir, made.clone());
        assert_eq!((kept.default_account, kept.name.as_str()), (chosen.default_account, "Farm neu"));
        std::fs::remove_file(dir.join(instance::FILE)).unwrap();
        assert_eq!(latest(&dir, made.clone()), made, "without a readable file the copy read at the start stands");
    }

    #[test]
    fn a_world_open_in_a_game_is_left_out_and_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, world) = setup(root.path());
        let lock = std::fs::File::create(world.join("session.lock")).unwrap();
        lock.lock().unwrap();
        let snap = take_at(&dir, &made, Reason::ModUpdates(1), 10, 10, &Progress::default()).unwrap().unwrap();
        std::fs::write(world.join("region").join("r.0.0.mca"), b"played on").unwrap();
        drop(lock);
        restore_in(root.path(), &made.id, &snap.id, 20).unwrap();
        assert_eq!(std::fs::read(world.join("region").join("r.0.0.mca")).unwrap(), b"played on");
    }

    #[test]
    fn only_the_newest_three_stay() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, _) = setup(root.path());
        for n in 1..=5 {
            take_at(&dir, &made, Reason::ModUpdates(n), n as i64, n as u64, &Progress::default()).unwrap();
        }
        let kept: Vec<String> = list(&dir).into_iter().map(|s| s.id).collect();
        assert_eq!(kept, ["5", "4", "3"]);
    }

    #[test]
    fn a_failed_snapshot_leaves_nothing_and_keeps_every_older_one() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, _) = setup(root.path());
        for n in 1..=3 {
            take_at(&dir, &made, Reason::Reset, n, n as u64, &Progress::default()).unwrap();
        }
        std::fs::remove_file(dir.join(instance::FILE)).unwrap();
        std::fs::remove_file(store::backup_path(&dir.join(instance::FILE))).ok();
        assert!(take_at(&dir, &made, Reason::Reset, 4, 4, &Progress::default()).is_err());
        let kept: Vec<String> = list(&dir).into_iter().map(|s| s.id).collect();
        assert_eq!(kept, ["3", "2", "1"]);
        assert_eq!(std::fs::read_dir(self::dir(&dir)).unwrap().count(), 3);
    }

    #[test]
    fn a_damaged_snapshot_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, world) = setup(root.path());
        let snap = take_at(&dir, &made, Reason::Reset, 1, 1, &Progress::default()).unwrap().unwrap();
        std::fs::write(self::dir(&dir).join(&snap.id).join(ZIP), b"not a zip").unwrap();
        std::fs::write(world.join("region").join("r.0.0.mca"), b"now").unwrap();
        assert!(restore_in(root.path(), &made.id, &snap.id, 2).is_err());
        assert_eq!(std::fs::read(world.join("region").join("r.0.0.mca")).unwrap(), b"now");
        assert!(!dir.join(".restore-2").exists());
        assert!(!dir.join(".trash").exists() || std::fs::read_dir(dir.join(".trash")).unwrap().count() == 0);
    }

    #[test]
    fn large_worlds_are_skipped_once_turned_off() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, _) = setup(root.path());
        assert_eq!(needs_choice(&dir), None);
        set_large_choice(&dir, false).unwrap();
        assert_eq!(large_choice(&dir), Some(false));
        assert!(take_at(&dir, &made, Reason::Reset, 1, 1, &Progress::default()).unwrap().is_some());
        assert_eq!(list(&dir).len(), 1);
    }

    #[test]
    fn a_restore_is_refused_while_a_game_runs() {
        let root = tempfile::tempdir().unwrap();
        let (made, dir, _) = setup(root.path());
        let snap = take_at(&dir, &made, Reason::Reset, 1, 1, &Progress::default()).unwrap().unwrap();
        let _game = instance::hold_in(root.path(), &made.id).unwrap();
        assert!(restore_in(root.path(), &made.id, &snap.id, 2).is_err());
        assert_eq!(std::fs::read(dir.join("mods").join("a.jar")).unwrap(), b"a1");
    }
}
