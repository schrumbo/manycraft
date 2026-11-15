//! Installs a game version with Fabric: version metadata, libraries, assets, natives and the Java runtime.

mod assets;
pub mod clean;
mod download;
mod fabric;
mod java;
pub mod meta;
mod natives;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use self::download::{Downloader, Job, join_safe, read_json};
use self::meta::{Env, VersionJson};
use super::instance::Loader;
use super::{net, paths};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const FALLBACK_JAVA_COMPONENT: &str = "jre-legacy";
const LOADER_PREFIX: &str = "fabric-loader-";

/// What the installer is working on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Metadata,
    Client,
    Libraries,
    Natives,
    Assets,
    Runtime,
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

/// A game version with Fabric, ready to launch.
#[derive(Clone, Debug)]
pub struct Installed {
    pub game_version: String,
    pub loader_version: String,
    pub id: String,
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
}

/// Newest release version Mojang lists.
pub fn latest_release() -> Result<String> {
    let manifest: Manifest = net::get_json(&net::agent(None)?, MANIFEST_URL)?;
    Ok(manifest.latest.release)
}

/// Release versions Mojang lists, newest first.
pub fn releases() -> Result<Vec<String>> {
    let manifest: Manifest = net::get_json(&net::agent(None)?, MANIFEST_URL)?;
    Ok(release_ids(manifest))
}

fn release_ids(manifest: Manifest) -> Vec<String> {
    manifest.versions.into_iter().filter(|v| v.kind == "release").map(|v| v.id).collect()
}

/// Newest stable Fabric loader for `game_version`.
pub fn latest_loader(game_version: &str) -> Result<String> {
    fabric::latest_loader(&net::agent(None)?, game_version)
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

/// Installs `game_version` with Fabric (`loader_version`, or the newest stable loader) into the shared folders.
/// Files that are already there are not downloaded again. Without a connection, a version that was installed
/// before is used as it is.
pub fn install(game_version: &str, loader_version: Option<&str>, progress: &(dyn Fn(Progress) + Sync)) -> Result<Installed> {
    let _in_use = clean::in_use()?;
    let root = paths::data_dir();
    let (versions, libraries, assets_dir, runtimes) = (root.join("versions"), root.join("libraries"), root.join("assets"), root.join("runtimes"));
    let dl = Downloader::new()?;
    progress(Progress { stage: Stage::Metadata, done: 0, total: 1 });
    let plan = match online_plan(&dl, &versions, game_version, loader_version) {
        Ok(plan) => plan,
        Err(online) => local_plan(&versions, game_version, loader_version).map_err(|_| online)?,
    };
    progress(Progress { stage: Stage::Metadata, done: 1, total: 1 });
    let Plan { profile, loader, runtime } = plan;
    let id = profile.id.clone();
    let version_dir = join_safe(&versions, &id)?;
    let _lock = lock(&version_dir)?;

    let client = profile.downloads.as_ref().and_then(|d| d.client.as_ref()).context("version has no client download")?;
    let client_jar = join_safe(&versions, game_version)?.join(format!("{game_version}.jar"));
    dl.all(Stage::Client, &[Job::new(client.url.clone(), client_jar.clone(), client.sha1.clone(), client.size)], progress)?;

    let resolved = meta::resolve(&profile.libraries, &Env::current(Vec::new()));
    let mut deps = resolved.classpath.clone();
    deps.extend(resolved.natives.iter().map(|n| n.dep.clone()).filter(|d| !resolved.classpath.contains(d)));
    let mut jobs = Vec::with_capacity(deps.len());
    for dep in &deps {
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

    let component = profile.java_version.as_ref().map_or(FALLBACK_JAVA_COMPONENT, |j| j.component.as_str());
    let java = match &runtime {
        Some(runtime) => java::ensure(&dl, &runtimes, runtime, progress)?,
        None => java::local(&runtimes, component).with_context(|| format!("Java runtime {component} is not installed and there is no connection"))?,
    };

    let mut classpath = Vec::with_capacity(resolved.classpath.len() + 1);
    for dep in &resolved.classpath {
        classpath.push(join_safe(&libraries, &dep.path)?);
    }
    classpath.push(client_jar);
    Ok(Installed {
        game_version: game_version.to_owned(),
        loader_version: loader,
        id,
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

fn online_plan(dl: &Downloader, versions: &Path, game_version: &str, loader_version: Option<&str>) -> Result<Plan> {
    let manifest: Manifest = net::get_json(dl.agent(), MANIFEST_URL)?;
    let entry = manifest.versions.iter().find(|v| v.id == game_version).with_context(|| format!("unknown Minecraft version {game_version}"))?;
    let vanilla_path = join_safe(versions, game_version)?.join(format!("{game_version}.json"));
    dl.one(&Job::new(entry.url.clone(), vanilla_path.clone(), Some(entry.sha1.clone()), None))?;
    let vanilla: VersionJson = read_json(&vanilla_path)?;
    let loader = match loader_version {
        Some(v) => v.to_owned(),
        None => fabric::latest_loader(dl.agent(), game_version)?,
    };
    let profile = meta::merge(vanilla, fabric::profile(dl.agent(), game_version, &loader)?);
    let component = profile.java_version.as_ref().map_or(FALLBACK_JAVA_COMPONENT, |j| j.component.as_str());
    let runtime = java::lookup(dl, component)?;
    let path = join_safe(versions, &profile.id)?.join(format!("{}.json", profile.id));
    net::write_atomic(&path, &serde_json::to_vec(&profile)?)?;
    Ok(Plan { profile, loader, runtime: Some(runtime) })
}

fn local_plan(versions: &Path, game_version: &str, loader_version: Option<&str>) -> Result<Plan> {
    let suffix = format!("-{game_version}");
    let wanted = loader_version.map(|l| format!("{LOADER_PREFIX}{l}{suffix}"));
    let found = std::fs::read_dir(versions)
        .with_context(|| format!("cannot read {}", versions.display()))?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let matches = match &wanted {
                Some(wanted) => *wanted == name,
                None => name.starts_with(LOADER_PREFIX) && name.ends_with(&suffix),
            };
            let json = e.path().join(format!("{name}.json"));
            let modified = json.metadata().and_then(|m| m.modified()).ok()?;
            matches.then_some((modified, name, json))
        })
        .max_by_key(|(modified, _, _)| *modified);
    let (_, name, json) = found.with_context(|| format!("Minecraft {game_version} with Fabric is not installed"))?;
    let loader = name.strip_prefix(LOADER_PREFIX).and_then(|n| n.strip_suffix(&suffix)).unwrap_or_default().to_owned();
    Ok(Plan { profile: read_json(&json)?, loader, runtime: None })
}

fn lock(dir: &Path) -> Result<File> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(".install.lock");
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

    #[test]
    fn local_plan_picks_the_newest_loader_for_the_game() {
        let dir = tempfile::tempdir().unwrap();
        write_version(dir.path(), "fabric-loader-0.19.0-26.3", 100);
        write_version(dir.path(), "fabric-loader-0.20.1-26.3", 10);
        write_version(dir.path(), "fabric-loader-0.20.1-26.2", 1);
        write_version(dir.path(), "26.3", 1);
        let plan = local_plan(dir.path(), "26.3", None).unwrap();
        assert_eq!((plan.loader.as_str(), plan.profile.id.as_str()), ("0.20.1", "fabric-loader-0.20.1-26.3"));
        assert!(plan.runtime.is_none());
    }

    #[test]
    fn local_plan_honours_an_explicit_loader() {
        let dir = tempfile::tempdir().unwrap();
        write_version(dir.path(), "fabric-loader-0.19.0-26.3", 100);
        write_version(dir.path(), "fabric-loader-0.20.1-26.3", 10);
        assert_eq!(local_plan(dir.path(), "26.3", Some("0.19.0")).unwrap().loader, "0.19.0");
        assert!(local_plan(dir.path(), "26.3", Some("9.9.9")).is_err());
    }

    #[test]
    fn local_plan_fails_when_nothing_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(local_plan(dir.path(), "26.3", None).is_err());
        write_version(dir.path(), "fabric-loader-0.20.1-26.3", 1);
        assert!(local_plan(dir.path(), "26.4", None).is_err());
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
