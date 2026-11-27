//! "Sync": whether this instance takes part in the sync of the other instances or keeps its own files.

use freya::prelude::*;

use super::Facts;
use crate::app::event::Event as AppEvent;
use crate::core::i18n::tr;
use crate::core::share;
use crate::view::kit::group::{Line, group};
use crate::view::kit::switch::Switch as Toggle;
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::store::use_store;
use crate::view::sync::Ctx;

/// The sync row of the Einstellungen tab; nothing while the sync is off.
#[derive(PartialEq)]
pub struct Sharing {
    pub facts: Facts,
}

impl Component for Sharing {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ctx = Ctx { store, asked: use_state(|| 0u32) };
        let mut on = use_state(|| false);
        let mut working = use_state(|| false);
        use_side_effect(move || {
            spawn_in_window(async move {
                let found = off_ui(|| share::state().on).await;
                if on.try_peek().is_some()
                    && let Some(found) = found
                {
                    on.set(found);
                }
            });
        });
        if !*on.read() {
            return rect().into_element();
        }
        let made = &self.facts.made;
        let joined = !made.sync_out;
        let running = self.facts.running;
        let hint = if *working.read() {
            tr("Wird umgestellt …")
        } else if joined {
            tr("Teilt, was auf der Seite Sync eingeschaltet ist.")
        } else {
            tr("Behält ihre eigenen Dateien.")
        };
        let id = made.id.clone();
        let mut flip = move |want: bool| {
            if *working.peek() {
                return;
            }
            working.set(true);
            let id = id.clone();
            spawn_in_window(async move {
                match off_ui(move || share::set_opt_out(&id, !want, running)).await {
                    Some(Ok(saved)) => {
                        store.dispatch(AppEvent::InstanceChanged(saved));
                        if running {
                            let _ = toast::show(store.ui, tr("Gilt ab dem nächsten Start."), None);
                        }
                    }
                    Some(Err(e)) => ctx.failed(tr("Nicht umgestellt."), &e),
                    None => ctx.unexplained(),
                }
                if working.try_peek().is_some() {
                    working.set(false);
                }
            });
        };
        let mut press = flip.clone();
        let line = Line::new("instance.settings.sync.row", tr("Mit den anderen Instanzen synchronisieren")).hint(hint).control(Toggle::new("instance.settings.sync", tr("Mit den anderen Instanzen synchronisieren"), joined).on_toggle(move |want| flip(want))).on_press(move |_| press(!joined));
        group(&t, Some(tr("Sync")), vec![line.into_element()], None)
    }
}
