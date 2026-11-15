//! What the views show: accounts, batches and the live status of every instance, held in one entity.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use gpui_kit::{App, AppContext, Entity, EventEmitter, Global, RenderImage};
use uuid::Uuid;

use crate::core::account::Account;
use crate::core::batch::Batch;
use crate::core::fit::Fit;
use crate::core::fleet::{self, Target};
use crate::core::history::{self, Entry};
use crate::core::instance::Instance;
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

    /// Whether the instance is on its way or in the game: a process exists, or a restart after a crash is due.
    pub fn is_active(&self) -> bool {
        self.is_running() || matches!(self, Phase::Restarting { .. })
    }

    /// The server the instance is on or was last on, when it is known.
    pub fn server(&self) -> Option<&str> {
        match self {
            Phase::Connecting { server } | Phase::Queue { server } | Phase::Playing { server, .. } => Some(server),
            Phase::Disconnected { server, .. } | Phase::Dead { server, .. } | Phase::Banned { server, .. } => server.as_deref(),
            _ => None,
        }
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

/// Live status of one account in one batch, or playing an instance alone when `batch_id` is empty.
#[derive(Clone, Debug)]
pub struct Game {
    pub key: String,
    pub batch_id: String,
    pub instance_id: String,
    pub account_id: Uuid,
    pub name: String,
    pub phase: Phase,
    pub reconnect_attempts: u32,
    pub unread_whispers: u32,
    pub preview: Option<Arc<RenderImage>>,
    pub preview_seq: u32,
    /// Whether `preview` came from the running game; otherwise it is the still of an earlier run.
    pub live: bool,
    pub stopping: bool,
}

/// How long after a server line the same line from another instance counts as the same line.
pub const CHAT_MERGE: Duration = Duration::from_secs(5);
const CHAT_RECENT_KEPT: usize = 512;

/// One line of the merged chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLine {
    pub instance: String,
    /// Name of the account whose game showed the line.
    pub account: String,
    pub batch: String,
    /// The other instances that received the same line from the same server, by key, account name and batch.
    pub also: Vec<(String, String, String)>,
    /// Seconds since the Unix epoch when the line arrived.
    pub at: i64,
    pub whisper: bool,
    pub from: Option<String>,
    pub text: String,
}

/// Sent instead of a notify when only chat lines arrived, which nothing but the chat page shows.
pub struct ChatArrived;

impl EventEmitter<ChatArrived> for Model {}

/// Everything the views read. Only `ui::state` changes it.
#[derive(Default)]
pub struct Model {
    pub accounts: Vec<Account>,
    pub batches: Vec<Batch>,
    /// Whether accounts and batches were read from disk at least once.
    pub loaded: bool,
    pub instances: Vec<Instance>,
    pub games: HashMap<String, Game>,
    pub chat: Vec<ChatLine>,
    /// How many lines were ever added to `chat`, so a view can tell new lines from dropped old ones.
    pub chat_seq: u64,
    /// Where the latest line of each (server, sender, text) went, so the copies other instances receive join it.
    pub chat_recent: HashMap<(String, String, String), (u64, Instant)>,
    pub playing: Option<String>,
    pub error: Option<String>,
    /// Batches whose game is being installed, with the percent done.
    pub preparing: HashMap<String, u8>,
    /// Changes whenever an instance changes place in the sort order, so views know when to sort again.
    pub order_seq: u64,
    /// Last events of every instance by key, oldest first, at most `history::KEPT` each.
    pub history: HashMap<String, history::Log>,
    /// Skin faces by account id, for tiles of instances without a still.
    pub faces: HashMap<Uuid, Arc<RenderImage>>,
    /// What the updater has found, downloaded and kept.
    pub updates: Updates,
    /// Packs and Prism instances being imported, oldest first.
    pub imports: Vec<Import>,
    /// Batches being written as a `.mrpack`, by id.
    pub exporting: HashSet<String>,
    /// Why the last message to the phone did not arrive, until one does.
    pub phone_trouble: Option<String>,
}

/// An import as the sidebar shows it.
#[derive(Clone, Debug)]
pub struct Import {
    pub id: u64,
    pub name: String,
    pub percent: Option<u8>,
    /// Set to stop the import; the row stays until it stopped.
    pub cancel: Arc<AtomicBool>,
}

/// Where the updater is, as the sidebar footer and Settings show it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Current,
    /// A newer release; `installs` when this install can take it by itself, otherwise its page is the way.
    Available { version: String, page: String, installs: bool },
    Downloading { version: String, percent: Option<u8> },
    /// Downloaded and verified; it goes in when the app quits.
    Ready { version: String },
}

/// The updater's part of the model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Updates {
    pub status: UpdateStatus,
    /// Version and first line of the notes of the update just installed, until dismissed.
    pub news: Option<(String, String)>,
    /// The version kept for going back.
    pub previous: Option<String>,
    pub trouble: Option<crate::core::updater::Trouble>,
    /// The check the user asked for could not reach GitHub.
    pub offline: bool,
    /// The user asked for the running check or download, so its progress shows in the sidebar too.
    pub asked: bool,
    /// The running download was stopped and is winding down.
    pub cancelling: bool,
}

impl Model {
    /// Instances of `batch_id` (all batches when `None`), attention first, then running, then stopped, each by name.
    pub fn sorted(&self, batch_id: Option<&str>) -> Vec<&Game> {
        let mut list: Vec<&Game> = self.games.values().filter(|i| batch_id.is_none_or(|b| i.batch_id == b)).collect();
        list.sort_by_key(|i| (rank(&i.phase), i.name.to_lowercase(), i.key.clone()));
        list
    }

    /// How many instances of `batch_id` (all when `None`) need attention.
    pub fn attention_count(&self, batch_id: Option<&str>) -> usize {
        self.games.values().filter(|i| batch_id.is_none_or(|b| i.batch_id == b) && i.phase.needs_attention()).count()
    }

    /// The saved account with this id.
    pub fn account(&self, id: Uuid) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// The batch with this id.
    pub fn batch(&self, id: &str) -> Option<&Batch> {
        self.batches.iter().find(|b| b.id == id)
    }

    /// The instance with this id.
    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.instances.iter().find(|i| i.id == id)
    }

    /// The instance batch `id` plays.
    pub fn instance_of(&self, batch_id: &str) -> Option<&Instance> {
        self.batch(batch_id).and_then(|b| self.instance(&b.instance_id))
    }

    /// What `owner` plays: a batch with its instance, or an instance played alone by the games it owns.
    pub fn target(&self, owner: &str) -> Option<Target> {
        if let Some(batch) = self.batch(owner) {
            return Some(Target::of_batch(batch, self.instance(&batch.instance_id)?));
        }
        let game = self.games.values().find(|g| g.batch_id == owner)?;
        Some(Target::solo(self.instance(&game.instance_id)?))
    }

    /// Where `account` already plays `instance` other than in game `key`: the batch name, or the instance's when alone.
    pub fn playing_elsewhere(&self, instance: &str, account: Uuid, key: &str) -> Option<String> {
        let other = self.games.values().find(|g| g.key != key && g.account_id == account && g.instance_id == instance && g.phase.is_active())?;
        match self.batch(&other.batch_id) {
            Some(batch) => Some(batch.name.clone()),
            None => self.instance(instance).map(|i| trf("%@ · allein", &[&i.name])),
        }
    }

    /// Games of `instance` played alone.
    pub fn solo_games(&self, instance: &str) -> impl Iterator<Item = &Game> {
        let owner = fleet::solo_owner(instance);
        self.games.values().filter(move |g| g.batch_id == owner)
    }

    /// Adds a chat line. A `shared` line, one the whole server sees, that another instance on `server` received within `CHAT_MERGE` joins that line instead of repeating it.
    pub fn receive_chat(&mut self, line: ChatLine, server: Option<&str>, shared: bool, now: Instant, kept: usize) {
        if let (true, Some(server)) = (shared, server) {
            let key = (server.to_owned(), line.from.clone().unwrap_or_default(), line.text.clone());
            let base = self.chat_base();
            if let Some((seq, at)) = self.chat_recent.get(&key).copied()
                && now.duration_since(at) < CHAT_MERGE
                && seq >= base
                && let Some(first) = self.chat.get_mut((seq - base) as usize)
                && first.instance != line.instance
                && !first.also.iter().any(|(k, ..)| *k == line.instance)
            {
                first.also.push((line.instance, line.account, line.batch));
                return;
            }
            if self.chat_recent.len() >= CHAT_RECENT_KEPT {
                self.chat_recent.retain(|_, (_, at)| now.duration_since(*at) < CHAT_MERGE);
                if self.chat_recent.len() >= CHAT_RECENT_KEPT {
                    self.chat_recent.clear();
                }
            }
            self.chat_recent.insert(key, (self.chat_seq, now));
        }
        self.push_chat(line, kept);
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
        self.games.values().map(|i| i.unread_whispers).sum()
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
        return trf("Ein Account braucht %@, dein PC hat %@ frei", &[&label(fit.needed_mb), &label(fit.free_mb)]);
    }
    trf("%lld Accounts × %@ = %@, dein PC hat %@ frei", &[&count, &label(u64::from(each_mb)), &label(fit.needed_mb), &label(fit.free_mb)])
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
    } else if phase.is_active() {
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
        assert_eq!(fit_line(50, 2048, &fit(102_400, 32_768)), "50 Accounts × 2 GB = 100 GB, dein PC hat 32 GB frei");
        assert_eq!(fit_line(3, 2560, &fit(7680, 4096)), "3 Accounts × 2,5 GB = 7,5 GB, dein PC hat 4 GB frei");
        assert_eq!(fit_line(1, 8192, &fit(8192, 4096)), "Ein Account braucht 8 GB, dein PC hat 4 GB frei");
    }

    fn instance(key: &str, name: &str, batch: &str, phase: Phase) -> Game {
        Game { key: key.into(), batch_id: batch.into(), instance_id: batch.into(), account_id: Uuid::nil(), name: name.into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, live: false, stopping: false }
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
            m.games.insert(i.key.clone(), i);
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
            m.games.insert(i.key.clone(), i);
        }
        let names: Vec<&str> = m.sorted(None).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["bob", "zed", "amy"]);
    }

    #[test]
    fn banned_needs_attention_and_runs() {
        let banned = Phase::Banned { server: None, reason: "x".into() };
        assert!(banned.needs_attention() && banned.is_running());
        let mut m = Model::default();
        m.games.insert("1".into(), instance("1", "a", "a", banned));
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
        ChatLine { instance: "k".into(), account: "Alex".into(), batch: "b".into(), also: Vec::new(), at: 0, whisper: false, from: None, text: n.to_string() }
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
            m.games.insert(key.into(), i);
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

    fn said(key: &str, account: &str, from: Option<&str>, text: &str) -> ChatLine {
        ChatLine { instance: key.into(), account: account.into(), batch: "b".into(), also: Vec::new(), at: 0, whisper: false, from: from.map(Into::into), text: text.into() }
    }

    #[test]
    fn one_server_line_received_by_many_instances_is_one_line() {
        let mut m = Model::default();
        let t0 = Instant::now();
        for (n, key) in ["1", "2", "3"].into_iter().enumerate() {
            m.receive_chat(said(key, &format!("Dev{key}"), Some("Notch"), "hi"), Some("s"), true, t0 + Duration::from_millis(n as u64 * 50), 100);
        }
        assert_eq!(m.chat.len(), 1);
        assert_eq!(m.chat_seq, 1);
        assert_eq!(m.chat[0].account, "Dev1");
        assert_eq!(m.chat[0].also, [("2".to_owned(), "Dev2".to_owned(), "b".to_owned()), ("3".to_owned(), "Dev3".to_owned(), "b".to_owned())]);
    }

    #[test]
    fn same_text_later_or_from_elsewhere_stays_separate() {
        let mut m = Model::default();
        let t0 = Instant::now();
        m.receive_chat(said("1", "A", Some("Notch"), "hi"), Some("s"), true, t0, 100);
        m.receive_chat(said("2", "B", Some("Notch"), "hi"), Some("s"), true, t0 + CHAT_MERGE, 100);
        m.receive_chat(said("3", "C", Some("Notch"), "hi"), Some("other"), true, t0, 100);
        m.receive_chat(said("4", "D", Some("Alina"), "hi"), Some("s"), true, t0, 100);
        m.receive_chat(said("5", "E", Some("Notch"), "ho"), Some("s"), true, t0, 100);
        assert_eq!(m.chat.len(), 5);
    }

    #[test]
    fn the_same_instance_repeating_itself_is_not_merged() {
        let mut m = Model::default();
        let t0 = Instant::now();
        for _ in 0..3 {
            m.receive_chat(said("1", "A", Some("Notch"), "hi"), Some("s"), true, t0, 100);
        }
        assert_eq!(m.chat.len(), 3);
    }

    #[test]
    fn lines_not_shared_by_the_server_stay_per_account() {
        let mut m = Model::default();
        let t0 = Instant::now();
        for key in ["1", "2"] {
            m.receive_chat(said(key, "A", Some("Notch"), "psst"), Some("s"), false, t0, 100);
            m.receive_chat(said(key, "A", Some("Notch"), "hello"), None, true, t0, 100);
        }
        assert_eq!(m.chat.len(), 4);
    }

    #[test]
    fn a_trimmed_first_copy_does_not_swallow_the_next() {
        let mut m = Model::default();
        let t0 = Instant::now();
        m.receive_chat(said("1", "A", Some("Notch"), "hi"), Some("s"), true, t0, 10);
        for n in 0..10 {
            m.receive_chat(said("1", "A", None, &format!("x{n}")), None, false, t0, 10);
        }
        m.receive_chat(said("2", "B", Some("Notch"), "hi"), Some("s"), true, t0, 10);
        assert_eq!(m.chat.last().map(|l| l.account.as_str()), Some("B"));
    }

    #[test]
    fn recent_table_stays_bounded() {
        let mut m = Model::default();
        let t0 = Instant::now();
        for n in 0..5000 {
            m.receive_chat(said("1", "A", Some("Notch"), &n.to_string()), Some("s"), true, t0 + Duration::from_millis(n), 100);
        }
        assert!(m.chat_recent.len() <= 512);
    }
}
