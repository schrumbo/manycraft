//! Entity models as the game builds them: every model layer's part tree with its rest pose and baked cube polygons
//! (generated into `models.bin` by `.claude/bin/render-models`), and drawing a posed tree through the rasterizer
//! with the game's two-light entity shading.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{Context, Result, bail};

use super::assets::{Texture, Transparency};
use super::raster::{Target, Vertex};
use super::scene::View;

const MODELS: &[u8] = include_bytes!("models.bin");
const LIGHT_0: [f32; 3] = [0.16169041, 0.80845207, -0.56591645];
const LIGHT_1: [f32; 3] = [-0.16169041, 0.80845207, 0.56591645];

/// One corner of a model polygon: position in pixels (y down) and texture coordinates as a fraction of the sheet.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Corner {
    pub p: [f32; 3],
    pub uv: [f32; 2],
}

/// Where a part sits relative to its parent: pivot in pixels, rotation in radians (applied z, y, x) and scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pivot: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

/// A model part and its children.
#[derive(Debug)]
pub struct Part {
    pub name: Box<str>,
    pub pose: Pose,
    pub visible: bool,
    pub polys: Box<[[Corner; 4]]>,
    pub children: Box<[Part]>,
}

impl Part {
    /// The child named `name`, searched depth first.
    pub fn find(&self, name: &str) -> Option<&Part> {
        self.children.iter().find_map(|c| if &*c.name == name { Some(c) } else { c.find(name) })
    }
}

/// Every model layer of the game by its id (`minecraft:player#main`).
pub struct Models {
    layers: HashMap<Box<str>, Part>,
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let Some((head, rest)) = self.bytes.split_first_chunk::<N>() else { bail!("model data ends early") };
        self.bytes = rest;
        Ok(*head)
    }

    fn name(&mut self) -> Result<Box<str>> {
        let [len] = self.take::<1>()?;
        let len = usize::from(len);
        if self.bytes.len() < len {
            bail!("model data ends early");
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(std::str::from_utf8(head)?.into())
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take()?))
    }

    fn i16(&mut self) -> Result<f32> {
        Ok(f32::from(i16::from_le_bytes(self.take()?)))
    }

    fn part(&mut self, depth: usize) -> Result<Part> {
        if depth > 32 {
            bail!("model parts nest too deep");
        }
        let name = self.name()?;
        let mut v = [0.0; 9];
        for x in &mut v {
            *x = self.f32()?;
        }
        let pose = Pose { pivot: [v[0], v[1], v[2]], rotation: [v[3], v[4], v[5]], scale: [v[6], v[7], v[8]] };
        let [visible] = self.take::<1>()?;
        let count = u16::from_le_bytes(self.take()?);
        let mut polys = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let mut quad = [Corner::default(); 4];
            for c in &mut quad {
                let p = [self.i16()? / 64.0, self.i16()? / 64.0, self.i16()? / 64.0];
                c.p = p;
                c.uv = [self.i16()? / 16384.0, self.i16()? / 16384.0];
            }
            polys.push(quad);
        }
        let [children] = self.take::<1>()?;
        let children = (0..children).map(|_| self.part(depth + 1)).collect::<Result<Vec<_>>>()?;
        Ok(Part { name, pose, visible: visible != 0, polys: polys.into(), children: children.into() })
    }
}

impl Models {
    /// Unpacks the model layers built into the app.
    pub fn load() -> Result<Self> {
        let mut bytes = Vec::new();
        flate2::read::ZlibDecoder::new(MODELS).take(16 << 20).read_to_end(&mut bytes).context("model data")?;
        let mut r = Reader { bytes: &bytes };
        let count = u16::from_le_bytes(r.take()?);
        let mut layers = HashMap::with_capacity(usize::from(count));
        for _ in 0..count {
            let name = r.name()?;
            layers.insert(name, r.part(0)?);
        }
        Ok(Self { layers })
    }

    /// The layer `id` (`minecraft:cow#main`).
    pub fn layer(&self, id: &str) -> Option<&Part> {
        self.layers.get(id)
    }
}

/// An affine transform: `m` (row-major) times a point, plus `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [[f32; 3]; 3],
    pub t: [f32; 3],
}

impl Affine {
    pub const IDENTITY: Self = Self { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };

    /// Moves the origin to `p`.
    pub fn at(p: [f32; 3]) -> Self {
        Self { t: p, ..Self::IDENTITY }
    }

    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + self.t[r])
    }

    pub fn dir(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2])
    }

    fn mul_m(&self, b: [[f32; 3]; 3]) -> Self {
        let a = &self.m;
        Self { m: [0, 1, 2].map(|r| [0, 1, 2].map(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c])), t: self.t }
    }

    /// This transform after `other`: points go through `other` first.
    pub fn then(&self, other: &Affine) -> Self {
        let mut out = self.mul_m(other.m);
        out.t = self.apply(other.t);
        out
    }

    pub fn translate(&self, v: [f32; 3]) -> Self {
        Self { m: self.m, t: self.apply(v) }
    }

    pub fn scale(&self, s: [f32; 3]) -> Self {
        let mut m = self.m;
        for row in &mut m {
            for c in 0..3 {
                row[c] *= s[c];
            }
        }
        Self { m, t: self.t }
    }

    pub fn rotate_x(&self, a: f32) -> Self {
        let (s, c) = a.sin_cos();
        self.mul_m([[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]])
    }

    pub fn rotate_y(&self, a: f32) -> Self {
        let (s, c) = a.sin_cos();
        self.mul_m([[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]])
    }

    pub fn rotate_z(&self, a: f32) -> Self {
        let (s, c) = a.sin_cos();
        self.mul_m([[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]])
    }

    /// Rotates by the unit quaternion `[x, y, z, w]`.
    pub fn quat(&self, q: [f32; 4]) -> Self {
        let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        if !n.is_finite() || n < 1e-6 {
            return *self;
        }
        let [x, y, z, w] = q.map(|v| v / n);
        self.mul_m([
            [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
            [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
            [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
        ])
    }

    /// Steps into a model part posed by `pose`, as the game does before drawing it.
    pub fn part(&self, pose: &Pose) -> Self {
        let mut out = self.translate(pose.pivot.map(|v| v / 16.0));
        let [x, y, z] = pose.rotation;
        if z != 0.0 {
            out = out.rotate_z(z);
        }
        if y != 0.0 {
            out = out.rotate_y(y);
        }
        if x != 0.0 {
            out = out.rotate_x(x);
        }
        if pose.scale != [1.0; 3] {
            out = out.scale(pose.scale);
        }
        out
    }
}

/// A texture sheet for models: a square texture and the factor that maps the model's texture coordinates onto it
/// (sheets wider than tall are padded to a square).
pub struct Sheet {
    pub texture: Texture,
    pub scale: [f32; 2],
}

impl Sheet {
    /// A sheet from RGBA rows of `width` × `height`, without mip levels like the game's entity textures.
    pub fn new(width: u32, height: u32, pixels: &[[u8; 4]]) -> Option<Self> {
        if width == 0 || height == 0 || pixels.len() != (width * height) as usize {
            return None;
        }
        let side = width.max(height);
        let mut square = vec![[0u8; 4]; (side * side) as usize];
        for y in 0..height {
            let row = (y * width) as usize;
            square[(y * side) as usize..(y * side + width) as usize].copy_from_slice(&pixels[row..row + width as usize]);
        }
        Some(Self { texture: Texture { size: side, levels: vec![square] }, scale: [width as f32 / side as f32, height as f32 / side as f32] })
    }

    /// A sheet from PNG bytes.
    pub fn png(bytes: &[u8]) -> Option<Self> {
        let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?.into_rgba8();
        let (w, h) = image.dimensions();
        if w > 1024 || h > 1024 {
            return None;
        }
        let pixels: Vec<[u8; 4]> = image.pixels().map(|p| p.0).collect();
        Self::new(w, h, &pixels)
    }
}

/// How one draw lights and colours a model.
#[derive(Clone, Copy, Debug)]
pub struct Paint {
    /// The lightmap colour where the entity stands.
    pub light: [f32; 3],
    /// Multiplied with every texel (dye, wool colour); white for none.
    pub tint: [f32; 3],
    pub mode: Transparency,
}

/// Draws model parts into a picture.
pub struct Painter<'a> {
    pub target: &'a mut Target,
    pub view: &'a View,
}

/// Brightness of a face with world-space normal `n` under the game's two entity lights.
pub fn diffuse(n: [f32; 3]) -> f32 {
    let d = |l: [f32; 3]| (l[0] * n[0] + l[1] * n[1] + l[2] * n[2]).max(0.0);
    ((d(LIGHT_0) + d(LIGHT_1)) * 0.6 + 0.4).min(1.0)
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 { v.map(|c| c / l) } else { [0.0, 1.0, 0.0] }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

impl Painter<'_> {
    /// Draws `part` and its children with `at` mapping model space (blocks) to world space relative to the camera.
    /// `pose` may change each part's pose by name and return false to hide it.
    pub fn part(&mut self, part: &Part, at: &Affine, sheet: &Sheet, paint: &Paint, pose: &mut dyn FnMut(&str, &mut Pose) -> bool) {
        let mut own = part.pose;
        if !part.visible || !pose(&part.name, &mut own) {
            return;
        }
        let at = at.part(&own);
        for poly in &*part.polys {
            self.poly(poly, &at, sheet, paint);
        }
        for child in &*part.children {
            self.part(child, &at, sheet, paint, pose);
        }
    }

    /// Draws a quad given in world space relative to the camera, with texture coordinates on `texture`.
    /// `shade` replaces the entity lighting with a fixed factor, as blocks get from the side they face.
    pub fn face(&mut self, world: [[f32; 3]; 4], uv: [[f32; 2]; 4], texture: &Texture, paint: &Paint, shade: Option<f32>) {
        let view = world.map(|p| self.view.to_view(p));
        if !self.target.visible(&view) {
            return;
        }
        let mut n = cross(sub(world[1], world[0]), sub(world[2], world[0]));
        if n.iter().all(|c| c.abs() < 1e-12) {
            n = cross(sub(world[2], world[1]), sub(world[3], world[1]));
        }
        let f = shade.unwrap_or_else(|| diffuse(normalize(n)));
        let c = [0, 1, 2].map(|k| paint.light[k] * paint.tint[k] * f);
        let v = [0, 1, 2, 3].map(|i| Vertex { p: view[i], uv: uv[i], c });
        self.target.quad(&v, texture, paint.mode);
    }

    /// Draws one model polygon.
    pub fn poly(&mut self, poly: &[Corner; 4], at: &Affine, sheet: &Sheet, paint: &Paint) {
        let world = poly.map(|c| at.apply(c.p.map(|v| v / 16.0)));
        let view = world.map(|p| self.view.to_view(p));
        if !self.target.visible(&view) {
            return;
        }
        let n = normalize(cross(sub(world[1], world[0]), sub(world[2], world[0])));
        let n = if n == [0.0, 1.0, 0.0] && world[1] == world[0] { normalize(cross(sub(world[2], world[1]), sub(world[3], world[1]))) } else { n };
        let f = diffuse(n);
        let c = [0, 1, 2].map(|k| paint.light[k] * paint.tint[k] * f);
        let v = [0, 1, 2, 3].map(|i| Vertex { p: view[i], uv: [poly[i].uv[0] * sheet.scale[0], poly[i].uv[1] * sheet.scale[1]], c });
        self.target.quad(&v, &sheet.texture, paint.mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layer_unpacks_and_the_player_has_its_limbs() {
        let models = Models::load().unwrap();
        assert!(models.layers.len() > 300);
        let player = models.layer("minecraft:player#main").unwrap();
        for limb in ["head", "hat", "body", "jacket", "right_arm", "left_arm", "right_leg", "left_leg"] {
            assert!(player.find(limb).is_some(), "{limb}");
        }
        let head = player.find("head").unwrap();
        assert_eq!(head.polys.len(), 6);
        assert!(head.polys.iter().flatten().any(|c| c.p == [-4.0, -8.0, -4.0]));
    }

    #[test]
    fn the_top_of_a_cube_faces_up_under_the_entity_flip() {
        let models = Models::load().unwrap();
        let head = models.layer("minecraft:player#main").unwrap().find("head").unwrap();
        let flip = Affine::IDENTITY.scale([-1.0, -1.0, 1.0]);
        let top = head.polys.iter().find(|q| q.iter().all(|c| c.p[1] == -8.0)).unwrap();
        let w = top.map(|c| flip.apply(c.p));
        let n = normalize(cross(sub(w[1], w[0]), sub(w[2], w[0])));
        assert!(n[1] > 0.99, "{n:?}");
        assert!((diffuse([0.0, 1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((diffuse([0.0, -1.0, 0.0]) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn transforms_compose_like_a_pose_stack() {
        let a = Affine::at([1.0, 2.0, 3.0]).rotate_y(std::f32::consts::FRAC_PI_2);
        let p = a.apply([1.0, 0.0, 0.0]);
        assert!((p[0] - 1.0).abs() < 1e-6 && (p[2] - 2.0).abs() < 1e-6, "{p:?}");
        let q = Affine::IDENTITY.quat([0.0, (std::f32::consts::FRAC_PI_4).sin(), 0.0, (std::f32::consts::FRAC_PI_4).cos()]);
        let r = Affine::IDENTITY.rotate_y(std::f32::consts::FRAC_PI_2);
        for (x, y) in q.m.iter().flatten().zip(r.m.iter().flatten()) {
            assert!((x - y).abs() < 1e-6);
        }
    }
}
