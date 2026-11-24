//! The real logins of accounts whose games run behind a vault. Kept in memory only, and only while such a game runs.

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::SystemTime;

use uuid::Uuid;

/// The current login of one account, shared by the vaults of its games.
pub(super) struct Cell {
    account: Uuid,
    key: Mutex<Option<Key>>,
}

struct Key {
    token: String,
    expires_at: SystemTime,
}

impl Cell {
    /// The login when it is still valid at `at`.
    pub(super) fn current(&self, at: SystemTime) -> Option<String> {
        let key = self.key.lock().unwrap_or_else(PoisonError::into_inner);
        key.as_ref().filter(|k| k.expires_at > at).map(|k| k.token.clone())
    }

    fn set(&self, token: &str, expires_at: SystemTime) {
        *self.key.lock().unwrap_or_else(PoisonError::into_inner) = Some(Key { token: token.to_owned(), expires_at });
    }
}

static CELLS: Mutex<Vec<Weak<Cell>>> = Mutex::new(Vec::new());

/// The cell of `account`, shared with the other running vaults of it.
pub(super) fn hold(account: Uuid) -> Arc<Cell> {
    let mut cells = CELLS.lock().unwrap_or_else(PoisonError::into_inner);
    cells.retain(|c| c.strong_count() > 0);
    if let Some(cell) = cells.iter().filter_map(Weak::upgrade).find(|c| c.account == account) {
        return cell;
    }
    let cell = Arc::new(Cell { account, key: Mutex::new(None) });
    cells.push(Arc::downgrade(&cell));
    cell
}

/// Hands a fresh login of `account`, valid until `expires_at`, to the vaults of its running games; kept nowhere when
/// none runs.
pub fn publish(account: Uuid, token: &str, expires_at: SystemTime) {
    let cells = CELLS.lock().unwrap_or_else(PoisonError::into_inner);
    for cell in cells.iter().filter_map(Weak::upgrade).filter(|c| c.account == account) {
        cell.set(token, expires_at);
    }
}
