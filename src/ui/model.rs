//! What the views show: accounts, batches and the live status of every instance, held in one entity.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::{App, AppContext, Entity, Global, RenderImage};
use uuid::Uuid;

use crate::core::account::Account;
use crate::core::batch::Batch;
use crate::core::history::{self, Entry};
use crate::core::i18n::{tr, trf};

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

    /// Whether the instance is on a server, where a chat line can go out.
    pub fn on_server(&self) -> bool {
        matches!(self, Phase::Queue { .. } | Phase::Playing { .. })
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
    /// Name of the account whose game showed the line.
    pub account: String,
    pub batch: String,
    /// Seconds since the Unix epoch when the line arrived.
    pub at: i64,
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
    /// How many lines were ever added to `chat`, so a view can tell new lines from dropped old ones.
    pub chat_seq: u64,
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

    /// Adds a chat line, dropping the oldest tenth once `kept` lines are held.
    pub fn push_chat(&mut self, line: ChatLine, kept: usize) {
        if self.chat.len() >= kept {
            self.chat.drain(..(kept / 10).max(1));
        }
        self.chat.push(line);
        self.chat_seq += 1;
    }

    /// Sequence number of the oldest line still in `chat`; line `n` is at index `n - chat_base()`.
    pub fn chat_base(&self) -> u64 {
        self.chat_seq - self.chat.len() as u64
    }

    /// Whispers not yet seen, over all instances.
    pub fn unread_whispers_total(&self) -> u32 {
        self.instances.values().map(|i| i.unread_whispers).sum()
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

fn rank(phase: &Phase) -> u8 {
    if phase.needs_attention() {
        0
    } else if phase.is_running() {
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

    fn line(n: u64) -> ChatLine {
        ChatLine { instance: "k".into(), account: "Alex".into(), batch: "b".into(), at: 0, whisper: false, from: None, text: n.to_string() }
    }

    #[test]
    fn chat_keeps_the_newest_lines_and_numbers_them() {
        let mut m = Model::default();
        for n in 0..25 {
            m.push_chat(line(n), 20);
        }
        assert_eq!(m.chat_seq, 25);
        assert_eq!(m.chat.len(), 19);
        assert_eq!(m.chat_base(), 6);
        assert_eq!(m.chat.first().map(|l| l.text.as_str()), Some("6"));
        assert_eq!(m.chat.last().map(|l| l.text.as_str()), Some("24"));
    }

    #[test]
    fn whispers_add_up_over_instances() {
        let mut m = Model::default();
        for (key, n) in [("1", 2), ("2", 0), ("3", 5)] {
            let mut i = instance(key, key, "a", Phase::Stopped);
            i.unread_whispers = n;
            m.instances.insert(key.into(), i);
        }
        assert_eq!(m.unread_whispers_total(), 7);
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
