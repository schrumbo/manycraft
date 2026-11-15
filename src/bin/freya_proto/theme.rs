//! Drawing tokens for the Freya UI, built from a JSON theme's palette: colours, shape, spacing and elevation.

use std::cell::RefCell;

use freya::prelude::*;

use super::themes::{self, Density, Rgba, Signature, Token};

/// Spacing scale on a 4 pt grid.
pub mod space {
    pub const XXS: f32 = 2.;
    pub const XS: f32 = 4.;
    pub const S: f32 = 8.;
    pub const M: f32 = 12.;
    pub const L: f32 = 16.;
    pub const XL: f32 = 24.;
}

/// Fixed sizes of the shell around the content.
pub mod shell {
    pub const SIDEBAR: f32 = 232.;
    pub const TOOLBAR: f32 = 56.;
    pub const ROW: f32 = 30.;
    pub const SEARCH: f32 = 240.;
    pub const CONTROL: f32 = 28.;
    pub const CONTROL_L: f32 = 32.;
    pub const ICON: f32 = 16.;
    pub const ICON_S: f32 = 12.;
    pub const BADGE: f32 = 18.;
    pub const COLUMNS: usize = 4;
    pub const SHEET: f32 = 600.;
    pub const SHEET_TOP: f32 = 64.;
    pub const MOD_ROW: f32 = 52.;
    pub const MOD_ICON: f32 = 32.;
    pub const HERO_ICON: f32 = 48.;
    pub const SWITCH_W: f32 = 32.;
    pub const SWITCH_H: f32 = 18.;
    pub const FIELD_VERSION: f32 = 180.;
    pub const RING: f32 = 2.;
    pub const RING_GAP: f32 = 2.;
    pub const PROGRESS: f32 = 3.;
}

thread_local! {
    static FONT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The font family the shown theme asks for, tried before the system faces.
pub fn font() -> Option<String> {
    FONT.with(|f| f.borrow().clone())
}

/// Sets the font family of the shown theme.
pub fn set_font(font: Option<&str>) {
    FONT.with(|f| *f.borrow_mut() = font.map(str::to_string));
}

/// Corner radii.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub tile: f32,
    pub control: f32,
    pub field: f32,
    pub sheet: f32,
    pub chip: f32,
}

/// Text sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Type {
    pub title: f32,
    pub headline: f32,
    pub body: f32,
    pub callout: f32,
    pub caption: f32,
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
    pub content: Color,
    pub raised: Color,
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
    pub identity: Vec<Color>,
    /// Whether the theme names its own account colours.
    pub own_identity: bool,
    pub shape: Shape,
    pub kind: Type,
    pub elevation: Elevation,
    pub turf_band: bool,
    pub icon_stroke: f32,
    pub grid_gap: f32,
    pub grid_pad: f32,
    pub strip: f32,
    pub turf: f32,
    pub hidden_dim: f32,
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
        let (grid_gap, grid_pad, strip, icon_stroke) = match source.density {
            Density::Compact => (10., 20., 48., 1.5),
            Density::Comfortable => (14., 24., 50., 1.5),
            Density::Spacious => (18., 28., 52., 1.75),
        };
        let turf_band = source.signature == Signature::Turf;
        let attention = get(Token::Attention);
        Self {
            dark,
            surface: get(Token::Surface),
            sidebar: get(Token::Sidebar),
            content: get(Token::Raised),
            raised: get(Token::Control),
            well: get(Token::Sunken),
            text1: get(Token::Text1),
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
            identity: p.identity.iter().map(|&c| color(c)).collect(),
            own_identity: p.identity != themes::IDENTITY,
            shape: Shape { tile: 10. * r, control: 8. * r, field: 7. * r, sheet: 14. * r, chip: if r < 0.6 { 4. * r } else if turf_band { 6. * r } else { 999. } },
            kind: Type { title: 22., headline: 15., body: 13., callout: 12., caption: 11. },
            elevation: Elevation {
                card: (!dark).then(|| shadow(0., 1., 3., 0., color(p.get(Token::Text1)).with_af32(0.08))),
                float: if dark { shadow(0., 16., 40., 0., Color::BLACK.with_af32(0.55)) } else { shadow(0., 14., 36., 0., color(p.get(Token::Text1)).with_af32(0.18)) },
                rim: dark.then(|| color(p.get(Token::Text1)).with_af32(0.06)),
                glow: (source.signature == Signature::Glow).then(|| if dark { shadow(0., 0., 28., 2., attention.with_af32(0.42)) } else { shadow(0., 0., 22., 1., attention.with_af32(0.30)) }),
            },
            turf_band,
            icon_stroke,
            grid_gap,
            grid_pad,
            strip,
            turf: if turf_band { 4. } else { 0. },
            hidden_dim: 0.4,
        }
    }

    /// Text colour that reads on identity colour `fill`.
    pub fn ink_on(&self, fill: Color) -> Color {
        let luma = 0.299 * f32::from(fill.r()) + 0.587 * f32::from(fill.g()) + 0.114 * f32::from(fill.b());
        if luma > 150. { Color::from(0xFF15_130F) } else { Color::WHITE }
    }
}
