//! Secrets in the OS keychain. On macOS every secret lives in one keychain item, so a new build asks for access once instead of once per secret. Every change reads the item fresh under a lock, so other Manycraft processes never lose what they stored.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};

use super::account::KEYCHAIN_SERVICE;
use super::lock;

const POOL: &str = "secrets";
const LOCK_WAIT: Duration = Duration::from_secs(10);

static SHARED: Pool<Keyring> = Pool { shelf: Keyring, turn: Mutex::new(()), lock: shared_lock };

/// Whether secrets go through the one shared item: on macOS, outside tests.
pub fn pooled() -> bool {
    cfg!(target_os = "macos") && !cfg!(test)
}

/// Reads the secret `name`; a secret still in its own item from an older version moves into the shared one.
pub fn get(name: &str) -> Result<Option<String>> {
    SHARED.get(name)
}

/// Stores the secret `name`.
pub fn set(name: &str, value: &str) -> Result<()> {
    SHARED.set(name, value)
}

/// Removes the secret `name`, also its own item from an older version; a missing one is not an error.
pub fn delete(name: &str) -> Result<()> {
    SHARED.delete(name)
}

trait Shelf {
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn set(&self, name: &str, value: &str) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}

struct Keyring;

impl Shelf for Keyring {
    fn get(&self, name: &str) -> Result<Option<String>> {
        match entry(name)?.get_secret() {
            Ok(bytes) => Ok(Some(String::from_utf8(bytes).context("a stored secret is unreadable")?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("cannot read from the system keychain"),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        entry(name)?.set_secret(value.as_bytes()).context("cannot save in the system keychain")
    }

    fn delete(&self, name: &str) -> Result<()> {
        match entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context("cannot remove from the system keychain"),
        }
    }
}

fn entry(name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, name).context("system keychain is not available")
}

struct Pool<S> {
    shelf: S,
    turn: Mutex<()>,
    lock: fn() -> Option<PathBuf>,
}

impl<S: Shelf> Pool<S> {
    fn get(&self, name: &str) -> Result<Option<String>> {
        let _turn = self.turn()?;
        let mut map = self.load()?;
        if let Some(value) = map.remove(name) {
            return Ok(Some(value));
        }
        let Some(value) = self.shelf.get(name)? else { return Ok(None) };
        map.insert(name.to_owned(), value.clone());
        let _ = self.save(&map);
        Ok(Some(value))
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        let _turn = self.turn()?;
        let mut map = self.load()?;
        if map.get(name).is_some_and(|v| v == value) {
            return Ok(());
        }
        map.insert(name.to_owned(), value.to_owned());
        self.save(&map)
    }

    fn delete(&self, name: &str) -> Result<()> {
        let _turn = self.turn()?;
        self.shelf.delete(name)?;
        let mut map = self.load()?;
        if map.remove(name).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }

    fn turn(&self) -> Result<(MutexGuard<'_, ()>, Option<lock::Guard>)> {
        let turn = self.turn.lock().unwrap_or_else(PoisonError::into_inner);
        let held = match (self.lock)() {
            Some(path) => Some(lock::exclusive_within(&path, LOCK_WAIT).context("another Manycraft is using the keychain")?),
            None => None,
        };
        Ok((turn, held))
    }

    fn load(&self) -> Result<BTreeMap<String, String>> {
        match self.shelf.get(POOL)? {
            Some(text) => serde_json::from_str(&text).context("the shared keychain item is unreadable"),
            None => Ok(BTreeMap::new()),
        }
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<()> {
        self.shelf.set(POOL, &serde_json::to_string(map)?)
    }
}

fn shared_lock() -> Option<PathBuf> {
    Some(std::env::temp_dir().join("manycraft-secrets.lock"))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use anyhow::bail;

    use super::*;

    #[derive(Default)]
    struct Fake {
        items: RefCell<BTreeMap<String, String>>,
        fail_writes: Cell<bool>,
        fail_reads: Cell<bool>,
        fail_deletes: Cell<bool>,
    }

    impl Shelf for Rc<Fake> {
        fn get(&self, name: &str) -> Result<Option<String>> {
            if self.fail_reads.get() {
                bail!("denied");
            }
            Ok(self.items.borrow().get(name).cloned())
        }

        fn set(&self, name: &str, value: &str) -> Result<()> {
            if self.fail_writes.get() {
                bail!("denied");
            }
            self.items.borrow_mut().insert(name.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete(&self, name: &str) -> Result<()> {
            if self.fail_deletes.get() {
                bail!("denied");
            }
            self.items.borrow_mut().remove(name);
            Ok(())
        }
    }

    fn store(items: &[(&str, &str)]) -> Rc<Fake> {
        let fake = Rc::new(Fake::default());
        fake.items.borrow_mut().extend(items.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        fake
    }

    fn pool(store: &Rc<Fake>) -> Pool<Rc<Fake>> {
        Pool { shelf: store.clone(), turn: Mutex::new(()), lock: || None }
    }

    fn shared(store: &Rc<Fake>) -> BTreeMap<String, String> {
        serde_json::from_str(&store.items.borrow()[POOL]).unwrap()
    }

    #[test]
    fn secrets_share_one_item() {
        let s = store(&[]);
        let p = pool(&s);
        p.set("a", "1").unwrap();
        p.set("b", "2").unwrap();
        assert_eq!(p.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(p.get("b").unwrap().as_deref(), Some("2"));
        assert_eq!(s.items.borrow().keys().collect::<Vec<_>>(), [POOL]);
    }

    #[test]
    fn two_processes_never_drop_or_revert_what_the_other_stored() {
        let s = store(&[]);
        let (gui, cli) = (pool(&s), pool(&s));
        gui.set("a", "tA").unwrap();
        assert_eq!(gui.get("a").unwrap().as_deref(), Some("tA"));
        cli.set("b", "tB").unwrap();
        cli.set("a", "tA2").unwrap();
        gui.set("a.session", "s").unwrap();
        assert_eq!(gui.get("b").unwrap().as_deref(), Some("tB"));
        assert_eq!(gui.get("a").unwrap().as_deref(), Some("tA2"));
        assert_eq!(shared(&s).len(), 3);
    }

    #[test]
    fn an_old_single_item_moves_into_the_shared_one_and_stays() {
        let s = store(&[("a", "old")]);
        let p = pool(&s);
        assert_eq!(p.get("a").unwrap().as_deref(), Some("old"));
        assert_eq!(shared(&s)["a"], "old");
        assert_eq!(s.items.borrow()["a"], "old");
        p.set("a", "new").unwrap();
        assert_eq!(p.get("a").unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn an_unreadable_shared_item_is_never_overwritten() {
        let s = store(&[(POOL, "not json")]);
        let p = pool(&s);
        assert!(p.set("a", "1").is_err());
        assert!(p.get("a").is_err());
        assert!(p.delete("a").is_err());
        assert_eq!(s.items.borrow()[POOL], "not json");
    }

    #[test]
    fn a_denied_read_changes_nothing() {
        let s = store(&[]);
        let p = pool(&s);
        p.set("a", "1").unwrap();
        let text = s.items.borrow()[POOL].clone();
        s.fail_reads.set(true);
        assert!(p.set("b", "2").is_err());
        assert_eq!(s.items.borrow()[POOL], text);
        s.fail_reads.set(false);
        assert_eq!(p.get("a").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn a_failed_write_keeps_the_old_value() {
        let s = store(&[]);
        let p = pool(&s);
        p.set("a", "1").unwrap();
        s.fail_writes.set(true);
        assert!(p.set("a", "2").is_err());
        assert!(p.delete("a").is_err());
        s.fail_writes.set(false);
        assert_eq!(p.get("a").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn delete_removes_the_old_item_first_and_then_the_shared_entry() {
        let s = store(&[("a", "old")]);
        let p = pool(&s);
        assert_eq!(p.get("a").unwrap().as_deref(), Some("old"));
        s.fail_deletes.set(true);
        assert!(p.delete("a").is_err());
        assert_eq!(shared(&s)["a"], "old");
        s.fail_deletes.set(false);
        p.delete("a").unwrap();
        assert_eq!(p.get("a").unwrap(), None);
        assert!(!s.items.borrow().contains_key("a"));
        p.delete("a").unwrap();
    }

    #[test]
    fn storing_the_same_value_writes_nothing() {
        let s = store(&[]);
        let p = pool(&s);
        p.set("a", "1").unwrap();
        s.fail_writes.set(true);
        p.set("a", "1").unwrap();
    }

    #[test]
    fn the_lock_lives_outside_any_manycraft_home() {
        let path = shared_lock().unwrap();
        assert!(path.starts_with(std::env::temp_dir()));
    }
}
