//! Item stacks as the game shows them in the world: the item definition picks a model, block-shaped models bake like
//! blocks, flat ones extrude their layer textures one pixel thick, and the model's display transform places it for the
//! hand, the ground, a frame or a display entity.

use std::sync::Arc;

use serde_json::Value;

use super::assets::{Assets, Texture, Transparency};
use super::entity::Item;
use super::figure::{Affine, Paint, Painter, Sheet};
use super::model::{self, Variant};

const MAX_PARENTS: usize = 16;
const MAX_DEPTH: usize = 8;

/// Where an item is shown, as the game numbers its display contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    None = 0,
    ThirdLeft = 1,
    ThirdRight = 2,
    FirstLeft = 3,
    FirstRight = 4,
    Head = 5,
    Gui = 6,
    Ground = 7,
    Fixed = 8,
}

impl Context {
    /// The context with display context id `id`; unknown ids show untransformed.
    pub fn from_id(id: u8) -> Self {
        match id {
            1 => Self::ThirdLeft,
            2 => Self::ThirdRight,
            3 => Self::FirstLeft,
            4 => Self::FirstRight,
            5 => Self::Head,
            6 => Self::Gui,
            7 => Self::Ground,
            8 => Self::Fixed,
            _ => Self::None,
        }
    }

    fn key(self) -> Option<&'static str> {
        Some(match self {
            Self::None => return None,
            Self::ThirdLeft => "thirdperson_lefthand",
            Self::ThirdRight => "thirdperson_righthand",
            Self::FirstLeft => "firstperson_lefthand",
            Self::FirstRight => "firstperson_righthand",
            Self::Head => "head",
            Self::Gui => "gui",
            Self::Ground => "ground",
            Self::Fixed => "fixed",
        })
    }

    fn left(self) -> bool {
        matches!(self, Self::ThirdLeft | Self::FirstLeft)
    }

    fn mirror(self) -> Option<Self> {
        match self {
            Self::ThirdLeft => Some(Self::ThirdRight),
            Self::FirstLeft => Some(Self::FirstRight),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
enum Tex {
    Atlas(u32),
    Sheet(usize),
}

struct Quad {
    pos: [[f32; 3]; 4],
    uv: [[f32; 2]; 4],
    tex: Tex,
    tint: [f32; 3],
    mode: Transparency,
    shade: Option<f32>,
}

/// An item's model ready to draw, in item space (one block, `0..1` on every axis).
pub struct Model {
    quads: Vec<Quad>,
    sheets: Vec<Arc<Sheet>>,
    display: Option<Value>,
    /// Shaped like a block rather than a flat sprite.
    pub block: bool,
}

fn strip(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn rgb(c: i64) -> [f32; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| v as f32 / 255.0)
}

fn tint(spec: &Value, item: &Item) -> [f32; 3] {
    let default = spec.get("default").or_else(|| spec.get("value")).and_then(Value::as_i64);
    match spec.get("type").and_then(Value::as_str).map(strip) {
        Some("dye") => item.dye.map(i64::from).or(default).map_or([1.0; 3], rgb),
        Some("grass") => rgb(0x7FB238),
        Some("foliage") => rgb(0x48B518),
        _ => default.map_or([1.0; 3], rgb),
    }
}

/// Follows an item definition to the model it shows by default, with its tints.
fn pick<'a>(def: &'a Value, depth: usize) -> Option<(&'a str, Option<&'a Vec<Value>>)> {
    if depth > MAX_DEPTH {
        return None;
    }
    match def.get("type").and_then(Value::as_str).map(strip)? {
        "model" => Some((def.get("model")?.as_str()?, def.get("tints").and_then(Value::as_array))),
        "condition" => pick(def.get("on_false")?, depth + 1),
        "select" | "range_dispatch" => def.get("fallback").and_then(|f| pick(f, depth + 1)).or_else(|| {
            let list = def.get("cases").or_else(|| def.get("entries"))?.as_array()?;
            list.iter().find_map(|c| pick(c.get("model")?, depth + 1))
        }),
        "composite" => def.get("models")?.as_array()?.iter().find_map(|m| pick(m, depth + 1)),
        "special" => def.get("base").and_then(Value::as_str).map(|b| (b, None)),
        _ => None,
    }
}

impl Model {
    /// The model `item` shows, or `None` for an item the jar does not know.
    pub fn of(assets: &Assets, item: &Item) -> Option<Self> {
        let name = strip(item.id);
        let def = assets.data(name)?.get("model")?;
        let (path, tints) = pick(def, 0)?;
        let path = strip(path).to_string();
        let tints: Vec<[f32; 3]> = tints.map(|t| t.iter().map(|s| tint(s, item)).collect()).unwrap_or_default();
        let mut chain = Vec::new();
        let mut next = Some(path.clone());
        let mut generated = false;
        while let Some(p) = next.take() {
            if chain.len() >= MAX_PARENTS {
                break;
            }
            if p == "builtin/generated" {
                generated = true;
                break;
            }
            let Some(m) = model::Source::model(assets, &p) else { break };
            chain.push(m);
            next = m.get("parent").and_then(Value::as_str).map(|s| strip(s).to_string());
        }
        let display = chain.iter().find_map(|m| m.get("display")).cloned();
        let mut out = Model { quads: Vec::new(), sheets: Vec::new(), display, block: !generated };
        let tint_of = |i: usize| tints.get(i).copied().unwrap_or([1.0; 3]);
        if generated {
            for layer in 0..8 {
                let Some(texture) = chain.iter().find_map(|m| m.pointer(&format!("/textures/layer{layer}")).and_then(Value::as_str)) else { break };
                let texture = strip(texture);
                if let Some(sheet) = assets.sheet(texture) {
                    out.sheets.push(sheet.clone());
                    out.extrude(&sheet.texture, sheet.scale, Tex::Sheet(out.sheets.len() - 1), tint_of(layer), layer);
                } else if let Some(id) = assets.texture_id(texture) {
                    out.extrude(assets.texture(id), [1.0; 2], Tex::Atlas(id), tint_of(layer), layer);
                }
            }
        } else {
            for q in model::bake(assets, &Variant { model: path, x: 0, y: 0, uvlock: false }) {
                let tint = if q.tint >= 0 { tint_of(q.tint as usize) } else { [1.0; 3] };
                let tex = match assets.texture_id(&q.texture) {
                    Some(id) => Tex::Atlas(id),
                    None => match assets.sheet(&q.texture) {
                        Some(s) => {
                            out.sheets.push(s);
                            Tex::Sheet(out.sheets.len() - 1)
                        }
                        None => continue,
                    },
                };
                out.quads.push(Quad { pos: q.pos, uv: q.uv, tex, tint, mode: Transparency::Cutout, shade: None });
            }
        }
        (!out.quads.is_empty()).then_some(out)
    }

    fn extrude(&mut self, t: &Texture, s: [f32; 2], tex: Tex, tint: [f32; 3], layer: usize) {
        let (w, h) = ((t.size as f32 * s[0]).round() as i32, (t.size as f32 * s[1]).round() as i32);
        if w <= 0 || h <= 0 || t.levels.is_empty() {
            return;
        }
        let lift = layer as f32 * 0.0004;
        let (z0, z1) = (7.5 / 16.0 - lift, 8.5 / 16.0 + lift);
        let solid = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h && t.levels[0][(y as u32 * t.size + x as u32) as usize][3] > 0;
        let uv = |u: f32, v: f32| [u * s[0], v * s[1]];
        self.quads.push(Quad { pos: [[0.0, 1.0, z1], [0.0, 0.0, z1], [1.0, 0.0, z1], [1.0, 1.0, z1]], uv: [uv(0.0, 0.0), uv(0.0, 1.0), uv(1.0, 1.0), uv(1.0, 0.0)], tex, tint, mode: Transparency::Cutout, shade: None });
        self.quads.push(Quad { pos: [[1.0, 1.0, z0], [1.0, 0.0, z0], [0.0, 0.0, z0], [0.0, 1.0, z0]], uv: [uv(1.0, 0.0), uv(1.0, 1.0), uv(0.0, 1.0), uv(0.0, 0.0)], tex, tint, mode: Transparency::Cutout, shade: None });
        let (fw, fh) = (w as f32, h as f32);
        for y in 0..h {
            for x in 0..w {
                if !solid(x, y) {
                    continue;
                }
                let (x0, x1) = (x as f32 / fw, (x + 1) as f32 / fw);
                let (y1, y0) = (1.0 - y as f32 / fh, 1.0 - (y + 1) as f32 / fh);
                let (u0, u1, v0, v1) = (x as f32 / fw + 0.01 / fw, (x + 1) as f32 / fw - 0.01 / fw, y as f32 / fh + 0.01 / fh, (y + 1) as f32 / fh - 0.01 / fh);
                let px = [uv(u0, v0), uv(u0, v1), uv(u1, v1), uv(u1, v0)];
                let mut side = |pos: [[f32; 3]; 4]| self.quads.push(Quad { pos, uv: px, tex, tint, mode: Transparency::Cutout, shade: None });
                if !solid(x, y - 1) {
                    side([[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]]);
                }
                if !solid(x, y + 1) {
                    side([[x0, y0, z1], [x0, y0, z0], [x1, y0, z0], [x1, y0, z1]]);
                }
                if !solid(x - 1, y) {
                    side([[x0, y1, z0], [x0, y0, z0], [x0, y0, z1], [x0, y1, z1]]);
                }
                if !solid(x + 1, y) {
                    side([[x1, y1, z1], [x1, y0, z1], [x1, y0, z0], [x1, y1, z0]]);
                }
            }
        }
    }

    fn transform(&self, context: Context) -> Option<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let display = self.display.as_ref()?;
        let key = context.key()?;
        let entry = display.get(key).or_else(|| display.get(context.mirror()?.key()?))?;
        let three = |k: &str, d: f32| entry.get(k).and_then(Value::as_array).map(|a| a.iter().take(3).map(|v| v.as_f64().unwrap_or(f64::from(d)) as f32).collect::<Vec<_>>()).filter(|v| v.len() == 3).unwrap_or(vec![d; 3]);
        Some((three("rotation", 0.0), three("translation", 0.0), three("scale", 1.0)))
    }

    /// Where item space lands for `context` under `at`: the display transform, then the model centred on the origin.
    pub fn placed(&self, at: &Affine, context: Context) -> Affine {
        let mut at = *at;
        if let Some((r, t, s)) = self.transform(context) {
            let sign = if context.left() { -1.0 } else { 1.0 };
            let t = [sign * (t[0] / 16.0).clamp(-5.0, 5.0), (t[1] / 16.0).clamp(-5.0, 5.0), (t[2] / 16.0).clamp(-5.0, 5.0)];
            let (ry, rz) = if context.left() { (-r[1], -r[2]) } else { (r[1], r[2]) };
            at = at.translate(t).rotate_x(r[0].to_radians()).rotate_y(ry.to_radians()).rotate_z(rz.to_radians()).scale(s.iter().map(|v| v.clamp(-4.0, 4.0)).collect::<Vec<_>>().try_into().unwrap_or([1.0; 3]));
        }
        at.translate([-0.5, -0.5, -0.5])
    }

    /// Draws the model shown in `context` with `at` mapping the context's space to world space around the camera.
    pub fn draw(&self, painter: &mut Painter, assets: &Assets, at: &Affine, context: Context, light: [f32; 3]) {
        let at = self.placed(at, context);
        for q in &self.quads {
            let world = q.pos.map(|p| at.apply(p));
            let paint = Paint { light, tint: q.tint, mode: q.mode };
            match q.tex {
                Tex::Atlas(id) => painter.face(world, q.uv, assets.texture(id), &paint, q.shade),
                Tex::Sheet(i) => painter.face(world, q.uv, &self.sheets[i].texture, &paint, q.shade),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_definitions_fall_back_to_their_plain_model() {
        let def: Value = serde_json::json!({"type": "minecraft:condition", "on_false": {"type": "minecraft:model", "model": "minecraft:item/bow"}, "on_true": {"type": "minecraft:model", "model": "x"}});
        assert_eq!(pick(&def, 0).map(|p| p.0), Some("minecraft:item/bow"));
        let select: Value = serde_json::json!({"type": "minecraft:select", "cases": [], "fallback": {"type": "minecraft:model", "model": "minecraft:item/leather_chestplate", "tints": [{"type": "minecraft:dye", "default": -6265536}]}});
        let (path, tints) = pick(&select, 0).unwrap();
        assert_eq!(path, "minecraft:item/leather_chestplate");
        let item = Item { id: "leather_chestplate", count: 1, glint: false, dye: None, trim: None, profile: None };
        assert_eq!(tint(&tints.unwrap()[0], &item), rgb(-6265536));
        assert_eq!(Context::from_id(8), Context::Fixed);
    }
}
