//! Finding the games a manager launched again after it was killed: a record of every launch under `MANYCRAFT_HOME`,
//! checked against the running processes so a reused pid is never taken for a game, and one manager per data folder.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{lock, net, paths};

const RECORD_FILE: &str = "launched.json";
const ADDRESS_FILE: &str = "manager.address";
const LOCK_FILE: &str = "manager.lock";
const REOPEN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const EXIT_POLL: Duration = Duration::from_millis(50);

static MANAGER: OnceLock<lock::Guard> = OnceLock::new();

/// One game as the manager launched it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub batch: Option<String>,
    pub instance: String,
    pub account: Uuid,
    pub pid: u32,
    /// When the process started, in the platform's own units.
    pub started: u64,
    pub exe: PathBuf,
    pub game_dir: PathBuf,
    /// The secret its companion mod presents when it connects.
    pub token: String,
    #[serde(default)]
    pub server_addrs: Vec<SocketAddr>,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run").field("batch", &self.batch).field("instance", &self.instance).field("account", &self.account).field("pid", &self.pid).field("started", &self.started).field("exe", &self.exe).field("game_dir", &self.game_dir).field("token", &"<hidden>").field("server_addrs", &self.server_addrs).finish()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    port: Option<u16>,
    #[serde(default)]
    games: BTreeMap<String, Run>,
}

/// The launch record of this data folder, written through on every change.
#[derive(Clone)]
pub struct Ledger {
    path: PathBuf,
    record: Arc<Mutex<Record>>,
}

impl Ledger {
    /// The record under the data folder; empty when it is missing or unreadable.
    pub fn open() -> Self {
        Self::at(paths::data_dir().join(RECORD_FILE))
    }

    pub(crate) fn at(path: PathBuf) -> Self {
        let record = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path, record: Arc::new(Mutex::new(record)) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Record> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, record: &Record) {
        if let Err(e) = serde_json::to_vec_pretty(record).map_err(anyhow::Error::from).and_then(|b| net::write_atomic(&self.path, &b)) {
            eprintln!("launch record: {e:#}");
        }
    }

    /// The port the last manager listened on.
    pub fn port(&self) -> Option<u16> {
        self.lock().port
    }

    /// Records the port the manager listens on now, and writes it where companion mods look for it.
    pub fn set_port(&self, port: u16) {
        let mut record = self.lock();
        if let Some(dir) = self.path.parent()
            && let Err(e) = net::write_atomic(&dir.join(ADDRESS_FILE), address(port).as_bytes())
        {
            eprintln!("manager address: {e:#}");
        }
        if record.port != Some(port) {
            record.port = Some(port);
            self.save(&record);
        }
    }

    /// The recorded games, by key.
    pub fn games(&self) -> BTreeMap<String, Run> {
        self.lock().games.clone()
    }

    /// Records the game `key`, replacing an older launch of it.
    pub fn add(&self, key: &str, run: Run) {
        let mut record = self.lock();
        record.games.insert(key.to_owned(), run);
        self.save(&record);
    }

    /// Forgets the games `keys` launched as `pids`; a newer launch under the same key stays.
    pub fn remove(&self, ended: &[(String, u32)]) {
        let mut record = self.lock();
        let before = record.games.len();
        record.games.retain(|key, run| !ended.iter().any(|(k, pid)| k == key && *pid == run.pid));
        if record.games.len() != before {
            self.save(&record);
        }
    }

    /// Keeps only the games `keep`.
    pub fn keep_only(&self, keep: &[String]) {
        let mut record = self.lock();
        let before = record.games.len();
        record.games.retain(|key, _| keep.contains(key));
        if record.games.len() != before {
            self.save(&record);
        }
    }
}

/// The address companion mods connect to for `port`.
pub fn address(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// Where the manager writes its address, for `-Dmanycraft.ipcfile`.
pub fn address_file() -> PathBuf {
    paths::data_dir().join(ADDRESS_FILE)
}

/// What a start found about the manager of this data folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// This process is the manager now.
    Manager,
    /// Another process is.
    Taken,
}

/// Becomes the one manager of this data folder, for the rest of the process.
pub fn claim() -> Result<Claim> {
    if MANAGER.get().is_some() {
        return Ok(Claim::Manager);
    }
    match lock::try_exclusive(&paths::data_dir().join(LOCK_FILE))? {
        Some(guard) => {
            let _ = MANAGER.set(guard);
            Ok(Claim::Manager)
        }
        None => Ok(Claim::Taken),
    }
}

/// Whether this process is the manager of its data folder, so it may take over the recorded games.
pub fn is_manager() -> bool {
    MANAGER.get().is_some()
}

/// Asks the running manager to come to the front. False when it does not answer.
pub fn reopen_first() -> bool {
    send_first(&serde_json::json!({ "t": "reopen" }))
}

/// Asks the running manager to play an instance. False when it does not answer.
pub fn play_first(play: &super::shortcut::Play) -> bool {
    send_first(&serde_json::json!({ "t": "play", "instance": play.instance, "account": play.account }))
}

fn send_first(message: &serde_json::Value) -> bool {
    let Some(addr) = std::fs::read_to_string(address_file()).ok().and_then(|a| a.trim().parse::<SocketAddr>().ok()) else { return false };
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
        AllowSetForegroundWindow(ASFW_ANY);
    }
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, REOPEN_TIMEOUT) else { return false };
    let _ = stream.set_write_timeout(Some(REOPEN_TIMEOUT));
    stream.write_all(format!("{message}\n").as_bytes()).and_then(|()| stream.flush()).is_ok()
}

/// Who a process is: when it started and which program it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub started: u64,
    pub exe: PathBuf,
}

/// Whether `seen`, the process now behind `run.pid`, is the game that was launched, and its folder is still there.
pub fn verified(run: &Run, seen: Option<&Identity>) -> bool {
    seen.is_some_and(|s| s.started == run.started && same_path(&s.exe, &run.exe)) && run.game_dir.is_dir()
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) { a.as_os_str().eq_ignore_ascii_case(b.as_os_str()) } else { a == b }
}

/// Who the process `pid` is, while it runs.
pub fn identify(pid: u32) -> Option<Identity> {
    Process::open(pid)?.identity()
}

/// A running process the manager did not start itself, held so its exit can be seen and it can be ended.
pub struct Process {
    pid: u32,
    #[cfg(windows)]
    handle: usize,
    #[cfg(unix)]
    identity: Option<Identity>,
}

impl Process {
    /// Opens `run`'s process when it is still the game that was launched.
    pub fn adopt(run: &Run) -> Option<Self> {
        let process = Self::open(run.pid)?;
        verified(run, process.identity().as_ref()).then_some(process)
    }

    /// The process id.
    pub fn id(&self) -> u32 {
        self.pid
    }
}

#[cfg(windows)]
impl Process {
    fn open(pid: u32) -> Option<Self> {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let process = Self { pid, handle: handle as usize };
        process.exit_code().is_none().then_some(process)
    }

    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle as windows_sys::Win32::Foundation::HANDLE
    }

    fn exit_code(&self) -> Option<Option<i32>> {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        if unsafe { WaitForSingleObject(self.raw(), 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        if unsafe { GetExitCodeProcess(self.raw(), &mut code) } == 0 {
            return Some(None);
        }
        Some(Some(code as i32))
    }

    fn identity(&self) -> Option<Identity> {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetProcessTimes, PROCESS_NAME_WIN32, QueryFullProcessImageNameW};
        let zero = || FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
        if unsafe { GetProcessTimes(self.raw(), &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        if unsafe { QueryFullProcessImageNameW(self.raw(), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } == 0 {
            return None;
        }
        use std::os::windows::ffi::OsStringExt;
        let exe = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize]));
        Some(Identity { started: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime), exe })
    }

    /// The exit code once the process ended, `Some(None)` when it ended without one.
    pub fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        Ok(self.exit_code())
    }

    /// Ends the process.
    pub fn kill(&mut self) -> io::Result<()> {
        use windows_sys::Win32::System::Threading::TerminateProcess;
        if unsafe { TerminateProcess(self.raw(), 1) } == 0 && self.exit_code().is_none() {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Waits until the process ended and returns its exit code.
    pub fn wait(&mut self) -> io::Result<Option<i32>> {
        use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
        unsafe { WaitForSingleObject(self.raw(), INFINITE) };
        Ok(self.exit_code().flatten())
    }
}

#[cfg(windows)]
impl Drop for Process {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.raw()) };
    }
}

#[cfg(unix)]
impl Process {
    fn open(pid: u32) -> Option<Self> {
        let identity = probe(pid)?;
        Some(Self { pid, identity: Some(identity) })
    }

    fn identity(&self) -> Option<Identity> {
        self.identity.clone()
    }

    fn alive(&self) -> bool {
        self.identity.is_some() && probe(self.pid) == self.identity
    }

    /// The exit code once the process ended; a process the manager did not start never tells it, so `Some(None)`.
    pub fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        Ok((!self.alive()).then_some(None))
    }

    /// Ends the process.
    pub fn kill(&mut self) -> io::Result<()> {
        if !self.alive() {
            return Ok(());
        }
        let pid = i32::try_from(self.pid).map_err(io::Error::other)?;
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Waits until the process ended.
    pub fn wait(&mut self) -> io::Result<Option<i32>> {
        while self.alive() {
            std::thread::sleep(EXIT_POLL);
        }
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn probe(pid: u32) -> Option<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.get(stat.rfind(')')? + 1..)?.split_whitespace().collect();
    if matches!(fields.first(), Some(&("Z" | "X"))) {
        return None;
    }
    let started = fields.get(19)?.parse().ok()?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(Identity { started, exe })
}

#[cfg(target_os = "macos")]
fn probe(pid: u32) -> Option<Identity> {
    let pid = i32::try_from(pid).ok()?;
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) } != size || info.pbi_status == libc::SZOMB {
        return None;
    }
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    use std::os::unix::ffi::OsStrExt;
    let exe = PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len as usize]));
    Some(Identity { started: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec, exe })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn probe(_pid: u32) -> Option<Identity> {
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn run(dir: &Path, pid: u32, seen: &Identity) -> Run {
        Run {
            batch: Some("b".into()),
            instance: "i".into(),
            account: Uuid::nil(),
            pid,
            started: seen.started,
            exe: seen.exe.clone(),
            game_dir: dir.to_path_buf(),
            token: "t".into(),
            server_addrs: vec!["127.0.0.1:25565".parse().unwrap()],
        }
    }

    pub(crate) fn sleeper() -> std::process::Child {
        #[cfg(windows)]
        let mut command = {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        };
        #[cfg(unix)]
        let mut command = {
            let mut c = std::process::Command::new("sleep");
            c.arg("60");
            c
        };
        command.stdout(std::process::Stdio::null()).spawn().unwrap()
    }

    #[test]
    fn a_reused_pid_another_program_or_a_gone_folder_is_never_taken_for_the_game() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Identity { started: 1000, exe: PathBuf::from("/jre/bin/java") };
        let game = run(dir.path(), 42, &seen);
        assert!(verified(&game, Some(&seen)));
        assert!(!verified(&game, None), "the process is gone");
        assert!(!verified(&game, Some(&Identity { started: 1001, ..seen.clone() })), "same pid, started later: reused");
        assert!(!verified(&game, Some(&Identity { exe: PathBuf::from("/usr/bin/other"), ..seen.clone() })), "same pid and time, another program");
        let moved = Run { game_dir: dir.path().join("gone"), ..game };
        assert!(!verified(&moved, Some(&seen)));
    }

    #[test]
    fn a_live_process_is_adopted_until_it_exits_and_its_pid_never_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = sleeper();
        let seen = identify(child.id()).expect("a running child has an identity");
        assert_eq!(identify(child.id()), Some(seen.clone()), "the identity is stable");
        let game = run(dir.path(), child.id(), &seen);
        let mut adopted = Process::adopt(&game).expect("the launched process is adopted");
        assert_eq!(adopted.id(), child.id());
        assert_eq!(adopted.try_wait().unwrap(), None);
        assert!(Process::adopt(&Run { started: seen.started + 1, ..game.clone() }).is_none());
        adopted.kill().unwrap();
        adopted.wait().unwrap();
        assert!(adopted.try_wait().unwrap().is_some());
        let _ = child.wait();
        assert!(Process::adopt(&game).is_none(), "an ended game is not adopted");
        assert!(identify(child.id()).is_none_or(|now| now != seen), "a pid that comes back is someone else");
    }

    #[test]
    fn the_token_never_shows_in_debug_output() {
        let seen = Identity { started: 7, exe: PathBuf::from("java") };
        let text = format!("{:?}", Run { token: "secret-token".into(), ..run(Path::new("g"), 1, &seen) });
        assert!(!text.contains("secret-token") && text.contains("<hidden>"), "{text}");
    }

    #[test]
    fn this_process_has_an_identity() {
        let me = identify(std::process::id()).unwrap();
        assert_eq!(me.exe.file_name(), std::env::current_exe().unwrap().file_name());
    }

    #[test]
    fn the_ledger_survives_a_restart_and_forgets_only_the_launch_that_ended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RECORD_FILE);
        let seen = Identity { started: 7, exe: PathBuf::from("java") };
        let ledger = Ledger::at(path.clone());
        assert_eq!(ledger.port(), None);
        ledger.set_port(4321);
        ledger.add("a", run(dir.path(), 1, &seen));
        ledger.add("b", run(dir.path(), 2, &seen));
        assert_eq!(std::fs::read_to_string(dir.path().join(ADDRESS_FILE)).unwrap(), "127.0.0.1:4321");
        let again = Ledger::at(path.clone());
        assert_eq!(again.port(), Some(4321));
        assert_eq!(again.games().len(), 2);
        again.add("a", run(dir.path(), 3, &seen));
        again.remove(&[("a".into(), 1), ("b".into(), 2)]);
        let games = Ledger::at(path.clone()).games();
        assert_eq!(games.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(games["a"].pid, 3, "the relaunch under the same key stays");
        again.keep_only(&[]);
        assert!(Ledger::at(path).games().is_empty());
    }

    #[test]
    fn a_broken_record_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RECORD_FILE);
        std::fs::write(&path, b"{not json").unwrap();
        let ledger = Ledger::at(path);
        assert!(ledger.games().is_empty());
        assert_eq!(ledger.port(), None);
    }
}
