//! Instance-wide game settings: the `options.txt` keys and the `servers.dat` one game saves reach the instance
//! template and every other running game of the instance, whichever batch it runs in.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};

use uuid::Uuid;

use super::instance::{self, games_dir};
use super::ipc::{self, Event, FromMod, ToMod};
use super::{net, paths};

/// Options every game keeps for itself: window, screen and session state.
pub const PER_INSTANCE_KEYS: &[&str] = &["fullscreen", "exclusiveFullscreen", "fullscreenResolution", "overrideWidth", "overrideHeight", "lastServer", "startedCleanly"];
/// File name of the game options in a game folder and of the instance template.
pub const OPTIONS_FILE: &str = "options.txt";
/// File name of the server list in a game folder and of the instance template.
pub const SERVERS_FILE: &str = "servers.dat";

static TEMPLATES: Mutex<()> = Mutex::new(());

/// Which instance and account each game plays, by key; the fleet fills it before a game starts.
#[derive(Clone, Default)]
pub struct Games(Arc<Mutex<HashMap<String, (String, String)>>>);

/// Records that the game `key` plays `instance` with `account`.
pub(super) fn register(games: &Games, key: &str, instance: &str, account: Uuid) {
    games.0.lock().unwrap_or_else(PoisonError::into_inner).insert(key.to_owned(), (instance.to_owned(), account.simple().to_string()));
}

trait Peers {
    fn connected(&self) -> Vec<String>;
    fn send(&self, key: &str, message: &ToMod);
    fn game(&self, key: &str) -> Option<(String, String)>;
}

struct Live {
    server: ipc::Server,
    games: Games,
}

impl Peers for Live {
    fn connected(&self) -> Vec<String> {
        self.server.connected()
    }

    fn send(&self, key: &str, message: &ToMod) {
        let _ = self.server.send(key, message);
    }

    fn game(&self, key: &str) -> Option<(String, String)> {
        self.games.0.lock().unwrap_or_else(PoisonError::into_inner).get(key).cloned()
    }
}

struct Roots {
    instances: PathBuf,
}

impl Roots {
    fn current() -> Self {
        Self { instances: paths::instances_dir() }
    }

    fn instance(&self, instance: &str) -> PathBuf {
        self.instances.join(instance)
    }

    fn game(&self, instance: &str, account: &str) -> PathBuf {
        games_dir(&self.instance(instance)).join(account)
    }
}

/// Takes the settings messages out of `raw` and passes every other event on, in order, on the returned receiver.
pub(super) fn spawn(server: ipc::Server, games: Games, raw: Receiver<Event>) -> io::Result<Receiver<Event>> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("game-sync".into()).spawn(move || {
        let roots = Roots::current();
        let server = Live { server, games };
        for event in raw {
            match event {
                Event::Message { instance, message: FromMod::Options { values } } => {
                    let _ = options_saved(&roots, &server, &instance, &values);
                }
                Event::Message { instance, message: FromMod::Servers } => {
                    let _ = servers_saved(&roots, &server, &instance);
                }
                event => {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            }
        }
    })?;
    Ok(rx)
}

fn game_of(peers: &impl Peers, key: &str) -> Option<(String, String)> {
    peers.game(key).filter(|(i, a)| instance::valid_id(i) && instance::valid_id(a))
}

fn in_instance(peers: &impl Peers, instance: &str) -> Vec<String> {
    peers.connected().into_iter().filter(|k| game_of(peers, k).is_some_and(|(i, _)| i == instance)).collect()
}

fn options_saved(roots: &Roots, peers: &impl Peers, key: &str, values: &BTreeMap<String, String>) -> Result<()> {
    let Some((instance, _)) = game_of(peers, key) else { return Ok(()) };
    let changes = {
        let _turn = TEMPLATES.lock().unwrap_or_else(PoisonError::into_inner);
        let template = roots.instance(&instance).join(OPTIONS_FILE);
        if !roots.instance(&instance).is_dir() {
            return Ok(());
        }
        let (text, changes) = match fs::read_to_string(&template) {
            Ok(text) => {
                let changes = changed(Some(&text), values);
                (text, changes)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (String::new(), changed(None, values)),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", template.display())),
        };
        if changes.is_empty() {
            return Ok(());
        }
        net::write_atomic(&template, apply(&text, &changes).as_bytes())?;
        changes
    };
    let message = ToMod::Options { values: changes };
    for game in in_instance(peers, &instance) {
        peers.send(&game, &message);
    }
    Ok(())
}

fn servers_saved(roots: &Roots, peers: &impl Peers, key: &str) -> Result<()> {
    let Some((instance, account)) = game_of(peers, key) else { return Ok(()) };
    let source = roots.game(&instance, &account).join(SERVERS_FILE);
    let bytes = fs::read(&source).with_context(|| format!("cannot read {}", source.display()))?;
    let targets: Vec<String> = in_instance(peers, &instance).into_iter().filter(|k| k != key).collect();
    {
        let _turn = TEMPLATES.lock().unwrap_or_else(PoisonError::into_inner);
        if !roots.instance(&instance).is_dir() {
            return Ok(());
        }
        net::write_atomic(&roots.instance(&instance).join(SERVERS_FILE), &bytes)?;
        for other in &targets {
            if let Some((_, account)) = game_of(peers, other) {
                let _ = net::write_atomic(&roots.game(&instance, &account).join(SERVERS_FILE), &bytes);
            }
        }
    }
    for other in &targets {
        peers.send(other, &ToMod::Servers);
    }
    Ok(())
}

fn key(line: &str) -> Option<&str> {
    line.split_once(':').map(|(k, _)| k)
}

fn shared_lines(text: &str) -> String {
    text.lines().filter(|l| key(l).is_none_or(|k| !PER_INSTANCE_KEYS.contains(&k))).map(|l| format!("{l}\n")).collect()
}

fn valid(key: &str, value: &str) -> bool {
    !key.is_empty() && !key.contains([':', '\n', '\r']) && !value.contains(['\n', '\r'])
}

/// The `values` that are shared and differ from `template`; all shared ones without a template.
fn changed(template: Option<&str>, values: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let current: BTreeMap<&str, &str> = template.unwrap_or_default().lines().filter_map(|l| l.split_once(':')).collect();
    let fresh = template.is_none();
    values
        .iter()
        .filter(|(k, v)| valid(k, v) && !PER_INSTANCE_KEYS.contains(&k.as_str()) && (fresh || current.get(k.as_str()) != Some(&v.as_str())))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `text` with every key in `changes` set: lines replaced in place, new keys appended.
fn apply(text: &str, changes: &BTreeMap<String, String>) -> String {
    let mut seen = Vec::new();
    let mut out: Vec<String> = text
        .lines()
        .map(|l| match key(l).and_then(|k| changes.get_key_value(k)) {
            Some((k, v)) => {
                seen.push(k.as_str());
                format!("{k}:{v}")
            }
            None => l.to_owned(),
        })
        .collect();
    out.extend(changes.iter().filter(|(k, _)| !seen.contains(&k.as_str())).map(|(k, v)| format!("{k}:{v}")));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// Without an instance options `template`, the newest `options.txt` among `dir` and `siblings`, without the keys every
/// game keeps for itself, becomes it, so that a new game starts from the instance's settings rather than spreading its
/// defaults. Every other `options.txt` that differs is first kept next to itself as `options.txt.old-N`.
pub(super) fn adopt_options(template: &Path, dir: &Path, siblings: &[PathBuf]) -> Result<()> {
    let _turn = TEMPLATES.lock().unwrap_or_else(PoisonError::into_inner);
    if fs::symlink_metadata(template).is_ok() {
        return Ok(());
    }
    let Some(bytes) = adopt(dir, siblings, OPTIONS_FILE)? else { return Ok(()) };
    net::write_atomic(template, shared_lines(&String::from_utf8_lossy(&bytes)).as_bytes())
}

/// Copies the instance server list `template` into the game folder `dir`. Without a template, the newest list among
/// `dir` and `siblings` becomes it, and every other list that differs is first kept next to itself as `servers.dat.old-N`.
pub(super) fn copy_servers(template: &Path, dir: &Path, siblings: &[PathBuf]) -> Result<()> {
    let _turn = TEMPLATES.lock().unwrap_or_else(PoisonError::into_inner);
    if fs::symlink_metadata(template).is_err() {
        let Some(bytes) = adopt(dir, siblings, SERVERS_FILE)? else { return Ok(()) };
        net::write_atomic(template, &bytes)?;
    }
    let bytes = fs::read(template).with_context(|| format!("cannot read {}", template.display()))?;
    let dest = dir.join(SERVERS_FILE);
    if fs::read(&dest).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    net::write_atomic(&dest, &bytes)
}

fn adopt(dir: &Path, siblings: &[PathBuf], name: &str) -> Result<Option<Vec<u8>>> {
    let files: Vec<PathBuf> = siblings.iter().map(PathBuf::as_path).chain([dir]).map(|d| d.join(name)).filter(|p| p.is_file()).collect();
    let Some(newest) = files.iter().max_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok()) else { return Ok(None) };
    let bytes = fs::read(newest).with_context(|| format!("cannot read {}", newest.display()))?;
    for file in &files {
        if fs::read(file).is_ok_and(|b| b != bytes) {
            let free = (1..).map(|n| file.with_file_name(format!("{name}.old-{n}"))).find(|p| fs::symlink_metadata(p).is_err()).context("no free name")?;
            fs::copy(file, &free).with_context(|| format!("cannot copy {} to {}", file.display(), free.display()))?;
        }
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const A: &str = "b1-a1";
    const B: &str = "b1-a2";
    const C: &str = "b1-a3";
    const OTHER: &str = "b2-a1";

    #[derive(Default)]
    struct Recorder {
        connected: Vec<String>,
        sent: RefCell<Vec<(String, ToMod)>>,
        games: HashMap<String, (String, String)>,
    }

    impl Peers for Recorder {
        fn connected(&self) -> Vec<String> {
            self.connected.clone()
        }

        fn send(&self, key: &str, message: &ToMod) {
            self.sent.borrow_mut().push((key.to_owned(), message.clone()));
        }

        fn game(&self, key: &str) -> Option<(String, String)> {
            if let Some(found) = self.games.get(key) {
                return Some(found.clone());
            }
            split_key(key).map(|(i, a)| (i.to_owned(), a.to_owned()))
        }
    }

    fn split_key(key: &str) -> Option<(&str, &str)> {
        let (instance, account) = key.split_once('-')?;
        let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric());
        (plain(instance) && plain(account)).then_some((instance, account))
    }

    struct World {
        _root: tempfile::TempDir,
        roots: Roots,
    }

    fn world() -> World {
        let root = tempfile::tempdir().unwrap();
        let roots = Roots { instances: root.path().join("instances") };
        for key in [A, B, C, OTHER] {
            let (b, a) = split_key(key).unwrap();
            fs::create_dir_all(roots.instance(b)).unwrap();
            fs::create_dir_all(roots.game(b, a)).unwrap();
        }
        World { _root: root, roots }
    }

    fn peers() -> Recorder {
        Recorder { connected: [A, B, C, OTHER].map(str::to_owned).to_vec(), ..Recorder::default() }
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn template(w: &World) -> String {
        fs::read_to_string(w.roots.instance("b1").join(OPTIONS_FILE)).unwrap()
    }

    #[test]
    fn apply_replaces_in_place_and_appends_new_keys() {
        let text = "version:4000\nfov:0.0\nkey_key.jump:key.keyboard.space\n";
        let out = apply(text, &map(&[("key_key.jump", "key.keyboard.j"), ("guiScale", "3")]));
        assert_eq!(out, "version:4000\nfov:0.0\nkey_key.jump:key.keyboard.j\nguiScale:3\n");
        assert_eq!(apply("", &map(&[("a", "1")])), "a:1\n");
        assert_eq!(apply("odd line\nresourcePacks:[\"vanilla\"]\n", &map(&[("resourcePacks", "[\"vanilla\",\"file/x.zip\"]")])), "odd line\nresourcePacks:[\"vanilla\",\"file/x.zip\"]\n");
    }

    #[test]
    fn changed_drops_equal_per_instance_and_broken_keys() {
        let text = "renderDistance:12\nguiScale:2\n";
        let values = map(&[("renderDistance", "12"), ("guiScale", "3"), ("fullscreen", "true"), ("overrideWidth", "800"), ("lastServer", "x"), ("bad\nkey", "1"), ("evil", "1\nfov:9"), ("a:b", "1"), ("", "1")]);
        assert_eq!(changed(Some(text), &values), map(&[("guiScale", "3")]));
        assert_eq!(changed(None, &values), map(&[("guiScale", "3"), ("renderDistance", "12")]));
    }

    #[test]
    fn a_save_updates_the_template_and_reaches_every_game_of_the_instance() {
        let w = world();
        fs::write(w.roots.instance("b1").join(OPTIONS_FILE), "renderDistance:12\nguiScale:2\nfullscreen:false\n").unwrap();
        let p = peers();
        options_saved(&w.roots, &p, A, &map(&[("renderDistance", "16"), ("guiScale", "2"), ("fullscreen", "true")])).unwrap();
        assert_eq!(template(&w), "renderDistance:16\nguiScale:2\nfullscreen:false\n");
        let sent = p.sent.borrow();
        let expect = ToMod::Options { values: map(&[("renderDistance", "16")]) };
        assert_eq!(*sent, vec![(A.to_owned(), expect.clone()), (B.to_owned(), expect.clone()), (C.to_owned(), expect)]);
    }

    #[test]
    fn an_applied_change_saved_again_does_not_echo() {
        let w = world();
        fs::write(w.roots.instance("b1").join(OPTIONS_FILE), "guiScale:2\n").unwrap();
        let p = peers();
        options_saved(&w.roots, &p, A, &map(&[("guiScale", "4")])).unwrap();
        assert_eq!(p.sent.borrow().len(), 3);
        options_saved(&w.roots, &p, B, &map(&[("guiScale", "4")])).unwrap();
        options_saved(&w.roots, &p, C, &map(&[("guiScale", "4"), ("fullscreen", "true")])).unwrap();
        assert_eq!(p.sent.borrow().len(), 3);
        assert_eq!(template(&w), "guiScale:4\n");
    }

    #[test]
    fn a_missing_template_holds_only_what_was_changed() {
        let w = world();
        fs::write(w.roots.game("b1", "a1").join(OPTIONS_FILE), "version:4000\nguiScale:3\nfullscreen:true\nfov:0.5\n").unwrap();
        let p = peers();
        options_saved(&w.roots, &p, A, &map(&[("guiScale", "3"), ("fullscreen", "true")])).unwrap();
        assert_eq!(template(&w), "guiScale:3\n");
        assert_eq!(p.sent.borrow().len(), 3);
        options_saved(&w.roots, &p, B, &map(&[("guiScale", "4")])).unwrap();
        assert_eq!(template(&w), "guiScale:4\n");
    }

    #[test]
    fn batches_sharing_an_instance_share_its_settings_and_others_do_not() {
        let w = world();
        let game = |i: &str, a: &str| (i.to_owned(), a.to_owned());
        let p = Recorder {
            connected: vec!["farm1-a1".into(), "spawn1-a2".into(), "i1-a3-solo".into(), "other1-a1".into()],
            games: HashMap::from([("farm1-a1".into(), game("b1", "a1")), ("spawn1-a2".into(), game("b1", "a2")), ("i1-a3-solo".into(), game("b1", "a3")), ("other1-a1".into(), game("b2", "a1"))]),
            ..Recorder::default()
        };
        options_saved(&w.roots, &p, "spawn1-a2", &map(&[("guiScale", "4")])).unwrap();
        assert_eq!(template(&w), "guiScale:4\n");
        let to: Vec<String> = p.sent.borrow().iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(to, ["farm1-a1", "spawn1-a2", "i1-a3-solo"]);
        assert!(!w.roots.instance("b2").join(OPTIONS_FILE).exists());
        fs::write(w.roots.game("b1", "a3").join(SERVERS_FILE), b"solo list").unwrap();
        p.sent.borrow_mut().clear();
        servers_saved(&w.roots, &p, "i1-a3-solo").unwrap();
        assert_eq!(fs::read(w.roots.game("b1", "a1").join(SERVERS_FILE)).unwrap(), b"solo list");
        assert_eq!(fs::read(w.roots.game("b1", "a2").join(SERVERS_FILE)).unwrap(), b"solo list");
        assert!(!w.roots.game("b2", "a1").join(SERVERS_FILE).exists());
        assert_eq!(*p.sent.borrow(), vec![("farm1-a1".to_owned(), ToMod::Servers), ("spawn1-a2".to_owned(), ToMod::Servers)]);
    }

    #[test]
    fn an_unknown_game_is_ignored() {
        let w = world();
        let p = Recorder { connected: vec![A.into()], games: HashMap::from([(A.to_owned(), ("../x".to_owned(), "a1".to_owned()))]), ..Recorder::default() };
        options_saved(&w.roots, &p, A, &map(&[("guiScale", "4")])).unwrap();
        assert!(p.sent.borrow().is_empty());
        let games = Games::default();
        register(&games, "k", "i1", Uuid::from_u128(1));
        let live = games.0.lock().unwrap().get("k").cloned();
        assert_eq!(live, Some(("i1".to_owned(), Uuid::from_u128(1).simple().to_string())));
    }

    #[test]
    fn a_deleted_instance_is_not_recreated() {
        let w = world();
        fs::remove_dir_all(w.roots.instance("b1")).unwrap();
        let p = peers();
        options_saved(&w.roots, &p, A, &map(&[("guiScale", "4")])).unwrap();
        assert!(!w.roots.instance("b1").exists());
        assert!(p.sent.borrow().is_empty());
    }

    #[test]
    fn keys_that_are_not_instances_are_ignored() {
        let w = world();
        for key in ["nodash", "../x-a1", "b1-", "-a1", "b1-a1/../.."] {
            options_saved(&w.roots, &peers(), key, &map(&[("guiScale", "4")])).unwrap();
        }
        assert!(!w.roots.instance("b1").join(OPTIONS_FILE).exists());
    }

    #[test]
    fn concurrent_saves_all_land_in_the_template() {
        let w = world();
        fs::write(w.roots.instance("b1").join(OPTIONS_FILE), "seed:0\n").unwrap();
        std::thread::scope(|s| {
            for i in 0..16 {
                let roots = &w.roots;
                s.spawn(move || {
                    let key = format!("k{i}");
                    for n in 0..10 {
                        options_saved(roots, &Recorder::default(), A, &map(&[(&key, &n.to_string()), ("shared", &format!("{i}-{n}"))])).unwrap();
                    }
                });
            }
        });
        let text = template(&w);
        for i in 0..16 {
            assert!(text.lines().any(|l| l == format!("k{i}:9")), "k{i} lost in {text}");
        }
        assert_eq!(text.lines().filter(|l| l.starts_with("shared:")).count(), 1);
        assert!(text.starts_with("seed:0\n"));
    }

    #[test]
    fn a_saved_server_list_reaches_the_template_and_the_running_games() {
        let w = world();
        fs::write(w.roots.game("b1", "a1").join(SERVERS_FILE), b"list A").unwrap();
        fs::write(w.roots.game("b1", "a2").join(SERVERS_FILE), b"old").unwrap();
        let p = Recorder { connected: vec![A.into(), B.into(), OTHER.into()], ..Recorder::default() };
        servers_saved(&w.roots, &p, A).unwrap();
        assert_eq!(fs::read(w.roots.instance("b1").join(SERVERS_FILE)).unwrap(), b"list A");
        assert_eq!(fs::read(w.roots.game("b1", "a2").join(SERVERS_FILE)).unwrap(), b"list A");
        assert!(!w.roots.game("b1", "a3").join(SERVERS_FILE).exists());
        assert!(!w.roots.game("b2", "a1").join(SERVERS_FILE).exists());
        assert_eq!(*p.sent.borrow(), vec![(B.to_owned(), ToMod::Servers)]);
    }

    #[test]
    fn the_newest_list_becomes_the_template_and_the_others_are_kept() {
        let w = world();
        let dir = |a: &str| w.roots.game("b1", a);
        fs::write(dir("a2").join(SERVERS_FILE), b"older").unwrap();
        fs::write(dir("a3").join(SERVERS_FILE), b"newest").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        fs::File::options().write(true).open(dir("a3").join(SERVERS_FILE)).unwrap().set_modified(later).unwrap();
        let template = w.roots.instance("b1").join(SERVERS_FILE);
        copy_servers(&template, &dir("a1"), &[dir("a2"), dir("a3")]).unwrap();
        assert_eq!(fs::read(&template).unwrap(), b"newest");
        assert_eq!(fs::read(dir("a1").join(SERVERS_FILE)).unwrap(), b"newest");
        assert_eq!(fs::read(dir("a2").join("servers.dat.old-1")).unwrap(), b"older");
        assert!(!dir("a3").join("servers.dat.old-1").exists());
        copy_servers(&template, &dir("a2"), &[dir("a1"), dir("a3")]).unwrap();
        assert_eq!(fs::read(dir("a2").join(SERVERS_FILE)).unwrap(), b"newest");
        assert!(!dir("a2").join("servers.dat.old-2").exists());
    }

    #[test]
    fn without_any_list_nothing_is_written() {
        let w = world();
        let template = w.roots.instance("b1").join(SERVERS_FILE);
        copy_servers(&template, &w.roots.game("b1", "a1"), &[]).unwrap();
        assert!(!template.exists());
        assert!(!w.roots.game("b1", "a1").join(SERVERS_FILE).exists());
    }

    #[test]
    fn the_newest_options_become_the_missing_template_once() {
        let w = world();
        let dir = |a: &str| w.roots.game("b1", a);
        let template = w.roots.instance("b1").join(OPTIONS_FILE);
        adopt_options(&template, &dir("a1"), &[dir("a2")]).unwrap();
        assert!(!template.exists());
        fs::write(dir("a2").join(OPTIONS_FILE), "guiScale:3\nfullscreen:true\nlastServer:x\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        fs::File::options().write(true).open(dir("a2").join(OPTIONS_FILE)).unwrap().set_modified(later).unwrap();
        fs::write(dir("a3").join(OPTIONS_FILE), "guiScale:1\n").unwrap();
        adopt_options(&template, &dir("a1"), &[dir("a2"), dir("a3")]).unwrap();
        assert_eq!(fs::read_to_string(&template).unwrap(), "guiScale:3\n");
        assert_eq!(fs::read_to_string(dir("a3").join("options.txt.old-1")).unwrap(), "guiScale:1\n");
        assert!(!dir("a2").join("options.txt.old-1").exists());
        fs::write(dir("a3").join(OPTIONS_FILE), "guiScale:9\n").unwrap();
        adopt_options(&template, &dir("a3"), &[dir("a2")]).unwrap();
        assert_eq!(fs::read_to_string(&template).unwrap(), "guiScale:3\n");
    }
}
