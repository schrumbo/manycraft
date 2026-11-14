//! Player heads for the account lists: vanilla's Steve or Alex for offline accounts, the real skin for Microsoft ones.
//! Heads live as small PNGs in the cache folder; the session server is asked off the UI thread and at most once a day.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use image::{ImageFormat, Rgba, RgbaImage};
use serde::Deserialize;
use uuid::Uuid;

use super::account::{Account, AccountKind};
use super::{drive, net, paths};

const SESSION_PROFILE: &str = "https://sessionserver.mojang.com/session/minecraft/profile/";
const TEXTURE_HOST: &str = "textures.minecraft.net";
const FRESH_FOR: Duration = Duration::from_secs(24 * 3600);
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
const MAX_SKIN: u64 = 256 << 10;
const SCALE: u32 = 8;

static HEADS: LazyLock<Heads> = LazyLock::new(Heads::default);
static PREPARING: Mutex<()> = Mutex::new(());
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Counts how often a head appeared or changed, so a view can tell cheaply whether its cached rows are stale.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

#[derive(Default)]
struct Heads {
    ready: Mutex<HashMap<Uuid, PathBuf>>,
    next_try: Mutex<HashMap<Uuid, Instant>>,
    blocked: Mutex<HashMap<Option<String>, Instant>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The PNG of `account`'s head, once [`prepare`] has made one; never touches the disk or the network.
pub fn head(account: &Account) -> Option<PathBuf> {
    lock(&HEADS.ready).get(&account.id).cloned()
}

/// Makes heads for `accounts` that have none yet. Blocking: run it off the UI thread.
/// Returns whether any head appeared or changed, so the caller knows to redraw.
pub fn prepare(accounts: &[Account]) -> bool {
    let Ok(_one_at_a_time) = PREPARING.try_lock() else { return false };
    prepare_in(&paths::data_dir().join("cache").join("skins"), accounts, &HEADS, &fetch_skin)
}

fn prepare_in(dir: &Path, accounts: &[Account], heads: &Heads, fetch: &dyn Fn(&Account) -> Result<Option<(String, Vec<u8>)>>) -> bool {
    let mut changed = false;
    let fetch = |account: &Account| {
        let route = account.proxy.clone();
        if lock(&heads.blocked).get(&route).is_some_and(|until| Instant::now() < *until) {
            bail!("skipped after an earlier failure on the same route");
        }
        let got = fetch(account);
        if got.is_err() {
            lock(&heads.blocked).insert(route, Instant::now() + RETRY_AFTER);
        }
        got
    };
    for account in accounts {
        let path = match account.kind {
            AccountKind::Offline => default_head(dir, account.id),
            AccountKind::Microsoft => real_head(dir, account, heads, &fetch),
        };
        let Some(path) = path else { continue };
        if lock(&heads.ready).insert(account.id, path.clone()).as_ref() != Some(&path) {
            changed = true;
        }
    }
    if changed {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
    changed
}

/// The cached head PNG of `account`'s own skin, read from disk only; `None` for offline accounts and before the first fetch.
pub fn own_head(account: &Account) -> Option<PathBuf> {
    own_head_in(&paths::data_dir().join("cache").join("skins"), account)
}

fn own_head_in(dir: &Path, account: &Account) -> Option<PathBuf> {
    if account.kind != AccountKind::Microsoft {
        return None;
    }
    let hash = std::fs::read_to_string(dir.join(format!("{}.skin", account.id.simple()))).ok()?;
    let hash = hash.trim();
    valid_hash(hash).then(|| dir.join(format!("head-{hash}.png"))).filter(|p| p.is_file())
}

fn real_head(dir: &Path, account: &Account, heads: &Heads, fetch: &dyn Fn(&Account) -> Result<Option<(String, Vec<u8>)>>) -> Option<PathBuf> {
    let index = dir.join(format!("{}.skin", account.id.simple()));
    let known = std::fs::read_to_string(&index).ok().map(|h| h.trim().to_owned()).filter(|h| valid_hash(h)).map(|h| dir.join(format!("head-{h}.png"))).filter(|p| p.is_file());
    let fresh = std::fs::metadata(&index).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age < FRESH_FOR);
    if known.is_some() && fresh {
        return known;
    }
    let fallback = || known.clone().or_else(|| default_head(dir, account.id));
    if lock(&heads.next_try).get(&account.id).is_some_and(|at| Instant::now() < *at) {
        return fallback();
    }
    let (shown, wait) = match fetch(account).and_then(|got| got.map(|(hash, skin)| store_head(dir, &index, &hash, &skin)).transpose()) {
        Ok(Some(path)) => (Some(path), FRESH_FOR),
        Ok(None) => (fallback(), FRESH_FOR),
        Err(_) => (fallback(), RETRY_AFTER),
    };
    lock(&heads.next_try).insert(account.id, Instant::now() + wait);
    shown
}

fn store_head(dir: &Path, index: &Path, hash: &str, skin: &[u8]) -> Result<PathBuf> {
    let path = dir.join(format!("head-{hash}.png"));
    if !path.is_file() {
        let decoded = image::load_from_memory_with_format(skin, ImageFormat::Png).context("the skin is not a PNG")?.to_rgba8();
        let face = face(&decoded).context("the skin has an unexpected size")?;
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        net::write_atomic(&path, &png(&scaled(&face))?)?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    net::write_atomic(index, hash.as_bytes())?;
    Ok(path)
}

fn default_head(dir: &Path, id: Uuid) -> Option<PathBuf> {
    let alex = default_is_alex(id);
    let path = dir.join(if alex { "default-alex.png" } else { "default-steve.png" });
    if !path.is_file() {
        std::fs::create_dir_all(dir).ok()?;
        let bytes = png(&scaled(&default_face(alex))).ok()?;
        net::write_atomic(&path, &bytes).ok()?;
    }
    Some(path)
}

/// Whether vanilla gives the profile `id` the slim default skin (Alex) rather than Steve: the low bit of Java's `UUID.hashCode`.
pub fn default_is_alex(id: Uuid) -> bool {
    let (hi, lo) = id.as_u64_pair();
    let both = hi ^ lo;
    (((both >> 32) as u32) ^ both as u32) & 1 == 1
}

fn fetch_skin(account: &Account) -> Result<Option<(String, Vec<u8>)>> {
    if drive::fenced("skin fetch", &account.name) {
        return Ok(None);
    }
    let agent = net::agent(account.proxy.as_deref())?;
    let profile: Profile = net::get_json(&agent, &format!("{SESSION_PROFILE}{}", account.id.simple()))?;
    let Some(hash) = skin_hash(&profile) else { return Ok(None) };
    let url = format!("https://{TEXTURE_HOST}/texture/{hash}");
    let mut reply = agent.get(&url).call().with_context(|| format!("cannot load the skin of {}", account.name))?;
    let bytes = reply.body_mut().with_config().limit(MAX_SKIN).read_to_vec().context("cannot read the skin")?;
    Ok(Some((hash, bytes)))
}

#[derive(Deserialize)]
struct Profile {
    #[serde(default)]
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
    skin: Option<Skin>,
}

#[derive(Deserialize)]
struct Skin {
    url: String,
}

fn valid_hash(hash: &str) -> bool {
    !hash.is_empty() && hash.len() <= 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The texture id of the skin the session server names, only when it lives on Mojang's texture host.
fn skin_hash(profile: &Profile) -> Option<String> {
    let value = &profile.properties.iter().find(|p| p.name == "textures")?.value;
    let json = base64_decode(value)?;
    let textures: Textures = serde_json::from_slice(&json).ok()?;
    let url = textures.textures.skin?.url;
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let hash = path.strip_prefix("texture/")?;
    (host == TEXTURE_HOST && valid_hash(hash)).then(|| hash.to_ascii_lowercase())
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut have) = (0u32, 0u32);
    for b in text.bytes().filter(|b| *b != b'=' && !b.is_ascii_whitespace()) {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(v);
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    Some(out)
}

/// The face of a skin with its hat layer on top, 8 × 8 pixels; `None` for an image that is not a skin.
fn face(skin: &RgbaImage) -> Option<RgbaImage> {
    if skin.width() < 64 || skin.height() < 16 {
        return None;
    }
    let mut out = RgbaImage::new(8, 8);
    for y in 0..8 {
        for x in 0..8 {
            let base = skin.get_pixel(8 + x, 8 + y).0;
            let hat = skin.get_pixel(40 + x, 8 + y).0;
            let a = u32::from(hat[3]);
            let mix = |i: usize| ((u32::from(hat[i]) * a + u32::from(base[i]) * (255 - a)) / 255) as u8;
            out.put_pixel(x, y, Rgba([mix(0), mix(1), mix(2), 255]));
        }
    }
    Some(out)
}

fn scaled(face: &RgbaImage) -> RgbaImage {
    RgbaImage::from_fn(face.width() * SCALE, face.height() * SCALE, |x, y| *face.get_pixel(x / SCALE, y / SCALE))
}

fn png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).context("cannot encode the head")?;
    if bytes.is_empty() {
        bail!("empty head image");
    }
    Ok(bytes)
}

const STEVE: [&str; 8] = ["hhhhhhhh", "hhhhhhhh", "hssssssh", "ssssssss", "webssbew", "ssssssss", "sshnnhss", "sshhhhss"];
const ALEX: [&str; 8] = ["oooooooo", "oooooooo", "osssssso", "ssssssss", "wgssssgw", "ssssssss", "sssnnsss", "ssssssss"];

fn default_face(alex: bool) -> RgbaImage {
    let rows = if alex { &ALEX } else { &STEVE };
    RgbaImage::from_fn(8, 8, |x, y| {
        let c = rows[y as usize].as_bytes()[x as usize];
        Rgba(match (c, alex) {
            (b'h', _) => [0x3b, 0x26, 0x15, 255],
            (b'o', _) => [0xc2, 0x6a, 0x1b, 255],
            (b'w', _) => [0xff, 0xff, 0xff, 255],
            (b'e' | b'b', _) => [0x4a, 0x3a, 0x8f, 255],
            (b'g', _) => [0x3f, 0x8a, 0x3a, 255],
            (b'n', true) => [0xb0, 0x7a, 0x5c, 255],
            (b'n', false) => [0x8a, 0x5a, 0x3c, 255],
            (_, true) => [0xe8, 0xb4, 0x92, 255],
            (_, false) => [0xc6, 0x8d, 0x6b, 255],
        })
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn microsoft(n: u128) -> Account {
        Account { id: Uuid::from_u128(n), name: format!("P{n}"), kind: AccountKind::Microsoft, proxy: None, color: None }
    }

    fn skin_png(face: [u8; 4], hat: [u8; 4]) -> Vec<u8> {
        let mut skin = RgbaImage::new(64, 64);
        for y in 0..8 {
            for x in 0..8 {
                skin.put_pixel(8 + x, 8 + y, Rgba(face));
                skin.put_pixel(40 + x, 8 + y, Rgba(hat));
            }
        }
        png(&skin).unwrap()
    }

    #[test]
    fn vanilla_picks_alex_by_the_low_bit_of_the_java_hash() {
        assert!(default_is_alex(Uuid::from_u128(1 << 64)));
        assert!(!default_is_alex(Uuid::from_u128(0)));
        assert!(!default_is_alex(Uuid::from_u128(3 << 64 | 3)));
        assert!(default_is_alex(Uuid::from_u128(1 << 96)));
        assert!(default_is_alex(Uuid::from_u128(2 << 64 | 3)));
    }

    #[test]
    fn the_face_shows_the_hat_where_it_is_opaque() {
        let mut skin = RgbaImage::from_pixel(64, 64, Rgba([10, 20, 30, 255]));
        skin.put_pixel(40, 8, Rgba([200, 0, 0, 255]));
        skin.put_pixel(41, 8, Rgba([0, 0, 0, 0]));
        let face = face(&skin).unwrap();
        assert_eq!(face.get_pixel(0, 0).0, [200, 0, 0, 255]);
        assert_eq!(face.get_pixel(1, 0).0, [10, 20, 30, 255]);
        assert!(super::face(&RgbaImage::new(32, 8)).is_none());
    }

    #[test]
    fn a_session_profile_names_the_skin_only_on_mojangs_host() {
        let profile = |url: &str| {
            let json = format!(r#"{{"textures":{{"SKIN":{{"url":"{url}"}}}}}}"#);
            let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut text = String::new();
            for chunk in json.as_bytes().chunks(3) {
                let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
                for i in 0..=chunk.len() {
                    text.push(table[(n >> (18 - 6 * i) & 63) as usize] as char);
                }
                text.extend(std::iter::repeat_n('=', 3 - chunk.len()));
            }
            Profile { properties: vec![Property { name: "textures".into(), value: text }] }
        };
        assert_eq!(skin_hash(&profile("http://textures.minecraft.net/texture/AB12cd")).as_deref(), Some("ab12cd"));
        assert_eq!(skin_hash(&profile("https://evil.example/texture/ab12")), None);
        assert_eq!(skin_hash(&profile("https://textures.minecraft.net.evil.example/texture/ab12")), None);
        assert_eq!(skin_hash(&profile("https://textures.minecraft.net/texture/../../x")), None);
        assert_eq!(skin_hash(&Profile { properties: Vec::new() }), None);
    }

    #[test]
    fn base64_reads_padded_and_unpadded_text() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert!(base64_decode("a$b").is_none());
    }

    #[test]
    fn offline_accounts_get_a_default_and_microsoft_ones_their_skin_once() {
        let dir = tempfile::tempdir().unwrap();
        let heads = Heads::default();
        let offline = Account::offline("Steve").unwrap();
        let ms = microsoft(7);
        let calls = Cell::new(0);
        let fetch = |_: &Account| {
            calls.set(calls.get() + 1);
            Ok(Some(("abc123".to_owned(), skin_png([1, 2, 3, 255], [0, 0, 0, 0]))))
        };
        let before = generation();
        assert!(prepare_in(dir.path(), &[offline.clone(), ms.clone()], &heads, &fetch));
        assert!(generation() > before);
        let default = lock(&heads.ready).get(&offline.id).cloned().unwrap();
        assert!(default.file_name().unwrap().to_string_lossy().starts_with("default-"));
        let own = lock(&heads.ready).get(&ms.id).cloned().unwrap();
        assert_eq!(own, dir.path().join("head-abc123.png"));
        assert_eq!(own_head_in(dir.path(), &ms), Some(own.clone()));
        assert_eq!(own_head_in(dir.path(), &offline), None);
        let drawn = image::open(&own).unwrap().to_rgba8();
        assert_eq!((drawn.width(), drawn.height(), drawn.get_pixel(63, 63).0), (64, 64, [1, 2, 3, 255]));
        assert!(!prepare_in(dir.path(), &[offline, ms], &heads, &fetch));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_failed_fetch_shows_the_default_and_waits_before_asking_again() {
        let dir = tempfile::tempdir().unwrap();
        let heads = Heads::default();
        let ms = microsoft(8);
        let calls = Cell::new(0);
        let fetch = |_: &Account| {
            calls.set(calls.get() + 1);
            bail!("offline")
        };
        assert!(prepare_in(dir.path(), std::slice::from_ref(&ms), &heads, &fetch));
        let shown = lock(&heads.ready).get(&ms.id).cloned().unwrap();
        assert!(shown.file_name().unwrap().to_string_lossy().starts_with("default-"));
        prepare_in(dir.path(), std::slice::from_ref(&ms), &heads, &fetch);
        assert_eq!(calls.get(), 1);
        lock(&heads.next_try).insert(ms.id, Instant::now());
        lock(&heads.blocked).clear();
        prepare_in(dir.path(), std::slice::from_ref(&ms), &heads, &fetch);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn an_account_without_a_skin_keeps_the_default_for_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let heads = Heads::default();
        let ms = microsoft(9);
        let calls = Cell::new(0);
        let fetch = |_: &Account| {
            calls.set(calls.get() + 1);
            Ok(None)
        };
        prepare_in(dir.path(), std::slice::from_ref(&ms), &heads, &fetch);
        assert!(lock(&heads.ready).contains_key(&ms.id));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn the_defaults_differ_and_fill_the_whole_face() {
        let (steve, alex) = (default_face(false), default_face(true));
        assert_ne!(steve, alex);
        assert!(steve.pixels().chain(alex.pixels()).all(|p| p.0[3] == 255));
    }

    #[test]
    fn a_stored_head_is_trusted_for_a_day_and_fetched_again_after_that() {
        let dir = tempfile::tempdir().unwrap();
        let ms = microsoft(10);
        let calls = Cell::new(0);
        let fetch = |_: &Account| {
            calls.set(calls.get() + 1);
            Ok(Some(("def456".to_owned(), skin_png([4, 5, 6, 255], [0, 0, 0, 0]))))
        };
        prepare_in(dir.path(), std::slice::from_ref(&ms), &Heads::default(), &fetch);
        assert_eq!(calls.get(), 1);
        let index = dir.path().join(format!("{}.skin", ms.id.simple()));
        let age = |hours: u64| std::fs::File::options().write(true).open(&index).unwrap().set_modified(SystemTime::now() - Duration::from_secs(hours * 3600)).unwrap();
        age(23);
        prepare_in(dir.path(), std::slice::from_ref(&ms), &Heads::default(), &fetch);
        assert_eq!(calls.get(), 1);
        age(25);
        prepare_in(dir.path(), std::slice::from_ref(&ms), &Heads::default(), &fetch);
        assert_eq!(calls.get(), 2);
        std::fs::remove_file(dir.path().join("head-def456.png")).unwrap();
        prepare_in(dir.path(), std::slice::from_ref(&ms), &Heads::default(), &fetch);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn a_failure_stops_the_pass_and_the_wait_is_ten_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let heads = Heads::default();
        let accounts = [microsoft(11), microsoft(12), microsoft(13)];
        let calls = Cell::new(0);
        let fetch = |_: &Account| {
            calls.set(calls.get() + 1);
            bail!("offline")
        };
        prepare_in(dir.path(), &accounts, &heads, &fetch);
        assert_eq!(calls.get(), 1);
        assert!(accounts.iter().all(|a| lock(&heads.ready).contains_key(&a.id)));
        let wait = lock(&heads.next_try).get(&accounts[0].id).unwrap().saturating_duration_since(Instant::now());
        assert!(wait > Duration::from_secs(9 * 60) && wait <= Duration::from_secs(10 * 60), "{wait:?}");
    }

    #[test]
    fn the_head_is_known_once_prepared_and_a_busy_pass_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let account = microsoft(0xfeed_0000_0000_0000_0000_0000_0000_0001);
        assert_eq!(head(&account), None);
        prepare_in(dir.path(), std::slice::from_ref(&account), &HEADS, &|_| Ok(None));
        assert!(head(&account).is_some_and(|p| p.starts_with(dir.path())));
        let _busy = lock(&PREPARING);
        assert!(!prepare(&[account]));
    }

    #[test]
    fn the_limits_are_a_day_ten_minutes_and_a_quarter_megabyte() {
        assert_eq!(FRESH_FOR, Duration::from_secs(86_400));
        assert_eq!(RETRY_AFTER, Duration::from_secs(600));
        assert_eq!(MAX_SKIN, 262_144);
    }
}
