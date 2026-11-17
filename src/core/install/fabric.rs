use anyhow::{Result, bail};
use serde::Deserialize;

use super::{Versions, offered, unsupported};
use super::meta::VersionJson;
use crate::core::net;

const LOADERS_URL: &str = "https://meta.fabricmc.net/v2/versions/loader";
const QUILT_URL: &str = "https://meta.quiltmc.org/v3/versions/loader";

#[derive(Deserialize)]
struct Entry {
    loader: Loader,
}

#[derive(Deserialize)]
struct Loader {
    version: String,
    #[serde(default)]
    stable: bool,
}

/// Newest stable Fabric loader for `game`, or the newest of any kind when none is stable.
pub fn latest_loader(agent: &ureq::Agent, game: &str) -> Result<String> {
    let entries: Vec<Entry> = offered("Fabric", game, net::get_json(agent, &format!("{LOADERS_URL}/{game}")))?;
    pick_loader(entries).ok_or_else(|| unsupported("Fabric", game))
}

/// Fabric loaders for `game`, newest first, with the newest stable one as default.
pub fn versions(agent: &ureq::Agent, game: &str) -> Result<Versions> {
    let entries: Vec<Entry> = offered("Fabric", game, net::get_json(agent, &format!("{LOADERS_URL}/{game}")))?;
    if entries.is_empty() {
        return Err(unsupported("Fabric", game));
    }
    Ok(listed(entries))
}

fn listed(entries: Vec<Entry>) -> Versions {
    let default = entries.iter().find(|e| e.loader.stable).or(entries.first()).map(|e| e.loader.version.clone());
    Versions { list: entries.into_iter().map(|e| e.loader.version).collect(), default }
}

/// Newest stable Fabric loader for `game`, `None` when it has only unstable ones.
pub fn stable_loader(agent: &ureq::Agent, game: &str) -> Result<Option<String>> {
    let entries: Vec<Entry> = net::get_json(agent, &format!("{LOADERS_URL}/{game}"))?;
    Ok(entries.into_iter().find(|e| e.loader.stable).map(|e| e.loader.version))
}

/// Newest Quilt loader for `game` that is not a pre-release.
pub fn stable_quilt(agent: &ureq::Agent, game: &str) -> Result<Option<String>> {
    let entries: Vec<Entry> = net::get_json(agent, &format!("{QUILT_URL}/{game}"))?;
    Ok(entries.into_iter().map(|e| e.loader.version).find(|v| !v.contains('-')))
}

fn pick_loader(entries: Vec<Entry>) -> Option<String> {
    let index = entries.iter().position(|e| e.loader.stable).unwrap_or(0);
    entries.into_iter().nth(index).map(|e| e.loader.version)
}

/// Fabric's launcher profile for a game and loader version.
pub fn profile(agent: &ureq::Agent, game: &str, loader: &str) -> Result<VersionJson> {
    let profile: VersionJson = net::get_json(agent, &format!("{LOADERS_URL}/{game}/{loader}/profile/json"))?;
    if profile.id.is_empty() || profile.id.contains(['/', '\\']) || profile.id.starts_with('.') {
        bail!("Fabric returned an unusable profile id");
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(json: &str) -> Vec<Entry> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn first_stable_loader_wins() {
        let e = entries(r#"[{"loader":{"version":"0.21.0-beta","stable":false}},{"loader":{"version":"0.20.1","stable":true}},{"loader":{"version":"0.20.0","stable":true}}]"#);
        assert_eq!(pick_loader(e).unwrap(), "0.20.1");
    }

    #[test]
    fn newest_unstable_loader_is_the_fallback() {
        let e = entries(r#"[{"loader":{"version":"0.21.0-beta","stable":false}}]"#);
        assert_eq!(pick_loader(e).unwrap(), "0.21.0-beta");
    }

    #[test]
    fn versions_keep_meta_order_and_default_to_stable() {
        let e = entries(r#"[{"loader":{"version":"0.21.0-beta","stable":false}},{"loader":{"version":"0.20.1","stable":true}}]"#);
        let v = listed(e);
        assert_eq!((v.list.as_slice(), v.default.as_deref()), (&["0.21.0-beta".to_owned(), "0.20.1".to_owned()][..], Some("0.20.1")));
    }

    #[test]
    fn no_loaders_means_unsupported() {
        assert!(pick_loader(Vec::new()).is_none());
    }
}
