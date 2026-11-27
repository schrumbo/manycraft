//! Connection-only accounts: a small protocol client keeps an account on a server without starting the game.
//! It reports to the same channel as a game's companion, so the rest of the app treats both alike.
//!
//! The client reports the `vanilla` brand like an unmodded game, never moves, and sends only what the protocol asks of it (keep-alive,
//! pong, teleport and chunk batch confirmations, the loaded world, client settings with view distance 2, configuration
//! answers) plus what the user wrote. It speaks Minecraft 26.3 (protocol 777). Packet ids and the English texts it
//! shows come from the game's own data generator: `.claude/bin/headless-packets` regenerates `v777.rs` from a server jar.
//! Chat is signed when the account has a certified key and unsigned otherwise; proxies and server transfers are not supported yet. What the server shows the player (the map in
//! hand, an open menu, titles) is reported for the manager's screen view; menu clicks go out only when the user clicks.

mod classify;
mod cli;
mod client;
mod commands;
mod conn;
mod crowd;
mod crypto;
mod item;
mod maps;
mod picture;
pub mod render;
pub mod resume;
mod screen;
mod signing;
mod status;
mod text;
mod v777;
mod wire;
mod world;

use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::Result;
use uuid::Uuid;

use super::account::Account;
use super::auth::Session;
use super::batch::Packs;
use super::ipc;
use super::proxy::Proxy;

pub use cli::cli;
pub use item::icon as item_icon;
pub use maps::{SIDE as MAP_SIDE, rgba as map_rgba};

const STACK: usize = 512 << 10;

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
    /// The Microsoft account behind `session`, which a long connection asks for a fresh session when its chat key runs out.
    pub account: Option<Account>,
    /// The Minecraft version the batch plays, which picks the protocol.
    pub version: String,
    pub packs: Packs,
    /// The account's proxy. Connection-only accounts cannot use one yet, so [`start`] refuses an account that has one.
    pub proxy: Option<Proxy>,
    /// Hashes of the Codes of Conduct this batch already accepted on this server (see [`conduct`]); the same text is
    /// accepted again without asking.
    pub conduct: Vec<String>,
}

/// Why [`start`] refused an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The account has a proxy, which this mode cannot use yet; it must not connect without it.
    Proxy,
    /// The batch plays a version this client does not speak.
    Version,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refused::Proxy => "accounts with a proxy cannot connect without the game yet",
            Refused::Version => "this Minecraft version cannot connect without the game yet",
        })
    }
}

impl std::error::Error for Refused {}

/// A running connection. It ends on [`ipc::ToMod::Quit`], when the server drops it for good, or with the app.
pub struct Connection {
    shared: Arc<client::Shared>,
}

impl Connection {
    /// Hands the connection a message as a game's companion would get it; it acts on the ones that make sense
    /// without a game (chat, reconnect, respawn, quit, pack policy and answers) and ignores the rest.
    pub fn send(&self, message: &ipc::ToMod) -> Result<()> {
        self.shared.send(message)
    }

    /// Whether it still runs.
    pub fn is_running(&self) -> bool {
        self.shared.is_running()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.shared.send(&ipc::ToMod::Quit);
    }
}

/// Whether the client speaks the protocol of Minecraft `version`.
pub fn supports(version: &str) -> bool {
    version == "26.3"
}

/// The hash of a Code of Conduct when `offer` is one, held like a resource pack offer until the user answers it.
/// Accepting it once is meant to hold for its batch on that server: keep the hash and pass it in [`Spec::conduct`].
pub fn conduct(offer: &ipc::Offer) -> Option<&str> {
    offer.id.strip_prefix(client::CONDUCT)
}

/// Connects in the background. Reports to `events` as [`ipc::Event::Connected`], then [`ipc::Event::Message`]
/// with states and chat, and [`ipc::Event::Disconnected`] when it ends. A dropped connection reports
/// [`ipc::StateKind::Disconnected`] with the reason and waits for [`ipc::ToMod::Reconnect`], as a game would.
/// The `pid` in its `Connected` event is 0: there is no process of its own. Refuses with [`Refused`].
pub fn start(spec: Spec, events: Sender<ipc::Event>) -> Result<Connection> {
    if spec.proxy.is_some() {
        return Err(Refused::Proxy.into());
    }
    if !supports(&spec.version) {
        return Err(Refused::Version.into());
    }
    let shared = Arc::new(client::Shared::new(&spec, events));
    let running = Arc::clone(&shared);
    std::thread::Builder::new().name(format!("headless {}", spec.name)).stack_size(STACK).spawn(move || client::run(&running, &spec))?;
    Ok(Connection { shared })
}