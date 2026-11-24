//! The wardrobe: every skin the person keeps, in one library on disk, and what each account wears and owns, cached so
//! the page opens at once and refreshed in the background.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use uuid::Uuid;

use super::account::Account;
use super::skin_change::{self as change, MAX_PNG, Model, Services, SkinError};
use super::{auth, drive, lock, net, paths, proxy, skin, skins};

mod canned;

const LOOKUP: &str = "https://api.minecraftservices.com/minecraft/profile/lookup/name/";
const NAME_MAX: usize = 48;
/// How long a cached outfit counts as current; the page asks again after that.
pub const FRESH: u64 = 10 * 60;

/// Whether an outfit fetched at `fetched` (Unix seconds) counts as current.
pub fn fresh(fetched: u64) -> bool {
    now().saturating_sub(fetched) < FRESH
}

/// What a long call to Minecraft services is doing right now, for the page to say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    SignIn,
    Profile,
    Capes { done: usize, total: usize },
    Upload,
    Saving,
}

/// Where a skin of the library came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    File,
    Link,
    Player,
    Worn,
    Recent,
}

/// A skin of the library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skin {
    /// The file name inside the library, the SHA-1 of the pixels.
    pub file: String,
    pub name: String,
    pub model: Model,
    pub origin: Origin,
    /// The player, link host or account the skin came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default)]
    pub added: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    skins: Vec<Skin>,
    #[serde(default)]
    seen: Vec<String>,
}

/// What [`add`] did with a skin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Added {
    pub skin: Skin,
    /// False when the library already held these pixels.
    pub new: bool,
}

/// Why a skin could not be fetched, for the cases a person can act on. Found with `downcast_ref`.
#[derive(Debug, PartialEq, Eq)]
pub enum AddError {
    BadLink,
    NotAnImage,
    BadName,
    NoSuchPlayer,
    NoSkin,
    RateLimited,
    Unreachable,
}

impl std::fmt::Display for AddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadLink => "not a web address",
            Self::NotAnImage => "the link does not lead to an image",
            Self::BadName => "not a player name",
            Self::NoSuchPlayer => "no such player",
            Self::NoSkin => "the player has no skin of their own",
            Self::RateLimited => "too many requests",
            Self::Unreachable => "cannot reach the server",
        })
    }
}

impl std::error::Error for AddError {}

/// A skin fetched or read, not yet in the library.
#[derive(Clone, Debug)]
pub struct Fetched {
    pub png: Vec<u8>,
    pub model: Model,
    pub name: String,
    pub origin: Origin,
    pub from: Option<String>,
}

fn library_dir() -> PathBuf {
    paths::data_dir().join("skins").join("library")
}

fn cache_dir() -> PathBuf {
    paths::data_dir().join("cache").join("skins")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn pixel_hash(image: &RgbaImage) -> String {
    let digest = Sha1::new().chain_update(image.width().to_le_bytes()).chain_update(image.height().to_le_bytes()).chain_update(image.as_raw()).finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The name a skin with the pixels of `image` has in the library.
pub fn file_of(image: &RgbaImage) -> String {
    format!("{}.png", pixel_hash(image))
}

fn valid_file(name: &str) -> bool {
    name.len() == 44 && name.ends_with(".png") && name[..40].bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_texture(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn clean(name: &str, fallback: &str) -> String {
    let name: String = name.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(NAME_MAX).collect();
    if name.is_empty() { fallback.to_owned() } else { name }
}

fn gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

fn stored_index(dir: &Path) -> Result<Index> {
    let path = dir.join("library.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if gone(&e) => return Ok(Index::default()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_slice(&bytes) else {
        let _ = std::fs::copy(&path, dir.join("library.json.bad"));
        return Ok(Index::default());
    };
    let skins = match map.remove("skins") {
        Some(serde_json::Value::Array(items)) => items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect(),
        _ => Vec::new(),
    };
    let seen = map.remove("seen").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    Ok(Index { skins, seen })
}

fn read_index(dir: &Path) -> Result<Index> {
    let mut index = stored_index(dir)?;
    index.skins.retain(|s| valid_file(&s.file) && std::fs::metadata(dir.join(&s.file)).map_or_else(|e| !gone(&e), |m| m.is_file()));
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if gone(&e) => return Ok(index),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut found: Vec<Skin> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            if !valid_file(&file) || index.skins.iter().any(|s| s.file == file) {
                return None;
            }
            let image = change::check_png(&std::fs::read(entry.path()).ok()?).ok()?;
            let added = entry.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some(Skin { file, name: "Skin".into(), model: change::guess_model(&image), origin: Origin::File, from: None, added })
        })
        .collect();
    found.sort_by_key(|s| std::cmp::Reverse(s.added));
    index.skins.extend(found);
    Ok(index)
}

fn write_index(dir: &Path, index: &Index) -> Result<()> {
    net::write_atomic(&dir.join("library.json"), &serde_json::to_vec_pretty(index)?)
}

fn locked<T>(dir: &Path, work: impl FnOnce(&mut Index) -> Result<T>) -> Result<T> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let _guard = lock::exclusive(&dir.join("library.lock"))?;
    let mut index = read_index(dir)?;
    let out = work(&mut index)?;
    write_index(dir, &index)?;
    Ok(out)
}

/// Every skin of the library, newest first.
pub fn list() -> Vec<Skin> {
    read_index(&library_dir()).map(|i| i.skins).unwrap_or_default()
}

/// The file of `skin`, for uploading.
pub fn bytes(skin: &Skin) -> Result<Vec<u8>> {
    std::fs::read(library_dir().join(&skin.file)).with_context(|| format!("cannot read {}", skin.file))
}

/// Puts a skin into the library, or finds it there when the pixels are the same; the library never forgets a skin by itself.
pub fn add(fetched: &Fetched) -> Result<Added> {
    add_in(&library_dir(), fetched)
}

fn add_in(dir: &Path, fetched: &Fetched) -> Result<Added> {
    let image = change::check_png(&fetched.png)?;
    let file = file_of(&image);
    locked(dir, |index| {
        if let Some(have) = index.skins.iter().find(|s| s.file == file) {
            return Ok(Added { skin: have.clone(), new: false });
        }
        net::write_atomic(&dir.join(&file), &fetched.png)?;
        let skin = Skin { file, name: clean(&fetched.name, "Skin"), model: fetched.model, origin: fetched.origin, from: fetched.from.clone(), added: now() };
        index.skins.insert(0, skin.clone());
        Ok(Added { skin, new: true })
    })
}

/// Takes a skin out of the library; accounts that wear it keep wearing it.
pub fn remove(file: &str) -> Result<()> {
    remove_in(&library_dir(), file)
}

fn remove_in(dir: &Path, file: &str) -> Result<()> {
    if !valid_file(file) {
        return Ok(());
    }
    locked(dir, |index| {
        index.skins.retain(|s| s.file != file);
        let _ = std::fs::remove_file(dir.join(file));
        Ok(())
    })
}

/// Puts a removed skin back at place `at` of the library, with the name and arms it had.
pub fn restore(skin: &Skin, png: &[u8], at: usize) -> Result<()> {
    restore_in(&library_dir(), skin, png, at)
}

fn restore_in(dir: &Path, skin: &Skin, png: &[u8], at: usize) -> Result<()> {
    if !valid_file(&skin.file) {
        return Ok(());
    }
    change::check_png(png)?;
    locked(dir, |index| {
        if index.skins.iter().any(|s| s.file == skin.file) {
            return Ok(());
        }
        net::write_atomic(&dir.join(&skin.file), png)?;
        index.skins.insert(at.min(index.skins.len()), skin.clone());
        Ok(())
    })
}

/// Renames a skin of the library.
pub fn rename(file: &str, name: &str) -> Result<()> {
    rename_in(&library_dir(), file, name)
}

fn rename_in(dir: &Path, file: &str, name: &str) -> Result<()> {
    edit(dir, file, |s| s.name = clean(name, &s.name))
}

/// Sets the arm model a skin of the library is worn with.
pub fn set_model(file: &str, model: Model) -> Result<()> {
    edit(&library_dir(), file, |s| s.model = model)
}

fn edit(dir: &Path, file: &str, change: impl FnOnce(&mut Skin)) -> Result<()> {
    locked(dir, |index| {
        if let Some(skin) = index.skins.iter_mut().find(|s| s.file == file) {
            change(skin);
        }
        Ok(())
    })
}

/// Copies the skins `account` wore lately, kept by the old skin sheet, into the library, once.
pub fn adopt_recent(account: &Account) {
    let marker = paths::data_dir().join("skins").join(account.id.simple().to_string()).join("in-library");
    if marker.is_file() {
        return;
    }
    let mut all = true;
    for recent in change::recent(account.id) {
        let added = std::fs::read(&recent.path)
            .map_err(anyhow::Error::from)
            .and_then(|png| add(&Fetched { png, model: recent.model, name: format!("{} (früher)", account.name), origin: Origin::Recent, from: Some(account.name.clone()) }));
        all &= added.is_ok();
    }
    if all {
        let _ = net::write_atomic(&marker, b"1");
    }
}

/// Reads and checks a skin file the person picked.
pub fn from_file(path: &Path) -> Result<Fetched> {
    let (png, image) = change::read_png(path)?;
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Fetched { png, model: change::guess_model(&image), name, origin: Origin::File, from: None })
}

fn status(e: &anyhow::Error) -> Option<u16> {
    e.chain().find_map(|c| match c.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::StatusCode(code)) => Some(*code),
        _ => None,
    })
}

fn reach(e: anyhow::Error) -> anyhow::Error {
    match status(&e) {
        Some(429) => AddError::RateLimited.into(),
        _ if net::no_connection(&e) => AddError::Unreachable.into(),
        _ => e,
    }
}

/// The address a person typed, as a web address: `https` is assumed when the scheme is missing, and nothing but `https` is taken.
pub fn parse_link(input: &str) -> Result<url::Url, AddError> {
    let text = input.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return Err(AddError::BadLink);
    }
    let full = if text.contains("://") { text.to_owned() } else { format!("https://{text}") };
    let url = url::Url::parse(&full).map_err(|_| AddError::BadLink)?;
    (url.scheme() == "https" && url.host_str().is_some_and(|h| h.contains('.'))).then_some(url).ok_or(AddError::BadLink)
}

/// Downloads the skin behind a link to a PNG.
pub fn fetch_link(input: &str) -> Result<Fetched> {
    let url = parse_link(input)?;
    let host = url.host_str().unwrap_or_default().to_owned();
    if drive::fenced("skin link", &url) {
        return canned::fetched(url.as_str(), Origin::Link, Some(host));
    }
    let name = url.path_segments().and_then(|mut s| s.next_back()).map(|s| proxy::percent_decode(s.trim_end_matches(".png"))).filter(|s| !s.is_empty()).unwrap_or_else(|| host.clone());
    let agent = net::agent_https(None)?;
    let mut reply = agent.get(url.as_str()).call().map_err(|e| match e {
        ureq::Error::StatusCode(404 | 410) => AddError::NotAnImage.into(),
        e => reach(e.into()),
    })?;
    let png = match reply.body_mut().with_config().limit(MAX_PNG + 1).read_to_vec() {
        Ok(png) => png,
        Err(ureq::Error::BodyExceedsLimit(_)) => return Err(SkinError::TooLarge.into()),
        Err(e) => return Err(reach(e.into())),
    };
    let image = change::check_png(&png).map_err(|e| if matches!(e, SkinError::NotPng) { anyhow::Error::from(AddError::NotAnImage) } else { e.into() })?;
    Ok(Fetched { model: change::guess_model(&image), png, name, origin: Origin::Link, from: Some(host) })
}

#[derive(Deserialize)]
struct Found {
    id: Uuid,
    name: String,
}

/// Downloads the skin a player wears, found by name or id through Mojang's public profile services.
pub fn fetch_player(input: &str) -> Result<Fetched> {
    let typed = input.trim().trim_start_matches('@').to_owned();
    let by_id = Uuid::parse_str(&typed).ok();
    if by_id.is_none() && (typed.is_empty() || typed.len() > 16 || !typed.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')) {
        return Err(AddError::BadName.into());
    }
    if drive::fenced("skin player", &typed) {
        return canned::fetched(&typed, Origin::Player, Some(typed.clone()));
    }
    let agent = net::agent(None)?;
    let (id, name) = match by_id {
        Some(id) => (id, typed),
        None => {
            let found: Found = net::get_json(&agent, &format!("{LOOKUP}{typed}")).map_err(|e| if net::not_found(&e) { AddError::NoSuchPlayer.into() } else { reach(e) })?;
            (found.id, found.name)
        }
    };
    let skin = skins::profile_skin(&agent, id).map_err(reach)?.ok_or(AddError::NoSkin)?;
    change::check_png(&skin.png)?;
    Ok(Fetched { png: skin.png, model: skin.model, name: name.clone(), origin: Origin::Player, from: Some(name) })
}

/// A cape an account owns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owned {
    pub id: String,
    pub alias: String,
    pub texture: String,
}

/// What an account wears and owns, as Minecraft services last told.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outfit {
    /// The texture id of the skin worn.
    pub skin: Option<String>,
    pub model: Model,
    pub capes: Vec<Owned>,
    /// The id of the cape worn.
    pub cape: Option<String>,
    /// When Minecraft services told, in Unix seconds.
    pub fetched: u64,
}

fn outfit_path(id: Uuid) -> PathBuf {
    cache_dir().join(format!("outfit-{}.json", id.simple()))
}

fn texture_path(texture: &str) -> PathBuf {
    cache_dir().join(format!("texture-{texture}.png"))
}

/// What `id` wore when the page last asked, from disk only.
pub fn cached(id: Uuid) -> Option<Outfit> {
    serde_json::from_slice(&std::fs::read(outfit_path(id)).ok()?).ok()
}

/// The skin or cape picture with texture id `texture`, from the cache only.
pub fn texture(texture: &str) -> Option<RgbaImage> {
    if !valid_texture(texture) {
        return None;
    }
    image::open(texture_path(texture)).ok().map(|i| i.into_rgba8())
}

fn keep_texture(texture: &str, picture: &RgbaImage) -> Result<()> {
    let path = texture_path(texture);
    if !path.is_file() && valid_texture(texture) {
        net::write_atomic(&path, &change::png(picture)?)?;
    }
    Ok(())
}

/// What came of asking Minecraft services: the outfit, and the face when the skin worn changed.
#[derive(Debug)]
pub struct Told {
    pub outfit: Outfit,
    pub face: Option<Vec<u8>>,
}

/// Asks Minecraft services what `account` wears and owns, keeps the pictures and the answer on disk, and puts a skin
/// the library has not seen into it. Blocking: run it off the UI thread.
pub fn refresh(account: &Account, step: &dyn Fn(Step)) -> Result<Told> {
    let started = now();
    let before = cached(account.id);
    let outfit = if drive::fenced("skin profile", &account.name) {
        canned::outfit(account, step)?
    } else {
        step(Step::SignIn);
        let session = auth::session_for(account)?;
        let services = Services::new(account.proxy.as_deref(), session.access_token)?;
        read(&services, step)?
    };
    if let Some(newer) = cached(account.id).filter(|c| c.fetched >= started && Some(c) != before.as_ref()) {
        return Ok(Told { outfit: newer, face: None });
    }
    step(Step::Saving);
    settle(account, before.as_ref(), outfit)
}

fn read(services: &Services, step: &dyn Fn(Step)) -> Result<Outfit> {
    step(Step::Profile);
    let wardrobe = services.wardrobe()?;
    let skin = wardrobe.skin_url.as_deref().and_then(change::texture_hash);
    if let (Some(texture), Some(url)) = (&skin, wardrobe.skin_url.as_deref())
        && !texture_path(texture).is_file()
        && let Ok(picture) = services.texture(url)
    {
        keep_texture(texture, &picture)?;
    }
    let mut capes = Vec::new();
    let total = wardrobe.capes.len();
    for (done, cape) in wardrobe.capes.iter().enumerate() {
        step(Step::Capes { done, total });
        let Some(texture) = change::texture_hash(&cape.url) else { continue };
        if !texture_path(&texture).is_file()
            && let Ok(picture) = services.texture(&cape.url)
        {
            keep_texture(&texture, &picture)?;
        }
        capes.push(Owned { id: cape.id.clone(), alias: cape.alias.clone(), texture });
    }
    Ok(Outfit { skin, model: wardrobe.model, capes, cape: wardrobe.cape().map(str::to_owned), fetched: now() })
}

fn settle(account: &Account, before: Option<&Outfit>, outfit: Outfit) -> Result<Told> {
    std::fs::create_dir_all(cache_dir()).with_context(|| format!("cannot create {}", cache_dir().display()))?;
    net::write_atomic(&outfit_path(account.id), &serde_json::to_vec(&outfit)?)?;
    let mut face = None;
    if let Some(worn) = outfit.skin.as_deref()
        && let Some(picture) = texture(worn)
    {
        if before.and_then(|b| b.skin.as_deref()) != Some(worn) {
            face = skin::adopt(account.id, &picture);
            if let Ok(png) = change::png(&picture) {
                let _ = skins::adopt(account, worn, &png);
            }
        }
        if !read_index(&library_dir())?.seen.iter().any(|t| t == worn) {
            add(&Fetched { png: change::png(&picture)?, model: outfit.model, name: account.name.clone(), origin: Origin::Worn, from: Some(account.name.clone()) })?;
            locked(&library_dir(), |index| {
                if !index.seen.iter().any(|t| t == worn) {
                    index.seen.push(worn.to_owned());
                }
                Ok(())
            })?;
        }
    }
    Ok(Told { outfit, face })
}

/// What a change of skin or cape came to: the outfit afterwards when Minecraft services told it, and what failed.
#[derive(Debug)]
pub struct Changed {
    pub told: Option<Told>,
    pub error: Option<anyhow::Error>,
}

/// Puts the skin file `upload` on `account`, and wears `cape` (`Some(None)` takes it off). Blocking: run it off the UI thread.
pub fn wear(account: &Account, upload: Option<(&[u8], Model)>, cape: Option<Option<&str>>, step: &dyn Fn(Step)) -> Changed {
    let before = cached(account.id);
    let png = upload.map(|(png, _)| png);
    let applied = if drive::fenced("skin wear", &account.name) {
        step(Step::Upload);
        canned::apply(before.as_ref(), upload, cape)
    } else {
        step(Step::SignIn);
        match auth::session_for(account).and_then(|s| Services::new(account.proxy.as_deref(), s.access_token)) {
            Ok(services) => {
                step(Step::Upload);
                change::apply(&services, upload, cape)
            }
            Err(e) => change::Applied { wardrobe: None, skin_done: false, error: Some(e) },
        }
    };
    step(Step::Saving);
    let told = applied.wardrobe.and_then(|w| {
        let worn = w.skin_url.as_deref().and_then(change::texture_hash);
        if let (Some(texture), Some(png)) = (&worn, png)
            && let Ok(picture) = change::check_png(png)
        {
            let _ = keep_texture(texture, &picture);
        }
        let mut outfit = before.clone().unwrap_or_default();
        outfit.skin = worn.or(outfit.skin);
        outfit.model = w.model;
        outfit.cape = w.cape().map(str::to_owned);
        outfit.fetched = now();
        for c in &w.capes {
            if let Some(texture) = change::texture_hash(&c.url)
                && !outfit.capes.iter().any(|o| o.id == c.id)
            {
                outfit.capes.push(Owned { id: c.id.clone(), alias: c.alias.clone(), texture });
            }
        }
        settle(account, before.as_ref(), outfit).ok()
    });
    Changed { told, error: applied.error }
}

/// The skin vanilla shows for an account that never signed in, and its arm model, from an installed client.
pub fn default_for(id: Uuid) -> Option<(RgbaImage, Model)> {
    skin::default_skin(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetched(seed: u8, name: &str) -> Fetched {
        let image = canned::skin([seed, 90, 140], [30, seed, 70]);
        Fetched { png: change::png(&image).unwrap(), model: Model::Classic, name: name.into(), origin: Origin::File, from: None }
    }

    #[test]
    fn links_need_https_and_a_host_with_a_dot() {
        assert_eq!(parse_link("example.com/skin.png").unwrap().as_str(), "https://example.com/skin.png");
        assert_eq!(parse_link("  https://example.com/a  ").unwrap().host_str(), Some("example.com"));
        for bad in ["", "   ", "http://example.com/a.png", "ftp://example.com/a.png", "localhost/a.png", "two words.com", "https://", "file:///c:/skin.png"] {
            assert_eq!(parse_link(bad), Err(AddError::BadLink), "{bad}");
        }
    }

    #[test]
    fn names_are_checked_before_the_network() {
        for bad in ["", "two words", "waytoolongplayername", "bad-name", "Ünï"] {
            let e = fetch_player(bad).unwrap_err();
            assert_eq!(e.downcast_ref::<AddError>(), Some(&AddError::BadName), "{bad}");
        }
    }

    #[test]
    fn a_removed_skin_comes_back_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let first = add_in(dir.path(), &fetched(10, "Alpha")).unwrap().skin;
        let second = add_in(dir.path(), &fetched(100, "Beta")).unwrap().skin;
        let third = add_in(dir.path(), &fetched(200, "Gamma")).unwrap().skin;
        let kept = std::fs::read(dir.path().join(&second.file)).unwrap();
        remove_in(dir.path(), &second.file).unwrap();
        restore_in(dir.path(), &second, &kept, 1).unwrap();
        restore_in(dir.path(), &second, &kept, 0).unwrap();
        let order: Vec<String> = read_index(dir.path()).unwrap().skins.into_iter().map(|s| s.file).collect();
        assert_eq!(order, [third.file, second.file.clone(), first.file]);
        assert!(restore_in(dir.path(), &second, b"not a png", 0).is_err());
    }

    #[test]
    fn restoring_clamps_the_place_and_ignores_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let only = add_in(dir.path(), &fetched(10, "Alpha")).unwrap().skin;
        let kept = std::fs::read(dir.path().join(&only.file)).unwrap();
        remove_in(dir.path(), &only.file).unwrap();
        restore_in(dir.path(), &only, &kept, 99).unwrap();
        assert_eq!(read_index(dir.path()).unwrap().skins.len(), 1);
        let bad = Skin { file: "../x.png".into(), ..only };
        restore_in(dir.path(), &bad, &kept, 0).unwrap();
        assert_eq!(read_index(dir.path()).unwrap().skins.len(), 1);
    }

    #[test]
    fn the_library_keeps_every_skin_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = add_in(dir.path(), &fetched(10, "Alpha")).unwrap();
        assert!(first.new);
        let again = add_in(dir.path(), &fetched(10, "Other name")).unwrap();
        assert!(!again.new);
        assert_eq!(again.skin.name, "Alpha");
        let second = add_in(dir.path(), &fetched(200, "Beta")).unwrap();
        assert!(second.new);
        let names: Vec<String> = read_index(dir.path()).unwrap().skins.into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["Beta", "Alpha"]);
    }

    #[test]
    fn removing_a_skin_deletes_its_file_and_ignores_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let added = add_in(dir.path(), &fetched(10, "Alpha")).unwrap().skin;
        remove_in(dir.path(), "../outside.png").unwrap();
        remove_in(dir.path(), &added.file).unwrap();
        assert!(read_index(dir.path()).unwrap().skins.is_empty());
        assert!(!dir.path().join(&added.file).exists());
    }

    #[test]
    fn a_skin_file_without_an_index_entry_is_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let added = add_in(dir.path(), &fetched(10, "Alpha")).unwrap().skin;
        std::fs::remove_file(dir.path().join("library.json")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.path().join(format!("{}.png", "0".repeat(40))), b"not a png").unwrap();
        let found = read_index(dir.path()).unwrap().skins;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, added.file);
    }

    #[test]
    fn an_index_entry_without_its_file_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let added = add_in(dir.path(), &fetched(10, "Alpha")).unwrap().skin;
        std::fs::remove_file(dir.path().join(&added.file)).unwrap();
        assert!(read_index(dir.path()).unwrap().skins.is_empty());
    }

    #[test]
    fn a_damaged_index_does_not_lose_the_skins() {
        let dir = tempfile::tempdir().unwrap();
        let added = add_in(dir.path(), &fetched(10, "Alpha")).unwrap().skin;
        std::fs::write(dir.path().join("library.json"), b"{ damaged").unwrap();
        assert_eq!(read_index(dir.path()).unwrap().skins.first().map(|s| s.file.clone()), Some(added.file));
        assert!(dir.path().join("library.json.bad").is_file());
    }

    #[test]
    fn an_unknown_entry_does_not_take_the_others_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = add_in(dir.path(), &fetched(10, "Alpha")).unwrap().skin;
        let second = add_in(dir.path(), &fetched(200, "Beta")).unwrap().skin;
        let mut json: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join("library.json")).unwrap()).unwrap();
        json["skins"][0]["origin"] = "from the future".into();
        std::fs::write(dir.path().join("library.json"), serde_json::to_vec(&json).unwrap()).unwrap();
        rename_in(dir.path(), &first.file, "Renamed").unwrap();
        let names: Vec<String> = read_index(dir.path()).unwrap().skins.into_iter().map(|s| s.name).collect();
        assert!(names.contains(&"Renamed".to_owned()));
        assert!(read_index(dir.path()).unwrap().skins.iter().any(|s| s.file == second.file));
    }

    #[test]
    fn an_unreadable_index_stops_the_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("library.json")).unwrap();
        assert!(add_in(dir.path(), &fetched(10, "Alpha")).is_err());
        assert!(dir.path().join("library.json").is_dir());
    }

    #[test]
    fn names_are_trimmed_and_capped() {
        assert_eq!(clean("  a   b  ", "x"), "a b");
        assert_eq!(clean("   ", "Skin"), "Skin");
        assert_eq!(clean(&"n".repeat(100), "x").chars().count(), NAME_MAX);
    }

    #[test]
    fn texture_ids_are_hex_only() {
        assert!(valid_texture("ca11ed"));
        for bad in ["", "../x", "ca11ed.png", "zz"] {
            assert!(!valid_texture(bad), "{bad}");
        }
        assert!(texture("../../etc/passwd").is_none());
    }

    #[test]
    fn an_outfit_is_fresh_for_a_while() {
        assert!(fresh(now()));
        assert!(!fresh(now() - FRESH - 1));
        assert!(!fresh(Outfit::default().fetched));
    }

    #[test]
    fn the_same_pixels_hash_alike_whatever_the_encoding() {
        let image = canned::skin([1, 2, 3], [4, 5, 6]);
        let other = change::check_png(&change::png(&image).unwrap()).unwrap();
        assert_eq!(pixel_hash(&image), pixel_hash(&other));
        assert_ne!(pixel_hash(&image), pixel_hash(&canned::skin([1, 2, 4], [4, 5, 6])));
    }
}
