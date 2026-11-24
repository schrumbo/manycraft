//! Pictures of what a connection-only account sees, drawn on the CPU like the game draws them: block models and
//! textures from the game's own client jar, smooth lighting, biome tints, water, sky, sun and fog.
//!
//! [`render`] is a pure function of the world, the camera and the art; [`shared`] loads the art once for every
//! account and drops it after a minute without a picture, so nothing costs anything until a picture is asked for.

#[cfg(test)]
mod anvil;
mod assets;
mod actor;
mod block;
mod entity;
mod figure;
mod font;
pub mod hud;
mod item;
mod label;
mod skins;
mod light;
mod map;
mod model;
mod random;
mod raster;
mod scene;
mod sky;
mod states;

use serde_json::Value;

pub use self::assets::{Assets, shared};
pub use self::entity::{Data, Display, DisplayKind, Entities, Entity, Hand, Item, MapPicture, Nobody, Pose, Skin, Text, slot};
use self::light::Lightmap;
use self::raster::{Fog, Later, Target};
use self::scene::{Scene, View};
use self::sky::Atmosphere;

/// The blocks a picture is drawn from.
pub trait Blocks {
    /// The global block state id at a block (Minecraft 26.3 ids); 0 for air and for blocks not loaded.
    fn state(&self, x: i32, y: i32, z: i32) -> u32;
    /// The biome at a block, as an index into the biomes passed to [`render`].
    fn biome(&self, x: i32, y: i32, z: i32) -> u16;
    /// Sky and block light (0..15) at a block, or `None` where the server sent no light.
    fn light(&self, x: i32, y: i32, z: i32) -> Option<(u8, u8)>;
    /// The lowest block height of the dimension.
    fn floor(&self) -> i32;
    /// How many blocks high the dimension is.
    fn height(&self) -> i32;
}

/// Where a picture looks from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// The eyes, in world coordinates.
    pub pos: [f64; 3],
    /// Degrees, as the game sends them: yaw 0 faces south, 90 west; pitch 90 looks straight down.
    pub yaw: f32,
    pub pitch: f32,
    /// Vertical field of view in degrees; the game's default is 70.
    pub fov: f32,
    /// Render distance in chunks (1..=4): blocks farther than this many chunks are not drawn and fog ends there.
    pub distance: u8,
}

/// Time and weather; the day at noon without rain when unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sky {
    /// The day time in ticks (0..24000 is one day), as the server sends it.
    pub time: Option<i64>,
    /// Rain and thunder strength, 0..1.
    pub rain: f32,
    pub thunder: f32,
}

/// How a biome changes grass colour on top of its climate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Modifier {
    #[default]
    None,
    DarkForest,
    Swamp,
}

/// What a picture needs to know of a biome: its climate and colours.
#[derive(Clone, Debug, PartialEq)]
pub struct Biome {
    pub temperature: f32,
    pub downfall: f32,
    pub water: u32,
    pub grass: Option<u32>,
    pub foliage: Option<u32>,
    pub dry_foliage: Option<u32>,
    pub modifier: Modifier,
    pub sky: Option<u32>,
    pub fog: Option<u32>,
}

impl Default for Biome {
    fn default() -> Self {
        Self { temperature: 0.8, downfall: 0.4, water: 0x3F76E4, grass: None, foliage: None, dry_foliage: None, modifier: Modifier::None, sky: Some(0x78A7FF), fog: None }
    }
}

fn color(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::String(s) => u32::from_str_radix(s.strip_prefix('#')?, 16).ok(),
        Value::Number(n) => n.as_i64().map(|c| (c & 0xFF_FFFF) as u32),
        Value::Object(o) => color(o.get("argument")),
        _ => None,
    }
}

impl Biome {
    /// Reads a biome from its registry entry or data pack file (`temperature`, `downfall`, `effects` and the visual
    /// attributes); what is missing stays as in plains.
    pub fn from_json(v: &Value) -> Self {
        let plains = Self::default();
        let effects = v.get("effects");
        let attributes = v.get("attributes");
        let f = |k: &str, d: f32| v.get(k).and_then(Value::as_f64).map_or(d, |x| x as f32);
        Self {
            temperature: f("temperature", plains.temperature),
            downfall: f("downfall", plains.downfall),
            water: color(effects.and_then(|e| e.get("water_color"))).unwrap_or(plains.water),
            grass: color(effects.and_then(|e| e.get("grass_color"))),
            foliage: color(effects.and_then(|e| e.get("foliage_color"))),
            dry_foliage: color(effects.and_then(|e| e.get("dry_foliage_color"))),
            modifier: match effects.and_then(|e| e.get("grass_color_modifier")).and_then(Value::as_str) {
                Some("dark_forest") => Modifier::DarkForest,
                Some("swamp") => Modifier::Swamp,
                _ => Modifier::None,
            },
            sky: color(attributes.and_then(|a| a.get("minecraft:visual/sky_color"))).or(plains.sky),
            fog: color(attributes.and_then(|a| a.get("minecraft:visual/fog_color"))),
        }
    }
}

impl View {
    fn new(camera: &Camera) -> Self {
        let (yaw, pitch) = (camera.yaw.to_radians(), camera.pitch.to_radians());
        let forward = [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
        let right = [-yaw.cos(), 0.0, -yaw.sin()];
        let up = [right[1] * forward[2] - right[2] * forward[1], right[2] * forward[0] - right[0] * forward[2], right[0] * forward[1] - right[1] * forward[0]];
        Self { eye: camera.pos, right, up, forward }
    }
}

const NOON: i64 = 6000;

/// Draws what `camera` sees of `world` as an RGBA picture of `width` × `height`, rows top down.
/// Sides are clamped to 1..=4096, the render distance to 1..=4; a camera off the map or not a number is moved onto it.
pub fn render(world: &impl Blocks, biomes: &[Biome], entities: &impl Entities, camera: &Camera, sky: &Sky, width: u32, height: u32, assets: &Assets) -> image::RgbaImage {
    let (w, h) = (width.clamp(1, MAX_SIDE) as usize, height.clamp(1, MAX_SIDE) as usize);
    let camera = &sane_camera(camera, world);
    let sky = &Sky { time: sky.time, rain: unit(sky.rain), thunder: unit(sky.thunder) };
    let view = View::new(camera);
    let ticks = sky.time.unwrap_or(NOON);
    let time = ticks.rem_euclid(24000) as f32;
    let eye = camera.pos.map(|v| v.floor() as i32);
    let here = biomes.get(usize::from(world.biome(eye[0], eye[1], eye[2]))).cloned().unwrap_or_default();
    let distance = camera.distance.clamp(1, MAX_DISTANCE);
    let air = Atmosphere::new(ticks, sky.rain, sky.thunder, &here, distance, view.forward, camera.pos[1]);
    let end = f32::from(distance) * 16.0;
    let fog = Fog { color: air.fog, start: end - (end / 10.0).clamp(4.0, 64.0), end, sphere: [-160.0 * sky.rain, 1024.0 - 256.0 * sky.rain] };
    let vertical = [view.right[1], view.up[1], view.forward[1]];
    let mut target = Target::new(w, h, camera.fov.clamp(10.0, 170.0), fog, vertical);
    sky::paint(&mut target, &view, &air, assets);
    let lightmap = Lightmap::new(time, sky.rain, sky.thunder);
    let mut scene = Scene::new(world, assets, biomes, camera.pos, i32::from(distance));
    let mut later = Later::default();
    scene.draw(&view, &lightmap, &mut target, &mut later);
    let mut labels = actor::draw(entities, &scene, assets, &lightmap, &view, &mut target, end + 16.0);
    later.draw(&mut target);
    if let Some(font) = &assets.font {
        label::draw(&mut figure::Painter { target: &mut target, view: &view }, font, &mut labels);
    }
    actor::hand(entities, &scene, assets, &lightmap, &view, &mut target);
    if let Some(texture) = &assets.vignette {
        let (sky_light, block_light) = world.light(eye[0], eye[1], eye[2]).unwrap_or((15, 0));
        target.vignette(texture, light::vignette(sky_light, block_light, light::sky_level(time, sky.rain, sky.thunder)));
    }
    let mut out = image::RgbaImage::new(w as u32, h as u32);
    for (px, c) in out.pixels_mut().zip(&target.color) {
        *px = image::Rgba([c[0], c[1], c[2], 1.0].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
    }
    out
}

const MAX_SIDE: u32 = 4096;
const MAX_DISTANCE: u8 = 4;
const MAX_HORIZONTAL: f64 = 30_000_000.0;

fn unit(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

fn sane_camera(camera: &Camera, world: &impl Blocks) -> Camera {
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    let angle = |v: f32| if v.is_finite() { v } else { 0.0 };
    let floor = f64::from(world.floor());
    let top = floor + f64::from(world.height().max(0));
    let [x, y, z] = camera.pos.map(finite);
    Camera {
        pos: [x.clamp(-MAX_HORIZONTAL, MAX_HORIZONTAL), y.clamp(floor - 64.0, top + 64.0), z.clamp(-MAX_HORIZONTAL, MAX_HORIZONTAL)],
        yaw: angle(camera.yaw),
        pitch: angle(camera.pitch).clamp(-90.0, 90.0),
        fov: if camera.fov.is_finite() { camera.fov.clamp(10.0, 170.0) } else { 70.0 },
        distance: camera.distance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flat;

    impl Blocks for Flat {
        fn state(&self, x: i32, y: i32, z: i32) -> u32 {
            match y {
                ..62 => block::encode("stone", &[]).unwrap(),
                62 => block::encode("grass_block", &[("snowy", "false")]).unwrap(),
                63 if x == 2 && z == 6 => block::encode("short_grass", &[]).unwrap(),
                63..66 if x == 0 && z == 6 => block::encode("oak_log", &[("axis", "y")]).unwrap(),
                _ => 0,
            }
        }

        fn biome(&self, _: i32, _: i32, _: i32) -> u16 {
            0
        }

        fn light(&self, _: i32, _: i32, _: i32) -> Option<(u8, u8)> {
            None
        }

        fn floor(&self) -> i32 {
            -64
        }

        fn height(&self) -> i32 {
            384
        }
    }

    fn jar() -> Option<std::path::PathBuf> {
        let jar = crate::core::paths::data_dir().join("versions/26.3/26.3.jar");
        jar.exists().then_some(jar)
    }

    #[test]
    fn a_flat_world_shows_sky_above_and_grass_below() {
        let Some(jar) = jar() else { return };
        let assets = Assets::open(&jar).unwrap();
        let camera = Camera { pos: [0.5, 64.62, 0.5], yaw: 0.0, pitch: 0.0, fov: 70.0, distance: 2 };
        let start = std::time::Instant::now();
        let image = render(&Flat, &[Biome::default()], &Nobody, &camera, &Sky::default(), 320, 180, &assets);
        let took = start.elapsed();
        image.save(std::env::temp_dir().join("manycraft-render-flat.png")).ok();
        let top = image.get_pixel(160, 2).0;
        let bottom = image.get_pixel(160, 178).0;
        assert!(top[2] > top[0] && top[2] > 200, "sky is blue: {top:?}");
        assert!(bottom[1] > bottom[0] && bottom[1] > bottom[2], "grass is green: {bottom:?}");
        assert!(took.as_secs_f32() < 2.0, "{took:?}");
    }

    #[test]
    fn odd_input_draws_a_picture_instead_of_failing() {
        let Some(jar) = jar() else { return };
        let assets = Assets::open(&jar).unwrap();
        let camera = Camera { pos: [f64::NAN, 1e12, f64::INFINITY], yaw: f32::NAN, pitch: 400.0, fov: f32::NAN, distance: 255 };
        let sky = Sky { time: Some(i64::MIN), rain: f32::NAN, thunder: f32::INFINITY };
        let image = render(&Flat, &[], &Nobody, &camera, &sky, 0, 100_000, &assets);
        assert_eq!(image.dimensions(), (1, MAX_SIDE));
        assert!(image.pixels().any(|p| p.0[..3] != [0, 0, 0]), "a camera put back on the map still sees something");
    }

    struct Crowd(Vec<Entity>);

    impl Entities for Crowd {
        fn entities(&self) -> &[Entity] {
            &self.0
        }

        fn hand(&self) -> Option<&Hand> {
            None
        }
    }

    struct Holding(Vec<Entity>, Hand);

    impl Entities for Holding {
        fn entities(&self) -> &[Entity] {
            &self.0
        }

        fn hand(&self) -> Option<&Hand> {
            Some(&self.1)
        }
    }

    struct Shot {
        entities: Vec<Entity>,
        hand: Option<Hand>,
        map: Option<MapPicture>,
    }

    impl Entities for Shot {
        fn entities(&self) -> &[Entity] {
            &self.entities
        }

        fn hand(&self) -> Option<&Hand> {
            self.hand.as_ref()
        }

        fn map(&self, id: i32) -> Option<&MapPicture> {
            self.map.as_ref().filter(|_| id == 0)
        }
    }

    struct Mapped(Vec<Entity>, MapPicture);

    impl Entities for Mapped {
        fn entities(&self) -> &[Entity] {
            &self.0
        }

        fn hand(&self) -> Option<&Hand> {
            None
        }

        fn map(&self, id: i32) -> Option<&MapPicture> {
            (id == 7).then_some(&self.1)
        }
    }

    #[test]
    fn a_filled_map_in_a_frame_shows_its_picture() {
        let Some(jar) = jar() else { return };
        let assets = Assets::open(&jar).unwrap();
        let mut frame = crowd_entity("item_frame 0.5 64.5 2.5 180 item=filled_map facing=2").unwrap();
        if let Data::Frame { item: Some(item), .. } = &mut frame.data {
            item.map = Some(7);
        }
        let mut colors = [12 * 4 + 2; 128 * 128];
        for z in 0..64 {
            colors[z * 128..z * 128 + 64].fill(28 * 4 + 2);
        }
        let quarter = MapPicture { colors: std::sync::Arc::new(colors) };
        let camera = Camera { pos: [0.5, 64.5, 0.5], yaw: 0.0, pitch: 0.0, fov: 70.0, distance: 2 };
        let shown = render(&Flat, &[Biome::default()], &Mapped(vec![frame.clone()], quarter), &camera, &Sky::default(), 64, 64, &assets);
        let unknown = render(&Flat, &[Biome::default()], &Crowd(vec![frame]), &camera, &Sky::default(), 64, 64, &assets);
        let (north_west, south_east) = (shown.get_pixel(28, 28).0, shown.get_pixel(36, 36).0);
        assert!(north_west[0] > north_west[1] * 2 && north_west[0] > north_west[2] * 2, "north-west quarter is red: {north_west:?}");
        assert!(south_east[2] > south_east[0] * 2, "the rest is water blue: {south_east:?}");
        assert_ne!(shown.get_pixel(36, 36), unknown.get_pixel(36, 36));
    }

    fn item(id: &str) -> Item {
        Item { id: Box::leak(format!("minecraft:{id}").into_boxed_str()), count: 1, glint: false, dye: None, trim: None, profile: None, map: None }
    }

    /// An entity from `kind x y z yaw [key=value …]`, the scene format the comparison shots share with the server
    /// commands that place the real ones.
    fn crowd_entity(line: &str) -> Option<Entity> {
        let f: Vec<&str> = line.split_whitespace().collect();
        let n = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
        let kv = |k: &str| f.iter().skip(5).find_map(|p| p.strip_prefix(k).and_then(|r| r.strip_prefix('=')));
        let flag = |k: &str| f.iter().skip(5).any(|p| *p == k);
        let kind: &'static str = Box::leak(format!("minecraft:{}", f.first()?).into_boxed_str());
        let yaw = n(4)? as f32;
        let mut equipment: [Option<Item>; 6] = Default::default();
        for (key, slot) in [("main", slot::MAIN_HAND), ("off", slot::OFF_HAND), ("feet", slot::FEET), ("legs", slot::LEGS), ("chest", slot::CHEST), ("head", slot::HEAD)] {
            equipment[slot] = kv(key).map(item);
        }
        let uuid = kv("uuid").and_then(|u| uuid::Uuid::parse_str(u).ok()).unwrap_or_default();
        let data = match f[0] {
            "player" => Data::Player { skin: Skin { uuid, parts: 0x7F, ..Skin::default() }, main_left: false },
            "villager" | "zombie_villager" => Data::Villager { kind: kv("type").unwrap_or("plains").into(), profession: kv("profession").unwrap_or("none").into(), level: kv("level").and_then(|v| v.parse().ok()).unwrap_or(1) },
            "sheep" => Data::Sheep { color: kv("color").and_then(|v| v.parse().ok()).unwrap_or(0), sheared: flag("sheared") },
            "horse" => Data::Horse { variant: kv("variant").and_then(|v| v.parse().ok()).unwrap_or(0) },
            "wolf" => Data::Wolf { tame: flag("tame"), collar: 14, variant: kv("variant").unwrap_or("pale").into() },
            "cat" => Data::Cat { variant: kv("variant").unwrap_or("tabby").into(), collar: 14 },
            "slime" | "magma_cube" => Data::Slime { size: kv("size").and_then(|v| v.parse().ok()).unwrap_or(1) },
            "item" => Data::Item { item: item(kv("item")?) },
            "item_frame" | "glow_item_frame" => Data::Frame { item: kv("item").map(|i| Item { map: kv("map").and_then(|v| v.parse().ok()), ..item(i) }), rotation: kv("rot").and_then(|v| v.parse().ok()).unwrap_or(0), facing: kv("facing").and_then(|v| v.parse().ok()).unwrap_or(3), glow: f[0] == "glow_item_frame", invisible: flag("invisible") },
            "armor_stand" => Data::ArmorStand { small: flag("small"), arms: flag("arms"), no_plate: false, marker: false, poses: [[0.0; 3], [0.0; 3], [-10.0, 0.0, -10.0], [-15.0, 0.0, 10.0], [-1.0, 0.0, -1.0], [1.0, 0.0, 1.0]] },
            "block_display" | "item_display" | "text_display" => {
                let scale = kv("scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                let common = Display { scale: [scale; 3], billboard: kv("billboard").and_then(|v| v.parse().ok()).unwrap_or(0), ..Display::default() };
                let kind = match f[0] {
                    "block_display" => DisplayKind::Block { state: block::encode(kv("block")?, &[])? },
                    "item_display" => DisplayKind::Item { item: kv("item").map(item), transform: kv("transform").and_then(|v| v.parse().ok()).unwrap_or(0) },
                    _ => DisplayKind::Text {
                        text: vec![crate::core::chat_text::Run { text: kv("text")?.replace('_', " ").replace('|', "\n"), color: kv("color").and_then(|c| u32::from_str_radix(c, 16).ok()), ..Default::default() }],
                        line_width: kv("width").and_then(|v| v.parse().ok()).unwrap_or(200),
                        background: kv("background").and_then(|c| u32::from_str_radix(c, 16).ok()).unwrap_or(0x4000_0000),
                        opacity: 255,
                        flags: kv("flags").and_then(|v| v.parse().ok()).unwrap_or(0),
                    },
                };
                Data::Display { common, kind }
            }
            _ => kv("variant").map(|v| Data::Variant(v.into())).unwrap_or_default(),
        };
        Some(Entity {
            id: 0,
            kind,
            pos: [n(1)?, n(2)?, n(3)?],
            yaw,
            pitch: kv("pitch").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            head_yaw: kv("head").and_then(|v| v.parse().ok()).unwrap_or(yaw),
            baby: flag("baby"),
            pose: if flag("sneak") { Pose::Crouching } else { Pose::Standing },
            flags: if flag("hidden") { 0x20 } else { 0 },
            name: kv("name").map(|t| vec![crate::core::chat_text::Run { text: t.replace('_', " "), color: kv("color").and_then(|c| u32::from_str_radix(c, 16).ok()), ..Default::default() }]),
            equipment,
            data,
        })
    }

    #[test]
    fn a_crowd_of_mobs_draws_inside_the_budget() {
        let Some(jar) = jar() else { return };
        let assets = Assets::open(&jar).unwrap();
        let kinds = ["player main=diamond_sword off=shield head=iron_helmet chest=diamond_chestplate", "villager profession=farmer level=2", "zombie main=iron_shovel legs=golden_leggings feet=leather_boots", "skeleton main=bow", "creeper", "cow", "pig", "sheep color=14", "chicken", "horse variant=258", "wolf tame", "cat", "iron_golem", "enderman", "spider", "slime size=2", "armor_stand arms head=diamond_helmet chest=iron_chestplate main=apple", "item item=diamond", "item item=oak_log", "item_frame item=compass facing=2", "block_display block=glowstone scale=0.5", "item_display item=golden_apple billboard=1", "player sneak main=torch uuid=00000000-0000-0000-0000-00000000000f", "item_frame item=stone rot=1 facing=2"];
        let row: Vec<Entity> = kinds.iter().enumerate().filter_map(|(i, k)| {
            let (kind, rest) = k.split_once(' ').unwrap_or((k, ""));
            crowd_entity(&format!("{kind} {} 63 {} 180 {rest}", (i % 8) as f64 * 2.0 - 7.0, 8.0 + (i / 8) as f64 * 3.0))
        }).collect();
        assert_eq!(row.len(), kinds.len());
        let camera = Camera { pos: [0.5, 64.62, 0.5], yaw: 0.0, pitch: 10.0, fov: 70.0, distance: 2 };
        let bare = render(&Flat, &[Biome::default()], &Nobody, &camera, &Sky::default(), 640, 360, &assets);
        let image = render(&Flat, &[Biome::default()], &Crowd(row.clone()), &camera, &Sky::default(), 640, 360, &assets);
        let out = crate::core::paths::data_dir().join("..").join("render-crowd.png");
        image.save(std::env::temp_dir().join("manycraft-render-crowd.png")).ok();
        let _ = out;
        let props: Vec<Entity> = row.iter().skip(16).enumerate().map(|(i, e)| { let mut e = e.clone(); e.pos = [i as f64 * 1.2 - 4.0, 63.0 + if matches!(e.data, Data::Frame { .. } | Data::Display { .. }) { 1.0 } else { 0.0 }, 4.0]; e }).collect();
        render(&Flat, &[Biome::default()], &Crowd(props), &camera, &Sky::default(), 640, 360, &assets).save(std::env::temp_dir().join("manycraft-render-props.png")).ok();
        let labels: Vec<Entity> = ["text_display -2.5 64.2 4 180 text=Welcome_to_the|§_spawn_§ flags=1 billboard=0", "text_display 0.5 65.4 4.5 180 text=Left_aligned|text_on_a|red_background flags=8 background=A0FF0000 billboard=3", "text_display 3.5 64.2 4 180 text=Right|aligned_lines flags=18 color=FFD700", "player -1.5 63 5 180 name=Steve main=diamond_sword", "villager 1.5 63 5 180 name=Farmer_Bob color=55FF55", "armor_stand 3 63 3.5 180 hidden name=Hidden_hologram head=golden_helmet", "player 4.5 63 6 180 sneak name=Sneaky"].iter().filter_map(|l| crowd_entity(l)).collect();
        assert_eq!(labels.len(), 7);
        render(&Flat, &[Biome::default()], &Crowd(labels), &camera, &Sky::default(), 640, 360, &assets).save(std::env::temp_dir().join("manycraft-render-labels.png")).ok();
        let solo = vec![crowd_entity("player 0.5 63 3.5 180 main=diamond_sword").unwrap()];
        render(&Flat, &[Biome::default()], &Crowd(solo.clone()), &Camera { pos: [0.5, 64.62, 0.5], yaw: 0.0, pitch: 10.0, fov: 70.0, distance: 2 }, &Sky::default(), 320, 320, &assets).save(std::env::temp_dir().join("manycraft-render-solo-front.png")).ok();
        render(&Flat, &[Biome::default()], &Crowd(solo), &Camera { pos: [3.5, 64.62, 3.5], yaw: 90.0, pitch: 10.0, fov: 70.0, distance: 2 }, &Sky::default(), 320, 320, &assets).save(std::env::temp_dir().join("manycraft-render-solo-side.png")).ok();
        for (name, main) in [("sword", Some(item("diamond_sword"))), ("empty", None), ("block", Some(item("oak_log")))] {
            let hand = Hand { main, left_handed: false, skin: Skin { parts: 0x7F, ..Skin::default() } };
            render(&Flat, &[Biome::default()], &Holding(row.clone(), hand), &camera, &Sky::default(), 640, 360, &assets).save(std::env::temp_dir().join(format!("manycraft-render-hand-{name}.png"))).ok();
        }
        let changed = bare.pixels().zip(image.pixels()).filter(|(a, b)| a != b).count();
        assert!(changed > 640 * 360 / 20, "the mobs cover part of the picture: {changed}");
        let many: Vec<Entity> = (0..200).map(|i| { let mut e = row[i % row.len()].clone(); e.pos = [(i % 20) as f64 - 10.0, 63.0, 6.0 + (i / 20) as f64 * 2.0]; e }).collect();
        let start = std::time::Instant::now();
        render(&Flat, &[Biome::default()], &Crowd(many), &camera, &Sky::default(), 1280, 720, &assets);
        let took = start.elapsed();
        eprintln!("200 entities at 1280x720: {took:?}");
        assert!(took.as_secs_f32() < if cfg!(debug_assertions) { 30.0 } else { 1.0 }, "{took:?}");
    }

    /// Renders the scenes in `RENDER_SCENES` (lines of `name x y z yaw pitch time [fov]`, `y` as `~` for eyes above the ground) of the world in `RENDER_WORLD` to
    /// `RENDER_OUT/ours-<name>.png`, at `RENDER_SIZE` (`1280x720`), holding the item in `RENDER_HAND` (`<item|-> [uuid]`, `-` the bare arm of `uuid`'s default skin) when set, with the raw map
    /// colours in the file `RENDER_MAP` as map 0.
    #[test]
    #[ignore]
    fn shots() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} not set"));
        let jar = jar().expect("no 26.3 client jar");
        let assets = Assets::open(&jar).unwrap();
        let (w, h) = env("RENDER_SIZE").split_once('x').map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap())).unwrap_or((1280, 720));
        let out = std::path::PathBuf::from(env("RENDER_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        for line in std::fs::read_to_string(env("RENDER_SCENES")).unwrap().lines().filter(|l| !l.trim().is_empty()) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let n = |i: usize| f[i].parse::<f64>().unwrap();
            let world = anvil::World::load(std::path::Path::new(&env("RENDER_WORLD")), n(1).floor() as i32, n(3).floor() as i32, 3).unwrap();
            let biomes = world.biomes(&jar).unwrap();
            let (x, z) = (n(1), n(3));
            let y = if f[2] == "~" {
                let ground = (-64..320).rev().find(|&y| {
                    let s = block::decode(world.state(x.floor() as i32, y, z.floor() as i32)).map_or("air", |s| s.name);
                    !(s.ends_with("air") || s.ends_with("leaves") || s.ends_with("log") || s.ends_with("_grass") || s.ends_with("fern") || s.ends_with("bush") || s == "water")
                });
                f64::from(ground.unwrap_or(64) + 1) + 1.62
            } else {
                n(2)
            };
            let fov = if f.len() > 7 { n(7) as f32 } else { 70.0 };
            let camera = Camera { pos: [x, y, z], yaw: n(4) as f32, pitch: n(5) as f32, fov, distance: 2 };
            let sky = Sky { time: Some(n(6) as i64), rain: 0.0, thunder: 0.0 };
            let start = std::time::Instant::now();
            let crowd: Vec<Entity> = std::env::var("RENDER_ENTITIES").ok().map(|p| std::fs::read_to_string(p).unwrap().lines().filter(|l| l.starts_with(f[0])).filter_map(|l| crowd_entity(l.split_once(' ')?.1)).collect()).unwrap_or_default();
            let hand = std::env::var("RENDER_HAND").ok().map(|held| {
                let (held, uuid) = held.split_once(' ').unwrap_or((&held, ""));
                let skin = Skin { uuid: uuid::Uuid::parse_str(uuid).unwrap_or_default(), parts: 0x7F, ..Skin::default() };
                Hand { main: (held != "-").then(|| item(held)), left_handed: false, skin }
            });
            let map = std::env::var("RENDER_MAP").ok().map(|p| MapPicture { colors: std::sync::Arc::new(std::fs::read(p).unwrap().try_into().expect("128 × 128 colour bytes")) });
            let image = render(&world, &biomes, &Shot { entities: crowd, hand, map }, &camera, &sky, w, h, &assets);
            eprintln!("{} eye {y:.2} {:?}", f[0], start.elapsed());
            image.save(out.join(format!("ours-{}.png", f[0]))).unwrap();
        }
    }
}