//! Packs in an instance: where each kind of project goes, which pack files a catalog knows by hash, and installing,
//! updating and removing them. [`Catalog`] is the seam for a second source next to Modrinth.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};

use super::batch::ModRef;
use super::instance::Loader;
use super::modrinth::{self, Details, Filter, Kind, Page, Search, Update, Version};
use super::{i18n, net, paths, trash};

const DATAPACKS: &str = "datapacks";
const GLOBAL_PACK_MODS: [&str; 2] = [GLOBAL_PACKS, "gEUERjxK"];
const GLOBAL_PACK_NAMES: [&str; 4] = ["global packs", "globalpacks", "global_packs", "data loader"];
const SHADER_MODS: [&str; 2] = ["YL57xq9U", "GchcoXML"];
const SHADER_NAMES: [&str; 2] = ["iris", "oculus"];
const SHADER_SETTINGS: [&str; 2] = ["iris.properties", "oculus.properties"];
const OPTION_KEYS: [&str; 2] = ["resourcePacks:", "incompatibleResourcePacks:"];
const CDN: &str = "https://cdn.modrinth.com/";
const SHOT_WIDTH: u32 = 960;
const MAX_SIDE: u32 = 8192;
const MAX_DECODED: u64 = 256 << 20;

static HASHES: LazyLock<Mutex<HashMap<PathBuf, (u64, SystemTime, String)>>> = LazyLock::new(Mutex::default);

/// A source of projects: search, project pages, versions, files and lookups by hash.
pub trait Catalog: Sync {
    fn search(&self, search: &Search, offset: usize) -> Result<Page>;
    fn details(&self, project_id: &str) -> Result<(Details, Option<SystemTime>)>;
    fn versions(&self, project_id: &str) -> Result<(Vec<Version>, Option<SystemTime>)>;
    fn titles(&self, ids: &[String]) -> Result<HashMap<String, String>>;
    fn file_for(&self, project_id: &str, title: &str, filter: &Filter) -> Result<ModRef>;
    fn updates(&self, filter: &Filter, current: &[ModRef]) -> Result<Vec<Update>>;
    fn by_hash(&self, hashes: &[String]) -> Result<HashMap<String, ModRef>>;
}

/// Modrinth as a [`Catalog`].
pub struct Modrinth;

impl Catalog for Modrinth {
    fn search(&self, search: &Search, offset: usize) -> Result<Page> {
        modrinth::search_for(search, offset)
    }

    fn details(&self, project_id: &str) -> Result<(Details, Option<SystemTime>)> {
        modrinth::details(project_id)
    }

    fn versions(&self, project_id: &str) -> Result<(Vec<Version>, Option<SystemTime>)> {
        modrinth::versions(project_id)
    }

    fn titles(&self, ids: &[String]) -> Result<HashMap<String, String>> {
        modrinth::titles(ids)
    }

    fn file_for(&self, project_id: &str, title: &str, filter: &Filter) -> Result<ModRef> {
        modrinth::file_for(project_id, title, filter)
    }

    fn updates(&self, filter: &Filter, current: &[ModRef]) -> Result<Vec<Update>> {
        modrinth::updates_for(filter, current)
    }

    fn by_hash(&self, hashes: &[String]) -> Result<HashMap<String, ModRef>> {
        modrinth::match_hashes(hashes)
    }
}

/// Where packs of `kind` go in the instance folder `instance_dir`: the shared resource or shader pack folder, the
/// `datapacks` folder of the world folder `world`, or without a world the shared data pack folder that a mod like
/// Global Packs loads into every new world. Mods and modpacks have no pack folder.
pub fn pack_dir(instance_dir: &Path, kind: Kind, world: Option<&Path>) -> Option<PathBuf> {
    match kind {
        Kind::ResourcePack => Some(instance_dir.join("resourcepacks")),
        Kind::Shader => Some(instance_dir.join("shaderpacks")),
        Kind::DataPack => Some(world.map_or_else(|| instance_dir.join(DATAPACKS), |w| w.join(DATAPACKS))),
        Kind::Mod | Kind::Modpack => None,
    }
}

/// The Modrinth id of Global Packs, which loads the shared data pack folder into every new world.
pub const GLOBAL_PACKS: &str = "NRLPy2mk";

/// Whether `mods` or the instance's own files `local_mods` have a mod that loads the shared data pack folder into
/// every new world.
pub fn loads_global_packs(mods: &[ModRef], local_mods: &[String]) -> bool {
    has_mod(mods, local_mods, &GLOBAL_PACK_MODS, &GLOBAL_PACK_NAMES)
}

/// The shader loader among `mods` and `local_mods`, as Modrinth names the shader packs for it: Iris and Oculus
/// both load `iris` packs.
pub fn shader_loader(mods: &[ModRef], local_mods: &[String]) -> Option<&'static str> {
    has_mod(mods, local_mods, &SHADER_MODS, &SHADER_NAMES).then_some("iris")
}

/// The Modrinth id and name of the mod that loads shader packs on `loader`: Oculus on Forge, Iris on the others,
/// none on Vanilla.
pub fn shader_mod(loader: &Loader) -> Option<(&'static str, &'static str)> {
    match loader {
        Loader::Vanilla => None,
        Loader::Forge(_) => Some((SHADER_MODS[1], "Oculus")),
        Loader::Fabric(_) | Loader::Quilt(_) | Loader::NeoForge(_) => Some((SHADER_MODS[0], "Iris")),
    }
}

fn has_mod(mods: &[ModRef], local_mods: &[String], ids: &[&str], names: &[&str]) -> bool {
    let named = |name: &str| {
        let lower = name.to_lowercase();
        names.iter().any(|n| lower.starts_with(n))
    };
    mods.iter().any(|m| ids.contains(&m.project_id.as_str()) || named(&m.title)) || local_mods.iter().any(|f| named(f))
}

/// One pack file that a catalog knows: its name in the folder and the catalog's file it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub file: String,
    pub found: ModRef,
}

/// The `.zip` files directly in `dir` with their SHA-1, by name. A hash is computed once per path, size and time.
pub fn pack_files(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            if !name.to_lowercase().ends_with(".zip") {
                return None;
            }
            Some((name, sha1_of(&e.path())?))
        })
        .collect();
    files.sort_by_cached_key(|(name, _)| name.to_lowercase());
    files
}

fn sha1_of(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let stamp = (meta.len(), meta.modified().ok()?);
    let mut hashes = HASHES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((len, at, hash)) = hashes.get(path)
        && (*len, *at) == stamp
    {
        return Some(hash.clone());
    }
    drop(hashes);
    let hash = net::file_sha1(path).ok()?;
    HASHES.lock().unwrap_or_else(PoisonError::into_inner).insert(path.to_owned(), (stamp.0, stamp.1, hash.clone()));
    Some(hash)
}

/// Which of `files` (name and SHA-1) are in `known`, a lookup of lowercase SHA-1 to the catalog's file.
/// One entry per project: of two files of one project the first by name wins.
pub fn match_installed(files: &[(String, String)], known: &HashMap<String, ModRef>) -> Vec<Installed> {
    let mut out: Vec<Installed> = Vec::new();
    for (file, hash) in files {
        let Some(found) = known.get(&hash.to_ascii_lowercase()) else { continue };
        if out.iter().any(|i| i.found.project_id == found.project_id) {
            continue;
        }
        out.push(Installed { file: file.clone(), found: found.clone() });
    }
    out
}

/// The pack files in `dir` that `catalog` knows.
pub fn installed(catalog: &dyn Catalog, dir: &Path) -> Result<Vec<Installed>> {
    let files = pack_files(dir);
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let hashes: Vec<String> = files.iter().map(|(_, h)| h.clone()).collect();
    Ok(match_installed(&files, &catalog.by_hash(&hashes)?))
}

/// Puts `found` into `dir`, verified by hash, and returns its file name. A different file of the same name stays and
/// is refused, unless it is `replacing`, the file this one updates; that one is removed once the new one is in place.
pub fn place(found: &ModRef, dir: &Path, replacing: Option<&str>, download: impl FnOnce(&ModRef, &Path) -> Result<()>) -> Result<String> {
    modrinth::safe_file_name(&found.file_name)?;
    if let Some(old) = replacing {
        modrinth::safe_file_name(old)?;
    }
    let path = dir.join(&found.file_name);
    let same_name = replacing.is_some_and(|old| old.eq_ignore_ascii_case(&found.file_name));
    if path.exists() && !same_name && !net::file_sha1(&path).is_ok_and(|h| h.eq_ignore_ascii_case(&found.sha1)) {
        bail!(super::instance::Final(i18n::trf("Dort liegt schon eine andere Datei „%@“.", &[&found.file_name])));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    download(found, &path)?;
    if let Some(old) = replacing.filter(|_| !same_name) {
        let old = dir.join(old);
        if old.exists() {
            std::fs::remove_file(&old).with_context(|| format!("cannot delete {}", old.display()))?;
        }
    }
    Ok(found.file_name.clone())
}

/// Downloads `found` to `path` and checks its SHA-1.
pub fn download(found: &ModRef, path: &Path) -> Result<()> {
    net::fetch_verified(&net::agent(None)?, &found.url, path, Some(&found.sha1)).with_context(|| format!("cannot download {}", found.title))
}

/// Moves the pack `file` in `dir`, a folder inside `instance_dir`, to the instance's trash and returns the entry
/// [`put_back`] takes.
pub fn remove(instance_dir: &Path, dir: &Path, file: &str) -> Result<String> {
    modrinth::safe_file_name(file)?;
    trash::put(instance_dir, &dir.join(file))
}

/// Puts a pack [`remove`] took back where it was.
pub fn put_back(instance_dir: &Path, entry: &str) -> Result<PathBuf> {
    trash::take_back(instance_dir, entry)
}

/// Points what selects the pack `old` at `new` after an update renamed its file: the resource pack lists of the
/// instance's options template, or the shader pack setting of Iris and Oculus in its shared config.
pub fn rename_selected(instance_dir: &Path, kind: Kind, old: &str, new: &str) -> Result<()> {
    if old == new {
        return Ok(());
    }
    match kind {
        Kind::ResourcePack => super::sync::with_templates(|| rewrite(&instance_dir.join(super::sync::OPTIONS_FILE), |text| renamed_packs(text, old, new))),
        Kind::Shader => {
            for name in SHADER_SETTINGS {
                rewrite(&instance_dir.join("config").join(name), |text| renamed_shader(text, old, new))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn rewrite(path: &Path, change: impl Fn(&str) -> Option<String>) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    match change(&text) {
        Some(next) => net::write_atomic(path, next.as_bytes()),
        None => Ok(()),
    }
}

fn renamed_packs(text: &str, old: &str, new: &str) -> Option<String> {
    let (from, to) = (serde_json::json!(format!("file/{old}")).to_string(), serde_json::json!(format!("file/{new}")).to_string());
    let mut changed = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            if OPTION_KEYS.iter().any(|k| line.starts_with(k)) && line.contains(&from) {
                changed = true;
                line.replace(&from, &to)
            } else {
                line.to_owned()
            }
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

fn renamed_shader(text: &str, old: &str, new: &str) -> Option<String> {
    let mut changed = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| match line.trim_end_matches('\r').split_once('=') {
            Some((key, value)) if key.trim() == "shaderPack" && value.trim() == old => {
                changed = true;
                format!("shaderPack={new}{}", if line.ends_with('\r') { "\r" } else { "" })
            }
            _ => line.to_owned(),
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

/// Downloads the `.mrpack` of a modpack to the cache folder for an import and returns its path.
pub fn fetch_modpack(found: &ModRef) -> Result<PathBuf> {
    modrinth::safe_file_name(&found.file_name)?;
    let path = paths::data_dir().join("cache").join("modpacks").join(&found.file_name);
    download(found, &path)?;
    Ok(path)
}

/// A project description reduced to the Markdown the browser shows: no pictures, no HTML tags (their text stays),
/// common entities decoded, at most one empty line in a row. Code blocks stay as they are.
pub fn readable(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fenced = false;
    for line in body.replace("\r\n", "\n").split('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if fenced {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        out.push_str(&entities(&strip_tags(&strip_images(line))));
        out.push('\n');
    }
    let mut squeezed = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.split('\n') {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
            squeezed.push('\n');
        } else {
            blank = 0;
            squeezed.push_str(line.trim_end());
            squeezed.push('\n');
        }
    }
    squeezed.trim().to_owned()
}

fn strip_images(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("![") {
        let linked = rest[..at].ends_with('[');
        out.push_str(&rest[..if linked { at - 1 } else { at }]);
        let after = &rest[at..];
        let Some(close) = after.find("](").and_then(|open| after[open..].find(')').map(|c| open + c + 1)) else {
            out.push_str(after);
            return out;
        };
        rest = &after[close..];
        if linked && rest.starts_with("](") {
            rest = rest.find(')').map_or("", |c| &rest[c + 1..]);
        }
        if out.trim().is_empty() {
            out.clear();
            rest = rest.trim_start();
        }
    }
    out.push_str(rest);
    out
}

fn strip_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[at..]);
            return out;
        };
        let tag = &after[..end];
        let opens = tag.starts_with('/') || tag.starts_with('!') || tag.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
        if tag.starts_with("http://") || tag.starts_with("https://") {
            out.push_str(tag);
        } else if opens && !tag.contains('<') {
            let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
            if (name == "br" || name == "p" || name == "div") && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push('<');
            rest = after;
            continue;
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn entities(text: &str) -> String {
    text.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// A gallery picture as a file in the cache folder, at most `SHOT_WIDTH` wide. Blocking: run it off the UI thread.
pub fn shot_file(url: &str) -> Result<PathBuf> {
    if !url.starts_with(CDN) {
        bail!("picture is not on Modrinth's CDN");
    }
    let dir = paths::data_dir().join("cache").join("gallery");
    let path = dir.join(format!("{}-{SHOT_WIDTH}.png", sha1_hex(url)));
    if path.is_file() {
        return Ok(path);
    }
    let bytes = net::get_bytes(&net::agent(None)?, url)?;
    net::write_atomic(&path, &shrink(&bytes)?)?;
    Ok(path)
}

fn shrink(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED);
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits);
    let full = reader.decode().context("not a supported picture")?;
    let small = if full.width() > SHOT_WIDTH { full.resize(SHOT_WIDTH, MAX_SIDE, FilterType::Triangle) } else { full };
    let mut out = std::io::Cursor::new(Vec::new());
    small.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn sha1_hex(text: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(project: &str, file: &str, sha1: &str) -> ModRef {
        ModRef { project_id: project.into(), version_id: format!("v-{project}"), title: project.into(), file_name: file.into(), url: String::new(), sha1: sha1.into() }
    }

    #[test]
    fn each_kind_goes_to_its_folder() {
        let dir = Path::new("inst");
        assert_eq!(pack_dir(dir, Kind::ResourcePack, None), Some(dir.join("resourcepacks")));
        assert_eq!(pack_dir(dir, Kind::Shader, Some(Path::new("w"))), Some(dir.join("shaderpacks")));
        let world = dir.join("games").join("a").join("saves").join("World");
        assert_eq!(pack_dir(dir, Kind::DataPack, Some(&world)), Some(world.join("datapacks")));
        assert_eq!(pack_dir(dir, Kind::DataPack, None), Some(dir.join("datapacks")));
        assert_eq!(pack_dir(dir, Kind::Mod, None), None);
        assert_eq!(pack_dir(dir, Kind::Modpack, None), None);
    }

    #[test]
    fn installed_packs_are_matched_by_hash_once_per_project() {
        let known: HashMap<String, ModRef> = [("aa".to_owned(), found("fresh", "Fresh.zip", "aa")), ("bb".to_owned(), found("fresh", "Fresh-old.zip", "bb")), ("cc".to_owned(), found("bsl", "BSL.zip", "cc"))].into();
        let files = vec![("A renamed.zip".to_owned(), "AA".to_owned()), ("B.zip".to_owned(), "bb".to_owned()), ("mine.zip".to_owned(), "dd".to_owned()), ("BSL.zip".to_owned(), "cc".to_owned())];
        let got = match_installed(&files, &known);
        assert_eq!(got.iter().map(|i| (i.file.as_str(), i.found.project_id.as_str())).collect::<Vec<_>>(), [("A renamed.zip", "fresh"), ("BSL.zip", "bsl")]);
        assert!(match_installed(&files, &HashMap::new()).is_empty());
    }

    #[test]
    fn pack_files_lists_zips_with_their_hashes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.zip"), b"two").unwrap();
        std::fs::write(dir.path().join("A.ZIP"), b"one").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("folder.zip")).unwrap();
        let files = pack_files(dir.path());
        assert_eq!(files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["A.ZIP", "b.zip"]);
        assert_eq!(files[0].1, net::file_sha1(&dir.path().join("A.ZIP")).unwrap());
        assert!(pack_files(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn placing_refuses_a_foreign_file_and_replaces_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let write = |body: &'static [u8]| move |_: &ModRef, path: &Path| -> Result<()> { Ok(std::fs::write(path, body)?) };
        std::fs::write(dir.path().join("Pack.zip"), b"mine").unwrap();
        assert!(place(&found("p", "Pack.zip", "00"), dir.path(), None, write(b"new")).is_err());
        assert_eq!(std::fs::read(dir.path().join("Pack.zip")).unwrap(), b"mine");
        std::fs::write(dir.path().join("Old.zip"), b"old").unwrap();
        assert_eq!(place(&found("p", "New.zip", "00"), dir.path(), Some("Old.zip"), write(b"new")).unwrap(), "New.zip");
        assert!(!dir.path().join("Old.zip").exists() && dir.path().join("New.zip").is_file());
        assert!(place(&found("p", "../x.zip", "00"), dir.path(), None, write(b"x")).is_err());
        let failing = |_: &ModRef, _: &Path| -> Result<()> { bail!("offline") };
        std::fs::write(dir.path().join("Keep.zip"), b"keep").unwrap();
        assert!(place(&found("p", "Next.zip", "00"), dir.path(), Some("Keep.zip"), failing).is_err());
        assert!(dir.path().join("Keep.zip").is_file());
    }

    #[test]
    fn a_removed_pack_comes_back() {
        let inst = tempfile::tempdir().unwrap();
        let packs = inst.path().join("resourcepacks");
        std::fs::create_dir_all(&packs).unwrap();
        std::fs::write(packs.join("P.zip"), b"x").unwrap();
        let entry = remove(inst.path(), &packs, "P.zip").unwrap();
        assert!(!packs.join("P.zip").exists());
        put_back(inst.path(), &entry).unwrap();
        assert!(packs.join("P.zip").is_file());
        assert!(remove(inst.path(), &packs, "..\\P.zip").is_err());
    }

    #[test]
    fn an_update_keeps_the_pack_selected() {
        let inst = tempfile::tempdir().unwrap();
        std::fs::write(inst.path().join("options.txt"), "lang:de_de\nresourcePacks:[\"vanilla\",\"file/Old 1.zip\"]\nincompatibleResourcePacks:[]\n").unwrap();
        rename_selected(inst.path(), Kind::ResourcePack, "Old 1.zip", "New 2.zip").unwrap();
        assert_eq!(std::fs::read_to_string(inst.path().join("options.txt")).unwrap(), "lang:de_de\nresourcePacks:[\"vanilla\",\"file/New 2.zip\"]\nincompatibleResourcePacks:[]\n");
        std::fs::create_dir_all(inst.path().join("config")).unwrap();
        std::fs::write(inst.path().join("config").join("iris.properties"), "enableShaders=true\r\nshaderPack=BSL v1.zip\r\n").unwrap();
        rename_selected(inst.path(), Kind::Shader, "BSL v1.zip", "BSL v2.zip").unwrap();
        assert_eq!(std::fs::read_to_string(inst.path().join("config").join("iris.properties")).unwrap(), "enableShaders=true\r\nshaderPack=BSL v2.zip\r\n");
        rename_selected(inst.path(), Kind::Shader, "Other.zip", "X.zip").unwrap();
        assert!(!inst.path().join("config").join("oculus.properties").exists());
    }

    #[test]
    fn shader_and_global_pack_mods_are_recognised() {
        let (mut mods, mut local) = (vec![found("sodium", "sodium.jar", "0")], Vec::new());
        assert_eq!(shader_loader(&mods, &local), None);
        assert!(!loads_global_packs(&mods, &local));
        mods.push(found("YL57xq9U", "iris.jar", "0"));
        assert_eq!(shader_loader(&mods, &local), Some("iris"));
        local.push("GlobalPacks-1.2.jar".to_owned());
        assert!(loads_global_packs(&mods, &local));
        assert!(loads_global_packs(&[found(GLOBAL_PACKS, "x.jar", "0")], &[]));
        assert_eq!(shader_loader(&[ModRef { title: "Oculus".into(), ..found("other", "o.jar", "0") }], &[]), Some("iris"));
        assert_eq!(shader_mod(&Loader::Forge(None)).map(|m| m.1), Some("Oculus"));
        assert_eq!(shader_mod(&Loader::NeoForge(None)).map(|m| m.1), Some("Iris"));
        assert_eq!(shader_mod(&Loader::Vanilla), None);
    }

    #[test]
    fn descriptions_lose_pictures_and_tags_but_keep_text_and_code() {
        let body = "<center>\n\n[![Banner](https://cdn.x/b.png)](https://x.org)\n\n# Fresh &amp; new</center>\n\n![shot](https://cdn.x/s.png) Moves <b>mobs</b> 3 < 4<br>more\n\n\n\nSee <https://x.org>.\n```\n<keep> ![this](x)\n```";
        assert_eq!(readable(body), "# Fresh & new\n\nMoves mobs 3 < 4 more\n\nSee https://x.org.\n```\n<keep> ![this](x)\n```");
        assert_eq!(readable("![unclosed](x"), "![unclosed](x");
        assert_eq!(readable("a < b and c > d"), "a < b and c > d");
    }
}
