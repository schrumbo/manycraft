//! What the grid keeps while it is not shown: whether the page of all groups by batch, the folded groups, and the last
//! mass stop, which stays undoable until the next mass action or a manual start.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use freya::prelude::*;

use crate::app::event::Event;
use crate::app::model::Model;
use crate::core::i18n::{tr, trf};
use crate::view::kit::toast;
use crate::view::store::Store;
use crate::view::ui::{SessionUndo, remember};

const KEY: &str = "grid.mass";

static NEXT: AtomicU64 = AtomicU64::new(1);

/// The last mass stop: which games, when, and how the menu names its undo.
#[derive(Clone, Debug, PartialEq)]
pub struct Mass {
    pub id: u64,
    pub keys: Vec<String>,
    pub at: u64,
    pub what: String,
}

/// The grid's view state.
#[derive(Clone, Copy, PartialEq)]
pub struct Shared {
    /// Whether the page of all games groups them by batch once there are two batches.
    pub grouped: State<bool>,
    /// The batches whose group is folded to its header.
    pub folded: State<HashSet<String>>,
    pub mass: State<Option<Mass>>,
}

impl Shared {
    pub fn create() -> Self {
        Self { grouped: State::create(true), folded: State::create(HashSet::new()), mass: State::create(None) }
    }
}

/// "15 in Farm gestoppt" and "Farm stoppen (15)" for the games of `keys`, naming the batch when they share one; one
/// game by its name.
pub fn words(m: &Model, keys: &[String]) -> (String, String) {
    if let [one] = keys {
        return m.games.get(one).map_or_else(|| (tr("Gestoppt"), tr("Stoppen")), |g| (trf("%@ gestoppt", &[&g.name]), trf("%@ stoppen", &[&g.name])));
    }
    let n = keys.len();
    let first = keys.first().and_then(|k| m.games.get(k)).map(|g| g.batch_id.as_str());
    let shared = first.filter(|b| keys.iter().all(|k| m.games.get(k).is_some_and(|g| g.batch_id == *b))).and_then(|b| m.batch(b));
    match shared {
        Some(b) => (trf("%1$lld in %2$@ gestoppt", &[&n, &b.name]), trf("%1$@ stoppen (%2$lld)", &[&b.name, &n])),
        None => (trf("%lld gestoppt", &[&n]), trf("%lld stoppen", &[&n])),
    }
}

/// Offers to start the games of a mass stop again: in a toast, then from the menu, the palette and Ctrl+Z until the
/// next mass action or a manual start.
pub fn stopped(store: Store, keys: Vec<String>) {
    let (text, what) = words(&store.model.peek(), &keys);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let back = keys.clone();
    let undo = EventHandler::new(move |_| {
        forget(store, Some(id));
        store.dispatch(Event::StartAgain(back.clone()));
    });
    let mut shared = store.ui.grid_view;
    shared.mass.set(Some(Mass { id, keys, at, what: what.clone() }));
    let mut session = store.ui.session_undo;
    remember(&mut session.write(), SessionUndo { key: KEY.into(), label: trf("Rückgängig: %@", &[&what]), run: undo.clone() });
    let toast = toast::show(store.ui, text, Some(undo));
    crate::view::commands::name_undo(store, toast, what);
}

/// Drops the undo of the last mass stop, or only of mass stop `id` when it is still the last.
pub fn forget(store: Store, id: Option<u64>) {
    let mut shared = store.ui.grid_view;
    if shared.mass.peek().as_ref().is_none_or(|m| id.is_some_and(|id| m.id != id)) {
        return;
    }
    shared.mass.set(None);
    let mut session = store.ui.session_undo;
    if session.peek().iter().any(|u| u.key == KEY) {
        session.write().retain(|u| u.key != KEY);
    }
}

/// Whether `event` ends the undo of the last mass stop: another mass action or a start by hand.
pub fn ends(event: &Event) -> bool {
    matches!(
        event,
        Event::Start(_) | Event::StartMany(_) | Event::StartBatch(_) | Event::StartAll | Event::Restart(_) | Event::PlayInstance { .. } | Event::QuickPlay { .. } | Event::ReconnectMany(_) | Event::StartAnyway(_)
    )
}

/// Runs the undo of the last mass stop.
pub fn undo(store: Store) {
    let run = store.ui.session_undo.peek().iter().find(|u| u.key == KEY).map(|u| u.run.clone());
    if let Some(run) = run {
        run.call(());
    }
}
