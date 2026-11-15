//! Installs a game version with its mod loader: version metadata, libraries, assets, natives, the Java runtime and,
//! for Forge and NeoForge, the installer's processors.

pub mod assets;
pub mod clean;
mod download;
mod fabric;
mod forge;
pub use self::forge::neoforge_game;
mod java;
pub mod meta;
mod natives;
mod quilt;
mod seed;

use std::cmp::Ordering;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use self::download::{Downloader, Job, join_safe, read_json};
use self::forge::Flavor;
use self::meta::{Env, VersionJson};
use super::instance::Loader;
use super::{net, online, paths};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const FALLBACK_JAVA_COMPONENT: &str = "jre-legacy";
const QUICK_METADATA: Duration = Duration::from_secs(5);
const RELEASES_CACHE: &str = "releases.json";

/// What the installer is working on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Metadata,
    Client,
    Libraries,
    Natives,
    Assets,
    Runtime,
    Processors,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Metadata => "metadata",
            Stage::Client => "game",
            Stage::Libraries => "libraries",
            Stage::Natives => "natives",
            Stage::Assets => "assets",
            Stage::Runtime => "java",
            Stage::Processors => "loader",
        }
    }
}

/// Progress of one install stage: `done` of `total` files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
}

/// A game version with its loader, ready to launch.
#[derive(Clone, Debug)]
pub struct Installed {
    pub game_version: String,
    pub loader_version: String,
    pub id: String,
    /// What `${version_name}` stands for: for Forge and NeoForge the name of the game jar, which their launchers skip by that name.
    pub version_name: String,
    pub libraries_dir: PathBuf,
    pub profile: VersionJson,
    pub java: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub natives_dir: PathBuf,
    pub assets_dir: PathBuf,
    pub asset_index: String,
    pub log_config: Option<PathBuf>,
    pub downloaded: usize,
}

#[derive(Deserialize)]
struct Manifest {
    latest: Latest,
    versions: Vec<ManifestVersion>,
}

#[derive(Deserialize)]
struct Latest {
    release: String,
}

#[derive(Deserialize)]
struct ManifestVersion {
    id: String,
    #[serde(rename = "type", default)]
    kind: String,
    url: String,
    sha1: String,
}

struct Plan {
    profile: VersionJson,
    loader: String,
    runtime: Option<java::Runtime>,
    installer: Option<forge::Installer>,
}

/// Loader versions for a game, newest first, and the one an instance that pins none follows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Versions {
    pub list: Vec<String>,
    pub default: Option<String>,
}

/// Orders loader versions numerically, a pre-release before its release: 0.9 < 0.10-beta.2 < 0.10.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let split = |v: &str| v.split_once('-').map_or((v.to_owned(), None), |(r, p)| (r.to_owned(), Some(p.to_owned())));
    let ((ar, ap), (br, bp)) = (split(a), split(b));
    parts(&ar).cmp(&parts(&br)).then_with(|| match (ap, bp) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => parts(&x).cmp(&parts(&y)),
    })
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Num(u64),
    Text(String),
}

fn parts(v: &str) -> Vec<Part> {
    v.split(['.', '-', '+']).map(|p| p.parse().map_or_else(|_| Part::Text(p.to_owned()), Part::Num)).collect()
}

fn newest_first(mut list: Vec<String>) -> Vec<String> {
    list.sort_by(|a, b| compare_versions(b, a));
    list.dedup();
    list
}

fn flavor(loader: &Loader) -> Option<Flavor> {
    match loader {
        Loader::Forge(_) => Some(Flavor::Forge),
        Loader::NeoForge(_) => Some(Flavor::NeoForge),
        _ => None,
    }
}

/// Start of the folder name of every installed version of `loader`; `None` for Vanilla, whose folder is the game version.
fn id_prefix(loader: &Loader) -> Option<&'static str> {
    match loader {
        Loader::Vanilla => None,
        Loader::Fabric(_) => Some("fabric-loader-"),
        Loader::Quilt(_) => Some("quilt-loader-"),
        Loader::Forge(_) => Some("forge-"),
        Loader::NeoForge(_) => Some("neoforge-"),
    }
}

/// Folder name of `game` with `loader` at `version`.
fn version_id(loader: &Loader, game: &str, version: &str) -> String {
    match id_prefix(loader) {
        Some(prefix) => format!("{prefix}{version}-{game}"),
        None => game.to_owned(),
    }
}

#[derive(Serialize, Deserialize)]
struct Known {
    latest: String,
    releases: Vec<String>,
}

/// Newest release version Mojang lists; without a connection, the newest from the last list that arrived.
pub fn latest_release() -> Result<String> {
    let agent = net::agent(None)?;
    Ok(known(&paths::data_dir().join("cache"), || net::get_json(&agent, MANIFEST_URL), online::offline())?.latest)
}

/// Release versions Mojang lists, newest first; without a connection, the last list that arrived.
pub fn releases() -> Result<Vec<String>> {
    let agent = net::agent(None)?;
    Ok(known(&paths::data_dir().join("cache"), || net::get_json(&agent, MANIFEST_URL), online::offline())?.releases)
}

fn known(cache: &Path, fetch: impl FnOnce() -> Result<Manifest>, offline: bool) -> Result<Known> {
    let path = cache.join(RELEASES_CACHE);
    let saved = || std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Known>(&bytes).ok());
    if offline && let Some(known) = saved() {
        return Ok(known);
    }
    match fetch() {
        Ok(manifest) => {
            let known = Known { latest: manifest.latest.release.clone(), releases: release_ids(manifest) };
            let _ = serde_json::to_vec(&known).map_err(anyhow::Error::from).and_then(|bytes| net::write_atomic(&path, &bytes));
            Ok(known)
        }
        Err(e) if net::no_connection(&e) => saved().ok_or(e),
        Err(e) => Err(e),
    }
}

fn release_ids(manifest: Manifest) -> Vec<String> {
    manifest.versions.into_iter().filter(|v| v.kind == "release").map(|v| v.id).collect()
}

/// Versions of `loader` for `game`, newest first; empty for Vanilla.
pub fn loader_versions(loader: &Loader, game: &str) -> Result<Versions> {
    let agent = net::agent(None)?;
    match loader {
        Loader::Vanilla => Ok(Versions::default()),
        Loader::Fabric(_) => fabric::versions(&agent, game),
        Loader::Quilt(_) => quilt::versions(&agent, game),
        Loader::Forge(_) => forge::versions(&agent, Flavor::Forge, game),
        Loader::NeoForge(_) => forge::versions(&agent, Flavor::NeoForge, game),
    }
}

/// Newest stable version of `loader` for `game`. Vanilla has none.
pub fn latest_loader(loader: &Loader, game: &str) -> Result<String> {
    let agent = net::agent(None)?;
    match loader {
        Loader::Vanilla => bail!("Vanilla has no loader version"),
        Loader::Fabric(_) => fabric::latest_loader(&agent, game),
        Loader::Quilt(_) => quilt::latest_loader(&agent, game),
        Loader::Forge(_) | Loader::NeoForge(_) => loader_versions(loader, game)?.default.with_context(|| format!("{} does not support Minecraft {game}", loader.name())),
    }
}

/// The newest stable loader of the same kind as `loader` for `game_version`; `None` for kinds Manycraft has no list for, and when only unstable ones exist.
pub fn newest_loader(loader: &Loader, game_version: &str) -> Result<Option<String>> {
    let agent = net::agent(None)?;
    match loader {
        Loader::Fabric(_) => fabric::stable_loader(&agent, game_version),
        Loader::Quilt(_) => fabric::stable_quilt(&agent, game_version),
        _ => Ok(None),
    }
}

/// Whether loader version `latest` is newer than `current`, comparing the dotted numbers; unreadable versions are never newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn numbers(v: &str) -> Option<Vec<u64>> {
        let core = v.split(['-', '+']).next()?;
        core.split('.').map(|p| p.parse().ok()).collect()
    }
    match (numbers(latest), numbers(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Installs `game_version` with `loader` (its pinned version, or the newest stable one) into the shared folders.
/// Files that are already there are not downloaded again. A version that was installed before starts without asking
/// the network while the connection is known to be gone, and asks it only briefly otherwise.
pub fn install(game_version: &str, loader: &Loader, progress: &(dyn Fn(Progress) + Sync)) -> Result<Installed> {
    let _in_use = clean::in_use()?;
    let root = paths::data_dir();
    let dl = Downloader::new(net::agent(None)?)?.seeded(seed::Seeds::new(&root, seed::launchers()));
    install_in(&root, &dl, &net::agent_within(QUICK_METADATA), online::offline(), game_version, loader, progress)
}

fn install_in(root: &Path, dl: &Downloader, quick: &ureq::Agent, offline: bool, game_version: &str, loader: &Loader, progress: &(dyn Fn(Progress) + Sync)) -> Result<Installed> {
    let (versions, libraries, assets_dir, runtimes) = (root.join("versions"), root.join("libraries"), root.join("assets"), root.join("runtimes"));
    progress(Progress { stage: Stage::Metadata, done: 0, total: 1 });
    let local = local_plan(&versions, &libraries, game_version, loader).filter(|plan| java::local(&runtimes, component(&plan.profile)).is_some());
    let plan = match local {
        Some(plan) if offline => plan,
        local => match online_plan(dl, if local.is_some() { quick } else { dl.agent() }, &versions, &libraries, game_version, loader) {
            Ok(plan) => plan,
            Err(online) => local.ok_or(online)?,
        },
    };
    progress(Progress { stage: Stage::Metadata, done: 1, total: 1 });
    let Plan { profile, loader: loader_version, runtime, installer } = plan;
    let id = profile.id.clone();
    let version_dir = join_safe(&versions, &id)?;
    let _lock = lock(&version_dir)?;

    let client = profile.downloads.as_ref().and_then(|d| d.client.as_ref()).context("version has no client download")?;
    let client_jar = join_safe(&versions, game_version)?.join(format!("{game_version}.jar"));
    dl.all(Stage::Client, &[Job::new(client.url.clone(), client_jar.clone(), client.sha1.clone(), client.size)], progress)?;

    let env = Env::current(Vec::new());
    let resolved = meta::resolve(&profile.libraries, &env);
    let mut deps = resolved.classpath.clone();
    deps.extend(resolved.natives.iter().map(|n| n.dep.clone()).filter(|d| !resolved.classpath.contains(d)));
    let processed = installer.as_ref().is_none_or(|i| i.processed(&version_dir, &libraries));
    if let Some(installer) = installer.as_ref().filter(|_| !processed) {
        let _shared = lock(&libraries)?;
        installer.unpack_bundled(&libraries)?;
        let tools: Vec<_> = meta::resolve(installer.libraries(), &env).classpath.into_iter().filter(|d| !deps.contains(d)).collect();
        deps.extend(tools);
    }
    let mut jobs = Vec::with_capacity(deps.len());
    for dep in deps.iter().filter(|d| !d.url.is_empty()) {
        jobs.push(Job::new(dep.url.clone(), join_safe(&libraries, &dep.path)?, dep.sha1.clone(), dep.size));
    }
    dl.all(Stage::Libraries, &jobs, progress)?;

    let natives_dir = version_dir.join("natives");
    let mut jars = Vec::with_capacity(resolved.natives.len());
    for native in &resolved.natives {
        jars.push((join_safe(&libraries, &native.dep.path)?, native.exclude.clone()));
    }
    progress(Progress { stage: Stage::Natives, done: 0, total: jars.len() });
    natives::extract(&jars, &natives_dir)?;
    progress(Progress { stage: Stage::Natives, done: jars.len(), total: jars.len() });

    let index_ref = profile.asset_index.as_ref().context("version has no asset index")?;
    let index_path = join_safe(&assets_dir.join("indexes"), &format!("{}.json", index_ref.id))?;
    dl.one(&Job::new(index_ref.url.clone(), index_path.clone(), index_ref.sha1.clone(), index_ref.size).strict())?;
    let index: assets::AssetIndex = read_json(&index_path)?;
    let mut asset_jobs = assets::jobs(&index, &assets_dir)?;
    let log_config = match profile.logging.as_ref().and_then(|l| l.client.as_ref()) {
        Some(log) => {
            let path = join_safe(&assets_dir.join("log_configs"), &log.file.id)?;
            asset_jobs.push(Job::new(log.file.url.clone(), path.clone(), log.file.sha1.clone(), log.file.size).strict());
            Some(path)
        }
        None => None,
    };
    dl.all(Stage::Assets, &asset_jobs, progress)?;

    let component = component(&profile);
    let java = match &runtime {
        Some(runtime) => java::ensure(dl, &runtimes, runtime, progress)?,
        None => java::local(&runtimes, component).with_context(|| format!("Java runtime {component} is not installed and there is no connection"))?,
    };

    if let Some(installer) = installer.as_ref().filter(|_| !processed) {
        let _shared = lock(&libraries)?;
        let site = forge::Site { root, libraries: &libraries, version_dir: &version_dir, client_jar: &client_jar, game: game_version, java: &java };
        if !installer.processed(&version_dir, &libraries) {
            installer.process(&site, progress)?;
        }
    }

    let mut classpath = Vec::with_capacity(resolved.classpath.len() + 1);
    for dep in &resolved.classpath {
        let path = join_safe(&libraries, &dep.path)?;
        if !path.is_file() {
            bail!("{} is missing after the install", dep.path);
        }
        classpath.push(path);
    }
    classpath.push(client_jar);
    let version_name = if installer.is_some() { game_version.to_owned() } else { id.clone() };
    Ok(Installed {
        game_version: game_version.to_owned(),
        loader_version,
        id,
        version_name,
        libraries_dir: libraries,
        asset_index: index_ref.id.clone(),
        profile,
        java,
        classpath,
        natives_dir,
        assets_dir,
        log_config,
        downloaded: dl.fetched(),
    })
}

fn component(profile: &VersionJson) -> &str {
    profile.java_version.as_ref().map_or(FALLBACK_JAVA_COMPONENT, |j| j.component.as_str())
}

fn online_plan(dl: &Downloader, meta: &ureq::Agent, versions: &Path, libraries: &Path, game_version: &str, loader: &Loader) -> Result<Plan> {
    let (vanilla, loaded, runtimes) = std::thread::scope(|s| {
        let runtimes = s.spawn(|| java::index(meta));
        let loaded = s.spawn(|| loader_profile(dl, meta, libraries, game_version, loader));
        let vanilla = vanilla_profile(dl, meta, versions, game_version);
        (vanilla, joined(loaded), joined(runtimes))
    });
    let vanilla = vanilla?;
    let (version, over, installer) = loaded?;
    let mut profile = match over {
        Some(over) => meta::merge(vanilla, over),
        None => vanilla,
    };
    profile.id = version_id(loader, game_version, &version);
    let runtime = java::pick(&runtimes?, component(&profile))?;
    if id_prefix(loader).is_some() {
        let path = join_safe(versions, &profile.id)?.join(format!("{}.json", profile.id));
        net::write_atomic(&path, &serde_json::to_vec(&profile)?)?;
    }
    Ok(Plan { profile, loader: version, runtime: Some(runtime), installer })
}

fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, Result<T>>) -> Result<T> {
    handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("the install metadata check stopped unexpectedly")))
}

fn vanilla_profile(dl: &Downloader, meta: &ureq::Agent, versions: &Path, game_version: &str) -> Result<VersionJson> {
    let manifest: Manifest = net::get_json(meta, MANIFEST_URL)?;
    let entry = manifest.versions.iter().find(|v| v.id == game_version).with_context(|| format!("unknown Minecraft version {game_version}"))?;
    let vanilla_path = join_safe(versions, game_version)?.join(format!("{game_version}.json"));
    dl.one(&Job::new(entry.url.clone(), vanilla_path.clone(), Some(entry.sha1.clone()), None))?;
    read_json(&vanilla_path)
}

fn loader_profile(dl: &Downloader, meta: &ureq::Agent, libraries: &Path, game_version: &str, loader: &Loader) -> Result<(String, Option<VersionJson>, Option<forge::Installer>)> {
    let pinned = loader.version().map(str::to_owned);
    Ok(match loader {
        Loader::Vanilla => (String::new(), None, None),
        Loader::Fabric(_) => {
            let version = pinned.map_or_else(|| fabric::latest_loader(meta, game_version), Ok)?;
            let profile = fabric::profile(meta, game_version, &version)?;
            (version, Some(profile), None)
        }
        Loader::Quilt(_) => {
            let version = pinned.map_or_else(|| quilt::latest_loader(meta, game_version), Ok)?;
            let profile = quilt::profile(meta, game_version, &version)?;
            (version, Some(profile), None)
        }
        Loader::Forge(_) | Loader::NeoForge(_) => {
            let flavor = flavor(loader).context("not an FML loader")?;
            let version = match pinned {
                Some(v) => v,
                None => forge::versions(meta, flavor, game_version)?.default.with_context(|| format!("{} does not support Minecraft {game_version}", loader.name()))?,
            };
            let installer = forge::open(&forge::fetch(dl, meta, libraries, flavor, game_version, &version)?)?.for_game(game_version)?;
            (version, Some(installer.version.clone()), Some(installer))
        }
    })
}

fn local_plan(versions: &Path, libraries: &Path, game_version: &str, loader: &Loader) -> Option<Plan> {
    let Some(prefix) = id_prefix(loader) else {
        let json = join_safe(versions, game_version).ok()?.join(format!("{game_version}.json"));
        return Some(Plan { profile: read_json(&json).ok()?, loader: String::new(), runtime: None, installer: None });
    };
    let suffix = format!("-{game_version}");
    let wanted = loader.version().map(|l| format!("{prefix}{l}{suffix}"));
    let found = std::fs::read_dir(versions)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let matches = match &wanted {
                Some(wanted) => *wanted == name,
                None => name.starts_with(prefix) && name.ends_with(&suffix),
            };
            let json = e.path().join(format!("{name}.json"));
            let modified = json.metadata().and_then(|m| m.modified()).ok()?;
            matches.then_some((modified, name, json))
        })
        .max_by_key(|(modified, _, _)| *modified);
    let (_, name, json) = found?;
    let version = name.strip_prefix(prefix).and_then(|n| n.strip_suffix(&suffix)).unwrap_or_default().to_owned();
    let installer = match flavor(loader) {
        Some(flavor) => Some(forge::open(&forge::installer_path(libraries, flavor, game_version, &version).ok()?).ok()?.for_game(game_version).ok()?),
        None => None,
    };
    Some(Plan { profile: read_json(&json).ok()?, loader: version, runtime: None, installer })
}

/// Folder names of installed versions an instance with `loader` on `game` may use: its pinned version, or every
/// version of the loader for the game when it follows the newest.
pub(crate) fn candidates<'a>(installed: &'a [String], loader: &Loader, game: &str) -> Vec<&'a String> {
    let Some(prefix) = id_prefix(loader) else { return installed.iter().filter(|n| *n == game).collect() };
    let suffix = format!("-{game}");
    match loader.version() {
        Some(v) => {
            let wanted = version_id(loader, game, v);
            installed.iter().filter(|n| **n == wanted).collect()
        }
        None => installed.iter().filter(|n| n.starts_with(prefix) && n.ends_with(&suffix)).collect(),
    }
}

/// Library files an installed version needs beyond its version JSON, relative to the libraries folder.
pub(crate) fn extra_files(version_dir: &Path) -> Vec<String> {
    forge::kept(version_dir)
}

const LOCK_FILE: &str = ".install.lock";

fn lock(dir: &Path) -> Result<File> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("cannot open {}", path.display()))?;
    file.lock().with_context(|| format!("cannot lock {}", path.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_loader_is_newer_by_its_dotted_numbers() {
        use super::is_newer;
        assert!(is_newer("0.18.1", "0.17.2"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("0.17.10", "0.17.9"));
        assert!(!is_newer("0.17.2", "0.17.2"));
        assert!(!is_newer("0.17.2", "0.18.0-beta.1"));
        assert!(!is_newer("latest", "0.17.2"));
        assert!(!is_newer("0.17.2", ""));
    }

    use super::*;

    fn write_version(versions: &Path, id: &str, age_secs: u64) {
        let json = versions.join(id).join(format!("{id}.json"));
        std::fs::create_dir_all(json.parent().unwrap()).unwrap();
        std::fs::write(&json, format!(r#"{{"id":"{id}"}}"#)).unwrap();
        let time = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        File::options().write(true).open(&json).unwrap().set_modified(time).unwrap();
    }

    fn fabric(v: Option<&str>) -> Loader {
        Loader::Fabric(v.map(str::to_owned))
    }

    fn cached_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let jar = root.join("versions/26.3/26.3.jar");
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, b"jar").unwrap();
        let index = root.join("assets/indexes/t.json");
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        std::fs::write(&index, br#"{"objects":{}}"#).unwrap();
        let (jar_sha, index_sha) = (net::file_sha1(&jar).unwrap(), net::file_sha1(&index).unwrap());
        let id = "fabric-loader-0.20.1-26.3";
        let profile = root.join("versions").join(id).join(format!("{id}.json"));
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, format!(
            r#"{{"id":"{id}","downloads":{{"client":{{"url":"https://example.invalid/c.jar","sha1":"{jar_sha}","size":3}}}},"assetIndex":{{"id":"t","url":"https://example.invalid/t.json","sha1":"{index_sha}","size":14}},"javaVersion":{{"component":"comp"}}}}"#
        )).unwrap();
        let runtime = root.join("runtimes/comp/abc");
        let java = runtime.join(java::executable(meta::Os::current()));
        std::fs::create_dir_all(java.parent().unwrap()).unwrap();
        std::fs::write(&java, b"").unwrap();
        std::fs::write(runtime.join(".complete"), b"").unwrap();
        dir
    }

    fn offline_install(root: &Path, offline: bool, loader: Option<&str>) -> (Result<Installed>, usize, usize) {
        let (agent, downloads) = net::fake::agent(None, Duration::from_secs(5));
        let (quick, metadata) = net::fake::agent(None, Duration::from_secs(5));
        let dl = Downloader::new(agent).unwrap();
        let result = install_in(root, &dl, &quick, offline, "26.3", &fabric(loader), &|_| {});
        (result, downloads.count(), metadata.count())
    }

    #[test]
    fn the_release_list_is_kept_and_comes_back_without_a_connection() {
        let manifest = || -> Result<Manifest> { Ok(serde_json::from_str(r#"{"latest":{"release":"26.3"},"versions":[{"id":"26.3","type":"release","url":"u","sha1":"s"},{"id":"26.4-pre1","type":"snapshot","url":"u","sha1":"s"}]}"#)?) };
        let (none, lookups) = net::fake::agent(None, Duration::from_secs(5));
        let unreachable = || net::get_json::<Manifest>(&none, "http://piston-meta.test/manifest.json");
        let cache = tempfile::tempdir().unwrap();
        assert!(net::no_connection(&known(cache.path(), unreachable, false).err().unwrap()));
        let fresh = known(cache.path(), manifest, false).unwrap();
        assert_eq!((fresh.latest.as_str(), fresh.releases.as_slice()), ("26.3", ["26.3".to_owned()].as_slice()));
        assert_eq!(known(cache.path(), unreachable, false).unwrap().releases, ["26.3"]);
        assert_eq!(lookups.count(), 2);
        assert_eq!(known(cache.path(), || panic!("asked the network while offline"), true).unwrap().latest, "26.3");
        let broken = || -> Result<Manifest> { anyhow::bail!("unexpected response") };
        assert!(known(cache.path(), broken, false).is_err());
    }

    #[test]
    fn a_cached_version_starts_offline_without_a_single_request() {
        let root = cached_root();
        for loader in [Some("0.20.1"), None] {
            let (installed, downloads, metadata) = offline_install(root.path(), true, loader);
            let installed = installed.unwrap();
            assert_eq!((downloads, metadata), (0, 0));
            assert_eq!((installed.loader_version.as_str(), installed.downloaded), ("0.20.1", 0));
            assert!(installed.java.ends_with(java::executable(meta::Os::current())));
        }
    }

    #[test]
    fn a_cached_version_falls_back_after_one_round_of_quick_failed_requests() {
        let root = cached_root();
        let (installed, downloads, metadata) = offline_install(root.path(), false, Some("0.20.1"));
        assert_eq!(installed.unwrap().loader_version, "0.20.1");
        assert_eq!((downloads, metadata), (0, 3));
    }

    #[test]
    fn a_missing_version_without_network_is_a_connection_error() {
        let root = tempfile::tempdir().unwrap();
        let (installed, _, _) = offline_install(root.path(), true, Some("0.20.1"));
        let e = installed.unwrap_err();
        assert!(net::no_connection(&e), "{e:#}");
    }

    #[test]
    fn a_cached_version_with_a_missing_runtime_asks_the_network() {
        let root = cached_root();
        std::fs::remove_file(root.path().join("runtimes/comp/abc/.complete")).unwrap();
        let (installed, downloads, metadata) = offline_install(root.path(), true, Some("0.20.1"));
        assert!(net::no_connection(&installed.unwrap_err()));
        assert_eq!((downloads, metadata), (3, 0));
    }

    #[test]
    fn local_plan_picks_the_newest_loader_for_the_game() {
        let dir = tempfile::tempdir().unwrap();
        write_version(dir.path(), "fabric-loader-0.19.0-26.3", 100);
        write_version(dir.path(), "fabric-loader-0.20.1-26.3", 10);
        write_version(dir.path(), "fabric-loader-0.20.1-26.2", 1);
        write_version(dir.path(), "quilt-loader-0.30.0-26.3", 1);
        write_version(dir.path(), "26.3", 1);
        let plan = local_plan(dir.path(), dir.path(), "26.3", &fabric(None)).unwrap();
        assert_eq!((plan.loader.as_str(), plan.profile.id.as_str()), ("0.20.1", "fabric-loader-0.20.1-26.3"));
        assert!(plan.runtime.is_none() && plan.installer.is_none());
        assert_eq!(local_plan(dir.path(), dir.path(), "26.3", &Loader::Quilt(None)).unwrap().loader, "0.30.0");
        assert_eq!(local_plan(dir.path(), dir.path(), "26.3", &Loader::Vanilla).unwrap().profile.id, "26.3");
    }

    #[test]
    fn local_plan_honours_an_explicit_loader() {
        let dir = tempfile::tempdir().unwrap();
        write_version(dir.path(), "fabric-loader-0.19.0-26.3", 100);
        write_version(dir.path(), "fabric-loader-0.20.1-26.3", 10);
        assert_eq!(local_plan(dir.path(), dir.path(), "26.3", &fabric(Some("0.19.0"))).unwrap().loader, "0.19.0");
        assert!(local_plan(dir.path(), dir.path(), "26.3", &fabric(Some("9.9.9"))).is_none());
    }

    #[test]
    fn local_plan_fails_when_nothing_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(local_plan(dir.path(), dir.path(), "26.3", &fabric(None)).is_none());
        write_version(dir.path(), "fabric-loader-0.20.1-26.3", 1);
        assert!(local_plan(dir.path(), dir.path(), "26.4", &fabric(None)).is_none());
    }

    #[test]
    fn local_forge_plan_needs_its_cached_installer() {
        let dir = tempfile::tempdir().unwrap();
        write_version(dir.path(), "neoforge-26.3.0.103-26.3", 1);
        assert!(local_plan(dir.path(), dir.path(), "26.3", &Loader::NeoForge(None)).is_none());
    }

    #[test]
    fn version_ids_name_the_loader_and_the_game() {
        assert_eq!(version_id(&fabric(None), "26.3", "0.20.1"), "fabric-loader-0.20.1-26.3");
        assert_eq!(version_id(&Loader::Quilt(None), "26.3", "0.30.0"), "quilt-loader-0.30.0-26.3");
        assert_eq!(version_id(&Loader::Forge(None), "1.20.1", "47.4.28"), "forge-47.4.28-1.20.1");
        assert_eq!(version_id(&Loader::NeoForge(None), "26.3", "26.3.0.103"), "neoforge-26.3.0.103-26.3");
        assert_eq!(version_id(&Loader::Vanilla, "26.3", ""), "26.3");
    }

    #[test]
    fn candidates_keep_forge_and_neoforge_apart() {
        let installed = ["forge-47.4.28-1.20.1", "neoforge-21.1.77-1.21.1", "forge-52.1.20-1.21.1", "1.21.1"].map(str::to_owned).to_vec();
        assert_eq!(candidates(&installed, &Loader::Forge(None), "1.21.1"), [&installed[2]]);
        assert_eq!(candidates(&installed, &Loader::NeoForge(None), "1.21.1"), [&installed[1]]);
        assert_eq!(candidates(&installed, &Loader::Forge(Some("47.4.28".into())), "1.20.1"), [&installed[0]]);
        assert_eq!(candidates(&installed, &Loader::Vanilla, "1.21.1"), [&installed[3]]);
    }

    #[test]
    fn versions_compare_numerically_with_pre_releases_first() {
        assert_eq!(compare_versions("0.10", "0.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.10-beta.2", "0.10"), Ordering::Less);
        assert_eq!(compare_versions("0.10-beta.10", "0.10-beta.9"), Ordering::Greater);
        assert_eq!(compare_versions("47.4.28", "47.4.3"), Ordering::Greater);
        assert_eq!(compare_versions("26.3.0.103", "26.3.0.99"), Ordering::Greater);
        assert_eq!(compare_versions("1.0+build.2", "1.0+build.10"), Ordering::Less);
        assert_eq!(newest_first(vec!["1".into(), "3".into(), "2".into(), "3".into()]), ["3", "2", "1"]);
    }

    #[test]
    fn releases_keep_only_release_ids_in_manifest_order() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"latest":{"release":"26.3"},"versions":[
                {"id":"26.4-snapshot-1","type":"snapshot","url":"u","sha1":"s"},
                {"id":"26.3","type":"release","url":"u","sha1":"s"},
                {"id":"b1.7.3","type":"old_beta","url":"u","sha1":"s"},
                {"id":"26.2","type":"release","url":"u","sha1":"s"}]}"#,
        )
        .unwrap();
        assert_eq!(release_ids(manifest), ["26.3", "26.2"]);
    }

    #[test]
    fn install_lock_is_exclusive_between_holders() {
        let dir = tempfile::tempdir().unwrap();
        let first = lock(dir.path()).unwrap();
        let second = OpenOptions::new().write(true).open(dir.path().join(".install.lock")).unwrap();
        assert!(second.try_lock().is_err());
        drop(first);
        assert!(second.try_lock().is_ok());
    }
}
