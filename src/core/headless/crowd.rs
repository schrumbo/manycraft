//! The entities near an account as the server describes them: spawned, moved, turned, dressed and described by entity
//! data, kept compact per entity and turned into the renderer's entities only when a picture is taken.

use std::collections::HashMap;

use anyhow::{Result, bail};
use base64::Engine;
use serde_json::Value;
use uuid::Uuid;

use super::item::{self, Stack};
use super::render::{self, Data, Display, DisplayKind, Entity, Hand, Item, Pose, Skin, Span, Text};
use super::text::{self, Line};
use super::v777::entity_data as d;
use super::v777::{ENTITY_TYPES, ITEMS, VILLAGER_PROFESSIONS, VILLAGER_TYPES, serializer as s};
use super::wire::In;

const MAX_ENTITIES: usize = 512;
const MAX_PLAYERS: usize = 1024;
const MAX_VARIANTS: usize = 1024;
const SKINS: &str = "http://textures.minecraft.net/texture/";
const AGEABLE: &[&str] = &[
    "armadillo", "axolotl", "bee", "camel", "camel_husk", "cat", "chicken", "cow", "dolphin", "donkey", "fox", "frog", "goat", "happy_ghast", "hoglin", "horse", "llama", "mooshroom", "mule", "ocelot", "panda", "parrot", "pig", "polar_bear", "rabbit", "sheep", "skeleton_horse", "sniffer", "strider", "trader_llama", "turtle", "villager", "wandering_trader", "wolf", "zombie_horse", "zoglin", "zombie", "husk", "drowned", "zombie_villager",
];
const VARIANTS: &[&str] = &["cat_variant", "wolf_variant", "pig_variant", "cow_variant", "chicken_variant", "frog_variant"];

/// The entities near one account, the profiles of the players it was told about and the names of the synchronized
/// variant registries.
#[derive(Default)]
pub struct Crowd {
    near: HashMap<i32, Mob>,
    players: HashMap<Uuid, Profile>,
    variants: HashMap<&'static str, Box<[String]>>,
}

#[derive(Clone, Debug, Default)]
struct Mob {
    kind: u16,
    uuid: Uuid,
    pos: [f64; 3],
    yaw: f32,
    pitch: f32,
    head: f32,
    flags: u8,
    pose: u8,
    baby: bool,
    name_shown: bool,
    name: Option<Box<Line>>,
    equipment: Option<Box<[Option<Item>; 6]>>,
    data: Option<Box<Data>>,
}

#[derive(Clone, Debug, Default)]
struct Profile {
    name: String,
    skin: Option<[u8; 32]>,
    slim: bool,
}

enum Meta {
    Byte(u8),
    Int(i32),
    Float(f32),
    Bool(bool),
    Text(Line),
    OptText(Option<Line>),
    Stack(Option<Stack>),
    Three([f32; 3]),
    Quat([f32; 4]),
    State(u32),
    Villager([i32; 3]),
    Holder(i32),
    Enum(i32),
    Skipped,
}

impl Crowd {
    /// Whether `id` is a packet the crowd reads.
    pub fn reads(id: i32) -> bool {
        use super::v777::play::clientbound as c;
        [c::ADD_ENTITY, c::REMOVE_ENTITIES, c::MOVE_ENTITY_POS, c::MOVE_ENTITY_POS_ROT, c::MOVE_ENTITY_ROT, c::ROTATE_HEAD, c::TELEPORT_ENTITY, c::ENTITY_POSITION_SYNC, c::SET_ENTITY_DATA, c::SET_EQUIPMENT, c::PLAYER_INFO_UPDATE, c::PLAYER_INFO_REMOVE].contains(&id)
    }

    /// Reads packet `id`, one [`Crowd::reads`]; `me` is where the account stands.
    pub fn read(&mut self, id: i32, r: &mut In, me: [f64; 3]) -> Result<()> {
        use super::v777::play::clientbound as c;
        match id {
            c::ADD_ENTITY => self.add(r, me),
            c::REMOVE_ENTITIES => self.remove(r),
            c::MOVE_ENTITY_POS => self.moved(r, true, false),
            c::MOVE_ENTITY_POS_ROT => self.moved(r, true, true),
            c::MOVE_ENTITY_ROT => self.moved(r, false, true),
            c::ROTATE_HEAD => self.head(r),
            c::TELEPORT_ENTITY => self.teleport(r),
            c::ENTITY_POSITION_SYNC => self.sync(r),
            c::SET_ENTITY_DATA => self.data(r),
            c::SET_EQUIPMENT => self.equipment(r),
            c::PLAYER_INFO_UPDATE => self.players(r),
            c::PLAYER_INFO_REMOVE => self.players_gone(r),
            _ => Ok(()),
        }
    }

    /// Forgets every entity (on a respawn, a dimension change or a new configuration); players and variants stay.
    pub fn clear(&mut self) {
        self.near = HashMap::new();
    }

    /// Reads one synchronized variant registry from `registry_data`, its name already read; `false` when it is no
    /// registry this keeps, with nothing read.
    pub fn registry(&mut self, name: &str, r: &mut In) -> Result<bool> {
        let Some(&key) = VARIANTS.iter().find(|v| name.strip_prefix("minecraft:") == Some(**v)) else { return Ok(false) };
        let mut names = Vec::new();
        for _ in 0..r.list()? {
            let entry = r.string()?;
            r.optional_nbt()?;
            if names.len() < MAX_VARIANTS {
                names.push(bare(&entry).to_owned());
            }
        }
        self.variants.insert(key, names.into());
        Ok(true)
    }

    fn add(&mut self, r: &mut In, me: [f64; 3]) -> Result<()> {
        let id = r.varint()?;
        let uuid = r.uuid()?;
        let kind = r.varint()?;
        let Some(kind) = usize::try_from(kind).ok().filter(|k| *k < ENTITY_TYPES.len()) else { bail!("entity type {kind}") };
        let pos = [r.f64()?, r.f64()?, r.f64()?];
        movement(r)?;
        let pitch = angle(r.u8()?);
        let yaw = angle(r.u8()?);
        let head = angle(r.u8()?);
        let extra = r.varint()?;
        let mut mob = Mob { kind: kind as u16, uuid, pos, yaw, pitch, head, ..Mob::default() };
        if let Some(data) = initial(ENTITY_TYPES[kind], extra) {
            mob.data = Some(Box::new(data));
        }
        if !self.near.contains_key(&id) && self.near.len() >= MAX_ENTITIES {
            let far = |p: &[f64; 3]| (p[0] - me[0]).powi(2) + (p[1] - me[1]).powi(2) + (p[2] - me[2]).powi(2);
            let farthest = self.near.iter().max_by(|a, b| far(&a.1.pos).total_cmp(&far(&b.1.pos))).map(|(id, m)| (*id, far(&m.pos)));
            match farthest {
                Some((gone, d)) if d > far(&pos) => {
                    self.near.remove(&gone);
                }
                _ => return Ok(()),
            }
        }
        self.near.insert(id, mob);
        Ok(())
    }

    fn remove(&mut self, r: &mut In) -> Result<()> {
        for _ in 0..r.list()? {
            self.near.remove(&r.varint()?);
        }
        Ok(())
    }

    fn moved(&mut self, r: &mut In, position: bool, rotation: bool) -> Result<()> {
        let id = r.varint()?;
        let delta = if position { [r.i16()?, r.i16()?, r.i16()?] } else { [0; 3] };
        let turn = if rotation { Some((angle(r.u8()?), angle(r.u8()?))) } else { None };
        if let Some(mob) = self.near.get_mut(&id) {
            for (p, d) in mob.pos.iter_mut().zip(delta) {
                *p += f64::from(d) / 4096.0;
            }
            if let Some((yaw, pitch)) = turn {
                mob.yaw = yaw;
                mob.pitch = pitch;
            }
        }
        Ok(())
    }

    fn head(&mut self, r: &mut In) -> Result<()> {
        let id = r.varint()?;
        let head = angle(r.u8()?);
        if let Some(mob) = self.near.get_mut(&id) {
            mob.head = head;
        }
        Ok(())
    }

    fn teleport(&mut self, r: &mut In) -> Result<()> {
        let id = r.varint()?;
        let pos = [r.f64()?, r.f64()?, r.f64()?];
        for _ in 0..3 {
            r.f64()?;
        }
        let (yaw, pitch) = (r.f32()?, r.f32()?);
        let relative = r.i32()?;
        if let Some(mob) = self.near.get_mut(&id) {
            for (axis, value) in pos.into_iter().enumerate() {
                mob.pos[axis] = if relative >> axis & 1 == 1 { mob.pos[axis] + value } else { value };
            }
            mob.yaw = if relative >> 3 & 1 == 1 { mob.yaw + yaw } else { yaw };
            mob.pitch = if relative >> 4 & 1 == 1 { mob.pitch + pitch } else { pitch };
        }
        Ok(())
    }

    fn sync(&mut self, r: &mut In) -> Result<()> {
        let id = r.varint()?;
        if r.varint()? != 0 {
            return Ok(());
        }
        let pos = [r.f64()?, r.f64()?, r.f64()?];
        let (yaw, pitch) = (r.f32()?, r.f32()?);
        if let Some(mob) = self.near.get_mut(&id) {
            mob.pos = pos;
            mob.yaw = yaw;
            mob.pitch = pitch;
        }
        Ok(())
    }

    fn data(&mut self, r: &mut In) -> Result<()> {
        let id = r.varint()?;
        let Some(mob) = self.near.get_mut(&id) else { return Ok(()) };
        loop {
            let index = r.u8()?;
            if index == 0xFF {
                return Ok(());
            }
            let serializer = r.varint()?;
            let value = meta(r, serializer)?;
            mob.set(index, value, &self.variants);
        }
    }

    fn equipment(&mut self, r: &mut In) -> Result<()> {
        let id = r.varint()?;
        let mut worn: Vec<(usize, Option<Item>)> = Vec::new();
        loop {
            let slot = r.u8()?;
            worn.push((usize::from(slot & 0x7F), item::stack(r)?.as_ref().and_then(picture_item)));
            if slot & 0x80 == 0 {
                break;
            }
        }
        if let Some(mob) = self.near.get_mut(&id) {
            let kept = mob.equipment.get_or_insert_with(Box::default);
            for (slot, item) in worn {
                if let Some(cell) = kept.get_mut(slot) {
                    *cell = item;
                }
            }
            if kept.iter().all(Option::is_none) {
                mob.equipment = None;
            }
        }
        Ok(())
    }

    fn players(&mut self, r: &mut In) -> Result<()> {
        let actions = r.u8()?;
        for _ in 0..r.list()? {
            let uuid = r.uuid()?;
            if actions & 0x01 != 0 {
                let name = r.string()?;
                let mut skin = None;
                for _ in 0..r.list()? {
                    let (key, value) = (r.string()?, r.string()?);
                    if r.bool()? {
                        r.string()?;
                    }
                    if key == "textures" {
                        skin = textures(&value);
                    }
                }
                if self.players.len() < MAX_PLAYERS || self.players.contains_key(&uuid) {
                    let (skin, slim) = skin.unwrap_or_default();
                    self.players.insert(uuid, Profile { name, skin, slim });
                }
            }
            if actions & 0x02 != 0 && r.bool()? {
                r.uuid()?;
                r.i64()?;
                r.bytes()?;
                r.bytes()?;
            }
            if actions & 0x04 != 0 {
                r.varint()?;
            }
            if actions & 0x08 != 0 {
                r.bool()?;
            }
            if actions & 0x10 != 0 {
                r.varint()?;
            }
            if actions & 0x20 != 0 && r.bool()? {
                r.nbt()?;
            }
            if actions & 0x40 != 0 {
                r.varint()?;
            }
            if actions & 0x80 != 0 {
                r.bool()?;
            }
        }
        Ok(())
    }

    fn players_gone(&mut self, r: &mut In) -> Result<()> {
        for _ in 0..r.list()? {
            self.players.remove(&r.uuid()?);
        }
        Ok(())
    }

    /// The entities within `range` blocks of `eye` as the renderer takes them, and the hand of the account
    /// `me` holding `held`.
    pub fn seen(&self, eye: [f64; 3], range: f64, me: Uuid, held: Option<&Stack>, left_handed: bool) -> Seen {
        let near = |p: &[f64; 3]| (p[0] - eye[0]).powi(2) + (p[1] - eye[1]).powi(2) + (p[2] - eye[2]).powi(2) <= range * range;
        let entities = self.near.iter().filter(|(_, m)| near(&m.pos)).map(|(id, m)| self.entity(*id, m)).collect();
        let hand = Hand { main: held.and_then(picture_item), left_handed, skin: self.skin(me) };
        Seen { entities, hand }
    }

    fn skin(&self, uuid: Uuid) -> Skin {
        let profile = self.players.get(&uuid);
        let url = profile.and_then(|p| p.skin).map(|hash| format!("{SKINS}{}", hash.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        Skin { url, uuid, slim: profile.is_some_and(|p| p.slim), parts: 0x7F }
    }

    fn entity(&self, id: i32, mob: &Mob) -> Entity {
        let kind = ENTITY_TYPES[usize::from(mob.kind)];
        let mut data = mob.data.as_deref().cloned().unwrap_or_default();
        let mut name = mob.name.as_deref().filter(|_| mob.name_shown).map(spans);
        if let Data::Player { skin, .. } = &mut data {
            let parts = skin.parts;
            *skin = self.skin(mob.uuid);
            skin.parts = parts;
            name = name.or_else(|| self.players.get(&mob.uuid).map(|p| vec![Span { text: p.name.clone(), ..Span::default() }]));
        }
        Entity {
            id,
            kind,
            pos: mob.pos,
            yaw: mob.yaw,
            pitch: mob.pitch,
            head_yaw: mob.head,
            baby: mob.baby,
            pose: pose(mob.pose),
            flags: mob.flags,
            name,
            equipment: mob.equipment.as_deref().cloned().unwrap_or_default(),
            data,
        }
    }

    /// How many entities are kept.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.near.len()
    }
}

/// What a picture shows of the entities: plain data the drawing thread owns.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub entities: Vec<Entity>,
    pub hand: Hand,
}

impl render::Entities for Seen {
    fn entities(&self) -> &[Entity] {
        &self.entities
    }

    fn hand(&self) -> Option<&Hand> {
        Some(&self.hand)
    }
}

impl Mob {
    fn set(&mut self, index: u8, value: Meta, variants: &HashMap<&'static str, Box<[String]>>) {
        match (index, value) {
            (d::entity::DATA_SHARED_FLAGS_ID, Meta::Byte(b)) => self.flags = b,
            (d::entity::DATA_CUSTOM_NAME, Meta::OptText(t)) => self.name = t.map(Box::new),
            (d::entity::DATA_CUSTOM_NAME_VISIBLE, Meta::Bool(b)) => self.name_shown = b,
            (d::entity::DATA_POSE, Meta::Enum(p)) => self.pose = u8::try_from(p).unwrap_or(u8::MAX),
            (i, value) => self.special(i, value, variants),
        }
    }

    fn special(&mut self, index: u8, value: Meta, variants: &HashMap<&'static str, Box<[String]>>) {
        let kind = ENTITY_TYPES[usize::from(self.kind)];
        let baby_at = match kind {
            "piglin" => Some(d::piglin::DATA_BABY_ID),
            k if AGEABLE.contains(&k) => Some(d::ageable_mob::DATA_BABY_ID),
            _ => None,
        };
        if let (Some(at), Meta::Bool(b)) = (baby_at, &value)
            && at == index
        {
            self.baby = *b;
            return;
        }
        let variant = |registry: &str, id: i32| -> String { variants.get(registry).and_then(|v| usize::try_from(id).ok().and_then(|i| v.get(i))).cloned().unwrap_or_default() };
        let data = self.data.get_or_insert_with(|| Box::new(Data::None));
        match (&mut **data, index, value) {
            (Data::Player { skin, .. }, d::avatar::DATA_PLAYER_MODE_CUSTOMISATION, Meta::Byte(p)) => skin.parts = p,
            (Data::Player { main_left, .. }, d::avatar::DATA_PLAYER_MAIN_HAND, Meta::Enum(arm)) => *main_left = arm == 0,
            (slot, d::item_entity::DATA_ITEM, Meta::Stack(stack)) if kind == "item" => {
                *slot = stack.as_ref().and_then(picture_item).map_or(Data::None, |item| Data::Item { item });
            }
            (Data::Frame { item, .. }, d::item_frame::DATA_ITEM, Meta::Stack(stack)) => *item = stack.as_ref().and_then(picture_item),
            (Data::Frame { rotation, .. }, d::item_frame::DATA_ROTATION, Meta::Int(r)) => *rotation = r.clamp(0, 7) as u8,
            (Data::ArmorStand { small, arms, no_plate, marker, .. }, d::armor_stand::DATA_CLIENT_FLAGS, Meta::Byte(f)) => {
                (*small, *arms, *no_plate, *marker) = (f & 0x01 != 0, f & 0x04 != 0, f & 0x08 != 0, f & 0x10 != 0);
            }
            (Data::ArmorStand { poses, .. }, i @ d::armor_stand::DATA_HEAD_POSE..=d::armor_stand::DATA_RIGHT_LEG_POSE, Meta::Three(r)) => {
                poses[usize::from(i - d::armor_stand::DATA_HEAD_POSE)] = r;
            }
            (Data::Display { common, kind }, i, value) => display(common, kind, i, value),
            (Data::Sheep { color, sheared }, d::sheep::DATA_WOOL_ID, Meta::Byte(b)) => (*color, *sheared) = (b & 15, b & 0x10 != 0),
            (Data::Villager { kind: k, profession, level }, i, Meta::Villager([t, p, l]))
                if i == if kind == "zombie_villager" { d::zombie_villager::DATA_VILLAGER_DATA } else { d::villager::DATA_VILLAGER_DATA } =>
            {
                *k = listed(VILLAGER_TYPES, t).to_owned();
                *profession = listed(VILLAGER_PROFESSIONS, p).to_owned();
                *level = l.clamp(1, 5) as u8;
            }
            (Data::Horse { variant }, d::horse::DATA_ID_TYPE_VARIANT, Meta::Int(v)) => *variant = v as u32,
            (Data::Wolf { tame, .. }, d::tamable_animal::DATA_FLAGS_ID, Meta::Byte(f)) => *tame = f & 0x04 != 0,
            (Data::Wolf { collar, .. }, d::wolf::DATA_COLLAR_COLOR, Meta::Enum(c)) => *collar = c.clamp(0, 15) as u8,
            (Data::Wolf { variant: v, .. }, d::wolf::DATA_VARIANT_ID, Meta::Holder(id)) => *v = variant("wolf_variant", id),
            (Data::Cat { variant: v, .. }, d::cat::DATA_VARIANT_ID, Meta::Holder(id)) => *v = variant("cat_variant", id),
            (Data::Cat { collar, .. }, d::cat::DATA_COLLAR_COLOR, Meta::Enum(c)) => *collar = c.clamp(0, 15) as u8,
            (Data::Slime { size }, d::abstract_cube_mob::ID_SIZE, Meta::Int(n)) => *size = n.clamp(1, 127) as u8,
            (Data::Creeper { charged }, d::creeper::DATA_IS_POWERED, Meta::Bool(b)) => *charged = b,
            (Data::Variant(v), i, Meta::Holder(id)) => {
                let at = match kind {
                    "pig" => Some((d::pig::DATA_VARIANT_ID, "pig_variant")),
                    "cow" => Some((d::cow::DATA_VARIANT_ID, "cow_variant")),
                    "chicken" => Some((d::chicken::DATA_VARIANT_ID, "chicken_variant")),
                    "frog" => Some((d::frog::DATA_VARIANT_ID, "frog_variant")),
                    _ => None,
                };
                if let Some((at, registry)) = at
                    && at == i
                {
                    *v = variant(registry, id);
                }
            }
            _ => {}
        }
    }
}

fn display(common: &mut Display, kind: &mut DisplayKind, index: u8, value: Meta) {
    use d::display as c;
    match (kind, index, value) {
        (_, c::DATA_TRANSLATION_ID, Meta::Three(v)) => common.translation = v,
        (_, c::DATA_SCALE_ID, Meta::Three(v)) => common.scale = v,
        (_, c::DATA_LEFT_ROTATION_ID, Meta::Quat(q)) => common.left_rotation = q,
        (_, c::DATA_RIGHT_ROTATION_ID, Meta::Quat(q)) => common.right_rotation = q,
        (_, c::DATA_BILLBOARD_RENDER_CONSTRAINTS_ID, Meta::Byte(b)) => common.billboard = b.min(3),
        (_, c::DATA_BRIGHTNESS_OVERRIDE_ID, Meta::Int(v)) => common.brightness = (v >= 0).then(|| ((v >> 20 & 15) as u8, (v >> 4 & 15) as u8)),
        (_, c::DATA_VIEW_RANGE_ID, Meta::Float(f)) => common.view_range = f,
        (_, c::DATA_SHADOW_RADIUS_ID, Meta::Float(f)) => common.shadow_radius = f,
        (_, c::DATA_SHADOW_STRENGTH_ID, Meta::Float(f)) => common.shadow_strength = f,
        (_, c::DATA_WIDTH_ID, Meta::Float(f)) => common.width = f,
        (_, c::DATA_HEIGHT_ID, Meta::Float(f)) => common.height = f,
        (_, c::DATA_GLOW_COLOR_OVERRIDE_ID, Meta::Int(v)) => common.glow_color = (v != -1).then_some(v as u32),
        (DisplayKind::Block { state }, d::display_block_display::DATA_BLOCK_STATE_ID, Meta::State(s)) => *state = s,
        (DisplayKind::Item { item, .. }, d::display_item_display::DATA_ITEM_STACK_ID, Meta::Stack(stack)) => *item = stack.as_ref().and_then(picture_item),
        (DisplayKind::Item { transform, .. }, d::display_item_display::DATA_ITEM_DISPLAY_ID, Meta::Byte(b)) => *transform = b.min(8),
        (DisplayKind::Text { text, .. }, d::display_text_display::DATA_TEXT_ID, Meta::Text(line)) => *text = spans(&line),
        (DisplayKind::Text { line_width, .. }, d::display_text_display::DATA_LINE_WIDTH_ID, Meta::Int(w)) => *line_width = w,
        (DisplayKind::Text { background, .. }, d::display_text_display::DATA_BACKGROUND_COLOR_ID, Meta::Int(c)) => *background = c as u32,
        (DisplayKind::Text { opacity, .. }, d::display_text_display::DATA_TEXT_OPACITY_ID, Meta::Byte(o)) => *opacity = o,
        (DisplayKind::Text { flags, .. }, d::display_text_display::DATA_STYLE_FLAGS_ID, Meta::Byte(f)) => *flags = f,
        _ => {}
    }
}

fn initial(kind: &str, extra: i32) -> Option<Data> {
    let variant = |name: &str| Some(Data::Variant(name.to_owned()));
    match kind {
        "player" => Some(Data::Player { skin: Skin { parts: 0, ..Skin::default() }, main_left: false }),
        "item_frame" | "glow_item_frame" => Some(Data::Frame { item: None, rotation: 0, facing: extra.clamp(0, 5) as u8, glow: kind == "glow_item_frame", invisible: false }),
        "armor_stand" => Some(Data::ArmorStand {
            small: false,
            arms: false,
            no_plate: false,
            marker: false,
            poses: [[0.0; 3], [0.0; 3], [-10.0, 0.0, -10.0], [-15.0, 0.0, 10.0], [-1.0, 0.0, -1.0], [1.0, 0.0, 1.0]],
        }),
        "block_display" => Some(Data::Display { common: Display::default(), kind: DisplayKind::Block { state: 0 } }),
        "item_display" => Some(Data::Display { common: Display::default(), kind: DisplayKind::Item { item: None, transform: 0 } }),
        "text_display" => Some(Data::Display { common: Display::default(), kind: DisplayKind::Text { text: Text::new(), line_width: 200, background: 0x4000_0000, opacity: 0xFF, flags: 0 } }),
        "sheep" => Some(Data::Sheep { color: 0, sheared: false }),
        "villager" | "zombie_villager" => Some(Data::Villager { kind: "plains".into(), profession: "none".into(), level: 1 }),
        "horse" => Some(Data::Horse { variant: 0 }),
        "wolf" => Some(Data::Wolf { tame: false, collar: 14, variant: "pale".into() }),
        "cat" => Some(Data::Cat { variant: "black".into(), collar: 14 }),
        "slime" | "magma_cube" => Some(Data::Slime { size: 1 }),
        "creeper" => Some(Data::Creeper { charged: false }),
        "pig" | "cow" | "chicken" | "frog" => variant("temperate"),
        _ => None,
    }
}

fn meta(r: &mut In, serializer: i32) -> Result<Meta> {
    let three = |r: &mut In| -> Result<[f32; 3]> { Ok([r.f32()?, r.f32()?, r.f32()?]) };
    Ok(match serializer {
        s::BYTE => Meta::Byte(r.u8()?),
        s::INT => Meta::Int(r.varint()?),
        s::LONG => {
            r.varlong()?;
            Meta::Skipped
        }
        s::FLOAT => Meta::Float(r.f32()?),
        s::STRING => {
            r.string()?;
            Meta::Skipped
        }
        s::COMPONENT => Meta::Text(text::line(&r.nbt()?)),
        s::OPTIONAL_COMPONENT => Meta::OptText(if r.bool()? { Some(text::line(&r.nbt()?)) } else { None }),
        s::ITEM_STACK => Meta::Stack(item::stack(r)?),
        s::BOOLEAN => Meta::Bool(r.bool()?),
        s::ROTATIONS | s::VECTOR3 => Meta::Three(three(r)?),
        s::QUATERNION => Meta::Quat([r.f32()?, r.f32()?, r.f32()?, r.f32()?]),
        s::BLOCK_POS => {
            r.i64()?;
            Meta::Skipped
        }
        s::OPTIONAL_BLOCK_POS => {
            if r.bool()? {
                r.i64()?;
            }
            Meta::Skipped
        }
        s::OPTIONAL_LIVING_ENTITY_REFERENCE => {
            if r.bool()? {
                r.uuid()?;
            }
            Meta::Skipped
        }
        s::OPTIONAL_GLOBAL_POS => {
            if r.bool()? {
                r.string()?;
                r.i64()?;
            }
            Meta::Skipped
        }
        s::BLOCK_STATE | s::OPTIONAL_BLOCK_STATE => Meta::State(r.varint()? as u32),
        s::VILLAGER_DATA => Meta::Villager([r.varint()?, r.varint()?, r.varint()?]),
        s::DIRECTION | s::POSE | s::HUMANOID_ARM | s::DYE_COLOR | s::OPTIONAL_UNSIGNED_INT | s::SNIFFER_STATE | s::ARMADILLO_STATE | s::COPPER_GOLEM_STATE | s::WEATHERING_COPPER_STATE => Meta::Enum(r.varint()?),
        s::CAT_VARIANT | s::CAT_SOUND_VARIANT | s::COW_VARIANT | s::COW_SOUND_VARIANT | s::WOLF_VARIANT | s::WOLF_SOUND_VARIANT | s::FROG_VARIANT | s::PIG_VARIANT | s::PIG_SOUND_VARIANT | s::CHICKEN_VARIANT | s::CHICKEN_SOUND_VARIANT | s::ZOMBIE_NAUTILUS_VARIANT => Meta::Holder(r.varint()?),
        s::PAINTING_VARIANT => {
            if r.varint()? == 0 {
                r.varint()?;
                r.varint()?;
                r.string()?;
                for _ in 0..2 {
                    if r.bool()? {
                        r.nbt()?;
                    }
                }
            }
            Meta::Skipped
        }
        s::RESOLVABLE_PROFILE => {
            item::profile(r)?;
            Meta::Skipped
        }
        s::PARTICLE | s::PARTICLES => bail!("particles in entity data are not read"),
        other => bail!("entity data of serializer {other} not read"),
    })
}

fn movement(r: &mut In) -> Result<()> {
    let low = r.u8()?;
    if low != 0 {
        r.u8()?;
        r.i32()?;
        if low & 4 != 0 {
            r.varint()?;
        }
    }
    Ok(())
}

fn angle(byte: u8) -> f32 {
    f32::from(byte as i8) * 360.0 / 256.0
}

fn pose(id: u8) -> Pose {
    match id {
        0 => Pose::Standing,
        1 => Pose::FallFlying,
        2 => Pose::Sleeping,
        3 => Pose::Swimming,
        5 => Pose::Crouching,
        10 => Pose::Sitting,
        _ => Pose::Other,
    }
}

fn bare(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn listed(names: &'static [&'static str], id: i32) -> &'static str {
    usize::try_from(id).ok().and_then(|i| names.get(i)).copied().unwrap_or_default()
}

fn picture_item(stack: &Stack) -> Option<Item> {
    let &(id, ..) = usize::try_from(stack.item).ok().and_then(|i| ITEMS.get(i))?;
    Some(Item { id, count: stack.count.clamp(1, 255) as u8, glint: stack.glint, dye: stack.dye, trim: None, profile: None })
}

/// The styled runs of a read component, as the renderer takes text.
pub fn spans(line: &Line) -> Text {
    line.runs.iter().map(|r| Span { text: r.text.clone(), color: r.color, bold: r.bold, italic: r.italic, underline: r.underline, strike: r.strike }).collect()
}

fn textures(value: &str) -> Option<(Option<[u8; 32]>, bool)> {
    let json: Value = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(value).ok()?).ok()?;
    let skin = json.get("textures")?.get("SKIN")?;
    let slim = skin.get("metadata").and_then(|m| m.get("model")).and_then(Value::as_str) == Some("slim");
    let hash = skin.get("url").and_then(Value::as_str).and_then(|u| u.strip_prefix(SKINS).or_else(|| u.strip_prefix("https://textures.minecraft.net/texture/")));
    let hash = hash.filter(|h| h.len() == 64).and_then(|h| {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(h.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(out)
    });
    Some((hash, slim))
}

#[cfg(test)]
mod tests {
    use super::super::wire::Out;
    use super::*;

    fn kind(name: &str) -> i32 {
        ENTITY_TYPES.iter().position(|k| *k == name).unwrap() as i32
    }

    fn item(name: &str) -> i32 {
        ITEMS.iter().position(|(id, ..)| *id == name).unwrap() as i32
    }

    fn add(c: &mut Crowd, id: i32, name: &str, pos: [f64; 3]) {
        let mut o = Out::default();
        o.varint(id).uuid(Uuid::from_u128(id as u128)).varint(kind(name)).f64(pos[0]).f64(pos[1]).f64(pos[2]).u8(0).u8(0).u8(64).u8(32).varint(3);
        c.add(&mut In::new(&o.0), [0.0; 3]).unwrap();
    }

    fn data(c: &mut Crowd, id: i32, fill: impl FnOnce(&mut Out)) -> Result<()> {
        let mut o = Out::default();
        o.varint(id);
        fill(&mut o);
        c.data(&mut In::new(&o.0))
    }

    fn seen(c: &Crowd) -> Vec<Entity> {
        let mut all = c.seen([0.0; 3], 1000.0, Uuid::nil(), None, false).entities;
        all.sort_by_key(|e| e.id);
        all
    }

    #[test]
    fn a_villager_spawns_moves_turns_and_is_described_by_its_entity_data() {
        let mut c = Crowd::default();
        add(&mut c, 7, "villager", [1.0, 64.0, 2.0]);
        data(&mut c, 7, |o| {
            o.u8(d::villager::DATA_VILLAGER_DATA).varint(s::VILLAGER_DATA).varint(VILLAGER_TYPES.iter().position(|t| *t == "desert").unwrap() as i32);
            o.varint(VILLAGER_PROFESSIONS.iter().position(|t| *t == "farmer").unwrap() as i32).varint(3);
            o.u8(d::ageable_mob::DATA_BABY_ID).varint(s::BOOLEAN).bool(true);
            o.u8(d::entity::DATA_CUSTOM_NAME).varint(s::OPTIONAL_COMPONENT).bool(true).u8(8).raw(&[0, 3]).raw(b"Bob");
            o.u8(d::entity::DATA_CUSTOM_NAME_VISIBLE).varint(s::BOOLEAN).bool(true).u8(0xFF);
        })
        .unwrap();
        let mut o = Out::default();
        o.varint(7).i16(4096).i16(0).i16(-2048).u8(0x40).u8(0).bool(true);
        c.moved(&mut In::new(&o.0), true, true).unwrap();
        let mut o = Out::default();
        o.varint(7).u8(0xC0);
        c.head(&mut In::new(&o.0)).unwrap();
        let e = &seen(&c)[0];
        assert_eq!((e.kind, e.pos, e.yaw, e.head_yaw, e.baby), ("villager", [2.0, 64.0, 1.5], 90.0, -90.0, true));
        assert_eq!(e.data, Data::Villager { kind: "desert".into(), profession: "farmer".into(), level: 3 });
        assert_eq!(e.name.as_ref().unwrap()[0].text, "Bob");
        let mut o = Out::default();
        o.varint(7).f64(1.0).f64(0.0).f64(-1.0).f64(0.0).f64(0.0).f64(0.0).f32(10.0).f32(5.0).i32(0b0_0010);
        c.teleport(&mut In::new(&o.0)).unwrap();
        assert_eq!(seen(&c)[0].pos, [1.0, 64.0, -1.0], "a relative axis adds, the others set");
        let mut o = Out::default();
        o.varint(1).varint(7);
        c.remove(&mut In::new(&o.0)).unwrap();
        assert!(seen(&c).is_empty());
    }

    #[test]
    fn display_entities_carry_their_transform_text_and_item() {
        let mut c = Crowd::default();
        add(&mut c, 1, "text_display", [0.0; 3]);
        add(&mut c, 2, "item_display", [0.0; 3]);
        data(&mut c, 1, |o| {
            o.u8(d::display::DATA_TRANSLATION_ID).varint(s::VECTOR3).f32(0.0).f32(1.5).f32(0.0);
            o.u8(d::display::DATA_BILLBOARD_RENDER_CONSTRAINTS_ID).varint(s::BYTE).u8(3);
            o.u8(d::display_text_display::DATA_TEXT_ID).varint(s::COMPONENT).u8(8).raw(&[0, 5]).raw(b"Shop!");
            o.u8(d::display_text_display::DATA_BACKGROUND_COLOR_ID).varint(s::INT).varint(0x7F00_0000);
            o.u8(d::display_text_display::DATA_STYLE_FLAGS_ID).varint(s::BYTE).u8(0x01).u8(0xFF);
        })
        .unwrap();
        data(&mut c, 2, |o| {
            o.u8(d::display_item_display::DATA_ITEM_STACK_ID).varint(s::ITEM_STACK).varint(1).varint(item("diamond_sword")).varint(0).varint(0);
            o.u8(d::display_item_display::DATA_ITEM_DISPLAY_ID).varint(s::BYTE).u8(5).u8(0xFF);
        })
        .unwrap();
        let all = seen(&c);
        let Data::Display { common, kind: DisplayKind::Text { text, background, flags, .. } } = &all[0].data else { panic!("{:?}", all[0].data) };
        assert_eq!((common.translation, common.billboard, text[0].text.as_str(), *background, *flags), ([0.0, 1.5, 0.0], 3, "Shop!", 0x7F00_0000, 1));
        let Data::Display { kind: DisplayKind::Item { item: Some(item), transform }, .. } = &all[1].data else { panic!("{:?}", all[1].data) };
        assert_eq!((item.id, *transform), ("diamond_sword", 5));
    }

    #[test]
    fn a_player_wears_the_skin_and_name_from_the_player_list_and_its_equipment() {
        let mut c = Crowd::default();
        let hash = "a".repeat(64);
        let textures = base64::engine::general_purpose::STANDARD.encode(format!(r#"{{"textures":{{"SKIN":{{"url":"{SKINS}{hash}","metadata":{{"model":"slim"}}}}}}}}"#));
        let mut o = Out::default();
        o.u8(0x09).varint(1).uuid(Uuid::from_u128(5)).string("Alex").varint(1).string("textures").string(&textures).bool(false).bool(true);
        c.players(&mut In::new(&o.0)).unwrap();
        add(&mut c, 5, "player", [0.0; 3]);
        data(&mut c, 5, |o| {
            o.u8(d::avatar::DATA_PLAYER_MODE_CUSTOMISATION).varint(s::BYTE).u8(0x7E).u8(0xFF);
        })
        .unwrap();
        let mut o = Out::default();
        o.varint(5).u8(0x80).varint(1).varint(item("iron_sword")).varint(0).varint(0).u8(5).varint(1).varint(item("diamond_helmet")).varint(0).varint(0);
        c.equipment(&mut In::new(&o.0)).unwrap();
        let e = &seen(&c)[0];
        let Data::Player { skin, main_left } = &e.data else { panic!() };
        assert_eq!((skin.url.as_deref(), skin.slim, skin.parts, *main_left), (Some(format!("{SKINS}{hash}").as_str()), true, 0x7E, false));
        assert_eq!(e.name.as_ref().unwrap()[0].text, "Alex");
        assert_eq!(e.equipment[render::slot::MAIN_HAND].as_ref().unwrap().id, "iron_sword");
        assert_eq!(e.equipment[render::slot::HEAD].as_ref().unwrap().id, "diamond_helmet");
    }

    #[test]
    fn entity_data_it_cannot_read_stops_there_and_keeps_what_came_before() {
        let mut c = Crowd::default();
        add(&mut c, 3, "sheep", [0.0; 3]);
        let read = data(&mut c, 3, |o| {
            o.u8(d::sheep::DATA_WOOL_ID).varint(s::BYTE).u8(0x14);
            o.u8(99).varint(s::PARTICLE).varint(0).u8(0xFF);
        });
        assert!(read.is_err());
        assert_eq!(seen(&c)[0].data, Data::Sheep { color: 4, sheared: true });
    }

    #[test]
    fn past_the_cap_a_nearer_entity_replaces_the_farthest() {
        let mut c = Crowd::default();
        for i in 0..MAX_ENTITIES as i32 {
            add(&mut c, i, "pig", [f64::from(i) + 10.0, 0.0, 0.0]);
        }
        add(&mut c, 10_000, "pig", [1.0, 0.0, 0.0]);
        add(&mut c, 10_001, "pig", [10_000.0, 0.0, 0.0]);
        assert_eq!(c.len(), MAX_ENTITIES);
        let ids: Vec<i32> = seen(&c).iter().map(|e| e.id).collect();
        assert!(ids.contains(&10_000) && !ids.contains(&10_001) && !ids.contains(&(MAX_ENTITIES as i32 - 1)));
    }
}
