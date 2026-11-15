//! The companion mod jar, built into the binary and written next to the data folder when instances need it.

use std::path::PathBuf;

use anyhow::Result;

use super::{net, paths};

const JAR: &[u8] = include_bytes!(env!("MANYCRAFT_COMPANION_JAR"));

/// Path of the companion mod jar on disk, written from the built-in copy when missing or different.
pub fn jar() -> Result<PathBuf> {
    let path = paths::data_dir().join("companion").join(super::fleet::COMPANION_FILE);
    let current = path.is_file() && net::file_sha1(&path).is_ok_and(|sha| sha == sha1());
    if !current {
        net::write_atomic(&path, JAR)?;
    }
    Ok(path)
}

fn sha1() -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(JAR).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mod_json() -> serde_json::Value {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(JAR)).unwrap();
        serde_json::from_reader(zip.by_name("fabric.mod.json").unwrap()).unwrap()
    }

    #[test]
    fn built_in_jar_is_a_fabric_mod() {
        assert!(JAR.starts_with(b"PK"));
        assert_eq!(mod_json()["id"], "manycraft");
    }

    #[test]
    fn built_in_jar_loads_on_the_older_fabric_loaders_packs_pin() {
        let wanted = mod_json()["depends"]["fabricloader"].as_str().unwrap().to_owned();
        let parts = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap()).collect::<Vec<_>>();
        let floor = wanted.strip_prefix(">=").unwrap();
        assert!(parts(floor) <= parts("0.16.0"), "{wanted}");
    }
}
