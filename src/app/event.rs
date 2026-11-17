//! Everything that happens to the application: reports of the games and the fleet, timers that fire, data read from disk and what the user asks for.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::effect::Unshown;
use super::session::Watch;
use super::model::Phase;
use crate::core::account::Account;
use crate::core::batch::Batch;
use crate::core::fit::Memory;
use crate::core::fleet::{Adoption, Quick};
use crate::core::history::Log;
use crate::core::instance::Instance;
use crate::core::ipc::{FromMod, HotkeyAction};
use crate::core::proxy::Fault;

/// One report of the fleet, a game's mod or a frame reader.
#[derive(Clone, Debug)]
pub enum Report {
    Preparing { batch: String, percent: u8 },
    /// The game is installed; the accounts of `batch` wait in line to start.
    Prepared { batch: String },
    Launched { key: String, plain: bool },
    Failed { key: String, reason: String, login: bool, offline: bool },
    /// Whether the connection came back (`true`) or is gone.
    Reach { online: bool },
    Exited { key: String, code: Option<i32> },
    Seed { key: String, phase: Phase, attempts: u32 },
    /// Whether the saved sign-in of an account still works, from a token refresh.
    SignIn { account: Uuid, valid: bool },
    /// What the check of an account's proxy found after a connection failure; `None` when it works.
    ProxyChecked { key: String, fault: Option<Fault> },
    /// Whether `server` answered its status ping.
    ServerProbed { server: String, up: bool },
    /// The game's mod connected from process `pid`.
    Connected { key: String, pid: u32 },
    /// The game's mod stopped answering.
    LinkLost { key: String },
    Mod { key: String, message: FromMod },
    /// Another start of Manycraft asked for the manager.
    Reopen,
    /// Another start of Manycraft or a shortcut asked to play `instance`.
    PlayRequest { instance: String, account: Option<String> },
    /// A new frame of the running game, the `image`-th of its batch.
    Frame { key: String, seq: u32, image: usize },
    /// The saved still of an earlier run, the `image`-th of its batch.
    Still { key: String, image: usize },
}

impl Report {
    /// The game the report is about.
    pub fn key(&self) -> Option<&str> {
        match self {
            Report::Launched { key, .. } | Report::Failed { key, .. } | Report::Exited { key, .. } | Report::Seed { key, .. } | Report::ProxyChecked { key, .. } => Some(key),
            Report::Connected { key, .. } | Report::LinkLost { key } | Report::Mod { key, .. } | Report::Frame { key, .. } | Report::Still { key, .. } => Some(key),
            Report::Preparing { .. } | Report::Prepared { .. } | Report::Reach { .. } | Report::ServerProbed { .. } | Report::SignIn { .. } | Report::Reopen | Report::PlayRequest { .. } => None,
        }
    }
}

/// Accounts, batches and instances as read from disk.
#[derive(Debug)]
pub struct Loaded {
    /// The data, or why it could not be read.
    pub data: Result<(Vec<Account>, Vec<Batch>, Vec<Instance>), String>,
    /// Saved event logs by game key.
    pub logs: Vec<(String, Log)>,
    /// What moving old data failed at.
    pub problems: Vec<String>,
    /// What was damaged and restored or lost, as a notice.
    pub recovered: Option<String>,
    /// Whether the message already shown stays.
    pub keep: bool,
    /// Games an earlier manager launched that still run.
    pub adopted: Vec<Adoption>,
    /// The memory one game of each batch was measured to hold in earlier runs, in MB, by batch id.
    pub measured: HashMap<String, u32>,
}

/// Something that happened.
#[derive(Debug)]
pub enum Event {
    /// What the fleet, the mods and the frame readers reported since the last batch, in order.
    Reports(Vec<Report>),
    /// The reconnect of `key` is due, if no newer reconnect or stop came since `generation`.
    ReconnectDue { key: String, generation: u64 },
    /// Time to ping `server` again while games wait for it.
    ProbeDue { server: String },
    /// The restart of `key` after a crash is due, if nothing newer came since `generation`.
    RestartDue { key: String, generation: u64 },
    /// The next join command of `key` is due; the rest follow `delay` apart.
    JoinDue { key: String, generation: u64, lines: Vec<String>, delay: Duration },
    /// A start that waited for the connection tries again.
    WaitingStartDue { key: String },
    /// The link to `key` has been gone for a while; its tile says so unless it came back or the game went down.
    LinkLostDue { key: String },
    /// The attention of `batch` collected for a moment goes out as one notification.
    AlertDue { batch: String },
    /// The phone messages due at `due` go out.
    PhoneDue { due: Instant },
    Loaded(Box<Loaded>),
    /// The window of a new game was put in its place.
    Placed { key: String },
    /// The game asked to show could not, as the manager has no place yet.
    NotShown(Box<Unshown>),
    InstanceAdded(Instance),
    InstanceRemoved(String),
    /// An instance was saved; it replaces the one with its id.
    InstanceChanged(Instance),
    /// A message for the next reload.
    Notice(String),
    /// A message to show now.
    Error(String),
    /// Whether the last phone message arrived, or why not.
    PhoneOutcome(Option<String>),
    Start(Vec<String>),
    /// Starts these games, leaving out those that must sign in again.
    StartMany(Vec<String>),
    StartBatch(String),
    StartAll,
    /// Stops these games; those whose stop is overdue end at once.
    Stop(Vec<String>),
    /// The stop of `key` has run for a while, if no newer stop or start came since `generation`.
    StopOverdue { key: String, generation: u64 },
    /// Stops the game that plays this game's account elsewhere in its instance, then starts this one.
    TakeOver(String),
    StopBatch(String),
    StopAll,
    /// Starts again the games one stop of the user ended, those still on their way down once they are down.
    StartAgain(Vec<String>),
    /// Starts the games of `batch` (all when `None`) that wait for free memory now, whether they fit or not.
    StartAnyway(Option<String>),
    /// Cancels the bulk start of `batch` (all when `None`): every game of it that has not launched yet stops.
    CancelStart(Option<String>),
    /// The memory of the machine, read for the games that wait for it; `None` when the machine does not tell.
    Memory(Option<Memory>),
    /// The free memory is due to be read again for the games that wait for it.
    MemoryDue,
    /// Stops the running ones of these games and starts them all again, those that ran once they are down.
    Restart(Vec<String>),
    Reconnect(String),
    /// Reconnects these games one after another, leaving out banned ones and those without a process.
    ReconnectMany(Vec<String>),
    Respawn(String),
    Play(String),
    /// Plays `instance` alone as a normal game window, with `account` or the one it plays with by default.
    PlayInstance { instance: String, account: Option<Uuid> },
    /// Plays `instance` alone with `account` and opens `quick`, a world or server, right away.
    QuickPlay { instance: String, account: Uuid, quick: Quick },
    /// Brings the window of game `key`, played alone, to the front.
    Raise(String),
    /// Makes this the account that plays instances alone that name none.
    ChooseAccount(Uuid),
    /// Makes `account` the one `instance` always plays with alone; `None` leaves it to the active account.
    SetDefaultAccount { instance: String, account: Option<Uuid> },
    /// Lists `instance` in the library under `group`; ungrouped when `None`.
    SetGroup { instance: String, group: Option<String> },
    /// Plays `instance` for another start or a shortcut, waiting for the first load, and shows it once it can be shown.
    RequestPlay { instance: String, account: Option<String> },
    ShowGrid,
    Step(HotkeyAction),
    SendChat { key: String, text: String },
    Broadcast { batch: String, text: String },
    /// Gives the accounts `ids` the colour `color`.
    Recolor { ids: Vec<Uuid>, color: u8 },
    /// The tiles on screen changed.
    Visible(HashSet<String>),
    /// Where the user looks changed.
    Watch(Watch),
    /// The preview rates worked out at `generation` are due to go out, unless a newer change came.
    ShotsDue { generation: u64 },
    /// The memory of the running games is due to be read.
    UsageDue,
    /// The memory each running game holds for itself, in MB, by key.
    Usage(Vec<(String, u64)>),
    /// Batches, accounts or settings changed; every connected game hears its identity again.
    TellGames,
    /// The user closed the manager window, where closing it can put it in the notification area.
    CloseWindow,
    /// The one-time hint about the notification area was seen.
    TrayExplained,
    /// The user asked to quit Manycraft.
    Quit,
    /// The answer to the quit question: whether the running games keep running.
    QuitChosen { keep: bool },
    /// The user asked to restart for the downloaded update.
    RestartForUpdate,
    /// The updater moved on: what it found, loads and kept.
    Updates(Box<crate::app::model::Updates>),
}
