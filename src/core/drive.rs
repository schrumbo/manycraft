use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static DIR: LazyLock<Option<PathBuf>> = LazyLock::new(|| std::env::var_os("MANYCRAFT_DRIVE").map(PathBuf::from));
static EVENTS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static ANSWERS: Mutex<VecDeque<Option<Vec<PathBuf>>>> = Mutex::new(VecDeque::new());
const KEPT_EVENTS: usize = 500;

/// Whether an agent drives this instance; `MANYCRAFT_DRIVE` names its scratch folder. A driven instance never starts Minecraft.
pub fn on() -> bool {
    DIR.is_some()
}

/// The driven instance's folder.
pub fn dir() -> Option<&'static Path> {
    DIR.as_deref()
}

/// Refuses to drive an instance that would use the real accounts and instances.
pub fn check() {
    if on() && super::paths::home_override().is_none() {
        eprintln!("MANYCRAFT_DRIVE needs MANYCRAFT_HOME, so a driven instance never touches the real accounts");
        std::process::exit(2);
    }
}

/// Records something the driving agent should know about.
pub fn note(event: String) {
    if on() {
        let mut events = EVENTS.lock().unwrap_or_else(|e| e.into_inner());
        if events.len() == KEPT_EVENTS {
            events.pop_front();
        }
        events.push_back(event);
    }
}

/// True when `what` must not reach the system, a browser or another app; it is only recorded.
pub fn fenced(what: &str, target: impl std::fmt::Display) -> bool {
    if on() {
        note(format!("{what} {target}"));
    }
    on()
}

/// Whether driven windows open far outside every screen, so the owner never sees or touches them.
pub fn away() -> bool {
    on() && std::env::var_os("MANYCRAFT_DRIVE_AWAY").is_some()
}

/// Everything recorded since the last call.
pub fn take_events() -> Vec<String> {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
}

/// The answer for the next file dialog: the paths, or `None` to cancel it.
pub fn queue_answer(paths: Option<Vec<PathBuf>>) {
    ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).push_back(paths);
}

/// Answers a file dialog without showing it; with nothing queued the dialog is cancelled.
pub fn answer(kind: &str) -> Option<Vec<PathBuf>> {
    let answer = ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).pop_front().flatten();
    note(format!("dialog {kind} -> {}", answer.as_ref().map_or("cancel".to_string(), |p| p.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))));
    answer
}

/// Whether the process `pid` still runs.
#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Whether the process `pid` still runs.
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}
