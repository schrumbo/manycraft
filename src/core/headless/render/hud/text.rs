//! The text over the world: sidebar scoreboard, boss bars, title, action bar and chat.

use super::art::Art;
use super::canvas::Canvas;
use super::{Boss, Chat, Hud, Number, Score, Sidebar, Text};
use crate::core::chat_text::Run;

const WHITE: u32 = 0xFFFF_FFFF;
const SIDEBAR_RED: u32 = 0xFF_5555;
const MAX_ROWS: usize = 15;
const CHAT_WIDTH: i32 = 320;
const CHAT_LINES: usize = 10;
const CHAT_TICKS: i32 = 200;
const BOSS_COLORS: [&str; 7] = ["pink", "blue", "red", "green", "yellow", "purple", "white"];
const BOSS_OVERLAYS: [&str; 4] = ["notched_6", "notched_10", "notched_12", "notched_20"];

fn argb(alpha: f32, rgb: u32) -> u32 {
    (((alpha.clamp(0.0, 1.0) * 255.0).round() as u32) << 24) | (rgb & 0xFF_FFFF)
}

fn colored(text: &[Run], color: Option<u32>) -> Text {
    text.iter().map(|r| Run { color: r.color.or(color), ..r.clone() }).collect()
}

fn name(score: &Score) -> Text {
    let base: Text = match &score.display {
        Some(d) => d.clone(),
        None => vec![Run { text: score.owner.clone(), ..Run::default() }],
    };
    match &score.team {
        None => base,
        Some(team) => {
            let mut out = colored(&team.prefix, team.color);
            out.extend(colored(&base, team.color));
            out.extend(colored(&team.suffix, team.color));
            out
        }
    }
}

#[cfg(test)]
pub(super) fn name_for_test(score: &Score) -> Text {
    name(score)
}

fn number(score: &Score, default: &Number) -> Text {
    let format = if matches!(score.number, Number::Default) { default } else { &score.number };
    let text = score.value.to_string();
    match format {
        Number::Blank => Vec::new(),
        Number::Fixed(t) => t.clone(),
        Number::Styled(style) => vec![Run { text, ..style.clone() }],
        Number::Default => vec![Run { text, color: Some(SIDEBAR_RED), ..Run::default() }],
    }
}

pub(super) fn sidebar(c: &mut Canvas, art: &Art, side: &Sidebar) {
    let font = &art.font;
    let mut scores: Vec<&Score> = side.scores.iter().filter(|s| !s.owner.starts_with('#')).collect();
    scores.sort_by(|a, b| b.value.cmp(&a.value).then_with(|| a.owner.to_lowercase().cmp(&b.owner.to_lowercase())));
    scores.truncate(MAX_ROWS);
    let rows: Vec<(Text, Text, i32)> = scores
        .iter()
        .map(|s| {
            let (n, v) = (name(s), number(s, &side.number));
            let w = font.width(&v);
            (n, v, w)
        })
        .collect();
    let title_width = font.width(&side.title);
    let spacer = font.text_width(": ");
    let widest = rows.iter().fold(title_width, |m, (n, _, w)| m.max(font.width(n) + if *w > 0 { spacer + *w } else { 0 }));
    let count = rows.len() as i32;
    let total = count * 9;
    let bottom = c.height / 2 + total / 3;
    let left = c.width - widest - 3;
    let right = c.width - 3 + 2;
    let top = bottom - count * 9;
    c.fill(left - 2, top - 9 - 1, right, top - 1, argb(0.4, 0));
    c.fill(left - 2, top - 1, right, bottom, argb(0.3, 0));
    c.text(font, left + widest / 2 - title_width / 2, top - 9, 1, &side.title, WHITE, false);
    for (i, (n, v, w)) in rows.iter().enumerate() {
        let y = bottom - (count - i as i32) * 9;
        c.text(font, left, y, 1, n, WHITE, false);
        c.text(font, right - w, y, 1, v, WHITE, false);
    }
}

pub(super) fn bosses(c: &mut Canvas, art: &Art, bosses: &[Boss]) {
    let mut y = 12;
    for boss in bosses {
        let x = c.width / 2 - 91;
        let color = BOSS_COLORS[usize::from(boss.color).min(6)];
        if let Some(s) = art.sprite(&format!("boss_bar/{color}_background")) {
            c.sprite(s, x, y);
        }
        let overlay = usize::from(boss.overlay).checked_sub(1).and_then(|i| BOSS_OVERLAYS.get(i));
        if let Some(o) = overlay
            && let Some(s) = art.sprite(&format!("boss_bar/{o}_background"))
        {
            c.sprite(s, x, y);
        }
        let progress = if boss.progress.is_finite() { boss.progress.clamp(0.0, 1.0) } else { 0.0 };
        let filled = if progress > 0.0 { (progress * 181.0).floor() as u32 + 1 } else { 0 };
        if filled > 0 {
            if let Some(s) = art.sprite(&format!("boss_bar/{color}_progress")) {
                c.sprite_part(s, 0, 0, filled, 5, x, y, 1.0);
            }
            if let Some(o) = overlay
                && let Some(s) = art.sprite(&format!("boss_bar/{o}_progress"))
            {
                c.sprite_part(s, 0, 0, filled, 5, x, y, 1.0);
            }
        }
        let w = art.font.width(&boss.name);
        c.text(&art.font, c.width / 2 - w / 2, y - 9, 1, &boss.name, WHITE, true);
        y += 10 + 9;
        if y >= c.height / 3 {
            break;
        }
    }
}

pub(super) fn title(c: &mut Canvas, art: &Art, hud: &Hud) {
    let Some(t) = &hud.title else { return };
    let (fade_in, stay, fade_out, age) = (i64::from(t.fade_in), i64::from(t.stay), i64::from(t.fade_out), i64::from(t.age));
    let left = fade_in + stay + fade_out - age;
    if left <= 0 || (t.title.is_empty() && t.subtitle.is_empty()) {
        return;
    }
    let mut alpha = 255;
    if left > fade_out + stay {
        alpha = if fade_in > 0 { age * 255 / fade_in } else { 255 };
    } else if left <= fade_out {
        alpha = if fade_out > 0 { left * 255 / fade_out } else { 255 };
    }
    let alpha = alpha.clamp(0, 255) as i32;
    if alpha <= 0 {
        return;
    }
    let color = (alpha as u32) << 24 | 0xFF_FFFF;
    let (mx, my) = (c.width / 2, c.height / 2);
    if !t.title.is_empty() {
        let w = art.font.width(&t.title);
        c.text(&art.font, mx + (-w / 2) * 4, my - 10 * 4, 4, &t.title, color, true);
    }
    if !t.subtitle.is_empty() {
        let w = art.font.width(&t.subtitle);
        c.text(&art.font, mx + (-w / 2) * 2, my + 5 * 2, 2, &t.subtitle, color, true);
    }
}

fn hsv_rgb(h: f32, s: f32, v: f32) -> u32 {
    let i = (h.rem_euclid(1.0) * 6.0).floor() as i32;
    let f = h.rem_euclid(1.0) * 6.0 - i as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let c = |x: f32| (x * 255.0).round() as u32;
    c(r) << 16 | c(g) << 8 | c(b)
}

pub(super) fn action_bar(c: &mut Canvas, art: &Art, hud: &Hud) {
    let Some(bar) = &hud.action_bar else { return };
    if bar.ticks_left <= 0 || bar.text.is_empty() {
        return;
    }
    let alpha = (bar.ticks_left as f32 * 255.0 / 20.0).min(255.0) as i32;
    if alpha <= 0 {
        return;
    }
    let rgb = if bar.animated { hsv_rgb(bar.ticks_left as f32 / 50.0, 0.7, 0.6) } else { 0xFF_FFFF };
    let w = art.font.width(&bar.text);
    c.text(&art.font, c.width / 2 - w / 2, c.height - 68 - 4, 1, &bar.text, (alpha as u32) << 24 | rgb, true);
}

pub(super) fn chat(c: &mut Canvas, art: &Art, lines: &[Chat]) {
    let mut wrapped: Vec<(Vec<Run>, i32)> = Vec::new();
    for line in lines.iter().filter(|l| l.age >= 0 && l.age < CHAT_TICKS) {
        for part in art.font.wrap(&line.text, CHAT_WIDTH) {
            wrapped.push((part, line.age));
        }
    }
    let baseline = c.height - 40;
    for (i, (part, age)) in wrapped.iter().rev().take(CHAT_LINES).enumerate() {
        let f = ((1.0 - *age as f32 / CHAT_TICKS as f32) * 10.0).clamp(0.0, 1.0);
        let f = f * f;
        let alpha = (255.0 * f) as i32;
        if alpha <= 3 {
            continue;
        }
        let bottom = baseline - i as i32 * 9;
        c.fill(0, bottom - 9, CHAT_WIDTH + 12, bottom, argb(0.5 * f, 0));
        c.text(&art.font, 4, bottom - 8, 1, part, (alpha as u32) << 24 | 0xFF_FFFF, true);
    }
}
