use std::fs;
use std::path::PathBuf;

use super::migrate::migrate_in;
use super::*;
use crate::core::instance::{Loader, dir_in, save_in};

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    mother: Mother,
}

const ACCOUNT: &str = "0000000000000000000000000000000a";

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("instances");
    World { mother: Mother::at(tmp.path().join(DIR)), root, _tmp: tmp }
}

impl World {
    fn instance(&self, name: &str) -> Own {
        self.instance_of(Instance::new(name, "26.3", Loader::Vanilla))
    }

    fn instance_of(&self, made: Instance) -> Own {
        save_in(&self.root, &made).unwrap();
        Own { dir: dir_in(&self.root, &made.id), id: made.id, name: made.name }
    }

    fn game(&self, own: &Own, rel: &str) -> PathBuf {
        let path = own.dir.join("games").join(ACCOUNT).join(rel);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn put(&self, path: PathBuf, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn shared(&self, rel: &str) -> PathBuf {
        self.mother.path(rel)
    }

    fn on(&self, base: &Own) {
        setup_in(&self.mother, &self.root, &base.id, &[]).unwrap();
    }

    fn start(&self, own: &Own) {
        let made = instance::load_all_in(&self.root).unwrap().into_iter().find(|i| i.id == own.id).unwrap();
        inbound_in(&self.mother, own, false).unwrap();
        let game = own.dir.join("games").join(ACCOUNT);
        fs::create_dir_all(&game).unwrap();
        settle_game_in(&self.mother, &instance::dir_in(&self.root, &made.id), &game).unwrap();
    }
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(path).unwrap()
}

fn linked_to(path: &Path, target: &Path) -> bool {
    is_link(path) && folders::points_at(path, target)
}

#[test]
fn setup_makes_the_mother_the_shared_state_and_links_every_instance() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(a.dir.join("options.txt"), "version:3955\nguiScale:3\nresourcePacks:[\"x\"]\nfullscreen:true\n");
    w.put(a.dir.join("config").join("mod.toml"), "a=1");
    w.put(w.game(&a, "saves").join("World One").join("level.dat"), "w1");
    w.put(w.game(&a, "screenshots").join("1.png"), "s1");
    w.put(b.dir.join("config").join("mod.toml"), "b=1");
    w.put(w.game(&b, "saves").join("World B").join("level.dat"), "wb");
    w.on(&a);
    assert_eq!(read(w.shared("options.txt")), "guiScale:3\n");
    assert_eq!(read(w.shared("config/mod.toml")), "a=1");
    assert_eq!(read(w.shared("saves/World One/level.dat")), "w1");
    assert_eq!(read(w.shared("screenshots/1.png")), "s1");
    assert!(state::read(&w.mother).on);
    for own in [&a, &b] {
        assert!(linked_to(&own.dir.join("config"), &w.shared("config")), "{} config", own.name);
        assert!(linked_to(&own.dir.join("games").join(ACCOUNT).join("saves"), &w.shared("saves")), "{} saves", own.name);
    }
    assert_eq!(read(a.dir.join("options.txt")), "resourcePacks:[\"x\"]\nversion:3955\n", "the mother keeps what stays with it");
}

#[test]
fn a_joining_instance_keeps_its_own_folders_aside_once() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(a.dir.join("options.txt"), "version:3955\nguiScale:3\n");
    w.put(b.dir.join("options.txt"), "version:3955\nguiScale:1\nresourcePacks:[\"b\"]\n");
    w.put(b.dir.join("config").join("own.toml"), "own");
    w.put(w.game(&b, "saves").join("Mine").join("level.dat"), "mine");
    w.on(&a);
    assert_eq!(read(b.dir.join("config.before-sync").join("own.toml")), "own");
    assert_eq!(read(b.dir.join("games").join(ACCOUNT).join("saves.before-sync").join("Mine").join("level.dat")), "mine");
    assert_eq!(read(b.dir.join("options.txt.before-sync")), "version:3955\nguiScale:1\nresourcePacks:[\"b\"]\n");
    assert_eq!(read(b.dir.join("options.txt")), "resourcePacks:[\"b\"]\nversion:3955\n");
    assert!(!w.shared("config").join("own.toml").exists());
}

#[test]
fn a_world_made_in_one_instance_is_in_the_other() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.on(&a);
    w.start(&a);
    w.start(&b);
    w.put(a.dir.join("games").join(ACCOUNT).join("saves").join("New").join("level.dat"), "new");
    assert_eq!(read(b.dir.join("games").join(ACCOUNT).join("saves").join("New").join("level.dat")), "new");
    w.put(b.dir.join("games").join(ACCOUNT).join("screenshots").join("shot.png"), "png");
    assert_eq!(read(a.dir.join("games").join(ACCOUNT).join("screenshots").join("shot.png")), "png");
}

#[test]
fn an_instance_made_later_takes_part_at_its_first_start() {
    let w = world();
    let a = w.instance("A");
    w.put(a.dir.join("config").join("mod.toml"), "a=1");
    w.on(&a);
    let late = w.instance("Late");
    assert!(!linked_to(&late.dir.join("config"), &w.shared("config")));
    w.start(&late);
    assert!(linked_to(&late.dir.join("config"), &w.shared("config")));
    assert_eq!(read(late.dir.join("config").join("mod.toml")), "a=1");
    assert!(linked_to(&late.dir.join("games").join(ACCOUNT).join("saves"), &w.shared("saves")));
}

#[test]
fn an_opted_out_instance_keeps_its_own_files() {
    let w = world();
    let (a, out) = (w.instance("A"), w.instance("Out"));
    w.put(out.dir.join("config").join("mine.toml"), "mine");
    w.put(w.game(&out, "saves").join("Own World").join("level.dat"), "own");
    w.on(&a);
    assert!(linked_to(&out.dir.join("config"), &w.shared("config")));
    set_opt_out_in(&w.mother, &w.root, &out.id, true, false).unwrap();
    assert!(!is_link(&out.dir.join("config")));
    assert!(!is_link(&out.dir.join("games").join(ACCOUNT).join("saves")));
    w.start(&out);
    assert!(!is_link(&out.dir.join("config")));
    assert!(!is_link(&out.dir.join("games").join(ACCOUNT).join("saves")));
    assert!(instance::load_all_in(&w.root).unwrap().iter().find(|i| i.id == out.id).unwrap().sync_out);
    let b = w.instance("B");
    w.on_instance_opt_out(&b);
    w.start(&b);
    assert!(!is_link(&b.dir.join("config")));
    set_opt_out_in(&w.mother, &w.root, &out.id, false, false).unwrap();
    assert!(linked_to(&out.dir.join("config"), &w.shared("config")));
}

impl World {
    fn on_instance_opt_out(&self, own: &Own) {
        set_opt_out_in(&self.mother, &self.root, &own.id, true, false).unwrap();
    }
}

#[test]
fn a_shared_instance_that_stays_out_gets_a_real_copy_of_everything() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(a.dir.join("options.txt"), "version:3955\nguiScale:3\n");
    w.put(a.dir.join("config").join("mod.toml"), "a=1");
    w.put(w.game(&a, "saves").join("W").join("level.dat"), "w");
    w.on(&a);
    set_opt_out_in(&w.mother, &w.root, &b.id, true, false).unwrap();
    assert!(!is_link(&b.dir.join("config")));
    set_opt_out_in(&w.mother, &w.root, &a.id, true, false).unwrap();
    assert_eq!(read(a.dir.join("config").join("mod.toml")), "a=1");
    assert_eq!(read(a.dir.join("games").join(ACCOUNT).join("saves").join("W").join("level.dat")), "w");
    assert!(read(a.dir.join("options.txt")).contains("guiScale:3"));
}

#[test]
fn switching_a_kind_off_gives_every_instance_its_own_copy_and_empties_the_shared_one() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(w.game(&a, "saves").join("W").join("level.dat"), "w");
    w.put(a.dir.join("options.txt"), "version:3955\nguiScale:3\n");
    w.on(&a);
    w.start(&b);
    w.put(b.dir.join("games").join(ACCOUNT).join("saves").join("Fresh").join("level.dat"), "f");
    set_kind_in(&w.mother, &w.root, Kind::Saves, false, &[]).unwrap();
    for own in [&a, &b] {
        let saves = own.dir.join("games").join(ACCOUNT).join("saves");
        assert!(!is_link(&saves), "{} saves", own.name);
        assert_eq!(read(saves.join("W").join("level.dat")), "w");
        assert_eq!(read(saves.join("Fresh").join("level.dat")), "f");
    }
    assert!(!w.shared("saves").exists());
    assert!(linked_to(&a.dir.join("config"), &w.shared("config")), "other kinds stay shared");
    set_kind_in(&w.mother, &w.root, Kind::Options, false, &[]).unwrap();
    assert!(read(b.dir.join("options.txt")).contains("guiScale:3"));
    assert!(!w.shared("options.txt").exists());
}

#[test]
fn a_kind_switched_off_while_a_game_runs_waits_for_its_next_start() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(w.game(&a, "saves").join("W").join("level.dat"), "w");
    w.on(&a);
    set_kind_in(&w.mother, &w.root, Kind::Saves, false, &[b.id.clone()]).unwrap();
    assert!(!is_link(&a.dir.join("games").join(ACCOUNT).join("saves")));
    w.start(&b);
    assert!(w.shared("saves").is_dir(), "the shared folder stays while a game may still use it");
}

#[test]
fn a_kind_switched_on_later_takes_the_mothers_files() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.on(&a);
    set_kind_in(&w.mother, &w.root, Kind::Config, false, &[]).unwrap();
    w.put(a.dir.join("config").join("later.toml"), "later");
    set_kind_in(&w.mother, &w.root, Kind::Config, true, &[]).unwrap();
    assert_eq!(read(w.shared("config/later.toml")), "later");
    assert_eq!(read(b.dir.join("config").join("later.toml")), "later");
}

#[test]
fn turning_the_sync_off_leaves_every_instance_alone_with_its_files_and_it_can_start_over() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(a.dir.join("config").join("mod.toml"), "a=1");
    w.on(&a);
    turn_off_in(&w.mother, &w.root, &[]).unwrap();
    for own in [&a, &b] {
        assert!(!is_link(&own.dir.join("config")));
        assert_eq!(read(own.dir.join("config").join("mod.toml")), "a=1");
    }
    assert!(!w.shared("config").exists());
    assert!(!state::read(&w.mother).on);
    w.put(b.dir.join("config").join("mod.toml"), "b=2");
    w.on(&b);
    assert_eq!(read(w.shared("config/mod.toml")), "b=2");
    assert_eq!(read(a.dir.join("config").join("mod.toml")), "b=2");
}

#[test]
fn a_leftover_shared_folder_moves_aside_at_a_new_setup() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(a.dir.join("config").join("mod.toml"), "a=1");
    w.on(&a);
    turn_off_in(&w.mother, &w.root, &[b.id.clone()]).unwrap();
    assert!(w.shared("config").is_dir(), "kept while a game runs");
    w.put(b.dir.join("config").join("b.toml"), "b=2");
    w.on(&b);
    assert_eq!(read(w.shared("config/b.toml")), "b=2");
    assert_eq!(read(w.shared("config.before-setup/mod.toml")), "a=1");
}

#[test]
fn the_mother_cannot_be_chosen_while_it_runs() {
    let w = world();
    let a = w.instance("A");
    let err = setup_in(&w.mother, &w.root, &a.id, &[a.id.clone()]).unwrap_err();
    assert!(err.to_string().contains("A"));
    assert!(!state::read(&w.mother).on);
}

#[test]
fn a_running_instance_follows_at_its_next_start() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(b.dir.join("config").join("own.toml"), "own");
    setup_in(&w.mother, &w.root, &a.id, &[b.id.clone()]).unwrap();
    assert!(!is_link(&b.dir.join("config")));
    w.start(&b);
    assert!(linked_to(&b.dir.join("config"), &w.shared("config")));
    assert_eq!(read(b.dir.join("config.before-sync").join("own.toml")), "own");
}

#[test]
fn options_older_than_1_13_stay_out_of_the_shared_options() {
    let w = world();
    let a = w.instance("A");
    let old = w.instance_of(Instance::new("Old", "1.8.9", Loader::Vanilla));
    w.put(a.dir.join("options.txt"), "version:3955\nguiScale:3\n");
    w.put(old.dir.join("options.txt"), "version:3955\nguiScale:1\n");
    w.on(&a);
    assert!(!flags_in(&w.mother, &old.dir).options);
    assert!(flags_in(&w.mother, &old.dir).config);
    assert_eq!(read(old.dir.join("options.txt")), "version:3955\nguiScale:1\n");
}

#[test]
fn an_instance_the_sync_cannot_read_takes_part_in_nothing() {
    let w = world();
    let a = w.instance("A");
    w.on(&a);
    assert_eq!(flags_in(&w.mother, &w._tmp.path().join("nowhere")), Flags::default());
}

fn legacy(w: &World, name: &str, sync: serde_json::Value) -> Own {
    let mut made = Instance::new(name, "26.3", Loader::Vanilla);
    if !sync.is_null() {
        made.kept.insert("sync".into(), sync);
    }
    w.instance_of(made)
}

#[test]
fn the_old_settings_keep_sync_on_with_the_kinds_that_were_on() {
    let w = world();
    let both = legacy(&w, "Both", serde_json::json!({"options": true, "config": true, "servers": true}));
    let cfg = legacy(&w, "Cfg", serde_json::json!({"config": true}));
    let none = legacy(&w, "None", serde_json::Value::Null);
    w.put(w.shared("config/mod.toml"), "shared");
    w.put(w.shared("options.txt"), "guiScale:3\n");
    w.put(w.shared("servers.dat"), "servers");
    w.put(w.shared("setups.json"), &format!("[{{\"base\": \"{}\"}}]", cfg.id));
    w.put(w.shared("history/index.json"), "{}");
    assert!(migrate_in(&w.mother, &w.root).unwrap());
    let state = state::read(&w.mother);
    assert_eq!(state.mother.as_deref(), Some(cfg.id.as_str()), "the old base stays the mother");
    assert!(state.on && state.options && state.config && !state.saves && !state.screenshots);
    assert_eq!(read(w.shared("config/mod.toml")), "shared", "the shared state stays");
    assert_eq!(read(both.dir.join("servers.dat")), "servers", "the server list becomes a real file");
    assert!(!cfg.dir.join("servers.dat").exists());
    assert!(!w.shared("servers.dat").exists() && !w.shared("setups.json").exists() && !w.shared("history").exists());
    let all = instance::load_all_in(&w.root).unwrap();
    let get = |own: &Own| all.iter().find(|i| i.id == own.id).unwrap().clone();
    assert!(!get(&both).sync_out && !get(&cfg).sync_out && get(&none).sync_out);
    assert!(get(&both).kept.get("sync").is_none());
    assert!(flags_in(&w.mother, &both.dir).options && !flags_in(&w.mother, &cfg.dir).saves);
    assert_eq!(flags_in(&w.mother, &none.dir), Flags::default());
    w.start(&cfg);
    assert!(linked_to(&cfg.dir.join("config"), &w.shared("config")));
    assert!(!migrate_in(&w.mother, &w.root).unwrap(), "only once");
}

#[test]
fn without_old_sync_settings_the_sync_stays_off() {
    let w = world();
    let a = legacy(&w, "A", serde_json::Value::Null);
    assert!(!migrate_in(&w.mother, &w.root).unwrap());
    assert!(!state::read(&w.mother).on);
    assert!(!instance::load_all_in(&w.root).unwrap().iter().any(|i| i.id == a.id && i.sync_out));
}

#[test]
fn deleting_a_sharing_instance_leaves_the_shared_worlds_and_screenshots() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(w.game(&a, "saves").join("World One").join("level.dat"), "w1");
    w.put(w.game(&a, "screenshots").join("1.png"), "s1");
    w.on(&a);
    w.start(&b);
    assert!(linked_to(&b.dir.join("games").join(ACCOUNT).join("saves"), &w.shared("saves")));
    let made = instance::load_all_in(&w.root).unwrap().into_iter().find(|i| i.id == b.id).unwrap();
    instance::delete_in(&w.root, &made).unwrap();
    assert!(!b.dir.exists());
    assert_eq!(read(w.shared("saves").join("World One").join("level.dat")), "w1");
    assert_eq!(read(w.shared("screenshots").join("1.png")), "s1");
    assert_eq!(read(a.dir.join("games").join(ACCOUNT).join("saves").join("World One").join("level.dat")), "w1");
}

#[test]
fn accounts_sharing_one_folder_list_each_world_and_screenshot_once() {
    let w = world();
    let a = w.instance("A");
    w.put(w.game(&a, "saves").join("World One").join("level.dat"), "w1");
    w.put(w.game(&a, "screenshots").join("1.png"), "s1");
    w.on(&a);
    let second = a.dir.join("games").join("0000000000000000000000000000000b");
    fs::create_dir_all(&second).unwrap();
    settle_game_in(&w.mother, &a.dir, &second).unwrap();
    assert!(linked_to(&second.join("saves"), &w.shared("saves")));
    let worlds = crate::core::worlds::list(&a.dir);
    assert_eq!(worlds.iter().filter(|x| x.folder == "World One").count(), 1);
    assert_eq!(crate::core::shots::list(&a.dir).len(), 1);
    assert_eq!(crate::core::worlds::distinct_dirs(&a.dir, "saves").len(), 1);
}

#[test]
fn an_opted_out_mother_joins_again_and_keeps_its_worlds() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(w.game(&a, "saves").join("World One").join("level.dat"), "w1");
    let mut made = instance::load_all_in(&w.root).unwrap().into_iter().find(|i| i.id == a.id).unwrap();
    made.sync_out = true;
    save_in(&w.root, &made).unwrap();
    w.on(&a);
    assert!(flags_in(&w.mother, &a.dir).saves);
    w.start(&a);
    w.start(&b);
    assert_eq!(read(b.dir.join("games").join(ACCOUNT).join("saves").join("World One").join("level.dat")), "w1");
}

#[test]
fn the_sync_folder_stays_while_an_instance_sits_in_the_trash() {
    let w = world();
    let a = w.instance("A");
    w.put(w.game(&a, "saves").join("World One").join("level.dat"), "w1");
    w.on(&a);
    w.start(&a);
    fs::create_dir_all(w.root.join(".trash").join("x-1")).unwrap();
    turn_off_in(&w.mother, &w.root, &[]).unwrap();
    assert_eq!(read(w.shared("saves").join("World One").join("level.dat")), "w1");
}

#[test]
fn a_world_in_use_does_not_block_giving_instances_their_own_copy() {
    let w = world();
    let (a, b) = (w.instance("A"), w.instance("B"));
    w.put(w.game(&a, "saves").join("World One").join("level.dat"), "w1");
    w.on(&a);
    w.start(&b);
    w.put(w.shared("saves").join("World One").join("session.lock"), "locked");
    set_kind_in(&w.mother, &w.root, Kind::Saves, false, &[]).unwrap();
    assert_eq!(read(b.dir.join("games").join(ACCOUNT).join("saves").join("World One").join("level.dat")), "w1");
    assert!(!b.dir.join("games").join(ACCOUNT).join("saves").join("World One").join("session.lock").exists());
}
