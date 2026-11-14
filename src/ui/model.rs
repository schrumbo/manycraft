//! What the views show: accounts, batches and the live status of every instance, held in one entity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use gpui_kit::{App, AppContext, Entity, Global, RenderImage};
use uuid::Uuid;

use crate::core::account::Account;
use crate::core::batch::Batch;
use crate::core::fit::Fit;
use crate::core::history::{self, Entry};
use crate::core::i18n::{tr, trf};
use crate::core::proxy::Fault;

use super::forms;

/// Where an instance is in its life, as the grid shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Stopped,
    Starting { done: usize, total: usize },
    Menu,
    Connecting { server: String },
    Queue { server: String },
    Playing { server: String, since: Instant },
    Disconnected { server: Option<String>, reason: String },
    Dead { server: Option<String>, reason: String },
    Crashed { code: Option<i32> },
    /// Waiting to start again after a crash: try `attempt` of `of`.
    Restarting { attempt: u32, of: u32 },
    /// The server banned the account; nothing reconnects until the user retries.
    Banned { server: Option<String>, reason: String },
    LoginNeeded,
    Failed { reason: String },
}

impl Phase {
    /// Whether the user has work here: dropped off the server after auto-reconnect gave up, banned, crashed, or must sign in.
    pub fn needs_attention(&self) -> bool {
        matches!(self, Phase::Disconnected { .. } | Phase::Dead { .. } | Phase::Crashed { .. } | Phase::Banned { .. } | Phase::LoginNeeded | Phase::Failed { .. })
    }

    /// Whether a game process exists for the instance.
    pub fn is_running(&self) -> bool {
        !matches!(self, Phase::Stopped | Phase::Crashed { .. } | Phase::Restarting { .. } | Phase::LoginNeeded | Phase::Failed { .. })
    }
}

/// Live status of one account in one batch.
#[derive(Clone, Debug)]
pub struct Instance {
    pub key: String,
    pub batch_id: String,
    pub account_id: Uuid,
    pub name: String,
    pub phase: Phase,
    pub reconnect_attempts: u32,
    pub unread_whispers: u32,
    pub preview: Option<Arc<RenderImage>>,
    pub preview_seq: u32,
    pub stopping: bool,
}

/// One line of the merged chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLine {
    pub instance: String,
    pub whisper: bool,
    pub from: Option<String>,
    pub text: String,
}

/// Everything the views read. Only `ui::state` changes it.
#[derive(Default)]
pub struct Model {
    pub accounts: Vec<Account>,
    pub batches: Vec<Batch>,
    pub instances: HashMap<String, Instance>,
    pub chat: Vec<ChatLine>,
    pub playing: Option<String>,
    pub error: Option<String>,
    /// Batches whose game is being installed, with the percent done.
    pub preparing: HashMap<String, u8>,
    /// Changes whenever an instance changes place in the sort order, so views know when to sort again.
    pub order_seq: u64,
    /// Last events of every instance by key, oldest first, at most `history::KEPT` each.
    pub history: HashMap<String, history::Log>,
}

impl Model {
    /// Instances of `batch_id` (all batches when `None`), attention first, then running, then stopped, each by name.
    pub fn sorted(&self, batch_id: Option<&str>) -> Vec<&Instance> {
        let mut list: Vec<&Instance> = self.instances.values().filter(|i| batch_id.is_none_or(|b| i.batch_id == b)).collect();
        list.sort_by_key(|i| (rank(&i.phase), i.name.to_lowercase(), i.key.clone()));
        list
    }

    /// How many instances of `batch_id` (all when `None`) need attention.
    pub fn attention_count(&self, batch_id: Option<&str>) -> usize {
        self.instances.values().filter(|i| batch_id.is_none_or(|b| i.batch_id == b) && i.phase.needs_attention()).count()
    }

    /// The saved account with this id.
    pub fn account(&self, id: Uuid) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// The batch with this id.
    pub fn batch(&self, id: &str) -> Option<&Batch> {
        self.batches.iter().find(|b| b.id == id)
    }

    /// The recorded events of instance `key`, newest first.
    pub fn events(&self, key: &str) -> Vec<&Entry> {
        self.history.get(key).map(|log| log.iter().rev().collect()).unwrap_or_default()
    }
}

/// Tile status line of a banned account.
pub fn banned_status(reason: &str) -> String {
    if reason.is_empty() { tr("Gesperrt") } else { trf("Gesperrt · %@", &[&reason]) }
}

/// Tile status line of an instance that starts again after a crash.
pub fn restart_status(attempt: u32, of: u32) -> String {
    trf("Startet neu · Versuch %lld von %lld", &[&attempt, &of])
}

/// Extra facts about a live instance that its phase does not carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hint {
    /// The game runs but its link to the manager dropped.
    pub link_lost: bool,
    /// The account's proxy failed its check after a connection failure.
    pub proxy: Option<Fault>,
}

fn hints() -> &'static Mutex<HashMap<String, Hint>> {
    static HINTS: OnceLock<Mutex<HashMap<String, Hint>>> = OnceLock::new();
    HINTS.get_or_init(Mutex::default)
}

/// The hint of `key`; empty when nothing is known.
pub fn hint(key: &str) -> Hint {
    hints().lock().unwrap_or_else(PoisonError::into_inner).get(key).copied().unwrap_or_default()
}

/// Changes the hint of `key`; an empty hint is forgotten.
pub fn set_hint(key: &str, change: impl FnOnce(&mut Hint)) {
    let mut all = hints().lock().unwrap_or_else(PoisonError::into_inner);
    let mut hint = all.get(key).copied().unwrap_or_default();
    change(&mut hint);
    if hint == Hint::default() {
        all.remove(key);
    } else {
        all.insert(key.to_owned(), hint);
    }
}

/// What `count` instances of `each_mb` need against the memory that is free, as one German line.
pub fn fit_line(count: u32, each_mb: u32, fit: &Fit) -> String {
    let label = |mb: u64| forms::memory_label(u32::try_from(mb).unwrap_or(u32::MAX));
    if count == 1 {
        return trf("Eine Instanz braucht %@, dein PC hat %@ frei", &[&label(fit.needed_mb), &label(fit.free_mb)]);
    }
    trf("%lld Instanzen × %@ = %@, dein PC hat %@ frei", &[&count, &label(u64::from(each_mb)), &label(fit.needed_mb), &label(fit.free_mb)])
}

/// Tile status line of a proxy that failed its check.
pub fn proxy_status(fault: Fault) -> String {
    match fault {
        Fault::Unreachable => tr("Proxy nicht erreichbar"),
        Fault::Rejected => tr("Proxy lehnt die Anmeldung ab"),
    }
}

/// Tile status line of a running game the manager lost contact with.
pub fn link_status() -> String {
    tr("Keine Verbindung zum Spiel")
}

/// Sort group of a phase: attention first, then running or restarting, then stopped.
pub fn rank(phase: &Phase) -> u8 {
    if phase.needs_attention() {
        0
    } else if phase.is_running() || matches!(phase, Phase::Restarting { .. }) {
        1
    } else {
        2
    }
}

struct Shared(Entity<Model>);

impl Global for Shared {}

/// Creates the model entity once at startup.
pub fn init(cx: &mut App) {
    let model = cx.new(|_| Model::default());
    cx.set_global(Shared(model));
}

/// The model entity; views observe it and re-render on change.
pub fn get(cx: &App) -> Entity<Model> {
    cx.global::<Shared>().0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fit_line_names_what_is_needed_and_what_is_free() {
        crate::core::i18n::init("de");
        let fit = |needed_mb, free_mb| Fit { needed_mb, free_mb, suggested_count: 0 };
        assert_eq!(fit_line(50, 2048, &fit(102_400, 32_768)), "50 Instanzen × 2 GB = 100 GB, dein PC hat 32 GB frei");
        assert_eq!(fit_line(3, 2560, &fit(7680, 4096)), "3 Instanzen × 2,5 GB = 7,5 GB, dein PC hat 4 GB frei");
        assert_eq!(fit_line(1, 8192, &fit(8192, 4096)), "Eine Instanz braucht 8 GB, dein PC hat 4 GB frei");
    }

    fn instance(key: &str, name: &str, batch: &str, phase: Phase) -> Instance {
        Instance { key: key.into(), batch_id: batch.into(), account_id: Uuid::nil(), name: name.into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, stopping: false }
    }

    #[test]
    fn attention_comes_first_then_running_then_stopped_by_name() {
        let mut m = Model::default();
        for i in [
            instance("1", "bob", "a", Phase::Stopped),
            instance("2", "Alex", "a", Phase::Playing { server: "s".into(), since: Instant::now() }),
            instance("3", "zed", "a", Phase::Disconnected { server: None, reason: "Kicked".into() }),
            instance("4", "amy", "b", Phase::Stopped),
            instance("5", "Abe", "a", Phase::LoginNeeded),
        ] {
            m.instances.insert(i.key.clone(), i);
        }
        let names: Vec<&str> = m.sorted(None).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Abe", "zed", "Alex", "amy", "bob"]);
        let only_a: Vec<&str> = m.sorted(Some("a")).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(only_a, ["Abe", "zed", "Alex", "bob"]);
        assert_eq!(m.attention_count(None), 2);
        assert_eq!(m.attention_count(Some("b")), 0);
    }

    #[test]
    fn restarting_sorts_with_the_running_ones() {
        let mut m = Model::default();
        for i in [
            instance("1", "amy", "a", Phase::Stopped),
            instance("2", "zed", "a", Phase::Restarting { attempt: 1, of: 3 }),
            instance("3", "bob", "a", Phase::Playing { server: "s".into(), since: Instant::now() }),
        ] {
            m.instances.insert(i.key.clone(), i);
        }
        let names: Vec<&str> = m.sorted(None).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["bob", "zed", "amy"]);
    }

    #[test]
    fn banned_needs_attention_and_runs() {
        let banned = Phase::Banned { server: None, reason: "x".into() };
        assert!(banned.needs_attention() && banned.is_running());
        let mut m = Model::default();
        m.instances.insert("1".into(), instance("1", "a", "a", banned));
        assert_eq!(m.attention_count(None), 1);
    }

    #[test]
    fn status_lines_for_banned_and_restart() {
        crate::core::i18n::init("de");
        assert_eq!(banned_status(""), "Gesperrt");
        assert_eq!(banned_status("Cheating"), "Gesperrt · Cheating");
        assert_eq!(restart_status(1, 3), "Startet neu · Versuch 1 von 3");
    }

    #[test]
    fn events_come_newest_first() {
        let mut m = Model::default();
        let log = m.history.entry("k".into()).or_default();
        for at in 1..=3 {
            history::push(log, Entry { at, kind: history::Kind::Kicked, server: None, reason: String::new() });
        }
        assert_eq!(m.events("k").iter().map(|e| e.at).collect::<Vec<_>>(), [3, 2, 1]);
        assert!(m.events("none").is_empty());
    }
}
