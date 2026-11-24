//! Player skins fetched from the game's texture server, once per skin, in the background. A skin not here yet, or one
//! that failed, draws as the default skin, so a picture never waits on the network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::figure::Sheet;

const HOST: &str = "textures.minecraft.net/texture/";
const MAX_SKINS: usize = 512;
const MAX_FETCHING: usize = 4;
const MAX_BYTES: u64 = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

enum Slot {
    Fetching,
    Ready(Arc<Sheet>),
    Failed,
}

/// Skins by texture hash, shared by every picture of a set of assets.
#[derive(Default)]
pub struct Skins {
    slots: Arc<Mutex<HashMap<String, Slot>>>,
}

/// The texture hash of a skin address on the game's texture server, or `None` for any other address.
fn hash(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let hash = rest.strip_prefix(HOST)?;
    (!hash.is_empty() && hash.len() <= 128 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

impl Skins {
    /// The skin at `url` when it is here; otherwise starts fetching it and returns `None`.
    pub fn get(&self, url: &str) -> Option<Arc<Sheet>> {
        let hash = hash(url)?.to_ascii_lowercase();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        match slots.get(&hash) {
            Some(Slot::Ready(sheet)) => return Some(sheet.clone()),
            Some(_) => return None,
            None => {}
        }
        let fetching = slots.values().filter(|s| matches!(s, Slot::Fetching)).count();
        if slots.len() >= MAX_SKINS || fetching >= MAX_FETCHING {
            return None;
        }
        slots.insert(hash.clone(), Slot::Fetching);
        drop(slots);
        let slots = self.slots.clone();
        let spawned = std::thread::Builder::new().name("skin".into()).spawn(move || {
            let slot = fetch(&hash).map_or(Slot::Failed, |s| Slot::Ready(Arc::new(s)));
            slots.lock().unwrap_or_else(|e| e.into_inner()).insert(hash, slot);
        });
        if spawned.is_err() {
            return None;
        }
        None
    }
}

fn fetch(hash: &str) -> Option<Sheet> {
    let agent = crate::core::net::agent_within(TIMEOUT);
    let mut res = agent.get(&format!("https://{HOST}{hash}")).call().ok()?;
    let bytes = res.body_mut().with_config().limit(MAX_BYTES).read_to_vec().ok()?;
    decode(&bytes)
}

/// A skin image as the game uses it: 64x64, with old 64x32 skins widened and the base layer made opaque.
fn decode(bytes: &[u8]) -> Option<Sheet> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?.into_rgba8();
    let (w, h) = image.dimensions();
    if w != 64 || (h != 64 && h != 32) {
        return None;
    }
    let mut px = vec![[0u8; 4]; 64 * 64];
    for (i, p) in image.pixels().enumerate() {
        px[i] = p.0;
    }
    if h == 32 {
        const COPIES: [(usize, usize, i32, i32, usize, usize); 12] = [
            (4, 16, 16, 32, 4, 4), (8, 16, 16, 32, 4, 4), (0, 20, 24, 32, 4, 12), (4, 20, 16, 32, 4, 12), (8, 20, 8, 32, 4, 12), (12, 20, 16, 32, 4, 12),
            (44, 16, -8, 32, 4, 4), (48, 16, -8, 32, 4, 4), (40, 20, 0, 32, 4, 12), (44, 20, -8, 32, 4, 12), (48, 20, -16, 32, 4, 12), (52, 20, -8, 32, 4, 12),
        ];
        for (x, y, dx, dy, cw, ch) in COPIES {
            for row in 0..ch {
                for col in 0..cw {
                    let from = (y + row) * 64 + x + col;
                    let to_x = (x as i32 + dx) as usize + cw - 1 - col;
                    let to = (y as i32 + dy) as usize * 64 + row * 64 + to_x;
                    px[to] = px[from];
                }
            }
        }
        opaque(&mut px, 0, 0, 32, 16);
        let hat: Vec<usize> = (0..32).flat_map(|y| (32..64).map(move |x| y * 64 + x)).collect();
        if hat.iter().all(|&i| px[i][3] >= 128) {
            for i in hat {
                px[i] = [0; 4];
            }
        }
        opaque(&mut px, 0, 16, 64, 32);
        opaque(&mut px, 16, 48, 48, 64);
    } else {
        opaque(&mut px, 0, 0, 32, 16);
        opaque(&mut px, 0, 16, 64, 32);
        opaque(&mut px, 16, 48, 48, 64);
    }
    Sheet::new(64, 64, &px)
}

fn opaque(px: &mut [[u8; 4]], x0: usize, y0: usize, x1: usize, y1: usize) {
    for y in y0..y1 {
        for x in x0..x1 {
            px[y * 64 + x][3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_texture_server_addresses_are_fetched() {
        assert_eq!(hash("http://textures.minecraft.net/texture/1a2B3c"), Some("1a2B3c"));
        assert_eq!(hash("https://textures.minecraft.net/texture/abc"), Some("abc"));
        assert_eq!(hash("https://textures.minecraft.net/texture/../x"), None);
        assert_eq!(hash("https://evil.example/textures.minecraft.net/texture/abc"), None);
        assert_eq!(hash("https://textures.minecraft.net.evil/texture/abc"), None);
        assert_eq!(hash("https://textures.minecraft.net/texture/"), None);
        assert!(Skins::default().get("file:///etc/passwd").is_none());
    }

    #[test]
    fn old_skins_get_their_left_limbs_mirrored_from_the_right() {
        let mut image = image::RgbaImage::new(64, 32);
        image.put_pixel(4, 20, image::Rgba([200, 10, 10, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        let sheet = decode(&bytes).unwrap();
        let at = |x: usize, y: usize| sheet.texture.levels[0][y * 64 + x];
        assert_eq!(at(23, 52), [200, 10, 10, 255]);
        assert_eq!(at(40, 0)[3], 0);
        assert_eq!(at(1, 17)[3], 255);
    }
}
