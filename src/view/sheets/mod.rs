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

#[derive(Debug, PartialEq, Eq)]
enum Onward {
    Batch,
    Instance,
    Close,
}

fn onward(no_instance: bool, no_batch: bool, added: usize) -> Onward {
    if no_batch && added > 1 {
        Onward::Batch
    } else if no_instance {
        Onward::Instance
    } else {
        Onward::Close
    }
}

/// What follows the very first accounts, `added` by id: several accounts without a batch yet open the first batch, with a new
/// instance preselected when there is none; one account sets up the instance to play with.
pub fn after_first_account(store: Store, added: Vec<String>) {
    let (no_instance, no_batch) = {
        let model = store.model.peek();
        (model.instances.is_empty(), model.batches.is_empty())
    };
    match onward(no_instance, no_batch, added.len()) {
        Onward::Batch => batch::open_new(store, None, Some(added.iter().filter_map(|id| uuid::Uuid::parse_str(id).ok()).collect())),
        Onward::Instance => instance::create(store, Some(match added.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        })),
        Onward::Close => sheet::close(store.ui),
    }
}

#[cfg(test)]
mod tests {
    use super::{Onward, onward};

    #[test]
    fn several_first_accounts_open_the_batch_and_one_sets_up_the_instance() {
        assert_eq!(onward(true, true, 3), Onward::Batch);
        assert_eq!(onward(false, true, 2), Onward::Batch);
        assert_eq!(onward(true, true, 1), Onward::Instance);
        assert_eq!(onward(true, false, 2), Onward::Instance);
        assert_eq!(onward(false, true, 1), Onward::Close);
        assert_eq!(onward(false, false, 4), Onward::Close);
    }
}
