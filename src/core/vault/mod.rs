//! The token vault: a game gets a worthless stand-in instead of its login, and a relay on this machine puts the real
//! login into the few requests that go to Mojang's own services. Anything else the game or a mod asks of it is refused.
//! A game whose account has a proxy also gets a local SOCKS relay that serves the vault itself and sends everything
//! else through the account's proxy, so the game never connects directly.

mod http;
mod keys;
mod login;
mod relay;
mod routes;
mod server;

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use uuid::Uuid;

pub use self::keys::publish;
pub use self::login::{FIRST_VERSION, Login, may_hide};

use self::routes::{Upstream, Vaulted};
use self::server::{Bound, Server};
use super::proxy;

/// Gets a fresh session for the account and publishes it with [`publish`]; called when the login ran out.
pub type Refresh = Box<dyn Fn() -> Result<()> + Send + Sync>;

/// A random string that stands in for a secret. Debug output hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct StandIn(String);

impl StandIn {
    fn new() -> Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness for the login stand-in: {e}"))?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StandIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<stand-in>")
    }
}

/// The vault of one running game; dropping it closes the vault and its relay.
pub struct Vault {
    stand_in: StandIn,
    login: Login,
    http: Server,
    relay: Option<Server>,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault").field("login", &self.login).field("address", &self.http.address()).field("relay", &self.relay.as_ref().map(Server::address)).finish()
    }
}

impl Vault {
    /// Opens the vault for one game of `account` on a random local port. The real login comes from [`publish`]; without
    /// one (offline accounts) every request that needs it is refused. Requests to Mojang leave through `proxy` when the
    /// account has one, and then a SOCKS relay for the game is opened too; the game must not start when this fails.
    pub fn start(login: Login, account: Uuid, proxy: Option<&str>, refresh: Option<Refresh>) -> Result<Self> {
        let parsed = proxy.map(proxy::parse).transpose().context("the account's proxy is invalid")?;
        let upstream = routes::Mojang::new(proxy)?;
        let dial: Option<relay::Dial> = parsed.map(|p| Arc::new(move |to: &proxy::Destination| proxy::tunnel_to(&p, to)) as relay::Dial);
        Self::open(login, account, Arc::new(upstream), dial, refresh)
    }

    fn open(login: Login, account: Uuid, upstream: Arc<dyn Upstream>, dial: Option<relay::Dial>, refresh: Option<Refresh>) -> Result<Self> {
        let stand_in = StandIn::new()?;
        let bound = Bound::new()?;
        let vaulted = Vaulted::new(stand_in.clone(), base(bound.address()), keys::hold(account), upstream, refresh);
        let http = bound.serve("vault", routes::handler(vaulted))?;
        let relay = dial.map(|dial| relay::start(http.address(), dial)).transpose()?;
        Ok(Self { stand_in, login, http, relay })
    }

    /// What the game gets instead of its login.
    pub fn stand_in(&self) -> &StandIn {
        &self.stand_in
    }

    /// The JVM options that point the game's login library at the vault.
    pub fn jvm_args(&self) -> Vec<String> {
        self.login.jvm_args(&base(self.http.address()))
    }

    /// The SOCKS relay the game must use as its only way out; only for accounts with a proxy.
    pub fn relay(&self) -> Option<SocketAddr> {
        self.relay.as_ref().map(Server::address)
    }
}

fn base(address: SocketAddr) -> String {
    format!("http://{address}")
}

#[cfg(test)]
mod tests;
