//! The sheets: short tasks above the window, such as creating an instance, adding accounts or restarting for an update.

pub mod account;
pub mod skin;
pub mod trash;
pub mod updates;

use crate::view::kit::sheet;
use crate::view::store::Store;

/// What follows the very first account: the instance to play with is set up next.
pub fn after_first_account(store: Store) {
    sheet::close(store.ui);
}
