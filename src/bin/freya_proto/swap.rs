use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GW_HWNDPREV, GetWindowLongW, WS_EX_TOPMOST, GW_OWNER, GetClassNameW, GetClientRect, GetWindow, GetWindowRect, GetWindowThreadProcessId, HWND_TOP, IsWindow, IsWindowVisible, SW_HIDE, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos, ShowWindow, WINDOWPOS, WM_DPICHANGED, WM_NCDESTROY, WM_SIZE, WM_WINDOWPOSCHANGED, SIZE_MINIMIZED,
};

/// A rectangle in screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Px {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A tile's area in the manager's logical layout units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Default)]
struct Swap {
    manager: usize,
    custom_scale: f64,
    tiles: HashMap<usize, Area>,
    games: HashMap<usize, u32>,
    active: Option<(usize, usize)>,
    parked: Option<usize>,
}

static STATE: Mutex<Option<Swap>> = Mutex::new(None);
static ASYNC: AtomicBool = AtomicBool::new(false);
/// How many times the game window was moved after the manager or a tile moved.
pub static PLACED: AtomicU64 = AtomicU64::new(0);

fn with<T>(f: impl FnOnce(&mut Swap) -> T) -> T {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Swap::default))
}

/// Remembers which game process plays in which tile, and whether game windows are moved without waiting for the game.
pub fn configure(games: HashMap<usize, u32>, asynchronous: bool) {
    ASYNC.store(asynchronous, Ordering::Relaxed);
    with(|s| s.games = games);
}

/// Hooks the manager window so the game window follows every move, resize and DPI change in the same message.
pub fn attach(manager: HWND) {
    with(|s| s.manager = manager as usize);
    unsafe { SetWindowSubclass(manager, Some(follow), 1, 0) };
}

/// The zoom Freya applies on top of the monitor's scale.
pub fn set_custom_scale(scale: f64) {
    with(|s| s.custom_scale = scale);
    place();
}

/// The manager window.
pub fn manager() -> HWND {
    with(|s| s.manager) as HWND
}

/// Records where tile `index` is laid out, and moves the game along when it plays there.
pub fn set_tile(index: usize, area: Area) {
    let moved = with(|s| s.tiles.insert(index, area) != Some(area) && s.active.is_some_and(|(i, _)| i == index));
    if moved {
        place();
    }
}

/// Puts the game of tile `index` over the tile and shows it. Returns whether that tile has a game window.
pub fn play(index: usize) -> bool {
    hide();
    with(|s| s.parked = None);
    let Some(game) = with(|s| s.games.get(&index).copied()).and_then(game_window) else { return false };
    with(|s| s.active = Some((index, game as usize)));
    place();
    unsafe {
        ShowWindow(game, SW_SHOWNOACTIVATE);
        SetWindowPos(game, HWND_TOP, 0, 0, 0, 0, 0x0001 | 0x0002 | SWP_NOACTIVATE);
    }
    true
}

/// Hides the game window that plays over a tile, if any.
pub fn hide() {
    with(|s| s.parked = None);
    if let Some((_, game)) = with(|s| s.active.take()) {
        unsafe { ShowWindow(game as HWND, SW_HIDE) };
    }
}

/// The tile whose game is shown.
pub fn active() -> Option<usize> {
    with(|s| s.active.map(|(i, _)| i))
}

/// The game window shown over a tile.
pub fn active_window() -> Option<HWND> {
    with(|s| s.active.map(|(_, g)| g as HWND))
}

fn scale(s: &Swap) -> f64 {
    let dpi = unsafe { GetDpiForWindow(s.manager as HWND) };
    let os = if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 };
    os * if s.custom_scale > 0.0 { s.custom_scale } else { 1.0 }
}

/// Where tile `index` is on screen, in pixels.
pub fn tile_on_screen(index: usize) -> Option<Px> {
    with(|s| {
        let area = *s.tiles.get(&index)?;
        let mut origin = POINT { x: 0, y: 0 };
        unsafe { ClientToScreen(s.manager as HWND, &mut origin) };
        let k = scale(s);
        let edge = |v: f32| (v as f64 * k).round() as i32;
        let (x0, y0, x1, y1) = (edge(area.x), edge(area.y), edge(area.x + area.w), edge(area.y + area.h));
        Some(Px { x: origin.x + x0, y: origin.y + y0, w: x1 - x0, h: y1 - y0 })
    })
}

/// The client area of `hwnd` on screen.
pub fn client_on_screen(hwnd: HWND) -> Option<Px> {
    let mut client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let mut origin = POINT { x: 0, y: 0 };
    let ok = unsafe { GetClientRect(hwnd, &mut client) != 0 && ClientToScreen(hwnd, &mut origin) != 0 };
    ok.then_some(Px { x: origin.x, y: origin.y, w: client.right, h: client.bottom })
}

fn place() {
    let Some((index, game)) = with(|s| s.active) else { return };
    let game = game as HWND;
    if unsafe { IsWindow(game) } == 0 {
        with(|s| s.active = None);
        return;
    }
    let (Some(tile), Some(client)) = (tile_on_screen(index), client_on_screen(game)) else { return };
    let mut outer = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetWindowRect(game, &mut outer) };
    let left = client.x - outer.left;
    let top = client.y - outer.top;
    let right = outer.right - (client.x + client.w);
    let bottom = outer.bottom - (client.y + client.h);
    let mut flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER;
    if ASYNC.load(Ordering::Relaxed) {
        flags |= SWP_ASYNCWINDOWPOS;
    }
    unsafe { SetWindowPos(game, std::ptr::null_mut(), tile.x - left, tile.y - top, tile.w + left + right, tile.h + top + bottom, flags) };
    PLACED.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "system" fn follow(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_WINDOWPOSCHANGED | WM_DPICHANGED => {
            let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            place();
            if msg == WM_WINDOWPOSCHANGED && unsafe { (*(lparam as *const WINDOWPOS)).flags } & SWP_NOZORDER == 0 {
                raise(hwnd);
            }
            return result;
        }
        WM_SIZE if wparam == SIZE_MINIMIZED as usize => {
            let index = active();
            hide();
            with(|s| s.parked = index);
        }
        WM_SIZE => {
            if let Some(index) = with(|s| s.parked.take()) {
                play(index);
            }
        }
        WM_NCDESTROY => unsafe {
            RemoveWindowSubclass(hwnd, Some(follow), 1);
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn raise(manager: HWND) {
    let Some(game) = active_window() else { return };
    let above = unsafe { GetWindow(manager, GW_HWNDPREV) };
    if above == game {
        return;
    }
    let topmost = !above.is_null() && unsafe { GetWindowLongW(above, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST != 0;
    let after = if above.is_null() || topmost { HWND_TOP } else { above };
    unsafe { SetWindowPos(game, after, 0, 0, 0, 0, 0x0001 | 0x0002 | SWP_NOACTIVATE | SWP_NOOWNERZORDER) };
}

/// The top-level game window of process `pid`: SDL or GLFW for Minecraft, or the prototype's stand-in.
pub fn game_window(pid: u32) -> Option<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let found = unsafe { &mut *(data as *mut (u32, HWND)) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        if owner != found.0 || !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null() {
            return 1;
        }
        let mut class = [0u16; 32];
        let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        if matches!(class.as_str(), "SDL_app" | "GLFW30") || class == super::stand_in::CLASS {
            found.1 = hwnd;
            return 0;
        }
        1
    }
    let mut found: (u32, HWND) = (pid, std::ptr::null_mut());
    unsafe { EnumWindows(Some(each), &raw mut found as LPARAM) };
    (!found.1.is_null()).then_some(found.1)
}

/// Whether the game shown over a tile is still visible.
pub fn game_visible() -> bool {
    active_window().is_some_and(|g| unsafe { IsWindowVisible(g) } != 0)
}
