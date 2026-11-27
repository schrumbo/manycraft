//! Mods from Modrinth: search, version resolution with required dependencies, verified downloads into an instance.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Deserialize;

use super::batch::ModRef;
use super::compat::{Miss, Needs, Target};
use super::i18n::{tr, trf};
use super::instance::{Instance, Loader};
use super::{install, net, online, paths};

const API: &str = "https://api.modrinth.com/v2";

/// One mod of the performance preset: its name for notes, the Modrinth project ids to try in turn (a mod published per
/// loader has one id each) and whether it only runs on Windows.
struct Slot {
    name: &'static str,
    ids: &'static [&'static str],
    windows_only: bool,
}

const fn slot(name: &'static str, ids: &'static [&'static str]) -> Slot {
    Slot { name, ids, windows_only: false }
}

const PRESET: [Slot; 15] = [
    slot("Debugify", &["QwxR6Gcd"]),
    slot("Sodium", &["AANobbMI"]),
    slot("Lithium", &["gvQqBUqZ"]),
    slot("EntityCulling", &["NNAgCjsB"]),
    slot("FastNoise", &["OnlVIpq5"]),
    slot("FerriteCore", &["uXXizFIs"]),
    slot("Gnetum", &["7MoE34WK"]),
    slot("ImmediatelyFast", &["5ZwdcRci"]),
    slot("Ixeris", &["p8RJPJIC"]),
    slot("ModernFix", &["nmDcB62a"]),
    slot("Noxesium", &["Kw7Sm3Xf"]),
    slot("Optimized Block Entities", &["AtOSAunf"]),
    slot("Sodium Extra", &["PtjYWJkn"]),
    slot("Very Many Players", &["wnEe9KBa", "LlSMGYzF"]),
    Slot { name: "Windows Priority Boost", ids: &["7ZoK90Xo"], windows_only: true },
];
/// Modrinth project id of Fabric API, which the companion mod needs in every instance.
pub const FABRIC_API: &str = "P7dR8mSH";
const POOL_THREADS: usize = 8;
const HASH_CHUNK: usize = 200;
const PROJECT_CHUNK: usize = 100;
const KEPT_PAGES: usize = 300;
const KEPT_JARS: usize = 100;
const CHECKED: usize = 8;
/// How many results one search page holds.
pub const PAGE: usize = 20;
const CHANGELOGS: usize = 5;
pub(super) const DISABLED: &str = ".disabled";
const LEDGER: &str = "mods.managed";
const USAGE: &str = "usage: manycraft mods search <query> [--version <version>]\n       manycraft mods resolve <slug>... [--version <version>]";

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Hit {
    pub title: String,
    pub slug: String,
    pub project_id: String,
    pub description: String,
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub author: String,
    /// The game versions the project has a file for.
    #[serde(default)]
    pub versions: Vec<String>,
    /// The project's categories; the mod loaders it supports are among them.
    #[serde(default)]
    pub categories: Vec<String>,
}

/// Whether a search result runs on a game version with a loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Fits,
    OtherVersion,
    OtherLoader,
}

impl Hit {
    /// Whether the mod has a file for `game_version` and `loader`, and if not, which of the two is missing.
    #[cfg(test)]
    pub fn support(&self, game_version: &str, loader: &Loader) -> Support {
        if keys(loader).is_empty() {
            return Support::OtherLoader;
        }
        self.fits(&Filter::of(Kind::Mod, game_version, loader, None))
    }

    /// Whether the project has a file the filter asks for, and if not, which of loader and version is missing.
    pub fn fits(&self, filter: &Filter) -> Support {
        if !filter.loaders.is_empty() && !self.categories.iter().any(|c| filter.loaders.contains(c)) {
            Support::OtherLoader
        } else if filter.game_version.as_ref().is_some_and(|wanted| !self.versions.contains(wanted)) {
            Support::OtherVersion
        } else {
            Support::Fits
        }
    }
}

fn keys(loader: &Loader) -> &'static [&'static str] {
    match loader {
        Loader::Vanilla => &[],
        Loader::Fabric(_) => &["fabric"],
        Loader::Quilt(_) => &["quilt", "fabric"],
        Loader::Forge(_) => &["forge"],
        Loader::NeoForge(_) => &["neoforge"],
    }
}

fn preset(loader: &Loader, windows: bool) -> Vec<&'static Slot> {
    match loader {
        Loader::Vanilla => Vec::new(),
        _ => PRESET.iter().filter(|s| windows || !s.windows_only).collect(),
    }
}

/// The names of the performance preset's mods for `loader` on this system, in preset order.
pub fn default_names(loader: &Loader) -> Vec<&'static str> {
    preset(loader, cfg!(windows)).iter().map(|slot| slot.name).collect()
}

/// How search results are ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sort {
    Relevance,
    Downloads,
    Updated,
    Newest,
}

impl Sort {
    /// Every order, in the order the browser offers them.
    pub const ALL: [Sort; 4] = [Sort::Relevance, Sort::Downloads, Sort::Updated, Sort::Newest];

    fn index(self) -> &'static str {
        match self {
            Sort::Relevance => "relevance",
            Sort::Downloads => "downloads",
            Sort::Updated => "updated",
            Sort::Newest => "newest",
        }
    }
}

/// The kinds of projects the browser finds on Modrinth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Mod,
    ResourcePack,
    Shader,
    DataPack,
    Modpack,
}

impl Kind {
    /// Every kind, in the order of the browser's tabs.
    pub const ALL: [Kind; 5] = [Kind::Mod, Kind::ResourcePack, Kind::Shader, Kind::DataPack, Kind::Modpack];

    /// Modrinth's name for this kind of project.
    pub fn project_type(self) -> &'static str {
        match self {
            Kind::Mod => "mod",
            Kind::ResourcePack => "resourcepack",
            Kind::Shader => "shader",
            Kind::DataPack => "datapack",
            Kind::Modpack => "modpack",
        }
    }

    /// Modrinth's categories for the kind that the browser offers as a filter.
    pub fn categories(self) -> &'static [&'static str] {
        match self {
            Kind::Mod | Kind::DataPack => &["adventure", "decoration", "economy", "equipment", "food", "game-mechanics", "library", "magic", "management", "minigame", "mobs", "optimization", "social", "storage", "technology", "transportation", "utility", "worldgen"],
            Kind::ResourcePack => &["combat", "decoration", "modded", "realistic", "simplistic", "themed", "tweaks", "utility", "vanilla-like"],
            Kind::Shader => &["cartoon", "fantasy", "realistic", "semi-realistic", "vanilla-like"],
            Kind::Modpack => &["adventure", "challenging", "combat", "kitchen-sink", "lightweight", "magic", "multiplayer", "optimization", "quests", "technology"],
        }
    }
}

/// What a search looks for: a kind of project with a file for a game version and one of some loaders.
/// No game version or no loaders means any.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Filter {
    pub kind: Kind,
    pub game_version: Option<String>,
    pub loaders: Vec<String>,
}

impl Filter {
    /// What fits an instance on `game_version` with `loader`: mods for its loader, modpacks for exactly that loader, resource packs and data
    /// packs as such, shader packs for `shader_loader` (Iris when it has none yet).
    pub fn of(kind: Kind, game_version: &str, loader: &Loader, shader_loader: Option<&str>) -> Self {
        let loaders: Vec<String> = match kind {
            Kind::Mod => keys(loader).iter().map(|k| (*k).to_owned()).collect(),
            Kind::Modpack => keys(loader).iter().take(1).map(|k| (*k).to_owned()).collect(),
            Kind::ResourcePack => vec!["minecraft".into()],
            Kind::Shader => vec![shader_loader.unwrap_or("iris").to_owned()],
            Kind::DataPack => vec!["datapack".into()],
        };
        Self { kind, game_version: (!game_version.is_empty()).then(|| game_version.to_owned()), loaders }
    }

    /// Every project of `kind`, for any version and loader.
    pub fn any(kind: Kind) -> Self {
        Self { kind, game_version: None, loaders: Vec::new() }
    }
}

/// One search: what it filters for, the words, the order and an optional category.
/// With `all` the version and loader filters are left out and [`Hit::fits`] tells the results apart.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Search {
    pub filter: Filter,
    pub query: String,
    pub sort: Sort,
    pub category: Option<String>,
    pub all: bool,
}

/// One page of search results and how many results the whole search has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub hits: Vec<Hit>,
    pub total: usize,
    /// When the results were fetched, for a page kept from an earlier search because there was no connection.
    pub saved: Option<SystemTime>,
}

/// The default mods for a game version, why any of them is missing and which.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defaults {
    pub mods: Vec<ModRef>,
    pub notes: Vec<String>,
    /// The names of the preset mods that were left out, in preset order.
    pub skipped: Vec<String>,
}

/// A newer file for a mod of an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub current: ModRef,
    pub latest: ModRef,
    /// Version number and changelog (Markdown) of every version after the current one up to the latest, newest first, at most 5.
    pub changelog: Vec<(String, String)>,
}

#[derive(Debug)]
struct Unavailable(String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

fn is_unavailable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unavailable>().is_some()
}

#[derive(Deserialize)]
struct SearchResponse {
    hits: Vec<Hit>,
    #[serde(default)]
    total_hits: usize,
}

#[derive(Deserialize)]
struct Project {
    id: String,
    title: String,
}

/// One version of a project: its files, what it runs on and what it needs.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version_number: String,
    pub version_type: String,
    pub date_published: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<File>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub changelog: Option<String>,
}

/// One file of a version.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct File {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    pub hashes: Hashes,
    #[serde(default)]
    pub size: u64,
}

/// The hashes Modrinth publishes for a file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Hashes {
    pub sha1: String,
}

/// Another project a version needs, may use, or cannot run with: `dependency_type` is `required`, `optional`,
/// `incompatible` or `embedded`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Dependency {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    pub dependency_type: String,
}

/// What a project page shows: texts, numbers, dates and the gallery.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Details {
    pub id: String,
    #[serde(default)]
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// The long description, in Markdown with some HTML.
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub followers: u64,
    /// RFC 3339.
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub gallery: Vec<Shot>,
    #[serde(default)]
    pub icon_url: Option<String>,
    /// The project's own pages: where its source, wiki, bug tracker and Discord live.
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub wiki_url: Option<String>,
    #[serde(default)]
    pub issues_url: Option<String>,
    #[serde(default)]
    pub discord_url: Option<String>,
}

/// One picture of a project's gallery.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Shot {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub featured: bool,
    #[serde(default)]
    pub ordering: i64,
    /// The picture at full size; `url` is a small copy.
    #[serde(default)]
    pub raw_url: Option<String>,
}

trait Source: Sync {
    fn search(&self, query: &str, game_version: &str, loader: &Loader) -> Result<Vec<Hit>>;
    fn project(&self, key: &str) -> Result<Option<Project>>;
    fn versions(&self, project_id: &str, game_version: &str, loader: &Loader) -> Result<Vec<Version>>;
    fn version(&self, id: &str) -> Result<Version>;
    fn versions_by_hash(&self, hashes: &[String]) -> Result<HashMap<String, Version>>;
    fn projects(&self, ids: &[String]) -> Result<Vec<Project>>;
    fn target(&self, game_version: &str, loader: &Loader) -> Target;
    fn needs(&self, found: &ModRef, quilt: bool) -> Result<Needs>;
}

struct Api {
    agent: ureq::Agent,
}

impl Api {
    fn new() -> Result<Self> {
        Ok(Self { agent: net::agent(None)? })
    }
}

impl Source for Api {
    fn search(&self, query: &str, game_version: &str, loader: &Loader) -> Result<Vec<Hit>> {
        Ok(net::get_json::<SearchResponse>(&self.agent, &search_url(query, game_version, loader, Sort::Relevance, false, 0))?.hits)
    }

    fn project(&self, key: &str) -> Result<Option<Project>> {
        match net::get_json(&self.agent, &format!("{API}/project/{}", encode(key))) {
            Ok(project) => Ok(Some(project)),
            Err(e) if matches!(e.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(404))) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn versions(&self, project_id: &str, game_version: &str, loader: &Loader) -> Result<Vec<Version>> {
        net::get_json(&self.agent, &versions_url(project_id, game_version, loader))
    }

    fn version(&self, id: &str) -> Result<Version> {
        net::get_json(&self.agent, &format!("{API}/version/{}", encode(id)))
    }

    fn versions_by_hash(&self, hashes: &[String]) -> Result<HashMap<String, Version>> {
        net::post_json(&self.agent, &format!("{API}/version_files"), &serde_json::json!({ "hashes": hashes, "algorithm": "sha1" }))
    }

    fn projects(&self, ids: &[String]) -> Result<Vec<Project>> {
        net::get_json(&self.agent, &format!("{API}/projects?ids={}", encode(&serde_json::json!(ids).to_string())))
    }

    fn target(&self, game_version: &str, loader: &Loader) -> Target {
        if checked(loader) { install::mod_target(game_version, loader) } else { Target::default() }
    }

    fn needs(&self, found: &ModRef, quilt: bool) -> Result<Needs> {
        needs_in(&self.agent, &jar_cache(), found, quilt)
    }
}

fn needs_in(agent: &ureq::Agent, cache: &Path, found: &ModRef, quilt: bool) -> Result<Needs> {
    let Some(path) = cached_jar(cache, &found.sha1) else { return Ok(Needs::default()) };
    let open = || std::fs::File::options().read(true).write(true).open(&path).ok();
    let file = match open() {
        Some(file) => {
            let _ = file.set_modified(SystemTime::now());
            Some(file)
        }
        None => match net::fetch_verified(agent, &found.url, &path, Some(&found.sha1)) {
            Ok(()) => {
                let file = open();
                prune(cache, KEPT_JARS);
                file
            }
            Err(e) if net::no_connection(&e) => return Err(e.context(format!("cannot download {}", found.title))),
            Err(_) => None,
        },
    };
    Ok(file.and_then(|file| Needs::read(std::io::BufReader::new(file), quilt).ok()).unwrap_or_default())
}

fn checked(loader: &Loader) -> bool {
    matches!(loader, Loader::Fabric(_) | Loader::Quilt(_))
}

fn jar_cache() -> PathBuf {
    paths::data_dir().join("cache").join("jars")
}

fn cached_jar(cache: &Path, sha1: &str) -> Option<PathBuf> {
    (sha1.len() == 40 && sha1.bytes().all(|b| b.is_ascii_hexdigit())).then(|| cache.join(format!("{}.jar", sha1.to_ascii_lowercase())))
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn search_url(query: &str, game_version: &str, loader: &Loader, sort: Sort, all: bool, offset: usize) -> String {
    search_url_for(&Search { filter: Filter::of(Kind::Mod, game_version, loader, None), query: query.to_owned(), sort, category: None, all }, offset)
}

fn search_url_for(search: &Search, offset: usize) -> String {
    let filter = &search.filter;
    let mut facets = vec![vec![format!("project_type:{}", filter.kind.project_type())]];
    if !search.all {
        if !filter.loaders.is_empty() {
            facets.push(filter.loaders.iter().map(|key| format!("categories:{key}")).collect());
        }
        if let Some(version) = &filter.game_version {
            facets.push(vec![format!("versions:{version}")]);
        }
    }
    if let Some(category) = &search.category {
        facets.push(vec![format!("categories:{category}")]);
    }
    if matches!(filter.kind, Kind::Mod | Kind::Modpack) {
        facets.push(vec!["client_side:required".to_owned(), "client_side:optional".to_owned()]);
    }
    format!("{API}/search?query={}&facets={}&index={}&offset={offset}&limit={PAGE}", encode(search.query.trim()), encode(&serde_json::json!(facets).to_string()), search.sort.index())
}

fn versions_url(project_id: &str, game_version: &str, loader: &Loader) -> String {
    format!(
        "{API}/project/{}/version?loaders={}&game_versions={}",
        encode(project_id),
        encode(&serde_json::json!(keys(loader)).to_string()),
        encode(&serde_json::json!([game_version]).to_string()),
    )
}

fn versions_url_for(project_id: &str, filter: &Filter) -> String {
    let mut url = format!("{API}/project/{}/version", encode(project_id));
    let mut query = Vec::new();
    if !filter.loaders.is_empty() {
        query.push(format!("loaders={}", encode(&serde_json::json!(filter.loaders).to_string())));
    }
    if let Some(version) = &filter.game_version {
        query.push(format!("game_versions={}", encode(&serde_json::json!([version]).to_string())));
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

/// The newest version of `versions` the filter takes, releases before betas before alphas.
pub fn newest_for<'a>(versions: &'a [Version], filter: &Filter) -> Option<&'a Version> {
    versions
        .iter()
        .filter(|v| version_fits(v, filter))
        .max_by(|a, b| rank(&a.version_type).cmp(&rank(&b.version_type)).then_with(|| a.date_published.cmp(&b.date_published)))
}

/// Whether `filter` takes `version`: one of its loaders and its game version.
pub fn version_fits(version: &Version, filter: &Filter) -> bool {
    (filter.loaders.is_empty() || version.loaders.iter().any(|l| filter.loaders.contains(l))) && filter.game_version.as_ref().is_none_or(|g| version.game_versions.contains(g))
}

fn rank(version_type: &str) -> u8 {
    match version_type {
        "release" => 2,
        "beta" => 1,
        _ => 0,
    }
}

fn fits(version: &Version, game_version: &str, loader: &Loader) -> bool {
    version.loaders.iter().any(|l| keys(loader).contains(&l.as_str())) && version.game_versions.iter().any(|g| g == game_version)
}

#[cfg(test)]
fn newest<'a>(versions: &'a [Version], game_version: &str, loader: &Loader) -> Option<&'a Version> {
    ranked(versions, game_version, loader).into_iter().next()
}

fn ranked<'a>(versions: &'a [Version], game_version: &str, loader: &Loader) -> Vec<&'a Version> {
    let mut fit: Vec<&Version> = versions.iter().filter(|v| fits(v, game_version, loader)).collect();
    fit.sort_by(|a, b| rank(&b.version_type).cmp(&rank(&a.version_type)).then_with(|| b.date_published.cmp(&a.date_published)));
    fit
}

enum Pick<'a> {
    Found(&'a Version, ModRef),
    Misfit(Miss),
    Missing,
}

fn pick<'a>(src: &dyn Source, title: &str, versions: &'a [Version], game_version: &str, loader: &Loader, target: &Target, stop: impl Fn(&Version) -> bool) -> Result<Pick<'a>> {
    let mut misfit = None;
    for version in ranked(versions, game_version, loader).into_iter().take(CHECKED) {
        if stop(version) {
            break;
        }
        let found = mod_ref(title, version)?;
        if !checked(loader) {
            return Ok(Pick::Found(version, found));
        }
        match src.needs(&found, target.quilt)?.check(target) {
            Ok(()) => return Ok(Pick::Found(version, found)),
            Err(miss) => {
                misfit.get_or_insert(miss);
            }
        }
    }
    Ok(misfit.map_or(Pick::Missing, Pick::Misfit))
}

fn misfit_note(title: &str, miss: Miss, game_version: &str, loader: &Loader, target: &Target) -> String {
    match miss {
        Miss::Loader => {
            let version = if target.quilt { &target.quilt_loader } else { &target.fabricloader };
            trf("„%1$@“ läuft nicht mit %2$@ %3$@.", &[&title, &loader.name(), &version.as_deref().unwrap_or_default()])
        }
        Miss::Minecraft => trf("„%1$@“ läuft nicht mit Minecraft %2$@.", &[&title, &game_version]),
        Miss::Java => trf("„%1$@“ läuft nicht mit Java %2$@.", &[&title, &target.java.unwrap_or_default()]),
    }
}

/// The primary file of `version` as a [`ModRef`] named `title`.
pub fn version_ref(title: &str, version: &Version) -> Result<ModRef> {
    mod_ref(title, version)
}

fn mod_ref(title: &str, version: &Version) -> Result<ModRef> {
    let file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or(version.files.first())
        .with_context(|| format!("{title} {} has no file", version.version_number))?;
    Ok(file_ref(title, version, file))
}

fn file_ref(title: &str, version: &Version, file: &File) -> ModRef {
    ModRef {
        project_id: version.project_id.clone(),
        version_id: version.id.clone(),
        title: title.to_owned(),
        file_name: file.filename.clone(),
        url: file.url.clone(),
        sha1: file.hashes.sha1.clone(),
    }
}

#[derive(Debug)]
struct Entry {
    found: ModRef,
    required: Vec<Dependency>,
    incompatible: Vec<String>,
}

fn dependencies_of<'a>(version: &'a Version, kind: &'a str) -> impl Iterator<Item = &'a Dependency> {
    version.dependencies.iter().filter(move |d| d.dependency_type == kind)
}

#[cfg(test)]
fn lookup(src: &dyn Source, key: &str, game_version: &str, loader: &Loader) -> Result<Entry> {
    lookup_for(src, key, game_version, loader, &src.target(game_version, loader))
}

fn lookup_for(src: &dyn Source, key: &str, game_version: &str, loader: &Loader, target: &Target) -> Result<Entry> {
    let project = src.project(key)?.ok_or_else(|| Unavailable(trf("„%@“ gibt es nicht auf Modrinth.", &[&key])))?;
    let versions = src.versions(&project.id, game_version, loader)?;
    let (version, found) = match pick(src, &project.title, &versions, game_version, loader, target, |_| false)? {
        Pick::Found(version, found) => (version, found),
        Pick::Misfit(miss) => return Err(Unavailable(misfit_note(&project.title, miss, game_version, loader, target)).into()),
        Pick::Missing => return Err(Unavailable(trf("„%1$@“ gibt es nicht für %2$@ %3$@.", &[&project.title, &loader.name(), &game_version])).into()),
    };
    Ok(Entry {
        found,
        required: dependencies_of(version, "required").cloned().collect(),
        incompatible: dependencies_of(version, "incompatible").filter_map(|d| d.project_id.clone()).collect(),
    })
}

/// Adds the required dependencies of `entries` to them, recursively. A dependency that has no build for the game and loader
/// is left out and explained in the returned notes, since most mods run without it; any other failure is an error.
fn expand(src: &dyn Source, entries: &mut Vec<Entry>, seen: &mut HashSet<String>, game_version: &str, loader: &Loader, target: &Target) -> Result<Vec<String>> {
    let mut skipped = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let parent = entries[i].found.title.clone();
        let required = entries[i].required.clone();
        for dep in required {
            let project_id = match (dep.project_id, dep.version_id) {
                (Some(project_id), _) => project_id,
                (None, Some(version_id)) => src.version(&version_id)?.project_id,
                (None, None) => continue,
            };
            if seen.contains(&project_id) {
                continue;
            }
            let entry = match lookup_for(src, &project_id, game_version, loader, target) {
                Ok(entry) => entry,
                Err(e) if is_unavailable(&e) => {
                    seen.insert(project_id);
                    skipped.push(trf("%1$@ „%2$@“ wird trotzdem installiert.", &[&e.to_string(), &parent]));
                    continue;
                }
                Err(e) => return Err(e.context(trf("Für „%@“ nötig", &[&parent]))),
            };
            seen.insert(entry.found.project_id.clone());
            entries.push(entry);
        }
        i += 1;
    }
    Ok(skipped)
}

fn check_compatible(entries: &[Entry]) -> Result<()> {
    for entry in entries {
        for id in &entry.incompatible {
            if let Some(other) = entries.iter().find(|e| &e.found.project_id == id) {
                bail!("{} is incompatible with {}", entry.found.title, other.found.title);
            }
        }
    }
    Ok(())
}

/// The mods a resolution settled on, with the required dependencies it had to leave out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub mods: Vec<ModRef>,
    /// One sentence per dependency that has no build for the game and loader.
    pub skipped: Vec<String>,
}

struct Graph {
    entries: Vec<Entry>,
    requested: usize,
    skipped: Vec<String>,
}

fn resolve_all<S: AsRef<str>>(src: &dyn Source, roots: &[S], game_version: &str, loader: &Loader) -> Result<Graph> {
    resolve_for(src, roots, game_version, loader, &src.target(game_version, loader))
}

fn resolve_for<S: AsRef<str>>(src: &dyn Source, roots: &[S], game_version: &str, loader: &Loader, target: &Target) -> Result<Graph> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let entry = lookup_for(src, root.as_ref(), game_version, loader, target)?;
        if seen.insert(entry.found.project_id.clone()) {
            entries.push(entry);
        }
    }
    let requested = entries.len();
    let skipped = expand(src, &mut entries, &mut seen, game_version, loader, target)?;
    check_compatible(&entries)?;
    Ok(Graph { entries, requested, skipped })
}

fn resolve_version_from(src: &dyn Source, version_id: &str, game_version: &str, loader: &Loader) -> Result<Resolved> {
    let version = src.version(version_id)?;
    let project = src.project(&version.project_id)?.ok_or_else(|| Unavailable(format!("{} is not on Modrinth", version.project_id)))?;
    let root = Entry {
        found: mod_ref(&project.title, &version)?,
        required: dependencies_of(&version, "required").cloned().collect(),
        incompatible: dependencies_of(&version, "incompatible").filter_map(|d| d.project_id.clone()).collect(),
    };
    let mut seen = HashSet::from([root.found.project_id.clone()]);
    let mut entries = vec![root];
    let skipped = expand(src, &mut entries, &mut seen, game_version, loader, &src.target(game_version, loader))?;
    check_compatible(&entries)?;
    Ok(Resolved { mods: entries.into_iter().map(|e| e.found).collect(), skipped })
}

fn clash(kept: &[Entry], fresh: &[Entry]) -> Option<(String, String)> {
    fresh.iter().find_map(|entry| {
        kept.iter().find(|k| k.incompatible.contains(&entry.found.project_id) || entry.incompatible.contains(&k.found.project_id)).map(|k| (entry.found.title.clone(), k.found.title.clone()))
    })
}

fn defaults_from(src: &dyn Source, game_version: &str, loader: &Loader) -> Result<Defaults> {
    defaults_with(src, game_version, loader, &preset(loader, cfg!(windows)))
}

fn defaults_with(src: &dyn Source, game_version: &str, loader: &Loader, slots: &[&Slot]) -> Result<Defaults> {
    let target = src.target(game_version, loader);
    let resolve_slot = |slot: &&Slot| -> Result<Result<Graph, String>> {
        let mut first = None;
        for id in slot.ids {
            match resolve_for(src, &[id], game_version, loader, &target) {
                Ok(graph) => return Ok(Ok(graph)),
                Err(e) if net::no_connection(&e) => return Err(e),
                Err(e) => {
                    first.get_or_insert_with(|| format!("{e:#}"));
                }
            }
        }
        Ok(Err(first.unwrap_or_default()))
    };
    let resolved = pool()?.install(|| slots.par_iter().map(resolve_slot).collect::<Result<Vec<_>>>())?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = HashSet::new();
    let mut notes = Vec::new();
    let mut skipped = Vec::new();
    for (slot, found) in slots.iter().zip(resolved) {
        match found {
            Ok(graph) => {
                let fresh: Vec<Entry> = graph.entries.into_iter().filter(|entry| !seen.contains(&entry.found.project_id)).collect();
                if let Some((title, other)) = clash(&entries, &fresh) {
                    notes.push(trf("„%1$@“ passt nicht zu „%2$@“ und wurde ausgelassen.", &[&title, &other]));
                    skipped.push(slot.name.to_owned());
                } else {
                    seen.extend(fresh.iter().map(|entry| entry.found.project_id.clone()));
                    entries.extend(fresh);
                    for note in graph.skipped {
                        if !notes.contains(&note) {
                            notes.push(note);
                        }
                    }
                }
            }
            Err(note) => {
                notes.push(note);
                skipped.push(slot.name.to_owned());
            }
        }
    }
    Ok(Defaults { mods: entries.into_iter().map(|e| e.found).collect(), notes, skipped })
}

fn updates_from(src: &dyn Source, game_version: &str, loader: &Loader, mods: &[ModRef]) -> Result<Vec<Update>> {
    let target = src.target(game_version, loader);
    let check = |current: &ModRef| -> Result<Option<Update>> {
        let versions = src.versions(&current.project_id, game_version, loader)?;
        let published = versions.iter().find(|v| v.id == current.version_id).map(|v| v.date_published.as_str());
        let reached = |v: &Version| v.id == current.version_id || published.is_some_and(|date| v.date_published.as_str() <= date);
        let Pick::Found(latest, found) = pick(src, &current.title, &versions, game_version, loader, &target, reached)? else {
            return Ok(None);
        };
        Ok(Some(Update { current: current.clone(), latest: found, changelog: changelog_since(&versions, latest, published, game_version, loader) }))
    };
    let found = pool()?.install(|| mods.par_iter().map(check).collect::<Result<Vec<_>>>())?;
    Ok(found.into_iter().flatten().collect())
}

fn changelog_since(versions: &[Version], latest: &Version, published: Option<&str>, game_version: &str, loader: &Loader) -> Vec<(String, String)> {
    let mut newer: Vec<&Version> = match published {
        Some(date) => versions.iter().filter(|v| fits(v, game_version, loader) && v.date_published.as_str() > date && v.date_published <= latest.date_published).collect(),
        None => vec![latest],
    };
    newer.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    newer.truncate(CHANGELOGS);
    newer.into_iter().map(|v| (v.version_number.clone(), v.changelog.clone().unwrap_or_default())).collect()
}

fn matched_from(src: &dyn Source, hashes: &[String]) -> Result<HashMap<String, ModRef>> {
    let mut wanted: Vec<String> = hashes.iter().map(|h| h.to_ascii_lowercase()).collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut found = Vec::new();
    for chunk in wanted.chunks(HASH_CHUNK) {
        for (hash, version) in src.versions_by_hash(chunk)? {
            found.push((hash.to_ascii_lowercase(), version));
        }
    }
    found.retain(|(hash, _)| wanted.binary_search(hash).is_ok());
    let mut ids: Vec<String> = found.iter().map(|(_, v)| v.project_id.clone()).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut titles = HashMap::new();
    for chunk in ids.chunks(PROJECT_CHUNK) {
        for project in src.projects(chunk)? {
            titles.insert(project.id, project.title);
        }
    }
    let mut matched = HashMap::new();
    for (hash, version) in found {
        let Some(file) = version.files.iter().find(|f| f.hashes.sha1.eq_ignore_ascii_case(&hash)) else {
            continue;
        };
        let title = titles.get(&version.project_id).map_or(file.filename.as_str(), String::as_str);
        let mut found = file_ref(title, &version, file);
        found.sha1 = hash.clone();
        matched.insert(hash, found);
    }
    Ok(matched)
}

fn replaced(instance: &Instance, selection: &[Update]) -> Result<Instance> {
    let mut next = instance.clone();
    for update in selection {
        let current = &update.current;
        let slot = next
            .mods
            .iter_mut()
            .find(|m| m.project_id == current.project_id && m.version_id == current.version_id)
            .with_context(|| format!("{} is no longer in the instance at that version", current.title))?;
        *slot = update.latest.clone();
        if let Some(kept) = next.disabled.iter_mut().find(|name| name.eq_ignore_ascii_case(&current.file_name)) {
            kept.clone_from(&update.latest.file_name);
        }
    }
    Ok(next)
}

fn update_with(instance: &mut Instance, selection: &[Update], sync: impl FnOnce(&Instance) -> Result<usize>) -> Result<usize> {
    if selection.is_empty() {
        return Ok(0);
    }
    let next = replaced(instance, selection)?;
    let fetched = sync(&next)?;
    *instance = next;
    Ok(fetched)
}

fn pool() -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new().num_threads(POOL_THREADS).build().context("cannot start download threads")
}

pub(super) fn safe_file_name(name: &str) -> Result<()> {
    if name.contains(['/', '\\', ':']) || name.chars().any(char::is_control) || Path::new(name).file_name().and_then(std::ffi::OsStr::to_str) != Some(name) {
        bail!("unsafe mod file name {name:?}");
    }
    Ok(())
}

pub(super) struct Found {
    pub name: String,
    pub on: Option<PathBuf>,
    pub off: Option<PathBuf>,
}

pub(super) fn jar_name(name: &str) -> Option<(&str, bool)> {
    let (base, off) = match name.len().checked_sub(DISABLED.len()).and_then(|at| Some((name.get(..at)?, name.get(at..)?))) {
        Some((base, tail)) if tail.eq_ignore_ascii_case(DISABLED) => (base, true),
        _ => (name, false),
    };
    let at = base.len().checked_sub(4)?;
    base.get(at..)?.eq_ignore_ascii_case(".jar").then_some((base, off))
}

pub(super) fn list_jars(dir: &Path) -> Result<HashMap<String, Found>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut found: HashMap<String, Found> = HashMap::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Some((base, off)) = jar_name(&name) else { continue };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let slot = found.entry(base.to_lowercase()).or_insert_with(|| Found { name: base.to_owned(), on: None, off: None });
        if off {
            slot.off = Some(entry.path());
        } else {
            slot.on = Some(entry.path());
            base.clone_into(&mut slot.name);
        }
    }
    Ok(found)
}

pub(super) fn rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).with_context(|| format!("cannot rename {} to {}", from.display(), to.display()))
}

fn place_local(local: &[String], disabled: &HashSet<String>, dir: &Path) -> Result<()> {
    let found = list_jars(dir)?;
    for name in local {
        if safe_file_name(name).is_err() {
            continue;
        }
        let key = name.to_lowercase();
        let Some(file) = found.get(&key) else { continue };
        match (&file.on, &file.off, disabled.contains(&key)) {
            (Some(on), None, true) => rename(on, &dir.join(format!("{}{DISABLED}", file.name)))?,
            (None, Some(off), false) => rename(off, &dir.join(&file.name))?,
            _ => {}
        }
    }
    Ok(())
}

/// What the file the ledger recorded looked like on disk: its size and modification time in nanoseconds since the epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    size: u64,
    mtime: u64,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = path.metadata().ok().filter(|m| m.is_file())?;
        let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(Self { size: meta.len(), mtime: u64::try_from(mtime.as_nanos()).ok()? })
    }
}

/// One line of the ledger: the SHA-1 a synced file had and, when known, the stamp it had then. A file whose stamp still
/// matches is the recorded file, so it is not hashed again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Recorded {
    sha1: String,
    stamp: Option<Stamp>,
}

impl Recorded {
    fn has(&self, path: &Path, sha1: &str) -> Result<bool> {
        if !path.is_file() {
            return Ok(false);
        }
        if self.sha1.eq_ignore_ascii_case(sha1) && self.stamp.is_some() && self.stamp == Stamp::of(path) {
            return Ok(true);
        }
        Ok(net::file_sha1(path)?.eq_ignore_ascii_case(sha1))
    }
}

fn place(agent: &ureq::Agent, jars: Option<&Path>, m: &ModRef, off: bool, dir: &Path, managed: &HashMap<String, Recorded>) -> Result<bool> {
    let (on_path, off_path) = (dir.join(&m.file_name), dir.join(format!("{}{DISABLED}", m.file_name)));
    let (want, other) = if off { (off_path, on_path) } else { (on_path, off_path) };
    let recorded = managed.get(&m.file_name.to_lowercase());
    let right = |path: &Path| -> Result<bool> {
        match recorded {
            Some(entry) => entry.has(path, &m.sha1),
            None => Ok(path.is_file() && net::file_sha1(path)?.eq_ignore_ascii_case(&m.sha1)),
        }
    };
    if right(&want)? {
        return Ok(false);
    }
    if right(&other)? {
        rename(&other, &want)?;
        return Ok(false);
    }
    let cached = jars.and_then(|jars| cached_jar(jars, &m.sha1)).filter(|path| path.is_file());
    if !cached.is_some_and(|path| net::copy_verified(&path, &want, &m.sha1).unwrap_or(false)) {
        net::fetch_verified(agent, &m.url, &want, Some(&m.sha1)).with_context(|| format!("cannot download {}", m.title))?;
    }
    if other.is_file() {
        std::fs::remove_file(&other).with_context(|| format!("cannot delete {}", other.display()))?;
    }
    Ok(true)
}

fn read_ledger(path: &Path) -> HashMap<String, Recorded> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (rest, last) = line.trim().rsplit_once(char::is_whitespace)?;
            let (rest, hash, stamp) = match last.parse::<u64>() {
                Ok(mtime) => {
                    let (rest, size) = rest.trim_end().rsplit_once(char::is_whitespace)?;
                    let (rest, hash) = rest.trim_end().rsplit_once(char::is_whitespace)?;
                    (rest, hash, Some(Stamp { size: size.parse().ok()?, mtime }))
                }
                Err(_) => (rest, last, None),
            };
            let (name, sha1) = (rest.trim().to_lowercase(), hash.to_lowercase());
            (!name.is_empty() && sha1.len() == 40 && sha1.bytes().all(|b| b.is_ascii_hexdigit())).then_some((name, Recorded { sha1, stamp }))
        })
        .collect()
}

pub(super) fn managed_files(instance_dir: &Path) -> HashMap<String, Recorded> {
    read_ledger(&instance_dir.join(LEDGER))
}

pub(super) fn is_managed(managed: &HashMap<String, Recorded>, name: &str, path: &Path) -> bool {
    managed.get(&name.to_lowercase()).is_some_and(|entry| entry.has(path, &entry.sha1).unwrap_or(false))
}

#[cfg(test)]
fn sync_into(agent: &ureq::Agent, mods: &[ModRef], local: &[String], disabled: &[String], dir: &Path, ledger: &Path) -> Result<usize> {
    sync_from(agent, None, mods, local, disabled, dir, ledger, &|_, _| {})
}

/// Called with how many mod files are in place of how many the instance lists.
pub type Progress<'a> = &'a (dyn Fn(usize, usize) + Sync);

#[allow(clippy::too_many_arguments)]
fn sync_from(agent: &ureq::Agent, jars: Option<&Path>, mods: &[ModRef], local: &[String], disabled: &[String], dir: &Path, ledger: &Path, progress: Progress) -> Result<usize> {
    let local_keys: HashSet<String> = local.iter().map(|name| name.to_lowercase()).collect();
    let off: HashSet<String> = disabled.iter().map(|name| name.to_lowercase()).collect();
    let mut keep = HashSet::new();
    let mut jobs = Vec::new();
    for m in mods {
        safe_file_name(&m.file_name)?;
        if local_keys.contains(&m.file_name.to_lowercase()) {
            bail!("{} is called {} like a local mod; rename or remove the local one", m.title, m.file_name);
        }
        if keep.insert(m.file_name.to_lowercase()) {
            jobs.push(m);
        }
    }
    let managed = read_ledger(ledger);
    let mut synced: Vec<(String, String)> = jobs.iter().map(|m| (m.file_name.to_lowercase(), m.sha1.to_lowercase())).collect();
    synced.sort_unstable();
    keep.extend(local_keys);
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    place_local(local, &off, dir)?;
    let (total, placed) = (jobs.len(), std::sync::atomic::AtomicUsize::new(0));
    progress(0, total);
    let fetched = pool()?.install(|| {
        jobs.par_iter()
            .map(|m| {
                let made = place(agent, jars, m, off.contains(&m.file_name.to_lowercase()), dir, &managed);
                progress(placed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1, total);
                made
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for (name, file) in list_jars(dir)? {
        if keep.contains(&name) || name == super::fleet::COMPANION_FILE {
            continue;
        }
        for path in file.on.iter().chain(&file.off).filter(|path| is_managed(&managed, &name, path)) {
            std::fs::remove_file(path).with_context(|| format!("cannot delete {}", path.display()))?;
        }
    }
    let found = list_jars(dir)?;
    let lines: String = synced
        .iter()
        .map(|(name, sha1)| match found.get(name).and_then(|f| f.on.iter().chain(&f.off).find_map(|p| Stamp::of(p))) {
            Some(Stamp { size, mtime }) => format!("{name} {sha1} {size} {mtime}\n"),
            None => format!("{name} {sha1}\n"),
        })
        .collect();
    net::write_atomic(ledger, lines.as_bytes())?;
    Ok(fetched.into_iter().filter(|downloaded| *downloaded).count())
}

/// Client mods for `loader` matching `query` that have a version for `game_version`, most relevant first.
pub fn search(query: &str, game_version: &str, loader: &Loader) -> Result<Vec<Hit>> {
    Api::new()?.search(query, game_version, loader)
}

/// One page of projects for `search`, starting at result `offset`.
/// Every page that arrives is kept on disk; without a connection the kept page comes back with [`Page::saved`].
pub fn search_for(search: &Search, offset: usize) -> Result<Page> {
    search_page_in(&cache_dir(), &Api::new()?.agent, online::offline(), &search_url_for(search, offset))
}

/// A project's page, and when it was fetched if it comes from the disk because there is no connection.
pub fn details(project_id: &str) -> Result<(Details, Option<SystemTime>)> {
    cached_in(&cache_dir(), &Api::new()?.agent, online::offline(), &format!("{API}/project/{}", encode(project_id)))
}

/// Every version of a project, newest first, and when they were fetched if they come from the disk.
pub fn versions(project_id: &str) -> Result<(Vec<Version>, Option<SystemTime>)> {
    cached_in(&cache_dir(), &Api::new()?.agent, online::offline(), &format!("{API}/project/{}/version", encode(project_id)))
}

/// The titles of projects by id; ids Modrinth does not know are absent.
pub fn titles(ids: &[String]) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let agent = Api::new()?.agent;
    for chunk in ids.chunks(PROJECT_CHUNK) {
        let url = format!("{API}/projects?ids={}", encode(&serde_json::json!(chunk).to_string()));
        let (projects, _): (Vec<Project>, _) = cached_in(&cache_dir(), &agent, online::offline(), &url)?;
        out.extend(projects.into_iter().map(|p| (p.id, p.title)));
    }
    Ok(out)
}

/// The newest file of a project that `filter` takes, as a reference with its hash.
pub fn file_for(project_id: &str, title: &str, filter: &Filter) -> Result<ModRef> {
    let api = Api::new()?;
    let versions: Vec<Version> = net::get_json(&api.agent, &versions_url_for(project_id, filter))?;
    let version = newest_for(&versions, filter).ok_or_else(|| {
        Unavailable(match &filter.game_version {
            Some(game) => format!("{title} has no version for {game}"),
            None => format!("{title} has no version"),
        })
    })?;
    mod_ref(title, version)
}

/// Newer files for `current`, files of projects `filter` describes; those without a newer one are left out.
pub fn updates_for(filter: &Filter, current: &[ModRef]) -> Result<Vec<Update>> {
    let api = Api::new()?;
    let check = |m: &ModRef| -> Result<Option<Update>> {
        let versions: Vec<Version> = net::get_json(&api.agent, &versions_url_for(&m.project_id, filter))?;
        Ok(newer(&versions, filter, m)?.map(|latest| Update { current: m.clone(), latest, changelog: Vec::new() }))
    };
    let found = pool()?.install(|| current.par_iter().map(check).collect::<Result<Vec<_>>>())?;
    Ok(found.into_iter().flatten().collect())
}

fn newer(versions: &[Version], filter: &Filter, current: &ModRef) -> Result<Option<ModRef>> {
    let Some(latest) = newest_for(versions, filter) else { return Ok(None) };
    if latest.id == current.version_id {
        return Ok(None);
    }
    let published = versions.iter().find(|v| v.id == current.version_id).map(|v| v.date_published.as_str());
    if published.is_some_and(|date| latest.date_published.as_str() <= date) {
        return Ok(None);
    }
    mod_ref(&current.title, latest).map(Some)
}

fn cache_dir() -> std::path::PathBuf {
    paths::data_dir().join("cache").join("modrinth")
}

fn search_page_in(cache: &Path, agent: &ureq::Agent, offline: bool, url: &str) -> Result<Page> {
    let (response, saved): (SearchResponse, _) = cached_in(cache, agent, offline, url)?;
    Ok(Page { hits: response.hits, total: response.total_hits, saved })
}

fn cached_in<T: serde::de::DeserializeOwned>(cache: &Path, agent: &ureq::Agent, offline: bool, url: &str) -> Result<(T, Option<SystemTime>)> {
    let path = cache.join(format!("{}.json", sha1_hex(url.as_bytes())));
    let saved = || -> Option<(T, Option<SystemTime>)> {
        let at = path.metadata().and_then(|m| m.modified()).ok()?;
        let value: T = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        Some((value, Some(at)))
    };
    if offline && let Some(kept) = saved() {
        return Ok(kept);
    }
    let bytes = match net::get_bytes(agent, url) {
        Ok(bytes) => bytes,
        Err(e) if net::no_connection(&e) => return saved().ok_or(e),
        Err(e) => return Err(e),
    };
    let value: T = serde_json::from_slice(&bytes).with_context(|| format!("unexpected response from {url}"))?;
    if net::write_atomic(&path, &bytes).is_ok() {
        prune(cache, KEPT_PAGES);
    }
    Ok((value, None))
}

fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn prune(dir: &Path, kept: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, std::path::PathBuf)> = entries.filter_map(Result::ok).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if files.len() <= kept {
        return;
    }
    files.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (_, path) in files.into_iter().skip(kept) {
        let _ = std::fs::remove_file(path);
    }
}

/// The newest version of a mod (slug or project id) for `game_version` and `loader`, releases before betas before alphas.
/// Versions whose `fabric.mod.json` or `quilt.mod.json` asks for another loader, game or Java version are passed over.
pub fn resolve(slug_or_id: &str, game_version: &str, loader: &Loader) -> Result<ModRef> {
    let api = Api::new()?;
    Ok(lookup_for(&api, slug_or_id, game_version, loader, &api.target(game_version, loader))?.found)
}

/// The version `version_id` of a mod, whatever newer ones exist, with its required dependencies resolved like
/// [`resolve_with_dependencies`]; the mod itself comes first. Two incompatible mods are an error naming both.
pub fn resolve_version(version_id: &str, game_version: &str, loader: &Loader) -> Result<Resolved> {
    resolve_version_from(&Api::new()?, version_id, game_version, loader)
}

/// Resolves every mod in `list` and, recursively, its required dependencies, each project once.
/// Listed mods come first. Two incompatible mods are an error naming both; a required dependency without a build for
/// `game_version` and the loader is left out and named in `skipped`.
pub fn resolve_with_dependencies<S: AsRef<str>>(list: &[S], game_version: &str, loader: &Loader) -> Result<Resolved> {
    let graph = resolve_all(&Api::new()?, list, game_version, loader)?;
    Ok(Resolved { mods: graph.entries.into_iter().map(|e| e.found).collect(), skipped: graph.skipped })
}

/// The performance preset for every mod loader, for Vanilla none, each mod with its dependencies: Debugify, Sodium,
/// Lithium, EntityCulling, FastNoise, FerriteCore, Gnetum, ImmediatelyFast, Ixeris, ModernFix, Noxesium, Optimized
/// Block Entities, Sodium Extra, Very Many Players and, on Windows only, Windows Priority Boost.
/// A default without a version for `game_version` and the loader, or one that clashes with an earlier default, is
/// skipped: `notes` explains it in one sentence and `skipped` names it. The preset never fails for that.
pub fn default_mods(game_version: &str, loader: &Loader) -> Result<Defaults> {
    if preset(loader, cfg!(windows)).is_empty() {
        return Ok(Defaults { mods: Vec::new(), notes: Vec::new(), skipped: Vec::new() });
    }
    defaults_from(&Api::new()?, game_version, loader)
}

fn needs_fabric_api(instance: &Instance) -> bool {
    if !matches!(instance.loader, Loader::Fabric(_) | Loader::Quilt(_)) {
        return false;
    }
    let has_local = instance.local_mods.iter().any(|name| name.to_lowercase().starts_with("fabric-api"));
    !has_local && !instance.mods.iter().any(|m| m.project_id == FABRIC_API)
}

/// Makes the instance's mod folder hold exactly its mods: downloads missing or changed files (SHA-1 checked),
/// then deletes `.jar` files directly in the folder that are neither in the list nor in `local_mods`. Returns how many were downloaded.
/// On Fabric and Quilt, Fabric API is added for the sync when the instance does not list it, since most of their mods need it.
pub fn sync_mods(instance: &Instance) -> Result<usize> {
    with_mods_lock(&instance.dir(), || sync_locked(&saved(instance), &|_, _| {}))
}

fn not_searching(instance_dir: &Path) -> Result<()> {
    if super::content::searching(instance_dir) {
        bail!(tr("Beende zuerst die Fehlersuche."));
    }
    Ok(())
}

pub(super) fn with_mods_lock<T>(instance_dir: &Path, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let _held = super::lock::exclusive(&instance_dir.join("mods.lock"))?;
    work()
}

fn saved(instance: &Instance) -> Instance {
    saved_in(&instance.dir(), instance)
}

fn saved_in(dir: &Path, instance: &Instance) -> Instance {
    std::fs::read(dir.join("instance.json")).ok().and_then(|bytes| serde_json::from_slice::<Instance>(&bytes).ok()).filter(|b| b.id == instance.id).unwrap_or_else(|| instance.clone())
}

fn sync_locked(instance: &Instance, progress: Progress) -> Result<usize> {
    let mut mods = instance.mods.clone();
    let mut local = instance.local_mods.clone();
    if needs_fabric_api(instance) {
        let present = fabric_api_on_disk(&instance.mods_dir());
        match present {
            Some(name) if super::online::offline() => local.push(name),
            _ => match resolve(FABRIC_API, &instance.game_version, &instance.loader) {
                Ok(found) => mods.push(found),
                Err(e) if net::no_connection(&e) && present.is_some() => local.extend(present),
                Err(e) if is_unavailable(&e) => eprintln!("{e:#}"),
                Err(e) => return Err(e.context("Fabric API could not be found")),
            },
        }
    }
    sync_from(&net::agent(None)?, Some(&jar_cache()), &mods, &local, &instance.disabled, &instance.mods_dir(), &instance.dir().join(LEDGER), progress)
}

fn fabric_api_on_disk(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir).ok()?.filter_map(Result::ok).filter_map(|e| e.file_name().to_str().map(str::to_owned)).find(|name| {
        let lower = name.to_lowercase();
        lower.starts_with("fabric-api") && lower.ends_with(".jar")
    })
}

/// Mods of the instance that have a newer file for its game version.
pub fn updates(instance: &Instance) -> Result<Vec<Update>> {
    updates_from(&Api::new()?, &instance.game_version, &instance.loader, &instance.mods)
}

/// Looks up files by SHA-1 (hex, any case) on Modrinth. The result maps each lowercase hash that is on
/// Modrinth to a `ModRef` of exactly that file; hashes Modrinth does not know are absent.
pub fn match_hashes(hashes: &[String]) -> Result<HashMap<String, ModRef>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    matched_from(&Api::new()?, hashes)
}

/// Applies `selection` (from `updates`): replaces those mods in `instance`, makes the mod folder match
/// (local jars stay) and saves the instance. Returns how many files were downloaded.
/// On any error `instance` and its saved file are unchanged. Call it only while no game of the instance
/// runs: running games hold the old jars open, so the sync could not remove them.
pub fn update_mods(instance: &mut Instance, selection: &[Update], progress: Progress) -> Result<usize> {
    update_with(instance, selection, |next| {
        with_mods_lock(&next.dir(), || {
            not_searching(&next.dir())?;
            let fetched = sync_locked(next, progress)?;
            super::instance::save(next)?;
            Ok(fetched)
        })
    })
}

/// applies `selection` to the instance's list and saves it without touching the mod folder, for an instance whose
/// games run. The files change at the next start, when the mod folder is synced.
pub fn update_mods_later(instance: &mut Instance, selection: &[Update]) -> Result<()> {
    update_with(instance, selection, |next| {
        not_searching(&next.dir())?;
        super::instance::save(next)?;
        Ok(0)
    })
    .map(|_| ())
}

/// `manycraft mods`: searches and resolves mods from the terminal.
pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn parse(args: &[String]) -> Option<(Vec<String>, Option<String>)> {
    let mut words = Vec::new();
    let mut version = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--version" {
            version = Some(it.next()?.clone());
        } else if arg.starts_with("--") {
            return None;
        } else {
            words.push(arg.clone());
        }
    }
    Some((words, version))
}

fn run(args: &[String]) -> Result<i32> {
    let parsed = args.split_first().and_then(|(cmd, rest)| parse(rest).map(|(words, version)| (cmd.as_str(), words, version)));
    let Some((cmd, words, version)) = parsed.filter(|(cmd, words, _)| matches!(*cmd, "search" | "resolve") && !words.is_empty()) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let version = match version {
        Some(v) => v,
        None => install::latest_release()?,
    };
    if cmd == "search" {
        let loader = Loader::default();
        let hits = search(&words.join(" "), &version, &loader)?;
        if hits.is_empty() {
            println!("no {} mods for {version} match", loader.name());
        }
        for hit in hits {
            println!("{} ({})  {} downloads\n  {}", hit.title, hit.slug, hit.downloads, hit.description);
        }
    } else {
        let Graph { entries, requested, skipped } = resolve_all(&Api::new()?, &words, &version, &Loader::default())?;
        for note in skipped {
            println!("{note}");
        }
        for (i, entry) in entries.iter().enumerate() {
            let m = &entry.found;
            let marker = if i < requested { "" } else { "  + " };
            println!("{marker}{}  {}  {}  sha1 {}", m.title, m.version_id, m.file_name, m.sha1);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_search_page_is_kept_and_comes_back_with_its_age_without_a_connection() {
        use std::io::{Read, Write};
        use std::time::Duration;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || {
            let body = r#"{"hits":[{"project_id":"AANobbMI","slug":"sodium","title":"Sodium","description":"fast","downloads":5,"versions":["26.3"],"categories":["fabric"]}],"total_hits":1}"#;
            let mut stream = server.incoming().next().unwrap().unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(format!("HTTP/1.1 200 OK
Content-Length: {}
Connection: close

{body}", body.len()).as_bytes());
        });
        let cache = tempfile::tempdir().unwrap();
        let url = "http://modrinth.test/v2/search?query=sodium";
        let (none, lookups) = super::net::fake::agent(None, Duration::from_secs(5));
        assert!(super::net::no_connection(&super::search_page_in(cache.path(), &none, false, url).err().unwrap()));
        let (up, _) = super::net::fake::agent(Some(addr), Duration::from_secs(5));
        let fresh = super::search_page_in(cache.path(), &up, false, url).unwrap();
        assert_eq!((fresh.hits.len(), fresh.total, fresh.saved), (1, 1, None));
        let kept = super::search_page_in(cache.path(), &none, false, url).unwrap();
        assert_eq!((kept.hits, kept.total), (fresh.hits.clone(), 1));
        assert!(kept.saved.is_some_and(|at| at.elapsed().unwrap_or_default() < Duration::from_secs(60)));
        assert_eq!(lookups.count(), 2);
        assert!(super::search_page_in(cache.path(), &none, true, url).unwrap().saved.is_some());
        assert_eq!(lookups.count(), 2);
        assert!(super::search_page_in(cache.path(), &none, true, "http://modrinth.test/v2/search?query=other").is_err());
    }

    #[test]
    fn pruning_keeps_the_newest_files() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            let path = dir.path().join(format!("{i}.json"));
            std::fs::write(&path, "{}").unwrap();
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i)).unwrap();
        }
        super::prune(dir.path(), 2);
        let mut left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["3.json", "4.json"]);
    }

    #[test]
    fn mod_folder_syncs_of_one_instance_run_one_after_another() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let inside = AtomicUsize::new(0);
        let overlap = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    with_mods_lock(dir.path(), || {
                        if inside.fetch_add(1, Ordering::SeqCst) > 0 {
                            overlap.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(40));
                        inside.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(overlap.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_sync_after_an_update_uses_the_saved_list_not_a_stale_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut stale = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        stale.local_mods = vec!["old.jar".into()];
        assert_eq!(saved_in(dir.path(), &stale).local_mods, ["old.jar"]);
        let mut current = stale.clone();
        current.local_mods = vec!["new.jar".into()];
        std::fs::write(dir.path().join("instance.json"), serde_json::to_vec(&current).unwrap()).unwrap();
        assert_eq!(saved_in(dir.path(), &stale).local_mods, ["new.jar"]);
        let mut other = current.clone();
        other.id = "else".into();
        std::fs::write(dir.path().join("instance.json"), serde_json::to_vec(&other).unwrap()).unwrap();
        assert_eq!(saved_in(dir.path(), &stale).local_mods, ["old.jar"]);
    }

    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("testdata/modrinth/", $name, ".json"))
        };
    }

    #[derive(Default)]
    struct Fixtures {
        projects: Vec<(String, String, Project)>,
        versions: Vec<Version>,
        target: Target,
        jars: HashMap<String, Vec<u8>>,
        opened: std::sync::Mutex<Vec<String>>,
    }

    impl Fixtures {
        fn recorded() -> Self {
            let mut fx = Self::default();
            for (slug, project, versions) in [
                ("lithium", fixture!("project_lithium"), fixture!("versions_lithium")),
                ("sodium", fixture!("project_sodium"), fixture!("versions_sodium")),
                ("fabric-api", fixture!("project_fabric-api"), fixture!("versions_fabric-api")),
                ("ferrite-core", fixture!("project_ferrite-core"), fixture!("versions_ferrite-core")),
                ("krypton", fixture!("project_krypton"), fixture!("versions_krypton")),
                ("modernfix", fixture!("project_modernfix"), fixture!("versions_modernfix")),
                ("sodium-extra", fixture!("project_sodium-extra"), fixture!("versions_sodium-extra")),
                ("modmenu", fixture!("project_modmenu"), fixture!("versions_modmenu")),
                ("iris", fixture!("project_iris"), fixture!("versions_iris")),
                ("placeholder-api", fixture!("project_placeholder-api"), fixture!("versions_placeholder-api")),
                ("embeddium", fixture!("project_embeddium"), fixture!("versions_embeddium")),
            ] {
                fx.add(slug, project, versions);
            }
            fx
        }

        fn add(&mut self, slug: &str, project: &str, versions: &str) {
            let project: Project = serde_json::from_str(project).unwrap();
            let versions: Vec<Version> = serde_json::from_str(versions).unwrap();
            self.projects.push((slug.to_owned(), project.id.clone(), project));
            self.versions.extend(versions);
        }

        fn synthetic(mods: &[(&str, &[(&str, &str)])]) -> Self {
            let mut fx = Self::default();
            for (slug, deps) in mods {
                let deps: Vec<String> = deps.iter().map(|(id, kind)| format!(r#"{{"project_id":"{id}","version_id":null,"dependency_type":"{kind}"}}"#)).collect();
                let project = format!(r#"{{"id":"id-{slug}","slug":"{slug}","title":"T-{slug}"}}"#);
                let version = format!(
                    r#"[{{"id":"v-{slug}","project_id":"id-{slug}","version_number":"1","version_type":"release","date_published":"2026-01-01T00:00:00Z","game_versions":["26.3"],"loaders":["fabric"],"files":[{{"url":"https://x/{slug}.jar","filename":"{slug}.jar","primary":true,"hashes":{{"sha1":"00"}}}}],"dependencies":[{}]}}]"#,
                    deps.join(",")
                );
                fx.add(slug, &project, &version);
            }
            fx
        }
    }

    impl Source for Fixtures {
        fn search(&self, _query: &str, _game_version: &str, _loader: &Loader) -> Result<Vec<Hit>> {
            Ok(serde_json::from_str::<SearchResponse>(fixture!("search_sodium"))?.hits)
        }

        fn project(&self, key: &str) -> Result<Option<Project>> {
            let found = self.projects.iter().find(|(slug, id, _)| slug.eq_ignore_ascii_case(key) || id == key);
            Ok(found.map(|(_, id, p)| Project { id: id.clone(), title: p.title.clone() }))
        }

        fn versions(&self, project_id: &str, _game_version: &str, _loader: &Loader) -> Result<Vec<Version>> {
            Ok(self.versions.iter().filter(|v| v.project_id == project_id).cloned().collect())
        }

        fn version(&self, id: &str) -> Result<Version> {
            self.versions.iter().find(|v| v.id == id).cloned().context("unknown version")
        }

        fn versions_by_hash(&self, hashes: &[String]) -> Result<HashMap<String, Version>> {
            assert!(hashes.len() <= HASH_CHUNK);
            let mut found = HashMap::new();
            for hash in hashes {
                if let Some(v) = self.versions.iter().find(|v| v.files.iter().any(|f| f.hashes.sha1 == *hash)) {
                    found.insert(hash.clone(), v.clone());
                }
            }
            Ok(found)
        }

        fn projects(&self, ids: &[String]) -> Result<Vec<Project>> {
            assert!(ids.len() <= PROJECT_CHUNK);
            Ok(self.projects.iter().filter(|(_, id, _)| ids.contains(id)).map(|(_, id, p)| Project { id: id.clone(), title: p.title.clone() }).collect())
        }

        fn target(&self, _game_version: &str, _loader: &Loader) -> Target {
            self.target.clone()
        }

        fn needs(&self, found: &ModRef, quilt: bool) -> Result<Needs> {
            self.opened.lock().unwrap().push(found.version_id.clone());
            match self.jars.get(&found.sha1) {
                Some(jar) => Needs::read(std::io::Cursor::new(jar.clone()), quilt),
                None => Ok(Needs::default()),
            }
        }
    }

    fn fabric_jar(depends: &str) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(format!(r#"{{"schemaVersion":1,"id":"m","version":"1","depends":{depends}}}"#).as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    fn snapshot_api(loaders: &[&str]) -> Fixtures {
        let mut fx = Fixtures::default();
        let project = r#"{"id":"P7dR8mSH","slug":"fabric-api","title":"Fabric API"}"#;
        let mut versions = Vec::new();
        for (i, loader) in loaders.iter().enumerate() {
            let sha1 = format!("{i:040}");
            versions.push(format!(
                r#"{{"id":"v{i}","project_id":"P7dR8mSH","version_number":"0.16{i}","version_type":"release","date_published":"2026-11-0{}T00:00:00Z","game_versions":["26.4-snapshot-7"],"loaders":["fabric"],"files":[{{"url":"https://x/{i}.jar","filename":"api-{i}.jar","primary":true,"hashes":{{"sha1":"{sha1}"}}}}]}}"#,
                9 - i
            ));
            fx.jars.insert(sha1, fabric_jar(&format!(r#"{{"fabricloader":"{loader}","minecraft":"~26.4-","java":">=25"}}"#)));
        }
        fx.add("fabric-api", project, &format!("[{}]", versions.join(",")));
        fx.target = Target { minecraft: Some("26.4-snapshot-7".into()), java: Some(25), fabricloader: Some("0.19.5".into()), quilt_loader: Some("0.30.2".into()), quilt: true };
        fx
    }

    const SNAPSHOT: &str = "26.4-snapshot-7";

    #[test]
    fn quilt_gets_the_newest_fabric_api_whose_loader_range_it_meets() {
        let fx = snapshot_api(&[">=0.20.0", ">=0.20.0", ">=0.16 <0.20", ">=0.15"]);
        let found = lookup(&fx, "fabric-api", SNAPSHOT, &Loader::Quilt(None)).unwrap().found;
        assert_eq!(found.version_id, "v2");
        assert_eq!(*fx.opened.lock().unwrap(), ["v0", "v1", "v2"]);
    }

    #[test]
    fn fabric_with_a_new_enough_loader_keeps_the_newest() {
        let mut fx = snapshot_api(&[">=0.20.0", ">=0.16"]);
        fx.target = Target { minecraft: Some(SNAPSHOT.into()), java: Some(25), fabricloader: Some("0.20.1".into()), ..Target::default() };
        assert_eq!(lookup(&fx, "fabric-api", SNAPSHOT, &fabric()).unwrap().found.version_id, "v0");
    }

    #[test]
    fn no_fitting_version_skips_the_mod_with_a_note_that_names_the_loader() {
        let fx = snapshot_api(&[">=0.20.0", ">=0.20.0", ">=0.20.0"]);
        let err = lookup(&fx, "fabric-api", SNAPSHOT, &Loader::Quilt(None)).unwrap_err();
        assert!(is_unavailable(&err));
        assert_eq!(err.to_string(), "„Fabric API“ läuft nicht mit Quilt 0.30.2.");
        let mut fx = snapshot_api(&[">=0.20.0"]);
        fx.add("lithium", r#"{"id":"id-l","slug":"lithium","title":"Lithium"}"#, r#"[{"id":"vl","project_id":"id-l","version_type":"release","date_published":"2026-11-01T00:00:00Z","game_versions":["26.4-snapshot-7"],"loaders":["fabric"],"files":[{"url":"https://x/l.jar","filename":"l.jar","primary":true,"hashes":{"sha1":"11"}}],"dependencies":[{"project_id":"P7dR8mSH","dependency_type":"required"}]}]"#);
        let lithium = Slot { name: "Lithium", ids: &["lithium"], windows_only: false };
        let defaults = defaults_with(&fx, SNAPSHOT, &Loader::Quilt(None), &[&lithium]).unwrap();
        assert_eq!(titles(&defaults.mods), ["Lithium"]);
        assert!(defaults.skipped.is_empty(), "{:?}", defaults.skipped);
        assert!(defaults.notes.contains(&"„Fabric API“ läuft nicht mit Quilt 0.30.2. „Lithium“ wird trotzdem installiert.".to_owned()), "{:?}", defaults.notes);
    }

    #[test]
    fn only_the_newest_eight_are_opened_before_giving_up() {
        let fx = snapshot_api(&[">=0.20.0"; 9]);
        assert!(lookup(&fx, "fabric-api", SNAPSHOT, &Loader::Quilt(None)).is_err());
        assert_eq!(fx.opened.lock().unwrap().len(), CHECKED);
    }

    #[test]
    fn game_and_java_ranges_are_checked_and_forge_opens_no_jar() {
        let mut fx = snapshot_api(&[">=0.15"]);
        fx.target.minecraft = Some("26.5-snapshot-1".into());
        assert_eq!(lookup(&fx, "fabric-api", SNAPSHOT, &Loader::Quilt(None)).unwrap_err().to_string(), "„Fabric API“ läuft nicht mit Minecraft 26.4-snapshot-7.");
        fx.target.minecraft = Some(SNAPSHOT.into());
        fx.target.java = Some(21);
        assert_eq!(lookup(&fx, "fabric-api", SNAPSHOT, &Loader::Quilt(None)).unwrap_err().to_string(), "„Fabric API“ läuft nicht mit Java 21.");
        fx.opened.lock().unwrap().clear();
        for v in &mut fx.versions {
            v.loaders = vec!["forge".into()];
        }
        assert_eq!(lookup(&fx, "fabric-api", SNAPSHOT, &Loader::Forge(None)).unwrap().found.version_id, "v0");
        assert!(fx.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn an_update_skips_a_newer_version_the_loader_cannot_run_and_never_opens_older_ones() {
        let fx = snapshot_api(&[">=0.20.0", ">=0.16", ">=0.16"]);
        let current = mod_ref("Fabric API", &fx.versions[2]).unwrap();
        let updates = updates_from(&fx, SNAPSHOT, &Loader::Quilt(None), std::slice::from_ref(&current)).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].latest.version_id, "v1");
        fx.opened.lock().unwrap().clear();
        let newest_fit = mod_ref("Fabric API", &fx.versions[1]).unwrap();
        assert!(updates_from(&fx, SNAPSHOT, &Loader::Quilt(None), &[newest_fit]).unwrap().is_empty());
        assert_eq!(*fx.opened.lock().unwrap(), ["v0"]);
    }

    #[test]
    fn a_cached_jar_is_read_and_kept_fresh_and_only_a_lost_connection_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let jar = fabric_jar(r#"{"fabricloader":">=0.20.0"}"#);
        let m = ModRef { project_id: "p".into(), version_id: "v".into(), title: "T".into(), file_name: "t.jar".into(), url: "http://modrinth.test/t.jar".into(), sha1: sha1_hex(&jar) };
        let path = cached_jar(dir.path(), &m.sha1).unwrap();
        std::fs::write(&path, &jar).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(SystemTime::UNIX_EPOCH).unwrap();
        let (none, _) = super::net::fake::agent(None, std::time::Duration::from_secs(1));
        let old = Target { fabricloader: Some("0.19.5".into()), ..Target::default() };
        assert_eq!(needs_in(&none, dir.path(), &m, true).unwrap().check(&old), Err(Miss::Loader));
        assert!(path.metadata().unwrap().modified().unwrap() > SystemTime::UNIX_EPOCH);
        std::fs::remove_file(&path).unwrap();
        assert!(super::net::no_connection(&needs_in(&none, dir.path(), &m, true).unwrap_err()));
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in server.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = [0; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found
Content-Length: 0
Connection: close

");
            }
        });
        let (up, _) = super::net::fake::agent(Some(addr), std::time::Duration::from_secs(5));
        assert_eq!(needs_in(&up, dir.path(), &m, true).unwrap(), Needs::default());
    }

    #[test]
    fn a_cached_jar_is_copied_instead_of_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let (jars, mods) = (dir.path().join("jars"), dir.path().join("mods"));
        std::fs::create_dir_all(&jars).unwrap();
        let bytes = b"cached jar";
        let sha1 = sha1_hex(bytes);
        std::fs::write(cached_jar(&jars, &sha1).unwrap(), bytes).unwrap();
        let m = ModRef { project_id: "p".into(), version_id: "v".into(), title: "T".into(), file_name: "t.jar".into(), url: "http://127.0.0.1:9/t.jar".into(), sha1 };
        let (agent, lookups) = super::net::fake::agent(None, std::time::Duration::from_secs(1));
        assert!(place(&agent, Some(&jars), &m, false, &mods, &HashMap::new()).unwrap());
        assert_eq!(std::fs::read(mods.join("t.jar")).unwrap(), bytes);
        assert_eq!(lookups.count(), 0);
        assert!(cached_jar(&jars, "../../evil").is_none());
    }

    fn fabric() -> Loader {
        Loader::Fabric(None)
    }

    const FABRIC_TITLES: [&str; 4] = ["Sodium", "Lithium", "FerriteCore", "Sodium Extra"];

    fn titles(mods: &[ModRef]) -> Vec<&str> {
        mods.iter().map(|m| m.title.as_str()).collect()
    }

    fn resolved<S: AsRef<str>>(fx: &Fixtures, list: &[S]) -> Result<Vec<ModRef>> {
        Ok(resolve_all(fx, list, "26.3", &fabric())?.entries.into_iter().map(|e| e.found).collect())
    }

    #[test]
    fn release_is_preferred_over_newer_betas() {
        let fx = Fixtures::recorded();
        let sodium = lookup(&fx, "sodium", "26.3", &fabric()).unwrap().found;
        assert_eq!(sodium.version_id, "bAZQdGpg");
        assert_eq!(sodium.title, "Sodium");
        assert!(sodium.url.starts_with("https://cdn.modrinth.com/") && sodium.file_name.ends_with(".jar"));
        assert_eq!(sodium.sha1.len(), 40);
    }

    #[test]
    fn loader_and_game_version_are_filtered() {
        let fx = Fixtures::recorded();
        assert_eq!(lookup(&fx, "lithium", "26.3", &fabric()).unwrap().found.version_id, "fWv5bnZk");
        let versions = fx.versions("gvQqBUqZ", "26.3", &fabric()).unwrap();
        assert_eq!(versions.len(), 6);
        assert!(newest(&versions, "1.21", &fabric()).is_none());
        assert!(newest(&versions[..1], "26.3", &fabric()).is_none());
        let err = lookup(&fx, "lithium", "1.21", &fabric()).unwrap_err();
        assert!(is_unavailable(&err) && err.to_string().contains("Lithium"), "{err:#}");
    }

    #[test]
    fn quilt_takes_fabric_versions_and_other_loaders_are_not_mixed() {
        let fx = Fixtures::synthetic(&[("a", &[])]);
        assert_eq!(lookup(&fx, "a", "26.3", &Loader::Quilt(None)).unwrap().found.version_id, "v-a");
        assert!(newest(&fx.versions, "26.3", &Loader::Forge(None)).is_none());
        assert!(newest(&fx.versions, "26.3", &Loader::NeoForge(None)).is_none());
        assert!(newest(&fx.versions, "26.3", &Loader::Vanilla).is_none());
        let err = lookup(&fx, "a", "26.3", &Loader::Forge(None)).unwrap_err();
        assert!(is_unavailable(&err) && err.to_string().contains("Forge"), "{err:#}");
    }

    #[test]
    fn the_preset_is_the_fifteen_mods_and_windows_boost_only_runs_on_windows() {
        let names = |windows| preset(&fabric(), windows).iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names(true).len(), 15);
        assert_eq!(names(true)[14], "Windows Priority Boost");
        assert_eq!(names(false).len(), 14);
        assert!(!names(false).contains(&"Windows Priority Boost"));
        assert!(preset(&Loader::Vanilla, true).is_empty());
        for loader in [Loader::Quilt(None), Loader::Forge(None), Loader::NeoForge(None)] {
            assert_eq!(preset(&loader, true).len(), 15);
        }
        let ids: HashSet<&str> = PRESET.iter().flat_map(|s| s.ids.iter().copied()).collect();
        assert_eq!(ids.len(), 16);
    }

    #[test]
    fn each_loader_gets_its_own_files_of_the_preset() {
        let fx = Fixtures::recorded();
        let quilt = defaults_from(&fx, "26.3", &Loader::Quilt(None)).unwrap();
        assert_eq!(titles(&quilt.mods), FABRIC_TITLES);
        let neoforge = defaults_from(&fx, "26.3", &Loader::NeoForge(None)).unwrap();
        assert!(neoforge.mods.iter().all(|m| !m.file_name.contains("fabric")), "{:?}", neoforge.mods);
        assert_eq!(titles(&neoforge.mods), FABRIC_TITLES);
        let files = |d: &Defaults| d.mods.iter().map(|m| m.file_name.clone()).collect::<Vec<_>>();
        assert_ne!(files(&neoforge), files(&quilt));
        let vanilla = default_mods("26.3", &Loader::Vanilla).unwrap();
        assert!(vanilla.mods.is_empty() && vanilla.notes.is_empty() && vanilla.skipped.is_empty());
    }

    #[test]
    fn betas_beat_alphas_and_alpha_is_a_last_resort() {
        let versions = |kinds: &[(&str, &str)]| -> Vec<Version> {
            let items: Vec<String> = kinds
                .iter()
                .enumerate()
                .map(|(i, (id, kind))| {
                    format!(
                        r#"{{"id":"{id}","project_id":"p","version_type":"{kind}","date_published":"2026-01-0{}T00:00:00Z","game_versions":["26.3"],"loaders":["fabric"],"files":[],"dependencies":[]}}"#,
                        i + 1
                    )
                })
                .collect();
            serde_json::from_str(&format!("[{}]", items.join(","))).unwrap()
        };
        let mixed = versions(&[("a", "beta"), ("b", "alpha")]);
        assert_eq!(newest(&mixed, "26.3", &fabric()).unwrap().id, "a");
        let alphas = versions(&[("a", "alpha"), ("b", "alpha")]);
        assert_eq!(newest(&alphas, "26.3", &fabric()).unwrap().id, "b");
    }

    #[test]
    fn missing_primary_file_falls_back_to_the_first() {
        let fx = Fixtures::recorded();
        let mut v = fx.version("fWv5bnZk").unwrap();
        v.files[0].primary = false;
        assert_eq!(mod_ref("Lithium", &v).unwrap().file_name, v.files[0].filename);
        v.files.clear();
        assert!(mod_ref("Lithium", &v).is_err());
    }

    #[test]
    fn required_dependencies_are_added_recursively() {
        let fx = Fixtures::recorded();
        let Graph { entries, requested, .. } = resolve_all(&fx, &["modmenu"], "26.3", &fabric()).unwrap();
        let mods: Vec<ModRef> = entries.into_iter().map(|e| e.found).collect();
        assert_eq!(requested, 1);
        assert_eq!(titles(&mods)[0], "Mod Menu");
        let mut rest = titles(&mods)[1..].to_vec();
        rest.sort_unstable();
        assert_eq!(rest, ["Fabric API", "Text Placeholder API"]);
        let iris = resolved(&fx, &["iris"]).unwrap();
        assert_eq!(titles(&iris), ["Iris Shaders", "Sodium"]);
    }

    #[test]
    fn optional_dependencies_are_not_added() {
        let fx = Fixtures::synthetic(&[("a", &[("id-b", "optional"), ("id-c", "embedded")]), ("b", &[]), ("c", &[])]);
        assert_eq!(titles(&resolved(&fx, &["a"]).unwrap()), ["T-a"]);
    }

    #[test]
    fn projects_are_deduplicated_by_id() {
        let fx = Fixtures::recorded();
        let mods = resolved(&fx, &["modmenu", "fabric-api", "P7dR8mSH", "sodium-extra", "iris"]).unwrap();
        assert_eq!(mods.len(), 6);
        let ids: HashSet<&str> = mods.iter().map(|m| m.project_id.as_str()).collect();
        assert_eq!(ids.len(), 6);
        assert_eq!(&titles(&mods)[..4], ["Mod Menu", "Fabric API", "Sodium Extra", "Iris Shaders"]);
    }

    #[test]
    fn dependency_cycles_terminate() {
        let fx = Fixtures::synthetic(&[("a", &[("id-b", "required")]), ("b", &[("id-c", "required")]), ("c", &[("id-a", "required")])]);
        assert_eq!(titles(&resolved(&fx, &["a"]).unwrap()), ["T-a", "T-b", "T-c"]);
        assert_eq!(titles(&resolved(&fx, &["b", "a"]).unwrap()), ["T-b", "T-a", "T-c"]);
    }

    #[test]
    fn incompatible_mods_are_an_error_naming_both() {
        let fx = Fixtures::synthetic(&[("a", &[("id-b", "incompatible")]), ("b", &[]), ("c", &[("id-b", "required")])]);
        let err = resolved(&fx, &["a", "b"]).unwrap_err().to_string();
        assert!(err.contains("T-a") && err.contains("T-b") && err.contains("incompatible"), "{err}");
        let err = resolved(&fx, &["c", "a"]).unwrap_err().to_string();
        assert!(err.contains("T-a") && err.contains("T-b"), "{err}");
        assert_eq!(titles(&resolved(&fx, &["a"]).unwrap()), ["T-a"]);
    }

    fn with_old_a(deps: &[(&str, &str)]) -> Fixtures {
        let mut fx = Fixtures::synthetic(&[("a", &[("id-c", "required")]), ("b", &[]), ("c", &[])]);
        let mut old = fx.versions[0].clone();
        old.id = "v-a-old".into();
        old.date_published = "2025-01-01T00:00:00Z".into();
        old.dependencies = deps.iter().map(|(id, kind)| Dependency { project_id: Some((*id).into()), version_id: None, dependency_type: (*kind).into() }).collect();
        fx.versions.push(old);
        fx
    }

    #[test]
    fn a_pinned_version_is_kept_even_when_a_newer_one_exists() {
        let fx = with_old_a(&[]);
        let found = resolve_version_from(&fx, "v-a-old", "26.3", &fabric()).unwrap();
        assert_eq!(found.mods.iter().map(|m| m.version_id.as_str()).collect::<Vec<_>>(), ["v-a-old"]);
        assert_eq!(found.mods[0].title, "T-a");
        assert_eq!(resolved(&fx, &["a"]).unwrap()[0].version_id, "v-a");
    }

    #[test]
    fn a_pinned_version_brings_its_own_required_dependencies_after_it() {
        let fx = with_old_a(&[("id-b", "required"), ("id-c", "optional")]);
        let found = resolve_version_from(&fx, "v-a-old", "26.3", &fabric()).unwrap();
        assert_eq!(titles(&found.mods), ["T-a", "T-b"]);
        let newest = resolve_version_from(&fx, "v-a", "26.3", &fabric()).unwrap();
        assert_eq!(titles(&newest.mods), ["T-a", "T-c"]);
    }

    #[test]
    fn a_pinned_version_with_an_incompatible_dependency_is_an_error() {
        let mut fx = with_old_a(&[("id-b", "incompatible"), ("id-c", "required")]);
        fx.versions.iter_mut().find(|v| v.id == "v-c").unwrap().dependencies.push(Dependency { project_id: Some("id-b".into()), version_id: None, dependency_type: "required".into() });
        let err = resolve_version_from(&fx, "v-a-old", "26.3", &fabric()).unwrap_err().to_string();
        assert!(err.contains("T-a") && err.contains("T-b") && err.contains("incompatible"), "{err}");
        assert!(resolve_version_from(&fx, "nope", "26.3", &fabric()).is_err());
    }

    #[test]
    fn a_version_fits_when_the_filter_takes_its_loader_and_game_version() {
        let fx = Fixtures::synthetic(&[("a", &[])]);
        let version = &fx.versions[0];
        assert!(version_fits(version, &Filter::of(Kind::Mod, "26.3", &fabric(), None)));
        assert!(!version_fits(version, &Filter::of(Kind::Mod, "1.21", &fabric(), None)));
        assert!(!version_fits(version, &Filter::of(Kind::Mod, "26.3", &Loader::Forge(None), None)));
        assert!(newest_for(&fx.versions, &Filter::of(Kind::Mod, "26.3", &fabric(), None)).is_some());
        assert!(newest_for(&fx.versions, &Filter::of(Kind::Mod, "1.21", &fabric(), None)).is_none());
    }

    #[test]
    fn required_dependency_without_a_version_is_left_out_and_named() {
        let fx = Fixtures::synthetic(&[("a", &[("id-gone", "required"), ("id-b", "required")]), ("b", &[])]);
        let graph = resolve_all(&fx, &["a"], "26.3", &fabric()).unwrap();
        assert_eq!(graph.entries.iter().map(|e| e.found.title.as_str()).collect::<Vec<_>>(), ["T-a", "T-b"]);
        assert_eq!(graph.skipped.len(), 1, "{:?}", graph.skipped);
        assert!(graph.skipped[0].contains("id-gone") && graph.skipped[0].contains("T-a"), "{:?}", graph.skipped);
        assert!(resolved(&fx, &["nope"]).is_err_and(|e| is_unavailable(&e)));
    }

    #[test]
    fn a_dependency_left_out_is_named_once_however_many_mods_need_it() {
        let fx = Fixtures::synthetic(&[("a", &[("id-gone", "required")]), ("b", &[("id-gone", "required")])]);
        assert_eq!(resolve_all(&fx, &["a", "b"], "26.3", &fabric()).unwrap().skipped.len(), 1);
    }

    #[test]
    fn a_pinned_version_without_one_of_its_dependencies_still_resolves() {
        let fx = with_old_a(&[("id-gone", "required")]);
        let found = resolve_version_from(&fx, "v-a-old", "26.3", &fabric()).unwrap();
        assert_eq!(titles(&found.mods), ["T-a"]);
        assert_eq!(found.skipped.len(), 1);
    }

    #[test]
    fn dependency_known_only_by_version_id_is_followed() {
        let mut fx = Fixtures::synthetic(&[("a", &[]), ("b", &[])]);
        fx.versions[0].dependencies.push(Dependency { project_id: None, version_id: Some("v-b".into()), dependency_type: "required".into() });
        assert_eq!(titles(&resolved(&fx, &["a"]).unwrap()), ["T-a", "T-b"]);
    }

    #[test]
    fn defaults_skip_a_mod_without_a_version() {
        let fx = Fixtures::recorded();
        let defaults = defaults_from(&fx, "26.3", &fabric()).unwrap();
        assert_eq!(titles(&defaults.mods), FABRIC_TITLES);
        assert!(defaults.skipped.contains(&"ModernFix".to_owned()), "{:?}", defaults.skipped);
        let note = defaults.notes.iter().find(|n| n.to_lowercase().contains("modernfix")).unwrap();
        assert!(note.contains("26.3"), "{note}");
        assert_eq!(defaults.notes.len(), defaults.skipped.len());
        assert_eq!(defaults.skipped.len() + 4, default_names(&fabric()).len(), "{:?}", defaults.skipped);
    }

    const ONE: Slot = Slot { name: "One", ids: &["id-a"], windows_only: false };
    const TWO: Slot = Slot { name: "Two", ids: &["id-b"], windows_only: false };
    const EITHER: Slot = Slot { name: "Either", ids: &["id-none", "id-b"], windows_only: false };

    #[test]
    fn a_default_that_clashes_is_left_out_with_a_note_and_never_fails_the_rest() {
        let fx = Fixtures::synthetic(&[("a", &[("id-b", "incompatible")]), ("b", &[])]);
        let defaults = defaults_with(&fx, "26.3", &fabric(), &[&ONE, &TWO]).unwrap();
        assert_eq!(titles(&defaults.mods), ["T-a"]);
        assert_eq!(defaults.skipped, ["Two"]);
        assert_eq!(defaults.notes.len(), 1);
        assert!(defaults.notes[0].contains("T-b") && defaults.notes[0].contains("T-a"), "{:?}", defaults.notes);
    }

    #[test]
    fn a_slot_whose_own_files_clash_is_left_out_and_never_fails_the_rest() {
        let fx = Fixtures::synthetic(&[("a", &[("id-b", "required"), ("id-b", "incompatible")]), ("b", &[])]);
        let defaults = defaults_with(&fx, "26.3", &fabric(), &[&ONE, &TWO]).unwrap();
        assert_eq!(defaults.skipped, ["One"]);
        assert_eq!(titles(&defaults.mods), ["T-b"]);
    }

    #[test]
    fn a_slot_falls_back_to_its_next_id_and_names_a_slot_that_is_not_on_modrinth() {
        let fx = Fixtures::synthetic(&[("b", &[])]);
        let defaults = defaults_with(&fx, "26.3", &fabric(), &[&EITHER, &ONE]).unwrap();
        assert_eq!(titles(&defaults.mods), ["T-b"]);
        assert_eq!(defaults.skipped, ["One"]);
    }

    fn pinned(fx: &Fixtures, id: &str) -> ModRef {
        let v = fx.version(id).unwrap();
        mod_ref("x", &v).unwrap()
    }

    #[test]
    fn updates_offer_only_newer_files() {
        let fx = Fixtures::recorded();
        let mods = [
            pinned(&fx, "xS0Q8LSi"),
            pinned(&fx, "fWv5bnZk"),
            pinned(&fx, "v7Lw0h1m"),
            pinned(&fx, "v4PSXean"),
        ];
        let updates = updates_from(&fx, "26.3", &fabric(), &mods).unwrap();
        let pairs: Vec<(&str, &str)> = updates.iter().map(|u| (u.current.version_id.as_str(), u.latest.version_id.as_str())).collect();
        assert_eq!(pairs, [("xS0Q8LSi", "fWv5bnZk")]);
        assert_eq!(updates[0].latest.title, "x");
        let unknown = ModRef { version_id: "gone".into(), ..pinned(&fx, "fWv5bnZk") };
        assert_eq!(updates_from(&fx, "26.3", &fabric(), &[unknown]).unwrap().len(), 1);
    }

    fn sha_of(fx: &Fixtures, version: &str) -> String {
        fx.version(version).unwrap().files[0].hashes.sha1.clone()
    }

    #[test]
    fn files_are_matched_by_hash_to_the_exact_version() {
        let fx = Fixtures::recorded();
        let old = sha_of(&fx, "xS0Q8LSi");
        let new = sha_of(&fx, "fWv5bnZk");
        let sodium = sha_of(&fx, "bAZQdGpg");
        let upper = old.to_uppercase();
        let matched = matched_from(&fx, &[upper, new.clone(), "0".repeat(40), new.clone(), sodium.clone()]).unwrap();
        assert_eq!(matched.len(), 3);
        let m = &matched[&old];
        assert_eq!((m.version_id.as_str(), m.title.as_str(), m.sha1.as_str()), ("xS0Q8LSi", "Lithium", old.as_str()));
        assert_eq!(matched[&new].version_id, "fWv5bnZk");
        assert_eq!(matched[&sodium].title, "Sodium");
        assert!(m.url.starts_with("https://cdn.modrinth.com/") && m.file_name.ends_with(".jar"));
        assert!(matched_from(&fx, &[]).unwrap().is_empty());
        assert!(matched_from(&fx, &["0".repeat(40)]).unwrap().is_empty());
    }

    #[test]
    fn the_matched_file_is_the_one_with_the_hash_and_unknown_titles_fall_back_to_the_file_name() {
        let mut fx = Fixtures::recorded();
        let id = fx.versions.iter().position(|v| v.id == "fWv5bnZk").unwrap();
        let mut second = fx.versions[id].files[0].clone();
        second.filename = "lithium-extra.jar".into();
        second.hashes.sha1 = "ab".repeat(20);
        second.primary = false;
        fx.versions[id].files[0].primary = true;
        fx.versions[id].files.push(second);
        let matched = matched_from(&fx, &["AB".repeat(20)]).unwrap();
        let m = &matched["ab".repeat(20).as_str()];
        assert_eq!((m.file_name.as_str(), m.sha1.as_str(), m.version_id.as_str()), ("lithium-extra.jar", "ab".repeat(20).as_str(), "fWv5bnZk"));
        fx.projects.clear();
        assert_eq!(matched_from(&fx, &["ab".repeat(20)]).unwrap()["ab".repeat(20).as_str()].title, "lithium-extra.jar");
    }

    #[test]
    fn many_hashes_and_projects_are_asked_in_chunks() {
        let fx = Fixtures::synthetic(&[("a", &[])]);
        let hashes: Vec<String> = (0..HASH_CHUNK * 2 + 1).map(|i| format!("{i:040x}")).collect();
        assert!(matched_from(&fx, &hashes).unwrap().is_empty());
        let mut many = Fixtures::synthetic(&[]);
        for i in 0..PROJECT_CHUNK + 1 {
            let more = Fixtures::synthetic(&[(&format!("m{i}"), &[])]);
            many.projects.extend(more.projects);
            many.versions.extend(more.versions);
        }
        for v in &mut many.versions {
            v.files[0].hashes.sha1 = format!("{:040x}", v.id.len() + v.id.bytes().map(usize::from).sum::<usize>());
        }
        let hashes: Vec<String> = many.versions.iter().map(|v| v.files[0].hashes.sha1.clone()).collect();
        let matched = matched_from(&many, &hashes).unwrap();
        assert_eq!(matched.len(), hashes.iter().collect::<HashSet<_>>().len());
    }

    fn update_for(fx: &Fixtures, from: &str, to: &str) -> Update {
        Update { current: pinned(fx, from), latest: pinned(fx, to), changelog: Vec::new() }
    }

    #[test]
    fn an_update_replaces_the_mod_syncs_the_new_instance_and_keeps_the_others() {
        let fx = Fixtures::recorded();
        let mut batch = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        batch.mods = vec![pinned(&fx, "xS0Q8LSi"), pinned(&fx, "v7Lw0h1m")];
        batch.local_mods = vec!["mine.jar".into()];
        let update = update_for(&fx, "xS0Q8LSi", "fWv5bnZk");
        let seen = std::cell::RefCell::new(None);
        let fetched = update_with(&mut batch, &[update.clone()], |next| {
            *seen.borrow_mut() = Some(next.clone());
            Ok(3)
        })
        .unwrap();
        assert_eq!(fetched, 3);
        assert_eq!(batch.mods[0], update.latest);
        assert_eq!(batch.mods[1], pinned(&fx, "v7Lw0h1m"));
        assert_eq!(batch.local_mods, ["mine.jar"]);
        assert_eq!(seen.into_inner().unwrap(), batch);
    }

    #[test]
    fn a_failed_sync_or_a_stale_update_leaves_the_instance_as_it_was() {
        let fx = Fixtures::recorded();
        let mut batch = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        batch.mods = vec![pinned(&fx, "xS0Q8LSi")];
        let before = batch.clone();
        let update = update_for(&fx, "xS0Q8LSi", "fWv5bnZk");
        assert!(update_with(&mut batch, &[update], |_| bail!("offline")).is_err());
        assert_eq!(batch, before);
        let stale = update_for(&fx, "v7Lw0h1m", "fWv5bnZk");
        let err = update_with(&mut batch, &[stale], |_| panic!("must not sync")).unwrap_err().to_string();
        assert!(err.contains("no longer in the instance"), "{err}");
        assert_eq!(batch, before);
        assert_eq!(update_with(&mut batch, &[], |_| panic!("must not sync")).unwrap(), 0);
    }

    #[test]
    fn an_update_needs_both_the_project_and_the_pinned_version_to_match() {
        let mut batch = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        let a = ModRef { project_id: "p1".into(), version_id: "v1".into(), ..jar("a.jar", b"a") };
        let b = ModRef { project_id: "p2".into(), version_id: "v2".into(), ..jar("b.jar", b"b") };
        batch.mods = vec![a.clone(), b.clone()];
        let latest = ModRef { version_id: "v9".into(), ..jar("a2.jar", b"a2") };
        let same_project_other_version = Update { current: ModRef { version_id: "vX".into(), ..a.clone() }, latest: latest.clone(), changelog: Vec::new() };
        assert!(replaced(&batch, &[same_project_other_version]).is_err());
        let same_version_other_project = Update { current: ModRef { project_id: "pX".into(), ..a.clone() }, latest: latest.clone(), changelog: Vec::new() };
        assert!(replaced(&batch, &[same_version_other_project]).is_err());
        let next = replaced(&batch, &[Update { current: a, latest: latest.clone(), changelog: Vec::new() }]).unwrap();
        assert_eq!(next.mods, [latest, b]);
    }

    #[test]
    fn a_mod_file_called_like_a_local_jar_is_refused_before_anything_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.jar"), b"mine").unwrap();
        std::fs::write(dir.path().join("stray.jar"), b"stray").unwrap();
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[]);
        let err = sync_into(&agent, &[jar("foo.jar", b"theirs")], &["FOO.jar".into()], &[], dir.path(), &ledger).unwrap_err().to_string();
        assert!(err.contains("local mod"), "{err}");
        assert_eq!(std::fs::read(dir.path().join("Foo.jar")).unwrap(), b"mine");
        assert!(dir.path().join("stray.jar").is_file());
    }

    #[test]
    fn fabric_api_is_added_unless_the_batch_has_it_by_id_or_as_a_local_jar() {
        let mut batch = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        assert!(needs_fabric_api(&batch));
        for other in [Loader::Forge(None), Loader::NeoForge(None), Loader::Vanilla] {
            assert!(!needs_fabric_api(&Instance::new("b", "26.3", other)));
        }
        assert!(needs_fabric_api(&Instance::new("b", "26.3", Loader::Quilt(None))));
        batch.local_mods = vec!["Fabric-API-0.100.jar".into()];
        assert!(!needs_fabric_api(&batch));
        batch.local_mods = vec!["lithium.jar".into()];
        assert!(needs_fabric_api(&batch));
        batch.mods = vec![ModRef { project_id: FABRIC_API.into(), ..jar("x.jar", b"x") }];
        assert!(!needs_fabric_api(&batch));
    }

    #[test]
    fn syncing_after_an_update_swaps_the_jar_and_keeps_local_jars() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.jar"), b"old").unwrap();
        std::fs::write(dir.path().join("new.jar"), b"new").unwrap();
        std::fs::write(dir.path().join("mine.jar"), b"mine").unwrap();
        std::fs::write(dir.path().join("Other.JAR"), b"other").unwrap();
        std::fs::write(dir.path().join("stray.jar"), b"stray").unwrap();
        let mut batch = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        batch.mods = vec![jar("old.jar", b"old")];
        batch.local_mods = vec!["MINE.jar".into(), "other.jar".into()];
        let update = Update { current: batch.mods[0].clone(), latest: ModRef { version_id: "v2".into(), ..jar("new.jar", b"new") }, changelog: Vec::new() };
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[("old.jar", &b"old"[..]), ("stray.jar", &b"stray"[..])]);
        let fetched = update_with(&mut batch, &[update], |next| sync_into(&agent, &next.mods, &next.local_mods, &next.disabled, dir.path(), &ledger)).unwrap();
        assert_eq!(fetched, 0);
        let mut left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["Other.JAR", "mine.jar", "new.jar"]);
        assert_eq!(batch.mods[0].file_name, "new.jar");
    }

    #[test]
    fn search_hits_parse() {
        let fx = Fixtures::recorded();
        let hits = fx.search("sodium", "26.3", &fabric()).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!((hits[0].slug.as_str(), hits[0].title.as_str(), hits[0].project_id.as_str()), ("sodium", "Sodium", "AANobbMI"));
        assert!(hits[0].downloads > 0 && !hits[0].description.is_empty());
        let none: Hit = serde_json::from_str(r#"{"title":"t","slug":"s","project_id":"p","description":"d","downloads":1,"icon_url":null}"#).unwrap();
        assert_eq!(none.icon_url, None);
        let full: SearchResponse = serde_json::from_str(r#"{"hits":[{"title":"t","slug":"s","project_id":"p","description":"d","downloads":1,"author":"someone","versions":["26.3","26.2"],"categories":["fabric","optimization"]}],"offset":0,"limit":20,"total_hits":57}"#).unwrap();
        assert_eq!((full.total_hits, full.hits[0].author.as_str(), full.hits[0].versions.len(), full.hits[0].support("26.2", &fabric())), (57, "someone", 2, Support::Fits));
    }

    #[test]
    fn urls_are_encoded_and_filter_for_the_loader() {
        assert_eq!(encode("a b&é/~-_."), "a%20b%26%C3%A9%2F~-_.");
        let url = search_url("shaders & more", "26.3", &fabric(), Sort::Relevance, false, 0);
        assert!(url.starts_with("https://api.modrinth.com/v2/search?query=shaders%20%26%20more&facets=%5B%5B"), "{url}");
        assert!(url.contains("categories%3Afabric") && url.contains("versions%3A26.3") && url.contains("client_side%3Arequired"));
        assert!(url.ends_with("&index=relevance&offset=0&limit=20"), "{url}");
        assert_eq!(
            versions_url("lithium", "26.3", &fabric()),
            "https://api.modrinth.com/v2/project/lithium/version?loaders=%5B%22fabric%22%5D&game_versions=%5B%2226.3%22%5D"
        );
        let quilt = Loader::Quilt(None);
        let url = search_url("x", "26.3", &quilt, Sort::Relevance, false, 0);
        assert!(url.contains("%5B%22categories%3Aquilt%22%2C%22categories%3Afabric%22%5D"), "{url}");
        assert_eq!(
            versions_url("lithium", "26.3", &quilt),
            "https://api.modrinth.com/v2/project/lithium/version?loaders=%5B%22quilt%22%2C%22fabric%22%5D&game_versions=%5B%2226.3%22%5D"
        );
        let url = search_url("x", "26.3", &Loader::NeoForge(None), Sort::Relevance, false, 0);
        assert!(url.contains("%5B%22categories%3Aneoforge%22%5D") && !url.contains("categories%3Afabric"), "{url}");
        assert!(versions_url("lithium", "26.3", &Loader::NeoForge(None)).contains("loaders=%5B%22neoforge%22%5D"));
    }

    fn facets_of(url: &str) -> Vec<Vec<String>> {
        let raw = url.split("&facets=").nth(1).unwrap().split('&').next().unwrap();
        let mut bytes = Vec::new();
        let mut it = raw.bytes();
        while let Some(b) = it.next() {
            if b == b'%' {
                let hex = [it.next().unwrap(), it.next().unwrap()];
                bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap());
            } else {
                bytes.push(b);
            }
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn each_kind_searches_its_project_type_with_the_instance_filters() {
        let search = |kind, shader: Option<&str>, loader: &Loader| Search { filter: Filter::of(kind, "26.3", loader, shader), query: " fresh ".into(), sort: Sort::Updated, category: None, all: false };
        let facets = |s: &Search| facets_of(&search_url_for(s, 0));
        let v = || vec!["versions:26.3".to_owned()];
        assert_eq!(facets(&search(Kind::Mod, None, &fabric())), [vec!["project_type:mod".to_owned()], vec!["categories:fabric".into()], v(), vec!["client_side:required".into(), "client_side:optional".into()]]);
        assert_eq!(facets(&search(Kind::ResourcePack, None, &fabric())), [vec!["project_type:resourcepack".to_owned()], vec!["categories:minecraft".into()], v()]);
        assert_eq!(facets(&search(Kind::Shader, None, &Loader::Vanilla)), [vec!["project_type:shader".to_owned()], vec!["categories:iris".into()], v()]);
        assert_eq!(facets(&search(Kind::Shader, Some("optifine"), &fabric())), [vec!["project_type:shader".to_owned()], vec!["categories:optifine".into()], v()]);
        assert_eq!(facets(&search(Kind::DataPack, None, &fabric())), [vec!["project_type:datapack".to_owned()], vec!["categories:datapack".into()], v()]);
        assert_eq!(facets(&search(Kind::Modpack, None, &Loader::Quilt(None)))[1], ["categories:quilt"]);
        assert_eq!(facets(&search(Kind::Mod, None, &Loader::Quilt(None)))[1], ["categories:quilt", "categories:fabric"]);
        let url = search_url_for(&search(Kind::ResourcePack, None, &fabric()), 20);
        assert!(url.contains("query=fresh&") && url.ends_with("&index=updated&offset=20&limit=20"), "{url}");
        let newest = Search { sort: Sort::Newest, category: Some("vanilla-like".into()), ..search(Kind::Shader, None, &fabric()) };
        assert!(search_url_for(&newest, 0).contains("&index=newest&"));
        assert_eq!(facets_of(&search_url_for(&newest, 0)).last().unwrap(), &["categories:vanilla-like"]);
        let anything = Search { filter: Filter::any(Kind::Modpack), query: String::new(), sort: Sort::Downloads, category: None, all: false };
        assert_eq!(facets_of(&search_url_for(&anything, 0)), [vec!["project_type:modpack".to_owned()], vec!["client_side:required".into(), "client_side:optional".into()]]);
        let all = Search { all: true, ..search(Kind::DataPack, None, &fabric()) };
        assert_eq!(facets_of(&search_url_for(&all, 0)), [vec!["project_type:datapack".to_owned()]]);
    }

    #[test]
    fn version_lookups_follow_the_filter() {
        assert_eq!(versions_url_for("fresh", &Filter::of(Kind::ResourcePack, "26.3", &fabric(), None)), "https://api.modrinth.com/v2/project/fresh/version?loaders=%5B%22minecraft%22%5D&game_versions=%5B%2226.3%22%5D");
        assert_eq!(versions_url_for("fo", &Filter::any(Kind::Modpack)), "https://api.modrinth.com/v2/project/fo/version");
        let version = |id: &str, kind: &str, date: &str, loaders: &[&str], games: &[&str]| Version {
            id: id.into(),
            project_id: "p".into(),
            name: String::new(),
            version_number: id.into(),
            version_type: kind.into(),
            date_published: date.into(),
            game_versions: games.iter().map(|s| (*s).to_owned()).collect(),
            loaders: loaders.iter().map(|s| (*s).to_owned()).collect(),
            files: Vec::new(),
            dependencies: Vec::new(),
            changelog: None,
        };
        let list = [version("a", "release", "2026-01", &["iris"], &["26.3"]), version("b", "beta", "2026-03", &["iris"], &["26.3"]), version("c", "release", "2026-05", &["optifine"], &["26.3"]), version("d", "release", "2026-06", &["iris"], &["26.2"])];
        let shader = Filter::of(Kind::Shader, "26.3", &fabric(), None);
        assert_eq!(newest_for(&list, &shader).map(|v| v.id.as_str()), Some("a"));
        assert_eq!(newest_for(&list, &Filter::any(Kind::Shader)).map(|v| v.id.as_str()), Some("d"));
        let hit = Hit { title: "t".into(), slug: "s".into(), project_id: "p".into(), description: String::new(), downloads: 0, icon_url: None, author: String::new(), versions: vec!["26.3".into()], categories: vec!["optifine".into()] };
        assert_eq!(hit.fits(&shader), Support::OtherLoader);
        assert_eq!(hit.fits(&Filter::of(Kind::ResourcePack, "26.2", &fabric(), None)), Support::OtherLoader);
        assert_eq!(hit.fits(&Filter::any(Kind::Shader)), Support::Fits);
    }

    #[test]
    fn showing_all_drops_the_version_and_loader_filters_and_pages_by_offset() {
        let url = search_url("", "26.3", &fabric(), Sort::Downloads, true, 40);
        assert!(!url.contains("categories%3Afabric") && !url.contains("versions%3A") && url.contains("project_type%3Amod"), "{url}");
        assert!(url.ends_with("&index=downloads&offset=40&limit=20"), "{url}");
    }

    #[test]
    fn a_hit_says_which_of_version_and_loader_is_missing() {
        let hit = |versions: &[&str], categories: &[&str]| Hit {
            title: "t".into(),
            slug: "s".into(),
            project_id: "p".into(),
            description: String::new(),
            downloads: 0,
            icon_url: None,
            author: String::new(),
            versions: versions.iter().map(|s| (*s).to_owned()).collect(),
            categories: categories.iter().map(|s| (*s).to_owned()).collect(),
        };
        assert_eq!(hit(&["26.3"], &["fabric", "forge"]).support("26.3", &fabric()), Support::Fits);
        assert_eq!(hit(&["26.2"], &["fabric"]).support("26.3", &fabric()), Support::OtherVersion);
        assert_eq!(hit(&["26.3"], &["forge"]).support("26.3", &fabric()), Support::OtherLoader);
        assert_eq!(hit(&[], &[]).support("26.3", &fabric()), Support::OtherLoader);
        assert_eq!(hit(&["26.3"], &["fabric"]).support("26.3", &Loader::Quilt(None)), Support::Fits);
        assert_eq!(hit(&["26.3"], &["quilt"]).support("26.3", &fabric()), Support::OtherLoader);
        assert_eq!(hit(&["26.3"], &["fabric"]).support("26.3", &Loader::NeoForge(None)), Support::OtherLoader);
        assert_eq!(hit(&["26.3"], &["neoforge"]).support("26.3", &Loader::NeoForge(None)), Support::Fits);
        assert_eq!(hit(&["26.3"], &["fabric"]).support("26.3", &Loader::Vanilla), Support::OtherLoader);
    }

    #[test]
    fn file_names_cannot_escape_the_mod_folder() {
        assert!(safe_file_name("lithium-0.1.jar").is_ok());
        for bad in ["", "..", "../x.jar", "a/b.jar", "a\\b.jar", "C:x.jar", "/x.jar"] {
            assert!(safe_file_name(bad).is_err(), "{bad}");
        }
    }

    fn ledger(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mods.managed");
        let text: String = entries.iter().map(|(name, bytes)| format!("{name} {}\n", jar(name, bytes).sha1)).collect();
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut left: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        left
    }

    fn jar(name: &str, bytes: &[u8]) -> ModRef {
        use sha1::{Digest, Sha1};
        let sha1: String = Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        ModRef {
            project_id: name.into(),
            version_id: name.into(),
            title: name.into(),
            file_name: name.into(),
            url: "http://127.0.0.1:1/unreachable".into(),
            sha1,
        }
    }

    #[test]
    fn sync_deletes_stray_jars_and_keeps_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let mods = dir.path().join("mods");
        std::fs::create_dir_all(mods.join("manual")).unwrap();
        std::fs::create_dir_all(mods.join("folder.jar")).unwrap();
        std::fs::write(mods.join("a.jar"), b"aaa").unwrap();
        std::fs::write(mods.join("b.jar"), b"bbb").unwrap();
        std::fs::write(mods.join("stray.jar"), b"old").unwrap();
        std::fs::write(mods.join("STRAY2.JAR"), b"old").unwrap();
        std::fs::write(mods.join("notes.txt"), b"keep").unwrap();
        std::fs::write(mods.join("manual").join("mine.jar"), b"keep").unwrap();
        let list = [jar("a.jar", b"aaa"), jar("b.jar", b"bbb"), jar("a.jar", b"aaa")];
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[("stray.jar", &b"old"[..]), ("stray2.jar", &b"old"[..])]);
        assert_eq!(sync_into(&agent, &list, &[], &[], &mods, &ledger).unwrap(), 0);
        let mut left: Vec<String> = std::fs::read_dir(&mods).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["a.jar", "b.jar", "folder.jar", "manual", "notes.txt"]);
        assert!(mods.join("manual").join("mine.jar").is_file());
        assert_eq!(sync_into(&agent, &[], &[], &[], &mods, &ledger).unwrap(), 0);
        assert!(!mods.join("a.jar").exists() && !mods.join("b.jar").exists());
        assert!(mods.join("folder.jar").is_dir() && mods.join("notes.txt").is_file());
    }

    #[test]
    fn sync_creates_the_folder_and_refuses_unsafe_names() {
        let dir = tempfile::tempdir().unwrap();
        let mods = dir.path().join("new").join("mods");
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[]);
        assert_eq!(sync_into(&agent, &[], &[], &[], &mods, &ledger).unwrap(), 0);
        assert!(mods.is_dir());
        let mut evil = jar("x.jar", b"x");
        evil.file_name = "../x.jar".into();
        assert!(sync_into(&agent, &[evil], &[], &[], &mods, &ledger).is_err());
        assert!(!dir.path().join("new").join("x.jar").exists());
    }

    #[test]
    fn sync_downloads_missing_and_changed_files_and_counts_them() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.unwrap();
                let mut buf = [0; 2048];
                let n = stream.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let body: &[u8] = if request.starts_with("GET /new.jar") { b"fresh" } else { b"updated" };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(body).unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kept.jar"), b"kept").unwrap();
        std::fs::write(dir.path().join("changed.jar"), b"outdated").unwrap();
        std::fs::write(dir.path().join("stray.jar"), b"x").unwrap();
        let url = |name: &str| format!("http://127.0.0.1:{port}/{name}");
        let list = [
            ModRef { url: url("kept.jar"), ..jar("kept.jar", b"kept") },
            ModRef { url: url("new.jar"), ..jar("new.jar", b"fresh") },
            ModRef { url: url("changed.jar"), ..jar("changed.jar", b"updated") },
        ];
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[("stray.jar", &b"x"[..])]);
        assert_eq!(sync_into(&agent, &list, &[], &[], dir.path(), &ledger).unwrap(), 2);
        server.join().unwrap();
        assert_eq!(std::fs::read(dir.path().join("new.jar")).unwrap(), b"fresh");
        assert_eq!(std::fs::read(dir.path().join("changed.jar")).unwrap(), b"updated");
        assert_eq!(std::fs::read(dir.path().join("kept.jar")).unwrap(), b"kept");
        assert!(!dir.path().join("stray.jar").exists());
    }

    #[test]
    fn sync_deletes_nothing_when_a_download_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stray.jar"), b"x").unwrap();
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[("stray.jar", &b"x"[..])]);
        assert!(sync_into(&agent, &[jar("missing.jar", b"m")], &[], &[], dir.path(), &ledger).is_err());
        assert!(dir.path().join("stray.jar").is_file());
    }

    #[test]
    fn a_disabled_modrinth_mod_is_downloaded_as_a_disabled_file() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 2048];
            let _ = stream.read(&mut buf).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n").unwrap();
            stream.write_all(b"fresh").unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let list = [ModRef { url: format!("http://127.0.0.1:{port}/new.jar"), ..jar("new.jar", b"fresh") }];
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[]);
        assert_eq!(sync_into(&agent, &list, &[], &["NEW.jar".into()], dir.path(), &ledger).unwrap(), 1);
        server.join().unwrap();
        assert_eq!(names(dir.path()), ["new.jar.disabled"]);
        assert_eq!(std::fs::read(dir.path().join("new.jar.disabled")).unwrap(), b"fresh");
        assert_eq!(read_ledger(&ledger).get("new.jar").map(|e| e.sha1.as_str()), Some(jar("new.jar", b"fresh").sha1.as_str()));
        assert_eq!(read_ledger(&ledger).get("new.jar").and_then(|e| e.stamp).map(|s| s.size), Some(5));
        assert_eq!(sync_into(&agent, &list, &[], &["new.jar".into()], dir.path(), &ledger).unwrap(), 0);
    }

    #[test]
    fn a_mod_switched_on_or_off_is_renamed_in_place_and_not_downloaded_again() {
        let dir = tempfile::tempdir().unwrap();
        let mods = dir.path();
        for (name, body) in [("a.jar", "aaa"), ("b.jar.disabled", "bbb"), ("own.jar", "own"), ("back.JAR.disabled", "back"), ("both.jar", "x"), ("both.jar.disabled", "y")] {
            std::fs::write(mods.join(name), body).unwrap();
        }
        let list = [jar("a.jar", b"aaa"), jar("b.jar", b"bbb")];
        let local: Vec<String> = ["own.jar", "back.jar", "both.jar"].map(String::from).to_vec();
        let off: Vec<String> = ["a.jar", "own.jar", "both.jar"].map(String::from).to_vec();
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[]);
        assert_eq!(sync_into(&agent, &list, &local, &off, mods, &ledger).unwrap(), 0);
        assert_eq!(names(mods), ["a.jar.disabled", "b.jar", "back.JAR", "both.jar", "both.jar.disabled", "own.jar.disabled"]);
        assert_eq!(std::fs::read(mods.join("b.jar")).unwrap(), b"bbb");
    }

    #[test]
    fn a_stray_jar_the_user_added_survives_a_sync_and_one_an_earlier_sync_placed_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let mods = dir.path();
        for name in ["a.jar", "user.jar", "User2.jar.disabled", "old.jar", "Old2.jar.disabled", crate::core::fleet::COMPANION_FILE] {
            std::fs::write(mods.join(name), "x").unwrap();
        }
        std::fs::write(mods.join("a.jar"), b"aaa").unwrap();
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[("old.jar", &b"x"[..]), ("old2.jar", &b"x"[..]), ("OLD3.jar", &b"x"[..]), (crate::core::fleet::COMPANION_FILE, &b"x"[..])]);
        sync_into(&agent, &[jar("a.jar", b"aaa")], &[], &[], mods, &ledger).unwrap();
        assert_eq!(names(mods), ["User2.jar.disabled", "a.jar", "manycraft-companion.jar", "user.jar"]);
        assert_eq!(read_ledger(&ledger).keys().collect::<Vec<_>>(), ["a.jar"]);
        assert_eq!(read_ledger(&ledger)["a.jar"].sha1, jar("a.jar", b"aaa").sha1);
        sync_into(&agent, &[], &[], &[], mods, &ledger).unwrap();
        assert_eq!(names(mods), ["User2.jar.disabled", "manycraft-companion.jar", "user.jar"]);
    }

    #[test]
    fn a_jar_with_a_ledger_name_is_deleted_only_when_its_hash_is_the_recorded_one() {
        let dir = tempfile::tempdir().unwrap();
        let mods = dir.path();
        for (name, body) in [("x.jar", "synced"), ("x.jar.disabled", "mine"), ("y.jar", "mine"), ("z.jar.disabled", "synced"), ("z.jar", "mine")] {
            std::fs::write(mods.join(name), body).unwrap();
        }
        let agent = net::agent(None).unwrap();
        let (_held, ledger) = ledger(&[("x.jar", &b"synced"[..]), ("y.jar", &b"synced"[..]), ("z.jar", &b"synced"[..])]);
        sync_into(&agent, &[], &[], &[], mods, &ledger).unwrap();
        assert_eq!(names(mods), ["x.jar.disabled", "y.jar", "z.jar"]);
        assert_eq!(std::fs::read_to_string(&ledger).unwrap(), "");
        sync_into(&agent, &[], &[], &[], mods, &ledger).unwrap();
        assert_eq!(names(mods), ["x.jar.disabled", "y.jar", "z.jar"]);
    }

    #[test]
    fn the_ledger_keeps_a_lowercase_hash_per_name_and_ignores_lines_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mods.managed");
        let hash = "AB".repeat(20);
        std::fs::write(&path, format!("old.jar\nbad name.jar zz\nOk Name.JAR {hash}\n\n{hash}\n")).unwrap();
        let read = read_ledger(&path);
        assert_eq!(read.len(), 1);
        assert_eq!(read.get("ok name.jar"), Some(&Recorded { sha1: hash.to_lowercase(), stamp: None }));
        assert!(read_ledger(&dir.path().join("none")).is_empty());
        let (_held, written) = ledger(&[]);
        let agent = net::agent(None).unwrap();
        let mut api = jar("Fabric-API.jar", b"api");
        api.sha1 = api.sha1.to_uppercase();
        let mods = dir.path().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("Fabric-API.jar"), b"api").unwrap();
        sync_into(&agent, &[api.clone()], &[], &[], &mods, &written).unwrap();
        assert_eq!(read_ledger(&written).get("fabric-api.jar").map(|e| &e.sha1), Some(&jar("Fabric-API.jar", b"api").sha1));
    }

    #[test]
    fn the_ledger_reads_old_lines_and_lines_with_a_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mods.managed");
        let (a, b) = ("ab".repeat(20), "cd".repeat(20));
        std::fs::write(&path, format!("old.jar {a}\nSome Name.jar {b} 12 1700000000123456789\nbad.jar {a} x 5\n")).unwrap();
        let read = read_ledger(&path);
        assert_eq!(read.get("old.jar"), Some(&Recorded { sha1: a, stamp: None }));
        assert_eq!(read.get("some name.jar"), Some(&Recorded { sha1: b, stamp: Some(Stamp { size: 12, mtime: 1_700_000_000_123_456_789 }) }));
        assert_eq!(read.len(), 2);
    }

    fn stamped(dir: &Path, name: &str, written: &[u8], recorded: &ModRef) -> std::path::PathBuf {
        let ledger = dir.join("mods.managed");
        let mods = dir.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join(name), written).unwrap();
        let Stamp { size, mtime } = Stamp::of(&mods.join(name)).unwrap();
        std::fs::write(&ledger, format!("{} {} {size} {mtime}\n", name.to_lowercase(), recorded.sha1)).unwrap();
        ledger
    }

    #[test]
    fn a_jar_that_matches_the_ledger_stamp_is_not_hashed_again() {
        let dir = tempfile::tempdir().unwrap();
        let want = jar("a.jar", b"real");
        let ledger = stamped(dir.path(), "a.jar", b"fake", &want);
        let agent = net::agent(None).unwrap();
        let mods = dir.path().join("mods");
        assert_eq!(sync_into(&agent, &[want.clone()], &[], &[], &mods, &ledger).unwrap(), 0);
        assert_eq!(std::fs::read(mods.join("a.jar")).unwrap(), b"fake");
        let kept = read_ledger(&ledger);
        assert_eq!(kept["a.jar"].stamp, Stamp::of(&mods.join("a.jar")));
    }

    #[test]
    fn a_jar_whose_stamp_differs_from_the_ledger_is_hashed_again() {
        let dir = tempfile::tempdir().unwrap();
        let want = jar("a.jar", b"real");
        let ledger = stamped(dir.path(), "a.jar", b"real", &want);
        let mods = dir.path().join("mods");
        let agent = net::agent(None).unwrap();
        let touch = |later: u64| std::fs::File::options().write(true).open(mods.join("a.jar")).unwrap().set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(later)).unwrap();
        touch(60);
        assert_eq!(sync_into(&agent, &[want.clone()], &[], &[], &mods, &ledger).unwrap(), 0);
        assert_eq!(read_ledger(&ledger)["a.jar"].stamp, Stamp::of(&mods.join("a.jar")));
        std::fs::write(mods.join("a.jar"), b"edit").unwrap();
        touch(120);
        let err = sync_into(&agent, &[want], &[], &[], &mods, &ledger).unwrap_err();
        assert!(format!("{err:#}").contains("cannot download"), "{err:#}");
    }

    #[test]
    fn a_stray_jar_is_deleted_by_stamp_and_by_hash_only_when_it_is_the_recorded_one() {
        let dir = tempfile::tempdir().unwrap();
        let gone = jar("x.jar", b"synced");
        let ledger = stamped(dir.path(), "x.jar", b"synced", &gone);
        let mods = dir.path().join("mods");
        std::fs::write(mods.join("y.jar"), b"mine").unwrap();
        let agent = net::agent(None).unwrap();
        let managed = read_ledger(&ledger);
        assert!(is_managed(&managed, "X.JAR", &mods.join("x.jar")));
        assert!(!is_managed(&managed, "y.jar", &mods.join("y.jar")));
        std::fs::write(mods.join("x.jar"), b"swapped").unwrap();
        assert!(!is_managed(&managed, "x.jar", &mods.join("x.jar")));
        sync_into(&agent, &[], &[], &[], &mods, &ledger).unwrap();
        assert_eq!(names(&mods), ["x.jar", "y.jar"]);
    }

    #[test]
    fn updates_are_refused_while_the_troubleshooter_runs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(not_searching(dir.path()).is_ok());
        let mut state = crate::core::content::Bisect { before: Vec::new(), suspects: Vec::new(), off: Vec::new(), round: 0, rounds: 1, outcome: None };
        std::fs::write(dir.path().join("bisect.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        let err = not_searching(dir.path()).unwrap_err().to_string();
        assert!(err.contains("Fehlersuche") || err.contains("troubleshooter"), "{err}");
        let mut batch = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        let current = jar("a.jar", b"a");
        batch.mods = vec![current.clone()];
        let update = Update { current, latest: ModRef { version_id: "v2".into(), ..jar("a2.jar", b"a2") }, changelog: Vec::new() };
        let before = batch.clone();
        assert!(update_with(&mut batch, &[update], |_| not_searching(dir.path()).map(|()| 0)).is_err());
        assert_eq!(batch, before);
        state.outcome = Some(crate::core::content::Outcome::Nothing);
        std::fs::write(dir.path().join("bisect.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(not_searching(dir.path()).is_ok());
    }

    #[test]
    fn an_update_keeps_a_disabled_mod_disabled_under_its_new_file_name() {
        let mut batch = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        batch.mods = vec![jar("old.jar", b"old"), jar("other.jar", b"other")];
        batch.disabled = vec!["OLD.jar".into(), "gone.jar".into()];
        let update = Update { current: batch.mods[0].clone(), latest: ModRef { version_id: "v2".into(), ..jar("new.jar", b"new") }, changelog: Vec::new() };
        let next = replaced(&batch, &[update]).unwrap();
        assert_eq!(next.disabled, ["new.jar", "gone.jar"]);
        let other = Update { current: batch.mods[1].clone(), latest: ModRef { version_id: "v2".into(), ..jar("other2.jar", b"o2") }, changelog: Vec::new() };
        assert_eq!(replaced(&batch, &[other]).unwrap().disabled, batch.disabled);
    }

    fn versions_json(rows: &[(&str, &str, &str, Option<&str>)]) -> Vec<Version> {
        let items: Vec<String> = rows
            .iter()
            .map(|(id, kind, date, log)| {
                let log = log.map_or("null".to_owned(), |l| format!("{l:?}"));
                format!(
                    r#"{{"id":"{id}","project_id":"p","version_number":"n-{id}","version_type":"{kind}","date_published":"{date}","game_versions":["26.3"],"loaders":["fabric"],"files":[],"dependencies":[],"changelog":{log}}}"#
                )
            })
            .collect();
        serde_json::from_str(&format!("[{}]", items.join(","))).unwrap()
    }

    #[test]
    fn the_changelog_is_every_version_after_the_current_one_up_to_the_latest_newest_first_and_capped() {
        let versions = versions_json(&[
            ("v1", "release", "2026-01-01T00:00:00Z", Some("one")),
            ("v2", "release", "2026-01-02T00:00:00Z", None),
            ("v3", "release", "2026-01-03T00:00:00Z", Some("three")),
            ("v4", "release", "2026-01-04T00:00:00Z", Some("four")),
            ("v5", "release", "2026-01-05T00:00:00Z", Some("five")),
            ("v6", "release", "2026-01-06T00:00:00Z", Some("six")),
            ("v7", "release", "2026-01-07T00:00:00Z", Some("seven")),
            ("v8", "beta", "2026-01-08T00:00:00Z", Some("beta")),
        ]);
        let latest = newest(&versions, "26.3", &fabric()).unwrap();
        assert_eq!(latest.id, "v7");
        let from = |date: Option<&str>| changelog_since(&versions, latest, date, "26.3", &fabric());
        let all = from(Some("2026-01-01T00:00:00Z"));
        assert_eq!(all.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["n-v7", "n-v6", "n-v5", "n-v4", "n-v3"]);
        assert_eq!(all[0].1, "seven");
        assert_eq!(from(Some("2026-01-05T00:00:00Z")), [("n-v7".to_owned(), "seven".to_owned()), ("n-v6".to_owned(), "six".to_owned())]);
        assert_eq!(from(Some("2026-01-01T12:00:00Z")).last().unwrap(), &("n-v3".to_owned(), "three".to_owned()));
        assert_eq!(from(None), [("n-v7".to_owned(), "seven".to_owned())]);
        let single = versions_json(&[("a", "release", "2026-01-02T00:00:00Z", None)]);
        assert_eq!(changelog_since(&single, &single[0], Some("2026-01-01T00:00:00Z"), "26.3", &fabric()), [("n-a".to_owned(), String::new())]);
    }

    #[test]
    fn jar_files_are_told_by_their_ending_in_any_case() {
        assert_eq!(jar_name("a.jar"), Some(("a.jar", false)));
        assert_eq!(jar_name("A.JAR.Disabled"), Some(("A.JAR", true)));
        for other in ["a.txt", "a.jar.bak", ".disabled", "jar", "a.jar.disabled.disabled", "é.disabled", "日本.jar.disable"] {
            assert_eq!(jar_name(other), None, "{other}");
        }
        assert_eq!(jar_name("日本.jar.disabled"), Some(("日本.jar", true)));
    }

    #[test]
    fn arguments_parse() {
        let args = |s: &[&str]| s.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        assert_eq!(parse(&args(&["a", "b", "--version", "26.3"])), Some((args(&["a", "b"]), Some("26.3".into()))));
        assert_eq!(parse(&args(&["--version", "1.21", "x"])), Some((args(&["x"]), Some("1.21".into()))));
        assert_eq!(parse(&args(&["x"])), Some((args(&["x"]), None)));
        assert_eq!(parse(&args(&["x", "--version"])), None);
        assert_eq!(parse(&args(&["x", "--nope"])), None);
        assert_eq!(cli(&args(&[])), 2);
        assert_eq!(cli(&args(&["resolve"])), 2);
        assert_eq!(cli(&args(&["list", "x"])), 2);
    }
}
