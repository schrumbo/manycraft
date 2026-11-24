//! "Mit anderen Instanzen teilen": which settings this instance takes from the one shared file, said in words.

use freya::prelude::*;

use super::Facts;
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{self, tr, trf};
use crate::core::share::{self, ITEMS, Item, Note, Sharing as Shared};
use crate::ui::forms;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::switch::Switch as Toggle;
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::native;
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
        let mut switched = use_state(Vec::<Item>::new);
        let mut notes = use_state(Vec::<Note>::new);
        use_side_effect(move || {
            spawn(async move {
                if let Some(found) = off_ui(share::notes).await
                    && notes.try_peek().is_some()
                {
                    notes.set(found);
                }
            });
        });
        let made = &self.facts.made;
        let now = pending.read().unwrap_or(made.sync);
        let saving = pending.read().is_some();
        let old = !share::options_travel(&made.game_version);
        let others = sharers(store, Some(&made.id));
        let running = self.facts.running;
        if !running && !switched.peek().is_empty() {
            spawn(async move { switched.set(Vec::new()) });
        }
        let later = if running { switched.read().clone() } else { Vec::new() };
        let mut rows: Vec<Element> = ITEMS.into_iter().map(|item| item_row(store, pending, switched, &made.id, now, made.sync, item, saving, old && item == Item::Options, others[index(item)].len(), running, later.contains(&item))).collect();
        if let Some(note) = notes.read().first() {
            rows.push(note_line(&t, "instance.settings.share.note", note, notes));
        }
        rows.push(folder_line("instance.settings.share.folder"));
        let footer = if running { tr("Ein Spiel dieser Instanz läuft. Was du umstellst, gilt ab seinem nächsten Start.") } else { tr("Was du in einem teilenden Spiel änderst, gilt für alle teilenden Instanzen. Frühere Stände findest du in den App-Einstellungen unter „Teilen“.") };
        group(&t, Some(tr("Mit anderen Instanzen teilen")), rows, Some(footer))
    }
}

/// The name of `item` as the settings list it.
pub(crate) fn label(item: Item) -> String {
    match item {
        Item::Options => tr("Spiel-Optionen"),
        Item::Config => tr("Mod-Einstellungen"),
        Item::Servers => tr("Serverliste"),
    }
}

pub(crate) fn index(item: Item) -> usize {
    ITEMS.iter().position(|i| *i == item).unwrap_or(0)
}

/// The names of the instances that share each item, but `except`, in the order of [`ITEMS`].
pub(crate) fn sharers(store: Store, except: Option<&str>) -> [Vec<String>; 3] {
    let model = store.model.read();
    ITEMS.map(|item| model.instances.iter().filter(|i| Some(i.id.as_str()) != except && i.sync.get(item) && (item != Item::Options || share::options_travel(&i.game_version))).map(|i| i.name.clone()).collect())
}

/// A row saying what sharing restored on its own, with a way to say it was seen.
pub(crate) fn note_line(t: &crate::view::theme::Theme, id: &str, note: &Note, mut notes: State<Vec<Note>>) -> Element {
    let when = chrono::DateTime::from_timestamp_millis(note.at).map(|d| i18n::date_time(d.with_timezone(&chrono::Local).naive_local())).unwrap_or_default();
    let what = if note.files.len() == 1 { trf("„%@“", &[&note.files[0]]) } else { trf("%@ Mod-Einstellungen", &[&note.files.len()]) };
    let hint = trf("Der Start von „%@“ hat %@ zurückgesetzt oder beschädigt (%@). Der Stand davor ist wieder da.", &[&note.instance, &what, &when]);
    let seen = Button::new(format!("{id}.seen"), tr("Verstanden")).plain().on_press(move |_| {
        notes.set(Vec::new());
        spawn(async move {
            let _ = off_ui(share::clear_notes).await;
        });
    });
    Line::new(id.to_owned(), tr("Automatisch wiederhergestellt")).hint(hint).hint_tone(t.attention_text).control(seen).into_element()
}

/// A row that opens the folder holding the shared files.
pub(crate) fn folder_line(id: &str) -> Element {
    let open = Button::new(format!("{id}.open"), tr("Ordner öffnen")).plain().on_press(|_| native::open(&share::folder()));
    Line::new(id.to_owned(), tr("Gemeinsame Dateien")).hint(tr("Eine Datei für alle teilenden Instanzen, keine Kopien.")).control(open).into_element()
}

#[allow(clippy::too_many_arguments)]
fn item_row(store: Store, pending: State<Option<Shared>>, switched: State<Vec<Item>>, id: &str, now: Shared, saved: Shared, item: Item, saving: bool, unreachable: bool, others: usize, running: bool, later: bool) -> Element {
    let key = match item {
        Item::Options => "options",
        Item::Config => "config",
        Item::Servers => "servers",
    };
    let label = label(item);
    let on = now.get(item) && !unreachable;
    let state = if unreachable {
        tr("Erst ab Minecraft 1.13. Ältere Versionen speichern Tasten anders und behalten ihre eigene Datei.")
    } else if saving && now.get(item) != saved.get(item) {
        tr("Wird umgestellt …")
    } else if later && on {
        tr("Geteilt ab dem nächsten Start.")
    } else if later {
        tr("Eigene Datei ab dem nächsten Start.")
    } else if on && others == 0 {
        tr("Gemeinsame Datei. Noch teilt keine andere Instanz.")
    } else if on && others == 1 {
        tr("Geteilt mit einer anderen Instanz.")
    } else if on {
        trf("Geteilt mit %@ anderen Instanzen.", &[&others])
    } else {
        tr("Eigene Datei, nur für diese Instanz.")
    };
    let what = match item {
        Item::Options if unreachable => String::new(),
        Item::Options => tr("Grafik, Steuerung und Ton. Fenster und Ressourcenpakete bleiben pro Instanz."),
        Item::Config => tr("Der Ordner „config“ aller Mods."),
        Item::Servers => tr("Die Liste unter Mehrspieler."),
    };
    let hint = if what.is_empty() { state } else { format!("{state} {what}") };
    let locked = saving || unreachable;
    let id = id.to_owned();
    let flip = move |want: bool| set(store, pending, switched, id.clone(), now, now.with(item, want), item, running);
    let mut line = Line::new(format!("instance.settings.share.{key}.row"), label.clone()).hint(hint).control(Toggle::new(format!("instance.settings.share.{key}"), label, on).disabled(locked).on_toggle(flip.clone()));
    if !locked {
        line = line.on_press(move |_| flip(!on));
    }
    line.into_element()
}

/// What happened after `item` was turned on or off, short enough for a toast with an undo.
fn outcome(item: Item, on: bool, running: bool) -> String {
    match (item, on, running) {
        (_, true, true) => tr("Ab dem nächsten Start geteilt."),
        (_, false, true) => tr("Eigene Kopie ab dem nächsten Start."),
        (Item::Options, true, false) => tr("Spiel-Optionen werden geteilt."),
        (Item::Config, true, false) => tr("Mod-Einstellungen werden geteilt."),
        (Item::Servers, true, false) => tr("Die Serverliste wird geteilt."),
        (Item::Options, false, false) => tr("Eigene Kopie der Spiel-Optionen."),
        (Item::Config, false, false) => tr("Eigene Kopie der Mod-Einstellungen."),
        (Item::Servers, false, false) => tr("Eigene Kopie der Serverliste."),
    }
}

/// Marks `item` as switched while a game runs, or unmarks it when it is switched back.
fn mark(mut switched: State<Vec<Item>>, item: Item) {
    let mut next = switched.peek().clone();
    if let Some(at) = next.iter().position(|i| *i == item) {
        next.remove(at);
    } else {
        next.push(item);
    }
    switched.set(next);
}

#[allow(clippy::too_many_arguments)]
fn set(store: Store, mut pending: State<Option<Shared>>, switched: State<Vec<Item>>, id: String, before: Shared, next: Shared, item: Item, running: bool) {
    if pending.peek().is_some() {
        return;
    }
    pending.set(Some(next));
    spawn(async move {
        let target = id.clone();
        match off_ui(move || share::set(&target, next, running)).await {
            Some(Ok(saved)) => {
                store.dispatch(AppEvent::InstanceChanged(saved));
                if running && switched.try_peek().is_some() {
                    mark(switched, item);
                }
                let undo = EventHandler::new(move |_| take_back(store, switched, id.clone(), before, item, running));
                let _ = toast::show(store.ui, outcome(item, next.get(item), running), Some(undo));
            }
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
fn take_back(store: Store, switched: State<Vec<Item>>, id: String, before: Shared, item: Item, running: bool) {
    spawn(async move {
        match off_ui(move || share::set(&id, before, running)).await {
            Some(Ok(saved)) => {
                store.dispatch(AppEvent::InstanceChanged(saved));
                if running && switched.try_peek().is_some() {
                    mark(switched, item);
                }
            }
            Some(Err(e)) => {
                let _ = toast::show(store.ui, forms::explain_as(tr("Nicht zurückgenommen."), &e).text, None);
            }
            None => {
                let _ = toast::show(store.ui, forms::unexplained(), None);
            }
        }
    });
}