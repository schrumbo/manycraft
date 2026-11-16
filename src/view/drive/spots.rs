//! The named elements on screen, for the drive tool to find, click and read.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use freya::prelude::*;

use crate::view::kit::Bounds;

/// One named element: what it is, what it says and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    pub id: String,
    pub role: &'static str,
    pub label: String,
    pub bounds: Bounds,
    pub enabled: bool,
}

thread_local! {
    static SPOTS: RefCell<BTreeMap<String, Spot>> = const { RefCell::new(BTreeMap::new()) };
    static ON: Cell<bool> = const { Cell::new(false) };
}

/// Starts keeping spots; until then naming elements costs nothing.
pub fn enable() {
    ON.with(|on| on.set(true));
}

/// Names the element this component draws: pass the result to its `on_sized`. An empty `id` names nothing.
pub fn use_spot(id: &str, role: &'static str, label: &str, enabled: bool) -> impl FnMut(Event<SizedEventData>) + 'static {
    let on = ON.with(Cell::get) && !id.is_empty();
    let key = use_hook(|| Rc::new(RefCell::new(String::new())));
    if on {
        let mut old = key.borrow_mut();
        if *old != id {
            let previous = std::mem::replace(&mut *old, id.to_string());
            SPOTS.with(|s| {
                let mut s = s.borrow_mut();
                let bounds = s.remove(&previous).map(|p| p.bounds).unwrap_or_default();
                s.insert(id.to_string(), Spot { id: id.to_string(), role, label: label.to_string(), bounds, enabled });
            });
        } else {
            SPOTS.with(|s| {
                if let Some(spot) = s.borrow_mut().get_mut(id) {
                    if spot.label != label {
                        spot.label = label.to_string();
                    }
                    spot.enabled = enabled;
                }
            });
        }
    }
    let gone = key.clone();
    use_drop(move || {
        let id = gone.borrow().clone();
        if !id.is_empty() {
            SPOTS.with(|s| s.borrow_mut().remove(&id));
        }
    });
    let id = id.to_string();
    move |e: Event<SizedEventData>| {
        if on {
            SPOTS.with(|s| {
                if let Some(spot) = s.borrow_mut().get_mut(&id) {
                    spot.bounds = Bounds::of(&e);
                }
            });
        }
    }
}

/// Every named element, by id.
pub fn all() -> Vec<Spot> {
    SPOTS.with(|s| s.borrow().values().cloned().collect())
}

/// The element `target` names: `id:x` exactly, `text:x` by its label, `role:x`, `~x` or plain `x` as part of id or label,
/// exact matches first; `nth` picks among several.
pub fn find(target: &str, nth: usize) -> Result<Spot, String> {
    let spots = all();
    let matches: Vec<Spot> = if let Some(id) = target.strip_prefix("id:") {
        spots.into_iter().filter(|s| s.id == id).collect()
    } else if let Some(text) = target.strip_prefix("text:") {
        spots.into_iter().filter(|s| s.label == text).collect()
    } else if let Some(role) = target.strip_prefix("role:") {
        spots.into_iter().filter(|s| s.role == role).collect()
    } else {
        let part = target.strip_prefix('~').unwrap_or(target).to_lowercase();
        let mut exact: Vec<Spot> = spots.iter().filter(|s| s.id.to_lowercase() == part || s.label.to_lowercase() == part).cloned().collect();
        exact.extend(spots.into_iter().filter(|s| (s.id.to_lowercase().contains(&part) || s.label.to_lowercase().contains(&part)) && s.id.to_lowercase() != part && s.label.to_lowercase() != part));
        exact
    };
    match matches.get(nth) {
        Some(spot) => Ok(spot.clone()),
        None if matches.is_empty() => {
            let near: Vec<String> = all().into_iter().map(|s| s.id).filter(|id| id.contains(target.rsplit(':').next().unwrap_or(target))).take(8).collect();
            Err(format!("no element {target}{}", if near.is_empty() { String::new() } else { format!("; similar: {}", near.join(", ")) }))
        }
        None => Err(format!("{target} matches {} elements; --nth picks one", matches.len())),
    }
}
