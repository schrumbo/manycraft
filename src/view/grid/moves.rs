//! Moving picked games to another batch and taking them out of theirs, from the selection bar, the palette and the chip
//! of banned accounts; running games stop first, and a toast takes the move back.

use std::cell::RefCell;
use std::rc::Rc;

use freya::prelude::*;
use uuid::Uuid;

use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::ui::{forms, remote};
use crate::view::accounts::off;
use crate::view::kit::Bounds;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::toast;
use crate::view::store::Store;

/// The accounts of `keys` grouped by the batch they play in, in the order of `keys`.
fn groups(store: Store, keys: &[String]) -> Vec<(String, Vec<Uuid>)> {
    let m = store.model.peek();
    let mut groups: Vec<(String, Vec<Uuid>)> = Vec::new();
    for g in keys.iter().filter_map(|k| m.games.get(k)).filter(|g| m.batch(&g.batch_id).is_some()) {
        match groups.iter_mut().find(|(b, _)| *b == g.batch_id) {
            Some((_, ids)) => ids.push(g.account_id),
            None => groups.push((g.batch_id.clone(), vec![g.account_id])),
        }
    }
    groups
}

/// Whether some of `keys` play in a batch, so they can leave it or move.
pub fn batched(store: Store, keys: &[String]) -> bool {
    !groups(store, keys).is_empty()
}

/// Stops the games of `keys` and takes their accounts out of their batches, with a toast to put them back.
pub fn leave(store: Store, keys: Vec<String>) {
    let groups = groups(store, &keys);
    if groups.is_empty() {
        return;
    }
    store.dispatch(AppEvent::Stop(keys));
    crate::view::accounts::take_out(store, groups);
}

/// Opens the menu of batches the games of `keys` can move to, under `at` or, from the palette, in the upper middle of the
/// window.
pub fn ask_move(store: Store, keys: Vec<String>, at: Option<Bounds>) {
    let items = move_items(store, keys);
    if items.is_empty() {
        return;
    }
    let menu = match at {
        Some(at) => Menu::under(items, at),
        None => {
            let (w, h) = *store.ui.window.peek();
            Menu::at(items, (w / 2. - 120., h / 3.))
        }
    };
    menu::open(store.ui, menu);
}

fn move_items(store: Store, keys: Vec<String>) -> Vec<Item> {
    let groups = groups(store, &keys);
    let m = store.model.peek();
    let mut batches: Vec<(String, String, bool)> = m.batches.iter().map(|b| (b.id.clone(), b.name.clone(), groups.iter().all(|(from, _)| *from == b.id))).collect();
    batches.sort_by_key(|(_, name, _)| name.to_lowercase());
    batches
        .into_iter()
        .map(|(id, name, there)| {
            let keys = keys.clone();
            let to = id.clone();
            Item::new(format!("grid.move.{id}"), name, move |_| move_to(store, to.clone(), keys.clone())).checked(there).disabled(there)
        })
        .collect()
}

/// Moves the accounts of `keys` from their batches into batch `to`, stopping those that run; a toast moves them back.
pub fn move_to(store: Store, to: String, keys: Vec<String>) {
    let groups: Vec<(String, Vec<Uuid>)> = groups(store, &keys).into_iter().filter(|(from, _)| *from != to).collect();
    if groups.is_empty() {
        return;
    }
    let running: Vec<String> = {
        let m = store.model.peek();
        let moving: Vec<&Uuid> = groups.iter().flat_map(|(_, ids)| ids).collect();
        keys.iter().filter(|k| m.games.get(*k).is_some_and(|g| g.batch_id != to && moving.contains(&&g.account_id) && g.phase.is_active())).cloned().collect()
    };
    if !running.is_empty() {
        store.dispatch(AppEvent::Stop(running));
    }
    let name = store.model.peek().batch(&to).map(|b| b.name.clone()).unwrap_or_default();
    let target = to.clone();
    off("move-batch", move || {
        let mut ids: Vec<Uuid> = groups.iter().flat_map(|(_, ids)| ids.iter().copied()).collect();
        ids.sort();
        ids.dedup();
        let added = remote::join_batch(&target, &ids)?;
        let mut left = Vec::new();
        for (from, ids) in &groups {
            match remote::leave_batch(from, ids) {
                Ok(places) if places.is_empty() => {}
                Ok(places) => left.push((from.clone(), places)),
                Err(e) => {
                    let back = remote::leave_batch(&target, &added).and_then(|_| left.iter().try_for_each(|(from, places)| remote::rejoin_batch(from, places)));
                    return Err(match back {
                        Ok(()) => e,
                        Err(b) => b.context(tr("Ein Teil wurde verschoben und ließ sich nicht zurücknehmen. Prüfe die Batches.")),
                    });
                }
            }
        }
        Ok((added, left))
    }, move |result: anyhow::Result<(Vec<Uuid>, Vec<(String, Vec<(Uuid, usize)>)>)>| {
        store.reload(true);
        let (added, left) = match result {
            Ok(done) => done,
            Err(e) => {
                toast::show(store.ui, forms::explain_as(tr("Die Accounts wurden nicht verschoben."), &e).text, None);
                return;
            }
        };
        let n: usize = left.iter().map(|(_, places)| places.len()).sum::<usize>().max(added.len());
        let text = if n == 1 { trf("1 Account nach „%@“ verschoben", &[&name]) } else { trf("%1$lld Accounts nach „%2$@“ verschoben", &[&n, &name]) };
        let back = Rc::new(RefCell::new(Some((added, left))));
        let id = toast::show(store.ui, text, Some(EventHandler::new(move |_| {
            let Some((added, left)) = back.borrow_mut().take() else { return };
            let to = to.clone();
            off("move-batch-back", move || {
                remote::leave_batch(&to, &added)?;
                left.iter().try_for_each(|(from, places)| remote::rejoin_batch(from, places))
            }, move |result| {
                if let Err(e) = result {
                    toast::show(store.ui, forms::explain_as(tr("Die Accounts wurden nicht zurückverschoben."), &e).text, None);
                }
                store.reload(true);
            });
        })));
        crate::view::commands::name_undo(store, id, tr("In Batch verschieben"));
    });
}
