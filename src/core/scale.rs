//! The size scales every front end draws with: spacing, radii, text, controls and icons.

/// Spacing scale for padding, margins and gaps.
pub mod space {
    pub const XXS: f32 = 2.;
    pub const XS: f32 = 4.;
    pub const S: f32 = 8.;
    pub const M: f32 = 12.;
    pub const L: f32 = 16.;
    pub const XL: f32 = 24.;
    pub const XXL: f32 = 32.;
}

/// Corner radii.
pub mod radius {
    /// Keyboard chips, small marks.
    pub const SMALL: f32 = 4.;
    /// Buttons, inputs, nested rows.
    pub const INNER: f32 = 6.;
    /// Chips and inline controls.
    pub const CONTROL: f32 = 8.;
    /// Cards, tiles, grouped containers.
    pub const CARD: f32 = 12.;
    /// Capsules and circles.
    pub const FULL: f32 = 999.;
}

/// Text styles in points, after the macOS text styles.
pub mod font {
    pub const LARGE_TITLE: f32 = 26.;
    pub const TITLE: f32 = 22.;
    pub const TITLE2: f32 = 17.;
    pub const TITLE3: f32 = 15.;
    pub const BODY: f32 = 13.;
    pub const CALLOUT: f32 = 12.;
    pub const CAPTION: f32 = 11.;
    pub const CAPTION2: f32 = 10.;
    pub const LINE: f32 = 1.22;
}

/// Control sizes.
pub mod control {
    /// Compact control height.
    pub const S: f32 = 24.;
    /// Standard control height.
    pub const M: f32 = 28.;
    /// Prominent control height, like the main button of an empty state.
    pub const L: f32 = 36.;
    /// The widest a block of explanatory text gets.
    pub const READING: f32 = 360.;
    /// Narrowest and widest a name being renamed in place gets.
    pub const RENAME_MIN: f32 = 160.;
    pub const RENAME_MAX: f32 = 360.;
    /// Width of a character of a name being renamed in place, as a share of its font size.
    pub const RENAME_GLYPH: f32 = 0.6;
    /// Width of the value between a stepper's minus and plus.
    pub const STEPPER_VALUE: f32 = 64.;
    /// Width of a stepper value with a unit, like "32 Chunks".
    pub const STEPPER_VALUE_WIDE: f32 = 80.;
    /// Width of a field for a number of up to five digits, like a window width.
    pub const NUMBER: f32 = 80.;
    /// Width of the version menu, enough for a snapshot id beside its kind.
    pub const VERSION_MENU: f32 = 280.;
    /// Width of the Java menu, enough for a version, vendor and path.
    pub const JAVA_MENU: f32 = 360.;
}

/// Icon sizes.
pub mod iconsize {
    pub const XS: f32 = 12.;
    pub const S: f32 = 14.;
    pub const M: f32 = 16.;
    pub const L: f32 = 20.;
    /// Empty states.
    pub const HERO: f32 = 40.;
}

/// Durations of motion, and whether the system asks for less of it.
pub mod motion {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::Duration;

    /// Hover, press, small confirmations.
    pub const FAST: Duration = Duration::from_millis(120);
    /// Reveals, sheets.
    pub const MEDIUM: Duration = Duration::from_millis(180);
    /// Elements travelling across the window.
    pub const SLOW: Duration = Duration::from_millis(240);

    static REDUCED: AtomicU8 = AtomicU8::new(0);

    /// Whether the person asked the system for less motion; `MANYCRAFT_REDUCE_MOTION=1` forces it.
    pub fn reduced() -> bool {
        match REDUCED.load(Ordering::Relaxed) {
            1 => false,
            2 => true,
            _ => {
                let on = std::env::var("MANYCRAFT_REDUCE_MOTION").map_or_else(|_| system(), |v| v == "1");
                REDUCED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
                on
            }
        }
    }

    /// `duration`, or none at all when the person asked for less motion.
    pub fn timed(duration: Duration) -> Duration {
        if reduced() { Duration::ZERO } else { duration }
    }

    #[cfg(windows)]
    fn system() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
        let mut on: i32 = 1;
        let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut on).cast(), 0) } != 0;
        ok && on == 0
    }

    #[cfg(target_os = "macos")]
    fn system() -> bool {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn system() -> bool {
        false
    }
}
