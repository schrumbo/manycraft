//! A small software rasterizer: perspective-correct textured quads with a depth buffer, alpha cutout, back-to-front
//! translucency and per-pixel fog.

use super::assets::{Texture, Transparency};

const NEAR: f32 = 0.05;

/// One corner of a quad in view space (x right, y up, z forward, in blocks).
#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex {
    pub p: [f32; 3],
    pub uv: [f32; 2],
    /// Colour the texel is multiplied with: shade, ambient occlusion, light and tint.
    pub c: [f32; 3],
}

/// Distance fog: the colour terrain fades to between `start` and `end` blocks (measured as the larger of the
/// horizontal and the vertical distance), and over `sphere` (straight-line distance) for weather.
#[derive(Clone, Copy, Debug)]
pub struct Fog {
    pub color: [f32; 3],
    pub start: f32,
    pub end: f32,
    pub sphere: [f32; 2],
}

#[inline]
fn ramp(distance: f32, start: f32, end: f32) -> f32 {
    if distance <= start {
        0.0
    } else if distance >= end {
        1.0
    } else {
        (distance - start) / (end - start)
    }
}

/// The picture being drawn: linear colour and depth per pixel.
pub struct Target {
    pub width: usize,
    pub height: usize,
    pub color: Vec<[f32; 3]>,
    depth: Vec<f32>,
    focal: f32,
    pub fog: Fog,
    /// The camera's up axis in view space, so fog can measure height apart from distance.
    pub vertical: [f32; 3],
}

struct Deferred<'a> {
    far: f32,
    v: [Vertex; 4],
    texture: &'a Texture,
}

/// Translucent quads, drawn after the opaque ones from far to near.
#[derive(Default)]
pub struct Later<'a> {
    quads: Vec<Deferred<'a>>,
}

impl<'a> Later<'a> {
    pub fn push(&mut self, v: [Vertex; 4], texture: &'a Texture) {
        let far = v.iter().map(|x| x.p[0] * x.p[0] + x.p[1] * x.p[1] + x.p[2] * x.p[2]).fold(0.0, f32::max);
        self.quads.push(Deferred { far, v, texture });
    }

    pub fn draw(mut self, target: &mut Target) {
        self.quads.sort_by(|a, b| b.far.total_cmp(&a.far));
        for q in &self.quads {
            target.quad(&q.v, q.texture, Transparency::Translucent);
        }
    }
}

impl Target {
    /// A picture of `width` × `height` whose vertical field of view is `fov` degrees, filled by `background`.
    pub fn new(width: usize, height: usize, fov: f32, fog: Fog, vertical: [f32; 3]) -> Self {
        let focal = height as f32 * 0.5 / (fov.to_radians() * 0.5).tan();
        Self { width, height, color: vec![[0.0; 3]; width * height], depth: vec![f32::INFINITY; width * height], focal, fog, vertical }
    }

    /// The view direction through pixel `(x, y)`, not normalised.
    pub fn ray(&self, x: f32, y: f32) -> [f32; 3] {
        [(x - self.width as f32 * 0.5) / self.focal, (self.height as f32 * 0.5 - y) / self.focal, 1.0]
    }

    /// Whether a quad with these view-space corners can show at all.
    pub fn visible(&self, p: &[[f32; 3]]) -> bool {
        let (hw, hh) = (self.width as f32 * 0.5 / self.focal, self.height as f32 * 0.5 / self.focal);
        let out = |f: &dyn Fn(&[f32; 3]) -> bool| p.iter().all(f);
        !(out(&|v| v[2] < NEAR) || out(&|v| v[0] > v[2] * hw) || out(&|v| v[0] < -v[2] * hw) || out(&|v| v[1] > v[2] * hh) || out(&|v| v[1] < -v[2] * hh))
    }

    /// Draws a quad; opaque and cutout ones write depth, translucent ones blend.
    pub fn quad(&mut self, v: &[Vertex; 4], texture: &Texture, mode: Transparency) {
        let mut poly = [Vertex::default(); 8];
        let mut n = 0;
        for i in 0..4 {
            let (a, b) = (v[i], v[(i + 1) % 4]);
            if a.p[2] >= NEAR {
                poly[n] = a;
                n += 1;
            }
            if (a.p[2] >= NEAR) != (b.p[2] >= NEAR) {
                let t = (NEAR - a.p[2]) / (b.p[2] - a.p[2]);
                poly[n] = Vertex {
                    p: [0, 1, 2].map(|k| a.p[k] + (b.p[k] - a.p[k]) * t),
                    uv: [0, 1].map(|k| a.uv[k] + (b.uv[k] - a.uv[k]) * t),
                    c: [0, 1, 2].map(|k| a.c[k] + (b.c[k] - a.c[k]) * t),
                };
                n += 1;
            }
        }
        if n < 3 {
            return;
        }
        let level = self.level(&v[..], texture);
        for i in 1..n - 1 {
            self.triangle(&poly[0], &poly[i], &poly[i + 1], texture, mode, level);
        }
    }

    /// Darkens the picture towards its edges like the game's vignette: each pixel by `texture` (stretched over the
    /// whole picture) times `strength`.
    pub fn vignette(&mut self, texture: &Texture, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let (w, h) = (self.width as f32, self.height as f32);
        for (i, c) in self.color.iter_mut().enumerate() {
            let (x, y) = ((i % self.width) as f32 + 0.5, (i / self.width) as f32 + 0.5);
            let t = texture.smooth(x / w, y / h);
            for k in 0..3 {
                c[k] *= 1.0 - t[k] * strength;
            }
        }
    }

    /// Fills a convex polygon of view-space corners without depth, handing each covered pixel and the
    /// perspective-correct attributes there to `shade`; for the sky, which lies behind everything.
    pub fn paint(&mut self, poly: &[([f32; 3], [f32; 2])], mut shade: impl FnMut(&mut [f32; 3], [f32; 2])) {
        let mut clipped: Vec<([f32; 3], [f32; 2])> = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            if a.0[2] >= NEAR {
                clipped.push(a);
            }
            if (a.0[2] >= NEAR) != (b.0[2] >= NEAR) {
                let t = (NEAR - a.0[2]) / (b.0[2] - a.0[2]);
                clipped.push(([0, 1, 2].map(|k| a.0[k] + (b.0[k] - a.0[k]) * t), [0, 1].map(|k| a.1[k] + (b.1[k] - a.1[k]) * t)));
            }
        }
        if clipped.len() < 3 {
            return;
        }
        let (cx, cy) = (self.width as f32 * 0.5, self.height as f32 * 0.5);
        let project = |(p, a): ([f32; 3], [f32; 2])| {
            let w = 1.0 / p[2];
            [cx + p[0] * w * self.focal, cy - p[1] * w * self.focal, w, a[0] * w, a[1] * w]
        };
        let points: Vec<[f32; 5]> = clipped.into_iter().map(project).collect();
        for i in 1..points.len() - 1 {
            let (pa, pb, pc) = (points[0], points[i], points[i + 1]);
            let area = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pc[0] - pa[0]) * (pb[1] - pa[1]);
            if area.abs() < 1e-9 {
                continue;
            }
            let min_x = pa[0].min(pb[0]).min(pc[0]).floor().max(0.0) as usize;
            let max_x = (pa[0].max(pb[0]).max(pc[0]).ceil() as isize).min(self.width as isize - 1);
            let min_y = pa[1].min(pb[1]).min(pc[1]).floor().max(0.0) as usize;
            let max_y = (pa[1].max(pb[1]).max(pc[1]).ceil() as isize).min(self.height as isize - 1);
            if max_x < min_x as isize || max_y < min_y as isize {
                continue;
            }
            let inv = 1.0 / area;
            for y in min_y..=max_y as usize {
                let py = y as f32 + 0.5;
                for x in min_x..=max_x as usize {
                    let px = x as f32 + 0.5;
                    let w0 = ((pb[0] - px) * (pc[1] - py) - (pc[0] - px) * (pb[1] - py)) * inv;
                    let w1 = ((pc[0] - px) * (pa[1] - py) - (pa[0] - px) * (pc[1] - py)) * inv;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let at = |k: usize| pa[k] * w0 + pb[k] * w1 + pc[k] * w2;
                    let z = 1.0 / at(2);
                    shade(&mut self.color[y * self.width + x], [at(3) * z, at(4) * z]);
                }
            }
        }
    }

    fn level(&self, v: &[Vertex], texture: &Texture) -> usize {
        let screen = |p: [f32; 3]| [p[0] / p[2].max(NEAR) * self.focal, p[1] / p[2].max(NEAR) * self.focal];
        let area = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs();
        let (s0, s1, s2, s3) = (screen(v[0].p), screen(v[1].p), screen(v[2].p), screen(v[3].p));
        let pixels = area(s0, s1, s2) + area(s0, s2, s3);
        let side = texture.size as f32;
        let texels = (area(v[0].uv, v[1].uv, v[2].uv) + area(v[0].uv, v[2].uv, v[3].uv)) * side * side;
        if pixels <= 0.0 || texels <= 0.0 {
            return 0;
        }
        let lod = 0.5 * (texels / pixels).log2();
        if lod <= 0.5 { 0 } else { (lod + 0.5) as usize }
    }

    fn triangle(&mut self, a: &Vertex, b: &Vertex, c: &Vertex, texture: &Texture, mode: Transparency, level: usize) {
        let (cx, cy) = (self.width as f32 * 0.5, self.height as f32 * 0.5);
        let project = |v: &Vertex| {
            let w = 1.0 / v.p[2];
            [cx + v.p[0] * w * self.focal, cy - v.p[1] * w * self.focal, w]
        };
        let (pa, pb, pc) = (project(a), project(b), project(c));
        let area = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pc[0] - pa[0]) * (pb[1] - pa[1]);
        if area.abs() < 1e-9 {
            return;
        }
        let min_x = pa[0].min(pb[0]).min(pc[0]).floor().max(0.0) as usize;
        let max_x = (pa[0].max(pb[0]).max(pc[0]).ceil() as isize).min(self.width as isize - 1);
        let min_y = pa[1].min(pb[1]).min(pc[1]).floor().max(0.0) as usize;
        let max_y = (pa[1].max(pb[1]).max(pc[1]).ceil() as isize).min(self.height as isize - 1);
        if max_x < min_x as isize || max_y < min_y as isize {
            return;
        }
        let inv = 1.0 / area;
        let attrs = |v: &Vertex, w: f32| [v.uv[0] * w, v.uv[1] * w, v.c[0] * w, v.c[1] * w, v.c[2] * w, v.p[0] * w, v.p[1] * w, w];
        let (aa, ab, ac) = (attrs(a, pa[2]), attrs(b, pb[2]), attrs(c, pc[2]));
        let vertical = self.vertical;
        let fog = self.fog;
        for y in min_y..=max_y as usize {
            let py = y as f32 + 0.5;
            let row = y * self.width;
            for x in min_x..=max_x as usize {
                let px = x as f32 + 0.5;
                let w0 = ((pb[0] - px) * (pc[1] - py) - (pc[0] - px) * (pb[1] - py)) * inv;
                let w1 = ((pc[0] - px) * (pa[1] - py) - (pa[0] - px) * (pc[1] - py)) * inv;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let at = |k: usize| aa[k] * w0 + ab[k] * w1 + ac[k] * w2;
                let iw = at(7);
                let z = 1.0 / iw;
                let i = row + x;
                if z >= self.depth[i] {
                    continue;
                }
                let texel = texture.sample(at(0) * z, at(1) * z, level);
                let alpha = f32::from(texel[3]) / 255.0;
                if mode == Transparency::Cutout && alpha < 0.5 {
                    continue;
                }
                let tex = [0, 1, 2].map(|k| f32::from(texel[k]) / 255.0);
                let mut rgb = [tex[0] * at(2) * z, tex[1] * at(3) * z, tex[2] * at(4) * z];
                let p = [at(5) * z, at(6) * z, z];
                let up = p[0] * vertical[0] + p[1] * vertical[1] + p[2] * vertical[2];
                let flat = ((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]) - up * up).max(0.0).sqrt();
                let f = ramp(flat.max(up.abs()), fog.start, fog.end).max(ramp((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt(), fog.sphere[0], fog.sphere[1]));
                for k in 0..3 {
                    rgb[k] += (fog.color[k] - rgb[k]) * f;
                }
                if mode == Transparency::Translucent {
                    let dst = &mut self.color[i];
                    for k in 0..3 {
                        dst[k] += (rgb[k] - dst[k]) * alpha;
                    }
                } else {
                    self.color[i] = rgb;
                    self.depth[i] = z;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white() -> Texture {
        Texture { size: 1, levels: vec![vec![[255, 255, 255, 255]]] }
    }

    fn square(z: f32, c: [f32; 3]) -> [Vertex; 4] {
        [[-1.0, -1.0], [-1.0, 1.0], [1.0, 1.0], [1.0, -1.0]].map(|[x, y]| Vertex { p: [x, y, z], uv: [0.0, 0.0], c })
    }

    fn target() -> Target {
        Target::new(32, 32, 90.0, Fog { color: [0.0; 3], start: 100.0, end: 200.0, sphere: [1000.0, 2000.0] }, [0.0, 1.0, 0.0])
    }

    #[test]
    fn nearer_quads_win_and_translucent_ones_blend_over() {
        let mut t = target();
        let tex = white();
        t.quad(&square(5.0, [1.0, 0.0, 0.0]), &tex, Transparency::Opaque);
        t.quad(&square(10.0, [0.0, 1.0, 0.0]), &tex, Transparency::Opaque);
        assert_eq!(t.color[16 * 32 + 16], [1.0, 0.0, 0.0]);
        let glass = Texture { size: 1, levels: vec![vec![[255, 255, 255, 128]]] };
        let mut later = Later::default();
        later.push(square(2.0, [0.0, 0.0, 1.0]), &glass);
        later.draw(&mut t);
        let c = t.color[16 * 32 + 16];
        assert!((c[0] - 0.498).abs() < 0.01 && (c[2] - 0.502).abs() < 0.01, "{c:?}");
    }

    #[test]
    fn a_quad_crossing_the_camera_plane_is_clipped_not_lost() {
        let mut t = target();
        let tex = white();
        let floor = [[-4.0, -1.0, -2.0], [-4.0, -1.0, 8.0], [4.0, -1.0, 8.0], [4.0, -1.0, -2.0]].map(|p| Vertex { p, uv: [0.0, 0.0], c: [1.0; 3] });
        t.quad(&floor, &tex, Transparency::Opaque);
        assert!(t.color[31 * 32 + 16].iter().all(|c| (c - 1.0).abs() < 1e-4), "the floor reaches the bottom row: {:?}", t.color[31 * 32 + 16]);
        assert_eq!(t.color[0], [0.0; 3]);
    }

    #[test]
    fn fog_fades_far_pixels_to_its_colour() {
        let mut t = Target::new(8, 8, 90.0, Fog { color: [0.0, 0.0, 1.0], start: 1.0, end: 4.0, sphere: [0.0, 1024.0] }, [0.0, 1.0, 0.0]);
        t.quad(&square(6.0, [1.0, 0.0, 0.0]), &white(), Transparency::Opaque);
        assert_eq!(t.color[4 * 8 + 4], [0.0, 0.0, 1.0]);
    }
}
