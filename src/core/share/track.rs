//! What runs: when each game loaded, the baseline of its options then, and what flows from it to the sync folder after.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::{Mother, Own, flags_in, keys, put_options, read_file, read_text};
use crate::core::sync::{self, OPTIONS_FILE, Peers, Roots, SERVERS_FILE};

const SHOWN_GRACE: Duration = Duration::from_secs(180);
/// How long after loading a game's first start may still save what it detected; the game does so within seconds, a
/// person changing options takes longer.
const FRESH_GRACE: Duration = Duration::from_secs(10);
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
    /// The game folder had no options file of its own: the game fills in its first-start defaults after loading.
    pub fresh: bool,
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
    saved_early: bool,
}

#[derive(Debug)]
struct Base {
    options: BTreeMap<String, String>,
    servers: Option<Vec<u8>>,
    seen: Option<(u64, Option<SystemTime>)>,
    at: Instant,
    settled: bool,
}

impl Base {
    /// Whether what the game writes now is still its own first-start setup rather than a choice: only its first save
    /// after loading, and only within seconds.
    fn settling(&self, start: &Start, now: Instant) -> bool {
        start.fresh && !self.settled && now.saturating_duration_since(self.at) < FRESH_GRACE
    }
}

/// The running games as sharing follows them.
pub(crate) struct Tracker {
    mother: Mother,
    shown: fn(u32) -> bool,
    games: HashMap<String, Game>,
}

impl Tracker {
    pub(crate) fn new(mother: Mother, shown: fn(u32) -> bool) -> Self {
        Self { mother, shown, games: HashMap::new() }
    }

    /// Follows a game that started; until it loaded nothing it writes is trusted.
    pub(crate) fn started(&mut self, roots: &Roots, peers: &impl Peers, start: Start) {
        if let Some(old) = self.games.get(&start.key).map(|g| g.start.pid) {
            self.ended(roots, peers, &start.key.clone(), old);
        }
        let key = start.key.clone();
        let adopted = start.adopted;
        self.games.insert(key.clone(), Game { start, shown: None, read: 0, base: None, saved_early: false });
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
        game.base = Some(Base { options, servers, seen, at: Instant::now(), settled: game.saved_early });
    }

    /// The game `key` saved the options `values` changed. The companion leaves out keys the save only added, so what
    /// arrives is a choice, except the first save of a first start, which holds what the game detected.
    pub(crate) fn options(&mut self, roots: &Roots, peers: &impl Peers, key: &str, values: &BTreeMap<String, String>) {
        let Some(game) = self.games.get_mut(key) else { return };
        let Some(base) = game.base.as_mut() else {
            game.saved_early = true;
            return;
        };
        let start = game.start.clone();
        let settling = base.settling(&start, Instant::now());
        base.settled = true;
        base.options.extend(values.iter().map(|(k, v)| (k.clone(), v.clone())));
        if settling {
            return;
        }
        if let Err(e) = sync::instance_options(roots, peers, &start.instance, values) {
            eprintln!("the options of {} were not kept: {e:#}", start.name);
        }
        if values.is_empty() || !flags_in(&self.mother, &start.dir).options {
            return;
        }
        self.options_up(roots, peers, &start.own(), values);
    }

    fn options_up(&self, roots: &Roots, peers: &impl Peers, own: &Own, changes: &BTreeMap<String, String>) {
        let applied = match put_options(&self.mother, changes) {
            Ok(applied) => applied,
            Err(e) => {
                eprintln!("the options of {} were not shared: {e:#}", own.name);
                return;
            }
        };
        if applied.is_empty() {
            return;
        }
        for other in self.running_sharers(roots, peers, &own.id) {
            sync::send_options(peers, &other, &applied);
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
    }

    /// The game `key` with process `pid` ended: what it changed since its baseline flows up a last time.
    pub(crate) fn ended(&mut self, roots: &Roots, peers: &impl Peers, key: &str, pid: u32) {
        if self.games.get(key).is_none_or(|g| g.start.pid != pid) {
            return;
        }
        let Some(game) = self.games.remove(key) else { return };
        if let Some(base) = &game.base
            && !game.start.companion
        {
            self.plain_exit(roots, peers, &game.start, base);
        }
    }

    fn plain_exit(&self, roots: &Roots, peers: &impl Peers, start: &Start, base: &Base) {
        let own = start.own();
        if let Ok(Some(text)) = read_text(&start.game.join(OPTIONS_FILE))
            && keys::readable(&text)
        {
            let changes: BTreeMap<String, String> = keys::changed(&base.options, &keys::map(&text)).into_iter().filter(|(k, _)| !sync::PER_INSTANCE_KEYS.contains(&k.as_str())).collect();
            if !changes.is_empty() {
                let _ = sync::instance_options(roots, peers, &start.instance, &changes);
                if flags_in(&self.mother, &start.dir).options {
                    self.options_up(roots, peers, &own, &changes);
                }
            }
        }
        if let Ok(Some(bytes)) = read_file(&start.game.join(SERVERS_FILE))
            && !bytes.is_empty()
            && base.servers.as_deref() != Some(bytes.as_slice())
        {
            let _ = sync::instance_servers(roots, peers, &start.instance, &bytes, None);
        }
    }

    /// Looks for games without the companion that loaded, at most every few seconds.
    pub(crate) fn tick(&mut self, now: Instant) {
        let shown = self.shown;
        let mut loaded = Vec::new();
        for (key, game) in &mut self.games {
            if game.start.companion {
                continue;
            }
            if let Some(base) = &mut game.base {
                let settling = base.settling(&game.start, now);
                learn_defaults(&game.start.game.join(OPTIONS_FILE), base, settling);
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
    }

    fn running_sharers(&self, roots: &Roots, peers: &impl Peers, except: &str) -> Vec<String> {
        let mut out: Vec<String> = peers.connected().iter().filter_map(|k| peers.game(k)).map(|(i, _)| i).filter(|i| i != except && crate::core::instance::valid_id(i)).collect();
        out.sort();
        out.dedup();
        out.retain(|i| flags_in(&self.mother, &roots.instance(i)).options);
        out
    }
}

/// Takes the keys the game added to its options file since `base` saw it as defaults it filled in: a later change of
/// one of them is a choice that flows up, the key appearing is not. While `settling`, every value counts as a default.
fn learn_defaults(path: &Path, base: &mut Base, settling: bool) {
    let now = file_stamp(path);
    if now == base.seen {
        return;
    }
    let Ok(Some(text)) = read_text(path) else { return };
    if !keys::readable(&text) || file_stamp(path) != now {
        return;
    }
    base.seen = now;
    base.settled |= settling;
    for (k, v) in keys::map(&text) {
        if settling {
            base.options.insert(k, v);
        } else {
            base.options.entry(k).or_insert(v);
        }
    }
}

fn file_stamp(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()))
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

