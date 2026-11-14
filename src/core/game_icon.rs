//! The icon of each game window: the face of the account's own skin, or a square in the account's colour with the
//! first letter of its name.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use image::{ImageFormat, Rgba, RgbaImage};

use super::account::Account;
use super::{net, skins};

const GRID: u32 = 16;
const SCALE: u32 = 4;
const GLYPH_SCALE: u32 = 2;
const DARK: [u8; 3] = [0x1c, 0x1c, 0x1e];
const LIGHT: [u8; 3] = [0xff, 0xff, 0xff];

const GLYPHS: [(u8, [&str; 7]); 37] = [
    (b'A', [".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"]),
    (b'B', ["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."]),
    (b'C', [".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."]),
    (b'D', ["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."]),
    (b'E', ["#####", "#....", "#....", "####.", "#....", "#....", "#####"]),
    (b'F', ["#####", "#....", "#....", "####.", "#....", "#....", "#...."]),
    (b'G', [".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".####"]),
    (b'H', ["#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"]),
    (b'I', [".###.", "..#..", "..#..", "..#..", "..#..", "..#..", ".###."]),
    (b'J', ["..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##.."]),
    (b'K', ["#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"]),
    (b'L', ["#....", "#....", "#....", "#....", "#....", "#....", "#####"]),
    (b'M', ["#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#"]),
    (b'N', ["#...#", "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#"]),
    (b'O', [".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."]),
    (b'P', ["####.", "#...#", "#...#", "####.", "#....", "#....", "#...."]),
    (b'Q', [".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#"]),
    (b'R', ["####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"]),
    (b'S', [".####", "#....", "#....", ".###.", "....#", "....#", "####."]),
    (b'T', ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."]),
    (b'U', ["#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."]),
    (b'V', ["#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.."]),
    (b'W', ["#...#", "#...#", "#...#", "#.#.#", "#.#.#", "#.#.#", ".#.#."]),
    (b'X', ["#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"]),
    (b'Y', ["#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.."]),
    (b'Z', ["#####", "....#", "...#.", "..#..", ".#...", "#....", "#####"]),
    (b'0', [".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."]),
    (b'1', ["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."]),
    (b'2', [".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"]),
    (b'3', ["#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###."]),
    (b'4', ["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."]),
    (b'5', ["#####", "#....", "####.", "....#", "....#", "#...#", ".###."]),
    (b'6', ["..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."]),
    (b'7', ["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."]),
    (b'8', [".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."]),
    (b'9', [".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."]),
    (b'_', [".....", ".....", ".....", ".....", ".....", ".....", "#####"]),
];

/// Writes the window icon of `account` as a PNG to `path`, leaving an identical file untouched.
pub fn write(account: &Account, path: &Path) -> Result<()> {
    let bytes = match skins::own_head(account).and_then(|head| std::fs::read(head).ok()) {
        Some(face) => face,
        None => png(&letter(account))?,
    };
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    net::write_atomic(path, &bytes)
}

fn letter(account: &Account) -> RgbaImage {
    let rgb = account.rgb();
    let [r, g, b] = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
    let ink = if u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 150_000 { DARK } else { LIGHT };
    let first = account.name.bytes().next().unwrap_or(b'_').to_ascii_uppercase();
    let rows = GLYPHS.iter().find(|(c, _)| *c == first).map_or(GLYPHS[GLYPHS.len() - 1].1, |(_, rows)| *rows);
    let (left, top) = ((GRID - 5 * GLYPH_SCALE).div_ceil(2), (GRID - 7 * GLYPH_SCALE) / 2);
    RgbaImage::from_fn(GRID * SCALE, GRID * SCALE, |x, y| {
        let (gx, gy) = (x / SCALE, y / SCALE);
        let lit = gx >= left && gy >= top && rows.get(((gy - top) / GLYPH_SCALE) as usize).and_then(|row| row.as_bytes().get(((gx - left) / GLYPH_SCALE) as usize)) == Some(&b'#');
        let [r, g, b] = if lit { ink } else { [r, g, b] };
        Rgba([r, g, b, 255])
    })
}

fn png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).context("cannot encode the window icon")?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_is_five_by_seven() {
        for (c, rows) in GLYPHS {
            assert!(rows.iter().all(|r| r.len() == 5 && r.bytes().all(|b| b == b'.' || b == b'#')), "{}", c as char);
        }
    }

    #[test]
    fn offline_accounts_get_their_letter_on_their_colour() {
        let account = Account::offline("lisa").unwrap();
        let icon = letter(&account);
        assert_eq!((icon.width(), icon.height()), (64, 64));
        let rgb = account.rgb();
        assert_eq!(icon.get_pixel(0, 0).0, [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255]);
        let lit = |gx: u32, gy: u32| icon.get_pixel(gx * SCALE, gy * SCALE).0 != icon.get_pixel(0, 0).0;
        assert!(lit(3, 1) && lit(3, 13) && lit(12, 13), "L has its stem and foot");
        assert!(!lit(12, 1), "L has no top bar");
    }

    #[test]
    fn written_once_and_left_alone_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.png");
        let account = Account::offline("Bob").unwrap();
        write(&account, &path).unwrap();
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();
        let decoded = image::open(&path).unwrap().to_rgba8();
        assert_eq!(decoded, letter(&account));
        write(&account, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first);
    }
}
