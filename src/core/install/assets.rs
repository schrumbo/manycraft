use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::download::{Job, join_safe};

const RESOURCES_URL: &str = "https://resources.download.minecraft.net";

#[derive(Deserialize)]
pub struct AssetIndex {
    pub objects: HashMap<String, AssetObject>,
    /// Games up to 1.7.2 read assets by name from `virtual/<index>` instead of by hash.
    #[serde(default, rename = "virtual")]
    pub by_name: bool,
    /// Games before 1.6 read assets from `resources` in the game folder.
    #[serde(default)]
    pub map_to_resources: bool,
}

#[derive(Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

/// One download per distinct object, stored as `objects/<first two hash chars>/<hash>`.
pub fn jobs(index: &AssetIndex, assets_dir: &Path) -> Result<Vec<Job>> {
    let mut unique = BTreeMap::new();
    for object in index.objects.values() {
        unique.insert(object.hash.as_str(), object.size);
    }
    let objects = assets_dir.join("objects");
    let mut jobs = Vec::with_capacity(unique.len());
    for (hash, size) in unique {
        let valid = hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        let Some(prefix) = hash.get(..2).filter(|_| valid) else {
            bail!("invalid asset hash {hash}");
        };
        jobs.push(Job::new(format!("{RESOURCES_URL}/{prefix}/{hash}"), objects.join(prefix).join(hash), Some(hash.to_owned()), Some(size)));
    }
    Ok(jobs)
}

fn object_path(assets_dir: &Path, hash: &str) -> Result<PathBuf> {
    let prefix = hash.get(..2).filter(|_| hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit())).with_context(|| format!("invalid asset hash {hash}"))?;
    Ok(assets_dir.join("objects").join(prefix).join(hash))
}

/// Where a game that reads its assets by name finds them: `virtual/<index>` for an index read by name, the game folder's
/// `resources` for one mapped there, `None` for every newer index. Reads the index the install left in `indexes`.
pub fn legacy_dir(assets_dir: &Path, index_id: &str, game_dir: &Path) -> Option<PathBuf> {
    let index: AssetIndex = serde_json::from_slice(&std::fs::read(assets_dir.join("indexes").join(format!("{index_id}.json"))).ok()?).ok()?;
    legacy_target(&index, assets_dir, index_id, game_dir).ok().flatten()
}

fn legacy_target(index: &AssetIndex, assets_dir: &Path, index_id: &str, game_dir: &Path) -> Result<Option<PathBuf>> {
    Ok(if index.map_to_resources {
        Some(game_dir.join("resources"))
    } else if index.by_name {
        Some(join_safe(&assets_dir.join("virtual"), index_id)?)
    } else {
        None
    })
}

/// Lays the assets of an old index out by name where its game reads them, linking to the downloaded objects where the
/// file system allows and copying otherwise. Files already there are kept. Returns that folder, or `None` for newer indexes.
pub fn place_legacy(assets_dir: &Path, index_id: &str, game_dir: &Path) -> Result<Option<PathBuf>> {
    let path = assets_dir.join("indexes").join(format!("{index_id}.json"));
    let Ok(bytes) = std::fs::read(&path) else { return Ok(None) };
    let index: AssetIndex = serde_json::from_slice(&bytes).with_context(|| format!("cannot read {}", path.display()))?;
    let Some(target) = legacy_target(&index, assets_dir, index_id, game_dir)? else { return Ok(None) };
    for (name, object) in &index.objects {
        let to = join_safe(&target, name)?;
        if to.metadata().is_ok_and(|m| m.len() == object.size) {
            continue;
        }
        let from = object_path(assets_dir, &object.hash)?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let _ = std::fs::remove_file(&to);
        match std::fs::hard_link(&from, &to) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                std::fs::copy(&from, &to).with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
            }
        }
    }
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const HASH_B: &str = "ffffffffffffffffffffffffffffffffffffffff";

    fn index(json: &str) -> AssetIndex {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn objects_map_to_prefixed_paths_and_urls() {
        let idx = index(&format!(r#"{{"objects":{{"a.png":{{"hash":"{HASH_A}","size":5}},"b.png":{{"hash":"{HASH_B}","size":7}}}}}}"#));
        let jobs = jobs(&idx, Path::new("assets")).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].url, format!("https://resources.download.minecraft.net/01/{HASH_A}"));
        assert_eq!(jobs[0].path, Path::new("assets").join("objects").join("01").join(HASH_A));
        assert_eq!(jobs[0].size, Some(5));
        assert_eq!(jobs[1].sha1.as_deref(), Some(HASH_B));
    }

    #[test]
    fn identical_objects_are_downloaded_once() {
        let idx = index(&format!(r#"{{"objects":{{"a":{{"hash":"{HASH_A}","size":5}},"b":{{"hash":"{HASH_A}","size":5}}}}}}"#));
        assert_eq!(jobs(&idx, Path::new("assets")).unwrap().len(), 1);
    }

    fn legacy_root(index_json: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("assets");
        let object = assets.join("objects").join("01").join(HASH_A);
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&object, b"sound").unwrap();
        std::fs::create_dir_all(assets.join("indexes")).unwrap();
        std::fs::write(assets.join("indexes").join("old.json"), index_json).unwrap();
        let game = dir.path().join("game");
        (dir, assets, game)
    }

    #[test]
    fn an_index_read_by_name_is_laid_out_in_virtual() {
        let (_dir, assets, game) = legacy_root(&format!(r#"{{"virtual":true,"objects":{{"sound/step.ogg":{{"hash":"{HASH_A}","size":5}}}}}}"#));
        let placed = place_legacy(&assets, "old", &game).unwrap().unwrap();
        assert_eq!(placed, assets.join("virtual").join("old"));
        assert_eq!(std::fs::read(placed.join("sound").join("step.ogg")).unwrap(), b"sound");
        assert_eq!(legacy_dir(&assets, "old", &game), Some(placed.clone()));
        assert_eq!(place_legacy(&assets, "old", &game).unwrap(), Some(placed));
    }

    #[test]
    fn an_index_mapped_to_resources_goes_into_the_game_folder() {
        let (_dir, assets, game) = legacy_root(&format!(r#"{{"map_to_resources":true,"objects":{{"sound/step.ogg":{{"hash":"{HASH_A}","size":5}}}}}}"#));
        let placed = place_legacy(&assets, "old", &game).unwrap().unwrap();
        assert_eq!(placed, game.join("resources"));
        assert!(game.join("resources").join("sound").join("step.ogg").is_file());
    }

    #[test]
    fn a_modern_index_needs_no_layout() {
        let (_dir, assets, game) = legacy_root(&format!(r#"{{"objects":{{"a":{{"hash":"{HASH_A}","size":5}}}}}}"#));
        assert_eq!(place_legacy(&assets, "old", &game).unwrap(), None);
        assert_eq!(legacy_dir(&assets, "old", &game), None);
        assert_eq!(place_legacy(&assets, "missing", &game).unwrap(), None);
    }

    #[test]
    fn a_legacy_name_cannot_leave_its_folder() {
        let (_dir, assets, game) = legacy_root(&format!(r#"{{"virtual":true,"objects":{{"../../evil":{{"hash":"{HASH_A}","size":5}}}}}}"#));
        assert!(place_legacy(&assets, "old", &game).is_err());
    }

    #[test]
    fn malformed_hashes_are_rejected() {
        let idx = index(r#"{"objects":{"a":{"hash":"../../x","size":5}}}"#);
        assert!(jobs(&idx, Path::new("assets")).is_err());
    }
}
