//! The bottom of the screen: hotbar, crosshair, health, food, armor, air and experience.

use super::art::Art;
use super::canvas::Canvas;
use super::icon::icon;
use super::{Hud, Stack};
use crate::core::chat_text::Run;

fn plain(text: &str) -> Vec<Run> {
    vec![Run { text: text.to_owned(), ..Run::default() }]
}

fn sprite(c: &mut Canvas, art: &Art, name: &str, x: i32, y: i32) {
    if let Some(s) = art.sprite(name) {
        c.sprite(s, x, y);
    }
}

pub(super) fn crosshair(c: &mut Canvas, art: &Art) {
    if let Some(s) = art.sprite("hud/crosshair") {
        let (x, y) = ((c.width - 15) / 2, (c.height - 15) / 2);
        c.inverted(s, x, y);
    }
}

fn item(c: &mut Canvas, art: &Art, stack: &Stack, x: i32, y: i32) {
    if stack.count <= 0 {
        return;
    }
    let px = (16 * c.scale) as u32;
    let img = icon(art, stack, px);
    let s = c.scale as i32;
    for (ix, iy, p) in img.enumerate_pixels() {
        if p.0[3] != 0 {
            let (dx, dy) = (x * s + ix as i32, y * s + iy as i32);
            if dx >= 0 && dy >= 0 && (dx as u32) < c.image.width() && (dy as u32) < c.image.height() {
                super::super::font::blend(c.image.get_pixel_mut(dx as u32, dy as u32), p.0);
            }
        }
    }
    if stack.max_damage > 0 && stack.damage > 0 {
        let left = (stack.max_damage - stack.damage).max(0) as f32 / stack.max_damage as f32;
        let step = (13.0 - stack.damage as f32 * 13.0 / stack.max_damage as f32).round() as i32;
        let [r, g, b] = hsv(left.clamp(0.0, 1.0) / 3.0);
        c.fill(x + 2, y + 13, x + 15, y + 15, 0xFF00_0000);
        c.fill(x + 2, y + 13, x + 2 + step.clamp(0, 13), y + 14, 0xFF00_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b));
    }
    if stack.count != 1 {
        let text = plain(&stack.count.to_string());
        let width = art.font.width(&text);
        c.text(&art.font, x + 19 - 2 - width, y + 6 + 3, 1, &text, 0xFFFF_FFFF, true);
    }
}

fn hsv(h: f32) -> [u8; 3] {
    let i = (h * 6.0).floor() as i32;
    let f = h * 6.0 - i as f32;
    let (p, q, t) = (0.0, 1.0 - f, f);
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (1.0, t, p),
        1 => (q, 1.0, p),
        2 => (p, 1.0, t),
        3 => (p, q, 1.0),
        4 => (t, p, 1.0),
        _ => (1.0, p, q),
    };
    [r, g, b].map(|v: f32| (v * 255.0).clamp(0.0, 255.0) as u8)
}

pub(super) fn hotbar(c: &mut Canvas, art: &Art, hud: &Hud) {
    let (mid, h) = (c.width / 2, c.height);
    sprite(c, art, "hud/hotbar", mid - 91, h - 22);
    sprite(c, art, "hud/hotbar_selection", mid - 91 - 1 + i32::from(hud.selected.min(8)) * 20, h - 22 - 1);
    let off_left = !hud.left_handed;
    if hud.offhand.is_some() {
        if off_left {
            sprite(c, art, "hud/hotbar_offhand_left", mid - 91 - 29, h - 23);
        } else {
            sprite(c, art, "hud/hotbar_offhand_right", mid + 91, h - 23);
        }
    }
    for (i, stack) in hud.hotbar.iter().enumerate() {
        if let Some(stack) = stack {
            item(c, art, stack, mid - 90 + i as i32 * 20 + 2, h - 16 - 3);
        }
    }
    if let Some(stack) = &hud.offhand {
        let x = if off_left { mid - 91 - 26 } else { mid + 91 + 10 };
        item(c, art, stack, x, h - 16 - 3);
    }
}

pub(super) fn experience(c: &mut Canvas, art: &Art, hud: &Hud) {
    let (x, y) = ((c.width - 182) / 2, c.height - 24 - 5);
    sprite(c, art, "hud/experience_bar_background", x, y);
    let filled = (hud.xp.clamp(0.0, 1.0) * 183.0) as u32;
    if filled > 0
        && let Some(s) = art.sprite("hud/experience_bar_progress")
    {
        c.sprite_part(s, 0, 0, filled.min(182), 5, x, y, 1.0);
    }
    if hud.level > 0 {
        let text = plain(&hud.level.to_string());
        let tx = (c.width - art.font.width(&text)) / 2;
        let ty = c.height - 24 - 9 - 2;
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            c.text(&art.font, tx + dx, ty + dy, 1, &text, 0xFF00_0000, false);
        }
        c.text(&art.font, tx, ty, 1, &text, 0xFF80_FF20, false);
    }
}

fn heart_sprite(kind: &str, hardcore: bool, half: bool, blink: bool) -> String {
    let core = if half { "half" } else { "full" };
    let mut name = String::from("hud/heart/");
    if kind != "normal" {
        name.push_str(kind);
        name.push('_');
    }
    if hardcore {
        name.push_str("hardcore_");
    }
    name.push_str(core);
    if blink {
        name.push_str("_blinking");
    }
    name
}

pub(super) fn stats(c: &mut Canvas, art: &Art, hud: &Hud) {
    let (left, right, base) = (c.width / 2 - 91, c.width / 2 + 91, c.height - 39);
    let max = if hud.max_health.is_finite() { hud.max_health.clamp(1.0, 1024.0) } else { 20.0 };
    let health = if hud.health.is_finite() { hud.health.clamp(0.0, max).ceil() as i32 } else { 0 };
    let absorption = if hud.absorption.is_finite() { hud.absorption.clamp(0.0, 2048.0).ceil() as i32 } else { 0 };
    let rows = ((max + absorption as f32) / 2.0 / 10.0).ceil() as i32;
    let row_height = (10 - (rows - 2)).max(3);
    let kind = if hud.poisoned {
        "poisoned"
    } else if hud.withered {
        "withered"
    } else if hud.frozen {
        "frozen"
    } else {
        "normal"
    };
    let max_hearts = (max / 2.0).ceil() as i32;
    let absorb_hearts = (absorption as f32 / 2.0).ceil() as i32;
    for i in (0..max_hearts + absorb_hearts).rev() {
        let (x, y) = (left + (i % 10) * 8, base - (i / 10) * row_height);
        let container = if hud.hardcore { "hud/heart/container_hardcore" } else { "hud/heart/container" };
        sprite(c, art, container, x, y);
        let v = i * 2;
        if i >= max_hearts {
            let rel = v - max_hearts * 2;
            if rel < absorption {
                let k = if hud.withered { "withered" } else { "absorbing" };
                sprite(c, art, &heart_sprite(k, hud.hardcore, rel + 1 == absorption, false), x, y);
            }
        } else if v < health {
            sprite(c, art, &heart_sprite(kind, hud.hardcore, v + 1 == health, false), x, y);
        }
    }
    if hud.armor > 0 {
        let y = base - (rows - 1) * row_height - 10;
        for i in 0..10 {
            let v = i * 2 + 1;
            let name = if v < hud.armor {
                "hud/armor_full"
            } else if v == hud.armor {
                "hud/armor_half"
            } else {
                "hud/armor_empty"
            };
            sprite(c, art, name, left + i * 8, y);
        }
    }
    let hunger = if hud.hungry { "_hunger" } else { "" };
    for i in 0..10 {
        let x = right - i * 8 - 9;
        sprite(c, art, &format!("hud/food_empty{hunger}"), x, base);
        let v = i * 2 + 1;
        if v < hud.food {
            sprite(c, art, &format!("hud/food_full{hunger}"), x, base);
        } else if v == hud.food {
            sprite(c, art, &format!("hud/food_half{hunger}"), x, base);
        }
    }
    air(c, art, hud, right, base - 10);
}

fn air(c: &mut Canvas, art: &Art, hud: &Hud, right: i32, y: i32) {
    let max = hud.max_air.clamp(1, 1 << 20);
    let air = hud.air.clamp(0, max);
    if !hud.under_water && air >= max {
        return;
    }
    let bubble = |extra: i32| ((air + extra) * 10 + max - 1).div_euclid(max);
    let (full, popping) = (bubble(-2), bubble(0));
    let delay = i32::from(air != 0 && hud.under_water);
    for n in 1..=10 {
        let x = right - (n - 1) * 8 - 9;
        if n <= full {
            sprite(c, art, "hud/air", x, y);
        } else if full != popping && n == popping && hud.under_water {
            sprite(c, art, "hud/air_bursting", x, y);
        } else if n > bubble(delay) {
            sprite(c, art, "hud/air_empty", x, y);
        }
    }
}
