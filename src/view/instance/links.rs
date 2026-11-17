//! Where the instance page hands over to sheets other pages own: the mod browser, the batch sheet and the .mrpack export.

use super::{actions::Action, held};
use crate::core::i18n::{tr, trf};
use crate::core::modrinth::Kind;
use crate::view::commands::{Want, want, want_blocked};
use crate::view::icons::Icon;
use crate::view::sheets::browser;
use crate::view::store::Store;

/// Exports instance `id` as a .mrpack.
pub fn export(store: Store, id: &str, name: &str) -> Action {
    let busy = store.model.peek().exporting.contains(id).then(|| tr("Wird exportiert…"));
    let id = id.to_owned();
    Action::new("export", tr("Exportieren…"), trf("%@ als Modpack-Datei exportieren…", &[&name]), Icon::FileDown, move |_| {
        want(store, Want::ExportInstance(id.clone()));
    })
    .disabled(busy.or_else(|| want_blocked(&Want::ExportInstance(String::new()))))
}

/// Opens the batch sheet for a new batch that plays instance `id`.
pub fn new_batch(store: Store, id: &str, name: &str) -> Action {
    let id = id.to_owned();
    Action::new("batch.new", tr("Neuer Batch mit dieser Instanz…"), trf("Neuer Batch mit %@…", &[&name]), Icon::Layers, move |_| {
        want(store, Want::NewBatchFor(id.clone()));
    })
    .disabled(want_blocked(&Want::NewBatchFor(String::new())))
}

/// Opens the mod browser to add mods to instance `id`.
pub fn add_mods(store: Store, id: &str, name: &str) -> Action {
    let (why, id) = (held(store, id), id.to_owned());
    Action::new("mods.add", tr("Mods hinzufügen…"), trf("Mods zu %@ hinzufügen…", &[&name]), Icon::Plus, move |_| {
        want(store, Want::AddMods(id.clone()));
    })
    .disabled(why)
}

/// Opens the mod browser on resource packs for instance `id`.
pub fn resource_packs(store: Store, id: &str) -> Action {
    let (why, id) = (held(store, id), id.to_owned());
    Action::new("packs", tr("Ressourcenpakete suchen…"), tr("Ressourcenpakete suchen…"), Icon::Search, move |_| browser::find(store, &id, Kind::ResourcePack)).disabled(why)
}

/// Opens the mod browser on shaders for instance `id`.
pub fn shaders(store: Store, id: &str) -> Action {
    let (why, id) = (held(store, id), id.to_owned());
    Action::new("shaders", tr("Shader suchen…"), tr("Shader suchen…"), Icon::Search, move |_| browser::find(store, &id, Kind::Shader)).disabled(why)
}
