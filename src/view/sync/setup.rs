//! The setup: what to synchronize, which instance is the base and which instances take its state over.

use std::collections::BTreeSet;

use freya::prelude::*;

use super::words::{blurb, effective, label, names};
use super::{Ctx, Who};
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::core::share::{self, ITEMS, Item, Setup, Sharing, Standing};
use crate::view::kit::button::Button;
use crate::view::kit::check;
use crate::view::kit::group::{Line, group};
use crate::view::kit::select::Select;
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::library::{self, Sort};
use crate::view::theme::{control, space};

/// The three steps in one card: what, from whom, and for whom.
#[derive(PartialEq)]
pub(super) struct SetupPanel {
    pub ctx: Ctx,
    pub who: Vec<Who>,
    pub only: Option<Item>,
    pub standing: Vec<Standing>,
    pub on_done: EventHandler<()>,
}

impl Component for SetupPanel {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ctx = self.ctx;
        let only = self.only;
        let mut kinds = use_state(move || only.map_or(Sharing::ALL, |i| Sharing::NONE.with(i, true)));
        let mut base = use_state(|| None::<String>);
        let mut chosen = use_state(|| None::<BTreeSet<String>>);
        let busy = use_state(|| false);
        let wanted = *kinds.read();
        let infos = ctx.store.ui.infos.read().clone();
        let by_played: Vec<&Who> = {
            let m = ctx.store.model.read();
            let list: Vec<&crate::core::instance::Instance> = m.instances.iter().collect();
            let order: Vec<String> = library::ordered(list, Sort::Played, &infos).into_iter().map(|i| i.id.clone()).collect();
            let mut v: Vec<&Who> = self.who.iter().collect();
            v.sort_by_key(|w| order.iter().position(|id| *id == w.id).unwrap_or(usize::MAX));
            v
        };
        let Some(first) = by_played.first() else { return rect().into_element() };
        let base_id = base.read().clone().filter(|id| self.who.iter().any(|w| w.id == *id)).unwrap_or_else(|| first.id.clone());
        let base_who = self.who.iter().find(|w| w.id == base_id).unwrap_or(first);
        let travels = share::options_travel(&base_who.version);

        let kind_rows: Vec<Element> = ITEMS
            .into_iter()
            .map(|item| {
                let on = wanted.get(item);
                let key = format!("sync.setup.kind.{}", label(item));
                Line::new(key, label(item))
                    .hint(blurb(item))
                    .control(check::mark(&t, on))
                    .on_press(move |_| {
                        let next = kinds.peek().with(item, !on);
                        kinds.set(next);
                    })
                    .into_element()
            })
            .collect();
        let kinds_group = group(&t, Some(tr("Was wird synchronisiert")), kind_rows, None);

        let labels: Vec<String> = by_played.iter().map(|w| format!("{} · {}", w.name, w.version)).collect();
        let at = by_played.iter().position(|w| w.id == base_id).unwrap_or(0);
        let ids: Vec<String> = by_played.iter().map(|w| w.id.clone()).collect();
        let pick = Select::new("sync.setup.base", tr("Basis"), labels, at).width(control::VERSION_MENU).on_choose(move |i| base.set(ids.get(i).cloned()));
        let base_group = group(&t, Some(tr("Basis")), vec![Line::new("sync.setup.base.row", tr("Instanz, von der der Stand kommt")).hint(tr("Ihre Spiel-Optionen, Mod-Einstellungen und Serverliste werden die gemeinsamen. Zuletzt gespielte Instanzen stehen oben.")).control(pick).into_element()], None);

        let targets: Vec<&Who> = self.who.iter().filter(|w| w.id != base_id).collect();
        let missing = |w: &Who| -> Vec<Item> {
            let now = effective(w.sync, &w.version);
            ITEMS.into_iter().filter(|i| wanted.get(*i) && !now.get(*i) && (*i != Item::Options || share::options_travel(&w.version))).collect()
        };
        let default: BTreeSet<String> = targets.iter().filter(|w| !missing(w).is_empty()).map(|w| w.id.clone()).collect();
        let picked: BTreeSet<String> = chosen.read().clone().unwrap_or_else(|| default.clone());
        let mut rows: Vec<Element> = Vec::new();
        for w in &targets {
            let gap = missing(w);
            let id = format!("sync.setup.target.{}", w.id);
            if gap.is_empty() {
                rows.push(Line::new(id, w.name.clone()).hint(tr("Synchronisiert schon alles Gewählte. Nichts zu ersetzen.")).dim(true).into_element());
                continue;
            }
            let on = picked.contains(&w.id);
            let hint = trf("Eigene Dateien gehen in den Verlauf: %@.", &[&names(&gap)]);
            let (wid, current) = (w.id.clone(), picked.clone());
            rows.push(
                Line::new(id, w.name.clone())
                    .hint(hint)
                    .control(check::mark(&t, on))
                    .on_press(move |_| {
                        let mut next = current.clone();
                        if !next.remove(&wid) {
                            next.insert(wid.clone());
                        }
                        chosen.set(Some(next));
                    })
                    .into_element(),
            );
        }
        let selectable: BTreeSet<String> = targets.iter().filter(|w| !missing(w).is_empty()).map(|w| w.id.clone()).collect();
        let count = picked.iter().filter(|id| selectable.contains(*id)).count();
        if !selectable.is_empty() {
            let all = selectable.clone();
            let none = Button::new("sync.setup.none", tr("Keine")).plain().disabled((count == 0).then(|| tr("Keine ausgewählt."))).on_press(move |_| chosen.set(Some(BTreeSet::new())));
            let every = Button::new("sync.setup.all", tr("Alle")).plain().disabled((count == selectable.len()).then(|| tr("Alle sind ausgewählt."))).on_press(move |_| chosen.set(Some(all.clone())));
            rows.push(rect().width(Size::fill()).padding((space::S, space::L)).horizontal().main_align(Alignment::End).spacing(space::S).child(none).child(every).into_element());
        }
        let targets_group = if rows.is_empty() { None } else { Some(group(&t, Some(tr("Instanzen angleichen")), rows, Some(tr("Ihre Spiel-Optionen, Mod-Einstellungen und Serverliste werden durch den Stand der Basis ersetzt. Vorher landen sie im Verlauf, und „Rückgängig“ holt sie zurück.")))) };

        let running = targets.iter().any(|w| w.running && picked.contains(&w.id)) || base_who.running;
        let reason = if *busy.read() {
            Some(tr("Wird eingerichtet …"))
        } else if !wanted.any() {
            Some(tr("Wähle, was synchronisiert werden soll."))
        } else if wanted.options && !travels {
            Some(trf("„%@“ ist älter als Minecraft 1.13, ihre Spiel-Optionen lassen sich nicht übernehmen. Wähle eine andere Basis oder nimm die Spiel-Optionen heraus.", &[&base_who.name]))
        } else {
            None
        };
        let items: Vec<Item> = ITEMS.into_iter().filter(|i| wanted.get(*i)).collect();
        let summary = trf("„%@“ wird die Basis für %@.", &[&base_who.name, &names(&items)]);
        let mut detail = if count == 0 { tr("Keine weitere Instanz wird verändert.") } else { trf("Bei %lld Instanzen werden die eigenen Dateien ersetzt. Gleichnamige Einträge der Basis ersetzen die gemeinsamen, alles andere bleibt. Gelöscht wird nichts.", &[&count]) };
        if running {
            detail = format!("{detail} {}", tr("Bei laufenden Spielen gelten Mod-Einstellungen ab dem nächsten Start."));
        }
        let label_go = if count == 0 { tr("Als Basis übernehmen") } else if count == 1 { tr("Bei 1 Instanz ersetzen") } else { trf("Bei %lld Instanzen ersetzen", &[&count]) };
        let (go_base, go_ids, running_ids): (String, Vec<String>, Vec<String>) = (base_id.clone(), picked.iter().filter(|id| selectable.contains(*id)).cloned().collect(), self.who.iter().filter(|w| w.running).map(|w| w.id.clone()).collect());
        let on_done = self.on_done.clone();
        let go = Button::new("sync.setup.go", label_go).primary().disabled(reason).on_press(move |_| {
            if *busy.peek() {
                return;
            }
            run(ctx, Setup { base: go_base.clone(), items: *kinds.peek(), replace: go_ids.clone() }, running_ids.clone(), busy, on_done.clone());
        });
        let sum = group(&t, None, vec![Line::new("sync.setup.sum", summary).hint(detail).control(go).into_element()], None);
        let mut col = rect().width(Size::fill()).spacing(crate::view::theme::frame::SECTION).child(kinds_group).child(base_group);
        if let Some(g) = targets_group {
            col = col.child(g);
        }
        col.child(sum).into_element()
    }
}

fn run(ctx: Ctx, request: Setup, running: Vec<String>, mut busy: State<bool>, done: EventHandler<()>) {
    busy.set(true);
    let store = ctx.store;
    let back = running.clone();
    spawn_in_window(async move {
        let base = store.model.peek().instance(&request.base).map(|i| i.name.clone()).unwrap_or_default();
        match off_ui(move || share::setup(&request, &running)).await {
            Some(Ok(result)) => {
                let count = result.instances.len();
                for made in result.instances {
                    store.dispatch(AppEvent::InstanceChanged(made));
                }
                let mut text = if count > 1 { trf("„%@“ ist die Basis, %lld Instanzen sind angeglichen.", &[&base, &count]) } else { trf("„%@“ ist die Basis.", &[&base]) };
                if !result.nothing.is_empty() {
                    text = format!("{text} {}", trf("Davon hatte sie nichts: %@.", &[&names(&result.nothing)]));
                }
                let undo = EventHandler::new(move |_| undo_with(ctx, back.clone()));
                let _ = toast::show_kept(store.ui, text, undo);
                done.call(());
            }
            Some(Err(e)) => ctx.failed(tr("Nicht eingerichtet."), &e),
            None => ctx.unexplained(),
        }
        if busy.try_peek().is_some() {
            busy.set(false);
        }
        ctx.refresh();
    });
}

/// Takes back the last setup.
pub(super) fn undo(ctx: Ctx) {
    let running: Vec<String> = super::whos(&ctx.store.model.peek()).into_iter().filter(|w| w.running).map(|w| w.id).collect();
    undo_with(ctx, running);
}

fn undo_with(ctx: Ctx, running: Vec<String>) {
    let store = ctx.store;
    spawn_in_window(async move {
        match off_ui(move || share::undo_setup(&running)).await {
            Some(Ok(made)) => {
                for one in made {
                    store.dispatch(AppEvent::InstanceChanged(one));
                }
                let _ = toast::show(store.ui, tr("Einrichtung zurückgenommen."), None);
            }
            Some(Err(e)) => ctx.failed(tr("Nicht zurückgenommen."), &e),
            None => ctx.unexplained(),
        }
        ctx.refresh();
    });
}
