//! A small accept loop on a random local port that answers every connection on its own thread.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

/// Answers one connection; runs on a thread of its own.
pub(super) type Handler = Arc<dyn Fn(TcpStream) + Send + Sync>;

const MAX_OPEN: usize = 64;
const WAKE_TIMEOUT: Duration = Duration::from_secs(1);
const AFTER_ERROR: Duration = Duration::from_millis(50);

/// A listening socket on 127.0.0.1 that does not answer yet.
pub(super) struct Bound {
    listener: TcpListener,
    address: SocketAddr,
}

impl Bound {
    pub(super) fn new() -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("cannot open a local port for the login relay")?;
        let address = listener.local_addr().context("cannot open a local port for the login relay")?;
        Ok(Self { listener, address })
    }

    pub(super) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Starts answering; at most a few connections are open at a time, more are dropped.
    pub(super) fn serve(self, name: &'static str, handler: Handler) -> Result<Server> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let listener = self.listener;
        let thread = std::thread::Builder::new().name(format!("{name}-accept")).spawn(move || accept(&listener, &stopped, name, &handler)).context("cannot start the login relay")?;
        Ok(Server { address: self.address, stop, thread: Some(thread) })
    }
}

/// A running accept loop; dropping it stops answering and frees the port.
pub(super) struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub(super) fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.address, WAKE_TIMEOUT);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Open(Arc<AtomicUsize>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept(listener: &TcpListener, stop: &AtomicBool, name: &'static str, handler: &Handler) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else {
            std::thread::sleep(AFTER_ERROR);
            continue;
        };
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_OPEN {
            open.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let counted = Open(Arc::clone(&open));
        let handler = Arc::clone(handler);
        let _ = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
            let _counted = counted;
            handler(stream);
        });
    }
}
