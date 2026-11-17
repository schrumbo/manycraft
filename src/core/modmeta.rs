//! What a mod jar says about itself: id, name, version, icon and dependencies, read from the metadata its loader expects.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use zip::ZipArchive;
use zip::result::ZipError;

const MAX_ENTRY: u64 = 1 << 20;
const MAX_NESTED: u64 = 16 << 20;
const ICON_SIDE: u64 = 128;
const LOADER_IDS: [&str; 8] = ["minecraft", "java", "fabricloader", "fabric-loader", "quilt_loader", "forge", "neoforge", "javafml"];
const MANIFEST: &str = "META-INF/MANIFEST.MF";
const JAR_VERSION: &str = "${file.jarVersion}";

/// What a mod jar says about itself.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Meta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: Option<String>,
    pub depends: Vec<String>,
    pub provides: Vec<String>,
}

/// Reads the metadata of the jar at `path`; `None` when it has none Manycraft knows.
pub fn read(path: &Path) -> Result<Option<Meta>> {
    let mut zip = open(path)?;
    meta_of(&mut zip, true)
}

/// The bytes of `entry` in the jar at `path`, at most 1 MiB.
pub fn entry_bytes(path: &Path, entry: &str) -> Result<Vec<u8>> {
    let mut zip = open(path)?;
    read_entry(&mut zip, entry, MAX_ENTRY)?.with_context(|| format!("{} has no {entry}", path.display()))
}

fn open(path: &Path) -> Result<ZipArchive<BufReader<File>>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    ZipArchive::new(BufReader::new(file)).with_context(|| format!("{} is not a jar", path.display()))
}

fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let mut file = match zip.by_name(name) {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {name}")),
    };
    if file.size() > limit {
        bail!("{name} is larger than {limit} bytes");
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes).with_context(|| format!("cannot read {name}"))?;
    if u64::try_from(bytes.len()).is_ok_and(|len| len > limit) {
        bail!("{name} is larger than {limit} bytes");
    }
    Ok(Some(bytes))
}

pub(super) fn zip<R: Read + Seek>(jar: R) -> Result<ZipArchive<R>> {
    ZipArchive::new(jar).context("not a jar")
}

pub(super) fn text_in<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Option<String>> {
    read_text(zip, name)
}

fn read_text<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Option<String>> {
    Ok(read_entry(zip, name, MAX_ENTRY)?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

fn meta_of<R: Read + Seek>(zip: &mut ZipArchive<R>, nest: bool) -> Result<Option<Meta>> {
    let Some((mut meta, jars)) = parse(zip)? else { return Ok(None) };
    if nest {
        for jar in jars {
            let Some(bytes) = read_entry(zip, &jar, MAX_NESTED).ok().flatten() else { continue };
            let Ok(mut inner) = ZipArchive::new(Cursor::new(bytes)) else { continue };
            if let Ok(Some(found)) = meta_of(&mut inner, false) {
                meta.provides.push(found.id);
                meta.provides.extend(found.provides);
            }
        }
        meta.provides = unique(std::mem::take(&mut meta.provides));
    }
    Ok(Some(meta))
}

fn parse<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<Option<(Meta, Vec<String>)>> {
    if let Some(text) = read_text(zip, "fabric.mod.json")? {
        return Ok(Some((fabric(&text)?, nested(&text, false))));
    }
    if let Some(text) = read_text(zip, "quilt.mod.json")? {
        return Ok(Some((quilt(&text)?, nested(&text, true))));
    }
    for name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(text) = read_text(zip, name)? {
            let version = if text.contains(JAR_VERSION) { read_text(zip, MANIFEST).ok().flatten().and_then(|manifest| manifest_version(&manifest)) } else { None };
            return Ok(Some((forge(&text, version.as_deref())?, Vec::new())));
        }
    }
    Ok(None)
}

fn manifest_version(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim().eq_ignore_ascii_case("Implementation-Version") && !value.trim().is_empty()).then(|| value.trim().to_owned())
    })
}

pub(super) fn json(text: &str) -> Result<Value> {
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str(text).or_else(|first| serde_json::from_str(&text.replace(['\n', '\r', '\t'], " ")).map_err(|_| first)).context("invalid mod metadata")
}

fn nested(text: &str, quilt: bool) -> Vec<String> {
    let Ok(root) = json(text) else { return Vec::new() };
    let jars = if quilt { root.pointer("/quilt_loader/jars") } else { root.get("jars") };
    let Some(jars) = jars.and_then(Value::as_array) else { return Vec::new() };
    jars.iter()
        .filter_map(|jar| match jar {
            Value::String(path) => Some(path.clone()),
            other => other.get("file").and_then(Value::as_str).map(str::to_owned),
        })
        .collect()
}

fn unique(list: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    list.into_iter().filter(|item| seen.insert(item.clone())).collect()
}

fn lowered(items: impl IntoIterator<Item = String>) -> Vec<String> {
    unique(items.into_iter().map(|item| item.to_lowercase()).collect())
}

fn dependencies(items: impl IntoIterator<Item = String>) -> Vec<String> {
    lowered(items).into_iter().filter(|item| !LOADER_IDS.contains(&item.as_str())).collect()
}

fn string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn id_of(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        other => other.get("id").and_then(Value::as_str).map(str::to_owned),
    }
}

fn icon(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(path) => Some(path.clone()).filter(|p| !p.is_empty()),
        Value::Object(sizes) => {
            let mut sized: Vec<(u64, &str)> = sizes.iter().filter_map(|(size, path)| Some((size.trim().parse().ok()?, path.as_str().filter(|p| !p.is_empty())?))).collect();
            sized.sort_unstable_by_key(|(size, _)| *size);
            sized.iter().rev().find(|(size, _)| *size <= ICON_SIDE).or_else(|| sized.first()).map(|(_, path)| (*path).to_owned())
        }
        _ => None,
    }
}

pub(super) fn fabric(source: &str) -> Result<Meta> {
    let root = json(source)?;
    let id = string(root.get("id")).context("fabric.mod.json has no id")?.to_lowercase();
    let depends = root.get("depends").and_then(Value::as_object).into_iter().flat_map(|deps| deps.keys().cloned());
    let provides = root.get("provides").and_then(Value::as_array).into_iter().flatten().filter_map(id_of);
    Ok(Meta {
        name: string(root.get("name")).unwrap_or_else(|| id.clone()),
        version: string(root.get("version")).unwrap_or_default(),
        icon: icon(root.get("icon")),
        depends: dependencies(depends),
        provides: lowered(provides),
        id,
    })
}

pub(super) fn quilt(source: &str) -> Result<Meta> {
    let root = json(source)?;
    let loader = root.get("quilt_loader").context("quilt.mod.json has no quilt_loader")?;
    let id = string(loader.get("id")).context("quilt.mod.json has no id")?.to_lowercase();
    let depends = loader.get("depends").and_then(Value::as_array).into_iter().flatten().filter(|dep| dep.get("optional").and_then(Value::as_bool) != Some(true)).filter_map(id_of);
    let provides = loader.get("provides").and_then(Value::as_array).into_iter().flatten().filter_map(id_of);
    Ok(Meta {
        name: string(loader.pointer("/metadata/name")).unwrap_or_else(|| id.clone()),
        version: string(loader.get("version")).unwrap_or_default(),
        icon: icon(loader.pointer("/metadata/icon")),
        depends: dependencies(depends),
        provides: lowered(provides),
        id,
    })
}

pub(super) fn forge(source: &str, jar_version: Option<&str>) -> Result<Meta> {
    let root: toml::Table = toml::from_str(source).context("invalid mods.toml")?;
    let mut mods = root.get("mods").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_table);
    let first = mods.next().context("mods.toml lists no mod")?;
    let model = first.get("modId").and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).context("mods.toml has no modId")?;
    let id = model.to_lowercase();
    let version = first.get("version").and_then(toml::Value::as_str).map(|v| v.replace(JAR_VERSION, jar_version.unwrap_or_default()).trim().to_owned()).unwrap_or_default();
    let name = first.get("displayName").and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map_or_else(|| id.clone(), str::to_owned);
    let logo = first.get("logoFile").or_else(|| root.get("logoFile")).and_then(toml::Value::as_str).map(|path| path.trim().trim_start_matches('/')).filter(|path| !path.is_empty()).map(str::to_owned);
    let provides = mods.filter_map(|other| other.get("modId").and_then(toml::Value::as_str).map(str::to_owned));
    let listed = root.get("dependencies").and_then(toml::Value::as_table).and_then(|all| all.get(model).or_else(|| all.iter().find(|(key, _)| key.eq_ignore_ascii_case(model)).map(|(_, deps)| deps)));
    let depends = listed.and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_table).filter(|dep| forge_required(dep)).filter_map(|dep| dep.get("modId").and_then(toml::Value::as_str).map(str::to_owned));
    Ok(Meta { id, name, version, icon: logo, depends: dependencies(depends), provides: lowered(provides) })
}

fn forge_required(dep: &toml::Table) -> bool {
    let mandatory = dep.get("mandatory").and_then(toml::Value::as_bool);
    let kind = dep.get("type").and_then(toml::Value::as_str);
    mandatory == Some(true) || kind.is_some_and(|k| k.trim().eq_ignore_ascii_case("required")) || (mandatory.is_none() && kind.is_none())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn jar(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, zip_bytes(entries)).unwrap();
        path
    }

    const INNER: &str = r#"{"schemaVersion":1,"id":"Inner-Lib","version":"1.2.0","provides":["inner-api"]}"#;

    const SODIUM: &str = r#"{
        "schemaVersion": 1,
        "id": "sodium",
        "version": "0.6.13+mc1.21.4",
        "name": "Sodium",
        "description": "The fastest and most compatible rendering optimization mod for Minecraft.",
        "authors": ["JellySquid", "IMS212"],
        "icon": {"32": "assets/sodium/icon32.png", "64": "assets/sodium/icon64.png", "256": "assets/sodium/icon256.png"},
        "environment": "client",
        "provides": ["Sodium-Compat"],
        "depends": {"fabricloader": ">=0.16.10", "minecraft": "~1.21.4", "java": ">=21", "Fabric-API": "*"},
        "recommends": {"indium": "*"},
        "jars": [{"file": "META-INF/jars/inner-1.2.0.jar"}, {"file": "META-INF/jars/missing.jar"}]
    }"#;

    #[test]
    fn a_fabric_mod_gives_its_id_name_icon_dependencies_and_nested_mods() {
        let dir = tempfile::tempdir().unwrap();
        let inner = zip_bytes(&[("fabric.mod.json", INNER.as_bytes())]);
        let path = jar(dir.path(), "sodium.jar", &[("fabric.mod.json", SODIUM.as_bytes()), ("META-INF/jars/inner-1.2.0.jar", &inner), ("assets/sodium/icon64.png", b"png")]);
        let meta = read(&path).unwrap().unwrap();
        assert_eq!((meta.id.as_str(), meta.name.as_str(), meta.version.as_str()), ("sodium", "Sodium", "0.6.13+mc1.21.4"));
        assert_eq!(meta.icon.as_deref(), Some("assets/sodium/icon64.png"));
        assert_eq!(meta.depends, ["fabric-api"]);
        assert_eq!(meta.provides, ["sodium-compat", "inner-lib", "inner-api"]);
        assert_eq!(entry_bytes(&path, "assets/sodium/icon64.png").unwrap(), b"png");
    }

    #[test]
    fn the_icon_is_the_largest_up_to_128_else_the_smallest() {
        let with = |icon: &str| fabric(&format!(r#"{{"id":"a","icon":{icon}}}"#)).unwrap().icon;
        assert_eq!(with(r#""logo.png""#).as_deref(), Some("logo.png"));
        assert_eq!(with(r#"{"16":"a16","128":"a128","512":"a512"}"#).as_deref(), Some("a128"));
        assert_eq!(with(r#"{"256":"a256","512":"a512"}"#).as_deref(), Some("a256"));
        assert_eq!(with(r#"{"x":"bad"}"#), None);
        assert_eq!(with("5"), None);
        assert_eq!(fabric(r#"{"id":"a"}"#).unwrap().icon, None);
    }

    #[test]
    fn a_fabric_mod_without_a_name_takes_its_id_and_one_without_an_id_is_an_error() {
        let meta = fabric(r#"{"id":"Lithium","version":"0.14.3"}"#).unwrap();
        assert_eq!((meta.id.as_str(), meta.name.as_str()), ("lithium", "lithium"));
        assert!(meta.depends.is_empty() && meta.provides.is_empty());
        assert!(fabric(r#"{"name":"x"}"#).is_err());
        assert!(fabric("not json").is_err());
    }

    #[test]
    fn raw_control_characters_inside_strings_do_not_stop_the_metadata_from_being_read() {
        let text = "{\"id\":\"messy\",\n\t\"name\":\"Messy\nMod\",\r\n\"description\":\"line one\nline two\ttabbed\",\"depends\":{\"fabric-api\":\"*\"}}";
        assert!(serde_json::from_str::<Value>(text).is_err());
        let meta = fabric(text).unwrap();
        assert_eq!((meta.id.as_str(), meta.name.as_str()), ("messy", "Messy Mod"));
        assert_eq!(meta.depends, ["fabric-api"]);
        assert_eq!(fabric("\u{feff}{\"id\":\"bom\"}").unwrap().id, "bom");
    }

    const QUILT: &str = r#"{
        "schema_version": 1,
        "quilt_loader": {
            "group": "org.example",
            "id": "Cool_Mod",
            "version": "2.0.1",
            "metadata": {"name": "Cool Mod", "icon": {"64": "icon64.png", "512": "icon.png"}},
            "provides": ["old_cool", {"id": "cool_api", "version": "1.0"}],
            "depends": [
                "quilt_base",
                {"id": "quilt_loader", "versions": ">=0.20"},
                {"id": "minecraft", "versions": ">=1.21"},
                {"id": "required_lib", "versions": "*"},
                {"id": "optional_lib", "optional": true},
                {"id": "explicit_lib", "optional": false},
                [{"id": "either_a"}, {"id": "either_b"}]
            ],
            "jars": ["META-INF/jars/inner.jar"]
        }
    }"#;

    #[test]
    fn a_quilt_mod_skips_optional_and_loader_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let inner = zip_bytes(&[("quilt.mod.json", br#"{"quilt_loader":{"id":"inner_q","version":"1","provides":["inner_alias"]}}"#)]);
        let path = jar(dir.path(), "cool.jar", &[("quilt.mod.json", QUILT.as_bytes()), ("META-INF/jars/inner.jar", &inner)]);
        let meta = read(&path).unwrap().unwrap();
        assert_eq!((meta.id.as_str(), meta.name.as_str(), meta.version.as_str()), ("cool_mod", "Cool Mod", "2.0.1"));
        assert_eq!(meta.icon.as_deref(), Some("icon64.png"));
        assert_eq!(meta.depends, ["quilt_base", "required_lib", "explicit_lib"]);
        assert_eq!(meta.provides, ["old_cool", "cool_api", "inner_q", "inner_alias"]);
        assert!(quilt(r#"{"id":"x"}"#).is_err());
    }

    const FORGE: &str = r#"
modLoader = "javafml"
loaderVersion = "[47,)"
license = "MIT"
logoFile = "logo.png"

[[mods]]
modId = "Create"
version = "${file.jarVersion}"
displayName = "Create"

[[mods]]
modId = "create_extras"

[[dependencies.Create]]
modId = "forge"
mandatory = true
versionRange = "[47,)"
side = "BOTH"

[[dependencies.Create]]
modId = "minecraft"
mandatory = true
versionRange = "[1.20.1,1.21)"

[[dependencies.Create]]
modId = "Flywheel"
mandatory = true
versionRange = "[0.6,)"

[[dependencies.Create]]
modId = "jei"
mandatory = false
versionRange = "[15,)"

[[dependencies.other]]
modId = "unrelated"
mandatory = true
"#;

    #[test]
    fn a_forge_mod_takes_its_version_from_the_manifest_and_keeps_only_mandatory_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = "Manifest-Version: 1.0\r\nImplementation-Title: create\r\nImplementation-Version: 0.5.1.f\r\n\r\n";
        let path = jar(dir.path(), "create.jar", &[("META-INF/mods.toml", FORGE.as_bytes()), (MANIFEST, manifest.as_bytes())]);
        let meta = read(&path).unwrap().unwrap();
        assert_eq!((meta.id.as_str(), meta.name.as_str(), meta.version.as_str(), meta.icon.as_deref()), ("create", "Create", "0.5.1.f", Some("logo.png")));
        assert_eq!(meta.depends, ["flywheel"]);
        assert_eq!(meta.provides, ["create_extras"]);
        assert_eq!(forge(FORGE, None).unwrap().version, "");
        assert_eq!(forge(FORGE, Some("9")).unwrap().version, "9");
        assert!(forge("[[mods]]\nversion = \"1\"", None).is_err());
        assert!(forge("not = [toml", None).is_err());
    }

    const NEOFORGE: &str = r#"
modLoader = "javafml"
loaderVersion = "[2,)"
license = "LGPL"

[[mods]]
modId = "examplemod"
version = "3.1.4"
displayName = "Example Mod"
logoFile = "assets/examplemod/logo.png"

[[dependencies.examplemod]]
modId = "neoforge"
type = "required"
versionRange = "[21,)"
ordering = "NONE"
side = "BOTH"

[[dependencies.examplemod]]
modId = "Core_Lib"
type = "REQUIRED"
versionRange = "[1,)"

[[dependencies.examplemod]]
modId = "nice_to_have"
type = "optional"

[[dependencies.examplemod]]
modId = "untyped_lib"
versionRange = "[1,)"

[[dependencies.examplemod]]
modId = "avoided"
type = "incompatible"
"#;

    #[test]
    fn a_neoforge_mod_counts_required_and_untyped_dependencies_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = jar(dir.path(), "example.jar", &[("META-INF/neoforge.mods.toml", NEOFORGE.as_bytes())]);
        let meta = read(&path).unwrap().unwrap();
        assert_eq!((meta.id.as_str(), meta.name.as_str(), meta.version.as_str(), meta.icon.as_deref()), ("examplemod", "Example Mod", "3.1.4", Some("assets/examplemod/logo.png")));
        assert_eq!(meta.depends, ["core_lib", "untyped_lib"]);
        assert!(meta.provides.is_empty());
    }

    #[test]
    fn neoforge_metadata_wins_over_forge_metadata_and_fabric_over_both() {
        let dir = tempfile::tempdir().unwrap();
        let neo = "[[mods]]\nmodId = \"neo\"\n";
        let old = "[[mods]]\nmodId = \"old\"\n";
        let both = jar(dir.path(), "both.jar", &[("META-INF/mods.toml", old.as_bytes()), ("META-INF/neoforge.mods.toml", neo.as_bytes())]);
        assert_eq!(read(&both).unwrap().unwrap().id, "neo");
        let all = jar(dir.path(), "all.jar", &[("META-INF/mods.toml", old.as_bytes()), ("quilt.mod.json", QUILT.as_bytes()), ("fabric.mod.json", br#"{"id":"fab"}"#)]);
        assert_eq!(read(&all).unwrap().unwrap().id, "fab");
    }

    #[test]
    fn a_jar_without_metadata_is_none_and_a_file_that_is_no_jar_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let plain = jar(dir.path(), "plain.jar", &[("com/example/A.class", b"x")]);
        assert_eq!(read(&plain).unwrap(), None);
        let text = dir.path().join("text.jar");
        std::fs::write(&text, b"not a zip").unwrap();
        assert!(read(&text).is_err());
        assert!(read(&dir.path().join("missing.jar")).is_err());
        assert!(entry_bytes(&plain, "nothing.png").is_err());
    }

    #[test]
    fn an_entry_larger_than_a_mebibyte_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![7u8; (MAX_ENTRY + 1) as usize];
        let fits = vec![7u8; MAX_ENTRY as usize];
        let path = jar(dir.path(), "big.jar", &[("big.bin", &big), ("fits.bin", &fits)]);
        assert!(entry_bytes(&path, "big.bin").is_err());
        assert_eq!(entry_bytes(&path, "fits.bin").unwrap().len(), fits.len());
    }

    #[test]
    fn nested_jars_are_read_one_level_deep_only() {
        let dir = tempfile::tempdir().unwrap();
        let deepest = zip_bytes(&[("fabric.mod.json", br#"{"id":"deepest"}"#)]);
        let middle_json = r#"{"id":"middle","jars":[{"file":"deepest.jar"}]}"#;
        let middle = zip_bytes(&[("fabric.mod.json", middle_json.as_bytes()), ("deepest.jar", &deepest)]);
        let outer = r#"{"id":"outer","jars":[{"file":"middle.jar"}]}"#;
        let path = jar(dir.path(), "outer.jar", &[("fabric.mod.json", outer.as_bytes()), ("middle.jar", &middle)]);
        assert_eq!(read(&path).unwrap().unwrap().provides, ["middle"]);
    }
}
