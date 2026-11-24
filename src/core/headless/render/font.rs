//! The game's own text: bitmap fonts from the client jar's font definitions, with colours, shadows, bold, italic,
//! underline and strike-through as the game draws them.

use std::collections::HashMap;

use image::{Rgba, RgbaImage};
use serde_json::Value;

use crate::core::chat_text::Run;

/// How tall a line of text is, in font pixels.
pub const LINE: i32 = 9;

const MAX_INCLUDE: usize = 8;

struct Glyph {
    sheet: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    advance: f32,
    up: f32,
    scale: f32,
}

/// The glyphs of the default font.
pub struct Font {
    sheets: Vec<RgbaImage>,
    glyphs: HashMap<char, Glyph>,
    spaces: HashMap<char, f32>,
}

fn strip(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

impl Font {
    /// Reads the default font; `read` returns the bytes of a file below `assets/minecraft/`.
    pub fn load(mut read: impl FnMut(&str) -> Option<Vec<u8>>) -> Option<Font> {
        let mut font = Font { sheets: Vec::new(), glyphs: HashMap::new(), spaces: HashMap::new() };
        let mut providers = Vec::new();
        let root: Value = serde_json::from_slice(&read("font/default.json")?).ok()?;
        font.collect(&root, &mut read, &mut providers, 0);
        let mut loaded: HashMap<String, usize> = HashMap::new();
        for p in providers {
            match p.get("type").and_then(Value::as_str) {
                Some("space") => {
                    if let Some(adv) = p.get("advances").and_then(Value::as_object) {
                        for (k, v) in adv {
                            if let (Some(c), Some(a)) = (k.chars().next(), v.as_f64()) {
                                font.spaces.entry(c).or_insert(a as f32);
                            }
                        }
                    }
                }
                Some("bitmap") => {
                    let Some(file) = p.get("file").and_then(Value::as_str) else { continue };
                    let file = strip(file).replacen(':', "/", 1);
                    let path = format!("textures/{file}");
                    let sheet = match loaded.get(&path) {
                        Some(i) => *i,
                        None => {
                            let Some(bytes) = read(&path) else { continue };
                            let Ok(img) = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png) else { continue };
                            font.sheets.push(img.into_rgba8());
                            loaded.insert(path, font.sheets.len() - 1);
                            font.sheets.len() - 1
                        }
                    };
                    font.add_bitmap(sheet, &p);
                }
                _ => {}
            }
        }
        (!font.glyphs.is_empty()).then_some(font)
    }

    fn collect(&self, def: &Value, read: &mut impl FnMut(&str) -> Option<Vec<u8>>, out: &mut Vec<Value>, depth: usize) {
        let Some(list) = def.get("providers").and_then(Value::as_array) else { return };
        for p in list {
            if p.get("type").and_then(Value::as_str) == Some("reference") {
                if depth >= MAX_INCLUDE || p.get("filter").and_then(|f| f.get("uniform")).and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let Some(id) = p.get("id").and_then(Value::as_str) else { continue };
                if let Some(inner) = read(&format!("font/{}.json", strip(id))).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                    self.collect(&inner, read, out, depth + 1);
                }
            } else {
                out.push(p.clone());
            }
        }
    }

    fn add_bitmap(&mut self, sheet: usize, p: &Value) {
        let Some(rows) = p.get("chars").and_then(Value::as_array) else { return };
        let img = &self.sheets[sheet];
        let height = p.get("height").and_then(Value::as_f64).unwrap_or(8.0) as f32;
        let ascent = p.get("ascent").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let nrows = rows.len() as u32;
        let ncols = rows.iter().filter_map(|r| r.as_str()).map(|r| r.chars().count()).max().unwrap_or(0) as u32;
        if nrows == 0 || ncols == 0 {
            return;
        }
        let (cw, ch) = (img.width() / ncols, img.height() / nrows);
        if cw == 0 || ch == 0 {
            return;
        }
        let scale = height / ch as f32;
        for (r, row) in rows.iter().filter_map(Value::as_str).enumerate() {
            for (c, chr) in row.chars().enumerate() {
                if chr == '\0' || self.glyphs.contains_key(&chr) || self.spaces.contains_key(&chr) {
                    continue;
                }
                let (x0, y0) = (c as u32 * cw, r as u32 * ch);
                let used = (0..cw).rev().find(|&dx| (0..ch).any(|dy| img.get_pixel(x0 + dx, y0 + dy).0[3] != 0)).map_or(0, |dx| dx + 1);
                let advance = (0.5 + f64::from(used as f32 * scale)).floor() as f32 + 1.0;
                self.glyphs.insert(chr, Glyph { sheet, x: x0, y: y0, w: cw, h: ch, advance, up: 7.0 - ascent, scale });
            }
        }
    }

    fn advance(&self, c: char, bold: bool) -> f32 {
        let base = match self.spaces.get(&c) {
            Some(a) => *a,
            None => self.glyphs.get(&c).map_or(6.0, |g| g.advance),
        };
        if bold && base > 0.0 { base + 1.0 } else { base }
    }

    /// The width of the widest line of `runs`, in font pixels.
    pub fn width(&self, runs: &[Run]) -> i32 {
        let mut widest = 0.0f32;
        let mut line = 0.0f32;
        for run in runs {
            for c in run.text.chars() {
                if c == '\n' {
                    widest = widest.max(line);
                    line = 0.0;
                } else {
                    line += self.advance(c, run.bold);
                }
            }
        }
        widest.max(line).ceil() as i32
    }

    /// The width of `text` in the plain style.
    pub fn text_width(&self, text: &str) -> i32 {
        self.width(&[Run { text: text.to_owned(), ..Run::default() }])
    }

    /// How many lines `runs` takes.
    pub fn lines(&self, runs: &[Run]) -> usize {
        1 + runs.iter().map(|r| r.text.matches('\n').count()).sum::<usize>()
    }

    /// Splits `runs` into lines no wider than `max` font pixels, at spaces where there are any.
    pub fn wrap(&self, runs: &[Run], max: i32) -> Vec<Vec<Run>> {
        let mut out: Vec<Vec<Run>> = vec![Vec::new()];
        let mut width = 0.0f32;
        let mut word: Vec<(char, Run)> = Vec::new();
        let mut word_width = 0.0f32;
        fn push(line: &mut Vec<Run>, c: char, style: &Run) {
            match line.last_mut() {
                Some(last) if last.color == style.color && last.bold == style.bold && last.italic == style.italic && last.underline == style.underline && last.strike == style.strike => last.text.push(c),
                _ => line.push(Run { text: c.to_string(), ..style.clone() }),
            }
        }
        let flush = |out: &mut Vec<Vec<Run>>, width: &mut f32, word: &mut Vec<(char, Run)>, word_width: &mut f32| {
            if *width + *word_width > max as f32 && *width > 0.0 {
                out.push(Vec::new());
                *width = 0.0;
            }
            for (c, style) in word.drain(..) {
                push(out.last_mut().expect("line"), c, &style);
            }
            *width += *word_width;
            *word_width = 0.0;
        };
        for run in runs {
            let style = Run { text: String::new(), ..run.clone() };
            for c in run.text.chars() {
                if c == '\n' {
                    flush(&mut out, &mut width, &mut word, &mut word_width);
                    out.push(Vec::new());
                    width = 0.0;
                    continue;
                }
                let a = self.advance(c, run.bold);
                word.push((c, style.clone()));
                word_width += a;
                if c == ' ' {
                    flush(&mut out, &mut width, &mut word, &mut word_width);
                } else if word_width > max as f32 {
                    let last = word.pop().expect("char");
                    word_width -= a;
                    flush(&mut out, &mut width, &mut word, &mut word_width);
                    out.push(Vec::new());
                    width = 0.0;
                    word.push(last);
                    word_width = a;
                }
            }
        }
        flush(&mut out, &mut width, &mut word, &mut word_width);
        out
    }

    /// Draws `runs` with the top left of the first line at (`x`, `y`) in picture pixels, each font pixel `px` picture
    /// pixels wide. `color` (ARGB) is the colour of runs without one and gives the alpha of all of them; `shadow`
    /// adds the game's dark copy one font pixel down and right.
    pub fn draw(&self, image: &mut RgbaImage, x: i32, y: i32, px: u32, runs: &[Run], color: u32, shadow: bool) {
        if shadow {
            self.pass(image, x, y, px, runs, color, true);
        }
        self.pass(image, x, y, px, runs, color, false);
    }

    fn pass(&self, image: &mut RgbaImage, x: i32, y: i32, px: u32, runs: &[Run], color: u32, shadow: bool) {
        let unit = px as f32;
        let alpha = (color >> 24) as u8;
        let (mut cx, mut line) = (0.0f32, 0i32);
        let offset = if shadow { 1.0 } else { 0.0 };
        for run in runs {
            let rgb = run.color.unwrap_or(color & 0xFF_FFFF);
            let rgb = if shadow { ((rgb >> 16 & 0xFF) / 4) << 16 | ((rgb >> 8 & 0xFF) / 4) << 8 | (rgb & 0xFF) / 4 } else { rgb };
            let paint = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, alpha];
            for c in run.text.chars() {
                if c == '\n' {
                    cx = 0.0;
                    line += 1;
                    continue;
                }
                let advance = self.advance(c, run.bold);
                let top = (line * LINE) as f32;
                let origin = |dx: f32| x + ((cx + offset + dx) * unit).round() as i32;
                if let Some(g) = self.glyphs.get(&c) {
                    let sheet = &self.sheets[g.sheet];
                    let passes = if run.bold { 2 } else { 1 };
                    for pass in 0..passes {
                        let gx = origin(pass as f32);
                        let gy_top = top + g.up + offset;
                        for row in 0..g.h {
                            let rows_top = y + ((gy_top + row as f32 * g.scale) * unit).round() as i32;
                            let rows_bottom = y + ((gy_top + (row + 1) as f32 * g.scale) * unit).round() as i32;
                            for col in 0..g.w {
                                if sheet.get_pixel(g.x + col, g.y + row).0[3] == 0 {
                                    continue;
                                }
                                let x0 = gx + (col as f32 * g.scale * unit).round() as i32;
                                let x1 = gx + ((col + 1) as f32 * g.scale * unit).round() as i32;
                                for yy in rows_top..rows_bottom {
                                    let shear = if run.italic { (1.0 - 0.25 * ((yy - y) as f32 / unit - (line * LINE) as f32)) * unit } else { 0.0 };
                                    let s = shear.round() as i32;
                                    fill(image, x0 + s, yy, x1 + s, yy + 1, paint);
                                }
                            }
                        }
                    }
                } else if !self.spaces.contains_key(&c) && !c.is_whitespace() {
                    let gx = origin(0.0);
                    let gy = y + ((top + offset) * unit).round() as i32;
                    let (w, h) = ((5.0 * unit) as i32, (8.0 * unit) as i32);
                    let t = unit as i32;
                    fill(image, gx, gy, gx + w, gy + t, paint);
                    fill(image, gx, gy + h - t, gx + w, gy + h, paint);
                    fill(image, gx, gy, gx + t, gy + h, paint);
                    fill(image, gx + w - t, gy, gx + w, gy + h, paint);
                }
                let (x0, x1) = (origin(0.0), x + ((cx + offset + advance) * unit).round() as i32);
                if run.underline {
                    let y0 = y + ((top + 8.0 + offset) * unit).round() as i32;
                    fill(image, x0, y0, x1, y0 + px as i32, paint);
                }
                if run.strike {
                    let y0 = y + ((top + 3.5 + offset) * unit).round() as i32;
                    fill(image, x0, y0, x1, y0 + px as i32, paint);
                }
                cx += advance;
            }
        }
    }

    /// Draws `runs` into a new transparent picture, one picture pixel per font pixel with a pixel of room for the
    /// shadow; `None` when there is nothing to draw.
    pub fn render(&self, runs: &[Run], color: u32, shadow: bool) -> Option<RgbaImage> {
        let (w, h) = (self.width(runs) + 1, self.lines(runs) as i32 * LINE + 1);
        if w <= 1 {
            return None;
        }
        let mut image = RgbaImage::new(w as u32, h as u32);
        self.draw(&mut image, 0, 0, 1, runs, color, shadow);
        Some(image)
    }
}

/// Puts `src` (straight alpha) over every pixel of the rectangle, clipped to the picture.
pub fn fill(image: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, src: [u8; 4]) {
    let (w, h) = (image.width() as i32, image.height() as i32);
    let (x0, x1, y0, y1) = (x0.clamp(0, w), x1.clamp(0, w), y0.clamp(0, h), y1.clamp(0, h));
    if src[3] == 0 {
        return;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            blend(image.get_pixel_mut(x as u32, y as u32), src);
        }
    }
}

/// Puts `src` (straight alpha) over `dst`.
#[inline]
pub fn blend(dst: &mut Rgba<u8>, src: [u8; 4]) {
    let a = u32::from(src[3]);
    if a == 255 {
        *dst = Rgba([src[0], src[1], src[2], 255.max(dst.0[3])]);
        return;
    }
    let inv = 255 - a;
    for c in 0..3 {
        dst.0[c] = ((u32::from(src[c]) * a + u32::from(dst.0[c]) * inv + 127) / 255) as u8;
    }
    dst.0[3] = (a + u32::from(dst.0[3]) * inv / 255).min(255) as u8;
}
