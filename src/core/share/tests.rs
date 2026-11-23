use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use uuid::Uuid;

use super::*;
use crate::core::instance::{Loader, dir_in, save_in};
use crate::core::ipc::ToMod;
use crate::core::sync::{Peers, Roots};

#[derive(Default)]
struct Recorder {
    connected: Vec<String>,
    games: HashMap<String, (String, String)>,
    sent: RefCell<Vec<(String, ToMod)>>,
}

impl Peers for Recorder {
    fn connected(&self) -> Vec<String> {
        self.connected.clone()
    }

    fn send(&self, key: &str, message: &ToMod) {
        self.sent.borrow_mut().push((key.to_owned(), message.clone()));
    }

    fn game(&self, key: &str) -> Option<(String, String)> {
        self.games.get(key).cloned()
    }
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    mother: Mother,
    roots: Roots,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    World { mother: Mother::at(root.join(DIR)), roots: Roots { instances: root.join("instances") }, root, _tmp: tmp }
}

const ACCOUNT: &str = "0000000000000000000000000000000a";

impl World {
    fn instance(&self, name: &str, sync: Sharing) -> Own {
        let made = Instance { sync, ..Instance::new(name, "26.3", Loader::Vanilla) };
        save_in(&self.roots.instances, &made).unwrap();
        let dir = dir_in(&self.roots.instances, &made.id);
        Own { id: made.id, name: made.name, dir }
    }

    fn game(&self, own: &Own) -> PathBuf {
        let dir = own.dir.join("games").join(ACCOUNT);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mother(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.mother.path(rel)).ok()
    }

    fn put_mother(&self, rel: &str, text: &str) {
        let path = self.mother.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn tracker(&self) -> Tracker {
        Tracker::new(self.mother.clone(), |_| false)
    }

    fn peers(&self, games: &[(&str, &Own)]) -> Recorder {
        Recorder {
            connected: games.iter().map(|(k, _)| (*k).to_owned()).collect(),
            games: games.iter().map(|(k, o)| ((*k).to_owned(), (o.id.clone(), ACCOUNT.to_owned()))).collect(),
            ..Recorder::default()
        }
    }
}

fn start(key: &str, own: &Own, game: &Path, companion: bool) -> Start {
    Start { key: key.into(), instance: own.id.clone(), name: own.name.clone(), dir: own.dir.clone(), game: game.to_path_buf(), pid: 1, companion, at: SystemTime::now(), adopted: false }
}

fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn scan_now(t: &mut Tracker) {
    t.tick(Instant::now() + Duration::from_secs(3600));
}

#[test]
fn a_reset_at_start_never_reaches_the_sync_folder() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    w.put_mother("config/mod.json", "{\"a\":1}");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    assert!(read(&a.dir.join("options.txt")).contains("fov:0.5"));
    assert_eq!(read(&a.dir.join("config").join("mod.json")), "{\"a\":1}");
    let game = w.game(&a);
    let p = w.peers(&[("k1", &a)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, true));
    fs::write(a.dir.join("config").join("mod.json"), "{}").unwrap();
    t.options(&w.roots, &p, "k1", &values(&[("fov", "0.0")]));
    t.loaded("k1");
    scan_now(&mut t);
    t.ended(&w.roots, &p, "k1", 1);
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.5\n");
    assert_eq!(w.mother("config/mod.json").unwrap(), "{\"a\":1}");
    assert!(read(&a.dir.join("options.txt")).contains("fov:0.5"));
    inbound_in(&w.mother, &a, false).unwrap();
    assert_eq!(read(&a.dir.join("config").join("mod.json")), "{\"a\":1}");
}

#[test]
fn a_missing_options_file_the_game_recreates_never_reaches_the_sync_folder() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    let game = w.game(&a);
    let p = w.peers(&[]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, false));
    fs::write(game.join("options.txt"), "version:4440\nfov:0.0\ngamma:0.5\n").unwrap();
    t.loaded("k1");
    t.ended(&w.roots, &p, "k1", 1);
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.5\n");
    assert!(read(&a.dir.join("options.txt")).contains("fov:0.5"));
}

#[test]
fn a_change_after_loading_reaches_the_sync_folder_and_the_next_start_of_another_instance() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    let (a, b) = (w.instance("A", Sharing::ALL), w.instance("B", Sharing::ALL));
    fs::write(b.dir.join("options.txt"), "version:3955\nfov:0.1\nguiScale:1\n").unwrap();
    inbound_in(&w.mother, &a, false).unwrap();
    let game = w.game(&a);
    let p = w.peers(&[("k1", &a)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, true));
    fs::write(game.join("options.txt"), "fov:0.5
").unwrap();
    t.loaded("k1");
    t.options(&w.roots, &p, "k1", &values(&[("fov", "0.9")]));
    assert!(w.mother("options.txt").unwrap().contains("fov:0.9"));
    assert!(read(&a.dir.join("options.txt")).contains("fov:0.9"));
    let plain = a.dir.join("games").join("other");
    fs::create_dir_all(&plain).unwrap();
    t.started(&w.roots, &p, start("k2", &a, &plain, false));
    fs::write(plain.join("options.txt"), "fov:0.9\nguiScale:2\n").unwrap();
    t.loaded("k2");
    fs::write(plain.join("options.txt"), "fov:0.9\nguiScale:3\n").unwrap();
    t.ended(&w.roots, &p, "k2", 1);
    assert!(w.mother("options.txt").unwrap().contains("guiScale:3"));
    inbound_in(&w.mother, &b, false).unwrap();
    let theirs = read(&b.dir.join("options.txt"));
    assert!(theirs.contains("fov:0.9") && theirs.contains("guiScale:3") && theirs.contains("version:3955"), "{theirs}");
}

#[test]
fn a_removed_key_does_not_propagate() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\ngamma:0.5\n");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    let game = w.game(&a);
    let p = w.peers(&[]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, false));
    fs::write(game.join("options.txt"), "fov:0.5\ngamma:0.5\n").unwrap();
    t.loaded("k1");
    fs::write(game.join("options.txt"), "fov:0.7\n").unwrap();
    t.ended(&w.roots, &p, "k1", 1);
    let shared = w.mother("options.txt").unwrap();
    assert!(shared.contains("gamma:0.5") && shared.contains("fov:0.7"), "{shared}");
}

#[test]
fn per_instance_keys_never_move() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    let (a, b) = (w.instance("A", Sharing::ALL), w.instance("B", Sharing::ALL));
    fs::write(b.dir.join("options.txt"), "version:3955\nfullscreen:false\nresourcePacks:[\"own\"]\n").unwrap();
    inbound_in(&w.mother, &a, false).unwrap();
    let game = w.game(&a);
    let p = w.peers(&[("k1", &a)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, true));
    fs::write(game.join("options.txt"), "fov:0.5
fullscreen:false
").unwrap();
    t.loaded("k1");
    t.options(&w.roots, &p, "k1", &values(&[("fullscreen", "true"), ("lastServer", "x"), ("version", "9999"), ("resourcePacks", "[\"file/a.zip\"]"), ("incompatibleResourcePacks", "[]"), ("fov", "0.7")]));
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.7\n");
    inbound_in(&w.mother, &b, false).unwrap();
    assert_eq!(read(&b.dir.join("options.txt")), "version:3955\nfullscreen:false\nresourcePacks:[\"own\"]\nfov:0.7\n");
}

#[test]
fn an_instance_that_opted_out_neither_reads_nor_writes() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    w.put_mother("servers.dat", "shared list");
    w.put_mother("config/mod.json", "shared");
    let c = w.instance("C", Sharing::NONE);
    fs::write(c.dir.join("options.txt"), "fov:0.1\n").unwrap();
    fs::create_dir_all(c.dir.join("config")).unwrap();
    fs::write(c.dir.join("config").join("mod.json"), "own").unwrap();
    inbound_in(&w.mother, &c, false).unwrap();
    assert_eq!(read(&c.dir.join("options.txt")), "fov:0.1\n");
    assert_eq!(read(&c.dir.join("config").join("mod.json")), "own");
    assert!(!c.dir.join("servers.dat").exists());
    let game = w.game(&c);
    let p = w.peers(&[("k1", &c)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &c, &game, true));
    t.loaded("k1");
    t.options(&w.roots, &p, "k1", &values(&[("fov", "0.2")]));
    fs::write(c.dir.join("config").join("mod.json"), "changed").unwrap();
    fs::write(game.join("servers.dat"), "own list").unwrap();
    t.servers(&w.roots, &p, "k1");
    scan_now(&mut t);
    t.ended(&w.roots, &p, "k1", 1);
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.5\n");
    assert_eq!(w.mother("servers.dat").unwrap(), "shared list");
    assert_eq!(w.mother("config/mod.json").unwrap(), "shared");
    assert_eq!(read(&c.dir.join("options.txt")), "fov:0.2\n");
}

#[test]
fn turning_sharing_on_keeps_the_own_copy_first() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    let a = w.instance("A", Sharing::NONE);
    fs::write(a.dir.join("options.txt"), "fov:0.1\n").unwrap();
    fs::create_dir_all(a.dir.join("config")).unwrap();
    fs::write(a.dir.join("config").join("mod.json"), "mine").unwrap();
    let made = set_in(&w.mother, &w.roots.instances, &a.id, Sharing::ALL).unwrap();
    assert_eq!(made.sync, Sharing::ALL);
    assert_eq!(crate::core::instance::load_all_in(&w.roots.instances).unwrap()[0].sync, Sharing::ALL);
    let own: Vec<Version> = history::versions(&w.mother, Item::Options).unwrap().into_iter().filter(|v| v.own).collect();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].source.as_deref(), Some("A"));
    assert_eq!(history::files(&w.mother, &own[0]).unwrap(), vec![("options.txt".to_owned(), b"fov:0.1\n".to_vec())]);
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.5\n");
    assert_eq!(w.mother("config/mod.json").unwrap(), "mine");
    assert_eq!(read(&a.dir.join("options.txt")), "fov:0.1\n");
}

#[test]
fn the_history_keeps_fifty_versions_and_restores_one() {
    let w = world();
    let a = w.instance("A", Sharing::ALL);
    for n in 0..60 {
        put_options(&w.mother, &a, &values(&[("fov", &n.to_string())])).unwrap();
    }
    let kept = versions_in(&w.mother, Item::Options);
    assert_eq!(kept.len(), history::KEEP);
    assert_eq!(history::files(&w.mother, &kept[0]).unwrap()[0].1, b"fov:58\n");
    assert_eq!(kept[0].source.as_deref(), Some("A"));
    let old = kept[20].clone();
    let text = String::from_utf8(history::files(&w.mother, &old).unwrap()[0].1.clone()).unwrap();
    restore_in(&w.mother, &old).unwrap();
    assert_eq!(w.mother("options.txt").unwrap(), text);
    let after = versions_in(&w.mother, Item::Options);
    assert_eq!(after.len(), history::KEEP);
    assert_eq!(history::files(&w.mother, &after[0]).unwrap()[0].1, b"fov:59\n");
}

fn versions_in(mother: &Mother, item: Item) -> Vec<Version> {
    history::versions(mother, item).unwrap()
}

#[test]
fn concurrent_writes_end_with_the_last_writer_and_a_whole_file() {
    let w = world();
    let (a, b) = (w.instance("A", Sharing::ALL), w.instance("B", Sharing::ALL));
    std::thread::scope(|s| {
        for i in 0..8 {
            let (mother, own) = (&w.mother, if i % 2 == 0 { &a } else { &b });
            s.spawn(move || {
                for n in 0..10 {
                    put_options(mother, own, &values(&[(&format!("k{i}"), &n.to_string()), ("shared", &format!("{i}-{n}"))])).unwrap();
                    put_files(mother, own, vec![("config/x.json".into(), format!("{{\"by\":{i},\"n\":{n}}}").repeat(200).into_bytes())]).unwrap();
                }
            });
        }
    });
    let text = w.mother("options.txt").unwrap();
    for i in 0..8 {
        assert!(text.lines().any(|l| l == format!("k{i}:9")), "k{i} lost in {text}");
    }
    assert_eq!(text.lines().filter(|l| l.starts_with("shared:")).count(), 1);
    let config = w.mother("config/x.json").unwrap();
    assert!((0..8).any(|i| config == format!("{{\"by\":{i},\"n\":9}}").repeat(200)), "torn file");
}

fn played(w: &World, own: &Own, ago: u64) {
    let logs = w.game(own).join("logs");
    fs::create_dir_all(&logs).unwrap();
    let log = logs.join("latest.log");
    fs::write(&log, "x").unwrap();
    fs::File::options().write(true).open(&log).unwrap().set_modified(SystemTime::now() - Duration::from_secs(ago)).unwrap();
}

#[test]
fn turning_on_many_takes_the_most_recently_played_and_keeps_the_others() {
    let w = world();
    let (a, b, c) = (w.instance("A", Sharing::NONE), w.instance("B", Sharing::NONE), w.instance("C", Sharing::NONE));
    for (own, fov) in [(&a, "0.1"), (&b, "0.2"), (&c, "0.3")] {
        fs::write(own.dir.join("options.txt"), format!("version:3955\nfov:{fov}\n")).unwrap();
        fs::create_dir_all(own.dir.join("config")).unwrap();
        fs::write(own.dir.join("config").join("mod.json"), fov).unwrap();
    }
    played(&w, &a, 3000);
    played(&w, &b, 10);
    played(&w, &c, 600);
    let ids = vec![a.id.clone(), b.id.clone(), c.id.clone()];
    let made = turn_on_in(&w.mother, &w.roots.instances, &ids).unwrap();
    assert!(made.iter().all(|i| i.sync == Sharing::ALL));
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.2\n");
    assert_eq!(w.mother("config/mod.json").unwrap(), "0.2");
    let own: Vec<String> = history::versions(&w.mother, Item::Options).unwrap().into_iter().filter(|v| v.own).filter_map(|v| v.source).collect();
    for name in ["A", "B", "C"] {
        assert!(own.iter().any(|s| s == name), "{name} not kept: {own:?}");
    }
    assert_eq!(read(&a.dir.join("options.txt")), "version:3955\nfov:0.1\n");
}

fn tree(root: &Path, skip: &[&str]) -> BTreeMap<PathBuf, (Vec<u8>, Option<SystemTime>)> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for e in fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if skip.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                todo.push(path);
            } else {
                out.insert(rel, (fs::read(&path).unwrap_or_default(), meta.modified().ok()));
            }
        }
    }
    out
}

#[test]
fn nothing_outside_instance_folders_and_the_sync_folder_is_touched() {
    let w = world();
    for (rel, text) in [("settings.json", "{}"), ("batches/b1/options.txt", "soundCategory_master:0.0\n"), ("other/x.txt", "x"), ("accounts.json", "[]")] {
        let path = w.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let a = w.instance("A", Sharing::NONE);
    let b = w.instance("B", Sharing::ALL);
    fs::write(a.dir.join("options.txt"), "fov:0.1\n").unwrap();
    let before = tree(&w.root, &["instances", "sync"]);
    set_in(&w.mother, &w.roots.instances, &a.id, Sharing::ALL).unwrap();
    turn_on_in(&w.mother, &w.roots.instances, &[b.id.clone()]).unwrap();
    inbound_in(&w.mother, &b, false).unwrap();
    let game = w.game(&b);
    let p = w.peers(&[("k1", &b)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &b, &game, false));
    t.loaded("k1");
    fs::write(game.join("options.txt"), "fov:0.4\n").unwrap();
    t.ended(&w.roots, &p, "k1", 1);
    let first = versions_in(&w.mother, Item::Options).pop().unwrap();
    restore_in(&w.mother, &first).unwrap();
    assert_eq!(tree(&w.root, &["instances", "sync"]), before);
}

#[test]
fn a_game_that_ends_before_loading_sends_nothing_up() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    w.put_mother("servers.dat", "list");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    let template = read(&a.dir.join("options.txt"));
    let game = w.game(&a);
    let p = w.peers(&[("k1", &a)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, false));
    fs::write(game.join("options.txt"), "fov:0.0\n").unwrap();
    fs::write(game.join("servers.dat"), "").unwrap();
    t.ended(&w.roots, &p, "k1", 1);
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.5\n");
    assert_eq!(w.mother("servers.dat").unwrap(), "list");
    assert_eq!(read(&a.dir.join("options.txt")), template);
}

#[test]
fn a_hidden_muted_batch_game_never_moves_its_background_values() {
    let w = world();
    w.put_mother("options.txt", "renderDistance:12\nsoundCategory_master:1.0\n");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    let template = read(&a.dir.join("options.txt"));
    let game = w.game(&a);
    fs::write(game.join("options.txt"), &template).unwrap();
    let p = w.peers(&[("k1", &a)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, true));
    t.loaded("k1");
    fs::write(game.join("options.txt"), "renderDistance:2\nsoundCategory_master:0.0\n").unwrap();
    t.ended(&w.roots, &p, "k1", 1);
    assert_eq!(w.mother("options.txt").unwrap(), "renderDistance:12\nsoundCategory_master:1.0\n");
    assert_eq!(read(&a.dir.join("options.txt")), template);
}

#[test]
fn a_server_list_saved_after_loading_reaches_the_sync_folder_and_running_sharers() {
    let w = world();
    let (a, b) = (w.instance("A", Sharing::ALL), w.instance("B", Sharing::ALL));
    let (ga, gb) = (w.game(&a), w.game(&b));
    let p = w.peers(&[("k1", &a), ("k2", &b)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &ga, true));
    t.started(&w.roots, &p, start("k2", &b, &gb, true));
    t.loaded("k1");
    t.loaded("k2");
    fs::write(ga.join("servers.dat"), "new list").unwrap();
    t.servers(&w.roots, &p, "k1");
    assert_eq!(w.mother("servers.dat").unwrap(), "new list");
    assert_eq!(read(&b.dir.join("servers.dat")), "new list");
    assert_eq!(read(&gb.join("servers.dat")), "new list");
    assert!(p.sent.borrow().contains(&("k2".to_owned(), ToMod::Servers)));
}

#[test]
fn a_mod_config_changed_after_loading_reaches_the_sync_folder_and_an_empty_one_does_not() {
    let w = world();
    w.put_mother("config/a.toml", "x = 1\n");
    w.put_mother("config/b.toml", "y = 1\n");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    let game = w.game(&a);
    let p = w.peers(&[("k1", &a)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, true));
    t.loaded("k1");
    fs::write(a.dir.join("config").join("a.toml"), "x = 2\n").unwrap();
    fs::write(a.dir.join("config").join("b.toml"), "").unwrap();
    fs::create_dir_all(a.dir.join("config").join("sub")).unwrap();
    fs::write(a.dir.join("config").join("sub").join("new.json"), "{}").unwrap();
    fs::remove_file(a.dir.join("config").join("a.toml")).ok();
    fs::write(a.dir.join("config").join("a.toml"), "x = 2\n").unwrap();
    scan_now(&mut t);
    assert_eq!(w.mother("config/a.toml").unwrap(), "x = 2\n");
    assert_eq!(w.mother("config/b.toml").unwrap(), "y = 1\n");
    assert_eq!(w.mother("config/sub/new.json").unwrap(), "{}");
    t.ended(&w.roots, &p, "k1", 1);
}

#[test]
fn a_joining_game_leaves_the_running_config_alone() {
    let w = world();
    w.put_mother("config/a.toml", "x = 1\n");
    let a = w.instance("A", Sharing::ALL);
    fs::create_dir_all(a.dir.join("config")).unwrap();
    fs::write(a.dir.join("config").join("a.toml"), "x = 5\n").unwrap();
    inbound_in(&w.mother, &a, true).unwrap();
    assert_eq!(read(&a.dir.join("config").join("a.toml")), "x = 5\n");
}

fn template_and_game(w: &World) -> (PathBuf, PathBuf) {
    let a = w.instance("A", Sharing::ALL);
    fs::write(a.dir.join("options.txt"), "fov:0.5\nguiScale:3\n").unwrap();
    let game = w.game(&a).join("options.txt");
    (a.dir.join("options.txt"), game)
}

fn holds_template(game: &Path) {
    let text = read(game);
    assert!(text.contains("fov:0.5") && text.contains("guiScale:3") && text.contains("version:4440"), "{text}");
}

#[test]
fn a_deleted_game_options_file_starts_with_the_shared_values() {
    let w = world();
    let (template, game) = template_and_game(&w);
    verify_game(&template, &game, Some(4440)).unwrap();
    holds_template(&game);
}

#[test]
fn a_garbage_game_options_file_starts_with_the_shared_values() {
    let w = world();
    let (template, game) = template_and_game(&w);
    fs::write(&game, b"\x00\xff\xfe garbage").unwrap();
    verify_game(&template, &game, Some(4440)).unwrap();
    holds_template(&game);
    fs::write(&game, "fov:0.0\nguiScale:1\n").unwrap();
    verify_game(&template, &game, Some(4440)).unwrap();
    holds_template(&game);
}

#[test]
fn a_read_only_game_options_file_starts_with_the_shared_values() {
    let w = world();
    let (template, game) = template_and_game(&w);
    fs::write(&game, "fov:0.0\n").unwrap();
    let mut perms = fs::metadata(&game).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&game, perms).unwrap();
    verify_game(&template, &game, Some(4440)).unwrap();
    holds_template(&game);
}

#[test]
fn a_dangling_link_as_game_options_file_starts_with_the_shared_values() {
    let w = world();
    let (template, game) = template_and_game(&w);
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(w.root.join("nowhere.txt"), &game);
    #[cfg(not(windows))]
    let linked = std::os::unix::fs::symlink(w.root.join("nowhere.txt"), &game);
    if linked.is_err() {
        return;
    }
    verify_game(&template, &game, Some(4440)).unwrap();
    holds_template(&game);
    assert!(!w.root.join("nowhere.txt").exists());
}

#[test]
fn a_game_folder_that_cannot_take_the_options_refuses_the_start() {
    let w = world();
    let (template, game) = template_and_game(&w);
    fs::create_dir_all(game.join("blocked")).unwrap();
    assert!(verify_game(&template, &game, Some(4440)).is_err());
}

#[test]
fn old_instance_files_share_nothing_and_new_instances_share_everything() {
    let old: Instance = serde_json::from_str(r#"{"id":"a1","name":"Alt","game_version":"1.21"}"#).unwrap();
    assert_eq!(old.sync, Sharing::NONE);
    assert_eq!(Instance::new("Neu", "1.21", Loader::Vanilla).sync, Sharing::ALL);
    let partial: Instance = serde_json::from_str(r#"{"id":"a1","name":"Alt","game_version":"1.21","sync":{"options":true}}"#).unwrap();
    assert_eq!(partial.sync, Sharing { options: true, config: false, servers: false });
    let text = serde_json::to_string(&Instance::new("Neu", "1.21", Loader::Vanilla)).unwrap();
    assert!(text.contains("\"sync\":{\"options\":true,\"config\":true,\"servers\":true}"), "{text}");
    let _ = Uuid::nil();
}
fn old_instance(w: &World, name: &str, game: &str) -> Own {
    let made = Instance { sync: Sharing::ALL, ..Instance::new(name, game, Loader::Vanilla) };
    save_in(&w.roots.instances, &made).unwrap();
    let dir = dir_in(&w.roots.instances, &made.id);
    Own { id: made.id, name: made.name, dir }
}

fn own_files(w: &World, item: Item) -> Vec<Vec<u8>> {
    history::versions(&w.mother, item).unwrap().into_iter().filter(|v| v.own).flat_map(|v| history::files(&w.mother, &v).unwrap().into_iter().map(|(_, b)| b)).collect()
}

#[test]
fn options_travel_from_1_13_on() {
    for game in ["1.13", "1.13-pre1", "1.20.4", "26.3", "26w14a", "17w43a", "18w22c"] {
        assert!(options_travel(game), "{game}");
    }
    for game in ["1.12.2", "1.8.9", "17w31a", "b1.7.3", "rd-132211", "", "wild"] {
        assert!(!options_travel(game), "{game}");
    }
}

#[test]
fn options_of_games_before_1_13_move_neither_way() {
    let w = world();
    let old = old_instance(&w, "Old", "1.12.2");
    fs::write(old.dir.join("options.txt"), "key_key.jump:57\nlang:en_US\n").unwrap();
    inbound_in(&w.mother, &old, false).unwrap();
    assert_eq!(w.mother("options.txt"), None);
    w.put_mother("options.txt", "key_key.jump:key.keyboard.space\n");
    inbound_in(&w.mother, &old, false).unwrap();
    assert_eq!(read(&old.dir.join("options.txt")), "key_key.jump:57\nlang:en_US\n");
    let game = w.game(&old);
    let p = w.peers(&[]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &old, &game, false));
    fs::write(game.join("options.txt"), "key_key.jump:57\n").unwrap();
    t.loaded("k1");
    fs::write(game.join("options.txt"), "key_key.jump:58\nlang:de_DE\n").unwrap();
    t.ended(&w.roots, &p, "k1", 1);
    assert_eq!(w.mother("options.txt").unwrap(), "key_key.jump:key.keyboard.space\n");
    let fresh = w.instance("Fresh", Sharing::NONE);
    let older = old_instance(&w, "Older", "1.8.9");
    fs::write(older.dir.join("options.txt"), "key_key.jump:57\n").unwrap();
    set_in(&w.mother, &w.roots.instances, &older.id, Sharing::NONE).unwrap();
    turn_on_in(&w.mother, &w.roots.instances, &[older.id.clone(), fresh.id.clone()]).unwrap();
    assert_eq!(w.mother("options.txt").unwrap(), "key_key.jump:key.keyboard.space\n");
    assert!(own_files(&w, Item::Options).iter().all(|b| !String::from_utf8_lossy(b).contains(":57")));
}

#[test]
fn every_own_copy_that_differs_is_kept_before_the_shared_one_replaces_it() {
    let w = world();
    w.put_mother("config/mod.json", "{\"shared\":1}");
    w.put_mother("options.txt", "fov:0.5\n");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    fs::write(a.dir.join("config").join("mod.json"), "{\"by hand\":1}").unwrap();
    fs::write(a.dir.join("options.txt"), "fov:0.3\n").unwrap();
    inbound_in(&w.mother, &a, false).unwrap();
    assert_eq!(read(&a.dir.join("config").join("mod.json")), "{\"shared\":1}");
    assert!(own_files(&w, Item::Config).contains(&b"{\"by hand\":1}".to_vec()));
    assert!(own_files(&w, Item::Options).contains(&b"fov:0.3\n".to_vec()));
}

#[test]
fn what_was_kept_stays_restorable_when_a_later_step_fails() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    w.put_mother("servers.dat", "list");
    let a = w.instance("A", Sharing::ALL);
    fs::write(a.dir.join("options.txt"), "fov:0.1\n").unwrap();
    fs::create_dir_all(a.dir.join("servers.dat").join("blocked")).unwrap();
    assert!(inbound_in(&w.mother, &a, false).is_err());
    assert!(own_files(&w, Item::Options).contains(&b"fov:0.1\n".to_vec()));
}

#[test]
fn an_options_template_that_is_not_utf8_keeps_its_own_lines_and_bytes() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    let a = w.instance("A", Sharing::ALL);
    let raw = b"version:3955\nfov:0.1\nresourcePacks:[\"\xdcbersicht.zip\"]\n".to_vec();
    fs::write(a.dir.join("options.txt"), &raw).unwrap();
    inbound_in(&w.mother, &a, false).unwrap();
    let now = String::from_utf8_lossy(&fs::read(a.dir.join("options.txt")).unwrap()).into_owned();
    assert!(now.contains("version:3955") && now.contains("resourcePacks:") && now.contains("fov:0.5"), "{now}");
    assert!(own_files(&w, Item::Options).contains(&raw));
}

#[test]
fn a_broken_history_index_moves_aside_and_starts_still_work() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    w.put_mother("history/index.json", "{ not json");
    w.put_mother("history/kept/options.txt", "fov:0.2\n");
    let a = w.instance("A", Sharing::ALL);
    fs::write(a.dir.join("options.txt"), "fov:0.1\n").unwrap();
    inbound_in(&w.mother, &a, false).unwrap();
    assert!(read(&a.dir.join("options.txt")).contains("fov:0.5"));
    let aside: Vec<PathBuf> = fs::read_dir(&w.mother.root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("history.broken-")).collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(read(&aside[0].join("kept").join("options.txt")), "fov:0.2\n");
}

#[test]
fn history_entries_pointing_outside_are_ignored() {
    let w = world();
    let a = w.instance("A", Sharing::ALL);
    w.put_mother("config/mod.json", "shared");
    inbound_in(&w.mother, &a, false).unwrap();
    let index = format!("{{\"entries\":[{{\"at\":1,\"rel\":\"../../evil.txt\",\"sha\":\"x\",\"size\":1,\"file\":\"a/b\"}},{{\"at\":1,\"rel\":\"config/x.json\",\"sha\":\"x\",\"size\":1,\"file\":\"../../../escape\"}}]}}");
    w.put_mother("history/index.json", &index);
    for item in ITEMS {
        assert!(history::versions(&w.mother, item).unwrap().is_empty());
    }
}

#[cfg(windows)]
fn link_to(target: &Path, link: &Path) -> std::io::Result<()> {
    junction::create(target, link)
}

#[cfg(not(windows))]
fn link_to(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[test]
fn shared_config_never_goes_through_a_linked_folder() {
    let w = world();
    let a = w.instance("A", Sharing::ALL);
    let outside = w.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(a.dir.join("config")).unwrap();
    if link_to(&outside, &a.dir.join("config").join("sub")).is_err() {
        return;
    }
    w.put_mother("config/sub/x.json", "shared");
    w.put_mother("config/top.json", "top");
    inbound_in(&w.mother, &a, false).unwrap();
    assert!(!outside.join("x.json").exists());
    assert_eq!(read(&a.dir.join("config").join("top.json")), "top");
}

#[test]
fn an_unreadable_config_folder_keeps_the_baseline() {
    let w = world();
    w.put_mother("config/mod.json", "{\"a\":1}");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    let game = w.game(&a);
    let p = w.peers(&[]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k1", &a, &game, false));
    t.loaded("k1");
    w.put_mother("config/mod.json", "{\"b\":2}");
    let config = a.dir.join("config");
    let moved = a.dir.join("config-away");
    fs::rename(&config, &moved).unwrap();
    fs::write(&config, "not a folder").unwrap();
    scan_now(&mut t);
    fs::remove_file(&config).unwrap();
    fs::rename(&moved, &config).unwrap();
    scan_now(&mut t);
    t.ended(&w.roots, &p, "k1", 1);
    assert_eq!(w.mother("config/mod.json").unwrap(), "{\"b\":2}");
}

#[test]
fn an_old_options_template_after_a_version_change_never_reaches_the_sync_folder() {
    let w = world();
    let a = w.instance("A", Sharing::ALL);
    fs::write(a.dir.join("options.txt"), "version:1343\nkey_key.jump:57\nfov:0.1\n").unwrap();
    inbound_in(&w.mother, &a, false).unwrap();
    assert_eq!(w.mother("options.txt"), None);
    let b = w.instance("B", Sharing::NONE);
    fs::write(b.dir.join("options.txt"), "key_key.jump:57\n").unwrap();
    set_in(&w.mother, &w.roots.instances, &b.id, Sharing::ALL).unwrap();
    assert_eq!(w.mother("options.txt"), None);
    w.put_mother("options.txt", "key_key.jump:key.keyboard.space\n");
    inbound_in(&w.mother, &a, false).unwrap();
    let now = read(&a.dir.join("options.txt"));
    assert!(now.contains("key_key.jump:key.keyboard.space") && !now.contains(":57"), "{now}");
    assert!(own_files(&w, Item::Options).contains(&b"version:1343\nkey_key.jump:57\nfov:0.1\n".to_vec()));
}

#[test]
fn keys_a_game_adds_after_loading_never_reach_the_sync_folder() {
    let w = world();
    w.put_mother("options.txt", "fov:0.5\n");
    let a = w.instance("A", Sharing::ALL);
    inbound_in(&w.mother, &a, false).unwrap();
    let game = w.game(&a);
    let p = w.peers(&[("k1", &a)]);
    let mut t = w.tracker();
    t.started(&w.roots, &p, start("k2", &a, &game, false));
    fs::write(game.join("options.txt"), "version:5023\nfov:0.5\n").unwrap();
    t.loaded("k2");
    fs::write(game.join("options.txt"), "version:5023\nfov:0.5\nrenderDistance:16\n").unwrap();
    t.ended(&w.roots, &p, "k2", 1);
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.5\n");
    let companion = a.dir.join("games").join("c");
    fs::create_dir_all(&companion).unwrap();
    fs::write(companion.join("options.txt"), "fov:0.5\n").unwrap();
    t.started(&w.roots, &p, start("k1", &a, &companion, true));
    t.loaded("k1");
    t.options(&w.roots, &p, "k1", &values(&[("fov", "0.5"), ("cloudRange", "64")]));
    assert_eq!(w.mother("options.txt").unwrap(), "fov:0.5\n");
    t.options(&w.roots, &p, "k1", &values(&[("cloudRange", "32")]));
    assert!(w.mother("options.txt").unwrap().contains("cloudRange:32"));
}
