//! Draws the entities of a picture: players and mobs from the game's model layers and textures, posed as they stand
//! idle, lit by the light where they are.

use std::f32::consts::PI;
use std::sync::Arc;

use super::Blocks;
use super::assets::{Assets, Transparency};
use super::entity::{Data, DisplayKind, Entities, Entity, Pose as Stance, Skin, slot};
use super::figure::{Affine, Painter, Paint, Part, Pose, Sheet};
use super::item::{Context, Model};
use super::label::Label;
use super::light::{self, Lightmap};
use super::map;
use super::raster::Target;
use super::raster::Vertex;
use super::scene::{Scene, View};

const DYES: [u32; 16] = [0xF9FFFE, 0xF9801D, 0xC74EBD, 0x3AB3DA, 0xFED83D, 0x80C71F, 0xF38BAA, 0x474F52, 0x9D9D97, 0x169C9C, 0x8932B8, 0x3C44AA, 0x835432, 0x5E7C16, 0xB02E26, 0x1D1D21];
const SKINS: [&str; 9] = ["alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri"];
const HORSES: [&str; 7] = ["white", "creamy", "chestnut", "brown", "black", "gray", "darkbrown"];
const MARKINGS: [&str; 5] = ["", "white", "whitefield", "whitedots", "blackdots"];
const LEVELS: [&str; 5] = ["stone", "iron", "gold", "emerald", "diamond"];
const INVISIBLE: u8 = 0x20;
const WHITE: [f32; 3] = [1.0; 3];
const GRASS: u32 = 0x91BD59;

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
    slim: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Rig {
    Plain,
    Humanoid,
    Zombie,
    Stand([[f32; 3]; 6]),
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
    let mut slim = false;
    let suffix = if baby { "_baby" } else { "" };
    match (kind, &e.data) {
        ("player", data) => {
            let skin_data = match data {
                Data::Player { skin, .. } => skin.clone(),
                _ => Skin::default(),
            };
            let (fallback, fallback_slim) = default_skin(&skin_data);
            slim = if skin.is_some() { skin_data.slim } else { fallback_slim };
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
                Data::Slime { size } => f32::from((*size).clamp(1, 127)),
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
        ("armor_stand", data) => {
            let (small, arms, no_plate, poses) = match data {
                Data::ArmorStand { small, arms, no_plate, poses, .. } => (*small, *arms, *no_plate, *poses),
                _ => (false, false, false, STAND),
            };
            let hide: &'static [&'static str] = match (arms, no_plate) {
                (true, true) => &["hat", "base_plate"],
                (true, false) => &["hat"],
                (false, true) => &["hat", "base_plate", "right_arm", "left_arm"],
                (false, false) => &["hat", "right_arm", "left_arm"],
            };
            let layer = if small { "minecraft:armor_stand_small#main" } else { "minecraft:armor_stand#main" };
            b.layer(layer, "entity/armorstand/armorstand", WHITE, Transparency::Cutout, false, hide);
            rig = Rig::Stand(poses);
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
    (!b.coats.is_empty()).then_some(Figure { coats: b.coats, scale, rig, slim })
}

fn hidden_parts(parts: u8) -> &'static [&'static str] {
    const ALL: [&str; 6] = ["jacket", "left_sleeve", "right_sleeve", "left_pants", "right_pants", "hat"];
    const BITS: [u8; 6] = [0x02, 0x04, 0x08, 0x10, 0x20, 0x40];
    static TABLE: std::sync::OnceLock<Vec<Vec<&'static str>>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| (0..=127u8).map(|p| ALL.iter().zip(BITS).filter(|(_, b)| p & b == 0).map(|(n, _)| *n).collect()).collect());
    &table[usize::from(parts & 0x7F)]
}

const STAND: [[f32; 3]; 6] = [[0.0; 3], [0.0; 3], [-10.0, 0.0, -10.0], [-15.0, 0.0, 10.0], [-1.0, 0.0, -1.0], [1.0, 0.0, 1.0]];

fn wrap(degrees: f32) -> f32 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

/// Changes a part's pose the way the entity's model does when it stands still.
fn pose(rig: Rig, e: &Entity, name: &str, p: &mut Pose) {
    if let Rig::Stand(poses) = rig {
        let index = match name {
            "head" => 0,
            "body" | "left_body_stick" | "right_body_stick" | "shoulder_stick" => 1,
            "left_arm" => 2,
            "right_arm" => 3,
            "left_leg" => 4,
            "right_leg" => 5,
            _ => return,
        };
        p.rotation = poses[index].map(f32::to_radians);
        return;
    }
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

/// The transform of part `name` inside `part`, posed like the drawn model.
fn locate(part: &Part, name: &str, at: &Affine, pose: &mut dyn FnMut(&str, &mut Pose)) -> Option<Affine> {
    let mut own = part.pose;
    pose(&part.name, &mut own);
    let at = at.part(&own);
    if &*part.name == name {
        return Some(at);
    }
    part.children.iter().find_map(|c| locate(c, name, &at, pose))
}

fn equipment_asset(item: &str) -> Option<&'static str> {
    let name = plain(item);
    let (material, piece) = name.rsplit_once('_')?;
    if !matches!(piece, "helmet" | "chestplate" | "leggings" | "boots") {
        return None;
    }
    Some(match material {
        "leather" => "leather",
        "chainmail" => "chainmail",
        "iron" => "iron",
        "golden" => "gold",
        "diamond" => "diamond",
        "netherite" => "netherite",
        "copper" => "copper",
        "turtle" => "turtle_scute",
        _ => return None,
    })
}

/// Draws the armor an entity wears over its model.
#[allow(clippy::too_many_arguments)]
fn armor(painter: &mut Painter, assets: &Assets, e: &Entity, kind: &str, slim: bool, rig: Rig, at: &Affine, light: [f32; 3]) {
    const SLOTS: [(usize, &str, &str); 4] = [(slot::FEET, "boots", "humanoid"), (slot::LEGS, "leggings", "humanoid_leggings"), (slot::CHEST, "chestplate", "humanoid"), (slot::HEAD, "helmet", "humanoid")];
    let owner = match (kind, slim, e.baby) {
        ("player", true, _) => "player_slim".to_string(),
        (_, _, true) => format!("{kind}_baby"),
        _ => kind.to_string(),
    };
    for (index, layer_name, kind_key) in SLOTS {
        let Some(item) = &e.equipment[index] else { continue };
        let Some(asset) = equipment_asset(item.id) else { continue };
        let Some(layers) = assets.data(&format!("equipment/{asset}")).and_then(|d| d.pointer(&format!("/layers/{kind_key}"))).and_then(|l| l.as_array()) else { continue };
        let figures = &assets.figures;
        let Some(part) = figures.layer(&format!("minecraft:{owner}#{layer_name}")).or_else(|| figures.layer(&format!("minecraft:player#{layer_name}"))) else { continue };
        for layer in layers {
            let Some(texture) = layer.get("texture").and_then(|t| t.as_str()) else { continue };
            let Some(sheet) = assets.sheet(&format!("entity/equipment/{kind_key}/{}", plain(texture))) else { continue };
            let tint = match layer.get("dyeable") {
                Some(d) => item.dye.map(rgb).or_else(|| d.get("color_when_undyed").and_then(|c| c.as_i64()).map(|c| rgb(c as u32))).unwrap_or(WHITE),
                None => WHITE,
            };
            let paint = Paint { light, tint, mode: Transparency::Cutout };
            painter.part(part, at, &sheet, &paint, &mut |name, p| {
                pose(rig, e, name, p);
                true
            });
        }
    }
}

/// Draws what an entity holds in its hands.
#[allow(clippy::too_many_arguments)]
fn held(painter: &mut Painter, assets: &Assets, e: &Entity, part: &Part, slim: bool, rig: Rig, at: &Affine, light: [f32; 3]) {
    let main_left = matches!(e.data, Data::Player { main_left: true, .. });
    for (index, main) in [(slot::MAIN_HAND, true), (slot::OFF_HAND, false)] {
        let Some(item) = &e.equipment[index] else { continue };
        let Some(model) = Model::of(assets, item) else { continue };
        let left = main == main_left;
        let arm = if left { "left_arm" } else { "right_arm" };
        let shift = if slim { if left { -0.5 } else { 0.5 } } else { 0.0 };
        let Some(hand) = locate(part, arm, at, &mut |name, p| {
            pose(rig, e, name, p);
            if name == arm {
                p.pivot[0] += shift;
            }
        }) else {
            continue;
        };
        let baby = e.baby && plain(e.kind) != "armor_stand";
        let (ox, oy, oz) = if baby { (0.0, 1.0, -4.5) } else { (1.0, 2.0, -10.0) };
        let hand = hand.rotate_x(-PI / 2.0).rotate_y(PI).translate([if left { -ox } else { ox } / 16.0, oy / 16.0, oz / 16.0]);
        model.draw(painter, assets, &hand, if left { Context::ThirdLeft } else { Context::ThirdRight }, light);
    }
}

fn step(facing: u8) -> [f32; 3] {
    match facing {
        0 => [0.0, -1.0, 0.0],
        1 => [0.0, 1.0, 0.0],
        2 => [0.0, 0.0, -1.0],
        3 => [0.0, 0.0, 1.0],
        4 => [-1.0, 0.0, 0.0],
        _ => [1.0, 0.0, 0.0],
    }
}

fn y_rot(facing: u8) -> f32 {
    match facing {
        2 => 180.0,
        3 => 0.0,
        4 => 90.0,
        _ => 270.0,
    }
}

/// Draws an item frame and what it holds.
fn frame(painter: &mut Painter, assets: &Assets, entities: &impl Entities, lightmap: &Lightmap, e: &Entity, light: [f32; 3], hidden: bool) {
    let Data::Frame { item, rotation, facing, glow, invisible } = &e.data else { return };
    let invisible = &(*invisible || hidden);
    let rel = [0, 1, 2].map(|k| (e.pos[k] - painter.view.eye[k]) as f32);
    let d = step(*facing);
    let (x_rot, y_rot) = if *facing >= 2 { (0.0, 180.0 - y_rot(*facing)) } else { (-90.0 * d[1], 180.0) };
    let at = Affine::at(rel).translate(d.map(|v| v * 0.46875)).rotate_x(x_rot.to_radians()).rotate_y(y_rot.to_radians());
    if !invisible {
        let name = if *glow { "glow_item_frame" } else { "item_frame" };
        let map = item.as_ref().is_some_and(|i| plain(i.id) == "filled_map");
        let variant = super::model::Variant { model: format!("block/{name}{}", if map { "_map" } else { "" }), x: 0, y: 0, uvlock: false };
        block_model(painter, assets, &variant, &at.translate([-0.5, -0.5, -0.5]), light);
    }
    let Some(item) = item else { return };
    let at = at.translate([0.0, 0.0, if *invisible { 0.5 } else { 0.4375 }]);
    if let Some(picture) = item.map.filter(|_| plain(item.id) == "filled_map").and_then(|id| entities.map(id)) {
        let at = at.rotate_z((f32::from(*rotation % 4) * 90.0).to_radians()).rotate_z(PI).scale([1.0 / 128.0; 3]).translate([-64.0, -64.0, -1.0]);
        let corners = [[0.0, 128.0, -0.01], [128.0, 128.0, -0.01], [128.0, 0.0, -0.01], [0.0, 0.0, -0.01]];
        let light = if *glow { lightmap.color(light::pack(15, 13)) } else { light };
        painter.face(corners.map(|p| at.apply(p)), [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]], &map::texture(picture), &Paint { light, tint: WHITE, mode: Transparency::Cutout }, Some(1.0));
        return;
    }
    let Some(model) = Model::of(assets, item) else { return };
    let at = at.rotate_z((f32::from(*rotation) * 45.0).to_radians()).scale([0.5; 3]);
    model.draw(painter, assets, &at, Context::Fixed, if *glow { WHITE } else { light });
}

fn block_model(painter: &mut Painter, assets: &Assets, variant: &super::model::Variant, at: &Affine, light: [f32; 3]) {
    for q in super::model::bake(assets, variant) {
        let Some(id) = assets.texture_id(&q.texture) else { continue };
        let tint = if q.tint >= 0 { rgb(GRASS) } else { WHITE };
        let shade = q.shade.map_or(1.0, |d| d.shade());
        let light = if q.emission > 0 { WHITE } else { light };
        painter.face(q.pos.map(|p| at.apply(p)), q.uv, assets.texture(id), &Paint { light, tint, mode: Transparency::Cutout }, Some(shade));
    }
}

/// Draws a block or item display entity; a text display is handed back to be drawn with the other text.
fn display<'a>(painter: &mut Painter, assets: &Assets, e: &'a Entity, lightmap: &Lightmap, lit: [f32; 3]) -> Option<Label<'a>> {
    let Data::Display { common, kind } = &e.data else { return None };
    let light = common.brightness.map_or(lit, |(sky, block)| lightmap.color(light::pack(sky.min(15), block.min(15))));
    let f = painter.view.forward;
    let camera_yaw = (-f[0]).atan2(f[2]).to_degrees();
    let camera_pitch = (-f[1]).clamp(-1.0, 1.0).asin().to_degrees();
    let (yaw, pitch) = match common.billboard {
        1 => (camera_yaw - 180.0, e.pitch),
        2 => (e.yaw, -camera_pitch),
        3 => (camera_yaw - 180.0, -camera_pitch),
        _ => (e.yaw, e.pitch),
    };
    let rel = [0, 1, 2].map(|k| (e.pos[k] - painter.view.eye[k]) as f32);
    let scale = common.scale.map(|s| if s.is_finite() { s.clamp(-64.0, 64.0) } else { 1.0 });
    let translation = common.translation.map(|t| if t.is_finite() { t.clamp(-64.0, 64.0) } else { 0.0 });
    let at = Affine::at(rel).rotate_y((-yaw).to_radians()).rotate_x(pitch.to_radians()).translate(translation).quat(common.left_rotation).scale(scale).quat(common.right_rotation);
    match kind {
        DisplayKind::Block { state } => {
            let Some(state) = super::block::decode(*state) else { return None };
            let Some(blockstate) = super::model::Source::blockstate(assets, state.name) else { return None };
            for variant in super::model::variants(blockstate, &state, 0) {
                block_model(painter, assets, &variant, &at, light);
            }
        }
        DisplayKind::Item { item: Some(item), transform } => {
            if let Some(model) = Model::of(assets, item) {
                model.draw(painter, assets, &at.rotate_y(PI), Context::from_id(*transform), light);
            }
        }
        DisplayKind::Text { .. } => return Some(Label::Display { at, kind, light }),
        DisplayKind::Item { item: None, .. } => {}
    }
    None
}

/// Where an entity's name hangs above its feet: the top of its box in its current pose, plus half a block.
fn name_height(e: &Entity) -> f32 {
    let kind = plain(e.kind);
    let height = match (kind, e.pose, &e.data) {
        (_, Stance::Sleeping, _) => 0.2,
        ("player", Stance::Crouching, _) => 1.5,
        ("player", Stance::Swimming | Stance::FallFlying, _) => 0.6,
        ("armor_stand", _, Data::ArmorStand { marker: true, .. }) => 0.0,
        ("armor_stand", _, Data::ArmorStand { small: true, .. }) => standing_height(kind) * 0.5,
        ("slime" | "magma_cube", _, Data::Slime { size }) => standing_height(kind) * f32::from((*size).clamp(1, 127)),
        ("sniffer", _, _) => 2.05,
        _ => standing_height(kind),
    };
    (if e.baby { height * 0.5 } else { height }) + 0.5
}

/// The height of an entity type's standing box, where the game hangs its name.
fn standing_height(kind: &str) -> f32 {
    match kind {
        "block_display" | "interaction" | "item_display" | "lightning_bolt" | "marker" | "text_display" => 0.0,
        "cushion" | "egg" | "ender_pearl" | "experience_bottle" | "eye_of_ender" | "firework_rocket" | "item" | "llama_spit" | "ominous_item_spawner" | "splash_potion" | "lingering_potion" | "snowball" | "fishing_bobber" => 0.25,
        "cod" | "endermite" | "silverfish" | "tadpole" => 0.3,
        "breeze_wind_charge" | "shulker_bullet" | "small_fireball" | "wind_charge" | "wither_skull" => 0.3125,
        "salmon" | "tropical_fish" | "turtle" => 0.4,
        "axolotl" => 0.42,
        "sulfur_cube" => 0.49,
        "magma_cube" | "slime" => 0.52,
        "allay" | "dolphin" | "rabbit" => 0.6,
        "armadillo" => 0.65,
        "cat" | "chest_minecart" | "chicken" | "command_block_minecart" | "fox" | "furnace_minecart" | "hopper_minecart" | "minecart" | "ocelot" | "pufferfish" | "spawner_minecart" | "tnt_minecart" => 0.7,
        "evoker_fangs" | "glow_squid" | "squid" | "vex" => 0.8,
        "guardian" | "wolf" => 0.85,
        "bat" | "parrot" | "pig" | "spider" => 0.9,
        "nautilus" | "zombie_nautilus" => 0.95,
        "copper_golem" | "falling_block" | "tnt" => 0.98,
        "dragon_fireball" | "fireball" | "shulker" => 1.0,
        "panda" => 1.25,
        "goat" | "sheep" => 1.3,
        "cow" | "hoglin" | "mooshroom" | "polar_bear" | "zoglin" => 1.4,
        "donkey" => 1.5,
        "horse" | "mule" | "skeleton_horse" | "zombie_horse" => 1.6,
        "creeper" | "strider" => 1.7,
        "sniffer" => 1.75,
        "breeze" => 1.77,
        "blaze" | "mannequin" | "player" => 1.8,
        "llama" | "trader_llama" => 1.87,
        "snow_golem" => 1.9,
        "drowned" | "evoker" | "husk" | "illusioner" | "piglin" | "piglin_brute" | "pillager" | "villager" | "vindicator" | "wandering_trader" | "witch" | "zombie" | "zombie_villager" | "zombified_piglin" => 1.95,
        "armor_stand" => 1.975,
        "bogged" | "parched" | "skeleton" | "stray" => 1.99,
        "elder_guardian" => 1.9975,
        "end_crystal" => 2.0,
        "ravager" => 2.2,
        "camel" | "camel_husk" => 2.375,
        "wither_skeleton" => 2.4,
        "creaking" | "iron_golem" => 2.7,
        "enderman" | "warden" => 2.9,
        "wither" => 3.5,
        "ghast" | "happy_ghast" => 4.0,
        "ender_dragon" => 8.0,
        "giant" => 12.0,
        _ if kind.ends_with("_boat") || kind.ends_with("_raft") => 0.5625,
        _ => 0.5,
    }
}

/// How far a mob's shadow reaches, as its renderer sets it.
fn shadow_of(e: &Entity) -> f32 {
    let radius = match plain(e.kind) {
        "slime" | "magma_cube" => match e.data {
            Data::Slime { size } => 0.25 * f32::from(size.clamp(1, 127)),
            _ => 0.25,
        },
        "cow" | "mooshroom" | "pig" | "sheep" | "iron_golem" | "panda" | "polar_bear" | "cave_spider" | "goat" | "camel" | "sniffer" | "turtle" => 0.7,
        "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" | "llama" | "trader_llama" => 0.75,
        "spider" => 0.8,
        "chicken" | "rabbit" | "frog" | "parrot" | "bat" | "allay" | "vex" | "silverfish" | "endermite" | "bee" => 0.3,
        "cat" | "ocelot" | "fox" => 0.4,
        "armor_stand" | "item_frame" | "glow_item_frame" | "marker" | "interaction" => 0.0,
        _ => 0.5,
    };
    if e.baby { radius * 0.5 } else { radius }
}

/// Darkens the ground under an entity with the game's round shadow.
#[allow(clippy::too_many_arguments)]
fn shadow<W: Blocks>(painter: &mut Painter, scene: &Scene<'_, W>, lightmap: &Lightmap, sheet: &Sheet, e: &Entity, radius: f32, strength: f32) {
    if !radius.is_finite() || !strength.is_finite() {
        return;
    }
    let (radius, strength) = (radius.min(32.0), strength.clamp(0.0, 1.0));
    let d = [0, 1, 2].map(|k| e.pos[k] - painter.view.eye[k]);
    let pow = ((1.0 - (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) / 256.0) as f32) * strength;
    if radius <= 0.0 || pow <= 0.0 {
        return;
    }
    let depth = (pow / 0.5 - 1.0).min(radius);
    let r = f64::from(radius);
    let (x0, x1, z0, z1) = ((e.pos[0] - r).floor() as i32, (e.pos[0] + r).floor() as i32, (e.pos[2] - r).floor() as i32, (e.pos[2] + r).floor() as i32);
    let (y0, y1) = ((e.pos[1] - f64::from(depth)).floor() as i32, e.pos[1].floor() as i32);
    for z in z0..=z1 {
        for x in x0..=x1 {
            for y in y0..=y1 {
                if !scene.full(x, y - 1, z) {
                    continue;
                }
                let (sky, block) = scene.light(x, y, z);
                let brightness = lightmap.level(sky, block);
                if brightness <= 3 {
                    continue;
                }
                let level = f32::from(brightness) / 15.0;
                let alpha = ((pow - (e.pos[1] - f64::from(y)) as f32 * 0.5) * 0.5 * level / (4.0 - 3.0 * level)).clamp(0.0, 1.0);
                if alpha <= 0.0 {
                    continue;
                }
                let (ax, bx) = (f64::from(x).max(e.pos[0] - r), f64::from(x + 1).min(e.pos[0] + r));
                let (az, bz) = (f64::from(z).max(e.pos[2] - r), f64::from(z + 1).min(e.pos[2] + r));
                if ax >= bx || az >= bz {
                    continue;
                }
                let corner = |cx: f64, cz: f64| [(cx - painter.view.eye[0]) as f32, (f64::from(y) - painter.view.eye[1]) as f32 + 0.001, (cz - painter.view.eye[2]) as f32];
                let uv = |cx: f64, cz: f64| [(0.5 - (cx - e.pos[0]) / 2.0 / r) as f32, (0.5 - (cz - e.pos[2]) / 2.0 / r) as f32];
                let world_pos = [corner(ax, az), corner(ax, bz), corner(bx, bz), corner(bx, az)];
                let uvs = [uv(ax, az), uv(ax, bz), uv(bx, bz), uv(bx, az)];
                let view = world_pos.map(|p| painter.view.to_view(p));
                if !painter.target.visible(&view) {
                    continue;
                }
                let v = [0, 1, 2, 3].map(|i| Vertex { p: view[i], uv: uvs[i], c: WHITE });
                painter.target.blend(&v, &sheet.texture, alpha);
            }
        }
    }
}

/// Draws an item lying on the ground.
fn dropped(painter: &mut Painter, assets: &Assets, e: &Entity, light: [f32; 3]) -> bool {
    let Data::Item { item } = &e.data else { return false };
    let Some(model) = Model::of(assets, item) else { return false };
    let ground = model.placed(&Affine::IDENTITY, Context::Ground);
    let low = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0]].iter().map(|p| ground.apply(*p)[1]).fold(f32::MAX, f32::min);
    let rel = [0, 1, 2].map(|k| (e.pos[k] - painter.view.eye[k]) as f32);
    let at = Affine::at(rel).translate([0.0, 0.1 - low + 0.0625, 0.0]).rotate_y(e.yaw.to_radians());
    model.draw(painter, assets, &at, Context::Ground, light);
    true
}

/// The game draws the own hand with this fixed field of view, whatever the world's.
const HAND_FOV: f32 = 70.0;

/// Draws the account's own hand and held item over the picture, as the game shows them in first person.
pub fn hand<W: Blocks>(entities: &impl Entities, scene: &Scene<'_, W>, assets: &Assets, lightmap: &Lightmap, view: &View, target: &mut Target) {
    let Some(hand) = entities.hand() else { return };
    let eye = view.eye.map(|v| v.floor() as i32);
    let (sky, block) = scene.light(eye[0], eye[1], eye[2]);
    let light = lightmap.color(light::pack(sky, block));
    let camera = Affine { m: [0, 1, 2].map(|r| [view.right[r], view.up[r], -view.forward[r]]), t: [0.0; 3] };
    let left = hand.left_handed;
    let invert = if left { -1.0 } else { 1.0 };
    target.clear_depth();
    target.set_fov(HAND_FOV);
    let mut painter = Painter { target, view };
    if let Some(item) = &hand.main {
        if let Some(model) = Model::of(assets, item) {
            let at = camera.translate([invert * 0.56, -0.52, -0.72]);
            model.draw(&mut painter, assets, &at, if left { Context::FirstLeft } else { Context::FirstRight }, light);
        }
        return;
    }
    let (fallback, fallback_slim) = default_skin(&hand.skin);
    let fetched = hand.skin.url.as_deref().and_then(|u| assets.skins.get(u));
    let slim = if fetched.is_some() { hand.skin.slim } else { fallback_slim };
    let Some(sheet) = fetched.or_else(|| assets.sheet(&fallback)) else { return };
    let Some(model) = assets.figures.layer(if slim { "minecraft:player_slim#main" } else { "minecraft:player#main" }) else { return };
    let arm_name = if left { "left_arm" } else { "right_arm" };
    let Some(arm) = model.find(arm_name) else { return };
    let at = camera
        .translate([invert * 0.64, -0.6, -0.72])
        .rotate_y((invert * 45.0f32).to_radians())
        .translate([invert * -1.0, 3.6, 3.5])
        .rotate_z((invert * 120.0f32).to_radians())
        .rotate_x(200.0f32.to_radians())
        .rotate_y((invert * -135.0f32).to_radians())
        .translate([invert * 5.6, 0.0, 0.0]);
    let hide = hidden_parts(hand.skin.parts);
    painter.part(arm, &at, &sheet, &Paint { light, tint: WHITE, mode: Transparency::Cutout }, &mut |name, p| {
        if hide.contains(&name) {
            return false;
        }
        if name == arm_name {
            p.rotation = [0.0, 0.0, invert * 0.1];
        }
        true
    });
}

/// Draws every entity within `reach` blocks of the camera.
pub fn draw<'e, W: Blocks>(entities: &'e impl Entities, scene: &Scene<'_, W>, assets: &Assets, lightmap: &Lightmap, view: &View, target: &mut Target, reach: f32) -> Vec<Label<'e>> {
    let mut painter = Painter { target, view };
    let shadow_sheet = assets.sheet("misc/shadow");
    let mut labels = Vec::new();
    for e in entities.entities() {
        let d = [0, 1, 2].map(|k| (e.pos[k] - view.eye[k]) as f32);
        if !d.iter().all(|v| v.is_finite()) || d[0].abs() > reach || d[2].abs() > reach || d[1].abs() > 384.0 {
            continue;
        }
        let invisible = e.flags & INVISIBLE != 0;
        let skin = match &e.data {
            Data::Player { skin: Skin { url: Some(url), .. }, .. } => assets.skins.get(url),
            _ => None,
        };
        let probe = [e.pos[0].floor() as i32, (e.pos[1] + 0.5).floor() as i32, e.pos[2].floor() as i32];
        let (sky, block) = scene.light(probe[0], probe[1], probe[2]);
        let lit = lightmap.color(light::pack(sky, block));
        let kind = plain(e.kind);
        if let Some(name) = &e.name {
            let discrete = e.pose == Stance::Crouching;
            let far = if discrete { 32.0 } else { 64.0 };
            if d.iter().map(|v| v * v).sum::<f32>() < far * far && (!invisible || kind == "armor_stand") {
                let at = [d[0], d[1] + name_height(e), d[2]];
                labels.push(Label::Name { at, text: name, discrete, light: lit, glow: lightmap.color(light::pack(sky, block.max(2))) });
            }
        }
        match kind {
            "item" => {
                if dropped(&mut painter, assets, e, lit) && let Some(sheet) = &shadow_sheet {
                    shadow(&mut painter, scene, lightmap, sheet, e, 0.15, 0.75);
                }
                continue;
            }
            "item_frame" | "glow_item_frame" => {
                frame(&mut painter, assets, entities, lightmap, e, lit, invisible);
                continue;
            }
            "block_display" | "item_display" | "text_display" => {
                labels.extend(display(&mut painter, assets, e, lightmap, lit));
                if let (Some(sheet), Data::Display { common, .. }) = (&shadow_sheet, &e.data) {
                    shadow(&mut painter, scene, lightmap, sheet, e, common.shadow_radius, common.shadow_strength);
                }
                continue;
            }
            _ => {}
        }
        let Some(figure) = figure(e, assets, skin.as_ref()) else { continue };
        if !invisible && let Some(sheet) = &shadow_sheet {
            shadow(&mut painter, scene, lightmap, sheet, e, shadow_of(e), 1.0);
        }
        let at = placement(e, view.eye, figure.scale);
        for coat in figure.coats.iter().filter(|_| !invisible) {
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
        if figure.rig != Rig::Plain {
            armor(&mut painter, assets, e, kind, figure.slim, figure.rig, &at, lit);
            if let Some(coat) = figure.coats.first() {
                held(&mut painter, assets, e, coat.part, figure.slim, figure.rig, &at, lit);
            }
        }
    }
    labels
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
