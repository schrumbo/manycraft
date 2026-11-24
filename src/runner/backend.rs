//! The fleet behind the shells: starts, stops and talks to games off the UI thread and reports back as [`Update`]s.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::app::model::Phase;
use crate::core::account::{Account, AccountKind};
use crate::core::auth::{self, AuthError};
use crate::core::fleet::{self, Fleet, Target};
use crate::core::ipc::{self, Event, FromMod, ToMod};
use crate::core::place::Rect;
use crate::core::proxy::{self, Fault};
use crate::core::{adopt, drive, headless, install, net, online};

const PRESENT_EVERY: Duration = Duration::from_millis(100);
const EXIT_POLL: Duration = Duration::from_millis(500);
const STAGGER_STEP: Duration = Duration::from_millis(100);
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// An image a shell can draw, made from an RGBA frame off the UI thread.
pub trait Pixels: Send + 'static {
    fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self>
    where
        Self: Sized;
}

enum PreviewJob {
    Read(String, PathBuf),
    Release(String),
}

/// What the fleet reports back to the UI thread.
pub enum Update<P> {
    Preparing { batch: String, progress: install::Progress },
    /// The game is installed; the accounts of `batch` wait in line to start.
    Prepared { batch: String },
    Launched { key: String, plain: bool },
    Failed { key: String, reason: String, login: bool, offline: bool },
    /// Whether the connection came back (`true`) or is gone.
    Reach { online: bool },
    Ipc(Event),
    Exited { key: String, code: Option<i32> },
    Preview { key: String, seq: u32, image: P },
    Seed { key: String, phase: Phase, attempts: u32 },
    /// Whether the saved sign-in of an account still works, from a token refresh.
    SignIn { account: uuid::Uuid, valid: bool },
    /// The saved still of an instance, read from the cache.
    Still { key: String, image: P },
    /// The skin face of an account.
    Face { account: uuid::Uuid, image: P },
    /// What the check of an account's proxy found after a connection failure; `None` when it works.
    ProxyChecked { key: String, fault: Option<Fault> },
    /// Whether `server` answered its status ping.
    ServerProbed { server: String, up: bool },
}

/// Runs and talks to instances off the UI thread: the real fleet, or a fake one when an agent drives the app.
pub trait Backend: Send + Sync {
    /// Starts `accounts` of `target` one after another until `cancel` is set.
    fn start(&self, target: Target, accounts: Vec<Account>, cancel: Arc<AtomicBool>, home: Option<Rect>);
    /// Asks the instances to quit and ends them after a grace period.
    fn stop(&self, keys: Vec<String>);
    /// Ends the instances at once, without waiting for them to save.
    fn kill(&self, keys: Vec<String>);
    /// Sends a message to an instance's companion mod.
    fn send(&self, key: &str, message: ToMod);
    /// Reads the newest preview frame from `path` and reports it.
    fn read_preview(&self, key: String, path: PathBuf);
    /// Closes the kept preview file of `key` after its game ended.
    fn release_preview(&self, key: String);
    /// Stops every instance and waits for them, at quit.
    fn quit(&self);
    /// Refuses new starts and lets the running games be, at a quit that keeps them for the next start.
    fn leave(&self) {}
    /// Learns the instances the model holds, by key and account name.
    fn known(&self, _instances: Vec<(String, String)>) {}
    /// Refreshes the sign-in of `accounts` one after another and reports each that works or needs a new sign-in.
    fn refresh(&self, _accounts: Vec<Account>) {}
    /// Checks the proxy `url` of instance `key` and reports the result.
    fn check_proxy(&self, _key: String, _url: String) {}
    /// Pings `server` once, through the proxy at `proxy` when given, and reports whether it answered.
    fn probe(&self, _server: String, _proxy: Option<String>) {}
    /// The games an earlier manager launched that still run; only the first call returns them.
    fn adopted(&self) -> Vec<fleet::Adoption> {
        Vec::new()
    }
    /// Writes down the connection-only accounts `keys`, which end with this manager, for the next start to connect again.
    fn resume(&self, _keys: &[String]) {}
    /// The connection-only accounts the last manager wrote down; only the first call returns them.
    fn resumed(&self) -> Vec<String> {
        Vec::new()
    }
    /// Lets the adopted games that got a tile talk to the manager again and stops tracking the rest, which keep running.
    fn settle(&self, _placed: &[String], _dropped: &[String]) {}
    /// Brings the window of the game `key`, played alone, to the front.
    fn raise(&self, _key: &str) {}
}

/// The real fleet of Minecraft processes.
pub struct Real<P> {
    fleet: Arc<Fleet>,
    out: async_channel::Sender<Update<P>>,
    watch: Arc<Mutex<HashSet<String>>>,
    owners: Arc<Mutex<HashMap<String, u64>>>,
    runs: AtomicU64,
    sends: mpsc::Sender<(String, ToMod)>,
    previews: mpsc::Sender<PreviewJob>,
    refreshing: Arc<Mutex<HashSet<uuid::Uuid>>>,
    checking: Arc<Mutex<HashSet<String>>>,
    adopted: Mutex<Vec<fleet::Adoption>>,
    resumed: Mutex<Vec<String>>,
}

impl<P: Pixels> Real<P> {
    /// Starts the fleet threads; what they find arrives on `out`.
    pub fn new(out: async_channel::Sender<Update<P>>) -> anyhow::Result<Self> {
        crate::core::fleet::scrub_crash_files();
        let (fleet, events, adopted) = if adopt::is_manager() {
            Fleet::manager()?
        } else {
            let (fleet, events) = Fleet::new()?;
            (fleet, events, Vec::new())
        };
        let fleet = Arc::new(fleet);
        let reach = out.clone();
        online::subscribe(move |up| {
            let _ = reach.send_blocking(Update::Reach { online: up });
        });
        let forward = out.clone();
        let loading = Arc::clone(&fleet);
        std::thread::Builder::new().name("fleet-events".into()).spawn(move || {
            while let Ok(event) = events.recv() {
                if let Event::Message { instance, message: FromMod::State { .. } } = &event {
                    loading.loaded(instance);
                }
                if let Event::Message { instance, message: FromMod::Chat { text, .. } } = &event
                    && drive::on()
                {
                    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
                    drive::note(format!("chat {at} {instance} {text}"));
                }
                let update = match event {
                    Event::Picture { instance, seq, width, height, rgba } => match P::from_rgba(width, height, rgba) {
                        Some(image) => Update::Preview { key: instance, seq, image },
                        None => continue,
                    },
                    event => Update::Ipc(event),
                };
                if forward.send_blocking(update).is_err() {
                    return;
                }
            }
        })?;
        let (previews, wanted) = mpsc::channel::<PreviewJob>();
        let frames = out.clone();
        std::thread::Builder::new().name("previews".into()).spawn(move || {
            let mut reader = ipc::PreviewReader::default();
            let mut changes = ipc::PreviewChanges::default();
            let mut presented = Instant::now();
            while let Ok(first) = wanted.recv() {
                std::thread::sleep(PRESENT_EVERY.saturating_sub(presented.elapsed()));
                presented = Instant::now();
                let mut batch: HashMap<String, PathBuf> = HashMap::new();
                for job in std::iter::once(first).chain(wanted.try_iter()) {
                    match job {
                        PreviewJob::Read(key, path) => {
                            batch.insert(key, path);
                        }
                        PreviewJob::Release(key) => {
                            batch.remove(&key);
                            reader.release(&key);
                        }
                    }
                }
                for (key, path) in batch {
                    let Some(preview) = reader.read(&key, &path) else { continue };
                    if !changes.changed(&key, &preview) {
                        continue;
                    }
                    let Some(image) = P::from_rgba(preview.width, preview.height, preview.rgba) else { continue };
                    if frames.send_blocking(Update::Preview { key, seq: preview.seq, image }).is_err() {
                        return;
                    }
                }
            }
        })?;
        let watch: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(adopted.iter().map(|a| a.key.clone()).collect()));
        let (watching, exits, exiting) = (Arc::clone(&watch), out.clone(), Arc::clone(&fleet));
        std::thread::Builder::new().name("fleet-exits".into()).spawn(move || {
            loop {
                std::thread::sleep(EXIT_POLL);
                let keys: Vec<String> = lock(&watching).iter().cloned().collect();
                for key in keys {
                    if let Some(code) = exiting.exit_code(&key) {
                        lock(&watching).remove(&key);
                        if exits.send_blocking(Update::Exited { key, code }).is_err() {
                            return;
                        }
                    }
                }
            }
        })?;
        let (sends, outgoing) = mpsc::channel::<(String, ToMod)>();
        let sender = Arc::clone(&fleet);
        let failed = out.clone();
        std::thread::Builder::new().name("fleet-send".into()).spawn(move || {
            while let Ok((key, message)) = outgoing.recv() {
                if sender.send(&key, &message).is_err()
                    && let ToMod::ChatLine { id, .. } = message
                {
                    let outcome = crate::core::ipc::ChatOutcome::Refused(crate::core::ipc::ChatRefusal::Failed);
                    let _ = failed.send_blocking(Update::Ipc(crate::core::ipc::Event::Message { instance: key, message: crate::core::ipc::FromMod::ChatOutcome { id, outcome } }));
                }
            }
        })?;
        Ok(Self { fleet, out, watch, owners: Arc::default(), runs: AtomicU64::new(0), sends, previews, refreshing: Arc::default(), checking: Arc::default(), adopted: Mutex::new(adopted), resumed: Mutex::new(if adopt::is_manager() { headless::resume::take() } else { Vec::new() }) })
    }
}

impl<P: Pixels> Backend for Real<P> {
    fn start(&self, target: Target, accounts: Vec<Account>, cancel: Arc<AtomicBool>, home: Option<Rect>) {
        let (fleet, out, watch, owners) = (Arc::clone(&self.fleet), self.out.clone(), Arc::clone(&self.watch), Arc::clone(&self.owners));
        let owner = target.owner();
        let parallel = target.batch.as_ref().map_or(0, |b| b.launch_parallel);
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut owners = lock(&owners);
            for account in &accounts {
                owners.insert(target.key(account.id), run);
            }
        }
        let _ = std::thread::Builder::new().name("fleet-start".into()).spawn(move || {
            let mine = |key: &str| lock(&owners).get(key) == Some(&run);
            let release = |key: &str| {
                if !lock(&owners).get(key).is_some_and(|owner| *owner != run) {
                    fleet.give_up(key);
                }
            };
            let gone = std::cell::OnceCell::new();
            let cut_off = |e: &anyhow::Error, account: &Account| account.proxy.is_none() && net::no_connection(e) && *gone.get_or_init(|| !online::reachable());
            let fail_all = |e: anyhow::Error| {
                let reason = format!("{e:#}");
                for account in &accounts {
                    let key = target.key(account.id);
                    if mine(&key) {
                        let _ = out.send_blocking(Update::Failed { key, reason: reason.clone(), login: false, offline: cut_off(&e, account) });
                    }
                }
            };
            let last = Mutex::new(None);
            let progress = |p: install::Progress| {
                let mark = (p.stage, p.percent());
                let mut last = lock(&last);
                if *last != Some(mark) {
                    *last = Some(mark);
                    let _ = out.send_blocking(Update::Preparing { batch: owner.clone(), progress: p });
                }
            };
            let prepared = match fleet.prepare_launch(&target, &accounts, None, &progress) {
                Ok(prepared) => prepared,
                Err(e) => return fail_all(e),
            };
            let _ = out.send_blocking(Update::Prepared { batch: owner.clone() });
            let keys: Vec<String> = accounts.iter().map(|a| target.key(a.id)).collect();
            let queued: Vec<String> = keys.iter().filter(|k| mine(k)).cloned().collect();
            fleet.queue(&queued);
            for (account, key) in accounts.iter().zip(&keys) {
                let stand_down = || cancel.load(Ordering::SeqCst) || !mine(key);
                if !fleet.wait_turn(key, parallel, prepared.plain(), &stand_down) {
                    if cancel.load(Ordering::SeqCst) {
                        keys.iter().for_each(|k| release(k));
                        return;
                    }
                    release(key);
                    continue;
                }
                let update = match fleet.start_one(&prepared, account) {
                    Ok(key) => {
                        lock(&watch).insert(key.clone());
                        if !lock(&owners).contains_key(&key) {
                            fleet.kill(std::slice::from_ref(&key));
                        } else if let Some(home) = home.filter(|_| prepared.plain()) {
                            fleet.bring_home(&key, home);
                        }
                        Some(Update::Launched { key, plain: prepared.plain() })
                    }
                    Err(_) if fleet.is_running(key) => {
                        release(key);
                        Some(Update::Launched { key: key.clone(), plain: prepared.plain() })
                    }
                    Err(_) if !mine(key) => {
                        release(key);
                        None
                    }
                    Err(e) => {
                        release(key);
                        Some(Update::Failed { key: key.clone(), login: needs_sign_in(&e), offline: cut_off(&e, account), reason: format!("{e:#}") })
                    }
                };
                if let Some(update) = update {
                    let _ = out.send_blocking(update);
                }
            }
        });
    }

    fn stop(&self, keys: Vec<String>) {
        {
            let mut owners = lock(&self.owners);
            for key in &keys {
                owners.remove(key);
            }
        }
        let fleet = Arc::clone(&self.fleet);
        let _ = std::thread::Builder::new().name("fleet-stop".into()).spawn(move || fleet.stop_all(&keys));
    }

    fn kill(&self, keys: Vec<String>) {
        let fleet = Arc::clone(&self.fleet);
        let _ = std::thread::Builder::new().name("fleet-kill".into()).spawn(move || fleet.kill(&keys));
    }

    fn send(&self, key: &str, message: ToMod) {
        let _ = self.sends.send((key.to_owned(), message));
    }

    fn read_preview(&self, key: String, path: PathBuf) {
        let _ = self.previews.send(PreviewJob::Read(key, path));
    }

    fn release_preview(&self, key: String) {
        let _ = self.previews.send(PreviewJob::Release(key));
    }

    fn refresh(&self, accounts: Vec<Account>) {
        let todo: Vec<Account> = {
            let mut busy = lock(&self.refreshing);
            accounts.into_iter().filter(|a| a.kind == AccountKind::Microsoft && busy.insert(a.id)).collect()
        };
        if todo.is_empty() {
            return;
        }
        let (out, busy) = (self.out.clone(), Arc::clone(&self.refreshing));
        let _ = std::thread::Builder::new().name("token-refresh".into()).spawn(move || {
            for account in todo {
                let outcome = auth::refresh_account(&account);
                lock(&busy).remove(&account.id);
                let valid = match &outcome {
                    Ok(()) => true,
                    Err(e) if needs_sign_in(e) => false,
                    Err(_) => continue,
                };
                if out.send_blocking(Update::SignIn { account: account.id, valid }).is_err() {
                    return;
                }
            }
        });
    }

    fn adopted(&self) -> Vec<fleet::Adoption> {
        let (live, ended): (Vec<_>, Vec<_>) = std::mem::take(&mut *lock(&self.adopted)).into_iter().partition(|a| self.fleet.is_running(&a.key));
        self.settle(&[], &ended.into_iter().map(|a| a.key).collect::<Vec<_>>());
        live
    }

    fn resume(&self, keys: &[String]) {
        headless::resume::remember(keys);
    }

    fn resumed(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.resumed))
    }

    fn raise(&self, key: &str) {
        self.fleet.raise(key);
    }

    fn settle(&self, placed: &[String], dropped: &[String]) {
        self.fleet.admit(placed);
        self.fleet.release(dropped);
        lock(&self.watch).retain(|k| !dropped.contains(k));
    }

    fn check_proxy(&self, key: String, url: String) {
        if !lock(&self.checking).insert(key.clone()) {
            return;
        }
        let (out, busy) = (self.out.clone(), Arc::clone(&self.checking));
        let spawned = std::thread::Builder::new().name("proxy-check".into()).spawn(move || {
            let fault = proxy::parse(&url).map_or(Some(Fault::Unreachable), |p| proxy::check(&p).err());
            lock(&busy).remove(&key);
            let _ = out.send_blocking(Update::ProxyChecked { key, fault });
        });
        if spawned.is_err() {
            lock(&self.checking).clear();
        }
    }

    fn probe(&self, server: String, proxy: Option<String>) {
        let out = self.out.clone();
        let spawned = std::thread::Builder::new().name("server-ping".into()).spawn(move || {
            let up = match proxy.as_deref().map(proxy::parse) {
                Some(Ok(p)) => crate::core::ping::answers(&server, Some(&p)),
                Some(Err(_)) => false,
                None => crate::core::ping::answers(&server, None),
            };
            let _ = out.send_blocking(Update::ServerProbed { server, up });
        });
        if let Err(e) = spawned {
            eprintln!("server ping: {e}");
        }
    }

    fn quit(&self) {
        self.fleet.close();
        let until = Instant::now() + QUIT_WAIT;
        while self.fleet.launching() && Instant::now() < until {
            std::thread::sleep(STAGGER_STEP);
        }
        for _ in 0..3 {
            let keys = self.fleet.running_keys();
            if keys.is_empty() {
                break;
            }
            self.fleet.stop_all(&keys);
        }
        self.fleet.settle_sharing();
    }

    fn leave(&self) {
        self.fleet.close();
        let until = Instant::now() + QUIT_WAIT;
        while self.fleet.launching() && Instant::now() < until {
            std::thread::sleep(STAGGER_STEP);
        }
    }
}

pub(crate) fn needs_sign_in(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<AuthError>() == Some(&AuthError::SignInAgain))
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}