//! What a picture shows besides blocks: players, mobs, dropped items, item frames, armor stands and display entities,
//! as plain data the caller fills from what the server sent.

use std::sync::Arc;

use uuid::Uuid;

use crate::core::chat_text::Run;

/// The entities a picture shows and the account's own hand.
pub trait Entities {
    /// The entities to draw, positions in world coordinates.
    fn entities(&self) -> &[Entity];
    /// The account's own first-person hand, when the picture is taken through its eyes.
    fn hand(&self) -> Option<&Hand>;
    /// The picture of the map with this id, when the server sent it.
    fn map(&self, _id: i32) -> Option<&MapPicture> {
        None
    }
}

/// A map's picture as the server sent it.
#[derive(Clone, Debug, PartialEq)]
pub struct MapPicture {
    /// Packed colours (base colour × 4 + shade, 0 transparent), rows from the north-west corner.
    pub colors: Arc<[u8; 128 * 128]>,
}

/// No entities and no hand.
pub struct Nobody;

impl Entities for Nobody {
    fn entities(&self) -> &[Entity] {
        &[]
    }

    fn hand(&self) -> Option<&Hand> {
        None
    }
}

/// Styled text: its runs in order; a `\n` starts a new line.
pub type Text = Vec<Run>;

/// A player's skin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Skin {
    /// The skin texture URL from the profile's `textures` property; `None` draws the default skin for `uuid`.
    pub url: Option<String>,
    pub uuid: Uuid,
    /// The slim (three pixel) arms.
    pub slim: bool,
    /// Which outer layers show, as the skin-parts byte (cape 0x01, jacket 0x02, sleeves 0x04 and 0x08, pants 0x10 and
    /// 0x20, hat 0x40).
    pub parts: u8,
}

/// An item stack as a picture shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The item's registry name without namespace (`diamond_sword`).
    pub id: &'static str,
    pub count: u8,
    /// Drawn with the enchantment glint.
    pub glint: bool,
    /// The dye colour of leather armor and other dyeable items, RGB.
    pub dye: Option<u32>,
    /// An armor trim: pattern and material names.
    pub trim: Option<(String, String)>,
    /// The skin of a player head.
    pub profile: Option<Skin>,
    /// The id of a filled map's picture.
    pub map: Option<i32>,
}

/// Indices into [`Entity::equipment`].
pub mod slot {
    pub const MAIN_HAND: usize = 0;
    pub const OFF_HAND: usize = 1;
    pub const FEET: usize = 2;
    pub const LEGS: usize = 3;
    pub const CHEST: usize = 4;
    pub const HEAD: usize = 5;
}

/// How an entity holds its body, as the entity data sends it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pose {
    #[default]
    Standing,
    Crouching,
    Sleeping,
    Swimming,
    FallFlying,
    Sitting,
    Other,
}

/// How a display entity is placed and lit, from its entity data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub translation: [f32; 3],
    /// Quaternions `[x, y, z, w]`.
    pub left_rotation: [f32; 4],
    pub scale: [f32; 3],
    pub right_rotation: [f32; 4],
    /// 0 fixed, 1 vertical, 2 horizontal, 3 center.
    pub billboard: u8,
    /// Sky and block light it is drawn with instead of the light where it stands.
    pub brightness: Option<(u8, u8)>,
    pub view_range: f32,
    pub shadow_radius: f32,
    pub shadow_strength: f32,
    /// ARGB, when it glows.
    pub glow_color: Option<u32>,
    pub width: f32,
    pub height: f32,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            left_rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            right_rotation: [0.0, 0.0, 0.0, 1.0],
            billboard: 0,
            brightness: None,
            view_range: 1.0,
            shadow_radius: 0.0,
            shadow_strength: 1.0,
            glow_color: None,
            width: 0.0,
            height: 0.0,
        }
    }
}

/// What a display entity shows.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayKind {
    Block { state: u32 },
    /// `transform` is the item display context id (0 none … 8 fixed).
    Item { item: Option<Item>, transform: u8 },
    /// `flags`: shadow 0x01, see-through 0x02, default background 0x04, align left 0x08, right 0x10.
    Text { text: Text, line_width: i32, background: u32, opacity: u8, flags: u8 },
}

/// What decides an entity's looks beyond its type.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Data {
    #[default]
    None,
    Player { skin: Skin, main_left: bool },
    /// A dropped item.
    Item { item: Item },
    Frame { item: Option<Item>, rotation: u8, facing: u8, glow: bool, invisible: bool },
    /// `poses`: head, body, left arm, right arm, left leg, right leg; degrees `[x, y, z]`.
    ArmorStand { small: bool, arms: bool, no_plate: bool, marker: bool, poses: [[f32; 3]; 6] },
    Display { common: Display, kind: DisplayKind },
    /// `color`: the dye colour id (0 white … 15 black).
    Sheep { color: u8, sheared: bool },
    /// `kind` and `profession` are registry names without namespace (`plains`, `farmer`).
    Villager { kind: String, profession: String, level: u8 },
    /// The horse variant as the entity data sends it: colour in the low byte, markings above.
    Horse { variant: u32 },
    Wolf { tame: bool, collar: u8, variant: String },
    Cat { variant: String, collar: u8 },
    Slime { size: u8 },
    Creeper { charged: bool },
    /// The registry variant of any other type that has one (pig, cow, chicken, frog …).
    Variant(String),
}

/// One entity in the picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub id: i32,
    /// The entity type's registry name without namespace (`villager`).
    pub kind: &'static str,
    /// Its feet, in world coordinates.
    pub pos: [f64; 3],
    /// Yaw (also the body's) and pitch in Minecraft degrees, and the head's yaw.
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub baby: bool,
    pub pose: Pose,
    /// The entity flags byte: on fire 0x01, invisible 0x20, glowing 0x40.
    pub flags: u8,
    /// The name shown above it: a visible custom name or a player's name.
    pub name: Option<Text>,
    /// See [`slot`].
    pub equipment: [Option<Item>; 6],
    pub data: Data,
}

/// The account's own first-person hand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hand {
    pub main: Option<Item>,
    pub left_handed: bool,
    pub skin: Skin,
}