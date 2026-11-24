//! Turns the blocks around the camera into lit, tinted quads for the rasterizer, the way the game compiles a chunk
//! section: fluids first, then the block's model parts, faces hidden by full neighbours skipped.

use std::collections::HashMap;

use super::assets::{Assets, Transparency};
use super::block::{self, State};
use super::light::{self, CORNERS, Lightmap, Packed, Samples};
use super::model::{self, Dir, Source};
use super::random::{self, Legacy};
use super::states::STATES;
use super::raster::{Later, Target, Vertex};
use super::{Biome, Blocks, Modifier};

const EPS: f32 = 1e-4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tint {
    None,
    Grass,
    Foliage,
    DryFoliage,
    Water,
    Fixed(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fluid {
    Water,
    Lava,
}

struct Face {
    pos: [[f32; 3]; 4],
    uv: [[f32; 2]; 4],
    texture: u32,
    cull: Option<Dir>,
    face: Dir,
    shade: Dir,
    tint: bool,
    mode: Transparency,
    lo: [f32; 3],
    hi: [f32; 3],
}

struct Info {
    state: u32,
    parts: Vec<Vec<(Vec<Face>, i32)>>,
    multipart: bool,
    ao: bool,
    occludes: [bool; 6],
    full: bool,
    solid: bool,
    shade: f32,
    emissive: bool,
    fluid: Option<(Fluid, u8)>,
    offset: Offset,
    skip_same: bool,
    tint: Tint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Offset {
    None,
    Xz,
    Xyz,
}

const AIR: &[&str] = &["air", "cave_air", "void_air", "barrier", "light", "structure_void", "moving_piston"];

fn tint_of(name: &str) -> Tint {
    match name {
        "grass_block" | "short_grass" | "tall_grass" | "fern" | "large_fern" | "potted_fern" | "sugar_cane" | "bush" | "pink_petals" | "wildflowers" => Tint::Grass,
        "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "mangrove_leaves" | "vine" => Tint::Foliage,
        "leaf_litter" => Tint::DryFoliage,
        "birch_leaves" => Tint::Fixed(0x80A755),
        "spruce_leaves" => Tint::Fixed(0x619961),
        "lily_pad" => Tint::Fixed(0x208030),
        "attached_melon_stem" | "attached_pumpkin_stem" => Tint::Fixed(0xE0C71C),
        "melon_stem" | "pumpkin_stem" => Tint::Fixed(0x8BA82E),
        "redstone_wire" => Tint::Fixed(0x4B0000),
        "water" | "bubble_column" | "water_cauldron" => Tint::Water,
        _ => Tint::None,
    }
}

fn offset_of(name: &str) -> Offset {
    const XZ: &[&str] = &[
        "short_grass", "tall_grass", "fern", "large_fern", "dandelion", "poppy", "blue_orchid", "allium", "azure_bluet", "red_tulip", "orange_tulip",
        "white_tulip", "pink_tulip", "oxeye_daisy", "cornflower", "lily_of_the_valley", "wither_rose", "torchflower", "sunflower", "lilac", "rose_bush",
        "peony", "bush", "firefly_bush", "short_dry_grass", "tall_dry_grass", "open_eyeblossom", "closed_eyeblossom", "cactus_flower", "mangrove_propagule",
        "bamboo_sapling", "warped_roots", "crimson_roots", "nether_sprouts", "hanging_roots", "pointed_dripstone", "pitcher_plant", "golden_dandelion",
    ];
    match name {
        _ if XZ.contains(&name) => Offset::Xz,
        "small_dripleaf" | "bamboo" => Offset::Xyz,
        _ => Offset::None,
    }
}

fn emissive(s: &State) -> bool {
    let lit = s.get("lit") != Some("false");
    matches!(s.name, "glowstone" | "lava" | "sea_lantern" | "shroomlight" | "magma_block" | "beacon" | "end_rod" | "fire" | "soul_fire" | "crying_obsidian" | "conduit" | "respawn_anchor" | "sculk_catalyst")
        || s.name.ends_with("torch") || s.name.ends_with("lantern") || s.name.ends_with("froglight")
        || (lit && s.get("lit").is_some())
}

fn waterlogged(s: &State) -> bool {
    s.get("waterlogged") == Some("true") || matches!(s.name, "kelp" | "kelp_plant" | "seagrass" | "tall_seagrass" | "bubble_column")
}

fn covers(faces: &[&Face], d: Dir) -> bool {
    let axes = match d {
        Dir::Down | Dir::Up => (0, 2),
        Dir::North | Dir::South => (0, 1),
        Dir::West | Dir::East => (1, 2),
    };
    let area: f32 = faces.iter().map(|f| (f.hi[axes.0] - f.lo[axes.0]) * (f.hi[axes.1] - f.lo[axes.1])).sum();
    let reach = faces.iter().fold(([1.0f32; 2], [0.0f32; 2]), |(lo, hi), f| ([lo[0].min(f.lo[axes.0]), lo[1].min(f.lo[axes.1])], [hi[0].max(f.hi[axes.0]), hi[1].max(f.hi[axes.1])]));
    area >= 1.0 - EPS && reach.0[0] <= EPS && reach.0[1] <= EPS && reach.1[0] >= 1.0 - EPS && reach.1[1] >= 1.0 - EPS
}

fn on_boundary(f: &Face, d: Dir) -> bool {
    match d {
        Dir::Down => f.hi[1] <= EPS,
        Dir::Up => f.lo[1] >= 1.0 - EPS,
        Dir::North => f.hi[2] <= EPS,
        Dir::South => f.lo[2] >= 1.0 - EPS,
        Dir::West => f.hi[0] <= EPS,
        Dir::East => f.lo[0] >= 1.0 - EPS,
    }
}

impl Info {
    fn new(id: u32, assets: &Assets) -> Self {
        let state = block::decode(id).unwrap_or(State { name: "air", properties: Vec::new() });
        let mut info = Info {
            state: id,
            parts: Vec::new(),
            multipart: false,
            ao: true,
            occludes: [false; 6],
            full: false,
            solid: false,
            shade: 1.0,
            emissive: emissive(&state),
            fluid: None,
            offset: offset_of(state.name),
            skip_same: false,
            tint: tint_of(state.name),
        };
        if AIR.contains(&state.name) {
            return info;
        }
        info.fluid = match state.name {
            "water" => Some((Fluid::Water, state.get("level").and_then(|l| l.parse().ok()).unwrap_or(0))),
            "lava" => Some((Fluid::Lava, state.get("level").and_then(|l| l.parse().ok()).unwrap_or(0))),
            _ if waterlogged(&state) => Some((Fluid::Water, 0)),
            _ => None,
        };
        if matches!(state.name, "water" | "lava") {
            return info;
        }
        let Some(blockstate) = assets.blockstate(state.name) else { return info };
        info.multipart = blockstate.get("variants").is_none();
        let mut first = true;
        for part in model::parts(blockstate, &state) {
            let mut alternatives = Vec::with_capacity(part.len());
            for (variant, weight) in part {
                let quads = model::bake(assets, &variant);
                if first {
                    info.ao = quads.first().is_none_or(|q| q.ao);
                    first = false;
                }
                let faces = quads.into_iter().filter_map(|q| face(q, assets)).collect();
                alternatives.push((faces, weight));
            }
            info.parts.push(alternatives);
        }
        if let Some(first) = info.parts.first().and_then(|p| p.first()) {
            for d in Dir::ALL {
                let edge: Vec<&Face> = first.0.iter().filter(|f| f.cull == Some(d) && on_boundary(f, d)).collect();
                let opaque: Vec<&Face> = edge.iter().copied().filter(|f| f.mode == Transparency::Opaque).collect();
                info.occludes[d.index()] = info.parts.len() == 1 && covers(&opaque, d);
                info.full |= false;
                if d == Dir::Down {
                    info.full = covers(&edge, d);
                } else {
                    info.full &= covers(&edge, d);
                }
            }
        }
        info.solid = info.occludes.iter().all(|o| *o);
        let n = state.name;
        info.skip_same = (n.contains("glass") && !n.contains("pane")) || matches!(n, "ice" | "slime_block" | "honey_block" | "powder_snow");
        let clear = info.skip_same || n.ends_with("copper_grate") || matches!(n, "barrier" | "light" | "structure_void");
        info.shade = if clear {
            1.0
        } else if matches!(n, "mud" | "soul_sand") || (n == "snow" && state.get("layers") == Some("8")) || info.full {
            0.2
        } else {
            1.0
        };
        info
    }

    fn visible(&self) -> bool {
        !self.parts.is_empty() || self.fluid.is_some()
    }
}

fn face(q: model::Quad, assets: &Assets) -> Option<Face> {
    let texture = assets.texture_id(&q.texture)?;
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in &q.pos {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let ulo = [q.uv.iter().map(|u| u[0]).fold(f32::MAX, f32::min), q.uv.iter().map(|u| u[1]).fold(f32::MAX, f32::min)];
    let uhi = [q.uv.iter().map(|u| u[0]).fold(f32::MIN, f32::max), q.uv.iter().map(|u| u[1]).fold(f32::MIN, f32::max)];
    let mode = assets.texture(texture).transparency(ulo, uhi);
    let facing = q.face.unwrap_or(Dir::Up);
    Some(Face { pos: q.pos, uv: q.uv, texture, cull: q.cull, face: facing, shade: q.shade.unwrap_or(facing), tint: q.tint >= 0, mode, lo, hi })
}

/// Where the camera is and how it looks, already turned into a view basis.
pub struct View {
    pub eye: [f64; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub forward: [f32; 3],
}

impl View {
    #[inline]
    pub fn to_view(&self, p: [f32; 3]) -> [f32; 3] {
        let d = |a: [f32; 3]| a[0] * p[0] + a[1] * p[1] + a[2] * p[2];
        [d(self.right), d(self.up), d(self.forward)]
    }
}

/// The blocks near the camera, copied out of the world once per picture.
pub struct Scene<'a, W: Blocks> {
    world: &'a W,
    assets: &'a Assets,
    biomes: &'a [Biome],
    lo: [i32; 3],
    size: [i32; 3],
    cells: Vec<u16>,
    light: Vec<u8>,
    infos: Vec<Info>,
    tints: HashMap<(u8, i32, i32, i32), [f32; 3]>,
}

const BLEND: i32 = 2;

impl<'a, W: Blocks> Scene<'a, W> {
    /// Copies the blocks of the chunks within `distance` chunks of the camera (and that many sections up and down).
    pub fn new(world: &'a W, assets: &'a Assets, biomes: &'a [Biome], eye: [f64; 3], distance: i32) -> Self {
        let chunk = |v: f64| (v.floor() as i32) >> 4;
        let (cx, cy, cz) = (chunk(eye[0]), chunk(eye[1]), chunk(eye[2]));
        let floor = world.floor();
        let top = floor.saturating_add(world.height());
        let y0 = ((cy - distance) * 16).max(floor) - 1;
        let y1 = ((cy + distance + 1) * 16).min(top) + 1;
        let lo = [(cx - distance) * 16 - 1, y0, (cz - distance) * 16 - 1];
        let side = (2 * distance + 1) * 16 + 2;
        let size = [side, (y1 - y0).max(0), side];
        let total = (size[0] * size[1] * size[2]) as usize;
        let mut scene = Scene { world, assets, biomes, lo, size, cells: vec![0; total], light: vec![0xF0; total], infos: Vec::new(), tints: HashMap::new() };
        scene.infos.push(Info::new(0, assets));
        let mut index: HashMap<u32, u16> = HashMap::from([(0, 0)]);
        let (mut last_state, mut last_index) = (0u32, 0u16);
        let has_light = world.light(eye[0].floor() as i32, eye[1].floor() as i32, eye[2].floor() as i32).is_some();
        for z in 0..size[2] {
            for x in 0..size[0] {
                let mut open = true;
                for y in (0..size[1]).rev() {
                    let (wx, wy, wz) = (lo[0] + x, lo[1] + y, lo[2] + z);
                    let i = ((y * size[2] + z) * size[0] + x) as usize;
                    let state = if wy < floor || wy >= top { 0 } else { world.state(wx, wy, wz) };
                    let state = if state < STATES { state } else { 0 };
                    let at = if state == last_state {
                        last_index
                    } else {
                        let next = scene.infos.len() as u16;
                        let at = *index.entry(state).or_insert(next);
                        if at == next {
                            scene.infos.push(Info::new(state, assets));
                        }
                        (last_state, last_index) = (state, at);
                        at
                    };
                    scene.cells[i] = at;
                    let solid = scene.infos[usize::from(at)].solid;
                    scene.light[i] = match world.light(wx, wy, wz) {
                        Some((sky, blk)) => (sky.min(15) << 4) | blk.min(15),
                        None if has_light => 0xF0,
                        None => {
                            let lit = if open && !solid { 0xF0 } else if solid { 0 } else { 0x90 };
                            open &= !solid;
                            lit
                        }
                    };
                }
            }
        }
        scene
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (dx, dy, dz) = (x - self.lo[0], y - self.lo[1], z - self.lo[2]);
        (dx >= 0 && dy >= 0 && dz >= 0 && dx < self.size[0] && dy < self.size[1] && dz < self.size[2]).then(|| ((dy * self.size[2] + dz) * self.size[0] + dx) as usize)
    }

    #[inline]
    fn info(&self, x: i32, y: i32, z: i32) -> &Info {
        &self.infos[self.index(x, y, z).map_or(0, |i| usize::from(self.cells[i]))]
    }

    #[inline]
    fn packed(&self, x: i32, y: i32, z: i32) -> Packed {
        let l = self.index(x, y, z).map_or(0xF0, |i| self.light[i]);
        light::pack(l >> 4, l & 15)
    }

    fn tint(&mut self, kind: Tint, x: i32, y: i32, z: i32) -> [f32; 3] {
        let rgb = |c: u32| [(c >> 16 & 0xFF) as f32 / 255.0, (c >> 8 & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0];
        let code = match kind {
            Tint::None => return [1.0; 3],
            Tint::Fixed(c) => return rgb(c),
            Tint::Grass => 0,
            Tint::Foliage => 1,
            Tint::DryFoliage => 2,
            Tint::Water => 3,
        };
        if let Some(c) = self.tints.get(&(code, x, y, z)) {
            return *c;
        }
        let mut sum = [0.0f32; 3];
        for dz in -BLEND..=BLEND {
            for dx in -BLEND..=BLEND {
                let (bx, bz) = (x + dx, z + dz);
                let biome = self.biomes.get(usize::from(self.world.biome(bx, y, bz))).cloned().unwrap_or_default();
                let c = biome.color(kind_name(kind), self.assets, bx, bz);
                for k in 0..3 {
                    sum[k] += c[k];
                }
            }
        }
        let n = ((2 * BLEND + 1) * (2 * BLEND + 1)) as f32;
        let c = sum.map(|v| v / n);
        self.tints.insert((code, x, y, z), c);
        c
    }

    fn samples(&self, info: &Info, at: [i32; 3], f: &Face) -> Samples {
        let d = f.face;
        let step = d.step();
        let cubic = on_boundary(f, d) || (f.lo[axis(d)] == f.hi[axis(d)] && info.full);
        let base = if cubic { add(at, step) } else { at };
        let sample = |p: [i32; 3]| {
            let i = self.info(p[0], p[1], p[2]);
            (i.shade, self.packed(p[0], p[1], p[2]))
        };
        let corners = CORNERS[d.index()].map(Dir::step);
        let side = corners.map(|c| sample(add(base, c)));
        let permeable = corners.map(|c| {
            let p = add(add(base, c), step);
            !self.info(p[0], p[1], p[2]).solid
        });
        let diag = |a: usize, b: usize| if !permeable[a] && !permeable[b] { side[0] } else { sample(add(add(base, corners[a]), corners[b])) };
        let next = add(at, step);
        let center_light = if cubic || !self.info(next[0], next[1], next[2]).solid { self.packed(next[0], next[1], next[2]) } else { self.packed(at[0], at[1], at[2]) };
        let center_shade = if cubic { sample(base).0 } else { info.shade };
        Samples { side, diagonal: [diag(0, 2), diag(0, 3), diag(1, 2), diag(1, 3)], center: (center_shade, center_light) }
    }

    /// Draws every visible face into `target`; translucent ones go to `later`.
    pub fn draw<'t>(&mut self, view: &View, lightmap: &Lightmap, target: &mut Target, later: &mut Later<'t>)
    where
        'a: 't,
    {
        let assets: &'t Assets = self.assets;
        for y in 1..self.size[1] - 1 {
            for z in 1..self.size[2] - 1 {
                for x in 1..self.size[0] - 1 {
                    let i = ((y * self.size[2] + z) * self.size[0] + x) as usize;
                    let at = usize::from(self.cells[i]);
                    if !self.infos[at].visible() {
                        continue;
                    }
                    let pos = [self.lo[0] + x, self.lo[1] + y, self.lo[2] + z];
                    let rel = [0, 1, 2].map(|k| (f64::from(pos[k]) - view.eye[k]) as f32);
                    if !Self::cube_visible(view, target, rel) {
                        continue;
                    }
                    if self.infos[at].fluid.is_some() {
                        self.fluid(at, pos, rel, view, lightmap, target, later, assets);
                    }
                    self.model(at, pos, rel, view, lightmap, target, later, assets);
                }
            }
        }
    }

    fn cube_visible(view: &View, target: &Target, rel: [f32; 3]) -> bool {
        let corners = [[-0.5, -0.5, -0.5], [1.5, -0.5, -0.5], [-0.5, 1.5, -0.5], [1.5, 1.5, -0.5], [-0.5, -0.5, 1.5], [1.5, -0.5, 1.5], [-0.5, 1.5, 1.5], [1.5, 1.5, 1.5]].map(|c| view.to_view(add_f(rel, c)));
        target.visible(&corners)
    }

    #[allow(clippy::too_many_arguments)]
    fn model<'t>(&mut self, at: usize, pos: [i32; 3], rel: [f32; 3], view: &View, lightmap: &Lightmap, target: &mut Target, later: &mut Later<'t>, assets: &'t Assets) {
        let info = &self.infos[at];
        if info.parts.is_empty() {
            return;
        }
        let seed = random::seed(pos[0], pos[1], pos[2]);
        let part_seed = if info.multipart { Legacy::new(seed).next_long() } else { seed };
        let shift = match info.offset {
            Offset::None => [0.0; 3],
            kind => {
                let s = random::seed(pos[0], 0, pos[2]);
                let h = |v: i64| ((((v & 15) as f32) / 15.0 - 0.5) * 0.5).clamp(-0.25, 0.25);
                let y = if kind == Offset::Xyz { (((s >> 4 & 15) as f32) / 15.0 - 1.0) * 0.2 } else { 0.0 };
                [h(s), y, h(s >> 8)]
            }
        };
        let mut faces: Vec<(usize, usize, usize)> = Vec::new();
        for (p, part) in info.parts.iter().enumerate() {
            let alt = if part.len() == 1 { 0 } else { model::pick(part.iter().map(|a| a.1), part_seed) };
            for (k, f) in part[alt].0.iter().enumerate() {
                if let Some(d) = f.cull {
                    let n = add(pos, d.step());
                    let neighbour = self.info(n[0], n[1], n[2]);
                    if neighbour.occludes[d.opposite().index()] || (info.skip_same && neighbour.state == info.state) {
                        continue;
                    }
                }
                faces.push((p, alt, k));
            }
        }
        let tint = info.tint;
        let smooth = info.ao && !info.emissive;
        let mut color_tint = None;
        for (p, alt, k) in faces {
            let info = &self.infos[at];
            let f = &info.parts[p][alt].0[k];
            let world = f.pos.map(|c| [rel[0] + c[0] + shift[0], rel[1] + c[1] + shift[1], rel[2] + c[2] + shift[2]]);
            let v = world.map(|c| view.to_view(c));
            let n = cross(sub(v[1], v[0]), sub(v[2], v[0]));
            if n[0] * v[0][0] + n[1] * v[0][1] + n[2] * v[0][2] <= 0.0 || !target.visible(&v) {
                continue;
            }
            let shade = f.shade.shade();
            let lit: [(f32, Packed); 4] = if smooth {
                light::ambient(f.face, &self.samples(info, pos, f), &light::shape(f.lo, f.hi))
            } else {
                let cubic = on_boundary(f, f.face) || (f.lo[axis(f.face)] == f.hi[axis(f.face)] && info.full);
                let q = if f.cull.is_some() || cubic { add(pos, f.face.step()) } else { pos };
                [(1.0, self.packed(q[0], q[1], q[2])); 4]
            };
            let (texture, mode, uv, tinted) = (f.texture, f.mode, f.uv, f.tint);
            let tint = if tinted {
                *color_tint.get_or_insert_with(|| self.tint(tint, pos[0], pos[1], pos[2]))
            } else {
                [1.0; 3]
            };
            let corners: [Vertex; 4] = [0, 1, 2, 3].map(|i| {
                let l = lightmap.color(lit[i].1);
                let g = lit[i].0 * shade;
                Vertex { p: v[i], uv: uv[i], c: [g * l[0] * tint[0], g * l[1] * tint[1], g * l[2] * tint[2]] }
            });
            let tex = assets.texture(texture);
            if mode == Transparency::Translucent {
                later.push(corners, tex);
            } else {
                target.quad(&corners, tex, mode);
            }
        }
    }

    fn fluid_height(&self, fluid: Fluid, p: [i32; 3]) -> f32 {
        let info = self.info(p[0], p[1], p[2]);
        match info.fluid {
            Some((f, level)) if f == fluid => {
                let above = self.info(p[0], p[1] + 1, p[2]);
                if above.fluid.is_some_and(|(a, _)| a == fluid) {
                    1.0
                } else {
                    let amount = if level == 0 || level >= 8 { 8 } else { 8 - level };
                    f32::from(amount) / 9.0
                }
            }
            _ if !info.solid => 0.0,
            _ => -1.0,
        }
    }

    fn corner_height(&self, fluid: Fluid, own: f32, at: [i32; 3], a: Dir, b: Dir) -> f32 {
        let (pa, pb) = (add(at, a.step()), add(at, b.step()));
        let (ha, hb) = (self.fluid_height(fluid, pa), self.fluid_height(fluid, pb));
        if ha >= 1.0 || hb >= 1.0 {
            return 1.0;
        }
        let mut acc = (0.0f32, 0.0f32);
        let mut add_h = |h: f32| {
            if h >= 0.8 {
                acc.0 += h * 10.0;
                acc.1 += 10.0;
            } else if h >= 0.0 {
                acc.0 += h;
                acc.1 += 1.0;
            }
        };
        if ha > 0.0 || hb > 0.0 {
            let hd = self.fluid_height(fluid, add(pa, b.step()));
            if hd >= 1.0 {
                return 1.0;
            }
            add_h(hd);
        }
        add_h(own);
        add_h(ha);
        add_h(hb);
        if acc.1 > 0.0 { acc.0 / acc.1 } else { own }
    }

    #[allow(clippy::too_many_arguments)]
    fn fluid<'t>(&mut self, at: usize, pos: [i32; 3], rel: [f32; 3], view: &View, lightmap: &Lightmap, target: &mut Target, later: &mut Later<'t>, assets: &'t Assets) {
        let Some((fluid, _)) = self.infos[at].fluid else { return };
        let (still, flow) = match fluid {
            Fluid::Water => ("block/water_still", "block/water_flow"),
            Fluid::Lava => ("block/lava_still", "block/lava_flow"),
        };
        let (Some(still), Some(flow)) = (assets.texture_id(still), assets.texture_id(flow)) else { return };
        let same = |s: &Self, p: [i32; 3]| s.info(p[0], p[1], p[2]).fluid.is_some_and(|(f, _)| f == fluid);
        let own = self.fluid_height(fluid, pos);
        let h = if own >= 1.0 {
            [1.0; 4]
        } else {
            [
                self.corner_height(fluid, own, pos, Dir::North, Dir::West),
                self.corner_height(fluid, own, pos, Dir::South, Dir::West),
                self.corner_height(fluid, own, pos, Dir::South, Dir::East),
                self.corner_height(fluid, own, pos, Dir::North, Dir::East),
            ]
        };
        let tint = if fluid == Fluid::Water { self.tint(Tint::Water, pos[0], pos[1], pos[2]) } else { [1.0; 3] };
        let mode = if fluid == Fluid::Water { Transparency::Translucent } else { Transparency::Opaque };
        let light_at = |s: &Self, p: [i32; 3]| {
            let (a, b) = (s.packed(p[0], p[1], p[2]), s.packed(p[0], p[1] + 1, p[2]));
            ((a & 0xFF).max(b & 0xFF)) | ((a & 0xFF_0000).max(b & 0xFF_0000))
        };
        let mut emit = |s: &mut Self, local: [[f32; 3]; 4], uv: [[f32; 2]; 4], texture: u32, d: Dir, light: Packed| {
            let v = local.map(|c| view.to_view(add_f(rel, c)));
            let n = cross(sub(v[1], v[0]), sub(v[2], v[0]));
            if n[0] * v[0][0] + n[1] * v[0][1] + n[2] * v[0][2] <= 0.0 || !target.visible(&v) {
                return;
            }
            let l = lightmap.color(light);
            let g = d.shade();
            let c = [g * l[0] * tint[0], g * l[1] * tint[1], g * l[2] * tint[2]];
            let corners = [0, 1, 2, 3].map(|i| Vertex { p: v[i], uv: uv[i], c });
            let tex = assets.texture(texture);
            let _ = s;
            if mode == Transparency::Translucent {
                later.push(corners, tex);
            } else {
                target.quad(&corners, tex, mode);
            }
        };
        let up = add(pos, [0, 1, 0]);
        if !same(self, up) && !self.info(up[0], up[1], up[2]).occludes[Dir::Down.index()] {
            let top = [[0.0, h[0] - 0.001, 0.0], [0.0, h[1] - 0.001, 1.0], [1.0, h[2] - 0.001, 1.0], [1.0, h[3] - 0.001, 0.0]];
            let uv = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
            let l = light_at(self, pos);
            emit(self, top, uv, still, Dir::Up, l);
            let back = [top[0], top[3], top[2], top[1]];
            emit(self, back, [uv[0], uv[3], uv[2], uv[1]], still, Dir::Up, l);
        }
        let down = add(pos, [0, -1, 0]);
        if !same(self, down) && !self.info(down[0], down[1], down[2]).occludes[Dir::Up.index()] {
            let bottom = [[0.0, 0.001, 1.0], [0.0, 0.001, 0.0], [1.0, 0.001, 0.0], [1.0, 0.001, 1.0]];
            let l = light_at(self, down);
            emit(self, bottom, [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]], still, Dir::Down, l);
        }
        for (d, a, b, x0, z0, x1, z1) in [
            (Dir::North, 3, 0, 1.0, 0.001, 0.0, 0.001),
            (Dir::South, 1, 2, 0.0, 0.999, 1.0, 0.999),
            (Dir::West, 0, 1, 0.001, 0.0, 0.001, 1.0),
            (Dir::East, 2, 3, 0.999, 1.0, 0.999, 0.0),
        ] {
            let n = add(pos, d.step());
            if same(self, n) || self.info(n[0], n[1], n[2]).occludes[d.opposite().index()] {
                continue;
            }
            let (ha, hb) = (h[a], h[b]);
            let quad = [[x0, ha, z0], [x0, 0.0, z0], [x1, 0.0, z1], [x1, hb, z1]];
            let uv = [[0.0, (1.0 - ha) * 0.5], [0.0, 0.5], [0.5, 0.5], [0.5, (1.0 - hb) * 0.5]];
            let l = light_at(self, n);
            emit(self, quad, uv, flow, d, l);
        }
    }
}

fn kind_name(kind: Tint) -> &'static str {
    match kind {
        Tint::Grass => "grass",
        Tint::Foliage => "foliage",
        Tint::DryFoliage => "dry_foliage",
        _ => "water",
    }
}

impl Biome {
    fn color(&self, kind: &str, assets: &Assets, x: i32, z: i32) -> [f32; 3] {
        let from_map = |map: &Option<super::assets::Texture>, fallback: u32| {
            let Some(map) = map else { return rgb(fallback) };
            let t = self.temperature.clamp(0.0, 1.0);
            let d = self.downfall.clamp(0.0, 1.0) * t;
            let px = map.sample((1.0 - t) * 255.0 / 256.0 + 0.5 / 256.0, (1.0 - d) * 255.0 / 256.0 + 0.5 / 256.0, 0);
            [px[0], px[1], px[2]].map(|c| f32::from(c) / 255.0)
        };
        let _ = (x, z);
        match kind {
            "grass" => {
                let base = self.grass.map_or_else(|| from_map(&assets.grass, 0x91BD59), rgb);
                match self.modifier {
                    Modifier::None => base,
                    Modifier::Swamp => rgb(0x6A7039),
                    Modifier::DarkForest => {
                        let c = to_u32(base);
                        rgb(((c & 0xFE_FEFE) + 0x28_340A) >> 1)
                    }
                }
            }
            "foliage" => self.foliage.map_or_else(|| from_map(&assets.foliage, 0x48B518), rgb),
            "dry_foliage" => self.dry_foliage.map_or_else(|| from_map(&assets.dry_foliage, 0x7B5334), rgb),
            _ => rgb(self.water),
        }
    }
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16 & 0xFF) as f32 / 255.0, (c >> 8 & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0]
}

fn to_u32(c: [f32; 3]) -> u32 {
    c.iter().fold(0, |acc, v| (acc << 8) | (v * 255.0).round() as u32)
}

fn axis(d: Dir) -> usize {
    match d {
        Dir::West | Dir::East => 0,
        Dir::Down | Dir::Up => 1,
        Dir::North | Dir::South => 2,
    }
}

fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn add_f(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
