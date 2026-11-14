//! Icons of mods for the mod browser: small image files in the cache folder, fetched off the UI thread, once per address.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha1::{Digest, Sha1};

use super::{drive, net, paths};

const PREFIX: &str = "https://cdn.modrinth.com/";
const MAX_ICON: u64 = 1 << 20;
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
const EXTENSIONS: [&str; 4] = ["png", "webp", "jpg", "gif"];

static ICONS: LazyLock<Icons> = LazyLock::new(Icons::default);

#[derive(Default)]
struct Icons {
    ready: Mutex<HashMap<String, PathBuf>>,
    failed: Mutex<HashMap<String, Instant>>,
    busy: Mutex<HashSet<String>>,
}

type Fetch<'a> = &'a (dyn Fn(&str) -> Result<Vec<u8>> + Sync);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The image file of a project's icon, once [`prepare`] has made one; never touches the disk or the network.
pub fn icon(project_id: &str) -> Option<PathBuf> {
    lock(&ICONS.ready).get(project_id).cloned()
}

/// Makes icon files for the `(project id, icon address)` pairs that have none yet. Blocking: run it off the UI thread.
/// Returns whether any icon appeared, so the caller knows to redraw.
pub fn prepare(wanted: &[(String, String)]) -> bool {
    prepare_in(&paths::data_dir().join("cache").join("modicons"), wanted, &ICONS, &fetch)
}

fn prepare_in(dir: &Path, wanted: &[(String, String)], icons: &Icons, fetch: Fetch) -> bool {
    let todo: Vec<&(String, String)> = wanted
        .iter()
        .filter(|(id, _)| !lock(&icons.ready).contains_key(id) && lock(&icons.failed).get(id).is_none_or(|at| at.elapsed() >= RETRY_AFTER) && lock(&icons.busy).insert(id.clone()))
        .collect();
    let loaded: Vec<(&String, Result<PathBuf>)> = std::thread::scope(|scope| {
        let jobs: Vec<_> = todo.iter().map(|(id, url)| (id, scope.spawn(move || load(dir, id, url, fetch)))).collect();
        jobs.into_iter().map(|(id, job)| (id, job.join().unwrap_or_else(|_| Err(anyhow::anyhow!("icon thread failed"))))).collect()
    });
    let mut changed = false;
    for (id, result) in loaded {
        lock(&icons.busy).remove(id);
        match result {
            Ok(path) => {
                lock(&icons.ready).insert(id.clone(), path);
                changed = true;
            }
            Err(_) => {
                lock(&icons.failed).insert(id.clone(), Instant::now());
            }
        }
    }
    changed
}

fn load(dir: &Path, id: &str, url: &str, fetch: Fetch) -> Result<PathBuf> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
        bail!("unsafe project id");
    }
    if !url.starts_with(PREFIX) {
        bail!("icon is not on Modrinth's CDN");
    }
    let digest = Sha1::digest(url.as_bytes());
    let stem = format!("{id}-{:02x}{:02x}{:02x}{:02x}", digest[0], digest[1], digest[2], digest[3]);
    for ext in EXTENSIONS {
        let path = dir.join(format!("{stem}.{ext}"));
        if path.is_file() {
            return Ok(path);
        }
    }
    let bytes = fetch(url)?;
    let ext = extension(&bytes).context("not a supported image")?;
    let path = dir.join(format!("{stem}.{ext}"));
    net::write_atomic(&path, &bytes)?;
    Ok(path)
}

fn extension(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [b'G', b'I', b'F', b'8', ..] => Some("gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("webp"),
        _ => None,
    }
}

fn fetch(url: &str) -> Result<Vec<u8>> {
    if drive::fenced("modrinth icon", url) && !drive::live() {
        bail!("fenced");
    }
    let agent = net::agent(None)?;
    let mut res = agent.get(url).call()?;
    Ok(res.body_mut().with_config().limit(MAX_ICON).read_to_vec()?)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    fn wanted(id: &str, url: &str) -> Vec<(String, String)> {
        vec![(id.to_owned(), url.to_owned())]
    }

    #[test]
    fn an_icon_is_fetched_once_and_found_on_disk_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let icons = Icons::default();
        let calls = AtomicUsize::new(0);
        let fetch = |_: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(PNG.to_vec())
        };
        let list = wanted("AANobbMI", "https://cdn.modrinth.com/data/AANobbMI/icon.png");
        assert!(prepare_in(dir.path(), &list, &icons, &fetch));
        let path = lock(&icons.ready).get("AANobbMI").cloned().unwrap();
        assert!(path.is_file() && path.extension().is_some_and(|e| e == "png"));
        assert!(!prepare_in(dir.path(), &list, &icons, &fetch));
        let fresh = Icons::default();
        assert!(prepare_in(dir.path(), &list, &fresh, &fetch));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn only_images_from_modrinth_with_a_safe_project_id_are_stored() {
        let dir = tempfile::tempdir().unwrap();
        let icons = Icons::default();
        let fetch = |_: &str| Ok(PNG.to_vec());
        for (id, url) in [("../x", "https://cdn.modrinth.com/a.png"), ("ok", "https://evil.example/a.png"), ("ok", "http://cdn.modrinth.com/a.png"), ("", "https://cdn.modrinth.com/a.png")] {
            assert!(!prepare_in(dir.path(), &wanted(id, url), &icons, &fetch), "{id} {url}");
        }
        assert!(!prepare_in(dir.path(), &wanted("ok", "https://cdn.modrinth.com/a.svg"), &icons, &|_| Ok(b"<svg/>".to_vec())));
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_failed_icon_is_not_asked_for_again_right_away() {
        let dir = tempfile::tempdir().unwrap();
        let icons = Icons::default();
        let list = wanted("p1", "https://cdn.modrinth.com/a.png");
        assert!(!prepare_in(dir.path(), &list, &icons, &|_| bail!("offline")));
        let asked = AtomicBool::new(false);
        assert!(!prepare_in(dir.path(), &list, &icons, &|_| {
            asked.store(true, Ordering::SeqCst);
            Ok(PNG.to_vec())
        }));
        assert!(!asked.load(Ordering::SeqCst));
    }

    #[test]
    fn an_icon_already_being_fetched_is_left_to_that_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let icons = Icons::default();
        lock(&icons.busy).insert("p1".to_owned());
        let list = wanted("p1", "https://cdn.modrinth.com/a.png");
        assert!(!prepare_in(dir.path(), &list, &icons, &|_| Ok(PNG.to_vec())));
        assert!(icons.ready.lock().unwrap().is_empty());
    }

    #[test]
    fn image_kinds_are_told_by_their_first_bytes() {
        assert_eq!(extension(&PNG), Some("png"));
        assert_eq!(extension(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(extension(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(extension(b"GIF89a"), Some("gif"));
        assert_eq!(extension(b"<svg"), None);
    }
}
