//! Light the way the game computes it: smooth lighting (ambient occlusion with blended light per corner) for model faces,
//! and the lightmap that turns sky and block light levels into a colour.

use super::model::Dir;

/// Light as the game packs it: block light in bits 0..7 and sky light in bits 16..23, each a level times 16.
pub type Packed = u32;

pub fn pack(sky: u8, block: u8) -> Packed {
    (u32::from(block.min(15)) << 4) | (u32::from(sky.min(15)) << 20)
}

fn sky_of(p: Packed) -> u32 {
    (p >> 16) & 0xFF
}

fn block_of(p: Packed) -> u32 {
    p & 0xFF
}

/// Blends a corner's three neighbours with the face's centre, as the game does: dark neighbours borrow the centre.
pub fn smooth(n1: Packed, n2: Packed, n3: Packed, center: Packed) -> Packed {
    let mut n = [n1, n2, n3];
    if sky_of(center) > 2 << 4 || block_of(center) > 2 << 4 {
        for v in &mut n {
            if *v == 0 {
                *v = center;
            } else if sky_of(*v) == 0 {
                *v |= center & 0x00FF_0000;
            }
        }
    }
    ((n[0] + n[1] + n[2] + center) >> 2) & 0x00FF_00FF
}

fn weighted(c: [Packed; 4], w: [f32; 4]) -> Packed {
    let channel = |shift: u32| {
        let v: f32 = (0..4).map(|i| ((c[i] >> shift) & 0xFF) as f32 * w[i]).sum();
        (v as u32).min(0xFF)
    };
    channel(0) | (channel(16) << 16)
}

/// The four blocks around a face, per face direction.
pub const CORNERS: [[Dir; 4]; 6] = [
    [Dir::West, Dir::East, Dir::North, Dir::South],
    [Dir::East, Dir::West, Dir::North, Dir::South],
    [Dir::Up, Dir::Down, Dir::East, Dir::West],
    [Dir::West, Dir::East, Dir::Down, Dir::Up],
    [Dir::Up, Dir::Down, Dir::North, Dir::South],
    [Dir::Down, Dir::Up, Dir::North, Dir::South],
];

const REMAP: [[usize; 4]; 6] = [[0, 1, 2, 3], [2, 3, 0, 1], [3, 0, 1, 2], [0, 1, 2, 3], [3, 0, 1, 2], [1, 2, 3, 0]];

const D: u8 = 0;
const U: u8 = 1;
const N: u8 = 2;
const S: u8 = 3;
const W: u8 = 4;
const E: u8 = 5;
const FD: u8 = 6;
const FU: u8 = 7;
const FN: u8 = 8;
const FS: u8 = 9;
const FW: u8 = 10;
const FE: u8 = 11;

const WEIGHTS: [[[u8; 8]; 4]; 6] = [
    [[FW, S, FW, FS, W, FS, W, S], [FW, N, FW, FN, W, FN, W, N], [FE, N, FE, FN, E, FN, E, N], [FE, S, FE, FS, E, FS, E, S]],
    [[E, S, E, FS, FE, FS, FE, S], [E, N, E, FN, FE, FN, FE, N], [W, N, W, FN, FW, FN, FW, N], [W, S, W, FS, FW, FS, FW, S]],
    [[U, FW, U, W, FU, W, FU, FW], [U, FE, U, E, FU, E, FU, FE], [D, FE, D, E, FD, E, FD, FE], [D, FW, D, W, FD, W, FD, FW]],
    [[U, FW, FU, FW, FU, W, U, W], [D, FW, FD, FW, FD, W, D, W], [D, FE, FD, FE, FD, E, D, E], [U, FE, FU, FE, FU, E, U, E]],
    [[U, S, U, FS, FU, FS, FU, S], [U, N, U, FN, FU, FN, FU, N], [D, N, D, FN, FD, FN, FD, N], [D, S, D, FS, FD, FS, FD, S]],
    [[FD, S, FD, FS, D, FS, D, S], [FD, N, FD, FN, D, FN, D, N], [FU, N, FU, FN, U, FN, U, N], [FU, S, FU, FS, U, FS, U, S]],
];

/// The shape values of a face's bounds, indexed like the weight tables: down, up, north, south, west, east, then flipped.
pub fn shape(lo: [f32; 3], hi: [f32; 3]) -> [f32; 12] {
    let s = [lo[1], hi[1], lo[2], hi[2], lo[0], hi[0]];
    let mut out = [0.0; 12];
    for i in 0..6 {
        out[i] = s[i];
        out[i + 6] = 1.0 - s[i];
    }
    out
}

/// What smooth lighting samples around one face.
pub struct Samples {
    /// Shade brightness and light of the four side blocks.
    pub side: [(f32, Packed); 4],
    /// Of the four diagonal blocks, in the order 0-2, 0-3, 1-2, 1-3.
    pub diagonal: [(f32, Packed); 4],
    pub center: (f32, Packed),
}

/// The AO factor and light of each of the face's four vertices.
pub fn ambient(face: Dir, samples: &Samples, shape: &[f32; 12]) -> [(f32, Packed); 4] {
    let [(s0, l0), (s1, l1), (s2, l2), (s3, l3)] = samples.side;
    let [(c02, k02), (c03, k03), (c12, k12), (c13, k13)] = samples.diagonal;
    let (sc, lc) = samples.center;
    let shades = [(s3 + s0 + c03 + sc) * 0.25, (s2 + s0 + c02 + sc) * 0.25, (s2 + s1 + c12 + sc) * 0.25, (s3 + s1 + c13 + sc) * 0.25];
    let lights = [smooth(l3, l0, k03, lc), smooth(l2, l0, k02, lc), smooth(l2, l1, k12, lc), smooth(l3, l1, k13, lc)];
    let f = face.index();
    let mut out = [(0.0, 0); 4];
    for k in 0..4 {
        let t = &WEIGHTS[f][k];
        let w = [0, 1, 2, 3].map(|i| shape[usize::from(t[i * 2])] * shape[usize::from(t[i * 2 + 1])]);
        let ao = (0..4).map(|i| shades[i] * w[i]).sum::<f32>().clamp(0.0, 1.0);
        out[REMAP[f][k]] = (ao, weighted(lights, w));
    }
    out
}

/// The overworld lightmap at one moment: sky and block levels (0..15) to a colour.
pub struct Lightmap {
    table: [[[f32; 3]; 16]; 16],
}

const NIGHT: [f32; 3] = [122.0 / 255.0, 122.0 / 255.0, 1.0];
const AMBIENT: f32 = 10.0 / 255.0;
const BLOCK_TINT: [f32; 3] = [1.0, 216.0 / 255.0, 140.0 / 255.0];
const BLOCK_FACTOR: f32 = 1.4;
const BRIGHTNESS: f32 = 0.5;

fn lerp(t: f32, a: f32, b: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(t: f32, a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| lerp(t, a[i], b[i]))
}

/// A linear keyframe track over the day (0..24000), wrapping from the last key to the first.
pub fn track(keys: &[(f32, f32)], time: f32) -> f32 {
    let t = time.rem_euclid(24000.0);
    for pair in keys.windows(2) {
        let ((a, va), (b, vb)) = (pair[0], pair[1]);
        if t >= a && t <= b {
            return lerp((t - a) / (b - a), va, vb);
        }
    }
    let ((a, va), (b, vb)) = (keys[keys.len() - 1], keys[0]);
    let (b, t) = (b + 24000.0, if t < a { t + 24000.0 } else { t });
    lerp((t - a) / (b - a), va, vb)
}

const SKY_FACTOR: [(f32, f32); 4] = [(730.0, 1.0), (11270.0, 1.0), (13140.0, 0.24), (22860.0, 0.24)];
const SKY_DAYLIGHT: [(f32, f32); 4] = [(730.0, 1.0), (11270.0, 1.0), (13140.0, 0.0), (22860.0, 0.0)];

impl Lightmap {
    pub fn new(time: f32, rain: f32, thunder: f32) -> Self {
        let mut factor = track(&SKY_FACTOR, time);
        let mut color = lerp3(track(&SKY_DAYLIGHT, time), NIGHT, [1.0; 3]);
        let wet = (rain - thunder).max(0.0);
        if wet > 0.0 {
            factor = lerp(wet, factor, lerp(0.3125, factor, 0.24));
            color = lerp3(wet, color, lerp3(79.0 / 255.0, color, NIGHT));
        }
        if thunder > 0.0 {
            factor = lerp(thunder, factor, lerp(0.527_343_75, factor, 0.24));
            color = lerp3(thunder, color, lerp3(134.0 / 255.0, color, NIGHT));
        }
        let g = |l: f32| l / (4.0 - 3.0 * l);
        let mut table = [[[0.0; 3]; 16]; 16];
        for (s, row) in table.iter_mut().enumerate() {
            for (b, cell) in row.iter_mut().enumerate() {
                let (bl, sl) = (b as f32 / 15.0, s as f32 / 15.0);
                let sky = g(sl) * factor;
                let block = g(bl) * BLOCK_FACTOR;
                let warm = lerp3(0.9 * (2.0 * bl - 1.0).powi(2), BLOCK_TINT, [1.0; 3]);
                let c = [0, 1, 2].map(|i| (AMBIENT + color[i] * sky + warm[i] * block).clamp(0.0, 1.0));
                let most = c[0].max(c[1]).max(c[2]);
                let lifted = if most > 0.0 { c.map(|v| v * ((1.0 - (1.0 - most).powi(4)) / most)) } else { c };
                *cell = lerp3(BRIGHTNESS, c, lifted);
            }
        }
        Self { table }
    }

    /// The colour of packed light `p`, interpolating between levels like the game's lightmap texture.
    pub fn color(&self, p: Packed) -> [f32; 3] {
        let at = |raw: u32| {
            let x = (raw as f32 / 16.0).clamp(0.0, 15.0);
            let i = (x.floor() as usize).min(14);
            (i, x - i as f32)
        };
        let (s, ts) = at(sky_of(p));
        let (b, tb) = at(block_of(p));
        let row = |s: usize| lerp3(tb, self.table[s][b], self.table[s][b + 1]);
        lerp3(ts, row(s), row(s + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: [f32; 3]) -> [u8; 3] {
        c.map(|v| (v * 255.0).round() as u8)
    }

    #[test]
    fn the_lightmap_matches_the_game_at_day_and_night() {
        let day = Lightmap::new(6000.0, 0.0, 0.0);
        assert_eq!(rgb(day.color(pack(15, 0))), [255, 255, 255]);
        assert!(rgb(day.color(pack(0, 0)))[0].abs_diff(24) <= 1);
        let night = Lightmap::new(18000.0, 0.0, 0.0);
        let n = rgb(night.color(pack(15, 0)));
        assert!(n[0].abs_diff(71) <= 1 && n[2].abs_diff(129) <= 1, "{n:?}");
        let torch = rgb(night.color(pack(0, 8)));
        assert!(torch[0].abs_diff(150) <= 1 && torch[2].abs_diff(90) <= 1, "{torch:?}");
    }

    #[test]
    fn a_dark_neighbour_borrows_the_centre_light() {
        let lit = pack(15, 0);
        assert_eq!(smooth(0, 0, 0, lit), lit);
        assert_eq!(smooth(lit, lit, lit, lit), lit);
    }

    #[test]
    fn an_open_full_face_is_fully_lit_and_a_corner_against_two_walls_darkens() {
        let lit = pack(15, 0);
        let full = shape([0.0; 3], [1.0, 1.0, 1.0]);
        let open = Samples { side: [(1.0, lit); 4], diagonal: [(1.0, lit); 4], center: (1.0, lit) };
        assert!(ambient(Dir::Up, &open, &full).iter().all(|(ao, l)| *ao == 1.0 && *l == lit));
        let mut walled = Samples { side: [(1.0, lit); 4], diagonal: [(1.0, lit); 4], center: (1.0, lit) };
        walled.side[0] = (0.2, 0);
        walled.side[2] = (0.2, 0);
        walled.diagonal[0] = (0.2, 0);
        let v = ambient(Dir::Up, &walled, &full);
        let darkest = v.iter().map(|x| x.0).fold(1.0f32, f32::min);
        assert!((darkest - 0.4).abs() < 1e-6, "{v:?}");
    }
}
