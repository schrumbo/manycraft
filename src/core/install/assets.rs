use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::download::Job;

const RESOURCES_URL: &str = "https://resources.download.minecraft.net";

#[derive(Deserialize)]
pub struct AssetIndex {
    pub objects: HashMap<String, AssetObject>,
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

    #[test]
    fn malformed_hashes_are_rejected() {
        let idx = index(r#"{"objects":{"a":{"hash":"../../x","size":5}}}"#);
        assert!(jobs(&idx, Path::new("assets")).is_err());
    }
}
