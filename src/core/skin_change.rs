//! Changing the skin and cape of a Microsoft account through Minecraft services, and the skins it wore lately.
//! Every call carries the Minecraft access token; it never shows up in errors, logs or `Debug`.

use std::fmt;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::{ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use uuid::Uuid;

use super::{net, paths};

const SERVICES: &str = "https://api.minecraftservices.com";
const TEXTURES: &str = "https://textures.minecraft.net/texture/";
const MAX_BODY: u64 = 1 << 20;
/// Largest skin file accepted, in bytes.
pub const MAX_PNG: u64 = 256 << 10;
const KEPT: usize = 6;
/// Width of a front or back view in skin pixels.
pub const VIEW_W: u32 = 16;
/// Height of a front or back view in skin pixels.
pub const VIEW_H: u32 = 32;
/// Width of a cape's front in cape pixels.
pub const CAPE_W: u32 = 10;
/// Height of a cape's front in cape pixels.
pub const CAPE_H: u32 = 16;

/// The arm width a skin is made for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    #[default]
    Classic,
    Slim,
}

impl Model {
    fn variant(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Slim => "slim",
        }
    }

    fn arm(self) -> u32 {
        match self {
            Self::Classic => 4,
            Self::Slim => 3,
        }
    }
}

/// A cape the account owns.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cape {
    pub id: String,
    pub alias: String,
    pub url: String,
    pub active: bool,
}

/// What the account wears, as Minecraft services report it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Wardrobe {
    pub skin_url: Option<String>,
    pub model: Model,
    pub capes: Vec<Cape>,
}

impl Wardrobe {
    /// The id of the cape worn, if any.
    pub fn cape(&self) -> Option<&str> {
        self.capes.iter().find(|c| c.active).map(|c| c.id.as_str())
    }
}

/// Why a skin or cape change failed, for the cases a person can act on. Found with `downcast_ref`.
#[derive(Debug, PartialEq, Eq)]
pub enum SkinError {
    NotPng,
    WrongSize(u32, u32),
    TooLarge,
    RateLimited,
    SignInAgain,
    SkinRejected,
    CapeRejected,
    Unavailable(u16),
}

impl fmt::Display for SkinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPng => f.write_str("the file is not a PNG image"),
            Self::WrongSize(w, h) => write!(f, "a skin is 64 × 64 or 64 × 32 pixels, this one is {w} × {h}"),
            Self::TooLarge => write!(f, "the file is larger than {} KB", MAX_PNG >> 10),
            Self::RateLimited => f.write_str("Minecraft services allow no change right now"),
            Self::SignInAgain => f.write_str("Minecraft services no longer accept this sign-in"),
            Self::SkinRejected => f.write_str("Minecraft services rejected the skin"),
            Self::CapeRejected => f.write_str("Minecraft services rejected the cape"),
            Self::Unavailable(status) => write!(f, "Minecraft services answered with status {status}"),
        }
    }
}

impl std::error::Error for SkinError {}

/// Checks that `bytes` is a skin Minecraft takes: a PNG of 64 × 64 or 64 × 32 pixels, not too large.
pub fn check_png(bytes: &[u8]) -> Result<RgbaImage, SkinError> {
    if bytes.len() as u64 > MAX_PNG {
        return Err(SkinError::TooLarge);
    }
    let image = image::load_from_memory_with_format(bytes, ImageFormat::Png).map_err(|_| SkinError::NotPng)?.into_rgba8();
    match image.dimensions() {
        (64, 64 | 32) => Ok(image),
        (w, h) => Err(SkinError::WrongSize(w, h)),
    }
}

/// The model a skin was most likely drawn for: slim when the outer columns of its arms are empty.
pub fn guess_model(skin: &RgbaImage) -> Model {
    let empty = |x0: u32, y0: u32| (y0..y0 + 12).all(|y| (x0..x0 + 2).all(|x| skin.get_pixel(x, y)[3] == 0));
    if skin.height() == 64 && empty(54, 20) && empty(46, 52) { Model::Slim } else { Model::Classic }
}

/// Reads and checks a skin file the person picked.
pub fn read_png(path: &Path) -> Result<(Vec<u8>, RgbaImage)> {
    let size = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?.len();
    if size > MAX_PNG {
        return Err(SkinError::TooLarge.into());
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let image = check_png(&bytes)?;
    Ok((bytes, image))
}

/// The multipart body that uploads `png` as a skin for `model`, separated by `boundary`.
fn multipart(boundary: &str, model: Model, png: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(png.len() + 256);
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"variant\"\r\n\r\n{}\r\n", model.variant()).as_bytes());
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn wardrobe(body: &[u8]) -> Result<Wardrobe> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        skins: Vec<RawSkin>,
        #[serde(default)]
        capes: Vec<RawCape>,
    }
    #[derive(Deserialize)]
    struct RawSkin {
        state: String,
        url: String,
        #[serde(default)]
        variant: String,
    }
    #[derive(Deserialize)]
    struct RawCape {
        id: String,
        state: String,
        url: String,
        #[serde(default)]
        alias: String,
    }
    let raw: Raw = serde_json::from_slice(body).context("unexpected answer from Minecraft services")?;
    let skin = raw.skins.iter().find(|s| s.state.eq_ignore_ascii_case("active")).or(raw.skins.first());
    Ok(Wardrobe {
        skin_url: skin.map(|s| s.url.clone()),
        model: if skin.is_some_and(|s| s.variant.eq_ignore_ascii_case("slim")) { Model::Slim } else { Model::Classic },
        capes: raw.capes.into_iter().map(|c| Cape { active: c.state.eq_ignore_ascii_case("active"), id: c.id, alias: c.alias, url: c.url }).collect(),
    })
}

#[derive(Clone, Copy)]
enum Call {
    Read,
    Skin,
    Cape,
}

fn failure(call: Call, status: u16) -> SkinError {
    match (status, call) {
        (429, _) => SkinError::RateLimited,
        (401, _) => SkinError::SignInAgain,
        (400 | 404 | 403, Call::Skin) => SkinError::SkinRejected,
        (400 | 404 | 403, Call::Cape) => SkinError::CapeRejected,
        _ => SkinError::Unavailable(status),
    }
}

/// The texture id at the end of a texture URL on Mojang's texture host.
pub fn texture_hash(url: &str) -> Option<String> {
    let hash = url.replacen("http://", "https://", 1).strip_prefix(TEXTURES)?.to_owned();
    (!hash.is_empty() && hash.len() <= 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
}

/// Minecraft services for one signed-in account.
pub struct Services {
    agent: ureq::Agent,
    base: String,
    textures: String,
    token: String,
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Services").field("base", &self.base).finish_non_exhaustive()
    }
}

impl Services {
    /// Talks to Minecraft services through `proxy` with the access token of a fresh session.
    pub fn new(proxy: Option<&str>, token: String) -> Result<Self> {
        Ok(Self { agent: net::agent(proxy)?, base: SERVICES.into(), textures: TEXTURES.into(), token })
    }

    fn send(&self, call: Call, request: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Wardrobe> {
        let mut res = request.context("cannot reach Minecraft services")?;
        let status = res.status().as_u16();
        let body = res.body_mut().with_config().limit(MAX_BODY).read_to_vec().context("cannot read the answer from Minecraft services")?;
        if !(200..300).contains(&status) {
            return Err(failure(call, status).into());
        }
        if matches!(call, Call::Read) {
            return wardrobe(&body);
        }
        wardrobe(&body).or_else(|_| self.wardrobe())
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// What the account wears now.
    pub fn wardrobe(&self) -> Result<Wardrobe> {
        let url = format!("{}/minecraft/profile", self.base);
        self.send(Call::Read, self.agent.get(&url).header("Authorization", self.bearer()).config().http_status_as_error(false).build().call())
    }

    /// Uploads `png` as the account's skin for `model`.
    pub fn upload(&self, png: &[u8], model: Model) -> Result<Wardrobe> {
        let boundary = format!("manycraft-{}", Uuid::new_v4().simple());
        let body = multipart(&boundary, model, png);
        let url = format!("{}/minecraft/profile/skins", self.base);
        let request = self.agent.post(&url).header("Authorization", self.bearer()).header("Content-Type", format!("multipart/form-data; boundary={boundary}")).config().http_status_as_error(false).build();
        self.send(Call::Skin, request.send(&body[..]))
    }

    /// Wears the cape `id`, or none.
    pub fn wear_cape(&self, id: Option<&str>) -> Result<Wardrobe> {
        let url = format!("{}/minecraft/profile/capes/active", self.base);
        let request = match id {
            Some(id) => self.agent.put(&url).header("Authorization", self.bearer()).config().http_status_as_error(false).build().send_json(serde_json::json!({ "capeId": id })),
            None => self.agent.delete(&url).header("Authorization", self.bearer()).config().http_status_as_error(false).build().call(),
        };
        self.send(Call::Cape, request)
    }

    /// Loads a skin or cape texture, only from Mojang's texture host.
    pub fn texture(&self, url: &str) -> Result<RgbaImage> {
        let url = url.replacen("http://", "https://", 1);
        let url = if self.textures == TEXTURES { url } else { url.replacen(TEXTURES, &self.textures, 1) };
        if !url.starts_with(&self.textures) {
            anyhow::bail!("texture outside Mojang's texture host");
        }
        let bytes = self.agent.get(&url).call().context("cannot load the texture")?.body_mut().with_config().limit(MAX_PNG).read_to_vec().context("cannot read the texture")?;
        Ok(image::load_from_memory_with_format(&bytes, ImageFormat::Png).context("the texture is not a PNG")?.into_rgba8())
    }
}

/// What came of [`apply`]: the wardrobe after the last step that worked, whether the skin changed, and what failed.
#[derive(Debug)]
pub struct Applied {
    pub wardrobe: Option<Wardrobe>,
    pub skin_done: bool,
    pub error: Option<anyhow::Error>,
}

/// Uploads `skin` if given, then wears `cape` if given (`Some(None)` takes the cape off). Stops at the first failure.
pub fn apply(services: &Services, skin: Option<(&[u8], Model)>, cape: Option<Option<&str>>) -> Applied {
    let mut done = Applied { wardrobe: None, skin_done: false, error: None };
    if let Some((png, model)) = skin {
        match services.upload(png, model) {
            Ok(w) => {
                done.wardrobe = Some(w);
                done.skin_done = true;
            }
            Err(e) => {
                done.error = Some(e);
                return done;
            }
        }
    }
    if let Some(cape) = cape {
        match services.wear_cape(cape) {
            Ok(w) => done.wardrobe = Some(w),
            Err(e) => done.error = Some(e),
        }
    }
    done
}

fn base_opaque(skin: &RgbaImage, x: u32, y: u32) -> Rgba<u8> {
    let p = skin.get_pixel(x, y).0;
    Rgba([p[0], p[1], p[2], 255])
}

fn over(below: Rgba<u8>, top: Rgba<u8>) -> Rgba<u8> {
    let a = u16::from(top[3]);
    let mix = |i: usize| ((u16::from(top[i]) * a + u16::from(below[i]) * (255 - a)) / 255) as u8;
    Rgba([mix(0), mix(1), mix(2), 255])
}

struct Part {
    at: (u32, u32),
    size: (u32, u32),
    base: (u32, u32),
    layer: Option<(u32, u32)>,
    mirror: bool,
}

fn paint(view: &mut RgbaImage, skin: &RgbaImage, part: &Part, hat_off: bool) {
    let (w, h) = part.size;
    for y in 0..h {
        for x in 0..w {
            let sx = if part.mirror { w - 1 - x } else { x };
            let mut px = base_opaque(skin, part.base.0 + sx, part.base.1 + y);
            if let Some(layer) = part.layer.filter(|_| !hat_off) {
                px = over(px, *skin.get_pixel(layer.0 + sx, layer.1 + y));
            }
            view.put_pixel(part.at.0 + x, part.at.1 + y, px);
        }
    }
}

/// The skin seen from the front and from the back, [`VIEW_W`] × [`VIEW_H`] pixels each, transparent around slim arms.
/// `skin` must have passed [`check_png`].
pub fn front_and_back(skin: &RgbaImage, model: Model) -> (RgbaImage, RgbaImage) {
    let legacy = skin.height() == 32;
    let arm = model.arm();
    let pad = 4 - arm;
    let solid_hat = (8..16).all(|y| (32..64).all(|x| skin.get_pixel(x, y)[3] == 255));
    let hat_off = legacy && solid_hat;
    let layer = |at: (u32, u32)| (!legacy).then_some(at);
    let mut front = RgbaImage::new(VIEW_W, VIEW_H);
    let mut back = RgbaImage::new(VIEW_W, VIEW_H);
    let (left_arm, left_arm_back, left_leg, left_leg_back) = if legacy { ((44, 20), (48 + arm, 20), (4, 20), (12, 20)) } else { ((36, 52), (40 + arm, 52), (20, 52), (28, 52)) };
    let front_parts = [
        Part { at: (4, 0), size: (8, 8), base: (8, 8), layer: Some((40, 8)), mirror: false },
        Part { at: (4, 8), size: (8, 12), base: (20, 20), layer: layer((20, 36)), mirror: false },
        Part { at: (pad, 8), size: (arm, 12), base: (44, 20), layer: layer((44, 36)), mirror: false },
        Part { at: (12, 8), size: (arm, 12), base: left_arm, layer: layer((52, 52)), mirror: legacy },
        Part { at: (4, 20), size: (4, 12), base: (4, 20), layer: layer((4, 36)), mirror: false },
        Part { at: (8, 20), size: (4, 12), base: left_leg, layer: layer((4, 52)), mirror: legacy },
    ];
    let back_parts = [
        Part { at: (4, 0), size: (8, 8), base: (24, 8), layer: Some((56, 8)), mirror: false },
        Part { at: (4, 8), size: (8, 12), base: (32, 20), layer: layer((32, 36)), mirror: false },
        Part { at: (pad, 8), size: (arm, 12), base: left_arm_back, layer: layer((56 + arm, 52)), mirror: legacy },
        Part { at: (12, 8), size: (arm, 12), base: (48 + arm, 20), layer: layer((48 + arm, 36)), mirror: false },
        Part { at: (4, 20), size: (4, 12), base: left_leg_back, layer: layer((12, 52)), mirror: legacy },
        Part { at: (8, 20), size: (4, 12), base: (12, 20), layer: layer((12, 36)), mirror: false },
    ];
    for (i, part) in front_parts.iter().enumerate() {
        paint(&mut front, skin, part, i == 0 && hat_off);
    }
    for (i, part) in back_parts.iter().enumerate() {
        paint(&mut back, skin, part, i == 0 && hat_off);
    }
    (front, back)
}

/// The outside of a cape, [`CAPE_W`] × [`CAPE_H`] pixels; `None` for an image too small to be one.
pub fn cape_front(cape: &RgbaImage) -> Option<RgbaImage> {
    let scale = cape.width() / 64;
    if scale == 0 || cape.height() < 17 * scale {
        return None;
    }
    Some(RgbaImage::from_fn(CAPE_W, CAPE_H, |x, y| base_opaque(cape, (1 + x) * scale, (1 + y) * scale)))
}

/// `image` blown up `scale` times without smoothing.
pub fn scaled(image: &RgbaImage, scale: u32) -> RgbaImage {
    RgbaImage::from_fn(image.width() * scale, image.height() * scale, |x, y| *image.get_pixel(x / scale, y / scale))
}

/// `image` as PNG bytes.
pub fn png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).context("cannot encode the image")?;
    Ok(bytes)
}

/// A skin the account wore lately, kept on disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Recent {
    pub path: PathBuf,
    pub model: Model,
}

#[derive(Serialize, Deserialize)]
struct Kept {
    file: String,
    model: Model,
}

fn recent_dir(account: Uuid) -> PathBuf {
    paths::data_dir().join("skins").join(account.simple().to_string())
}

/// The skins `account` wore lately, newest first.
pub fn recent(account: Uuid) -> Vec<Recent> {
    recent_in(&recent_dir(account))
}

/// Keeps `png` as the newest of `account`'s recent skins; an older copy of the same file moves to the front.
pub fn remember(account: Uuid, png: &[u8], model: Model) -> Result<()> {
    remember_in(&recent_dir(account), png, model)
}

fn valid_file(name: &str) -> bool {
    name.len() == 44 && name.ends_with(".png") && name[..40].bytes().all(|b| b.is_ascii_hexdigit())
}

fn read_index(dir: &Path) -> Vec<Kept> {
    std::fs::read(dir.join("recent.json")).ok().and_then(|b| serde_json::from_slice::<Vec<Kept>>(&b).ok()).unwrap_or_default().into_iter().filter(|k| valid_file(&k.file)).collect()
}

fn recent_in(dir: &Path) -> Vec<Recent> {
    read_index(dir).into_iter().map(|k| Recent { path: dir.join(k.file), model: k.model }).filter(|r| r.path.is_file()).collect()
}

fn remember_in(dir: &Path, png: &[u8], model: Model) -> Result<()> {
    let pixels = image::load_from_memory_with_format(png, ImageFormat::Png).map(|i| i.into_rgba8());
    let digest = match &pixels {
        Ok(image) => Sha1::new().chain_update(image.width().to_le_bytes()).chain_update(image.height().to_le_bytes()).chain_update(image.as_raw()).finalize(),
        Err(_) => Sha1::digest(png),
    };
    let hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let file = format!("{hash}.png");
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(&file);
    if !path.is_file() {
        net::write_atomic(&path, png)?;
    }
    let mut kept = read_index(dir);
    kept.retain(|k| k.file != file && dir.join(&k.file).is_file());
    kept.insert(0, Kept { file, model });
    let dropped = kept.split_off(kept.len().min(KEPT));
    net::write_atomic(&dir.join("recent.json"), &serde_json::to_vec_pretty(&kept)?)?;
    for gone in dropped.iter().filter(|g| !kept.iter().any(|k| k.file == g.file)) {
        let _ = std::fs::remove_file(dir.join(&gone.file));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use image::ImageEncoder;

    use super::*;

    const PROFILE: &str = r#"{"id":"0123456789abcdef0123456789abcdef","name":"Tester","skins":[{"id":"s1","state":"INACTIVE","url":"http://textures.minecraft.net/texture/aa11","variant":"CLASSIC"},{"id":"s2","state":"ACTIVE","url":"http://textures.minecraft.net/texture/bb22","variant":"SLIM","textureKey":"bb22"}],"capes":[{"id":"c1","state":"INACTIVE","url":"http://textures.minecraft.net/texture/cc33","alias":"Migrator"},{"id":"c2","state":"ACTIVE","url":"http://textures.minecraft.net/texture/dd44","alias":"Pan"}]}"#;
    const NO_CAPE: &str = r#"{"id":"0123456789abcdef0123456789abcdef","name":"Tester","skins":[{"id":"s2","state":"ACTIVE","url":"http://textures.minecraft.net/texture/bb22","variant":"SLIM"}],"capes":[{"id":"c1","state":"INACTIVE","url":"u","alias":"Migrator"}]}"#;

    struct Seen {
        method: String,
        path: String,
        headers: String,
        body: Vec<u8>,
    }

    type Routes = Vec<(&'static str, Vec<(u16, Vec<u8>)>)>;

    fn serve(routes: Routes) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let mut queues: HashMap<&str, VecDeque<(u16, Vec<u8>)>> = routes.into_iter().map(|(route, replies)| (route, replies.into())).collect();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut words = line.split_whitespace();
                let method = words.next().unwrap().to_owned();
                let path = words.next().unwrap().to_owned();
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    headers.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let route = format!("{method} {path}");
                log.lock().unwrap().push(Seen { method, path, headers, body });
                let (status, text) = match queues.get_mut(route.as_str()) {
                    Some(q) if q.len() > 1 => q.pop_front().unwrap(),
                    Some(q) => q.front().unwrap().clone(),
                    None => (404, b"{}".to_vec()),
                };
                let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", text.len());
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&text).unwrap();
            }
        });
        (base, seen)
    }

    fn services(base: &str) -> Services {
        Services { agent: net::agent(None).unwrap(), base: base.into(), textures: format!("{base}/texture/"), token: "SECRET-MC-TOKEN".into() }
    }

    fn json(text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }

    fn skin(w: u32, h: u32) -> Vec<u8> {
        png(&RgbaImage::from_pixel(w, h, Rgba([1, 2, 3, 255]))).unwrap()
    }

    #[test]
    fn only_64_by_64_or_64_by_32_pngs_pass() {
        assert!(check_png(&skin(64, 64)).is_ok());
        assert!(check_png(&skin(64, 32)).is_ok());
        assert_eq!(check_png(&skin(128, 128)), Err(SkinError::WrongSize(128, 128)));
        assert_eq!(check_png(&skin(64, 48)), Err(SkinError::WrongSize(64, 48)));
        assert_eq!(check_png(b"GIF89a not a png"), Err(SkinError::NotPng));
        assert_eq!(check_png(&[]), Err(SkinError::NotPng));
        let mut jpeg = Vec::new();
        RgbaImage::from_pixel(64, 64, Rgba([1, 2, 3, 255])).write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Bmp).unwrap();
        assert_eq!(check_png(&jpeg), Err(SkinError::NotPng));
        assert_eq!(check_png(&vec![0; MAX_PNG as usize + 1]), Err(SkinError::TooLarge));
    }

    #[test]
    fn a_picked_file_is_read_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.png");
        std::fs::write(&good, skin(64, 32)).unwrap();
        let (bytes, image) = read_png(&good).unwrap();
        assert_eq!((bytes.len() > 0, image.height()), (true, 32));
        let big = dir.path().join("big.png");
        std::fs::write(&big, vec![0; MAX_PNG as usize + 1]).unwrap();
        assert_eq!(read_png(&big).unwrap_err().downcast_ref::<SkinError>(), Some(&SkinError::TooLarge));
        assert!(read_png(&dir.path().join("missing.png")).is_err());
    }

    #[test]
    fn the_multipart_body_carries_variant_and_file() {
        let body = multipart("B0UND", Model::Slim, b"\x89PNG-bytes");
        let expected = b"--B0UND\r\nContent-Disposition: form-data; name=\"variant\"\r\n\r\nslim\r\n--B0UND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n\x89PNG-bytes\r\n--B0UND--\r\n";
        assert_eq!(body, expected.to_vec());
        assert!(String::from_utf8_lossy(&multipart("x", Model::Classic, b"")).contains("\r\n\r\nclassic\r\n"));
    }

    #[test]
    fn the_profile_names_the_active_skin_and_every_owned_cape() {
        let w = wardrobe(PROFILE.as_bytes()).unwrap();
        assert_eq!(w.skin_url.as_deref(), Some("http://textures.minecraft.net/texture/bb22"));
        assert_eq!(w.model, Model::Slim);
        assert_eq!(w.capes.len(), 2);
        assert_eq!((w.capes[0].id.as_str(), w.capes[0].alias.as_str(), w.capes[0].active), ("c1", "Migrator", false));
        assert_eq!(w.cape(), Some("c2"));
        let bare = wardrobe(br#"{"id":"x","name":"y"}"#).unwrap();
        assert_eq!(bare, Wardrobe::default());
        assert_eq!(bare.cape(), None);
        assert!(wardrobe(b"<html>").is_err());
    }

    #[test]
    fn statuses_map_to_what_a_person_can_do() {
        assert_eq!(failure(Call::Skin, 429), SkinError::RateLimited);
        assert_eq!(failure(Call::Cape, 429), SkinError::RateLimited);
        assert_eq!(failure(Call::Read, 401), SkinError::SignInAgain);
        assert_eq!(failure(Call::Skin, 400), SkinError::SkinRejected);
        assert_eq!(failure(Call::Cape, 400), SkinError::CapeRejected);
        assert_eq!(failure(Call::Cape, 404), SkinError::CapeRejected);
        assert_eq!(failure(Call::Read, 404), SkinError::Unavailable(404));
        assert_eq!(failure(Call::Skin, 503), SkinError::Unavailable(503));
    }

    #[test]
    fn errors_never_carry_the_token() {
        let (base, _) = serve(vec![("POST /minecraft/profile/skins", vec![(500, b"SECRET-MC-TOKEN".to_vec())])]);
        let s = services(&base);
        let e = s.upload(&skin(64, 64), Model::Classic).unwrap_err();
        assert!(!format!("{e:#} {e:?} {s:?}").contains("SECRET"), "{e:#}");
        let dead = Services { base: "http://127.0.0.1:1".into(), ..services(&base) };
        let e = dead.wardrobe().unwrap_err();
        assert!(!format!("{e:#} {e:?}").contains("SECRET"));
    }

    #[test]
    fn uploading_posts_multipart_with_the_bearer_token() {
        let (base, seen) = serve(vec![("POST /minecraft/profile/skins", vec![(200, json(PROFILE))])]);
        let png = skin(64, 64);
        let w = services(&base).upload(&png, Model::Slim).unwrap();
        assert_eq!(w.model, Model::Slim);
        let seen = seen.lock().unwrap();
        let req = &seen[0];
        assert_eq!((req.method.as_str(), req.path.as_str()), ("POST", "/minecraft/profile/skins"));
        assert!(req.headers.to_ascii_lowercase().contains("authorization: bearer secret-mc-token"), "{}", req.headers);
        let boundary = req.headers.lines().find_map(|l| l.to_ascii_lowercase().starts_with("content-type: multipart/form-data; boundary=").then(|| l.rsplit("boundary=").next().unwrap())).unwrap().trim();
        assert_eq!(req.body, multipart(boundary, Model::Slim, &png));
    }

    #[test]
    fn an_empty_answer_reads_the_profile_again() {
        let (base, seen) = serve(vec![("DELETE /minecraft/profile/capes/active", vec![(204, Vec::new())]), ("GET /minecraft/profile", vec![(200, json(NO_CAPE))])]);
        let w = services(&base).wear_cape(None).unwrap();
        assert_eq!(w.cape(), None);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.iter().map(|s| s.method.as_str()).collect::<Vec<_>>(), ["DELETE", "GET"]);
    }

    #[test]
    fn wearing_a_cape_puts_its_id() {
        let (base, seen) = serve(vec![("PUT /minecraft/profile/capes/active", vec![(200, json(PROFILE))])]);
        let w = services(&base).wear_cape(Some("c2")).unwrap();
        assert_eq!(w.cape(), Some("c2"));
        let body: serde_json::Value = serde_json::from_slice(&seen.lock().unwrap()[0].body).unwrap();
        assert_eq!(body, serde_json::json!({ "capeId": "c2" }));
    }

    #[test]
    fn a_rate_limit_is_its_own_error() {
        let (base, _) = serve(vec![("POST /minecraft/profile/skins", vec![(429, json(r#"{"path":"/minecraft/profile/skins","errorMessage":"Too many requests"}"#))])]);
        let e = services(&base).upload(&skin(64, 64), Model::Classic).unwrap_err();
        assert_eq!(e.downcast_ref::<SkinError>(), Some(&SkinError::RateLimited));
    }

    #[test]
    fn apply_stops_at_the_first_failure_and_reports_what_worked() {
        let (base, seen) = serve(vec![("POST /minecraft/profile/skins", vec![(200, json(NO_CAPE))]), ("PUT /minecraft/profile/capes/active", vec![(400, json("{}"))])]);
        let s = services(&base);
        let png = skin(64, 64);
        let done = apply(&s, Some((&png, Model::Classic)), Some(Some("zz")));
        assert!(done.skin_done);
        assert_eq!(done.wardrobe.as_ref().map(|w| w.model), Some(Model::Slim));
        assert_eq!(done.error.unwrap().downcast_ref::<SkinError>(), Some(&SkinError::CapeRejected));
        let (bad, _) = serve(vec![("POST /minecraft/profile/skins", vec![(400, json("{}"))])]);
        let done = apply(&services(&bad), Some((&png, Model::Classic)), Some(None));
        assert!(!done.skin_done && done.wardrobe.is_none());
        assert_eq!(done.error.unwrap().downcast_ref::<SkinError>(), Some(&SkinError::SkinRejected));
        let calls = seen.lock().unwrap().len();
        let only_cape = apply(&s, None, None);
        assert!(only_cape.wardrobe.is_none() && only_cape.error.is_none());
        assert_eq!(seen.lock().unwrap().len(), calls);
    }

    #[test]
    fn textures_load_only_from_the_texture_host() {
        let (base, _) = serve(vec![("GET /texture/bb22", vec![(200, skin(64, 64))])]);
        let s = services(&base);
        assert_eq!(s.texture("http://textures.minecraft.net/texture/bb22").unwrap().dimensions(), (64, 64));
        assert!(s.texture("https://evil.example/texture/bb22").is_err());
        assert_eq!(texture_hash("http://textures.minecraft.net/texture/AB12").as_deref(), Some("ab12"));
        assert_eq!(texture_hash("https://textures.minecraft.net/texture/../x"), None);
        assert_eq!(texture_hash("https://evil.example/texture/ab"), None);
    }

    #[test]
    fn front_and_back_show_the_right_faces() {
        let mut s = RgbaImage::new(64, 64);
        let fill = |s: &mut RgbaImage, x0: u32, y0: u32, w: u32, h: u32, c: [u8; 4]| {
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    s.put_pixel(x, y, Rgba(c));
                }
            }
        };
        fill(&mut s, 8, 8, 8, 8, [10, 0, 0, 255]);
        fill(&mut s, 24, 8, 8, 8, [20, 0, 0, 255]);
        fill(&mut s, 20, 20, 8, 12, [30, 0, 0, 255]);
        fill(&mut s, 36, 52, 4, 12, [40, 0, 0, 255]);
        fill(&mut s, 44, 20, 4, 12, [50, 0, 0, 255]);
        fill(&mut s, 40, 8, 1, 1, [99, 0, 0, 255]);
        let (front, back) = front_and_back(&s, Model::Classic);
        assert_eq!(front.dimensions(), (VIEW_W, VIEW_H));
        assert_eq!(front.get_pixel(4, 0).0, [99, 0, 0, 255]);
        assert_eq!(front.get_pixel(5, 0).0, [10, 0, 0, 255]);
        assert_eq!(back.get_pixel(5, 0).0, [20, 0, 0, 255]);
        assert_eq!(front.get_pixel(6, 10).0, [30, 0, 0, 255]);
        assert_eq!(front.get_pixel(0, 8).0, [50, 0, 0, 255]);
        assert_eq!(front.get_pixel(12, 8).0, [40, 0, 0, 255]);
        let (slim, _) = front_and_back(&s, Model::Slim);
        assert_eq!(slim.get_pixel(0, 8).0[3], 0);
        assert_eq!(slim.get_pixel(1, 8).0, [50, 0, 0, 255]);
        assert_eq!(slim.get_pixel(15, 8).0[3], 0);
    }

    #[test]
    fn legacy_skins_mirror_the_right_limbs_and_drop_a_solid_hat() {
        let mut s = RgbaImage::from_pixel(64, 32, Rgba([0, 0, 0, 255]));
        s.put_pixel(44, 20, Rgba([7, 0, 0, 255]));
        let (front, _) = front_and_back(&s, Model::Classic);
        assert_eq!(front.get_pixel(0, 8).0, [7, 0, 0, 255]);
        assert_eq!(front.get_pixel(15, 8).0, [7, 0, 0, 255]);
        s.put_pixel(8, 8, Rgba([5, 5, 5, 255]));
        let (front, _) = front_and_back(&s, Model::Classic);
        assert_eq!(front.get_pixel(4, 0).0, [5, 5, 5, 255]);
    }

    #[test]
    fn a_cape_shows_its_outside_at_any_resolution() {
        let mut c = RgbaImage::new(64, 32);
        c.put_pixel(1, 1, Rgba([1, 2, 3, 255]));
        let front = cape_front(&c).unwrap();
        assert_eq!((front.dimensions(), front.get_pixel(0, 0).0), ((CAPE_W, CAPE_H), [1, 2, 3, 255]));
        let mut hd = RgbaImage::new(128, 64);
        hd.put_pixel(2, 2, Rgba([4, 5, 6, 255]));
        assert_eq!(cape_front(&hd).unwrap().get_pixel(0, 0).0, [4, 5, 6, 255]);
        assert!(cape_front(&RgbaImage::new(22, 17)).is_none());
    }

    #[test]
    fn empty_outer_arm_columns_mean_slim() {
        let mut s = RgbaImage::from_pixel(64, 64, Rgba([1, 1, 1, 255]));
        assert_eq!(guess_model(&s), Model::Classic);
        for y in 20..32 {
            s.put_pixel(54, y, Rgba([0; 4]));
            s.put_pixel(55, y, Rgba([0; 4]));
        }
        assert_eq!(guess_model(&s), Model::Classic);
        for y in 52..64 {
            s.put_pixel(46, y, Rgba([0; 4]));
            s.put_pixel(47, y, Rgba([0; 4]));
        }
        assert_eq!(guess_model(&s), Model::Slim);
        assert_eq!(guess_model(&RgbaImage::new(64, 32)), Model::Classic);
    }

    #[test]
    fn the_same_picture_in_another_encoding_is_kept_once() {
        let dir = tempfile::tempdir().unwrap();
        let image = RgbaImage::from_pixel(64, 64, Rgba([3, 4, 5, 255]));
        let plain = png(&image).unwrap();
        let mut other = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(&mut other, image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Paeth).write_image(image.as_raw(), 64, 64, image::ExtendedColorType::Rgba8).unwrap();
        assert_ne!(plain, other);
        remember_in(dir.path(), &plain, Model::Classic).unwrap();
        remember_in(dir.path(), &other, Model::Slim).unwrap();
        let kept = recent_in(dir.path());
        assert_eq!((kept.len(), kept[0].model), (1, Model::Slim));
    }

    #[test]
    fn recent_skins_keep_the_newest_six_without_doubles() {
        let dir = tempfile::tempdir().unwrap();
        let skins: Vec<Vec<u8>> = (0..8u8).map(|n| png(&RgbaImage::from_pixel(64, 64, Rgba([n, 0, 0, 255]))).unwrap()).collect();
        for s in &skins {
            remember_in(dir.path(), s, Model::Classic).unwrap();
        }
        let kept = recent_in(dir.path());
        assert_eq!(kept.len(), KEPT);
        assert_eq!(std::fs::read(&kept[0].path).unwrap(), skins[7]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), KEPT + 1);
        remember_in(dir.path(), &skins[3], Model::Slim).unwrap();
        let kept = recent_in(dir.path());
        assert_eq!((kept.len(), kept[0].model), (KEPT, Model::Slim));
        assert_eq!(std::fs::read(&kept[0].path).unwrap(), skins[3]);
        assert_eq!(std::fs::read(&kept[1].path).unwrap(), skins[7]);
        std::fs::remove_file(&kept[1].path).unwrap();
        assert_eq!(recent_in(dir.path()).len(), KEPT - 1);
        std::fs::write(dir.path().join("recent.json"), r#"[{"file":"../../evil.png","model":"slim"}]"#).unwrap();
        assert!(recent_in(dir.path()).is_empty());
        assert!(recent_in(&dir.path().join("none")).is_empty());
    }
}
