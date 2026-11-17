//! The window's own frame: which parts of the title bar move the window, the window buttons, and the system frame
//! folded into the title bar on Windows and macOS.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::OnceLock;

use freya::prelude::*;
use freya_winit::WinitPlatformExt;

use crate::core::drive;
use crate::view::kit::Bounds;

/// What a part of the title bar is to the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Zone {
    /// The bar itself: it moves the window.
    Bar,
    /// A control on the bar that takes its own clicks.
    Control,
    /// A window button.
    Button(Caption),
}

/// The window buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Caption {
    Minimize,
    Maximize,
    Close,
}

/// What lies over the title bar right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cover {
    #[default]
    None,
    /// A sheet: the bar still moves the window, its controls wait.
    Sheet,
    /// A menu: a click anywhere but on a window button closes it.
    Menu,
}

/// How the window stands, as its buttons draw it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Chrome {
    pub maximized: bool,
    pub inactive: bool,
    /// The window button under the pointer, while the system tracks it.
    pub hover: Option<Caption>,
    /// The window button held down.
    pub pressed: Option<Caption>,
    /// Whether the window wears the desktop's own frame, on Linux.
    pub framed: bool,
}

/// What the system told the title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    Chrome(Chrome),
    /// The bar was pressed where it moves the window.
    Bar,
}

thread_local! {
    static ZONES: RefCell<HashMap<u64, (Zone, Bounds)>> = RefCell::new(HashMap::new());
    static NEXT: Cell<u64> = const { Cell::new(0) };
    static SCALE: Cell<f32> = const { Cell::new(1.) };
    static COVER: Cell<Cover> = const { Cell::new(Cover::None) };
    static POPOVERS: Cell<u32> = const { Cell::new(0) };
    static CHROME: Cell<Chrome> = const { Cell::new(Chrome { maximized: false, inactive: false, hover: None, pressed: None, framed: false }) };
}

fn channel() -> &'static (async_channel::Sender<Heard>, async_channel::Receiver<Heard>) {
    static CHANNEL: OnceLock<(async_channel::Sender<Heard>, async_channel::Receiver<Heard>)> = OnceLock::new();
    CHANNEL.get_or_init(async_channel::unbounded)
}

/// What the system says about the title bar, for the one component that draws it.
pub fn heard() -> async_channel::Receiver<Heard> {
    channel().1.clone()
}

/// The window as its buttons last drew it.
pub fn chrome() -> Chrome {
    CHROME.with(Cell::get)
}

fn tell(heard: Heard) {
    let _ = channel().0.try_send(heard);
}

/// Changes how the window stands and tells the title bar when that changed anything.
pub fn update(f: impl FnOnce(&mut Chrome)) {
    let before = chrome();
    let mut next = before;
    f(&mut next);
    if next != before {
        CHROME.with(|c| c.set(next));
        tell(Heard::Chrome(next));
    }
}

/// Notes the scale the title bar's zones are measured in.
pub fn set_scale(scale: f32) {
    SCALE.with(|s| s.set(scale.max(0.1)));
}

/// Notes what lies over the title bar.
pub fn set_cover(cover: Cover) {
    COVER.with(|c| c.set(cover));
}

/// Hands the title bar's clicks to the calling full-window popover while it lives, so a click there closes it instead of
/// moving the window; the window buttons stay live.
pub fn use_popover() {
    use_hook(|| POPOVERS.with(|p| p.set(p.get() + 1)));
    use_drop(|| POPOVERS.with(|p| p.set(p.get().saturating_sub(1))));
}

/// Marks the element this measures as `zone` while the calling component lives.
pub fn use_zone(zone: Zone) -> impl FnMut(&Event<SizedEventData>) + 'static {
    let key = use_hook(|| {
        NEXT.with(|n| {
            let key = n.get();
            n.set(key + 1);
            key
        })
    });
    use_drop(move || {
        ZONES.with(|z| z.borrow_mut().remove(&key));
    });
    move |e: &Event<SizedEventData>| {
        let b = Bounds::of(e);
        ZONES.with(|z| z.borrow_mut().insert(key, (zone, b)));
    }
}

/// The zone at `(x, y)` in logical pixels among `zones`, with `cover` over the bar; none outside the bar.
pub fn zone_in(zones: impl IntoIterator<Item = (Zone, Bounds)>, cover: Cover, x: f32, y: f32) -> Option<Zone> {
    let mut bar = false;
    let mut control = None;
    for (zone, b) in zones {
        if !b.contains(x, y) {
            continue;
        }
        match zone {
            Zone::Bar => bar = true,
            Zone::Button(_) => return Some(zone),
            Zone::Control => control = Some(zone),
        }
    }
    match cover {
        Cover::Menu => None,
        Cover::Sheet => bar.then_some(Zone::Bar),
        Cover::None => control.or(bar.then_some(Zone::Bar)),
    }
}

/// The zone under `(x, y)` in device pixels from the top left of the window's content.
pub fn zone_at(x: f32, y: f32) -> Option<Zone> {
    let scale = SCALE.with(Cell::get);
    let cover = if POPOVERS.with(Cell::get) > 0 { Cover::Menu } else { COVER.with(Cell::get) };
    ZONES.with(|z| zone_in(z.borrow().values().copied(), cover, x / scale, y / scale))
}

fn with_window(f: impl FnOnce(&mut freya::winit::window::Window) + 'static) {
    let platform = Platform::get();
    platform.with_window(Platform::window_id(), f);
}

/// Minimizes the window; a driven window only notes it.
pub fn minimize() {
    if drive::fenced("minimize", "main window") {
        return;
    }
    with_window(|w| w.set_minimized(true));
}

/// Maximizes the window, or gives it its size back; a driven window only notes it, since a maximized window would land
/// on the owner's screen.
pub fn toggle_maximize() {
    if drive::fenced("maximize", "main window") {
        return;
    }
    with_window(|w| w.set_maximized(!w.is_maximized()));
}

/// Whether the bar moves the window through Freya: everywhere but Windows, whose hit test does it, and never in a driven
/// instance, whose double click would maximize it onto the owner's screen.
pub fn draws_drag() -> bool {
    !cfg!(target_os = "windows") && !drive::on()
}

/// Opens the system's window menu at `(x, y)` in logical pixels.
pub fn window_menu(x: f64, y: f64) {
    if drive::fenced("window menu", "main window") {
        return;
    }
    with_window(move |w| w.show_window_menu(freya::winit::dpi::LogicalPosition::new(x, y)));
}

/// Whether the app draws the window buttons for a window standing as `chrome`: on Windows, and on a Linux desktop
/// without the system's frame.
pub fn own_buttons(chrome: Chrome) -> bool {
    cfg!(target_os = "windows") || (cfg!(target_os = "linux") && !chrome.framed)
}

/// Whether the window wears the system's frame on Linux.
pub fn system_frame() -> bool {
    chrome().framed
}

/// Gives the window the system's frame, or takes it away, on Linux; `apply` false only notes the choice, before the
/// window exists.
pub fn set_system_frame(on: bool, apply: bool) {
    update(|c| c.framed = on);
    if apply && cfg!(target_os = "linux") {
        with_window(move |w| w.set_decorations(on));
    }
}

/// The window buttons in the order the desktop shows them, and whether they sit at the left: from GNOME's
/// `button-layout` such as `appmenu:minimize,maximize,close`.
pub fn buttons_from(layout: &str) -> (Vec<Caption>, bool) {
    let (left, right) = layout.split_once(':').unwrap_or(("", layout));
    let parse = |side: &str| {
        side.split(',')
            .filter_map(|b| match b.trim() {
                "minimize" => Some(Caption::Minimize),
                "maximize" => Some(Caption::Maximize),
                "close" => Some(Caption::Close),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let (l, r) = (parse(left), parse(right));
    if l.contains(&Caption::Close) && !r.contains(&Caption::Close) { (l, true) } else { (r, false) }
}

/// The window buttons this desktop wants, and whether they sit at the left.
pub fn buttons() -> (Vec<Caption>, bool) {
    static LAYOUT: OnceLock<(Vec<Caption>, bool)> = OnceLock::new();
    LAYOUT
        .get_or_init(|| {
            let fallback = || (vec![Caption::Minimize, Caption::Maximize, Caption::Close], false);
            if !cfg!(target_os = "linux") {
                return fallback();
            }
            let out = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.wm.preferences", "button-layout"]).output();
            match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let found = buttons_from(text.trim().trim_matches('\''));
                    if found.0.contains(&Caption::Close) { found } else { fallback() }
                }
                _ => fallback(),
            }
        })
        .clone()
}

/// The glyph font of the Windows window buttons: Segoe Fluent Icons, or Segoe MDL2 Assets before Windows 11.
pub fn glyph_font() -> &'static str {
    static FONT: OnceLock<&'static str> = OnceLock::new();
    FONT.get_or_init(|| {
        let fluent = std::env::var_os("WINDIR").map(std::path::PathBuf::from).is_some_and(|w| w.join("Fonts").join("SegoeIcons.ttf").exists());
        if fluent { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" }
    })
}

/// The glyph of `caption` in the Windows glyph font.
pub fn glyph(caption: Caption, maximized: bool) -> &'static str {
    match caption {
        Caption::Minimize => "\u{E921}",
        Caption::Maximize if maximized => "\u{E923}",
        Caption::Maximize => "\u{E922}",
        Caption::Close => "\u{E8BB}",
    }
}

#[cfg(target_os = "windows")]
pub use self::win::{fold, set_inner};

#[cfg(target_os = "windows")]
mod win {
    use std::sync::atomic::{AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::Controls::MARGINS;
    use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, GWL_EXSTYLE, GWL_STYLE, GWLP_WNDPROC, GetClientRect, GetWindowLongPtrW, GetWindowRect, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON,
        HTMINBUTTON, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsZoomed, MINMAXINFO, NCCALCSIZE_PARAMS, PostMessageW, SM_CXPADDEDBORDER, SM_CYFRAME, SM_CYSIZEFRAME, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_DPICHANGED,
        WM_GETMINMAXINFO, WM_MOUSEMOVE, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONDOWN, WM_SIZE,
        SIZE_MAXIMIZED, SIZE_MINIMIZED,
    };

    use super::{Caption, Heard, Zone, chrome, tell, update, zone_at};
    use crate::core::drive;

    static NEXT: AtomicIsize = AtomicIsize::new(0);

    const RECT0: RECT = RECT { left: 0, top: 0, right: 0, bottom: 0 };

    /// What winit adds above the content for the window's style at `dpi`, which the folded frame leaves out.
    fn top_inset(hwnd: HWND, dpi: u32) -> i32 {
        let mut r = RECT0;
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            AdjustWindowRectExForDpi(&mut r, style, 0, ex, dpi);
        }
        -r.top
    }

    fn frame_y(dpi: u32) -> i32 {
        unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
    }

    fn band(dpi: u32) -> i32 {
        unsafe { GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
    }

    fn shrink(hwnd: HWND, by: i32) {
        let mut r = RECT0;
        if by <= 0 || unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
            return;
        }
        unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, r.right - r.left, (r.bottom - r.top - by).max(1), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE) };
    }

    /// Folds the system frame of `hwnd` into the app's title bar: the caption goes, the borders that resize, snap and
    /// cast the shadow stay.
    pub fn fold(hwnd: HWND) {
        if NEXT.load(Ordering::SeqCst) != 0 {
            return;
        }
        let old = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as usize as isize) };
        NEXT.store(old, Ordering::SeqCst);
        let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
        unsafe {
            DwmExtendFrameIntoClientArea(hwnd, &margins);
            SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        shrink(hwnd, top_inset(hwnd, unsafe { GetDpiForWindow(hwnd) }));
        update(|c| c.maximized = unsafe { IsZoomed(hwnd) } != 0);
    }

    /// Gives the content of `hwnd` `w` by `h` device pixels.
    pub fn set_inner(hwnd: HWND, w: u32, h: u32) {
        let (mut outer, mut inner) = (RECT0, RECT0);
        if unsafe { GetWindowRect(hwnd, &mut outer) == 0 || GetClientRect(hwnd, &mut inner) == 0 } {
            return;
        }
        let extra = (outer.right - outer.left - inner.right, outer.bottom - outer.top - inner.bottom);
        unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, w as i32 + extra.0, h as i32 + extra.1, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE) };
    }

    fn caption_of(hit: usize) -> Option<Caption> {
        match hit as u32 {
            HTMINBUTTON => Some(Caption::Minimize),
            HTMAXBUTTON => Some(Caption::Maximize),
            HTCLOSE => Some(Caption::Close),
            _ => None,
        }
    }

    fn press(hwnd: HWND, caption: Caption) {
        if drive::fenced("caption", format!("{caption:?}")) {
            return;
        }
        match caption {
            Caption::Minimize => unsafe { ShowWindow(hwnd, SW_MINIMIZE) },
            Caption::Maximize => unsafe { ShowWindow(hwnd, if IsZoomed(hwnd) != 0 { SW_RESTORE } else { SW_MAXIMIZE }) },
            Caption::Close => unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) },
        };
    }

    fn track(hwnd: HWND) {
        let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: hwnd, dwHoverTime: 0 };
        unsafe { TrackMouseEvent(&mut t) };
    }

    fn hit_test(hwnd: HWND, lparam: LPARAM) -> Option<u32> {
        let mut pt = POINT { x: i32::from((lparam & 0xffff) as u16 as i16), y: i32::from(((lparam >> 16) & 0xffff) as u16 as i16) };
        unsafe { ScreenToClient(hwnd, &mut pt) };
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if unsafe { IsZoomed(hwnd) } == 0 && pt.y < band(dpi) {
            let mut r = RECT0;
            unsafe { GetClientRect(hwnd, &mut r) };
            let b = band(dpi);
            return Some(if pt.x < b { HTTOPLEFT } else if pt.x >= r.right - b { HTTOPRIGHT } else { HTTOP });
        }
        Some(match zone_at(pt.x as f32, pt.y as f32)? {
            Zone::Bar => HTCAPTION,
            Zone::Control => HTCLIENT,
            Zone::Button(Caption::Minimize) => HTMINBUTTON,
            Zone::Button(Caption::Maximize) => HTMAXBUTTON,
            Zone::Button(Caption::Close) => HTCLOSE,
        })
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let next = NEXT.load(Ordering::SeqCst);
        let call = |msg: u32, wparam: WPARAM, lparam: LPARAM| -> LRESULT {
            if next == 0 {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let f: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT = unsafe { std::mem::transmute(next) };
            unsafe { CallWindowProcW(Some(f), hwnd, msg, wparam, lparam) }
        };
        match msg {
            WM_NCCALCSIZE if wparam != 0 => {
                let params = lparam as *mut NCCALCSIZE_PARAMS;
                let top = unsafe { (*params).rgrc[0].top };
                let out = call(msg, wparam, lparam);
                let inset = if unsafe { IsZoomed(hwnd) } != 0 { frame_y(unsafe { GetDpiForWindow(hwnd) }) } else { 0 };
                unsafe { (*params).rgrc[0].top = top + inset };
                out
            }
            WM_NCHITTEST => {
                let out = call(msg, wparam, lparam);
                if out != HTCLIENT as LRESULT {
                    return out;
                }
                hit_test(hwnd, lparam).map_or(out, |h| h as LRESULT)
            }
            WM_NCMOUSEMOVE => {
                let hover = caption_of(wparam);
                if hover.is_some() {
                    track(hwnd);
                }
                update(|c| c.hover = hover);
                call(msg, wparam, lparam)
            }
            WM_NCMOUSELEAVE => {
                update(|c| {
                    c.hover = None;
                    c.pressed = None;
                });
                call(msg, wparam, lparam)
            }
            WM_MOUSEMOVE => {
                update(|c| c.hover = None);
                call(msg, wparam, lparam)
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if caption_of(wparam).is_some() => {
                tell(Heard::Bar);
                update(|c| c.pressed = caption_of(wparam));
                0
            }
            WM_NCLBUTTONUP if caption_of(wparam).is_some() => {
                let held = chrome().pressed;
                update(|c| c.pressed = None);
                if let Some(caption) = caption_of(wparam).filter(|c| held == Some(*c)) {
                    press(hwnd, caption);
                }
                0
            }
            WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN if wparam == HTCAPTION as usize => {
                tell(Heard::Bar);
                call(msg, wparam, lparam)
            }
            WM_NCLBUTTONUP => {
                update(|c| c.pressed = None);
                call(msg, wparam, lparam)
            }
            WM_SIZE if wparam != SIZE_MINIMIZED as usize => {
                update(|c| c.maximized = wparam == SIZE_MAXIMIZED as usize);
                call(msg, wparam, lparam)
            }
            WM_ACTIVATE => {
                update(|c| c.inactive = (wparam & 0xffff) as u32 == WA_INACTIVE);
                call(msg, wparam, lparam)
            }
            WM_GETMINMAXINFO => {
                let out = call(msg, wparam, lparam);
                let inset = top_inset(hwnd, unsafe { GetDpiForWindow(hwnd) });
                let mmi = lparam as *mut MINMAXINFO;
                unsafe { (*mmi).ptMinTrackSize.y = ((*mmi).ptMinTrackSize.y - inset).max(inset) };
                out
            }
            WM_DPICHANGED => {
                let out = call(msg, wparam, lparam);
                if unsafe { IsZoomed(hwnd) } == 0 {
                    shrink(hwnd, top_inset(hwnd, (wparam & 0xffff) as u32));
                }
                out
            }
            _ => call(msg, wparam, lparam),
        }
    }
}

/// The window's attributes: on macOS the title bar turns transparent and the content runs under it.
#[cfg(target_os = "macos")]
pub fn attributes(a: freya::winit::window::WindowAttributes) -> freya::winit::window::WindowAttributes {
    use freya::winit::platform::macos::WindowAttributesExtMacOS;
    a.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true)
}

/// The window's attributes: on macOS the title bar turns transparent and the content runs under it.
#[cfg(not(target_os = "macos"))]
pub fn attributes(a: freya::winit::window::WindowAttributes) -> freya::winit::window::WindowAttributes {
    a
}

/// The height of the bar on macOS, where the traffic lights sit centred.
#[cfg(target_os = "macos")]
const MAC_BAR: f64 = 38.;

/// How far the traffic lights sit from the left edge on macOS.
#[cfg(target_os = "macos")]
const MAC_INSET: f64 = 12.;

/// Moves the traffic lights of `window` into the 38 pt title bar, centred, inset from the left; AppKit lays them out
/// again on every resize and on leaving full screen, so this runs after each.
#[cfg(target_os = "macos")]
pub fn place_lights(window: &freya::winit::window::Window) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(RawWindowHandle::AppKit(h)) = window.window_handle().map(|h| h.as_raw()) else { return };
    let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
    let Some(ns) = view.window() else { return };
    let buttons = [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton].map(|b| ns.standardWindowButton(b));
    let [Some(close), Some(mini), Some(zoom)] = buttons else { return };
    let Some(bar) = (unsafe { close.superview() }).and_then(|v| unsafe { v.superview() }) else { return };
    let mut frame = bar.frame();
    frame.size.height = MAC_BAR;
    frame.origin.y = ns.frame().size.height - MAC_BAR;
    bar.setFrame(frame);
    let step = mini.frame().origin.x - close.frame().origin.x;
    for (i, b) in [close, mini, zoom].iter().enumerate() {
        let mut origin = b.frame().origin;
        origin.x = MAC_INSET + step * i as f64;
        origin.y = (MAC_BAR - b.frame().size.height) / 2.;
        b.setFrameOrigin(origin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds {
        Bounds { x, y, w, h }
    }

    fn zones() -> Vec<(Zone, Bounds)> {
        vec![(Zone::Bar, b(0., 0., 1000., 38.)), (Zone::Control, b(10., 4., 30., 30.)), (Zone::Button(Caption::Close), b(954., 0., 46., 38.))]
    }

    #[test]
    fn the_bar_moves_the_window_except_on_its_controls() {
        assert_eq!(zone_in(zones(), Cover::None, 500., 20.), Some(Zone::Bar));
        assert_eq!(zone_in(zones(), Cover::None, 20., 20.), Some(Zone::Control));
        assert_eq!(zone_in(zones(), Cover::None, 980., 1.), Some(Zone::Button(Caption::Close)));
        assert_eq!(zone_in(zones(), Cover::None, 500., 60.), None);
    }

    #[test]
    fn a_sheet_keeps_the_bar_moving_and_the_window_buttons_live() {
        assert_eq!(zone_in(zones(), Cover::Sheet, 20., 20.), Some(Zone::Bar));
        assert_eq!(zone_in(zones(), Cover::Sheet, 980., 20.), Some(Zone::Button(Caption::Close)));
    }

    #[test]
    fn a_menu_leaves_only_the_window_buttons_to_the_system() {
        assert_eq!(zone_in(zones(), Cover::Menu, 500., 20.), None);
        assert_eq!(zone_in(zones(), Cover::Menu, 980., 20.), Some(Zone::Button(Caption::Close)));
    }

    #[test]
    fn gnome_button_layouts_pick_side_and_order() {
        assert_eq!(buttons_from("appmenu:minimize,maximize,close"), (vec![Caption::Minimize, Caption::Maximize, Caption::Close], false));
        assert_eq!(buttons_from("close,minimize,maximize:"), (vec![Caption::Close, Caption::Minimize, Caption::Maximize], true));
        assert_eq!(buttons_from("appmenu:close"), (vec![Caption::Close], false));
        assert_eq!(buttons_from(""), (vec![], false));
    }
}
