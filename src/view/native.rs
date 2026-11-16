//! The manager window as the operating system sees it: where it is, and getting it out of the way of a game.

use std::sync::atomic::{AtomicIsize, Ordering};

use crate::core::drive;
use crate::core::place::{Mode, Place, Rect};

static HWND: AtomicIsize = AtomicIsize::new(0);

/// How far outside every screen a driven window goes.
const AWAY: i32 = 12_000;

/// Remembers the native handle of the manager window.
pub fn attach(window: &freya::winit::window::Window) {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(RawWindowHandle::Win32(h)) = window.window_handle().map(|h| h.as_raw()) {
            HWND.store(h.hwnd.get(), Ordering::SeqCst);
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = window;
}

/// The native handle of the manager window, for the drive tool; zero before the window exists.
pub fn handle() -> isize {
    HWND.load(Ordering::SeqCst)
}

#[cfg(target_os = "windows")]
fn hwnd() -> Option<windows_sys::Win32::Foundation::HWND> {
    let h = HWND.load(Ordering::SeqCst);
    (h != 0).then_some(h as windows_sys::Win32::Foundation::HWND)
}

/// Where the manager window is and how it is shown; nothing while it is minimized.
pub fn place() -> Option<(Rect, Mode)> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsZoomed};
        let hwnd = hwnd()?;
        if unsafe { IsIconic(hwnd) } != 0 {
            return None;
        }
        let mode = if unsafe { IsZoomed(hwnd) } != 0 { Mode::Maximized } else { Mode::Normal };
        Some((crate::core::game_window::win::frame(hwnd)?, mode))
    }
    #[cfg(not(target_os = "windows"))]
    None
}

/// Gets the manager out of the way of the game that takes its place. A driven window stays, so agents keep seeing it.
pub fn hide(_place: Option<Place>) {
    if drive::fenced("hide", "main window") {
        return;
    }
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWMINNOACTIVE, ShowWindow};
        unsafe { ShowWindow(hwnd, SW_SHOWMINNOACTIVE) };
    }
}

/// Brings the manager back in front, in the place the game had.
pub fn show(place: Option<Place>) {
    if drive::fenced("show", "main window") {
        return;
    }
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow};
        unsafe { ShowWindow(hwnd, SW_RESTORE) };
        if let Some(p) = place.filter(|p| p.mode == Mode::Normal) {
            unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), p.frame.x, p.frame.y, p.frame.w as i32, p.frame.h as i32, SWP_NOZORDER | SWP_NOACTIVATE) };
        }
        unsafe { SetForegroundWindow(hwnd) };
    }
    #[cfg(not(target_os = "windows"))]
    let _ = place;
}

/// Moves a driven window outside every screen and off the taskbar before anyone sees it, still drawing.
pub fn send_away() {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd() {
        use windows_sys::Win32::Foundation::RECT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, GetWindowRect, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_APPWINDOW,
            WS_EX_TOOLWINDOW,
        };
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return;
        }
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize));
            SetWindowPos(hwnd, std::ptr::null_mut(), rect.left - AWAY, rect.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

/// Takes the manager window off the screen and the taskbar while it waits in the notification area.
pub fn stow() {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
    #[cfg(not(target_os = "windows"))]
    {
        use freya_winit::WinitPlatformExt;
        let platform = freya::prelude::Platform::get();
        platform.with_window(freya::prelude::Platform::window_id(), |w| w.set_visible(false));
    }
}

/// Brings a stowed manager window back; a driven one comes back without taking the focus.
pub fn unstow() {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, ShowWindow};
        if drive::on() {
            unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
            return;
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        use freya_winit::WinitPlatformExt;
        let platform = freya::prelude::Platform::get();
        platform.with_window(freya::prelude::Platform::window_id(), |w| w.set_visible(true));
    }
    show(None);
}
