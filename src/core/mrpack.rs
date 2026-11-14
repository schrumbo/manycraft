//! Modrinth modpacks (`.mrpack`): a Fabric pack becomes a batch and a batch becomes a pack.
//! Files come only from the hosts the format allows and must match their SHA-1 and SHA-512.
//! Also the entry point for importing a Prism instance export (`.zip`) or a Prism instance folder.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use zip::ZipArchive;

use super::batch::{self, Batch, ModRef};
use super::{fleet, install, modrinth, net, prism};

/// File extension of a Modrinth modpack.
pub const EXTENSION: &str = "mrpack";
const INDEX: &str = "modrinth.index.json";
const OVERRIDES: [&str; 2] = ["overrides/", "client-overrides/"];
const HOSTS: [&str; 4] = ["cdn.modrinth.com", "github.com", "raw.githubusercontent.com", "gitlab.com"];
const OTHER_LOADERS: [(&str, &str); 3] = [("forge", "Forge"), ("neoforge", "NeoForge"), ("quilt-loader", "Quilt")];
const FABRIC: &str = "fabric-loader";
const MINECRAFT: &str = "minecraft";
const OPTIONS_FILE: &str = "options.txt";
const SERVERS_FILE: &str = "servers.dat";
const SKIPPED: [&str; 5] = ["saves", "logs", "crash-reports", "screenshots", "batch.json"];
const SHARED: [&str; 3] = ["config", "resourcepacks", "shaderpacks"];
const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
const MAX_INDEX: u64 = 16 << 20;
const MAX_ENTRY: u64 = 1 << 30;
const MAX_TOTAL: u64 = 8 << 30;
const MAX_DEPTH: usize = 32;
const MAX_NAME: usize = 40;
const THREADS: usize = 8;
const FALLBACK_NAME: &str = "Modpack";
const USAGE: &str = "usage: manycraft mrpack import <file.mrpack|prism.zip|prism folder>
       manycraft mrpack export <batch> <file.mrpack>";

/// Why a pack or folder cannot become a batch, in terms the UI can explain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No readable `modrinth.index.json`, or one of another game or format.
    NotAPack,
    /// The pack is built for this loader instead of Fabric.
    Loader(String),
    /// The pack names no mod loader.
    NoLoader,
    /// The pack names no Minecraft version.
    NoGame,
    /// A file may only be downloaded from a host the format does not allow.
    Host(String),
    /// A file has no usable hash or did not match it.
    Hash(String),
    /// A path that would leave the batch folder.
    Path(String),
    /// The Prism instance is not a Fabric instance; carries Prism's reason.
    Prism(String),
    /// Neither a Modrinth pack nor a Prism instance.
    Unknown,
    Cancelled,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAPack => write!(f, "not a Modrinth modpack"),
            Self::Loader(loader) => write!(f, "the pack uses {loader}, only Fabric is supported"),
            Self::NoLoader => write!(f, "the pack has no mod loader, only Fabric packs can be imported"),
            Self::NoGame => write!(f, "the pack names no Minecraft version"),
            Self::Host(url) => write!(f, "{url} is not an allowed download host"),
            Self::Hash(path) => write!(f, "{path} does not match its hash"),
            Self::Path(path) => write!(f, "unsafe path {path:?} in the pack"),
            Self::Prism(reason) => write!(f, "the Prism instance cannot be imported: {reason}"),
            Self::Unknown => write!(f, "neither a .mrpack nor a Prism instance"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for Refused {}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    format_version: u32,
    game: String,
    #[serde(default)]
    version_id: String,
    #[serde(default)]
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(default)]
    files: Vec<IndexFile>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    path: String,
    #[serde(default)]
    hashes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    downloads: Vec<String>,
    #[serde(default)]
    file_size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Wanted {
    rel: PathBuf,
    sha1: String,
    sha512: String,
    url: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Pack {
    name: String,
    game: String,
    loader: String,
    files: Vec<Wanted>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Download(usize),
    Entry(usize),
}

struct Placement {
    source: Source,
    dest: PathBuf,
}

struct Slot {
    name: String,
    sha1: String,
    found: Option<ModRef>,
    source: Source,
}

type Lookup<'a> = &'a dyn Fn(&[String]) -> Result<HashMap<String, ModRef>>;
type Fetch<'a> = &'a (dyn Fn(&str, &Path, &str) -> Result<()> + Sync);
type Progress<'a> = &'a (dyn Fn(usize, usize) + Sync);

/// Whether `url` may be downloaded from: HTTPS on one of the hosts the `.mrpack` format allows.
pub fn allowed(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| HOSTS.contains(&h)))
}

/// The name [`import`] would start from, after the same checks but without downloading or writing anything.
pub fn peek(path: &Path) -> Result<String> {
    if path.is_dir() {
        if !prism::is_instance(path) {
            bail!(Refused::Unknown);
        }
        let instance = prism::instance_at(path)?;
        return match instance.unsupported {
            Some(reason) => bail!(Refused::Prism(reason)),
            None => Ok(instance.name),
        };
    }
    let file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let Ok(mut zip) = ZipArchive::new(file) else { bail!(Refused::Unknown) };
    if zip.index_for_name(INDEX).is_some() {
        let pack = read_index(&mut zip)?;
        overrides(&mut zip)?;
        return Ok(pack.name);
    }
    let Some(prefix) = prism::export_prefix(&zip) else { bail!(Refused::Unknown) };
    let staging = tempfile::tempdir().context("cannot create a temporary folder")?;
    peek(&prism::unpack_export(&mut zip, &prefix, staging.path())?)
}

/// A batch name for `base` that no batch in `taken` uses: `base`, `base (2)`, … within the name length limit.
pub fn free_name(base: &str, taken: &[String]) -> String {
    let base: String = base.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let base = if base.trim().is_empty() { FALLBACK_NAME.to_owned() } else { base.trim().to_owned() };
    let free = |name: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(name));
    if free(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!(" ({n})");
            let head: String = base.chars().take(MAX_NAME.saturating_sub(suffix.chars().count())).collect();
            format!("{}{suffix}", head.trim_end())
        })
        .find(|name| free(name))
        .unwrap_or(base)
}

/// Imports `path` as a new batch with a free name: a `.mrpack`, a Prism instance export (`.zip`) or a Prism
/// instance folder. Reports `(done, total)` steps while it works and stops with [`Refused::Cancelled`] once
/// `cancel` is set. Nothing is left behind when it fails; the source is only read.
pub fn import(path: &Path, cancel: &AtomicBool, progress: Progress) -> Result<Batch> {
    let taken: Vec<String> = batch::load_all()?.into_iter().map(|b| b.name).collect();
    let root = batch::batches_dir();
    if path.is_dir() {
        return import_prism_dir(path, &root, &taken, &modrinth::match_hashes, cancel);
    }
    let file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let Ok(mut zip) = ZipArchive::new(file) else { bail!(Refused::Unknown) };
    if zip.index_for_name(INDEX).is_some() {
        return import_pack(&mut zip, &root, &taken, &modrinth::match_hashes, &download, cancel, progress);
    }
    match prism::export_prefix(&zip) {
        Some(prefix) => {
            let staging = tempfile::tempdir().context("cannot create a temporary folder")?;
            let dir = prism::unpack_export(&mut zip, &prefix, staging.path())?;
            import_prism_dir(&dir, &root, &taken, &modrinth::match_hashes, cancel)
        }
        None => bail!(Refused::Unknown),
    }
}

/// Writes `batch` as a `.mrpack` to `out`: Modrinth mods as downloads with their hashes; the batch's own jars,
/// config, resource and shader packs and `options.txt` as overrides. Nothing else of the batch folder is read,
/// so accounts, servers, join commands, logs and saves never end up in the pack. Replaces `out` atomically.
pub fn export(batch: &Batch, out: &Path) -> Result<()> {
    let loader = match &batch.loader_version {
        Some(loader) => loader.clone(),
        None => install::latest_loader(&batch.game_version).context("cannot find the Fabric loader version")?,
    };
    export_with(batch, &batch.dir(), &loader, out, &download)
}

fn download(url: &str, dest: &Path, sha1: &str) -> Result<()> {
    net::fetch_verified(&net::agent(None)?, url, dest, Some(sha1))
}

fn import_prism_dir(dir: &Path, root: &Path, taken: &[String], lookup: Lookup, cancel: &AtomicBool) -> Result<Batch> {
    if !prism::is_instance(dir) {
        bail!(Refused::Unknown);
    }
    let instance = prism::instance_at(dir)?;
    if let Some(reason) = &instance.unsupported {
        bail!(Refused::Prism(reason.clone()));
    }
    if cancel.load(Ordering::SeqCst) {
        bail!(Refused::Cancelled);
    }
    let made = prism::import_into(&instance, &free_name(&instance.name, taken), root, lookup)?;
    if cancel.load(Ordering::SeqCst) {
        let _ = std::fs::remove_dir_all(root.join(&made.id));
        bail!(Refused::Cancelled);
    }
    Ok(made)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn reserved(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    RESERVED.contains(&stem.as_str()) || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit())
}

/// `path` from a pack as a relative path, refusing anything absolute, with `..`, drive letters, backslashes or names Windows reserves.
pub(super) fn safe_rel(path: &str) -> Result<PathBuf> {
    let bad = || Refused::Path(path.to_owned());
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':']) || path.chars().any(char::is_control) {
        bail!(bad());
    }
    let mut out = PathBuf::new();
    for part in path.split('/') {
        if part.is_empty() || part.ends_with(['.', ' ']) || reserved(part) {
            bail!(bad());
        }
        out.push(part);
    }
    Ok(out)
}

/// Where `rel` from a pack goes in the batch folder: `options.txt` to the options template, folders by name
/// (the first one in lower case), nothing for other top-level files, saves, logs and screenshots.
fn target(rel: &Path) -> Option<PathBuf> {
    let mut parts = rel.iter();
    let top = parts.next()?.to_str()?.to_ascii_lowercase();
    let rest: PathBuf = parts.collect();
    if rest.as_os_str().is_empty() {
        return [OPTIONS_FILE, SERVERS_FILE].into_iter().find(|f| *f == top).map(PathBuf::from);
    }
    if SKIPPED.contains(&top.as_str()) {
        return None;
    }
    Some(Path::new(&top).join(rest))
}

/// The file name of `rel` when it is a jar directly in `mods/`.
fn mod_name(rel: &Path) -> Option<String> {
    let parts: Vec<&str> = rel.iter().map(|p| p.to_str()).collect::<Option<_>>()?;
    match parts.as_slice() {
        [top, name] if top.eq_ignore_ascii_case("mods") && name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".jar") => Some((*name).to_owned()),
        _ => None,
    }
}

fn parse_index(bytes: &[u8]) -> Result<Pack> {
    let index: Index = serde_json::from_slice(bytes).map_err(|_| Refused::NotAPack)?;
    if index.format_version != 1 || index.game != MINECRAFT {
        bail!(Refused::NotAPack);
    }
    if let Some((_, loader)) = OTHER_LOADERS.iter().find(|(key, _)| index.dependencies.contains_key(*key)) {
        bail!(Refused::Loader((*loader).to_owned()));
    }
    let loader = index.dependencies.get(FABRIC).filter(|v| !v.trim().is_empty()).ok_or(Refused::NoLoader)?.trim().to_owned();
    let game = index.dependencies.get(MINECRAFT).filter(|v| !v.trim().is_empty()).ok_or(Refused::NoGame)?.trim().to_owned();
    let mut files = Vec::new();
    for file in index.files {
        if file.env.as_ref().and_then(|e| e.get("client")).is_some_and(|c| c == "unsupported") {
            continue;
        }
        let rel = safe_rel(&file.path)?;
        let hash = |key: &str, len: usize| file.hashes.get(key).filter(|h| is_hex(h, len)).map(|h| h.to_ascii_lowercase());
        let (Some(sha1), Some(sha512)) = (hash("sha1", 40), hash("sha512", 128)) else { bail!(Refused::Hash(file.path)) };
        let url = file.downloads.iter().find(|u| allowed(u)).cloned().ok_or_else(|| Refused::Host(file.downloads.first().cloned().unwrap_or_else(|| file.path.clone())))?;
        files.push(Wanted { rel, sha1, sha512, url });
    }
    Ok(Pack { name: index.name, game, loader, files })
}

fn read_index<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<Pack> {
    let Ok(entry) = zip.by_name(INDEX) else { bail!(Refused::NotAPack) };
    let mut bytes = Vec::new();
    entry.take(MAX_INDEX + 1).read_to_end(&mut bytes).context("cannot read the pack")?;
    if bytes.len() as u64 > MAX_INDEX {
        bail!(Refused::NotAPack);
    }
    parse_index(&bytes)
}

struct Override {
    index: usize,
    rel: PathBuf,
}

fn overrides<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<Vec<Override>> {
    let mut found: Vec<Vec<Override>> = OVERRIDES.iter().map(|_| Vec::new()).collect();
    for index in 0..zip.len() {
        let entry = zip.by_index(index).context("cannot read the pack")?;
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let name = entry.name().context("cannot read the pack")?.into_owned();
        for (n, prefix) in OVERRIDES.iter().enumerate() {
            if let Some(rest) = name.strip_prefix(prefix) {
                found[n].push(Override { index, rel: safe_rel(rest)? });
            }
        }
    }
    Ok(found.into_iter().flatten().collect())
}

pub(super) fn copy_limited(mut from: impl Read, to: &mut impl Write, total: &mut u64) -> Result<()> {
    let copied = std::io::copy(&mut (&mut from).take(MAX_ENTRY + 1), to)?;
    *total += copied;
    if copied > MAX_ENTRY || *total > MAX_TOTAL {
        bail!("the pack unpacks to more than Manycraft accepts");
    }
    Ok(())
}

fn entry_sha1<R: Read + Seek>(zip: &mut ZipArchive<R>, index: usize, total: &mut u64) -> Result<String> {
    let entry = zip.by_index(index).context("cannot read the pack")?;
    let mut hasher = Sha1::new();
    copy_limited(entry, &mut hasher, total)?;
    Ok(hex(&hasher.finalize()))
}

fn file_sha512(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut hasher = Sha512::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn place_mod(slots: &mut Vec<Slot>, mut slot: Slot) -> Result<()> {
    if slot.name.eq_ignore_ascii_case(fleet::COMPANION_FILE) || slots.iter().any(|s| s.sha1 == slot.sha1) {
        return Ok(());
    }
    if let Some(found) = &slot.found {
        slot.name.clone_from(&found.file_name);
    }
    modrinth::safe_file_name(&slot.name)?;
    match slots.iter_mut().find(|s| s.name.eq_ignore_ascii_case(&slot.name)) {
        Some(same) => *same = slot,
        None => slots.push(slot),
    }
    Ok(())
}

fn import_pack<R: Read + Seek>(zip: &mut ZipArchive<R>, root: &Path, taken: &[String], lookup: Lookup, fetch: Fetch, cancel: &AtomicBool, progress: Progress) -> Result<Batch> {
    let pack = read_index(zip)?;
    let extra = overrides(zip)?;
    let mut total = 0;
    let mut jars: Vec<(usize, String, String)> = Vec::new();
    for o in &extra {
        if let Some(name) = mod_name(&o.rel) {
            jars.push((o.index, name, entry_sha1(zip, o.index, &mut total)?));
        }
    }
    let mut hashes: Vec<String> = pack.files.iter().filter(|w| mod_name(&w.rel).is_some()).map(|w| w.sha1.clone()).collect();
    hashes.extend(jars.iter().map(|(_, _, sha1)| sha1.clone()));
    if cancel.load(Ordering::SeqCst) {
        bail!(Refused::Cancelled);
    }
    let matched = lookup(&hashes).context("cannot look the mods up on Modrinth")?;

    let mut slots = Vec::new();
    let mut placements = Vec::new();
    for (n, wanted) in pack.files.iter().enumerate() {
        match mod_name(&wanted.rel) {
            Some(name) => place_mod(&mut slots, Slot { name, sha1: wanted.sha1.clone(), found: matched.get(&wanted.sha1).cloned(), source: Source::Download(n) })?,
            None => placements.extend(target(&wanted.rel).map(|dest| Placement { source: Source::Download(n), dest })),
        }
    }
    let jar_of: HashMap<usize, &(usize, String, String)> = jars.iter().map(|j| (j.0, j)).collect();
    for o in &extra {
        match jar_of.get(&o.index) {
            Some((_, name, sha1)) => {
                slots.retain(|s| !s.name.eq_ignore_ascii_case(name) || s.sha1 == *sha1);
                place_mod(&mut slots, Slot { name: name.clone(), sha1: sha1.clone(), found: matched.get(sha1).cloned(), source: Source::Entry(o.index) })?;
            }
            None => placements.extend(target(&o.rel).map(|dest| Placement { source: Source::Entry(o.index), dest })),
        }
    }

    let mut made = Batch::new(&free_name(&pack.name, taken), &pack.game);
    made.loader_version = Some(pack.loader.clone());
    for slot in &slots {
        match &slot.found {
            Some(found) => made.mods.push(found.clone()),
            None => made.local_mods.push(slot.name.clone()),
        }
        placements.push(Placement { source: slot.source, dest: Path::new("mods").join(&slot.name) });
    }

    let dest = root.join(&made.id);
    let result = write_pack(zip, &pack, &placements, &dest, fetch, cancel, progress).and_then(|()| batch::save_in(root, &made));
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(made)
}

fn write_pack<R: Read + Seek>(zip: &mut ZipArchive<R>, pack: &Pack, placements: &[Placement], dest: &Path, fetch: Fetch, cancel: &AtomicBool, progress: Progress) -> Result<()> {
    for dir in ["mods", "config"] {
        std::fs::create_dir_all(dest.join(dir)).with_context(|| format!("cannot create {}", dest.join(dir).display()))?;
    }
    let downloads: Vec<(&Wanted, PathBuf)> = placements
        .iter()
        .filter_map(|p| match p.source {
            Source::Download(n) => Some((&pack.files[n], dest.join(&p.dest))),
            Source::Entry(_) => None,
        })
        .collect();
    let steps = downloads.len() + 1;
    let done = AtomicUsize::new(0);
    progress(0, steps);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(THREADS).build().context("cannot start download threads")?;
    pool.install(|| {
        downloads.par_iter().try_for_each(|(wanted, path)| -> Result<()> {
            if cancel.load(Ordering::SeqCst) {
                bail!(Refused::Cancelled);
            }
            let shown = wanted.rel.to_string_lossy().replace('\\', "/");
            fetch(&wanted.url, path, &wanted.sha1).with_context(|| format!("cannot download {shown}"))?;
            if !net::file_sha1(path)?.eq_ignore_ascii_case(&wanted.sha1) || !file_sha512(path)?.eq_ignore_ascii_case(&wanted.sha512) {
                bail!(Refused::Hash(shown));
            }
            progress(done.fetch_add(1, Ordering::SeqCst) + 1, steps);
            Ok(())
        })
    })?;
    let mut total = 0;
    for p in placements {
        let Source::Entry(index) = p.source else { continue };
        if cancel.load(Ordering::SeqCst) {
            bail!(Refused::Cancelled);
        }
        let path = dest.join(&p.dest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let entry = zip.by_index(index).context("cannot read the pack")?;
        let mut out = File::create(&path).with_context(|| format!("cannot write {}", path.display()))?;
        copy_limited(entry, &mut out, &mut total)?;
    }
    progress(steps, steps);
    Ok(())
}

fn add_file<W: Write + Seek>(zip: &mut zip::ZipWriter<W>, name: &str, from: &Path) -> Result<()> {
    zip.start_file(name, zip::write::SimpleFileOptions::default().large_file(true))?;
    let mut file = File::open(from).with_context(|| format!("cannot read {}", from.display()))?;
    std::io::copy(&mut file, zip)?;
    Ok(())
}

fn shared_files(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("{} is nested too deeply", dir.display());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut found: Vec<_> = entries.collect::<std::io::Result<_>>()?;
    found.sort_by_key(std::fs::DirEntry::file_name);
    for entry in found {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let kind = entry.file_type()?;
        let inner = format!("{prefix}/{name}");
        if kind.is_dir() {
            shared_files(&entry.path(), &inner, depth + 1, out)?;
        } else if kind.is_file() {
            out.push((inner, entry.path()));
        }
    }
    Ok(())
}

fn export_with(batch: &Batch, dir: &Path, loader: &str, out: &Path, fetch: Fetch) -> Result<()> {
    let staging = tempfile::tempdir().context("cannot create a temporary folder")?;
    let mut files = Vec::new();
    let mut extra: Vec<(String, PathBuf)> = Vec::new();
    for m in &batch.mods {
        modrinth::safe_file_name(&m.file_name)?;
        let local = dir.join("mods").join(&m.file_name);
        let path = if local.is_file() && net::file_sha1(&local)?.eq_ignore_ascii_case(&m.sha1) {
            local
        } else {
            let fetched = staging.path().join(&m.file_name);
            fetch(&m.url, &fetched, &m.sha1).with_context(|| format!("cannot download {}", m.title))?;
            fetched
        };
        let name = format!("mods/{}", m.file_name);
        if allowed(&m.url) {
            let hashes = BTreeMap::from([("sha1".to_owned(), m.sha1.to_ascii_lowercase()), ("sha512".to_owned(), file_sha512(&path)?)]);
            let file_size = std::fs::metadata(&path)?.len();
            files.push(IndexFile { path: name, hashes, env: None, downloads: vec![m.url.clone()], file_size });
        } else {
            extra.push((format!("overrides/{name}"), path));
        }
    }
    for name in &batch.local_mods {
        modrinth::safe_file_name(name)?;
        let path = dir.join("mods").join(name);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            extra.push((format!("overrides/mods/{name}"), path));
        }
    }
    let options = dir.join(OPTIONS_FILE);
    if std::fs::symlink_metadata(&options).is_ok_and(|m| m.is_file()) {
        extra.push((format!("overrides/{OPTIONS_FILE}"), options));
    }
    for folder in SHARED {
        let path = dir.join(folder);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            shared_files(&path, &format!("overrides/{folder}"), 0, &mut extra)?;
        }
    }
    let dependencies = BTreeMap::from([(MINECRAFT.to_owned(), batch.game_version.clone()), (FABRIC.to_owned(), loader.to_owned())]);
    let index = Index { format_version: 1, game: MINECRAFT.to_owned(), version_id: "1".to_owned(), name: batch.name.clone(), summary: None, files, dependencies };

    let parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent).with_context(|| format!("cannot write {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(tmp);
    zip.start_file(INDEX, zip::write::SimpleFileOptions::default())?;
    zip.write_all(&serde_json::to_vec_pretty(&index)?)?;
    for (name, path) in &extra {
        add_file(&mut zip, name, path)?;
    }
    let tmp = zip.finish()?;
    tmp.as_file().sync_all()?;
    tmp.persist(out).map_err(|e| e.error).with_context(|| format!("cannot write {}", out.display()))?;
    Ok(())
}

/// `manycraft mrpack`: imports a pack or Prism instance as a batch, or exports a batch as a pack.
pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["import", path] => {
            let last = std::sync::Mutex::new(usize::MAX);
            let report = |done: usize, total: usize| {
                let percent = (done * 100).checked_div(total).unwrap_or(0);
                let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                if *last != percent {
                    *last = percent;
                    eprintln!("{percent} %");
                }
            };
            let made = import(Path::new(path), &AtomicBool::new(false), &report)?;
            println!("Batch {} ({}) for Minecraft {} with Fabric {}", made.name, made.id, made.game_version, made.loader_version.as_deref().unwrap_or("?"));
            println!("{} mods from Modrinth, {} local", made.mods.len(), made.local_mods.len());
            Ok(0)
        }
        ["export", who, out] => {
            let found = batch::load_all()?;
            let made = found.iter().find(|b| b.id == *who || b.name.eq_ignore_ascii_case(who)).with_context(|| format!("no batch {who}"))?;
            export(made, Path::new(out))?;
            println!("wrote {out}");
            Ok(0)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use super::*;

    fn sha1_of(bytes: &[u8]) -> String {
        hex(&Sha1::digest(bytes))
    }

    fn sha512_of(bytes: &[u8]) -> String {
        hex(&Sha512::digest(bytes))
    }

    fn file_entry(path: &str, bytes: &[u8], url: &str) -> serde_json::Value {
        serde_json::json!({ "path": path, "hashes": { "sha1": sha1_of(bytes), "sha512": sha512_of(bytes) }, "downloads": [url], "fileSize": bytes.len() })
    }

    fn index(files: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "formatVersion": 1, "game": "minecraft", "versionId": "1.0", "name": "Farm Pack", "files": files, "dependencies": { "minecraft": "1.21.1", "fabric-loader": "0.16.5" } })
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn pack_of(index: &serde_json::Value, extra: &[(&str, &[u8])]) -> ZipArchive<Cursor<Vec<u8>>> {
        let json = serde_json::to_vec(index).unwrap();
        let mut entries: Vec<(&str, &[u8])> = vec![(INDEX, &json)];
        entries.extend_from_slice(extra);
        ZipArchive::new(Cursor::new(zip_of(&entries))).unwrap()
    }

    fn modref(bytes: &[u8], file: &str, title: &str) -> ModRef {
        ModRef { project_id: format!("p-{title}"), version_id: format!("v-{title}"), title: title.into(), file_name: file.into(), url: format!("https://cdn.modrinth.com/data/{title}/{file}"), sha1: sha1_of(bytes) }
    }

    fn lookup_of(known: Vec<ModRef>) -> impl Fn(&[String]) -> Result<HashMap<String, ModRef>> {
        move |hashes: &[String]| Ok(known.iter().filter(|m| hashes.contains(&m.sha1)).map(|m| (m.sha1.clone(), m.clone())).collect())
    }

    fn served(files: Vec<(&'static str, Vec<u8>)>) -> impl Fn(&str, &Path, &str) -> Result<()> + Sync {
        let files: HashMap<String, Vec<u8>> = files.into_iter().map(|(u, b)| (u.to_owned(), b)).collect();
        move |url: &str, dest: &Path, _: &str| {
            let bytes = files.get(url).with_context(|| format!("404 {url}"))?;
            fs::create_dir_all(dest.parent().unwrap())?;
            fs::write(dest, bytes)?;
            Ok(())
        }
    }

    fn quiet() -> impl Fn(usize, usize) + Sync {
        |_, _| {}
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut found: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        found.sort();
        found
    }

    fn refused(e: &anyhow::Error) -> Option<Refused> {
        e.chain().find_map(|c| c.downcast_ref::<Refused>()).cloned()
    }

    #[test]
    fn hosts_are_allowed_by_exact_name_over_https() {
        for ok in ["https://cdn.modrinth.com/data/x/a.jar", "https://github.com/o/r/releases/download/v1/a.jar", "https://raw.githubusercontent.com/o/r/main/a.zip", "https://gitlab.com/o/r/-/raw/a.jar"] {
            assert!(allowed(ok), "{ok}");
        }
        for bad in ["http://cdn.modrinth.com/a.jar", "https://evil.com/a.jar", "https://cdn.modrinth.com.evil.com/a.jar", "https://evilgithub.com/a.jar", "https://user@evil.com/github.com", "file:///etc/passwd", "cdn.modrinth.com/a.jar", ""] {
            assert!(!allowed(bad), "{bad}");
        }
    }

    #[test]
    fn the_index_gives_versions_and_files_and_skips_server_only_ones() {
        let mut idx = index(vec![file_entry("mods/a.jar", b"a", "https://cdn.modrinth.com/a.jar"), file_entry("mods/server.jar", b"s", "https://cdn.modrinth.com/s.jar")]);
        idx["files"][1]["env"] = serde_json::json!({ "client": "unsupported", "server": "required" });
        idx["files"][0]["downloads"] = serde_json::json!(["https://evil.com/a.jar", "https://cdn.modrinth.com/a.jar"]);
        let pack = parse_index(&serde_json::to_vec(&idx).unwrap()).unwrap();
        assert_eq!((pack.name.as_str(), pack.game.as_str(), pack.loader.as_str()), ("Farm Pack", "1.21.1", "0.16.5"));
        assert_eq!(pack.files, [Wanted { rel: PathBuf::from("mods").join("a.jar"), sha1: sha1_of(b"a"), sha512: sha512_of(b"a"), url: "https://cdn.modrinth.com/a.jar".into() }]);
    }

    #[test]
    fn bad_packs_are_refused_with_a_reason() {
        let parse = |idx: &serde_json::Value| refused(&parse_index(&serde_json::to_vec(idx).unwrap()).unwrap_err());
        assert_eq!(refused(&parse_index(b"{").unwrap_err()), Some(Refused::NotAPack));
        let mut idx = index(Vec::new());
        idx["formatVersion"] = 2.into();
        assert_eq!(parse(&idx), Some(Refused::NotAPack));
        let mut idx = index(Vec::new());
        idx["game"] = "minetest".into();
        assert_eq!(parse(&idx), Some(Refused::NotAPack));
        for (key, name) in OTHER_LOADERS {
            let mut idx = index(Vec::new());
            idx["dependencies"][key] = "1".into();
            assert_eq!(parse(&idx), Some(Refused::Loader(name.into())));
        }
        let mut idx = index(Vec::new());
        idx["dependencies"].as_object_mut().unwrap().remove("fabric-loader");
        assert_eq!(parse(&idx), Some(Refused::NoLoader));
        let mut idx = index(Vec::new());
        idx["dependencies"].as_object_mut().unwrap().remove("minecraft");
        assert_eq!(parse(&idx), Some(Refused::NoGame));
        let idx = index(vec![file_entry("mods/a.jar", b"a", "https://evil.com/a.jar")]);
        assert_eq!(parse(&idx), Some(Refused::Host("https://evil.com/a.jar".into())));
        let mut idx = index(vec![file_entry("mods/a.jar", b"a", "https://cdn.modrinth.com/a.jar")]);
        idx["files"][0]["hashes"].as_object_mut().unwrap().remove("sha512");
        assert_eq!(parse(&idx), Some(Refused::Hash("mods/a.jar".into())));
        let mut idx = index(vec![file_entry("mods/a.jar", b"a", "https://cdn.modrinth.com/a.jar")]);
        idx["files"][0]["hashes"]["sha1"] = "zz".into();
        assert_eq!(parse(&idx), Some(Refused::Hash("mods/a.jar".into())));
        let idx = index(vec![file_entry("../evil.jar", b"a", "https://cdn.modrinth.com/a.jar")]);
        assert_eq!(parse(&idx), Some(Refused::Path("../evil.jar".into())));
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for bad in ["", "/etc/passwd", "../x", "mods/../../x", "a/./b", "C:/x", "mods\\..\\x", "a//b", "mods/", "con", "config/NUL.txt", "lpt1", "x.", "x ", "a\u{0}b"] {
            assert!(safe_rel(bad).is_err(), "{bad:?}");
        }
        assert_eq!(safe_rel("config/sodium/a.json").unwrap(), Path::new("config").join("sodium").join("a.json"));
        assert_eq!(safe_rel("mods/console.jar").unwrap(), Path::new("mods").join("console.jar"));
    }

    #[test]
    fn overrides_map_to_the_batch_folder_by_name() {
        let t = |p: &str| target(&safe_rel(p).unwrap());
        assert_eq!(t("options.txt"), Some(PathBuf::from("options.txt")));
        assert_eq!(t("Options.TXT"), Some(PathBuf::from("options.txt")));
        assert_eq!(t("config/a.json"), Some(Path::new("config").join("a.json")));
        assert_eq!(t("Config/a.json"), Some(Path::new("config").join("a.json")));
        assert_eq!(t("resourcepacks/x.zip"), Some(Path::new("resourcepacks").join("x.zip")));
        assert_eq!(t("shaderpacks/x.zip"), Some(Path::new("shaderpacks").join("x.zip")));
        assert_eq!(t("kubejs/startup.js"), Some(Path::new("kubejs").join("startup.js")));
        assert_eq!(t("servers.dat"), Some(PathBuf::from("servers.dat")));
        for skipped in ["batch.json", "saves/w/level.dat", "logs/latest.log", "screenshots/a.png", "crash-reports/c.txt"] {
            assert_eq!(t(skipped), None, "{skipped}");
        }
        assert_eq!(mod_name(&safe_rel("mods/a.JAR").unwrap()).as_deref(), Some("a.JAR"));
        assert_eq!(mod_name(&safe_rel("mods/sub/a.jar").unwrap()), None);
        assert_eq!(mod_name(&safe_rel("mods/a.txt").unwrap()), None);
    }

    #[test]
    fn names_stay_free_and_short() {
        let taken = vec!["Farm".to_owned(), "farm (2)".to_owned()];
        assert_eq!(free_name("Farm", &taken), "Farm (3)");
        assert_eq!(free_name("  New  ", &taken), "New");
        assert_eq!(free_name("", &taken), FALLBACK_NAME);
        let long = "x".repeat(60);
        assert_eq!(free_name(&long, &[]).chars().count(), MAX_NAME);
        let taken = vec!["x".repeat(MAX_NAME)];
        let next = free_name(&long, &taken);
        assert!(next.ends_with(" (2)") && next.chars().count() == MAX_NAME, "{next}");
    }

    #[test]
    fn a_pack_becomes_a_batch_with_modrinth_mods_local_mods_and_overrides() {
        let out = tempfile::tempdir().unwrap();
        let idx = index(vec![
            file_entry("mods/sodium.jar", b"sodium", "https://cdn.modrinth.com/sodium.jar"),
            file_entry("mods/gh.jar", b"github mod", "https://github.com/o/r/releases/download/1/gh.jar"),
            file_entry("resourcepacks/pack.zip", b"rp", "https://cdn.modrinth.com/rp.zip"),
        ]);
        let mut zip = pack_of(&idx, &[
            ("overrides/config/sodium.json", b"{}"),
            ("overrides/options.txt", b"fov:1.0\n"),
            ("overrides/mods/mine.jar", b"my jar"),
            ("overrides/mods/manycraft-companion.jar", b"old companion"),
            ("overrides/servers.dat", b"servers"),
            ("overrides/saves/w/level.dat", b"world"),
            ("overrides/shaderpacks/s.zip", b"shader"),
            ("client-overrides/options.txt", b"fov:0.5\n"),
            ("other/ignored.txt", b"x"),
        ]);
        let known = vec![modref(b"sodium", "sodium-fabric.jar", "Sodium")];
        let fetch = served(vec![("https://cdn.modrinth.com/sodium.jar", b"sodium".to_vec()), ("https://github.com/o/r/releases/download/1/gh.jar", b"github mod".to_vec()), ("https://cdn.modrinth.com/rp.zip", b"rp".to_vec())]);
        let steps = std::sync::Mutex::new(Vec::new());
        let progress = |done: usize, total: usize| steps.lock().unwrap().push((done, total));

        let made = import_pack(&mut zip, out.path(), &["Other".into()], &lookup_of(known.clone()), &fetch, &AtomicBool::new(false), &progress).unwrap();

        assert_eq!((made.name.as_str(), made.game_version.as_str(), made.loader_version.as_deref()), ("Farm Pack", "1.21.1", Some("0.16.5")));
        assert_eq!(made.mods, known);
        assert_eq!(made.local_mods, ["gh.jar", "mine.jar"]);
        let dest = out.path().join(&made.id);
        assert_eq!(names(&dest.join("mods")), ["gh.jar", "mine.jar", "sodium-fabric.jar"]);
        assert_eq!(fs::read(dest.join("mods").join("sodium-fabric.jar")).unwrap(), b"sodium");
        assert_eq!(fs::read(dest.join("config").join("sodium.json")).unwrap(), b"{}");
        assert_eq!(fs::read(dest.join("options.txt")).unwrap(), b"fov:0.5\n");
        assert_eq!(fs::read(dest.join("resourcepacks").join("pack.zip")).unwrap(), b"rp");
        assert_eq!(fs::read(dest.join("shaderpacks").join("s.zip")).unwrap(), b"shader");
        let top: Vec<String> = names(&dest).into_iter().filter(|n| !n.ends_with(".lock")).collect();
        assert_eq!(fs::read(dest.join("servers.dat")).unwrap(), b"servers");
        assert_eq!(top, ["batch.json", "config", "mods", "options.txt", "resourcepacks", "servers.dat", "shaderpacks"]);
        let saved: Batch = serde_json::from_slice(&fs::read(dest.join("batch.json")).unwrap()).unwrap();
        assert_eq!(saved, made);
        let steps = steps.into_inner().unwrap();
        assert_eq!((steps.first(), steps.last()), (Some(&(0, 4)), Some(&(4, 4))));
    }

    #[test]
    fn a_hash_mismatch_or_a_failed_download_leaves_nothing_behind() {
        let out = tempfile::tempdir().unwrap();
        let idx = index(vec![file_entry("mods/a.jar", b"a", "https://cdn.modrinth.com/a.jar")]);
        let none = lookup_of(Vec::new());
        let tampered = served(vec![("https://cdn.modrinth.com/a.jar", b"tampered".to_vec())]);
        let err = import_pack(&mut pack_of(&idx, &[]), out.path(), &[], &none, &tampered, &AtomicBool::new(false), &quiet()).unwrap_err();
        assert_eq!(refused(&err), Some(Refused::Hash("mods/a.jar".into())));
        let missing = served(Vec::new());
        assert!(import_pack(&mut pack_of(&idx, &[]), out.path(), &[], &none, &missing, &AtomicBool::new(false), &quiet()).is_err());
        let failing = |_: &[String]| -> Result<HashMap<String, ModRef>> { bail!("offline") };
        let ok = served(vec![("https://cdn.modrinth.com/a.jar", b"a".to_vec())]);
        let err = format!("{:#}", import_pack(&mut pack_of(&idx, &[]), out.path(), &[], &failing, &ok, &AtomicBool::new(false), &quiet()).unwrap_err());
        assert!(err.contains("Modrinth"), "{err}");
        assert!(names(out.path()).is_empty());
    }

    #[test]
    fn a_sha512_mismatch_is_caught_even_when_sha1_matches() {
        let out = tempfile::tempdir().unwrap();
        let mut idx = index(vec![file_entry("mods/a.jar", b"a", "https://cdn.modrinth.com/a.jar")]);
        idx["files"][0]["hashes"]["sha512"] = sha512_of(b"other").into();
        let fetch = served(vec![("https://cdn.modrinth.com/a.jar", b"a".to_vec())]);
        let err = import_pack(&mut pack_of(&idx, &[]), out.path(), &[], &lookup_of(Vec::new()), &fetch, &AtomicBool::new(false), &quiet()).unwrap_err();
        assert_eq!(refused(&err), Some(Refused::Hash("mods/a.jar".into())));
        assert!(names(out.path()).is_empty());
    }

    #[test]
    fn zip_slip_in_overrides_is_refused_before_anything_is_written() {
        let out = tempfile::tempdir().unwrap();
        for evil in ["overrides/../../evil.txt", "overrides//etc/passwd", "client-overrides/C:/x.txt", "overrides/config\\..\\..\\x"] {
            let mut zip = pack_of(&index(Vec::new()), &[(evil, b"x")]);
            let err = import_pack(&mut zip, out.path(), &[], &lookup_of(Vec::new()), &served(Vec::new()), &AtomicBool::new(false), &quiet()).unwrap_err();
            assert!(matches!(refused(&err), Some(Refused::Path(_))), "{evil}: {err:#}");
        }
        assert!(names(out.path()).is_empty());
        assert!(!out.path().parent().unwrap().join("evil.txt").exists());
    }

    #[test]
    fn cancelling_stops_and_removes_the_half_batch() {
        let out = tempfile::tempdir().unwrap();
        let idx = index(vec![file_entry("mods/a.jar", b"a", "https://cdn.modrinth.com/a.jar")]);
        let cancel = AtomicBool::new(false);
        let fetch = |_: &str, _: &Path, _: &str| -> Result<()> {
            cancel.store(true, Ordering::SeqCst);
            bail!("stopped")
        };
        assert!(import_pack(&mut pack_of(&idx, &[("overrides/config/a.json", b"{}")]), out.path(), &[], &lookup_of(Vec::new()), &fetch, &cancel, &quiet()).is_err());
        let err = import_pack(&mut pack_of(&idx, &[]), out.path(), &[], &lookup_of(Vec::new()), &served(Vec::new()), &cancel, &quiet()).unwrap_err();
        assert_eq!(refused(&err), Some(Refused::Cancelled));
        assert!(names(out.path()).is_empty());
    }

    #[test]
    fn export_then_import_gives_the_same_batch() {
        let home = tempfile::tempdir().unwrap();
        let src = home.path().join("src");
        let mods = src.join("mods");
        fs::create_dir_all(src.join("config").join("deep")).unwrap();
        fs::create_dir_all(src.join("resourcepacks")).unwrap();
        fs::create_dir_all(src.join("saves").join("w")).unwrap();
        fs::create_dir_all(src.join("logs")).unwrap();
        fs::create_dir_all(&mods).unwrap();
        fs::write(mods.join("sodium-fabric.jar"), b"sodium").unwrap();
        fs::write(mods.join("mine.jar"), b"my jar").unwrap();
        fs::write(mods.join(fleet::COMPANION_FILE), b"companion").unwrap();
        fs::write(src.join("config").join("deep").join("a.toml"), b"a=1").unwrap();
        fs::write(src.join("resourcepacks").join("rp.zip"), b"rp").unwrap();
        fs::write(src.join("options.txt"), b"fov:1.0\n").unwrap();
        fs::write(src.join("saves").join("w").join("level.dat"), b"world").unwrap();
        fs::write(src.join("logs").join("latest.log"), b"token=SECRET").unwrap();
        let sodium = modref(b"sodium", "sodium-fabric.jar", "Sodium");
        let lithium = modref(b"lithium", "lithium.jar", "Lithium");
        let mut b = Batch::new("Round Trip", "1.21.1");
        b.loader_version = Some("0.16.5".into());
        b.mods = vec![sodium.clone(), lithium.clone()];
        b.local_mods = vec!["mine.jar".into()];
        b.server = Some("play.example.net".into());
        b.join_commands = vec!["/login hunter2".into()];
        b.accounts = vec![uuid::Uuid::from_u128(7)];
        fs::write(src.join("batch.json"), serde_json::to_vec(&b).unwrap()).unwrap();
        let cdn = served(vec![("https://cdn.modrinth.com/data/Sodium/sodium-fabric.jar", b"sodium".to_vec()), ("https://cdn.modrinth.com/data/Lithium/lithium.jar", b"lithium".to_vec())]);
        let file = home.path().join("out.mrpack");

        export_with(&b, &src, "0.16.5", &file, &cdn).unwrap();

        let mut zip = ZipArchive::new(File::open(&file).unwrap()).unwrap();
        let mut inside: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        inside.sort();
        assert_eq!(inside, ["modrinth.index.json", "overrides/config/deep/a.toml", "overrides/mods/mine.jar", "overrides/options.txt", "overrides/resourcepacks/rp.zip"]);
        let mut raw = String::new();
        zip.by_name(INDEX).unwrap().read_to_string(&mut raw).unwrap();
        for secret in ["hunter2", "play.example.net", "SECRET", "00000000-0000-0000-0000-000000000007"] {
            assert!(!raw.contains(secret), "{secret} leaked into the index");
        }
        let pack = parse_index(raw.as_bytes()).unwrap();
        assert_eq!(pack.files.len(), 2);
        assert_eq!(pack.files[1].sha512, sha512_of(b"lithium"));

        let out = tempfile::tempdir().unwrap();
        let back = import_pack(&mut zip, out.path(), &[], &lookup_of(vec![sodium, lithium]), &cdn, &AtomicBool::new(false), &quiet()).unwrap();
        assert_eq!((&back.name, &back.game_version, &back.loader_version, &back.mods, &back.local_mods), (&b.name, &b.game_version, &b.loader_version, &b.mods, &b.local_mods));
        let dest = out.path().join(&back.id);
        assert_eq!(fs::read(dest.join("config").join("deep").join("a.toml")).unwrap(), b"a=1");
        assert_eq!(fs::read(dest.join("resourcepacks").join("rp.zip")).unwrap(), b"rp");
        assert_eq!(fs::read(dest.join("options.txt")).unwrap(), b"fov:1.0\n");
        assert_eq!(names(&dest.join("mods")), ["lithium.jar", "mine.jar", "sodium-fabric.jar"]);
    }

    #[test]
    fn export_never_follows_links_out_of_the_batch() {
        let home = tempfile::tempdir().unwrap();
        let src = home.path().join("src");
        let outside = home.path().join("outside");
        fs::create_dir_all(src.join("config")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        fs::write(src.join("config").join("a.json"), b"{}").unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, src.join("config").join("link")).is_ok();
        #[cfg(windows)]
        let linked = junction::create(&outside, src.join("config").join("link")).is_ok();
        let b = Batch::new("Links", "1.21.1");
        let file = home.path().join("out.mrpack");
        export_with(&b, &src, "0.16.5", &file, &served(Vec::new())).unwrap();
        let zip = ZipArchive::new(File::open(&file).unwrap()).unwrap();
        let inside: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        assert!(linked);
        assert!(inside.contains(&"overrides/config/a.json".to_owned()));
        assert!(!inside.iter().any(|n| n.contains("secret")), "{inside:?}");
    }
}
