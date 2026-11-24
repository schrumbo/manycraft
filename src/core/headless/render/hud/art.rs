//! The art the overlay is drawn from, read once from the 26.3 client jar: the font, the GUI sprites, item definitions,
//! item and block models and their textures. Shared by every account's pictures and dropped after a minute unused.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use image::RgbaImage;
use serde_json::Value;

use super::super::font::Font;
use super::super::model::Source;

const VERSION: &str = "26.3";
const IDLE: Duration = Duration::from_secs(60);
const SWEEP: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(10);
const MAX_ENTRY: u64 = 8 << 20;

struct Lazy {
    bytes: Vec<u8>,
    image: OnceLock<Option<RgbaImage>>,
}

/// What the overlay draws with.
pub struct Art {
    pub font: Font,
    sprites: HashMap<String, RgbaImage>,
    items: HashMap<String, Value>,
    models: HashMap<String, Value>,
    textures: HashMap<String, Lazy>,
}

impl Source for Art {
    fn blockstate(&self, _: &str) -> Option<&Value> {
        None
    }

    fn model(&self, path: &str) -> Option<&Value> {
        self.models.get(path)
    }
}

fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok().map(|i| i.into_rgba8())
}

impl Art {
    /// Reads the overlay's art from a client jar.
    pub fn from_jar(reader: impl Read + Seek) -> Result<Self> {
        let mut zip = zip::ZipArchive::new(reader).context("not a jar")?;
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        let (mut sprites, mut items, mut models, mut textures) = (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
        let mut bytes = Vec::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let Some(path) = entry.name().ok().and_then(|n| n.strip_prefix("assets/minecraft/").map(str::to_string)) else { continue };
            let wanted = path.starts_with("font/") || path.starts_with("textures/font/") || path.starts_with("textures/gui/sprites/hud/") || path.starts_with("textures/gui/sprites/boss_bar/") || path.starts_with("items/") || path.starts_with("models/item/") || path.starts_with("models/block/") || path.starts_with("textures/item/") || path.starts_with("textures/block/") || path.starts_with("textures/colormap/") || path == "textures/misc/enchanted_glint_item.png";
            if !wanted || entry.size() > MAX_ENTRY || !(path.ends_with(".json") || path.ends_with(".png")) {
                continue;
            }
            bytes.clear();
            entry.read_to_end(&mut bytes)?;
            if path.starts_with("font/") || path.starts_with("textures/font/") {
                files.insert(path, bytes.clone());
            } else if let Some(name) = path.strip_prefix("textures/gui/sprites/").and_then(|p| p.strip_suffix(".png")) {
                if let Some(img) = decode(&bytes) {
                    sprites.insert(name.to_string(), img);
                }
            } else if let Some(name) = path.strip_prefix("items/").and_then(|p| p.strip_suffix(".json")) {
                if let Ok(v) = serde_json::from_slice(&bytes) {
                    items.insert(name.to_string(), v);
                }
            } else if let Some(name) = path.strip_prefix("models/").and_then(|p| p.strip_suffix(".json")) {
                if let Ok(v) = serde_json::from_slice(&bytes) {
                    models.insert(name.to_string(), v);
                }
            } else if let Some(name) = path.strip_prefix("textures/").and_then(|p| p.strip_suffix(".png")) {
                textures.insert(name.to_string(), Lazy { bytes: bytes.clone(), image: OnceLock::new() });
            }
        }
        let font = Font::load(|p| files.get(p).cloned()).context("the jar has no font")?;
        anyhow::ensure!(sprites.contains_key("hud/hotbar"), "the jar has no overlay art");
        Ok(Art { font, sprites, items, models, textures })
    }

    /// Reads a client jar file.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        Self::from_jar(std::io::BufReader::new(file))
    }

    /// The GUI sprite `name` (`hud/hotbar`).
    pub fn sprite(&self, name: &str) -> Option<&RgbaImage> {
        self.sprites.get(name)
    }

    /// The item definition of item `name` (`stone`).
    pub fn item(&self, name: &str) -> Option<&Value> {
        self.items.get(name)
    }

    /// The texture `name` (`item/stick`), its first frame when it is an animation strip.
    pub fn texture(&self, name: &str) -> Option<&RgbaImage> {
        let lazy = self.textures.get(name)?;
        lazy.image
            .get_or_init(|| {
                let img = decode(&lazy.bytes)?;
                let side = img.width();
                Some(if img.height() > side && side > 0 { image::imageops::crop_imm(&img, 0, 0, side, side).to_image() } else { img })
            })
            .as_ref()
    }
}

struct Held {
    art: Arc<Art>,
    used: Instant,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);
static LOADING: Mutex<Option<(Instant, String)>> = Mutex::new(None);
static SWEEPING: AtomicBool = AtomicBool::new(false);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fresh() -> Option<Arc<Art>> {
    let mut held = lock(&HELD);
    let held = held.as_mut()?;
    held.used = Instant::now();
    Some(Arc::clone(&held.art))
}

/// The shared overlay art, loaded from the 26.3 client jar on first use and dropped after a minute without a picture.
/// After a failed load every caller gets that error for ten seconds instead of trying again.
pub fn shared() -> Result<Arc<Art>> {
    if let Some(art) = fresh() {
        return Ok(art);
    }
    let mut failure = lock(&LOADING);
    if let Some(art) = fresh() {
        return Ok(art);
    }
    if let Some((_, error)) = failure.as_ref().filter(|(at, _)| at.elapsed() < RETRY) {
        return Err(anyhow!("{error}"));
    }
    match crate::core::install::with_client_jar(VERSION, Art::open) {
        Ok(art) => {
            let art = Arc::new(art);
            *failure = None;
            *lock(&HELD) = Some(Held { art: Arc::clone(&art), used: Instant::now() });
            sweep();
            Ok(art)
        }
        Err(e) => {
            *failure = Some((Instant::now(), format!("{e:#}")));
            Err(e)
        }
    }
}

fn sweep() {
    if SWEEPING.swap(true, Ordering::SeqCst) {
        return;
    }
    let started = std::thread::Builder::new().name("hud-art".into()).spawn(|| {
        loop {
            std::thread::sleep(SWEEP);
            let mut held = lock(&HELD);
            if held.as_ref().is_none_or(|h| h.used.elapsed() >= IDLE) {
                *held = None;
                SWEEPING.store(false, Ordering::SeqCst);
                return;
            }
        }
    });
    if started.is_err() {
        *lock(&HELD) = None;
        SWEEPING.store(false, Ordering::SeqCst);
    }
}
