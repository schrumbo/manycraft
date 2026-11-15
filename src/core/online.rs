//! Whether this computer reaches the internet: learned from requests and a probe, and watched while it is gone.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

const UNKNOWN: u8 = 0;
const ONLINE: u8 = 1;
const OFFLINE: u8 = 2;
const PROBE_HOSTS: [&str; 2] = ["piston-meta.mojang.com:443", "api.modrinth.com:443"];
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const WAITS_SECS: [u64; 6] = [2, 4, 8, 15, 30, 60];

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);
static WATCHING: AtomicBool = AtomicBool::new(false);
static LISTENERS: Mutex<Vec<Box<dyn Fn(bool) + Send + Sync>>> = Mutex::new(Vec::new());

/// Whether the last probe found no way out. Unknown and online both count as not offline.
pub fn offline() -> bool {
    STATE.load(Ordering::SeqCst) == OFFLINE
}

/// Calls `listener` with `false` when the connection is found gone and with `true` when it is back.
pub fn subscribe(listener: impl Fn(bool) + Send + Sync + 'static) {
    LISTENERS.lock().unwrap_or_else(PoisonError::into_inner).push(Box::new(listener));
}

/// Probes right away, on the calling thread, at most a few seconds; starts watching when nothing answers.
pub fn reachable() -> bool {
    let up = probe(&targets(), PROBE_TIMEOUT);
    if up {
        set(ONLINE);
    } else {
        set(OFFLINE);
        suspect();
    }
    up
}

/// A request got an answer, so the connection works.
pub fn reached() {
    set(ONLINE);
}

/// A request could not connect. Probes in the background and, if nothing is reachable, keeps probing with growing waits until the connection is back.
pub fn suspect() {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("online-watch".into()).spawn(|| {
        watch(&|| probe(&targets(), PROBE_TIMEOUT), &|attempt| {
            std::thread::sleep(super::reconnect::wait(&WAITS_SECS, attempt, 20, super::reconnect::unit()));
        }, &set);
        WATCHING.store(false, Ordering::SeqCst);
    });
    if spawned.is_err() {
        WATCHING.store(false, Ordering::SeqCst);
    }
}

fn watch(probe: &dyn Fn() -> bool, wait: &dyn Fn(u32), set: &dyn Fn(u8)) {
    let mut attempt = 0;
    loop {
        if probe() {
            set(ONLINE);
            return;
        }
        set(OFFLINE);
        wait(attempt);
        attempt += 1;
    }
}

fn set(state: u8) {
    let listeners = LISTENERS.lock().unwrap_or_else(PoisonError::into_inner);
    let before = STATE.swap(state, Ordering::SeqCst);
    let changed = match (before, state) {
        (OFFLINE, ONLINE) => Some(true),
        (UNKNOWN | ONLINE, OFFLINE) => Some(false),
        _ => None,
    };
    if let Some(up) = changed {
        for listener in listeners.iter() {
            listener(up);
        }
    }
}

fn targets() -> Vec<String> {
    match ureq::Proxy::try_from_env() {
        Some(proxy) => vec![format!("{}:{}", proxy.host(), proxy.port())],
        None => PROBE_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
    }
}

/// Whether a TCP connection to any of `hosts` (`host:port`) opens within `timeout` per address.
pub fn probe(hosts: &[String], timeout: Duration) -> bool {
    hosts.iter().any(|host| host.to_socket_addrs().is_ok_and(|mut addrs| addrs.any(|addr: SocketAddr| TcpStream::connect_timeout(&addr, timeout).is_ok())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::net::TcpListener;

    #[test]
    fn probe_sees_an_open_port_and_not_a_closed_one() {
        let open = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(probe(&[open.local_addr().unwrap().to_string()], Duration::from_secs(1)));
        assert!(!probe(&[closed.to_string()], Duration::from_secs(1)));
        assert!(!probe(&["no-such-host.invalid:443".to_owned()], Duration::from_secs(1)));
        assert!(probe(&[closed.to_string(), open.local_addr().unwrap().to_string()], Duration::from_secs(1)));
    }

    #[test]
    fn watch_waits_with_growing_attempts_until_a_probe_succeeds() {
        let left = Cell::new(3);
        let waits = std::cell::RefCell::new(Vec::new());
        let states = std::cell::RefCell::new(Vec::new());
        watch(&|| {
            left.set(left.get() - 1);
            left.get() < 0
        }, &|attempt| waits.borrow_mut().push(attempt), &|s| states.borrow_mut().push(s));
        assert_eq!(*waits.borrow(), [0, 1, 2]);
        assert_eq!(*states.borrow(), [OFFLINE, OFFLINE, OFFLINE, ONLINE]);
    }
}
