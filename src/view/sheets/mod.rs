//! The sheets: short tasks above the window, such as creating an instance, adding accounts or restarting for an update.

pub mod account;
pub mod skin;
pub mod trash;
pub mod updates;

use crate::view::commands::{Answer, Want, answer_wants};
use crate::view::kit::sheet;
use crate::view::store::Store;

/// Lets the sheets answer what other parts of the window ask for.
pub fn answer() {
    answer_wants(Answer { takes, run });
}

fn takes(want: &Want) -> bool {
    matches!(want, Want::AddAccount | Want::AddMicrosoft | Want::SignInAgain(_) | Want::SignInNames(_) | Want::ChangeSkin(_) | Want::OpenTrash | Want::FetchUpdate | Want::CancelUpdate | Want::RestartToUpdate | Want::DismissNews)
}

fn run(store: Store, want: Want) {
    match want {
        Want::AddAccount => account::open(store, account::Start::Choose),
        Want::AddMicrosoft => account::open(store, account::Start::Microsoft),
        Want::SignInAgain(ids) => account::open(store, account::Start::Again(ids)),
        Want::SignInNames(names) => account::open(store, account::Start::Names(names)),
        Want::ChangeSkin(id) => skin::open(store, id),
        Want::OpenTrash => trash::open(store),
        Want::FetchUpdate => crate::view::updates::fetch(store),
        Want::CancelUpdate => crate::view::updates::cancel(store),
        Want::RestartToUpdate => updates::restart(store),
        Want::DismissNews => crate::view::updates::dismiss_news(store),
        _ => {}
    }
}

/// What follows the very first account: the instance to play with is set up next.
pub fn after_first_account(store: Store) {
    sheet::close(store.ui);
}
