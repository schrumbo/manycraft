//! The SOCKS5 relay of a game whose account has a proxy: connections to the vault stay on this machine, everything else
//! goes through the account's proxy. Nothing ever connects directly.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use super::StandIn;
use super::server::{Bound, Server};
use crate::core::proxy::Destination;

/// The user name the game signs in to the relay with; the password is the relay's own stand-in.
pub(super) const USER: &str = "manycraft";

/// Opens a connection through the account's proxy; none when the proxy refuses or cannot reach the destination.
pub(super) type Dial = Arc<dyn Fn(&Destination) -> Option<TcpStream> + Send + Sync>;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SUCCEEDED: u8 = 0;
const FAILED: u8 = 1;
const REFUSED: u8 = 5;
const UNSUPPORTED: u8 = 7;

/// Starts the relay in front of the vault at `vault`; only a client that signs in with `password` gets through.
pub(super) fn start(vault: SocketAddr, dial: Dial, password: StandIn) -> Result<Server> {
    Bound::new()?.serve("vault-relay", Arc::new(move |stream| relay(stream, vault, &dial, &password)))
}

fn relay(mut client: TcpStream, vault: SocketAddr, dial: &Dial, password: &StandIn) {
    let _ = client.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let Ok(Some(to)) = handshake(&mut client, password) else { return };
    let upstream = if is_vault(&to, vault) { TcpStream::connect_timeout(&vault, HANDSHAKE_TIMEOUT).ok() } else { dial(&to) };
    let Some(upstream) = upstream else {
        let _ = answer(&mut client, REFUSED);
        return;
    };
    if answer(&mut client, SUCCEEDED).is_err() {
        return;
    }
    let _ = client.set_read_timeout(None);
    let _ = upstream.set_read_timeout(None);
    let _ = upstream.set_write_timeout(None);
    pump(client, upstream);
}

/// Reads the greeting, the sign-in and the CONNECT request; `None` after answering a request the relay does not serve.
fn handshake(client: &mut TcpStream, password: &StandIn) -> io::Result<Option<Destination>> {
    let mut head = [0u8; 2];
    client.read_exact(&mut head)?;
    let mut methods = vec![0u8; usize::from(head[1])];
    client.read_exact(&mut methods)?;
    if head[0] != 5 || !methods.contains(&2) {
        client.write_all(&[5, 0xff])?;
        return Ok(None);
    }
    client.write_all(&[5, 2])?;
    let [version, user_len] = read_array::<2>(client)?;
    let mut user = vec![0u8; usize::from(user_len)];
    client.read_exact(&mut user)?;
    let [password_len] = read_array::<1>(client)?;
    let mut presented = vec![0u8; usize::from(password_len)];
    client.read_exact(&mut presented)?;
    let signed_in = version == 1 && user == USER.as_bytes() && password.matches(&presented);
    client.write_all(&[1, if signed_in { 0 } else { 1 }])?;
    if !signed_in {
        return Ok(None);
    }
    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    let host = match request[3] {
        1 => Host::Ip(IpAddr::from(read_array::<4>(client)?)),
        4 => Host::Ip(IpAddr::from(read_array::<16>(client)?)),
        3 => {
            let [len] = read_array::<1>(client)?;
            let mut name = vec![0u8; usize::from(len)];
            client.read_exact(&mut name)?;
            Host::Name(String::from_utf8_lossy(&name).into_owned())
        }
        _ => {
            answer(client, UNSUPPORTED)?;
            return Ok(None);
        }
    };
    let port = u16::from_be_bytes(read_array::<2>(client)?);
    if request[0] != 5 || request[1] != 1 {
        answer(client, if request[0] == 5 { UNSUPPORTED } else { FAILED })?;
        return Ok(None);
    }
    Ok(Some(match host {
        Host::Ip(ip) => Destination::Address(SocketAddr::new(ip, port)),
        Host::Name(name) => Destination::Name(name, port),
    }))
}

enum Host {
    Ip(IpAddr),
    Name(String),
}

fn read_array<const N: usize>(client: &mut TcpStream) -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    client.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn is_vault(to: &Destination, vault: SocketAddr) -> bool {
    match to {
        Destination::Address(address) => address.port() == vault.port() && address.ip().is_loopback(),
        Destination::Name(host, port) => *port == vault.port() && (host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())),
    }
}

fn answer(client: &mut TcpStream, code: u8) -> io::Result<()> {
    let mut reply = vec![5, code, 0, 1];
    reply.extend_from_slice(&Ipv4Addr::UNSPECIFIED.octets());
    reply.extend_from_slice(&[0, 0]);
    client.write_all(&reply)
}

fn pump(client: TcpStream, upstream: TcpStream) {
    let (Ok(client_in), Ok(upstream_out)) = (client.try_clone(), upstream.try_clone()) else { return };
    let Ok(ahead) = std::thread::Builder::new().name("vault-relay-up".into()).spawn(move || copy(client_in, upstream_out)) else { return };
    copy(upstream, client);
    let _ = ahead.join();
}

fn copy(mut from: TcpStream, mut to: TcpStream) {
    let _ = io::copy(&mut from, &mut to);
    let _ = to.shutdown(Shutdown::Write);
    let _ = from.shutdown(Shutdown::Read);
}