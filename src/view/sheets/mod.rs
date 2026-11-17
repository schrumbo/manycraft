//! The sheets: short tasks above the window, such as creating an instance, adding accounts or restarting for an update.

pub mod account;
pub mod batch;
pub mod browser;
pub mod import;
pub mod instance;
pub mod skin;
pub mod trash;
pub mod updates;

use crate::view::commands::{Answer, Want, answer_wants};
use crate::view::instance::actions;
use crate::view::kit::sheet;
use crate::view::store::Store;

/// Lets the sheets answer what other parts of the window ask for.
pub fn answer() {
    answer_wants(Answer { takes, run });
}

fn takes(want: &Want) -> bool {
    matches!(want, Want::AddAccount | Want::AddMicrosoft | Want::SignInAgain(_) | Want::SignInNames(_) | Want::ChangeSkin(_) | Want::OpenTrash | Want::Import | Want::ImportFile(_) | Want::NewInstance | Want::EditMods(_) | Want::AddMods(_) | Want::FindModpacks | Want::RenameInstance(_) | Want::CloneInstance(_) | Want::CreateShortcut(_) | Want::ExportInstance(_) | Want::ExportBatch(_) | Want::UpdateLoader(_) | Want::ResetInstance(_) | Want::DeleteInstance(_) | Want::NewBatch | Want::NewBatchFor(_) | Want::EditBatch(_) | Want::DeleteBatch(_) | Want::FetchUpdate | Want::CancelUpdate | Want::RestartToUpdate | Want::DismissNews)
}

fn run(store: Store, want: Want) {
    if store.ui.sheet.peek().is_some() && !matches!(want, Want::FetchUpdate | Want::CancelUpdate | Want::DismissNews) {
        return;
    }
    match want {
        Want::AddAccount => account::open(store, account::Start::Choose),
        Want::AddMicrosoft => account::open(store, account::Start::Microsoft),
        Want::SignInAgain(ids) => account::open(store, account::Start::Again(ids)),
        Want::SignInNames(names) => account::open(store, account::Start::Names(names)),
        Want::ChangeSkin(id) => skin::open(store, id),
        Want::OpenTrash => trash::open(store),
        Want::Import => import::open(store),
        Want::ImportFile(path) => import::open_file(store, path),
        Want::NewInstance => instance::create(store, None),
        Want::EditMods(id) => instance::mods(store, id),
        Want::AddMods(id) => browser::add_mods(store, id),
        Want::FindModpacks => browser::find_modpacks(store),
        Want::RenameInstance(id) => actions::begin_rename(store, &id),
        Want::CloneInstance(id) => actions::clone(store, &id),
        Want::CreateShortcut(id) => actions::shortcut(store, &id),
        Want::ExportInstance(id) => actions::export(store, &id),
        Want::ExportBatch(id) => {
            let made = store.model.peek().instance_of(&id).map(|i| i.id.clone());
            if let Some(made) = made {
                actions::export(store, &made);
            }
        }
        Want::UpdateLoader(id) => actions::look_for_loader(store, &id),
        Want::ResetInstance(id) => actions::ask_reset(store, &id),
        Want::DeleteInstance(id) => actions::ask_delete(store, &id),
        Want::NewBatch => batch::open_new(store, None, None),
        Want::NewBatchFor(id) => batch::open_new(store, Some(id), None),
        Want::EditBatch(id) => batch::open_edit(store, &id),
        Want::DeleteBatch(id) => batch::ask_delete(store, &id),
        Want::FetchUpdate => crate::view::updates::fetch(store),
        Want::CancelUpdate => crate::view::updates::cancel(store),
        Want::RestartToUpdate => updates::restart(store),
        Want::DismissNews => crate::view::updates::dismiss_news(store),
        _ => {}
    }
}

/// What follows the very first accounts, `added` by id: the sheet that sets up the instance to play with, playing the
/// account when only one was added.
pub fn after_first_account(store: Store, added: Vec<String>) {
    let play = match added.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    if store.model.peek().instances.is_empty() {
        instance::create(store, Some(play));
    } else {
        sheet::close(store.ui);
    }
}

