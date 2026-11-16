//! The sheets that restart Manycraft for the updater: into the ready update, or back to the previous version.

use freya::prelude::*;

use crate::app::model::UpdateStatus;
use crate::core::i18n::{tr, trf};
use crate::view::kit::group::status;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::use_theme;
use crate::view::store::{Store, use_store};
use crate::view::theme::frame;
use crate::view::updates::{self, Quit};

/// Restarts into the ready update; asks first while games run, since they come back afterwards.
pub fn restart(store: Store) {
    if !matches!(store.model.peek().updates.status, UpdateStatus::Ready { .. }) {
        return;
    }
    let (launching, running) = updates::counts(store);
    if launching == 0 && running == 0 {
        updates::quit(store, Quit::Install);
        return;
    }
    sheet::open(store.ui, Ask { how: Quit::Install });
}

/// Asks before going back to the version kept from the last update.
pub fn go_back(store: Store) {
    if store.model.peek().updates.previous.is_none() {
        return;
    }
    sheet::open(store.ui, Ask { how: Quit::Back });
}

#[derive(PartialEq)]
struct Ask {
    how: Quit,
}

impl Component for Ask {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let previous = store.model.read().updates.previous.clone().unwrap_or_default();
        let (_, running) = updates::counts(store);
        let blocked = updates::blocked(store);
        let how = self.how;
        let (id, heading, message, action, danger) = match how {
            Quit::Install => ("update.restart", tr("Jetzt neu starten?"), tr("Deine Spiele laufen weiter und erscheinen danach wieder hier."), tr("Neu starten"), false),
            Quit::Back => {
                let mut message = tr("Manycraft startet neu mit der vorherigen Version. Die neuere wird nicht mehr angeboten.");
                if running > 0 {
                    let stops = if running == 1 { tr("1 laufende Instanz wird beendet.") } else { trf("%lld laufende Instanzen werden beendet.", &[&running]) };
                    message = format!("{message} {stops}");
                }
                ("update.back", trf("Zu Version %@ zurückkehren?", &[&previous]), message, tr("Zurückkehren"), running > 0)
            }
        };
        let primary = Primary { label: action, disabled: blocked.clone(), danger, run: EventHandler::new(move |_| {
            sheet::close(store.ui);
            updates::quit(store, how);
        }) };
        let mut card = Card::new(id, heading).message(message).primary(primary).width(frame::SHEET_NARROW);
        if let Some(reason) = blocked {
            card = card.body(status(&t, reason, t.attention_text));
        }
        card
    }
}
