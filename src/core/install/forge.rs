use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::download::{Downloader, Job, join_safe};
use super::meta::{Library, VersionJson, maven_path};
use super::{Progress, Stage, Versions, newest_first};
use crate::core::net;

const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/";
const FORGE_METADATA: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json";
const FORGE_PROMOTIONS: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/";
const NEOFORGE_VERSIONS: &str = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const MARKER: &str = ".processed";
const DATA_DIR: &str = ".installer";
const LOG_FILE: &str = "install.log";
const SIDE: &str = "client";
const LOG_TAIL: usize = 12;

/// Which of the two FML loaders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Forge,
    NeoForge,
}

impl Flavor {
    fn name(self) -> &'static str {
        match self {
            Flavor::Forge => "Forge",
            Flavor::NeoForge => "NeoForge",
        }
    }

    fn installer(self, game: &str, version: &str) -> String {
        match self {
            Flavor::Forge => format!("net.minecraftforge:forge:{game}-{version}:installer"),
            Flavor::NeoForge => format!("net.neoforged:neoforge:{version}:installer"),
        }
    }

    fn maven(self) -> &'static str {
        match self {
            Flavor::Forge => FORGE_MAVEN,
            Flavor::NeoForge => NEOFORGE_MAVEN,
        }
    }
}

#[derive(Deserialize)]
struct Promotions {
    promos: HashMap<String, String>,
}

#[derive(Deserialize)]
struct NeoForgeVersions {
    versions: Vec<String>,
}

/// Versions of `flavor` for `game`, newest first, with the recommended or newest stable one as default.
pub fn versions(agent: &ureq::Agent, flavor: Flavor, game: &str) -> Result<Versions> {
    let found = match flavor {
        Flavor::Forge => {
            let metadata: HashMap<String, Vec<String>> = net::get_json(agent, FORGE_METADATA)?;
            let promotions: Promotions = net::get_json(agent, FORGE_PROMOTIONS)?;
            forge_versions(&metadata, &promotions.promos, game)
        }
        Flavor::NeoForge => {
            let all: NeoForgeVersions = net::get_json(agent, NEOFORGE_VERSIONS)?;
            neoforge_versions(all.versions, game)
        }
    };
    if found.list.is_empty() {
        bail!("{} does not support Minecraft {game}", flavor.name());
    }
    Ok(found)
}

fn forge_versions(metadata: &HashMap<String, Vec<String>>, promos: &HashMap<String, String>, game: &str) -> Versions {
    let prefix = format!("{game}-");
    let list = newest_first(metadata.get(game).into_iter().flatten().filter_map(|v| v.strip_prefix(&prefix)).filter(|v| !v.contains('-')).map(str::to_owned).collect());
    let promoted = [format!("{game}-recommended"), format!("{game}-latest")].into_iter().find_map(|k| promos.get(&k).filter(|v| list.contains(v)).cloned());
    let default = promoted.or_else(|| list.first().cloned());
    Versions { list, default }
}

fn neoforge_versions(all: Vec<String>, game: &str) -> Versions {
    let list = newest_first(all.into_iter().filter(|v| neoforge_game(v).as_deref() == Some(game)).collect());
    let default = list.iter().find(|v| !v.contains('-')).or(list.first()).cloned();
    Versions { list, default }
}

/// The Minecraft version a NeoForge version is for: 21.1.77 is 1.21.1, 26.3.0.103 is 26.3.
pub fn neoforge_game(version: &str) -> Option<String> {
    let release = version.split('-').next()?;
    let parts: Vec<u32> = release.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
    match parts.as_slice() {
        [year, drop, 0, _] if *year >= 25 => Some(format!("{year}.{drop}")),
        [year, drop, patch, _] if *year >= 25 => Some(format!("{year}.{drop}.{patch}")),
        [minor, 0, _] if *minor >= 20 && *minor < 25 => Some(format!("1.{minor}")),
        [minor, patch, _] if *minor >= 20 && *minor < 25 => Some(format!("1.{minor}.{patch}")),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Sided {
    #[serde(default)]
    client: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Processor {
    jar: String,
    #[serde(default)]
    classpath: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    outputs: HashMap<String, String>,
    #[serde(default)]
    sides: Option<Vec<String>>,
}

impl Processor {
    fn for_client(&self) -> bool {
        self.sides.as_ref().is_none_or(|s| s.iter().any(|s| s == SIDE))
    }
}

#[derive(Clone, Debug, Deserialize)]
struct InstallProfile {
    #[serde(default)]
    spec: u32,
    #[serde(default)]
    minecraft: String,
    #[serde(default)]
    json: Option<String>,
    #[serde(default)]
    data: HashMap<String, Sided>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    libraries: Vec<Library>,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    installer: String,
    files: Vec<String>,
}

/// An official Forge or NeoForge installer, opened: its install profile and the version it installs.
pub struct Installer {
    path: PathBuf,
    sha1: String,
    profile: InstallProfile,
    /// The version JSON the installer would write, to be laid onto vanilla.
    pub version: VersionJson,
}

/// Where the processors work.
pub struct Site<'a> {
    pub root: &'a Path,
    pub libraries: &'a Path,
    pub version_dir: &'a Path,
    pub client_jar: &'a Path,
    pub game: &'a str,
    pub java: &'a Path,
}

/// Library path of the installer of `flavor` `version` for `game`.
pub fn installer_path(libraries: &Path, flavor: Flavor, game: &str, version: &str) -> Result<PathBuf> {
    join_safe(libraries, &maven_path(&flavor.installer(game, version)).context("bad installer coordinates")?)
}

/// Downloads the installer into the libraries folder, checked against the SHA-1 its repository publishes.
pub fn fetch(dl: &Downloader, meta: &ureq::Agent, libraries: &Path, flavor: Flavor, game: &str, version: &str) -> Result<PathBuf> {
    let path = installer_path(libraries, flavor, game, version)?;
    let rel = maven_path(&flavor.installer(game, version)).context("bad installer coordinates")?;
    let url = format!("{}{rel}", flavor.maven());
    let sha1 = published_sha1(meta, &format!("{url}.sha1"));
    dl.one(&Job::new(url, path.clone(), sha1, None).strict()).with_context(|| format!("cannot download the {} {version} installer", flavor.name()))?;
    Ok(path)
}

fn published_sha1(agent: &ureq::Agent, url: &str) -> Option<String> {
    let text = agent.get(url).call().ok()?.body_mut().read_to_string().ok()?;
    let sha = text.split_whitespace().next()?.to_ascii_lowercase();
    (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

/// Opens the installer at `path`: reads its install profile and version JSON.
pub fn open(path: &Path) -> Result<Installer> {
    let mut zip = zip::ZipArchive::new(File::open(path).with_context(|| format!("cannot open {}", path.display()))?).with_context(|| format!("{} is not an installer", path.display()))?;
    let profile: InstallProfile = serde_json::from_slice(&entry(&mut zip, "install_profile.json")?).context("the installer's install_profile.json is not valid")?;
    if profile.spec > 1 || profile.processors.is_empty() && profile.data.is_empty() && profile.minecraft.is_empty() {
        bail!("this installer is too old or too new for Manycraft");
    }
    let json = profile.json.as_deref().unwrap_or("/version.json").trim_start_matches('/').to_owned();
    let version: VersionJson = serde_json::from_slice(&entry(&mut zip, &json)?).context("the installer's version.json is not valid")?;
    if version.main_class.is_none() {
        bail!("the installer's version has no main class");
    }
    Ok(Installer { path: path.to_path_buf(), sha1: net::file_sha1(path)?, profile, version })
}

fn entry<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    let mut file = zip.by_name(name).with_context(|| format!("the installer has no {name}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).with_context(|| format!("cannot read {name} from the installer"))?;
    Ok(bytes)
}

impl Installer {
    /// The installer itself, refused when it was made for another game version than `game`.
    pub fn for_game(self, game: &str) -> Result<Self> {
        if !self.profile.minecraft.is_empty() && self.profile.minecraft != game {
            bail!("this loader version is made for Minecraft {}, not {game}; pick another loader version", self.profile.minecraft);
        }
        Ok(self)
    }

    /// Libraries the processors need.
    pub fn libraries(&self) -> &[Library] {
        &self.profile.libraries
    }

    /// Unpacks the artifacts the installer carries under `maven/` into the libraries folder, keeping files that match.
    pub fn unpack_bundled(&self, libraries: &Path) -> Result<()> {
        let mut zip = zip::ZipArchive::new(File::open(&self.path)?)?;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i)?;
            if file.is_dir() {
                continue;
            }
            let Some(path) = file.enclosed_name() else { continue };
            let path = path.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
            let Some(rel) = path.strip_prefix("maven/").filter(|r| !r.is_empty()).map(str::to_owned) else { continue };
            let dest = join_safe(libraries, &rel)?;
            if dest.metadata().is_ok_and(|m| m.len() == file.size()) {
                continue;
            }
            let mut bytes = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
            file.read_to_end(&mut bytes)?;
            net::write_atomic(&dest, &bytes)?;
        }
        Ok(())
    }

    /// Whether the processors already ran for this installer and their results are still there.
    pub fn processed(&self, version_dir: &Path, libraries: &Path) -> bool {
        read_marker(version_dir).is_some_and(|m| m.installer == self.sha1 && m.files.iter().all(|f| join_safe(libraries, f).is_ok_and(|p| p.is_file())))
    }

    /// Runs the client processors with the game's Java, skipping those whose outputs are already right.
    pub fn process(&self, site: &Site, progress: &(dyn Fn(Progress) + Sync)) -> Result<()> {
        let work = site.version_dir.join(DATA_DIR);
        let _ = std::fs::remove_dir_all(&work);
        let data = self.data(site, &work)?;
        let processors: Vec<&Processor> = self.profile.processors.iter().filter(|p| p.for_client()).collect();
        let log_path = site.version_dir.join(LOG_FILE);
        let total = processors.len();
        progress(Progress { stage: Stage::Processors, done: 0, total });
        for (done, processor) in processors.iter().enumerate() {
            run(processor, &data, site, &log_path).with_context(|| format!("installing {} failed at step {} of {total}", self.version.id, done + 1))?;
            progress(Progress { stage: Stage::Processors, done: done + 1, total });
        }
        let _ = std::fs::remove_dir_all(&work);
        let mut files: Vec<String> = self.profile.data.values().filter_map(|s| artifact(&s.client)).filter(|rel| join_safe(site.libraries, rel).is_ok_and(|p| p.is_file())).collect();
        files.extend(self.path.strip_prefix(site.libraries).ok().and_then(|p| p.to_str()).map(|p| p.replace('\\', "/")));
        files.extend(super::meta::resolve(&self.profile.libraries, &super::meta::Env::current(Vec::new())).classpath.into_iter().map(|d| d.path));
        files.sort();
        files.dedup();
        let marker = Marker { installer: self.sha1.clone(), files };
        net::write_atomic(&site.version_dir.join(MARKER), &serde_json::to_vec(&marker)?)
    }

    fn data(&self, site: &Site, work: &Path) -> Result<HashMap<String, String>> {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let mut data = HashMap::from([
            ("SIDE".to_owned(), SIDE.to_owned()),
            ("MINECRAFT_JAR".to_owned(), text(site.client_jar)),
            ("MINECRAFT_VERSION".to_owned(), site.game.to_owned()),
            ("ROOT".to_owned(), text(site.root)),
            ("INSTALLER".to_owned(), text(&self.path)),
            ("LIBRARY_DIR".to_owned(), text(site.libraries)),
        ]);
        let mut zip = None;
        for (key, sided) in &self.profile.data {
            let value = match data_value(&sided.client) {
                DataValue::Artifact(coords) => text(&library(site.libraries, coords)?),
                DataValue::Literal(s) => s.to_owned(),
                DataValue::Bundled(name) => {
                    let zip = match &mut zip {
                        Some(zip) => zip,
                        None => zip.insert(zip::ZipArchive::new(File::open(&self.path)?)?),
                    };
                    let dest = join_safe(work, name)?;
                    net::write_atomic(&dest, &entry(zip, name)?)?;
                    text(&dest)
                }
            };
            data.insert(key.clone(), value);
        }
        Ok(data)
    }
}

/// Library-relative paths the processors of the installed version produced; cleaning keeps them.
pub fn kept(version_dir: &Path) -> Vec<String> {
    read_marker(version_dir).map(|m| m.files).unwrap_or_default()
}

fn read_marker(version_dir: &Path) -> Option<Marker> {
    serde_json::from_slice(&std::fs::read(version_dir.join(MARKER)).ok()?).ok()
}

enum DataValue<'a> {
    Artifact(&'a str),
    Literal(&'a str),
    Bundled(&'a str),
}

fn data_value(value: &str) -> DataValue<'_> {
    if let Some(coords) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        DataValue::Artifact(coords)
    } else if let Some(literal) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        DataValue::Literal(literal)
    } else {
        DataValue::Bundled(value.trim_start_matches('/'))
    }
}

fn artifact(value: &str) -> Option<String> {
    match data_value(value) {
        DataValue::Artifact(coords) => maven_path(coords),
        _ => None,
    }
}

fn library(libraries: &Path, coords: &str) -> Result<PathBuf> {
    join_safe(libraries, &maven_path(coords).with_context(|| format!("bad Maven coordinates {coords}"))?)
}

/// An argument or output of a processor: `[coords]` is a library path, otherwise `{KEY}` takes a data value and
/// `'text'` stays as written, with `\` escaping the next character.
fn resolve(text: &str, data: &HashMap<String, String>, libraries: &Path) -> Result<String> {
    if let Some(coords) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        return Ok(library(libraries, coords)?.to_string_lossy().into_owned());
    }
    replace_tokens(text, data)
}

fn replace_tokens(text: &str, data: &HashMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next().with_context(|| format!("bad escape in {text}"))?),
            '{' | '\'' => {
                let close = if c == '{' { '}' } else { '\'' };
                let mut key = String::new();
                loop {
                    match chars.next() {
                        None => bail!("unclosed {c} in {text}"),
                        Some('\\') => key.push(chars.next().with_context(|| format!("bad escape in {text}"))?),
                        Some(d) if d == close => break,
                        Some(d) => key.push(d),
                    }
                }
                if c == '\'' {
                    out.push_str(&key);
                } else {
                    out.push_str(data.get(&key).with_context(|| format!("the installer has no value {key}"))?);
                }
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

fn run(processor: &Processor, data: &HashMap<String, String>, site: &Site, log_path: &Path) -> Result<()> {
    let mut outputs = Vec::with_capacity(processor.outputs.len());
    for (path, sha1) in &processor.outputs {
        outputs.push((PathBuf::from(resolve(path, data, site.libraries)?), replace_tokens(sha1, data)?.to_ascii_lowercase()));
    }
    let matches = |(path, sha1): &(PathBuf, String)| net::file_sha1(path).is_ok_and(|have| have.eq_ignore_ascii_case(sha1));
    if !outputs.is_empty() && outputs.iter().all(matches) {
        return Ok(());
    }
    let jar = library(site.libraries, &processor.jar)?;
    let main = main_class(&jar)?;
    let mut classpath = vec![jar];
    for coords in &processor.classpath {
        classpath.push(library(site.libraries, coords)?);
    }
    let classpath = std::env::join_paths(&classpath).context("a library path cannot be on the classpath")?;
    let mut args = Vec::with_capacity(processor.args.len());
    for arg in &processor.args {
        args.push(resolve(arg, data, site.libraries)?);
    }
    let log = File::create(log_path).with_context(|| format!("cannot write {}", log_path.display()))?;
    let status = Command::new(site.java)
        .arg("-cp")
        .arg(&classpath)
        .arg(&main)
        .args(&args)
        .current_dir(site.version_dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()
        .with_context(|| format!("cannot start {}", site.java.display()))?;
    if !status.success() {
        bail!("{main} stopped with {status}: {}", tail(log_path));
    }
    for output in &outputs {
        if !matches(output) {
            let _ = std::fs::remove_file(&output.0);
            bail!("{main} wrote {} with the wrong checksum", output.0.display());
        }
    }
    Ok(())
}

fn tail(log_path: &Path) -> String {
    let text = std::fs::read_to_string(log_path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n")
}

/// The `Main-Class` of a jar's manifest.
fn main_class(jar: &Path) -> Result<String> {
    let mut zip = zip::ZipArchive::new(File::open(jar).with_context(|| format!("cannot open {}", jar.display()))?)?;
    let manifest = String::from_utf8_lossy(&entry(&mut zip, "META-INF/MANIFEST.MF")?).into_owned();
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("Main-Class:"))
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty())
        .with_context(|| format!("{} has no main class", jar.display()))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const FORGE_PROFILE: &str = include_str!("../testdata/install/forge-1.20.1-install_profile.json");
    const FORGE_VERSION: &str = include_str!("../testdata/install/forge-1.20.1-version.json");
    const NEO_PROFILE: &str = include_str!("../testdata/install/neoforge-26.3-install_profile.json");
    const NEO_VERSION: &str = include_str!("../testdata/install/neoforge-26.3-version.json");
    const FORGE26_PROFILE: &str = include_str!("../testdata/install/forge-26.3-install_profile.json");
    const FORGE26_VERSION: &str = include_str!("../testdata/install/forge-26.3-version.json");

    fn installer(dir: &Path, profile: &str, version: &str, extra: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join("installer.jar");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in [("install_profile.json", profile.as_bytes()), ("version.json", version.as_bytes())].into_iter().chain(extra.iter().copied()) {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn data() -> HashMap<String, String> {
        HashMap::from([("SIDE".to_owned(), "client".to_owned()), ("MAPPINGS".to_owned(), "/libs/m.txt".to_owned())])
    }

    #[test]
    fn neoforge_versions_map_to_their_game() {
        assert_eq!(neoforge_game("21.1.77").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_game("21.0.167").as_deref(), Some("1.21"));
        assert_eq!(neoforge_game("20.2.3-beta").as_deref(), Some("1.20.2"));
        assert_eq!(neoforge_game("26.3.0.103").as_deref(), Some("26.3"));
        assert_eq!(neoforge_game("26.1.2.126").as_deref(), Some("26.1.2"));
        assert_eq!(neoforge_game("0.25w14craftmine.3-beta"), None);
        assert_eq!(neoforge_game("47.1.3"), None);
    }

    #[test]
    fn neoforge_lists_the_game_newest_first_and_defaults_to_stable() {
        let all = ["21.1.1", "26.3.0.9-beta", "26.3.0.10", "26.3.0.101", "26.3.0.102-beta", "26.2.0.93"].map(str::to_owned).to_vec();
        let found = neoforge_versions(all, "26.3");
        assert_eq!(found.list, ["26.3.0.102-beta", "26.3.0.101", "26.3.0.10", "26.3.0.9-beta"]);
        assert_eq!(found.default.as_deref(), Some("26.3.0.101"));
        assert!(neoforge_versions(vec!["26.3.0.1".into()], "1.20.1").list.is_empty());
    }

    #[test]
    fn forge_prefers_the_recommended_version() {
        let metadata = HashMap::from([("1.20.1".to_owned(), ["1.20.1-47.0.0", "1.20.1-47.4.28", "1.20.1-47.4.29", "1.20.1-47.4.3"].map(str::to_owned).to_vec())]);
        let promos = HashMap::from([("1.20.1-recommended".to_owned(), "47.4.28".to_owned()), ("1.20.1-latest".to_owned(), "47.4.29".to_owned())]);
        let found = forge_versions(&metadata, &promos, "1.20.1");
        assert_eq!(found.list, ["47.4.29", "47.4.28", "47.4.3", "47.0.0"]);
        assert_eq!(found.default.as_deref(), Some("47.4.28"));
        let latest_only = HashMap::from([("1.20.1-latest".to_owned(), "47.4.29".to_owned())]);
        assert_eq!(forge_versions(&metadata, &latest_only, "1.20.1").default.as_deref(), Some("47.4.29"));
        assert_eq!(forge_versions(&metadata, &HashMap::new(), "1.20.1").default.as_deref(), Some("47.4.29"));
        assert!(forge_versions(&metadata, &promos, "26.3").list.is_empty());
    }

    #[test]
    fn data_values_are_artifacts_literals_or_bundled_files() {
        assert!(matches!(data_value("[net.minecraft:client:1.20.1:slim]"), DataValue::Artifact("net.minecraft:client:1.20.1:slim")));
        assert!(matches!(data_value("'abc'"), DataValue::Literal("abc")));
        assert!(matches!(data_value("/data/client.lzma"), DataValue::Bundled("data/client.lzma")));
        assert_eq!(artifact("[a.b:c:1:extra@zip]").as_deref(), Some("a/b/c/1/c-1-extra.zip"));
        assert_eq!(artifact("'x'"), None);
    }

    #[test]
    fn processor_arguments_are_substituted() {
        let libs = Path::new("libs");
        let d = data();
        assert_eq!(resolve("{MAPPINGS}", &d, libs).unwrap(), "/libs/m.txt");
        assert_eq!(resolve("--side={SIDE}", &d, libs).unwrap(), "--side=client");
        assert_eq!(resolve("'{SIDE}'", &d, libs).unwrap(), "{SIDE}");
        assert_eq!(resolve(r"\{SIDE\}", &d, libs).unwrap(), "{SIDE}");
        assert_eq!(resolve("--task", &d, libs).unwrap(), "--task");
        assert_eq!(PathBuf::from(resolve("[de.oceanlabs.mcp:mcp_config:1.20.1@zip]", &d, libs).unwrap()), libs.join("de/oceanlabs/mcp/mcp_config/1.20.1/mcp_config-1.20.1.zip"));
        assert!(resolve("{NOPE}", &d, libs).is_err());
        assert!(resolve("{SIDE", &d, libs).is_err());
        assert!(resolve("end\\", &d, libs).is_err());
        assert!(resolve("[../../evil:x:1]", &d, libs).is_err());
    }

    #[test]
    fn an_installer_for_another_game_is_refused() {
        let installer = || Installer { path: PathBuf::new(), sha1: String::new(), profile: serde_json::from_str(FORGE_PROFILE).unwrap(), version: serde_json::from_str(FORGE_VERSION).unwrap() };
        assert!(installer().for_game("1.20.1").is_ok());
        assert!(installer().for_game("1.21.1").err().is_some_and(|e| e.to_string().contains("1.20.1")));
    }

    #[test]
    fn forge_install_profile_parses_with_client_processors() {
        let profile: InstallProfile = serde_json::from_str(FORGE_PROFILE).unwrap();
        assert_eq!((profile.spec, profile.minecraft.as_str()), (1, "1.20.1"));
        let client: Vec<&str> = profile.processors.iter().filter(|p| p.for_client()).map(|p| p.jar.as_str()).collect();
        assert_eq!(client.first(), Some(&"net.minecraftforge:installertools:1.4.1"));
        assert_eq!(client.last(), Some(&"net.minecraftforge:binarypatcher:1.1.1"));
        assert!(profile.processors.iter().filter(|p| !p.for_client()).all(|p| p.sides.as_deref() == Some(&["server".to_owned()][..])));
        assert_eq!(profile.data["MC_SLIM_SHA"].client, "'de86b035d2da0f78940796bb95c39a932ed84834'");
        let splitter = profile.processors.iter().find(|p| p.jar.contains("jarsplitter") && p.for_client()).unwrap();
        assert_eq!(splitter.outputs["{MC_SLIM}"], "{MC_SLIM_SHA}");
    }

    #[test]
    fn neoforge_install_profile_patches_the_client_jar() {
        let profile: InstallProfile = serde_json::from_str(NEO_PROFILE).unwrap();
        let client: Vec<&Processor> = profile.processors.iter().filter(|p| p.for_client()).collect();
        assert_eq!(client.len(), 1);
        let d = HashMap::from([
            ("MINECRAFT_JAR".to_owned(), "/v/26.3.jar".to_owned()),
            ("PATCHED".to_owned(), "/l/patched.jar".to_owned()),
            ("ROOT".to_owned(), "/r".to_owned()),
            ("BINPATCH".to_owned(), "/w/client.lzma".to_owned()),
        ]);
        let args: Vec<String> = client[0].args.iter().map(|a| resolve(a, &d, Path::new("l")).unwrap()).collect();
        assert_eq!(args, ["--task", "PROCESS_MINECRAFT_JAR", "--no-mod-manifest", "--input", "/v/26.3.jar", "--output", "/l/patched.jar", "--extract-libraries-to", "/r/libraries/", "--apply-patches", "/w/client.lzma"]);
    }

    #[test]
    fn installers_open_with_their_version() {
        let dir = tempfile::tempdir().unwrap();
        for (profile, version, main) in [
            (FORGE_PROFILE, FORGE_VERSION, "cpw.mods.bootstraplauncher.BootstrapLauncher"),
            (NEO_PROFILE, NEO_VERSION, "net.neoforged.fml.startup.Client"),
            (FORGE26_PROFILE, FORGE26_VERSION, "net.minecraftforge.bootstrap.ForgeBootstrap"),
        ] {
            let opened = open(&installer(dir.path(), profile, version, &[])).unwrap();
            assert_eq!(opened.version.main_class.as_deref(), Some(main));
            assert!(!opened.libraries().is_empty());
        }
    }

    #[test]
    fn bundled_artifacts_and_data_files_are_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let path = installer(dir.path(), FORGE26_PROFILE, FORGE26_VERSION, &[("maven/net/x/a/1/a-1.jar", b"jar"), ("maven/net/", b""), ("data/client.lzma", b"patch")]);
        let opened = open(&path).unwrap();
        let libraries = dir.path().join("libraries");
        opened.unpack_bundled(&libraries).unwrap();
        assert_eq!(std::fs::read(libraries.join("net/x/a/1/a-1.jar")).unwrap(), b"jar");
        let version_dir = dir.path().join("v");
        let site = Site { root: dir.path(), libraries: &libraries, version_dir: &version_dir, client_jar: Path::new("c.jar"), game: "26.3", java: Path::new("java") };
        let data = opened.data(&site, &version_dir.join(DATA_DIR)).unwrap();
        assert_eq!(std::fs::read(&data["BINPATCH"]).unwrap(), b"patch");
        assert_eq!(data["PATCHED_SHA"], "5204a74b2c71ae37aa872bffe09ce73971747d23");
        assert_eq!(PathBuf::from(&data["PATCHED"]), libraries.join("net/minecraftforge/forge/26.3-66.1.8/forge-26.3-66.1.8-client.jar"));
        assert_eq!(data["MINECRAFT_VERSION"], "26.3");
    }

    #[test]
    fn processed_needs_the_same_installer_and_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(&installer(dir.path(), NEO_PROFILE, NEO_VERSION, &[])).unwrap();
        let libraries = dir.path().join("libraries");
        let version_dir = dir.path().join("v");
        assert!(!opened.processed(&version_dir, &libraries));
        let marker = |installer: &str| net::write_atomic(&version_dir.join(MARKER), &serde_json::to_vec(&Marker { installer: installer.into(), files: vec!["a/b.jar".into()] }).unwrap()).unwrap();
        marker(&opened.sha1);
        assert!(!opened.processed(&version_dir, &libraries));
        net::write_atomic(&libraries.join("a/b.jar"), b"x").unwrap();
        assert!(opened.processed(&version_dir, &libraries));
        assert_eq!(kept(&version_dir), ["a/b.jar"]);
        marker("other");
        assert!(!opened.processed(&version_dir, &libraries));
    }

    #[test]
    fn main_class_comes_from_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool.jar");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("META-INF/MANIFEST.MF", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"Manifest-Version: 1.0\r\nMain-Class: net.minecraftforge.installertools.ConsoleTool\r\n\r\n").unwrap();
        zip.finish().unwrap();
        assert_eq!(main_class(&path).unwrap(), "net.minecraftforge.installertools.ConsoleTool");
    }
}
