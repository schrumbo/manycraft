//! Where the instance page hands over to sheets other pages own: the mod browser, the batch sheet and the .mrpack export.
//! Each hook says why it is unavailable until its sheet moved into the new interface.

use super::actions::Action;
use crate::core::i18n::{tr, trf};
use crate::view::icons::Icon;
use crate::view::store::Store;

fn moving() -> Option<String> {
    Some(tr("Zieht noch in die neue Oberfläche um."))
}

/// Exports instance `id` as a .mrpack.
pub fn export(store: Store, id: &str, name: &str) -> Action {
    let _ = (store, id);
    Action::new("export", tr("Als .mrpack exportieren…"), trf("%@ als .mrpack exportieren…", &[&name]), Icon::FileDown, |_| {}).disabled(moving())
}

/// Opens the batch sheet for a new batch that plays instance `id`.
pub fn new_batch(store: Store, id: &str, name: &str) -> Action {
    let _ = (store, id);
    Action::new("batch.new", tr("Neuer Batch mit dieser Instanz…"), trf("Neuer Batch mit %@…", &[&name]), Icon::Layers, |_| {}).disabled(moving())
}

/// Opens the mod browser to add mods to instance `id`.
pub fn add_mods(store: Store, id: &str, name: &str) -> Action {
    let _ = (store, id);
    Action::new("mods.add", tr("Mods hinzufügen…"), trf("Mods zu %@ hinzufügen…", &[&name]), Icon::Plus, |_| {}).disabled(moving())
}

/// Opens the mod browser on resource packs for instance `id`.
pub fn resource_packs(store: Store, id: &str) -> Action {
    let _ = (store, id);
    Action::new("packs", tr("Ressourcenpakete suchen…"), tr("Ressourcenpakete suchen…"), Icon::Search, |_| {}).disabled(moving())
}

/// Opens the mod browser on shaders for instance `id`.
pub fn shaders(store: Store, id: &str) -> Action {
    let _ = (store, id);
    Action::new("shaders", tr("Shader suchen…"), tr("Shader suchen…"), Icon::Search, |_| {}).disabled(moving())
}
