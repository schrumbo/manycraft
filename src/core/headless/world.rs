//! What a connection-only account has seen of the world: the block data of the chunks the server sent, kept deflated in
//! memory, the blocks changed since, and the dimension it is in, so a picture can be drawn on request.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, ensure};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use serde_json::Value;

use super::render::{Biome, Sky};
use super::v777::{BLOCK_LOOKS, BLOCK_STATES};
use super::wire::{In, Out};

const MAX_CHUNKS: usize = 1024;
const MAX_BYTES: usize = 512 << 10;
const MAX_CHANGES: usize = 2048;
const MAX_TYPES: usize = 256;
const MAX_BIOMES: usize = 4096;
const MAX_RELIT: usize = 64 << 10;
const MAX_CLOCKS: usize = 64;
const DAY: i64 = 24000;
const LIGHT: usize = BLOCKS / 2;
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
    /// How many sections a chunk of this dimension has.
    pub fn sections(&self) -> usize {
        usize::try_from(self.height / 16).unwrap_or(0).min(MAX_SECTIONS)
    }

    fn sane(&self) -> bool {
        let span = (MAX_SECTIONS * 16) as i32;
        self.height > 0 && self.height <= span && self.height % 16 == 0 && self.floor % 16 == 0 && self.floor >= -span / 2 && self.floor <= span / 2 - self.height
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
    biomes: Arc<[Biome]>,
    relit: HashMap<(i32, i32), Relit>,
    clocks: Vec<String>,
    sky: Sky,
}

/// Light a `light_update` sent for some sections of a kept chunk after the chunk itself, by section counted from one
/// below the floor; the newest wins.
#[derive(Clone, Debug, Default)]
pub struct Relit {
    pub sky: BTreeMap<usize, Light>,
    pub block: BTreeMap<usize, Light>,
}

impl Relit {
    /// Lays this light over the sky and block light of a chunk's sections, bottom up.
    pub fn apply<'a>(&self, sections: impl IntoIterator<Item = (&'a mut Light, &'a mut Light)>) {
        for (i, (sky, block)) in sections.into_iter().enumerate() {
            if let Some(light) = self.sky.get(&(i + 1)) {
                *sky = light.clone();
            }
            if let Some(light) = self.block.get(&(i + 1)) {
                *block = light.clone();
            }
        }
    }

    fn bytes(&self) -> usize {
        self.sky.values().chain(self.block.values()).map(|l| if matches!(l, Light::Levels(_)) { LIGHT + 32 } else { 32 }).sum()
    }
}

/// One kept chunk: its position and its deflated block data.
pub type Kept = ((i32, i32), Arc<[u8]>);

/// Everything a picture needs from a [`World`], cheap to take and free to send to the drawing thread.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub dimension: Dimension,
    pub chunks: Vec<Kept>,
    pub changes: Vec<([i32; 3], u32)>,
    pub biomes: Arc<[Biome]>,
    pub relit: Vec<((i32, i32), Relit)>,
    pub sky: Sky,
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

    /// Reads the server's `minecraft:worldgen/biome` registry, whose name is already read, in registry order; an entry
    /// without data reads as plains, entries past the first 4096 are not kept.
    pub fn biomes(&mut self, r: &mut In) -> Result<()> {
        let mut biomes = Vec::new();
        for _ in 0..r.list()? {
            let _name = r.string()?;
            let data = r.optional_nbt()?;
            if biomes.len() < MAX_BIOMES {
                biomes.push(data.as_ref().map(Biome::from_json).unwrap_or_default());
            }
        }
        self.biomes = biomes.into();
        Ok(())
    }

    /// Reads the server's `minecraft:world_clock` registry, whose name is already read: the clock names in registry order.
    pub fn clocks(&mut self, r: &mut In) -> Result<()> {
        self.clocks.clear();
        for _ in 0..r.list()? {
            let name = r.string()?;
            r.optional_nbt()?;
            if self.clocks.len() < MAX_CLOCKS {
                self.clocks.push(name);
            }
        }
        Ok(())
    }

    /// Reads `set_time`: the time of day is the overworld clock's, or another clock's when it sends no overworld clock.
    pub fn time(&mut self, r: &mut In) -> Result<()> {
        let _age = r.i64()?;
        let mut day = None;
        for _ in 0..r.list()? {
            let clock = r.varint()?;
            let ticks = r.varlong()?;
            r.f32()?;
            r.f32()?;
            let overworld = usize::try_from(clock).ok().and_then(|i| self.clocks.get(i)).is_some_and(|n| n.trim_start_matches("minecraft:") == "overworld");
            if day.is_none() || overworld {
                day = Some(ticks.rem_euclid(DAY));
            }
        }
        if day.is_some() {
            self.sky.time = day;
        }
        Ok(())
    }

    /// Takes in game event `event` with `value` as the game does for the weather: rain starting from nothing, stopping
    /// from full, and rain and thunder strength.
    pub fn weather(&mut self, event: u8, value: f32) {
        match event {
            1 => self.sky.rain = 0.0,
            2 => self.sky.rain = 1.0,
            7 => self.sky.rain = value.clamp(0.0, 1.0),
            8 => self.sky.thunder = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    /// The account entered the dimension of type `id` (on login or respawn): what it saw before is gone.
    pub fn enter(&mut self, id: i32) {
        self.clear();
        self.dimension = usize::try_from(id).ok().and_then(|i| self.types.get(i)).copied().unwrap_or_default();
    }

    /// Forgets every chunk and change, and the weather.
    pub fn clear(&mut self) {
        self.chunks = HashMap::new();
        self.kept = 0;
        self.changes = HashMap::new();
        self.relit = HashMap::new();
        (self.sky.rain, self.sky.thunder) = (0.0, 0.0);
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
        let light = match block_entities(r) {
            Ok(()) => r.take(r.left()).unwrap_or_default(),
            Err(_) => &[],
        };
        let light = if light.len() <= most_light(self.dimension.sections()) { light } else { &[] };
        let packed = pack(data, light)?;
        self.kept += packed.len();
        if let Some(old) = self.chunks.insert(at, packed) {
            self.kept -= old.len();
        }
        self.changes.retain(|p, _| (p[0] >> 4, p[2] >> 4) != at);
        self.relit.remove(&at);
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
        self.relit.remove(&at);
    }

    /// Applies a `light_update` packet to its kept chunk: the sections it names get the light sent, the rest keep theirs.
    /// Past the budget for such light, the chunk holding most of it takes it into its kept data.
    pub fn light(&mut self, r: &mut In) -> Result<()> {
        let at = (r.varint()?, r.varint()?);
        if !self.chunks.contains_key(&at) {
            return Ok(());
        }
        let (sky, block) = sent(r, self.dimension.sections())?;
        let relit = self.relit.entry(at).or_default();
        relit.sky.extend(sky);
        relit.block.extend(block);
        while self.relit.values().map(Relit::bytes).sum::<usize>() > MAX_RELIT {
            let Some(most) = self.relit.iter().max_by_key(|(_, l)| l.bytes()).map(|(at, _)| *at) else { break };
            self.fold(most)?;
        }
        Ok(())
    }

    fn fold(&mut self, at: (i32, i32)) -> Result<()> {
        let (Some(relit), Some(packed)) = (self.relit.remove(&at), self.chunks.get(&at)) else { return Ok(()) };
        let kept = inflate(packed)?;
        let (data, light) = split(&kept)?;
        let count = self.dimension.sections();
        let (mut sky, mut block) = lights(light, count).unwrap_or_else(|_| (unknown(count), unknown(count)));
        relit.apply(sky.iter_mut().zip(block.iter_mut()));
        let packed = pack(data, &region(&sky, &block))?;
        self.kept += packed.len();
        if let Some(old) = self.chunks.insert(at, packed) {
            self.kept -= old.len();
        }
        Ok(())
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

    /// Whether every chunk within `radius` chunks of block `(x, z)` arrived.
    pub fn around(&self, x: f64, z: f64, radius: i32) -> bool {
        let center = ((x.floor() as i32) >> 4, (z.floor() as i32) >> 4);
        (-radius..=radius).all(|dx| (-radius..=radius).all(|dz| self.chunks.contains_key(&(center.0 + dx, center.1 + dz))))
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
            biomes: Arc::clone(&self.biomes),
            relit: self.relit.iter().filter(|(at, _)| near(at)).map(|(at, l)| (*at, l.clone())).collect(),
            sky: self.sky,
        }
    }

    /// Bytes held on the heap, roughly.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        let chunks: usize = self.chunks.values().map(|c| c.len() + 48).sum();
        chunks + self.changes.capacity() * 24 + self.types.capacity() * 8 + self.relit.values().map(Relit::bytes).sum::<usize>()
    }
}

fn pack(data: &[u8], light: &[u8]) -> Result<Arc<[u8]>> {
    ensure!(data.len() + light.len() <= MAX_INFLATED - 4, "chunk data of {} bytes", data.len() + light.len());
    let mut packed = ZlibEncoder::new(Vec::new(), Compression::default());
    packed.write_all(&(data.len() as u32).to_le_bytes())?;
    packed.write_all(data)?;
    packed.write_all(light)?;
    Ok(packed.finish()?.into())
}

fn most_light(count: usize) -> usize {
    let masks = 4 * (5 + 8 * (count + 2).div_ceil(64));
    masks + 2 * (5 + (count + 2) * (3 + LIGHT))
}

/// The looks of one 16³ section, `y * 256 + z * 16 + x`.
pub type Section = Box<[Look; BLOCKS]>;

fn block_entities(r: &mut In) -> Result<()> {
    for _ in 0..r.list()? {
        r.u8()?;
        r.i16()?;
        r.varint()?;
        r.nbt()?;
    }
    Ok(())
}

/// The looks of the sections of one chunk's deflated block data, bottom up; `None` for a section of air only.
pub fn sections(packed: &[u8], dimension: Dimension) -> Result<Vec<Option<Section>>> {
    let kept = inflate(packed)?;
    let (data, _) = split(&kept)?;
    Ok(columns(data, dimension)?.into_iter().map(|(blocks, _)| looks(blocks)).collect())
}

/// The light of one section as the server sent it.
#[derive(Clone, Debug)]
pub enum Light {
    /// Nothing sent for this section.
    Unknown,
    /// Sent as all dark.
    Dark,
    /// Two levels a byte, `y * 256 + z * 16 + x`, the even index in the low half.
    Levels(Box<[u8; LIGHT]>),
}

impl Light {
    /// The level (0..15) of block `i` of the section, or `None` when unknown.
    pub fn level(&self, i: usize) -> Option<u8> {
        match self {
            Light::Unknown => None,
            Light::Dark => Some(0),
            Light::Levels(l) => Some(l[i / 2] >> (i % 2 * 4) & 15),
        }
    }
}

/// What a picture needs of one section.
pub struct Cell {
    /// No states for a section of air only.
    pub states: Option<States>,
    pub biomes: Biomes,
    pub sky: Light,
    pub block: Light,
}

/// The global block states of one 16³ section, `y * 256 + z * 16 + x`.
pub type States = Box<[u16; BLOCKS]>;

/// The biome ids of one section's 4³ cells, `y * 16 + z * 4 + x`.
pub type Biomes = [u16; BIOMES];

/// The block states, biomes and light of the sections of one kept chunk, bottom up.
pub fn cells(packed: &[u8], dimension: Dimension) -> Result<Vec<Cell>> {
    let kept = inflate(packed)?;
    let (data, light) = split(&kept)?;
    let columns = columns(data, dimension)?;
    let (mut sky, mut block) = lights(light, columns.len()).unwrap_or_default();
    sky.resize_with(columns.len(), || Light::Unknown);
    block.resize_with(columns.len(), || Light::Unknown);
    Ok(columns.into_iter().zip(sky.into_iter().zip(block)).map(|((blocks, biomes), (sky, block))| Cell { states: block_states(blocks), biomes: biome_ids(biomes), sky, block }).collect())
}

fn inflate(packed: &[u8]) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    ZlibDecoder::new(packed).take(MAX_INFLATED as u64 + 1).read_to_end(&mut data)?;
    ensure!(data.len() <= MAX_INFLATED, "chunk data inflates past {MAX_INFLATED} bytes");
    Ok(data)
}

fn split(kept: &[u8]) -> Result<(&[u8], &[u8])> {
    let (len, rest) = kept.split_first_chunk::<4>().context("kept chunk without its length")?;
    let len = u32::from_le_bytes(*len) as usize;
    ensure!(len <= rest.len(), "kept chunk shorter than its block data");
    Ok(rest.split_at(len))
}

fn lights(region: &[u8], count: usize) -> Result<(Vec<Light>, Vec<Light>)> {
    let (sky, block) = sent(&mut In::new(region), count)?;
    let place = |sent: Vec<(usize, Light)>| {
        let mut out = unknown(count);
        for (i, light) in sent {
            if let Some(slot) = i.checked_sub(1).and_then(|i| out.get_mut(i)) {
                *slot = light;
            }
        }
        out
    };
    Ok((place(sky), place(block)))
}

fn unknown(count: usize) -> Vec<Light> {
    (0..count).map(|_| Light::Unknown).collect()
}

type Sent = Vec<(usize, Light)>;

fn sent(r: &mut In, count: usize) -> Result<(Sent, Sent)> {
    let (sky, block, empty_sky, empty_block) = (mask(r)?, mask(r)?, mask(r)?, mask(r)?);
    let sky = arrays(r, &sky, &empty_sky, count)?;
    Ok((sky, arrays(r, &block, &empty_block, count)?))
}

fn region(sky: &[Light], block: &[Light]) -> Vec<u8> {
    let bits = |kept: &[Light], dark: bool| {
        let mut mask = vec![0u64; (kept.len() + 2).div_ceil(64)];
        for (i, light) in kept.iter().enumerate() {
            if matches!((light, dark), (Light::Levels(_), false) | (Light::Dark, true)) {
                mask[(i + 1) / 64] |= 1 << ((i + 1) % 64);
            }
        }
        mask
    };
    let mut out = Out::default();
    for mask in [bits(sky, false), bits(block, false), bits(sky, true), bits(block, true)] {
        let bytes: Vec<u8> = mask.iter().flat_map(|word| word.to_le_bytes()).collect();
        out.bytes(&bytes);
    }
    for kept in [sky, block] {
        let levels: Vec<&[u8; LIGHT]> = kept.iter().filter_map(|l| if let Light::Levels(l) = l { Some(&**l) } else { None }).collect();
        out.varint(levels.len() as i32);
        for l in levels {
            out.bytes(l);
        }
    }
    out.0
}

fn mask(r: &mut In) -> Result<Vec<u64>> {
    let bytes = r.bytes()?;
    let mut words = vec![0u64; bytes.len().div_ceil(8)];
    for (i, byte) in bytes.iter().enumerate() {
        words[i / 8] |= u64::from(*byte) << (i % 8 * 8);
    }
    Ok(words)
}

fn bit(mask: &[u64], i: usize) -> bool {
    mask.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
}

fn arrays(r: &mut In, has: &[u64], empty: &[u64], count: usize) -> Result<Sent> {
    let n = r.list()?;
    ensure!(n <= count + 2, "{n} light arrays for {count} sections");
    let mut read = 0;
    let mut out = Vec::new();
    for i in 0..count + 2 {
        if bit(has, i) {
            if read < n {
                read += 1;
                if let Ok(levels) = <[u8; LIGHT]>::try_from(r.bytes()?) {
                    out.push((i, Light::Levels(Box::new(levels))));
                }
            }
        } else if bit(empty, i) {
            out.push((i, Light::Dark));
        }
    }
    for _ in read..n {
        r.bytes()?;
    }
    Ok(out)
}

enum Values {
    Single(u32),
    Each(Box<[u32]>),
}

fn looks(values: Values) -> Option<Section> {
    match values {
        Values::Single(s) => (look(s) != 0).then(|| Box::new([look(s); BLOCKS])),
        Values::Each(v) => {
            let mut section = Box::new([0; BLOCKS]);
            for (slot, s) in section.iter_mut().zip(v.iter()) {
                *slot = look(*s);
            }
            section.iter().any(|l| *l != 0).then_some(section)
        }
    }
}

fn block_states(values: Values) -> Option<States> {
    let state = |s: u32| if (s as usize) < BLOCK_STATES { s as u16 } else { 0 };
    match values {
        Values::Single(s) => (state(s) != 0).then(|| Box::new([state(s); BLOCKS])),
        Values::Each(v) => {
            let mut section = Box::new([0; BLOCKS]);
            for (slot, s) in section.iter_mut().zip(v.iter()) {
                *slot = state(*s);
            }
            section.iter().any(|s| *s != 0).then_some(section)
        }
    }
}

fn biome_ids(values: Values) -> Biomes {
    let id = |b: u32| u16::try_from(b).unwrap_or(0);
    match values {
        Values::Single(b) => [id(b); BIOMES],
        Values::Each(v) => std::array::from_fn(|i| v.get(i).copied().map_or(0, id)),
    }
}

fn columns(data: &[u8], dimension: Dimension) -> Result<Vec<(Values, Values)>> {
    let count = usize::try_from(dimension.height / 16).unwrap_or(0).min(MAX_SECTIONS);
    let mut r = In::new(data);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if r.left() == 0 {
            break;
        }
        let _blocks = r.i16()?;
        let _fluids = r.i16()?;
        let blocks = container(&mut r, BLOCKS, 4, 8)?;
        out.push((blocks, container(&mut r, BIOMES, 1, 3)?));
    }
    Ok(out)
}

fn container(r: &mut In, entries: usize, least: u8, most_indirect: u8) -> Result<Values> {
    let bits = r.u8()?;
    if bits == 0 {
        return Ok(Values::Single(r.varint()? as u32));
    }
    let palette: Option<Vec<u32>> = if bits <= most_indirect {
        let n = r.list()?;
        let mut palette = Vec::with_capacity(n);
        for _ in 0..n {
            palette.push(r.varint()? as u32);
        }
        Some(palette)
    } else {
        None
    };
    let bits = if palette.is_some() { bits.max(least) } else { bits };
    ensure!(bits <= 32, "{bits} bits per entry");
    let per = usize::from(64 / bits);
    let longs = r.take(entries.div_ceil(per) * 8)?;
    let mask = (1u64 << bits) - 1;
    let mut values = vec![0u32; entries].into_boxed_slice();
    for (i, slot) in values.iter_mut().enumerate() {
        let word = u64::from_be_bytes(longs[i / per * 8..i / per * 8 + 8].try_into()?);
        let value = (word >> (i % per * usize::from(bits))) & mask;
        *slot = match &palette {
            Some(p) => p.get(value as usize).copied().unwrap_or(0),
            None => value as u32,
        };
    }
    Ok(Values::Each(values))
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
        z.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
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
    fn a_kept_chunk_gives_back_its_states_biome_cells_and_light() {
        let stone = state(solid);
        let mut data = Out::default();
        data.i16(4096).i16(0).u8(0).varint(stone as i32);
        data.u8(2).varint(4).varint(10).varint(11).varint(12).varint(13);
        for _ in 0..2 {
            data.i64((0..32).fold(0u64, |long, k| long | ((k % 4) << (k * 2))) as i64);
        }
        let mut sky = vec![0u8; 2048];
        sky[0] = 0x5F;
        let mut packet = Out::default();
        packet.i32(0).i32(0).varint(0).bytes(&data.0).varint(0);
        packet.varint(1).u8(2).varint(0).varint(0).varint(1).u8(2);
        packet.varint(1).bytes(&sky).varint(0);
        let mut w = World::default();
        w.dimension = Dimension { floor: 0, height: 16 };
        w.chunk(&mut In::new(&packet.0), (0, 0)).unwrap();
        let kept = cells(&w.snapshot(0.0, 0.0, 0).chunks[0].1, w.dimension).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].states.as_ref().unwrap()[4095], stone as u16);
        assert_eq!(kept[0].biomes[..6], [10, 11, 12, 13, 10, 11]);
        assert_eq!((kept[0].sky.level(0), kept[0].sky.level(1), kept[0].sky.level(2)), (Some(15), Some(5), Some(0)));
        assert_eq!(kept[0].block.level(7), Some(0), "an empty section reads dark");
        let bare = cells(&packed(&data.0), w.dimension).unwrap();
        assert!(bare[0].sky.level(0).is_none(), "no light sent is unknown light");
    }

    #[test]
    fn a_light_update_relights_only_the_sections_it_names_and_survives_folding() {
        let mut sky = vec![0u8; 2048];
        sky[0] = 0x5F;
        let mut air = Out::default();
        for _ in 0..2 {
            air.i16(0).i16(0).u8(0).varint(0).u8(0).varint(0);
        }
        let mut packet = Out::default();
        packet.i32(0).i32(0).varint(0).bytes(&air.0).varint(0);
        packet.varint(1).u8(2).varint(0).varint(0).varint(0);
        packet.varint(1).bytes(&sky).varint(0);
        let mut w = World::default();
        w.dimension = Dimension { floor: 0, height: 32 };
        w.chunk(&mut In::new(&packet.0), (0, 0)).unwrap();
        let mut torch = vec![0u8; 2048];
        torch[1] = 0xE0;
        let mut update = Out::default();
        update.varint(0).varint(0).varint(1).u8(4).varint(1).u8(2).varint(0).varint(0);
        update.varint(1).bytes(&sky).varint(1).bytes(&torch);
        w.light(&mut In::new(&update.0)).unwrap();
        update.0[0] = 1;
        w.light(&mut In::new(&update.0)).unwrap();
        assert_eq!(w.relit.len(), 1, "light for a chunk not kept is not kept");
        let snap = w.snapshot(0.0, 0.0, 0);
        let (mut s, mut b) = lights(&inflate(&snap.chunks[0].1).map(|k| split(&k).unwrap().1.to_vec()).unwrap(), 2).unwrap();
        snap.relit[0].1.apply(s.iter_mut().zip(b.iter_mut()));
        let levels = |s: &[Light], b: &[Light]| (s[0].level(0), s[1].level(0), b[0].level(3), b[1].level(0));
        assert_eq!(levels(&s, &b), (Some(15), Some(15), Some(14), None));
        w.fold((0, 0)).unwrap();
        assert!(w.relit.is_empty());
        let kept = cells(&w.snapshot(0.0, 0.0, 0).chunks[0].1, w.dimension).unwrap();
        let (s, b): (Vec<Light>, Vec<Light>) = kept.into_iter().map(|c| (c.sky, c.block)).unzip();
        assert_eq!(levels(&s, &b), (Some(15), Some(15), Some(14), None), "folded light reads as before");
    }

    fn nbt_int(out: &mut Vec<u8>, key: &str, value: i32) {
        out.push(3);
        out.extend((key.len() as u16).to_be_bytes());
        out.extend(key.as_bytes());
        out.extend(value.to_be_bytes());
    }

    fn registry(types: &[(&str, i32, i32)]) -> Vec<u8> {
        let mut reg = Out::default();
        reg.varint(types.len() as i32);
        for (name, floor, height) in types {
            reg.string(name).bool(true);
            let mut nbt = vec![10];
            nbt_int(&mut nbt, "min_y", *floor);
            nbt_int(&mut nbt, "height", *height);
            nbt.push(0);
            reg.raw(&nbt);
        }
        reg.0
    }

    fn light_update(x: i32, z: i32, sky_bits: &[u8], block_bits: &[u8], sky: usize, block: usize) -> Vec<u8> {
        let mut out = Out::default();
        out.varint(x).varint(z).bytes(sky_bits).bytes(block_bits).bytes(&[]).bytes(&[]);
        out.varint(sky as i32);
        for _ in 0..sky {
            out.bytes(&[0xFF; 2048]);
        }
        out.varint(block as i32);
        for _ in 0..block {
            out.bytes(&[0x11; 2048]);
        }
        out.0
    }

    #[test]
    fn light_of_the_nether_after_the_overworld_reads_in_the_nethers_height() {
        let mut w = World::default();
        let types = registry(&[("minecraft:overworld", -64, 384), ("minecraft:overworld_caves", -64, 384), ("minecraft:the_end", 0, 256), ("minecraft:the_nether", 0, 256)]);
        w.dimension_types(&mut In::new(&types)).unwrap();
        w.enter(0);
        w.chunk(&mut In::new(&chunk(0, 0, &[0; 24])), (0, 0)).unwrap();
        w.enter(3);
        assert_eq!(w.dimension, Dimension { floor: 0, height: 256 });
        w.chunk(&mut In::new(&chunk(-3, -1, &[0; 16])), (0, 0)).unwrap();
        w.light(&mut In::new(&light_update(-3, -1, &[], &[0x18], 0, 2))).unwrap();
        let relit = &w.snapshot(-40.0, -10.0, 0).relit;
        assert_eq!(relit.len(), 1);
        assert_eq!(relit[0].1.block.keys().copied().collect::<Vec<_>>(), [3, 4]);
        let mut all = light_update(-3, -1, &[], &[0xFF, 0xFF, 0x03], 0, 18);
        w.light(&mut In::new(&all)).unwrap();
        all.truncate(0);
        assert_eq!(w.snapshot(-40.0, -10.0, 0).relit[0].1.block.len(), 18);
    }

    #[test]
    fn light_with_more_arrays_than_sections_is_not_read() {
        let mut region = Out::default();
        region.varint(0).varint(0).varint(0).varint(0).varint(1000);
        region.raw(&[0; 1000]).varint(0);
        assert!(lights(&region.0, 2).is_err());
        let mut packet = Out::default();
        packet.i32(0).i32(0).varint(0).bytes(&[]).varint(0).raw(&vec![0; most_light(1) + 1]);
        let mut w = World::default();
        w.dimension = Dimension { floor: 0, height: 16 };
        w.chunk(&mut In::new(&packet.0), (0, 0)).unwrap();
        let kept = inflate(&w.chunks[&(0, 0)]).unwrap();
        assert!(split(&kept).unwrap().1.is_empty(), "light past what the dimension can have is dropped, the chunk is kept");
    }

    #[test]
    fn only_a_height_range_the_game_accepts_counts() {
        assert!(Dimension::default().sane());
        assert!(Dimension { floor: 0, height: 256 }.sane());
        for bad in [(0, 0), (0, 2_000_000_000), (-64, 380), (-2_000_000_000, 384), (2016, 32), (-8, 384), (2_147_483_632, 16)] {
            assert!(!Dimension { floor: bad.0, height: bad.1 }.sane(), "{bad:?}");
        }
    }

    #[test]
    fn the_sky_follows_the_overworld_clock_and_the_weather_events() {
        let mut w = World::default();
        let mut o = Out::default();
        o.varint(2).string("minecraft:the_end").bool(false).string("minecraft:overworld").bool(false);
        w.clocks(&mut In::new(&o.0)).unwrap();
        let mut o = Out::default();
        o.i64(5).varint(2).varint(0).raw(&[100]).f32(0.0).f32(1.0).varint(1).raw(&[0x88, 0xA1, 0x02]).f32(0.0).f32(1.0);
        w.time(&mut In::new(&o.0)).unwrap();
        for (event, value) in [(1, 0.0), (2, 0.0), (8, 0.5)] {
            w.weather(event, value);
        }
        assert_eq!(w.snapshot(0.0, 0.0, 1).sky, Sky { time: Some(13_000), rain: 1.0, thunder: 0.5 });
    }

    #[test]
    fn the_world_is_around_a_place_once_every_chunk_near_it_arrived() {
        let mut w = World::default();
        assert!(!w.around(8.0, 8.0, 1));
        for x in -1..=1 {
            for z in -1..=1 {
                if (x, z) != (1, 1) {
                    w.chunk(&mut In::new(&chunk(x, z, &[0; 24])), (0, 0)).unwrap();
                }
            }
        }
        assert!(w.around(8.0, 8.0, 0));
        assert!(!w.around(8.0, 8.0, 1), "one chunk near it is missing");
        w.chunk(&mut In::new(&chunk(1, 1, &[0; 24])), (0, 0)).unwrap();
        assert!(w.around(8.0, 8.0, 1));
    }
}
