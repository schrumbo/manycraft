use std::sync::Mutex;

use gpui_kit::component::{Theme as ComponentTheme, ThemeMode};
use gpui_kit::{App, FocusHandle, Global, Hsla, WeakFocusHandle, Window, WindowAppearance, black, px, rgb, white};

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

/// The instance grid and its tiles.
pub mod tile {
    /// Smallest width a tile gets in each size.
    pub const SMALL: f32 = 168.;
    pub const MEDIUM: f32 = 240.;
    pub const LARGE: f32 = 340.;
    /// Smallest tile width "Einpassen" goes down to before it scrolls after all.
    pub const FIT_MIN: f32 = 96.;
    /// Below this width the status line keeps only its first words.
    pub const DENSE: f32 = 150.;
    /// Below this width a tile shows only its name under the preview; the status moves into a tooltip.
    pub const TINY: f32 = 110.;
    /// Height of the strip under the preview.
    pub const STRIP: f32 = 44.;
    /// Height of the strip on small tiles, where only the name and status fit.
    pub const STRIP_SMALL: f32 = 38.;
    /// Height of the one-line strip on tiny tiles.
    pub const STRIP_TINY: f32 = 26.;
    /// Space between tiles, by tile size.
    pub const GAP: f32 = 16.;
    pub const GAP_SMALL: f32 = 12.;
    pub const GAP_TINY: f32 = 8.;
    /// Space around the grid, the same as the toolbar's edges.
    pub const INSET: f32 = 16.;
    /// Width of the attention and focus ring.
    pub const RING: f32 = 2.;
    /// How bright the preview stays behind an account that needs work.
    pub const DIMMED: f32 = 0.7;
    /// The status mark in front of the status line.
    pub const GLYPH: f32 = 7.;
    /// Height of the shade that carries the actions at the bottom of a hovered preview.
    pub const SHADE: f32 = 56.;
    /// The skin head in the strip.
    pub const HEAD: f32 = 22.;
    /// Unread whispers badge.
    pub const BADGE: f32 = 18.;
    /// Height of a batch heading in the grid of all batches.
    pub const HEADING: f32 = 40.;
    /// Smallest tile width whose strip still carries a labelled sign-in button.
    pub const SIGN_IN: f32 = 280.;
    /// Blur of the soft shadow under a tile in the light appearance.
    pub const SHADOW_BLUR: f32 = 3.;
    /// Height of the fade at the bottom of a still, so the strip below stays apart from it.
    pub const FADE: f32 = 2.;
    /// Side of the skin face on a tile without a still.
    pub const FACE: f32 = 32.;
    /// How far past the visible rows the grid lays out, so the wheel always knows the next row's height.
    pub const OVERDRAW: f32 = 8.;
}

/// The sidebar and toolbar around the grid.
pub mod shell {
    pub const SIDEBAR: f32 = 220.;
    pub const TOOLBAR: f32 = 48.;
    /// Height of the quiet notice line under the toolbar.
    pub const NOTICE: f32 = 32.;
    pub const ROW: f32 = 28.;
    pub const SEARCH: f32 = 180.;
    /// The attention dot in a sidebar row.
    pub const DOT: f32 = 6.;
    /// The attention count in a sidebar row.
    pub const BADGE: f32 = 19.;
    /// Below this content width the toolbar shows its buttons as icons and its filter as a pop-up button.
    pub const COMPACT: f32 = 860.;
    /// Width the toolbar title keeps before anything else shrinks.
    pub const TITLE: f32 = 96.;
    /// Below this window width the sidebar floats over the grid instead of beside it.
    pub const NARROW: f32 = 880.;
}

/// The command palette.
pub mod palette {
    pub const WIDTH: f32 = 560.;
    /// Distance from the top of the window.
    pub const TOP: f32 = 72.;
    /// Height of the search field row.
    pub const FIELD: f32 = 44.;
    /// Height of one entry or group heading.
    pub const ROW: f32 = 32.;
    /// How many rows show before the list scrolls.
    pub const ROWS: f32 = 9.5;
}

/// Several selected tiles and the bar that acts on them.
pub mod selection {
    /// Height of the floating bar.
    pub const BAR: f32 = 44.;
    /// Space between the bar and the bottom of the grid.
    pub const BOTTOM: f32 = 16.;
    /// The check mark on a selected tile.
    pub const CHECK: f32 = 20.;
    /// Width of the message field in the bar.
    pub const FIELD: f32 = 300.;
}

/// The Chat page.
pub mod chat {
    /// Width of the column naming the account and batch of a line.
    pub const LABEL: f32 = 148.;
    /// The wash behind a whisper.
    pub const WASH: f32 = 0.1;
    /// Width of the bar at the edge of a whisper.
    pub const BAR: f32 = 2.;
    /// Width of the picker for the batch shown and for the one written to.
    pub const PICKER: f32 = 200.;
    /// Width of the time at the start of a line.
    pub const TIME: f32 = 44.;
    /// Space under the last line, and the gap between it and the jump button.
    pub const INSET: f32 = 12.;
    /// The most characters shown of a line before it is cut off.
    pub const SHOWN_CHARS: usize = 600;
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
    /// The shade behind the floating sidebar.
    pub const SCRIM: f32 = 0.28;
    /// The bottom of the shade under a hovered preview's actions.
    pub const PREVIEW_SHADE: f32 = 0.55;
    /// How bright a tile stays while it is pressed.
    pub const PRESSED: f32 = 0.85;
    /// How much of a stopped instance's last frame shows through.
    pub const STILL: f32 = 0.4;
    /// How much of the skin face shows on a tile that never ran.
    pub const FACE: f32 = 0.5;
    /// The soft shadow under a card in the light appearance.
    pub const CARD_SHADOW: f32 = 0.08;
    /// Controls floating over a preview.
    pub const FLOATING: f32 = 0.92;
    /// An outline in a status color.
    pub const OUTLINE: f32 = 0.5;
}

/// The ring the keyboard focus draws around a control.
pub mod ring {
    use std::sync::RwLock;

    use gpui_kit::Hsla;

    /// Width of the ring outside the control.
    pub const WIDTH: f32 = 3.;

    static ACCENT: RwLock<Hsla> = RwLock::new(Hsla { h: 0.49, s: 0.83, l: 0.26, a: 1. });

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
    /// A recessed area inside a card, like a tile with no picture yet.
    pub well: Hsla,
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
    /// Text on a solid attention fill.
    pub on_attention: Hsla,
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
            well: if dark { c(0x242426) } else { c(0xECECF1) },
            primary: ink.opacity(0.88),
            secondary: ink.opacity(0.62),
            tertiary: ink.opacity(if dark { 0.47 } else { 0.56 }),
            separator: ink.opacity(if dark { 0.14 } else { 0.12 }),
            accent: if dark { c(0x0E7C77) } else { c(0x0B7A75) },
            accent_text: if dark { c(0x4FD1C5) } else { c(0x0A6560) },
            on_accent: c(0xFFFFFF),
            attention: if dark { c(0xFF9F0A) } else { c(0xFF9500) },
            attention_text: if dark { c(0xFFB340) } else { c(0x824800) },
            on_attention: c(0x1C1C1E),
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

const TEXT_FIELD: &str = "Input";

#[derive(Default)]
struct FocusSeen(Option<WeakFocusHandle>, bool);

impl Global for FocusSeen {}

/// Shows the component library's focus rings only on controls the keyboard moved focus to, like focus-visible:
/// a button, segment or menu focused by a click, a theme change or a resize looks like an unfocused one.
/// Text fields always show their ring, as on every platform. Call it from the render of every window's root view.
pub fn follow_focus(window: &Window, cx: &mut App) {
    if !window.is_window_active() {
        return;
    }
    note_focus(window, window.last_input_was_keyboard(), cx);
    show_rings(cx);
}

/// Lets every keystroke in any window, including Tab and arrows that controls consume, count as the keyboard moving focus.
pub fn watch_focus(cx: &mut App) {
    cx.intercept_keystrokes(|_, window, cx| window.defer(cx, |window, cx| focus_moved(window, true, cx))).detach();
}

/// Notes whether the keyboard moved focus, right after an input event in `window`, then updates the rings like `follow_focus`.
pub fn focus_moved(window: &mut Window, keyboard: bool, cx: &mut App) {
    note_focus(window, keyboard, cx);
    show_rings(cx);
}

fn show_rings(cx: &mut App) {
    let keyboard = cx.default_global::<FocusSeen>().1;
    let ct = ComponentTheme::global(cx);
    let wanted = if keyboard { ring::color() } else { ct.colors.input };
    if ct.focus_ring == keyboard && ct.colors.ring == wanted {
        return;
    }
    let ct = ComponentTheme::global_mut(cx);
    ct.focus_ring = keyboard;
    ct.colors.ring = wanted;
}

/// Whether focus rings show now; for library buttons, whose focus line ignores the theme's ring colour.
pub fn rings(cx: &App) -> bool {
    cx.try_global::<FocusSeen>().is_some_and(|s| s.1)
}

fn note_focus(window: &Window, keyboard: bool, cx: &mut App) {
    let focused = window.focused(cx);
    let seen = cx.default_global::<FocusSeen>();
    let same = seen.0.as_ref().and_then(WeakFocusHandle::upgrade) == focused;
    if same && seen.1 {
        return;
    }
    let text = focused.is_some() && window.context_stack().iter().any(|c| c.contains(TEXT_FIELD));
    let seen = cx.default_global::<FocusSeen>();
    if same {
        seen.1 = text;
    } else {
        *seen = FocusSeen(focused.as_ref().map(FocusHandle::downgrade), keyboard || text);
    }
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
                ("tertiary on well", contrast(t.tertiary, t.well), 3.),
                ("secondary on well", contrast(t.secondary, t.well), 4.5),
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
