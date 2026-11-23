//! Item stacks as the server sends them. Clientbound data components carry no length, so every component type the game
//! synchronizes is read to its last byte, and only what a player sees of the stack is kept.

use anyhow::{Result, bail, ensure};

use super::text::{self, Line};
use super::v777::{component, consume_effect};
use super::wire::In;

const MAX_DEPTH: usize = 8;
const MAX_LORE: usize = 256;

/// What the player sees of an item stack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stack {
    pub item: i32,
    pub count: i32,
    pub name: Option<Line>,
    pub lore: Vec<Line>,
    pub glint: bool,
    pub map: Option<i32>,
}

#[derive(Default)]
struct View {
    name: Option<Line>,
    item_name: Option<Line>,
    lore: Vec<Line>,
    enchanted: bool,
    glint: Option<bool>,
    map: Option<i32>,
}

/// Reads an optional item stack; `None` for an empty slot.
pub fn stack(r: &mut In) -> Result<Option<Stack>> {
    let count = r.varint()?;
    if count <= 0 {
        return Ok(None);
    }
    let item = r.varint()?;
    let view = patch(r, 0)?;
    Ok(Some(Stack { item, count, name: view.name.or(view.item_name), lore: view.lore, glint: view.glint.unwrap_or(view.enchanted), map: view.map }))
}

/// Reads a list of optional item stacks (VarInt length), at most `max` of them.
pub fn stacks(r: &mut In, max: usize) -> Result<Vec<Option<Stack>>> {
    let n = r.list()?;
    ensure!(n <= max, "{n} item stacks, more than {max}");
    (0..n).map(|_| stack(r)).collect()
}

fn nest(depth: usize) -> Result<usize> {
    ensure!(depth < MAX_DEPTH, "items nested deeper than {MAX_DEPTH}");
    Ok(depth + 1)
}

fn patch(r: &mut In, depth: usize) -> Result<View> {
    let added = r.list()?;
    let removed = r.list()?;
    let mut view = View::default();
    for _ in 0..added {
        let id = r.varint()?;
        read(r, id, depth, &mut view)?;
    }
    for _ in 0..removed {
        r.varint()?;
    }
    Ok(view)
}

fn template(r: &mut In, depth: usize) -> Result<()> {
    let depth = nest(depth)?;
    r.varint()?;
    r.varint()?;
    patch(r, depth).map(drop)
}

fn read(r: &mut In, id: i32, depth: usize, view: &mut View) -> Result<()> {
    match id {
        component::CUSTOM_NAME => view.name = Some(text::line(&r.nbt()?)),
        component::ITEM_NAME => view.item_name = Some(text::line(&r.nbt()?)),
        component::LORE => view.lore = lore(r)?,
        component::ENCHANTMENTS => view.enchanted = enchantments(r)? > 0,
        component::ENCHANTMENT_GLINT_OVERRIDE => view.glint = Some(r.bool()?),
        component::MAP_ID => view.map = Some(r.varint()?),
        _ => {
            if !plain(r, id)? {
                structured(r, id, depth)?;
            }
        }
    }
    Ok(())
}

fn plain(r: &mut In, id: i32) -> Result<bool> {
    match id {
        component::CUSTOM_DATA
        | component::INTANGIBLE_PROJECTILE
        | component::MAP_DECORATIONS
        | component::DEBUG_STICK_STATE
        | component::RECIPES
        | component::LOCK
        | component::CONTAINER_LOOT
        | component::BUCKET_ENTITY_DATA => nbt(r)?,
        component::UNBREAKABLE | component::CREATIVE_SLOT_LOCK | component::GLIDER | component::WAXED => {}
        component::MAX_STACK_SIZE
        | component::MAX_DAMAGE
        | component::DAMAGE
        | component::REPAIR_COST
        | component::ADDITIONAL_TRADE_COST
        | component::VILLAGER_FOOD
        | component::ENCHANTABLE
        | component::OMINOUS_BOTTLE_AMPLIFIER
        | component::RARITY
        | component::MAP_POST_PROCESSING
        | component::DAMAGE_TYPE
        | component::BLOCK_TRANSFORMER
        | component::PROVIDES_POTTERY_PATTERN
        | component::DYE
        | component::BASE_COLOR
        | component::WOLF_COLLAR
        | component::CAT_COLLAR
        | component::SHEEP_COLOR
        | component::SHULKER_COLOR
        | component::CUSHION_COLOR
        | component::TROPICAL_FISH_BASE_COLOR
        | component::TROPICAL_FISH_PATTERN_COLOR
        | component::VILLAGER_VARIANT
        | component::WOLF_VARIANT
        | component::WOLF_SOUND_VARIANT
        | component::FOX_VARIANT
        | component::SALMON_SIZE
        | component::PARROT_VARIANT
        | component::TROPICAL_FISH_PATTERN
        | component::MOOSHROOM_VARIANT
        | component::RABBIT_VARIANT
        | component::PIG_VARIANT
        | component::PIG_SOUND_VARIANT
        | component::COW_VARIANT
        | component::COW_SOUND_VARIANT
        | component::CHICKEN_VARIANT
        | component::CHICKEN_SOUND_VARIANT
        | component::ZOMBIE_NAUTILUS_VARIANT
        | component::FROG_VARIANT
        | component::HORSE_VARIANT
        | component::LLAMA_VARIANT
        | component::AXOLOTL_VARIANT
        | component::CAT_VARIANT
        | component::CAT_SOUND_VARIANT => varint(r)?,
        component::MINIMUM_ATTACK_CHARGE | component::POTION_DURATION_SCALE => float(r)?,
        component::DYED_COLOR => int(r)?,
        component::ITEM_MODEL | component::TOOLTIP_STYLE | component::NOTE_BLOCK_SOUND => string(r)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn structured(r: &mut In, id: i32, depth: usize) -> Result<()> {
    match id {
        component::USE_EFFECTS => {
            flag(r)?;
            flag(r)?;
            float(r)
        }
        component::STORED_ENCHANTMENTS => enchantments(r).map(drop),
        component::CAN_PLACE_ON | component::CAN_BREAK => each(r, |r| block_predicate(r, depth)),
        component::ATTRIBUTE_MODIFIERS => each(r, attribute_modifier),
        component::CUSTOM_MODEL_DATA => {
            each(r, float)?;
            each(r, flag)?;
            each(r, string)?;
            each(r, int)
        }
        component::TOOLTIP_DISPLAY => {
            flag(r)?;
            each(r, varint)
        }
        component::USE_REMAINDER | component::SULFUR_CUBE_CONTENT => template(r, depth),
        component::CHARGED_PROJECTILES | component::BUNDLE_CONTENTS => each(r, |r| template(r, depth)),
        component::CONTAINER => each(r, |r| opt(r, |r| template(r, depth))),
        component::POT_DECORATIONS => {
            for _ in 0..4 {
                opt(r, |r| template(r, depth))?;
            }
            Ok(())
        }
        component::POTION_CONTENTS => {
            opt(r, varint)?;
            opt(r, int)?;
            each(r, |r| mob_effect_instance(r, depth))?;
            opt(r, string)
        }
        component::SUSPICIOUS_STEW_EFFECTS => each(r, |r| {
            varint(r)?;
            varint(r)
        }),
        component::BEES => each(r, |r| {
            typed_entity_data(r)?;
            varint(r)?;
            varint(r)
        }),
        component::ENTITY_DATA | component::BLOCK_ENTITY_DATA => typed_entity_data(r),
        _ => equipment(r, id),
    }
}

fn equipment(r: &mut In, id: i32) -> Result<()> {
    match id {
        component::FOOD => {
            varint(r)?;
            float(r)?;
            flag(r)
        }
        component::CONSUMABLE => {
            float(r)?;
            varint(r)?;
            sound_event(r)?;
            flag(r)?;
            each(r, consume_effect)
        }
        component::DEATH_PROTECTION => each(r, consume_effect),
        component::USE_COOLDOWN => {
            float(r)?;
            opt(r, string)
        }
        component::DAMAGE_RESISTANT | component::REPAIRABLE | component::PROVIDES_BANNER_PATTERNS => holder_set(r),
        component::TOOL => {
            each(r, |r| {
                holder_set(r)?;
                opt(r, float)?;
                opt(r, flag)
            })?;
            float(r)?;
            varint(r)?;
            flag(r)
        }
        component::WEAPON => {
            varint(r)?;
            float(r)
        }
        component::ATTACK_RANGE => floats(r, 6),
        component::EQUIPPABLE => {
            varint(r)?;
            sound_event(r)?;
            opt(r, string)?;
            opt(r, string)?;
            opt(r, holder_set)?;
            for _ in 0..5 {
                flag(r)?;
            }
            sound_event(r)
        }
        component::BLOCKS_ATTACKS => blocks_attacks(r),
        component::PIERCING_WEAPON => {
            flag(r)?;
            flag(r)?;
            opt(r, sound_event)?;
            opt(r, sound_event)
        }
        component::KINETIC_WEAPON => kinetic_weapon(r),
        component::ATTACK_ANIMATION | component::INTERACT_ANIMATION => {
            varint(r)?;
            varint(r)
        }
        component::COMPOSTABLE => resolvable_int(r),
        component::COOKING_FUEL | component::BREWING_FUEL => {
            resolvable_int(r)?;
            resolvable_float(r)
        }
        component::MOB_VISIBILITY => {
            holder_set(r)?;
            float(r)
        }
        component::BREAK_SOUND => sound_event(r),
        _ => content(r, id),
    }
}

fn content(r: &mut In, id: i32) -> Result<()> {
    match id {
        component::WRITABLE_BOOK_CONTENT => each(r, |r| filterable(r, string)),
        component::WRITTEN_BOOK_CONTENT => {
            filterable(r, string)?;
            string(r)?;
            varint(r)?;
            each(r, |r| filterable(r, nbt))?;
            flag(r)
        }
        component::TRIM => {
            holder(r, trim_material)?;
            holder(r, trim_pattern)
        }
        component::PROVIDES_TRIM_MATERIAL => holder(r, trim_material),
        component::INSTRUMENT => holder(r, |r| {
            sound_event(r)?;
            float(r)?;
            float(r)?;
            varint(r)?;
            nbt(r)
        }),
        component::JUKEBOX_PLAYABLE => holder(r, |r| {
            sound_event(r)?;
            nbt(r)?;
            float(r)?;
            varint(r)
        }),
        component::PAINTING_VARIANT => holder(r, |r| {
            varint(r)?;
            varint(r)?;
            string(r)?;
            opt(r, nbt)?;
            opt(r, nbt)
        }),
        component::LODESTONE_TRACKER => {
            opt(r, |r| {
                string(r)?;
                long(r)
            })?;
            flag(r)
        }
        component::FIREWORK_EXPLOSION => firework_explosion(r),
        component::FIREWORKS => {
            varint(r)?;
            each(r, firework_explosion)
        }
        component::PROFILE => profile(r),
        component::BANNER_PATTERNS => each(r, |r| {
            holder(r, |r| {
                string(r)?;
                string(r)
            })?;
            varint(r)
        }),
        component::BLOCK_STATE => each(r, |r| {
            string(r)?;
            string(r)
        }),
        component::SIGN_TEXT_FRONT | component::SIGN_TEXT_BACK => {
            sign_lines(r)?;
            opt(r, sign_lines)?;
            varint(r)?;
            flag(r)
        }
        other => bail!("unknown data component type {other}"),
    }
}

fn varint(r: &mut In) -> Result<()> {
    r.varint().map(drop)
}

fn string(r: &mut In) -> Result<()> {
    r.string().map(drop)
}

fn nbt(r: &mut In) -> Result<()> {
    r.nbt().map(drop)
}

fn float(r: &mut In) -> Result<()> {
    r.f32().map(drop)
}

fn int(r: &mut In) -> Result<()> {
    r.i32().map(drop)
}

fn long(r: &mut In) -> Result<()> {
    r.i64().map(drop)
}

fn flag(r: &mut In) -> Result<()> {
    r.bool().map(drop)
}

fn floats(r: &mut In, n: usize) -> Result<()> {
    for _ in 0..n {
        float(r)?;
    }
    Ok(())
}

fn each(r: &mut In, mut one: impl FnMut(&mut In) -> Result<()>) -> Result<()> {
    for _ in 0..r.list()? {
        one(r)?;
    }
    Ok(())
}

fn opt(r: &mut In, one: impl FnOnce(&mut In) -> Result<()>) -> Result<()> {
    if r.bool()? { one(r) } else { Ok(()) }
}

fn either(r: &mut In, left: impl FnOnce(&mut In) -> Result<()>, right: impl FnOnce(&mut In) -> Result<()>) -> Result<()> {
    if r.bool()? { left(r) } else { right(r) }
}

fn holder(r: &mut In, direct: impl FnOnce(&mut In) -> Result<()>) -> Result<()> {
    if r.varint()? == 0 { direct(r) } else { Ok(()) }
}

fn holder_set(r: &mut In) -> Result<()> {
    match r.varint()? {
        0 => string(r),
        n if n > 0 => {
            let elements = usize::try_from(n - 1)?;
            ensure!(elements <= r.left(), "holder set of {elements} past the packet");
            for _ in 0..elements {
                varint(r)?;
            }
            Ok(())
        }
        n => bail!("negative holder set size {n}"),
    }
}

fn sound_event(r: &mut In) -> Result<()> {
    holder(r, |r| {
        string(r)?;
        opt(r, float)
    })
}

fn filterable(r: &mut In, value: fn(&mut In) -> Result<()>) -> Result<()> {
    value(r)?;
    opt(r, value)
}

fn lore(r: &mut In) -> Result<Vec<Line>> {
    let n = r.list()?;
    ensure!(n <= MAX_LORE, "{n} lore lines");
    (0..n).map(|_| Ok(text::line(&r.nbt()?))).collect()
}

fn enchantments(r: &mut In) -> Result<usize> {
    let n = r.list()?;
    for _ in 0..n {
        varint(r)?;
        varint(r)?;
    }
    Ok(n)
}

fn block_predicate(r: &mut In, depth: usize) -> Result<()> {
    opt(r, holder_set)?;
    opt(r, state_properties)?;
    opt(r, nbt)?;
    each(r, |r| {
        let depth = nest(depth)?;
        let id = r.varint()?;
        read(r, id, depth, &mut View::default())
    })?;
    each(r, |r| {
        flag(r)?;
        varint(r)?;
        nbt(r)
    })
}

fn state_properties(r: &mut In) -> Result<()> {
    each(r, |r| {
        string(r)?;
        either(r, string, |r| {
            opt(r, string)?;
            opt(r, string)
        })
    })
}

fn attribute_modifier(r: &mut In) -> Result<()> {
    varint(r)?;
    string(r)?;
    r.f64()?;
    varint(r)?;
    varint(r)?;
    if r.varint()? == 2 {
        nbt(r)?;
    }
    Ok(())
}

fn mob_effect_instance(r: &mut In, depth: usize) -> Result<()> {
    varint(r)?;
    effect_details(r, depth)
}

fn effect_details(r: &mut In, depth: usize) -> Result<()> {
    varint(r)?;
    varint(r)?;
    for _ in 0..3 {
        flag(r)?;
    }
    if r.bool()? {
        effect_details(r, nest(depth)?)?;
    }
    Ok(())
}

fn consume_effect(r: &mut In) -> Result<()> {
    match r.varint()? {
        consume_effect::APPLY_EFFECTS => {
            each(r, |r| mob_effect_instance(r, 0))?;
            float(r)
        }
        consume_effect::REMOVE_EFFECTS => holder_set(r),
        consume_effect::CLEAR_ALL_EFFECTS => Ok(()),
        consume_effect::TELEPORT_RANDOMLY => {
            float(r)?;
            flag(r)
        }
        consume_effect::PLAY_SOUND => sound_event(r),
        id => bail!("unknown consume effect type {id}"),
    }
}

fn typed_entity_data(r: &mut In) -> Result<()> {
    varint(r)?;
    nbt(r)
}

fn blocks_attacks(r: &mut In) -> Result<()> {
    floats(r, 2)?;
    each(r, |r| {
        float(r)?;
        opt(r, holder_set)?;
        floats(r, 2)
    })?;
    floats(r, 3)?;
    opt(r, holder_set)?;
    opt(r, sound_event)?;
    opt(r, sound_event)
}

fn kinetic_weapon(r: &mut In) -> Result<()> {
    fn condition(r: &mut In) -> Result<()> {
        varint(r)?;
        floats(r, 2)
    }
    varint(r)?;
    varint(r)?;
    for _ in 0..3 {
        opt(r, condition)?;
    }
    floats(r, 2)?;
    opt(r, sound_event)?;
    opt(r, sound_event)
}

fn trim_material(r: &mut In) -> Result<()> {
    string(r)?;
    nbt(r)
}

fn trim_pattern(r: &mut In) -> Result<()> {
    string(r)?;
    nbt(r)?;
    flag(r)
}

fn firework_explosion(r: &mut In) -> Result<()> {
    varint(r)?;
    each(r, int)?;
    each(r, int)?;
    flag(r)?;
    flag(r)
}

fn profile(r: &mut In) -> Result<()> {
    either(
        r,
        |r| {
            r.uuid()?;
            string(r)?;
            profile_properties(r)
        },
        |r| {
            opt(r, string)?;
            opt(r, |r| r.uuid().map(drop))?;
            profile_properties(r)
        },
    )?;
    for _ in 0..3 {
        opt(r, string)?;
    }
    opt(r, flag)
}

fn profile_properties(r: &mut In) -> Result<()> {
    each(r, |r| {
        string(r)?;
        string(r)?;
        opt(r, string)
    })
}

fn resolvable_int(r: &mut In) -> Result<()> {
    either(r, int, string)
}

fn resolvable_float(r: &mut In) -> Result<()> {
    either(r, float, string)
}

fn sign_lines(r: &mut In) -> Result<()> {
    for _ in 0..4 {
        nbt(r)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;
    use crate::core::headless::wire::Out;

    type Comp = (i32, Vec<u8>);

    fn nbt_text(o: &mut Out, s: &str) {
        o.u8(8).u16(u16::try_from(s.len()).unwrap()).raw(s.as_bytes());
    }

    fn nbt_empty(o: &mut Out) {
        o.u8(10).u8(0);
    }

    fn line(s: &str) -> Line {
        text::line(&Value::String(s.into()))
    }

    fn comp(id: i32, f: impl FnOnce(&mut Out)) -> Comp {
        let mut o = Out::default();
        f(&mut o);
        (id, o.0)
    }

    fn patch_bytes(o: &mut Out, comps: &[Comp], removed: &[i32]) {
        o.varint(comps.len() as i32).varint(removed.len() as i32);
        for (id, body) in comps {
            o.varint(*id).raw(body);
        }
        for id in removed {
            o.varint(*id);
        }
    }

    fn item(count: i32, id: i32, comps: &[Comp], removed: &[i32]) -> Vec<u8> {
        let mut o = Out::default();
        o.varint(count).varint(id);
        patch_bytes(&mut o, comps, removed);
        o.0
    }

    fn template(id: i32, count: i32, comps: &[Comp]) -> Vec<u8> {
        let mut o = Out::default();
        o.varint(id).varint(count);
        patch_bytes(&mut o, comps, &[]);
        o.0
    }

    fn decode(bytes: &[u8]) -> Stack {
        let mut r = In::new(bytes);
        let s = stack(&mut r).unwrap().unwrap();
        assert_eq!(r.left(), 0);
        s
    }

    fn aligned(id: i32, f: impl FnOnce(&mut Out)) {
        let end = comp(component::CUSTOM_NAME, |o| nbt_text(o, "end"));
        let s = decode(&item(1, 1, &[comp(id, f), end], &[]));
        assert_eq!(s.name, Some(line("end")), "component {id}");
    }

    fn failure(comps: &[Comp]) -> String {
        format!("{:#}", stack(&mut In::new(&item(1, 1, comps, &[]))).unwrap_err())
    }

    fn sound_ref(o: &mut Out) {
        o.varint(7);
    }

    fn sound_direct(o: &mut Out) {
        o.varint(0).string("minecraft:x").bool(true).f32(16.0);
    }

    fn bundle(inner: &[u8]) -> Comp {
        comp(component::BUNDLE_CONTENTS, |o| {
            o.varint(1).raw(inner);
        })
    }

    fn nested(levels: usize) -> Vec<u8> {
        let mut inner = template(1, 1, &[]);
        for _ in 1..levels {
            inner = template(1, 1, &[bundle(&inner)]);
        }
        item(1, 1, &[bundle(&inner)], &[])
    }

    fn on_connection_stack(bytes: Vec<u8>) -> Result<Option<Stack>> {
        std::thread::Builder::new().stack_size(512 << 10).spawn(move || stack(&mut In::new(&bytes))).unwrap().join().unwrap()
    }

    #[test]
    fn an_empty_slot_has_no_stack() {
        for count in [0, -1] {
            let mut o = Out::default();
            o.varint(count);
            let mut r = In::new(&o.0);
            assert_eq!(stack(&mut r).unwrap(), None);
            assert_eq!(r.left(), 0);
        }
    }

    #[test]
    fn a_plain_stack_has_no_extras() {
        assert_eq!(decode(&item(5, 3, &[], &[])), Stack { item: 3, count: 5, ..Stack::default() });
    }

    #[test]
    fn a_list_of_stacks_reads_slot_by_slot() {
        let mut o = Out::default();
        o.varint(3).raw(&item(2, 4, &[], &[])).varint(0).raw(&item(1, 5, &[], &[]));
        let mut r = In::new(&o.0);
        let slots = stacks(&mut r, 8).unwrap();
        assert_eq!(slots.iter().map(|s| s.as_ref().map(|s| s.item)).collect::<Vec<_>>(), [Some(4), None, Some(5)]);
        assert_eq!(r.left(), 0);
    }

    #[test]
    fn name_lore_enchantments_and_map_come_through() {
        let comps = [
            comp(component::CUSTOM_NAME, |o| nbt_text(o, "Hi")),
            comp(component::LORE, |o| {
                o.varint(2);
                nbt_text(o, "a");
                nbt_text(o, "b");
            }),
            comp(component::ENCHANTMENTS, |o| {
                o.varint(1).varint(7).varint(3);
            }),
            comp(component::MAP_ID, |o| {
                o.varint(12);
            }),
        ];
        let s = decode(&item(2, 9, &comps, &[]));
        assert_eq!(s, Stack { item: 9, count: 2, name: Some(line("Hi")), lore: vec![line("a"), line("b")], glint: true, map: Some(12) });
    }

    #[test]
    fn the_glint_override_beats_the_enchantments() {
        let comps = [
            comp(component::ENCHANTMENTS, |o| {
                o.varint(1).varint(7).varint(3);
            }),
            comp(component::ENCHANTMENT_GLINT_OVERRIDE, |o| {
                o.bool(false);
            }),
        ];
        assert!(!decode(&item(1, 1, &comps, &[])).glint);
        let on = [comp(component::ENCHANTMENT_GLINT_OVERRIDE, |o| {
            o.bool(true);
        })];
        assert!(decode(&item(1, 1, &on, &[])).glint);
    }

    #[test]
    fn removed_components_are_only_ids_and_the_item_name_is_the_fallback() {
        let comps = [comp(component::ITEM_NAME, |o| nbt_text(o, "base"))];
        let s = decode(&item(1, 1, &comps, &[component::CUSTOM_NAME, component::ENCHANTMENTS]));
        assert_eq!(s.name, Some(line("base")));
        let both = [comp(component::ITEM_NAME, |o| nbt_text(o, "base")), comp(component::CUSTOM_NAME, |o| nbt_text(o, "mine"))];
        assert_eq!(decode(&item(1, 1, &both, &[])).name, Some(line("mine")));
    }

    #[test]
    fn a_container_keeps_the_outer_stack_aligned() {
        let inner = template(11, 3, &[comp(component::CUSTOM_NAME, |o| nbt_text(o, "inner"))]);
        let container = comp(component::CONTAINER, |o| {
            o.varint(3).bool(false).bool(true).raw(&inner).bool(false);
        });
        let outer = comp(component::CUSTOM_NAME, |o| nbt_text(o, "outer"));
        assert_eq!(decode(&item(1, 1, &[container, outer], &[])).name, Some(line("outer")));
    }

    #[test]
    fn nesting_stops_past_eight_levels_without_overflowing_a_connection_stack() {
        assert!(on_connection_stack(nested(8)).unwrap().is_some());
        let err = on_connection_stack(nested(9)).unwrap_err();
        assert!(format!("{err:#}").contains("nested deeper than 8"), "{err:#}");
    }

    #[test]
    fn unknown_ids_and_hostile_lengths_fail() {
        let past = component::CUSHION_COLOR + 1;
        assert!(failure(&[comp(past, |_| {})]).contains(&past.to_string()));
        assert!(failure(&[comp(-1, |_| {})]).contains("-1"));
        let huge = comp(component::CONTAINER, |o| {
            o.varint(1_000_000);
        });
        assert!(failure(&[huge]).contains("past the packet"));
        let negative = comp(component::BUNDLE_CONTENTS, |o| {
            o.varint(-3);
        });
        assert!(failure(&[negative]).contains("negative"));
        let mut o = Out::default();
        o.varint(1).varint(1).varint(5_000_000).varint(0);
        assert!(stack(&mut In::new(&o.0)).is_err());
    }

    #[test]
    fn components_without_a_sync_codec_travel_as_nbt() {
        for id in [
            component::CUSTOM_DATA,
            component::INTANGIBLE_PROJECTILE,
            component::MAP_DECORATIONS,
            component::DEBUG_STICK_STATE,
            component::RECIPES,
            component::LOCK,
            component::CONTAINER_LOOT,
            component::BUCKET_ENTITY_DATA,
        ] {
            aligned(id, nbt_empty);
        }
    }

    #[test]
    fn a_consumable_reads_with_inline_and_registered_sounds() {
        aligned(component::CONSUMABLE, |o| {
            o.f32(1.6).varint(1);
            sound_direct(o);
            o.bool(true).varint(0);
        });
        aligned(component::CONSUMABLE, |o| {
            o.f32(1.6).varint(1);
            sound_ref(o);
            o.bool(false).varint(0);
        });
    }

    #[test]
    fn consume_effects_read_every_type() {
        aligned(component::CONSUMABLE, |o| {
            o.f32(1.6).varint(1);
            sound_ref(o);
            o.bool(true).varint(5);
            o.varint(0).varint(1).varint(9).varint(1).varint(600).bool(false).bool(true).bool(true).bool(false).f32(0.5);
            o.varint(1).varint(2).varint(3);
            o.varint(2);
            o.varint(3).f32(16.).bool(true);
            o.varint(4);
            sound_ref(o);
        });
        aligned(component::DEATH_PROTECTION, |o| {
            o.varint(1).varint(2);
        });
        let unknown = comp(component::DEATH_PROTECTION, |o| {
            o.varint(1).varint(9);
        });
        assert!(failure(&[unknown]).contains("consume effect type 9"));
    }

    #[test]
    fn potion_contents_read_effects_with_hidden_effects() {
        aligned(component::POTION_CONTENTS, |o| {
            o.bool(true).varint(4).bool(true).i32(0x0011_2233).varint(3);
            o.varint(9).varint(1).varint(600).bool(false).bool(true).bool(true).bool(false);
            o.varint(3).varint(0).varint(20).bool(true).bool(false).bool(true).bool(true);
            o.varint(2).varint(40).bool(false).bool(false).bool(false).bool(false);
            o.varint(5).varint(0).varint(1).bool(false).bool(false).bool(false).bool(false);
            o.bool(true).string("brew");
        });
        aligned(component::POTION_CONTENTS, |o| {
            o.bool(false).bool(false).varint(0).bool(false);
        });
    }

    #[test]
    fn attribute_modifiers_read_every_display() {
        aligned(component::ATTRIBUTE_MODIFIERS, |o| {
            o.varint(3);
            o.varint(5).string("minecraft:base").f64(1.5).varint(0).varint(2).varint(0);
            o.varint(6).string("minecraft:other").f64(-0.25).varint(2).varint(1).varint(1);
            o.varint(7).string("minecraft:third").f64(4.0).varint(1).varint(0).varint(2);
            nbt_text(o, "shown");
        });
    }

    #[test]
    fn a_tool_reads_named_and_listed_block_sets() {
        aligned(component::TOOL, |o| {
            o.varint(2);
            o.varint(3).varint(5).varint(6).bool(true).f32(2.0).bool(true).bool(true);
            o.varint(0).string("minecraft:mineable/pickaxe").bool(false).bool(false);
            o.f32(1.0).varint(1).bool(true);
        });
    }

    #[test]
    fn equippable_reads_its_optional_parts() {
        aligned(component::EQUIPPABLE, |o| {
            o.varint(2);
            sound_ref(o);
            o.bool(true).string("minecraft:diamond").bool(false);
            o.bool(true).varint(3).varint(2).varint(4);
            o.bool(true).bool(false).bool(true).bool(false).bool(true);
            sound_direct(o);
        });
        aligned(component::EQUIPPABLE, |o| {
            o.varint(0);
            sound_ref(o);
            o.bool(false).bool(true).string("minecraft:overlay").bool(false);
            o.bool(false).bool(false).bool(false).bool(false).bool(false);
            sound_ref(o);
        });
    }

    #[test]
    fn a_profile_reads_a_full_or_a_partial_profile_with_properties() {
        aligned(component::PROFILE, |o| {
            o.bool(true).uuid(Uuid::nil()).string("Name").varint(2);
            o.string("textures").string("value").bool(true).string("sig");
            o.string("other").string("v").bool(false);
            o.bool(true).string("minecraft:body").bool(false).bool(false).bool(true).bool(true);
        });
        aligned(component::PROFILE, |o| {
            o.bool(false).bool(true).string("Name").bool(false).varint(0);
            o.bool(false).bool(false).bool(false).bool(false);
        });
    }

    #[test]
    fn fireworks_read_their_explosions() {
        aligned(component::FIREWORKS, |o| {
            o.varint(2).varint(2);
            o.varint(1).varint(2).i32(0x00ff_0000).i32(0x0000_ff00).varint(1).i32(7).bool(true).bool(false);
            o.varint(0).varint(0).varint(0).bool(false).bool(true);
        });
        aligned(component::FIREWORK_EXPLOSION, |o| {
            o.varint(3).varint(1).i32(1).varint(0).bool(true).bool(true);
        });
    }

    #[test]
    fn written_books_read_filterable_pages() {
        aligned(component::WRITTEN_BOOK_CONTENT, |o| {
            o.string("Title").bool(true).string("Filtered");
            o.string("Author").varint(1).varint(2);
            nbt_text(o, "page one");
            o.bool(false);
            nbt_text(o, "page two");
            o.bool(true);
            nbt_text(o, "filtered two");
            o.bool(true);
        });
        aligned(component::WRITABLE_BOOK_CONTENT, |o| {
            o.varint(2).string("a").bool(false).string("b").bool(true).string("c");
        });
    }

    #[test]
    fn banner_patterns_read_inline_and_registered_patterns() {
        aligned(component::BANNER_PATTERNS, |o| {
            o.varint(2);
            o.varint(0).string("minecraft:custom").string("block.minecraft.custom").varint(3);
            o.varint(4).varint(1);
        });
        aligned(component::PROVIDES_BANNER_PATTERNS, |o| {
            o.varint(0).string("minecraft:pattern_item/flower");
        });
    }

    #[test]
    fn trims_read_inline_materials_and_patterns() {
        aligned(component::TRIM, |o| {
            o.varint(0).string("minecraft:quartz");
            nbt_text(o, "Quartz");
            o.varint(6);
        });
        aligned(component::TRIM, |o| {
            o.varint(2).varint(0).string("minecraft:sentry");
            nbt_text(o, "Sentry");
            o.bool(true);
        });
        aligned(component::PROVIDES_TRIM_MATERIAL, |o| {
            o.varint(0).string("minecraft:gold");
            nbt_text(o, "Gold");
        });
    }

    #[test]
    fn jukebox_playable_and_instruments_read_inline_values() {
        aligned(component::JUKEBOX_PLAYABLE, |o| {
            o.varint(0);
            sound_direct(o);
            nbt_text(o, "Song");
            o.f32(120.0).varint(9);
        });
        aligned(component::JUKEBOX_PLAYABLE, |o| {
            o.varint(3);
        });
        aligned(component::INSTRUMENT, |o| {
            o.varint(0);
            sound_ref(o);
            o.f32(7.0).f32(256.0).varint(1);
            nbt_text(o, "Horn");
        });
    }

    #[test]
    fn tooltip_display_and_custom_model_data_read_their_lists() {
        aligned(component::TOOLTIP_DISPLAY, |o| {
            o.bool(true).varint(2).varint(component::CUSTOM_NAME).varint(component::LORE);
        });
        aligned(component::CUSTOM_MODEL_DATA, |o| {
            o.varint(2).f32(1.5).f32(2.5);
            o.varint(1).bool(true);
            o.varint(1).string("tag");
            o.varint(1).i32(-1);
        });
    }

    #[test]
    fn nested_stacks_in_other_components_read_through() {
        let inner = template(
            5,
            2,
            &[comp(component::LORE, |o| {
                o.varint(0);
            })],
        );
        aligned(component::USE_REMAINDER, |o| {
            o.raw(&inner);
        });
        aligned(component::CHARGED_PROJECTILES, |o| {
            o.varint(2).raw(&inner).raw(&inner);
        });
        aligned(component::POT_DECORATIONS, |o| {
            o.bool(true).raw(&inner).bool(false).bool(false).bool(true).raw(&inner);
        });
        aligned(component::SULFUR_CUBE_CONTENT, |o| {
            o.raw(&inner);
        });
    }

    #[test]
    fn can_break_reads_predicates_with_component_matchers() {
        aligned(component::CAN_BREAK, |o| {
            o.varint(1);
            o.bool(true).varint(3).varint(8).varint(9);
            o.bool(true).varint(2);
            o.string("facing").bool(true).string("north");
            o.string("age").bool(false).bool(true).string("1").bool(false);
            o.bool(true);
            nbt_empty(o);
            o.varint(1).varint(component::DAMAGE).varint(4);
            o.varint(1).bool(true).varint(2);
            nbt_empty(o);
        });
    }

    #[test]
    fn the_remaining_components_read_to_their_end() {
        aligned(component::USE_EFFECTS, |o| {
            o.bool(true).bool(false).f32(0.5);
        });
        aligned(component::FOOD, |o| {
            o.varint(4).f32(2.0).bool(true);
        });
        aligned(component::ATTACK_RANGE, |o| {
            for n in 0u8..6 {
                o.f32(f32::from(n));
            }
        });
        aligned(component::BLOCKS_ATTACKS, |o| {
            o.f32(0.0).f32(1.0).varint(1);
            o.f32(90.0).bool(true).varint(0).string("minecraft:is_fire").f32(0.0).f32(0.5);
            o.f32(1.0).f32(2.0).f32(3.0);
            o.bool(false).bool(true);
            sound_ref(o);
            o.bool(false);
        });
        aligned(component::KINETIC_WEAPON, |o| {
            o.varint(1).varint(2);
            o.bool(true).varint(3).f32(1.0).f32(2.0);
            o.bool(false).bool(false);
            o.f32(0.1).f32(0.2).bool(false).bool(true);
            sound_ref(o);
        });
        aligned(component::PIERCING_WEAPON, |o| {
            o.bool(true).bool(false).bool(true);
            sound_ref(o);
            o.bool(false);
        });
        aligned(component::LODESTONE_TRACKER, |o| {
            o.bool(true).string("minecraft:overworld").i64(-5).bool(true);
        });
        aligned(component::SIGN_TEXT_FRONT, |o| {
            for _ in 0..4 {
                nbt_text(o, "l");
            }
            o.bool(true);
            for _ in 0..4 {
                nbt_text(o, "f");
            }
            o.varint(2).bool(false);
        });
        aligned(component::COOKING_FUEL, |o| {
            o.bool(true).i32(200).bool(false).string("minecraft:speed");
        });
        aligned(component::COMPOSTABLE, |o| {
            o.bool(false).string("minecraft:layers");
        });
        aligned(component::BEES, |o| {
            o.varint(1).varint(33);
            nbt_empty(o);
            o.varint(100).varint(600);
        });
        aligned(component::ENTITY_DATA, |o| {
            o.varint(4);
            nbt_empty(o);
        });
        aligned(component::PAINTING_VARIANT, |o| {
            o.varint(0).varint(2).varint(1).string("minecraft:kebab").bool(true);
            nbt_text(o, "Kebab");
            o.bool(false);
        });
        aligned(component::BLOCK_STATE, |o| {
            o.varint(1).string("lit").string("true");
        });
        aligned(component::MOB_VISIBILITY, |o| {
            o.varint(2).varint(8).f32(0.5);
        });
        aligned(component::DYED_COLOR, |o| {
            o.i32(0x00ff_ffff);
        });
        aligned(component::UNBREAKABLE, |_| {});
    }

    #[test]
    fn every_component_type_is_known() {
        for id in 0..=component::CUSHION_COLOR {
            if let Err(e) = stack(&mut In::new(&item(1, 1, &[comp(id, |_| {})], &[]))) {
                assert!(!format!("{e:#}").contains("unknown data component"), "{id}: {e:#}");
            }
        }
    }
}
