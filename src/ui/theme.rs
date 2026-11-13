use std::sync::Mutex;

use gpui_kit::component::{Theme as ComponentTheme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, black, px, rgb, white};

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

/// Opacities that tint states.
pub mod tint {
    /// A control that waits for something.
    pub const DISABLED: f32 = 0.4;
    /// What the pointer adds over a control.
    pub const HOVER: f32 = 0.06;
    /// What the pointer adds over a filled control; pressing doubles it.
    pub const SHADE: f32 = 0.1;
    /// The keyboard focus ring.
    pub const RING: f32 = 0.8;
    /// The wash behind an account that needs work.
    pub const ATTENTION: f32 = 0.12;
}

/// The ring the keyboard focus draws around a control.
pub mod ring {
    use std::sync::RwLock;

    use gpui_kit::Hsla;

    /// Width of the ring outside the control.
    pub const WIDTH: f32 = 3.;

    static ACCENT: RwLock<Hsla> = RwLock::new(Hsla { h: 0.58, s: 1., l: 0.5, a: 1. });

    /// Remembers the accent of the theme now drawn, so `color` follows it.
    pub fn set(accent: Hsla) {
        *ACCENT.write().unwrap_or_else(|e| e.into_inner()) = accent;
    }

    /// The ring color for the theme now drawn.
    pub fn color() -> Hsla {
        super::with_alpha(*ACCENT.read().unwrap_or_else(|e| e.into_inner()), super::tint::RING)
    }
}

/// What the user chose for the appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    System,
    Light,
    Dark,
}

impl Choice {
    /// Reads `light`, `dark` or `system`; anything else follows the system.
    pub fn parse(name: &str) -> Self {
        match name {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

static CHOICE: Mutex<Choice> = Mutex::new(Choice::System);

/// Semantic colors for the light or dark appearance, after Apple's system palette.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Behind everything: the window.
    pub window: Hsla,
    /// Surfaces that carry content, like tiles and sheets.
    pub content: Hsla,
    /// Grouped areas and inset fields on the window.
    pub raised: Hsla,
    pub primary: Hsla,
    pub secondary: Hsla,
    pub tertiary: Hsla,
    pub separator: Hsla,
    pub accent: Hsla,
    /// The accent for text, which needs more contrast than a fill.
    pub accent_text: Hsla,
    /// Text and icons on a solid accent fill.
    pub on_accent: Hsla,
    /// An account that needs work. Never the accent, so it stands out from selection and actions.
    pub attention: Hsla,
    pub attention_text: Hsla,
    pub success: Hsla,
    pub success_text: Hsla,
    pub danger: Hsla,
    pub danger_text: Hsla,
    /// Text and icons on a solid danger fill.
    pub on_danger: Hsla,
    /// Black in light, white in dark: what hover and press layers are made of.
    pub ink: Hsla,
    /// Darkens a filled control on hover and press.
    pub shade: Hsla,
    /// Popovers and menus.
    pub material: Hsla,
}

impl Global for Theme {}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// `color` with its opacity set to `alpha`.
pub fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

impl Theme {
    /// The light or dark palette.
    pub fn plain(dark: bool) -> Self {
        let ink = if dark { white() } else { black() };
        Self {
            dark,
            window: if dark { c(0x1E1E1E) } else { c(0xF2F2F7) },
            content: if dark { c(0x2C2C2E) } else { c(0xFFFFFF) },
            raised: if dark { c(0x3A3A3C) } else { c(0xE8E8ED) },
            primary: ink.opacity(0.88),
            secondary: ink.opacity(0.62),
            tertiary: ink.opacity(if dark { 0.47 } else { 0.56 }),
            separator: ink.opacity(if dark { 0.14 } else { 0.12 }),
            accent: if dark { c(0x0A6FE8) } else { c(0x0071E3) },
            accent_text: if dark { c(0x6CB3FF) } else { c(0x0053B4) },
            on_accent: c(0xFFFFFF),
            attention: if dark { c(0xFF9F0A) } else { c(0xFF9500) },
            attention_text: if dark { c(0xFFB340) } else { c(0x824800) },
            success: if dark { c(0x30D158) } else { c(0x34C759) },
            success_text: if dark { c(0x30D158) } else { c(0x1E7A34) },
            danger: if dark { c(0xFF453A) } else { c(0xFF3B30) },
            danger_text: if dark { c(0xFF6961) } else { c(0xC4271D) },
            on_danger: c(0xFFFFFF),
            ink,
            shade: black().opacity(tint::SHADE),
            material: if dark { c(0x3A3A3C) } else { c(0xFBFBFD) },
        }
    }

    /// The theme drawn now.
    pub fn get(cx: &App) -> Self {
        *cx.global::<Theme>()
    }

    pub fn font_family() -> &'static str {
        if cfg!(target_os = "macos") {
            ".SystemUIFont"
        } else if cfg!(target_os = "windows") {
            "Segoe UI"
        } else {
            "Inter"
        }
    }

    pub fn mono_family() -> &'static str {
        if cfg!(target_os = "macos") {
            "Menlo"
        } else if cfg!(target_os = "windows") {
            "Consolas"
        } else {
            "DejaVu Sans Mono"
        }
    }
}

/// Whether a window appearance is one of the dark ones.
pub fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Whether the choice, with the system's current appearance, is dark.
pub fn resolve(choice: Choice, system_dark: bool) -> bool {
    match choice {
        Choice::System => system_dark,
        Choice::Light => false,
        Choice::Dark => true,
    }
}

/// Sets the appearance the app starts in.
pub fn start_with(choice: Choice) {
    *CHOICE.lock().unwrap_or_else(|e| e.into_inner()) = choice;
}

fn current(window: &Window) -> Theme {
    let choice = *CHOICE.lock().unwrap_or_else(|e| e.into_inner());
    Theme::plain(resolve(choice, is_dark(window.appearance())))
}

/// Draws the app in the chosen appearance, following the system where the choice says so.
pub fn refresh(window: &mut Window, cx: &mut App) {
    apply(current(window), Some(window), cx);
}

/// Switches the appearance now, in every window.
pub fn choose(choice: Choice, window: &mut Window, cx: &mut App) {
    start_with(choice);
    refresh(window, cx);
    cx.refresh_windows();
}

/// Sets the palette and maps it onto the component library so its controls match.
pub fn apply(t: Theme, window: Option<&mut Window>, cx: &mut App) {
    let dark = t.dark;
    cx.set_global(t);
    ring::set(t.accent);
    ComponentTheme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, window, cx);
    let ct = ComponentTheme::global_mut(cx);
    ct.font_family = Theme::font_family().into();
    ct.font_size = px(font::BODY);
    ct.mono_font_family = Theme::mono_family().into();
    ct.motion.duration_slow = super::motion::SLOW;
    ct.scrollbar_mode = gpui_kit::base::ScrollbarMode::Hover;
    ct.radius = px(radius::INNER);
    ct.radius_lg = px(radius::CARD);
    let k = &mut ct.colors;
    k.background = t.window;
    k.foreground = t.primary;
    k.border = t.separator;
    k.input = t.ink.opacity(if dark { 0.14 } else { 0.16 });
    k.ring = ring::color();
    k.primary = t.accent;
    k.primary_hover = t.accent.opacity(0.9);
    k.primary_active = t.accent.opacity(0.8);
    k.primary_foreground = t.on_accent;
    k.button_primary = t.accent;
    k.button_primary_hover = t.accent.blend(white().opacity(0.08));
    k.button_primary_active = t.accent.blend(black().opacity(0.12));
    k.button_primary_foreground = t.on_accent;
    k.button = t.ink.opacity(if dark { 0.1 } else { 0.05 });
    k.button_hover = t.ink.opacity(if dark { 0.14 } else { 0.08 });
    k.button_active = t.ink.opacity(0.18);
    k.button_foreground = t.primary;
    k.button_danger = t.danger;
    k.button_danger_foreground = t.on_danger;
    k.secondary = t.ink.opacity(if dark { 0.1 } else { 0.05 });
    k.secondary_hover = t.ink.opacity(if dark { 0.14 } else { 0.08 });
    k.secondary_active = t.ink.opacity(0.18);
    k.secondary_foreground = t.primary;
    k.muted = t.raised;
    k.muted_foreground = t.secondary;
    k.accent = t.ink.opacity(0.06);
    k.accent_foreground = t.primary;
    k.popover = t.material;
    k.popover_foreground = t.primary;
    k.selection = t.accent.opacity(0.3);
    k.caret = t.accent;
    k.link = t.accent_text;
    k.link_hover = t.accent_text.opacity(0.85);
    k.link_active = t.accent_text.opacity(0.7);
    k.switch = t.ink.opacity(if dark { 0.22 } else { 0.15 });
    k.switch_thumb = white();
    k.list_active = t.accent.opacity(0.15);
    k.list_hover = t.ink.opacity(0.06);
    k.tab_bar = t.raised;
    k.tab_active = t.content;
    k.danger = t.danger;
    k.danger_foreground = t.on_danger;
    k.success = t.success;
    k.warning = t.attention;
    k.info = t.accent;
    k.overlay = black().opacity(if dark { 0.45 } else { 0.25 });
    k.scrollbar_thumb = t.ink.opacity(0.25);
    k.scrollbar_thumb_hover = t.ink.opacity(0.4);
    k.title_bar = t.window;
    k.title_bar_border = t.separator;
    k.window_border = t.separator;
    ct.tokens = ct.colors.into();
}

#[cfg(test)]
mod tests {
    use gpui_kit::Rgba;

    use super::*;

    fn linear(v: f32) -> f32 {
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }

    fn luminance(c: Rgba) -> f32 {
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
    }

    fn over(top: Hsla, bottom: Hsla) -> Rgba {
        let (t, b) = (Rgba::from(top), Rgba::from(bottom));
        let mix = |a: f32, z: f32| a * t.a + z * (1. - t.a);
        Rgba { r: mix(t.r, b.r), g: mix(t.g, b.g), b: mix(t.b, b.b), a: 1. }
    }

    fn contrast(text: Hsla, background: Hsla) -> f32 {
        let (a, b) = (luminance(over(text, background)), luminance(Rgba::from(background)));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_is_readable_in_both_appearances() {
        for dark in [false, true] {
            let t = Theme::plain(dark);
            let checks = [
                ("primary on window", contrast(t.primary, t.window), 4.5),
                ("primary on content", contrast(t.primary, t.content), 4.5),
                ("secondary on window", contrast(t.secondary, t.window), 4.5),
                ("secondary on content", contrast(t.secondary, t.content), 4.5),
                ("tertiary on window", contrast(t.tertiary, t.window), 3.),
                ("on accent", contrast(t.on_accent, t.accent), 4.5),
                ("on danger", contrast(t.on_danger, t.danger), 3.),
                ("accent text on window", contrast(t.accent_text, t.window), 4.5),
                ("attention text on window", contrast(t.attention_text, t.window), 4.5),
                ("success text on window", contrast(t.success_text, t.window), 4.5),
                ("danger text on window", contrast(t.danger_text, t.window), 4.5),
                ("accent on window", contrast(t.accent, t.window), 3.),
            ];
            for (name, ratio, least) in checks {
                assert!(ratio >= least, "{name} ({}): {ratio:.2} < {least}", if dark { "dark" } else { "light" });
            }
        }
    }

    #[test]
    fn choice_parses_names() {
        assert_eq!(Choice::parse("dark"), Choice::Dark);
        assert_eq!(Choice::parse("light"), Choice::Light);
        assert_eq!(Choice::parse(""), Choice::System);
        assert!(resolve(Choice::System, true) && !resolve(Choice::Light, true) && resolve(Choice::Dark, false));
    }
}
