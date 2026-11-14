//! The game window seen from outside the game: its border tinted in the account's colour, where it sits and moving it while hidden.

use super::account::Account;
use super::place::{Insets, Mode, Rect};

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

/// Moves the hidden main window of process `pid` so that its frame covers `frame`, without showing or activating it and without waiting for the game.
pub fn place_hidden(pid: u32, frame: Rect) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = win::main_window(pid) {
        win::place_hidden(hwnd, frame);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (pid, frame);
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
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_STYLE, GetClassNameW, GetClientRect, GetWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
        SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos, WS_CAPTION,
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
        let titled = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32 & WS_CAPTION == WS_CAPTION;
        let monitor = monitor_info(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) })?;
        let mode = place::mode_of(titled, unsafe { IsZoomed(hwnd) } != 0, frame, rect(monitor.rcMonitor));
        Some(Seen { frame, insets: place::insets(frame, client), mode, visible: unsafe { IsWindowVisible(hwnd) } != 0 })
    }

    /// Moves `hwnd` while hidden so that its frame covers `frame`; a shown, minimized, maximized or fullscreen window stays.
    pub fn place_hidden(hwnd: HWND, frame: Rect) {
        let Some(seen) = seen(hwnd) else { return };
        if seen.visible || !seen.mode.is_normal() || seen.frame == frame {
            return;
        }
        let Some(margins) = margins(hwnd) else { return };
        let r = grown(frame, margins);
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
    fn colorref_swaps_red_and_blue() {
        assert_eq!(colorref(0x12_34_56), 0x56_34_12);
        assert_eq!(colorref(0xFF_00_00), 0x00_00_FF);
    }
}
