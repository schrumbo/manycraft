//! What a dimension type tells the picture about its light and sky: the sky box, the fixed sky light of the Nether and
//! the End, the ambient light, the fog range and how faces are shaded.

use serde_json::Value;

use super::color;

/// What the sky shows behind the world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Skybox {
    /// The day cycle: sky disc, sun, moon and stars.
    #[default]
    Overworld,
    End,
    /// Nothing: the fog colour fills the sky.
    None,
}

/// How the game shades faces by the way they point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cardinal {
    #[default]
    Default,
    /// Up and down faces share one brightness, as in a world without a sun.
    Nether,
}

/// A dimension type's visual and light attributes; what is not set stays as in the overworld.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Environment {
    pub skybox: Skybox,
    pub cardinal: Cardinal,
    /// The light every block gets whatever its sky and block light.
    pub ambient: [f32; 3],
    /// A sky light that never changes with the time of day: its colour, its strength and the level the game counts.
    pub sky_light: Option<[f32; 3]>,
    pub sky_factor: Option<f32>,
    pub sky_level: Option<f32>,
    pub fog: Option<u32>,
    pub sky: Option<u32>,
    /// Where fog starts and ends, in blocks, when the dimension sets it.
    pub fog_range: Option<[f32; 2]>,
}

impl Default for Environment {
    fn default() -> Self {
        Self { skybox: Skybox::Overworld, cardinal: Cardinal::Default, ambient: [10.0 / 255.0; 3], sky_light: None, sky_factor: None, sky_level: None, fog: None, sky: None, fog_range: None }
    }
}

fn number(v: Option<&Value>) -> Option<f32> {
    let v = match v? {
        Value::Object(o) => o.get("argument")?,
        v => v,
    };
    v.as_f64().map(|x| x as f32).filter(|x| x.is_finite())
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16 & 0xFF) as f32 / 255.0, (c >> 8 & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0]
}

impl Environment {
    /// Reads a `minecraft:dimension_type` registry entry.
    pub fn from_json(v: &Value) -> Self {
        let plain = Self::default();
        let attributes = v.get("attributes");
        let attribute = |key: &str| attributes.and_then(|a| a.get(format!("minecraft:{key}")));
        Self {
            skybox: match v.get("skybox").and_then(Value::as_str) {
                Some("none") => Skybox::None,
                Some("end") => Skybox::End,
                _ => Skybox::Overworld,
            },
            cardinal: match v.get("cardinal_light").and_then(Value::as_str) {
                Some("nether") => Cardinal::Nether,
                _ => Cardinal::Default,
            },
            ambient: color(attribute("visual/ambient_light_color")).map_or(plain.ambient, rgb),
            sky_light: color(attribute("visual/sky_light_color")).map(rgb),
            sky_factor: number(attribute("visual/sky_light_factor")).map(|f| f.clamp(0.0, 1.0)),
            sky_level: number(attribute("gameplay/sky_light_level")).map(|l| l.clamp(0.0, 15.0)),
            fog: color(attribute("visual/fog_color")),
            sky: color(attribute("visual/sky_color")),
            fog_range: number(attribute("visual/fog_start_distance")).zip(number(attribute("visual/fog_end_distance"))).filter(|(s, e)| *s >= 0.0 && e > s).map(|(s, e)| [s, e]),
        }
    }

    /// Whether the time of day and the weather reach this dimension: its sky light and its sky follow them.
    pub fn follows_the_day(&self) -> bool {
        self.skybox == Skybox::Overworld && self.sky_factor.is_none()
    }
}

#[cfg(test)]
pub(super) mod tests {
    use serde_json::json;

    use super::*;

    pub fn nether() -> Value {
        json!({
            "ambient_light": 0.1, "cardinal_light": "nether", "skybox": "none", "has_skylight": false,
            "attributes": {
                "minecraft:gameplay/sky_light_level": 4.0,
                "minecraft:visual/ambient_light_color": "#302821",
                "minecraft:visual/fog_end_distance": 96.0,
                "minecraft:visual/fog_start_distance": 10.0,
                "minecraft:visual/sky_light_color": "#7a7aff",
                "minecraft:visual/sky_light_factor": 0.0
            }
        })
    }

    pub fn end() -> Value {
        json!({
            "ambient_light": 0.25, "skybox": "end",
            "attributes": {
                "minecraft:visual/ambient_light_color": "#3f473f",
                "minecraft:visual/fog_color": "#181318",
                "minecraft:visual/sky_color": "#000000",
                "minecraft:visual/sky_light_color": "#ac60cd",
                "minecraft:visual/sky_light_factor": 0.0
            }
        })
    }

    #[test]
    fn the_nether_has_no_sky_a_fixed_ambient_and_its_own_face_shading() {
        let e = Environment::from_json(&nether());
        assert_eq!((e.skybox, e.cardinal, e.sky_factor, e.sky_level), (Skybox::None, Cardinal::Nether, Some(0.0), Some(4.0)));
        assert_eq!(e.ambient.map(|c| (c * 255.0).round() as u8), [0x30, 0x28, 0x21]);
        assert_eq!(e.fog_range, Some([10.0, 96.0]));
        assert!(!e.follows_the_day());
    }

    #[test]
    fn the_end_has_its_own_sky_box_and_a_fixed_light() {
        let e = Environment::from_json(&end());
        assert_eq!((e.skybox, e.cardinal, e.fog, e.sky), (Skybox::End, Cardinal::Default, Some(0x181318), Some(0)));
        assert_eq!(e.sky_light.unwrap().map(|c| (c * 255.0).round() as u8), [0xac, 0x60, 0xcd]);
    }

    #[test]
    fn a_type_without_attributes_is_the_overworld() {
        assert_eq!(Environment::from_json(&json!({"min_y": -64})), Environment::default());
        assert!(Environment::default().follows_the_day());
        assert_eq!(Environment::from_json(&json!({"attributes": {"minecraft:visual/fog_start_distance": 50.0, "minecraft:visual/fog_end_distance": 20.0}})).fog_range, None);
    }
}
