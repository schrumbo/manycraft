//! Drawing tokens of the Freya front end, built from a JSON theme's palette: colours, shape, type and elevation.
//! Sizes come from [`crate::core::scale`], the scale the GPUI front end draws with too.

use freya::prelude::*;

pub use crate::core::scale::{control, font, iconsize, motion, radius, space};
use crate::core::themes::{self, Density, Library, Rgba, Signature, Token};

/// Paint levels of the window-wide overlays; Freya paints nodes that share a level in no fixed order.
pub mod level {
    pub const SHEET: u8 = 2;
    pub const PALETTE: u8 = 3;
    pub const MENU: u8 = 4;
    pub const TOAST: u8 = 5;
    pub const TIP: u8 = 6;
}

/// Fixed sizes of the window's frame around the content.
pub mod frame {
    pub const SIDEBAR: f32 = 232.;
    pub const TOOLBAR: f32 = 56.;
    pub const ROW: f32 = 30.;
    pub const SEARCH: f32 = 220.;
    pub const SEARCH_NARROW: f32 = 160.;
    /// The content width below which the toolbar drops the tile size control, left to the palette and shortcuts.
    pub const TOOLBAR_FULL: f32 = 780.;
    /// The content width below which the search field narrows.
    pub const TOOLBAR_SEARCH: f32 = 540.;
    pub const BADGE: f32 = 18.;
    pub const SHEET: f32 = 560.;
    /// The width of a sheet that only asks or tells.
    pub const SHEET_NARROW: f32 = 448.;
    pub const SHEET_TOP: f32 = 72.;
    /// The width of a picker on a sheet, such as the Minecraft version or the world to install into.
    pub const PICK_W: f32 = 232.;
    /// The bars a loading row of the content browser shows in place of its name and its text, and their height.
    pub const BAR_NAME: f32 = 120.;
    pub const BAR_TEXT: f32 = 220.;
    pub const BAR_H: f32 = 10.;
    pub const PALETTE: f32 = 560.;
    pub const PALETTE_ROW: f32 = 36.;
    pub const PALETTE_ROWS: usize = 9;
    pub const MENU: f32 = 220.;
    pub const MENU_ROW: f32 = 26.;
    /// Rows a long menu shows before it scrolls.
    pub const MENU_ROWS: usize = 12;
    /// The content width from which the instance page shows its inspector beside the tabs.
    pub const INSPECTOR_SPLIT: f32 = 860.;
    pub const INSPECTOR: f32 = 300.;
    /// The widest the content of an instance tab grows.
    pub const CONTENT_MAX: f32 = 760.;
    pub const TOAST: f32 = 420.;
    pub const TOASTS: usize = 3;
    pub const TOOLTIP: f32 = 280.;
    pub const SWITCH_W: f32 = 32.;
    pub const SWITCH_H: f32 = 18.;
    pub const RING: f32 = 2.;
    pub const RING_GAP: f32 = 2.;
    pub const PROGRESS: f32 = 3.;
    /// The hairline that divides the rows of a group.
    pub const HAIRLINE: f32 = 1.;
    pub const FACE: f32 = 18.;
    pub const DOT: f32 = 7.;
    pub const NOTICE: f32 = 32.;
    /// How strongly the attention colour washes a notice strip.
    pub const NOTICE_WASH: f32 = 0.12;
    /// How strongly the accent washes the glyph tile of an entry.
    pub const TILE_WASH: f32 = 0.14;
    /// How long the waiting arc takes for one turn, in milliseconds.
    pub const SPIN_MS: u64 = 900;
}

/// The fonts the UI asks for, tried in order before the system's own.
pub mod faces {
    pub const BODY: [&str; 4] = ["Segoe UI Variable Text", "SF Pro Text", "Segoe UI", "Inter"];
    pub const DISPLAY: [&str; 4] = ["Segoe UI Variable Display", "SF Pro Display", "Segoe UI", "Inter"];
    pub const MONO: [&str; 4] = ["Cascadia Mono", "SF Mono", "Consolas", "Menlo"];
}

/// Corner radii, scaled by the theme's roundness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub tile: f32,
    pub control: f32,
    pub field: f32,
    pub sheet: f32,
    pub chip: f32,
    pub small: f32,
}

/// Shadows and rims that lift surfaces.
#[derive(Clone, Debug, PartialEq)]
pub struct Elevation {
    pub card: Option<Shadow>,
    pub float: Shadow,
    pub rim: Option<Color>,
    pub glow: Option<Shadow>,
}

/// The tokens one screen is drawn with.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub surface: Color,
    pub sidebar: Color,
    pub raised: Color,
    pub control: Color,
    pub well: Color,
    pub text1: Color,
    pub text2: Color,
    pub text3: Color,
    pub separator: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub on_accent: Color,
    pub attention: Color,
    pub attention_text: Color,
    pub on_attention: Color,
    pub danger: Color,
    pub danger_text: Color,
    pub on_danger: Color,
    pub success: Color,
    pub success_text: Color,
    pub focus: Color,
    pub tile_ring: Color,
    pub selection: Color,
    pub scrim: Color,
    /// Washes over a control on hover and press.
    pub hover: Color,
    pub press: Color,
    pub identity: Vec<Color>,
    pub shape: Shape,
    pub elevation: Elevation,
    pub grid_gap: f32,
    pub grid_pad: f32,
    pub icon_stroke: f32,
    pub hidden_dim: f32,
    pub font: Option<String>,
}

/// A theme colour as a Freya colour.
pub fn color(c: Rgba) -> Color {
    Color::from(u32::from_be_bytes([c.a, c.r, c.g, c.b]))
}

fn shadow(x: f32, y: f32, blur: f32, spread: f32, color: Color) -> Shadow {
    Shadow::from((x, y, blur, spread, color))
}

impl Theme {
    /// The tokens of `source` in the dark or light appearance, or in the only one it ships.
    pub fn new(source: &themes::Theme, dark: bool) -> Self {
        let dark = if source.has(dark) { dark } else { !dark };
        let p = source.palette(dark);
        let get = |t: Token| color(p.get(t));
        let r = source.radius;
        let (grid_gap, grid_pad, icon_stroke) = match source.density {
            Density::Compact => (space::S, space::L, 1.5),
            Density::Comfortable => (space::M, space::XL, 1.5),
            Density::Spacious => (space::L, space::XXL, 1.75),
        };
        let text1 = get(Token::Text1);
        let attention = get(Token::Attention);
        Self {
            dark,
            surface: get(Token::Surface),
            sidebar: get(Token::Sidebar),
            raised: get(Token::Raised),
            control: get(Token::Control),
            well: get(Token::Sunken),
            text1,
            text2: get(Token::Text2),
            text3: get(Token::Text3),
            separator: get(Token::Separator),
            accent: get(Token::Accent),
            accent_text: get(Token::AccentText),
            on_accent: get(Token::OnAccent),
            attention,
            attention_text: get(Token::AttentionText),
            on_attention: get(Token::OnAttention),
            danger: get(Token::Danger),
            danger_text: get(Token::DangerText),
            on_danger: get(Token::OnDanger),
            success: get(Token::Success),
            success_text: get(Token::SuccessText),
            focus: get(Token::FocusRing),
            tile_ring: get(Token::TileRing),
            selection: get(Token::Selection),
            scrim: get(Token::Scrim),
            hover: text1.with_af32(if dark { 0.07 } else { 0.05 }),
            press: text1.with_af32(if dark { 0.12 } else { 0.09 }),
            identity: p.identity.iter().map(|&c| color(c)).collect(),
            shape: Shape {
                tile: radius::CARD * r,
                control: radius::INNER * r,
                field: radius::INNER * r,
                sheet: (radius::CARD + space::XS) * r,
                chip: if r < 0.6 { radius::SMALL * r } else { radius::FULL },
                small: radius::SMALL * r,
            },
            elevation: Elevation {
                card: (!dark).then(|| shadow(0., 1., 3., 0., text1.with_af32(0.08))),
                float: if dark { shadow(0., 16., 40., 0., Color::BLACK.with_af32(0.55)) } else { shadow(0., 14., 36., 0., text1.with_af32(0.18)) },
                rim: dark.then(|| text1.with_af32(0.06)),
                glow: (source.signature == Signature::Glow).then(|| if dark { shadow(0., 0., 28., 2., attention.with_af32(0.42)) } else { shadow(0., 0., 22., 1., attention.with_af32(0.30)) }),
            },
            grid_gap,
            grid_pad,
            icon_stroke,
            hidden_dim: 0.4,
            font: source.font.clone(),
        }
    }

    /// The theme `id` of `library`, or the default theme, in the given appearance; `None` only when no theme parses.
    pub fn load(library: &Library, id: Option<&str>, dark: bool) -> Option<Self> {
        library.chosen(id).map(|source| Self::new(source, dark))
    }

    /// The font chain for body text: the theme's own font first.
    pub fn body(&self) -> Vec<String> {
        self.font.iter().cloned().chain(faces::BODY.iter().map(|f| (*f).to_string())).collect()
    }

    /// The font chain for titles.
    pub fn display(&self) -> Vec<String> {
        self.font.iter().cloned().chain(faces::DISPLAY.iter().map(|f| (*f).to_string())).collect()
    }

    /// The identity colour of account colour index `index`.
    pub fn identity(&self, index: usize) -> Color {
        self.identity.get(index % self.identity.len().max(1)).copied().unwrap_or(self.accent)
    }

    /// Text colour that reads on identity colour `fill`.
    pub fn ink_on(&self, fill: Color) -> Color {
        let luma = 0.299 * f32::from(fill.r()) + 0.587 * f32::from(fill.g()) + 0.114 * f32::from(fill.b());
        if luma > 150. { Color::from(0xFF15_130F) } else { Color::WHITE }
    }
}
