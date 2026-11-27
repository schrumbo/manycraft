//! The one-time setup: pick the instance whose files become the shared ones and turn the sync on.

use freya::prelude::*;

use super::{Ctx, Who, running};
use crate::core::i18n::{tr, trf};
use crate::core::share;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::select::Select;
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::library::{self, Sort};
use crate::view::theme::control;

#[derive(PartialEq)]
pub(super) struct SetupPanel {
    pub ctx: Ctx,
    pub who: Vec<Who>,
}

impl Component for SetupPanel {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ctx = self.ctx;
        let mut picked = use_state(|| None::<String>);
        let busy = use_state(|| false);
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
        let chosen_id = picked.read().clone().filter(|id| self.who.iter().any(|w| w.id == *id)).unwrap_or_else(|| first.id.clone());
        let chosen = self.who.iter().find(|w| w.id == chosen_id).unwrap_or(first);
        let labels: Vec<String> = by_played.iter().map(|w| format!("{} · {}", w.name, w.version)).collect();
        let at = by_played.iter().position(|w| w.id == chosen_id).unwrap_or(0);
        let ids: Vec<String> = by_played.iter().map(|w| w.id.clone()).collect();
        let pick = Select::new("sync.setup.base", tr("Vorlage"), labels, at).width(control::VERSION_MENU).on_choose(move |i| picked.set(ids.get(i).cloned()));
        let row = Line::new("sync.setup.base.row", tr("Vorlage")).hint(tr("Ihre Dateien werden der erste gemeinsame Stand. Zuletzt gespielte Instanzen stehen oben.")).control(pick);
        let reason = if *busy.read() {
            Some(tr("Wird eingerichtet …"))
        } else if chosen.running {
            Some(trf("Schließ zuerst die Instanz „%@“.", &[&chosen.name]))
        } else {
            None
        };
        let (id, name, running_ids) = (chosen_id.clone(), chosen.name.clone(), running(&self.who));
        let go = Button::new("sync.setup.go", tr("Sync einschalten")).primary().disabled(reason).on_press(move |_| {
            if *busy.peek() {
                return;
            }
            run(ctx, id.clone(), name.clone(), running_ids.clone(), busy);
        });
        let sum = Line::new("sync.setup.sum", trf("„%@“ wird die Vorlage.", &[&chosen.name])).hint(tr("Spiel-Optionen, Mod-Einstellungen, Welten und Screenshots. Danach kannst du jedes davon einzeln ausschalten.")).control(go);
        rect().width(Size::fill()).spacing(crate::view::theme::frame::SECTION).child(group(&t, Some(tr("Vorlage")), vec![row.into_element()], None)).child(group(&t, None, vec![sum.into_element()], None)).into_element()
    }
}

fn run(ctx: Ctx, id: String, name: String, running: Vec<String>, mut busy: State<bool>) {
    busy.set(true);
    let store = ctx.store;
    spawn_in_window(async move {
        match off_ui(move || share::setup(&id, &running)).await {
            Some(Ok(())) => {
                let _ = toast::show(store.ui, trf("Sync ist an. „%@“ ist die Vorlage.", &[&name]), None);
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
