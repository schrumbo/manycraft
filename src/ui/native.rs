//! The manager window's place on screen: hiding it while a game takes its place, bringing it back, and the attention badge.

use gpui_kit::*;

use crate::core::place::{Mode, Place, Rect};

/// Where the content of `window` is, in the game window's coordinates: points on macOS, pixels elsewhere.
#[cfg(not(target_os = "windows"))]
fn rect_of(window: &Window) -> Rect {
    let b = window.bounds();
    let scale = if cfg!(target_os = "macos") { 1. } else { window.scale_factor() };
    let at = |p: Pixels| (f32::from(p) * scale).round();
    Rect { x: at(b.origin.x) as i32, y: at(b.origin.y) as i32, w: at(b.size.width).max(1.) as u32, h: at(b.size.height).max(1.) as u32 }
}

#[cfg(target_os = "windows")]
static MARGINS: std::sync::Mutex<Option<crate::core::place::Insets>> = std::sync::Mutex::new(None);

/// The outer frame of `window` and its mode; nothing while it is minimized. Only Windows knows the frame; elsewhere it is the content area.
pub fn place_of(window: &Window) -> Option<(Rect, Mode)> {
    let fullscreen = window.is_fullscreen();
    #[cfg(target_os = "windows")]
    {
        use crate::core::game_window::win;
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsZoomed};
        let hwnd = hwnd(window)?;
        if unsafe { IsIconic(hwnd) } != 0 {
            return None;
        }
        let zoomed = unsafe { IsZoomed(hwnd) } != 0;
        if !zoomed && !fullscreen && let Some(margins) = win::margins(hwnd) {
            *MARGINS.lock().unwrap_or_else(|e| e.into_inner()) = Some(margins);
        }
        let mode = if fullscreen { Mode::Fullscreen } else if zoomed { Mode::Maximized } else { Mode::Normal };
        Some((win::frame(hwnd)?, mode))
    }
    #[cfg(not(target_os = "windows"))]
    Some((rect_of(window), if fullscreen { Mode::Fullscreen } else { Mode::Normal }))
}

/// Moves `window` to `place`'s frame, maximized when it says so; a minimized window comes back there in one step.
pub fn put(window: &Window, place: Place) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd(window) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SW_SHOWMAXIMIZED};
        let show = if place.mode == Mode::Maximized { SW_SHOWMAXIMIZED } else { SW_RESTORE };
        set_placement(hwnd, place, show);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (window, place);
}

#[cfg(target_os = "windows")]
fn set_placement(hwnd: windows_sys::Win32::Foundation::HWND, place: Place, show: windows_sys::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD) {
    use crate::core::game_window::win;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, SW_RESTORE, SetWindowPlacement, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED};
    let margins = MARGINS.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or_default();
    let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
    wp.length = size_of::<WINDOWPLACEMENT>() as u32;
    if unsafe { GetWindowPlacement(hwnd, &mut wp) } == 0 {
        return;
    }
    let maximized = place.mode == Mode::Maximized;
    wp.rcNormalPosition = win::workspace(win::grown(place.frame, margins));
    wp.flags = if maximized { WPF_RESTORETOMAXIMIZED } else { 0 };
    wp.showCmd = show as u32;
    unsafe { SetWindowPlacement(hwnd, &wp) };
    if show == SW_RESTORE
        && win::frame(hwnd).is_some_and(|f| f != place.frame)
        && let Some(fresh) = win::margins(hwnd)
    {
        *MARGINS.lock().unwrap_or_else(|e| e.into_inner()) = Some(fresh);
        wp.rcNormalPosition = win::workspace(win::grown(place.frame, fresh));
        unsafe { SetWindowPlacement(hwnd, &wp) };
    }
}

/// Lets the game process `pid` bring its window to the front, which Windows refuses to background processes otherwise.
pub fn allow_foreground(pid: u32) {
    #[cfg(target_os = "windows")]
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = pid;
}

#[cfg(target_os = "windows")]
fn hwnd(window: &Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get() as windows_sys::Win32::Foundation::HWND),
        _ => None,
    }
}

/// Makes GPUI draw at the size `hwnd` came back from minimized at; it keeps its old drawing size otherwise.
#[cfg(target_os = "windows")]
fn redraw_at_size(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, IsZoomed, SIZE_MAXIMIZED, SIZE_RESTORED, SendMessageW, WM_SIZE};
    let mut client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return;
    }
    let kind = if unsafe { IsZoomed(hwnd) } != 0 { SIZE_MAXIMIZED } else { SIZE_RESTORED };
    unsafe { SendMessageW(hwnd, WM_SIZE, kind as usize, size_param(client.right, client.bottom)) };
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn size_param(width: i32, height: i32) -> isize {
    ((height.clamp(0, 0xffff) as isize) << 16) | width.clamp(0, 0xffff) as isize
}

#[cfg(target_os = "macos")]
fn ns_window(window: &Window) -> Option<*mut objc2::runtime::AnyObject> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(RawWindowHandle::AppKit(h)) = HasWindowHandle::window_handle(window).map(|h| h.as_raw()) else { return None };
    let view = h.ns_view.as_ptr().cast::<AnyObject>();
    let ns_window: *mut AnyObject = unsafe { msg_send![view, window] };
    (!ns_window.is_null()).then_some(ns_window)
}

/// Gets the manager out of the game's way; its taskbar or dock icon stays, so clicking it brings the grid back, on Windows straight at `place`.
pub fn hide(window: &mut Window, place: Option<Place>) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd(window) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWMINNOACTIVE, ShowWindow};
        match place.filter(|p| p.mode != Mode::Fullscreen && !window.is_fullscreen()) {
            Some(place) => set_placement(hwnd, place, SW_SHOWMINNOACTIVE),
            None => unsafe {
                ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            },
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(ns_window) = ns_window(window) {
        use objc2::msg_send;
        let nil: *const objc2::runtime::AnyObject = std::ptr::null();
        let _: () = unsafe { msg_send![ns_window, orderOut: nil] };
    }
    #[cfg(not(target_os = "windows"))]
    let _ = place;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    window.minimize_window();
}

/// Brings the manager back to the front, at `place` when given, else where it was.
pub fn show(window: &mut Window, place: Option<Place>, cx: &mut App) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd(window) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
        match place {
            Some(place) => put(window, place),
            None if unsafe { IsIconic(hwnd) } != 0 => unsafe {
                ShowWindow(hwnd, SW_RESTORE);
            },
            None => {}
        }
        redraw_at_size(hwnd);
        unsafe { SetForegroundWindow(hwnd) };
    }
    #[cfg(not(target_os = "windows"))]
    let _ = place;
    #[cfg(target_os = "macos")]
    if let Some(ns_window) = ns_window(window) {
        use objc2::msg_send;
        let nil: *const objc2::runtime::AnyObject = std::ptr::null();
        let _: () = unsafe { msg_send![ns_window, makeKeyAndOrderFront: nil] };
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    window.activate_window();
    cx.activate(true);
}

/// Shows how many accounts need attention on the dock icon; other platforms show it in the window title.
pub fn set_badge(count: usize) {
    #[cfg(target_os = "macos")]
    unsafe {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let tile: *mut AnyObject = msg_send![app, dockTile];
        if count == 0 {
            let nil: *const AnyObject = std::ptr::null();
            let _: () = msg_send![tile, setBadgeLabel: nil];
        } else {
            let label = objc2_foundation::NSString::from_str(&count.to_string());
            let _: () = msg_send![tile, setBadgeLabel: &*label];
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = count;
}

#[cfg(test)]
mod tests {
    use super::size_param;

    #[test]
    fn size_param_packs_width_low_and_height_high() {
        assert_eq!(size_param(2560, 1369), 1369 << 16 | 2560);
        assert_eq!(size_param(0, 0), 0);
        assert_eq!(size_param(-5, 70_000), 0xffff << 16);
    }
}
