//! The companion agent, built into the binary and written next to the data folder when games need it.

use std::path::PathBuf;

use anyhow::Result;

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

/// The first release the companion reaches, as people read it: "26.1".
pub fn first() -> String {
    format!("{}.{}", FIRST.0, FIRST.1)
}

/// Whether this build carries the companion for `game`, which previews, background saving and chat need, on every loader.
pub fn available(game: &str) -> bool {
    !AGENT.is_empty() && release(game).is_some_and(|r| r >= FIRST)
}

/// Path of the companion agent for `game` on disk, written from the built-in copy when missing; `None` when there is
/// none, so the game runs without it. Each build gets its own folder, since running games hold their copy open.
pub fn jar(game: &str) -> Result<Option<PathBuf>> {
    if !available(game) {
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
    fn games_get_it_from_26_1_on() {
        assert!(available("26.1"));
        assert!(available("26.1.2"));
        assert!(available("26.3"));
        assert!(available("27.0"));
        assert!(!available("1.21.11"));
        assert!(!available("25.4"));
        assert_eq!(first(), "26.1");
    }
}
