//! Skin faces for tiles of instances that never ran: a Microsoft account's own skin from Mojang, cached on disk, or vanilla's default skin for the UUID, read from an installed client jar.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use image::codecs::png::PngEncoder;
use image::{ImageEncoder, ImageFormat, RgbaImage};
use serde::Deserialize;
use uuid::Uuid;
use zip::ZipArchive;

use super::account::{Account, AccountKind};
use super::{net, paths};

/// Width and height of a face in pixels.
pub const SIDE: u32 = 8;
const NAMES: [&str; 9] = ["alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri"];
const PROBE: &str = "assets/minecraft/textures/entity/player/wide/steve.png";
const SESSION: &str = "https://sessionserver.mojang.com/session/minecraft/profile/";
const TEXTURES: &str = "https://textures.minecraft.net/texture/";
const FRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_SKIN: u64 = 1 << 20;

/// Finds the face of every account and passes it to `found` as 8 by 8 RGBA pixels, cached and default faces first,
/// fetched ones after. Stops when `found` returns false. Offline accounts never touch the network; a Microsoft account
/// with a proxy fetches through it.
pub fn faces(accounts: &[Account], mut found: impl FnMut(Uuid, Vec<u8>) -> bool) {
    let mut defaults = Defaults::default();
    let mut fetch = Vec::new();
    for account in accounts {
        let face = match (account.kind, cached(account.id)) {
            (AccountKind::Offline, _) => defaults.face(account.id),
            (AccountKind::Microsoft, Some((face, fresh))) => {
                if !fresh {
                    fetch.push(account);
                }
                Some(face)
            }
            (AccountKind::Microsoft, None) => {
                fetch.push(account);
                defaults.face(account.id)
            }
        };
        if let Some(face) = face
            && !found(account.id, face)
        {
            return;
        }
    }
    for account in fetch {
        let face = match own_skin(account) {
            Ok(Some(skin)) => face_of(&skin),
            Ok(None) => defaults.face(account.id),
            Err(_) => continue,
        };
        let Some(face) = face else { continue };
        let _ = store(account.id, &face);
        if !found(account.id, face) {
            return;
        }
    }
}

fn default_index(id: Uuid) -> usize {
    let (most, least) = id.as_u64_pair();
    let both = most ^ least;
    (((both >> 32) as i32) ^ (both as i32)).rem_euclid(18) as usize
}

fn default_path(index: usize) -> String {
    let model = if index < NAMES.len() { "slim" } else { "wide" };
    format!("assets/minecraft/textures/entity/player/{model}/{}.png", NAMES[index % NAMES.len()])
}

#[derive(Default)]
struct Defaults {
    jar: Option<Option<ZipArchive<File>>>,
    faces: [Option<Option<Vec<u8>>>; 18],
}

impl Defaults {
    fn face(&mut self, id: Uuid) -> Option<Vec<u8>> {
        let index = default_index(id);
        if self.faces[index].is_none() {
            let jar = self.jar.get_or_insert_with(client_jar);
            let face = jar.as_mut().and_then(|jar| {
                let mut bytes = Vec::new();
                jar.by_name(&default_path(index)).ok()?.read_to_end(&mut bytes).ok()?;
                face_of(&image::load_from_memory_with_format(&bytes, ImageFormat::Png).ok()?.into_rgba8())
            });
            self.faces[index] = Some(face);
        }
        self.faces[index].clone().flatten()
    }
}

fn client_jar() -> Option<ZipArchive<File>> {
    let mut jars: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(paths::data_dir().join("versions"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let jar = entry.path().join(format!("{}.jar", entry.file_name().to_string_lossy()));
            Some((jar.metadata().ok()?.modified().ok()?, jar))
        })
        .collect();
    jars.sort_by(|a, b| b.0.cmp(&a.0));
    jars.into_iter().find_map(|(_, jar)| {
        let mut zip = ZipArchive::new(File::open(jar).ok()?).ok()?;
        let found = zip.by_name(PROBE).is_ok();
        found.then_some(zip)
    })
}

fn face_of(skin: &RgbaImage) -> Option<Vec<u8>> {
    if skin.width() != 64 || !matches!(skin.height(), 32 | 64) {
        return None;
    }
    let hat = |x: u32, y: u32| skin.get_pixel(x + 32, y).0;
    let solid_hat = (8..16).all(|y| (8..16).all(|x| hat(x, y)[3] == 255));
    let with_hat = !(skin.height() == 32 && solid_hat);
    let mut face = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 8..16 {
        for x in 8..16 {
            let base = skin.get_pixel(x, y).0;
            let top = if with_hat { hat(x, y) } else { [0; 4] };
            let a = u16::from(top[3]);
            face.extend((0..3).map(|c| ((u16::from(base[c]) * (255 - a) + u16::from(top[c]) * a) / 255) as u8));
            face.push(255);
        }
    }
    Some(face)
}

fn cache_path(id: Uuid) -> PathBuf {
    paths::data_dir().join("cache").join("skins").join(format!("{}.png", id.simple()))
}

fn cached(id: Uuid) -> Option<(Vec<u8>, bool)> {
    let path = cache_path(id);
    let age = path.metadata().ok()?.modified().ok()?.elapsed().unwrap_or_default();
    let face = image::open(&path).ok()?.into_rgba8();
    (face.dimensions() == (SIDE, SIDE)).then(|| (face.into_raw(), age < FRESH))
}

fn store(id: Uuid, face: &[u8]) -> Result<()> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes).write_image(face, SIDE, SIDE, image::ExtendedColorType::Rgba8)?;
    net::write_atomic(&cache_path(id), &bytes)
}

#[derive(Deserialize)]
struct Profile {
    properties: Vec<Property>,
}

#[derive(Deserialize)]
struct Property {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct Textures {
    textures: TextureSet,
}

#[derive(Deserialize)]
struct TextureSet {
    #[serde(rename = "SKIN")]
    skin: Option<Texture>,
}

#[derive(Deserialize)]
struct Texture {
    url: String,
}

fn own_skin(account: &Account) -> Result<Option<RgbaImage>> {
    let agent = net::agent(account.proxy.as_deref())?;
    let profile: Profile = net::get_json(&agent, &format!("{SESSION}{}", account.id.simple()))?;
    let value = profile.properties.into_iter().find(|p| p.name == "textures").context("profile without textures")?.value;
    let textures: Textures = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(value)?)?;
    let Some(skin) = textures.textures.skin else { return Ok(None) };
    let url = skin.url.replacen("http://", "https://", 1);
    if !url.starts_with(TEXTURES) {
        bail!("skin outside Mojang's texture server");
    }
    let bytes = agent.get(&url).call()?.body_mut().with_config().limit(MAX_SKIN).read_to_vec()?;
    Ok(Some(image::load_from_memory_with_format(&bytes, ImageFormat::Png)?.into_rgba8()))
}

#[cfg(test)]
mod tests {
    use super::{default_index, default_path, face_of};
    use crate::core::account::offline_uuid;
    use image::RgbaImage;
    use uuid::Uuid;

    #[test]
    fn default_skins_follow_vanilla() {
        assert_eq!(default_index(offline_uuid("Dev")), 2);
        assert_eq!(default_index(offline_uuid("Test")), 12);
        assert_eq!(default_index(offline_uuid("Dev001")), 14);
        assert_eq!(default_index(offline_uuid("Notch")), 4);
        assert_eq!(default_index(Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap()), 0);
        assert_eq!(default_path(2), "assets/minecraft/textures/entity/player/slim/efe.png");
        assert_eq!(default_path(12), "assets/minecraft/textures/entity/player/wide/kai.png");
    }

    #[test]
    fn the_face_wears_the_hat_except_a_solid_legacy_one() {
        let mut skin = RgbaImage::from_pixel(64, 64, image::Rgba([10, 20, 30, 255]));
        skin.put_pixel(40, 8, image::Rgba([250, 0, 0, 255]));
        let face = face_of(&skin).unwrap();
        assert_eq!(face.len(), 256);
        assert_eq!(&face[..4], &[250, 0, 0, 255]);
        assert_eq!(&face[4..8], &[10, 20, 30, 255]);
        let legacy = RgbaImage::from_pixel(64, 32, image::Rgba([10, 20, 30, 255]));
        assert_eq!(&face_of(&legacy).unwrap()[..4], &[10, 20, 30, 255]);
        assert!(face_of(&RgbaImage::new(16, 16)).is_none());
    }
}
