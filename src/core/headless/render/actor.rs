//! Draws the entities of a picture: players and mobs from the game's model layers and textures, posed as they stand
//! idle, lit by the light where they are.

use std::f32::consts::PI;
use std::sync::Arc;

use super::Blocks;
use super::assets::{Assets, Transparency};
use super::entity::{Data, Entities, Entity, Pose as Stance, Skin, slot};
use super::figure::{Affine, Painter, Paint, Part, Pose, Sheet};
use super::light::{self, Lightmap};
use super::raster::Target;
use super::scene::View;

const DYES: [u32; 16] = [0xF9FFFE, 0xF9801D, 0xC74EBD, 0x3AB3DA, 0xFED83D, 0x80C71F, 0xF38BAA, 0x474F52, 0x9D9D97, 0x169C9C, 0x8932B8, 0x3C44AA, 0x835432, 0x5E7C16, 0xB02E26, 0x1D1D21];
const SKINS: [&str; 9] = ["alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri"];
const HORSES: [&str; 7] = ["white", "creamy", "chestnut", "brown", "black", "gray", "darkbrown"];
const MARKINGS: [&str; 5] = ["", "white", "whitefield", "whitedots", "blackdots"];
const LEVELS: [&str; 5] = ["stone", "iron", "gold", "emerald", "diamond"];
const INVISIBLE: u8 = 0x20;
const WHITE: [f32; 3] = [1.0; 3];

/// One model layer drawn with one sheet.
struct Coat<'a> {
    part: &'a Part,
    sheet: Arc<Sheet>,
    tint: [f32; 3],
    mode: Transparency,
    glow: bool,
    hide: &'static [&'static str],
}

/// What an entity looks like: its layers, scale and how its parts are posed.
struct Figure<'a> {
    coats: Vec<Coat<'a>>,
    scale: [f32; 3],
    rig: Rig,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rig {
    Plain,
    Humanoid,
    Zombie,
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| v as f32 / 255.0)
}

fn wool(color: u8) -> [f32; 3] {
    let color = usize::from(color.min(15));
    if color == 0 {
        return rgb(0xE6E6E6);
    }
    rgb(DYES[color]).map(|v| ((v * 255.0 * 0.75).floor() / 255.0).clamp(0.0, 1.0))
}

fn plain(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// The default skin the game gives a profile without one: picked by the UUID's Java hash.
pub fn default_skin(skin: &Skin) -> (String, bool) {
    let (hi, lo) = skin.uuid.as_u64_pair();
    let bits = hi ^ lo;
    let hash = ((bits >> 32) as i32) ^ (bits as i32);
    let index = hash.rem_euclid(18) as usize;
    let slim = index < 9;
    (format!("entity/player/{}/{}", if slim { "slim" } else { "wide" }, SKINS[index % 9]), slim)
}

struct Builder<'a> {
    assets: &'a Assets,
    coats: Vec<Coat<'a>>,
}

impl<'a> Builder<'a> {
    fn coat(&mut self, layer: &str, texture: &str, tint: [f32; 3]) -> bool {
        self.layer(layer, texture, tint, Transparency::Cutout, false, &[])
    }

    fn layer(&mut self, layer: &str, texture: &str, tint: [f32; 3], mode: Transparency, glow: bool, hide: &'static [&'static str]) -> bool {
        let figures = &self.assets.figures;
        let (Some(part), Some(sheet)) = (figures.layer(layer), self.assets.sheet(texture)) else { return false };
        self.coats.push(Coat { part, sheet, tint, mode, glow, hide });
        true
    }
}

fn asset(id: &str) -> String {
    plain(id).to_string()
}

fn variant_textures(assets: &Assets, dir: &str, variant: Option<&str>, baby: bool) -> Option<(String, Option<String>)> {
    let data = assets.data(&format!("{dir}/{}", plain(variant?)))?;
    let id = data.get(if baby { "baby_asset_id" } else { "asset_id" }).or_else(|| data.get("asset_id"))?.as_str()?;
    Some((asset(id), data.get("model").and_then(|m| m.as_str()).map(str::to_string)))
}

fn figure<'a>(e: &Entity, assets: &'a Assets, skin: Option<&Arc<Sheet>>) -> Option<Figure<'a>> {
    let kind = plain(e.kind);
    let baby = e.baby;
    let mut b = Builder { assets, coats: Vec::new() };
    let mut scale = [1.0; 3];
    let mut rig = Rig::Plain;
    let suffix = if baby { "_baby" } else { "" };
    match (kind, &e.data) {
        ("player", data) => {
            let skin_data = match data {
                Data::Player { skin, .. } => skin.clone(),
                _ => Skin::default(),
            };
            let (fallback, fallback_slim) = default_skin(&skin_data);
            let slim = if skin.is_some() { skin_data.slim } else { fallback_slim };
            let layer = if slim { "minecraft:player_slim#main" } else { "minecraft:player#main" };
            let part = assets.figures.layer(layer)?;
            let sheet = match skin {
                Some(s) => s.clone(),
                None => assets.sheet(&fallback)?,
            };
            b.coats.push(Coat { part, sheet, tint: WHITE, mode: Transparency::Cutout, glow: false, hide: hidden_parts(skin_data.parts) });
            scale = [0.9375; 3];
            rig = Rig::Humanoid;
        }
        ("villager" | "zombie_villager", data) => {
            let (kind_id, profession, level) = match data {
                Data::Villager { kind, profession, level } => (plain(kind).to_string(), plain(profession).to_string(), *level),
                _ => ("plains".into(), "none".into(), 1),
            };
            let base = if kind == "villager" { format!("entity/villager/villager{suffix}") } else { format!("entity/zombie_villager/zombie_villager{suffix}") };
            let main = format!("minecraft:{kind}{suffix}#main");
            let no_hat = format!("minecraft:{kind}{suffix}_no_hat#main");
            b.coat(&main, &base, WHITE);
            let hat = |path: &str| assets.data(&format!("{path}.mcmeta")).and_then(|m| m.pointer("/villager/hat")).and_then(|h| h.as_str()).unwrap_or("none").to_string();
            let type_path = format!("entity/{kind}/{}/{kind_id}", if baby { "baby" } else { "type" });
            let profession_path = format!("entity/{kind}/profession/{profession}");
            let profession_hat = if profession == "none" || baby { "none".to_string() } else { hat(&profession_path) };
            let type_hat = hat(&format!("entity/{kind}/type/{kind_id}"));
            let type_hat_visible = profession_hat == "none" || profession_hat == "partial" && type_hat != "full";
            b.coat(if type_hat_visible { &main } else { &no_hat }, &type_path, WHITE);
            if profession != "none" && !baby {
                b.coat(&main, &profession_path, WHITE);
                if profession != "nitwit" {
                    b.coat(&main, &format!("entity/{kind}/profession_level/{}", LEVELS[usize::from(level.clamp(1, 5)) - 1]), WHITE);
                }
            }
            rig = if kind == "zombie_villager" { Rig::Zombie } else { Rig::Plain };
        }
        ("sheep", data) => {
            let (color, sheared) = match data {
                Data::Sheep { color, sheared } => (*color, *sheared),
                _ => (0, false),
            };
            b.coat(&format!("minecraft:sheep{suffix}#main"), &format!("entity/sheep/sheep{suffix}"), WHITE);
            if !baby && color != 0 {
                b.coat("minecraft:sheep#wool_undercoat", "entity/sheep/sheep_wool_undercoat", wool(color));
            }
            if !sheared {
                b.coat(&format!("minecraft:sheep{suffix}#wool"), &format!("entity/sheep/sheep_wool{suffix}"), wool(color));
            }
        }
        ("cow" | "pig" | "chicken" | "cat" | "frog", data) => {
            let variant = match data {
                Data::Variant(v) => Some(v.as_str()),
                Data::Cat { variant, .. } => Some(variant.as_str()),
                _ => None,
            };
            let default = match kind {
                "cat" => "tabby",
                _ => "temperate",
            };
            let (texture, model) = variant_textures(assets, &format!("{kind}_variant"), variant, baby).or_else(|| variant_textures(assets, &format!("{kind}_variant"), Some(default), baby))?;
            let layer = match model.as_deref() {
                Some(m) if m != "normal" && !baby => format!("minecraft:{m}_{kind}#main"),
                _ => format!("minecraft:{kind}{suffix}#main"),
            };
            b.coat(&layer, &texture, WHITE);
        }
        ("wolf", data) => {
            let (tame, collar, variant) = match data {
                Data::Wolf { tame, collar, variant } => (*tame, *collar, Some(variant.as_str())),
                _ => (false, 14, None),
            };
            let entry = variant.and_then(|v| assets.data(&format!("wolf_variant/{}", plain(v)))).or_else(|| assets.data("wolf_variant/pale"))?;
            let set = entry.get(if baby { "baby_assets" } else { "assets" })?;
            let texture = set.get(if tame { "tame" } else { "wild" })?.as_str()?;
            b.coat(&format!("minecraft:wolf{suffix}#main"), &asset(texture), WHITE);
            if tame {
                b.coat(&format!("minecraft:wolf{suffix}#main"), &format!("entity/wolf/wolf_collar{suffix}"), rgb(DYES[usize::from(collar.min(15))]));
            }
        }
        ("horse", data) => {
            let variant = match data {
                Data::Horse { variant } => *variant,
                _ => 0,
            };
            let color = HORSES[(variant & 0xFF) as usize % HORSES.len()];
            let layer = format!("minecraft:horse{suffix}#main");
            b.coat(&layer, &format!("entity/horse/horse_{color}{suffix}"), WHITE);
            let marking = MARKINGS[((variant >> 8) & 0xFF) as usize % MARKINGS.len()];
            if !marking.is_empty() {
                b.layer(&layer, &format!("entity/horse/horse_markings_{marking}{suffix}"), WHITE, Transparency::Translucent, false, &[]);
            }
        }
        ("slime" | "magma_cube", data) => {
            let size = match data {
                Data::Slime { size } => f32::from((*size).max(1)),
                _ => 1.0,
            };
            scale = [size * 0.999; 3];
            if kind == "slime" {
                b.coat("minecraft:slime#main", "entity/slime/slime", WHITE);
                b.layer("minecraft:slime#outer", "entity/slime/slime", WHITE, Transparency::Translucent, false, &[]);
            } else {
                b.coat("minecraft:magma_cube#main", "entity/slime/magmacube", WHITE);
            }
        }
        ("enderman", _) => {
            b.coat("minecraft:enderman#main", "entity/enderman/enderman", WHITE);
            b.layer("minecraft:enderman#main", "entity/enderman/enderman_eyes", WHITE, Transparency::Cutout, true, &[]);
            rig = Rig::Humanoid;
        }
        ("spider" | "cave_spider", _) => {
            b.coat(&format!("minecraft:{kind}#main"), &format!("entity/spider/{kind}"), WHITE);
            b.layer(&format!("minecraft:{kind}#main"), "entity/spider/spider_eyes", WHITE, Transparency::Cutout, true, &[]);
        }
        _ => {
            let texture = match kind {
                "husk" => format!("entity/zombie/husk{suffix}"),
                "drowned" => format!("entity/zombie/drowned{suffix}"),
                "stray" => "entity/skeleton/stray".into(),
                "wither_skeleton" => "entity/skeleton/wither_skeleton".into(),
                "bogged" => "entity/skeleton/bogged".into(),
                "zombified_piglin" => format!("entity/piglin/zombified_piglin{suffix}"),
                "mooshroom" => format!("entity/cow/mooshroom_red{suffix}"),
                "witch" => "entity/witch".into(),
                _ => format!("entity/{kind}/{kind}{suffix}"),
            };
            let layer = format!("minecraft:{kind}{suffix}#main");
            if !(b.coat(&layer, &texture, WHITE) || b.coat(&layer, &format!("entity/{kind}{suffix}"), WHITE)) {
                return None;
            }
            rig = match kind {
                "zombie" | "husk" | "drowned" | "zombified_piglin" => Rig::Zombie,
                "skeleton" | "stray" | "wither_skeleton" | "bogged" | "piglin" | "piglin_brute" | "armor_stand" => Rig::Humanoid,
                _ => Rig::Plain,
            };
        }
    }
    (!b.coats.is_empty()).then_some(Figure { coats: b.coats, scale, rig })
}

fn hidden_parts(parts: u8) -> &'static [&'static str] {
    const ALL: [&str; 6] = ["jacket", "left_sleeve", "right_sleeve", "left_pants", "right_pants", "hat"];
    const BITS: [u8; 6] = [0x02, 0x04, 0x08, 0x10, 0x20, 0x40];
    static TABLE: std::sync::OnceLock<Vec<Vec<&'static str>>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| (0..=127u8).map(|p| ALL.iter().zip(BITS).filter(|(_, b)| p & b == 0).map(|(n, _)| *n).collect()).collect());
    &table[usize::from(parts & 0x7F)]
}

fn wrap(degrees: f32) -> f32 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

/// Changes a part's pose the way the entity's model does when it stands still.
fn pose(rig: Rig, e: &Entity, name: &str, p: &mut Pose) {
    let head_yaw = wrap(e.head_yaw - e.yaw).to_radians();
    let pitch = e.pitch.to_radians();
    let crouch = e.pose == Stance::Crouching;
    match name {
        "head" => {
            p.rotation[0] += pitch;
            p.rotation[1] += head_yaw;
            if crouch && rig != Rig::Plain {
                p.pivot[1] += 4.2;
            }
        }
        _ if rig == Rig::Plain => {}
        "body" if crouch => {
            p.rotation[0] = 0.5;
            p.pivot[1] += 3.2;
        }
        "right_arm" | "left_arm" => {
            let right = name == "right_arm";
            let sign = if right { 1.0 } else { -1.0 };
            if rig == Rig::Zombie {
                p.rotation = [-PI / 2.25, -0.1 * sign, 0.0];
            } else {
                let main_left = matches!(e.data, Data::Player { main_left: true, .. });
                let main = if main_left { !right } else { right };
                let held = if main { &e.equipment[slot::MAIN_HAND] } else { &e.equipment[slot::OFF_HAND] };
                if held.is_some() {
                    p.rotation[0] = p.rotation[0] * 0.5 - PI / 10.0;
                }
            }
            if crouch {
                p.rotation[0] += 0.4;
                p.pivot[1] += 3.2;
            }
            p.rotation[2] += sign * 0.05;
        }
        "right_leg" | "left_leg" => {
            let sign = if name == "right_leg" { 1.0 } else { -1.0 };
            p.rotation[1] = 0.005 * sign;
            p.rotation[2] = 0.005 * sign;
            if crouch {
                p.pivot[2] += 4.0;
            }
        }
        _ => {}
    }
}

/// Where the model's own space sits in the world, relative to the camera: feet at `pos`, turned by the body yaw,
/// flipped and lifted as the game does.
fn placement(e: &Entity, eye: [f64; 3], scale: [f32; 3]) -> Affine {
    let rel = [0, 1, 2].map(|k| (e.pos[k] - eye[k]) as f32);
    Affine::at(rel).rotate_y((180.0 - e.yaw).to_radians()).scale([-1.0, -1.0, 1.0]).scale(scale).translate([0.0, -1.501, 0.0])
}

/// Draws every entity within `reach` blocks of the camera.
pub fn draw(entities: &impl Entities, world: &impl Blocks, assets: &Assets, lightmap: &Lightmap, view: &View, target: &mut Target, reach: f32, skins: &dyn Fn(&Skin) -> Option<Arc<Sheet>>) {
    let mut painter = Painter { target, view };
    for e in entities.entities() {
        let d = [0, 1, 2].map(|k| (e.pos[k] - view.eye[k]) as f32);
        if !d.iter().all(|v| v.is_finite()) || d[0].abs() > reach || d[2].abs() > reach || d[1].abs() > 384.0 || e.flags & INVISIBLE != 0 {
            continue;
        }
        let skin = match &e.data {
            Data::Player { skin, .. } if skin.url.is_some() => skins(skin),
            _ => None,
        };
        let Some(figure) = figure(e, assets, skin.as_ref()) else { continue };
        let probe = [e.pos[0].floor() as i32, (e.pos[1] + 0.5).floor() as i32, e.pos[2].floor() as i32];
        let (sky, block) = world.light(probe[0], probe[1], probe[2]).unwrap_or((15, 0));
        let lit = lightmap.color(light::pack(sky, block));
        let at = placement(e, view.eye, figure.scale);
        for coat in &figure.coats {
            let paint = Paint { light: if coat.glow { WHITE } else { lit }, tint: coat.tint, mode: coat.mode };
            let hide = coat.hide;
            let rig = figure.rig;
            painter.part(coat.part, &at, &coat.sheet, &paint, &mut |name, p| {
                if hide.contains(&name) {
                    return false;
                }
                pose(rig, e, name, p);
                true
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_skins_follow_the_uuid_hash_like_the_game() {
        let skin = |s: &str| Skin { uuid: uuid::Uuid::parse_str(s).unwrap(), ..Skin::default() };
        let (path, slim) = default_skin(&skin("00000000-0000-0000-0000-000000000000"));
        assert_eq!((path.as_str(), slim), ("entity/player/slim/alex", true));
        let (path, _) = default_skin(&skin("00000000-0000-0006-0000-000000000000"));
        assert_eq!(path, "entity/player/slim/steve");
        let (path, slim) = default_skin(&skin("00000000-0000-0000-0000-00000000000f"));
        assert_eq!((path.as_str(), slim), ("entity/player/wide/steve", false));
    }

    #[test]
    fn skin_parts_hide_the_outer_layers_that_are_off() {
        assert_eq!(hidden_parts(0x7F), &[] as &[&str]);
        assert!(hidden_parts(0).contains(&"hat"));
        assert!(!hidden_parts(0x40).contains(&"hat") && hidden_parts(0x40).contains(&"jacket"));
    }

    #[test]
    fn wool_is_dimmed_like_the_game_and_white_is_its_own_grey() {
        assert_eq!(wool(0), rgb(0xE6E6E6));
        let red = wool(14);
        assert!((red[0] - (0xB0 as f32 * 0.75).floor() / 255.0).abs() < 1e-6);
    }
}
