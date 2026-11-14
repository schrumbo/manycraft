//! Daily look for a newer release on GitHub. It only reports; nothing is downloaded or installed.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{net, paths};

const LATEST: &str = "https://api.github.com/repos/schrumbo/manycraft/releases/latest";
const FILE: &str = "update.json";
const TIMEOUT: Duration = Duration::from_secs(10);
const INTERVAL_SECS: u64 = 24 * 3600;

/// A published release newer than the running app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Cache {
    checked_at: u64,
    newest: Option<Release>,
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The newest release if it is newer than `current`. Asks GitHub at most once per 24 hours and answers from `update.json` in between.
pub fn check(current: &str) -> Result<Option<Release>> {
    let agent = net::agent_within(TIMEOUT);
    check_in(&paths::data_dir(), current, now(), || net::get_json(&agent, LATEST))
}

fn check_in(dir: &Path, current: &str, now: u64, fetch: impl FnOnce() -> Result<serde_json::Value>) -> Result<Option<Release>> {
    let path = dir.join(FILE);
    let cache: Option<Cache> = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
    if let Some(cache) = cache
        && cache.checked_at <= now
        && now - cache.checked_at < INTERVAL_SECS
    {
        return Ok(cache.newest.filter(|r| is_newer(&r.version, current)));
    }
    let latest: Latest = serde_json::from_value(fetch()?)?;
    let newest = (!latest.draft && !latest.prerelease && is_newer(&latest.tag_name, current))
        .then(|| Release { version: latest.tag_name.trim_start_matches(['v', 'V']).to_owned(), url: latest.html_url });
    let saved = serde_json::to_vec(&Cache { checked_at: now, newest: newest.clone() }).map_err(anyhow::Error::from).and_then(|bytes| net::write_atomic(&path, &bytes));
    if let Err(e) = saved {
        eprintln!("update: cannot remember the check: {e:#}");
    }
    Ok(newest)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn parts(version: &str) -> Option<Vec<u64>> {
    let core = version.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next().unwrap_or(core);
    core.split('.').map(|p| p.parse().ok()).collect()
}

fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (parts(candidate), parts(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> serde_json::Value {
        json!({ "tag_name": tag, "html_url": format!("https://github.com/schrumbo/manycraft/releases/tag/{tag}"), "draft": false, "prerelease": false })
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.2.1", "0.2"));
        assert!(!is_newer("0.2.0", "0.2"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("0.3.0-beta.1", "0.2.0"));
    }

    #[test]
    fn unreadable_versions_are_never_newer() {
        assert!(!is_newer("nightly", "0.2.0"));
        assert!(!is_newer("0.3.x", "0.2.0"));
        assert!(!is_newer("", "0.2.0"));
        assert!(!is_newer("0.3.0", "dev"));
        assert!(!is_newer("ü.ö", "0.2.0"));
    }

    #[test]
    fn a_newer_release_is_reported_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let found = check_in(dir.path(), "0.2.0", 1000, || Ok(release("v0.3.0"))).unwrap().unwrap();
        assert_eq!(found.version, "0.3.0");
        assert!(found.url.ends_with("v0.3.0"));
        assert!(dir.path().join(FILE).is_file());
    }

    #[test]
    fn within_a_day_the_cache_answers_without_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let first = check_in(dir.path(), "0.2.0", 1000, || Ok(release("v0.3.0"))).unwrap();
        let second = check_in(dir.path(), "0.2.0", 1000 + INTERVAL_SECS - 1, || panic!("asked the network")).unwrap();
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn after_a_day_it_asks_again() {
        let dir = tempfile::tempdir().unwrap();
        check_in(dir.path(), "0.2.0", 1000, || Ok(release("v0.3.0"))).unwrap();
        let again = check_in(dir.path(), "0.2.0", 1000 + INTERVAL_SECS, || Ok(release("v0.4.0"))).unwrap().unwrap();
        assert_eq!(again.version, "0.4.0");
    }

    #[test]
    fn a_cached_release_the_app_has_caught_up_with_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        check_in(dir.path(), "0.2.0", 1000, || Ok(release("v0.3.0"))).unwrap();
        assert_eq!(check_in(dir.path(), "0.3.0", 2000, || panic!("asked the network")).unwrap(), None);
    }

    #[test]
    fn drafts_prereleases_and_older_releases_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = release("v0.9.0");
        draft["draft"] = json!(true);
        assert_eq!(check_in(dir.path(), "0.2.0", 0, || Ok(draft)).unwrap(), None);
        let mut pre = release("v0.9.0");
        pre["prerelease"] = json!(true);
        assert_eq!(check_in(dir.path(), "0.2.0", INTERVAL_SECS, || Ok(pre)).unwrap(), None);
        assert_eq!(check_in(dir.path(), "0.2.0", 2 * INTERVAL_SECS, || Ok(release("v0.1.0"))).unwrap(), None);
    }

    #[test]
    fn a_failed_fetch_is_an_error_and_is_not_remembered() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_in(dir.path(), "0.2.0", 1000, || anyhow::bail!("offline")).is_err());
        assert!(!dir.path().join(FILE).exists());
        assert!(check_in(dir.path(), "0.2.0", 1001, || Ok(release("v0.3.0"))).unwrap().is_some());
    }

    #[test]
    fn a_damaged_cache_or_odd_answer_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), b"{ oops").unwrap();
        assert!(check_in(dir.path(), "0.2.0", 5, || Ok(release("v0.3.0"))).unwrap().is_some());
        assert!(check_in(dir.path(), "0.2.0", 5 + INTERVAL_SECS, || Ok(json!({ "message": "Not Found" }))).is_err());
    }

    #[test]
    fn a_clock_set_back_does_not_freeze_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        check_in(dir.path(), "0.2.0", 10 * INTERVAL_SECS, || Ok(release("v0.3.0"))).unwrap();
        let again = check_in(dir.path(), "0.2.0", 1000, || Ok(release("v0.4.0"))).unwrap().unwrap();
        assert_eq!(again.version, "0.4.0");
    }
}
