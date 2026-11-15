use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::meta::VersionJson;
use super::{Versions, newest_first};
use crate::core::net;

const LOADERS_URL: &str = "https://meta.quiltmc.org/v3/versions/loader";

#[derive(Deserialize)]
struct Entry {
    loader: Loader,
}

#[derive(Deserialize)]
struct Loader {
    version: String,
}

/// Quilt loaders for `game`, newest first; the newest without a pre-release tag is the default.
pub fn versions(agent: &ureq::Agent, game: &str) -> Result<Versions> {
    let entries: Vec<Entry> = net::get_json(agent, &format!("{LOADERS_URL}/{game}"))?;
    let found = pick(entries);
    if found.list.is_empty() {
        bail!("Quilt does not support Minecraft {game}");
    }
    Ok(found)
}

fn pick(entries: Vec<Entry>) -> Versions {
    let list = newest_first(entries.into_iter().map(|e| e.loader.version).collect());
    let default = list.iter().find(|v| !v.contains('-')).or(list.first()).cloned();
    Versions { list, default }
}

/// Newest stable Quilt loader for `game`.
pub fn latest_loader(agent: &ureq::Agent, game: &str) -> Result<String> {
    versions(agent, game)?.default.with_context(|| format!("Quilt does not support Minecraft {game}"))
}

/// Quilt's launcher profile for a game and loader version.
pub fn profile(agent: &ureq::Agent, game: &str, loader: &str) -> Result<VersionJson> {
    let profile: VersionJson = net::get_json(agent, &format!("{LOADERS_URL}/{game}/{loader}/profile/json"))?;
    if profile.main_class.is_none() {
        bail!("Quilt returned a profile without a main class");
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(versions: &[&str]) -> Vec<Entry> {
        versions.iter().map(|v| Entry { loader: Loader { version: (*v).to_owned() } }).collect()
    }

    #[test]
    fn unsorted_meta_is_sorted_and_the_newest_stable_wins() {
        let found = pick(entries(&["0.20.0-beta.9", "0.24.0", "0.30.0", "0.29.2", "0.30.2", "0.31.0-beta.1", "0.30.10"]));
        assert_eq!(found.list, ["0.31.0-beta.1", "0.30.10", "0.30.2", "0.30.0", "0.29.2", "0.24.0", "0.20.0-beta.9"]);
        assert_eq!(found.default.as_deref(), Some("0.30.10"));
    }

    #[test]
    fn only_betas_fall_back_to_the_newest_beta() {
        let found = pick(entries(&["0.1.0-beta.1", "0.1.0-beta.2"]));
        assert_eq!(found.default.as_deref(), Some("0.1.0-beta.2"));
    }

    #[test]
    fn profile_json_parses_into_a_version() {
        let profile: VersionJson = serde_json::from_str(include_str!("../testdata/install/quilt.json")).unwrap();
        assert_eq!(profile.id, "quilt-loader-0.30.0-26.3");
        assert_eq!(profile.main_class.as_deref(), Some("org.quiltmc.loader.impl.launch.knot.KnotClient"));
        assert!(profile.libraries.iter().any(|l| l.name == "org.quiltmc:quilt-loader:0.30.0"));
    }
}
