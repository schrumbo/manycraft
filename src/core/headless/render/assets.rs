//! The game's own block art, read once from the client jar: blockstates, block models, block textures and the biome
//! colour maps. Every account's pictures share one copy, loaded on the first picture and dropped after a minute unused.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

use super::model::Source;

const IDLE: Duration = Duration::from_secs(60);
const SWEEP: Duration = Duration::from_secs(10);
const MAX_ENTRY: u64 = 8 << 20;
const VERSION: &str = "26.3";

/// A square texture with its mip levels, RGBA, rows top down.
pub struct Texture {
    pub size: u32,
    pub levels: Vec<Vec<[u8; 4]>>,
}

/// How a texture's pixels mix with what lies behind them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Transparency {
    Opaque,
    Cutout,
    Translucent,
}

impl Texture {
    fn new(size: u32, pixels: Vec<[u8; 4]>) -> Self {
        let mut levels = vec![pixels];
        let mut side = size;
        while side > 1 && levels.len() < 5 {
            let prev = levels.last().expect("level");
            let half = side / 2;
            let mut next = Vec::with_capacity((half * half) as usize);
            for y in 0..half {
                for x in 0..half {
                    let at = |dx: u32, dy: u32| prev[((y * 2 + dy) * side + x * 2 + dx) as usize];
                    let four = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
                    let alpha: u32 = four.iter().map(|p| u32::from(p[3])).sum();
                    let mut px = [0u8; 4];
                    for c in 0..3 {
                        let sum: u32 = four.iter().map(|p| u32::from(p[c]) * u32::from(p[3])).sum();
                        px[c] = if alpha == 0 { (four.iter().map(|p| u32::from(p[c])).sum::<u32>() / 4) as u8 } else { (sum / alpha) as u8 };
                    }
                    px[3] = (alpha / 4) as u8;
                    next.push(px);
                }
            }
            levels.push(next);
            side = half;
        }
        Self { size, levels }
    }

    /// How the pixels inside the rectangle `lo..hi` (texture space, 0..1) mix, as the game decides per face.
    pub fn transparency(&self, lo: [f32; 2], hi: [f32; 2]) -> Transparency {
        let s = self.size as f32;
        let (x0, y0) = ((lo[0] * s).floor().max(0.0) as u32, (lo[1] * s).floor().max(0.0) as u32);
        let (x1, y1) = (((hi[0] * s).ceil() as u32).min(self.size), ((hi[1] * s).ceil() as u32).min(self.size));
        let mut found = Transparency::Opaque;
        for y in y0..y1.max(y0 + 1).min(self.size) {
            for x in x0..x1.max(x0 + 1).min(self.size) {
                match self.levels[0][(y * self.size + x) as usize][3] {
                    255 => {}
                    0 => found = found.max(Transparency::Cutout),
                    _ => return Transparency::Translucent,
                }
            }
        }
        found
    }

    /// The colour at `(u, v)` (clamped to the edges), blended between the four nearest texels.
    pub fn smooth(&self, u: f32, v: f32) -> [f32; 4] {
        let s = self.size as f32;
        let (x, y) = ((u * s - 0.5).clamp(0.0, s - 1.0), (v * s - 0.5).clamp(0.0, s - 1.0));
        let (x0, y0) = (x.floor() as u32, y.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(self.size - 1), (y0 + 1).min(self.size - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |x: u32, y: u32| self.levels[0][(y * self.size + x) as usize].map(|c| f32::from(c) / 255.0);
        let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        [0, 1, 2, 3].map(|i| (a[i] + (b[i] - a[i]) * fx) * (1.0 - fy) + (c[i] + (d[i] - c[i]) * fx) * fy)
    }

    /// The texel at `(u, v)` (repeating) on mip `level`.
    #[inline]
    pub fn sample(&self, u: f32, v: f32, level: usize) -> [u8; 4] {
        let level = level.min(self.levels.len() - 1);
        let side = (self.size >> level).max(1) as i32;
        let x = ((u * side as f32).floor() as i32).rem_euclid(side);
        let y = ((v * side as f32).floor() as i32).rem_euclid(side);
        self.levels[level][(y * side + x) as usize]
    }
}

const MOON: [&str; 8] = ["/full_moon", "/waning_gibbous", "/third_quarter", "/waning_crescent", "/new_moon", "/waxing_crescent", "/first_quarter", "/waxing_gibbous"];

/// The art one picture needs.
pub struct Assets {
    blockstates: HashMap<String, Value>,
    models: HashMap<String, Value>,
    textures: Vec<Texture>,
    names: HashMap<String, u32>,
    pub(super) grass: Option<Texture>,
    pub(super) foliage: Option<Texture>,
    pub(super) dry_foliage: Option<Texture>,
    pub(super) sun: Option<Texture>,
    pub(super) moon: [Option<Texture>; 8],
    pub(super) vignette: Option<Texture>,
}

impl Source for Assets {
    fn blockstate(&self, name: &str) -> Option<&Value> {
        self.blockstates.get(name)
    }

    fn model(&self, path: &str) -> Option<&Value> {
        self.models.get(path)
    }
}

fn texture(bytes: &[u8], square: bool) -> Option<Texture> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?.into_rgba8();
    let (w, h) = image.dimensions();
    let side = if square { w.min(h) } else { w };
    if side == 0 || !square && w != h {
        return None;
    }
    let pixels = (0..side * side).map(|i| image.get_pixel(i % side, i / side).0).collect();
    Some(Texture::new(side, pixels))
}

impl Assets {
    /// Reads what pictures need from a client jar.
    pub fn from_jar(reader: impl Read + Seek) -> Result<Self> {
        let mut zip = zip::ZipArchive::new(reader).context("not a jar")?;
        let mut assets = Assets { blockstates: HashMap::new(), models: HashMap::new(), textures: Vec::new(), names: HashMap::new(), grass: None, foliage: None, dry_foliage: None, sun: None, moon: Default::default(), vignette: None };
        let mut bytes = Vec::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let Some(path) = entry.name().ok().and_then(|n| n.strip_prefix("assets/minecraft/").map(str::to_string)) else { continue };
            let wanted = path.starts_with("blockstates/") || path.starts_with("models/block/") || path.starts_with("textures/block/") || path.starts_with("textures/colormap/") || path.starts_with("textures/environment/") || path == "textures/misc/vignette.png";
            if !wanted || entry.size() > MAX_ENTRY {
                continue;
            }
            bytes.clear();
            entry.read_to_end(&mut bytes)?;
            if let Some(name) = path.strip_prefix("blockstates/").and_then(|p| p.strip_suffix(".json")) {
                if let Ok(v) = serde_json::from_slice(&bytes) {
                    assets.blockstates.insert(name.to_string(), v);
                }
            } else if let Some(name) = path.strip_prefix("models/").and_then(|p| p.strip_suffix(".json")) {
                if let Ok(v) = serde_json::from_slice(&bytes) {
                    assets.models.insert(name.to_string(), v);
                }
            } else if let Some(name) = path.strip_prefix("textures/").and_then(|p| p.strip_suffix(".png")) {
                match name {
                    "colormap/grass" => assets.grass = texture(&bytes, false),
                    "colormap/foliage" => assets.foliage = texture(&bytes, false),
                    "colormap/dry_foliage" => assets.dry_foliage = texture(&bytes, false),
                    "environment/celestial/sun" => assets.sun = texture(&bytes, true),
                    "misc/vignette" => assets.vignette = texture(&bytes, true),
                    _ if name.starts_with("environment/celestial/moon/") => {
                        if let Some(phase) = MOON.iter().position(|p| name.ends_with(p)) {
                            assets.moon[phase] = texture(&bytes, true);
                        }
                    }
                    _ if name.starts_with("block/") => {
                        if let Some(t) = texture(&bytes, true) {
                            assets.names.insert(name.to_string(), assets.textures.len() as u32);
                            assets.textures.push(t);
                        }
                    }
                    _ => {}
                }
            }
        }
        anyhow::ensure!(!assets.blockstates.is_empty() && !assets.textures.is_empty(), "the jar has no block art");
        Ok(assets)
    }

    /// Reads a client jar file.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        Self::from_jar(std::io::BufReader::new(file))
    }

    /// The id of texture `name` (`block/stone`).
    pub fn texture_id(&self, name: &str) -> Option<u32> {
        self.names.get(name).copied()
    }

    pub fn texture(&self, id: u32) -> &Texture {
        &self.textures[id as usize]
    }
}

struct Held {
    assets: Arc<Assets>,
    used: Instant,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

/// The shared art, loaded from the 26.3 client jar on first use (fetching the jar into the shared versions folder if
/// needed) and dropped after a minute without a picture.
pub fn shared() -> Result<Arc<Assets>> {
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(h) = held.as_mut() {
        h.used = Instant::now();
        return Ok(Arc::clone(&h.assets));
    }
    let jar = crate::core::install::client_jar(VERSION)?;
    let assets = Arc::new(Assets::open(&jar)?);
    *held = Some(Held { assets: Arc::clone(&assets), used: Instant::now() });
    std::thread::Builder::new().name("render-assets".into()).spawn(|| {
        loop {
            std::thread::sleep(SWEEP);
            let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
            if held.as_ref().is_none_or(|h| h.used.elapsed() >= IDLE) {
                *held = None;
                return;
            }
        }
    })?;
    Ok(assets)
}
