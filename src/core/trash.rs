//! The trash of one instance folder: worlds, screenshots and folders deleted or replaced there, kept for [`super::bin::KEPT`].

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::i18n::tr;
use super::instance::Final;

const TRASH: &str = ".trash";
const ORIGIN: &str = "origin";
const ITEM: &str = "item";

/// Moves `path`, a file or folder inside `instance_dir`, into its own entry of the instance's trash and returns the
/// entry name [`take_back`] takes.
pub fn put(instance_dir: &Path, path: &Path) -> Result<String> {
    put_at(instance_dir, path, now_millis())
}

fn put_at(instance_dir: &Path, path: &Path, millis: u64) -> Result<String> {
    let rel = inside(instance_dir, path)?;
    if std::fs::symlink_metadata(path).is_err() {
        bail!(Final(tr("Das gibt es nicht mehr.")));
    }
    let trash = instance_dir.join(TRASH);
    std::fs::create_dir_all(&trash).with_context(|| format!("cannot create {}", trash.display()))?;
    let mut free = None;
    for n in 0..1000 {
        let entry = format!("{millis}-{n}");
        let dir = trash.join(&entry);
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                free = Some((entry, dir));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("cannot create {}", dir.display())),
        }
    }
    let (entry, dir) = free.context("no free trash entry")?;
    let origin = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    let moved = std::fs::write(dir.join(ORIGIN), origin)
        .with_context(|| format!("cannot write {}", dir.display()))
        .and_then(|()| std::fs::rename(path, dir.join(ITEM)).with_context(|| format!("cannot move {} to the trash", path.display())));
    if let Err(e) = moved {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(entry)
}

/// Whether the trash entry `entry` still holds what was put there.
pub fn holds(instance_dir: &Path, entry: &str) -> bool {
    valid_entry(entry) && std::fs::symlink_metadata(instance_dir.join(TRASH).join(entry).join(ITEM)).is_ok()
}

/// Puts the trash entry `entry` back where it was and returns that path. Refused for good ([`Final`]) when something
/// is there again or the entry is gone.
pub fn take_back(instance_dir: &Path, entry: &str) -> Result<PathBuf> {
    if !valid_entry(entry) {
        bail!("invalid trash entry {entry:?}");
    }
    let dir = instance_dir.join(TRASH).join(entry);
    let gone = || Final(tr("Das liegt nicht mehr im Papierkorb."));
    let origin = std::fs::read_to_string(dir.join(ORIGIN)).map_err(|e| anyhow::Error::new(gone()).context(e))?;
    let rel = PathBuf::from_iter(origin.split('/'));
    let to = instance_dir.join(&rel);
    inside(instance_dir, &to)?;
    if std::fs::symlink_metadata(dir.join(ITEM)).is_err() {
        bail!(gone());
    }
    if std::fs::symlink_metadata(&to).is_ok() {
        bail!(Final(tr("Dort liegt inzwischen etwas mit demselben Namen.")));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::rename(dir.join(ITEM), &to).with_context(|| format!("cannot move {} back", to.display()))?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(to)
}

/// Deletes trash entries older than `older_than` or stamped more than a day ahead, in every instance folder under `root`.
pub fn purge_stale(root: &Path, older_than: Duration) -> Result<()> {
    purge_stale_at(root, now_millis(), older_than)
}

fn purge_stale_at(root: &Path, now: u64, older_than: Duration) -> Result<()> {
    let instances = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let cutoff = now.saturating_sub(u64::try_from(older_than.as_millis()).unwrap_or(u64::MAX));
    let ahead = now.saturating_add(24 * 60 * 60 * 1000);
    let mut first = Ok(());
    for instance in instances.filter_map(Result::ok) {
        let Ok(entries) = std::fs::read_dir(instance.path().join(TRASH)) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let Some(millis) = name.to_str().filter(|n| valid_entry(n)).and_then(|n| n.split('-').next()).and_then(|m| m.parse::<u64>().ok()) else { continue };
            if (millis < cutoff || millis > ahead)
                && let Err(e) = std::fs::remove_dir_all(entry.path())
                && first.is_ok()
            {
                first = Err(anyhow::Error::new(e).context(format!("cannot delete {}", entry.path().display())));
            }
        }
    }
    first
}

fn inside(instance_dir: &Path, path: &Path) -> Result<PathBuf> {
    let rel = path.strip_prefix(instance_dir).with_context(|| format!("{} is outside the instance", path.display()))?;
    if rel.as_os_str().is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) || rel.starts_with(TRASH) {
        bail!("{} is not a path inside the instance", path.display());
    }
    Ok(rel.to_path_buf())
}

fn valid_entry(entry: &str) -> bool {
    let mut parts = entry.split('-');
    let digits = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    digits(parts.next()) && digits(parts.next()) && parts.next().is_none()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trashed_folder_comes_back_with_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let world = dir.path().join("games").join("a").join("saves").join("Welt");
        std::fs::create_dir_all(world.join("region")).unwrap();
        std::fs::write(world.join("region").join("r.0.0.mca"), b"chunks").unwrap();
        let entry = put(dir.path(), &world).unwrap();
        assert!(!world.exists());
        assert_eq!(take_back(dir.path(), &entry).unwrap(), world);
        assert_eq!(std::fs::read(world.join("region").join("r.0.0.mca")).unwrap(), b"chunks");
        assert!(!dir.path().join(TRASH).join(&entry).exists());
    }

    #[test]
    fn taking_back_is_refused_for_good_when_the_place_is_taken_or_the_entry_gone() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("games").join("a").join("screenshots").join("1.png");
        std::fs::create_dir_all(shot.parent().unwrap()).unwrap();
        std::fs::write(&shot, b"old").unwrap();
        let entry = put(dir.path(), &shot).unwrap();
        std::fs::write(&shot, b"new").unwrap();
        let taken = take_back(dir.path(), &entry).unwrap_err();
        assert!(taken.is::<Final>());
        assert_eq!(std::fs::read(&shot).unwrap(), b"new");
        std::fs::remove_file(&shot).unwrap();
        take_back(dir.path(), &entry).unwrap();
        assert_eq!(std::fs::read(&shot).unwrap(), b"old");
        assert!(take_back(dir.path(), &entry).unwrap_err().is::<Final>());
        assert!(take_back(dir.path(), "../x").is_err());
    }

    #[test]
    fn only_paths_inside_the_instance_go_to_its_trash() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("x"), b"x").unwrap();
        assert!(put(dir.path(), &other.path().join("x")).is_err());
        assert!(put(dir.path(), dir.path()).is_err());
        assert!(put(dir.path(), &dir.path().join("missing")).unwrap_err().is::<Final>());
        assert!(other.path().join("x").exists());
    }

    #[test]
    fn two_entries_in_one_millisecond_stay_apart() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let a = put_at(dir.path(), &dir.path().join("a"), 5).unwrap();
        let b = put_at(dir.path(), &dir.path().join("b"), 5).unwrap();
        assert_ne!(a, b);
        take_back(dir.path(), &b).unwrap();
        assert_eq!(std::fs::read(dir.path().join("b")).unwrap(), b"b");
    }

    #[test]
    fn stale_entries_go_and_fresh_ones_and_strangers_stay() {
        let root = tempfile::tempdir().unwrap();
        let instance = root.path().join("i1");
        for name in ["a", "b"] {
            std::fs::create_dir_all(&instance).unwrap();
            std::fs::write(instance.join(name), name).unwrap();
        }
        let old = put_at(&instance, &instance.join("a"), 1_000).unwrap();
        let fresh = put_at(&instance, &instance.join("b"), 1_000_000).unwrap();
        std::fs::create_dir_all(instance.join(TRASH).join("keep")).unwrap();
        purge_stale_at(root.path(), 1_000_000, Duration::from_secs(60)).unwrap();
        assert!(!instance.join(TRASH).join(old).exists());
        assert!(instance.join(TRASH).join(fresh).exists());
        assert!(instance.join(TRASH).join("keep").exists());
    }
}
