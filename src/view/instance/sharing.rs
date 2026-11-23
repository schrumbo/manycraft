//! "Mit anderen Instanzen teilen": which settings this instance takes from and gives to the other sharing instances.

use freya::prelude::*;

use super::Facts;
use crate::app::event::Event as AppEvent;
use crate::core::i18n::tr;
use crate::core::share::{self, ITEMS, Item, Sharing as Shared};
use crate::ui::forms;
use crate::view::kit::group::{Line, group};
use crate::view::kit::switch::Switch as Toggle;
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::store::{Store, use_store};

/// The sharing group of the Einstellungen tab.
#[derive(PartialEq)]
pub struct Sharing {
    pub facts: Facts,
}

impl Component for Sharing {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let pending = use_state(|| None::<Shared>);
        let made = &self.facts.made;
        let now = pending.read().unwrap_or(made.sync);
        let saving = pending.read().is_some();
        let rows = ITEMS.into_iter().map(|item| item_row(store, pending, &made.id, now, item, saving)).collect();
        let mut footer = vec![tr("Beim Einschalten bekommt die Instanz beim nächsten Start die geteilten Einstellungen. Ihre eigenen bleiben unter „Frühere Stände“ in den App-Einstellungen.")];
        if self.facts.running {
            footer.push(tr("Laufende Spiele geben ihre Änderungen weiter, sobald sie gespeichert sind."));
        }
        group(&t, Some(tr("Mit anderen Instanzen teilen")), rows, Some(footer.join(" ")))
    }
}

fn item_row(store: Store, pending: State<Option<Shared>>, id: &str, now: Shared, item: Item, saving: bool) -> Element {
    let (key, label, hint) = match item {
        Item::Options => ("options", tr("Spiel-Optionen"), tr("Grafik, Steuerung und Ton. Vollbild, Fenster und Ressourcenpakete bleiben pro Instanz.")),
        Item::Config => ("config", tr("Mod-Einstellungen"), tr("Der Ordner „config“. Dateien, die nur diese Instanz hat, bleiben.")),
        Item::Servers => ("servers", tr("Serverliste"), tr("Die Liste unter Mehrspieler.")),
    };
    let on = now.get(item);
    let id = id.to_owned();
    let flip = move |want: bool| set(store, pending, id.clone(), now.with(item, want));
    let mut line = Line::new(format!("instance.settings.share.{key}.row"), label.clone()).hint(hint).control(Toggle::new(format!("instance.settings.share.{key}"), label, on).disabled(saving).on_toggle(flip.clone()));
    if !saving {
        line = line.on_press(move |_| flip(!on));
    }
    line.into_element()
}

fn set(store: Store, mut pending: State<Option<Shared>>, id: String, next: Shared) {
    if pending.peek().is_some() {
        return;
    }
    pending.set(Some(next));
    spawn(async move {
        match off_ui(move || share::set(&id, next)).await {
            Some(Ok(saved)) => store.dispatch(AppEvent::InstanceChanged(saved)),
            Some(Err(e)) => {
                let _ = toast::show(store.ui, forms::explain_as(tr("Nicht umgestellt."), &e).text, None);
            }
            None => {
                let _ = toast::show(store.ui, forms::unexplained(), None);
            }
        }
        if pending.try_peek().is_some() {
            pending.set(None);
        }
    });
}