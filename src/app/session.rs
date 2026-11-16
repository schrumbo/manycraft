//! What the core remembers about the running games between events: timer generations, processes, restarts, collected notifications.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

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

/// A whisper that reached a game nobody is playing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whisper {
    pub account: String,
    pub from: Option<String>,
    pub text: String,
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

/// The state of the running session that no view shows directly.
#[derive(Default)]
pub struct Session {
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
    /// When each game first joined in this run.
    pub started: HashMap<String, Instant>,
    /// Games that joined a server and have not left it since.
    pub joined: HashSet<String>,
    /// Whether the saved sign-ins were checked once.
    pub refreshed: bool,
    /// The game to show once it can be shown, asked for at that time.
    pub show_when_ready: Option<(String, Instant)>,
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
    pub whispers: Vec<Whisper>,
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
    /// Plays asked for before the data was read, by instance and account.
    pub waiting_plays: Vec<(String, Option<String>)>,
    /// Batch starts asked for before a load knew the batch, by batch and when they were asked.
    pub waiting_starts: Vec<(String, Instant)>,
}
