//! Icons of mods for the mod browser: small image files in the cache folder, fetched off the UI thread, once per address,
//! and stored at the size the browser draws them, so a decoded icon costs kilobytes instead of up to a megabyte.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};
use sha1::{Digest, Sha1};

use super::{drive, net, paths};

const PREFIX: &str = "https://cdn.modrinth.com/";
const MAX_ICON: u64 = 1 << 20;
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
const EXTENSIONS: [&str; 4] = ["png", "webp", "jpg", "gif"];
const SIDE: u32 = 72;
const MAX_SIDE: u32 = 4096;
const MAX_DECODED: u64 = 64 << 20;

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
    let path = dir.join(format!("{stem}-{SIDE}.png"));
    if path.is_file() {
        return Ok(path);
    }
    let old = EXTENSIONS.iter().map(|ext| dir.join(format!("{stem}.{ext}"))).find(|p| p.is_file());
    let kept = old.as_ref().and_then(|old| std::fs::read(old).ok()).and_then(|bytes| shrink(&bytes).ok());
    if let Some(old) = old {
        let _ = std::fs::remove_file(old);
    }
    let small = match kept {
        Some(small) => small,
        None => shrink(&fetch(url)?)?,
    };
    net::write_atomic(&path, &small)?;
    Ok(path)
}

pub(super) fn shrink(bytes: &[u8]) -> Result<Vec<u8>> {
    extension(bytes).context("not a supported image")?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED);
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits);
    let full = reader.decode()?;
    let side = full.width().min(full.height());
    let small = full.crop_imm((full.width() - side) / 2, (full.height() - side) / 2, side, side).resize_exact(SIDE, SIDE, FilterType::Triangle);
    let mut out = std::io::Cursor::new(Vec::new());
    small.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
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

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(width, height, image::Rgba([10, 200, 30, 255])).write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

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
            Ok(png(256, 128))
        };
        let list = wanted("AANobbMI", "https://cdn.modrinth.com/data/AANobbMI/icon.png");
        assert!(prepare_in(dir.path(), &list, &icons, &fetch));
        let path = lock(&icons.ready).get("AANobbMI").cloned().unwrap();
        assert!(path.is_file() && path.extension().is_some_and(|e| e == "png"));
        assert_eq!(image::image_dimensions(&path).unwrap(), (SIDE, SIDE));
        assert!(!prepare_in(dir.path(), &list, &icons, &fetch));
        let fresh = Icons::default();
        assert!(prepare_in(dir.path(), &list, &fresh, &fetch));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn only_images_from_modrinth_with_a_safe_project_id_are_stored() {
        let dir = tempfile::tempdir().unwrap();
        let icons = Icons::default();
        let fetch = |_: &str| Ok(png(8, 8));
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
            Ok(png(8, 8))
        }));
        assert!(!asked.load(Ordering::SeqCst));
    }

    #[test]
    fn an_icon_already_being_fetched_is_left_to_that_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let icons = Icons::default();
        lock(&icons.busy).insert("p1".to_owned());
        let list = wanted("p1", "https://cdn.modrinth.com/a.png");
        assert!(!prepare_in(dir.path(), &list, &icons, &|_| Ok(png(8, 8))));
        assert!(icons.ready.lock().unwrap().is_empty());
    }

    #[test]
    fn an_icon_cached_at_full_size_is_shrunk_without_asking_again() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://cdn.modrinth.com/data/p1/icon.png";
        let digest = Sha1::digest(url.as_bytes());
        let old = dir.path().join(format!("p1-{:02x}{:02x}{:02x}{:02x}.png", digest[0], digest[1], digest[2], digest[3]));
        std::fs::write(&old, png(512, 512)).unwrap();
        let icons = Icons::default();
        assert!(prepare_in(dir.path(), &wanted("p1", url), &icons, &|_| bail!("offline")));
        let path = lock(&icons.ready).get("p1").cloned().unwrap();
        assert_eq!(image::image_dimensions(&path).unwrap(), (SIDE, SIDE));
        assert!(!old.exists());
    }

    #[test]
    fn an_old_cached_icon_that_no_longer_decodes_is_fetched_again() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://cdn.modrinth.com/data/p1/icon.png";
        let digest = Sha1::digest(url.as_bytes());
        let old = dir.path().join(format!("p1-{:02x}{:02x}{:02x}{:02x}.png", digest[0], digest[1], digest[2], digest[3]));
        std::fs::write(&old, PNG).unwrap();
        let icons = Icons::default();
        assert!(prepare_in(dir.path(), &wanted("p1", url), &icons, &|_| Ok(png(64, 64))));
        assert!(!old.exists());
        assert_eq!(image::image_dimensions(lock(&icons.ready).get("p1").unwrap()).unwrap(), (SIDE, SIDE));
    }

    #[test]
    fn a_long_thin_icon_is_cropped_before_it_is_scaled() {
        let started = std::time::Instant::now();
        let small = shrink(&png(MAX_SIDE, 1)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
        assert_eq!(image::load_from_memory(&small).unwrap().width(), SIDE);
    }

    #[test]
    fn an_image_too_large_to_decode_safely_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let icons = Icons::default();
        assert!(!prepare_in(dir.path(), &wanted("p1", "https://cdn.modrinth.com/a.png"), &icons, &|_| Ok(png(MAX_SIDE + 1, 1))));
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
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
