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

    #[test]
    fn built_in_jar_is_a_fabric_mod() {
        assert!(JAR.starts_with(b"PK"));
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(JAR)).unwrap();
        let json = zip.by_name("fabric.mod.json").unwrap();
        let value: serde_json::Value = serde_json::from_reader(json).unwrap();
        assert_eq!(value["id"], "manycraft");
    }
}
