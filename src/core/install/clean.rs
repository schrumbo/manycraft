//! Removes what no instance needs any more: game versions, libraries and Java runtimes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

use super::meta::{self, Env, VersionJson};
use super::{FALLBACK_JAVA_COMPONENT, LOADER_PREFIX};
use crate::core::instance::{self, Instance, Loader};
use crate::core::{lock, paths};

/// How long unused files must have sat untouched before cleaning may remove them.
pub const MARGIN: Duration = Duration::from_secs(14 * 24 * 3600);

const MARKER: &str = ".complete";
pub(crate) const LOCK_FILE: &str = "in-use.lock";

/// What cleaning would remove.
#[derive(Debug, Default)]
pub struct Plan {
    pub remove: Vec<PathBuf>,
    pub bytes: u64,
}

/// Held while a game runs or installs; cleaning refuses to start while anyone holds it, and none can start during it.
pub fn in_use() -> Result<lock::Guard> {
    lock::try_shared(&paths::data_dir().join(LOCK_FILE))?.context("a cleanup is running, try again in a moment")
}

/// Plans a cleanup of the shared game files, and removes what it found when `apply` is set.
/// Applying fails when an instance runs or a game installs.
pub fn clean(apply: bool, margin: Duration) -> Result<Plan> {
    let root = paths::data_dir();
    let instances = instance::load_all()?;
    let _exclusive = if apply { Some(lock::try_exclusive(&root.join(LOCK_FILE))?.context("instances are running or a game is installing: stop them first")?) } else { None };
    let found = plan(&root, &instances, margin, SystemTime::now())?;
    if apply {
        remove(&root, &found)?;
    }
    Ok(found)
}

/// What under `root` no instance of `instances` needs and has not been touched for `margin`.
/// Fails without planning anything when a version an instance needs cannot be read.
pub fn plan(root: &Path, instances: &[Instance], margin: Duration, now: SystemTime) -> Result<Plan> {
    let versions = root.join("versions");
    let (kept, doomed) = split_versions(&versions, instances, margin, now);
    let mut components = HashSet::new();
    let mut keep_libraries = HashSet::new();
    let libraries = root.join("libraries");
    for id in &kept {
        let path = versions.join(id).join(format!("{id}.json"));
        let profile: VersionJson = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?).with_context(|| format!("{} is not valid", path.display()))?;
        components.insert(profile.java_version.as_ref().map_or(FALLBACK_JAVA_COMPONENT, |j| j.component.as_str()).to_owned());
        let resolved = meta::resolve(&profile.libraries, &Env::current(Vec::new()));
        for dep in resolved.classpath.iter().chain(resolved.natives.iter().map(|n| &n.dep)) {
            keep_libraries.insert(libraries.join(&dep.path));
        }
    }
    let mut plan = Plan::default();
    for path in doomed {
        plan.add(path);
    }
    if kept.is_empty() && !instances.is_empty() {
        bail!("none of the versions the instances need is installed, nothing to compare with");
    }
    for file in files_under(&libraries) {
        if !keep_libraries.contains(&file) && older(&file, margin, now) {
            plan.add(file);
        }
    }
    for path in unused_runtimes(&root.join("runtimes"), &components, margin, now) {
        plan.add(path);
    }
    Ok(plan)
}

impl Plan {
    fn add(&mut self, path: PathBuf) {
        self.bytes += size_of(&path);
        self.remove.push(path);
    }
}

fn remove(root: &Path, plan: &Plan) -> Result<()> {
    for path in &plan.remove {
        let outcome = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        match outcome {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("cannot remove {}", path.display())),
            _ => {}
        }
    }
    prune_empty(&root.join("libraries"));
    Ok(())
}

fn split_versions(versions: &Path, instances: &[Instance], margin: Duration, now: SystemTime) -> (Vec<String>, Vec<PathBuf>) {
    let installed: Vec<String> = std::fs::read_dir(versions)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| versions.join(name).join(format!("{name}.json")).is_file())
        .collect();
    let mut kept: HashSet<String> = HashSet::new();
    let games: HashSet<&str> = instances.iter().map(|b| b.game_version.as_str()).collect();
    for game in &games {
        if installed.iter().any(|n| n == game) {
            kept.insert((*game).to_owned());
        }
        let suffix = format!("-{game}");
        let loaders: Vec<&String> = installed.iter().filter(|n| n.starts_with(LOADER_PREFIX) && n.ends_with(&suffix)).collect();
        let here: Vec<&Instance> = instances.iter().filter(|b| b.game_version == *game).collect();
        for made in &here {
            if let Some(loader) = made.loader.version() {
                let wanted = format!("{LOADER_PREFIX}{loader}{suffix}");
                if loaders.iter().any(|n| **n == wanted) {
                    kept.insert(wanted);
                }
            }
        }
        if here.iter().any(|b| matches!(b.loader, Loader::Fabric(None)))
            && let Some(newest) = loaders.iter().max_by_key(|n| modified(&versions.join(n.as_str()).join(format!("{n}.json"))))
        {
            kept.insert((*newest).clone());
        }
    }
    let doomed = installed.iter().filter(|n| !kept.contains(*n)).map(|n| versions.join(n)).filter(|p| older(p, margin, now)).collect();
    let mut kept: Vec<String> = kept.into_iter().collect();
    kept.sort();
    (kept, doomed)
}

fn unused_runtimes(runtimes: &Path, components: &HashSet<String>, margin: Duration, now: SystemTime) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(runtimes).into_iter().flatten().filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !components.contains(entry.file_name().to_string_lossy().as_ref()) {
            if older(&path, margin, now) {
                out.push(path);
            }
            continue;
        }
        let finished: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(&path)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| Some((e.path().join(MARKER).metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        let Some(newest) = finished.iter().max_by_key(|(t, _)| *t).map(|(_, p)| p.clone()) else { continue };
        for revision in std::fs::read_dir(&path).into_iter().flatten().filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir() && *p != newest) {
            if older(&revision, margin, now) {
                let manifest = revision.with_extension("json");
                if manifest.is_file() {
                    out.push(manifest);
                }
                out.push(revision);
            }
        }
    }
    out
}

fn modified(path: &Path) -> Option<SystemTime> {
    if path.is_dir() {
        return files_under(path).iter().filter_map(|f| f.metadata().and_then(|m| m.modified()).ok()).max();
    }
    path.metadata().and_then(|m| m.modified()).ok()
}

fn older(path: &Path, margin: Duration, now: SystemTime) -> bool {
    modified(path).and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= margin)
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().filter_map(Result::ok) {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => pending.push(path),
                Ok(t) if t.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out
}

fn size_of(path: &Path) -> u64 {
    if path.is_dir() { files_under(path).iter().map(|f| f.metadata().map_or(0, |m| m.len())).sum() } else { path.metadata().map_or(0, |m| m.len()) }
}

fn prune_empty(dir: &Path) -> bool {
    let mut empty = true;
    for entry in std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() && prune_empty(&path) && std::fs::remove_dir(&path).is_ok() {
            continue;
        }
        empty = false;
    }
    empty
}

/// `manycraft clean [--apply] [--keep-days N]`: lists what no instance needs, and removes it with `--apply`.
pub fn cli(args: &[String]) -> i32 {
    let mut apply = false;
    let mut margin = MARGIN;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--keep-days" => match it.next().and_then(|d| d.parse::<u64>().ok()).and_then(|d| d.checked_mul(24 * 3600)) {
                Some(secs) => margin = Duration::from_secs(secs),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    match clean(apply, margin) {
        Ok(plan) => {
            for path in &plan.remove {
                println!("{}", path.display());
            }
            let verb = if apply { "removed" } else { "would remove" };
            println!("{verb} {} items, {} MB", plan.remove.len(), plan.bytes / (1024 * 1024));
            0
        }
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn usage() -> i32 {
    eprintln!("usage: manycraft clean [--apply] [--keep-days N]");
    2
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    const NOW_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

    fn touch(path: &Path, age: Duration) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if !path.exists() {
            std::fs::write(path, b"x").unwrap();
        }
        File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - age).unwrap();
    }

    fn version(root: &Path, id: &str, component: &str, libraries: &[&str], age: Duration) {
        let libs: Vec<String> = libraries.iter().map(|p| format!(r#"{{"name":"a:b:1","downloads":{{"artifact":{{"path":"{p}","url":"u","sha1":"s","size":1}}}}}}"#)).collect();
        let json = format!(r#"{{"id":"{id}","javaVersion":{{"component":"{component}","majorVersion":25}},"libraries":[{}]}}"#, libs.join(","));
        let path = root.join("versions").join(id).join(format!("{id}.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, json).unwrap();
        touch(&path, age);
        touch(&path.parent().unwrap().join(format!("{id}.jar")), age);
    }

    fn library(root: &Path, rel: &str, age: Duration) {
        touch(&root.join("libraries").join(rel), age);
    }

    fn runtime(root: &Path, component: &str, sha: &str, age: Duration) {
        let dir = root.join("runtimes").join(component).join(sha);
        std::fs::create_dir_all(&dir).unwrap();
        touch(&dir.join(MARKER), age);
        touch(&dir.join("bin").join("java"), age);
        touch(&root.join("runtimes").join(component).join(format!("{sha}.json")), age);
    }

    fn instance(game: &str, loader: Option<&str>) -> Instance {
        Instance::new("b", game, Loader::Fabric(loader.map(str::to_owned)))
    }

    fn names(root: &Path, plan: &Plan) -> Vec<String> {
        let mut out: Vec<String> = plan.remove.iter().map(|p| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")).collect();
        out.sort();
        out
    }

    #[test]
    fn what_an_instance_needs_stays_and_the_rest_goes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &["x/used.jar"], NOW_AGE);
        version(root, "fabric-loader-0.20-26.3", "delta", &["x/used.jar", "x/loader.jar"], NOW_AGE);
        version(root, "26.2", "gamma", &["x/old.jar"], NOW_AGE);
        version(root, "fabric-loader-0.19-26.2", "gamma", &["x/old.jar"], NOW_AGE);
        for lib in ["x/used.jar", "x/loader.jar", "x/old.jar", "x/orphan.jar"] {
            library(root, lib, NOW_AGE);
        }
        runtime(root, "delta", "aaa", NOW_AGE);
        runtime(root, "gamma", "bbb", NOW_AGE);
        let plan = plan(root, &[instance("26.3", None)], MARGIN, SystemTime::now()).unwrap();
        assert_eq!(
            names(root, &plan),
            ["libraries/x/old.jar", "libraries/x/orphan.jar", "runtimes/gamma", "versions/26.2", "versions/fabric-loader-0.19-26.2"]
        );
        assert!(plan.bytes > 0);
    }

    #[test]
    fn recently_touched_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &[], NOW_AGE);
        version(root, "26.4", "delta", &[], Duration::from_secs(3600));
        library(root, "x/fresh.jar", Duration::from_secs(60));
        library(root, "x/stale.jar", NOW_AGE);
        runtime(root, "gamma", "bbb", Duration::from_secs(60));
        let plan = plan(root, &[instance("26.3", None)], MARGIN, SystemTime::now()).unwrap();
        assert_eq!(names(root, &plan), ["libraries/x/stale.jar"]);
    }

    #[test]
    fn only_the_newest_finished_runtime_of_a_used_component_stays() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &[], NOW_AGE);
        runtime(root, "delta", "old", NOW_AGE * 2);
        runtime(root, "delta", "new", NOW_AGE);
        let plan = plan(root, &[instance("26.3", None)], MARGIN, SystemTime::now()).unwrap();
        assert_eq!(names(root, &plan), ["runtimes/delta/old", "runtimes/delta/old.json"]);
    }

    #[test]
    fn a_pinned_loader_stays_and_so_does_the_newest_for_unpinned_instances() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &[], NOW_AGE);
        version(root, "fabric-loader-0.18-26.3", "delta", &[], NOW_AGE * 3);
        version(root, "fabric-loader-0.19-26.3", "delta", &[], NOW_AGE * 2);
        version(root, "fabric-loader-0.20-26.3", "delta", &[], NOW_AGE);
        let pinned = instance("26.3", Some("0.18"));
        let both = plan(root, &[pinned.clone(), instance("26.3", None)], MARGIN, SystemTime::now()).unwrap();
        assert_eq!(names(root, &both), ["versions/fabric-loader-0.19-26.3"]);
        let only_pinned = plan(root, &[pinned], MARGIN, SystemTime::now()).unwrap();
        assert_eq!(names(root, &only_pinned), ["versions/fabric-loader-0.19-26.3", "versions/fabric-loader-0.20-26.3"]);
    }

    #[test]
    fn an_unreadable_needed_version_cancels_the_whole_plan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &["x/used.jar"], NOW_AGE);
        std::fs::write(root.join("versions/26.3/26.3.json"), b"{broken").unwrap();
        library(root, "x/used.jar", NOW_AGE);
        assert!(plan(root, &[instance("26.3", None)], MARGIN, SystemTime::now()).is_err());
        assert!(root.join("libraries/x/used.jar").exists());
    }

    #[test]
    fn a_needed_version_that_is_not_installed_cancels_the_plan_instead_of_wiping_everything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.2", "gamma", &["x/old.jar"], NOW_AGE);
        library(root, "x/old.jar", NOW_AGE);
        assert!(plan(root, &[instance("26.3", None)], MARGIN, SystemTime::now()).is_err());
    }

    #[test]
    fn without_instances_nothing_is_needed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.2", "gamma", &["x/old.jar"], NOW_AGE);
        library(root, "x/old.jar", NOW_AGE);
        let plan = plan(root, &[], MARGIN, SystemTime::now()).unwrap();
        assert_eq!(names(root, &plan), ["libraries/x/old.jar", "versions/26.2"]);
    }

    #[test]
    fn applying_removes_files_and_the_folders_they_leave_empty() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &["x/used.jar"], NOW_AGE);
        version(root, "26.2", "gamma", &[], NOW_AGE);
        library(root, "x/used.jar", NOW_AGE);
        library(root, "deep/er/orphan.jar", NOW_AGE);
        let found = plan(root, &[instance("26.3", None)], MARGIN, SystemTime::now()).unwrap();
        remove(root, &found).unwrap();
        assert!(root.join("libraries/x/used.jar").exists());
        assert!(!root.join("libraries/deep").exists());
        assert!(!root.join("versions/26.2").exists());
        assert!(root.join("versions/26.3/26.3.json").exists());
        remove(root, &found).unwrap();
    }

    #[test]
    fn the_in_use_lock_keeps_cleaning_out_and_cleaning_keeps_games_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        let running = lock::try_shared(&path).unwrap().unwrap();
        assert!(lock::try_exclusive(&path).unwrap().is_none());
        drop(running);
        let cleaning = lock::try_exclusive(&path).unwrap().unwrap();
        assert!(lock::try_shared(&path).unwrap().is_none());
        drop(cleaning);
    }

    #[test]
    fn the_default_margin_is_two_weeks() {
        assert_eq!(MARGIN, Duration::from_secs(1_209_600));
    }

    #[test]
    fn the_planned_size_adds_up_the_files_of_removed_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &[], NOW_AGE);
        version(root, "26.2", "gamma", &[], NOW_AGE);
        library(root, "orphan.jar", NOW_AGE);
        std::fs::write(root.join("libraries/orphan.jar"), [0u8; 700]).unwrap();
        touch(&root.join("libraries/orphan.jar"), NOW_AGE);
        let found = plan(root, &[instance("26.3", None)], MARGIN, SystemTime::now()).unwrap();
        let folder: u64 = ["26.2.json", "26.2.jar"].iter().map(|f| root.join("versions/26.2").join(f).metadata().unwrap().len()).sum();
        assert_eq!(found.bytes, 700 + folder);
        assert!(found.bytes > 700);
    }

    #[test]
    fn a_pinned_loader_of_another_game_version_keeps_nothing_here() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        version(root, "26.3", "delta", &[], NOW_AGE);
        version(root, "26.2", "delta", &[], NOW_AGE);
        version(root, "fabric-loader-0.19-26.3", "delta", &[], NOW_AGE * 2);
        version(root, "fabric-loader-0.20-26.3", "delta", &[], NOW_AGE);
        let found = plan(root, &[instance("26.3", Some("0.19")), instance("26.2", None)], MARGIN, SystemTime::now()).unwrap();
        assert_eq!(names(root, &found), ["versions/fabric-loader-0.20-26.3"]);
    }

    #[test]
    fn the_command_line_rejects_what_it_does_not_know() {
        assert_eq!(cli(&["--wat".into()]), 2);
        assert_eq!(cli(&["--keep-days".into(), "x".into()]), 2);
        assert_eq!(cli(&["--keep-days".into(), "213503982334602".into()]), 2);
    }
}
