use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::meta::VersionJson;
use crate::core::net;

const LOADERS_URL: &str = "https://meta.fabricmc.net/v2/versions/loader";

#[derive(Deserialize)]
struct Entry {
    loader: Loader,
}

#[derive(Deserialize)]
struct Loader {
    version: String,
    stable: bool,
}

/// Newest stable Fabric loader for `game`, or the newest of any kind when none is stable.
pub fn latest_loader(agent: &ureq::Agent, game: &str) -> Result<String> {
    let entries: Vec<Entry> = net::get_json(agent, &format!("{LOADERS_URL}/{game}"))?;
    pick_loader(entries).with_context(|| format!("Fabric does not support Minecraft {game}"))
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
    fn no_loaders_means_unsupported() {
        assert!(pick_loader(Vec::new()).is_none());
    }
}
