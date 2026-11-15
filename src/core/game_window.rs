//! The game window seen from outside the game: its border tinted in the account's colour, where it sits and moving it while hidden.

use std::time::Duration;
#[cfg(any(target_os = "windows", test))]
use std::time::Instant;

use super::account::Account;
use super::place::{Insets, Mode, Rect};

#[cfg(target_os = "windows")]
const HOME_POLL: Duration = Duration::from_millis(50);

/// How a game's main window sits on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    pub frame: Rect,
    pub insets: Insets,
    pub mode: Mode,
    pub visible: bool,
}

/// Where the main window of process `pid` is; nothing while it is minimized, or on platforms other than Windows.
pub fn seen(pid: u32) -> Option<Seen> {
    #[cfg(target_os = "windows")]
    return win::main_window(pid).and_then(win::seen);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = pid;
        None
    }
}

/// Whether the game of process `pid` has opened its window, which on platforms other than Windows nobody can tell.
pub fn shown(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    return win::game_window(pid).is_some();
    #[cfg(not(target_os = "windows"))]
    {
        let _ = pid;
        false
    }
}

/// Moves the hidden main window of process `pid` to the corner of `frame`, keeping its size, without showing or activating it and without waiting for the game.
pub fn place_hidden(pid: u32, frame: Rect) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = win::main_window(pid) {
        win::place_hidden(hwnd, frame);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (pid, frame);
}

/// Keeps the window of a game without Manycraft's mod, process `pid`, on the monitor of `home`: moves each new window of
/// the game into the middle of `home` without activating it, once. A window that was moved or already sat on `home` is
/// never touched again, so dragging it is final. Stops `settle` after the first window, or when `wait` passed without
/// a window or the process ended. Returns whether a window was moved.
pub fn bring_home(pid: u32, home: Rect, wait: Duration, settle: Duration) -> bool {
    #[cfg(target_os = "windows")]
    {
        let Some(process) = win::Process::open(pid) else { return false };
        let start = Instant::now();
        let mut homing = Homing::new(settle);
        loop {
            let now = Instant::now();
            if homing.done(now, start, wait) || process.ended() {
                return homing.moved();
            }
            if let Some(hwnd) = win::game_window(pid)
                && homing.visit(now, hwnd as usize)
            {
                homing.record(hwnd as usize, win::move_into(hwnd, home));
            }
            std::thread::sleep(HOME_POLL);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (pid, home, wait, settle);
        false
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Homed {
    Moved,
    Home,
    NotYet,
}

#[cfg(any(target_os = "windows", test))]
struct Homing {
    settle: Duration,
    handled: Vec<usize>,
    first: Option<Instant>,
    moved: bool,
}

#[cfg(any(target_os = "windows", test))]
impl Homing {
    fn new(settle: Duration) -> Self {
        Self { settle, handled: Vec::new(), first: None, moved: false }
    }

    fn visit(&mut self, now: Instant, window: usize) -> bool {
        if self.handled.contains(&window) {
            return false;
        }
        self.handled.push(window);
        self.first.get_or_insert(now);
        true
    }

    fn record(&mut self, window: usize, homed: Homed) {
        match homed {
            Homed::Moved => self.moved = true,
            Homed::Home => {}
            Homed::NotYet => self.handled.retain(|w| *w != window),
        }
    }

    fn moved(&self) -> bool {
        self.moved
    }

    fn done(&self, now: Instant, start: Instant, wait: Duration) -> bool {
        match self.first {
            Some(first) => now.duration_since(first) > self.settle,
            None => now.duration_since(start) > wait,
        }
    }
}

/// Asks the game of process `pid` to close as if its window's close button was pressed, so it saves first. Returns
/// whether it was asked.
pub fn close(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        win::game_window(pid).is_some_and(win::close)
    }
    #[cfg(unix)]
    {
        i32::try_from(pid).is_ok_and(|pid| unsafe { libc::kill(pid, libc::SIGTERM) } == 0)
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        let _ = pid;
        false
    }
}

/// The work areas of all monitors, empty where unknown.
pub fn work_areas() -> Vec<Rect> {
    #[cfg(target_os = "windows")]
    return win::work_areas();
    #[cfg(not(target_os = "windows"))]
    Vec::new()
}

/// Tints the border of the game windows of process `pid` in the colour of `account`. Only Windows 11 draws it; elsewhere nothing happens.
pub fn tint(pid: u32, account: &Account) {
    let rgb = account.rgb();
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows_sys::Win32::Graphics::Dwm::{DWMWA_BORDER_COLOR, DwmSetWindowAttribute};
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            let (pid, color) = unsafe { *(data as *const (u32, u32)) };
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
            if owner == pid {
                unsafe { DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR as u32, (&raw const color).cast(), 4) };
            }
            1
        }
        let data = (pid, colorref(rgb));
        EnumWindows(Some(each), &raw const data as LPARAM);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (pid, rgb);
}

/// Window geometry on Windows, shared with the manager window.
#[cfg(target_os = "windows")]
pub mod win {
    use super::super::place::{self, Insets, Rect};
    use super::Seen;
    use windows_sys::Win32::Foundation::{BOOL, CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetClassNameW, GetClientRect, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
        PostMessageW, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, WM_CLOSE,
    };

    const EMPTY: RECT = RECT { left: 0, top: 0, right: 0, bottom: 0 };

    fn rect(r: RECT) -> Rect {
        Rect { x: r.left, y: r.top, w: (r.right - r.left).max(0) as u32, h: (r.bottom - r.top).max(0) as u32 }
    }

    /// `frame` grown by `margins` into the rectangle Windows positions a window by.
    pub fn grown(frame: Rect, margins: Insets) -> RECT {
        RECT { left: frame.x - margins.left, top: frame.y - margins.top, right: frame.x + frame.w as i32 + margins.right, bottom: frame.y + frame.h as i32 + margins.bottom }
    }

    /// The top-level SDL window of process `pid`.
    pub fn main_window(pid: u32) -> Option<HWND> {
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            let found = unsafe { &mut *(data as *mut (u32, HWND)) };
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
            if owner != found.0 || !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null() {
                return 1;
            }
            let mut class = [0u16; 16];
            let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
            if String::from_utf16_lossy(&class[..n.max(0) as usize]) == "SDL_app" {
                found.1 = hwnd;
                return 0;
            }
            1
        }
        let mut found: (u32, HWND) = (pid, std::ptr::null_mut());
        unsafe { EnumWindows(Some(each), &raw mut found as LPARAM) };
        (!found.1.is_null()).then_some(found.1)
    }

    /// The visible frame of `hwnd`, without the invisible resize borders.
    pub fn frame(hwnd: HWND) -> Option<Rect> {
        let mut r = EMPTY;
        let ok = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS as u32, (&raw mut r).cast(), size_of::<RECT>() as u32) } == 0;
        ok.then(|| rect(r))
    }

    /// The invisible borders around the frame of `hwnd`.
    pub fn margins(hwnd: HWND) -> Option<Insets> {
        let mut r = EMPTY;
        if unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
            return None;
        }
        Some(place::insets(rect(r), frame(hwnd)?))
    }

    fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFO> {
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, rcMonitor: EMPTY, rcWork: EMPTY, dwFlags: 0 };
        (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then_some(info)
    }

    /// How `hwnd` sits on screen, nothing while it is minimized.
    pub fn seen(hwnd: HWND) -> Option<Seen> {
        if unsafe { IsIconic(hwnd) } != 0 {
            return None;
        }
        let frame = frame(hwnd)?;
        let mut client = EMPTY;
        let mut origin = POINT { x: 0, y: 0 };
        unsafe {
            GetClientRect(hwnd, &mut client);
            ClientToScreen(hwnd, &mut origin);
        }
        let client = Rect { x: origin.x, y: origin.y, w: client.right.max(0) as u32, h: client.bottom.max(0) as u32 };
        let monitor = monitor_info(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) })?;
        let mode = place::mode_of(unsafe { IsZoomed(hwnd) } != 0, client, rect(monitor.rcMonitor));
        Some(Seen { frame, insets: place::insets(frame, client), mode, visible: unsafe { IsWindowVisible(hwnd) } != 0 })
    }

    /// The top-level window of process `pid` that a game draws in, SDL for current versions and GLFW for older ones.
    pub fn game_window(pid: u32) -> Option<HWND> {
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            let found = unsafe { &mut *(data as *mut (u32, HWND)) };
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
            if owner != found.0 || !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null() {
                return 1;
            }
            let mut class = [0u16; 16];
            let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
            if matches!(String::from_utf16_lossy(&class[..n.max(0) as usize]).as_str(), "SDL_app" | "GLFW30") {
                found.1 = hwnd;
                return 0;
            }
            1
        }
        let mut found: (u32, HWND) = (pid, std::ptr::null_mut());
        unsafe { EnumWindows(Some(each), &raw mut found as LPARAM) };
        (!found.1.is_null()).then_some(found.1)
    }

    /// A process watched without keeping it alive.
    pub struct Process(HANDLE);

    impl Process {
        /// The running process `pid`, or `None` when it is gone.
        pub fn open(pid: u32) -> Option<Self> {
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            (!handle.is_null()).then_some(Self(handle))
        }

        /// Whether the process has exited.
        pub fn ended(&self) -> bool {
            unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Posts a close request to `hwnd`.
    pub fn close(hwnd: HWND) -> bool {
        unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0 }
    }

    /// Moves a normal `hwnd` into the middle of `home`, keeping its size, unless it is on that monitor already.
    pub fn move_into(hwnd: HWND, home: Rect) -> super::Homed {
        use super::Homed;
        let Some(seen) = seen(hwnd) else { return Homed::NotYet };
        if !seen.mode.is_normal() {
            return Homed::NotYet;
        }
        let works = work_areas();
        if place::same_work_area(seen.frame, home, &works) {
            return Homed::Home;
        }
        let Some(margins) = margins(hwnd) else { return Homed::NotYet };
        let target = place::centered(home, seen.frame, &works);
        let r = grown(target, margins);
        unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS) };
        Homed::Moved
    }

    /// Moves `hwnd` while hidden so that its frame covers `frame`; a shown, minimized, maximized or fullscreen window stays.
    pub fn place_hidden(hwnd: HWND, frame: Rect) {
        let Some(seen) = seen(hwnd) else { return };
        let target = place::parked(frame, seen.frame, &work_areas());
        if seen.visible || !seen.mode.is_normal() || seen.frame == target {
            return;
        }
        let Some(margins) = margins(hwnd) else { return };
        let r = grown(target, margins);
        unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS) };
    }

    /// The work areas of all monitors.
    pub fn work_areas() -> Vec<Rect> {
        unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
            let out = unsafe { &mut *(data as *mut Vec<Rect>) };
            if let Some(info) = monitor_info(monitor) {
                out.push(rect(info.rcWork));
            }
            1
        }
        let mut out: Vec<Rect> = Vec::new();
        unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(each), &raw mut out as LPARAM) };
        out
    }

    /// `r` in the workspace coordinates window placements use: screen coordinates shifted by the taskbar on its monitor.
    pub fn workspace(r: RECT) -> RECT {
        let Some(info) = monitor_info(unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST) }) else { return r };
        let (dx, dy) = (info.rcWork.left - info.rcMonitor.left, info.rcWork.top - info.rcMonitor.top);
        RECT { left: r.left - dx, top: r.top - dy, right: r.right - dx, bottom: r.bottom - dy }
    }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn colorref(rgb: u32) -> u32 {
    (rgb >> 16 & 0xff) | (rgb & 0xff00) | (rgb & 0xff) << 16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_moved_once_and_a_new_window_gets_its_turn() {
        let t0 = Instant::now();
        let mut homing = Homing::new(Duration::from_secs(20));
        assert!(homing.visit(t0, 1));
        homing.record(1, Homed::Moved);
        assert!(!homing.visit(t0 + Duration::from_secs(1), 1));
        assert!(homing.visit(t0 + Duration::from_secs(2), 2));
        assert!(!homing.visit(t0 + Duration::from_secs(3), 2));
        assert!(homing.moved());
    }

    #[test]
    fn a_window_already_home_is_left_alone_and_ends_the_watch_after_the_settle_time() {
        let t0 = Instant::now();
        let wait = Duration::from_secs(600);
        let mut homing = Homing::new(Duration::from_secs(20));
        assert!(!homing.done(t0 + Duration::from_secs(30), t0, wait));
        assert!(homing.visit(t0 + Duration::from_secs(30), 7));
        homing.record(7, Homed::Home);
        assert!(!homing.visit(t0 + Duration::from_secs(31), 7));
        assert!(!homing.done(t0 + Duration::from_secs(50), t0, wait));
        assert!(homing.done(t0 + Duration::from_secs(51), t0, wait));
        assert!(!homing.moved());
    }

    #[test]
    fn a_window_that_was_not_ready_is_tried_again() {
        let t0 = Instant::now();
        let mut homing = Homing::new(Duration::from_secs(20));
        assert!(homing.visit(t0, 3));
        homing.record(3, Homed::NotYet);
        assert!(homing.visit(t0 + Duration::from_secs(1), 3));
        homing.record(3, Homed::Moved);
        assert!(!homing.visit(t0 + Duration::from_secs(2), 3));
        assert!(homing.moved());
    }

    #[test]
    fn without_a_window_the_watch_ends_after_the_wait() {
        let t0 = Instant::now();
        let homing = Homing::new(Duration::from_secs(20));
        assert!(!homing.done(t0 + Duration::from_secs(600), t0, Duration::from_secs(600)));
        assert!(homing.done(t0 + Duration::from_secs(601), t0, Duration::from_secs(600)));
    }

    #[test]
    fn colorref_swaps_red_and_blue() {
        assert_eq!(colorref(0x12_34_56), 0x56_34_12);
        assert_eq!(colorref(0xFF_00_00), 0x00_00_FF);
    }
}
