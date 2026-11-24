//! Drawing in the game's GUI units: whole pixels of a picture scaled by the integer GUI scale the game picks.

use image::RgbaImage;

use super::super::font::{Font, blend};
use crate::core::chat_text::Run;

/// The GUI scale the game picks on its own (option "Auto") for a window of this size.
pub fn gui_scale(width: u32, height: u32) -> u32 {
    let mut scale = 1;
    while width / (scale + 1) >= 320 && height / (scale + 1) >= 240 {
        scale += 1;
    }
    scale
}

/// A picture seen in GUI units.
pub struct Canvas<'a> {
    pub image: &'a mut RgbaImage,
    pub scale: u32,
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(image: &'a mut RgbaImage) -> Self {
        let scale = gui_scale(image.width(), image.height());
        let (width, height) = (image.width().div_ceil(scale) as i32, image.height().div_ceil(scale) as i32);
        Self { image, scale, width, height }
    }

    fn rect(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> (i32, i32, i32, i32) {
        let s = self.scale as i32;
        let (w, h) = (self.image.width() as i32, self.image.height() as i32);
        ((x0 * s).clamp(0, w), (y0 * s).clamp(0, h), (x1 * s).clamp(0, w), (y1 * s).clamp(0, h))
    }

    /// Fills a rectangle of GUI units with an ARGB colour.
    pub fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, argb: u32) {
        let (x0, y0, x1, y1) = self.rect(x0, y0, x1, y1);
        let src = [(argb >> 16) as u8, (argb >> 8) as u8, argb as u8, (argb >> 24) as u8];
        for y in y0..y1 {
            for x in x0..x1 {
                blend(self.image.get_pixel_mut(x as u32, y as u32), src);
            }
        }
    }

    /// Draws the part (`u`, `v`, `w`, `h`) of a sprite with its top left at (`x`, `y`), every sprite pixel a GUI unit,
    /// its pixels multiplied by `alpha`.
    pub fn sprite_part(&mut self, sprite: &RgbaImage, u: u32, v: u32, w: u32, h: u32, x: i32, y: i32, alpha: f32) {
        let s = self.scale as i32;
        let (iw, ih) = (self.image.width() as i32, self.image.height() as i32);
        for sy in 0..h.min(sprite.height().saturating_sub(v)) {
            for sx in 0..w.min(sprite.width().saturating_sub(u)) {
                let px = sprite.get_pixel(u + sx, v + sy).0;
                if px[3] == 0 {
                    continue;
                }
                let src = [px[0], px[1], px[2], (f32::from(px[3]) * alpha).round() as u8];
                let (x0, y0) = ((x + sx as i32) * s, (y + sy as i32) * s);
                for dy in y0.max(0)..(y0 + s).min(ih) {
                    for dx in x0.max(0)..(x0 + s).min(iw) {
                        blend(self.image.get_pixel_mut(dx as u32, dy as u32), src);
                    }
                }
            }
        }
    }

    /// Draws a whole sprite with its top left at (`x`, `y`).
    pub fn sprite(&mut self, sprite: &RgbaImage, x: i32, y: i32) {
        self.sprite_part(sprite, 0, 0, sprite.width(), sprite.height(), x, y, 1.0);
    }

    /// Draws a sprite the way the game draws its crosshair: inverting what is behind it.
    pub fn inverted(&mut self, sprite: &RgbaImage, x: i32, y: i32) {
        let s = self.scale as i32;
        let (iw, ih) = (self.image.width() as i32, self.image.height() as i32);
        for sy in 0..sprite.height() {
            for sx in 0..sprite.width() {
                let px = sprite.get_pixel(sx, sy).0;
                if px[3] == 0 {
                    continue;
                }
                let (x0, y0) = ((x + sx as i32) * s, (y + sy as i32) * s);
                for dy in y0.max(0)..(y0 + s).min(ih) {
                    for dx in x0.max(0)..(x0 + s).min(iw) {
                        let dst = self.image.get_pixel_mut(dx as u32, dy as u32);
                        for c in 0..3 {
                            let (a, b) = (f32::from(px[c]) / 255.0, f32::from(dst.0[c]) / 255.0);
                            dst.0[c] = ((a * (1.0 - b) + b * (1.0 - a)).clamp(0.0, 1.0) * 255.0).round() as u8;
                        }
                    }
                }
            }
        }
    }

    /// Draws text with its top left at (`x`, `y`) GUI units; `size` multiplies the font pixel.
    pub fn text(&mut self, font: &Font, x: i32, y: i32, size: u32, runs: &[Run], color: u32, shadow: bool) {
        let s = self.scale as i32;
        font.draw(self.image, x * s, y * s, self.scale * size, runs, color, shadow);
    }
}
