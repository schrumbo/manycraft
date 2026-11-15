//! Worlds in the game folders of an instance: what their level.dat says, their size, backups and copies.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::instance::{self, Final};
use super::i18n::tr;
use super::trash;

const SAVES: &str = "saves";
const BACKUPS: &str = "backups";
const LEVEL: &str = "level.dat";
const ICON: &str = "icon.png";
const LOCK: &str = "session.lock";
const MAX_LEVEL: u64 = 32 << 20;
const MAX_DEPTH: u32 = 256;

/// How a world is played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

/// What level.dat says about a world; every part may be missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Level {
    pub name: Option<String>,
    /// Unix milliseconds.
    pub last_played: Option<i64>,
    pub mode: Option<Mode>,
    pub hardcore: bool,
}

/// One world in the game folder of one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct World {
    pub account: Uuid,
    pub folder: String,
    pub dir: PathBuf,
    pub level: Level,
    pub icon: Option<PathBuf>,
}

impl World {
    /// The name the game shows: LevelName, else the folder name.
    pub fn name(&self) -> &str {
        self.level.name.as_deref().filter(|n| !n.trim().is_empty()).unwrap_or(&self.folder)
    }
}

/// A backup zip of a world in `<instance>/backups`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    pub path: PathBuf,
    pub account: Uuid,
    pub folder: String,
    pub name: String,
    /// Unix seconds.
    pub at: i64,
    pub size: u64,
}

/// Bytes done of bytes total, shared with the thread doing the work.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    /// Done, from 0 to 1.
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 { 0. } else { (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.) as f32 }
    }

    fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
}

#[derive(Serialize, Deserialize)]
struct Tag {
    account: Uuid,
    folder: String,
    name: String,
    at: i64,
}

/// Reads level.dat, gzipped or raw NBT.
pub fn parse_level(bytes: &[u8]) -> Result<Level> {
    let raw;
    let data = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes).take(MAX_LEVEL + 1).read_to_end(&mut out).context("level.dat is not valid gzip")?;
        if out.len() as u64 > MAX_LEVEL {
            bail!("level.dat is too large");
        }
        raw = out;
        &raw[..]
    } else {
        bytes
    };
    let mut r = Nbt { b: data, at: 0 };
    if r.u8()? != 10 {
        bail!("level.dat does not start with a compound");
    }
    r.string()?;
    let mut level = Level::default();
    loop {
        let tag = r.u8()?;
        if tag == 0 {
            return Ok(level);
        }
        let name = r.string()?;
        if tag == 10 && name == "Data" {
            r.data(&mut level)?;
        } else {
            r.skip(tag, 0)?;
        }
    }
}

struct Nbt<'a> {
    b: &'a [u8],
    at: usize,
}

impl Nbt<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.at.checked_add(n).filter(|end| *end <= self.b.len()).context("level.dat ends early")?;
        let out = self.b.get(self.at..end).context("level.dat ends early")?;
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into()?))
    }

    fn len(&mut self) -> Result<usize> {
        usize::try_from(self.i32()?).context("negative length in level.dat")
    }

    fn string(&mut self) -> Result<String> {
        let n = u16::from_be_bytes(self.take(2)?.try_into()?);
        Ok(String::from_utf8_lossy(self.take(n.into())?).into_owned())
    }

    fn skip(&mut self, tag: u8, depth: u32) -> Result<()> {
        if depth > MAX_DEPTH {
            bail!("level.dat nests too deep");
        }
        let fixed = |tag: u8| match tag {
            1 => Some(1),
            2 => Some(2),
            3 | 5 => Some(4),
            4 | 6 => Some(8),
            _ => None,
        };
        if let Some(n) = fixed(tag) {
            self.take(n)?;
            return Ok(());
        }
        match tag {
            7 | 11 | 12 => {
                let unit = match tag {
                    7 => 1,
                    11 => 4,
                    _ => 8,
                };
                let n = self.len()?.checked_mul(unit).context("array too large in level.dat")?;
                self.take(n)?;
            }
            8 => {
                self.string()?;
            }
            9 => {
                let item = self.u8()?;
                let n = self.len()?;
                match fixed(item) {
                    Some(unit) => {
                        self.take(n.checked_mul(unit).context("list too large in level.dat")?)?;
                    }
                    None if n == 0 => {}
                    None => {
                        for _ in 0..n {
                            self.skip(item, depth + 1)?;
                        }
                    }
                }
            }
            10 => loop {
                let inner = self.u8()?;
                if inner == 0 {
                    break;
                }
                self.string()?;
                self.skip(inner, depth + 1)?;
            },
            _ => bail!("unknown tag {tag} in level.dat"),
        }
        Ok(())
    }

    fn data(&mut self, level: &mut Level) -> Result<()> {
        loop {
            let tag = self.u8()?;
            if tag == 0 {
                return Ok(());
            }
            let name = self.string()?;
            match (tag, name.as_str()) {
                (8, "LevelName") => level.name = Some(self.string()?),
                (4, "LastPlayed") => level.last_played = Some(self.i64()?),
                (3, "GameType") => {
                    level.mode = match self.i32()? {
                        0 => Some(Mode::Survival),
                        1 => Some(Mode::Creative),
                        2 => Some(Mode::Adventure),
                        3 => Some(Mode::Spectator),
                        _ => None,
                    }
                }
                (1, "hardcore") => level.hardcore = self.u8()? != 0,
                _ => self.skip(tag, 1)?,
            }
        }
    }
}

/// The game folders of `instance` with the account each belongs to.
pub fn game_dirs(instance_dir: &Path) -> Vec<(Uuid, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(instance::games_dir(instance_dir)) else { return Vec::new() };
    let mut dirs: Vec<(Uuid, PathBuf)> = entries.filter_map(Result::ok).filter(|e| e.path().is_dir()).filter_map(|e| Some((Uuid::try_parse(e.file_name().to_str()?).ok()?, e.path()))).collect();
    dirs.sort_by_key(|(id, _)| *id);
    dirs
}

/// Every world in every game folder of the instance in `instance_dir`, last played first.
pub fn list(instance_dir: &Path) -> Vec<World> {
    let mut worlds = Vec::new();
    for (account, game) in game_dirs(instance_dir) {
        let Ok(entries) = std::fs::read_dir(game.join(SAVES)) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let dir = entry.path();
            let Some(folder) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if folder.starts_with('.') || !dir.join(LEVEL).is_file() {
                continue;
            }
            let mut level = std::fs::read(dir.join(LEVEL)).ok().and_then(|b| parse_level(&b).ok()).unwrap_or_default();
            if level.last_played.is_none() {
                level.last_played = modified_millis(&dir.join(LEVEL));
            }
            let icon = Some(dir.join(ICON)).filter(|p| p.is_file());
            worlds.push(World { account, folder, dir, level, icon });
        }
    }
    worlds.sort_by(|a, b| b.level.last_played.cmp(&a.level.last_played).then_with(|| a.folder.cmp(&b.folder)));
    worlds
}

fn modified_millis(path: &Path) -> Option<i64> {
    let at = std::fs::metadata(path).ok()?.modified().ok()?;
    i64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// Bytes in `dir` and below, links not followed.
pub fn size(dir: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(dir) else { return 0 };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries.filter_map(Result::ok).map(|e| size(&e.path())).sum()
}

/// Whether a running game has the world in `dir` open: it holds the lock on session.lock.
pub fn is_open(dir: &Path) -> bool {
    let path = dir.join(LOCK);
    let file = match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return path.exists(),
    };
    locked(&file)
}

#[cfg(unix)]
fn locked(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as _;
    lock.l_whence = libc::SEEK_SET as _;
    let ok = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) } == 0;
    (ok && lock.l_type != libc::F_UNLCK as _) || file.try_lock().is_err()
}

#[cfg(not(unix))]
fn locked(file: &std::fs::File) -> bool {
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Folder holding the backups of the instance in `instance_dir`.
pub fn backups_dir(instance_dir: &Path) -> PathBuf {
    instance_dir.join(BACKUPS)
}

/// Every backup Manycraft made of a world of the instance in `instance_dir`, newest first. Zips without Manycraft's
/// tag are left out.
pub fn backups(instance_dir: &Path) -> Vec<Backup> {
    let Ok(entries) = std::fs::read_dir(backups_dir(instance_dir)) else { return Vec::new() };
    let mut found: Vec<Backup> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")))
        .filter_map(|path| {
            let file = std::fs::File::open(&path).ok()?;
            let size = file.metadata().ok()?.len();
            let zip = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;
            let tag: Tag = serde_json::from_slice(zip.comment()).ok()?;
            if !one_folder(&tag.folder) {
                return None;
            }
            Some(Backup { path, account: tag.account, folder: tag.folder, name: tag.name, at: tag.at, size })
        })
        .collect();
    found.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.path.cmp(&b.path)));
    found
}

/// Zips `world` into the instance's backup folder as "<folder> <date>.zip". Leaves no half-written zip behind.
pub fn back_up(instance_dir: &Path, world: &World, progress: &Progress) -> Result<Backup> {
    let now = chrono::Local::now();
    back_up_at(instance_dir, world, now.timestamp(), &now.format("%Y-%m-%d %H-%M-%S").to_string(), progress)
}

fn back_up_at(instance_dir: &Path, world: &World, at: i64, stamp: &str, progress: &Progress) -> Result<Backup> {
    let dir = backups_dir(instance_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = (1..).map(|n| if n == 1 { dir.join(format!("{} {stamp}.zip", world.folder)) } else { dir.join(format!("{} {stamp} ({n}).zip", world.folder)) }).find(|p| !p.exists()).context("no free backup name")?;
    let part = path.with_extension("zip.part");
    progress.total.store(size(&world.dir), Ordering::Relaxed);
    let written = (|| -> Result<()> {
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(&part).with_context(|| format!("cannot create {}", part.display()))?));
        let tag = Tag { account: world.account, folder: world.folder.clone(), name: world.name().to_owned(), at };
        zip.set_comment(serde_json::to_string(&tag)?)?;
        add_tree(&mut zip, &world.dir, &world.folder, progress)?;
        zip.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &path).with_context(|| format!("cannot finish {}", path.display()))?;
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    Ok(Backup { path, account: world.account, folder: world.folder.clone(), name: world.name().to_owned(), at, size })
}

fn add_tree<W: Write + std::io::Seek>(zip: &mut zip::ZipWriter<W>, dir: &Path, prefix: &str, progress: &Progress) -> Result<()> {
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(false);
    let mut entries: Vec<_> = std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let inner = format!("{prefix}/{name}");
        if meta.is_dir() {
            zip.add_directory(&inner, options)?;
            add_tree(zip, &path, &inner, progress)?;
        } else if meta.is_file() && name == LOCK {
            progress.add(meta.len());
        } else if meta.is_file() {
            zip.start_file(&inner, options.large_file(meta.len() >= u32::MAX.into()))?;
            let mut file = std::fs::File::open(&path).with_context(|| format!("cannot read {}", path.display()))?;
            let mut buf = vec![0; 64 << 10];
            loop {
                let n = file.read(&mut buf).with_context(|| format!("cannot read {}", path.display()))?;
                if n == 0 {
                    break;
                }
                zip.write_all(&buf[..n])?;
                progress.add(n as u64);
            }
        }
    }
    Ok(())
}

/// What a restore did: where the world is now, and the trash entry of the world it replaced.
#[derive(Debug)]
pub struct Restored {
    pub world: PathBuf,
    pub replaced: Option<String>,
}

/// Unpacks `backup` into the game folder `game_dir` under its folder name. A world there goes to the instance's trash
/// first; when the unpacking fails, nothing changes.
pub fn restore(instance_dir: &Path, backup: &Backup, game_dir: &Path, progress: &Progress) -> Result<Restored> {
    if !one_folder(&backup.folder) {
        bail!("invalid world folder {:?}", backup.folder);
    }
    let saves = game_dir.join(SAVES);
    let target = saves.join(&backup.folder);
    let open = || Final(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst."));
    if is_open(&target) {
        bail!(open());
    }
    std::fs::create_dir_all(&saves).with_context(|| format!("cannot create {}", saves.display()))?;
    let temp = fresh_temp(&saves, &backup.folder, "restoring")?;
    if let Err(e) = unpack(&backup.path, &backup.folder, &temp, progress) {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(e);
    }
    if is_open(&target) {
        let _ = std::fs::remove_dir_all(&temp);
        bail!(open());
    }
    let replaced = match std::fs::symlink_metadata(&target) {
        Ok(_) => match trash::put(instance_dir, &target) {
            Ok(entry) => Some(entry),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&temp);
                return Err(e);
            }
        },
        Err(_) => None,
    };
    if let Err(e) = std::fs::rename(&temp, &target) {
        let back = replaced.as_ref().is_none_or(|entry| trash::take_back(instance_dir, entry).is_ok());
        if back {
            let _ = std::fs::remove_dir_all(&temp);
        }
        return Err(anyhow::Error::new(e).context(format!("cannot move the restored world to {}", target.display())));
    }
    Ok(Restored { world: target, replaced })
}

fn unpack(zip_path: &Path, folder: &str, into: &Path, progress: &Progress) -> Result<()> {
    let file = std::fs::File::open(zip_path).with_context(|| format!("cannot read {}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).context("the backup is damaged")?;
    let total: u64 = (0..zip.len()).filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.size())).sum();
    progress.total.store(total, Ordering::Relaxed);
    let mut any = false;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).context("the backup is damaged")?;
        let Some(name) = entry.enclosed_name() else { bail!("the backup holds an unsafe path") };
        let Ok(rel) = name.strip_prefix(folder) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = into.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&out).with_context(|| format!("cannot write {}", out.display()))?;
        let mut buf = vec![0; 64 << 10];
        loop {
            let n = entry.read(&mut buf).context("the backup is damaged")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            progress.add(n as u64);
        }
        any = true;
    }
    if !any || !into.join(LEVEL).is_file() {
        bail!(Final(tr("In der Sicherung ist keine Welt.")));
    }
    Ok(())
}

/// Copies `world` into the saves of the game folder `game_dir`, under its folder name or "<name> (2)", … when that is
/// taken, and returns the new folder. A copy that fails halfway is removed again.
pub fn copy_to(world: &World, game_dir: &Path, progress: &Progress) -> Result<PathBuf> {
    if is_open(&world.dir) {
        bail!(Final(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst.")));
    }
    let saves = game_dir.join(SAVES);
    std::fs::create_dir_all(&saves).with_context(|| format!("cannot create {}", saves.display()))?;
    let target = (1..).map(|n| if n == 1 { saves.join(&world.folder) } else { saves.join(format!("{} ({n})", world.folder)) }).find(|p| std::fs::symlink_metadata(p).is_err()).context("no free world name")?;
    let temp = fresh_temp(&saves, &world.folder, "copying")?;
    progress.total.store(size(&world.dir), Ordering::Relaxed);
    let copied = copy_tree(&world.dir, &temp, progress).and_then(|()| std::fs::rename(&temp, &target).with_context(|| format!("cannot finish {}", target.display())));
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(e);
    }
    Ok(target)
}

fn one_folder(folder: &str) -> bool {
    let mut parts = Path::new(folder).components();
    matches!((parts.next(), parts.next()), (Some(Component::Normal(_)), None)) && !folder.contains(['/', '\\'])
}

fn fresh_temp(saves: &Path, folder: &str, what: &str) -> Result<PathBuf> {
    for n in 0..1000 {
        let temp = saves.join(format!(".{folder}.{n}.{what}"));
        match std::fs::create_dir(&temp) {
            Ok(()) => return Ok(temp),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("cannot create {}", temp.display())),
        }
    }
    bail!("no free temporary folder in {}", saves.display())
}

fn copy_tree(from: &Path, to: &Path, progress: &Progress) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("cannot read {}", from.display()))? {
        let entry = entry?;
        let (path, name) = (entry.path(), entry.file_name());
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            copy_tree(&path, &to.join(&name), progress)?;
        } else if meta.is_file() && name == LOCK {
            progress.add(meta.len());
        } else if meta.is_file() {
            std::fs::copy(&path, to.join(&name)).with_context(|| format!("cannot copy {}", path.display()))?;
            progress.add(meta.len());
        }
    }
    Ok(())
}

/// Moves `world` to the instance's trash and returns the entry.
pub fn delete(instance_dir: &Path, world: &Path) -> Result<String> {
    if is_open(world) {
        bail!(Final(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst.")));
    }
    trash::put(instance_dir, world)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tag(out: &mut Vec<u8>, kind: u8, name: &str) {
        out.push(kind);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
    }

    /// A gzipped level.dat with these values and some fields to skip around them.
    pub(crate) fn level_dat(name: &str, last_played: i64, game_type: i32, hardcore: bool) -> Vec<u8> {
        let mut b = Vec::new();
        tag(&mut b, 10, "");
        tag(&mut b, 10, "Data");
        tag(&mut b, 9, "ServerBrands");
        b.push(8);
        b.extend_from_slice(&1i32.to_be_bytes());
        b.extend_from_slice(&6u16.to_be_bytes());
        b.extend_from_slice(b"fabric");
        tag(&mut b, 10, "Version");
        tag(&mut b, 3, "Id");
        b.extend_from_slice(&4000i32.to_be_bytes());
        b.push(0);
        tag(&mut b, 12, "Seeds");
        b.extend_from_slice(&2i32.to_be_bytes());
        b.extend_from_slice(&[0; 16]);
        tag(&mut b, 8, "LevelName");
        b.extend_from_slice(&(name.len() as u16).to_be_bytes());
        b.extend_from_slice(name.as_bytes());
        tag(&mut b, 4, "LastPlayed");
        b.extend_from_slice(&last_played.to_be_bytes());
        tag(&mut b, 3, "GameType");
        b.extend_from_slice(&game_type.to_be_bytes());
        tag(&mut b, 1, "hardcore");
        b.push(u8::from(hardcore));
        tag(&mut b, 9, "Empty");
        b.push(0);
        b.extend_from_slice(&0i32.to_be_bytes());
        b.push(0);
        tag(&mut b, 3, "After");
        b.extend_from_slice(&1i32.to_be_bytes());
        b.push(0);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&b).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn level_dat_gives_name_last_played_mode_and_hardcore() {
        let level = parse_level(&level_dat("Meine Welt ü", 1_700_000_000_000, 1, true)).unwrap();
        assert_eq!(level, Level { name: Some("Meine Welt ü".into()), last_played: Some(1_700_000_000_000), mode: Some(Mode::Creative), hardcore: true });
        let level = parse_level(&level_dat("S", 1, 0, false)).unwrap();
        assert_eq!((level.mode, level.hardcore), (Some(Mode::Survival), false));
        assert_eq!(parse_level(&level_dat("X", 1, 9, false)).unwrap().mode, None);
    }

    #[test]
    fn broken_level_dats_are_errors_not_panics() {
        let good = level_dat("A", 1, 0, false);
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(&good[..]).read_to_end(&mut raw).unwrap();
        for cut in 0..raw.len() {
            assert!(parse_level(&raw[..cut]).is_err(), "{cut}");
        }
        assert!(parse_level(&[]).is_err());
        assert!(parse_level(&[0x1f, 0x8b, 0, 0]).is_err());
        let mut negative = Vec::new();
        tag(&mut negative, 10, "");
        tag(&mut negative, 7, "x");
        negative.extend_from_slice(&(-1i32).to_be_bytes());
        assert!(parse_level(&negative).is_err());
        let mut deep = Vec::new();
        tag(&mut deep, 10, "");
        for _ in 0..1000 {
            tag(&mut deep, 10, "a");
        }
        assert!(parse_level(&deep).is_err());
    }

    fn world_in(instance: &Path, account: Uuid, folder: &str) -> World {
        let dir = instance::games_dir(instance).join(account.simple().to_string()).join(SAVES).join(folder);
        std::fs::create_dir_all(dir.join("region")).unwrap();
        std::fs::write(dir.join(LEVEL), level_dat(folder, 5, 0, false)).unwrap();
        std::fs::write(dir.join("region").join("r.0.0.mca"), vec![7u8; 100_000]).unwrap();
        std::fs::write(dir.join(LOCK), "x").unwrap();
        list(instance).into_iter().find(|w| w.folder == folder && w.account == account).unwrap()
    }

    #[test]
    fn worlds_are_found_in_every_game_folder_with_their_account() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        world_in(dir.path(), a, "Eins");
        world_in(dir.path(), b, "Zwei");
        std::fs::create_dir_all(instance::games_dir(dir.path()).join(a.simple().to_string()).join(SAVES).join("kein level")).unwrap();
        let found = list(dir.path());
        assert_eq!(found.iter().map(|w| (w.account, w.name())).collect::<Vec<_>>(), vec![(a, "Eins"), (b, "Zwei")]);
        assert!(size(&found[0].dir) > 100_000);
    }

    #[test]
    fn backup_and_restore_round_trip_and_the_replaced_world_can_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let account = Uuid::from_u128(3);
        let world = world_in(dir.path(), account, "Farm");
        let progress = Progress::default();
        let backup = back_up_at(dir.path(), &world, 100, "2026-11-15 10-00-00", &progress).unwrap();
        assert_eq!(progress.fraction(), 1.);
        assert_eq!(backup.path.file_name().unwrap(), "Farm 2026-11-15 10-00-00.zip");
        let again = back_up_at(dir.path(), &world, 101, "2026-11-15 10-00-00", &Progress::default()).unwrap();
        assert_eq!(again.path.file_name().unwrap(), "Farm 2026-11-15 10-00-00 (2).zip");
        let listed = backups(dir.path());
        assert_eq!(listed.iter().map(|b| (b.at, b.account, b.folder.as_str(), b.name.as_str())).collect::<Vec<_>>(), vec![(101, account, "Farm", "Farm"), (100, account, "Farm", "Farm")]);
        std::fs::write(world.dir.join("region").join("r.0.0.mca"), b"griefed").unwrap();
        let game = instance::games_dir(dir.path()).join(account.simple().to_string());
        let restored = restore(dir.path(), &backup, &game, &Progress::default()).unwrap();
        assert_eq!(restored.world, world.dir);
        assert_eq!(std::fs::read(world.dir.join("region").join("r.0.0.mca")).unwrap(), vec![7u8; 100_000]);
        assert!(!world.dir.join(LOCK).exists());
        let entry = restored.replaced.unwrap();
        trash::put(dir.path(), &world.dir).unwrap();
        trash::take_back(dir.path(), &entry).unwrap();
        assert_eq!(std::fs::read(world.dir.join("region").join("r.0.0.mca")).unwrap(), b"griefed");
    }

    #[test]
    fn a_backup_restores_into_an_empty_game_folder_and_a_broken_one_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let account = Uuid::from_u128(4);
        let world = world_in(dir.path(), account, "Insel");
        let backup = back_up_at(dir.path(), &world, 1, "s", &Progress::default()).unwrap();
        std::fs::remove_dir_all(&world.dir).unwrap();
        let game = instance::games_dir(dir.path()).join(account.simple().to_string());
        let restored = restore(dir.path(), &backup, &game, &Progress::default()).unwrap();
        assert!(restored.replaced.is_none() && world.dir.join(LEVEL).is_file());
        std::fs::write(&backup.path, b"not a zip").unwrap();
        assert!(restore(dir.path(), &backup, &game, &Progress::default()).is_err());
        assert!(world.dir.join(LEVEL).is_file());
        assert!(std::fs::read_dir(game.join(SAVES)).unwrap().filter_map(Result::ok).all(|e| !e.file_name().to_string_lossy().starts_with('.')));
    }

    #[test]
    fn a_backup_tag_must_name_one_folder() {
        for bad in ["", ".", "..", "a/b", "a\\b", "/abs"] {
            assert!(!one_folder(bad), "{bad:?}");
        }
        assert!(one_folder("Neue Welt") && one_folder("Insel (2)"));
        let dir = tempfile::tempdir().unwrap();
        let account = Uuid::from_u128(5);
        let world = world_in(dir.path(), account, "Insel");
        let mut backup = back_up_at(dir.path(), &world, 1, "s", &Progress::default()).unwrap();
        backup.folder = String::new();
        let game = instance::games_dir(dir.path()).join(account.simple().to_string());
        assert!(restore(dir.path(), &backup, &game, &Progress::default()).is_err());
        assert!(world.dir.join(LEVEL).is_file());
    }

    #[test]
    fn copies_get_a_free_name_in_the_other_game_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::from_u128(5), Uuid::from_u128(6));
        let world = world_in(dir.path(), a, "Basis");
        world_in(dir.path(), b, "Basis");
        let game = instance::games_dir(dir.path()).join(b.simple().to_string());
        let copied = copy_to(&world, &game, &Progress::default()).unwrap();
        assert_eq!(copied.file_name().unwrap(), "Basis (2)");
        assert_eq!(size(&copied), size(&world.dir) - 1);
    }

    #[test]
    fn a_held_session_lock_means_open() {
        let dir = tempfile::tempdir().unwrap();
        let world = world_in(dir.path(), Uuid::from_u128(7), "Offen");
        assert!(!is_open(&world.dir));
        let held = std::fs::OpenOptions::new().read(true).write(true).open(world.dir.join(LOCK)).unwrap();
        held.lock().unwrap();
        assert!(is_open(&world.dir));
        drop(held);
        assert!(!is_open(&world.dir));
    }
}
