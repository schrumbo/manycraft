//! Putting a downloaded version in place, and going back to the kept one. A copy of the running binary does the work
//! once the app has ended (`manycraft apply-update <plan> <pid>`), so no file is replaced while it runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use super::release::{self, Kind};
use super::{Fresh, Previous, State, Trouble, edit, verify};

const PLAN: &str = "plan.json";
const KEPT: &str = "previous";
const KEEPING: &str = "previous-new";
const HELPER: &str = "helper";
const LOG: &str = "update.log";
const RUNNING: &str = "running.lock";
const WAIT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);
/// Starts in a row that ended early before a fresh version is given up.
pub const FAILED_STARTS: u32 = 2;

/// How this copy was installed and the file or bundle an update replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Here {
    pub kind: Kind,
    pub target: PathBuf,
}

/// How the running copy was installed.
pub fn here() -> Here {
    let exe = std::env::current_exe().unwrap_or_default();
    let image = std::env::var_os("APPIMAGE").map(PathBuf::from).zip(std::env::var_os("APPDIR").map(PathBuf::from));
    detect(std::env::consts::OS, &exe, image)
}

fn detect(os: &str, exe: &Path, appimage: Option<(PathBuf, PathBuf)>) -> Here {
    let kind = |kind, target: &Path| Here { kind, target: target.to_path_buf() };
    match os {
        "windows" if exe.parent().is_some_and(|d| d.join("Uninstall.exe").is_file()) => kind(Kind::Installer, exe),
        "macos" => {
            let bundle = exe.parent().filter(|d| d.ends_with("Contents/MacOS")).and_then(Path::parent).and_then(Path::parent);
            match bundle.filter(|b| b.extension().is_some_and(|e| e == "app")) {
                Some(bundle) => kind(Kind::Bundle, bundle),
                None => kind(Kind::Unknown, exe),
            }
        }
        "linux" => match appimage.filter(|(image, appdir)| image.is_file() && exe.starts_with(appdir)) {
            Some((image, _)) => kind(Kind::AppImage, &image),
            None if exe.starts_with("/usr/") => kind(Kind::Package, exe),
            None => kind(Kind::Unknown, exe),
        },
        _ => kind(Kind::Unknown, exe),
    }
}

/// What the helper does after the app has ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub rollback: bool,
    pub kind: Kind,
    pub target: PathBuf,
    /// The installer, or the kept previous version when going back.
    pub file: PathBuf,
    /// The version that will be in place afterwards.
    pub version: String,
    pub notes: String,
    /// The version running now.
    pub from: String,
    pub relaunch: bool,
    /// When the app that starts the helper began, to tell it from a later process that got its id.
    #[serde(default)]
    pub started: Option<String>,
}

/// The plan that installs the ready update over the running `current`, when there is one this install can take.
pub fn install_plan(state: &State, dir: &Path, here: &Here, current: &str, relaunch: bool) -> Option<Plan> {
    let ready = super::ready_for(state, dir, current)?;
    here.kind.installs_itself().then(|| Plan { rollback: false, kind: here.kind, target: here.target.clone(), file: dir.join(&ready.file), version: ready.version, notes: ready.notes, from: current.to_owned(), relaunch, started: None })
}

/// The plan that puts the kept previous version back.
pub fn rollback_plan(state: &State, here: &Here, current: &str, relaunch: bool) -> Option<Plan> {
    let previous = super::previous(state)?;
    here.kind.installs_itself().then(|| Plan { rollback: true, kind: here.kind, target: here.target.clone(), file: previous.path, version: previous.version, notes: String::new(), from: current.to_owned(), relaunch, started: None })
}

/// Starts the helper that carries out `plan` once this process has ended.
pub fn spawn(dir: &Path, plan: &Plan) -> Result<()> {
    let helper_dir = dir.join(HELPER);
    std::fs::create_dir_all(&helper_dir).with_context(|| format!("cannot create {}", helper_dir.display()))?;
    let helper = helper_dir.join(if cfg!(windows) { "manycraft-update.exe" } else { "manycraft-update" });
    let exe = std::env::current_exe().context("cannot find the running program")?;
    std::fs::copy(&exe, &helper).with_context(|| format!("cannot copy the updater to {}", helper.display()))?;
    let plan_path = dir.join(PLAN);
    let plan = Plan { started: started(std::process::id()), ..plan.clone() };
    super::net::write_atomic(&plan_path, &serde_json::to_vec_pretty(&plan)?)?;
    let mut command = Command::new(&helper);
    command.arg("apply-update").arg(&plan_path).arg(std::process::id().to_string());
    detached(&mut command).with_context(|| format!("cannot start {}", helper.display()))
}

#[cfg(windows)]
fn detached(command: &mut Command) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const NEW_GROUP: u32 = 0x0000_0200;
    const NO_WINDOW: u32 = 0x0800_0000;
    const BREAKAWAY: u32 = 0x0100_0000;
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match command.creation_flags(NEW_GROUP | NO_WINDOW | BREAKAWAY).spawn() {
        Ok(_) => Ok(()),
        Err(_) => command.creation_flags(NEW_GROUP | NO_WINDOW).spawn().map(drop),
    }
}

#[cfg(unix)]
fn detached(command: &mut Command) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().map(drop)
}

/// `manycraft apply-update <plan> <pid>`: waits for `pid` to end, then carries out the plan.
pub fn cli(args: &[String]) -> i32 {
    let [plan_path, pid] = args else {
        eprintln!("usage: manycraft apply-update <plan> <pid>");
        return 2;
    };
    let plan_path = PathBuf::from(plan_path);
    let dir = plan_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let plan: Plan = match std::fs::read(&plan_path).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice(&b)?)) {
        Ok(plan) => plan,
        Err(e) => {
            note(&dir, &format!("unreadable plan: {e:#}"));
            return 1;
        }
    };
    let _ = std::fs::remove_file(&plan_path);
    let ended = pid.parse().is_ok_and(|pid| wait_gone(pid, plan.started.as_deref()));
    let result = if !ended {
        Err(anyhow::anyhow!("Manycraft did not end"))
    } else {
        match crate::core::lock::exclusive_within(&dir.join(RUNNING), WAIT) {
            Ok(_alone) => apply(&dir, &plan, &verify::release_key()),
            Err(_) => Err(anyhow::anyhow!("another Manycraft is still running")),
        }
    };
    if let Err(e) = &result {
        note(&dir, &format!("{} {} failed: {e:#}", if plan.rollback { "going back to" } else { "updating to" }, plan.version));
    }
    if plan.relaunch && ended {
        if let Err(e) = relaunch(plan.kind, &plan.target) {
            note(&dir, &format!("cannot start Manycraft again: {e:#}"));
        }
    }
    i32::from(result.is_err())
}

fn wait_gone(pid: u32, started: Option<&str>) -> bool {
    let until = Instant::now() + WAIT;
    while !gone(pid, started) {
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

/// Whether the process that began at `started` as `pid` has ended; a later process with the same id does not count as it.
fn gone(pid: u32, started: Option<&str>) -> bool {
    !crate::core::drive::alive(pid) || started.is_some_and(|then| self::started(pid).is_none_or(|now| now != then))
}

#[cfg(windows)]
fn started(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut times: [FILETIME; 4] = std::mem::zeroed();
        let [created, exited, kernel, user] = &mut times;
        let ok = GetProcessTimes(handle, created, exited, kernel, user) != 0;
        CloseHandle(handle);
        ok.then(|| ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)).to_string())
    }
}

#[cfg(target_os = "linux")]
fn started(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(19).map(str::to_owned)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn started(pid: u32) -> Option<String> {
    let out = Command::new("ps").args(["-o", "lstart=", "-p"]).arg(pid.to_string()).stdin(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn note(dir: &Path, line: &str) {
    use std::io::Write;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(LOG)) {
        let _ = writeln!(f, "{stamp} {line}");
    }
}

/// Carries out `plan` and records the outcome in the state in `dir`.
pub fn apply(dir: &Path, plan: &Plan, key: &str) -> Result<()> {
    if plan.rollback {
        let result = roll_back(plan);
        edit(dir, |s| match &result {
            Ok(()) => {
                s.skipped = Some(plan.from.clone());
                s.fresh = None;
                s.previous = None;
                s.newest = None;
                s.checked_at = 0;
            }
            Err(_) => {
                s.trouble = Some(Trouble::Install);
                if let Some(fresh) = s.fresh.as_mut() {
                    fresh.proven = true;
                }
            }
        })?;
        if result.is_ok() {
            super::discard_ready(dir);
            remove_all(&dir.join(KEPT));
        }
        return result;
    }
    let name = plan.target.file_name().context("the install has no file name")?;
    if super::load(dir).previous.is_some_and(|p| p.path.starts_with(dir.join(KEEPING))) {
        ensure!(keep_previous(dir), "the copy kept from the last failed install is still in use");
        edit(dir, |s| {
            if let Some(previous) = s.previous.as_mut() {
                previous.path = dir.join(KEPT).join(name);
            }
        })?;
    }
    remove_all(&dir.join(KEEPING));
    let result = install(dir, plan, key, &dir.join(KEEPING).join(name));
    let unrestored = result.as_ref().is_err_and(|e| e.downcast_ref::<Unrestored>().is_some());
    let kept = (result.is_ok() || unrestored).then(|| keep_previous(dir).then(|| Previous { version: plan.from.clone(), path: dir.join(KEPT).join(name) }));
    let kept = match kept {
        Some(None) if unrestored => Some(Some(Previous { version: plan.from.clone(), path: dir.join(KEEPING).join(name) })),
        kept => {
            remove_all(&dir.join(KEEPING));
            kept
        }
    };
    super::discard_ready(dir);
    edit(dir, |s| match &result {
        Ok(()) => {
            s.newest = None;
            s.trouble = None;
            s.failed_at = 0;
            s.fresh = Some(Fresh { version: plan.version.clone(), notes: plan.notes.clone(), ..Fresh::default() });
            s.previous = kept.clone().flatten();
        }
        Err(_) => {
            s.failed_at = super::now();
            s.trouble = Some(Trouble::Install);
            if let Some(previous) = kept.clone().flatten() {
                s.previous = Some(previous);
            }
        }
    })?;
    result
}

/// The installer failed and the old version could not be put back; the copy kept during the install must stay.
#[derive(Debug)]
struct Unrestored;

impl std::fmt::Display for Unrestored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the old version could not be put back")
    }
}

fn install(dir: &Path, plan: &Plan, key: &str, kept: &Path) -> Result<()> {
    let read = |ext| std::fs::read_to_string(super::sidecar_path(&plan.file, ext)).with_context(|| format!("no .{ext} next to the installer"));
    let name = plan.file.file_name().context("the installer has no file name")?.to_string_lossy();
    verify::file(&plan.file, &read("sha256")?, &read("minisig")?, key, &verify::comment(&plan.version, &name))?;
    ensure!(plan.target.exists(), "{} is gone", plan.target.display());
    match plan.kind {
        Kind::Installer => {
            keep_copy(&plan.target, kept)?;
            match run_installer(&plan.file, plan.target.parent().context("the install has no folder")?) {
                Err(e) => match place_file(kept, &plan.target) {
                    Ok(()) => {
                        show_version(&plan.from);
                        Err(e)
                    }
                    Err(_) => Err(e.context(Unrestored)),
                },
                ok => ok,
            }
        }
        Kind::AppImage => {
            keep_copy(&plan.target, kept)?;
            place_file(&plan.file, &plan.target)
        }
        Kind::Bundle => install_bundle(dir, &plan.file, &plan.target, kept),
        Kind::Package | Kind::Unknown => bail!("this install cannot update itself"),
    }
}

fn roll_back(plan: &Plan) -> Result<()> {
    ensure!(plan.file.exists(), "the previous version is gone");
    match plan.kind {
        Kind::Installer => {
            place_file(&plan.file, &plan.target)?;
            show_version(&plan.version);
            Ok(())
        }
        Kind::AppImage => place_file(&plan.file, &plan.target),
        Kind::Bundle => swap_bundle(&|new| copy_bundle(&plan.file, new), &plan.target, None),
        Kind::Package | Kind::Unknown => bail!("this install cannot go back by itself"),
    }
}

#[cfg(windows)]
const UNINSTALL: &str =r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft";

/// Makes the version Windows lists for Manycraft under Apps match the files in place; nothing changes without an installed entry.
#[cfg(windows)]
fn show_version(version: &str) {
    registry::set_version(&uninstall_key(), version);
}

/// The registry key of the install entry. Debug builds take `MANYCRAFT_UNINSTALL_KEY`, for tests with an installer of their own.
#[cfg(windows)]
fn uninstall_key() -> String {
    #[cfg(debug_assertions)]
    if let Some(key) = std::env::var("MANYCRAFT_UNINSTALL_KEY").ok().filter(|k| !k.trim().is_empty()) {
        return key;
    }
    UNINSTALL.to_owned()
}

#[cfg(not(windows))]
fn show_version(_: &str) {}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegOpenKeyExW, RegSetValueExW};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    pub fn set_version(key: &str, version: &str) -> bool {
        let mut handle: HKEY = std::ptr::null_mut();
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, wide(key).as_ptr(), 0, KEY_SET_VALUE, &mut handle) != ERROR_SUCCESS {
                return false;
            }
            let data = wide(version);
            let status = RegSetValueExW(handle, wide("DisplayVersion").as_ptr(), 0, REG_SZ, data.as_ptr().cast(), u32::try_from(data.len() * 2).unwrap_or(0));
            RegCloseKey(handle);
            status == ERROR_SUCCESS
        }
    }
}

#[cfg(windows)]
fn run_installer(installer: &Path, folder: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let status = Command::new(installer).arg("/S").raw_arg(format!("/D={}", folder.display())).status().context("cannot start the installer")?;
    ensure!(status.success(), "the installer ended with {status}");
    Ok(())
}

#[cfg(not(windows))]
fn run_installer(_: &Path, _: &Path) -> Result<()> {
    bail!("the Windows installer runs only on Windows")
}

/// Makes the copy kept during this install the previous version, replacing the older one only once the new one is in place.
fn keep_previous(dir: &Path) -> bool {
    let (keeping, kept, older) = (dir.join(KEEPING), dir.join(KEPT), dir.join(format!("{KEPT}-old")));
    remove_all(&older);
    if kept.exists() && std::fs::rename(&kept, &older).is_err() {
        return false;
    }
    if std::fs::rename(&keeping, &kept).is_err() {
        let _ = std::fs::rename(&older, &kept);
        return false;
    }
    remove_all(&older);
    true
}

fn keep_copy(from: &Path, to: &Path) -> Result<()> {
    remove_all(to);
    std::fs::create_dir_all(to.parent().context("no folder for the kept version")?)?;
    std::fs::copy(from, to).with_context(|| format!("cannot keep {}", from.display()))?;
    Ok(())
}

/// Copies `from` next to `target` and renames it over `target`, so `target` is never half written.
fn place_file(from: &Path, target: &Path) -> Result<()> {
    let name = target.file_name().context("no file name")?.to_string_lossy();
    let new = target.with_file_name(format!(".{name}.new"));
    std::fs::copy(from, &new).with_context(|| format!("cannot write {}", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&new, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&new);
    })?;
    Ok(())
}

/// Builds the new bundle with `fill` next to `bundle`, then swaps it in with two renames. The old one goes to `keep`, or away.
fn swap_bundle(fill: &dyn Fn(&Path) -> Result<()>, bundle: &Path, keep: Option<&Path>) -> Result<()> {
    let name = bundle.file_name().context("no bundle name")?.to_string_lossy();
    let (new, old) = (bundle.with_file_name(format!(".{name}.new")), bundle.with_file_name(format!(".{name}.old")));
    remove_all(&new);
    remove_all(&old);
    if let Err(e) = fill(&new) {
        remove_all(&new);
        return Err(e);
    }
    std::fs::rename(bundle, &old).with_context(|| format!("cannot move {}", bundle.display()))?;
    if let Err(e) = std::fs::rename(&new, bundle) {
        let _ = std::fs::rename(&old, bundle);
        remove_all(&new);
        return Err(e).with_context(|| format!("cannot replace {}", bundle.display()));
    }
    match keep {
        Some(keep) => {
            remove_all(keep);
            let _ = keep.parent().map(std::fs::create_dir_all);
            if std::fs::rename(&old, keep).is_err() {
                let copied = copy_bundle(&old, keep);
                remove_all(&old);
                copied?;
            }
        }
        None => remove_all(&old),
    }
    Ok(())
}

fn install_bundle(dir: &Path, dmg: &Path, bundle: &Path, keep: &Path) -> Result<()> {
    let mount = dir.join("mount");
    remove_all(&mount);
    std::fs::create_dir_all(&mount)?;
    run(Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mount).arg(dmg))?;
    let name = bundle.file_name().context("no bundle name")?;
    let source = app_in(&mount, name).unwrap_or_else(|| mount.join(name));
    let result = if source.is_dir() { swap_bundle(&|new| copy_bundle(&source, new), bundle, Some(keep)) } else { Err(anyhow::anyhow!("the disk image holds no {}", source.display())) };
    let _ = run(Command::new("hdiutil").arg("detach").arg(&mount).arg("-force"));
    let _ = std::fs::remove_dir(&mount);
    result
}

/// The app a mounted disk image holds: the one called `name`, else the only one with another name.
fn app_in(mount: &Path, name: &std::ffi::OsStr) -> Option<PathBuf> {
    let same = mount.join(name);
    if same.is_dir() {
        return Some(same);
    }
    let mut apps = std::fs::read_dir(mount).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.extension().is_some_and(|e| e == "app"));
    let first = apps.next()?;
    apps.next().is_none().then_some(first)
}

#[cfg(target_os = "macos")]
fn copy_bundle(from: &Path, to: &Path) -> Result<()> {
    run(Command::new("ditto").arg(from).arg(to))
}

#[cfg(not(target_os = "macos"))]
fn copy_bundle(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_bundle(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    let out = command.stdin(Stdio::null()).output().with_context(|| format!("cannot run {command:?}"))?;
    ensure!(out.status.success(), "{command:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(())
}

fn remove_all(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => drop(std::fs::remove_dir_all(path)),
        Ok(_) => drop(std::fs::remove_file(path)),
        Err(_) => {}
    }
}

fn relaunch_command(kind: Kind, target: &Path) -> Command {
    match kind {
        Kind::Bundle => {
            let mut open = Command::new("open");
            open.arg(target);
            open
        }
        Kind::AppImage => {
            let mut command = Command::new(target);
            for var in ["APPDIR", "APPIMAGE", "ARGV0", "OWD"] {
                command.env_remove(var);
            }
            command
        }
        _ => Command::new(target),
    }
}

fn relaunch(kind: Kind, target: &Path) -> Result<()> {
    detached(&mut relaunch_command(kind, target)).with_context(|| format!("cannot start {}", target.display()))
}

/// Counts this start of `current` if it is a fresh update that has not yet proven itself. True when the earlier
/// starts ended early often enough that this one should go back to the previous version instead.
pub fn starting(state: &mut State, current: &str) -> bool {
    let kept = state.previous.is_some();
    let Some(fresh) = state.fresh.as_mut().filter(|f| release::same(&f.version, current) && !f.proven) else { return false };
    if fresh.failed_starts >= FAILED_STARTS && kept {
        return true;
    }
    fresh.failed_starts += 1;
    false
}

/// Marks the fresh `current` as working: its window stayed up, or it quit normally.
pub fn proven(state: &mut State, current: &str) {
    if let Some(fresh) = state.fresh.as_mut().filter(|f| release::same(&f.version, current)) {
        fresh.failed_starts = 0;
        fresh.proven = true;
    }
}

/// At start: counts the start and, after [`FAILED_STARTS`] early ends of a fresh update in a row, starts the helper
/// that goes back to the previous version. True when this process should end right away.
pub fn boot(current: &str) -> bool {
    let dir = super::dir();
    if !dir.join(super::STATE).is_file() {
        return false;
    }
    let back = match edit(&dir, |s| {
        let back = starting(s, current);
        if back && let Some(fresh) = s.fresh.as_mut() {
            fresh.proven = true;
        }
        back.then(|| s.clone())
    }) {
        Ok(back) => back,
        Err(e) => {
            eprintln!("update: cannot count the start: {e:#}");
            return false;
        }
    };
    let Some(plan) = back.and_then(|s| rollback_plan(&s, &here(), current, true)) else { return false };
    match spawn(&dir, &plan) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("update: cannot go back to {}: {e:#}", plan.version);
            false
        }
    }
}

/// At quit: records that `current` works, then starts the helper that installs the ready update, or goes back to the
/// kept version when `back`. Nothing happens while an instance is still launching. True when a helper was started.
pub fn on_exit(current: &str, back: bool, relaunch: bool, launching: bool) -> Result<bool> {
    let dir = super::dir();
    if !dir.join(super::STATE).is_file() {
        return Ok(false);
    }
    if !back {
        mark_proven(current);
    }
    if launching || !alone(&dir) {
        return Ok(false);
    }
    let state = super::load(&dir);
    let here = here();
    let plan = if back { rollback_plan(&state, &here, current, relaunch) } else { install_plan(&state, &dir, &here, current, relaunch) };
    let Some(plan) = plan else { return Ok(false) };
    spawn(&dir, &plan)?;
    Ok(true)
}

static HELD: std::sync::Mutex<Option<crate::core::lock::Guard>> = std::sync::Mutex::new(None);

/// Marks this process as a running Manycraft, so a helper never installs under another one.
pub fn hold() {
    if let Ok(Some(guard)) = crate::core::lock::try_shared(&super::dir().join(RUNNING)) {
        *HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(guard);
    }
}

/// Trades this process's mark for the exclusive lock, held until the process ends, and tells whether no other Manycraft runs.
fn alone(dir: &Path) -> bool {
    let mut held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held.take();
    match crate::core::lock::try_exclusive(&dir.join(RUNNING)) {
        Ok(Some(guard)) => {
            *held = Some(guard);
            true
        }
        _ => false,
    }
}

/// Tells whether no other Manycraft runs, trying the exclusive lock without waiting and giving it up again at once.
pub fn running_alone() -> bool {
    let mut held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held.take();
    let path = super::dir().join(RUNNING);
    let alone = matches!(crate::core::lock::try_exclusive(&path), Ok(Some(_)));
    *held = crate::core::lock::try_shared(&path).ok().flatten();
    alone
}

/// Records that the running `current` works.
pub fn mark_proven(current: &str) {
    let dir = super::dir();
    if load_fresh(&dir, current) {
        if let Err(e) = edit(&dir, |s| proven(s, current)) {
            eprintln!("update: {e:#}");
        }
    }
}

fn load_fresh(dir: &Path, current: &str) -> bool {
    super::load(dir).fresh.is_some_and(|f| release::same(&f.version, current) && !f.proven)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(version: &str, failed: u32) -> State {
        State {
            fresh: Some(Fresh { version: version.into(), notes: String::new(), failed_starts: failed, proven: false, seen: false }),
            previous: Some(Previous { version: "0.5.0".into(), path: PathBuf::from("kept") }),
            ..State::default()
        }
    }

    #[test]
    fn two_early_ends_in_a_row_go_back_on_the_third_start() {
        let mut state = fresh("0.6.0", 0);
        assert!(!starting(&mut state, "0.6.0"));
        assert!(!starting(&mut state, "0.6.0"));
        assert!(starting(&mut state, "0.6.0"));
    }

    #[test]
    fn a_start_that_stays_up_resets_the_count() {
        let mut state = fresh("0.6.0", 0);
        assert!(!starting(&mut state, "0.6.0"));
        proven(&mut state, "0.6.0");
        for _ in 0..5 {
            assert!(!starting(&mut state, "0.6.0"));
        }
        assert_eq!(state.fresh.as_ref().map(|f| f.failed_starts), Some(0));
    }

    #[test]
    fn only_a_fresh_update_with_a_kept_version_goes_back() {
        let mut other = fresh("0.6.0", 5);
        assert!(!starting(&mut other, "0.7.0"));
        let mut nothing_kept = State { previous: None, ..fresh("0.6.0", 5) };
        assert!(!starting(&mut nothing_kept, "0.6.0"));
        let mut plain = State::default();
        assert!(!starting(&mut plain, "0.6.0"));
        assert_eq!(plain, State::default());
    }

    #[test]
    fn installs_are_recognised() {
        let dir = tempfile::tempdir().unwrap();
        let win = dir.path().join("Programs").join("Manycraft");
        std::fs::create_dir_all(&win).unwrap();
        let exe = win.join("manycraft.exe");
        assert_eq!(detect("windows", &exe, None).kind, Kind::Unknown);
        std::fs::write(win.join("Uninstall.exe"), b"").unwrap();
        assert_eq!(detect("windows", &exe, None), Here { kind: Kind::Installer, target: exe.clone() });
        let bundle = dir.path().join("Manycraft.app");
        let mac = detect("macos", &bundle.join("Contents").join("MacOS").join("manycraft"), None);
        assert_eq!(mac, Here { kind: Kind::Bundle, target: bundle });
        assert_eq!(detect("macos", &dir.path().join("manycraft"), None).kind, Kind::Unknown);
        let image = dir.path().join("Manycraft.AppImage");
        std::fs::write(&image, b"").unwrap();
        assert_eq!(detect("linux", Path::new("/tmp/.mount_x/usr/bin/manycraft"), Some((image.clone(), PathBuf::from("/tmp/.mount_x")))), Here { kind: Kind::AppImage, target: image.clone() });
        assert_eq!(detect("linux", Path::new("/usr/bin/manycraft"), Some((image.clone(), PathBuf::from("/tmp/.mount_other")))).kind, Kind::Package);
        assert_eq!(detect("linux", Path::new("/home/x/manycraft"), Some((image.clone(), PathBuf::from("/tmp/.mount_vscodium")))).kind, Kind::Unknown);
        assert_eq!(detect("linux", Path::new("/home/x/manycraft"), None).kind, Kind::Unknown);
    }

    fn signed(dir: &Path) -> (PathBuf, String) {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/core/testdata/update");
        let file = dir.join("Manycraft-Setup-0.9.0-windows-x64.exe");
        std::fs::copy(fixtures.join("asset.bin"), &file).unwrap();
        std::fs::write(super::super::sidecar_path(&file, "sha256"), "bb0cabcbff009359250bafb84c7be5a7ac81f84340ffc6db268645997b17c308  asset.bin").unwrap();
        std::fs::copy(fixtures.join("asset.bin.minisig"), super::super::sidecar_path(&file, "minisig")).unwrap();
        let key = std::fs::read_to_string(fixtures.join("fixture.pub")).unwrap().lines().nth(1).unwrap().to_owned();
        (file, key)
    }

    #[test]
    fn a_single_file_install_keeps_the_old_one_and_goes_back() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("updates");
        std::fs::create_dir_all(&dir).unwrap();
        let target = root.path().join("Manycraft.AppImage");
        std::fs::write(&target, b"old version").unwrap();
        let (file, key) = signed(&dir);
        edit(&dir, |s| s.ready = Some(super::super::Ready { version: "0.9.0".into(), notes: "Faster.".into(), page: String::new(), file: file.file_name().unwrap().to_string_lossy().into_owned() })).unwrap();
        let here = Here { kind: Kind::AppImage, target: target.clone() };
        let plan = install_plan(&super::super::load(&dir), &dir, &here, "0.5.0", false).unwrap();
        apply(&dir, &plan, &key).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/core/testdata/update/asset.bin")).unwrap());
        assert!(!file.exists());
        let state = super::super::load(&dir);
        assert_eq!(state.ready, None);
        assert_eq!(super::super::news(&state, "0.9.0"), Some(("0.9.0".into(), "Faster.".into())));
        let kept = super::super::previous(&state).unwrap();
        assert_eq!(kept.version, "0.5.0");
        assert_eq!(std::fs::read(&kept.path).unwrap(), b"old version");
        let back = rollback_plan(&state, &here, "0.9.0", false).unwrap();
        apply(&dir, &back, &key).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old version");
        let state = super::super::load(&dir);
        assert_eq!(state.skipped.as_deref(), Some("0.9.0"));
        assert_eq!(state.previous, None);
        assert_eq!(state.fresh, None);
        assert!(!dir.join(KEPT).exists());
    }

    #[test]
    fn a_damaged_installer_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("updates");
        std::fs::create_dir_all(&dir).unwrap();
        let target = root.path().join("Manycraft.AppImage");
        std::fs::write(&target, b"old version").unwrap();
        let (file, key) = signed(&dir);
        std::fs::write(&file, b"swapped after the download").unwrap();
        let plan = Plan { rollback: false, kind: Kind::AppImage, target: target.clone(), file: file.clone(), version: "0.6.0".into(), notes: String::new(), from: "0.5.0".into(), relaunch: false, started: None };
        assert!(apply(&dir, &plan, &key).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old version");
        let state = super::super::load(&dir);
        assert_eq!(state.trouble, Some(Trouble::Install));
        assert_eq!(state.previous, None);
        assert_eq!(state.fresh, None);
    }

    #[test]
    fn a_bundle_swap_keeps_the_old_bundle_and_survives_a_failed_fill() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Manycraft.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Contents").join("v"), b"old").unwrap();
        let source = root.path().join("src.app");
        std::fs::create_dir_all(source.join("Contents")).unwrap();
        std::fs::write(source.join("Contents").join("v"), b"new").unwrap();
        assert!(swap_bundle(&|_| bail!("disk full"), &bundle, None).is_err());
        assert_eq!(std::fs::read(bundle.join("Contents").join("v")).unwrap(), b"old");
        let keep = root.path().join("updates").join(KEPT).join("Manycraft.app");
        swap_bundle(&|new| copy_bundle(&source, new), &bundle, Some(&keep)).unwrap();
        assert_eq!(std::fs::read(bundle.join("Contents").join("v")).unwrap(), b"new");
        assert_eq!(std::fs::read(keep.join("Contents").join("v")).unwrap(), b"old");
        let leftovers: Vec<_> = std::fs::read_dir(root.path()).unwrap().flatten().map(|e| e.file_name()).filter(|n| n.to_string_lossy().starts_with('.')).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_process_that_reused_the_id_is_not_waited_for() {
        let me = std::process::id();
        let stamp = started(me);
        assert!(stamp.is_some());
        assert!(!gone(me, stamp.as_deref()));
        assert!(!gone(me, None));
        assert!(gone(me, Some("an earlier process")));
        assert!(wait_gone(me, Some("an earlier process")));
    }

    #[test]
    fn a_plan_without_a_start_time_still_reads() {
        let old = r#"{"rollback":false,"kind":"Installer","target":"x","file":"y","version":"0.9.0","notes":"","from":"0.8.0","relaunch":true}"#;
        assert_eq!(serde_json::from_str::<Plan>(old).unwrap().started, None);
    }

    #[test]
    fn a_disk_image_with_a_renamed_app_is_still_found() {
        let mount = tempfile::tempdir().unwrap();
        assert_eq!(app_in(mount.path(), "Manycraft.app".as_ref()), None);
        std::fs::create_dir(mount.path().join("Manycraft Beta.app")).unwrap();
        std::fs::write(mount.path().join("README.txt"), b"").unwrap();
        assert_eq!(app_in(mount.path(), "Manycraft.app".as_ref()), Some(mount.path().join("Manycraft Beta.app")));
        std::fs::create_dir(mount.path().join("Other.app")).unwrap();
        assert_eq!(app_in(mount.path(), "Manycraft.app".as_ref()), None);
        std::fs::create_dir(mount.path().join("Manycraft.app")).unwrap();
        assert_eq!(app_in(mount.path(), "Manycraft.app".as_ref()), Some(mount.path().join("Manycraft.app")));
    }

    #[test]
    fn an_app_image_starts_without_the_old_images_environment() {
        let command = relaunch_command(Kind::AppImage, Path::new("Manycraft.AppImage"));
        let removed: Vec<_> = command.get_envs().filter(|(_, value)| value.is_none()).map(|(name, _)| name.to_string_lossy().into_owned()).collect();
        assert_eq!(removed, ["APPDIR", "APPIMAGE", "ARGV0", "OWD"]);
        assert_eq!(relaunch_command(Kind::Bundle, Path::new("Manycraft.app")).get_program(), "open");
        assert_eq!(relaunch_command(Kind::Installer, Path::new("manycraft.exe")).get_envs().count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn the_listed_version_follows_a_rollback() {
        let key = format!(r"Software\ManycraftUpdaterTest{}", std::process::id());
        let reg = |args: &[&str]| Command::new("reg").args(args).output().unwrap();
        let path = format!(r"HKCU\{key}");
        assert!(!registry::set_version(&key, "0.5.0"));
        assert!(reg(&["add", &path, "/v", "DisplayVersion", "/d", "0.9.0", "/f"]).status.success());
        assert!(registry::set_version(&key, "0.5.0"));
        let shown = String::from_utf8_lossy(&reg(&["query", &path, "/v", "DisplayVersion"]).stdout).into_owned();
        reg(&["delete", &path, "/f"]);
        assert!(shown.contains("0.5.0") && !shown.contains("0.9.0"), "{shown}");
    }

    #[test]
    fn nothing_is_planned_for_installs_that_cannot_update_themselves() {
        let root = tempfile::tempdir().unwrap();
        let (file, _) = signed(root.path());
        let state = State {
            ready: Some(super::super::Ready { version: "0.6.0".into(), notes: String::new(), page: String::new(), file: file.file_name().unwrap().to_string_lossy().into_owned() }),
            previous: Some(Previous { version: "0.4.0".into(), path: file.clone() }),
            ..State::default()
        };
        let package = Here { kind: Kind::Package, target: PathBuf::from("/usr/bin/manycraft") };
        assert_eq!(install_plan(&state, root.path(), &package, "0.5.0", true), None);
        assert_eq!(rollback_plan(&state, &package, "0.5.0", true), None);
        let image = Here { kind: Kind::AppImage, target: root.path().join("x") };
        assert!(install_plan(&state, root.path(), &image, "0.5.0", true).is_some());
        assert_eq!(install_plan(&state, root.path(), &image, "0.6.0", true), None);
        assert!(rollback_plan(&state, &image, "0.5.0", true).is_some_and(|p| p.rollback && p.version == "0.4.0"));
    }
}
