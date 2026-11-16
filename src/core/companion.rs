//! The companion agent, built into the binary and written next to the data folder when games need it.

use std::path::PathBuf;

use anyhow::Result;

use super::instance::Loader;
use super::{net, paths};

const AGENT: &[u8] = include_bytes!(env!("MANYCRAFT_AGENT_JAR"));

/// The file name the agent's manifest expects for itself; its boot classes are found by this name.
const AGENT_FILE: &str = "manycraft-agent.jar";

/// The first release the companion supports; older games run without it.
const FIRST: (u32, u32) = (26, 1);

fn release(game: &str) -> Option<(u32, u32)> {
    let (major, rest) = game.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether this build carries the companion for `game`, which previews, background saving and chat need. The loader
/// does not matter: the agent works the same on all of them.
pub fn available(_loader: &Loader, game: &str) -> bool {
    !AGENT.is_empty() && release(game).is_some_and(|r| r >= FIRST)
}

/// Path of the companion agent for `game` on disk, written from the built-in copy when missing; `None` when there is
/// none, so the game runs without it. Each build gets its own folder, since running games hold their copy open.
pub fn jar(loader: &Loader, game: &str) -> Result<Option<PathBuf>> {
    if !available(loader, game) {
        return Ok(None);
    }
    let sha = sha1(AGENT);
    let path = paths::data_dir().join("companion").join(format!("agent-{}", &sha[..12])).join(AGENT_FILE);
    let current = path.is_file() && net::file_sha1(&path).is_ok_and(|have| have == sha);
    if !current {
        net::write_atomic(&path, AGENT)?;
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

    #[test]
    fn built_in_jar_is_an_agent_that_finds_itself() {
        assert!(AGENT.starts_with(b"PK"));
        let manifest = entry(AGENT, "META-INF/MANIFEST.MF");
        assert!(manifest.contains("Premain-Class: io.github.schrumbo.manycraft.agent.Agent"), "{manifest}");
        assert!(manifest.contains(&format!("Boot-Class-Path: {AGENT_FILE}")), "{manifest}");
    }

    #[test]
    fn releases_parse_from_their_leading_numbers() {
        assert_eq!(release("26.1"), Some((26, 1)));
        assert_eq!(release("26.1.2"), Some((26, 1)));
        assert_eq!(release("26.3-rc-1"), Some((26, 3)));
        assert_eq!(release("1.21.4"), Some((1, 21)));
        assert_eq!(release("26w14a"), None);
        assert_eq!(release(""), None);
    }

    #[test]
    fn every_loader_gets_it_from_26_1_on() {
        for loader in [Loader::Vanilla, Loader::Fabric(None), Loader::Quilt(Some("0.30.0".into())), Loader::NeoForge(None), Loader::Forge(None)] {
            assert!(available(&loader, "26.1"));
            assert!(available(&loader, "26.1.2"));
            assert!(available(&loader, "26.3"));
            assert!(available(&loader, "27.0"));
            assert!(!available(&loader, "1.21.11"));
            assert!(!available(&loader, "25.4"));
        }
    }
}
