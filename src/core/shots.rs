//! Screenshots: the files in the game folders of an instance, and those connection-only accounts hold in memory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use uuid::Uuid;

use super::worlds::distinct_dirs;

/// One screenshot in the game folder of one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shot {
    pub account: Uuid,
    pub path: PathBuf,
    /// Unix seconds.
    pub taken: i64,
}

/// A screenshot of a connection-only account, held only while Manycraft runs: PNG files of its view from the eyes and
/// from above.
#[derive(Clone, PartialEq, Eq)]
pub struct Moment {
    /// Unix seconds.
    pub taken: i64,
    pub eyes: Arc<[u8]>,
    pub above: Arc<[u8]>,
}

impl std::fmt::Debug for Moment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Moment").field("taken", &self.taken).field("eyes", &self.eyes.len()).field("above", &self.above.len()).finish()
    }
}

/// RGBA rows of `width` by `height`, top down, as a PNG file.
pub fn encode(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image::write_buffer_with_format(&mut std::io::Cursor::new(&mut out), rgba, width, height, image::ExtendedColorType::Rgba8, image::ImageFormat::Png).context("cannot encode the picture")?;
    Ok(out)
}

/// Every screenshot in every game folder of the instance in `instance_dir`, newest first.
pub fn list(instance_dir: &Path) -> Vec<Shot> {
    let mut shots = Vec::new();
    for (account, game) in distinct_dirs(instance_dir, "screenshots") {
        let Ok(entries) = std::fs::read_dir(game.join("screenshots")) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let picture = path.extension().and_then(|x| x.to_str()).is_some_and(|x| ["png", "jpg", "jpeg"].iter().any(|k| x.eq_ignore_ascii_case(k)));
            let Ok(meta) = entry.metadata() else { continue };
            if !picture || !meta.is_file() {
                continue;
            }
            let taken = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_secs()).ok()).unwrap_or(0);
            shots.push(Shot { account, path, taken });
        }
    }
    shots.sort_by(|a, b| b.taken.cmp(&a.taken).then_with(|| b.path.cmp(&a.path)));
    shots
}

fn open(path: &Path) -> Result<image::DynamicImage> {
    image::ImageReader::open(path).with_context(|| format!("cannot read {}", path.display()))?.with_guessed_format()?.decode().with_context(|| format!("cannot decode {}", path.display()))
}

/// The picture at `path` in full size, as RGBA.
pub fn decode(path: &Path) -> Result<image::RgbaImage> {
    Ok(open(path)?.into_rgba8())
}

/// The picture at `path` scaled down to fit `width` by `height` as RGBA, with the size of the file's own picture; one that
/// is smaller keeps its size.
pub fn fitted(path: &Path, width: u32, height: u32) -> Result<(image::RgbaImage, (u32, u32))> {
    Ok(fit(open(path)?, width, height))
}

/// The PNG file in `png` scaled down like [`fitted`].
pub fn fitted_png(png: &[u8], width: u32, height: u32) -> Result<(image::RgbaImage, (u32, u32))> {
    Ok(fit(image::load_from_memory_with_format(png, image::ImageFormat::Png).context("cannot decode the picture")?, width, height))
}

/// The PNG file in `png` in full size, as RGBA.
pub fn decode_png(png: &[u8]) -> Result<image::RgbaImage> {
    Ok(image::load_from_memory_with_format(png, image::ImageFormat::Png).context("cannot decode the picture")?.into_rgba8())
}

fn fit(picture: image::DynamicImage, width: u32, height: u32) -> (image::RgbaImage, (u32, u32)) {
    let original = (picture.width(), picture.height());
    if original.0 <= width && original.1 <= height {
        return (picture.into_rgba8(), original);
    }
    (picture.resize(width, height, image::imageops::FilterType::Triangle).into_rgba8(), original)
}

/// The picture at `path` scaled to fit `width` by `height`, as RGBA.
pub fn thumbnail(path: &Path, width: u32, height: u32) -> Result<image::RgbaImage> {
    Ok(open(path)?.thumbnail(width, height).into_rgba8())
}

/// The file at `path` as PNG bytes, for the clipboard.
pub fn png(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if bytes.starts_with(b"\x89PNG") {
        return Ok(bytes);
    }
    let picture = image::load_from_memory(&bytes).with_context(|| format!("cannot decode {}", path.display()))?;
    let mut out = Vec::new();
    picture.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshots_of_every_account_newest_first_pictures_only() {
        let dir = tempfile::tempdir().unwrap();
        let games = super::super::instance::games_dir(dir.path());
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let picture = image::RgbaImage::from_pixel(64, 36, image::Rgba([10, 20, 30, 255]));
        for (account, name, age) in [(a, "old.png", 100), (b, "new.png", 10), (a, "notes.txt", 0)] {
            let shots = games.join(account.simple().to_string()).join("screenshots");
            std::fs::create_dir_all(&shots).unwrap();
            let path = shots.join(name);
            picture.save_with_format(&path, image::ImageFormat::Png).unwrap();
            let at = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(at).unwrap();
        }
        let found = list(dir.path());
        assert_eq!(found.iter().map(|s| (s.account, s.path.file_name().unwrap().to_str().unwrap())).collect::<Vec<_>>(), vec![(b, "new.png"), (a, "old.png")]);
        let thumb = thumbnail(&found[0].path, 32, 32).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (32, 18));
        assert!(png(&found[0].path).unwrap().starts_with(b"\x89PNG"));
    }

    #[test]
    fn a_held_picture_round_trips_as_png_and_its_debug_shows_only_sizes() {
        let rgba: Vec<u8> = (0..4 * 3 * 2).map(|i| i as u8).collect();
        let png = encode(&rgba, 3, 2).unwrap();
        assert_eq!(decode_png(&png).unwrap().into_raw(), rgba);
        let (fitted, original) = fitted_png(&png, 2, 2).unwrap();
        assert_eq!(original, (3, 2));
        assert!(fitted.width() <= 2);
        let moment = Moment { taken: 1, eyes: png.clone().into(), above: png.into() };
        assert!(!format!("{moment:?}").contains('['), "PNG bytes in Debug");
    }
}