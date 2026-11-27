//! The sky behind the blocks as the game draws it: the sky and fog colours of the day timeline and the weather, the
//! sky disc fading into fog, the sunrise glow, the sun, the moon in its phase, the stars, and the dark disc below.

use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

use super::assets::Assets;
use super::environment::{Environment, Skybox};
use super::Biome;
use super::light::track;
use super::random::Legacy;
use super::raster::Target;
use super::scene::View;

const SKY_MULTIPLIER: [(f32, f32); 4] = [(133.0, 1.0), (11867.0, 1.0), (13670.0, 0.0), (22330.0, 0.0)];
const FOG_NIGHT: [f32; 3] = [15.0 / 255.0, 15.0 / 255.0, 22.0 / 255.0];
const STARS: [(f32, f32); 12] = [
    (92.0, 0.037), (627.0, 0.0), (11373.0, 0.0), (11732.0, 0.016), (11959.0, 0.044), (12399.0, 0.143), (12729.0, 0.258), (13228.0, 0.5),
    (22772.0, 0.5), (23032.0, 0.364), (23356.0, 0.225), (23758.0, 0.101),
];
const GLOW: [(f32, u32); 32] = [
    (71.0, 0x5fefa333), (310.0, 0x29f5ba33), (565.0, 0x06fbd433), (730.0, 0x00ffe533), (11270.0, 0x00ffe533), (11397.0, 0x04fcd833),
    (11522.0, 0x0ff9cb33), (11690.0, 0x29f5ba33), (11929.0, 0x5fefa333), (12243.0, 0xb1e78733), (12358.0, 0xcce47e33), (12512.0, 0xe9e07233),
    (12613.0, 0xf6dd6b33), (12732.0, 0xfeda6333), (12841.0, 0xfed75c33), (13035.0, 0xecd25133), (13252.0, 0xc1cc4733), (13775.0, 0x36be3733),
    (13888.0, 0x1fbb3533), (14039.0, 0x09b73333), (14192.0, 0x00b33333), (21807.0, 0x00b23333), (21961.0, 0x09b73333), (22112.0, 0x1fbb3533),
    (22225.0, 0x36be3733), (22748.0, 0xc1cc4733), (22965.0, 0xecd25133), (23159.0, 0xfed75c33), (23272.0, 0xfeda6333), (23488.0, 0xe9e07233),
    (23642.0, 0xcce47e33), (23757.0, 0xb1e78733),
];
const DEFAULT_SKY: u32 = 0x78A7FF;
const DEFAULT_FOG: u32 = 0xC0D8FF;
const HORIZON: f64 = 63.0;

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16 & 0xFF) as f32 / 255.0, (c >> 8 & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn times(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2]]
}

fn glow_at(time: f32) -> [f32; 4] {
    let channel = |shift: u32| GLOW.map(|(t, c)| (t, (c >> shift & 0xFF) as f32 / 255.0));
    [track(&channel(16), time), track(&channel(8), time), track(&channel(0), time), track(&channel(24), time)]
}

fn bezier(p: f32, (x1, y1, x2, y2): (f32, f32, f32, f32)) -> f32 {
    let curve = |s: f32, a: f32, b: f32| {
        let (k3, k2, k1) = (3.0 * a - 3.0 * b + 1.0, -6.0 * a + 3.0 * b, 3.0 * a);
        ((k3 * s + k2) * s + k1) * s
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..32 {
        let mid = (lo + hi) * 0.5;
        if curve(mid, x1, x2) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    curve((lo + hi) * 0.5, y1, y2)
}

/// The sun's angle in radians at a day time: 0 at noon, the sun rising in the east.
pub fn sun_angle(time: f32) -> f32 {
    let p = (time - 6000.0).rem_euclid(24000.0) / 24000.0;
    TAU * bezier(p, (0.362, 0.241, 0.638, 0.759))
}

fn gray(c: [f32; 3], brightness: f32, factor: f32) -> [f32; 3] {
    let g = ((c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11) * brightness).clamp(0.0, 1.0);
    mix(c, [g; 3], factor)
}

/// Turns `v` the way the game poses celestial bodies at angle `a`.
fn pose(a: f32, v: [f32; 3]) -> [f32; 3] {
    let (s, c) = a.sin_cos();
    let r = [v[0], v[1] * c - v[2] * s, v[1] * s + v[2] * c];
    [-r[2], r[1], r[0]]
}

/// The colours and bodies of the sky at one moment, seen from one place.
pub struct Atmosphere {
    sky: [f32; 3],
    /// The colour distant terrain and the clear sky fade into.
    pub fog: [f32; 3],
    sun: f32,
    phase: usize,
    glow: [f32; 4],
    stars: f32,
    clear: f32,
    sky_end: f32,
    dark: bool,
    skybox: Skybox,
}

impl Atmosphere {
    /// `ticks` is the day time the server sent, `distance` the render distance in chunks, `forward` the view
    /// direction and `eye` the eye height.
    pub fn new(ticks: i64, rain: f32, thunder: f32, biome: &Biome, env: &Environment, distance: u8, forward: [f32; 3], eye: f64) -> Self {
        let time = ticks.rem_euclid(24000) as f32;
        let sun = sun_angle(time);
        let day = env.follows_the_day();
        let (rain, thunder) = if day { (rain, thunder) } else { (0.0, 0.0) };
        let wet = (rain - thunder).max(0.0);
        let light = if day { track(&SKY_MULTIPLIER, time) } else { 1.0 };
        let sky_color = biome.sky.or(env.sky).or(day.then_some(DEFAULT_SKY));
        let mut sky = rgb(sky_color.unwrap_or(0)).map(|c| c * light);
        let mut fog = times(rgb(biome.fog.or(env.fog).unwrap_or(DEFAULT_FOG)), mix(FOG_NIGHT, [1.0; 3], light));
        let mut glow = if day { glow_at(time) } else { [0.0; 4] };
        let mut stars = if day { track(&STARS, time) } else { 0.0 };
        if wet > 0.0 {
            sky = mix(sky, gray(sky, 0.6, 0.75), wet);
            fog = mix(fog, times(fog, [0.5, 0.5, 0.6]), wet);
            let g = times([glow[0], glow[1], glow[2]], [0.5, 0.5, 0.6]);
            glow = [0, 1, 2, 3].map(|i| if i < 3 { glow[i] + (g[i] - glow[i]) * wet } else { glow[3] });
            stars *= 1.0 - wet;
        }
        if thunder > 0.0 {
            sky = mix(sky, gray(sky, 0.24, 0.94), thunder);
            fog = mix(fog, times(fog, [0.25, 0.25, 0.3]), thunder);
            let g = times([glow[0], glow[1], glow[2]], [0.25, 0.25, 0.3]);
            glow = [0, 1, 2, 3].map(|i| if i < 3 { glow[i] + (g[i] - glow[i]) * thunder } else { glow[3] });
            stars *= 1.0 - thunder;
        }
        if distance >= 4 && glow[3] > 0.0 {
            let toward = if sun.sin() > 0.0 { -1.0 } else { 1.0 };
            let looking = forward[0] * toward;
            if looking > 0.0 {
                fog = mix(fog, [glow[0], glow[1], glow[2]], looking * glow[3]);
            }
        }
        let darkened = [sky[0] * (1.0 - 0.5 * rain), sky[1] * (1.0 - 0.5 * rain), sky[2] * (1.0 - 0.4 * rain)].map(|c| c * (1.0 - 0.5 * thunder));
        let chunks = f32::from(distance).min(32.0);
        let blend = 1.0 - (0.25 + 0.75 * (chunks / 32.0).clamp(0.0, 1.0)).powf(0.25);
        let fog = if sky_color.is_some() { mix(fog, darkened, blend) } else { fog };
        Self {
            sky,
            fog,
            sun,
            phase: ticks.div_euclid(24000).rem_euclid(8) as usize,
            glow,
            stars,
            clear: 1.0 - rain,
            sky_end: (f32::from(distance) * 16.0).min(512.0),
            dark: day && eye < HORIZON,
            skybox: env.skybox,
        }
    }

    fn disc(&self, d: [f32; 3], height: f32, base: [f32; 3]) -> Option<[f32; 3]> {
        let horizontal = (d[0] * d[0] + d[2] * d[2]).sqrt();
        let up = d[1] * height.signum();
        if up <= 0.0 {
            return None;
        }
        let r = height.abs() * horizontal / up;
        let sector = d[2].atan2(d[0]).rem_euclid(PI / 4.0) - PI / 8.0;
        let edge = 512.0 * (PI / 8.0).cos() / sector.cos();
        (r <= edge).then(|| mix(base, self.fog, ((16.0 + 496.25 * r / edge) / self.sky_end).clamp(0.0, 1.0)))
    }
}

fn stars() -> &'static [([f32; 3], [f32; 3], [f32; 3])] {
    static STARS: OnceLock<Vec<([f32; 3], [f32; 3], [f32; 3])>> = OnceLock::new();
    STARS.get_or_init(|| {
        let mut random = Legacy::new(10842);
        let mut out = Vec::new();
        for _ in 0..1500 {
            let p = [random.next_float() * 2.0 - 1.0, random.next_float() * 2.0 - 1.0, random.next_float() * 2.0 - 1.0];
            let size = 0.15 + random.next_float() * 0.1;
            let len2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
            if len2 <= 0.010000001 || len2 >= 1.0 {
                continue;
            }
            let spin = (random.next_double() * std::f64::consts::TAU) as f32;
            let len = len2.sqrt();
            let n = p.map(|v| v / len);
            let side = if n[1].abs() > 0.99 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            let a = normalize(cross(side, n));
            let b = cross(n, a);
            let (s, c) = spin.sin_cos();
            let u = [0, 1, 2].map(|k| (a[k] * c + b[k] * s) * size);
            let v = [0, 1, 2].map(|k| (b[k] * c - a[k] * s) * size);
            out.push((n.map(|x| x * 100.0), u, v));
        }
        out
    })
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    a.map(|v| v / l)
}

/// The End's sky: a cube of the dark nebula texture around the player, each face repeating it 16 times, drawn at
/// 40/255 of its brightness.
fn end_sky(d: [f32; 3], assets: &Assets) -> [f32; 3] {
    const FACES: [fn([f32; 3]) -> [f32; 3]; 6] = [
        |d| d,
        |d| [d[0], d[2], -d[1]],
        |d| [d[0], -d[2], d[1]],
        |d| [d[0], -d[1], -d[2]],
        |d| [d[1], -d[0], d[2]],
        |d| [-d[1], d[0], d[2]],
    ];
    let Some(texture) = assets.end_sky.as_ref() else { return [0.0; 3] };
    for face in FACES {
        let p = face(d);
        if p[1] < 0.0 && -p[1] >= p[0].abs() && -p[1] >= p[2].abs() {
            let (u, v) = ((p[0] / -p[1] + 1.0) * 8.0, (p[2] / -p[1] + 1.0) * 8.0);
            let px = texture.sample(u, v, 0);
            return [0, 1, 2].map(|k| f32::from(px[k]) / 255.0 * (40.0 / 255.0));
        }
    }
    [0.0; 3]
}

/// Paints the sky into every pixel of `target`, in the game's order: sky disc, sunrise glow, sun, moon, stars,
/// dark disc.
pub fn paint(target: &mut Target, view: &View, air: &Atmosphere, assets: &Assets) {
    let (w, h) = (target.width, target.height);
    let direction = |target: &Target, x: usize, y: usize| {
        let r = target.ray(x as f32 + 0.5, y as f32 + 0.5);
        normalize([0, 1, 2].map(|k| view.right[k] * r[0] + view.up[k] * r[1] + view.forward[k] * r[2]))
    };
    for y in 0..h {
        for x in 0..w {
            let d = direction(target, x, y);
            target.color[y * w + x] = air.disc(d, 16.0, air.sky).unwrap_or(air.fog);
        }
    }
    match air.skybox {
        Skybox::Overworld => {}
        Skybox::None => {
            target.color.fill(air.fog);
            return;
        }
        Skybox::End => {
            for y in 0..h {
                for x in 0..w {
                    target.color[y * w + x] = end_sky(direction(target, x, y), assets);
                }
            }
            return;
        }
    }
    let to_view = |p: [f32; 3]| view.to_view(p);
    if air.glow[3] > 0.001 {
        let flip = if air.sun.sin() < 0.0 { -1.0 } else { 1.0 };
        let centre = [-100.0 * flip, 0.0, 0.0];
        let ring = |i: usize| {
            let a = i as f32 * TAU / 16.0;
            [-120.0 * a.cos() * flip, 40.0 * air.glow[3] * a.cos() * flip, 120.0 * a.sin()]
        };
        let color = [air.glow[0], air.glow[1], air.glow[2]];
        for i in 0..16 {
            let poly = [(to_view(centre), [air.glow[3], 0.0]), (to_view(ring(i)), [0.0, 0.0]), (to_view(ring(i + 1)), [0.0, 0.0])];
            target.paint(&poly, |dst, a| *dst = mix(*dst, color, a[0]));
        }
    }
    let bodies = [(air.sun, 30.0, assets.sun.as_ref(), false), (air.sun + PI, 20.0, assets.moon[air.phase].as_ref(), true)];
    for (angle, size, texture, turned) in bodies {
        let Some(texture) = texture else { continue };
        let corner = |x: f32, z: f32| {
            let uv = if turned { [(1.0 - x) * 0.5, (1.0 - z) * 0.5] } else { [(x + 1.0) * 0.5, (z + 1.0) * 0.5] };
            (to_view(pose(angle, [x * size, 100.0, z * size])), uv)
        };
        let poly = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
        let clear = air.clear;
        target.paint(&poly, |dst, uv| {
            let px = texture.sample(uv[0].clamp(0.0, 0.9999), uv[1].clamp(0.0, 0.9999), 0);
            let a = f32::from(px[3]) / 255.0 * clear;
            for k in 0..3 {
                dst[k] += f32::from(px[k]) / 255.0 * a;
            }
        });
    }
    if air.stars > 0.0 {
        let star_angle = air.sun;
        let add = air.stars * air.stars;
        for (centre, u, v) in stars() {
            let corner = |a: f32, b: f32| (to_view(pose(star_angle, [0, 1, 2].map(|k| centre[k] + u[k] * a + v[k] * b))), [0.0, 0.0]);
            let poly = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
            target.paint(&poly, |dst, _| {
                for c in dst.iter_mut() {
                    *c += add;
                }
            });
        }
    }
    if air.dark {
        for y in 0..h {
            for x in 0..w {
                let d = direction(target, x, y);
                if let Some(c) = air.disc(d, -4.0, [0.0; 3]) {
                    target.color[y * w + x] = c;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sun_follows_the_eased_day() {
        let deg = |t: f32| sun_angle(t).to_degrees();
        assert!(deg(6000.0).abs() < 0.01 || (deg(6000.0) - 360.0).abs() < 0.01);
        assert!((deg(0.0) - 282.37).abs() < 0.1, "{}", deg(0.0));
        assert!((deg(13000.0) - 93.52).abs() < 0.1, "{}", deg(13000.0));
        assert!((deg(18000.0) - 180.0).abs() < 0.1);
        let east = pose(sun_angle(0.0), [0.0, 100.0, 0.0]);
        assert!(east[0] > 90.0, "{east:?}");
    }

    #[test]
    fn fog_at_render_distance_two_matches_plains_noon() {
        let air = Atmosphere::new(6000, 0.0, 0.0, &Biome::default(), &Environment::default(), 2, [0.0, 0.0, 1.0], 80.0);
        let c = air.fog.map(|v| (v * 255.0).round() as i32);
        assert!((c[0] - 173).abs() <= 1 && (c[1] - 203).abs() <= 1 && c[2] == 255, "{c:?}");
    }

    #[test]
    fn the_glow_peaks_at_sunset() {
        let g = glow_at(12732.0);
        assert!((g[3] - 254.0 / 255.0).abs() < 0.01 && (g[0] - 218.0 / 255.0).abs() < 0.01);
        assert!(glow_at(6000.0)[3] == 0.0);
    }
}
