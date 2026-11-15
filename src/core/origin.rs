//! What an instance started as, kept beside it: its first mods, config and packs, so it can go back there.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::batch::ModRef;
use super::instance::{self, Final, Instance, Loader};
use super::{lock, net, paths, store};

const DIR: &str = "origin";
const FILE: &str = "origin.json";
const PACKS: [&str; 3] = ["config", "resourcepacks", "shaderpacks"];
const SWAPPED: [&str; 4] = ["mods", "config", "resourcepacks", "shaderpacks"];
const MODS_LOCK: &str = "mods.lock";
const SUFFIX: &str = ".reset";

/// The mods, version and loader an instance was created with; its config and packs sit in the same folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub game_version: String,
    pub loader: Loader,
    pub mods: Vec<ModRef>,
    pub local_mods: Vec<String>,
}

/// The mods a reset puts back for the instance's current version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// These mods fit as they are.
    Ready(Vec<ModRef>),
    /// These mods were made for another version and have to be looked up again for the current one.
    Move(Vec<ModRef>),
}

fn fabric_like(loader: &Loader) -> bool {
    matches!(loader, Loader::Fabric(_) | Loader::Quilt(_))
}

impl Origin {
    fn carries_over(&self, loader: &Loader) -> bool {
        fabric_like(&self.loader) && fabric_like(loader)
    }

    /// The mods to put back into an instance now on `version` with `loader`.
    pub fn plan(&self, version: &str, loader: &Loader) -> Plan {
        if !self.carries_over(loader) || self.mods.is_empty() {
            return Plan::Ready(Vec::new());
        }
        if self.game_version == version { Plan::Ready(self.mods.clone()) } else { Plan::Move(self.mods.clone()) }
    }

    /// The own jars to put back into an instance now on `loader`.
    pub fn local_mods_for(&self, loader: &Loader) -> Vec<String> {
        if self.carries_over(loader) { self.local_mods.clone() } else { Vec::new() }
    }
}

fn origin_dir(root: &Path, id: &str) -> PathBuf {
    root.join(id).join(DIR)
}

/// Keeps what the just created `made` holds in its folder under `root`, unless that was kept before.
pub(super) fn record(root: &Path, made: &Instance) -> Result<()> {
    let (dir, target) = (root.join(&made.id), origin_dir(root, &made.id));
    if std::fs::symlink_metadata(&target).is_ok() {
        return Ok(());
    }
    let staging = dir.join(format!("{DIR}.tmp"));
    let _ = std::fs::remove_dir_all(&staging);
    let kept = (|| -> Result<()> {
        std::fs::create_dir_all(&staging).with_context(|| format!("cannot create {}", staging.display()))?;
        for pack in PACKS {
            instance::copy_tree(&dir.join(pack), &staging.join(pack))?;
        }
        for name in &made.local_mods {
            let from = dir.join("mods").join(name);
            if super::modrinth::safe_file_name(name).is_ok() && from.is_file() {
                std::fs::create_dir_all(staging.join("mods")).with_context(|| format!("cannot create {}", staging.join("mods").display()))?;
                std::fs::copy(&from, staging.join("mods").join(name)).with_context(|| format!("cannot copy {}", from.display()))?;
            }
        }
        let origin = Origin { game_version: made.game_version.clone(), loader: made.loader.clone(), mods: made.mods.clone(), local_mods: made.local_mods.clone() };
        net::write_atomic(&staging.join(FILE), &serde_json::to_vec_pretty(&origin)?)?;
        std::fs::rename(&staging, &target).with_context(|| format!("cannot move {} into place", staging.display()))
    })();
    if kept.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    kept
}

/// What `made` started as. An instance made before this was kept started empty. A damaged record is refused for good.
pub fn load(made: &Instance) -> Result<Origin> {
    load_in(&instance::instances_dir(), made)
}

/// Whether the start of `made` was kept; without it a reset leaves mods, config and packs empty.
pub fn is_kept(made: &Instance) -> bool {
    std::fs::symlink_metadata(origin_dir(&instance::instances_dir(), &made.id).join(FILE)).is_ok()
}

fn quiet() -> Result<lock::Guard> {
    lock::try_exclusive(&paths::data_dir().join(super::install::clean::LOCK_FILE))?.with_context(|| super::i18n::tr("Gerade laufen Spiele. Stoppe sie zuerst."))
}

pub(super) fn load_in(root: &Path, made: &Instance) -> Result<Origin> {
    let file = origin_dir(root, &made.id).join(FILE);
    match store::read(&file)? {
        None => Ok(Origin { game_version: made.game_version.clone(), loader: made.loader.clone(), mods: Vec::new(), local_mods: Vec::new() }),
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| anyhow::Error::new(Final(super::i18n::tr("Der Anfangszustand dieser Instanz ist beschädigt. Ihre Dateien bleiben unverändert."))).context(format!("{} is damaged: {e}", file.display()))),
    }
}

/// Moves the mods, config and packs of `made` into a trash entry, puts the ones of its origin in their place and
/// gives `made` the mods `mods`. Worlds, screenshots, `servers.dat` and `options.txt` stay. Returns the saved instance and
/// the trash entry [`unreset`] takes. Callers check first that no game of it runs.
pub fn reset(made: &Instance, mods: Vec<ModRef>) -> Result<(Instance, String)> {
    let _quiet = quiet()?;
    reset_in(&instance::instances_dir(), made, mods, instance::now_millis())
}

pub(super) fn reset_in(root: &Path, made: &Instance, mods: Vec<ModRef>, millis: u64) -> Result<(Instance, String)> {
    if !instance::valid_id(&made.id) {
        bail!("invalid instance id {:?}", made.id);
    }
    let origin = load_in(root, made)?;
    let dir = root.join(&made.id);
    let _held = lock::exclusive(&dir.join(MODS_LOCK))?;
    let staging = dir.join(format!(".reset-{millis}"));
    let _ = std::fs::remove_dir_all(&staging);
    let local = origin.local_mods_for(&made.loader);
    let built = (|| -> Result<()> {
        let kept = origin_dir(root, &made.id);
        for name in SWAPPED {
            let to = staging.join(name);
            std::fs::create_dir_all(&to).with_context(|| format!("cannot create {}", to.display()))?;
            if name == "mods" {
                for jar in &local {
                    let from = kept.join("mods").join(jar);
                    if from.is_file() {
                        std::fs::copy(&from, to.join(jar)).with_context(|| format!("cannot copy {}", from.display()))?;
                    }
                }
            } else {
                instance::copy_tree(&kept.join(name), &to)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let entry = format!("{}-{millis}{SUFFIX}", made.id);
    let trash = instance::trash_dir(root).join(&entry);
    if let Err(e) = exchange(&dir, &staging, &trash) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let _ = std::fs::remove_dir(&staging);
    let next = Instance { mods, local_mods: local, ..made.clone() };
    if let Err(e) = instance::save_in(root, &next) {
        let back = instance::trash_dir(root).join(format!("{}-{}{SUFFIX}", made.id, millis + 1));
        let _ = exchange(&dir, &trash, &back);
        return Err(e);
    }
    Ok((next, entry))
}

/// Puts back what [`reset`] moved into trash entry `entry`, keeps what is in the instance now in a trash entry of its
/// own, and gives `now`, the instance as it is saved, the mods and own files of `before`. Refused for good when the
/// entry is gone.
pub fn unreset(now: &Instance, before: &Instance, entry: &str) -> Result<Instance> {
    let _quiet = quiet()?;
    unreset_in(&instance::instances_dir(), now, before, entry, instance::now_millis())
}

pub(super) fn unreset_in(root: &Path, now: &Instance, before: &Instance, entry: &str, millis: u64) -> Result<Instance> {
    if !instance::valid_id(&now.id) || now.id != before.id {
        bail!("invalid instance id {:?}", now.id);
    }
    let from = instance::trash_dir(root).join(entry);
    if !entry.starts_with(&format!("{}-", now.id)) || !entry.ends_with(SUFFIX) || !SWAPPED.iter().any(|name| exists(&from.join(name))) {
        bail!(Final(super::i18n::tr("Der Stand vor dem Zurücksetzen liegt nicht mehr im Papierkorb.")));
    }
    let dir = root.join(&now.id);
    let _held = lock::exclusive(&dir.join(MODS_LOCK))?;
    let kept = instance::trash_dir(root).join(format!("{}-{millis}{SUFFIX}", now.id));
    exchange(&dir, &from, &kept)?;
    let next = Instance { mods: before.mods.clone(), local_mods: before.local_mods.clone(), ..now.clone() };
    if let Err(e) = instance::save_in(root, &next) {
        let _ = exchange(&dir, &kept, &from);
        return Err(e);
    }
    let _ = std::fs::remove_dir(&from);
    Ok(next)
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Moves the mods, config and packs in `dir` into the new folder `away` and the ones in `replacement` into `dir`.
/// Whatever fails, every folder is back where it was.
fn exchange(dir: &Path, replacement: &Path, away: &Path) -> Result<()> {
    std::fs::create_dir_all(away).with_context(|| format!("cannot create {}", away.display()))?;
    let mut moved_away: Vec<&str> = Vec::new();
    let mut placed: Vec<&str> = Vec::new();
    let result = (|| -> Result<()> {
        for name in SWAPPED {
            if exists(&dir.join(name)) {
                std::fs::rename(dir.join(name), away.join(name)).with_context(|| format!("cannot move {} out of the way", dir.join(name).display()))?;
                moved_away.push(name);
            }
        }
        for name in SWAPPED {
            if exists(&replacement.join(name)) {
                std::fs::rename(replacement.join(name), dir.join(name)).with_context(|| format!("cannot move {} into place", replacement.join(name).display()))?;
                placed.push(name);
            }
        }
        Ok(())
    })();
    let Err(error) = result else { return Ok(()) };
    for name in placed {
        let _ = std::fs::rename(dir.join(name), replacement.join(name));
    }
    for name in moved_away {
        let _ = std::fs::rename(away.join(name), dir.join(name));
    }
    if std::fs::remove_dir(away).is_err() {
        let kept = away.with_file_name(format!("kept-{}", away.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())));
        if std::fs::rename(away, &kept).is_ok() {
            return Err(error.context(format!("the folders that could not be moved back are in {}", kept.display())));
        }
        return Err(error.context(format!("the folders that could not be moved back are in {}", away.display())));
    }
    Err(error)
}

/// Whether trash entry name `entry` is one [`reset`] made, and when.
pub(super) fn parse_reset(entry: &str) -> Option<u64> {
    instance::parse_entry(entry.strip_suffix(SUFFIX)?).map(|(_, millis)| millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instance::{Instance, Loader, games_dir, save_in};

    fn mod_ref(id: &str) -> ModRef {
        ModRef { project_id: id.into(), version_id: "v".into(), title: id.into(), file_name: format!("{id}.jar"), url: String::new(), sha1: String::new() }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|_| "<missing>".into())
    }

    fn pack_instance(root: &Path) -> Instance {
        let mut made = Instance::new("Farm", "26.2", Loader::Fabric(Some("0.17".into())));
        made.mods = vec![mod_ref("a")];
        made.local_mods = vec!["mine.jar".into()];
        let dir = root.join(&made.id);
        write(&dir.join("mods/mine.jar"), "mine");
        write(&dir.join("mods/a.jar"), "a");
        write(&dir.join("config/sodium.json"), "{\"first\":true}");
        write(&dir.join("resourcepacks/faithful.zip"), "pack");
        write(&dir.join("shaderpacks/bsl.zip"), "shader");
        save_in(root, &made).unwrap();
        made
    }

    fn mess_up(root: &Path, made: &Instance) -> Instance {
        let dir = root.join(&made.id);
        write(&dir.join("mods/extra.jar"), "extra");
        write(&dir.join("config/sodium.json"), "{\"first\":false}");
        write(&dir.join("config/new.json"), "new");
        write(&dir.join("resourcepacks/other.zip"), "other");
        std::fs::remove_file(dir.join("shaderpacks/bsl.zip")).unwrap();
        std::fs::remove_file(dir.join("mods/mine.jar")).unwrap();
        let mut now = made.clone();
        now.mods = vec![mod_ref("a"), mod_ref("b")];
        now.local_mods.clear();
        save_in(root, &now).unwrap();
        now
    }

    #[test]
    fn a_new_instance_keeps_what_it_started_with_and_later_saves_leave_it() {
        let dir = tempfile::tempdir().unwrap();
        let made = pack_instance(dir.path());
        let kept = origin_dir(dir.path(), &made.id);
        assert_eq!(read(&kept.join("config/sodium.json")), "{\"first\":true}");
        assert_eq!(read(&kept.join("mods/mine.jar")), "mine");
        assert!(!kept.join("mods/a.jar").exists());
        mess_up(dir.path(), &made);
        record(dir.path(), &made).unwrap();
        assert_eq!(read(&kept.join("config/sodium.json")), "{\"first\":true}");
        let origin = load_in(dir.path(), &made).unwrap();
        assert_eq!((origin.mods, origin.local_mods, origin.game_version), (vec![mod_ref("a")], vec!["mine.jar".to_owned()], "26.2".to_owned()));
    }

    #[test]
    fn resetting_brings_mods_config_and_packs_back_and_leaves_worlds_options_and_servers() {
        let dir = tempfile::tempdir().unwrap();
        let made = pack_instance(dir.path());
        let root = dir.path();
        let folder = root.join(&made.id);
        write(&folder.join("options.txt"), "fov:90");
        write(&folder.join("servers.dat"), "servers");
        let account = games_dir(&folder).join("a1");
        write(&account.join("saves/World/level.dat"), "world");
        write(&account.join("screenshots/one.png"), "shot");
        let now = mess_up(root, &made);
        let (next, entry) = reset_in(root, &now, vec![mod_ref("a")], 1000).unwrap();
        assert_eq!((next.mods, next.local_mods, next.name), (vec![mod_ref("a")], vec!["mine.jar".to_owned()], "Farm".to_owned()));
        assert_eq!(read(&folder.join("config/sodium.json")), "{\"first\":true}");
        assert_eq!(read(&folder.join("mods/mine.jar")), "mine");
        assert!(!folder.join("mods/extra.jar").exists() && !folder.join("config/new.json").exists() && !folder.join("resourcepacks/other.zip").exists());
        assert_eq!(read(&folder.join("shaderpacks/bsl.zip")), "shader");
        assert_eq!((read(&folder.join("options.txt")), read(&folder.join("servers.dat"))), ("fov:90".to_owned(), "servers".to_owned()));
        assert_eq!((read(&account.join("saves/World/level.dat")), read(&account.join("screenshots/one.png"))), ("world".to_owned(), "shot".to_owned()));
        assert_eq!(read(&instance::trash_dir(root).join(&entry).join("config/new.json")), "new");
        assert!(!root.join(&made.id).join(".reset-1000").exists());
    }

    #[test]
    fn undoing_a_reset_puts_back_exactly_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let made = pack_instance(dir.path());
        let root = dir.path();
        let folder = root.join(&made.id);
        let now = mess_up(root, &made);
        let (after, entry) = reset_in(root, &now, vec![mod_ref("a")], 1000).unwrap();
        let back = unreset_in(root, &after, &now, &entry, 2000).unwrap();
        assert_eq!((back.mods.clone(), back.local_mods.clone()), (now.mods.clone(), now.local_mods.clone()));
        assert_eq!(read(&folder.join("config/sodium.json")), "{\"first\":false}");
        assert_eq!((read(&folder.join("config/new.json")), read(&folder.join("mods/extra.jar")), read(&folder.join("resourcepacks/other.zip"))), ("new".to_owned(), "extra".to_owned(), "other".to_owned()));
        assert_eq!((read(&folder.join("shaderpacks/bsl.zip")), read(&folder.join("mods/mine.jar"))), ("<missing>".to_owned(), "<missing>".to_owned()));
        let again = unreset_in(root, &back, &now, &entry, 3000).unwrap_err();
        assert!(again.chain().any(|c| c.is::<Final>()));
    }

    #[test]
    fn an_undo_that_cannot_save_keeps_the_files_for_the_next_try_instead_of_emptying_the_instance() {
        let dir = tempfile::tempdir().unwrap();
        let made = pack_instance(dir.path());
        let root = dir.path();
        let folder = root.join(&made.id);
        let now = mess_up(root, &made);
        let (after, entry) = reset_in(root, &now, vec![mod_ref("a")], 1000).unwrap();
        let file = folder.join(instance::FILE);
        let text = std::fs::read(&file).unwrap();
        std::fs::write(&file, [text.as_slice(), b" "].concat()).unwrap();
        assert!(unreset_in(root, &after, &now, &entry, 2000).is_err());
        let entry_dir = instance::trash_dir(root).join(&entry);
        assert_eq!(read(&entry_dir.join("config/new.json")), "new");
        assert_eq!(read(&folder.join("config/sodium.json")), "{\"first\":true}");
        store::read(&file).unwrap();
        let back = unreset_in(root, &after, &now, &entry, 3000).unwrap();
        assert_eq!(back.mods, now.mods);
        assert_eq!((read(&folder.join("config/new.json")), read(&folder.join("mods/extra.jar"))), ("new".to_owned(), "extra".to_owned()));
    }

    #[test]
    fn an_undo_whose_entry_holds_nothing_is_refused_for_good() {
        let dir = tempfile::tempdir().unwrap();
        let made = pack_instance(dir.path());
        let root = dir.path();
        let now = mess_up(root, &made);
        let (after, entry) = reset_in(root, &now, vec![mod_ref("a")], 1000).unwrap();
        let entry_dir = instance::trash_dir(root).join(&entry);
        for name in SWAPPED {
            std::fs::remove_dir_all(entry_dir.join(name)).unwrap();
        }
        let err = unreset_in(root, &after, &now, &entry, 2000).unwrap_err();
        assert!(err.chain().any(|c| c.is::<Final>()));
        assert_eq!(read(&root.join(&made.id).join("config/sodium.json")), "{\"first\":true}");
    }

    #[test]
    fn an_instance_without_a_kept_start_resets_to_empty_and_a_damaged_one_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let made = pack_instance(root);
        let folder = root.join(&made.id);
        std::fs::write(origin_dir(root, &made.id).join(FILE), "{").unwrap();
        let refused = reset_in(root, &made, Vec::new(), 1000).unwrap_err();
        assert!(refused.chain().any(|c| c.is::<Final>()));
        assert_eq!(read(&folder.join("mods/a.jar")), "a");
        std::fs::remove_dir_all(origin_dir(root, &made.id)).unwrap();
        let (next, _) = reset_in(root, &made, Vec::new(), 2000).unwrap();
        assert!(next.mods.is_empty() && next.local_mods.is_empty());
        assert!(!folder.join("mods/a.jar").exists() && !folder.join("config/sodium.json").exists());
        assert!(folder.join("config").is_dir());
    }

    #[test]
    fn a_failed_swap_leaves_every_folder_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let made = pack_instance(root);
        let folder = root.join(&made.id);
        let away = root.join("away");
        let replacement = root.join("replacement");
        write(&replacement.join("config/x"), "x");
        write(&replacement.join("shaderpacks/y"), "y");
        write(&away.join("shaderpacks/in-the-way"), "taken");
        let blocked = exchange(&folder, &replacement, &away);
        assert!(blocked.is_err());
        assert_eq!(read(&folder.join("shaderpacks/bsl.zip")), "shader");
        assert_eq!(read(&folder.join("config/sodium.json")), "{\"first\":true}");
        assert_eq!(read(&replacement.join("config/x")), "x");
    }

    #[test]
    fn mods_follow_the_version_and_loader_the_instance_has_now() {
        let origin = Origin { game_version: "26.2".into(), loader: Loader::Fabric(None), mods: vec![mod_ref("a")], local_mods: vec!["mine.jar".into()] };
        assert_eq!(origin.plan("26.2", &Loader::Fabric(Some("0.17".into()))), Plan::Ready(vec![mod_ref("a")]));
        assert_eq!(origin.plan("26.3", &Loader::Quilt(None)), Plan::Move(vec![mod_ref("a")]));
        assert_eq!(origin.plan("26.2", &Loader::Vanilla), Plan::Ready(Vec::new()));
        assert_eq!(origin.plan("26.2", &Loader::Forge(None)), Plan::Ready(Vec::new()));
        assert_eq!(origin.local_mods_for(&Loader::Quilt(None)), ["mine.jar"]);
        assert!(origin.local_mods_for(&Loader::Vanilla).is_empty());
        let empty = Origin { mods: Vec::new(), ..origin };
        assert_eq!(empty.plan("26.9", &Loader::Fabric(None)), Plan::Ready(Vec::new()));
    }

    #[test]
    fn only_reset_entries_parse_as_reset_entries() {
        assert_eq!(parse_reset("ab12-1000.reset"), Some(1000));
        assert_eq!(parse_reset("ab12-1000"), None);
        assert_eq!(parse_reset("ab12-x.reset"), None);
        assert_eq!(parse_reset("../x-1.reset"), None);
    }
}
