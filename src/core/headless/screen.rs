//! What the server shows the player, kept for the manager's Bildschirm: the map in hand, the open menu and the titles.
//! The player's own inventory is read only for the ids of the maps in it and the stacks in its hands. Pictures of those maps are kept while they are
//! carried, of any other map at most [`MAX_MAPS`] for a few seconds; nothing else of the world is kept.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};

use super::item::{self, Stack};
use super::text::{self, Line};
use super::v777::{ITEMS, MAP_COLORS, MENUS, play};
use super::wire::{In, Out};
use crate::core::ipc::{Click, FromMod, MapPicture, Menu, Slot, Titles, Words};

/// The side of a map picture in pixels.
pub const SIDE: usize = 128;
const PIXELS: usize = SIDE * SIDE;
const MAX_MAPS: usize = 4;
const MAX_SLOTS: usize = 256;
const INVENTORY: usize = 46;
const HOTBAR: usize = 36;
const OFFHAND: usize = 45;
const SHOWN_INVENTORY: usize = 36;
const PICKUP: i32 = 0;
const QUICK_MOVE: i32 = 1;
const AIR: i32 = 0;
const NEVER_HELD: i32 = -1;

struct Picture {
    id: i32,
    colors: Box<[u8]>,
    touched: Instant,
}

struct Open {
    id: i32,
    kind: i32,
    title: Line,
    slots: Vec<Option<Stack>>,
    state: i32,
    readable: bool,
}

/// One connection's screen.
#[derive(Default)]
pub struct Screen {
    held: usize,
    inventory: Vec<Option<i32>>,
    hands: [Option<Stack>; 10],
    pictures: Vec<Picture>,
    menu: Option<Open>,
    carried: Option<Stack>,
    titles: Titles,
    shown: Option<(i32, u64)>,
    map_dirty: bool,
    menu_dirty: bool,
    titles_dirty: bool,
}

impl Screen {
    /// Whether `id` is a packet this screen reads.
    pub fn reads(id: i32) -> bool {
        use play::clientbound as c;
        [c::OPEN_SCREEN, c::CONTAINER_SET_CONTENT, c::CONTAINER_SET_SLOT, c::CONTAINER_CLOSE, c::SET_CURSOR_ITEM, c::SET_PLAYER_INVENTORY, c::SET_HELD_SLOT, c::MAP_ITEM_DATA, c::SET_TITLE_TEXT, c::SET_SUBTITLE_TEXT, c::SET_ACTION_BAR_TEXT, c::CLEAR_TITLES].contains(&id)
    }

    /// Takes in packet `id`. A packet it cannot read leaves the rest as it was; an unreadable menu shows as such.
    pub fn read(&mut self, id: i32, r: &mut In) -> Result<()> {
        use play::clientbound as c;
        match id {
            c::OPEN_SCREEN => self.opened(r),
            c::CONTAINER_SET_CONTENT => self.content(r),
            c::CONTAINER_SET_SLOT => self.slot(r),
            c::CONTAINER_CLOSE => {
                let window = r.varint()?;
                if self.menu.as_ref().is_some_and(|m| m.id == window) {
                    self.close_local();
                }
                Ok(())
            }
            c::SET_CURSOR_ITEM => {
                self.carried = item::stack(r)?;
                self.menu_dirty |= self.menu.is_some();
                Ok(())
            }
            c::SET_PLAYER_INVENTORY => self.inventory_slot(r),
            c::SET_HELD_SLOT => {
                let slot = usize::try_from(r.varint()?).unwrap_or(0);
                ensure!(slot < 9, "held slot {slot}");
                self.held = slot;
                self.map_dirty = true;
                Ok(())
            }
            c::MAP_ITEM_DATA => self.map(r),
            c::SET_TITLE_TEXT => self.title(r, |t| &mut t.title),
            c::SET_SUBTITLE_TEXT => self.title(r, |t| &mut t.subtitle),
            c::SET_ACTION_BAR_TEXT => self.title(r, |t| &mut t.actionbar),
            c::CLEAR_TITLES => {
                if self.titles.title.take().is_some() | self.titles.subtitle.take().is_some() {
                    self.titles_dirty = true;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// What changed since the last call, as messages for the manager.
    pub fn changes(&mut self) -> Vec<FromMod> {
        let mut out = Vec::new();
        if std::mem::take(&mut self.map_dirty) {
            let mark = self.shown_picture().map(|p| (p.id, fingerprint(&p.colors)));
            if mark != self.shown {
                self.shown = mark;
                out.push(FromMod::Map { map: self.shown_picture().map(|p| MapPicture { id: p.id, colors: p.colors.to_vec() }) });
            }
        }
        if std::mem::take(&mut self.menu_dirty) {
            out.push(FromMod::Menu { menu: self.menu.as_ref().map(|m| menu_of(m, self.carried.as_ref())) });
        }
        if std::mem::take(&mut self.titles_dirty) {
            out.push(FromMod::Titles { titles: self.titles.clone() });
        }
        out
    }

    /// Forgets everything, for a new connection; the next [`Screen::changes`] reports the empty screen.
    pub fn reset(&mut self) {
        let had = (self.shown.is_some(), self.menu.is_some(), self.titles != Titles::default());
        *self = Self { shown: self.shown, map_dirty: had.0, menu_dirty: had.1, titles_dirty: had.2, ..Self::default() };
    }

    /// The click packet for the user's `click` on `slot` of the open menu `menu`, when that menu is open and has the slot.
    /// It claims no slots and a cursor stack no stack can match, so the server sends back every slot and the cursor the
    /// click changed instead of trusting a guess.
    pub fn click(&self, menu: i32, slot: i16, click: Click) -> Option<Out> {
        let open = self.menu.as_ref().filter(|m| m.id == menu)?;
        let index = usize::try_from(slot).ok().filter(|s| *s < open.slots.len())?;
        let (button, input) = match click {
            Click::Left => (0, PICKUP),
            Click::Right => (1, PICKUP),
            Click::Shift => (0, QUICK_MOVE),
        };
        let mut out = Out::default();
        out.varint(open.id).varint(open.state).i16(i16::try_from(index).ok()?).u8(button).varint(input).varint(0);
        out.bool(true).varint(AIR).varint(NEVER_HELD).varint(0).varint(0);
        Some(out)
    }

    /// The close packet for the open menu `menu`; the menu is gone at once.
    pub fn close(&mut self, menu: i32) -> Option<Out> {
        let id = self.menu.as_ref().filter(|m| m.id == menu)?.id;
        self.close_local();
        let mut out = Out::default();
        out.varint(id);
        Some(out)
    }

    fn close_local(&mut self) {
        self.menu = None;
        self.carried = None;
        self.menu_dirty = true;
    }

    fn opened(&mut self, r: &mut In) -> Result<()> {
        let id = r.varint()?;
        let kind = r.varint()?;
        let title = text::line(&r.nbt()?);
        self.menu = Some(Open { id, kind, title, slots: Vec::new(), state: 0, readable: true });
        self.carried = None;
        self.menu_dirty = true;
        Ok(())
    }

    fn content(&mut self, r: &mut In) -> Result<()> {
        let window = r.varint()?;
        let state = r.varint()?;
        if window == 0 {
            let stacks = item::stacks(r, INVENTORY)?;
            self.carried = item::stack(r)?;
            self.inventory = vec![None; INVENTORY];
            self.hands = Default::default();
            self.set_inventory(stacks.into_iter().enumerate());
            self.inventory_changed();
            return Ok(());
        }
        let Some(open) = self.menu.as_mut().filter(|m| m.id == window) else { return Ok(()) };
        open.state = state;
        self.menu_dirty = true;
        let read = item::stacks(r, MAX_SLOTS).and_then(|slots| Ok((slots, item::stack(r)?)));
        match read {
            Ok((slots, carried)) => {
                open.slots = slots;
                open.readable = true;
                self.carried = carried;
                let top = open.slots.len().saturating_sub(SHOWN_INVENTORY);
                let mine: Vec<(usize, Option<Stack>)> = open.slots[top..].iter().enumerate().map(|(i, s)| (inventory_index(i), s.clone())).collect();
                self.set_inventory(mine);
                Ok(())
            }
            Err(e) => {
                open.slots.clear();
                open.readable = false;
                Err(e)
            }
        }
    }

    fn slot(&mut self, r: &mut In) -> Result<()> {
        let window = r.varint()?;
        let state = r.varint()?;
        let slot = usize::try_from(r.i16()?).ok();
        let stack = item::stack(r)?;
        if window == 0 {
            if let Some(slot) = slot.filter(|s| *s < INVENTORY) {
                self.set_inventory([(slot, stack)]);
            }
            return Ok(());
        }
        let Some(open) = self.menu.as_mut().filter(|m| m.id == window) else { return Ok(()) };
        open.state = state;
        let Some(slot) = slot.filter(|s| *s < open.slots.len()) else { return Ok(()) };
        let mine = stack.clone();
        open.slots[slot] = stack;
        let top = open.slots.len().saturating_sub(SHOWN_INVENTORY);
        self.menu_dirty = true;
        if let Some(i) = slot.checked_sub(top) {
            self.set_inventory([(inventory_index(i), mine)]);
        }
        Ok(())
    }

    fn inventory_slot(&mut self, r: &mut In) -> Result<()> {
        let slot = r.varint()?;
        let stack = item::stack(r)?;
        let index = match usize::try_from(slot) {
            Ok(s @ 0..=8) => HOTBAR + s,
            Ok(s @ 9..=35) => s,
            Ok(40) => OFFHAND,
            _ => return Ok(()),
        };
        self.set_inventory([(index, stack)]);
        Ok(())
    }

    fn set_inventory(&mut self, slots: impl IntoIterator<Item = (usize, Option<Stack>)>) {
        self.inventory.resize(INVENTORY, None);
        let mut changed = false;
        for (index, stack) in slots {
            let map = stack.as_ref().and_then(|s| s.map);
            let hand = match index {
                HOTBAR..OFFHAND => Some(index - HOTBAR),
                OFFHAND => Some(9),
                _ => None,
            };
            if let Some(kept) = hand.and_then(|h| self.hands.get_mut(h)) {
                *kept = stack;
            }
            if let Some(cell) = self.inventory.get_mut(index)
                && *cell != map
            {
                *cell = map;
                changed = true;
            }
        }
        if changed {
            self.inventory_changed();
        }
    }

    fn inventory_changed(&mut self) {
        let carried: Vec<i32> = self.inventory.iter().flatten().copied().collect();
        let fresh = Instant::now();
        self.pictures.retain(|p| carried.contains(&p.id) || fresh.duration_since(p.touched).as_secs() < 5);
        self.map_dirty = true;
    }

    fn hand(&self) -> Option<i32> {
        let get = |i: usize| self.inventory.get(i).copied().flatten();
        get(HOTBAR + self.held).or_else(|| get(OFFHAND))
    }

    /// The stack in the player's main hand.
    pub fn held(&self) -> Option<&Stack> {
        self.hands.get(self.held)?.as_ref()
    }

    fn shown_picture(&self) -> Option<&Picture> {
        let id = self.hand()?;
        self.pictures.iter().find(|p| p.id == id)
    }

    fn map(&mut self, r: &mut In) -> Result<()> {
        let id = r.varint()?;
        let _scale = r.u8()?;
        let _locked = r.bool()?;
        if r.bool()? {
            for _ in 0..r.varint()? {
                let _kind = r.varint()?;
                r.take(3)?;
                let _name = r.optional_nbt()?;
            }
        }
        let width = usize::from(r.u8()?);
        if width == 0 {
            return Ok(());
        }
        let height = usize::from(r.u8()?);
        let (x0, y0) = (usize::from(r.u8()?), usize::from(r.u8()?));
        let colors = r.bytes()?.to_vec();
        ensure!(x0 + width <= SIDE && y0 + height <= SIDE && colors.len() == width * height, "map patch out of bounds");
        let carried: Vec<i32> = self.inventory.iter().flatten().copied().collect();
        self.pictures.retain(|p| p.id == id || carried.contains(&p.id) || p.touched.elapsed().as_secs() < 5);
        let at = match self.pictures.iter().position(|p| p.id == id) {
            Some(at) => at,
            None => {
                let loose = |p: &Picture| !carried.contains(&p.id);
                if !carried.contains(&id) && self.pictures.iter().filter(|p| loose(p)).count() >= MAX_MAPS {
                    let Some(oldest) = self.pictures.iter().enumerate().filter(|(_, p)| loose(p)).min_by_key(|(_, p)| p.touched).map(|(i, _)| i) else { return Ok(()) };
                    self.pictures.remove(oldest);
                }
                self.pictures.push(Picture { id, colors: vec![0; PIXELS].into_boxed_slice(), touched: Instant::now() });
                self.pictures.len() - 1
            }
        };
        let picture = &mut self.pictures[at];
        picture.touched = Instant::now();
        for y in 0..height {
            let row = (y0 + y) * SIDE + x0;
            picture.colors[row..row + width].copy_from_slice(&colors[y * width..(y + 1) * width]);
        }
        self.map_dirty |= self.hand() == Some(id);
        Ok(())
    }

    fn title(&mut self, r: &mut In, field: impl Fn(&mut Titles) -> &mut Option<Words>) -> Result<()> {
        let line = text::line(&r.nbt()?);
        let words = (!line.text.trim().is_empty()).then(|| words(&line));
        let slot = field(&mut self.titles);
        if slot.as_ref().map(|w| (&w.text, &w.runs)) != words.as_ref().map(|w| (&w.text, &w.runs)) {
            *slot = words;
            self.titles.at = now();
            self.titles_dirty = true;
        }
        Ok(())
    }
}

fn inventory_index(shown: usize) -> usize {
    if shown < 27 { 9 + shown } else { HOTBAR + shown - 27 }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn fingerprint(colors: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    colors.hash(&mut h);
    h.finish()
}

/// The picture of map colour ids `colors` as RGBA rows, top-down.
pub fn rgba(colors: &[u8]) -> Vec<u8> {
    colors.iter().flat_map(|c| MAP_COLORS[usize::from(*c)]).collect()
}

fn words(line: &Line) -> Words {
    Words { text: line.text.clone(), runs: line.runs.clone() }
}

fn menu_of(open: &Open, carried: Option<&Stack>) -> Menu {
    let kind = usize::try_from(open.kind).ok().and_then(|k| MENUS.get(k)).copied().unwrap_or_default();
    let top = open.slots.len().saturating_sub(SHOWN_INVENTORY);
    let columns = match kind {
        "generic_3x3" | "crafter_3x3" => 3,
        "hopper" => 5,
        _ => 9,
    };
    Menu {
        id: open.id,
        kind: kind.to_owned(),
        title: words(&open.title),
        columns,
        top: u16::try_from(top).unwrap_or(u16::MAX),
        slots: open.slots.iter().map(|s| s.as_ref().map(slot_of)).collect(),
        carried: carried.map(slot_of),
        readable: open.readable,
    }
}

fn slot_of(stack: &Stack) -> Slot {
    let known = usize::try_from(stack.item).ok().and_then(|i| ITEMS.get(i));
    let (id, english, rgb) = known.map_or(("unknown", "?", None), |(id, name, rgb)| (*id, *name, *rgb));
    let name = stack.name.as_ref().map_or_else(|| Words { text: english.to_owned(), runs: Vec::new() }, words);
    Slot { item: id.to_owned(), name, count: u32::try_from(stack.count).unwrap_or(0), lore: stack.lore.iter().map(words).collect(), glint: stack.glint, rgb, named: stack.name.is_some() }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::headless::v777::component;

    fn bytes(build: impl FnOnce(&mut Out)) -> Vec<u8> {
        let mut o = Out::default();
        build(&mut o);
        o.0
    }

    fn feed(s: &mut Screen, id: i32, build: impl FnOnce(&mut Out)) -> Result<()> {
        let body = bytes(build);
        s.read(id, &mut In::new(&body))
    }

    fn text(o: &mut Out, s: &str) {
        let len = u16::try_from(s.len()).unwrap();
        o.u8(8).u16(len).raw(s.as_bytes());
    }

    fn item(name: &str) -> i32 {
        i32::try_from(ITEMS.iter().position(|(id, ..)| *id == name).unwrap()).unwrap()
    }

    fn plain(o: &mut Out, id: i32) {
        o.varint(1).varint(id).varint(0).varint(0);
    }

    fn map(o: &mut Out, map: i32) {
        o.varint(1).varint(item("filled_map")).varint(1).varint(0).varint(component::MAP_ID).varint(map);
    }

    fn inventory(s: &mut Screen, hand: Option<i32>) {
        feed(s, play::clientbound::CONTAINER_SET_CONTENT, |o| {
            o.varint(0).varint(1).varint(46);
            for slot in 0..46 {
                match hand.filter(|_| slot == HOTBAR) {
                    Some(id) => map(o, id),
                    None => {
                        o.varint(0);
                    }
                }
            }
            o.varint(0);
        })
        .unwrap();
    }

    fn patch(s: &mut Screen, id: i32, x: u8, y: u8, colors: &[u8]) -> Result<()> {
        feed(s, play::clientbound::MAP_ITEM_DATA, |o| {
            o.varint(id).u8(0).bool(false).bool(false).u8(u8::try_from(colors.len()).unwrap()).u8(1).u8(x).u8(y).varint(i32::try_from(colors.len()).unwrap()).raw(colors);
        })
    }

    fn shown_map(changes: &[FromMod]) -> Option<Option<&MapPicture>> {
        changes.iter().find_map(|c| match c {
            FromMod::Map { map } => Some(map.as_ref()),
            _ => None,
        })
    }

    fn shown_menu(changes: &[FromMod]) -> Option<Option<&Menu>> {
        changes.iter().find_map(|c| match c {
            FromMod::Menu { menu } => Some(menu.as_ref()),
            _ => None,
        })
    }

    fn chest(s: &mut Screen) {
        feed(s, play::clientbound::OPEN_SCREEN, |o| {
            o.varint(3).varint(0);
            text(o, "Captcha");
        })
        .unwrap();
        feed(s, play::clientbound::CONTAINER_SET_CONTENT, |o| {
            o.varint(3).varint(5).varint(45);
            for slot in 0..45 {
                match slot {
                    0 => {
                        o.varint(2).varint(item("stone")).varint(1).varint(0).varint(component::CUSTOM_NAME);
                        text(o, "Click me");
                    }
                    36 => map(o, 9),
                    _ => {
                        o.varint(0);
                    }
                }
            }
            o.varint(0);
        })
        .unwrap();
    }

    #[test]
    fn map_colours_come_from_the_game() {
        assert_eq!(MAP_COLORS[50], [0x40, 0x40, 0xFF, 0xFF]);
        assert!(MAP_COLORS[..4].iter().all(|c| c[3] == 0));
        assert_eq!(rgba(&[50, 0]), vec![0x40, 0x40, 0xFF, 0xFF, 0, 0, 0, 0]);
    }

    #[test]
    fn a_map_in_hand_shows_with_its_patch_in_place() {
        let mut s = Screen::default();
        patch(&mut s, 7, 3, 4, &[50, 51]).unwrap();
        assert_eq!(shown_map(&s.changes()), None);
        inventory(&mut s, Some(7));
        let changes = s.changes();
        let picture = shown_map(&changes).flatten().unwrap();
        assert_eq!((picture.id, picture.colors.len()), (7, PIXELS));
        assert_eq!(&picture.colors[4 * SIDE + 3..4 * SIDE + 5], &[50, 51]);
        assert!(patch(&mut s, 7, 127, 0, &[1, 2]).is_err());
        assert_eq!(shown_map(&s.changes()), None);
    }

    #[test]
    fn switching_the_held_slot_or_the_offhand_moves_the_map() {
        let mut s = Screen::default();
        inventory(&mut s, Some(7));
        patch(&mut s, 7, 0, 0, &[50]).unwrap();
        assert!(shown_map(&s.changes()).flatten().is_some());
        feed(&mut s, play::clientbound::SET_HELD_SLOT, |o| {
            o.varint(1);
        })
        .unwrap();
        assert_eq!(shown_map(&s.changes()), Some(None));
        feed(&mut s, play::clientbound::SET_PLAYER_INVENTORY, |o| {
            o.varint(40);
            map(o, 7);
        })
        .unwrap();
        assert_eq!(shown_map(&s.changes()).flatten().map(|p| p.id), Some(7));
    }

    #[test]
    fn a_menu_keeps_its_slots_and_reads_the_inventory_below_it() {
        let mut s = Screen::default();
        chest(&mut s);
        let changes = s.changes();
        let menu = shown_menu(&changes).flatten().unwrap();
        assert_eq!((menu.id, menu.kind.as_str(), menu.columns, menu.top, menu.slots.len(), menu.readable), (3, "generic_9x1", 9, 9, 45, true));
        assert_eq!(menu.title.text, "Captcha");
        let first = menu.slots[0].as_ref().unwrap();
        assert_eq!((first.item.as_str(), first.name.text.as_str(), first.count, first.named), ("stone", "Click me", 2, true));
        assert_eq!(s.inventory[HOTBAR], Some(9));
        feed(&mut s, play::clientbound::CONTAINER_SET_SLOT, |o| {
            o.varint(3).varint(6).i16(0);
            o.varint(0);
        })
        .unwrap();
        let changes = s.changes();
        assert!(shown_menu(&changes).flatten().unwrap().slots[0].is_none());
        assert_eq!(s.click(3, 0, Click::Left).unwrap().0[1], 6);
    }

    #[test]
    fn clicks_encode_the_vanilla_container_click() {
        let mut s = Screen::default();
        chest(&mut s);
        let cursor = [1, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0, 0];
        assert_eq!(s.click(3, 0, Click::Left).unwrap().0, [&[3, 5, 0, 0, 0, 0, 0][..], &cursor].concat());
        assert_eq!(s.click(3, 4, Click::Right).unwrap().0, [&[3, 5, 0, 4, 1, 0, 0][..], &cursor].concat());
        assert_eq!(s.click(3, 44, Click::Shift).unwrap().0, [&[3, 5, 0, 44, 0, 1, 0][..], &cursor].concat());
        assert!(s.click(3, 45, Click::Left).is_none());
        assert!(s.click(3, -999, Click::Left).is_none());
        assert!(s.click(4, 0, Click::Left).is_none());
    }

    #[test]
    fn closing_sends_the_menu_and_forgets_it() {
        let mut s = Screen::default();
        chest(&mut s);
        s.changes();
        assert!(s.close(4).is_none());
        assert_eq!(s.close(3).unwrap().0, [3]);
        assert_eq!(shown_menu(&s.changes()), Some(None));
        assert!(s.click(3, 0, Click::Left).is_none());
    }

    #[test]
    fn an_unreadable_menu_stays_open_without_slots() {
        let mut s = Screen::default();
        feed(&mut s, play::clientbound::OPEN_SCREEN, |o| {
            o.varint(3).varint(0);
            text(o, "Odd");
        })
        .unwrap();
        let broken = feed(&mut s, play::clientbound::CONTAINER_SET_CONTENT, |o| {
            o.varint(3).varint(1).varint(1).varint(1).varint(item("stone")).varint(1).varint(0).varint(999);
        });
        assert!(broken.is_err());
        let changes = s.changes();
        let menu = shown_menu(&changes).flatten().unwrap();
        assert!(!menu.readable && menu.slots.is_empty());
        assert!(s.close(3).is_some());
    }

    #[test]
    fn a_menu_claiming_more_slots_than_any_menu_has_is_unreadable() {
        let mut s = Screen::default();
        chest(&mut s);
        s.changes();
        let err = feed(&mut s, play::clientbound::CONTAINER_SET_CONTENT, |o| {
            o.varint(3).varint(6).varint(100_000);
            o.raw(&[0; 100_000]);
            o.varint(0);
        });
        assert!(err.is_err());
        assert!(matches!(shown_menu(&s.changes()), Some(Some(m)) if !m.readable && m.slots.is_empty()));
    }

    #[test]
    fn titles_stay_until_cleared_and_the_actionbar_survives_a_clear() {
        let mut s = Screen::default();
        feed(&mut s, play::clientbound::SET_TITLE_TEXT, |o| text(o, "Solve")).unwrap();
        feed(&mut s, play::clientbound::SET_ACTION_BAR_TEXT, |o| text(o, "/captcha <code>")).unwrap();
        let changes = s.changes();
        let titles = changes.iter().find_map(|c| if let FromMod::Titles { titles } = c { Some(titles.clone()) } else { None }).unwrap();
        assert_eq!(titles.title.unwrap().text, "Solve");
        assert_eq!(titles.actionbar.unwrap().text, "/captcha <code>");
        feed(&mut s, play::clientbound::SET_TITLE_TEXT, |o| text(o, "Solve")).unwrap();
        assert!(s.changes().is_empty());
        feed(&mut s, play::clientbound::CLEAR_TITLES, |o| {
            o.bool(false);
        })
        .unwrap();
        assert!(s.titles.title.is_none() && s.titles.actionbar.is_some());
    }

    #[test]
    fn a_reset_reports_everything_gone() {
        let mut s = Screen::default();
        chest(&mut s);
        patch(&mut s, 9, 0, 0, &[50]).unwrap();
        assert!(shown_map(&s.changes()).flatten().is_some());
        s.reset();
        let changes = s.changes();
        assert_eq!(shown_map(&changes), Some(None));
        assert_eq!(shown_menu(&changes), Some(None));
        s.reset();
        assert!(s.changes().is_empty());
    }

    #[test]
    fn carried_pictures_stay_and_at_most_four_others_are_kept() {
        let mut s = Screen::default();
        inventory(&mut s, Some(1));
        for id in 1..=6 {
            patch(&mut s, id, 0, 0, &[50]).unwrap();
        }
        assert_eq!(s.pictures.len(), MAX_MAPS + 1);
        assert!(s.pictures.iter().any(|p| p.id == 1));
    }
}