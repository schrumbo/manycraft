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

/// Moves `window` to `place`'s frame, maximized when it says so. It must not be minimized: GPUI keeps its old drawing size when a window comes back from minimized at a new size.
pub fn put(window: &Window, place: Place) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd(window) {
        use crate::core::game_window::win;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowPlacement, SW_RESTORE, SW_SHOWMAXIMIZED, SetWindowPlacement, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
        };
        let margins = MARGINS.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or_default();
        let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
        wp.length = size_of::<WINDOWPLACEMENT>() as u32;
        if unsafe { GetWindowPlacement(hwnd, &mut wp) } == 0 {
            return;
        }
        let maximized = place.mode == Mode::Maximized;
        wp.rcNormalPosition = win::workspace(win::grown(place.frame, margins));
        wp.flags = if maximized { WPF_RESTORETOMAXIMIZED } else { 0 };
        wp.showCmd = (if maximized { SW_SHOWMAXIMIZED } else { SW_RESTORE }) as u32;
        unsafe { SetWindowPlacement(hwnd, &wp) };
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (window, place);
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

/// Gets the manager out of the game's way; its taskbar or dock icon stays, so clicking it brings the grid back.
pub fn hide(window: &mut Window) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd(window) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWMINNOACTIVE, ShowWindow};
        unsafe { ShowWindow(hwnd, SW_SHOWMINNOACTIVE) };
    }
    #[cfg(target_os = "macos")]
    if let Some(ns_window) = ns_window(window) {
        use objc2::msg_send;
        let nil: *const objc2::runtime::AnyObject = std::ptr::null();
        let _: () = unsafe { msg_send![ns_window, orderOut: nil] };
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    window.minimize_window();
}

/// Brings the manager back to the front, at `place` when given, else where it was.
pub fn show(window: &mut Window, place: Option<Place>, cx: &mut App) {
    #[cfg(target_os = "windows")]
    if let Some(hwnd) = hwnd(window) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
        if unsafe { IsIconic(hwnd) } != 0 {
            unsafe { ShowWindow(hwnd, SW_RESTORE) };
        }
        if let Some(place) = place {
            put(window, place);
        }
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
