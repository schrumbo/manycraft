//! What the views show: accounts, batches and the live status of every instance, held in one entity.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::{App, AppContext, Entity, Global, RenderImage};
use uuid::Uuid;

use crate::core::account::Account;
use crate::core::batch::Batch;

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
    LoginNeeded,
    Failed { reason: String },
}

impl Phase {
    /// Whether the user has work here: dropped off the server after auto-reconnect gave up, crashed, or must sign in.
    pub fn needs_attention(&self) -> bool {
        matches!(self, Phase::Disconnected { .. } | Phase::Dead { .. } | Phase::Crashed { .. } | Phase::LoginNeeded | Phase::Failed { .. })
    }

    /// Whether a game process exists for the instance.
    pub fn is_running(&self) -> bool {
        !matches!(self, Phase::Stopped | Phase::Crashed { .. } | Phase::LoginNeeded | Phase::Failed { .. })
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
}
