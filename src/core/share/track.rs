//! What runs: when each game loaded, the baseline of its files then, and what flows from it to the sync folder after.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

use super::{CONFIG, Mother, Own, files_in, flags_in, keys, put_files, put_options, read_file, read_text};
use crate::core::sync::{self, OPTIONS_FILE, Peers, Roots, SERVERS_FILE};

const SCAN_EVERY: Duration = Duration::from_secs(3);
const SHOWN_GRACE: Duration = Duration::from_secs(180);
const LOG_STEP: u64 = 4 * 1024 * 1024;
const LOADED_MARKS: [&str; 2] = ["Sound engine started", "Error starting SoundSystem"];

/// A game that just started.
#[derive(Clone, Debug)]
pub(crate) struct Start {
    pub key: String,
    pub instance: String,
    pub name: String,
    pub dir: PathBuf,
    pub game: PathBuf,
    pub pid: u32,
    pub companion: bool,
    pub at: SystemTime,
    /// A game an earlier manager started, long past loading.
    pub adopted: bool,
}

impl Start {
    fn own(&self) -> Own {
        Own { id: self.instance.clone(), name: self.name.clone(), dir: self.dir.clone() }
    }
}

#[derive(Debug)]
struct Game {
    start: Start,
    shown: Option<Instant>,
    read: u64,
    base: Option<Base>,
}

#[derive(Debug)]
struct Base {
    options: BTreeMap<String, String>,
    servers: Option<Vec<u8>>,
    seen: Option<(u64, Option<SystemTime>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    sha: [u8; 32],
}

/// The running games as sharing follows them.
pub(crate) struct Tracker {
    mother: Mother,
    shown: fn(u32) -> bool,
    games: HashMap<String, Game>,
    config: HashMap<String, Option<BTreeMap<String, Stamp>>>,
    scanned: Option<Instant>,
}

impl Tracker {
    pub(crate) fn new(mother: Mother, shown: fn(u32) -> bool) -> Self {
        Self { mother, shown, games: HashMap::new(), config: HashMap::new(), scanned: None }
    }

    /// Follows a game that started; until it loaded nothing it writes is trusted.
    pub(crate) fn started(&mut self, roots: &Roots, peers: &impl Peers, start: Start) {
        if let Some(old) = self.games.get(&start.key).map(|g| g.start.pid) {
            self.ended(roots, peers, &start.key.clone(), old);
        }
        let key = start.key.clone();
        let adopted = start.adopted;
        if let Some(Some(_)) = self.config.get(&start.instance) {
            let own = start.own();
            self.scan(&own);
            self.config.insert(start.instance.clone(), None);
        }
        self.config.entry(start.instance.clone()).or_insert(None);
        self.games.insert(key.clone(), Game { start, shown: None, read: 0, base: None });
        if adopted {
            self.loaded(&key);
        }
    }

    /// The game `key` finished loading: its files now are its baseline.
    pub(crate) fn loaded(&mut self, key: &str) {
        let Some(game) = self.games.get_mut(key) else { return };
        if game.base.is_some() {
            return;
        }
        let path = game.start.game.join(OPTIONS_FILE);
        let seen = file_stamp(&path);
        let options = read_text(&path).ok().flatten().map(|t| keys::map(&t)).unwrap_or_default();
        let servers = read_file(&game.start.game.join(SERVERS_FILE)).ok().flatten();
        game.base = Some(Base { options, servers, seen });
        let instance = game.start.instance.clone();
        let dir = game.start.dir.clone();
        let waiting = self.games.values().any(|g| g.start.instance == instance && g.base.is_none());
        if !waiting && matches!(self.config.get(&instance), Some(None) | None) {
            self.config.insert(instance, stamps(&dir.join(CONFIG), None));
        }
    }

    /// The game `key` saved the options `values` changed.
    pub(crate) fn options(&mut self, roots: &Roots, peers: &impl Peers, key: &str, values: &BTreeMap<String, String>) {
        let Some(game) = self.games.get_mut(key) else { return };
        let Some(base) = game.base.as_mut() else { return };
        let start = game.start.clone();
        if let Err(e) = sync::instance_options(roots, peers, &start.instance, values) {
            eprintln!("the options of {} were not kept: {e:#}", start.name);
        }
        let mine = keys::changed(&base.options, values);
        base.options.extend(values.iter().map(|(k, v)| (k.clone(), v.clone())));
        if mine.is_empty() || !flags_in(&start.dir).options {
            return;
        }
        self.options_up(roots, peers, &start.own(), &mine);
    }

    fn options_up(&self, roots: &Roots, peers: &impl Peers, own: &Own, changes: &BTreeMap<String, String>) {
        let applied = match put_options(&self.mother, own, changes) {
            Ok(applied) => applied,
            Err(e) => {
                eprintln!("the options of {} were not shared: {e:#}", own.name);
                return;
            }
        };
        if applied.is_empty() {
            return;
        }
        for other in running_sharers(roots, peers, &own.id, |f| f.options) {
            let _ = sync::instance_options(roots, peers, &other, &applied);
        }
    }

    /// The game `key` saved its server list.
    pub(crate) fn servers(&mut self, roots: &Roots, peers: &impl Peers, key: &str) {
        let Some(game) = self.games.get_mut(key) else { return };
        let Some(base) = game.base.as_mut() else { return };
        let start = game.start.clone();
        let Ok(Some(bytes)) = read_file(&start.game.join(SERVERS_FILE)) else { return };
        base.servers = Some(bytes.clone());
        if let Err(e) = sync::instance_servers(roots, peers, &start.instance, &bytes, Some(key)) {
            eprintln!("the server list of {} was not kept: {e:#}", start.name);
        }
        if flags_in(&start.dir).servers {
            self.servers_up(roots, peers, &start.own(), bytes);
        }
    }

    fn servers_up(&self, roots: &Roots, peers: &impl Peers, own: &Own, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        match put_files(&self.mother, own, vec![(SERVERS_FILE.to_owned(), bytes.clone())]) {
            Ok(0) => {}
            Ok(_) => {
                for other in running_sharers(roots, peers, &own.id, |f| f.servers) {
                    let _ = sync::instance_servers(roots, peers, &other, &bytes, None);
                }
            }
            Err(e) => eprintln!("the server list of {} was not shared: {e:#}", own.name),
        }
    }

    /// The game `key` with process `pid` ended: what it changed since its baseline flows up a last time.
    pub(crate) fn ended(&mut self, roots: &Roots, peers: &impl Peers, key: &str, pid: u32) {
        if self.games.get(key).is_none_or(|g| g.start.pid != pid) {
            return;
        }
        let Some(game) = self.games.remove(key) else { return };
        let own = game.start.own();
        if let Some(base) = &game.base
            && !game.start.companion
        {
            self.plain_exit(roots, peers, &game.start, base);
        }
        if let Some(Some(_)) = self.config.get(&own.id) {
            self.scan(&own);
        }
        let instance = &game.start.instance;
        if !self.games.values().any(|g| &g.start.instance == instance) {
            self.config.remove(instance);
        } else if !self.games.values().any(|g| &g.start.instance == instance && g.base.is_none()) && matches!(self.config.get(instance), Some(None)) {
            self.config.insert(instance.clone(), stamps(&own.dir.join(CONFIG), None));
        }
    }

    fn plain_exit(&self, roots: &Roots, peers: &impl Peers, start: &Start, base: &Base) {
        let flags = flags_in(&start.dir);
        let own = start.own();
        if flags.options
            && let Ok(Some(text)) = read_text(&start.game.join(OPTIONS_FILE))
            && keys::readable(&text)
        {
            let changes: BTreeMap<String, String> = keys::changed(&base.options, &keys::map(&text)).into_iter().filter(|(k, _)| !sync::PER_INSTANCE_KEYS.contains(&k.as_str())).collect();
            if !changes.is_empty() {
                let _ = sync::instance_options(roots, peers, &start.instance, &changes);
                self.options_up(roots, peers, &own, &changes);
            }
        }
        if flags.servers
            && let Ok(Some(bytes)) = read_file(&start.game.join(SERVERS_FILE))
            && !bytes.is_empty()
            && base.servers.as_deref() != Some(bytes.as_slice())
        {
            let _ = sync::instance_servers(roots, peers, &start.instance, &bytes, None);
            self.servers_up(roots, peers, &own, bytes);
        }
    }

    /// Looks for games without the companion that loaded and for changed mod configs, at most every few seconds.
    pub(crate) fn tick(&mut self, now: Instant) {
        let shown = self.shown;
        let mut loaded = Vec::new();
        for (key, game) in &mut self.games {
            if game.start.companion {
                continue;
            }
            if let Some(base) = &mut game.base {
                learn_defaults(&game.start.game.join(OPTIONS_FILE), base);
                continue;
            }
            if game.shown.is_none() && shown(game.start.pid) {
                game.shown = Some(now);
            }
            if game.shown.is_some_and(|at| now.saturating_duration_since(at) >= SHOWN_GRACE) || logged_loaded(game) {
                loaded.push(key.clone());
            }
        }
        for key in loaded {
            self.loaded(&key);
        }
        if self.scanned.is_some_and(|at| now.saturating_duration_since(at) < SCAN_EVERY) {
            return;
        }
        self.scanned = Some(now);
        let waiting: Vec<Own> = self.config.iter().filter(|(id, c)| c.is_none() && self.games.values().filter(|g| &g.start.instance == *id).all(|g| g.base.is_some())).filter_map(|(id, _)| self.games.values().find(|g| &g.start.instance == id).map(|g| g.start.own())).collect();
        for own in waiting {
            if let Some(base) = stamps(&own.dir.join(CONFIG), None) {
                self.config.insert(own.id, Some(base));
            }
        }
        let owns: Vec<Own> = self.config.iter().filter(|(_, c)| c.is_some()).filter_map(|(id, _)| self.games.values().find(|g| &g.start.instance == id).map(|g| g.start.own())).collect();
        for own in owns {
            self.scan(&own);
        }
    }

    fn scan(&mut self, own: &Own) {
        let Some(Some(before)) = self.config.get(&own.id) else { return };
        let dir = own.dir.join(CONFIG);
        let Some(now) = stamps(&dir, Some(before)) else { return };
        let changed: Vec<String> = now.iter().filter(|(rel, s)| before.get(*rel).is_none_or(|b| b.sha != s.sha)).map(|(rel, _)| rel.clone()).collect();
        self.config.insert(own.id.clone(), Some(now));
        if changed.is_empty() || !flags_in(&own.dir).config {
            return;
        }
        let mut up = Vec::new();
        for rel in changed {
            let Ok(Some(bytes)) = read_file(&rel.split('/').fold(dir.clone(), |p, part| p.join(part))) else { continue };
            let full = format!("{CONFIG}/{rel}");
            let theirs = read_file(&self.mother.path(&full)).ok().flatten();
            if broken(&rel, theirs.as_deref(), &bytes) {
                continue;
            }
            up.push((full, bytes));
        }
        if !up.is_empty()
            && let Err(e) = put_files(&self.mother, own, up)
        {
            eprintln!("the mod settings of {} were not shared: {e:#}", own.name);
        }
    }
}

/// Takes the keys the game added to its options file since `base` saw it as defaults it filled in: a later change of
/// one of them is a choice that flows up, the key appearing is not.
fn learn_defaults(path: &Path, base: &mut Base) {
    let now = file_stamp(path);
    if now == base.seen {
        return;
    }
    base.seen = now;
    let Ok(Some(text)) = read_text(path) else { return };
    for (k, v) in keys::map(&text) {
        base.options.entry(k).or_insert(v);
    }
}

fn file_stamp(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()))
}

/// Whether `bytes` for config file `rel` look like a file a crash or a reset left: empty, or no longer text or JSON
/// where `before` was.
fn broken(rel: &str, before: Option<&[u8]>, bytes: &[u8]) -> bool {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return true;
    }
    let Some(before) = before else { return std::str::from_utf8(bytes).is_err() && !binary(rel) };
    let was_text = std::str::from_utf8(before).is_ok();
    if was_text && std::str::from_utf8(bytes).is_err() {
        return true;
    }
    let json = |b: &[u8]| serde_json::from_slice::<serde_json::Value>(b).is_ok();
    rel.ends_with(".json") && json(before) && !json(bytes)
}

fn binary(rel: &str) -> bool {
    [".dat", ".nbt", ".png", ".bin", ".db"].iter().any(|e| rel.ends_with(e))
}

/// The stamp of every file under `dir`, `None` when the folder cannot be read. A file that cannot be read keeps its stamp
/// from `before`, so a passing error never looks like a change.
fn stamps(dir: &Path, before: Option<&BTreeMap<String, Stamp>>) -> Option<BTreeMap<String, Stamp>> {
    let mut out = BTreeMap::new();
    for (rel, path) in files_in(dir).ok()? {
        let old = before.and_then(|b| b.get(&rel));
        let Ok(meta) = fs::metadata(&path) else {
            if let Some(old) = old {
                out.insert(rel, old.clone());
            }
            continue;
        };
        let (len, modified) = (meta.len(), meta.modified().ok());
        if let Some(old) = old
            && old.len == len
            && old.modified == modified
            && modified.is_some()
        {
            out.insert(rel, old.clone());
            continue;
        }
        match fs::read(&path) {
            Ok(bytes) => {
                out.insert(rel, Stamp { len, modified, sha: Sha256::digest(&bytes).into() });
            }
            Err(_) => {
                if let Some(old) = old {
                    out.insert(rel, old.clone());
                }
            }
        }
    }
    Some(out)
}

fn logged_loaded(game: &mut Game) -> bool {
    let path = game.start.game.join("logs").join("latest.log");
    let Ok(meta) = fs::metadata(&path) else { return false };
    if meta.modified().is_ok_and(|m| m < game.start.at) {
        return false;
    }
    if meta.len() < game.read {
        game.read = 0;
    }
    let Ok(mut file) = fs::File::open(&path) else { return false };
    let from = game.read.saturating_sub(64);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return false;
    }
    let mut chunk = Vec::new();
    if file.take(LOG_STEP).read_to_end(&mut chunk).is_err() {
        return false;
    }
    game.read = from + chunk.len() as u64;
    let text = String::from_utf8_lossy(&chunk);
    LOADED_MARKS.iter().any(|m| text.contains(m))
}

fn running_sharers(roots: &Roots, peers: &impl Peers, except: &str, wanted: impl Fn(super::Sharing) -> bool) -> Vec<String> {
    let mut out: Vec<String> = peers.connected().iter().filter_map(|k| peers.game(k)).map(|(i, _)| i).filter(|i| i != except && crate::core::instance::valid_id(i)).collect();
    out.sort();
    out.dedup();
    out.retain(|i| wanted(flags_in(&roots.instance(i))));
    out
}