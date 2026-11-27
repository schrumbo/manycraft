//! Spielstart: the launcher defaults every instance starts with where it sets nothing of its own.

use std::path::PathBuf;
use std::time::Duration;

use freya::prelude::*;

use super::{Prefs, Section, toggle, use_prefs};
use crate::core::i18n::{tr, trf};
use crate::core::javas::Java;
use crate::core::instance::DEFAULT_MEMORY_MB;
use crate::core::options::{self, Gpu, MEMORY_FLOOR_MB, MEMORY_MIN_FLOOR_MB};
use crate::core::paths;
use crate::core::settings;
use crate::ui::{forms, remote};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::group::{Line, titled};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::text::line;
use crate::view::instance::memory::{Memory, Steps};
use crate::view::kit::{after, off_ui, open_external, use_theme};
use crate::view::native;
use crate::view::theme::{control, font, space};

const TYPING: Duration = Duration::from_millis(800);
const GPUS: [Option<Gpu>; 3] = [None, Some(Gpu::HighPerformance), Some(Gpu::PowerSaving)];

/// The name of a graphics card choice, here and on the instance page.
pub(crate) fn gpu_label(gpu: Option<Gpu>) -> String {
    match gpu {
        None => tr("Automatisch"),
        Some(Gpu::HighPerformance) => tr("Hohe Leistung"),
        Some(Gpu::PowerSaving) => tr("Energie sparen"),
    }
}

fn java_title(java: &Java) -> String {
    match &java.vendor {
        Some(vendor) => trf("Java %@ · %@", &[&java.major, vendor]),
        None => trf("Java %@", &[&java.major]),
    }
}

/// The defaults group: memory, window, graphics card and Java.
#[derive(PartialEq)]
pub struct GameGroup;

impl Component for GameGroup {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let p = use_prefs();
        let s = p.read();
        let game = s.game.clone();
        let mut javas = use_state(|| None::<Vec<Java>>);
        let mut java_problem = use_state(|| None::<String>);
        let mut picking = use_state(|| false);
        use_hook(move || {
            spawn(async move {
                if let Some(found) = off_ui(remote::detect_javas).await {
                    javas.set(Some(found));
                }
            });
        });
        let most = MemoryLine { p, limit: Limit::Most, saved: Some(game.memory_mb), ceiling: game.memory_mb }.into_element();
        let least = MemoryLine { p, limit: Limit::Least, saved: game.memory_min_mb, ceiling: game.memory_mb }.into_element();

        let mut choices: Vec<(Option<Option<PathBuf>>, String)> = vec![(Some(None), tr("Automatisch (empfohlen)"))];
        for found in javas.read().iter().flatten() {
            choices.push((Some(Some(found.path.clone())), java_title(found)));
        }
        if let Some(path) = game.java.as_ref().filter(|p| !choices.iter().any(|(c, _)| c.as_ref().is_some_and(|c| c.as_ref() == Some(*p)))) {
            choices.push((Some(Some(path.clone())), tr("Eigenes Java")));
        }
        choices.push((None, tr("Datei wählen…")));
        let at = choices.iter().position(|(c, _)| c.as_ref() == Some(&game.java)).unwrap_or(0);
        let labels = choices.iter().map(|(_, l)| l.clone()).collect();
        let paths: Vec<Option<Option<PathBuf>>> = choices.into_iter().map(|(c, _)| c).collect();
        let java_hint = match (&game.java, javas.read().is_none()) {
            (Some(path), _) => trf("%@ · Nur für Versionen, zu denen es passt; sonst nimmt Manycraft das passende.", &[&path.display()]),
            (None, true) => tr("Suche installierte Java-Versionen …"),
            (None, false) => tr("Manycraft lädt für jede Version das passende Java."),
        };
        let mut java = Line::new("settings.game.java.row", tr("Java")).hint(java_problem.read().clone().unwrap_or(java_hint)).control(
            Select::new("settings.game.java", tr("Java"), labels, at).width(control::VERSION_MENU).disabled(picking.read().then(|| tr("Datei wird gewählt …"))).on_choose(move |i| {
                java_problem.set_if_modified(None);
                match paths.get(i).cloned() {
                    Some(Some(chosen)) => p.change(|s| s.game.java = chosen),
                    Some(None) => pick_java(p, picking, java_problem),
                    None => {}
                }
            }),
        );
        if java_problem.read().is_some() {
            java = java.hint_tone(t.danger_text);
        }

        let window = WindowLine { p, saved: game.window.map(|w| (w.width.to_string(), w.height.to_string())).unwrap_or_default() }.into_element();
        let fullscreen = toggle(p, "settings.game.fullscreen", tr("Im Vollbild starten"), tr("Gilt für Spiele, die du allein spielst."), game.fullscreen, |s, on| s.game.fullscreen = on);

        let mut rows = vec![most, window, fullscreen];
        if cfg!(windows) || game.gpu.is_some() {
            let segments = GPUS.iter().enumerate().map(|(n, g)| Segment { id: format!("settings.game.gpu.{n}"), label: gpu_label(*g), tip: None }).collect();
            let chosen = GPUS.iter().position(|g| *g == game.gpu).unwrap_or(0);
            rows.push(
                Line::new("settings.game.gpu.row", tr("Grafikkarte"))
                    .hint(tr("Für Computer mit zwei Grafikkarten. Gilt für jedes Java, das ein Spiel nutzt."))
                    .control(Segmented::new(tr("Grafikkarte"), segments, chosen).on_choose(move |i| p.change(|s| s.game.gpu = GPUS[i.min(2)])))
                    .into_element(),
            );
        }
        rows.push(java.into_element());
        rows.push(least);
        titled(&t, Some(Section::Start.title()), Some(tr("Gilt für Instanzen ohne eigene Einstellung")), rows, None)
    }
}

/// The row that shows where the data lives and opens the folder.
pub(super) fn data_row() -> Element {
    let shown = paths::data_dir().display().to_string();
    Line::new("settings.game.data.row", tr("Datenordner"))
        .hint(shown.clone())
        .control(Button::new("settings.game.data.open", tr("Ordner öffnen")).on_press(move |_| open_external("open folder", shown.clone())))
        .into_element()
}

/// Which of the two memory lines.
#[derive(Clone, Copy, PartialEq)]
enum Limit {
    Most,
    Least,
}

/// One line of the memory a game starts with or may use at most, saved a moment after typing stops.
#[derive(PartialEq)]
struct MemoryLine {
    p: Prefs,
    limit: Limit,
    saved: Option<u32>,
    ceiling: u32,
}

fn saved_text(mb: Option<u32>) -> String {
    mb.map(|mb| mb.to_string()).unwrap_or_default()
}

fn save_max(p: Prefs, text: &str) -> Result<(), String> {
    let max = options::parse_memory(text)?.unwrap_or(DEFAULT_MEMORY_MB);
    options::check_memory_max(max, settings::active().game.memory_min_mb, forms::total_ram_mb())?;
    p.change(|s| s.game.memory_mb = max);
    Ok(())
}

fn save_min(p: Prefs, text: &str) -> Result<(), String> {
    let min = options::parse_memory(text)?;
    if let Some(min) = min {
        options::check_memory_min(min, settings::active().game.memory_mb)?;
    }
    p.change(|s| s.game.memory_min_mb = min);
    Ok(())
}

/// Saves `text` through `apply` once typing stopped for a moment, and keeps `text` in step with `saved` where something else changed it.
fn use_memory_text(p: Prefs, saved: String, apply: fn(Prefs, &str) -> Result<(), String>) -> (State<String>, State<Option<String>>) {
    let mut text = use_state(|| saved.clone());
    let mut problem = use_state(|| None::<String>);
    let typed = text.read().clone();
    use_side_effect_with_deps(&typed, move |typed| {
        let typed = typed.clone();
        spawn(async move {
            after(TYPING).await;
            if !p.alive() || *text.peek() != typed {
                return;
            }
            match apply(p, &typed) {
                Ok(()) => problem.set_if_modified(None),
                Err(e) => problem.set_if_modified(Some(e)),
            }
        });
    });
    use_side_effect_with_deps(&saved, move |saved| {
        let shown = options::parse_memory(&text.peek()).ok().flatten();
        if shown != options::parse_memory(saved).ok().flatten() {
            text.set(saved.clone());
        }
    });
    (text, problem)
}

impl Component for MemoryLine {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let p = self.p;
        let total = forms::total_ram_mb();
        let (text, problem) = use_memory_text(p, saved_text(self.saved), match self.limit {
            Limit::Most => save_max,
            Limit::Least => save_min,
        });
        let (id, label, hint) = match self.limit {
            Limit::Most => ("settings.game.memory", tr("Arbeitsspeicher"), trf("Mehr braucht ein Spiel nie. Dieser Computer hat %@.", &[&forms::memory_label(u32::try_from(total).unwrap_or(u32::MAX))])),
            Limit::Least => ("settings.game.memory_min", tr("Arbeitsspeicher beim Start"), tr("Womit ein Spiel startet. Leer lassen: Java entscheidet.")),
        };
        let field = Memory::new(id, label.clone(), text);
        let field = match self.limit {
            Limit::Most => {
                let ceiling = u32::try_from(total).unwrap_or(u32::MAX).max(MEMORY_FLOOR_MB);
                field.placeholder(DEFAULT_MEMORY_MB.to_string()).steps(Steps { floor: MEMORY_FLOOR_MB, ceiling, from: self.saved.unwrap_or(DEFAULT_MEMORY_MB) })
            }
            Limit::Least => {
                let ceiling = self.ceiling;
                field.placeholder(tr("Automatisch")).steps(Steps { floor: MEMORY_MIN_FLOOR_MB, ceiling, from: ceiling.min(1024) })
            }
        };
        let mut row = Line::new(format!("{id}.row"), label).hint(problem.read().clone().unwrap_or(hint)).control(field);
        if problem.read().is_some() {
            row = row.hint_tone(t.danger_text);
        }
        row
    }
}

fn pick_java(p: Prefs, mut picking: State<bool>, mut problem: State<Option<String>>) {
    picking.set(true);
    spawn(async move {
        let picked = off_ui(|| native::pick_files(&tr("Java wählen"), "Java", &[], false)).await.unwrap_or_default();
        let probed = match picked.into_iter().next() {
            Some(path) => off_ui(move || remote::probe_java(&path).map_err(|e| format!("{e:#}"))).await,
            None => None,
        };
        if !p.alive() {
            return;
        }
        picking.set(false);
        match probed {
            Some(Ok(java)) => p.change(|s| s.game.java = Some(java.path)),
            Some(Err(e)) => problem.set(Some(e)),
            None => {}
        }
    });
}

/// Width and height, saved a moment after typing stops; empty fields leave the size to Minecraft.
#[derive(PartialEq)]
struct WindowLine {
    p: Prefs,
    saved: (String, String),
}

impl Component for WindowLine {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let p = self.p;
        let saved = self.saved.clone();
        let width = use_state(|| saved.0.clone());
        let height = use_state(|| saved.1.clone());
        let mut problem = use_state(|| None::<String>);
        let typed = (width.read().clone(), height.read().clone());
        use_side_effect_with_deps(&typed, move |(w, h)| {
            let (w, h) = (w.clone(), h.clone());
            spawn(async move {
                after(TYPING).await;
                if !p.alive() || *width.peek() != w || *height.peek() != h {
                    return;
                }
                match options::parse_window(&w, &h) {
                    Ok(size) => {
                        problem.set_if_modified(None);
                        p.change(|s| s.game.window = size);
                    }
                    Err(e) => problem.set_if_modified(Some(e)),
                }
            });
        });
        let size = rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(Field::new("settings.game.window.width", width).placeholder(tr("Breite")).width(Size::px(control::NUMBER)))
            .child(line(&t, "×", font::BODY, t.text3))
            .child(Field::new("settings.game.window.height", height).placeholder(tr("Höhe")).width(Size::px(control::NUMBER)));
        let mut row = Line::new("settings.game.window.row", tr("Fenstergröße")).hint(problem.read().clone().unwrap_or_else(|| tr("Leer lassen für die Größe, die Minecraft wählt."))).control(size);
        if problem.read().is_some() {
            row = row.hint_tone(t.danger_text);
        }
        row
    }
}