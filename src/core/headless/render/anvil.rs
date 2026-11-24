//! Reads blocks, biomes and light from a saved world's region files, so pictures can be checked against the game
//! on a real world without a server connection.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::block;
use super::{Biome, Blocks};

#[derive(Debug)]
enum Tag {
    Number(f64),
    Bytes(Vec<u8>),
    Text(String),
    List(Vec<Tag>),
    Compound(HashMap<String, Tag>),
    Longs(Vec<i64>),
}

impl Tag {
    fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(m) => m.get(key),
            _ => None,
        }
    }

    fn list(&self) -> &[Tag] {
        match self {
            Tag::List(l) => l,
            _ => &[],
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Tag::Text(s) => Some(s),
            _ => None,
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.0.len() < n {
            bail!("truncated nbt");
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn int(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }

    fn text(&mut self) -> Result<String> {
        let n = u16::from_be_bytes(self.take(2)?.try_into()?) as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn payload(&mut self, kind: u8) -> Result<Tag> {
        Ok(match kind {
            1 => Tag::Number(f64::from(self.take(1)?[0] as i8)),
            2 => Tag::Number(f64::from(i16::from_be_bytes(self.take(2)?.try_into()?))),
            3 => Tag::Number(f64::from(self.int()?)),
            4 => Tag::Number(i64::from_be_bytes(self.take(8)?.try_into()?) as f64),
            5 => Tag::Number(f64::from(f32::from_be_bytes(self.take(4)?.try_into()?))),
            6 => Tag::Number(f64::from_be_bytes(self.take(8)?.try_into()?)),
            7 => {
                let n = self.int()?.max(0) as usize;
                Tag::Bytes(self.take(n)?.to_vec())
            }
            8 => Tag::Text(self.text()?),
            9 => {
                let inner = self.take(1)?[0];
                let n = self.int()?.max(0) as usize;
                Tag::List((0..n).map(|_| self.payload(inner)).collect::<Result<_>>()?)
            }
            10 => {
                let mut map = HashMap::new();
                loop {
                    let kind = self.take(1)?[0];
                    if kind == 0 {
                        break;
                    }
                    let name = self.text()?;
                    map.insert(name, self.payload(kind)?);
                }
                Tag::Compound(map)
            }
            11 => {
                let n = self.int()?.max(0) as usize;
                Tag::List((0..n).map(|_| self.int().map(|v| Tag::Number(f64::from(v)))).collect::<Result<_>>()?)
            }
            12 => {
                let n = self.int()?.max(0) as usize;
                Tag::Longs((0..n).map(|_| Ok(i64::from_be_bytes(self.take(8)?.try_into()?))).collect::<Result<_>>()?)
            }
            _ => bail!("unknown nbt tag {kind}"),
        })
    }

    fn root(&mut self) -> Result<Tag> {
        let kind = self.take(1)?[0];
        self.text()?;
        self.payload(kind)
    }
}

struct Section {
    states: Vec<u32>,
    biomes: Vec<u16>,
    sky: Option<Vec<u8>>,
    light: Option<Vec<u8>>,
}

fn unpack(data: Option<&Tag>, palette: usize, count: usize, min_bits: u32) -> Vec<usize> {
    let Some(Tag::Longs(longs)) = data else { return vec![0; count] };
    if palette <= 1 {
        return vec![0; count];
    }
    let bits = (usize::BITS - (palette - 1).leading_zeros()).max(min_bits) as usize;
    let per = 64 / bits;
    let mask = (1u64 << bits) - 1;
    (0..count).map(|i| longs.get(i / per).map_or(0, |l| ((*l as u64) >> ((i % per) * bits) & mask) as usize)).collect()
}

/// A saved world's blocks around one place.
pub struct World {
    chunks: HashMap<(i32, i32), HashMap<i32, Section>>,
    /// Biome names in the order of the ids [`Blocks::biome`] returns.
    pub biome_names: Vec<String>,
    floor: i32,
    height: i32,
}

impl World {
    /// Loads the chunks within `radius` chunks of block `(x, z)` from the `region` folder of a world.
    pub fn load(region: &Path, x: i32, z: i32, radius: i32) -> Result<Self> {
        let mut world = World { chunks: HashMap::new(), biome_names: Vec::new(), floor: -64, height: 384 };
        let mut names: HashMap<String, u16> = HashMap::new();
        let (ccx, ccz) = (x >> 4, z >> 4);
        let mut files: HashMap<(i32, i32), Vec<u8>> = HashMap::new();
        for cz in ccz - radius..=ccz + radius {
            for cx in ccx - radius..=ccx + radius {
                let key = (cx >> 5, cz >> 5);
                if !files.contains_key(&key) {
                    let path = region.join(format!("r.{}.{}.mca", key.0, key.1));
                    files.insert(key, std::fs::read(&path).unwrap_or_default());
                }
                let file = &files[&key];
                let slot = (((cz & 31) * 32 + (cx & 31)) * 4) as usize;
                if file.len() < 8192 {
                    continue;
                }
                let offset = (u32::from(file[slot]) << 16 | u32::from(file[slot + 1]) << 8 | u32::from(file[slot + 2])) as usize * 4096;
                if offset == 0 || offset + 5 > file.len() {
                    continue;
                }
                let len = u32::from_be_bytes(file[offset..offset + 4].try_into()?) as usize;
                let body = file.get(offset + 5..offset + 4 + len).context("chunk past the end of its region")?;
                let mut raw = Vec::new();
                match file[offset + 4] {
                    1 => flate2::read::GzDecoder::new(body).read_to_end(&mut raw)?,
                    2 => flate2::read::ZlibDecoder::new(body).read_to_end(&mut raw)?,
                    3 => {
                        raw.extend_from_slice(body);
                        raw.len()
                    }
                    other => bail!("chunk compression {other} not supported"),
                };
                let root = Reader(&raw).root()?;
                let mut sections = HashMap::new();
                for s in root.get("sections").map(Tag::list).unwrap_or(&[]) {
                    let Some(Tag::Number(y)) = s.get("Y") else { continue };
                    let states_tag = s.get("block_states");
                    let palette: Vec<u32> = states_tag
                        .and_then(|b| b.get("palette"))
                        .map(Tag::list)
                        .unwrap_or(&[])
                        .iter()
                        .map(|p| {
                            let name = p.text().or_else(|| ["Name", "id", ""].iter().find_map(|k| p.get(k).and_then(Tag::text))).unwrap_or("minecraft:air");
                            let name = name.strip_prefix("minecraft:").unwrap_or(name);
                            let props: Vec<(&str, &str)> = match p.get("Properties").or_else(|| p.get("properties")) {
                                Some(Tag::Compound(m)) => m.iter().filter_map(|(k, v)| Some((k.as_str(), v.text()?))).collect(),
                                _ => Vec::new(),
                            };
                            block::encode(name, &props).unwrap_or(0)
                        })
                        .collect();
                    let states = unpack(states_tag.and_then(|b| b.get("data")), palette.len(), 4096, 4).into_iter().map(|i| palette.get(i).copied().unwrap_or(0)).collect();
                    let biome_tag = s.get("biomes");
                    let biome_palette: Vec<u16> = biome_tag
                        .and_then(|b| b.get("palette"))
                        .map(Tag::list)
                        .unwrap_or(&[])
                        .iter()
                        .map(|p| {
                            let name = p.text().unwrap_or("minecraft:plains");
                            let name = name.strip_prefix("minecraft:").unwrap_or(name).to_string();
                            let next = names.len() as u16;
                            *names.entry(name.clone()).or_insert_with(|| {
                                world.biome_names.push(name);
                                next
                            })
                        })
                        .collect();
                    let biomes = unpack(biome_tag.and_then(|b| b.get("data")), biome_palette.len(), 64, 0).into_iter().map(|i| biome_palette.get(i).copied().unwrap_or(0)).collect();
                    let bytes = |key: &str| match s.get(key) {
                        Some(Tag::Bytes(b)) if b.len() == 2048 => Some(b.clone()),
                        _ => None,
                    };
                    sections.insert(*y as i32, Section { states, biomes, sky: bytes("SkyLight"), light: bytes("BlockLight") });
                }
                world.chunks.insert((cx, cz), sections);
            }
        }
        Ok(world)
    }

    fn section(&self, x: i32, y: i32, z: i32) -> Option<&Section> {
        self.chunks.get(&(x >> 4, z >> 4))?.get(&(y >> 4))
    }

    /// The biomes of this world, in the order of its biome ids, read from the game's data in `jar`.
    pub fn biomes(&self, jar: &Path) -> Result<Vec<Biome>> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(jar)?)?;
        let mut out = Vec::new();
        for name in &self.biome_names {
            let mut text = String::new();
            let biome = match zip.by_name(&format!("data/minecraft/worldgen/biome/{name}.json")) {
                Ok(mut f) => {
                    f.read_to_string(&mut text)?;
                    Biome::from_json(&serde_json::from_str(&text)?)
                }
                Err(_) => Biome::default(),
            };
            out.push(biome);
        }
        Ok(out)
    }
}

fn nibble(bytes: &[u8], i: usize) -> u8 {
    let b = bytes[i >> 1];
    if i & 1 == 0 { b & 15 } else { b >> 4 }
}

impl Blocks for World {
    fn state(&self, x: i32, y: i32, z: i32) -> u32 {
        self.section(x, y, z).map_or(0, |s| s.states[(((y & 15) * 16 + (z & 15)) * 16 + (x & 15)) as usize])
    }

    fn biome(&self, x: i32, y: i32, z: i32) -> u16 {
        self.section(x, y, z).map_or(0, |s| s.biomes[((((y & 15) >> 2) * 4 + ((z & 15) >> 2)) * 4 + ((x & 15) >> 2)) as usize])
    }

    fn light(&self, x: i32, y: i32, z: i32) -> Option<(u8, u8)> {
        let i = (((y & 15) * 16 + (z & 15)) * 16 + (x & 15)) as usize;
        let s = self.section(x, y, z);
        let sky = s.and_then(|s| s.sky.as_deref()).map_or(15, |b| nibble(b, i));
        let light = s.and_then(|s| s.light.as_deref()).map_or(0, |b| nibble(b, i));
        Some((sky, light))
    }

    fn floor(&self) -> i32 {
        self.floor
    }

    fn height(&self) -> i32 {
        self.height
    }
}
