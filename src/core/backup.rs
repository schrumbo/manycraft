//! A copy of the small files that hold accounts, instances, batches and settings, written once before a pre-release
//! first starts on a data folder, so going back to the release before it never depends on what the pre-release wrote.

use std::path::Path;

use anyhow::{Context, Result};

use super::paths;

const KEPT: [&str; 2] = ["options.txt", "servers.dat"];
const LARGEST: u64 = 16 * 1024 * 1024;

/// The folder name of the copy made before `version` first started.
pub fn folder(version: &str) -> String {
    format!("backup-before-{version}")
}

/// Writes the copy for `version` into the data folder when `version` is a pre-release and no copy exists yet.
/// True when a copy was written now. An error means no complete copy exists and nothing should write to the data.
pub fn before_first_start(version: &str) -> Result<bool> {
    if !super::updater::release::is_prerelease(version) {
        return Ok(false);
    }
    write_in(&paths::data_dir(), version)
}

pub(super) fn write_in(root: &Path, version: &str) -> Result<bool> {
    let done = root.join(folder(version));
    if done.is_dir() || !root.is_dir() {
        return Ok(false);
    }
    let _alone = super::lock::exclusive(&root.join("backup.lock"))?;
    if done.is_dir() {
        return Ok(false);
    }
    let part = root.join(format!("{}.part", folder(version)));
    if std::fs::symlink_metadata(&part).is_ok() {
        std::fs::remove_dir_all(&part).with_context(|| format!("cannot clear {}", part.display()))?;
    }
    std::fs::create_dir_all(&part).with_context(|| format!("cannot create {}", part.display()))?;
    copy_files(root, &part, &|name| name.ends_with(".json"))?;
    for top in ["instances", "batches"] {
        let Ok(entries) = std::fs::read_dir(root.join(top)) else { continue };
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read {}", root.join(top).display()))?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            copy_files(&entry.path(), &part.join(top).join(&name), &|n| n.ends_with(".json") || KEPT.contains(&n))?;
        }
    }
    std::fs::rename(&part, &done).with_context(|| format!("cannot finish {}", done.display()))?;
    Ok(true)
}

fn copy_files(from: &Path, to: &Path, wanted: &dyn Fn(&str) -> bool) -> Result<()> {
    let entries = std::fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", from.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !entry.file_type().is_ok_and(|t| t.is_file()) || !wanted(&name) || meta.len() > LARGEST {
            continue;
        }
        std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
        std::fs::copy(entry.path(), to.join(&name)).with_context(|| format!("cannot copy {}", entry.path().display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(path: &Path, body: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn the_copy_holds_the_data_files_and_leaves_worlds_and_games_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(&root.join("accounts.json"), b"[1]");
        put(&root.join("settings.json"), b"{}");
        put(&root.join("manycraft.log"), b"log");
        put(&root.join("instances/a1/instance.json"), b"{\"id\":\"a1\"}");
        put(&root.join("instances/a1/options.txt"), b"soundCategory_master:0.0");
        put(&root.join("instances/a1/games/x/saves/Welt/level.dat"), b"world");
        put(&root.join("instances/a1/mods/sodium.jar"), b"jar");
        put(&root.join("instances/.trash/a2-1/instance.json"), b"{}");
        put(&root.join("batches/b1/batch.json"), b"{\"id\":\"b1\"}");
        put(&root.join("versions/26.3/26.3.json"), b"{}");
        assert!(write_in(root, "0.10.0-beta.2").unwrap());
        let copy = root.join("backup-before-0.10.0-beta.2");
        let read = |p: &str| std::fs::read(copy.join(p)).unwrap();
        assert_eq!(read("accounts.json"), b"[1]");
        assert_eq!(read("settings.json"), b"{}");
        assert_eq!(read("instances/a1/instance.json"), b"{\"id\":\"a1\"}");
        assert_eq!(read("instances/a1/options.txt"), b"soundCategory_master:0.0");
        assert_eq!(read("batches/b1/batch.json"), b"{\"id\":\"b1\"}");
        for gone in ["manycraft.log", "instances/a1/games", "instances/a1/mods", "instances/.trash", "versions"] {
            assert!(!copy.join(gone).exists(), "{gone}");
        }
        assert!(!root.join("backup-before-0.10.0-beta.2.part").exists());
        assert_eq!(std::fs::read(root.join("accounts.json")).unwrap(), b"[1]", "the originals stay");
    }

    #[test]
    fn a_copy_is_written_once_per_version_and_a_broken_one_is_redone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(&root.join("accounts.json"), b"[1]");
        assert!(write_in(root, "0.10.0-beta.2").unwrap());
        put(&root.join("accounts.json"), b"[2]");
        assert!(!write_in(root, "0.10.0-beta.2").unwrap());
        assert_eq!(std::fs::read(root.join("backup-before-0.10.0-beta.2/accounts.json")).unwrap(), b"[1]", "the first copy stays");
        put(&root.join("backup-before-0.10.0-beta.3.part/accounts.json"), b"half");
        assert!(write_in(root, "0.10.0-beta.3").unwrap());
        assert_eq!(std::fs::read(root.join("backup-before-0.10.0-beta.3/accounts.json")).unwrap(), b"[2]");
        assert!(!root.join("backup-before-0.10.0-beta.3.part").exists());
    }

    #[test]
    fn two_first_starts_at_once_leave_one_whole_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for i in 0..40 {
            put(&root.join(format!("instances/i{i}/instance.json")), b"{}");
        }
        let both: Vec<bool> = std::thread::scope(|s| {
            let runs: Vec<_> = (0..2).map(|_| s.spawn(|| write_in(&root, "0.10.0-beta.2").unwrap())).collect();
            runs.into_iter().map(|r| r.join().unwrap()).collect()
        });
        assert_eq!(both.iter().filter(|w| **w).count(), 1);
        for i in 0..40 {
            assert!(root.join(format!("backup-before-0.10.0-beta.2/instances/i{i}/instance.json")).is_file());
        }
        assert!(!root.join("backup-before-0.10.0-beta.2.part").exists());
    }

    #[test]
    fn nothing_is_written_without_a_data_folder_or_for_a_release() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none");
        assert!(!write_in(&missing, "0.10.0-beta.2").unwrap());
        assert!(!missing.exists());
        assert!(!before_first_start("0.10.0").unwrap());
        assert_eq!(folder("0.10.0-beta.2"), "backup-before-0.10.0-beta.2");
    }
}
