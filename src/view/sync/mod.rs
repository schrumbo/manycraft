//! Sync: the one page where keeping game options, mod settings, worlds and screenshots the same across instances is
//! set up once and then switched on and off per kind.

mod folders;
mod setup;

use freya::prelude::*;

use self::folders::{PathCaption, open_button};
use self::setup::SetupPanel;
use crate::core::i18n::{tr, trf};
use crate::core::share::{self, Copies, KINDS, Kind, State as Shared};
use crate::ui::forms;
use crate::ui::instance_files::bytes;
use crate::view::kit::group::{Line, group};
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::switch::Switch;
use crate::view::kit::{off_ui, page, toast, use_theme};
use crate::view::library;
use crate::view::store::{Store, use_store};
use crate::view::theme::{frame, space};

#[derive(Clone, PartialEq)]
pub(crate) struct Who {
    pub id: String,
    pub name: String,
    pub version: String,
    pub running: bool,
}

pub(crate) fn whos(m: &crate::app::model::Model) -> Vec<Who> {
    let mut all: Vec<Who> = m.instances.iter().map(|i| Who { id: i.id.clone(), name: i.name.clone(), version: i.game_version.clone(), running: m.games.values().any(|g| g.instance_id == i.id && g.phase.is_active()) }).collect();
    all.sort_by_cached_key(|w| w.name.to_lowercase());
    all
}

pub(crate) fn running(who: &[Who]) -> Vec<String> {
    who.iter().filter(|w| w.running).map(|w| w.id.clone()).collect()
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Ctx {
    pub store: Store,
    pub asked: State<u32>,
}

impl Ctx {
    pub fn refresh(mut self) {
        if self.asked.try_peek().is_some() {
            let next = self.asked.peek().wrapping_add(1);
            self.asked.set(next);
        }
    }

    pub fn failed(self, what: String, e: &anyhow::Error) {
        let _ = toast::show(self.store.ui, forms::explain_as(what, e).text, None);
    }

    pub fn unexplained(self) {
        let _ = toast::show(self.store.ui, forms::unexplained(), None);
    }
}

fn label(kind: Kind) -> String {
    match kind {
        Kind::Options => tr("Spiel-Optionen"),
        Kind::Config => tr("Mod-Einstellungen"),
        Kind::Saves => tr("Welten"),
        Kind::Screenshots => tr("Screenshots"),
    }
}

fn blurb(kind: Kind) -> String {
    match kind {
        Kind::Options => tr("Grafik, Steuerung, Ton und alles andere aus „options.txt“."),
        Kind::Config => tr("Der Ordner „config“ der Mods."),
        Kind::Saves => tr("Alle Singleplayer-Welten, in jeder Instanz dieselben."),
        Kind::Screenshots => tr("Alle Screenshots an einem Ort."),
    }
}

/// The Sync page.
#[derive(PartialEq)]
pub struct SyncPage;

impl Component for SyncPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        library::use_infos(store);
        let scroll = use_scroll_controller(ScrollConfig::default);
        let asked = use_state(|| 0u32);
        let mut loaded = use_state(|| None::<Shared>);
        let mut busy = use_state(|| false);
        let ctx = Ctx { store, asked };
        use_side_effect(move || {
            let _ = asked.read();
            spawn_in_window(async move {
                let found = off_ui(share::state).await;
                if loaded.try_peek().is_some()
                    && let Some(found) = found
                {
                    loaded.set(Some(found));
                }
            });
        });
        let who = whos(&store.model.read());
        let mut col = page::column();
        let Some(state) = loaded.read().clone() else { return page::scrolling(scroll, col) };
        if who.is_empty() {
            let line = Line::new("sync.none", tr("Noch keine Instanzen")).hint(tr("Lege eine Instanz an, dann kannst du hier Sync einrichten."));
            return page::scrolling(scroll, col.child(group(&t, None, vec![line.into_element()], None)));
        }
        if !state.on {
            col = col.child(group(&t, Some(tr("So funktioniert Sync")), vec![Line::new("sync.how", tr("Eine Vorlage, alle gleich")).hint(tr("Du wählst eine Instanz als Vorlage. Ihre Spiel-Optionen, Mod-Einstellungen, Welten und Screenshots werden die gemeinsamen, und alle Instanzen nutzen sie, auch neue. Die eigenen Dateien der Instanzen bleiben als „….before-sync“ neben den Ordnern liegen.")).into_element()], None));
            return page::scrolling(scroll, col.child(SetupPanel { ctx, who }));
        }
        let running = running(&who);
        let any_running = !running.is_empty();
        let mut change = move |kind: Option<Kind>, on: bool| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            if on { apply(ctx, busy, running.clone(), kind, on) } else { ask(ctx, busy, running.clone(), kind) }
        };
        let working = *busy.read();
        let master_hint = if working { tr("Wird umgestellt …") } else { tr("Alle Instanzen nutzen, was unten eingeschaltet ist. Eine einzelne Instanz kann in ihren Einstellungen aussteigen.") };
        let mut off = change.clone();
        let mut off_press = change.clone();
        let master = Line::new("sync.master", tr("Sync")).hint(master_hint).control(Switch::new("sync.master.switch", tr("Sync"), true).on_toggle(move |_| off(None, false))).on_press(move |_| off_press(None, false));
        col = col.child(group(&t, None, vec![master.into_element()], if any_running { Some(tr("Ein Spiel läuft. Änderungen gelten für die Instanz ab ihrem nächsten Start.")) } else { None }));
        let rows: Vec<Element> = KINDS
            .into_iter()
            .map(|kind| {
                let on = state.get(kind);
                let mut flip = change.clone();
                let mut press = change.clone();
                let key = kind_key(kind);
                let path = share::path_of(kind);
                let open = open_button(format!("sync.kind.{key}.folder"), path.clone(), matches!(kind, Kind::Options));
                let switch = Switch::new(format!("sync.kind.{key}.switch"), label(kind), on).on_toggle(move |want| flip(Some(kind), want));
                let controls = rect().horizontal().cross_align(Alignment::Center).spacing(space::M).child(open).child(switch);
                Line::new(format!("sync.kind.{key}"), label(kind)).hint(blurb(kind)).control(controls).below(PathCaption { id: format!("sync.kind.{key}.path"), path, ctx }).on_press(move |_| press(Some(kind), !on)).into_element()
            })
            .collect();
        let home = Line::new("sync.folder", tr("Sync-Ordner"))
            .hint(tr("Dateien, die du hier ablegst oder ersetzt, gelten für alle synchronisierten Instanzen ab ihrem nächsten Start."))
            .control(open_button("sync.folder.open".into(), share::folder(), false))
            .below(PathCaption { id: "sync.folder.path".into(), path: share::folder(), ctx });
        col = col.child(group(&t, Some(tr("Was synchronisiert wird")), rows, Some(tr("Ausgeschaltet bekommt jede Instanz wieder eine eigene Kopie des gemeinsamen Stands."))));
        page::scrolling(scroll, col.child(group(&t, Some(tr("Wo die Dateien liegen")), vec![home.into_element()], None)))
    }
}

fn apply(ctx: Ctx, mut busy: State<bool>, running: Vec<String>, kind: Option<Kind>, on: bool) {
    busy.set(true);
    spawn_in_window(async move {
        let done = off_ui(move || match kind {
            Some(kind) => share::set_kind(kind, on, &running),
            None => share::turn_off(&running),
        })
        .await;
        match done {
            Some(Ok(())) => {}
            Some(Err(e)) => ctx.failed(tr("Nicht umgestellt."), &e),
            None => ctx.unexplained(),
        }
        if busy.try_peek().is_some() {
            busy.set(false);
        }
        ctx.refresh();
    });
}

/// Switching off gives each instance its own copy: more than one asks first, with Abbrechen as the default.
fn ask(ctx: Ctx, mut busy: State<bool>, running: Vec<String>, kind: Option<Kind>) {
    spawn_in_window(async move {
        let cost = off_ui(move || share::copies_if_off(kind)).await;
        match cost {
            Some(cost) if cost.count > 1 => {
                if busy.try_peek().is_some() {
                    busy.set(false);
                }
                confirm(ctx, busy, running, kind, cost);
            }
            Some(_) => apply(ctx, busy, running, kind, false),
            None => {
                ctx.unexplained();
                if busy.try_peek().is_some() {
                    busy.set(false);
                }
            }
        }
    });
}

fn cost_text(cost: Copies) -> String {
    match cost.bytes {
        Some(size) if size > 0 => trf("Es entstehen %1$lld eigene Kopien, zusammen etwa %2$@.", &[&cost.count, &bytes(size)]),
        _ => tr("Es entstehen mehrere Kopien."),
    }
}

fn confirm(ctx: Ctx, busy: State<bool>, running: Vec<String>, kind: Option<Kind>, cost: Copies) {
    let ui = ctx.store.ui;
    let heading = kind.map_or_else(|| tr("Sync ausschalten?"), |kind| trf("%@ ausschalten?", &[&label(kind)]));
    let keep = Primary { label: tr("Abbrechen"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(ui)) };
    let go = Primary {
        label: tr("Ausschalten"),
        disabled: None,
        danger: false,
        busy: false,
        run: EventHandler::new(move |_| {
            sheet::close(ui);
            if !*busy.peek() {
                apply(ctx, busy, running.clone(), kind, false);
            }
        }),
    };
    let card = Card::new("sync.confirm", heading).message(cost_text(cost)).primary(keep).primary_id("sync.confirm.cancel").secondary(go).secondary_id("sync.confirm.go").width(frame::SHEET_NARROW);
    sheet::open(ui, card);
}

fn kind_key(kind: Kind) -> &'static str {
    match kind {
        Kind::Options => "options",
        Kind::Config => "config",
        Kind::Saves => "saves",
        Kind::Screenshots => "screenshots",
    }
}
