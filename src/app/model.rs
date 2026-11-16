//! The application state every front end shows: accounts, instances, batches, games with their phase, chat, history and the updater.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::core::account::Account;
use crate::core::batch::Batch;
use crate::core::fleet::{self, Target};
use crate::core::history::{self, Entry};
use crate::core::instance::Instance;
use crate::core::i18n::{tr, trf};
use crate::core::proxy::Fault;

use super::session::{Frame, Session};

/// How far apart, in seconds, drops may lie to count as one outage.
const OUTAGE_WINDOW: u64 = 60;

/// Where an instance is in its life, as the grid shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Stopped,
    /// On its way: the `place`-th of `total` accounts of one start waits for its turn; `place` is 0 once its game launched.
    Starting { place: usize, total: usize },
    /// Runs as a normal game window without Manycraft's mod: no preview, no background mode, no swap.
    Plain,
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
    /// In the game, but the network is gone; reconnects to `server` by itself when it is back.
    NetworkLost { server: String },
    /// Not started because there was no connection; starts by itself when it is back.
    NoNetwork,
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

    /// Whether Stop has something to end: an active instance, or a start that waits for the connection.
    pub fn stoppable(&self) -> bool {
        self.is_active() || matches!(self, Phase::NoNetwork)
    }

    /// The server the instance is on or was last on, when it is known.
    pub fn server(&self) -> Option<&str> {
        match self {
            Phase::Connecting { server } | Phase::Queue { server } | Phase::Playing { server, .. } | Phase::NetworkLost { server } => Some(server),
            Phase::Disconnected { server, .. } | Phase::Dead { server, .. } | Phase::Banned { server, .. } => server.as_deref(),
            _ => None,
        }
    }

    /// Whether the instance is on a server, where a chat line can go out.
    pub fn on_server(&self) -> bool {
        matches!(self, Phase::Queue { .. } | Phase::Playing { .. })
    }

    /// Whether the game waits in line for its turn to start and has no process yet.
    pub fn waiting(&self) -> bool {
        matches!(self, Phase::Starting { place, .. } if *place > 0)
    }

    /// Whether the game counts as running: it has a process, or its process is on its way after it waited in line.
    pub fn has_process(&self) -> bool {
        self.is_running() && !self.waiting()
    }

    /// Whether a game process exists for the instance.
    pub fn is_running(&self) -> bool {
        !matches!(self, Phase::Stopped | Phase::Crashed { .. } | Phase::Restarting { .. } | Phase::LoginNeeded | Phase::Failed { .. } | Phase::NoNetwork)
    }

    /// Whether the game window can be swapped in: the mod answers and the game is past its start.
    pub fn playable(&self) -> bool {
        matches!(self, Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } | Phase::NetworkLost { .. } | Phase::Disconnected { .. } | Phase::Dead { .. })
    }

    /// What kind of work the user has here, when there is some.
    pub fn need(&self) -> Option<Need> {
        match self {
            Phase::LoginNeeded => Some(Need::SignIn),
            Phase::Disconnected { .. } => Some(Need::Disconnected),
            Phase::Dead { .. } => Some(Need::Dead),
            Phase::Crashed { .. } => Some(Need::Crashed),
            Phase::Failed { .. } => Some(Need::Failed),
            Phase::Banned { .. } => Some(Need::Banned),
            _ => None,
        }
    }

    /// Whether a reconnect can be asked of a game in this phase: it runs with the mod, is past its start and is not banned.
    pub fn reconnectable(&self) -> bool {
        self.is_running() && !matches!(self, Phase::Starting { .. } | Phase::Plain | Phase::Banned { .. })
    }
}

/// The kinds of work an account can need, in the order the grid lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Need {
    SignIn,
    Disconnected,
    Dead,
    Crashed,
    Failed,
    Banned,
}

impl Need {
    pub const ALL: [Need; 6] = [Need::SignIn, Need::Disconnected, Need::Dead, Need::Crashed, Need::Failed, Need::Banned];
}

/// How many of a set of games each selection action acts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Able {
    pub start: usize,
    pub stop: usize,
    /// Chosen games whose stop is overdue, which a stop now ends at once.
    pub force: usize,
    pub restart: usize,
    pub reconnect: usize,
    pub message: usize,
}

/// Many games of one batch dropped off the server together and wait to get back on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outage {
    /// Seconds since 1970 of the first drop among them, when it was recorded.
    pub since: Option<u64>,
    /// The games that dropped and can be reconnected, by key.
    pub keys: Vec<String>,
}

/// A bulk start under way: how many of its games launched, of how many, and how many wait for free memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub launched: usize,
    pub total: usize,
    pub memory: usize,
}

/// Extra facts about a live instance that its phase does not carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hint {
    /// The game runs but its link to the manager dropped.
    pub link_lost: bool,
    /// The account's proxy failed its check after a connection failure.
    pub proxy: Option<Fault>,
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
    pub stopping: bool,
    /// The stop has run for a while without the game ending; stopping it again ends it at once.
    pub overdue: bool,
    pub hint: Hint,
    /// Its place in the line of bulk starts that wait for free memory, while it waits there.
    pub memory_wait: Option<u64>,
}

impl Game {
    /// Whether the game plays its instance alone, as a normal window the manager never swaps, hides or previews.
    pub fn alone(&self) -> bool {
        fleet::solo_instance(&self.batch_id).is_some()
    }

    /// A stopped game of `account_id` named `name`, played by `batch_id` with instance `instance_id`.
    pub fn new(key: String, batch_id: String, instance_id: String, account_id: Uuid, name: String) -> Self {
        Self { key, batch_id, instance_id, account_id, name, phase: Phase::Stopped, reconnect_attempts: 0, unread_whispers: 0, stopping: false, overdue: false, hint: Hint::default(), memory_wait: None }
    }
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

/// Everything the views read. Only the reducer and the shell that runs its effects change it.
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
    /// What the updater has found, downloaded and kept.
    pub updates: Updates,
    /// Packs and Prism instances being imported, oldest first.
    pub imports: Vec<Import>,
    /// Batches being written as a `.mrpack`, by id.
    pub exporting: HashSet<String>,
    /// Why the last message to the phone did not arrive, until one does.
    pub phone_trouble: Option<String>,
    /// Whether the connection is known to be gone.
    pub offline: bool,
    /// The frame each tile has, by game key.
    pub frames: HashMap<String, Frame>,
    /// The memory one game of each batch was last measured to hold, in MB, by batch id.
    pub measured: HashMap<String, u32>,
    /// The account chosen to play instances alone that name none, as the settings keep it.
    pub active_account: Option<Uuid>,
    pub session: Session,
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
    /// The download just stopped; the sidebar says so for a moment instead of offering it again under the pointer.
    pub cancelled: bool,
}

impl Model {
    /// Instances of `batch_id` (all batches when `None`), attention first, then running, then stopped, each by name.
    pub fn sorted(&self, batch_id: Option<&str>) -> Vec<&Game> {
        let mut list: Vec<&Game> = self.games.values().filter(|i| batch_id.is_none_or(|b| i.batch_id == b)).collect();
        list.sort_by_key(|i| (rank(&i.phase), i.name.to_lowercase(), i.key.clone()));
        list
    }

    /// Whether game `key` waits for free memory before it starts.
    pub fn waits_for_memory(&self, key: &str) -> bool {
        self.games.get(key).is_some_and(|g| g.memory_wait.is_some() && g.phase.waiting())
    }

    /// The bulk start of `owner` (of every batch when `None`) while some of its games still wait to launch.
    pub fn run(&self, owner: Option<&str>) -> Option<Run> {
        let mut run = Run::default();
        for (_, keys) in self.session.runs.iter().filter(|(o, _)| owner.is_none_or(|w| w == o.as_str())) {
            for g in keys.iter().filter_map(|k| self.games.get(k)) {
                if g.phase.waiting() {
                    run.total += 1;
                    run.memory += usize::from(g.memory_wait.is_some());
                } else if g.phase.is_running() {
                    run.launched += 1;
                    run.total += 1;
                }
            }
        }
        (run.total > run.launched).then_some(run)
    }

    /// How many instances of `batch_id` (all when `None`) need attention.
    pub fn attention_count(&self, batch_id: Option<&str>) -> usize {
        self.games.values().filter(|i| batch_id.is_none_or(|b| i.batch_id == b) && !i.alone() && i.phase.needs_attention()).count()
    }

    /// How many instances of `batch_id` (all when `None`) need each kind of work, in the grid's order, leaving out kinds nobody needs.
    pub fn needs(&self, batch_id: Option<&str>) -> Vec<(Need, usize)> {
        let mut counts: HashMap<Need, usize> = HashMap::new();
        for need in self.games.values().filter(|i| batch_id.is_none_or(|b| i.batch_id == b) && !i.alone()).filter_map(|i| i.phase.need()) {
            *counts.entry(need).or_default() += 1;
        }
        Need::ALL.into_iter().filter_map(|n| counts.get(&n).map(|c| (n, *c))).collect()
    }

    /// How many of `keys` each selection action would act on.
    pub fn able(&self, keys: &[String]) -> Able {
        let mut n = Able::default();
        for i in keys.iter().filter_map(|k| self.games.get(k)) {
            let fresh = !i.stopping;
            n.start += usize::from(fresh && !i.phase.is_running() && i.phase != Phase::LoginNeeded);
            n.stop += usize::from((fresh || i.overdue) && i.phase.stoppable());
            n.force += usize::from(i.stopping && i.overdue && i.phase.stoppable());
            n.restart += usize::from(fresh && i.phase != Phase::LoginNeeded);
            n.reconnect += usize::from(fresh && i.phase.reconnectable());
            n.message += usize::from(i.phase.on_server());
        }
        n
    }

    /// The games of `batch_id` that lost the server together, within a minute of each other, and wait to reconnect or gave up,
    /// when they are at least two and at least half of the batch's running games: a server restart rather than one account's bad luck.
    /// Kicks and bans never count.
    pub fn outage(&self, batch_id: &str) -> Option<Outage> {
        let mine: Vec<&Game> = self.games.values().filter(|i| i.batch_id == batch_id && !i.stopping).collect();
        let mut dropped: Vec<(u64, &Game)> = mine
            .iter()
            .copied()
            .filter(|i| matches!(i.phase, Phase::Disconnected { .. }) || (matches!(i.phase, Phase::Connecting { .. }) && i.reconnect_attempts > 0))
            .filter_map(|i| {
                let log = self.history.get(&i.key)?;
                let run: Vec<&history::Entry> = log.iter().rev().take_while(|e| e.kind != history::Kind::Connected).collect();
                let first = run.last()?;
                run.iter().all(|e| e.kind == history::Kind::Disconnected).then_some((first.at, i))
            })
            .collect();
        dropped.sort_by_key(|(at, i)| (*at, i.key.clone()));
        let mut best = 0..0;
        let mut from = 0;
        for to in 0..dropped.len() {
            while dropped[to].0 - dropped[from].0 > OUTAGE_WINDOW {
                from += 1;
            }
            if to + 1 - from >= best.len() {
                best = from..to + 1;
            }
        }
        let together = &dropped[best];
        let running = mine.iter().filter(|i| i.phase.is_running() && !matches!(i.phase, Phase::Banned { .. })).count();
        if together.len() < 2 || together.len() * 2 < running {
            return None;
        }
        let since = together.first().map(|(at, _)| *at);
        let mut keys: Vec<String> = together.iter().map(|(_, i)| i.key.clone()).collect();
        keys.sort_by_key(|k| (self.games.get(k).map(|i| i.name.to_lowercase()), k.clone()));
        Some(Outage { since, keys })
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

    /// Whether a game of `account` waits for it to sign in again.
    pub fn sign_in_expired(&self, account: Uuid) -> bool {
        self.games.values().any(|g| g.account_id == account && g.phase == Phase::LoginNeeded)
    }

    /// Games of `instance` played alone.
    pub fn solo_games(&self, instance: &str) -> impl Iterator<Item = &Game> {
        let owner = fleet::solo_owner(instance);
        self.games.values().filter(move |g| g.batch_id == owner)
    }

    /// The hint of game `key`; empty when nothing is known.
    pub fn hint(&self, key: &str) -> Hint {
        self.games.get(key).map(|g| g.hint).unwrap_or_default()
    }

    /// Changes the hint of game `key`, when there is such a game.
    pub fn set_hint(&mut self, key: &str, change: impl FnOnce(&mut Hint)) {
        if let Some(game) = self.games.get_mut(key) {
            change(&mut game.hint);
        }
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

/// The main action of a tile, by the phase of its game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primary {
    Play,
    Reconnect,
    Respawn,
    Start,
    Retry,
    SignIn,
    /// Ends the account's game in another instance and starts this one.
    TakeOver,
    /// Ends a game whose stop is overdue at once.
    ForceQuit,
}

/// How far a stop has come: not stopping, stopping, or stopping for long enough to end the game at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopping {
    No,
    Yes,
    Overdue,
}

impl Stopping {
    /// The stop of a game from its `stopping` and `overdue` flags.
    pub fn of(stopping: bool, overdue: bool) -> Self {
        match (stopping, overdue) {
            (false, _) => Self::No,
            (true, false) => Self::Yes,
            (true, true) => Self::Overdue,
        }
    }
}

/// The main action of a game in `phase`, `busy` when its account plays the same instance in another game.
pub fn main_action(phase: &Phase, stopping: Stopping, busy: bool) -> Option<Primary> {
    match primary(phase, stopping) {
        Some(Primary::Start) if busy && !phase.is_active() => Some(Primary::TakeOver),
        other => other,
    }
}

/// The main action of a game in `phase`; nothing while it stops or while nothing helps, ending it at once once its stop is overdue.
pub fn primary(phase: &Phase, stopping: Stopping) -> Option<Primary> {
    match stopping {
        Stopping::Overdue => return Some(Primary::ForceQuit),
        Stopping::Yes => return None,
        Stopping::No => {}
    }
    match phase {
        Phase::Disconnected { .. } => Some(Primary::Reconnect),
        Phase::Dead { .. } => Some(Primary::Respawn),
        Phase::Stopped | Phase::Crashed { .. } | Phase::Failed { .. } => Some(Primary::Start),
        Phase::LoginNeeded => Some(Primary::SignIn),
        Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } | Phase::NetworkLost { .. } => Some(Primary::Play),
        Phase::NoNetwork => Some(Primary::Retry),
        Phase::Starting { .. } | Phase::Plain | Phase::Banned { .. } => None,
        Phase::Restarting { .. } => Some(Primary::Start),
    }
}

impl Primary {
    /// The label of its button.
    pub fn label(self) -> String {
        match self {
            Self::Play => tr("Spielen"),
            Self::Reconnect => tr("Neu verbinden"),
            Self::Respawn => tr("Wiederbeleben"),
            Self::Start => tr("Starten"),
            Self::Retry => tr("Erneut versuchen"),
            Self::SignIn => tr("Anmelden"),
            Self::TakeOver => tr("Dort beenden und hier starten"),
            Self::ForceQuit => tr("Sofort beenden"),
        }
    }
}

/// The ban that is still the latest event of a stopped instance, as its status line, so a stop does not hide it.
pub fn stopped_ban(i: &Game, m: &Model) -> Option<String> {
    if i.phase != Phase::Stopped || i.stopping {
        return None;
    }
    let last = m.history.get(&i.key)?.back()?;
    (last.kind == history::Kind::Banned).then(|| format!("{} · {}", tr("Gestoppt"), banned_status(&last.reason)))
}

/// The phase of an instance as one line of German for its tile.
pub fn status(i: &Game, preparing: Option<u8>) -> String {
    if i.stopping {
        return tr("Wird gestoppt…");
    }
    let hint = i.hint;
    if let Some(fault) = hint.proxy
        && matches!(i.phase, Phase::Connecting { .. } | Phase::Disconnected { .. })
    {
        return proxy_status(fault);
    }
    if hint.link_lost && i.phase.is_running() {
        return link_status();
    }
    match &i.phase {
        Phase::Stopped => tr("Gestoppt"),
        Phase::Starting { .. } if preparing.is_some() && i.memory_wait.is_none() => trf("Lädt Minecraft · %lld %%", &[&preparing.unwrap_or(0)]),
        Phase::Restarting { attempt, of } => restart_status(*attempt, *of),
        Phase::Starting { .. } if i.memory_wait.is_some() => tr("Wartet auf Arbeitsspeicher"),
        Phase::Starting { place, total } if *total > 1 && *place > 0 => trf("Wartet auf Start (%lld von %lld)", &[place, total]),
        Phase::Starting { .. } => tr("Startet…"),
        Phase::Plain => tr("Läuft · ohne Vorschau"),
        Phase::Menu => tr("Im Hauptmenü"),
        Phase::Connecting { .. } if i.reconnect_attempts > 0 => trf("Verbindet neu · Versuch %lld von %lld", &[&i.reconnect_attempts, &crate::core::settings::active().reconnect.max_attempts]),
        Phase::Connecting { server } if server.is_empty() => tr("Verbindet…"),
        Phase::Connecting { server } => trf("Verbindet mit %@…", &[server]),
        Phase::Queue { .. } => tr("In der Warteschlange"),
        Phase::Playing { server, .. } if server.is_empty() => tr("Spielt"),
        Phase::Playing { server, .. } => trf("Auf %@", &[server]),
        Phase::Disconnected { reason, .. } if reason.is_empty() => tr("Getrennt"),
        Phase::Disconnected { reason, .. } => trf("Getrennt · %@", &[reason]),
        Phase::Dead { .. } => tr("Gestorben"),
        Phase::Crashed { .. } => tr("Abgestürzt"),
        Phase::Banned { reason, .. } => banned_status(reason),
        Phase::LoginNeeded => tr("Anmeldung nötig"),
        Phase::Failed { reason } => trf("Start fehlgeschlagen · %@", &[reason]),
        Phase::NetworkLost { .. } => tr("Keine Verbindung · verbindet sich wieder, sobald das Internet zurück ist"),
        Phase::NoNetwork => tr("Keine Verbindung · startet, sobald das Internet zurück ist"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(key: &str, name: &str, batch: &str, phase: Phase) -> Game {
        Game { phase, ..Game::new(key.into(), batch.into(), batch.into(), Uuid::nil(), name.into()) }
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

    fn mixed() -> Model {
        let mut m = Model::default();
        let lost = || Phase::Disconnected { server: Some("s".into()), reason: "Zeitüberschreitung".into() };
        for (key, phase) in [
            ("1", Phase::LoginNeeded),
            ("2", lost()),
            ("3", Phase::Banned { server: None, reason: "x".into() }),
            ("4", lost()),
            ("5", Phase::Crashed { code: Some(1) }),
            ("6", Phase::Playing { server: "s".into(), since: Instant::now() }),
            ("7", Phase::Stopped),
            ("8", Phase::LoginNeeded),
        ] {
            m.games.insert(key.into(), instance(key, key, "a", phase));
        }
        m.games.insert("9".into(), instance("9", "9", "b", Phase::Dead { server: None, reason: String::new() }));
        m
    }

    #[test]
    fn needs_are_counted_per_kind_in_a_fixed_order() {
        let m = mixed();
        assert_eq!(m.needs(Some("a")), [(Need::SignIn, 2), (Need::Disconnected, 2), (Need::Crashed, 1), (Need::Banned, 1)]);
        assert_eq!(m.needs(None).iter().map(|(_, n)| n).sum::<usize>(), m.attention_count(None));
        assert_eq!(m.needs(Some("b")), [(Need::Dead, 1)]);
        assert!(m.needs(Some("none")).is_empty());
        assert!(Phase::Restarting { attempt: 1, of: 3 }.need().is_none() && Phase::Stopped.need().is_none());
    }

    #[test]
    fn selection_actions_count_only_the_games_they_can_act_on() {
        let m = mixed();
        let keys: Vec<String> = (1..=9).map(|n| n.to_string()).collect();
        let n = m.able(&keys);
        assert_eq!(n.reconnect, 4, "the playing, the dead and the two disconnected, never banned or signed out");
        assert_eq!(n.start, 2, "stopped and crashed, nothing that needs a sign-in");
        assert_eq!(n.stop, 5);
        assert_eq!(n.restart, 7);
        assert_eq!(n.message, 1);
        let mut m = m;
        m.games.get_mut("6").unwrap().stopping = true;
        assert_eq!(m.able(&keys).reconnect, 3);
        assert_eq!(m.able(&["gone".into()]), Able::default());
    }

    #[test]
    fn an_outage_needs_most_of_the_batch_dropped_and_starts_at_the_first_drop() {
        let mut m = Model::default();
        for n in 0..4 {
            m.games.insert(n.to_string(), instance(&n.to_string(), &format!("p{n}"), "a", Phase::Playing { server: "s".into(), since: Instant::now() }));
        }
        let note = |m: &mut Model, key: &str, at: u64, kind: history::Kind| history::push(m.history.entry(key.into()).or_default(), Entry { at, kind, server: None, reason: String::new() });
        let drop = |m: &mut Model, key: &str, at: u64, waits: bool| {
            let i = m.games.get_mut(key).unwrap();
            i.phase = if waits { Phase::Connecting { server: "s".into() } } else { Phase::Disconnected { server: Some("s".into()), reason: String::new() } };
            i.reconnect_attempts = 2;
            note(m, key, at, history::Kind::Disconnected);
        };
        drop(&mut m, "0", 100, true);
        assert!(m.outage("a").is_none(), "one drop is bad luck");
        drop(&mut m, "1", 90, false);
        let outage = m.outage("a").expect("half the batch dropped");
        assert_eq!(outage.since, Some(90));
        assert_eq!(outage.keys, ["0", "1"]);
        note(&mut m, "1", 400, history::Kind::Disconnected);
        assert_eq!(m.outage("a").and_then(|o| o.since), Some(90), "failed retries do not move the start");
        m.games.get_mut("0").unwrap().reconnect_attempts = 0;
        assert!(m.outage("a").is_none(), "a first connect is no reconnect");
        m.games.insert("4".into(), instance("4", "p4", "a", Phase::Banned { server: None, reason: String::new() }));
        m.games.get_mut("0").unwrap().reconnect_attempts = 1;
        assert_eq!(m.outage("a").map(|o| o.keys.len()), Some(2), "banned ones are not part of it");
        assert!(m.outage("b").is_none());
        note(&mut m, "0", 4000, history::Kind::Connected);
        drop(&mut m, "0", 9000, false);
        assert!(m.outage("a").is_none(), "drops hours apart are no outage");
        note(&mut m, "0", 9001, history::Kind::Kicked);
        drop(&mut m, "2", 9010, false);
        assert!(m.outage("a").is_none(), "a kick is no outage");
    }

    #[test]
    fn status_lines_for_banned_and_restart() {
        crate::core::i18n::init("de");
        assert_eq!(banned_status(""), "Gesperrt");
        assert_eq!(banned_status("Cheating"), "Gesperrt · Cheating");
        assert_eq!(restart_status(1, 3), "Startet neu · Versuch 1 von 3");
    }

    #[test]
    fn hints_belong_to_a_game_and_vanish_with_it() {
        let mut m = Model::default();
        m.set_hint("none", |h| h.link_lost = true);
        assert_eq!(m.hint("none"), Hint::default());
        m.games.insert("1".into(), instance("1", "a", "a", Phase::Menu));
        m.set_hint("1", |h| h.link_lost = true);
        assert!(m.hint("1").link_lost);
        m.games.remove("1");
        assert_eq!(m.hint("1"), Hint::default());
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
