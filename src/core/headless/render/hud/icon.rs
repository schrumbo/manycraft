//! Item icons as the inventory draws them: flat layers for generated models, the model's GUI display transform and
//! light for block models.

use image::RgbaImage;
use serde_json::Value;

use super::super::font::blend;
use super::super::model::{self, Quad, Source, Variant};
use super::Stack;
use super::art::Art;

const MAX_DEPTH: usize = 8;
const MAX_PARENTS: usize = 16;
const GRASS_DEFAULT: u32 = 0x7CBD6B;
const FOLIAGE_DEFAULT: u32 = 0x59AE30;
const GLINT_OFFSET: [f32; 2] = [38.0, 21.0];
const GLINT_STRENGTH: f32 = 0.75;

struct Layer {
    model: String,
    tints: Vec<[f32; 3]>,
}

fn strip(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn rgb(color: u32) -> [f32; 3] {
    [(color >> 16 & 0xFF) as f32 / 255.0, (color >> 8 & 0xFF) as f32 / 255.0, (color & 0xFF) as f32 / 255.0]
}

fn int(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_i64().map(|c| (c & 0xFF_FFFF) as u32),
        Value::Array(a) if a.len() >= 3 => {
            let c = |i: usize| (a[i].as_f64().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u32;
            Some(c(0) << 16 | c(1) << 8 | c(2))
        }
        _ => None,
    }
}

fn colormap(art: &Art, name: &str, temperature: f64, downfall: f64, fallback: u32) -> [f32; 3] {
    let Some(map) = art.texture(name) else { return rgb(fallback) };
    let (t, d) = (temperature.clamp(0.0, 1.0), downfall.clamp(0.0, 1.0) * temperature.clamp(0.0, 1.0));
    let (x, y) = (((1.0 - t) * 255.0) as u32, ((1.0 - d) * 255.0) as u32);
    let px = map.get_pixel(x.min(map.width() - 1), y.min(map.height() - 1)).0;
    rgb(u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2]))
}

fn tints(art: &Art, node: &Value, stack: &Stack) -> Vec<[f32; 3]> {
    let Some(list) = node.get("tints").and_then(Value::as_array) else { return Vec::new() };
    list.iter()
        .map(|t| {
            let default = t.get("default").and_then(int);
            match t.get("type").and_then(Value::as_str).map(strip) {
                Some("constant") => t.get("value").and_then(int).map_or([1.0; 3], rgb),
                Some("dye") => stack.dye.or(default).map_or([1.0; 3], rgb),
                Some("grass") => colormap(art, "colormap/grass", t.get("temperature").and_then(Value::as_f64).unwrap_or(0.5), t.get("downfall").and_then(Value::as_f64).unwrap_or(1.0), GRASS_DEFAULT),
                Some("foliage") => colormap(art, "colormap/foliage", 0.8, 0.4, FOLIAGE_DEFAULT),
                _ => default.map_or([1.0; 3], rgb),
            }
        })
        .collect()
}

fn resolve(art: &Art, node: &Value, stack: &Stack, out: &mut Vec<Layer>, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    let next = |n: Option<&Value>, out: &mut Vec<Layer>| {
        if let Some(n) = n {
            resolve(art, n, stack, out, depth + 1);
        }
    };
    match node.get("type").and_then(Value::as_str).map(strip) {
        Some("model") => {
            if let Some(m) = node.get("model").and_then(Value::as_str) {
                out.push(Layer { model: strip(m).to_string(), tints: tints(art, node, stack) });
            }
        }
        Some("composite") => {
            for m in node.get("models").and_then(Value::as_array).into_iter().flatten() {
                next(Some(m), out);
            }
        }
        Some("condition") => next(node.get("on_false"), out),
        Some("select") => {
            let gui = node.get("cases").and_then(Value::as_array).and_then(|cases| {
                cases.iter().find(|c| match c.get("when") {
                    Some(Value::String(s)) => s == "gui",
                    Some(Value::Array(a)) => a.iter().any(|w| w.as_str() == Some("gui")),
                    _ => false,
                })
            });
            let chosen = gui.and_then(|c| c.get("model")).or_else(|| node.get("fallback")).or_else(|| node.get("cases").and_then(|c| c.get(0)).and_then(|c| c.get("model")));
            next(chosen, out);
        }
        Some("range_dispatch") => next(node.get("fallback").or_else(|| node.get("entries").and_then(|e| e.get(0)).and_then(|e| e.get("model"))), out),
        Some("special") => {
            if let Some(m) = node.get("base").and_then(Value::as_str) {
                out.push(Layer { model: strip(m).to_string(), tints: Vec::new() });
            }
        }
        Some("empty") | None => {}
        Some(_) => next(node.get("fallback"), out),
    }
}

struct Chain<'a> {
    models: Vec<&'a Value>,
    generated: bool,
}

fn chain<'a>(art: &'a Art, path: &str) -> Chain<'a> {
    let mut models = Vec::new();
    let mut generated = false;
    let mut at = Some(strip(path).to_string());
    while let Some(p) = at.take() {
        if models.len() >= MAX_PARENTS {
            break;
        }
        if p == "builtin/generated" {
            generated = true;
            break;
        }
        let Some(m) = art.model(&p) else { break };
        models.push(m);
        at = m.get("parent").and_then(Value::as_str).map(|s| strip(s).to_string());
    }
    Chain { models, generated }
}

fn texture_name(models: &[&Value], key: &str) -> Option<String> {
    let mut name = key.to_string();
    for _ in 0..MAX_PARENTS {
        let value = models.iter().find_map(|m| m.get("textures").and_then(|t| t.get(&name)).and_then(Value::as_str))?;
        match value.strip_prefix('#') {
            Some(next) => name = next.to_string(),
            None => return Some(strip(value).to_string()),
        }
    }
    None
}

#[derive(Clone, Copy)]
struct Transform {
    rotation: [f32; 3],
    translation: [f32; 3],
    scale: [f32; 3],
}

fn display(models: &[&Value]) -> Transform {
    let mut t = Transform { rotation: [0.0; 3], translation: [0.0; 3], scale: [1.0; 3] };
    let Some(gui) = models.iter().find_map(|m| m.get("display").and_then(|d| d.get("gui"))) else { return t };
    let read = |k: &str, into: &mut [f32; 3]| {
        if let Some(a) = gui.get(k).and_then(Value::as_array).filter(|a| a.len() == 3) {
            for i in 0..3 {
                into[i] = a[i].as_f64().unwrap_or(0.0) as f32;
            }
        }
    };
    read("rotation", &mut t.rotation);
    read("translation", &mut t.translation);
    read("scale", &mut t.scale);
    t.translation = t.translation.map(|v| v.clamp(-80.0, 80.0) / 16.0);
    t.scale = t.scale.map(|v| v.clamp(-4.0, 4.0));
    t
}

fn rotate(v: [f32; 3], r: [f32; 3]) -> [f32; 3] {
    let [rx, ry, rz] = r.map(f32::to_radians);
    let (s, c) = rz.sin_cos();
    let v = [v[0] * c - v[1] * s, v[0] * s + v[1] * c, v[2]];
    let (s, c) = ry.sin_cos();
    let v = [v[0] * c + v[2] * s, v[1], -v[0] * s + v[2] * c];
    let (s, c) = rx.sin_cos();
    [v[0], v[1] * c - v[2] * s, v[1] * s + v[2] * c]
}

struct Buffer {
    px: u32,
    color: Vec<[u8; 4]>,
    depth: Vec<f32>,
}

impl Buffer {
    fn new(px: u32) -> Self {
        Self { px, color: vec![[0; 4]; (px * px) as usize], depth: vec![f32::MIN; (px * px) as usize] }
    }

    fn put(&mut self, x: i32, y: i32, z: f32, texel: [u8; 4], tint: [f32; 3], shade: f32) {
        if x < 0 || y < 0 || x >= self.px as i32 || y >= self.px as i32 || texel[3] < 8 {
            return;
        }
        let at = (y as u32 * self.px + x as u32) as usize;
        if z < self.depth[at] {
            return;
        }
        let lit = [0, 1, 2].map(|c| (f32::from(texel[c]) * tint[c] * shade).clamp(0.0, 255.0) as u8);
        if texel[3] >= 200 {
            self.color[at] = [lit[0], lit[1], lit[2], 255];
            self.depth[at] = z;
        } else {
            let mut dst = image::Rgba(self.color[at]);
            blend(&mut dst, [lit[0], lit[1], lit[2], texel[3]]);
            self.color[at] = dst.0;
        }
    }

    fn flat(&mut self, texture: &RgbaImage, tint: [f32; 3]) {
        let side = texture.width().min(texture.height());
        for y in 0..self.px {
            for x in 0..self.px {
                let t = texture.get_pixel(x * side / self.px, y * side / self.px).0;
                self.put(x as i32, y as i32, 0.0, t, tint, 1.0);
            }
        }
    }

    fn quad(&mut self, art: &Art, q: &Quad, t: &Transform, front: bool, tints: &[[f32; 3]]) {
        let Some(texture) = art.texture(strip(&q.texture)) else { return };
        let to = f32::from(self.px as u16) / 16.0;
        let mut p = [[0.0f32; 3]; 4];
        for i in 0..4 {
            let v = q.pos[i].map(|c| c - 0.5);
            let v = [v[0] * t.scale[0], v[1] * t.scale[1], v[2] * t.scale[2]];
            let v = rotate(v, t.rotation);
            let v = [v[0] + t.translation[0], v[1] + t.translation[1], v[2] + t.translation[2]];
            p[i] = [(8.0 + 16.0 * v[0]) * to, (8.0 - 16.0 * v[1]) * to, v[2]];
        }
        let shade = if front {
            1.0
        } else {
            let n = {
                let a = [q.pos[1][0] - q.pos[0][0], q.pos[1][1] - q.pos[0][1], q.pos[1][2] - q.pos[0][2]];
                let b = [q.pos[2][0] - q.pos[0][0], q.pos[2][1] - q.pos[0][1], q.pos[2][2] - q.pos[0][2]];
                let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
                rotate([n[0] / len, n[1] / len, n[2] / len], t.rotation)
            };
            (0.767 + 0.2688 * n[1] - 0.1414 * n[0]).clamp(0.4, 1.0)
        };
        let tint = usize::try_from(q.tint).ok().and_then(|i| tints.get(i)).copied().unwrap_or([1.0; 3]);
        let side = texture.width() as f32;
        for tri in [[0, 1, 2], [0, 2, 3]] {
            let [a, b, c] = tri.map(|i| p[i]);
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if area.abs() < 1e-6 {
                continue;
            }
            let (min_x, max_x) = (a[0].min(b[0]).min(c[0]).floor().max(0.0) as i32, a[0].max(b[0]).max(c[0]).ceil().min(self.px as f32) as i32);
            let (min_y, max_y) = (a[1].min(b[1]).min(c[1]).floor().max(0.0) as i32, a[1].max(b[1]).max(c[1]).ceil().min(self.px as f32) as i32);
            for y in min_y..max_y {
                for x in min_x..max_x {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((b[0] - fx) * (c[1] - fy) - (c[0] - fx) * (b[1] - fy)) / area;
                    let w1 = ((c[0] - fx) * (a[1] - fy) - (a[0] - fx) * (c[1] - fy)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                        continue;
                    }
                    let uv = [0, 1].map(|k| {
                        let (ua, ub, uc) = (q.uv[tri[0]][k], q.uv[tri[1]][k], q.uv[tri[2]][k]);
                        w0 * ua + w1 * ub + w2 * uc
                    });
                    let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                    let tx = ((uv[0] * side).floor() as i32).clamp(0, texture.width() as i32 - 1);
                    let ty = ((uv[1] * side).floor() as i32).clamp(0, texture.width() as i32 - 1);
                    self.put(x, y, z, texture.get_pixel(tx as u32, ty as u32).0, tint, shade);
                }
            }
        }
    }
}

struct Fallback<'a> {
    art: &'a Art,
    cube: Value,
}

impl Source for Fallback<'_> {
    fn blockstate(&self, _: &str) -> Option<&Value> {
        None
    }

    fn model(&self, path: &str) -> Option<&Value> {
        if path == "hud/fallback" { Some(&self.cube) } else { self.art.model(path) }
    }
}

fn paint(art: &Art, layer: &Layer, buffer: &mut Buffer) {
    let chain = chain(art, &layer.model);
    let t = display(&chain.models);
    if chain.generated {
        for i in 0..5 {
            let Some(name) = texture_name(&chain.models, &format!("layer{i}")) else { break };
            if let Some(texture) = art.texture(&name) {
                buffer.flat(texture, layer.tints.get(i).copied().unwrap_or([1.0; 3]));
            }
        }
        return;
    }
    let front = chain.models.iter().find_map(|m| m.get("gui_light").and_then(Value::as_str)) == Some("front");
    if chain.models.iter().any(|m| m.get("elements").is_some()) {
        let quads = model::bake(art, &Variant { model: strip(&layer.model).to_string(), x: 0, y: 0, uvlock: false });
        for q in &quads {
            buffer.quad(art, q, &t, front, &layer.tints);
        }
        return;
    }
    let Some(particle) = texture_name(&chain.models, "particle") else { return };
    let source = Fallback { art, cube: serde_json::json!({ "parent": "block/cube_all", "textures": { "all": particle } }) };
    let quads = model::bake(&source, &Variant { model: "hud/fallback".into(), x: 0, y: 0, uvlock: false });
    let cube = chain_of(&source);
    for q in &quads {
        buffer.quad(art, q, &cube, false, &layer.tints);
    }
}

fn chain_of(source: &Fallback) -> Transform {
    let models: Vec<&Value> = ["block/cube_all", "block/cube", "block/block"].iter().filter_map(|m| source.model(m)).collect();
    display(&models)
}

/// The icon of `stack`, `px` × `px` pixels, transparent where the item has no pixels.
pub fn icon(art: &Art, stack: &Stack, px: u32) -> RgbaImage {
    let mut buffer = Buffer::new(px.max(1));
    let name = strip(stack.id);
    let mut layers = Vec::new();
    if let Some(node) = art.item(name).and_then(|d| d.get("model")) {
        resolve(art, node, stack, &mut layers, 0);
    } else {
        layers.push(Layer { model: format!("item/{name}"), tints: Vec::new() });
    }
    for layer in &layers {
        paint(art, layer, &mut buffer);
    }
    if stack.glint
        && let Some(glint) = art.texture("misc/enchanted_glint_item")
    {
        let (side, unit) = (glint.width() as f32, buffer.px as f32 / 16.0);
        let (sin, cos) = 10f32.to_radians().sin_cos();
        for y in 0..buffer.px {
            for x in 0..buffer.px {
                let at = (y * buffer.px + x) as usize;
                if buffer.color[at][3] == 0 {
                    continue;
                }
                let (gx, gy) = ((x as f32 + 0.5) / unit, (y as f32 + 0.5) / unit);
                let (u, v) = (gx * cos - gy * sin + GLINT_OFFSET[0], gx * sin + gy * cos + GLINT_OFFSET[1]);
                let g = glint.get_pixel(u.rem_euclid(side) as u32 % glint.width(), v.rem_euclid(side) as u32 % glint.height()).0;
                for c in 0..3 {
                    let t = f32::from(g[c]) / 255.0 * GLINT_STRENGTH;
                    buffer.color[at][c] = (f32::from(buffer.color[at][c]) + t * t * 255.0).min(255.0) as u8;
                }
            }
        }
    }
    RgbaImage::from_fn(buffer.px, buffer.px, |x, y| image::Rgba(buffer.color[(y * buffer.px + x) as usize]))
}
