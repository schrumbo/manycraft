//! The companion mod jars, built into the binary and written next to the data folder when instances need them.

use std::path::PathBuf;

use anyhow::Result;

use super::instance::Loader;
use super::{net, paths};

const FABRIC: &[u8] = include_bytes!(env!("MANYCRAFT_COMPANION_JAR"));
const NEOFORGE: &[u8] = include_bytes!(env!("MANYCRAFT_COMPANION_NEOFORGE_JAR"));
const FORGE: &[u8] = include_bytes!(env!("MANYCRAFT_COMPANION_FORGE_JAR"));

/// The Minecraft version the companion mod is built for; it loads on its patch releases too.
pub const GAME: &str = "26.3";

fn built_in(loader: &Loader, game: &str) -> Option<(&'static [u8], &'static str)> {
    let fits = game == GAME || game.strip_prefix(GAME).is_some_and(|rest| rest.starts_with('.'));
    let (bytes, name) = match loader {
        _ if !fits => return None,
        Loader::Vanilla => return None,
        Loader::Fabric(_) | Loader::Quilt(_) => (FABRIC, "manycraft-companion.jar"),
        Loader::NeoForge(_) => (NEOFORGE, "manycraft-companion-neoforge.jar"),
        Loader::Forge(_) => (FORGE, "manycraft-companion-forge.jar"),
    };
    (!bytes.is_empty()).then_some((bytes, name))
}

/// Whether this build carries a companion mod for `loader` on `game`, which previews, background saving and chat need.
pub fn available(loader: &Loader, game: &str) -> bool {
    built_in(loader, game).is_some()
}

/// Path of the companion mod jar for `loader` on `game` on disk, written from the built-in copy when missing or
/// different; `None` when there is none, so the game runs without it.
pub fn jar(loader: &Loader, game: &str) -> Result<Option<PathBuf>> {
    let Some((bytes, name)) = built_in(loader, game) else { return Ok(None) };
    let path = paths::data_dir().join("companion").join(name);
    let current = path.is_file() && net::file_sha1(&path).is_ok_and(|sha| sha == sha1(bytes));
    if !current {
        net::write_atomic(&path, bytes)?;
    }
    Ok(Some(path))
}

fn sha1(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn entry(jar: &[u8], name: &str) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(jar)).unwrap();
        let mut text = String::new();
        zip.by_name(name).unwrap().read_to_string(&mut text).unwrap();
        text
    }

    fn mod_json() -> serde_json::Value {
        serde_json::from_str(&entry(FABRIC, "fabric.mod.json")).unwrap()
    }

    #[test]
    fn built_in_jar_is_a_fabric_mod() {
        assert!(FABRIC.starts_with(b"PK"));
        assert_eq!(mod_json()["id"], "manycraft");
    }

    #[test]
    fn built_in_jar_loads_on_the_older_fabric_loaders_packs_pin() {
        let wanted = mod_json()["depends"]["fabricloader"].as_str().unwrap().to_owned();
        let parts = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap()).collect::<Vec<_>>();
        let floor = wanted.strip_prefix(">=").unwrap();
        assert!(parts(floor) <= parts("0.16.0"), "{wanted}");
    }

    #[test]
    fn the_game_version_matches_what_the_fabric_jar_declares() {
        assert_eq!(mod_json()["depends"]["minecraft"], format!("~{GAME}"));
    }

    #[test]
    fn the_fml_jars_declare_the_same_game_version() {
        let range = format!("\"[{GAME},");
        if !NEOFORGE.is_empty() {
            assert!(entry(NEOFORGE, "META-INF/neoforge.mods.toml").contains(&range));
        }
        if !FORGE.is_empty() {
            assert!(entry(FORGE, "META-INF/mods.toml").contains(&range));
        }
    }

    #[test]
    fn only_loaders_and_versions_with_a_jar_get_one() {
        assert!(available(&Loader::Fabric(None), GAME));
        assert!(available(&Loader::Quilt(Some("0.30.0".into())), &format!("{GAME}.1")));
        assert!(!available(&Loader::Fabric(None), "1.20.1"));
        assert!(!available(&Loader::Fabric(None), &format!("{GAME}0")));
        assert!(!available(&Loader::Vanilla, GAME));
        assert_eq!(available(&Loader::NeoForge(None), GAME), !NEOFORGE.is_empty());
        assert_eq!(available(&Loader::Forge(None), GAME), !FORGE.is_empty());
        assert!(!available(&Loader::Forge(None), "1.20.1"));
    }
}
