//! The player's own state as the game's heads-up display shows it, from what the server sends: game mode, health,
//! food, experience, armor, effects, air, the sidebar scoreboard, boss bars and recent chat.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use uuid::Uuid;

use super::crowd::{self, Meta};
use super::item;
use super::render::hud::{self, Boss, Chat, Hud, Number, Score, Sidebar, Team, Text};
use super::screen::Screen;
use super::text;
use super::v777::{ATTRIBUTES, ITEMS, MAX_DAMAGE, MOB_EFFECTS, entity_data as d, play};
use super::wire::In;

const MAX_OBJECTIVES: usize = 64;
const MAX_SCORES: usize = 1024;
const MAX_TEAMS: usize = 256;
const MAX_MEMBERS: usize = 4096;
const MAX_BOSSES: usize = 16;
const MAX_EFFECTS: usize = 64;
const MAX_CHAT: usize = 20;
const CHAT_TICKS: i32 = 200;
const SIDEBAR: usize = 1;
const TEAM_SLOTS: usize = 3;
const SLOTS: usize = TEAM_SLOTS + TEAM_COLORS.len();
const FREEZE_TICKS: i32 = 140;
const MAX_AIR: i32 = 300;
const HEALTH: f32 = 20.0;
const FOOD: i32 = 20;
const SATURATION: f32 = 5.0;
const CHANGE_GAME_MODE: u8 = 3;
const KEEP_ATTRIBUTES: u8 = 1;
const KEEP_DATA: u8 = 2;
const MAX_NAME: usize = 256;
const TEAM_COLORS: [u32; 16] = [
    0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xFFAA00, 0xAAAAAA, 0x555555, 0x5555FF, 0x55FF55, 0x55FFFF, 0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF,
];

struct Objective {
    title: Text,
    number: Number,
}

struct Entry {
    value: i32,
    display: Option<Text>,
    number: Number,
}

#[derive(Default)]
struct Squad {
    team: Team,
    color: Option<usize>,
}

/// One connection's view of its own player.
pub struct Status {
    name: String,
    entity: Option<i32>,
    game_mode: u8,
    hardcore: bool,
    health: f32,
    food: i32,
    saturation: f32,
    xp: f32,
    level: i32,
    max_health: f32,
    armor: f32,
    effects: Vec<(i32, Option<Instant>)>,
    absorption: f32,
    air: i32,
    frozen: i32,
    left_handed: bool,
    objectives: HashMap<String, Objective>,
    scores: HashMap<(String, String), Entry>,
    slots: [Option<String>; SLOTS],
    teams: HashMap<String, Squad>,
    members: HashMap<String, String>,
    bosses: Vec<(Uuid, Boss)>,
    chat: VecDeque<(Text, Instant)>,
}

impl Status {
    /// The state of player `name` before the server said anything.
    pub fn new(name: String) -> Self {
        Self {
            name,
            entity: None,
            game_mode: 0,
            hardcore: false,
            health: HEALTH,
            food: FOOD,
            saturation: SATURATION,
            xp: 0.0,
            level: 0,
            max_health: HEALTH,
            armor: 0.0,
            effects: Vec::new(),
            absorption: 0.0,
            air: MAX_AIR,
            frozen: 0,
            left_handed: false,
            objectives: HashMap::new(),
            scores: HashMap::new(),
            slots: Default::default(),
            teams: HashMap::new(),
            members: HashMap::new(),
            bosses: Vec::new(),
            chat: VecDeque::new(),
        }
    }

    /// Forgets everything the server said, as a new connection or configuration does.
    pub fn reset(&mut self) {
        *self = Self::new(std::mem::take(&mut self.name));
    }

    /// Whether `id` is a packet the status reads on its own.
    pub fn reads(id: i32) -> bool {
        use play::clientbound as c;
        [c::SET_EXPERIENCE, c::UPDATE_ATTRIBUTES, c::UPDATE_MOB_EFFECT, c::REMOVE_MOB_EFFECT, c::SET_OBJECTIVE, c::SET_DISPLAY_OBJECTIVE, c::SET_SCORE, c::RESET_SCORE, c::SET_PLAYER_TEAM, c::BOSS_EVENT].contains(&id)
    }

    /// Takes in packet `id`, one of those [`Status::reads`] names.
    pub fn read(&mut self, id: i32, r: &mut In) -> Result<()> {
        use play::clientbound as c;
        match id {
            c::SET_EXPERIENCE => {
                self.xp = unit(r.f32()?);
                self.level = r.varint()?.max(0);
            }
            c::UPDATE_ATTRIBUTES => self.attributes(r)?,
            c::UPDATE_MOB_EFFECT => self.effect(r)?,
            c::REMOVE_MOB_EFFECT => {
                if self.mine(r.varint()?) {
                    let effect = r.varint()?;
                    self.effects.retain(|(e, _)| *e != effect);
                }
            }
            c::SET_OBJECTIVE => self.objective(r)?,
            c::SET_DISPLAY_OBJECTIVE => {
                let slot = r.varint()?;
                let name = r.string()?;
                let known = self.objectives.contains_key(&name);
                if let Some(slot) = usize::try_from(slot).ok().and_then(|s| self.slots.get_mut(s)) {
                    *slot = known.then_some(name);
                }
            }
            c::SET_SCORE => self.score(r)?,
            c::RESET_SCORE => {
                let owner = r.string()?;
                let objective = if r.bool()? { Some(r.string()?) } else { None };
                self.scores.retain(|(o, w), _| *w != owner || objective.as_ref().is_some_and(|x| x != o));
            }
            c::SET_PLAYER_TEAM => self.team(r)?,
            c::BOSS_EVENT => self.boss(r)?,
            _ => {}
        }
        Ok(())
    }

    /// The player joined a world as entity `entity`.
    pub fn login(&mut self, entity: i32, hardcore: bool) {
        self.entity = Some(entity);
        self.hardcore = hardcore;
    }

    /// The player came back after death or changed dimension, keeping what `keep` says (1 attribute modifiers, 2 entity
    /// data); its effects always start over.
    pub fn respawned(&mut self, keep: u8) {
        self.effects.clear();
        if keep & KEEP_ATTRIBUTES == 0 {
            (self.max_health, self.armor) = (HEALTH, 0.0);
        }
        if keep & KEEP_DATA == 0 {
            (self.absorption, self.air, self.frozen) = (0.0, MAX_AIR, 0);
        }
    }

    /// The game mode by its protocol id.
    pub fn game_mode(&mut self, id: i32) {
        self.game_mode = u8::try_from(id).ok().filter(|m| *m <= 3).unwrap_or(0);
    }

    /// Takes in a game event; only the change of game mode concerns the status.
    pub fn game_event(&mut self, event: u8, value: f32) {
        if event == CHANGE_GAME_MODE && value.is_finite() {
            self.game_mode(value as i32);
        }
    }

    /// Takes in the health packet.
    pub fn health(&mut self, health: f32, food: i32, saturation: f32) {
        self.health = if health.is_finite() { health.max(0.0) } else { 0.0 };
        self.food = food.clamp(0, FOOD);
        self.saturation = if saturation.is_finite() { saturation.max(0.0) } else { 0.0 };
    }

    /// Takes in an entity data packet; only the player's own entity concerns the status.
    pub fn data(&mut self, r: &mut In) -> Result<()> {
        if !self.mine(r.varint()?) {
            return Ok(());
        }
        crowd::items(r, |index, value| match (index, value) {
            (d::entity::DATA_AIR_SUPPLY_ID, Meta::Int(air)) => self.air = air.min(MAX_AIR),
            (d::entity::DATA_TICKS_FROZEN, Meta::Int(ticks)) => self.frozen = ticks,
            (d::player::DATA_PLAYER_ABSORPTION_ID, Meta::Float(more)) => self.absorption = if more.is_finite() { more.max(0.0) } else { 0.0 },
            (d::avatar::DATA_PLAYER_MAIN_HAND, Meta::Enum(arm)) => self.left_handed = arm == 0,
            _ => {}
        })
    }

    /// Keeps a chat line for the chat on the lower left.
    pub fn chat(&mut self, text: Text) {
        if self.chat.len() == MAX_CHAT {
            self.chat.pop_front();
        }
        self.chat.push_back((text, Instant::now()));
    }

    /// Whether the player's left hand is its main one.
    pub fn left_handed(&self) -> bool {
        self.left_handed
    }

    /// What the heads-up display shows now, with the hands and titles of `screen`.
    pub fn hud(&self, screen: &Screen) -> Hud {
        let now = Instant::now();
        let active = |name: &str| self.effects.iter().any(|(e, until)| usize::try_from(*e).ok().and_then(|e| MOB_EFFECTS.get(e)) == Some(&name) && until.is_none_or(|u| u > now));
        let (hands, selected) = screen.hands();
        Hud {
            game_mode: self.game_mode,
            hardcore: self.hardcore,
            health: self.health,
            max_health: self.max_health,
            absorption: self.absorption,
            poisoned: active("poison"),
            withered: active("wither"),
            frozen: self.frozen >= FREEZE_TICKS,
            food: self.food,
            saturation: self.saturation,
            hungry: active("hunger"),
            armor: (self.armor as i32).clamp(0, 20),
            air: self.air.max(0),
            max_air: MAX_AIR,
            under_water: self.air < MAX_AIR,
            xp: self.xp,
            level: self.level,
            selected: u8::try_from(selected.min(8)).unwrap_or(0),
            hotbar: std::array::from_fn(|i| hands[i].as_ref().and_then(stack)),
            offhand: hands[9].as_ref().and_then(stack),
            left_handed: self.left_handed,
            sidebar: self.sidebar(),
            bosses: self.bosses.iter().map(|(_, b)| b.clone()).collect(),
            title: screen.hud_title(),
            action_bar: screen.hud_action_bar(),
            chat: self.chat.iter().map(|(text, at)| Chat { text: text.clone(), age: ticks(now.duration_since(*at)) }).filter(|c| c.age <= CHAT_TICKS).collect(),
        }
    }

    fn mine(&self, entity: i32) -> bool {
        self.entity == Some(entity)
    }

    fn attributes(&mut self, r: &mut In) -> Result<()> {
        if !self.mine(r.varint()?) {
            return Ok(());
        }
        for _ in 0..r.list()? {
            let kind = r.varint()?;
            let base = r.f64()?;
            let (mut add, mut scale, mut total) = (0.0, 0.0, 1.0);
            for _ in 0..r.list()? {
                r.string()?;
                let amount = r.f64()?;
                match r.varint()? {
                    0 => add += amount,
                    1 => scale += amount,
                    2 => total *= 1.0 + amount,
                    other => bail!("attribute operation {other}"),
                }
            }
            let sum = base + add;
            let value = (sum + sum * scale) * total;
            if !value.is_finite() {
                continue;
            }
            match usize::try_from(kind).ok().and_then(|k| ATTRIBUTES.get(k)) {
                Some(&"max_health") => self.max_health = value.clamp(1.0, 1024.0) as f32,
                Some(&"armor") => self.armor = value.clamp(0.0, 30.0) as f32,
                _ => {}
            }
        }
        Ok(())
    }

    fn effect(&mut self, r: &mut In) -> Result<()> {
        if !self.mine(r.varint()?) {
            return Ok(());
        }
        let effect = r.varint()?;
        let _amplifier = r.varint()?;
        let duration = r.varint()?;
        r.u8()?;
        let until = u64::try_from(duration).ok().map(|t| Instant::now() + Duration::from_millis(t.saturating_mul(50)));
        let room = self.effects.len() < MAX_EFFECTS;
        match self.effects.iter_mut().find(|(e, _)| *e == effect) {
            Some(kept) => kept.1 = until,
            None if room => self.effects.push((effect, until)),
            None => {}
        }
        Ok(())
    }

    fn objective(&mut self, r: &mut In) -> Result<()> {
        let name = r.string()?;
        match r.u8()? {
            1 => {
                self.objectives.remove(&name);
                self.scores.retain(|(o, _), _| *o != name);
                for slot in &mut self.slots {
                    if slot.as_ref() == Some(&name) {
                        *slot = None;
                    }
                }
            }
            method @ (0 | 2) => {
                let title = text::line(&r.nbt()?).runs;
                let _render = r.varint()?;
                let number = number(r)?.unwrap_or_default();
                let fresh = Objective { title, number };
                let room = self.objectives.len() < MAX_OBJECTIVES;
                match self.objectives.get_mut(&name) {
                    Some(kept) => *kept = fresh,
                    None if method == 0 && room && fits(&name) => {
                        self.objectives.insert(name, fresh);
                    }
                    None => {}
                }
            }
            other => bail!("objective method {other}"),
        }
        Ok(())
    }

    fn score(&mut self, r: &mut In) -> Result<()> {
        let owner = r.string()?;
        let objective = r.string()?;
        let value = r.varint()?;
        let display = if r.bool()? { Some(text::line(&r.nbt()?).runs) } else { None };
        let number = number(r)?.unwrap_or_default();
        if !self.objectives.contains_key(&objective) || !fits(&owner) {
            return Ok(());
        }
        let key = (objective, owner);
        if self.scores.len() < MAX_SCORES || self.scores.contains_key(&key) {
            self.scores.insert(key, Entry { value, display, number });
        }
        Ok(())
    }

    fn team(&mut self, r: &mut In) -> Result<()> {
        let name = r.string()?;
        let method = r.u8()?;
        let squad = if matches!(method, 0 | 2) { Some(squad(r)?) } else { None };
        match (method, squad) {
            (0, Some(squad)) => {
                if (self.teams.len() >= MAX_TEAMS || !fits(&name)) && !self.teams.contains_key(&name) {
                    return Ok(());
                }
                self.teams.insert(name.clone(), squad);
            }
            (1, _) => {
                self.teams.remove(&name);
                self.members.retain(|_, t| *t != name);
                return Ok(());
            }
            (2, Some(squad)) => {
                if let Some(kept) = self.teams.get_mut(&name) {
                    *kept = squad;
                }
                return Ok(());
            }
            (3 | 4, _) => {}
            (other, _) => bail!("team method {other}"),
        }
        let known = self.teams.contains_key(&name);
        for _ in 0..r.list()? {
            let member = r.string()?;
            if method == 4 {
                if self.members.get(&member) == Some(&name) {
                    self.members.remove(&member);
                }
            } else if known && fits(&member) && (self.members.len() < MAX_MEMBERS || self.members.contains_key(&member)) {
                self.members.insert(member, name.clone());
            }
        }
        Ok(())
    }

    fn boss(&mut self, r: &mut In) -> Result<()> {
        let id = r.uuid()?;
        let operation = r.varint()?;
        match operation {
            0 => {
                let boss = Boss { name: text::line(&r.nbt()?).runs, progress: unit(r.f32()?), color: index(r.varint()?, 7), overlay: index(r.varint()?, 5) };
                r.u8()?;
                let room = self.bosses.len() < MAX_BOSSES;
                match self.bosses.iter_mut().find(|(u, _)| *u == id) {
                    Some(kept) => kept.1 = boss,
                    None if room => self.bosses.push((id, boss)),
                    None => {}
                }
                return Ok(());
            }
            1 => {
                self.bosses.retain(|(u, _)| *u != id);
                return Ok(());
            }
            2..=5 => {}
            other => bail!("boss bar operation {other}"),
        }
        let Some((_, boss)) = self.bosses.iter_mut().find(|(u, _)| *u == id) else { return Ok(()) };
        match operation {
            2 => boss.progress = unit(r.f32()?),
            3 => boss.name = text::line(&r.nbt()?).runs,
            4 => (boss.color, boss.overlay) = (index(r.varint()?, 7), index(r.varint()?, 5)),
            _ => {}
        }
        Ok(())
    }

    fn sidebar(&self) -> Option<Sidebar> {
        let own = self.members.get(&self.name).and_then(|t| self.teams.get(t)).and_then(|s| s.color);
        let name = own.and_then(|c| self.slots[TEAM_SLOTS + c].as_ref()).or(self.slots[SIDEBAR].as_ref())?;
        let objective = self.objectives.get(name)?;
        let scores = self
            .scores
            .iter()
            .filter(|((o, _), _)| o == name)
            .map(|((_, owner), e)| Score {
                owner: owner.clone(),
                display: e.display.clone(),
                value: e.value,
                number: e.number.clone(),
                team: self.members.get(owner).and_then(|t| self.teams.get(t)).map(|s| s.team.clone()),
            })
            .collect();
        Some(Sidebar { title: objective.title.clone(), scores, number: objective.number.clone() })
    }
}

fn squad(r: &mut In) -> Result<Squad> {
    let _display = r.nbt()?;
    let prefix = text::line(&r.nbt()?).runs;
    let suffix = text::line(&r.nbt()?).runs;
    let _visibility = r.varint()?;
    let _collision = r.varint()?;
    let color = if r.bool()? { usize::try_from(r.varint()?).ok().filter(|c| *c < TEAM_COLORS.len()) } else { None };
    let _options = r.u8()?;
    Ok(Squad { team: Team { prefix, suffix, color: color.map(|c| TEAM_COLORS[c]) }, color })
}

fn number(r: &mut In) -> Result<Option<Number>> {
    if !r.bool()? {
        return Ok(None);
    }
    Ok(Some(match r.varint()? {
        0 => Number::Blank,
        1 => Number::Styled(text::style(&r.nbt()?)),
        2 => Number::Fixed(text::line(&r.nbt()?).runs),
        other => bail!("number format {other}"),
    }))
}

fn stack(held: &item::Stack) -> Option<hud::Stack> {
    let at = usize::try_from(held.item).ok()?;
    let id = ITEMS.get(at)?.0;
    let most = held.max_damage.unwrap_or_else(|| {
        let at = u16::try_from(at).unwrap_or(u16::MAX);
        MAX_DAMAGE.binary_search_by_key(&at, |(item, _)| *item).map_or(0, |i| i32::from(MAX_DAMAGE[i].1))
    });
    let most = most.max(0);
    Some(hud::Stack { id, count: held.count, glint: held.glint, damage: if most > 0 { held.damage.clamp(0, most) } else { 0 }, max_damage: most, dye: held.dye })
}

/// Whether a scoreboard name is short enough to keep; vanilla allows far longer ones, but no real name comes close.
fn fits(name: &str) -> bool {
    name.len() <= MAX_NAME
}

fn unit(value: f32) -> f32 {
    if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 }
}

fn index(value: i32, count: u8) -> u8 {
    u8::try_from(value).ok().filter(|v| *v < count).unwrap_or(0)
}

/// Whole game ticks in `span`.
pub fn ticks(span: Duration) -> i32 {
    i32::try_from(span.as_millis() / 50).unwrap_or(i32::MAX)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chat_text;
    use crate::core::headless::v777::{PARTICLE_TYPES, serializer as s};
    use crate::core::headless::wire::Out;

    use play::clientbound as c;

    fn feed(status: &mut Status, id: i32, fill: impl FnOnce(&mut Out)) {
        let mut o = Out::default();
        fill(&mut o);
        status.read(id, &mut In::new(&o.0)).unwrap();
    }

    fn text(o: &mut Out, s: &str) {
        o.u8(8).u16(u16::try_from(s.len()).unwrap()).raw(s.as_bytes());
    }

    fn at(table: &[&str], name: &str) -> i32 {
        i32::try_from(table.iter().position(|n| *n == name).unwrap()).unwrap()
    }

    fn joined(name: &str) -> Status {
        let mut status = Status::new(name.into());
        status.login(7, true);
        status
    }

    #[test]
    fn vitals_attributes_and_effects_of_the_own_player_reach_the_display() {
        let mut st = joined("Dev");
        st.game_mode(2);
        st.health(13.0, 9, 1.5);
        feed(&mut st, c::SET_EXPERIENCE, |o| {
            o.f32(0.25).varint(4).varint(60);
        });
        feed(&mut st, c::UPDATE_ATTRIBUTES, |o| {
            o.varint(7).varint(2);
            o.varint(at(ATTRIBUTES, "armor")).f64(4.0).varint(2);
            o.string("a").f64(2.0).varint(0);
            o.string("b").f64(0.5).varint(1);
            o.varint(at(ATTRIBUTES, "max_health")).f64(20.0).varint(1).string("c").f64(1.0).varint(2);
        });
        feed(&mut st, c::UPDATE_ATTRIBUTES, |o| {
            o.varint(8).varint(1).varint(at(ATTRIBUTES, "armor")).f64(20.0).varint(0);
        });
        for (entity, effect) in [(7, "poison"), (7, "hunger"), (8, "wither")] {
            feed(&mut st, c::UPDATE_MOB_EFFECT, |o| {
                o.varint(entity).varint(at(MOB_EFFECTS, effect)).varint(0).varint(-1).u8(0);
            });
        }
        feed(&mut st, c::REMOVE_MOB_EFFECT, |o| {
            o.varint(7).varint(at(MOB_EFFECTS, "hunger"));
        });
        st.game_event(CHANGE_GAME_MODE, 0.0);
        let hud = st.hud(&Screen::default());
        assert_eq!((hud.game_mode, hud.hardcore, hud.health, hud.food, hud.saturation), (0, true, 13.0, 9, 1.5));
        assert_eq!((hud.xp, hud.level, hud.armor, hud.max_health), (0.25, 4, 9, 40.0));
        assert_eq!((hud.poisoned, hud.hungry, hud.withered), (true, false, false));
    }

    #[test]
    fn own_entity_data_is_read_past_effect_particles() {
        let mut st = joined("Dev");
        let effect = i32::try_from(PARTICLE_TYPES.iter().position(|(n, _)| *n == "entity_effect").unwrap()).unwrap();
        let simple = i32::try_from(PARTICLE_TYPES.iter().position(|(_, plain)| *plain).unwrap()).unwrap();
        let data = |entity: i32| {
            let mut o = Out::default();
            o.varint(entity);
            o.u8(d::entity::DATA_AIR_SUPPLY_ID).varint(s::INT).varint(120);
            o.u8(d::living_entity::DATA_EFFECT_PARTICLES).varint(s::PARTICLES).varint(2).varint(effect).i32(0x7F00FF00).varint(simple);
            o.u8(d::entity::DATA_TICKS_FROZEN).varint(s::INT).varint(140);
            o.u8(d::player::DATA_PLAYER_ABSORPTION_ID).varint(s::FLOAT).f32(4.0);
            o.u8(0xFF);
            o
        };
        st.data(&mut In::new(&data(8).0)).unwrap();
        assert_eq!(st.hud(&Screen::default()).absorption, 0.0);
        st.data(&mut In::new(&data(7).0)).unwrap();
        let hud = st.hud(&Screen::default());
        assert_eq!((hud.air, hud.under_water, hud.frozen, hud.absorption), (120, true, true, 4.0));
        st.respawned(KEEP_ATTRIBUTES | KEEP_DATA);
        assert_eq!(st.hud(&Screen::default()).absorption, 4.0);
        st.respawned(0);
        let hud = st.hud(&Screen::default());
        assert_eq!((hud.air, hud.under_water, hud.frozen, hud.absorption), (MAX_AIR, false, false, 0.0));
    }

    #[test]
    fn the_sidebar_follows_objectives_scores_teams_and_the_own_team_colour() {
        let mut st = joined("Dev");
        for (name, title) in [("main", "Main"), ("red", "Red side")] {
            feed(&mut st, c::SET_OBJECTIVE, |o| {
                o.string(name).u8(0);
                text(o, title);
                o.varint(0).bool(false);
            });
        }
        feed(&mut st, c::SET_DISPLAY_OBJECTIVE, |o| {
            o.varint(1).string("main");
        });
        feed(&mut st, c::SET_SCORE, |o| {
            o.string("Alex").string("main").varint(5).bool(true);
            text(o, "Alexander");
            o.bool(true).varint(0);
        });
        feed(&mut st, c::SET_SCORE, |o| {
            o.string("Dev").string("main").varint(2).bool(false).bool(false);
        });
        feed(&mut st, c::SET_SCORE, |o| {
            o.string("Ghost").string("gone").varint(9).bool(false).bool(false);
        });
        feed(&mut st, c::SET_PLAYER_TEAM, |o| {
            o.string("reds").u8(0);
            text(o, "Reds");
            text(o, "[R] ");
            text(o, "");
            o.varint(0).varint(0).bool(true).varint(12).u8(0).varint(1).string("Alex");
        });
        let side = st.hud(&Screen::default()).sidebar.unwrap();
        assert_eq!(chat_text::plain(&side.title), "Main");
        let alex = side.scores.iter().find(|s| s.owner == "Alex").unwrap();
        assert_eq!((alex.value, alex.number.clone(), chat_text::plain(alex.display.as_ref().unwrap())), (5, Number::Blank, "Alexander".into()));
        assert_eq!(alex.team.as_ref().unwrap().color, Some(0xFF5555));
        assert_eq!(side.scores.len(), 2);
        feed(&mut st, c::SET_SCORE, |o| {
            o.string(&"x".repeat(MAX_NAME + 1)).string("main").varint(1).bool(false).bool(false);
        });
        assert_eq!(st.hud(&Screen::default()).sidebar.unwrap().scores.len(), 2);
        feed(&mut st, c::SET_DISPLAY_OBJECTIVE, |o| {
            o.varint(i32::try_from(TEAM_SLOTS).unwrap() + 12).string("red");
        });
        feed(&mut st, c::SET_PLAYER_TEAM, |o| {
            o.string("reds").u8(3).varint(1).string("Dev");
        });
        assert_eq!(chat_text::plain(&st.hud(&Screen::default()).sidebar.unwrap().title), "Red side");
        feed(&mut st, c::SET_OBJECTIVE, |o| {
            o.string("red").u8(1);
        });
        assert_eq!(chat_text::plain(&st.hud(&Screen::default()).sidebar.unwrap().title), "Main");
        feed(&mut st, c::SET_PLAYER_TEAM, |o| {
            o.string("reds").u8(1);
        });
        feed(&mut st, c::RESET_SCORE, |o| {
            o.string("Alex").bool(false);
        });
        let side = st.hud(&Screen::default()).sidebar.unwrap();
        assert_eq!(side.scores.iter().map(|s| (s.owner.as_str(), s.team.is_some())).collect::<Vec<_>>(), [("Dev", false)]);
        feed(&mut st, c::SET_OBJECTIVE, |o| {
            o.string("main").u8(1);
        });
        assert!(st.hud(&Screen::default()).sidebar.is_none());
        feed(&mut st, c::SET_OBJECTIVE, |o| {
            o.string("main").u8(0);
            text(o, "Back");
            o.varint(0).bool(false);
        });
        feed(&mut st, c::SET_DISPLAY_OBJECTIVE, |o| {
            o.varint(0).string("unknown");
        });
        assert!(st.hud(&Screen::default()).sidebar.is_none() && st.slots.iter().all(Option::is_none));
    }

    #[test]
    fn boss_bars_are_added_changed_and_removed_by_id() {
        let mut st = joined("Dev");
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        for (id, name) in [(a, "Wither"), (b, "Event")] {
            feed(&mut st, c::BOSS_EVENT, |o| {
                o.uuid(id).varint(0);
                text(o, name);
                o.f32(1.0).varint(5).varint(1).u8(0);
            });
        }
        feed(&mut st, c::BOSS_EVENT, |o| {
            o.uuid(a).varint(2).f32(f32::NAN);
        });
        feed(&mut st, c::BOSS_EVENT, |o| {
            o.uuid(b).varint(4).varint(9).varint(4);
        });
        feed(&mut st, c::BOSS_EVENT, |o| {
            o.uuid(Uuid::from_u128(3)).varint(2).f32(0.5);
        });
        let bosses = st.hud(&Screen::default()).bosses;
        assert_eq!(bosses.iter().map(|b| (chat_text::plain(&b.name), b.progress, b.color, b.overlay)).collect::<Vec<_>>(), [("Wither".into(), 0.0, 5, 1), ("Event".into(), 1.0, 0, 4)]);
        feed(&mut st, c::BOSS_EVENT, |o| {
            o.uuid(a).varint(1);
        });
        assert_eq!(st.hud(&Screen::default()).bosses.len(), 1);
    }

    #[test]
    fn chat_keeps_the_latest_lines_oldest_first() {
        let mut st = joined("Dev");
        for n in 0..MAX_CHAT + 3 {
            st.chat(vec![chat_text::Run { text: n.to_string(), ..Default::default() }]);
        }
        let chat = st.hud(&Screen::default()).chat;
        assert_eq!(chat.len(), MAX_CHAT);
        assert_eq!((chat_text::plain(&chat[0].text), chat_text::plain(&chat[MAX_CHAT - 1].text)), ("3".into(), (MAX_CHAT + 2).to_string()));
    }

    #[test]
    fn stacks_show_their_damage_against_the_default_durability() {
        let item = |name: &str| i32::try_from(ITEMS.iter().position(|(n, ..)| *n == name).unwrap()).unwrap();
        let sword = item::Stack { item: item("diamond_sword"), count: 1, damage: 400, ..item::Stack::default() };
        assert_eq!(stack(&sword).map(|s| (s.id, s.damage, s.max_damage)), Some(("diamond_sword", 400, 1561)));
        let unbreakable = item::Stack { max_damage: Some(0), ..sword };
        assert_eq!(stack(&unbreakable).map(|s| (s.damage, s.max_damage)), Some((0, 0)));
        let dirt = item::Stack { item: item("dirt"), count: 64, ..item::Stack::default() };
        assert_eq!(stack(&dirt).map(|s| (s.count, s.max_damage)), Some((64, 0)));
    }
}