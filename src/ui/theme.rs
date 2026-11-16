use std::sync::Mutex;

use gpui_kit::component::{Theme as ComponentTheme, ThemeMode};
use gpui_kit::{App, FocusHandle, Global, Hsla, WeakFocusHandle, Window, WindowAppearance, black, px, rgb, white};

pub use crate::core::scale::{control, font, iconsize, radius, space};

/// The instance grid and its tiles.
pub mod tile {
    /// The smallest tile; below it the grid stops shrinking and scrolls.
    pub const MIN_W: f32 = 192.;
    pub const MIN_H: f32 = 108.;
    /// The shape tiles stay near, and the shape of a game's picture.
    pub const ASPECT: f32 = 16. / 9.;
    /// The line of window colour between tiles.
    pub const HAIRLINE: f32 = 1.;
    /// Below this width the overlay drops the status word and keeps the dot.
    pub const WORDLESS: f32 = 240.;
    /// Height of the overlay strip at the top of a tile.
    pub const BAR: f32 = 28.;
    /// Height of the line under the strip that says why an account dropped.
    pub const REASON: f32 = 16.;
    /// Height of the dark gradient that carries the overlay strip.
    pub const BAR_SHADE: f32 = 44.;
    /// The account chip in the overlay strip.
    pub const CHIP: f32 = 20.;
    /// The colour around a skin face on an account chip.
    pub const CHIP_RIM: f32 = 2.;
    /// Above this brightness (0–255) a chip's letter is dark, below it light.
    pub const CHIP_LUMA: f32 = 150.;
    /// Space around the grid's headings, the same as the toolbar's edges.
    pub const INSET: f32 = 16.;
    /// Width of the selection, hover and attention outline inside a tile.
    pub const RING: f32 = 2.;
    /// Width of the history popover a tile opens with Space.
    pub const HISTORY: f32 = 300.;
    /// Events the history popover lists.
    pub const HISTORY_EVENTS: usize = 5;
    /// Width of the time column in the history popover.
    pub const HISTORY_WHEN: f32 = 92.;
    /// How bright the preview stays behind an account that needs work.
    pub const DIMMED: f32 = 0.7;
    /// The status dot in the overlay strip.
    pub const GLYPH: f32 = 7.;
    /// Height of the shade that carries the actions at the bottom of a hovered preview.
    pub const SHADE: f32 = 56.;
    /// Unread whispers badge.
    pub const BADGE: f32 = 18.;
    /// Height of a batch heading in the grid of all batches.
    pub const HEADING: f32 = 40.;
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
    /// Height of the row of attention kinds under the toolbar.
    pub const NEEDS: f32 = 44.;
    pub const ROW: f32 = 28.;
    /// Widths the toolbar's search field steps down through as room shrinks; only the first keeps the full placeholder.
    pub const SEARCH: f32 = 240.;
    pub const SEARCH_MID: f32 = 180.;
    pub const SEARCH_MIN: f32 = 120.;
    /// The narrowest the search field squeezes to when even the smallest toolbar does not fit.
    pub const SEARCH_FLOOR: f32 = 80.;
    /// Average glyph width in em, used to estimate toolbar text without shaping it.
    pub const GLYPH: f32 = 0.55;
    /// The attention dot in a sidebar row.
    pub const DOT: f32 = 6.;
    /// The attention count in a sidebar row.
    pub const BADGE: f32 = 19.;
    /// The small button at the end of a sidebar row.
    pub const PILL: f32 = 24.;
    /// Below this content width the selection bar shows its buttons as icons.
    pub const COMPACT: f32 = 860.;
    /// Width the toolbar title keeps before anything else shrinks.
    pub const TITLE: f32 = 96.;
    /// Below this window width the sidebar floats over the grid instead of beside it.
    pub const NARROW: f32 = 880.;
    /// The widest an undo notice gets.
    pub const TOAST: f32 = 440.;
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
    /// Share of a row an entry's name may take before its detail.
    pub const LABEL: f32 = 0.6;
    /// Height of the line that says nothing matches.
    pub const EMPTY: f32 = 48.;
    /// Width of the shortcut column, so shortcuts line up on the right.
    pub const KEYS: f32 = 96.;
}

/// Several selected tiles and the bar that acts on them.
pub mod selection {
    /// Height of the floating bar.
    pub const BAR: f32 = 44.;
    /// Space between the bar and the bottom of the grid.
    pub const BOTTOM: f32 = 16.;
    /// Width of the message field in the bar.
    pub const FIELD: f32 = 300.;
    /// Blur and drop of the bar's shadow, a little deeper than a menu's.
    pub const SHADOW_BLUR: f32 = 24.;
    pub const SHADOW_Y: f32 = 8.;
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
    /// Height of the composer under the lines, with its hint line.
    pub const COMPOSER: f32 = 88.;
    /// Width of the time at the start of a line.
    pub const TIME: f32 = 44.;
    /// Space under the last line, and the gap between it and the jump button.
    pub const INSET: f32 = 12.;
    /// The most characters shown of a line before it is cut off.
    pub const SHOWN_CHARS: usize = 600;
}

/// The skin sheet.
pub mod wardrobe {
    /// Points per skin pixel in the front and back preview.
    pub const PREVIEW: f32 = 5.;
    /// Points per skin pixel in a recent skin.
    pub const RECENT: f32 = 2.;
    /// Points per cape pixel in a cape.
    pub const CAPE: f32 = 2.5;
    /// Image pixels per point, so the pixel art stays sharp on high-density screens.
    pub const DENSITY: f32 = 2.;
    /// Width of a cape choice.
    pub const CHOICE: f32 = 88.;
    /// Border width that marks the chosen skin or cape.
    pub const PICK: f32 = 2.;
    /// Width of the column with the preview.
    pub const COLUMN: f32 = 200.;
    /// Height of the body while the profile loads or failed, so the sheet keeps its size.
    pub const BODY: f32 = 260.;
}

/// The Welten, Screenshots and Protokolle tabs of the instance page.
pub mod files {
    /// Height of a world row.
    pub const WORLD_ROW: f32 = 52.;
    /// Height of a snapshot row under the worlds.
    pub const SNAPSHOT_ROW: f32 = 44.;
    /// The tallest the Papierkorb list gets before it scrolls.
    pub const TRASH_LIST: f32 = 360.;
    /// Side of a world icon.
    pub const WORLD_ICON: f32 = 36.;
    /// Pixels a world icon is decoded at.
    pub const WORLD_ICON_PX: u32 = 72;
    /// Narrowest a screenshot cell gets before the grid drops a column.
    pub const SHOT_MIN: f32 = 168.;
    /// Height of the caption under a screenshot.
    pub const SHOT_CAPTION: f32 = 22.;
    /// Pixels a screenshot thumbnail is decoded at, on its long side.
    pub const THUMB_PX: u32 = 256;
    /// Decoded screenshot thumbnails kept in memory.
    pub const THUMBS_KEPT: usize = 160;
    /// Thumbnails decoded at once.
    pub const DECODERS: usize = 4;
    /// Width of the log file picker.
    pub const LOG_PICKER: f32 = 220.;
    /// Width of the account picker of the logs.
    pub const ACCOUNT_PICKER: f32 = 160.;
    /// Width of the search field of the logs.
    pub const FIND_FIELD: f32 = 220.;
    /// Lines shown of a log, the newest; older ones stay in the file.
    pub const LOG_LINES: usize = 200_000;
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
    /// The aqua fill behind a selected row.
    pub const SELECTED: f32 = 0.22;
    /// The accent wash over the window while files are dragged onto it.
    pub const DROP: f32 = 0.1;
    /// The shade behind the floating sidebar.
    pub const SCRIM: f32 = 0.28;
    /// The bottom of the shade under a hovered preview's actions, and the top of the shade behind a tile's overlay strip.
    pub const PREVIEW_SHADE: f32 = 0.55;
    /// The dark letter on a light account chip.
    pub const CHIP_INK: f32 = 0.8;
    /// The status word over a preview.
    pub const OVERLAY_TEXT: f32 = 0.8;
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
    /// The light edge along the top of a floating bar.
    pub const RIM: f32 = 0.12;
    /// The shadow under a floating bar.
    pub const FLOAT_SHADOW: f32 = 0.24;
    /// The soft shadow that lifts a small badge off a preview.
    pub const BADGE_SHADOW: f32 = 0.35;
    /// How much of the selected segment's fill a hovered segment shows; pressing doubles it, still short of selected.
    pub const SEGMENT_HOVER: f32 = 0.4;
}

/// The edge that keeps a small badge readable over any preview.
pub mod edge {
    /// Width of the ring in the window colour.
    pub const RING: f32 = 1.;
    /// Blur of the soft shadow under it.
    pub const BLUR: f32 = 3.;
}

/// The ring the keyboard focus draws around a control.
pub mod ring {
    use std::sync::RwLock;

    use gpui_kit::Hsla;

    /// Width of the ring outside the control.
    pub const WIDTH: f32 = 3.;
    /// Width of the ring inside a control without a fill, where a ring outside would fill it.
    pub const INSET: f32 = 2.;

    static ACCENT: RwLock<Hsla> = RwLock::new(Hsla { h: 0.57, s: 1., l: 0.31, a: 1. });

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
    /// The aqua behind selected rows and chips; text and icons on it use `accent_text`.
    pub highlight: Hsla,
    /// The ring around the selected tiles, in place of the accent ring.
    pub selection_ring: Hsla,
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
    /// The track of a switch that is off.
    pub track: Hsla,
    /// The selected segment of a segmented control, raised above the bar.
    pub segment: Hsla,
}

impl Global for Theme {}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// `color` with its opacity set to `alpha`.
/// Account colour `index` (into `core::account::COLORS`) as a fill.
pub fn account_fill(index: u8) -> Hsla {
    let colors = crate::core::account::COLORS;
    rgb(colors[usize::from(index) % colors.len()]).into()
}

pub fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

impl Theme {
    /// The light or dark palette.
    pub fn plain(dark: bool) -> Self {
        let ink = if dark { white() } else { black() };
        Self {
            dark,
            window: if dark { c(0x0E1B2C) } else { c(0xF3F9FC) },
            content: if dark { c(0x14263D) } else { c(0xFFFFFF) },
            raised: if dark { c(0x1B365D) } else { c(0xE4F0F7) },
            well: if dark { c(0x0A1624) } else { c(0xEAF3F8) },
            primary: if dark { c(0xE8F1F8) } else { c(0x0E1B2C) },
            secondary: if dark { c(0x9DB4CA) } else { c(0x455D75) },
            tertiary: if dark { c(0x7D95AE) } else { c(0x586F85) },
            separator: if dark { c(0x9DB4CA).opacity(0.18) } else { c(0x0E1B2C).opacity(0.12) },
            accent: if dark { c(0x60C1E5) } else { c(0x005A9C) },
            accent_text: if dark { c(0xA0E0EF) } else { c(0x005A9C) },
            on_accent: if dark { c(0x0E1B2C) } else { c(0xFFFFFF) },
            highlight: c(0x60C1E5),
            selection_ring: if dark { c(0x60C1E5) } else { c(0x005A9C) },
            attention: if dark { c(0xFFB020) } else { c(0xC77800) },
            attention_text: if dark { c(0xFFB020) } else { c(0x8A5200) },
            on_attention: c(0x0E1B2C),
            success: if dark { c(0x5FD068) } else { c(0x2E8B3E) },
            success_text: if dark { c(0x5FD068) } else { c(0x1E7434) },
            danger: if dark { c(0xFF6F61) } else { c(0xD6382B) },
            danger_text: if dark { c(0xFF8A7E) } else { c(0xB72A1F) },
            on_danger: if dark { c(0x0E1B2C) } else { c(0xFFFFFF) },
            ink,
            shade: black().opacity(tint::SHADE),
            material: if dark { c(0x1B365D) } else { c(0xFFFFFF) },
            track: if dark { c(0xE8F1F8).opacity(0.4) } else { c(0x0E1B2C).opacity(0.5) },
            segment: if dark { c(0x2C4F7C) } else { c(0xFFFFFF) },
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
    k.selection = t.highlight.opacity(0.3);
    k.caret = t.accent;
    k.link = t.accent_text;
    k.link_hover = t.accent_text.opacity(0.85);
    k.link_active = t.accent_text.opacity(0.7);
    k.switch = t.track;
    k.switch_thumb = white();
    k.list_active = t.highlight.opacity(tint::SELECTED);
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

    fn selected(t: &Theme, under: Hsla) -> Hsla {
        let r = over(with_alpha(t.highlight, tint::SELECTED), under);
        Hsla::from(r)
    }

    fn hovered(t: &Theme) -> Hsla {
        Hsla::from(over(with_alpha(t.segment, tint::SEGMENT_HOVER), t.raised))
    }

    fn ring_on(t: &Theme) -> Hsla {
        with_alpha(t.accent, tint::RING)
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
                ("accent text on window", contrast(t.accent_text, t.window), 4.5),
                ("attention text on window", contrast(t.attention_text, t.window), 4.5),
                ("success text on window", contrast(t.success_text, t.window), 4.5),
                ("danger text on window", contrast(t.danger_text, t.window), 4.5),
                ("accent on window", contrast(t.accent, t.window), 3.),
                ("primary on raised", contrast(t.primary, t.raised), 4.5),
                ("secondary on raised", contrast(t.secondary, t.raised), 4.5),
                ("tertiary on raised", contrast(t.tertiary, t.raised), 3.),
                ("tertiary on content", contrast(t.tertiary, t.content), 3.),
                ("accent text on content", contrast(t.accent_text, t.content), 4.5),
                ("accent text on raised", contrast(t.accent_text, t.raised), 4.5),
                ("accent on content", contrast(t.accent, t.content), 3.),
                ("on attention", contrast(t.on_attention, t.attention), 4.5),
                ("attention on window", contrast(t.attention, t.window), 3.),
                ("attention text on content", contrast(t.attention_text, t.content), 4.5),
                ("success on window", contrast(t.success, t.window), 3.),
                ("success text on content", contrast(t.success_text, t.content), 4.5),
                ("danger on window", contrast(t.danger, t.window), 3.),
                ("danger text on content", contrast(t.danger_text, t.content), 4.5),
                ("on danger", contrast(t.on_danger, t.danger), 4.5),
                ("accent text on selected", contrast(t.accent_text, selected(&t, t.window)), 4.5),
                ("accent text on selected material", contrast(t.accent_text, selected(&t, t.material)), 4.5),
                ("switch track on window", contrast(t.track, t.window), 3.),
                ("switch track on content", contrast(t.track, t.content), 3.),
                ("switch track on raised", contrast(t.track, t.raised), 3.),
                ("primary on segment", contrast(t.primary, t.segment), 4.5),
                ("attention text on segment", contrast(t.attention_text, t.segment), 4.5),
                ("secondary on hovered segment", contrast(t.secondary, hovered(&t)), 4.5),
                ("accent ring on window", contrast(ring_on(&t), t.window), 3.),
            ];
            for (name, ratio, least) in checks {
                assert!(ratio >= least, "{name} ({}): {ratio:.2} < {least}", if dark { "dark" } else { "light" });
            }
        }
    }

    #[test]
    fn segments_rise_from_selected_over_hovered_to_the_bar() {
        for dark in [false, true] {
            let t = Theme::plain(dark);
            let (bar, hover, selected) = (luminance(Rgba::from(t.raised)), luminance(Rgba::from(hovered(&t))), luminance(Rgba::from(t.segment)));
            assert!(bar < hover && hover < selected, "{}: bar {bar:.3} hover {hover:.3} selected {selected:.3}", if dark { "dark" } else { "light" });
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
