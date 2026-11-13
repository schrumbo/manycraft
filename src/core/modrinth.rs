//! Mods from Modrinth: search, version resolution with required dependencies, verified downloads into a batch.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Deserialize;

use super::batch::{Batch, ModRef};
use super::{install, net};

const API: &str = "https://api.modrinth.com/v2";
const LOADER: &str = "fabric";
const DEFAULTS: [&str; 5] = ["fabric-api", "lithium", "ferrite-core", "modernfix", "krypton"];
/// Modrinth project id of Fabric API, which the companion mod needs in every batch.
pub const FABRIC_API: &str = "P7dR8mSH";
const POOL_THREADS: usize = 8;
const SEARCH_LIMIT: u32 = 20;
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
}

/// The default mods for a game version, and why any of them is missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defaults {
    pub mods: Vec<ModRef>,
    pub notes: Vec<String>,
}

/// A newer file for a mod of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub current: ModRef,
    pub latest: ModRef,
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
}

#[derive(Deserialize)]
struct Project {
    id: String,
    title: String,
}

#[derive(Clone, Deserialize)]
struct Version {
    id: String,
    project_id: String,
    #[serde(default)]
    version_number: String,
    version_type: String,
    date_published: String,
    game_versions: Vec<String>,
    loaders: Vec<String>,
    files: Vec<File>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Clone, Deserialize)]
struct File {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    hashes: Hashes,
}

#[derive(Clone, Deserialize)]
struct Hashes {
    sha1: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Dependency {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    version_id: Option<String>,
    dependency_type: String,
}

trait Source: Sync {
    fn search(&self, query: &str, game_version: &str) -> Result<Vec<Hit>>;
    fn project(&self, key: &str) -> Result<Option<Project>>;
    fn versions(&self, project_id: &str, game_version: &str) -> Result<Vec<Version>>;
    fn version(&self, id: &str) -> Result<Version>;
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
    fn search(&self, query: &str, game_version: &str) -> Result<Vec<Hit>> {
        Ok(net::get_json::<SearchResponse>(&self.agent, &search_url(query, game_version))?.hits)
    }

    fn project(&self, key: &str) -> Result<Option<Project>> {
        match net::get_json(&self.agent, &format!("{API}/project/{}", encode(key))) {
            Ok(project) => Ok(Some(project)),
            Err(e) if matches!(e.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(404))) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn versions(&self, project_id: &str, game_version: &str) -> Result<Vec<Version>> {
        net::get_json(&self.agent, &versions_url(project_id, game_version))
    }

    fn version(&self, id: &str) -> Result<Version> {
        net::get_json(&self.agent, &format!("{API}/version/{}", encode(id)))
    }
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

fn search_url(query: &str, game_version: &str) -> String {
    let facets = serde_json::json!([
        ["project_type:mod"],
        [format!("categories:{LOADER}")],
        [format!("versions:{game_version}")],
        ["client_side:required", "client_side:optional"],
    ]);
    format!("{API}/search?query={}&facets={}&limit={SEARCH_LIMIT}", encode(query), encode(&facets.to_string()))
}

fn versions_url(project_id: &str, game_version: &str) -> String {
    format!(
        "{API}/project/{}/version?loaders={}&game_versions={}",
        encode(project_id),
        encode(&serde_json::json!([LOADER]).to_string()),
        encode(&serde_json::json!([game_version]).to_string()),
    )
}

fn rank(version_type: &str) -> u8 {
    match version_type {
        "release" => 2,
        "beta" => 1,
        _ => 0,
    }
}

fn newest<'a>(versions: &'a [Version], game_version: &str) -> Option<&'a Version> {
    versions
        .iter()
        .filter(|v| v.loaders.iter().any(|l| l == LOADER) && v.game_versions.iter().any(|g| g == game_version))
        .max_by(|a, b| rank(&a.version_type).cmp(&rank(&b.version_type)).then_with(|| a.date_published.cmp(&b.date_published)))
}

fn mod_ref(title: &str, version: &Version) -> Result<ModRef> {
    let file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or(version.files.first())
        .with_context(|| format!("{title} {} has no file", version.version_number))?;
    Ok(ModRef {
        project_id: version.project_id.clone(),
        version_id: version.id.clone(),
        title: title.to_owned(),
        file_name: file.filename.clone(),
        url: file.url.clone(),
        sha1: file.hashes.sha1.clone(),
    })
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

fn lookup(src: &dyn Source, key: &str, game_version: &str) -> Result<Entry> {
    let project = src.project(key)?.ok_or_else(|| Unavailable(format!("{key} is not on Modrinth")))?;
    let versions = src.versions(&project.id, game_version)?;
    let version = newest(&versions, game_version).ok_or_else(|| Unavailable(format!("{} has no Fabric version for {game_version}", project.title)))?;
    Ok(Entry {
        found: mod_ref(&project.title, version)?,
        required: dependencies_of(version, "required").cloned().collect(),
        incompatible: dependencies_of(version, "incompatible").filter_map(|d| d.project_id.clone()).collect(),
    })
}

fn expand(src: &dyn Source, entries: &mut Vec<Entry>, seen: &mut HashSet<String>, game_version: &str) -> Result<()> {
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
            let entry = lookup(src, &project_id, game_version).with_context(|| format!("required by {parent}"))?;
            seen.insert(entry.found.project_id.clone());
            entries.push(entry);
        }
        i += 1;
    }
    Ok(())
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

fn resolve_all<S: AsRef<str>>(src: &dyn Source, roots: &[S], game_version: &str) -> Result<(Vec<Entry>, usize)> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let entry = lookup(src, root.as_ref(), game_version)?;
        if seen.insert(entry.found.project_id.clone()) {
            entries.push(entry);
        }
    }
    let requested = entries.len();
    expand(src, &mut entries, &mut seen, game_version)?;
    check_compatible(&entries)?;
    Ok((entries, requested))
}

fn defaults_from(src: &dyn Source, game_version: &str) -> Result<Defaults> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = HashSet::new();
    let mut notes = Vec::new();
    for slug in DEFAULTS {
        match resolve_all(src, &[slug], game_version) {
            Ok((found, _)) => {
                for entry in found {
                    if seen.insert(entry.found.project_id.clone()) {
                        entries.push(entry);
                    }
                }
            }
            Err(e) if is_unavailable(&e) => notes.push(format!("skipped {slug}: {e:#}")),
            Err(e) => return Err(e),
        }
    }
    check_compatible(&entries)?;
    Ok(Defaults { mods: entries.into_iter().map(|e| e.found).collect(), notes })
}

fn updates_from(src: &dyn Source, game_version: &str, mods: &[ModRef]) -> Result<Vec<Update>> {
    let check = |current: &ModRef| -> Result<Option<Update>> {
        let versions = src.versions(&current.project_id, game_version)?;
        let Some(latest) = newest(&versions, game_version) else {
            return Ok(None);
        };
        if latest.id == current.version_id {
            return Ok(None);
        }
        let published = versions.iter().find(|v| v.id == current.version_id).map(|v| v.date_published.as_str());
        if published.is_some_and(|date| latest.date_published.as_str() <= date) {
            return Ok(None);
        }
        Ok(Some(Update { current: current.clone(), latest: mod_ref(&current.title, latest)? }))
    };
    let found = pool()?.install(|| mods.par_iter().map(check).collect::<Result<Vec<_>>>())?;
    Ok(found.into_iter().flatten().collect())
}

fn pool() -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new().num_threads(POOL_THREADS).build().context("cannot start download threads")
}

fn safe_file_name(name: &str) -> Result<()> {
    if Path::new(name).file_name().and_then(std::ffi::OsStr::to_str) != Some(name) {
        bail!("unsafe mod file name {name:?}");
    }
    Ok(())
}

fn sync_into(agent: &ureq::Agent, mods: &[ModRef], dir: &Path) -> Result<usize> {
    let mut keep = HashSet::new();
    let mut jobs = Vec::new();
    for m in mods {
        safe_file_name(&m.file_name)?;
        if keep.insert(m.file_name.to_lowercase()) {
            jobs.push(m);
        }
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let fetched = pool()?.install(|| {
        jobs.par_iter()
            .map(|m| -> Result<bool> {
                let path = dir.join(&m.file_name);
                let current = path.is_file() && net::file_sha1(&path)?.eq_ignore_ascii_case(&m.sha1);
                if !current {
                    net::fetch_verified(agent, &m.url, &path, Some(&m.sha1)).with_context(|| format!("cannot download {}", m.title))?;
                }
                Ok(!current)
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_lowercase) else {
            continue;
        };
        if name.ends_with(".jar") && !keep.contains(&name) && name != super::fleet::COMPANION_FILE && entry.file_type()?.is_file() {
            std::fs::remove_file(entry.path()).with_context(|| format!("cannot delete {}", entry.path().display()))?;
        }
    }
    Ok(fetched.into_iter().filter(|downloaded| *downloaded).count())
}

/// Fabric client mods matching `query` that have a version for `game_version`, most relevant first.
pub fn search(query: &str, game_version: &str) -> Result<Vec<Hit>> {
    Api::new()?.search(query, game_version)
}

/// The newest Fabric version of a mod (slug or project id) for `game_version`, releases before betas before alphas.
pub fn resolve(slug_or_id: &str, game_version: &str) -> Result<ModRef> {
    Ok(lookup(&Api::new()?, slug_or_id, game_version)?.found)
}

/// Resolves every mod in `list` and, recursively, its required dependencies, each project once.
/// Listed mods come first. Two incompatible mods are an error naming both.
pub fn resolve_with_dependencies<S: AsRef<str>>(list: &[S], game_version: &str) -> Result<Vec<ModRef>> {
    let (entries, _) = resolve_all(&Api::new()?, list, game_version)?;
    Ok(entries.into_iter().map(|e| e.found).collect())
}

/// Fabric API, Lithium, FerriteCore, ModernFix and Krypton with their dependencies.
/// A default without a version for `game_version` is skipped and explained in `notes`.
pub fn default_mods(game_version: &str) -> Result<Defaults> {
    defaults_from(&Api::new()?, game_version)
}

/// Makes the batch's mod folder hold exactly its mods: downloads missing or changed files (SHA-1 checked),
/// then deletes `.jar` files directly in the folder that are not in the list. Returns how many were downloaded.
/// Fabric API is added for the sync when the batch does not list it, since the companion mod depends on it.
pub fn sync_mods(batch: &Batch) -> Result<usize> {
    let mut mods = batch.mods.clone();
    if !mods.iter().any(|m| m.project_id == FABRIC_API) {
        mods.push(resolve(FABRIC_API, &batch.game_version).context("the companion mod needs Fabric API, which could not be found")?);
    }
    sync_into(&net::agent(None)?, &mods, &batch.mods_dir())
}

/// Mods of the batch that have a newer file for the batch's game version.
pub fn updates(batch: &Batch) -> Result<Vec<Update>> {
    updates_from(&Api::new()?, &batch.game_version, &batch.mods)
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
        let hits = search(&words.join(" "), &version)?;
        if hits.is_empty() {
            println!("no Fabric mods for {version} match");
        }
        for hit in hits {
            println!("{} ({})  {} downloads\n  {}", hit.title, hit.slug, hit.downloads, hit.description);
        }
    } else {
        let (entries, requested) = resolve_all(&Api::new()?, &words, &version)?;
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("testdata/modrinth/", $name, ".json"))
        };
    }

    struct Fixtures {
        projects: Vec<(String, String, Project)>,
        versions: Vec<Version>,
    }

    impl Fixtures {
        fn recorded() -> Self {
            let mut fx = Self { projects: Vec::new(), versions: Vec::new() };
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
            let mut fx = Self { projects: Vec::new(), versions: Vec::new() };
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
        fn search(&self, _query: &str, _game_version: &str) -> Result<Vec<Hit>> {
            Ok(serde_json::from_str::<SearchResponse>(fixture!("search_sodium"))?.hits)
        }

        fn project(&self, key: &str) -> Result<Option<Project>> {
            let found = self.projects.iter().find(|(slug, id, _)| slug.eq_ignore_ascii_case(key) || id == key);
            Ok(found.map(|(_, id, p)| Project { id: id.clone(), title: p.title.clone() }))
        }

        fn versions(&self, project_id: &str, _game_version: &str) -> Result<Vec<Version>> {
            Ok(self.versions.iter().filter(|v| v.project_id == project_id).cloned().collect())
        }

        fn version(&self, id: &str) -> Result<Version> {
            self.versions.iter().find(|v| v.id == id).cloned().context("unknown version")
        }
    }

    fn titles(mods: &[ModRef]) -> Vec<&str> {
        mods.iter().map(|m| m.title.as_str()).collect()
    }

    fn resolved<S: AsRef<str>>(fx: &Fixtures, list: &[S]) -> Result<Vec<ModRef>> {
        Ok(resolve_all(fx, list, "26.3")?.0.into_iter().map(|e| e.found).collect())
    }

    #[test]
    fn release_is_preferred_over_newer_betas() {
        let fx = Fixtures::recorded();
        let sodium = lookup(&fx, "sodium", "26.3").unwrap().found;
        assert_eq!(sodium.version_id, "bAZQdGpg");
        assert_eq!(sodium.title, "Sodium");
        assert!(sodium.url.starts_with("https://cdn.modrinth.com/") && sodium.file_name.ends_with(".jar"));
        assert_eq!(sodium.sha1.len(), 40);
    }

    #[test]
    fn loader_and_game_version_are_filtered() {
        let fx = Fixtures::recorded();
        assert_eq!(lookup(&fx, "lithium", "26.3").unwrap().found.version_id, "fWv5bnZk");
        let versions = fx.versions("gvQqBUqZ", "26.3").unwrap();
        assert_eq!(versions.len(), 6);
        assert!(newest(&versions, "1.21").is_none());
        assert!(newest(&versions[..1], "26.3").is_none());
        let err = lookup(&fx, "lithium", "1.21").unwrap_err();
        assert!(is_unavailable(&err) && err.to_string().contains("Lithium"), "{err:#}");
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
        assert_eq!(newest(&mixed, "26.3").unwrap().id, "a");
        let alphas = versions(&[("a", "alpha"), ("b", "alpha")]);
        assert_eq!(newest(&alphas, "26.3").unwrap().id, "b");
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
        let (entries, requested) = resolve_all(&fx, &["modmenu"], "26.3").unwrap();
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

    #[test]
    fn required_dependency_without_a_version_is_an_error_naming_both() {
        let fx = Fixtures::synthetic(&[("a", &[("id-gone", "required")])]);
        let err = format!("{:#}", resolved(&fx, &["a"]).unwrap_err());
        assert!(err.contains("T-a"), "{err}");
        assert!(resolved(&fx, &["nope"]).is_err_and(|e| is_unavailable(&e)));
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
        let defaults = defaults_from(&fx, "26.3").unwrap();
        assert_eq!(titles(&defaults.mods), ["Fabric API", "Lithium", "FerriteCore", "Krypton"]);
        assert_eq!(defaults.notes.len(), 1);
        assert!(defaults.notes[0].contains("modernfix") && defaults.notes[0].contains("26.3"), "{:?}", defaults.notes);
    }

    #[test]
    fn defaults_skip_a_mod_that_is_not_on_modrinth_and_fail_on_incompatibility() {
        let mut fx = Fixtures::recorded();
        fx.projects.retain(|(slug, _, _)| slug != "krypton");
        let defaults = defaults_from(&fx, "26.3").unwrap();
        assert_eq!(defaults.notes.len(), 2);
        assert_eq!(defaults.mods.len(), 3);
        let fx = Fixtures::synthetic(&[("fabric-api", &[("id-lithium", "incompatible")]), ("lithium", &[])]);
        assert!(defaults_from(&fx, "26.3").unwrap_err().to_string().contains("incompatible"));
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
        let updates = updates_from(&fx, "26.3", &mods).unwrap();
        let pairs: Vec<(&str, &str)> = updates.iter().map(|u| (u.current.version_id.as_str(), u.latest.version_id.as_str())).collect();
        assert_eq!(pairs, [("xS0Q8LSi", "fWv5bnZk")]);
        assert_eq!(updates[0].latest.title, "x");
        let unknown = ModRef { version_id: "gone".into(), ..pinned(&fx, "fWv5bnZk") };
        assert_eq!(updates_from(&fx, "26.3", &[unknown]).unwrap().len(), 1);
    }

    #[test]
    fn search_hits_parse() {
        let fx = Fixtures::recorded();
        let hits = fx.search("sodium", "26.3").unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!((hits[0].slug.as_str(), hits[0].title.as_str(), hits[0].project_id.as_str()), ("sodium", "Sodium", "AANobbMI"));
        assert!(hits[0].downloads > 0 && !hits[0].description.is_empty());
        let none: Hit = serde_json::from_str(r#"{"title":"t","slug":"s","project_id":"p","description":"d","downloads":1,"icon_url":null}"#).unwrap();
        assert_eq!(none.icon_url, None);
    }

    #[test]
    fn urls_are_encoded_and_filter_for_fabric() {
        assert_eq!(encode("a b&é/~-_."), "a%20b%26%C3%A9%2F~-_.");
        let url = search_url("shaders & more", "26.3");
        assert!(url.starts_with("https://api.modrinth.com/v2/search?query=shaders%20%26%20more&facets=%5B%5B"), "{url}");
        assert!(url.contains("categories%3Afabric") && url.contains("versions%3A26.3") && url.contains("client_side%3Arequired"));
        assert_eq!(
            versions_url("lithium", "26.3"),
            "https://api.modrinth.com/v2/project/lithium/version?loaders=%5B%22fabric%22%5D&game_versions=%5B%2226.3%22%5D"
        );
    }

    #[test]
    fn file_names_cannot_escape_the_mod_folder() {
        assert!(safe_file_name("lithium-0.1.jar").is_ok());
        for bad in ["", "..", "../x.jar", "a/b.jar", "a\\b.jar", "C:x.jar", "/x.jar"] {
            assert!(safe_file_name(bad).is_err(), "{bad}");
        }
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
        assert_eq!(sync_into(&agent, &list, &mods).unwrap(), 0);
        let mut left: Vec<String> = std::fs::read_dir(&mods).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["a.jar", "b.jar", "folder.jar", "manual", "notes.txt"]);
        assert!(mods.join("manual").join("mine.jar").is_file());
        assert_eq!(sync_into(&agent, &[], &mods).unwrap(), 0);
        assert!(!mods.join("a.jar").exists() && !mods.join("b.jar").exists());
        assert!(mods.join("folder.jar").is_dir() && mods.join("notes.txt").is_file());
    }

    #[test]
    fn sync_creates_the_folder_and_refuses_unsafe_names() {
        let dir = tempfile::tempdir().unwrap();
        let mods = dir.path().join("new").join("mods");
        let agent = net::agent(None).unwrap();
        assert_eq!(sync_into(&agent, &[], &mods).unwrap(), 0);
        assert!(mods.is_dir());
        let mut evil = jar("x.jar", b"x");
        evil.file_name = "../x.jar".into();
        assert!(sync_into(&agent, &[evil], &mods).is_err());
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
        assert_eq!(sync_into(&agent, &list, dir.path()).unwrap(), 2);
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
        assert!(sync_into(&agent, &[jar("missing.jar", b"m")], dir.path()).is_err());
        assert!(dir.path().join("stray.jar").is_file());
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
