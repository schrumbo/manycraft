//! Block models as the game's resource pack describes them: a block state picks variants from its blockstate file,
//! each variant names a model whose parents and elements bake into quads in block space (0..1).

use serde_json::Value;

use super::block::State;

/// The six block faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Dir {
    pub const ALL: [Dir; 6] = [Dir::Down, Dir::Up, Dir::North, Dir::South, Dir::West, Dir::East];

    pub fn parse(s: &str) -> Option<Dir> {
        Some(match s {
            "down" | "bottom" => Dir::Down,
            "up" | "top" => Dir::Up,
            "north" => Dir::North,
            "south" => Dir::South,
            "west" => Dir::West,
            "east" => Dir::East,
            _ => return None,
        })
    }

    /// The unit step towards this face.
    pub fn step(self) -> [i32; 3] {
        match self {
            Dir::Down => [0, -1, 0],
            Dir::Up => [0, 1, 0],
            Dir::North => [0, 0, -1],
            Dir::South => [0, 0, 1],
            Dir::West => [-1, 0, 0],
            Dir::East => [1, 0, 0],
        }
    }

    pub fn opposite(self) -> Dir {
        match self {
            Dir::Down => Dir::Up,
            Dir::Up => Dir::Down,
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::West => Dir::East,
            Dir::East => Dir::West,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// How bright a face pointing this way is drawn, as the game shades it.
    pub fn shade(self) -> f32 {
        match self {
            Dir::Down => 0.5,
            Dir::Up => 1.0,
            Dir::North | Dir::South => 0.8,
            Dir::West | Dir::East => 0.6,
        }
    }

    fn closest(n: [f32; 3]) -> Option<Dir> {
        let mut best = None;
        let mut most = 0.0;
        for d in Dir::ALL {
            let s = d.step();
            let dot = n[0] * s[0] as f32 + n[1] * s[1] as f32 + n[2] * s[2] as f32;
            if dot >= 0.0 && dot > most {
                most = dot;
                best = Some(d);
            }
        }
        best
    }

    fn corners(self, from: [f32; 3], to: [f32; 3]) -> [[f32; 3]; 4] {
        let (a, b) = (from, to);
        match self {
            Dir::Down => [[a[0], a[1], b[2]], [a[0], a[1], a[2]], [b[0], a[1], a[2]], [b[0], a[1], b[2]]],
            Dir::Up => [[a[0], b[1], a[2]], [a[0], b[1], b[2]], [b[0], b[1], b[2]], [b[0], b[1], a[2]]],
            Dir::North => [[b[0], b[1], a[2]], [b[0], a[1], a[2]], [a[0], a[1], a[2]], [a[0], b[1], a[2]]],
            Dir::South => [[a[0], b[1], b[2]], [a[0], a[1], b[2]], [b[0], a[1], b[2]], [b[0], b[1], b[2]]],
            Dir::West => [[a[0], b[1], a[2]], [a[0], a[1], a[2]], [a[0], a[1], b[2]], [a[0], b[1], b[2]]],
            Dir::East => [[b[0], b[1], b[2]], [b[0], a[1], b[2]], [b[0], a[1], a[2]], [b[0], b[1], a[2]]],
        }
    }

    fn default_uv(self, p: [f32; 3]) -> [f32; 2] {
        match self {
            Dir::Down => [p[0], 16.0 - p[2]],
            Dir::Up => [p[0], p[2]],
            Dir::North => [16.0 - p[0], 16.0 - p[1]],
            Dir::South => [p[0], 16.0 - p[1]],
            Dir::West => [p[2], 16.0 - p[1]],
            Dir::East => [16.0 - p[2], 16.0 - p[1]],
        }
    }
}

/// One model a blockstate picks, with its rotation in quarter turns.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub model: String,
    pub x: u8,
    pub y: u8,
    pub uvlock: bool,
}

/// A baked face in block space.
#[derive(Clone, Debug, PartialEq)]
pub struct Quad {
    pub pos: [[f32; 3]; 4],
    /// Texture coordinates within the texture, 0..1.
    pub uv: [[f32; 2]; 4],
    /// The texture, as a path below `textures/` without extension (`block/stone`).
    pub texture: String,
    /// The neighbour whose full face hides this one.
    pub cull: Option<Dir>,
    /// The direction the face points closest to; `None` for faces that point between directions.
    pub face: Option<Dir>,
    /// The direction whose shade the face takes: the model's override, else the way it points.
    pub shade: Option<Dir>,
    pub tint: i32,
    pub emission: u8,
    pub ao: bool,
}

/// Where model and blockstate files come from.
pub trait Source {
    /// The blockstate file of block `name` (without namespace).
    fn blockstate(&self, name: &str) -> Option<&Value>;
    /// The model file at `path` (`block/stone`).
    fn model(&self, path: &str) -> Option<&Value>;
}

fn strip(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn matches(state: &State, key: &str, want: &str) -> bool {
    let (negate, want) = match want.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, want),
    };
    let have = state.get(key).unwrap_or("");
    want.split('|').any(|w| w == have) != negate
}

fn condition(state: &State, when: &Value) -> bool {
    let Some(map) = when.as_object() else { return true };
    if let Some(any) = map.get("OR").and_then(Value::as_array) {
        return any.iter().any(|c| condition(state, c));
    }
    if let Some(all) = map.get("AND").and_then(Value::as_array) {
        return all.iter().all(|c| condition(state, c));
    }
    map.iter().all(|(k, v)| match v {
        Value::String(s) => matches(state, k, s),
        Value::Bool(b) => matches(state, k, if *b { "true" } else { "false" }),
        Value::Number(n) => matches(state, k, &n.to_string()),
        _ => false,
    })
}

fn variant_key(state: &State, key: &str) -> bool {
    key.is_empty() || key.split(',').all(|pair| pair.split_once('=').is_some_and(|(k, v)| matches(state, k, v)))
}

fn variant(one: &Value) -> Option<(Variant, i32)> {
    let quarter = |k: &str| ((one.get(k).and_then(Value::as_i64).unwrap_or(0).rem_euclid(360)) / 90) as u8;
    let weight = one.get("weight").and_then(Value::as_i64).unwrap_or(1).clamp(1, i64::from(i32::MAX >> 8)) as i32;
    Some((Variant { model: strip(one.get("model")?.as_str()?).to_string(), x: quarter("x"), y: quarter("y"), uvlock: one.get("uvlock").and_then(Value::as_bool).unwrap_or(false) }, weight))
}

/// One part of a block's look: the weighted variants it picks among.
pub type Part = Vec<(Variant, i32)>;

fn part(entry: &Value) -> Option<Part> {
    let list: Part = match entry {
        Value::Array(list) => list.iter().filter_map(variant).collect(),
        Value::Object(_) => variant(entry).into_iter().collect(),
        _ => return None,
    };
    (!list.is_empty()).then_some(list)
}

/// Which of `weights` the game picks for the block whose position seed is `seed`.
pub fn pick(weights: impl Iterator<Item = i32> + Clone, seed: i64) -> usize {
    let total: i32 = weights.clone().sum();
    if total <= 0 {
        return 0;
    }
    let mut roll = super::random::Legacy::new(seed).next_int(total);
    for (i, w) in weights.enumerate() {
        roll -= w;
        if roll < 0 {
            return i;
        }
    }
    0
}

/// The parts `blockstate` gives `state`.
pub fn parts(blockstate: &Value, state: &State) -> Vec<Part> {
    if let Some(map) = blockstate.get("variants").and_then(Value::as_object) {
        return map.iter().find(|(k, _)| variant_key(state, k)).and_then(|(_, v)| part(v)).into_iter().collect();
    }
    let Some(list) = blockstate.get("multipart").and_then(Value::as_array) else { return Vec::new() };
    list.iter().filter(|p| p.get("when").is_none_or(|w| condition(state, w))).filter_map(|p| part(p.get("apply")?)).collect()
}

/// The variants `blockstate` picks for `state` at the block whose position seed is `seed`. Multipart blocks reseed every
/// part with one number drawn from the position seed, as the game does.
pub fn variants(blockstate: &Value, state: &State, seed: i64) -> Vec<Variant> {
    let multipart = blockstate.get("variants").is_none();
    let seed = if multipart { super::random::Legacy::new(seed).next_long() } else { seed };
    parts(blockstate, state).into_iter().map(|mut p| p.swap_remove(pick(p.iter().map(|v| v.1), seed)).0).collect()
}

const MAX_PARENTS: usize = 32;

fn vec3(v: Option<&Value>) -> Option<[f32; 3]> {
    let a = v?.as_array()?;
    Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32])
}

fn texture<'a>(chain: &[&'a Value], name: &str) -> Option<String> {
    let mut name = name.to_string();
    for _ in 0..MAX_PARENTS {
        let Some(key) = name.strip_prefix('#') else { return Some(strip(&name).to_string()) };
        let next = chain.iter().find_map(|m| m.get("textures")?.get(key)?.as_str().or_else(|| m.get("textures")?.get(key)?.get("sprite")?.as_str()))?;
        name = next.to_string();
    }
    None
}

type M3 = [[f32; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for (i, row) in r.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn apply(m: &M3, v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn axis_rotation(axis: usize, degrees: f32) -> M3 {
    let (s, c) = degrees.to_radians().sin_cos();
    let (s, c) = (if s.abs() < 1e-6 { 0.0 } else { s }, if c.abs() < 1e-6 { 0.0 } else { c });
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        1 => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        _ => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

fn element_rotation(r: &Value) -> Option<([f32; 3], M3)> {
    let origin = vec3(r.get("origin")).unwrap_or([8.0, 8.0, 8.0]);
    let mut m = if let Some(axis) = r.get("axis").and_then(Value::as_str) {
        let axis = match axis {
            "x" => 0,
            "y" => 1,
            _ => 2,
        };
        axis_rotation(axis, r.get("angle").and_then(Value::as_f64).unwrap_or(0.0) as f32)
    } else {
        let get = |k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        mul(&mul(&axis_rotation(2, get("z")), &axis_rotation(1, get("y"))), &axis_rotation(0, get("x")))
    };
    if r.get("rescale").and_then(Value::as_bool).unwrap_or(false) {
        for axis in 0..3 {
            let most = (0..3).map(|i| m[i][axis].abs()).fold(0.0f32, f32::max);
            if most > 1e-6 {
                for row in m.iter_mut() {
                    row[axis] /= most;
                }
            }
        }
    }
    Some((origin.map(|o| o / 16.0), m))
}

/// Bakes `variant` into quads; an unknown model bakes to nothing.
pub fn bake(source: &impl Source, variant: &Variant) -> Vec<Quad> {
    let mut chain = Vec::new();
    let mut path = Some(variant.model.clone());
    while let Some(p) = path.take() {
        if chain.len() >= MAX_PARENTS {
            break;
        }
        let Some(model) = source.model(&p) else { break };
        chain.push(model);
        path = model.get("parent").and_then(Value::as_str).map(|s| strip(s).to_string());
    }
    let Some(elements) = chain.iter().find_map(|m| m.get("elements").and_then(Value::as_array)) else { return Vec::new() };
    let ao = chain.iter().find_map(|m| m.get("ambientocclusion").and_then(Value::as_bool)).unwrap_or(true);
    let block = mul(&axis_rotation(1, -90.0 * f32::from(variant.y)), &axis_rotation(0, -90.0 * f32::from(variant.x)));
    let turned = variant.x != 0 || variant.y != 0;
    let mut quads = Vec::new();
    for element in elements {
        let (Some(from), Some(to)) = (vec3(element.get("from")), vec3(element.get("to"))) else { continue };
        let rotation = element.get("rotation").and_then(element_rotation);
        let shade = element.get("shade_direction_override").and_then(Value::as_str).and_then(Dir::parse);
        let emission = element.get("light_emission").and_then(Value::as_u64).unwrap_or(0).min(15) as u8;
        let Some(faces) = element.get("faces").and_then(Value::as_object) else { continue };
        for (name, face) in faces {
            let Some(dir) = Dir::parse(name) else { continue };
            let flat = |axis: usize| from[axis] == to[axis];
            let kept = match dir {
                Dir::West | Dir::East => !flat(1) && !flat(2),
                Dir::Down | Dir::Up => !flat(0) && !flat(2),
                Dir::North | Dir::South => !flat(0) && !flat(1),
            };
            if !kept {
                continue;
            }
            let Some(tex) = face.get("texture").and_then(Value::as_str).and_then(|t| texture(&chain, t)) else { continue };
            let uvs = match face.get("uv").and_then(Value::as_array) {
                Some(a) if a.len() == 4 => [0, 1, 2, 3].map(|i| a[i].as_f64().unwrap_or(0.0) as f32),
                _ => {
                    let c = dir.corners(from, to);
                    let (lo, hi) = (dir.default_uv(c[0]), dir.default_uv(c[2]));
                    [lo[0], lo[1], hi[0], hi[1]]
                }
            };
            let shift = ((face.get("rotation").and_then(Value::as_i64).unwrap_or(0).rem_euclid(360)) / 90) as usize;
            let corners = dir.corners(from, to);
            let mut pos = [[0.0f32; 3]; 4];
            let mut uv = [[0.0f32; 2]; 4];
            for i in 0..4 {
                let mut p = corners[i].map(|c| c / 16.0);
                if let Some((origin, m)) = &rotation {
                    let local = apply(m, [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]]);
                    p = [local[0] + origin[0], local[1] + origin[1], local[2] + origin[2]];
                }
                if turned {
                    let local = apply(&block, [p[0] - 0.5, p[1] - 0.5, p[2] - 0.5]);
                    p = [local[0] + 0.5, local[1] + 0.5, local[2] + 0.5];
                }
                pos[i] = p.map(|c| (c * 1e5).round() / 1e5);
                let k = (i + shift) % 4;
                uv[i] = [if k < 2 { uvs[0] } else { uvs[2] } / 16.0, if k == 0 || k == 3 { uvs[1] } else { uvs[3] } / 16.0];
            }
            let normal = {
                let (a, b) = (sub(pos[1], pos[0]), sub(pos[2], pos[0]));
                let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                n.map(|c| c / len)
            };
            let facing = if normal.iter().all(|c| c.is_finite()) { Dir::closest(normal) } else { None };
            if rotation.is_none() && let Some(f) = facing {
                rewind(&mut pos, &mut uv, f);
            }
            if variant.uvlock && turned && let Some(f) = facing {
                for i in 0..4 {
                    let p = pos[i].map(|c| c * 16.0);
                    uv[i] = f.default_uv(p).map(|c| c / 16.0);
                }
            }
            let cull = face.get("cullface").and_then(Value::as_str).and_then(Dir::parse).map(|d| turn(d, &block, turned));
            quads.push(Quad { pos, uv, texture: tex, cull, face: facing, shade: shade.or(facing), tint: face.get("tintindex").and_then(Value::as_i64).map_or(-1, |t| t as i32), emission, ao });
        }
    }
    quads
}

fn rewind(pos: &mut [[f32; 3]; 4], uv: &mut [[f32; 2]; 4], facing: Dir) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in pos.iter() {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let want = facing.corners(lo, hi);
    for k in 0..4 {
        if let Some(found) = (k..4).find(|&i| pos[i] == want[k]) {
            pos.swap(k, found);
            uv.swap(k, found);
        }
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn turn(d: Dir, m: &M3, turned: bool) -> Dir {
    if !turned {
        return d;
    }
    let s = d.step();
    Dir::closest(apply(m, [s[0] as f32, s[1] as f32, s[2] as f32])).unwrap_or(d)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::super::block::decode;
    use super::super::states::BLOCKS;
    use super::*;

    #[derive(Default)]
    struct Pack {
        states: HashMap<String, Value>,
        models: HashMap<String, Value>,
    }

    impl Source for Pack {
        fn blockstate(&self, name: &str) -> Option<&Value> {
            self.states.get(name)
        }
        fn model(&self, path: &str) -> Option<&Value> {
            self.models.get(path)
        }
    }

    fn state(name: &str, offset: u32) -> State {
        decode(BLOCKS.iter().find(|b| b.1 == name).unwrap().0 + offset).unwrap()
    }

    fn cube() -> Pack {
        let mut p = Pack::default();
        p.models.insert("block/cube".into(), json!({"elements": [{"from": [0,0,0], "to": [16,16,16], "faces": {
            "down": {"texture": "#down", "cullface": "down"}, "up": {"texture": "#up", "cullface": "up", "tintindex": 0},
            "north": {"texture": "#north", "cullface": "north"}, "south": {"texture": "#south", "cullface": "south"},
            "west": {"texture": "#west", "cullface": "west"}, "east": {"texture": "#east", "cullface": "east"}}}]}));
        p.models.insert("block/cube_all".into(), json!({"parent": "block/cube", "textures": {"down": "#all", "up": "#all", "north": "#all", "south": "#all", "west": "#all", "east": "#all"}}));
        p.models.insert("block/stone".into(), json!({"parent": "minecraft:block/cube_all", "textures": {"all": "minecraft:block/stone"}}));
        p
    }

    #[test]
    fn a_full_cube_inherits_elements_and_resolves_texture_references() {
        let p = cube();
        let quads = bake(&p, &Variant { model: "block/stone".into(), x: 0, y: 0, uvlock: false });
        assert_eq!(quads.len(), 6);
        assert!(quads.iter().all(|q| q.texture == "block/stone" && q.shade == q.face && q.ao && q.cull == q.face));
        let up = quads.iter().find(|q| q.face == Some(Dir::Up)).unwrap();
        assert_eq!(up.pos, [[0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0]]);
        assert_eq!(up.uv, [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]);
        assert_eq!(up.tint, 0);
    }

    #[test]
    fn variants_match_partial_keys_and_the_empty_key() {
        let all = json!({"variants": {"": {"model": "minecraft:block/stone"}}});
        assert_eq!(variants(&all, &state("stone", 0), 0)[0].model, "block/stone");
        let grass = json!({"variants": {"snowy=false": {"model": "block/grass_block"}, "snowy=true": {"model": "block/grass_block_snow"}}});
        assert_eq!(variants(&grass, &state("grass_block", 0), 0)[0].model, "block/grass_block_snow");
        assert_eq!(variants(&grass, &state("grass_block", 1), 0)[0].model, "block/grass_block");
    }

    #[test]
    fn multipart_applies_every_part_whose_condition_holds() {
        let fence = json!({"multipart": [
            {"apply": {"model": "block/oak_fence_post"}},
            {"when": {"north": "true"}, "apply": {"model": "block/oak_fence_side", "uvlock": true}},
            {"when": {"OR": [{"east": "true"}, {"west": "true"}]}, "apply": {"model": "block/oak_fence_side", "y": 90}},
            {"when": {"south": "false|true", "north": "!true"}, "apply": {"model": "block/x"}}
        ]});
        let s = State { name: "oak_fence", properties: vec![("east", "false"), ("north", "true"), ("south", "false"), ("waterlogged", "false"), ("west", "true")] };
        let got: Vec<_> = variants(&fence, &s, 0).into_iter().map(|v| (v.model, v.y, v.uvlock)).collect();
        assert_eq!(got, vec![("block/oak_fence_post".into(), 0, false), ("block/oak_fence_side".into(), 0, true), ("block/oak_fence_side".into(), 1, false)]);
    }

    #[test]
    fn a_y_turn_maps_north_to_east_and_turns_cull_faces() {
        let p = cube();
        let quads = bake(&p, &Variant { model: "block/stone".into(), x: 0, y: 1, uvlock: false });
        let was_north = quads.iter().find(|q| q.cull == Some(Dir::East)).unwrap();
        assert_eq!(was_north.face, Some(Dir::East));
        assert!(was_north.pos.iter().all(|p| (p[0] - 1.0).abs() < 1e-5));
        assert_eq!(was_north.pos, Dir::East.corners([1.0, 0.0, 0.0], [1.0, 1.0, 1.0]), "turned faces wind like unturned ones");
        let x = bake(&p, &Variant { model: "block/stone".into(), x: 1, y: 0, uvlock: false });
        let was_up = x.iter().find(|q| q.tint == 0).unwrap();
        assert_eq!(was_up.face, Some(Dir::North), "x=90 turns the top to the north");
    }

    #[test]
    fn uvlock_keeps_the_top_texture_aligned_to_the_world() {
        let p = cube();
        let locked = bake(&p, &Variant { model: "block/stone".into(), x: 0, y: 1, uvlock: true });
        let up = locked.iter().find(|q| q.face == Some(Dir::Up)).unwrap();
        for (pos, uv) in up.pos.iter().zip(up.uv) {
            assert_eq!(uv, [pos[0], pos[2]]);
        }
    }

    #[test]
    fn a_slab_face_takes_the_default_uv_of_its_extent() {
        let mut p = Pack::default();
        p.models.insert("block/slab".into(), json!({"textures": {"side": "block/s"}, "elements": [{"from": [0,0,0], "to": [16,8,16], "faces": {"north": {"texture": "#side", "cullface": "north"}, "up": {"texture": "#side"}}}]}));
        let quads = bake(&p, &Variant { model: "block/slab".into(), x: 0, y: 0, uvlock: false });
        let north = quads.iter().find(|q| q.face == Some(Dir::North)).unwrap();
        assert_eq!(north.uv[0], [0.0, 0.5]);
        assert_eq!(north.uv[1], [0.0, 1.0]);
        let up = quads.iter().find(|q| q.face == Some(Dir::Up)).unwrap();
        assert_eq!(up.cull, None);
        assert!(up.pos.iter().all(|p| p[1] == 0.5));
    }

    #[test]
    fn a_cross_plant_rotates_and_rescales_its_planes() {
        let mut p = Pack::default();
        p.models.insert("block/cross".into(), json!({"ambientocclusion": false, "textures": {"cross": "block/short_grass"}, "elements": [{"from": [0.8,0,8], "to": [15.2,16,8], "shade_direction_override": "up",
            "rotation": {"origin": [8,8,8], "axis": "y", "angle": 45, "rescale": true}, "faces": {"north": {"texture": "#cross"}, "south": {"texture": "#cross", "rotation": 90}}}]}));
        let quads = bake(&p, &Variant { model: "block/cross".into(), x: 0, y: 0, uvlock: false });
        assert_eq!(quads.len(), 2);
        let q = &quads[0];
        assert!(!q.ao && q.shade == Some(Dir::Up));
        let span = ((q.pos[0][0] - q.pos[3][0]).powi(2) + (q.pos[0][2] - q.pos[3][2]).powi(2)).sqrt();
        assert!((span - 14.4 / 16.0 * 2f32.sqrt()).abs() < 1e-3, "rescaled to the block's diagonal: {span}");
        assert!((quads[1].uv[0][0] - 0.05).abs() < 1e-5 && quads[1].uv[0][1] == 1.0, "a face rotation shifts the corners: {:?}", quads[1].uv);
    }

    #[test]
    fn a_weighted_list_picks_the_same_variant_for_the_same_seed() {
        let list = json!({"variants": {"": [{"model": "block/a"}, {"model": "block/a", "y": 90}, {"model": "block/a", "y": 180}, {"model": "block/a", "y": 270}]}});
        let s = state("stone", 0);
        let a = variants(&list, &s, 12345);
        assert_eq!(a, variants(&list, &s, 12345));
        let ys: std::collections::HashSet<u8> = (0..64).map(|i| variants(&list, &s, super::super::random::seed(i, 64, i * 7))[0].y).collect();
        assert_eq!(ys.len(), 4, "every rotation shows up");
    }
}
