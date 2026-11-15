//! Every Minecraft version Mojang lists, what kind it is, and which versions a loader supports.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::instance::Loader;
use super::{net, online, paths};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const FABRIC_GAMES: &str = "https://meta.fabricmc.net/v2/versions/game";
const QUILT_GAMES: &str = "https://meta.quiltmc.org/v3/versions/game";
const FORGE_METADATA: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json";
const NEOFORGE_VERSIONS: &str = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";

/// What kind of version Mojang published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Release,
    Snapshot,
    PreRelease,
    OldBeta,
    OldAlpha,
}

impl Kind {
    fn of(kind: &str, id: &str) -> Self {
        match kind {
            "release" => Self::Release,
            "old_beta" => Self::OldBeta,
            "old_alpha" => Self::OldAlpha,
            _ if id.contains("-pre") || id.contains("-rc") || id.contains(" Pre-Release") => Self::PreRelease,
            _ => Self::Snapshot,
        }
    }
}

/// A Minecraft version and its kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameVersion {
    pub id: String,
    pub kind: Kind,
}

#[derive(Deserialize)]
struct Manifest {
    versions: Vec<ManifestVersion>,
}

#[derive(Deserialize)]
struct ManifestVersion {
    id: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct LoaderGame {
    version: String,
}

#[derive(Deserialize)]
struct NeoForgeVersions {
    versions: Vec<String>,
}

/// Every version Mojang lists, newest first; without a connection, the last list that arrived.
pub fn catalog() -> Result<Vec<GameVersion>> {
    let agent = net::agent(None)?;
    cached(&cache_dir().join("versions.json"), || Ok(parse_catalog(net::get_json(&agent, MANIFEST_URL)?)), online::offline())
}

fn parse_catalog(manifest: Manifest) -> Vec<GameVersion> {
    manifest.versions.into_iter().map(|v| GameVersion { kind: Kind::of(&v.kind, &v.id), id: v.id }).collect()
}

/// The game versions `loader` supports, or `None` when it runs on every version (Vanilla).
pub fn supported(loader: &Loader) -> Result<Option<HashSet<String>>> {
    let agent = net::agent(None)?;
    let offline = online::offline();
    let dir = cache_dir();
    let list = match loader {
        Loader::Vanilla => return Ok(None),
        Loader::Fabric(_) => cached(&dir.join("games-fabric.json"), || Ok(names(net::get_json::<Vec<LoaderGame>>(&agent, FABRIC_GAMES)?)), offline)?,
        Loader::Quilt(_) => cached(&dir.join("games-quilt.json"), || Ok(names(net::get_json::<Vec<LoaderGame>>(&agent, QUILT_GAMES)?)), offline)?,
        Loader::Forge(_) => cached(&dir.join("games-forge.json"), || Ok(forge_games(net::get_json(&agent, FORGE_METADATA)?)), offline)?,
        Loader::NeoForge(_) => cached(&dir.join("games-neoforge.json"), || Ok(neoforge_games(net::get_json::<NeoForgeVersions>(&agent, NEOFORGE_VERSIONS)?.versions)), offline)?,
    };
    Ok(Some(list.into_iter().collect()))
}

fn names(games: Vec<LoaderGame>) -> Vec<String> {
    games.into_iter().map(|g| g.version).collect()
}

fn forge_games(metadata: HashMap<String, Vec<String>>) -> Vec<String> {
    metadata.into_iter().filter(|(_, builds)| !builds.is_empty()).map(|(game, _)| game).collect()
}

fn neoforge_games(versions: Vec<String>) -> Vec<String> {
    let mut games: Vec<String> = versions.iter().filter_map(|v| neoforge_game(v)).collect();
    games.sort();
    games.dedup();
    games
}

/// The Minecraft version a NeoForge version is for: 21.1.77 is 1.21.1, 26.3.0.103 is 26.3.
fn neoforge_game(version: &str) -> Option<String> {
    let release = version.split('-').next()?;
    let parts: Vec<u32> = release.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
    match parts.as_slice() {
        [year, drop, 0, _] if *year >= 25 => Some(format!("{year}.{drop}")),
        [year, drop, patch, _] if *year >= 25 => Some(format!("{year}.{drop}.{patch}")),
        [minor, 0, _] if (20..25).contains(minor) => Some(format!("1.{minor}")),
        [minor, patch, _] if (20..25).contains(minor) => Some(format!("1.{minor}.{patch}")),
        _ => None,
    }
}

/// The versions to offer, in catalog order: releases, or every kind with `everything`, limited to what the loader
/// supports. `keep` stays in the list whatever it is, so an instance's own version never disappears.
pub fn shown(catalog: &[GameVersion], everything: bool, supported: Option<&HashSet<String>>, keep: &str) -> Vec<GameVersion> {
    let mut out: Vec<GameVersion> = catalog
        .iter()
        .filter(|v| v.id == keep || ((everything || v.kind == Kind::Release) && supported.is_none_or(|s| s.contains(&v.id))))
        .cloned()
        .collect();
    if !keep.is_empty() && !out.iter().any(|v| v.id == keep) {
        out.insert(0, GameVersion { id: keep.to_owned(), kind: Kind::Release });
    }
    out
}

fn cache_dir() -> std::path::PathBuf {
    paths::data_dir().join("cache")
}

fn cached<T: Serialize + DeserializeOwned>(path: &Path, fetch: impl FnOnce() -> Result<T>, offline: bool) -> Result<T> {
    let saved = || std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<T>(&bytes).ok());
    if offline && let Some(found) = saved() {
        return Ok(found);
    }
    match fetch() {
        Ok(found) => {
            let _ = serde_json::to_vec(&found).map_err(anyhow::Error::from).and_then(|bytes| net::write_atomic(path, &bytes));
            Ok(found)
        }
        Err(e) if net::no_connection(&e) => saved().ok_or(e),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{"latest":{"release":"26.3","snapshot":"26.4-snapshot-1"},"versions":[
        {"id":"26.4-snapshot-1","type":"snapshot","url":"u","sha1":"s"},
        {"id":"26.3-rc1","type":"snapshot","url":"u","sha1":"s"},
        {"id":"26.3","type":"release","url":"u","sha1":"s"},
        {"id":"1.21-pre1","type":"snapshot","url":"u","sha1":"s"},
        {"id":"25w14a","type":"snapshot","url":"u","sha1":"s"},
        {"id":"1.20.1","type":"release","url":"u","sha1":"s"},
        {"id":"1.8.9","type":"release","url":"u","sha1":"s"},
        {"id":"b1.7.3","type":"old_beta","url":"u","sha1":"s"},
        {"id":"a1.2.6","type":"old_alpha","url":"u","sha1":"s"}]}"#;

    fn catalog() -> Vec<GameVersion> {
        parse_catalog(serde_json::from_str(MANIFEST).unwrap())
    }

    fn ids(list: &[GameVersion]) -> Vec<&str> {
        list.iter().map(|v| v.id.as_str()).collect()
    }

    #[test]
    fn manifest_types_become_kinds() {
        let kinds: Vec<Kind> = catalog().iter().map(|v| v.kind).collect();
        use Kind::*;
        assert_eq!(kinds, [Snapshot, PreRelease, Release, PreRelease, Snapshot, Release, Release, OldBeta, OldAlpha]);
    }

    #[test]
    fn releases_are_shown_by_default_and_everything_on_request() {
        let all = catalog();
        assert_eq!(ids(&shown(&all, false, None, "")), ["26.3", "1.20.1", "1.8.9"]);
        assert_eq!(shown(&all, true, None, "").len(), all.len());
    }

    #[test]
    fn a_loader_limits_the_list_to_what_it_supports() {
        let all = catalog();
        let fabric: HashSet<String> = ["26.4-snapshot-1", "26.3", "1.20.1", "25w14a"].map(str::to_owned).into();
        assert_eq!(ids(&shown(&all, false, Some(&fabric), "")), ["26.3", "1.20.1"]);
        assert_eq!(ids(&shown(&all, true, Some(&fabric), "")), ["26.4-snapshot-1", "26.3", "25w14a", "1.20.1"]);
    }

    #[test]
    fn the_current_version_always_stays() {
        let all = catalog();
        let fabric: HashSet<String> = ["26.3"].map(str::to_owned).into();
        assert_eq!(ids(&shown(&all, false, Some(&fabric), "b1.7.3")), ["26.3", "b1.7.3"]);
        assert_eq!(ids(&shown(&all, false, None, "9.9")), ["9.9", "26.3", "1.20.1", "1.8.9"]);
    }

    #[test]
    fn loader_lists_become_game_versions() {
        let fabric: Vec<LoaderGame> = serde_json::from_str(r#"[{"version":"26.3","stable":true},{"version":"25w14a","stable":false}]"#).unwrap();
        assert_eq!(names(fabric), ["26.3", "25w14a"]);
        let forge: HashMap<String, Vec<String>> = serde_json::from_str(r#"{"1.8.9":["1.8.9-11.15.1.2318-1.8.9"],"1.20.1":["1.20.1-47.3.0"],"1.0":[]}"#).unwrap();
        let mut games = forge_games(forge);
        games.sort();
        assert_eq!(games, ["1.20.1", "1.8.9"]);
        assert_eq!(neoforge_games(["21.1.77", "21.1.80", "20.4.237", "26.3.0.103-beta", "junk"].map(str::to_owned).into()), ["1.20.4", "1.21.1", "26.3"]);
    }

    #[test]
    fn the_list_is_kept_for_offline_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        let first = cached(&path, || Ok(catalog()), false).unwrap();
        let again: Vec<GameVersion> = cached(&path, || panic!("asked the network while offline"), true).unwrap();
        assert_eq!(first, again);
        let unreachable = || -> Result<Vec<GameVersion>> {
            let (agent, _) = net::fake::agent(None, std::time::Duration::from_secs(5));
            Ok(net::get_json(&agent, "http://piston-meta.test/manifest.json")?)
        };
        assert_eq!(cached(&path, unreachable, false).unwrap(), first);
    }
}
