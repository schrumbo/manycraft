//! The last preview frame of every instance, kept as a small JPEG in Manycraft's cache so a stopped tile still shows where it was.

use std::path::PathBuf;

use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageFormat, RgbImage};

use super::{net, paths};

const WIDTH: u32 = 320;
const QUALITY: u8 = 80;

fn path(key: &str) -> Option<PathBuf> {
    (!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| paths::data_dir().join("cache").join("frames").join(format!("{key}.jpg")))
}

/// Saves a frame of `width` by `height` BGRA pixels as the still of instance `key`, at most 320 pixels wide.
pub fn save(key: &str, width: u32, height: u32, bgra: &[u8]) -> Result<()> {
    let path = path(key).context("invalid instance key")?;
    net::write_atomic(&path, &encode(width, height, bgra)?)
}

/// The still of instance `key` as width, height and RGBA pixels.
pub fn load(key: &str) -> Option<(u32, u32, Vec<u8>)> {
    decode(&std::fs::read(path(key)?).ok()?)
}

fn encode(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>> {
    let rgb: Vec<u8> = bgra.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
    let mut frame = RgbImage::from_raw(width, height, rgb).context("frame size does not match its pixels")?;
    if width > WIDTH {
        frame = image::imageops::resize(&frame, WIDTH, (height * WIDTH / width).max(1), FilterType::Triangle);
    }
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, QUALITY).encode_image(&frame)?;
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let frame = image::load_from_memory_with_format(bytes, ImageFormat::Jpeg).ok()?.into_rgba8();
    Some((frame.width(), frame.height(), frame.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, path};

    #[test]
    fn a_still_comes_back_scaled_down_with_its_colours() {
        let bgra: Vec<u8> = (0..640 * 360).flat_map(|_| [200, 100, 50, 255]).collect();
        let (width, height, rgba) = decode(&encode(640, 360, &bgra).unwrap()).unwrap();
        assert_eq!((width, height), (320, 180));
        let p = &rgba[rgba.len() / 2..rgba.len() / 2 + 4];
        assert!(p[0].abs_diff(50) < 8 && p[1].abs_diff(100) < 8 && p[2].abs_diff(200) < 8 && p[3] == 255);
    }

    #[test]
    fn keys_cannot_leave_the_cache() {
        assert!(path("../accounts").is_none() && path("").is_none());
        assert!(path("abc-0123").is_some());
        assert!(encode(4, 4, &[0; 12]).is_err());
    }
}
