//! Reads the instance lists of each launcher's folders; nothing here writes.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;

use super::{Found, Launcher, Library, Unsupported};
use crate::core::install;
use crate::core::instance::Loader;
use crate::core::prism;

const PRISM_CONFIG: &str = "prismlauncher.cfg";
const MULTIMC_CONFIG: &str = "multimc.cfg";
const MODRINTH_DB: &str = "app.db";
const MODRINTH_PROFILE: &str = "profile.json";
const CURSEFORGE_FILE: &str = "minecraftinstance.json";
const INSTANCE_JSON: &str = "instance.json";
const GD_CONFIG: &str = "config.json";
const VANILLA_PROFILES: &str = "launcher_profiles.json";
const INSTANCE_FOLDERS: [&str; 5] = ["instances", "Instances", "minecraft/Instances", "profiles", "data/instances"];
const FLATPAK: &str = ".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher";

pub(super) fn default_roots(data: Option<PathBuf>, config: Option<PathBuf>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data) = &data {
        for name in ["PrismLauncher", "MultiMC", "multimc", "ModrinthApp", "com.modrinth.theseus", "ATLauncher", "gdlauncher_next", "gdlauncher_carbon/data"] {
            roots.push(data.join(name));
        }
        roots.push(data.join(if cfg!(target_os = "macos") { "minecraft" } else { ".minecraft" }));
    }
    if let Some(config) = &config {
        roots.push(config.join("ATLauncher"));
    }
    if let Some(home) = &home {
        roots.push(home.join(".minecraft"));
        roots.push(home.join("curseforge/minecraft"));
        roots.push(home.join("Documents/curseforge/minecraft"));
        roots.push(home.join("ATLauncher"));
        if cfg!(target_os = "linux") {
            roots.push(home.join(FLATPAK));
        }
    }
    if cfg!(windows) {
        if let Some(local) = dirs::data_local_dir() {
            roots.push(local.join("MultiMC"));
            roots.push(local.join("Programs/ModrinthApp"));
        }
    }
    roots.retain(|r| r.is_dir());
    roots
}

pub(super) fn libraries(root: &Path) -> Vec<Library> {
    let mut found: Vec<Found> = Vec::new();
    if root.join(VANILLA_PROFILES).is_file() {
        found.extend(vanilla(root));
    }
    if root.join(MODRINTH_DB).is_file() {
        found.extend(modrinth_db(root));
    }
    let mut folders = vec![root.to_path_buf()];
    folders.extend(INSTANCE_FOLDERS.iter().map(|f| root.join(f)));
    let mut seen = std::collections::HashSet::new();
    for folder in folders {
        if !seen.insert(std::fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone())) {
            continue;
        }
        if let Some(one) = instance_dir(&folder, root) {
            found.push(one);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let hidden = entry.file_name().to_str().is_none_or(|n| n.starts_with(['.', '_']));
            if !hidden && path.is_dir() && let Some(one) = instance_dir(&path, root) {
                found.push(one);
            }
        }
    }
    let mut libs: Vec<Library> = Vec::new();
    for one in found {
        match libs.iter_mut().find(|l| l.launcher == one.launcher) {
            Some(lib) => lib.instances.push(one),
            None => libs.push(Library { launcher: one.launcher, root: root.to_path_buf(), instances: vec![one] }),
        }
    }
    libs
}

fn folder_name(dir: &Path) -> String {
    dir.file_name().and_then(|n| n.to_str()).unwrap_or("instance").to_owned()
}

fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn count_mods(game_dir: &Path) -> Result<usize, Unsupported> {
    prism::jar_names(&game_dir.join("mods")).map(|j| j.len()).map_err(|_| Unsupported::Unreadable)
}

fn loader_of(kind: &str, version: Option<String>) -> Result<Loader, Unsupported> {
    let version = version.filter(|v| !v.trim().is_empty());
    match kind.trim().to_ascii_lowercase().replace(['_', ' '], "").as_str() {
        "" | "vanilla" | "none" => Ok(Loader::Vanilla),
        "fabric" => Ok(Loader::Fabric(version)),
        "quilt" => Ok(Loader::Quilt(version)),
        "forge" => Ok(Loader::Forge(version)),
        "neoforge" | "neoforged" => Ok(Loader::NeoForge(version)),
        other => Err(Unsupported::Loader(other.to_owned())),
    }
}

fn found(launcher: Launcher, id: String, name: String, game_dir: PathBuf, game: Option<String>, loader: Result<Loader, Unsupported>) -> Found {
    let mods = count_mods(&game_dir);
    let unsupported = loader.clone().err().or_else(|| mods.clone().err()).or_else(|| game.is_none().then_some(Unsupported::NoVersion));
    Found { launcher, id, name, game_dir, game_version: game, loader: loader.unwrap_or(Loader::Vanilla), mods: mods.unwrap_or(0), unsupported }
}

fn text(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn instance_dir(dir: &Path, root: &Path) -> Option<Found> {
    if dir.join("instance.cfg").is_file() {
        return Some(prism_instance(dir, root));
    }
    if let Some(json) = read_json(&dir.join(CURSEFORGE_FILE)) {
        return Some(curseforge(dir, &json));
    }
    if let Some(json) = read_json(&dir.join(INSTANCE_JSON)) {
        if json.pointer("/launcher").is_some() {
            return Some(atlauncher(dir, &json));
        }
        if json.pointer("/game_configuration").is_some() {
            return Some(gd_carbon(dir, &json));
        }
    }
    if let Some(json) = read_json(&dir.join(GD_CONFIG)).filter(|j| j.pointer("/loader").is_some()) {
        return Some(gd_legacy(dir, &json));
    }
    if let Some(json) = read_json(&dir.join(MODRINTH_PROFILE)).filter(|j| j.pointer("/metadata").is_some()) {
        return Some(modrinth_profile(dir, &json));
    }
    None
}

fn prism_instance(dir: &Path, root: &Path) -> Found {
    let launcher = [dir, root].iter().find_map(|d| d.ancestors().take(3).find_map(|a| a.join(MULTIMC_CONFIG).is_file().then_some(Launcher::MultiMc).or_else(|| a.join(PRISM_CONFIG).is_file().then_some(Launcher::Prism)))).unwrap_or(Launcher::Prism);
    let id = folder_name(dir);
    match prism::instance_at(dir) {
        Ok(i) => {
            let unsupported = i.unsupported.as_deref().map(|r| if let Some(rest) = r.strip_prefix("uses ") { Unsupported::Loader(rest.split(',').next().unwrap_or(rest).to_owned()) } else if r.contains("Minecraft version") { Unsupported::NoVersion } else { Unsupported::Unreadable });
            Found { launcher, id, name: i.name, game_dir: i.game_dir, game_version: i.game_version, loader: i.loader, mods: i.mods, unsupported }
        }
        Err(_) => Found { launcher, name: id.clone(), id, game_dir: dir.join(".minecraft"), game_version: None, loader: Loader::Vanilla, mods: 0, unsupported: Some(Unsupported::Unreadable) },
    }
}

fn strip_game(version: &str, game: Option<&str>) -> String {
    match game.and_then(|g| version.strip_suffix(&format!("-{g}"))) {
        Some(head) => head.to_owned(),
        None => version.to_owned(),
    }
}

fn curseforge(dir: &Path, json: &Value) -> Found {
    let id = folder_name(dir);
    let name = text(json, "/name").unwrap_or_else(|| id.clone());
    let game = text(json, "/baseModLoader/minecraftVersion").or_else(|| text(json, "/gameVersion"));
    let loader = match text(json, "/baseModLoader/name") {
        None => Ok(Loader::Vanilla),
        Some(full) => {
            let (kind, rest) = full.split_once('-').unwrap_or((&full, ""));
            let version = text(json, "/baseModLoader/forgeVersion").or_else(|| (!rest.is_empty()).then(|| strip_game(rest, game.as_deref())));
            loader_of(kind, version)
        }
    };
    found(Launcher::CurseForge, id, name, dir.to_path_buf(), game, loader)
}

fn atlauncher(dir: &Path, json: &Value) -> Found {
    let id = folder_name(dir);
    let name = text(json, "/launcher/name").unwrap_or_else(|| id.clone());
    let game = text(json, "/id").or_else(|| text(json, "/inheritsFrom"));
    let loader = match (text(json, "/launcher/loaderVersion/type"), text(json, "/launcher/loaderVersion/version")) {
        (Some(kind), version) => loader_of(&kind, version),
        _ => Ok(Loader::Vanilla),
    };
    found(Launcher::AtLauncher, id, name, dir.to_path_buf(), game, loader)
}

fn gd_legacy(dir: &Path, json: &Value) -> Found {
    let id = folder_name(dir);
    let game = text(json, "/loader/mcVersion");
    let loader = loader_of(&text(json, "/loader/loaderType").unwrap_or_default(), text(json, "/loader/loaderVersion"));
    found(Launcher::GdLauncher, id.clone(), id, dir.to_path_buf(), game, loader)
}

fn gd_carbon(dir: &Path, json: &Value) -> Found {
    let id = folder_name(dir);
    let name = text(json, "/name").unwrap_or_else(|| id.clone());
    let version = json.pointer("/game_configuration/version_info");
    let game = version.and_then(|v| text(v, "/minecraft_version").or_else(|| text(v, "/release")));
    let modloader = version.and_then(|v| v.pointer("/modloaders")).and_then(Value::as_array).and_then(|l| l.first());
    let loader = match modloader {
        Some(m) => loader_of(&text(m, "/type_").or_else(|| text(m, "/type")).unwrap_or_default(), text(m, "/version")),
        None => Ok(Loader::Vanilla),
    };
    let inner = dir.join("instance");
    found(Launcher::GdLauncher, id, name, if inner.is_dir() { inner } else { dir.to_path_buf() }, game, loader)
}

fn modrinth_profile(dir: &Path, json: &Value) -> Found {
    let id = folder_name(dir);
    let name = text(json, "/metadata/name").unwrap_or_else(|| id.clone());
    let game = text(json, "/metadata/game_version");
    let version = text(json, "/metadata/loader_version/id").or_else(|| text(json, "/metadata/loader_version"));
    let loader = loader_of(&text(json, "/metadata/loader").unwrap_or_default(), version);
    found(Launcher::Modrinth, id, name, dir.to_path_buf(), game, loader)
}

fn modrinth_db(root: &Path) -> Vec<Found> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(db) = Connection::open_with_flags(root.join(MODRINTH_DB), flags) else { return Vec::new() };
    let Ok(mut query) = db.prepare("SELECT path, name, game_version, mod_loader, mod_loader_version FROM profiles") else { return Vec::new() };
    let rows = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?)));
    let Ok(rows) = rows else { return Vec::new() };
    rows.flatten()
        .filter(|(path, ..)| !path.contains(['/', '\\']) && !path.starts_with('.'))
        .map(|(path, name, game, kind, version)| {
            let dir = root.join("profiles").join(&path);
            let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| path.clone());
            found(Launcher::Modrinth, path, name, dir, game.filter(|g| !g.is_empty()), loader_of(kind.as_deref().unwrap_or(""), version))
        })
        .collect()
}

#[derive(Deserialize)]
struct Profiles {
    #[serde(default)]
    profiles: std::collections::BTreeMap<String, Profile>,
}

#[derive(Deserialize)]
struct Profile {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "lastVersionId")]
    last_version_id: String,
    #[serde(default, rename = "gameDir")]
    game_dir: Option<String>,
}

fn vanilla(root: &Path) -> Vec<Found> {
    let Ok(bytes) = std::fs::read(root.join(VANILLA_PROFILES)) else { return Vec::new() };
    let Ok(file) = serde_json::from_slice::<Profiles>(&bytes) else { return Vec::new() };
    file.profiles
        .into_iter()
        .map(|(key, p)| {
            let game_dir = p.game_dir.filter(|d| !d.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
            let parsed = parse_version_id(&p.last_version_id, root);
            let (game, loader) = match parsed {
                Ok((game, loader)) => (Some(game), Ok(loader)),
                Err(why) => (None, Err(why)),
            };
            let name = Some(p.name.trim().to_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| game.as_ref().map_or_else(|| key.clone(), |g| format!("Minecraft {g}")));
            let mut one = found(Launcher::Vanilla, key, name, game_dir, game, loader.clone());
            if let Err(Unsupported::NoVersion) = loader {
                one.unsupported = Some(Unsupported::NoVersion);
            }
            one
        })
        .collect()
}

fn is_release(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    (2..=3).contains(&parts.len()) && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn newest_installed(root: &Path) -> Option<String> {
    let mut found: Vec<(Vec<u32>, String)> = std::fs::read_dir(root.join("versions")).ok()?.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| is_release(n)).map(|n| (n.split('.').filter_map(|p| p.parse().ok()).collect(), n)).collect();
    found.sort();
    found.pop().map(|(_, n)| n)
}

fn parse_version_id(id: &str, root: &Path) -> Result<(String, Loader), Unsupported> {
    let id = id.trim();
    if id.is_empty() || id.starts_with("latest-") {
        return newest_installed(root).map(|v| (v, Loader::Vanilla)).ok_or(Unsupported::NoVersion);
    }
    for (prefix, make) in [("fabric-loader-", Loader::Fabric as fn(Option<String>) -> Loader), ("quilt-loader-", Loader::Quilt)] {
        if let Some(rest) = id.strip_prefix(prefix) {
            let (loader, game) = rest.rsplit_once('-').ok_or(Unsupported::NoVersion)?;
            return Ok((game.to_owned(), make(Some(loader.to_owned()))));
        }
    }
    if let Some(version) = id.strip_prefix("neoforge-") {
        return install::neoforge_game(version).map(|g| (g, Loader::NeoForge(Some(version.to_owned())))).ok_or(Unsupported::NoVersion);
    }
    if let Some((game, rest)) = id.split_once("-forge") {
        let version = rest.trim_start_matches('-');
        let version = version.strip_prefix(&format!("{game}-")).unwrap_or(version);
        return Ok((game.to_owned(), Loader::Forge(Some(version.to_owned()).filter(|v| !v.is_empty()))));
    }
    if id.contains("optifine") || id.contains("OptiFine") || id.contains("LiteLoader") {
        return Err(Unsupported::Loader(id.to_owned()));
    }
    Ok((id.to_owned(), Loader::Vanilla))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn jar(dir: &Path, name: &str) {
        write(&dir.join("mods").join(name), "jar");
    }

    fn lib(root: &Path, launcher: Launcher) -> Vec<Found> {
        libraries(root).into_iter().find(|l| l.launcher == launcher).map(|l| l.instances).unwrap_or_default()
    }

    #[test]
    fn curseforge_instances_read_name_version_loader_and_mods() {
        let root = tempfile::tempdir().unwrap();
        let farm = root.path().join("Instances").join("Farm Pack");
        write(&farm.join(CURSEFORGE_FILE), r#"{"name":"Farm Pack","gameVersion":"1.20.1","baseModLoader":{"name":"forge-47.2.0","forgeVersion":"47.2.0","minecraftVersion":"1.20.1"}}"#);
        jar(&farm, "a.jar");
        jar(&farm, "b.jar");
        write(&farm.join("mods").join("off.jar.disabled"), "x");
        let fabric = root.path().join("Instances").join("Light");
        write(&fabric.join(CURSEFORGE_FILE), r#"{"name":"Light","baseModLoader":{"name":"fabric-0.15.7-1.20.1","minecraftVersion":"1.20.1"}}"#);
        let plain = root.path().join("Instances").join("Plain");
        write(&plain.join(CURSEFORGE_FILE), r#"{"name":"Plain","gameVersion":"1.21.1","baseModLoader":null}"#);
        let found = lib(root.path(), Launcher::CurseForge);
        assert_eq!(found.len(), 3);
        let by = |id: &str| found.iter().find(|f| f.id == id).unwrap();
        assert_eq!((by("Farm Pack").loader.clone(), by("Farm Pack").mods, by("Farm Pack").game_version.as_deref()), (Loader::Forge(Some("47.2.0".into())), 2, Some("1.20.1")));
        assert_eq!(by("Light").loader, Loader::Fabric(Some("0.15.7".into())));
        assert_eq!((by("Plain").loader.clone(), by("Plain").unsupported.clone()), (Loader::Vanilla, None));
        assert!(by("Farm Pack").game_dir.ends_with("Farm Pack"));
    }

    #[test]
    fn atlauncher_instances_read_loader_and_game_version() {
        let root = tempfile::tempdir().unwrap();
        let one = root.path().join("instances").join("Tech");
        write(&one.join(INSTANCE_JSON), r#"{"id":"1.20.1","launcher":{"name":"Tech","pack":"Made Up","loaderVersion":{"type":"Fabric","version":"0.15.7"}}}"#);
        jar(&one, "m.jar");
        let vanilla = root.path().join("instances").join("Bare");
        write(&vanilla.join(INSTANCE_JSON), r#"{"id":"1.21","launcher":{"name":"Bare","vanillaInstance":true}}"#);
        let found = lib(root.path(), Launcher::AtLauncher);
        assert_eq!(found.len(), 2);
        let tech = found.iter().find(|f| f.id == "Tech").unwrap();
        assert_eq!((tech.loader.clone(), tech.mods, tech.game_version.as_deref(), tech.name.as_str()), (Loader::Fabric(Some("0.15.7".into())), 1, Some("1.20.1"), "Tech"));
        assert_eq!(found.iter().find(|f| f.id == "Bare").unwrap().loader, Loader::Vanilla);
    }

    #[test]
    fn gdlauncher_legacy_and_carbon_instances_are_read() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("instances").join("Old Style");
        write(&old.join(GD_CONFIG), r#"{"loader":{"loaderType":"neoforge","mcVersion":"1.21.1","loaderVersion":"21.1.77"},"mods":[]}"#);
        let carbon = root.path().join("instances").join("New Style");
        write(&carbon.join(INSTANCE_JSON), r#"{"name":"New Style","game_configuration":{"version_info":{"minecraft_version":"1.20.4","modloaders":[{"type_":"quilt","version":"0.26.0"}]}}}"#);
        write(&carbon.join("instance").join("mods").join("q.jar"), "x");
        let found = lib(root.path(), Launcher::GdLauncher);
        assert_eq!(found.len(), 2);
        let old = found.iter().find(|f| f.id == "Old Style").unwrap();
        assert_eq!((old.loader.clone(), old.game_version.as_deref()), (Loader::NeoForge(Some("21.1.77".into())), Some("1.21.1")));
        let new = found.iter().find(|f| f.id == "New Style").unwrap();
        assert_eq!((new.loader.clone(), new.mods, new.game_dir.clone()), (Loader::Quilt(Some("0.26.0".into())), 1, carbon.join("instance")));
    }

    #[test]
    fn modrinth_profiles_come_from_the_database_and_from_profile_json() {
        let root = tempfile::tempdir().unwrap();
        let db = Connection::open(root.path().join(MODRINTH_DB)).unwrap();
        db.execute_batch("CREATE TABLE profiles (path TEXT PRIMARY KEY, name TEXT, game_version TEXT, mod_loader TEXT, mod_loader_version TEXT, icon_path TEXT);
             INSERT INTO profiles VALUES ('Alpha','Alpha Pack','1.21.1','fabric','0.16.5',NULL);
             INSERT INTO profiles VALUES ('Beta','Beta','1.20.1','forge','47.2.0',NULL);
             INSERT INTO profiles VALUES ('Gamma','Gamma','1.8.9','vanilla',NULL,NULL);
             INSERT INTO profiles VALUES ('..','Evil','1.8.9','vanilla',NULL,NULL);").unwrap();
        drop(db);
        jar(&root.path().join("profiles").join("Alpha"), "a.jar");
        let legacy = root.path().join("profiles").join("Legacy");
        write(&legacy.join(MODRINTH_PROFILE), r#"{"metadata":{"name":"Legacy Pack","game_version":"1.19.4","loader":"quilt","loader_version":{"id":"0.20.0"}}}"#);
        let found = lib(root.path(), Launcher::Modrinth);
        assert_eq!(found.len(), 4);
        let by = |id: &str| found.iter().find(|f| f.id == id).unwrap();
        assert_eq!((by("Alpha").name.as_str(), by("Alpha").loader.clone(), by("Alpha").mods), ("Alpha Pack", Loader::Fabric(Some("0.16.5".into())), 1));
        assert_eq!(by("Beta").loader, Loader::Forge(Some("47.2.0".into())));
        assert_eq!(by("Gamma").loader, Loader::Vanilla);
        assert_eq!((by("Legacy").name.as_str(), by("Legacy").loader.clone()), ("Legacy Pack", Loader::Quilt(Some("0.20.0".into()))));
        assert!(found.iter().all(|f| f.id != ".."));
    }

    #[test]
    fn prism_and_multimc_share_a_format_and_are_told_apart_by_their_config() {
        let prism = tempfile::tempdir().unwrap();
        let multi = tempfile::tempdir().unwrap();
        for (root, cfg) in [(prism.path(), PRISM_CONFIG), (multi.path(), MULTIMC_CONFIG)] {
            write(&root.join(cfg), "[General]\n");
            write(&root.join("instances").join("one").join("instance.cfg"), "[General]\nname=One\n");
            write(&root.join("instances").join("one").join("mmc-pack.json"), r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.minecraftforge","version":"47.2.0"}]}"#);
        }
        let p = lib(prism.path(), Launcher::Prism);
        let m = lib(multi.path(), Launcher::MultiMc);
        assert_eq!((p.len(), m.len()), (1, 1));
        assert_eq!((p[0].name.as_str(), p[0].loader.clone()), ("One", Loader::Forge(Some("47.2.0".into()))));
    }

    #[test]
    fn vanilla_profiles_resolve_loader_ids_and_latest() {
        let root = tempfile::tempdir().unwrap();
        write(
            &root.path().join(VANILLA_PROFILES),
            r#"{"profiles":{
                "a":{"name":"","type":"latest-release","lastVersionId":"latest-release"},
                "b":{"name":"Fabric Run","lastVersionId":"fabric-loader-0.16.5-1.21.1","gameDir":"GAMEDIR"},
                "c":{"name":"Old Forge","lastVersionId":"1.12.2-forge1.12.2-14.23.5.2860"},
                "d":{"name":"Snap","lastVersionId":"24w14a"},
                "e":{"name":"NeoRun","lastVersionId":"neoforge-21.1.77"},
                "f":{"name":"Opti","lastVersionId":"1.8.9-OptiFine_HD_U_M5"}
            }}"#,
        );
        fs::create_dir_all(root.path().join("versions").join("1.20.4")).unwrap();
        fs::create_dir_all(root.path().join("versions").join("1.21.1")).unwrap();
        fs::create_dir_all(root.path().join("versions").join("fabric-loader-0.16.5-1.21.1")).unwrap();
        let found = lib(root.path(), Launcher::Vanilla);
        let by = |id: &str| found.iter().find(|f| f.id == id).unwrap();
        assert_eq!((by("a").game_version.as_deref(), by("a").name.as_str()), (Some("1.21.1"), "Minecraft 1.21.1"));
        assert_eq!((by("b").game_version.as_deref(), by("b").loader.clone()), (Some("1.21.1"), Loader::Fabric(Some("0.16.5".into()))));
        assert_eq!((by("c").game_version.as_deref(), by("c").loader.clone()), (Some("1.12.2"), Loader::Forge(Some("14.23.5.2860".into()))));
        assert_eq!((by("d").game_version.as_deref(), by("d").loader.clone()), (Some("24w14a"), Loader::Vanilla));
        assert_eq!((by("e").game_version.as_deref(), by("e").loader.clone()), (Some("1.21.1"), Loader::NeoForge(Some("21.1.77".into()))));
        assert!(matches!(by("f").unsupported, Some(Unsupported::Loader(_))));
    }

    #[test]
    fn latest_without_installed_versions_has_no_version() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join(VANILLA_PROFILES), r#"{"profiles":{"a":{"name":"Latest","lastVersionId":"latest-release"}}}"#);
        let found = lib(root.path(), Launcher::Vanilla);
        assert_eq!(found[0].unsupported, Some(Unsupported::NoVersion));
    }

    #[test]
    fn broken_files_and_empty_folders_find_nothing_and_do_not_panic() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join(VANILLA_PROFILES), "{");
        write(&root.path().join(MODRINTH_DB), "not a database");
        write(&root.path().join("instances").join("x").join(INSTANCE_JSON), "[");
        write(&root.path().join("instances").join("y").join(CURSEFORGE_FILE), "");
        assert!(libraries(root.path()).iter().all(|l| l.instances.is_empty()));
    }

    #[test]
    fn a_single_instance_folder_is_found_by_itself() {
        let root = tempfile::tempdir().unwrap();
        let one = root.path().join("Solo");
        write(&one.join(INSTANCE_JSON), r#"{"id":"1.20.1","launcher":{"name":"Solo"}}"#);
        assert_eq!(libraries(&one).len(), 1);
    }
}
