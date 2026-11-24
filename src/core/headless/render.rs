//! Pictures of what a connection-only account sees, drawn on the CPU from the chunks it keeps: blocks in their map
//! colours with face shading, water and plants, sky and distance fog, from the player's eyes or from above. One worker
//! thread draws for every account, in the order asked.

use std::sync::mpsc::{self, Sender};
use std::sync::{LazyLock, Mutex};

use super::v777::MAP_COLORS;
use super::world::{self, Class, Dimension, Look, Section, Snapshot};

const EYE_HEIGHT: f64 = 1.62;
const FOV_Y: f32 = 70.0;
const REACH: f32 = 96.0;
const FOG_FROM: f32 = 0.55;
const ABOVE_SPAN: f32 = 104.0;
const PLANT_NEAR: f32 = 2.5;
const SKY_TOP: [f32; 3] = [0.47, 0.65, 1.0];
const SKY_HORIZON: [f32; 3] = [0.75, 0.85, 1.0];
const VOID: [f32; 3] = [0.08, 0.08, 0.1];
const WATER_SURFACE: f32 = 0.45;
const WATER_DEPTH: f32 = 0.12;
const PLANT_STEM: f32 = 0.16;

/// Where a picture looks from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Through the player's eyes, the way it faces.
    Eyes,
    /// Straight down on the player, north up.
    Above,
}

/// One picture to draw.
#[derive(Clone, Debug)]
pub struct Request {
    pub world: Snapshot,
    /// Where the player stands (feet).
    pub position: [f64; 3],
    /// Yaw and pitch in degrees, as the game sends them.
    pub rotation: [f32; 2],
    pub width: u32,
    pub height: u32,
}

type Job = Box<dyn FnOnce() + Send>;

static QUEUE: LazyLock<Mutex<Option<Sender<Job>>>> = LazyLock::new(|| Mutex::new(None));

/// Draws `request` from each of `views` on the drawing thread, from one decoded scene, and hands the RGBA pixels to
/// `done` there, in the order of `views`.
pub fn queue(request: Request, views: Vec<View>, done: impl FnOnce(Vec<Vec<u8>>) + Send + 'static) {
    let job: Job = Box::new(move || {
        let scene = Scene::new(&request.world);
        done(views.into_iter().map(|view| paint(&scene, &request, view)).collect());
    });
    let mut queue = QUEUE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let job = match queue.as_ref() {
        Some(q) => match q.send(job) {
            Ok(()) => return,
            Err(back) => back.0,
        },
        None => job,
    };
    let (tx, rx) = mpsc::channel::<Job>();
    let started = std::thread::Builder::new().name("headless-pictures".into()).spawn(move || {
        while let Ok(job) = rx.recv() {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                eprintln!("a picture failed to draw");
            }
        }
    });
    if started.is_ok() && tx.send(job).is_ok() {
        *queue = Some(tx);
    }
}

#[cfg(test)]
fn draw(request: &Request, view: View) -> Vec<u8> {
    paint(&Scene::new(&request.world), request, view)
}

fn paint(scene: &Scene, request: &Request, view: View) -> Vec<u8> {
    let (w, h) = (request.width.max(1), request.height.max(1));
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    match view {
        View::Eyes => eyes(scene, request, w, h, &mut rgba),
        View::Above => above(scene, request, w, h, &mut rgba),
    }
    rgba
}

struct Scene {
    origin: (i32, i32),
    side: i32,
    dimension: Dimension,
    levels: i32,
    filled: i32,
    sections: Vec<Option<Section>>,
}

impl Scene {
    fn new(world: &Snapshot) -> Self {
        let dimension = world.dimension;
        let levels = (dimension.height / 16).clamp(0, world::MAX_SECTIONS as i32);
        let (mut lo, mut hi) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for ((x, z), _) in &world.chunks {
            lo = (lo.0.min(*x), lo.1.min(*z));
            hi = (hi.0.max(*x), hi.1.max(*z));
        }
        if world.chunks.is_empty() {
            return Self { origin: (0, 0), side: 0, dimension, levels, filled: 0, sections: Vec::new() };
        }
        let side = (hi.0 - lo.0).max(hi.1 - lo.1) + 1;
        let mut scene = Self { origin: lo, side, dimension, levels, filled: 0, sections: Vec::new() };
        scene.sections.resize_with((side * side * levels) as usize, || None);
        for ((x, z), packed) in &world.chunks {
            let Ok(found) = world::sections(packed, dimension) else { continue };
            for (y, section) in found.into_iter().enumerate() {
                if let Some(slot) = scene.slot(*x, y as i32, *z) {
                    scene.sections[slot] = section;
                }
            }
        }
        for ([x, y, z], state) in &world.changes {
            scene.set(*x, *y, *z, world::look(*state));
        }
        scene.filled = scene.sections.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| i as i32 % levels + 1).max().unwrap_or(0);
        scene
    }

    fn slot(&self, cx: i32, sy: i32, cz: i32) -> Option<usize> {
        let (x, z) = (cx - self.origin.0, cz - self.origin.1);
        (x >= 0 && z >= 0 && x < self.side && z < self.side && sy >= 0 && sy < self.levels).then(|| ((z * self.side + x) * self.levels + sy) as usize)
    }

    fn set(&mut self, x: i32, y: i32, z: i32, look: Look) {
        let Some(slot) = self.slot(x >> 4, (y - self.dimension.floor) >> 4, z >> 4) else { return };
        let section = &mut self.sections[slot];
        if section.is_none() && look != 0 {
            *section = Some(Box::new([0; 4096]));
        }
        if let Some(section) = section {
            section[index(x, y - self.dimension.floor, z)] = look;
        }
    }

    fn at(&self, x: i32, y: i32, z: i32) -> Look {
        let y = y - self.dimension.floor;
        self.slot(x >> 4, y >> 4, z >> 4).and_then(|s| self.sections[s].as_ref()).map_or(0, |s| s[index(x, y, z)])
    }

    fn present(&self, x: i32, y: i32, z: i32) -> bool {
        let y = y - self.dimension.floor;
        self.slot(x >> 4, y >> 4, z >> 4).is_some_and(|s| self.sections[s].is_some())
    }

    fn inside(&self, x: i32, z: i32) -> bool {
        let (cx, cz) = ((x >> 4) - self.origin.0, (z >> 4) - self.origin.1);
        cx >= 0 && cz >= 0 && cx < self.side && cz < self.side
    }

    fn top(&self) -> i32 {
        self.dimension.floor + self.filled * 16
    }
}

fn index(x: i32, y: i32, z: i32) -> usize {
    ((y & 15) * 256 + (z & 15) * 16 + (x & 15)) as usize
}

fn rgb(look: Look) -> [f32; 3] {
    let c = MAP_COLORS[usize::from(world::color(look)) * 4 + 2];
    [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0]
}

fn scale(c: [f32; 3], k: f32) -> [f32; 3] {
    [c[0] * k, c[1] * k, c[2] * k]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sky(dy: f32) -> [f32; 3] {
    if dy < 0.0 { scale(SKY_HORIZON, 0.92) } else { mix(SKY_HORIZON, SKY_TOP, (dy * 1.6).min(1.0)) }
}

fn push(out: &mut Vec<u8>, c: [f32; 3]) {
    out.extend(c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
    out.push(255);
}

const FACE_SHADE: [f32; 6] = [0.6, 0.6, 0.5, 1.0, 0.8, 0.8];

fn eyes(scene: &Scene, request: &Request, w: u32, h: u32, out: &mut Vec<u8>) {
    let [x, y, z] = request.position;
    let origin = [x as f32, (y + EYE_HEIGHT) as f32, z as f32];
    let (yaw, pitch) = (request.rotation[0].to_radians(), request.rotation[1].to_radians());
    let forward = [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
    let flat = (forward[0] * forward[0] + forward[2] * forward[2]).sqrt().max(1e-6);
    let right = [-forward[2] / flat, 0.0, forward[0] / flat];
    let up = [right[1] * forward[2] - right[2] * forward[1], right[2] * forward[0] - right[0] * forward[2], right[0] * forward[1] - right[1] * forward[0]];
    let tan = (FOV_Y.to_radians() / 2.0).tan();
    let aspect = w as f32 / h as f32;
    for py in 0..h {
        let sy = (1.0 - 2.0 * (py as f32 + 0.5) / h as f32) * tan;
        for px in 0..w {
            let sx = (2.0 * (px as f32 + 0.5) / w as f32 - 1.0) * tan * aspect;
            let d = [forward[0] + right[0] * sx + up[0] * sy, forward[1] + right[1] * sx + up[1] * sy, forward[2] + right[2] * sx + up[2] * sy];
            let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            push(out, trace(scene, origin, [d[0] / n, d[1] / n, d[2] / n]));
        }
    }
}

fn trace(scene: &Scene, origin: [f32; 3], dir: [f32; 3]) -> [f32; 3] {
    let mut cell = origin.map(|v| v.floor() as i32);
    let step = dir.map(|v| if v > 0.0 { 1 } else { -1 });
    let delta = dir.map(|v| if v == 0.0 { f32::INFINITY } else { 1.0 / v.abs() });
    let mut next = [0.0f32; 3];
    for a in 0..3 {
        let edge = if dir[a] > 0.0 { cell[a] as f32 + 1.0 - origin[a] } else { origin[a] - cell[a] as f32 };
        next[a] = if dir[a] == 0.0 { f32::INFINITY } else { edge * delta[a] };
    }
    let mut light = [0.0f32; 3];
    let mut through = 1.0f32;
    let mut first = true;
    let mut t = 0.0f32;
    let mut face = 3usize;
    let start = scene.at(cell[0], cell[1], cell[2]);
    let mut wet = start != 0 && world::class(start) == Class::Fluid;
    let fogged = |c: [f32; 3], t: f32| mix(c, sky(dir[1]), ((t / REACH - FOG_FROM) / (1.0 - FOG_FROM)).clamp(0.0, 1.0));
    let finish = |light: [f32; 3], through: f32, c: [f32; 3]| add(light, scale(c, through));
    loop {
        let a = if next[0] < next[1] { if next[0] < next[2] { 0 } else { 2 } } else if next[1] < next[2] { 1 } else { 2 };
        let leave = next[a];
        if !first {
            let look = scene.at(cell[0], cell[1], cell[2]);
            if look != 0 {
                let shade = FACE_SHADE[face];
                match world::class(look) {
                    Class::Solid => return finish(light, through, fogged(scale(rgb(look), shade), t)),
                    Class::Fluid => {
                        let k = if wet { WATER_DEPTH } else { WATER_SURFACE };
                        light = add(light, scale(fogged(scale(rgb(look), shade), t), through * k));
                        through *= 1.0 - k;
                        wet = true;
                        if through < 0.04 {
                            return light;
                        }
                    }
                    Class::Plant if t > PLANT_NEAR => {
                        let mid = (t + leave) / 2.0;
                        let fx = origin[0] + dir[0] * mid - cell[0] as f32;
                        let fz = origin[2] + dir[2] * mid - cell[2] as f32;
                        if (fx - fz).abs() < PLANT_STEM || (fx + fz - 1.0).abs() < PLANT_STEM {
                            return finish(light, through, fogged(scale(rgb(look), 0.9), t));
                        }
                    }
                    Class::Plant => {}
                }
            } else {
                wet = false;
            }
        }
        first = false;
        t = leave;
        if t > REACH {
            return finish(light, through, sky(dir[1]));
        }
        cell[a] += step[a];
        next[a] += delta[a];
        face = a * 2 + usize::from(step[a] < 0);
        if !scene.inside(cell[0], cell[2]) || cell[1] >= scene.top() && dir[1] >= 0.0 {
            return finish(light, through, sky(dir[1]));
        }
        if cell[1] < scene.dimension.floor {
            return finish(light, through, VOID);
        }
    }
}

fn surface(scene: &Scene, x: i32, z: i32) -> Option<(i32, Look, u32)> {
    let mut y = scene.top() - 1;
    let mut depth = 0;
    while y >= scene.dimension.floor {
        if !scene.present(x, y, z) {
            y = ((y - scene.dimension.floor) & !15) + scene.dimension.floor - 1;
            continue;
        }
        let look = scene.at(x, y, z);
        if look != 0 {
            if world::class(look) == Class::Fluid {
                depth += 1;
            } else {
                return Some((y, look, depth));
            }
        }
        y -= 1;
    }
    None
}

fn above(scene: &Scene, request: &Request, w: u32, h: u32, out: &mut Vec<u8>) {
    let k = ABOVE_SPAN / w.max(h) as f32;
    let [cx, _, cz] = request.position;
    let water = rgb(12);
    for py in 0..h {
        for px in 0..w {
            let bx = (cx as f32 + (px as f32 + 0.5 - w as f32 / 2.0) * k).floor() as i32;
            let bz = (cz as f32 + (py as f32 + 0.5 - h as f32 / 2.0) * k).floor() as i32;
            let c = if !scene.inside(bx, bz) {
                VOID
            } else {
                match surface(scene, bx, bz) {
                    None => VOID,
                    Some((y, look, depth)) => {
                        let north = surface(scene, bx, bz - 1).map_or(y, |n| n.0);
                        let shade = if y > north { 1.0 } else if y < north { 0.72 } else { 0.86 };
                        let ground = scale(rgb(look), shade);
                        if depth > 0 { mix(ground, water, (0.5 + depth as f32 * 0.08).min(0.92)) } else { ground }
                    }
                }
            };
            push(out, c);
        }
    }
    mark(out, w, h, request.rotation[0]);
}

fn mark(out: &mut [u8], w: u32, h: u32, yaw: f32) {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let r = (w.min(h) as f32 / 40.0).max(3.0);
    let yaw = yaw.to_radians();
    let (fx, fy) = (-yaw.sin(), yaw.cos());
    let tip = [cx + fx * r * 1.8, cy + fy * r * 1.8];
    let left = [cx - fy * r - fx * r, cy + fx * r - fy * r];
    let right = [cx + fy * r - fx * r, cy - fx * r - fy * r];
    let span = (r * 2.0).ceil() as i32 + 2;
    for y in (cy as i32 - span).max(0)..(cy as i32 + span).min(h as i32) {
        for x in (cx as i32 - span).max(0)..(cx as i32 + span).min(w as i32) {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let edge = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
            let (e1, e2, e3) = (edge(tip, left), edge(left, right), edge(right, tip));
            let inside = (e1 >= 0.0 && e2 >= 0.0 && e3 >= 0.0) || (e1 <= 0.0 && e2 <= 0.0 && e3 <= 0.0);
            if inside {
                let i = (y as usize * w as usize + x as usize) * 4;
                out[i..i + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::world::fixture::{chunk, state};
    use super::super::world::{World, class, look};
    use super::super::wire::In;
    use super::*;

    fn solid(l: Look) -> bool {
        l != 0 && class(l) == Class::Solid
    }

    fn ground() -> (Snapshot, u32) {
        let stone = state(solid);
        let mut w = World::default();
        let mut sections = vec![0; 24];
        sections[3] = stone;
        for x in -1..=1 {
            for z in -1..=1 {
                w.chunk(&mut In::new(&chunk(x, z, &sections)), (0, 0)).unwrap();
            }
        }
        (w.snapshot(8.0, 8.0, 2), stone)
    }

    fn pixel(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }

    fn near(a: [u8; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (f32::from(*a) - (b.clamp(0.0, 1.0) * 255.0)).abs() <= 3.0)
    }

    fn request(world: Snapshot, rotation: [f32; 2]) -> Request {
        Request { world, position: [8.5, 0.0, 8.5], rotation, width: 64, height: 36 }
    }

    #[test]
    fn standing_on_stone_the_eyes_see_sky_above_and_lit_stone_below() {
        let (world, stone) = ground();
        let rgba = draw(&request(world, [0.0, 0.0]), View::Eyes);
        assert_eq!(rgba.len(), 64 * 36 * 4);
        assert!(near(pixel(&rgba, 64, 32, 0), sky(0.5)) || pixel(&rgba, 64, 32, 0)[2] > 200, "sky at the top: {:?}", pixel(&rgba, 64, 32, 0));
        assert!(near(pixel(&rgba, 64, 32, 35), rgb(look(stone))), "the top face of the stone underfoot: {:?}", pixel(&rgba, 64, 32, 35));
    }

    #[test]
    fn a_pillar_in_front_hides_what_is_behind_and_shows_its_side() {
        let (mut world, stone) = ground();
        for y in 0..6 {
            world.changes.push(([8, y, 11], stone));
        }
        let rgba = draw(&request(world, [0.0, 0.0]), View::Eyes);
        let c = pixel(&rgba, 64, 32, 18);
        assert!(near(c, scale(rgb(look(stone)), FACE_SHADE[4])), "the north face of the pillar: {c:?}");
    }

    #[test]
    fn from_above_the_ground_shows_its_colour_and_the_player_its_mark() {
        let (world, stone) = ground();
        let rgba = draw(&request(world, [0.0, 0.0]), View::Above);
        assert!(near(pixel(&rgba, 64, 20, 10), scale(rgb(look(stone)), 0.86)), "{:?}", pixel(&rgba, 64, 20, 10));
        assert_eq!(pixel(&rgba, 64, 32, 19), [255, 255, 255], "the mark points south, down the picture");
    }

    #[test]
    fn nothing_known_draws_sky_and_void_without_failing() {
        let empty = Snapshot { dimension: Dimension::default(), chunks: Vec::new(), changes: Vec::new() };
        let rgba = draw(&request(empty.clone(), [45.0, -30.0]), View::Eyes);
        assert!(near(pixel(&rgba, 64, 32, 0), sky(1.0)) || pixel(&rgba, 64, 32, 0)[2] > 200);
        let rgba = draw(&request(empty, [0.0, 0.0]), View::Above);
        assert!(near(pixel(&rgba, 64, 1, 1), VOID));
    }

    #[test]
    fn broken_chunk_data_is_skipped() {
        let (mut world, _) = ground();
        world.chunks.push(((5, 5), Arc::from(&b"junk"[..])));
        assert_eq!(draw(&request(world, [0.0, 0.0]), View::Above).len(), 64 * 36 * 4);
    }

    #[test]
    fn the_drawing_thread_answers_every_request() {
        let (world, _) = ground();
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..3 {
            let tx = tx.clone();
            queue(request(world.clone(), [0.0, 0.0]), vec![View::Eyes, View::Above], move |pictures| tx.send(pictures.iter().map(Vec::len).collect::<Vec<_>>()).unwrap());
        }
        for _ in 0..3 {
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(), [64 * 36 * 4; 2]);
        }
    }
}