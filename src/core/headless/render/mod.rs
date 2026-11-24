//! Pictures of what a connection-only account sees, drawn on the CPU like the game draws them: block models and
//! textures from the game's own client jar, smooth lighting, biome tints, water, sky, sun and fog.
//!
//! [`render`] is a pure function of the world, the camera and the art; [`shared`] loads the art once for every
//! account and drops it after a minute without a picture, so nothing costs anything until a picture is asked for.

#[cfg(test)]
mod anvil;
mod assets;
mod block;
mod entity;
mod light;
mod model;
mod random;
mod raster;
mod scene;
mod sky;
mod states;

use serde_json::Value;

pub use self::assets::{Assets, shared};
pub use self::entity::{Data, Display, DisplayKind, Entities, Entity, Hand, Item, Nobody, Pose, Skin, Span, Text, slot};
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
pub fn render(world: &impl Blocks, biomes: &[Biome], camera: &Camera, sky: &Sky, width: u32, height: u32, assets: &Assets) -> image::RgbaImage {
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
    later.draw(&mut target);
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
        let image = render(&Flat, &[Biome::default()], &camera, &Sky::default(), 320, 180, &assets);
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
        let image = render(&Flat, &[], &camera, &sky, 0, 100_000, &assets);
        assert_eq!(image.dimensions(), (1, MAX_SIDE));
        assert!(image.pixels().any(|p| p.0[..3] != [0, 0, 0]), "a camera put back on the map still sees something");
    }

    /// Renders the scenes in `RENDER_SCENES` (lines of `name x y z yaw pitch time [fov]`, `y` as `~` for eyes above the ground) of the world in `RENDER_WORLD` to
    /// `RENDER_OUT/ours-<name>.png`, at `RENDER_SIZE` (`1280x720`).
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
            let image = render(&world, &biomes, &camera, &sky, w, h, &assets);
            eprintln!("{} eye {y:.2} {:?}", f[0], start.elapsed());
            image.save(out.join(format!("ours-{}.png", f[0]))).unwrap();
        }
    }
}
