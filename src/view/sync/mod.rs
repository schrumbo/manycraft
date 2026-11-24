//! Sync: the one page where keeping game options, mod settings and the server list the same across instances is set up
//! and seen: a setup that picks the base and the instances to bring in line, what each item stands on, and each instance.

mod instances;
mod kinds;
mod setup;
pub(crate) mod words;

use freya::prelude::*;

use self::instances::InstancesBlock;
use self::kinds::KindsBlock;
use self::setup::SetupPanel;
use crate::app::event::Event as AppEvent;
use crate::app::model::Model;
use crate::core::i18n::{tr, trf};
use crate::core::share::{self, ITEMS, Item, Note, Sharing, Standing, Summary, Version};
use crate::ui::forms;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::switch::Switch;
use crate::view::kit::{off_ui, page, toast, use_theme};
use crate::view::library;
use crate::view::store::{Store, use_store};

#[derive(Clone, PartialEq)]
pub(crate) struct Loaded {
    pub standing: Vec<Standing>,
    pub versions: Vec<(Item, Option<Vec<Version>>)>,
    pub notes: Vec<Note>,
    pub last: Option<Summary>,
}

impl Loaded {
    fn read() -> Self {
        Self { standing: share::standing(), versions: ITEMS.into_iter().map(|i| (i, share::versions(i).ok())).collect(), notes: share::notes(), last: share::last_setup() }
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct Who {
    pub id: String,
    pub name: String,
    pub version: String,
    pub sync: Sharing,
    pub running: bool,
}

pub(crate) fn whos(m: &Model) -> Vec<Who> {
    let mut all: Vec<Who> = m.instances.iter().map(|i| Who { id: i.id.clone(), name: i.name.clone(), version: i.game_version.clone(), sync: i.sync, running: m.games.values().any(|g| g.instance_id == i.id && g.phase.is_active()) }).collect();
    all.sort_by_cached_key(|w| w.name.to_lowercase());
    all
}

/// "3 von 4 Instanzen synchronisieren" for the toolbar.
pub fn count_line(m: &Model) -> String {
    let all = whos(m);
    if all.is_empty() {
        return tr("Noch keine Instanzen");
    }
    let on = all.iter().filter(|w| w.sync.any()).count();
    trf("%lld von %lld Instanzen synchronisieren", &[&on, &all.len()])
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

pub(crate) fn switch(ctx: Ctx, id: String, before: Sharing, next: Sharing, running: bool, then: impl FnOnce(bool) + 'static) {
    let store = ctx.store;
    spawn_in_window(async move {
        let target = id.clone();
        let ok = match off_ui(move || share::set(&target, next, running)).await {
            Some(Ok(saved)) => {
                store.dispatch(AppEvent::InstanceChanged(saved));
                let undo = EventHandler::new(move |_| {
                    let id = id.clone();
                    spawn_in_window(async move {
                        match off_ui(move || share::set(&id, before, running)).await {
                            Some(Ok(saved)) => {
                                store.dispatch(AppEvent::InstanceChanged(saved));
                                ctx.refresh();
                            }
                            Some(Err(e)) => ctx.failed(tr("Nicht zurückgenommen."), &e),
                            None => ctx.unexplained(),
                        }
                    });
                });
                let text = match (next.any(), running) {
                    (true, true) => tr("Ab dem nächsten Start synchronisiert."),
                    (false, true) => tr("Eigene Dateien ab dem nächsten Start."),
                    (true, false) => tr("Synchronisiert."),
                    (false, false) => tr("Eigene Dateien, nicht mehr synchronisiert."),
                };
                let _ = toast::show(store.ui, text, Some(undo));
                true
            }
            Some(Err(e)) => {
                ctx.failed(tr("Nicht umgestellt."), &e);
                false
            }
            None => {
                ctx.unexplained();
                false
            }
        };
        ctx.refresh();
        then(ok);
    });
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
        let mut loaded = use_state(|| None::<Loaded>);
        let mut open = use_state(|| false);
        let mut only = use_state(|| None::<Item>);
        let ctx = Ctx { store, asked };
        use_side_effect(move || {
            let _ = asked.read();
            spawn_in_window(async move {
                let found = off_ui(Loaded::read).await;
                if loaded.try_peek().is_some()
                    && let Some(found) = found
                {
                    loaded.set(Some(found));
                }
            });
        });
        let who = whos(&store.model.read());
        let mut col = page::column();
        let Some(read) = loaded.read().clone() else { return page::scrolling(scroll, col) };
        if who.is_empty() {
            col = col.child(group(&t, None, vec![Line::new("sync.none", tr("Noch keine Instanzen")).hint(tr("Sync beginnt mit der ersten Instanz. Neue Instanzen synchronisieren automatisch.")).into_element()], None));
            return page::scrolling(scroll, col.child(auto_block(ctx)));
        }
        let set_up = who.iter().any(|w| w.sync.any()) || read.standing.iter().any(|s| s.present);
        if set_up {
            let mut lines = Vec::new();
            let undone = read.last.as_ref().is_none_or(|l| l.undone);
            let hint = match &read.last {
                Some(l) if !l.undone => trf("Zuletzt eingerichtet: Basis „%@“, %@, %lld Instanzen.", &[&l.base, &words::when(l.at), &l.instances]),
                _ => tr("Wähle eine Basis und bring weitere Instanzen auf ihren Stand. Was sie vorher hatten, liegt im Verlauf."),
            };
            let shown = *open.read();
            let toggle = Button::new("sync.setup.toggle", if shown { tr("Schließen") } else { tr("Neu einrichten …") }).on_press(move |_| {
                only.set(None);
                open.set(!shown);
            });
            let mut controls = rect().horizontal().spacing(crate::view::theme::space::S);
            if !undone {
                controls = controls.child(Button::new("sync.setup.undo", tr("Rückgängig")).plain().on_press(move |_| setup::undo(ctx)));
            }
            lines.push(Line::new("sync.setup", tr("Einrichten")).hint(hint).control(controls.child(toggle)).into_element());
            col = col.child(group(&t, None, lines, None));
            if *open.read() {
                col = col.child(rect().key(format!("{:?}", *only.read())).width(Size::fill()).child(SetupPanel { ctx, who: who.clone(), only: *only.read(), standing: read.standing.clone(), on_done: EventHandler::new(move |_| open.set(false)) }));
            }
            let change = EventHandler::new(move |item| {
                only.set(Some(item));
                open.set(true);
            });
            col = col.child(KindsBlock { ctx, loaded: read.clone(), who: who.clone(), change }).child(InstancesBlock { ctx, who }).child(auto_block(ctx));
        } else {
            col = col.child(group(&t, Some(tr("So funktioniert Sync")), vec![Line::new("sync.how", tr("Eine Basis, alle gleich")).hint(tr("Du wählst eine Instanz, deren Spiel-Optionen, Mod-Einstellungen und Serverliste die gemeinsamen werden. Die anderen Instanzen übernehmen sie; was sie vorher hatten, bleibt im Verlauf. Neue Instanzen synchronisieren automatisch.")).into_element()], None));
            col = col.child(SetupPanel { ctx, who, only: None, standing: read.standing.clone(), on_done: EventHandler::new(|_| {}) }).child(auto_block(ctx));
        }
        page::scrolling(scroll, col)
    }
}

fn auto_block(ctx: Ctx) -> Element {
    AutoBlock { ctx }.into_element()
}

#[derive(PartialEq)]
struct AutoBlock {
    ctx: Ctx,
}

impl Component for AutoBlock {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = self.ctx.store;
        let mut on = use_state(|| crate::core::settings::active().sync_new);
        let mut set = move |want: bool| {
            on.set(want);
            crate::core::settings::update(|s| s.sync_new = want);
            crate::view::settings::save(store);
        };
        let now = *on.read();
        let hint = if now { tr("Jede neue Instanz startet mit Sync an. Ihre erste Datei geht dann in den Verlauf, ersetzt wird beim ersten Start.") } else { tr("Neue Instanzen beginnen mit eigenen Dateien. Du kannst Sync pro Instanz einschalten.") };
        group(&t, None, vec![Line::new("sync.auto.row", tr("Neue Instanzen synchronisieren automatisch")).hint(hint).control(Switch::new("sync.auto", tr("Neue Instanzen synchronisieren automatisch"), now).on_toggle(set)).on_press(move |_| set(!now)).into_element()], None)
    }
}
