//! Spiel: the launcher defaults every instance starts with where it sets nothing of its own.

use std::path::PathBuf;
use std::time::Duration;

use freya::prelude::*;

use super::{Prefs, toggle, use_prefs};
use crate::core::i18n::{tr, trf};
use crate::core::javas::Java;
use crate::core::options::{self, Gpu};
use crate::core::paths;
use crate::ui::{forms, remote};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::group::{Line, titled};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::stepper::Stepper;
use crate::view::kit::text::line;
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

fn memory_text(mb: i64) -> String {
    forms::memory_label(u32::try_from(mb).unwrap_or(0))
}

/// The defaults group: memory, Java, window, graphics card and where the data lives.
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
        let total = forms::total_ram_mb();
        let max = forms::memory_max(total);
        let memory = Line::new("settings.game.memory.row", tr("Arbeitsspeicher"))
            .hint(trf("Dieser Computer hat %@.", &[&forms::memory_label(u32::try_from(total).unwrap_or(u32::MAX))]))
            .control(
                Stepper::new("settings.game.memory", tr("Arbeitsspeicher"), i64::from(game.memory_mb), i64::from(forms::MEMORY_MIN), i64::from(max))
                    .step(i64::from(forms::MEMORY_STEP))
                    .shown(memory_text)
                    .on_change(move |v: i64| {
                        p.change(|s| {
                            let now = s.game.memory_mb;
                            s.game.memory_mb = forms::memory_step(now, if v > i64::from(now) { 1 } else { -1 }, max);
                        })
                    }),
            )
            .into_element();

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

        let mut rows = vec![memory, java.into_element(), window, fullscreen];
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
        let data = paths::data_dir();
        let shown = data.display().to_string();
        rows.push(
            Line::new("settings.game.data.row", tr("Datenordner"))
                .hint(shown.clone())
                .control(Button::new("settings.game.data.open", tr("Ordner öffnen")).on_press(move |_| open_external("open folder", shown.clone())))
                .into_element(),
        );
        titled(&t, Some(tr("Spiel")), Some(tr("Gilt für Instanzen ohne eigene Einstellung")), rows, None)
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
