//! What the library knows about each instance that costs disk reads: when it was last played and when it was made.
//! Read on a thread of its own, so drawing never waits for the disk.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::SystemTime;

use freya::prelude::*;

use crate::core::instance::Instance;
use crate::view::kit::off_thread;
use crate::view::store::Store;

/// Times of one instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub played: Option<SystemTime>,
    pub made: Option<SystemTime>,
}

/// The times of every instance, by id.
pub type Infos = Rc<HashMap<String, Info>>;

fn read(instance: &Instance) -> Info {
    Info { played: instance.last_played(), made: std::fs::metadata(instance.dir()).and_then(|m| m.created()).ok() }
}

/// Reads the times of `instances` again and stores them.
pub fn refresh(store: Store, instances: Vec<Instance>) {
    let mut infos = store.ui.infos;
    let Some(rx) = off_thread("library-info", move || instances.iter().map(|i| (i.id.clone(), read(i))).collect::<HashMap<_, _>>()) else { return };
    spawn_in_window(async move {
        let Ok(next) = rx.recv().await else { return };
        let Some(mut slot) = infos.try_write() else { return };
        if **slot != next {
            *slot = Rc::new(next);
        }
    });
}

/// Keeps the times current while the component that calls it is on screen: read again when instances come or go and
/// when a game of one starts or ends.
pub fn use_infos(store: Store) {
    let m = store.model.read();
    let ids: Vec<String> = m.instances.iter().map(|i| i.id.clone()).collect();
    let active = m.games.values().filter(|g| g.alone() && g.phase.is_active()).count();
    let instances = m.instances.clone();
    drop(m);
    use_side_effect_with_deps(&(ids, active), move |_| refresh(store, instances.clone()));
}

/// "vor 2 Std." for a time in the past.
pub fn ago(at: SystemTime) -> String {
    use crate::core::i18n::{tr, trf};
    let secs = SystemTime::now().duration_since(at).map_or(0, |d| d.as_secs());
    match secs {
        0..=59 => tr("gerade eben"),
        60..=3599 => trf("vor %lld Min.", &[&(secs / 60)]),
        3600..=86_399 => trf("vor %lld Std.", &[&(secs / 3600)]),
        86_400..=172_799 => tr("gestern"),
        _ => trf("vor %lld Tagen", &[&(secs / 86_400)]),
    }
}
