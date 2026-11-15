//! The move from batches that held version, mods and config to batches that play an instance: once per batch,
//! by renaming only, resumable after a crash at any step.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::batch::ModRef;
use super::i18n::trf;
use super::install::clean::LOCK_FILE;
use super::instance::{self, Instance, Loader};
use super::{lock, net, paths};

const SHARED: [&str; 6] = ["mods", "config", "resourcepacks", "shaderpacks", "options.txt", "servers.dat"];
const MOVED_FIELDS: [&str; 6] = ["game_version", "loader_version", "mods", "local_mods", "memory_mb", "jvm_args"];
const BATCH_FILE: &str = "batch.json";

static DONE: Mutex<bool> = Mutex::new(false);

/// What a migration did: how many batches moved, and a line for each that could not.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub moved: usize,
    pub problems: Vec<String>,
}

/// Moves every batch that still holds its own version, mods and config into an instance of the same id, once per
/// process. Nothing moves while a game runs or a cleanup holds the shared files; that waits for the next start.
pub fn run() -> Report {
    let mut done = DONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if *done {
        return Report::default();
    }
    let report = run_in(&paths::data_dir());
    *done = report.problems.is_empty();
    report
}

pub(super) fn run_in(root: &Path) -> Report {
    let batches = root.join("batches");
    let pending = match pending(&batches) {
        Ok(pending) => pending,
        Err(e) => return Report { moved: 0, problems: vec![format!("{e:#}")] },
    };
    if pending.is_empty() {
        return Report::default();
    }
    let held = match lock::try_exclusive(&root.join(LOCK_FILE)) {
        Ok(Some(held)) => held,
        Ok(None) => return Report { moved: 0, problems: vec![trf("%@ Batches ziehen beim nächsten Start in Instanzen um; gerade laufen noch Spiele.", &[&pending.len().to_string()])] },
        Err(e) => return Report { moved: 0, problems: vec![format!("{e:#}")] },
    };
    let instances = root.join("instances");
    let mut taken: Vec<String> = instance::load_all_in(&instances).map(|all| all.into_iter().map(|i| i.name).collect()).unwrap_or_default();
    let mut report = Report::default();
    for (id, value) in pending {
        let name = value.get("name").and_then(Value::as_str).unwrap_or(&id).to_owned();
        match migrate_one(&batches.join(&id), &instances.join(&id), &id, value, &mut taken) {
            Ok(()) => report.moved += 1,
            Err(e) => report.problems.push(trf("Batch „%@“ konnte nicht in eine Instanz umziehen: %@", &[&name, &format!("{e:#}")])),
        }
    }
    drop(held);
    report
}

fn pending(batches: &Path) -> Result<Vec<(String, Value)>> {
    let entries = match fs::read_dir(batches) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", batches.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", batches.display()))?;
        let id = entry.file_name().to_string_lossy().into_owned();
        let file = entry.path().join(BATCH_FILE);
        if !instance::valid_id(&id) || !file.is_file() {
            continue;
        }
        let bytes = fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?;
        let value: Value = serde_json::from_slice(&bytes).with_context(|| format!("{} is damaged", file.display()))?;
        let migrated = value.get("instance_id").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
        if !migrated && value.get("id").and_then(Value::as_str) == Some(id.as_str()) {
            found.push((id, value));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

fn migrate_one(from: &Path, to: &Path, id: &str, mut value: Value, taken: &mut Vec<String>) -> Result<()> {
    if !to.join(instance::FILE).is_file() {
        move_games(to)?;
        for name in SHARED {
            rename_new(&from.join(name), &to.join(name))?;
        }
        let mut made = instance_of(id, &value)?;
        made.name = instance::free_name(&made.name, taken);
        taken.push(made.name.clone());
        net::write_atomic(&to.join(instance::FILE), &serde_json::to_vec_pretty(&made)?)?;
    }
    let fields: &mut Map<String, Value> = value.as_object_mut().context("batch.json is not an object")?;
    for field in MOVED_FIELDS {
        fields.remove(field);
    }
    fields.insert("instance_id".into(), Value::String(id.to_owned()));
    net::write_atomic(&from.join(BATCH_FILE), &serde_json::to_vec_pretty(&value)?)
}

fn move_games(dir: &Path) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let games = instance::games_dir(dir);
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
        if !is_dir || name.len() != 32 || Uuid::try_parse(&name).is_err() {
            continue;
        }
        fs::create_dir_all(&games).with_context(|| format!("cannot create {}", games.display()))?;
        rename_new(&entry.path(), &games.join(&name))?;
    }
    Ok(())
}

fn rename_new(from: &Path, to: &Path) -> Result<()> {
    match fs::symlink_metadata(from) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", from.display())),
        Ok(_) => {}
    }
    if fs::symlink_metadata(to).is_ok() {
        bail!("{} exists already, nothing was moved over it", to.display());
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::rename(from, to).with_context(|| format!("cannot move {} to {}", from.display(), to.display()))
}

fn instance_of(id: &str, value: &Value) -> Result<Instance> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let list = |key: &str| value.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect::<Vec<_>>()).unwrap_or_default();
    let game_version = text("game_version").context("batch.json names no Minecraft version")?;
    let mods: Vec<ModRef> = match value.get("mods") {
        Some(mods) if !mods.is_null() => serde_json::from_value(mods.clone()).context("the mod list in batch.json is damaged")?,
        _ => Vec::new(),
    };
    let mut made = Instance::new(&text("name").unwrap_or_else(|| id.to_owned()), &game_version, Loader::Fabric(text("loader_version")));
    made.id = id.to_owned();
    made.mods = mods;
    made.local_mods = list("local_mods");
    made.jvm_args = list("jvm_args");
    if let Some(mb) = value.get("memory_mb").and_then(Value::as_u64).and_then(|m| u32::try_from(m).ok()) {
        made.memory_mb = mb;
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::core::batch::{self, Batch};

    const ID: &str = "b1";

    fn account(n: u128) -> String {
        Uuid::from_u128(n).simple().to_string()
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[cfg(windows)]
    fn link(target: &Path, at: &Path) {
        junction::create(target, at).unwrap();
    }

    #[cfg(not(windows))]
    fn link(target: &Path, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Fixture {
        fn batch(&self) -> PathBuf {
            self.root.join("batches").join(ID)
        }

        fn instance(&self) -> PathBuf {
            self.root.join("instances").join(ID)
        }

        fn game(&self, n: u128) -> PathBuf {
            self.instance().join("games").join(account(n))
        }
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let batch = root.join("batches").join(ID);
        let legacy = format!(
            r#"{{"id":"{ID}","name":"Farm","game_version":"1.21.1","loader_version":"0.16.5","mods":[{{"project_id":"P","version_id":"V","title":"Lithium","file_name":"lithium.jar","url":"https://cdn.modrinth.com/x","sha1":"aa"}}],"local_mods":["mine.jar"],"server":"mc.example.org","memory_mb":3072,"accounts":["{}","{}"],"jvm_args":["-XX:+UseG1GC"],"join_commands":["/login x"],"join_delay_ms":500,"background":"balanced"}}"#,
            Uuid::from_u128(1),
            Uuid::from_u128(2)
        );
        write(&batch.join(BATCH_FILE), &legacy);
        write(&batch.join("mods").join("lithium.jar"), "jar");
        write(&batch.join("mods").join("mine.jar"), "local");
        write(&batch.join("config").join("sodium.json"), "{}");
        write(&batch.join("resourcepacks").join("pack.zip"), "zip");
        fs::create_dir_all(batch.join("shaderpacks")).unwrap();
        write(&batch.join("options.txt"), "fov:1.0\n");
        write(&batch.join("servers.dat"), "servers");
        write(&batch.join("options.txt.old-1"), "older");
        let instances = root.join("instances").join(ID);
        for n in [1, 2, 3] {
            let game = instances.join(account(n));
            write(&game.join("saves").join("World").join("level.dat"), &format!("world {n}"));
            write(&game.join("options.txt"), "fov:1.0\n");
            write(&game.join("mods.old-1").join("x.jar"), "aside");
            link(&batch.join("mods"), &game.join("mods"));
        }
        write(&instances.join("stray.txt"), "stray");
        Fixture { _dir: dir, root }
    }

    fn check_done(f: &Fixture) {
        let loaded = batch::load_all_in(&f.root.join("batches")).unwrap();
        assert_eq!(loaded.len(), 1);
        let b: &Batch = &loaded[0];
        assert_eq!((b.instance_id.as_str(), b.name.as_str(), b.server.as_deref()), (ID, "Farm", Some("mc.example.org")));
        assert_eq!(b.accounts, vec![Uuid::from_u128(1), Uuid::from_u128(2)]);
        assert_eq!((b.join_commands.clone(), b.join_delay_ms, b.background), (vec!["/login x".to_owned()], 500, batch::Background::Balanced));
        assert!(b.memory_override.is_none());
        let raw = read(&f.batch().join(BATCH_FILE));
        assert!(MOVED_FIELDS.iter().all(|k| !raw.contains(&format!("\"{k}\""))), "{raw}");
        let made = instance::load_all_in(&f.root.join("instances")).unwrap();
        assert_eq!(made.len(), 1);
        let i = &made[0];
        assert_eq!((i.id.as_str(), i.name.as_str(), i.game_version.as_str(), &i.loader), (ID, "Farm", "1.21.1", &Loader::Fabric(Some("0.16.5".into()))));
        assert_eq!((i.mods.len(), i.mods[0].title.as_str(), i.local_mods.clone(), i.memory_mb, i.jvm_args.clone()), (1, "Lithium", vec!["mine.jar".to_owned()], 3072, vec!["-XX:+UseG1GC".to_owned()]));
        assert_eq!(read(&f.instance().join("mods").join("lithium.jar")), "jar");
        assert_eq!(read(&f.instance().join("mods").join("mine.jar")), "local");
        assert_eq!(read(&f.instance().join("config").join("sodium.json")), "{}");
        assert_eq!(read(&f.instance().join("resourcepacks").join("pack.zip")), "zip");
        assert!(f.instance().join("shaderpacks").is_dir());
        assert_eq!(read(&f.instance().join("options.txt")), "fov:1.0\n");
        assert_eq!(read(&f.instance().join("servers.dat")), "servers");
        for n in [1, 2, 3] {
            assert_eq!(read(&f.game(n).join("saves").join("World").join("level.dat")), format!("world {n}"));
            assert_eq!(read(&f.game(n).join("mods.old-1").join("x.jar")), "aside");
            assert!(!f.instance().join(account(n)).exists());
        }
        assert_eq!(read(&f.batch().join("options.txt.old-1")), "older");
        assert_eq!(read(&f.instance().join("stray.txt")), "stray");
        for name in SHARED {
            assert!(fs::symlink_metadata(f.batch().join(name)).is_err(), "{name} still in the batch folder");
        }
    }

    #[test]
    fn a_batch_becomes_an_instance_by_renames_alone() {
        let f = fixture();
        assert_eq!(run_in(&f.root), Report { moved: 1, problems: Vec::new() });
        check_done(&f);
        assert_eq!(run_in(&f.root), Report::default());
        check_done(&f);
    }

    #[test]
    fn a_crash_after_any_step_resumes() {
        let f = fixture();
        fs::create_dir_all(f.instance().join("games")).unwrap();
        fs::rename(f.instance().join(account(1)), f.game(1)).unwrap();
        fs::rename(f.batch().join("mods"), f.instance().join("mods")).unwrap();
        assert_eq!(run_in(&f.root).moved, 1);
        check_done(&f);

        let f = fixture();
        move_games(&f.instance()).unwrap();
        for name in SHARED {
            rename_new(&f.batch().join(name), &f.instance().join(name)).unwrap();
        }
        let value: Value = serde_json::from_str(&read(&f.batch().join(BATCH_FILE))).unwrap();
        net::write_atomic(&f.instance().join(instance::FILE), &serde_json::to_vec(&instance_of(ID, &value).unwrap()).unwrap()).unwrap();
        assert_eq!(run_in(&f.root).moved, 1);
        check_done(&f);
    }

    #[test]
    fn a_clash_stops_that_batch_without_moving_over_anything() {
        let f = fixture();
        write(&f.instance().join("config"), "in the way");
        let report = run_in(&f.root);
        assert_eq!((report.moved, report.problems.len()), (0, 1));
        assert_eq!(read(&f.instance().join("config")), "in the way");
        assert_eq!(read(&f.batch().join("config").join("sodium.json")), "{}");
        assert!(batch::load_all_in(&f.root.join("batches")).unwrap().is_empty());
        assert!(!f.instance().join(instance::FILE).exists());
        fs::remove_file(f.instance().join("config")).unwrap();
        assert_eq!(run_in(&f.root).moved, 1);
        check_done(&f);
    }

    #[test]
    fn nothing_moves_while_a_game_runs() {
        let f = fixture();
        let running = lock::try_shared(&f.root.join(LOCK_FILE)).unwrap().unwrap();
        let report = run_in(&f.root);
        assert_eq!((report.moved, report.problems.len()), (0, 1));
        assert!(f.batch().join("mods").join("lithium.jar").is_file());
        assert!(f.instance().join(account(1)).join("saves").is_dir());
        drop(running);
        assert_eq!(run_in(&f.root).moved, 1);
        check_done(&f);
    }

    #[test]
    fn migrated_and_foreign_folders_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("batches").join("new1").join(BATCH_FILE), r#"{"id":"new1","name":"N","instance_id":"i9"}"#);
        write(&root.join("batches").join("other").join(BATCH_FILE), r#"{"id":"else","name":"X","game_version":"1.21.1"}"#);
        write(&root.join("batches").join("other").join("mods").join("a.jar"), "a");
        assert_eq!(run_in(root), Report::default());
        assert!(root.join("batches").join("other").join("mods").join("a.jar").is_file());
        assert!(!root.join("instances").exists());
    }

    #[test]
    fn a_batch_without_game_folders_or_mods_still_moves() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("batches").join("b2").join(BATCH_FILE), r#"{"id":"b2","name":"Bare","game_version":"26.3"}"#);
        assert_eq!(run_in(root).moved, 1);
        let i = &instance::load_all_in(&root.join("instances")).unwrap()[0];
        assert_eq!((i.name.as_str(), &i.loader, i.memory_mb), ("Bare", &Loader::Fabric(None), instance::DEFAULT_MEMORY_MB));
        assert_eq!(batch::load_all_in(&root.join("batches")).unwrap()[0].instance_id, "b2");
    }

    #[test]
    fn only_account_folders_move_into_games() {
        let f = fixture();
        let outside = f.root.join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        let linked = account(4);
        link(&outside, &f.instance().join(&linked));
        write(&f.instance().join(account(5)), "a file named like an account");
        fs::create_dir_all(f.instance().join("notes")).unwrap();
        fs::create_dir_all(f.instance().join("z".repeat(32))).unwrap();
        assert_eq!(run_in(&f.root).moved, 1);
        check_done(&f);
        assert!(fs::symlink_metadata(f.instance().join(&linked)).is_ok());
        assert!(!f.instance().join("games").join(&linked).exists());
        assert_eq!(read(&f.instance().join(account(5))), "a file named like an account");
        assert!(f.instance().join("notes").is_dir());
        assert!(f.instance().join("z".repeat(32)).is_dir());
        assert!(outside.is_dir());
    }

    #[test]
    fn no_batches_folder_is_nothing_to_do_and_an_unreadable_one_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_in(dir.path()), Report::default());
        write(&dir.path().join("batches"), "not a folder");
        assert!(!run_in(dir.path()).problems.is_empty());
    }

    #[test]
    fn folders_that_are_no_batch_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("batches").join("not an id").join(BATCH_FILE), r#"{"id":"not an id","name":"X","game_version":"26.3"}"#);
        fs::create_dir_all(root.join("batches").join("empty1")).unwrap();
        assert_eq!(run_in(root), Report::default());
        assert!(!root.join("instances").exists());
    }

    #[test]
    fn batches_of_one_name_become_instances_of_different_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for id in ["b4", "b5"] {
            write(&root.join("batches").join(id).join(BATCH_FILE), &format!(r#"{{"id":"{id}","name":"Farm","game_version":"26.3"}}"#));
        }
        assert_eq!(run_in(root).moved, 2);
        let mut names: Vec<String> = instance::load_all_in(&root.join("instances")).unwrap().into_iter().map(|i| i.name).collect();
        names.sort();
        assert_eq!(names, ["Farm", "Farm 2"]);
    }

    #[test]
    fn a_null_mod_list_moves_as_no_mods() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("batches").join("b3").join(BATCH_FILE), r#"{"id":"b3","name":"Null","game_version":"26.3","mods":null}"#);
        assert_eq!(run_in(root).moved, 1);
        assert!(instance::load_all_in(&root.join("instances")).unwrap()[0].mods.is_empty());
    }
}
