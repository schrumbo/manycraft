//! What the core asks the front end to do: talk to games and the fleet, write files, show windows and notifications, wait.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::event::Event;
use super::session::Reveal;
use crate::core::account::Account;
use crate::core::fleet::Target;
use crate::core::history::Log;
use crate::core::ipc::ToMod;

/// How much of the views a change touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Redraw {
    Nothing,
    /// Only chat lines arrived, which nothing but the chat page shows.
    Chat,
    All,
}

/// What was in place before a game was asked to show, to put back when it cannot be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unshown {
    pub playing: Option<String>,
    pub reveal: Option<Reveal>,
    pub show_when_ready: Option<(String, Instant)>,
    /// The whispers the game had unread.
    pub whispers: u32,
}

/// Something only the front end can do.
#[derive(Debug)]
pub enum Effect {
    Redraw(Redraw),
    Send { key: String, message: ToMod },
    /// Starts `accounts` of `target` for `owner`, in line, with a way to cancel the run.
    Start { owner: String, target: Target, accounts: Vec<Account> },
    /// Cancels the starts of `owner` that still wait in line.
    CancelStarts(String),
    /// Checks whether `count` games of `memory_mb` fit into the free memory, and says so when they do not.
    CheckFit { count: u32, memory_mb: u32 },
    Stop(Vec<String>),
    ReadPreview { key: String, path: PathBuf },
    /// The game ended: its preview file is closed so the instance folder can be moved or deleted.
    ReleasePreview(String),
    CheckProxy { key: String, url: String },
    /// The connection may be gone: check it now.
    SuspectOffline,
    NetworkBack,
    /// Delivers `event` after `delay`.
    After { delay: Duration, event: Box<Event> },
    SaveHistory { path: PathBuf, log: Log },
    /// Shows the `image`-th image of the reports on the tile of `key`; `still` saves it as the still of the run, `Some(true)` as its last.
    ShowFrame { key: String, image: usize, still: Option<bool> },
    /// Lets go of the `image`-th image of the reports, which no tile shows.
    DropImage(usize),
    /// Saves the frame the tile of `key` shows as the last still of its run.
    SaveStill(String),
    /// Colours the window of process `pid` for the account of `key`.
    Tint { key: String, pid: u32 },
    /// Tells the fleet which adopted games stay and which it lets go.
    Settle { placed: Vec<String>, dropped: Vec<String> },
    /// Puts the window of a game that came up hidden in the manager's place.
    Place(String),
    /// Shows an OS notification, unless the manager is in front with no game played.
    Notify { tag: String, title: String, body: String },
    Phone(String),
    /// Follows the window of the game `key` to where the player put it, as the manager's place.
    Follow(Option<String>),
    /// Shows game `key` in the manager's place and hides `skipped`, which was about to show; the place follows `before`, the game shown so far, or the manager window.
    Show { key: String, before: Option<String>, skipped: Option<String>, unshown: Box<Unshown> },
    HideManager,
    ShowManager,
    AddAccount,
    ShowInstance(String),
    RememberAccount { instance: String, account: Uuid },
    SaveColor { ids: Vec<Uuid>, color: u8 },
    /// Drops images and caches of games and accounts that are gone.
    Retain,
    /// Loads the saved stills of these games and the faces of accounts without one.
    LoadStills(Vec<String>),
    /// Tells the fleet the names of the games, by key.
    Known(Vec<(String, String)>),
    /// Checks whether the saved sign-ins of these accounts still work.
    Refresh(Vec<Account>),
    /// Puts the manager window away in the notification area; `explain` shows the one-time hint first.
    ToTray { explain: bool },
    /// Remembers that the hint about the notification area was seen.
    RememberTrayExplained,
    /// Asks whether the `running` games keep running when Manycraft quits.
    AskQuit { running: usize },
    /// Quits Manycraft; the games keep running when `keep`, for the next start to find again.
    Quit { keep: bool },
    /// A line for the test drive log.
    Trace(String),
    /// A line for the error log.
    Log(String),
}
