use std::fs::OpenOptions;
use std::os::windows::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, SetBkMode, SetTextColor, TRANSPARENT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DrawTextW,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW, IsWindowVisible, KillTimer, MSG, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage,
    WM_CLOSE, WM_DESTROY, WM_PAINT, WM_SHOWWINDOW, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

/// Window class of the stand-in game windows, so the manager can find them by process.
pub const CLASS: &str = "ManycraftStandIn";
const MAGIC: u32 = 0x4D43_5056;
const PREVIEW_W: u32 = 320;
const PREVIEW_H: u32 = 180;
const PREVIEW_EVERY: Duration = Duration::from_micros(16_667);
const REPAINT_TIMER: usize = 1;

static INDEX: AtomicU32 = AtomicU32::new(0);
static TICKS: AtomicU32 = AtomicU32::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Runs a stand-in game: a hidden native window that paints a test pattern while shown, and a 60 Hz preview writer
/// using the companion mod's file format. Arguments: index, preview path, pid of the manager to outlive.
pub fn run(args: &[String]) {
    let index: u32 = args.first().and_then(|a| a.parse().ok()).unwrap_or(0);
    let path = PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "preview.bin".into()));
    let parent: u32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(0);
    INDEX.store(index, Ordering::Relaxed);
    std::thread::spawn(move || write_previews(index, &path, parent));
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide(CLASS);
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        let title = wide(&format!("Stand-in {}", index + 1));
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPEDWINDOW, 0, 0, 870, 519, std::ptr::null_mut(), std::ptr::null_mut(), instance, std::ptr::null());
        if hwnd.is_null() {
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SHOWWINDOW => {
                if wparam != 0 {
                    SetTimer(hwnd, REPAINT_TIMER, 16, None);
                } else {
                    KillTimer(hwnd, REPAINT_TIMER);
                }
                0
            }
            WM_TIMER => {
                if IsWindowVisible(hwnd) != 0 {
                    TICKS.fetch_add(1, Ordering::Relaxed);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                0
            }
            WM_PAINT => {
                paint(hwnd);
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(hwnd, &mut client);
        let index = INDEX.load(Ordering::Relaxed);
        let (r, g, b) = tint(index);
        let bg = CreateSolidBrush(colorref(r / 3, g / 3, b / 3));
        FillRect(dc, &client, bg);
        DeleteObject(bg);
        let edge = CreateSolidBrush(colorref(255, 255, 255));
        let w = client.right;
        let h = client.bottom;
        for side in [
            RECT { left: 0, top: 0, right: w, bottom: 2 },
            RECT { left: 0, top: h - 2, right: w, bottom: h },
            RECT { left: 0, top: 0, right: 2, bottom: h },
            RECT { left: w - 2, top: 0, right: w, bottom: h },
        ] {
            FillRect(dc, &side, edge);
        }
        DeleteObject(edge);
        let bar = CreateSolidBrush(colorref(r, g, b));
        let x = (TICKS.load(Ordering::Relaxed) as i32 * 6) % w.max(1);
        FillRect(dc, &RECT { left: x, top: h / 2 - 40, right: (x + w / 8).min(w), bottom: h / 2 + 40 }, bar);
        DeleteObject(bar);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, colorref(255, 255, 255));
        let mut text = wide(&format!("Stand-in {} \u{2022} {}x{}", index + 1, w, h));
        let len = text.len() as i32 - 1;
        DrawTextW(dc, text.as_mut_ptr(), len, &mut client, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        EndPaint(hwnd, &ps);
    }
}

fn colorref(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

fn tint(index: u32) -> (u8, u8, u8) {
    const TINTS: [(u8, u8, u8); 6] = [(64, 160, 150), (220, 150, 60), (90, 130, 220), (200, 90, 120), (120, 190, 80), (170, 110, 210)];
    TINTS[index as usize % TINTS.len()]
}

fn alive(parent: u32) -> bool {
    if parent == 0 {
        return true;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, parent);
        if handle.is_null() {
            return false;
        }
        let ended = WaitForSingleObject(handle, 0) == WAIT_OBJECT_0;
        windows_sys::Win32::Foundation::CloseHandle(handle);
        !ended
    }
}

fn write_previews(index: u32, path: &PathBuf, parent: u32) {
    let Ok(file) = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path) else { return };
    let header = |seq: u32| -> [u8; 16] {
        let mut h = [0u8; 16];
        for (i, word) in [MAGIC, seq, PREVIEW_W, PREVIEW_H].into_iter().enumerate() {
            h[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        h
    };
    let (r, g, b) = tint(index);
    let mut rgba = vec![0u8; (PREVIEW_W * PREVIEW_H * 4) as usize];
    let mut seq = 0u32;
    let start = Instant::now();
    let mut next = start;
    let mut checked = start;
    loop {
        let t = start.elapsed().as_secs_f32();
        let bar = ((t * 120.0) as u32) % PREVIEW_W;
        for y in 0..PREVIEW_H {
            for x in 0..PREVIEW_W {
                let i = ((y * PREVIEW_W + x) * 4) as usize;
                let lit = x.abs_diff(bar) < 24;
                let shade = (y * 255 / PREVIEW_H) as u8;
                let px = if lit { [r, g, b, 255] } else { [shade / 3, shade / 3 + 10, shade / 2 + 20, 255] };
                rgba[i..i + 4].copy_from_slice(&px);
            }
        }
        let odd = seq.wrapping_add(1);
        let _ = file.seek_write(&odd.to_le_bytes(), 4);
        let _ = file.seek_write(&header(odd)[8..], 8);
        let _ = file.seek_write(&rgba, 16);
        let even = odd.wrapping_add(1);
        let _ = file.seek_write(&header(even), 0);
        seq = even;
        next += PREVIEW_EVERY;
        let now = Instant::now();
        if now.duration_since(checked) > Duration::from_secs(1) {
            checked = now;
            if !alive(parent) {
                std::process::exit(0);
            }
        }
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}
