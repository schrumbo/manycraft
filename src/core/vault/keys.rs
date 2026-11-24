//! The real logins of accounts whose games run behind a vault. Kept in memory only, and only while such a game runs.

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use uuid::Uuid;

use super::Refresh;
use crate::core::auth::AuthError;

const FIRST_WAIT: Duration = Duration::from_secs(30);
const LONGEST_WAIT: Duration = Duration::from_secs(15 * 60);

/// The current login of one account, shared by the vaults of its games.
pub(super) struct Cell {
    account: Uuid,
    key: Mutex<Option<Key>>,
    turn: Mutex<()>,
    backoff: Mutex<Backoff>,
}

/// When a failed renewal may be tried again; never once the account has to sign in again.
#[derive(Default)]
struct Backoff {
    retry_at: Option<Instant>,
    wait: Duration,
    dead: bool,
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

    /// Renews the login through `refresh` once for all vaults of the account: after waiting for a renewal already
    /// running it stops when `enough` says the login is good again, and it skips while a failed renewal waits out its
    /// backoff or the account has to sign in again.
    pub(super) fn renew(&self, refresh: &Refresh, enough: impl Fn() -> bool) {
        let _turn = self.turn.lock().unwrap_or_else(PoisonError::into_inner);
        if enough() {
            return;
        }
        {
            let backoff = self.backoff.lock().unwrap_or_else(PoisonError::into_inner);
            if backoff.dead || backoff.retry_at.is_some_and(|at| Instant::now() < at) {
                return;
            }
        }
        let Err(e) = refresh() else { return };
        eprintln!("vault: cannot renew the login: {e:#}");
        let mut backoff = self.backoff.lock().unwrap_or_else(PoisonError::into_inner);
        backoff.dead = e.chain().any(|c| c.downcast_ref::<AuthError>() == Some(&AuthError::SignInAgain));
        backoff.wait = if backoff.wait.is_zero() { FIRST_WAIT } else { (backoff.wait * 2).min(LONGEST_WAIT) };
        backoff.retry_at = Some(Instant::now() + backoff.wait);
    }

    fn set(&self, token: &str, expires_at: SystemTime) {
        *self.key.lock().unwrap_or_else(PoisonError::into_inner) = Some(Key { token: token.to_owned(), expires_at });
        *self.backoff.lock().unwrap_or_else(PoisonError::into_inner) = Backoff::default();
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
    let cell = Arc::new(Cell { account, key: Mutex::new(None), turn: Mutex::new(()), backoff: Mutex::new(Backoff::default()) });
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