//! The overlay of a picture through the player's eyes, drawn like the game's own heads-up display: hotbar, crosshair,
//! health, food, armor, air, experience, sidebar scoreboard, boss bars, titles and chat.
//!
//! [`Hud`] is plain data the caller fills from what the server sent; [`draw`] puts it on a picture at the GUI scale the
//! game picks on its own for that size.

use anyhow::Result;
use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::core::chat_text::Run;

mod art;
mod bars;
mod canvas;
mod icon;
mod text;

pub use self::art::{Art, shared};
use self::canvas::gui_scale;

/// Styled text: its runs in order; a `\n` in a run starts a new line.
pub type Text = Vec<Run>;

/// An item stack in a slot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stack {
    /// The item's registry name (`minecraft:diamond_sword`).
    pub id: &'static str,
    pub count: i32,
    /// Drawn with the enchantment glint.
    pub glint: bool,
    /// Damage taken and the damage the item survives, when it can break; 0 for both when it cannot.
    pub damage: i32,
    pub max_damage: i32,
    /// The dye colour of leather armor and other dyeable items, RGB.
    pub dye: Option<u32>,
}

/// How a score is shown.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Number {
    /// The score in red.
    #[default]
    Default,
    /// No number.
    Blank,
    /// The score in this style; the text of the run is ignored.
    Styled(Run),
    /// This text instead of the score.
    Fixed(Text),
}

/// A scoreboard team as far as names show it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Team {
    pub prefix: Text,
    pub suffix: Text,
    /// The team colour, RGB, that names carry between prefix and suffix.
    pub color: Option<u32>,
}

/// One line of the sidebar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Score {
    /// The score holder's name, as the server sent it.
    pub owner: String,
    /// The display name the server set for this score, instead of the holder's name.
    pub display: Option<Text>,
    pub value: i32,
    pub number: Number,
    /// The team the holder is in.
    pub team: Option<Team>,
}

/// The objective shown on the right of the screen and its scores.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sidebar {
    pub title: Text,
    pub scores: Vec<Score>,
    /// How scores are shown unless a score says otherwise.
    pub number: Number,
}

/// A boss bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Boss {
    pub name: Text,
    /// 0..1.
    pub progress: f32,
    /// 0 pink, 1 blue, 2 red, 3 green, 4 yellow, 5 purple, 6 white.
    pub color: u8,
    /// 0 plain, 1 six notches, 2 ten, 3 twelve, 4 twenty.
    pub overlay: u8,
}

/// The title and subtitle on the middle of the screen and how long they show, in ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Title {
    pub title: Text,
    pub subtitle: Text,
    pub fade_in: i32,
    pub stay: i32,
    pub fade_out: i32,
    /// Ticks since the title was set.
    pub age: i32,
}

/// The line above the hotbar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionBar {
    pub text: Text,
    /// Ticks it shows for, counting down from 60; the last 20 fade out.
    pub ticks_left: i32,
    /// Cycles colours, as a jukebox does.
    pub animated: bool,
}

/// A chat line on the lower left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chat {
    pub text: Text,
    /// Ticks since it arrived; lines older than 200 do not show.
    pub age: i32,
}

/// Which parts of a picture through the player's eyes are drawn; everything is on, as in the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Parts {
    pub chat: bool,
    /// The hotbar with the off-hand slot.
    pub hotbar: bool,
    /// The hand and the item in it.
    pub hand: bool,
    pub crosshair: bool,
    /// Health, food, armor and air.
    pub stats: bool,
    /// The experience bar and level.
    pub experience: bool,
    pub bosses: bool,
    pub sidebar: bool,
    /// Title, subtitle and the action bar.
    pub titles: bool,
    /// The darkening at the edges.
    pub vignette: bool,
}

impl Default for Parts {
    fn default() -> Self {
        Self { chat: true, hotbar: true, hand: true, crosshair: true, stats: true, experience: true, bosses: true, sidebar: true, titles: true, vignette: true }
    }
}

/// What the game shows over the world.
#[derive(Clone, Debug, PartialEq)]
pub struct Hud {
    /// What of it is drawn.
    pub parts: Parts,
    /// 0 survival, 1 creative, 2 adventure, 3 spectator; creative and spectator show no health, food or experience.
    pub game_mode: u8,
    pub hardcore: bool,
    pub health: f32,
    pub max_health: f32,
    pub absorption: f32,
    pub poisoned: bool,
    pub withered: bool,
    pub frozen: bool,
    /// 0..=20.
    pub food: i32,
    pub saturation: f32,
    /// The hunger effect is active.
    pub hungry: bool,
    /// The armor attribute, 0..=20.
    pub armor: i32,
    /// Air supply in ticks and its maximum (300), when not full or when under water.
    pub air: i32,
    pub max_air: i32,
    pub under_water: bool,
    /// 0..1 progress to the next level.
    pub xp: f32,
    pub level: i32,
    /// 0..=8.
    pub selected: u8,
    pub hotbar: [Option<Stack>; 9],
    pub offhand: Option<Stack>,
    /// True for the left hand being the main one.
    pub left_handed: bool,
    pub sidebar: Option<Sidebar>,
    pub bosses: Vec<Boss>,
    pub title: Option<Title>,
    pub action_bar: Option<ActionBar>,
    /// Oldest first.
    pub chat: Vec<Chat>,
}

impl Default for Hud {
    fn default() -> Self {
        Self {
            parts: Parts::default(),
            game_mode: 0,
            hardcore: false,
            health: 20.0,
            max_health: 20.0,
            absorption: 0.0,
            poisoned: false,
            withered: false,
            frozen: false,
            food: 20,
            saturation: 5.0,
            hungry: false,
            armor: 0,
            air: 300,
            max_air: 300,
            under_water: false,
            xp: 0.0,
            level: 0,
            selected: 0,
            hotbar: Default::default(),
            offhand: None,
            left_handed: false,
            sidebar: None,
            bosses: Vec::new(),
            title: None,
            action_bar: None,
            chat: Vec::new(),
        }
    }
}

/// Draws `hud` over `image`, a picture through the player's eyes, with the shared art.
pub fn draw(image: &mut RgbaImage, hud: &Hud) -> Result<()> {
    let art = shared()?;
    draw_with(image, hud, &art);
    Ok(())
}

/// Draws `hud` over `image` with `art`.
pub fn draw_with(image: &mut RgbaImage, hud: &Hud, art: &Art) {
    let mut canvas = canvas::Canvas::new(image);
    let c = &mut canvas;
    let (spectator, parts) = (hud.game_mode == 3, hud.parts);
    if !spectator {
        if parts.crosshair {
            bars::crosshair(c, art);
        }
        if parts.hotbar {
            bars::hotbar(c, art, hud);
        }
        if hud.game_mode != 1 {
            if parts.stats {
                bars::stats(c, art, hud);
            }
            if parts.experience {
                bars::experience(c, art, hud);
            }
        }
    }
    if parts.bosses && !hud.bosses.is_empty() {
        text::bosses(c, art, &hud.bosses);
    }
    if let (true, Some(side)) = (parts.sidebar, &hud.sidebar) {
        text::sidebar(c, art, side);
    }
    if parts.titles {
        text::title(c, art, hud);
        text::action_bar(c, art, hud);
    }
    if parts.chat {
        text::chat(c, art, &hud.chat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, color: Option<u32>) -> Text {
        vec![Run { text: text.to_owned(), color, ..Run::default() }]
    }

    fn stack(id: &'static str, count: i32) -> Option<Stack> {
        Some(Stack { id, count, ..Stack::default() })
    }

    fn full() -> Hud {
        let mut hud = Hud { health: 13.0, food: 17, armor: 15, xp: 0.6, level: 12, selected: 2, ..Hud::default() };
        hud.hotbar[0] = Some(Stack { id: "minecraft:diamond_sword", count: 1, glint: true, damage: 400, max_damage: 1561, dye: None });
        hud.hotbar[1] = stack("minecraft:stone", 64);
        hud.hotbar[2] = stack("minecraft:grass_block", 12);
        hud.hotbar[3] = stack("minecraft:bread", 5);
        hud.hotbar[4] = stack("minecraft:oak_log", 1);
        hud.hotbar[5] = stack("minecraft:torch", 33);
        hud.hotbar[6] = Some(Stack { id: "minecraft:leather_chestplate", count: 1, dye: Some(0xB02E26), ..Stack::default() });
        hud.hotbar[7] = stack("minecraft:oak_stairs", 2);
        hud.hotbar[8] = stack("minecraft:chest", 1);
        hud.offhand = stack("minecraft:shield", 1);
        hud.sidebar = Some(Sidebar {
            title: run("Stats", Some(0xFFAA00)),
            number: Number::Default,
            scores: vec![
                Score { owner: "Alice".into(), value: 12, ..Score::default() },
                Score { owner: "bob".into(), value: 12, team: Some(Team { prefix: run("[R] ", Some(0xFF5555)), suffix: Vec::new(), color: Some(0x55FF55) }), ..Score::default() },
                Score { owner: "zed".into(), value: 3, number: Number::Blank, ..Score::default() },
            ],
        });
        hud.bosses = vec![Boss { name: run("Ender Dragon", None), progress: 0.6, color: 5, overlay: 0 }, Boss { name: run("Wither", None), progress: 1.0, color: 2, overlay: 2 }];
        hud.title = Some(Title { title: run("Welcome", Some(0xFFFF55)), subtitle: run("to the server", None), fade_in: 10, stay: 70, fade_out: 20, age: 30 });
        hud.action_bar = Some(ActionBar { text: run("Action bar text", None), ticks_left: 40, animated: false });
        hud.chat = vec![Chat { text: run("Bob joined the game", Some(0xFFFF55)), age: 60 }, Chat { text: run("<Alice> hello there", None), age: 10 }];
        hud
    }

    #[test]
    fn the_game_picks_its_gui_scale_by_size() {
        assert_eq!(gui_scale(1280, 720), 3);
        assert_eq!(gui_scale(854, 480), 2);
        assert_eq!(gui_scale(320, 240), 1);
        assert_eq!(gui_scale(100, 100), 1);
        assert_eq!(gui_scale(3840, 2160), 9);
    }

    #[test]
    fn a_hud_draws_over_a_picture() {
        let Some(art) = jar().and_then(|j| Art::open(&j).ok()) else { return };
        let mut image = RgbaImage::from_pixel(1280, 720, image::Rgba([90, 140, 200, 255]));
        let start = std::time::Instant::now();
        draw_with(&mut image, &full(), &art);
        let took = start.elapsed();
        if let Some(out) = std::env::var_os("HUD_OUT") {
            let out = std::path::PathBuf::from(out);
            std::fs::create_dir_all(&out).unwrap();
            image.save(out.join("hud-ours.png")).unwrap();
        }
        let hotbar = image.get_pixel(640, 720 - 10).0;
        assert_ne!(hotbar, [90, 140, 200, 255], "the hotbar is drawn");
        assert!(took.as_millis() < if cfg!(debug_assertions) { 500 } else { 50 }, "{took:?}");
    }

    #[test]
    fn a_part_that_is_off_is_not_drawn() {
        let Some(art) = jar().and_then(|j| Art::open(&j).ok()) else { return };
        let blank = RgbaImage::from_pixel(1280, 720, image::Rgba([90, 140, 200, 255]));
        let drawn = |parts: Parts| {
            let mut image = blank.clone();
            draw_with(&mut image, &Hud { parts, ..full() }, &art);
            image
        };
        let off = Parts { chat: false, hotbar: false, hand: false, crosshair: false, stats: false, experience: false, bosses: false, sidebar: false, titles: false, vignette: false };
        assert!(drawn(off) == blank, "nothing is drawn with every part off");
        let only: [(&str, fn(&mut Parts)); 7] = [
            ("chat", |p| p.chat = true),
            ("hotbar", |p| p.hotbar = true),
            ("crosshair", |p| p.crosshair = true),
            ("stats", |p| p.stats = true),
            ("experience", |p| p.experience = true),
            ("bosses", |p| p.bosses = true),
            ("sidebar", |p| p.sidebar = true),
        ];
        for (name, on) in only {
            let mut parts = off;
            on(&mut parts);
            assert!(drawn(parts) != blank, "{name} draws when it is on");
        }
        let mut parts = off;
        parts.titles = true;
        assert!(drawn(parts) != blank, "titles draw when they are on");
        let all = drawn(Parts::default());
        let without_chat = drawn(Parts { chat: false, ..Parts::default() });
        assert!(all != without_chat, "chat is part of the full picture");
        if let Some(out) = std::env::var_os("HUD_OUT") {
            let out = std::path::PathBuf::from(out);
            std::fs::create_dir_all(&out).unwrap();
            without_chat.save(out.join("hud-chat-off.png")).unwrap();
        }
        assert!(without_chat.pixels().zip(all.pixels()).all(|(a, b)| a == b || b.0 != [90, 140, 200, 255]), "only chat pixels went");
    }

    #[test]
    fn sidebar_names_carry_the_team_colour() {
        let team = Team { prefix: run("[R] ", Some(0xFF5555)), suffix: run("!", None), color: Some(0x55FF55) };
        let score = Score { owner: "bob".into(), team: Some(team), ..Score::default() };
        let name = text::name_for_test(&score);
        assert_eq!(name.iter().map(|r| (r.text.as_str(), r.color)).collect::<Vec<_>>(), [("[R] ", Some(0xFF5555)), ("bob", Some(0x55FF55)), ("!", Some(0x55FF55))]);
    }

    fn jar() -> Option<std::path::PathBuf> {
        let jar = crate::core::paths::data_dir().join("versions/26.3/26.3.jar");
        jar.exists().then_some(jar)
    }
}