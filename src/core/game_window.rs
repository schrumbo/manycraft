//! The game window seen from outside the game: its border tinted in the account's colour.

use super::account::Account;

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
