//! Stand-in game pictures for the theme screens: small block landscapes drawn from a seed, so tiles look like
//! running games without shipping real screenshots.

/// Width and height of a picture in pixels, and the side of one block.
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 360;
const BLOCK: u32 = 10;

/// Time of day of a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sky {
    Day,
    Dusk,
    Night,
}

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

type Rgb = [f32; 3];

fn hex(v: u32) -> Rgb {
    [((v >> 16) & 0xFF) as f32, ((v >> 8) & 0xFF) as f32, (v & 0xFF) as f32]
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn scale(a: Rgb, k: f32) -> Rgb {
    [a[0] * k, a[1] * k, a[2] * k]
}

/// An RGBA picture of a block landscape for `seed` at time `sky`.
pub fn scene(seed: u32, sky: Sky) -> Vec<u8> {
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761).max(1));
    let cols = WIDTH / BLOCK;
    let rows = HEIGHT / BLOCK;
    let (top, horizon) = match sky {
        Sky::Day => (hex(0x6FA4F2), hex(0xB9D6FB)),
        Sky::Dusk => (hex(0x4E5C9C), hex(0xF2A96B)),
        Sky::Night => (hex(0x0B1026), hex(0x1E2A4E)),
    };
    let light = match sky {
        Sky::Day => 1.0,
        Sky::Dusk => 0.78,
        Sky::Night => 0.34,
    };
    let sea = rows * 2 / 3 + rng.below(3);
    let phase = [rng.unit() * std::f32::consts::TAU, rng.unit() * std::f32::consts::TAU, rng.unit() * std::f32::consts::TAU];
    let base = rows as f32 * (0.52 + rng.unit() * 0.12);
    let ground: Vec<u32> = (0..cols)
        .map(|x| {
            let x = x as f32;
            let h = base + (x * 0.11 + phase[0]).sin() * 3.5 + (x * 0.29 + phase[1]).sin() * 1.6;
            h.round().clamp(6., rows as f32 - 3.) as u32
        })
        .collect();
    let hills: Vec<u32> = (0..cols)
        .map(|x| {
            let h = base - 4. + (x as f32 * 0.07 + phase[2]).sin() * 4.;
            h.round().max(4.) as u32
        })
        .collect();
    let mut grid = vec![[0f32; 3]; (cols * rows) as usize];
    let set = |grid: &mut Vec<Rgb>, x: u32, y: u32, color: Rgb| {
        if x < cols && y < rows {
            grid[(y * cols + x) as usize] = color;
        }
    };
    for y in 0..rows {
        let t = (y as f32 / sea as f32).min(1.);
        for x in 0..cols {
            set(&mut grid, x, y, mix(top, horizon, t));
        }
    }
    if sky == Sky::Night {
        for _ in 0..40 {
            let (x, y) = (rng.below(cols), rng.below(sea.saturating_sub(6)));
            set(&mut grid, x, y, hex(0xC9D3F2));
        }
    }
    let (sun_x, sun_y) = (4 + rng.below(cols - 10), 2 + rng.below(4));
    let sun = match sky {
        Sky::Day => hex(0xFFF3C4),
        Sky::Dusk => hex(0xFFC27A),
        Sky::Night => hex(0xE6EAF4),
    };
    for dy in 0..3 {
        for dx in 0..3 {
            set(&mut grid, sun_x + dx, sun_y + dy, sun);
        }
    }
    if sky != Sky::Night {
        for _ in 0..3 + rng.below(3) {
            let (x, y, w) = (rng.below(cols), 3 + rng.below(6), 4 + rng.below(7));
            for dx in 0..w {
                set(&mut grid, x + dx, y, scale(hex(0xF4F7FB), light));
                if dx > 0 && dx + 1 < w {
                    set(&mut grid, x + dx, y + 1, scale(hex(0xE3E9F2), light));
                }
            }
        }
    }
    let haze = mix(hex(0x5E8F55), horizon, 0.55);
    for x in 0..cols {
        for y in hills[x as usize]..rows {
            set(&mut grid, x, y, scale(haze, light.max(0.5)));
        }
    }
    let grass = hex(0x6AA548);
    let dirt = hex(0x7A5636);
    let stone = hex(0x7E7E80);
    let sand = hex(0xD8C88E);
    let water = hex(0x3B6FD6);
    for x in 0..cols {
        let g = ground[x as usize];
        let beach = g + 1 >= sea;
        for y in g..rows {
            let depth = y - g;
            let color = match depth {
                0..=3 if beach => sand,
                0 => grass,
                1..=3 => dirt,
                _ => stone,
            };
            set(&mut grid, x, y, scale(color, light));
        }
        for y in sea..g {
            set(&mut grid, x, y, scale(water, light));
        }
    }
    let mut x = 2 + rng.below(4);
    while x + 3 < cols {
        let g = ground[x as usize];
        if g + 2 < sea && rng.unit() < 0.55 {
            let height = 3 + rng.below(2);
            for dy in 1..=height {
                set(&mut grid, x, g - dy, scale(hex(0x6B4E2E), light));
            }
            let leaves = scale(hex(0x3F7A30), light);
            for dy in 0..3 {
                for dx in 0..5 {
                    if dy == 0 && (dx == 0 || dx == 4) {
                        continue;
                    }
                    set(&mut grid, (x + dx).saturating_sub(2), g.saturating_sub(height + 3) + dy, leaves);
                }
            }
        }
        x += 5 + rng.below(6);
    }
    let mut out = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    for by in 0..rows {
        for bx in 0..cols {
            let k = 0.93 + rng.unit() * 0.12;
            let color = scale(grid[(by * cols + bx) as usize], k);
            for py in 0..BLOCK {
                for px in 0..BLOCK {
                    let grain = if (px ^ py ^ rng.below(3)) & 3 == 0 { 0.96 } else { 1.0 };
                    let i = (((by * BLOCK + py) * WIDTH + bx * BLOCK + px) * 4) as usize;
                    out[i] = (color[0] * grain).clamp(0., 255.) as u8;
                    out[i + 1] = (color[1] * grain).clamp(0., 255.) as u8;
                    out[i + 2] = (color[2] * grain).clamp(0., 255.) as u8;
                    out[i + 3] = 255;
                }
            }
        }
    }
    out
}

/// An RGBA 8×8 face for an account chip, drawn from `seed` around `skin`.
pub fn face(seed: u32, skin: [u8; 3]) -> Vec<u8> {
    let mut rng = Rng(seed.wrapping_mul(40_503).max(1));
    let skin = [f32::from(skin[0]), f32::from(skin[1]), f32::from(skin[2])];
    let hair = scale(hex([0x3B2A1E, 0x6B4A2B, 0xC9A56B, 0x2A2A2E, 0x8A3B24][rng.below(5) as usize]), 1.);
    let eyes = hex([0x3A6FD8, 0x3E8A3A, 0x5A3A22, 0x2A2A2E][rng.below(4) as usize]);
    let mut out = vec![0u8; 8 * 8 * 4];
    for y in 0..8u32 {
        for x in 0..8u32 {
            let mut color = scale(skin, 0.94 + rng.unit() * 0.1);
            if y < 2 || (y == 2 && (x == 0 || x == 7)) {
                color = scale(hair, 0.92 + rng.unit() * 0.14);
            }
            if y == 4 && (x == 1 || x == 2 || x == 5 || x == 6) {
                color = if x == 2 || x == 5 { eyes } else { hex(0xF2F2F2) };
            }
            if y == 6 && (3..=4).contains(&x) {
                color = scale(skin, 0.7);
            }
            let i = ((y * 8 + x) * 4) as usize;
            out[i] = color[0].clamp(0., 255.) as u8;
            out[i + 1] = color[1].clamp(0., 255.) as u8;
            out[i + 2] = color[2].clamp(0., 255.) as u8;
            out[i + 3] = 255;
        }
    }
    out
}
