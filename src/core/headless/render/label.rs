//! Text in the world: name tags over entities and text displays, set in the game's font where and as the game
//! places them.

use super::assets::{Texture, Transparency};
use super::entity::{DisplayKind, Text};
use super::figure::{Affine, Painter, cross, sub};
use super::font::Font;
use super::raster::Vertex;
use crate::core::chat_text::Run;

const PIXEL: f32 = 0.025;
const LINE_HEIGHT: f32 = 10.0;
const MAX_NAME: usize = 256;
const MAX_CHARS: usize = 2048;
const MAX_LINES: usize = 64;
const MAX_LINE_WIDTH: i32 = 4096;
const BACKGROUND: u32 = 0x3F00_0000;
const HALF_WHITE: u32 = 0x7FFF_FFFF;

#[derive(Clone, Copy, PartialEq)]
enum Depth {
    Tested,
    Through,
}

/// Text found while drawing the entities, drawn once everything solid is in the picture so that its translucent
/// parts blend over what lies behind them.
pub enum Label<'a> {
    /// A name above an entity.
    Name {
        /// Where the name hangs, relative to the eye.
        at: [f32; 3],
        text: &'a Text,
        /// Sneaking: the name hides behind blocks and fades.
        discrete: bool,
        /// The lightmap colour where the entity stands.
        light: [f32; 3],
        /// The same with at least block light 2, for the solid copy of the name.
        glow: [f32; 3],
    },
    /// A text display in its own space `at`.
    Display { at: Affine, kind: &'a DisplayKind, light: [f32; 3] },
}

impl Label<'_> {
    fn origin(&self) -> [f32; 3] {
        match self {
            Label::Name { at, .. } => *at,
            Label::Display { at, .. } => at.t,
        }
    }
}

/// Draws `labels` farthest first: what hides behind blocks, then what shows through them.
pub fn draw(painter: &mut Painter, font: &Font, labels: &mut [Label<'_>]) {
    let far = |l: &Label<'_>| l.origin().iter().map(|v| v * v).sum::<f32>();
    labels.sort_by(|a, b| far(b).total_cmp(&far(a)));
    for depth in [Depth::Tested, Depth::Through] {
        for label in labels.iter() {
            match label {
                Label::Name { at, text, discrete, light, glow } => name(painter, font, *at, text, *discrete, *light, *glow, depth),
                Label::Display { at, kind, light } => display(painter, font, at, kind, *light, depth),
            }
        }
    }
}

/// A name turned to the camera; one not sneaking also shows faintly through everything in front of it.
#[allow(clippy::too_many_arguments)]
fn name(painter: &mut Painter, font: &Font, at: [f32; 3], text: &Text, discrete: bool, light: [f32; 3], glow: [f32; 3], pass: Depth) {
    let runs = clip(text, MAX_NAME, true);
    let w = font.width(&runs) as f32;
    if w <= 0.0 {
        return;
    }
    let (right, up) = (painter.view.right, painter.view.up);
    let place = |x: f32, y: f32| [0, 1, 2].map(|k| at[k] + (right[k] * x - up[k] * y) * PIXEL);
    let x = -w / 2.0;
    let back = [x - 1.0, -1.0, x + w, 9.0];
    match (discrete, pass) {
        (true, Depth::Tested) => {
            rect(painter, &place, back, BACKGROUND, light, pass);
            line(painter, font, &place, x, 0.0, &runs, HALF_WHITE, false, light, pass);
        }
        (false, Depth::Tested) => line(painter, font, &place, x, 0.0, &runs, 0xFFFF_FFFF, false, glow, pass),
        (false, Depth::Through) => {
            rect(painter, &place, back, BACKGROUND, light, pass);
            line(painter, font, &place, x, 0.0, &runs, HALF_WHITE, false, light, pass);
        }
        (true, Depth::Through) => {}
    }
}

/// A text display's background and lines, readable from its front only.
fn display(painter: &mut Painter, font: &Font, at: &Affine, kind: &DisplayKind, light: [f32; 3], pass: Depth) {
    let DisplayKind::Text { text, line_width, background, opacity, flags } = kind else { return };
    let depth = if flags & 0x02 != 0 { Depth::Through } else { Depth::Tested };
    if depth != pass {
        return;
    }
    let runs = clip(text, MAX_CHARS, false);
    let lines: Vec<Vec<Run>> = font.wrap(&runs, (*line_width).clamp(1, MAX_LINE_WIDTH)).into_iter().take(MAX_LINES).collect();
    let widths: Vec<f32> = lines.iter().map(|l| font.width(l) as f32).collect();
    let w = widths.iter().copied().fold(0.0, f32::max);
    let h = lines.len() as f32 * LINE_HEIGHT - 1.0;
    let space = at.scale([PIXEL, -PIXEL, PIXEL]).translate([1.0 - w / 2.0, -h, 0.0]);
    let place = |x: f32, y: f32| space.apply([x, y, 0.0]);
    let origin = place(0.0, 0.0);
    let front = cross(sub(place(0.0, 1.0), origin), sub(place(1.0, 0.0), origin));
    if front[0] * origin[0] + front[1] * origin[1] + front[2] * origin[2] >= 0.0 {
        return;
    }
    let background = if flags & 0x04 != 0 { BACKGROUND } else { *background };
    if background >> 24 != 0 {
        rect(painter, &place, [-1.0, -1.0, w, h], background, light, depth);
    }
    let alpha = if *opacity < 4 { 0xFF } else { u32::from(*opacity) };
    let color = alpha << 24 | 0xFF_FFFF;
    for (i, (runs, lw)) in lines.iter().zip(&widths).enumerate() {
        let x = match (flags & 0x08 != 0, flags & 0x10 != 0) {
            (true, _) => 0.0,
            (false, true) => w - lw,
            _ => w / 2.0 - lw / 2.0,
        };
        line(painter, font, &place, x, i as f32 * LINE_HEIGHT, runs, color, flags & 0x01 != 0, light, depth);
    }
}

/// At most `max` characters of `text`, on one line when `flat`.
fn clip(text: &Text, max: usize, flat: bool) -> Vec<Run> {
    let mut left = max;
    let mut out = Vec::new();
    for run in text {
        if left == 0 {
            break;
        }
        let part: String = run.text.chars().take(left).map(|c| if flat && c == '\n' { ' ' } else { c }).collect();
        left -= part.chars().count();
        out.push(Run { text: part, ..run.clone() });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn line(painter: &mut Painter, font: &Font, place: &impl Fn(f32, f32) -> [f32; 3], x: f32, y: f32, runs: &[Run], color: u32, shadow: bool, light: [f32; 3], depth: Depth) {
    let Some(image) = font.render(runs, color, shadow) else { return };
    let (w, side) = image.dimensions();
    let mode = if color >> 24 == 0xFF && depth == Depth::Tested { Transparency::Cutout } else { Transparency::Translucent };
    for x0 in (0..w).step_by(side as usize) {
        let tw = side.min(w - x0);
        let mut pixels = vec![[0u8; 4]; (side * side) as usize];
        for ty in 0..side {
            for tx in 0..tw {
                pixels[(ty * side + tx) as usize] = image.get_pixel(x0 + tx, ty).0;
            }
        }
        let texture = Texture { size: side, levels: vec![pixels] };
        let left = x + x0 as f32;
        quad(painter, place, [left, y, left + tw as f32, y + side as f32], [tw as f32 / side as f32, 1.0], &texture, light, mode, depth);
    }
}

fn rect(painter: &mut Painter, place: &impl Fn(f32, f32) -> [f32; 3], r: [f32; 4], argb: u32, light: [f32; 3], depth: Depth) {
    let texture = Texture { size: 1, levels: vec![vec![[(argb >> 16) as u8, (argb >> 8) as u8, argb as u8, (argb >> 24) as u8]]] };
    quad(painter, place, r, [1.0, 1.0], &texture, light, Transparency::Translucent, depth);
}

#[allow(clippy::too_many_arguments)]
fn quad(painter: &mut Painter, place: &impl Fn(f32, f32) -> [f32; 3], r: [f32; 4], uv: [f32; 2], texture: &Texture, light: [f32; 3], mode: Transparency, depth: Depth) {
    let corners = [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]];
    let uvs = [[0.0, 0.0], [uv[0], 0.0], [uv[0], uv[1]], [0.0, uv[1]]];
    let view = corners.map(|[x, y]| painter.view.to_view(place(x, y)));
    if !painter.target.visible(&view) {
        return;
    }
    let v = [0, 1, 2, 3].map(|i| Vertex { p: view[i], uv: uvs[i], c: light });
    match (depth, mode) {
        (Depth::Through, _) => painter.target.overlay(&v, texture),
        (Depth::Tested, Transparency::Translucent) => painter.target.blend(&v, texture, 1.0),
        (Depth::Tested, mode) => painter.target.quad(&v, texture, mode),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::headless::render::raster::{Fog, Target};
    use crate::core::headless::render::scene::View;

    #[test]
    fn a_name_in_the_open_is_nearly_white_on_a_faint_dark_band() {
        let jar = crate::core::paths::data_dir().join("versions/26.3/26.3.jar");
        let Ok(assets) = super::super::Assets::open(&jar) else { return };
        let font = assets.font.as_ref().unwrap();
        let mut target = Target::new(160, 40, 30.0, Fog { color: [0.0; 3], start: 100.0, end: 200.0, sphere: [1000.0, 2000.0] }, [0.0, 1.0, 0.0]);
        target.color.fill([0.0, 0.0, 1.0]);
        let view = View { eye: [0.0; 3], right: [1.0, 0.0, 0.0], up: [0.0, 1.0, 0.0], forward: [0.0, 0.0, 1.0] };
        let text = vec![Run { text: "Name".into(), ..Run::default() }];
        let mut labels = [Label::Name { at: [0.0, 0.0, 2.0], text: &text, discrete: false, light: [1.0; 3], glow: [1.0; 3] }];
        draw(&mut Painter { target: &mut target, view: &view }, font, &mut labels);
        let brightest = target.color.iter().map(|c| c[0]).fold(0.0, f32::max);
        let band = target.color.iter().map(|c| c[2]).fold(1.0, f32::min);
        assert!((brightest - 0.875).abs() < 0.02, "text {brightest}");
        assert!((band - 0.753).abs() < 0.02, "band {band}");
    }
}
