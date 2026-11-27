//! What the core remembers about the running games between events: timer generations, processes, restarts, collected notifications.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use super::model::Phase;
use crate::core::phone;

/// The kind of attention an account needs, for the wording of a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Alert {
    Disconnected,
    Banned,
    Crashed,
    Login,
    Other,
}

impl Alert {
    /// The kind of attention `phase` asks for.
    pub fn of(phase: &Phase) -> Self {
        match phase {
            Phase::Disconnected { .. } => Self::Disconnected,
            Phase::Banned { .. } => Self::Banned,
            Phase::Crashed { .. } => Self::Crashed,
            Phase::LoginNeeded => Self::Login,
            _ => Self::Other,
        }
    }
}

/// A screenshot timer: the one armed last, every `minutes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timer {
    pub generation: u64,
    pub minutes: u32,
}

/// Join commands that came due while their game held a resource pack: the rest of them, sent once it is back in the game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldJoin {
    pub lines: Vec<String>,
    pub delay: Duration,
}

/// What a game that was asked to show covers once it appears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub key: String,
    pub hide: Vec<String>,
    /// Whether the manager window goes away too.
    pub manager: bool,
}

/// The frame a tile has: its number and whether the running game sent it, or it is the still of an earlier run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub seq: u32,
    pub live: bool,
}

/// Where the user looks, as the views report it: the tile under the pointer, the selected or focused ones, how wide tiles are and whether the manager is on screen at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    pub hovered: Option<String>,
    pub focused: Vec<String>,
    /// Width of a tile in device pixels.
    pub width: u32,
    /// The manager window is minimized, in the notification area or behind a played game.
    pub away: bool,
}

/// How often a game captures its preview and how wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    pub interval_ms: u32,
    pub width: u32,
}

/// One chat line of the user for one account; `ticket` tells the hold it belongs to apart from an older one.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: u64,
    pub text: String,
    pub ticket: u64,
    /// How often the game refused the line while it was on a server though it was on a server.
    pub tries: u8,
}

/// Chat lines that wait for their account to get on a server, and those handed to an account that did not write them yet.
#[derive(Default)]
pub struct Outbox {
    pub held: HashMap<String, VecDeque<Line>>,
    pub handed: HashMap<String, Vec<Line>>,
    pub tickets: u64,
}

impl Outbox {
    /// Whether any line waits for or sits at the account `key`.
    pub fn has(&self, key: &str) -> bool {
        self.held.contains_key(key) || self.handed.contains_key(key)
    }
}

/// The state of the running session that no view shows directly.
#[derive(Default)]
pub struct Session {
    /// The connection-only account whose screen is in view.
    pub screen_open: Option<String>,
    /// Bumped by every map a connection-only account shows, across all of them, so a picture's number never repeats.
    pub map_seq: u64,
    /// The games played alone while the manager window is put away for them; it comes back when none runs.
    pub slimmed: std::collections::HashSet<String>,
    /// The screenshot timer of each game without a window whose batch takes them on an interval, by key.
    pub captures: HashMap<String, Timer>,
    /// Bumped by every screenshot timer armed, so a timer of an older one does nothing.
    pub capture_seq: u64,
    /// Where each game played alone goes on its next start, by key.
    pub quick: HashMap<String, crate::core::fleet::Quick>,
    /// Tiles on screen, whose previews update at full rate.
    pub visible: HashSet<String>,
    /// Games whose newest preview was not read because their tile is off screen, since when.
    pub stale: HashMap<String, Instant>,
    /// When the preview of each game was last read.
    pub read_at: HashMap<String, Instant>,
    /// Bumped by every reconnect, restart, start and stop, so a timer of an older one does nothing.
    pub reconnects: HashMap<String, u64>,
    /// Bumped whenever a game leaves the server, so join commands of an older join stop.
    pub joins: HashMap<String, u64>,
    pub crash_restarts: HashMap<String, u32>,
    /// Rounds of starts that failed because the sign-in services were down, by key; a launch, a stop or the final failure ends the count.
    pub sign_in_rounds: HashMap<String, u32>,
    /// When each game first joined in this run.
    pub started: HashMap<String, Instant>,
    /// Games that joined a server and have not left it since.
    pub joined: HashSet<String>,
    /// Join commands waiting for their game's resource pack offer to be answered.
    pub held_joins: HashMap<String, HeldJoin>,
    /// The join generation whose commands are still being sent, per game: chat lines that waited go out after them.
    pub join_pending: HashMap<String, u64>,
    /// Chat lines of the user on their way to each account.
    pub outbox: Outbox,
    /// Servers whose status ping is on its way or waits for its next turn.
    pub probes: HashSet<String>,
    /// Games a server's answer woke from waiting that have not reached it yet; their next drop spends a try.
    pub woken: HashSet<String>,
    /// Whether the saved sign-ins were checked once.
    pub refreshed: bool,
    /// Attention collected per batch for one notification.
    pub alerts: HashMap<String, Vec<(String, Alert)>>,
    /// Process ids of the games whose mod connected.
    pub pids: HashMap<String, u32>,
    /// Games on their way down for a restart, by restart group.
    pub restarts: HashMap<String, u64>,
    /// Games of a restart group that are down, by group.
    pub restart_ready: HashMap<u64, Vec<String>>,
    pub restart_groups: u64,
    /// Games whose window still has to be put in the manager's place.
    pub unplaced: HashSet<String>,
    /// A message for the next reload.
    pub notice: Option<String>,
    pub reveal: Option<Reveal>,
    pub phone: phone::Coalescer,
    /// When the next phone message is due, while a timer waits for it.
    pub phone_at: Option<Instant>,
    /// Games an earlier manager launched whose mod has not reported its state since.
    pub adopted: HashSet<String>,
    /// Games an earlier manager launched; where the OS does not tell their exit code, an exit counts as clean.
    pub foreign: HashSet<String>,
    /// The games of each batch's bulk start, while some of them still wait to launch.
    pub runs: HashMap<String, Vec<String>>,
    /// Games stopped to free their account, by key, with the game that starts once they are down and until when it waits.
    pub take_overs: HashMap<String, (String, Instant)>,
    /// The last place given out in the line of games that wait for free memory.
    pub memory_seq: u64,
    /// Whether a reading of the free memory is on its way.
    pub memory_asked: bool,
    /// Whether a timer to read the free memory again waits.
    pub memory_timer: bool,
    /// Games whose link to Manycraft dropped and is not back yet, before their tile says so.
    pub lost: HashSet<String>,
    /// Plays asked for before the data was read, by instance and account.
    pub waiting_plays: Vec<(String, Option<String>)>,
    /// Batch starts asked for before a load knew the batch, by batch and when they were asked.
    pub waiting_starts: Vec<(String, Instant)>,
    pub watch: Watch,
    /// The memory each running game held at the last reading, in MB, by key.
    pub usage: HashMap<String, u64>,
    /// Whether a timer to read the memory of the running games waits.
    pub usage_timer: bool,
    /// Whether a game started or ended since the play time was last settled.
    pub playtime_dirty: bool,
    /// Whether a timer to write down the play time waits.
    pub playtime_timer: bool,
    /// The preview rate each connected game should get, as last worked out.
    pub planned: HashMap<String, Shot>,
    /// The preview rate each game was last told.
    pub shots: HashMap<String, Shot>,
    /// Bumped by every change of `planned`, so only the last one within the wait is sent.
    pub shots_seq: u64,
}