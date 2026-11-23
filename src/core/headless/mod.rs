//! Connection-only accounts: a small protocol client keeps an account on a server without starting the game.
//! It reports to the same channel as a game's companion, so the rest of the app treats both alike.

use std::sync::mpsc::Sender;

use anyhow::Result;
use uuid::Uuid;

use super::auth::Session;
use super::batch::Packs;
use super::ipc;

/// Everything one connection needs.
#[derive(Debug)]
pub struct Spec {
    /// The key its events carry, the same key a game of this account would have.
    pub key: String,
    /// The server, as the batch keeps it (`host` or `host:port`).
    pub address: String,
    pub name: String,
    pub uuid: Uuid,
    /// The signed-in session for online-mode servers; `None` for offline accounts.
    pub session: Option<Session>,
    /// The Minecraft version the batch plays, which picks the protocol.
    pub version: String,
    pub packs: Packs,
}

/// A running connection. It ends on [`ipc::ToMod::Quit`], when the server drops it for good, or with the app.
pub struct Connection {
    _private: (),
}

impl Connection {
    /// Hands the connection a message as a game's companion would get it; it acts on the ones that make sense
    /// without a game (chat, reconnect, respawn, quit, pack policy and answers) and ignores the rest.
    pub fn send(&self, _message: &ipc::ToMod) -> Result<()> {
        anyhow::bail!("connection-only accounts are not available yet")
    }

    /// Whether it still runs.
    pub fn is_running(&self) -> bool {
        false
    }
}

/// Whether the client speaks the protocol of Minecraft `version`.
pub fn supports(_version: &str) -> bool {
    false
}

/// Connects in the background. Reports to `events` as [`ipc::Event::Connected`], then [`ipc::Event::Message`]
/// with states and chat, and [`ipc::Event::Disconnected`] when it ends.
pub fn start(_spec: Spec, _events: Sender<ipc::Event>) -> Result<Connection> {
    anyhow::bail!("connection-only accounts are not available yet")
}
