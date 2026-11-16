//! The manager window as both shells tell the operating system about it: where its middle is, its content area in the
//! units a game window uses, the dock badge, and letting a game take the foreground.

use std::sync::Mutex;

use super::place::Rect;

static CENTER: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// Remembers the middle of the manager's `frame`, for windows that should open on its monitor.
pub fn note_center(frame: Rect) {
    *CENTER.lock().unwrap_or_else(|e| e.into_inner()) = Some((frame.x + (frame.w / 2) as i32, frame.y + (frame.h / 2) as i32));
}

/// The middle of the manager window as it was last seen, in screen coordinates.
pub fn center() -> Option<(i32, i32)> {
    *CENTER.lock().unwrap_or_else(|e| e.into_inner())
}

/// A content area of `logical` size and origin at device `scale` in the units a game window is placed in: points on
/// macOS, pixels elsewhere.
pub fn game_units(logical: (f64, f64, f64, f64), scale: f64) -> Rect {
    in_units(logical, scale, cfg!(target_os = "macos"))
}

fn in_units((x, y, w, h): (f64, f64, f64, f64), scale: f64, points: bool) -> Rect {
    let scale = if points { 1. } else { scale };
    let at = |v: f64| (v * scale).round();
    Rect { x: at(x) as i32, y: at(y) as i32, w: at(w).max(1.) as u32, h: at(h).max(1.) as u32 }
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

/// The title of the manager window: the count of accounts that need attention where no dock badge shows it.
pub fn title(attention: usize) -> String {
    if attention == 0 || cfg!(target_os = "macos") { "Manycraft".to_owned() } else { format!("Manycraft ({attention})") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_keep_the_logical_area_and_pixels_scale_it() {
        assert_eq!(in_units((10.4, 20., 800., 600.), 2., true), Rect { x: 10, y: 20, w: 800, h: 600 });
        assert_eq!(in_units((10.4, 20., 800., 600.), 2., false), Rect { x: 21, y: 40, w: 1600, h: 1200 });
        assert_eq!(in_units((-5., 0., 0., 0.2), 1.5, false), Rect { x: -8, y: 0, w: 1, h: 1 });
    }

    #[test]
    fn the_center_is_the_middle_of_the_frame() {
        note_center(Rect { x: -100, y: 50, w: 801, h: 400 });
        assert_eq!(center(), Some((300, 250)));
    }
}
