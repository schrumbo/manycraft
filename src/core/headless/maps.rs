//! Map pictures the server sent: map id to 128 by 128 packed colours, patched by the rectangles of later updates.
//! The server sends a map's full picture once, so pictures stay until [`MAX_KEPT`] are held; then the oldest one nobody carries or
//! frames goes first.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, ensure};

use super::v777::{MAP_COLORS, play};
use super::wire::In;

/// The side of a map picture in pixels.
pub const SIDE: usize = 128;
const PIXELS: usize = SIDE * SIDE;
const MAX_KEPT: usize = 256;

struct Picture {
    id: i32,
    colors: Arc<[u8; PIXELS]>,
    touched: Instant,
}

/// One connection's map pictures.
#[derive(Default)]
pub struct Maps {
    pictures: Vec<Picture>,
}

impl Maps {
    /// Whether `id` is the packet this reads.
    pub fn reads(id: i32) -> bool {
        id == play::clientbound::MAP_ITEM_DATA
    }

    /// Takes in a `map_item_data` packet and returns the id it patched, or none when it carried no colours. `keep` says which
    /// ids are in hand or in a frame; those go last when room is needed. A packet it cannot read leaves every picture as it was.
    pub fn read(&mut self, r: &mut In, keep: impl Fn(i32) -> bool) -> Result<Option<i32>> {
        let id = r.varint()?;
        let _scale = r.u8()?;
        let _locked = r.bool()?;
        if r.bool()? {
            for _ in 0..r.varint()? {
                let _kind = r.varint()?;
                r.take(3)?;
                let _name = r.optional_nbt()?;
            }
        }
        let width = usize::from(r.u8()?);
        if width == 0 {
            return Ok(None);
        }
        let height = usize::from(r.u8()?);
        let (x0, y0) = (usize::from(r.u8()?), usize::from(r.u8()?));
        let colors = r.bytes()?;
        ensure!(x0 + width <= SIDE && y0 + height <= SIDE && colors.len() == width * height, "map patch out of bounds");
        let at = match self.pictures.iter().position(|p| p.id == id) {
            Some(at) => at,
            None => {
                if self.pictures.len() >= MAX_KEPT {
                    let oldest = self.pictures.iter().enumerate().min_by_key(|(_, p)| (keep(p.id), p.touched)).map(|(i, _)| i);
                    if let Some(oldest) = oldest {
                        self.pictures.swap_remove(oldest);
                    }
                }
                self.pictures.push(Picture { id, colors: Arc::new([0; PIXELS]), touched: Instant::now() });
                self.pictures.len() - 1
            }
        };
        let picture = &mut self.pictures[at];
        picture.touched = Instant::now();
        let pixels = Arc::make_mut(&mut picture.colors);
        for y in 0..height {
            let row = (y0 + y) * SIDE + x0;
            pixels[row..row + width].copy_from_slice(&colors[y * width..(y + 1) * width]);
        }
        Ok(Some(id))
    }

    /// The picture of map `id`, when the server sent any of it.
    pub fn get(&self, id: i32) -> Option<&Arc<[u8; PIXELS]>> {
        self.pictures.iter().find(|p| p.id == id).map(|p| &p.colors)
    }

    /// Forgets every picture, for a new connection.
    pub fn reset(&mut self) {
        self.pictures = Vec::new();
    }

    /// How many pictures are kept.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.pictures.len()
    }
}

/// The picture of map colour ids `colors` as RGBA rows, top-down.
pub fn rgba(colors: &[u8]) -> Vec<u8> {
    colors.iter().flat_map(|c| MAP_COLORS[usize::from(*c)]).collect()
}

#[cfg(test)]
mod tests {
    use super::super::wire::Out;
    use super::*;

    fn packet(id: i32, x: u8, y: u8, width: u8, height: u8, colors: &[u8]) -> Vec<u8> {
        let mut o = Out::default();
        o.varint(id).u8(0).bool(false).bool(false).u8(width).u8(height).u8(x).u8(y).varint(i32::try_from(colors.len()).unwrap()).raw(colors);
        o.0
    }

    fn feed(m: &mut Maps, bytes: &[u8], keep: impl Fn(i32) -> bool) -> Result<Option<i32>> {
        m.read(&mut In::new(bytes), keep)
    }

    #[test]
    fn map_colours_come_from_the_game() {
        assert_eq!(MAP_COLORS[50], [0x40, 0x40, 0xFF, 0xFF]);
        assert!(MAP_COLORS[..4].iter().all(|c| c[3] == 0));
        assert_eq!(rgba(&[50, 0]), vec![0x40, 0x40, 0xFF, 0xFF, 0, 0, 0, 0]);
    }

    #[test]
    fn a_full_picture_and_later_patches_land_in_their_rectangles() {
        let mut m = Maps::default();
        let full: Vec<u8> = (0..PIXELS).map(|i| (i % 200) as u8 + 4).collect();
        assert_eq!(feed(&mut m, &packet(7, 0, 0, 128, 128, &full), |_| true).unwrap(), Some(7));
        assert_eq!(m.get(7).unwrap()[..], full[..]);
        feed(&mut m, &packet(7, 3, 4, 2, 2, &[1, 2, 3, 4]), |_| true).unwrap();
        let p = m.get(7).unwrap();
        assert_eq!((p[4 * SIDE + 3], p[4 * SIDE + 4], p[5 * SIDE + 3], p[5 * SIDE + 4]), (1, 2, 3, 4));
        assert_eq!(p[4 * SIDE + 5], full[4 * SIDE + 5]);
        assert_eq!(p[3 * SIDE + 3], full[3 * SIDE + 3]);
    }

    #[test]
    fn a_patch_outside_the_picture_or_of_the_wrong_size_is_refused_and_changes_nothing() {
        let mut m = Maps::default();
        feed(&mut m, &packet(1, 0, 0, 1, 1, &[50]), |_| true).unwrap();
        assert!(feed(&mut m, &packet(1, 127, 0, 2, 1, &[9, 9]), |_| true).is_err());
        assert!(feed(&mut m, &packet(1, 0, 0, 2, 2, &[9]), |_| true).is_err());
        assert_eq!(m.get(1).unwrap()[0], 50);
        assert!(feed(&mut m, &packet(2, 127, 127, 2, 2, &[9; 4]), |_| true).is_err());
        assert!(m.get(2).is_none());
    }

    #[test]
    fn decorations_are_skipped_and_a_packet_without_colours_stores_nothing() {
        let mut m = Maps::default();
        let mut o = Out::default();
        o.varint(3).u8(2).bool(true).bool(true).varint(1).varint(1).raw(&[10, 20, 0]).bool(false).u8(0);
        assert_eq!(feed(&mut m, &o.0, |_| true).unwrap(), None);
        assert!(m.get(3).is_none());
        let mut o = Out::default();
        o.varint(3).u8(0).bool(false).bool(true).varint(1).varint(2).raw(&[10, 20, 0]).bool(false).u8(1).u8(1).u8(5).u8(6).varint(1).raw(&[77]);
        assert_eq!(feed(&mut m, &o.0, |_| true).unwrap(), Some(3));
        assert_eq!(m.get(3).unwrap()[6 * SIDE + 5], 77);
    }

    #[test]
    fn a_picture_a_snapshot_holds_keeps_its_colours_while_the_next_patch_lands() {
        let mut m = Maps::default();
        feed(&mut m, &packet(1, 0, 0, 1, 1, &[50]), |_| true).unwrap();
        let snapshot = Arc::clone(m.get(1).unwrap());
        feed(&mut m, &packet(1, 0, 0, 1, 1, &[51]), |_| true).unwrap();
        assert_eq!((snapshot[0], m.get(1).unwrap()[0]), (50, 51));
    }

    #[test]
    fn a_still_map_stays_and_a_full_store_drops_the_oldest_one_nobody_holds() {
        let mut m = Maps::default();
        let held = |id: i32| id == 0;
        for id in 0..i32::try_from(MAX_KEPT).unwrap() {
            feed(&mut m, &packet(id, 0, 0, 1, 1, &[50]), held).unwrap();
        }
        assert_eq!(m.len(), MAX_KEPT);
        feed(&mut m, &packet(1000, 0, 0, 1, 1, &[50]), held).unwrap();
        assert_eq!(m.len(), MAX_KEPT);
        assert!(m.get(0).is_some() && m.get(1000).is_some() && m.get(1).is_none());
        std::thread::sleep(std::time::Duration::from_millis(5));
        feed(&mut m, &packet(1001, 0, 0, 1, 1, &[50]), |_| true).unwrap();
        assert_eq!(m.len(), MAX_KEPT);
    }
}
