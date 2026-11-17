//! Instance folders named after their instance, `instances/<Name>`, found by the id in their `instance.json` and never
//! by their name: the naming rules, the id-to-folder registry, the move on rename and the one-time move away from id folders.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::i18n::{tr, trf};
use super::{adopt, instance, lock, paths, store};

/// Longest folder name, in characters.
pub const MAX_CHARS: usize = 48;
const FALLBACK: &str = "Minecraft";
const LOCKS: &str = ".locks";
const NAMING: &str = "folders.lock";
const LOG: &str = ".folders.log";
const RECHECK: Duration = Duration::from_millis(500);
const MISS_WAIT: Duration = Duration::from_secs(1);
const NAMING_WAIT: Duration = Duration::from_secs(15);
const MOVE_ATTEMPTS: u32 = 5;
const CREATE_ATTEMPTS: usize = 100;
const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

#[derive(Default)]
struct Index {
    folders: HashMap<String, (OsString, Instant)>,
    missed: HashMap<String, Instant>,
}

static REGISTRY: LazyLock<Mutex<HashMap<PathBuf, Index>>> = LazyLock::new(Mutex::default);

fn registry() -> MutexGuard<'static, HashMap<PathBuf, Index>> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The folder name for an instance called `name`: letters, digits, space, `-`, `_` and `.` only, whitespace collapsed,
/// no leading or trailing dots and spaces, at most [`MAX_CHARS`] characters, and never a name Windows reserves for a device.
pub fn sanitize(name: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in name.chars().filter(|c| !c.is_control()) {
        if c.is_whitespace() {
            space = true;
        } else if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    let out = trimmed(&out, MAX_CHARS);
    if out.is_empty() {
        return FALLBACK.to_owned();
    }
    unreserved(out)
}

fn trimmed(name: &str, max: usize) -> String {
    let cut: String = name.trim_start_matches(['.', ' ']).chars().take(max).collect();
    cut.trim_end_matches(['.', ' ']).to_owned()
}

fn device(stem: &str) -> bool {
    let upper = stem.trim_end().to_uppercase();
    let numbered = ["COM", "LPT"].iter().any(|p| upper.strip_prefix(p).is_some_and(|n| matches!(n.chars().collect::<Vec<_>>().as_slice(), ['1'..='9' | '¹' | '²' | '³'])));
    numbered || DEVICES.contains(&upper.as_str())
}

fn unreserved(name: String) -> String {
    let (stem, rest) = name.split_once('.').map_or((name.as_str(), None), |(s, r)| (s, Some(r)));
    if !device(stem) {
        return name;
    }
    let marked = match rest {
        Some(rest) => format!("{stem}_.{rest}"),
        None => format!("{stem}_"),
    };
    trimmed(&marked, MAX_CHARS)
}

fn fold(name: &OsStr) -> String {
    name.to_string_lossy().to_lowercase()
}

/// `base` when no entry under `root` other than `except` has that name in any case, else "`base` 2", "`base` 3", … cut to fit.
pub(super) fn free_in(root: &Path, base: &str, except: Option<&OsStr>) -> String {
    let taken: HashSet<String> = fs::read_dir(root).map(|d| d.filter_map(Result::ok).map(|e| e.file_name()).filter(|n| Some(n.as_os_str()) != except).map(|n| fold(&n)).collect()).unwrap_or_default();
    let free = |n: &str| !taken.contains(&n.to_lowercase());
    if free(base) {
        return base.to_owned();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!(" {n}");
            format!("{}{suffix}", trimmed(base, MAX_CHARS - suffix.chars().count()))
        })
        .find(|n| free(n))
        .unwrap_or_else(|| base.to_owned())
}

#[derive(Deserialize)]
struct Head {
    id: String,
    #[serde(default)]
    name: String,
}

fn head_at(dir: &Path) -> io::Result<Option<Head>> {
    let file = dir.join(instance::FILE);
    for path in [file.clone(), store::backup_path(&file)] {
        match fs::read(&path) {
            Ok(bytes) => {
                if let Ok(head) = serde_json::from_slice::<Head>(&bytes) {
                    return Ok(Some(head));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

fn read(root: &Path) -> Vec<(OsString, io::Result<Option<Head>>)> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<(OsString, io::Result<Option<Head>>)> = entries.filter_map(Result::ok).filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir()).map(|e| (e.file_name(), head_at(&e.path()))).collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn refresh(root: &Path, index: &mut Index) {
    let now = Instant::now();
    let mut next: HashMap<String, (OsString, Instant)> = HashMap::new();
    for (folder, head) in read(root) {
        let id = match head {
            Ok(Some(head)) => head.id,
            Ok(None) => continue,
            Err(_) => match index.folders.iter().find(|(_, (f, _))| *f == folder) {
                Some((id, _)) => id.clone(),
                None => continue,
            },
        };
        let earlier = index.folders.get(&id).is_some_and(|(f, _)| *f == folder);
        if earlier || !next.contains_key(&id) {
            next.insert(id, (folder, now));
        }
    }
    index.folders = next;
}

/// Records which folders under `root` the instances read from them are in, one per id, and returns the folders kept:
/// of two folders with the same id the one known before wins, else the first by name.
pub(super) fn settle(root: &Path, found: &[(OsString, String)]) -> HashSet<OsString> {
    let mut all = registry();
    let index = all.entry(root.to_path_buf()).or_default();
    let now = Instant::now();
    let mut next: HashMap<String, (OsString, Instant)> = HashMap::new();
    let mut sorted: Vec<&(OsString, String)> = found.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (folder, id) in sorted {
        let earlier = index.folders.get(id).is_some_and(|(f, _)| f == folder);
        if earlier || !next.contains_key(id) {
            next.insert(id.clone(), (folder.clone(), now));
        }
    }
    index.missed.retain(|id, _| !next.contains_key(id));
    let kept = next.values().map(|(f, _)| f.clone()).collect();
    index.folders = next;
    kept
}

/// The folder of instance `id` under `root`, checked against its `instance.json` at most every half second; a miss reads
/// the folders again, at most once a second per id.
pub(super) fn find(root: &Path, id: &str) -> Option<PathBuf> {
    lookup(root, id, false)
}

/// Like [`find`], but always checks the folder and always reads the folders again on a miss; for anything that writes.
pub(super) fn find_fresh(root: &Path, id: &str) -> Option<PathBuf> {
    lookup(root, id, true)
}

fn lookup(root: &Path, id: &str, fresh: bool) -> Option<PathBuf> {
    if !instance::valid_id(id) {
        return None;
    }
    let mut all = registry();
    let index = all.entry(root.to_path_buf()).or_default();
    let now = Instant::now();
    if let Some((folder, checked)) = index.folders.get(id).cloned() {
        let dir = root.join(&folder);
        if !fresh && now.duration_since(checked) < RECHECK {
            return Some(dir);
        }
        match head_at(&dir) {
            Ok(Some(head)) if head.id == id => {
                index.folders.insert(id.to_owned(), (folder, now));
                return Some(dir);
            }
            Err(_) => return Some(dir),
            _ => {}
        }
    }
    if !fresh && index.missed.get(id).is_some_and(|t| now.duration_since(*t) < MISS_WAIT) {
        return None;
    }
    refresh(root, index);
    match index.folders.get(id) {
        Some((folder, _)) => {
            index.missed.remove(id);
            Some(root.join(folder))
        }
        None => {
            index.missed.insert(id.to_owned(), now);
            None
        }
    }
}

/// Fails unless `dir` holds the `instance.json` of instance `id`.
pub(super) fn verify(dir: &Path, id: &str) -> Result<()> {
    match head_at(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        Some(head) if head.id == id => Ok(()),
        _ => bail!(tr("Diese Instanz gibt es nicht mehr.")),
    }
}

/// Records that instance `id` is in `dir` now.
pub(super) fn remember(root: &Path, id: &str, dir: &Path) {
    let Some(folder) = dir.file_name() else { return };
    let mut all = registry();
    let index = all.entry(root.to_path_buf()).or_default();
    index.missed.remove(id);
    index.folders.insert(id.to_owned(), (folder.to_owned(), Instant::now()));
}

/// Records that instance `id` has no folder under `root` any more.
pub(super) fn forget(root: &Path, id: &str) {
    let mut all = registry();
    if let Some(index) = all.get_mut(root) {
        index.folders.remove(id);
    }
}

/// The lock held shared while a game of instance `id` runs and exclusively while its folder is reset or moved; outside
/// the folder, so holding it never keeps the folder from moving.
pub(super) fn in_use_lock(root: &Path, id: &str) -> PathBuf {
    root.join(LOCKS).join(format!("{id}.in-use"))
}

/// Held while a folder under `root` is made, moved or named, so no two take the same name.
pub(super) fn naming(root: &Path) -> Result<lock::Guard> {
    lock::exclusive_within(&root.join(LOCKS).join(NAMING), NAMING_WAIT)
}

/// A free path under `root` for an instance called `name`, not made yet; hold [`naming`] until it exists.
pub(super) fn claim(root: &Path, name: &str) -> PathBuf {
    root.join(free_in(root, &sanitize(name), None))
}

/// Makes an empty folder under `root` for an instance called `name`, named so no entry there has that name, and returns it.
pub(super) fn create(root: &Path, name: &str) -> Result<PathBuf> {
    fs::create_dir_all(root).with_context(|| format!("cannot create {}", root.display()))?;
    let _naming = naming(root)?;
    for _ in 0..CREATE_ATTEMPTS {
        let dir = claim(root, name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("cannot create {}", dir.display())),
        }
    }
    bail!("no free folder name for {name}")
}

/// Renames the folder `from` to `to` in one step, trying again for a moment while Windows reports it in use. Refuses
/// when `to` exists, unless it is `from` in another case.
pub(super) fn move_dir(from: &Path, to: &Path) -> Result<()> {
    let same = from.parent() == to.parent() && from.file_name().map(fold) == to.file_name().map(fold);
    if !same && fs::symlink_metadata(to).is_ok() {
        bail!("{} exists already", to.display());
    }
    for attempt in 1..=MOVE_ATTEMPTS {
        match fs::rename(from, to) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < MOVE_ATTEMPTS => std::thread::sleep(Duration::from_millis(100 * u64::from(attempt))),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(anyhow::Error::new(e).context(tr("Ein anderes Programm nutzt gerade Dateien der Instanz. Schließe es und versuch es noch einmal."))),
            Err(e) => return Err(anyhow::Error::new(e).context(format!("cannot move {} to {}", from.display(), to.display()))),
            Ok(()) => break,
        }
    }
    store::moved(&from.join(instance::FILE), &to.join(instance::FILE));
    Ok(())
}

/// Whether a game of instance `id` that a manager launched still runs, as the launch record of this data folder says.
pub fn running(id: &str) -> bool {
    adopt::Ledger::open().games().values().any(|run| run.instance == id && adopt::verified(run, adopt::identify(run.pid).as_ref()))
}

/// A folder move in progress: the locks that keep games from starting and other moves away until it is dropped.
pub(super) struct Moving {
    pub from: PathBuf,
    pub to: PathBuf,
    _quiet: Option<lock::Guard>,
    _naming: lock::Guard,
}

impl Moving {
    /// Moves the folder back where it was.
    pub fn undo(&self, root: &Path, id: &str) -> Result<()> {
        if self.from == self.to {
            return Ok(());
        }
        move_dir(&self.to, &self.from)?;
        remember(root, id, &self.from);
        relink(&self.from);
        Ok(())
    }
}

/// Moves the folder of instance `id` under `root` to one named after `name`, unless it has that name already. Refused
/// with "Erst stoppen" while a game of the instance runs; the move keeps games from starting until it is dropped.
pub(super) fn rename_in(root: &Path, id: &str, name: &str, busy: &dyn Fn(&str) -> bool) -> Result<Moving> {
    let naming = naming(root)?;
    let from = find_fresh(root, id).ok_or_else(|| anyhow::anyhow!(tr("Diese Instanz gibt es nicht mehr.")))?;
    verify(&from, id)?;
    let folder = from.file_name().context("instance folder has no name")?.to_owned();
    let to = root.join(free_in(root, &sanitize(name), Some(&folder)));
    if to == from {
        return Ok(Moving { from: from.clone(), to: from, _quiet: None, _naming: naming });
    }
    let stop = || anyhow::anyhow!(tr("Erst stoppen: Ein Spiel dieser Instanz läuft."));
    let quiet = lock::try_exclusive(&in_use_lock(root, id))?.ok_or_else(stop)?;
    if busy(id) {
        return Err(stop());
    }
    move_dir(&from, &to)?;
    remember(root, id, &to);
    relink(&to);
    Ok(Moving { from, to, _quiet: Some(quiet), _naming: naming })
}

fn relink(dir: &Path) {
    if let Err(e) = super::fleet::relink(dir) {
        eprintln!("links in {} were not renewed: {e:#}", dir.display());
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Moving,
    Moved,
    Stayed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Step {
    id: String,
    from: String,
    to: String,
    state: State,
}

fn note(root: &Path, step: &Step) -> Result<()> {
    let path = root.join(LOG);
    let mut line = serde_json::to_vec(step)?;
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(&path).with_context(|| format!("cannot open {}", path.display()))?;
    file.write_all(&line).and_then(|()| file.sync_all()).with_context(|| format!("cannot write {}", path.display()))
}

fn steps(root: &Path) -> Vec<Step> {
    fs::read_to_string(root.join(LOG)).map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

/// What a move of folders did: how many moved, how many wait for their games to stop, and a line for each that could not.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    pub moved: usize,
    pub waiting: usize,
    pub problems: Vec<String>,
}

fn finish_interrupted(root: &Path) {
    let mut last: HashMap<String, Step> = HashMap::new();
    for step in steps(root) {
        last.insert(step.id.clone(), step);
    }
    for step in last.into_values().filter(|s| s.state == State::Moving) {
        let now = find_fresh(root, &step.id);
        let state = if now.as_deref() == Some(root.join(&step.to).as_path()) { State::Moved } else { State::Stayed };
        if let Some(dir) = &now {
            relink(dir);
        }
        if let Err(e) = note(root, &Step { state, ..step }) {
            eprintln!("folder log: {e:#}");
        }
    }
}

/// Moves every instance still in a folder named after its id to a folder named after the instance, one rename each,
/// logged before and after in `.folders.log`. An instance whose game runs waits for the next start; a move cut off by
/// a crash is finished at the next start.
pub(super) fn migrate_in(root: &Path, busy: &dyn Fn(&str) -> bool) -> Moved {
    let mut report = Moved::default();
    if !root.is_dir() {
        return report;
    }
    let held = match lock::try_exclusive(&root.join(LOCKS).join(NAMING)) {
        Ok(Some(held)) => held,
        Ok(None) => return report,
        Err(e) => return Moved { problems: vec![format!("{e:#}")], ..report },
    };
    finish_interrupted(root);
    let found = read(root);
    let mut homes: HashMap<String, Vec<OsString>> = HashMap::new();
    for (folder, head) in &found {
        if let Ok(Some(head)) = head {
            homes.entry(head.id.clone()).or_default().push(folder.clone());
        }
    }
    for (folder, head) in found {
        let Ok(Some(head)) = head else { continue };
        if folder != OsStr::new(&head.id) || !instance::valid_id(&head.id) {
            continue;
        }
        if homes.get(&head.id).is_some_and(|f| f.len() > 1) {
            report.problems.push(trf("Die Instanz „%@“ liegt in mehr als einem Ordner und bleibt, wo sie ist.", &[&head.name]));
            continue;
        }
        match move_one(root, &folder, &head, busy) {
            Ok(Outcome::Moved) => report.moved += 1,
            Ok(Outcome::Waiting) => report.waiting += 1,
            Ok(Outcome::Kept) => {}
            Err(e) => report.problems.push(trf("Der Ordner der Instanz „%@“ wurde nicht umbenannt: %@", &[&head.name, &format!("{e:#}")])),
        }
    }
    drop(held);
    report
}

enum Outcome {
    Moved,
    Waiting,
    Kept,
}

fn move_one(root: &Path, folder: &OsStr, head: &Head, busy: &dyn Fn(&str) -> bool) -> Result<Outcome> {
    let target = free_in(root, &sanitize(&head.name), Some(folder));
    if OsStr::new(&target) == folder {
        return Ok(Outcome::Kept);
    }
    let Some(_quiet) = lock::try_exclusive(&in_use_lock(root, &head.id))? else { return Ok(Outcome::Waiting) };
    if busy(&head.id) {
        return Ok(Outcome::Waiting);
    }
    let (from, to) = (root.join(folder), root.join(&target));
    let step = Step { id: head.id.clone(), from: folder.to_string_lossy().into_owned(), to: target, state: State::Moving };
    note(root, &step)?;
    if let Err(e) = move_dir(&from, &to) {
        let _ = note(root, &Step { state: State::Stayed, ..step });
        return Err(e);
    }
    remember(root, &head.id, &to);
    relink(&to);
    note(root, &Step { state: State::Moved, ..step })?;
    Ok(Outcome::Moved)
}

/// `manycraft folders undo`: moves every instance back to a folder named after its id, for going back to a Manycraft
/// before 0.9.4. Close Manycraft first; an instance whose game runs stays.
pub fn cli(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("undo") || args.len() != 1 {
        eprintln!("usage: manycraft folders undo");
        return 2;
    }
    let report = undo_in(&paths::instances_dir(), &running);
    println!("{} moved back, {} wait for their games to stop", report.moved, report.waiting);
    for problem in &report.problems {
        eprintln!("{problem}");
    }
    i32::from(!report.problems.is_empty() || report.waiting > 0)
}

/// Moves every instance back to a folder named after its id, as Manycraft before 0.9.4 expects them, logged like the
/// move away; the next start of this version moves them again.
pub(super) fn undo_in(root: &Path, busy: &dyn Fn(&str) -> bool) -> Moved {
    let mut report = Moved::default();
    let held = match naming(root) {
        Ok(held) => held,
        Err(e) => return Moved { problems: vec![format!("{e:#}")], ..report },
    };
    finish_interrupted(root);
    let mut seen = HashSet::new();
    for (folder, head) in read(root) {
        let Ok(Some(head)) = head else { continue };
        if folder == OsStr::new(&head.id) || !instance::valid_id(&head.id) || !seen.insert(head.id.clone()) {
            continue;
        }
        let to = root.join(&head.id);
        if fs::symlink_metadata(&to).is_ok() {
            report.problems.push(trf("Die Instanz „%@“ bleibt, wo sie ist: Ein Ordner mit ihrer Kennung ist schon da.", &[&head.name]));
            continue;
        }
        let quiet = match lock::try_exclusive(&in_use_lock(root, &head.id)) {
            Ok(Some(quiet)) if !busy(&head.id) => quiet,
            Ok(_) => {
                report.waiting += 1;
                continue;
            }
            Err(e) => {
                report.problems.push(format!("{e:#}"));
                continue;
            }
        };
        let step = Step { id: head.id.clone(), from: folder.to_string_lossy().into_owned(), to: head.id.clone(), state: State::Moving };
        let moved = note(root, &step).and_then(|()| move_dir(&root.join(&folder), &to));
        match moved {
            Ok(()) => {
                remember(root, &head.id, &to);
                relink(&to);
                let _ = note(root, &Step { state: State::Moved, ..step });
                report.moved += 1;
            }
            Err(e) => {
                let _ = note(root, &Step { state: State::Stayed, ..step });
                report.problems.push(trf("Der Ordner der Instanz „%@“ wurde nicht umbenannt: %@", &[&head.name, &format!("{e:#}")]));
            }
        }
        drop(quiet);
    }
    drop(held);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instance::{Instance, Loader};

    #[cfg(windows)]
    fn link(target: &Path, at: &Path) {
        junction::create(target, at).unwrap();
    }

    #[cfg(not(windows))]
    fn link(target: &Path, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    fn idle(_: &str) -> bool {
        false
    }

    fn named(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root).unwrap().filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.')).collect();
        names.sort();
        names
    }

    fn old_style(root: &Path, name: &str) -> Instance {
        let made = Instance::new(name, "26.3", Loader::Vanilla);
        let dir = root.join(&made.id);
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::write(dir.join("mods").join("a.jar"), "jar").unwrap();
        fs::write(dir.join(instance::FILE), serde_json::to_vec(&made).unwrap()).unwrap();
        made
    }

    fn game_with_link(dir: &Path) {
        let game = instance::games_dir(dir).join("acc");
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(dir.join("mods")).unwrap();
        link(&dir.join("mods"), &game.join("mods"));
    }

    fn links_into(dir: &Path) -> bool {
        let (link, target) = (instance::games_dir(dir).join("acc").join("mods"), dir.join("mods"));
        matches!((fs::canonicalize(link), fs::canonicalize(target)), (Ok(a), Ok(b)) if a == b)
    }

    #[test]
    fn names_keep_letters_digits_and_a_few_marks() {
        assert_eq!(sanitize("Technik"), "Technik");
        assert_eq!(sanitize("A/B\\C:D*E?F\"G<H>I|J"), "ABCDEFGHIJ");
        assert_eq!(sanitize("  Farm \u{a0}  Welt  "), "Farm Welt");
        assert_eq!(sanitize("a\tb\nc"), "abc");
        assert_eq!(sanitize("Größe 世界 1.21_x-y"), "Größe 世界 1.21_x-y");
        assert_eq!(sanitize("Kühe 🐄"), "Kühe");
    }

    #[test]
    fn names_lose_leading_and_trailing_dots_and_spaces() {
        assert_eq!(sanitize("Name. . ."), "Name");
        assert_eq!(sanitize(".trash"), "trash");
        assert_eq!(sanitize(" . .hidden"), "hidden");
    }

    #[test]
    fn empty_names_fall_back() {
        assert_eq!(sanitize(""), FALLBACK);
        assert_eq!(sanitize("???///"), FALLBACK);
        assert_eq!(sanitize("..."), FALLBACK);
    }

    #[test]
    fn names_are_cut_to_the_limit_without_a_trailing_space() {
        let long = format!("{} tail", "a".repeat(47));
        assert_eq!(sanitize(&long), "a".repeat(47));
        assert_eq!(sanitize(&"ü".repeat(80)).chars().count(), MAX_CHARS);
    }

    #[test]
    fn windows_device_names_get_a_mark() {
        assert_eq!(sanitize("CON"), "CON_");
        assert_eq!(sanitize("nul.txt"), "nul_.txt");
        assert_eq!(sanitize("Com1"), "Com1_");
        assert_eq!(sanitize("LPT¹"), "LPT¹_");
        assert_eq!(sanitize("aux.a.b"), "aux_.a.b");
        assert_eq!(sanitize("CONSOLE"), "CONSOLE");
        assert_eq!(sanitize("COM10"), "COM10");
        assert_eq!(sanitize("COM0"), "COM0");
        let long = format!("CON.{}", "x".repeat(60));
        assert!(sanitize(&long).starts_with("CON_."));
        assert_eq!(sanitize(&long).chars().count(), MAX_CHARS);
    }

    #[test]
    fn a_taken_name_gets_the_next_number_in_any_case() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("Technik")).unwrap();
        fs::create_dir(dir.path().join("technik 2")).unwrap();
        fs::write(dir.path().join("Farm"), "a file counts too").unwrap();
        fs::create_dir(dir.path().join("Ärger")).unwrap();
        assert_eq!(free_in(dir.path(), "TECHNIK", None), "TECHNIK 3");
        assert_eq!(free_in(dir.path(), "Farm", None), "Farm 2");
        assert_eq!(free_in(dir.path(), "ärger", None), "ärger 2");
        assert_eq!(free_in(dir.path(), "Neu", None), "Neu");
        assert_eq!(free_in(dir.path(), "technik", Some(OsStr::new("Technik"))), "technik");
    }

    #[test]
    fn a_numbered_name_still_fits_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let base = "b".repeat(MAX_CHARS);
        fs::create_dir(dir.path().join(&base)).unwrap();
        let next = free_in(dir.path(), &base, None);
        assert_eq!(next.chars().count(), MAX_CHARS);
        assert!(next.ends_with(" 2"));
    }

    #[test]
    fn two_instances_of_one_name_get_two_folders() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Instance::new("Farm", "26.3", Loader::Vanilla), Instance::new("farm", "26.3", Loader::Vanilla));
        instance::save_in(dir.path(), &a).unwrap();
        instance::save_in(dir.path(), &b).unwrap();
        assert_eq!(named(dir.path()), ["Farm", "farm 2"]);
        assert_eq!(instance::dir_in(dir.path(), &a.id), dir.path().join("Farm"));
        assert_eq!(instance::dir_in(dir.path(), &b.id), dir.path().join("farm 2"));
        assert!(dir.path().join("farm 2").join(instance::FILE).is_file());
    }

    #[test]
    fn a_folder_moved_by_hand_is_found_by_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let made = Instance::new("Farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        fs::rename(dir.path().join("Farm"), dir.path().join("Elsewhere")).unwrap();
        assert_eq!(find_fresh(dir.path(), &made.id), Some(dir.path().join("Elsewhere")));
        assert_eq!(instance::load_all_in(dir.path()).unwrap()[0].id, made.id);
    }

    #[test]
    fn another_instance_in_the_known_folder_is_not_taken_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Instance::new("Farm", "26.3", Loader::Vanilla), Instance::new("Other", "26.3", Loader::Vanilla));
        instance::save_in(dir.path(), &a).unwrap();
        instance::save_in(dir.path(), &b).unwrap();
        fs::rename(dir.path().join("Farm"), dir.path().join("Gone")).unwrap();
        fs::rename(dir.path().join("Other"), dir.path().join("Farm")).unwrap();
        assert_eq!(find_fresh(dir.path(), &a.id), Some(dir.path().join("Gone")));
        instance::delete_in(dir.path(), &a).unwrap();
        assert!(dir.path().join("Farm").join(instance::FILE).is_file());
        assert!(!dir.path().join("Gone").exists());
    }

    #[test]
    fn of_two_folders_with_one_id_only_one_counts() {
        let dir = tempfile::tempdir().unwrap();
        let made = Instance::new("Farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        instance::copy_tree(&dir.path().join("Farm"), &dir.path().join("A copy")).unwrap();
        assert_eq!(instance::load_all_in(dir.path()).unwrap().len(), 1);
        assert_eq!(instance::dir_in(dir.path(), &made.id), dir.path().join("Farm"));
    }

    #[test]
    fn a_rename_moves_the_folder_and_its_links() {
        let dir = tempfile::tempdir().unwrap();
        let mut made = Instance::new("Farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        game_with_link(&dir.path().join("Farm"));
        made.name = "Technik: Neu?".into();
        instance::save_renamed_in(dir.path(), &made, &idle).unwrap();
        assert_eq!(named(dir.path()), ["Technik Neu"]);
        assert_eq!(instance::load_all_in(dir.path()).unwrap()[0].name, "Technik: Neu?");
        assert!(links_into(&dir.path().join("Technik Neu")));
    }

    #[test]
    fn a_rename_in_case_only_keeps_one_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut made = Instance::new("farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        made.name = "Farm".into();
        instance::save_renamed_in(dir.path(), &made, &idle).unwrap();
        assert_eq!(named(dir.path()), ["Farm"]);
    }

    #[test]
    fn a_rename_onto_a_taken_name_numbers_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut a, b) = (Instance::new("Farm", "26.3", Loader::Vanilla), Instance::new("Technik", "26.3", Loader::Vanilla));
        instance::save_in(dir.path(), &a).unwrap();
        instance::save_in(dir.path(), &b).unwrap();
        a.name = "technik".into();
        instance::save_renamed_in(dir.path(), &a, &idle).unwrap();
        assert_eq!(named(dir.path()), ["Technik", "technik 2"]);
    }

    #[test]
    fn a_rename_waits_while_a_game_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut made = Instance::new("Farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        made.name = "Neu".into();
        let game = instance::hold_in(dir.path(), &made.id).unwrap();
        let refused = instance::save_renamed_in(dir.path(), &made, &idle).unwrap_err();
        assert!(format!("{refused:#}").contains("Erst stoppen"), "{refused:#}");
        drop(game);
        let refused = instance::save_renamed_in(dir.path(), &made, &|_| true).unwrap_err();
        assert!(format!("{refused:#}").contains("Erst stoppen"));
        assert_eq!(named(dir.path()), ["Farm"]);
        assert_eq!(instance::load_all_in(dir.path()).unwrap()[0].name, "Farm");
    }

    #[test]
    fn a_game_cannot_start_while_its_folder_moves() {
        let dir = tempfile::tempdir().unwrap();
        let made = Instance::new("Farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        let moving = rename_in(dir.path(), &made.id, "Neu", &idle).unwrap();
        assert!(instance::hold_in(dir.path(), &made.id).is_none());
        drop(moving);
        assert!(instance::hold_in(dir.path(), &made.id).is_some());
    }

    #[test]
    fn a_failed_save_after_a_move_moves_the_folder_back() {
        let dir = tempfile::tempdir().unwrap();
        let made = Instance::new("Farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        let moving = rename_in(dir.path(), &made.id, "Neu", &idle).unwrap();
        assert_eq!(named(dir.path()), ["Neu"]);
        moving.undo(dir.path(), &made.id).unwrap();
        assert_eq!(named(dir.path()), ["Farm"]);
        assert_eq!(find_fresh(dir.path(), &made.id), Some(dir.path().join("Farm")));
    }

    #[test]
    fn an_instance_back_from_the_trash_gets_a_free_name_folder() {
        let dir = tempfile::tempdir().unwrap();
        let made = Instance::new("Farm", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        let entry = instance::trash_in(dir.path(), &made, 5).unwrap();
        assert!(named(dir.path()).is_empty());
        instance::save_in(dir.path(), &Instance::new("Farm", "26.3", Loader::Vanilla)).unwrap();
        let back = instance::untrash_in(dir.path(), &entry).unwrap();
        assert_eq!(back.id, made.id);
        assert_eq!(named(dir.path()), ["Farm", "Farm 2"]);
        assert_eq!(instance::dir_in(dir.path(), &made.id), dir.path().join("Farm 2"));
    }

    #[test]
    fn id_folders_move_to_name_folders_once_with_their_links() {
        let dir = tempfile::tempdir().unwrap();
        let made = old_style(dir.path(), "Technik");
        game_with_link(&dir.path().join(&made.id));
        let report = migrate_in(dir.path(), &idle);
        assert_eq!(report, Moved { moved: 1, waiting: 0, problems: Vec::new() });
        let to = dir.path().join("Technik");
        assert_eq!(named(dir.path()), ["Technik"]);
        assert_eq!(fs::read_to_string(to.join("mods").join("a.jar")).unwrap(), "jar");
        assert!(links_into(&to));
        let log: Vec<State> = steps(dir.path()).into_iter().map(|s| s.state).collect();
        assert_eq!(log, [State::Moving, State::Moved]);
        assert_eq!(steps(dir.path())[0].from, made.id);
        assert_eq!(migrate_in(dir.path(), &idle), Moved::default());
    }

    #[test]
    fn an_instance_whose_game_runs_keeps_its_id_folder_for_now() {
        let dir = tempfile::tempdir().unwrap();
        let made = old_style(dir.path(), "Technik");
        let report = migrate_in(dir.path(), &|id| id == made.id);
        assert_eq!(report, Moved { moved: 0, waiting: 1, problems: Vec::new() });
        let game = instance::hold_in(dir.path(), &made.id).unwrap();
        assert_eq!(migrate_in(dir.path(), &idle).waiting, 1);
        assert_eq!(named(dir.path()), [made.id.clone()]);
        drop(game);
        assert_eq!(migrate_in(dir.path(), &idle).moved, 1);
        assert_eq!(named(dir.path()), ["Technik"]);
    }

    #[test]
    fn a_name_folder_taken_already_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("technik")).unwrap();
        let made = old_style(dir.path(), "Technik");
        migrate_in(dir.path(), &idle);
        assert_eq!(instance::dir_in(dir.path(), &made.id), dir.path().join("Technik 2"));
    }

    #[test]
    fn a_crash_after_the_rename_is_finished_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let made = old_style(dir.path(), "Technik");
        game_with_link(&dir.path().join(&made.id));
        note(dir.path(), &Step { id: made.id.clone(), from: made.id.clone(), to: "Technik".into(), state: State::Moving }).unwrap();
        fs::rename(dir.path().join(&made.id), dir.path().join("Technik")).unwrap();
        assert_eq!(migrate_in(dir.path(), &idle).moved, 0);
        assert!(links_into(&dir.path().join("Technik")));
        assert_eq!(steps(dir.path()).last().map(|s| s.state.clone()), Some(State::Moved));
    }

    #[test]
    fn a_crash_before_the_rename_moves_it_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let made = old_style(dir.path(), "Technik");
        note(dir.path(), &Step { id: made.id.clone(), from: made.id.clone(), to: "Technik".into(), state: State::Moving }).unwrap();
        assert_eq!(migrate_in(dir.path(), &idle).moved, 1);
        assert_eq!(named(dir.path()), ["Technik"]);
        let log: Vec<State> = steps(dir.path()).into_iter().map(|s| s.state).collect();
        assert_eq!(log, [State::Moving, State::Stayed, State::Moving, State::Moved]);
    }

    #[test]
    fn a_damaged_instance_file_keeps_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let made = old_style(dir.path(), "Technik");
        fs::write(dir.path().join(&made.id).join(instance::FILE), "{").unwrap();
        assert_eq!(migrate_in(dir.path(), &idle), Moved::default());
        assert_eq!(named(dir.path()), [made.id]);
    }

    #[test]
    fn an_id_living_in_two_folders_stays_put_and_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let made = old_style(dir.path(), "Technik");
        instance::copy_tree(&dir.path().join(&made.id), &dir.path().join("Technik")).unwrap();
        let report = migrate_in(dir.path(), &idle);
        assert_eq!(report.moved, 0);
        assert_eq!(report.problems.len(), 1);
        let mut want = vec![made.id.clone(), "Technik".to_owned()];
        want.sort();
        assert_eq!(named(dir.path()), want);
    }

    #[test]
    fn undo_moves_every_instance_back_to_its_id_folder() {
        let dir = tempfile::tempdir().unwrap();
        let made = old_style(dir.path(), "Technik");
        game_with_link(&dir.path().join(&made.id));
        let fresh = Instance::new("Neu", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &fresh).unwrap();
        migrate_in(dir.path(), &idle);
        assert_eq!(named(dir.path()), ["Neu", "Technik"]);
        assert_eq!(undo_in(dir.path(), &idle), Moved { moved: 2, waiting: 0, problems: Vec::new() });
        let mut want = vec![made.id.clone(), fresh.id.clone()];
        want.sort();
        assert_eq!(named(dir.path()), want);
        let back = dir.path().join(&made.id);
        assert!(links_into(&back));
        assert_eq!(instance::dir_in(dir.path(), &made.id), back);
        assert_eq!(migrate_in(dir.path(), &idle).moved, 2);
    }

    #[test]
    fn undo_leaves_an_instance_whose_id_folder_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let made = Instance::new("Neu", "26.3", Loader::Vanilla);
        instance::save_in(dir.path(), &made).unwrap();
        fs::create_dir(dir.path().join(&made.id)).unwrap();
        let report = undo_in(dir.path(), &idle);
        assert_eq!(report.moved, 0);
        assert_eq!(report.problems.len(), 1);
        assert!(dir.path().join("Neu").join(instance::FILE).is_file());
    }
}
