//! What a connection-only account has seen of the world: the block data of the chunks the server sent, kept deflated in
//! memory, the blocks changed since, and the dimension it is in, so a picture can be drawn on request.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, ensure};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use serde_json::Value;

use super::v777::{BLOCK_LOOKS, BLOCK_STATES};
use super::wire::In;

const MAX_CHUNKS: usize = 1024;
const MAX_BYTES: usize = 512 << 10;
const MAX_CHANGES: usize = 2048;
const MAX_TYPES: usize = 256;
/// The most sections a dimension may stack: 4064 blocks, the game's own limit.
pub const MAX_SECTIONS: usize = 254;
const MAX_INFLATED: usize = 4 << 20;
const BLOCKS: usize = 4096;
const BIOMES: usize = 64;

/// How a block looks in a picture: 0 is air; otherwise its map colour in the low six bits and its [`Class`] above them.
pub type Look = u8;

/// How a visible block is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Solid,
    Fluid,
    Plant,
}

/// The class of a visible `look`.
pub fn class(look: Look) -> Class {
    match look >> 6 {
        1 => Class::Fluid,
        2 => Class::Plant,
        _ => Class::Solid,
    }
}

/// The map colour of `look`, as a base colour id.
pub fn color(look: Look) -> u8 {
    look & 0x3F
}

static LOOKS: LazyLock<Box<[Look]>> = LazyLock::new(|| {
    let mut looks = vec![0; BLOCK_STATES].into_boxed_slice();
    for (i, &(first, look)) in BLOCK_LOOKS.iter().enumerate() {
        let end = BLOCK_LOOKS.get(i + 1).map_or(BLOCK_STATES, |next| usize::from(next.0));
        looks[usize::from(first)..end].fill(look);
    }
    looks
});

/// How block state `state` looks; states the table does not know are air.
pub fn look(state: u32) -> Look {
    LOOKS.get(state as usize).copied().unwrap_or(0)
}

/// The height range of a dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dimension {
    pub floor: i32,
    pub height: i32,
}

impl Dimension {
    fn sane(&self) -> bool {
        let span = (MAX_SECTIONS * 16) as i32;
        self.height > 0 && self.height <= span && self.height % 16 == 0 && self.floor % 16 == 0 && self.floor >= -span / 2 && self.floor + self.height <= span / 2
    }
}

impl Default for Dimension {
    fn default() -> Self {
        Self { floor: -64, height: 384 }
    }
}

/// The world one account keeps.
#[derive(Default)]
pub struct World {
    chunks: HashMap<(i32, i32), Arc<[u8]>>,
    kept: usize,
    changes: HashMap<[i32; 3], u32>,
    types: Vec<Dimension>,
    dimension: Dimension,
}

/// One kept chunk: its position and its deflated block data.
pub type Kept = ((i32, i32), Arc<[u8]>);

/// Everything a picture needs from a [`World`], cheap to take and free to send to the drawing thread.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub dimension: Dimension,
    pub chunks: Vec<Kept>,
    pub changes: Vec<([i32; 3], u32)>,
}

impl World {
    /// Reads the height ranges of the dimension types from the server's `minecraft:dimension_type` registry, whose name is already read.
    /// Ranges the game itself would not accept read as the overworld's; types past the first 256 are not kept.
    pub fn dimension_types(&mut self, r: &mut In) -> Result<()> {
        self.types.clear();
        for _ in 0..r.list()? {
            let _name = r.string()?;
            let data = r.optional_nbt()?;
            let field = |key: &str| data.as_ref().and_then(|d| d.get(key)).and_then(Value::as_i64).and_then(|v| i32::try_from(v).ok());
            let found = field("min_y").zip(field("height")).map(|(floor, height)| Dimension { floor, height }).filter(Dimension::sane);
            if self.types.len() < MAX_TYPES {
                self.types.push(found.unwrap_or_default());
            }
        }
        Ok(())
    }

    /// The account entered the dimension of type `id` (on login or respawn): what it saw before is gone.
    pub fn enter(&mut self, id: i32) {
        self.clear();
        self.dimension = usize::try_from(id).ok().and_then(|i| self.types.get(i)).copied().unwrap_or_default();
    }

    /// Forgets every chunk and change.
    pub fn clear(&mut self) {
        self.chunks = HashMap::new();
        self.kept = 0;
        self.changes = HashMap::new();
    }

    /// Keeps the block data of a `level_chunk_with_light` packet; past the memory budget the chunks farthest from chunk
    /// `near`, where the player stands, go first.
    pub fn chunk(&mut self, r: &mut In, near: (i32, i32)) -> Result<()> {
        let at = (r.i32()?, r.i32()?);
        for _ in 0..r.list()? {
            r.varint()?;
            let longs = r.list()?;
            r.take(longs.checked_mul(8).context("heightmap too long")?)?;
        }
        let data = r.bytes()?;
        ensure!(data.len() <= MAX_INFLATED, "chunk data of {} bytes", data.len());
        let mut packed = ZlibEncoder::new(Vec::new(), Compression::fast());
        packed.write_all(data)?;
        let packed: Arc<[u8]> = packed.finish()?.into();
        self.kept += packed.len();
        if let Some(old) = self.chunks.insert(at, packed) {
            self.kept -= old.len();
        }
        self.changes.retain(|p, _| (p[0] >> 4, p[2] >> 4) != at);
        while self.kept > MAX_BYTES || self.chunks.len() > MAX_CHUNKS {
            let far = |c: &(i32, i32)| c.0.abs_diff(near.0).max(c.1.abs_diff(near.1));
            let Some(&gone) = self.chunks.keys().max_by_key(|c| far(c)) else { break };
            self.drop_chunk(gone);
        }
        Ok(())
    }

    fn drop_chunk(&mut self, at: (i32, i32)) {
        if let Some(old) = self.chunks.remove(&at) {
            self.kept -= old.len();
        }
        self.changes.retain(|p, _| (p[0] >> 4, p[2] >> 4) != at);
    }

    /// Drops the chunk of a `forget_level_chunk` packet.
    pub fn forget(&mut self, r: &mut In) -> Result<()> {
        let packed = r.i64()?;
        self.drop_chunk((packed as i32, (packed >> 32) as i32));
        Ok(())
    }

    /// Applies a `block_update` packet.
    pub fn block(&mut self, r: &mut In) -> Result<()> {
        let packed = r.i64()?;
        let at = [(packed >> 38) as i32, (packed << 52 >> 52) as i32, (packed << 26 >> 38) as i32];
        let state = r.varint()?;
        self.change(at, state as u32);
        Ok(())
    }

    /// Applies a `section_blocks_update` packet.
    pub fn section(&mut self, r: &mut In) -> Result<()> {
        let packed = r.i64()?;
        let base = [((packed >> 42) as i32) << 4, ((packed << 44 >> 44) as i32) << 4, ((packed << 22 >> 42) as i32) << 4];
        for _ in 0..r.list()? {
            let entry = r.varlong()?;
            let local = (entry & 0xFFF) as i32;
            self.change([base[0] + (local >> 8 & 15), base[1] + (local & 15), base[2] + (local >> 4 & 15)], (entry >> 12) as u32);
        }
        Ok(())
    }

    fn change(&mut self, at: [i32; 3], state: u32) {
        let known = self.chunks.contains_key(&(at[0] >> 4, at[2] >> 4));
        if known && (self.changes.len() < MAX_CHANGES || self.changes.contains_key(&at)) {
            self.changes.insert(at, state);
        }
    }

    /// The chunks within `radius` chunks of block `(x, z)` and their changes.
    pub fn snapshot(&self, x: f64, z: f64, radius: i32) -> Snapshot {
        let center = ((x.floor() as i32) >> 4, (z.floor() as i32) >> 4);
        let radius = radius.unsigned_abs();
        let near = |at: &(i32, i32)| at.0.abs_diff(center.0) <= radius && at.1.abs_diff(center.1) <= radius;
        Snapshot {
            dimension: self.dimension,
            chunks: self.chunks.iter().filter(|(at, _)| near(at)).map(|(at, data)| (*at, Arc::clone(data))).collect(),
            changes: self.changes.iter().filter(|(p, _)| near(&(p[0] >> 4, p[2] >> 4))).map(|(p, s)| (*p, *s)).collect(),
        }
    }

    /// Bytes held on the heap, roughly.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        let chunks: usize = self.chunks.values().map(|c| c.len() + 48).sum();
        chunks + self.changes.capacity() * 24 + self.types.capacity() * 8
    }
}

/// The looks of one 16³ section, `y * 256 + z * 16 + x`.
pub type Section = Box<[Look; BLOCKS]>;

/// The looks of the sections of one chunk's deflated block data, bottom up; `None` for a section of air only.
pub fn sections(packed: &[u8], dimension: Dimension) -> Result<Vec<Option<Section>>> {
    let mut data = Vec::new();
    ZlibDecoder::new(packed).take(MAX_INFLATED as u64 + 1).read_to_end(&mut data)?;
    ensure!(data.len() <= MAX_INFLATED, "chunk data inflates past {MAX_INFLATED} bytes");
    let count = usize::try_from(dimension.height / 16).unwrap_or(0).min(MAX_SECTIONS);
    let mut r = In::new(&data);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if r.left() == 0 {
            break;
        }
        let _blocks = r.i16()?;
        let _fluids = r.i16()?;
        out.push(container(&mut r, BLOCKS, 4, 8)?);
        container(&mut r, BIOMES, 1, 3)?;
    }
    Ok(out)
}

fn container(r: &mut In, entries: usize, least: u8, most_indirect: u8) -> Result<Option<Section>> {
    let bits = r.u8()?;
    if bits == 0 {
        let single = look(r.varint()? as u32);
        return Ok((entries == BLOCKS && single != 0).then(|| Box::new([single; BLOCKS])));
    }
    let palette: Option<Vec<Look>> = if bits <= most_indirect {
        let n = r.list()?;
        let mut palette = Vec::with_capacity(n);
        for _ in 0..n {
            palette.push(look(r.varint()? as u32));
        }
        Some(palette)
    } else {
        None
    };
    let bits = if palette.is_some() { bits.max(least) } else { bits };
    ensure!(bits <= 32, "{bits} bits per entry");
    let per = usize::from(64 / bits);
    let longs = r.take(entries.div_ceil(per) * 8)?;
    if entries != BLOCKS {
        return Ok(None);
    }
    let mask = (1u64 << bits) - 1;
    let mut section = Box::new([0; BLOCKS]);
    let mut any = false;
    for (i, slot) in section.iter_mut().enumerate() {
        let word = u64::from_be_bytes(longs[i / per * 8..i / per * 8 + 8].try_into()?);
        let value = (word >> (i % per * usize::from(bits))) & mask;
        *slot = match &palette {
            Some(p) => p.get(value as usize).copied().unwrap_or(0),
            None => look(value as u32),
        };
        any |= *slot != 0;
    }
    Ok(any.then_some(section))
}

#[cfg(test)]
pub(super) mod fixture {
    use super::super::wire::Out;
    use super::BLOCK_LOOKS;

    /// The first block state that looks like `want` says.
    pub fn state(want: impl Fn(u8) -> bool) -> u32 {
        u32::from(BLOCK_LOOKS.iter().find(|(_, look)| want(*look)).expect("no such block").0)
    }

    /// A block paletted container of `bits` per entry over `palette`, entry `i` being `value(i)`.
    pub fn container(out: &mut Out, bits: u8, palette: &[u32], value: impl Fn(usize) -> u64) {
        out.u8(bits);
        out.varint(palette.len() as i32);
        for p in palette {
            out.varint(*p as i32);
        }
        let per = 64 / usize::from(bits);
        for word in 0..4096usize.div_ceil(per) {
            let mut long = 0u64;
            for k in 0..per {
                let i = word * per + k;
                if i < 4096 {
                    long |= value(i) << (k * usize::from(bits));
                }
            }
            out.i64(long as i64);
        }
    }

    /// A `level_chunk_with_light` body at `(x, z)` whose sections, bottom up, are each filled with one state.
    pub fn chunk(x: i32, z: i32, sections: &[u32]) -> Vec<u8> {
        let mut data = Out::default();
        for s in sections {
            data.i16(if *s == 0 { 0 } else { 4096 }).i16(0).u8(0).varint(*s as i32).u8(0).varint(0);
        }
        let mut out = Out::default();
        out.i32(x).i32(z).varint(1).varint(1).varint(2).i64(7).i64(7).bytes(&data.0);
        out.0
    }
}

#[cfg(test)]
mod tests {
    use super::super::wire::{In, Out};
    use super::fixture::{chunk, container, state};
    use super::*;

    fn solid(look: Look) -> bool {
        look != 0 && class(look) == Class::Solid
    }

    fn fluid(look: Look) -> bool {
        look != 0 && class(look) == Class::Fluid
    }

    fn packed(bytes: &[u8]) -> Vec<u8> {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(bytes).unwrap();
        z.finish().unwrap()
    }

    #[test]
    fn every_block_state_has_a_look_and_air_is_air() {
        assert_eq!(BLOCK_LOOKS[0], (0, 0), "state 0 is air");
        assert!(BLOCK_LOOKS.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 != w[1].1), "runs rise and differ");
        assert!(usize::from(BLOCK_LOOKS[BLOCK_LOOKS.len() - 1].0) < BLOCK_STATES);
        assert!(solid(look(1)), "state 1 is stone");
        assert_eq!(look(u32::MAX), 0, "unknown states are air");
        assert!(BLOCK_LOOKS.iter().any(|(_, l)| fluid(*l)) && BLOCK_LOOKS.iter().any(|(_, l)| *l != 0 && class(*l) == Class::Plant));
    }

    #[test]
    fn a_section_with_a_palette_reads_as_looks_and_biomes_are_skipped() {
        let (stone, water) = (state(solid), state(fluid));
        let mut out = Out::default();
        out.i16(100).i16(50);
        container(&mut out, 4, &[0, stone, water], |i| (i % 3) as u64);
        out.u8(1).varint(2).varint(3).varint(4).i64(0);
        out.i16(0).i16(0).u8(0).varint(0).u8(0).varint(0);
        let found = sections(&packed(&out.0), Dimension { floor: 0, height: 32 }).unwrap();
        assert_eq!(found.len(), 2);
        let first = found[0].as_ref().expect("blocks");
        assert_eq!((first[0], first[1], first[2], first[3]), (0, look(stone), look(water), 0));
        assert!(found[1].is_none(), "air only");
    }

    #[test]
    fn a_direct_section_reads_global_states() {
        let stone = state(solid);
        let mut out = Out::default();
        out.i16(4096).i16(0);
        out.u8(16);
        for _ in 0..1024 {
            out.i64((u64::from(stone) * 0x0001_0001_0001_0001) as i64);
        }
        out.u8(0).varint(0);
        let found = sections(&packed(&out.0), Dimension { floor: 0, height: 16 }).unwrap();
        assert!(found[0].as_ref().unwrap().iter().all(|l| *l == look(stone)));
    }

    #[test]
    fn a_section_claiming_more_data_than_it_has_is_an_error_not_a_panic() {
        let mut out = Out::default();
        out.i16(1).i16(0).u8(4).varint(1).varint(1).i64(0);
        assert!(sections(&packed(&out.0), Dimension::default()).is_err());
        assert!(sections(b"not deflate", Dimension::default()).is_err());
    }

    #[test]
    fn chunks_are_kept_compressed_and_changes_follow_only_kept_chunks() {
        let stone = state(solid);
        let mut w = World::default();
        let mut sections_of = vec![0; 24];
        sections_of[0] = stone;
        w.chunk(&mut In::new(&chunk(2, -3, &sections_of)), (0, 0)).unwrap();
        let mut update = Out::default();
        let (x, y, z) = (2 * 16 + 5, -60i64, -3 * 16 + 7);
        update.i64(((x as i64 & 0x3FF_FFFF) << 38) | ((z as i64 & 0x3FF_FFFF) << 12) | (y & 0xFFF)).varint(0);
        w.block(&mut In::new(&update.0)).unwrap();
        let mut far = Out::default();
        far.i64((100i64 << 38) | (100 << 12)).varint(stone as i32);
        w.block(&mut In::new(&far.0)).unwrap();
        let snap = w.snapshot(f64::from(x), f64::from(z), 1);
        assert_eq!(snap.chunks.len(), 1);
        assert_eq!(snap.changes, vec![([x, y as i32, z], 0)], "a change outside kept chunks is not kept");
        let found = sections(&snap.chunks[0].1, w.dimension).unwrap();
        assert_eq!(found.len(), 24);
        assert!(found[0].is_some() && found[1..].iter().all(Option::is_none));
        assert!(w.bytes() < 1024, "{} bytes for a chunk of two sections", w.bytes());
        let mut forget = Out::default();
        forget.i64((-3i64 << 32) | 2);
        w.forget(&mut In::new(&forget.0)).unwrap();
        assert!(w.snapshot(f64::from(x), f64::from(z), 1).chunks.is_empty());
        assert!(w.snapshot(f64::from(x), f64::from(z), 1).changes.is_empty());
    }

    #[test]
    fn a_section_update_changes_each_block_it_names() {
        let stone = state(solid);
        let mut w = World::default();
        w.chunk(&mut In::new(&chunk(0, 0, &[0; 24])), (0, 0)).unwrap();
        let mut out = Out::default();
        out.i64(1).varint(2);
        for (local, s) in [(0x123u64, stone), (0xFFF, 0)] {
            let entry = (u64::from(s) << 12) | local;
            let mut v = entry;
            loop {
                let b = (v & 0x7F) as u8;
                v >>= 7;
                if v == 0 {
                    out.u8(b);
                    break;
                }
                out.u8(b | 0x80);
            }
        }
        w.section(&mut In::new(&out.0)).unwrap();
        let mut changes = w.snapshot(0.0, 0.0, 0).changes;
        changes.sort();
        assert_eq!(changes, vec![([1, 19, 2], stone), ([15, 31, 15], 0)]);
    }

    #[test]
    fn the_dimension_comes_from_the_registry_and_entering_one_forgets_the_last() {
        let mut w = World::default();
        let mut reg = Out::default();
        reg.varint(2).string("minecraft:overworld").bool(false).string("minecraft:the_nether").bool(false);
        w.dimension_types(&mut In::new(&reg.0)).unwrap();
        w.chunk(&mut In::new(&chunk(0, 0, &[0; 24])), (0, 0)).unwrap();
        w.enter(1);
        assert_eq!(w.dimension, Dimension::default(), "a type without data keeps the overworld's range");
        assert!(w.snapshot(0.0, 0.0, 4).chunks.is_empty());
        w.enter(99);
        assert_eq!(w.dimension, Dimension::default());
    }

    #[test]
    fn a_world_never_keeps_more_changes_than_its_cap() {
        let mut w = World::default();
        w.chunk(&mut In::new(&chunk(0, 0, &[0; 24])), (0, 0)).unwrap();
        for i in 0..(MAX_CHANGES as i32 + 100) {
            w.change([i & 15, i >> 8, (i >> 4) & 15], 1);
        }
        assert_eq!(w.changes.len(), MAX_CHANGES);
    }

    #[test]
    fn past_its_budget_a_world_lets_go_of_the_chunks_farthest_from_the_player() {
        let mut w = World::default();
        let mut seed = 0x9E37_79B9_u32;
        let mut noise = || {
            (0..64 << 10).map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            }).collect::<Vec<u8>>()
        };
        for x in 0..12 {
            let mut packet = Out::default();
            packet.i32(x).i32(0).varint(0).bytes(&noise());
            w.chunk(&mut In::new(&packet.0), (0, 0)).unwrap();
        }
        assert!(w.kept <= MAX_BYTES, "{} bytes kept", w.kept);
        assert!(w.chunks.contains_key(&(0, 0)) && !w.chunks.contains_key(&(11, 0)), "the nearest stay, the farthest go");
        let mut huge = Out::default();
        huge.i32(0).i32(1).varint(0).bytes(&vec![0; MAX_INFLATED + 1]);
        assert!(w.chunk(&mut In::new(&huge.0), (0, 0)).is_err());
    }

    #[test]
    fn only_a_height_range_the_game_accepts_counts() {
        assert!(Dimension::default().sane());
        assert!(Dimension { floor: 0, height: 256 }.sane());
        for bad in [(0, 0), (0, 2_000_000_000), (-64, 380), (-2_000_000_000, 384), (2016, 32), (-8, 384)] {
            assert!(!Dimension { floor: bad.0, height: bad.1 }.sane(), "{bad:?}");
        }
    }
}