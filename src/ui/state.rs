//! Runs the app behind the views: loads accounts and batches, drives the fleet, turns mod events into model changes and handles the actions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::actions::{Broadcast, CloneInstance, NextAttention, NextInstance, OpenChat, Play, PlayInstance, PreviousInstance, Reconnect, ReconnectMany, Reload, Respawn, RestartMany, SendChat, SendChatMany, SetColor, ShowGrid, Start, StartAll, StartBatch, StartMany, Stop, StopAll, StopBatch, StopMany};
use super::model::{self, ChatLine, Game, Model, Phase, rank};
use super::native;
use crate::core::account::{self, Account, AccountKind};
use crate::core::auth::{self, AuthError};
use crate::core::awake;
use crate::core::batch::{self, Batch};
use crate::core::fit;
use crate::core::fleet::{self, Fleet, Target};
use crate::core::history::{self, Kind};
use crate::core::instance::{self, Instance};
use crate::core::i18n::{tr, trf};
use crate::core::game_window;
use crate::core::ipc::{ChatKind, DisconnectCause, Event, FromMod, HotkeyAction, StateKind, ToMod};
use crate::core::place::{self, Insets, Place, Rect};
use crate::core::proxy::{self, Fault};
use crate::core::reconnect::{self, Decision, Reaction};
use crate::core::phone::{self, Incident, What};
use crate::core::settings;
use crate::core::store::{self, Recovery};
use crate::core::{adopt, drive, install, ipc, net, online};

const STABLE: Duration = Duration::from_secs(60);
const COALESCE: Duration = Duration::from_millis(40);
const ALERT_AFTER: Duration = Duration::from_secs(3);
const OFFSCREEN_PREVIEW: Duration = Duration::from_secs(30);
const EXIT_POLL: Duration = Duration::from_millis(500);
const STAGGER_STEP: Duration = Duration::from_millis(100);
const KEPT_CHAT: usize = 5000;
const ALERT_NAMES: usize = 4;
const QUIT_WAIT: Duration = Duration::from_secs(5);
const BACK_STAGGER: Duration = Duration::from_secs(1);
const BACK_WAIT_SECS: u64 = 2;
const WAITING_RETRY_SECS: u64 = 60;
const TRASH_KEPT: Duration = Duration::from_secs(600);
const SHOW_READY_FOR: Duration = Duration::from_secs(600);

static WAITING_PLAYS: Mutex<Vec<(String, Option<String>)>> = Mutex::new(Vec::new());

/// What the fleet reports back to the UI thread.
pub enum Update {
    Preparing { batch: String, percent: u8 },
    /// The game is installed; the accounts of `batch` wait in line to start.
    Prepared { batch: String },
    Launched { key: String, plain: bool },
    Failed { key: String, reason: String, login: bool, offline: bool },
    /// Whether the connection came back (`true`) or is gone.
    Reach { online: bool },
    Ipc(Event),
    Exited { key: String, code: Option<i32> },
    Preview { key: String, seq: u32, image: Arc<RenderImage> },
    Seed { key: String, phase: Phase, attempts: u32 },
    /// Whether the saved sign-in of an account still works, from a token refresh.
    SignIn { account: uuid::Uuid, valid: bool },
    /// The saved still of an instance, read from the cache.
    Still { key: String, image: Arc<RenderImage> },
    /// The skin face of an account.
    Face { account: uuid::Uuid, image: Arc<RenderImage> },
    /// What the check of an account's proxy found after a connection failure; `None` when it works.
    ProxyChecked { key: String, fault: Option<Fault> },
}

/// Runs and talks to instances off the UI thread: the real fleet, or a fake one when an agent drives the app.
pub trait Backend: Send + Sync {
    /// Starts `accounts` of `target` one after another until `cancel` is set.
    fn start(&self, target: Target, accounts: Vec<Account>, cancel: Arc<AtomicBool>, home: Option<Rect>);
    /// Asks the instances to quit and ends them after a grace period.
    fn stop(&self, keys: Vec<String>);
    /// Sends a message to an instance's companion mod.
    fn send(&self, key: &str, message: ToMod);
    /// Reads the newest preview frame from `path` and reports it.
    fn read_preview(&self, key: String, path: PathBuf);
    /// Stops every instance and waits for them, at quit.
    fn quit(&self);
    /// Learns the instances the model holds, by key and account name.
    fn known(&self, _instances: Vec<(String, String)>) {}
    /// Refreshes the sign-in of `accounts` one after another and reports each that works or needs a new sign-in.
    fn refresh(&self, _accounts: Vec<Account>) {}
    /// Checks the proxy `url` of instance `key` and reports the result.
    fn check_proxy(&self, _key: String, _url: String) {}
    /// The games an earlier manager launched that still run; only the first call returns them.
    fn adopted(&self) -> Vec<fleet::Adoption> {
        Vec::new()
    }
    /// Lets the adopted games that got a tile talk to the manager again and stops tracking the rest, which keep running.
    fn settle(&self, _placed: &[String], _dropped: &[String]) {}
}

struct Real {
    fleet: Arc<Fleet>,
    out: async_channel::Sender<Update>,
    watch: Arc<Mutex<HashSet<String>>>,
    owners: Arc<Mutex<HashMap<String, u64>>>,
    runs: AtomicU64,
    sends: mpsc::Sender<(String, ToMod)>,
    previews: mpsc::Sender<(String, PathBuf)>,
    refreshing: Arc<Mutex<HashSet<uuid::Uuid>>>,
    checking: Arc<Mutex<HashSet<String>>>,
    adopted: Mutex<Vec<fleet::Adoption>>,
}

impl Real {
    fn new(out: async_channel::Sender<Update>) -> anyhow::Result<Self> {
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
                if forward.send_blocking(Update::Ipc(event)).is_err() {
                    return;
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
        std::thread::Builder::new().name("fleet-send".into()).spawn(move || {
            while let Ok((key, message)) = outgoing.recv() {
                let _ = sender.send(&key, &message);
            }
        })?;
        let (previews, wanted) = mpsc::channel::<(String, PathBuf)>();
        let frames = out.clone();
        std::thread::Builder::new().name("previews".into()).spawn(move || {
            let mut changes = ipc::PreviewChanges::default();
            while let Ok(first) = wanted.recv() {
                let mut batch: HashMap<String, PathBuf> = HashMap::from([first]);
                batch.extend(wanted.try_iter());
                for (key, path) in batch {
                    let Some(preview) = ipc::read_preview(&path) else { continue };
                    if !changes.changed(&key, &preview) {
                        continue;
                    }
                    let Some(image) = render_image(preview.width, preview.height, preview.rgba) else { continue };
                    if frames.send_blocking(Update::Preview { key, seq: preview.seq, image }).is_err() {
                        return;
                    }
                }
            }
        })?;
        Ok(Self { fleet, out, watch, owners: Arc::default(), runs: AtomicU64::new(0), sends, previews, refreshing: Arc::default(), checking: Arc::default(), adopted: Mutex::new(adopted) })
    }
}

impl Backend for Real {
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
            let last = Mutex::new(u8::MAX);
            let progress = |p: install::Progress| {
                let percent = (p.done * 100).checked_div(p.total).unwrap_or(0).min(100) as u8;
                let mut last = lock(&last);
                if *last != percent {
                    *last = percent;
                    let _ = out.send_blocking(Update::Preparing { batch: owner.clone(), percent });
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
                            fleet.stop_all(std::slice::from_ref(&key));
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

    fn send(&self, key: &str, message: ToMod) {
        let _ = self.sends.send((key.to_owned(), message));
    }

    fn read_preview(&self, key: String, path: PathBuf) {
        let _ = self.previews.send((key, path));
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

    fn quit(&self) {
        self.fleet.close();
        let until = Instant::now() + QUIT_WAIT;
        while self.fleet.launching() && Instant::now() < until {
            std::thread::sleep(STAGGER_STEP);
        }
        for _ in 0..3 {
            let keys = self.fleet.running_keys();
            if keys.is_empty() {
                return;
            }
            self.fleet.stop_all(&keys);
        }
    }
}

fn needs_sign_in(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<AuthError>() == Some(&AuthError::SignInAgain))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Turns an RGBA frame into an image the GPU can draw.
pub fn render_image(width: u32, height: u32, mut rgba: Vec<u8>) -> Option<Arc<RenderImage>> {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Alert {
    Disconnected,
    Banned,
    Crashed,
    Login,
    Other,
}

impl Alert {
    fn of(phase: &Phase) -> Self {
        match phase {
            Phase::Disconnected { .. } => Self::Disconnected,
            Phase::Banned { .. } => Self::Banned,
            Phase::Crashed { .. } => Self::Crashed,
            Phase::LoginNeeded => Self::Login,
            _ => Self::Other,
        }
    }
}

struct Runtime {
    backend: Arc<dyn Backend>,
    cancel: HashMap<String, Vec<Arc<AtomicBool>>>,
    visible: HashSet<String>,
    shared_visible: Arc<Mutex<HashSet<String>>>,
    stale: HashMap<String, Instant>,
    read_at: HashMap<String, Instant>,
    reconnects: HashMap<String, u64>,
    joins: HashMap<String, u64>,
    crash_restarts: HashMap<String, u32>,
    started: HashMap<String, Instant>,
    joined: HashSet<String>,
    history: Option<history::Writer>,
    refreshed: bool,
    show_when_ready: Option<(String, Instant)>,
    alerts: HashMap<String, Vec<(String, Alert)>>,
    pids: HashMap<String, u32>,
    restarts: HashMap<String, u64>,
    restart_ready: HashMap<u64, Vec<String>>,
    restart_groups: u64,
    place: Option<Place>,
    insets: HashMap<String, Insets>,
    unplaced: HashSet<String>,
    spread: Option<Rect>,
    badge: usize,
    notice: Option<String>,
    whispers: Vec<Whisper>,
    reveal: Option<Reveal>,
    phone: phone::Coalescer,
    outbox: Option<phone::Outbox>,
    phone_at: Option<Instant>,
    /// Games an earlier manager launched whose mod has not reported its state since.
    adopted: HashSet<String>,
    /// Games an earlier manager launched; where the OS does not tell their exit code, an exit counts as clean.
    foreign: HashSet<String>,
}


/// What a game that was asked to show covers once it appears.
struct Reveal {
    key: String,
    hide: Vec<String>,
    manager: bool,
}impl Global for Runtime {}

/// Loads saved data into the model, starts the fleet and registers the action handlers.
pub fn init(cx: &mut App) {
    let (out, updates) = async_channel::unbounded::<Update>();
    super::stills::init(out.clone(), cx);
    let shared_visible: Arc<Mutex<HashSet<String>>> = Arc::default();
    let backend: Arc<dyn Backend> = if drive::on() && !drive::real() {
        Arc::new(super::fake::Fake::new(out, Arc::clone(&shared_visible)))
    } else {
        match Real::new(out) {
            Ok(real) => Arc::new(real),
            Err(e) => {
                let model = model::get(cx);
                model.update(cx, |m, cx| {
                    m.error = Some(format!("{e:#}"));
                    cx.notify();
                });
                return;
            }
        }
    };
    cx.set_global(Runtime {
        backend,
        cancel: HashMap::new(),
        visible: HashSet::new(),
        shared_visible,
        stale: HashMap::new(),
        read_at: HashMap::new(),
        reconnects: HashMap::new(),
        joins: HashMap::new(),
        crash_restarts: HashMap::new(),
        started: HashMap::new(),
        joined: HashSet::new(),
        history: history::Writer::start().ok(),
        refreshed: false,
        show_when_ready: None,
        alerts: HashMap::new(),
        pids: HashMap::new(),
        restarts: HashMap::new(),
        restart_ready: HashMap::new(),
        restart_groups: 0,
        place: None,
        insets: HashMap::new(),
        unplaced: HashSet::new(),
        spread: None,
        reveal: None,
        badge: 0,
        notice: None,
        whispers: Vec::new(),
        phone: phone::Coalescer::default(),
        outbox: None,
        phone_at: None,
        adopted: HashSet::new(),
        foreign: HashSet::new(),
    });
    if let Err(e) = settings::activate() {
        eprintln!("settings: {e:#}");
        cx.global_mut::<Runtime>().notice = Some(super::forms::untouched(&e).unwrap_or_else(|| format!("{e:#}")));
    }
    cx.background_spawn(async {
        if !crate::core::updater::install::running_alone() {
            return;
        }
        if let Err(e) = instance::purge_stale(TRASH_KEPT) {
            eprintln!("trash: {e:#}");
        }
        if let Err(e) = crate::core::trash::purge_stale(&instance::instances_dir(), TRASH_KEPT) {
            eprintln!("trash: {e:#}");
        }
    })
    .detach();
    reload(cx);
    cx.observe(&model::get(cx), |_, cx| keep_awake(cx)).detach();
    cx.spawn(async move |cx| {
        while let Ok(first) = updates.recv().await {
            let mut batch = vec![first];
            while let Ok(next) = updates.try_recv() {
                batch.push(next);
            }
            cx.update(|cx| apply_all(batch, cx));
            cx.background_executor().timer(COALESCE).await;
        }
    })
    .detach();
    cx.on_action(|a: &Start, cx| start_keys(std::slice::from_ref(&a.0), cx));
    cx.on_action(|a: &Stop, cx| stop_keys(vec![a.0.clone()], cx));
    cx.on_action(|a: &StartBatch, cx| start_batch(&a.0, cx));
    cx.on_action(|a: &StopBatch, cx| stop_batch(&a.0, cx));
    cx.on_action(|_: &StartAll, cx| {
        for id in batch_ids(cx) {
            start_batch(&id, cx);
        }
    });
    cx.on_action(|_: &StopAll, cx| {
        for id in batch_ids(cx) {
            stop_batch(&id, cx);
        }
    });
    cx.on_action(|a: &Reconnect, cx| reconnect(&a.0, cx));
    cx.on_action(|a: &Respawn, cx| respawn(&a.0, cx));
    cx.on_action(|a: &Play, cx| {
        let key = a.0.clone();
        cx.defer(move |cx| play(&key, cx));
    });
    cx.on_action(|a: &PlayInstance, cx| {
        let instance = a.instance.clone();
        let account = a.account.as_deref().and_then(|id| id.parse().ok());
        cx.defer(move |cx| {
            play_instance(&instance, account, cx);
        });
    });
    cx.on_action(|a: &CloneInstance, cx| clone_instance(&a.0, cx));
    cx.on_action(|_: &ShowGrid, cx| cx.defer(show_grid));
    cx.on_action(|_: &NextInstance, cx| cx.defer(|cx| step(HotkeyAction::Next, cx)));
    cx.on_action(|_: &PreviousInstance, cx| cx.defer(|cx| step(HotkeyAction::Prev, cx)));
    cx.on_action(|_: &NextAttention, cx| cx.defer(|cx| step(HotkeyAction::NextAttention, cx)));
    cx.on_action(|_: &Reload, cx| reload(cx));
    cx.on_action(|_: &OpenChat, cx| cx.defer(super::chat::open));
    cx.on_action(|a: &Broadcast, cx| broadcast(&a.batch, &a.text, cx));
    cx.on_action(|a: &SendChat, cx| send_chat(&a.key, &a.text, cx));
    cx.on_action(|a: &StartMany, cx| {
        let keys = signed_in(&a.0, cx);
        start_keys(&keys, cx);
    });
    cx.on_action(|a: &StopMany, cx| stop_keys(a.0.clone(), cx));
    cx.on_action(|a: &RestartMany, cx| {
        let keys = signed_in(&a.0, cx);
        restart_keys(&keys, cx);
    });
    cx.on_action(|a: &ReconnectMany, cx| {
        for key in &a.0 {
            reconnect(key, cx);
        }
    });
    cx.on_action(|a: &SetColor, cx| set_color(a, cx));
    cx.on_action(|a: &SendChatMany, cx| {
        for key in &a.keys {
            send_chat(key, &a.text, cx);
        }
    });
    cx.on_system_notification_response(|response, cx| {
        let batch = response.tag.strip_prefix("attention-").map(str::to_owned);
        let whisper = response.tag.as_ref() == WHISPER_TAG;
        cx.defer(move |cx| {
            show_grid(cx);
            if let Some(batch) = batch {
                super::main_window::select_batch(&batch, cx);
            }
            if whisper {
                super::chat::show(cx);
            }
        });
    });
    cx.on_app_quit(|cx| {
        awake::set(false);
        if let Some(rt) = cx.try_global::<Runtime>() {
            for cancel in rt.cancel.values().flatten() {
                cancel.store(true, Ordering::SeqCst);
            }
            let backend = Arc::clone(&rt.backend);
            match std::thread::Builder::new().name("quit".into()).spawn(move || backend.quit()) {
                Ok(closing) => close_games(closing, cx),
                Err(_) => rt.backend.quit(),
            }
        }
        async {}
    })
    .detach();
}

fn close_games(closing: std::thread::JoinHandle<()>, cx: &mut App) {
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| native::hide(window, None));
    }
    let _ = closing.join();
}

fn batch_ids(cx: &App) -> Vec<String> {
    model::get(cx).read(cx).batches.iter().map(|b| b.id.clone()).collect()
}

fn backend(cx: &App) -> Option<Arc<dyn Backend>> {
    cx.try_global::<Runtime>().map(|rt| Arc::clone(&rt.backend))
}

/// Reads accounts and batches from disk again and rebuilds the instance list, keeping what runs.
pub fn reload(cx: &mut App) {
    reload_with(cx, false);
}

/// Like [`reload`], but a message already shown stays.
pub fn reload_keeping_error(cx: &mut App) {
    reload_with(cx, true);
}

fn reload_with(cx: &mut App, keep: bool) {
    let task = cx.background_spawn(async {
        let moved = crate::core::migrate::run();
        let (accounts, batches, instances) = (account::load(), batch::load_all(), instance::load_all());
        let logs = match (&batches, &instances) {
            (Ok(batches), Ok(instances)) => saved_history(batches, instances),
            _ => Vec::new(),
        };
        let loaded = batches.and_then(|b| instances.map(|i| (b, i)));
        (accounts, loaded, logs, moved.problems, store::take_recovered())
    });
    cx.spawn(async move |cx| {
        let (accounts, batches, logs, problems, recovered) = task.await;
        cx.update(|cx| {
            if let Some(text) = recovered_notice(&recovered)
                && cx.has_global::<Runtime>()
            {
                let rt = cx.global_mut::<Runtime>();
                rt.notice = Some(rt.notice.take().map_or(text.clone(), |before| format!("{before} {text}")));
            }
            let model = model::get(cx);
            let loaded = accounts.is_ok() && batches.is_ok();
            let notice = if loaded && cx.has_global::<Runtime>() { cx.global_mut::<Runtime>().notice.take() } else { None };
            let notice = if problems.is_empty() { notice } else { Some(notice.into_iter().fold(problems.join("\n"), |p, n| format!("{p}\n{n}"))) };
            let mut retired = Vec::new();
            model.update(cx, |m, cx| {
                for (key, log) in logs {
                    m.history.entry(key).or_insert(log);
                }
                match (accounts, batches) {
                    (Ok(accounts), Ok((batches, instances))) => {
                        m.accounts = accounts;
                        m.batches = batches;
                        m.instances = instances;
                        m.loaded = true;
                        m.error = if keep { notice.or(m.error.take()) } else { notice };
                    }
                    (Err(e), _) | (_, Err(e)) => m.error = Some(super::forms::untouched(&e).unwrap_or_else(|| format!("{e:#}"))),
                }
                retired = rebuild(m);
                cx.notify();
            });
            for image in retired {
                cx.drop_image(image, None);
            }
            if model.read(cx).loaded
                && let Some(rt) = cx.try_global::<Runtime>()
            {
                let adopted = rt.backend.adopted();
                if !adopted.is_empty() {
                    update_model(cx, |m, rt, _| adopt_games(m, rt, adopted));
                }
            }
            let present: HashSet<String> = model.read(cx).games.keys().cloned().collect();
            super::tile::forget_except(&|k| present.contains(k), cx);
            super::stills::forget_except(&|k| present.contains(k), cx);
            let known: Vec<(String, String)> = model.read(cx).games.values().map(|i| (i.key.clone(), i.name.clone())).collect();
            let first = cx.try_global::<Runtime>().is_some_and(|rt| !rt.refreshed);
            let to_check: Vec<Account> = {
                let m = model.read(cx);
                m.accounts.iter().filter(|a| first || m.games.values().any(|i| i.account_id == a.id && i.phase == Phase::LoginNeeded)).cloned().collect()
            };
            let (bare, faceless): (Vec<String>, Vec<Account>) = {
                let m = model.read(cx);
                (m.games.values().filter(|i| i.preview.is_none()).map(|i| i.key.clone()).collect(), m.accounts.iter().filter(|a| !m.faces.contains_key(&a.id)).cloned().collect())
            };
            super::stills::load(bare, faceless, cx);
            if let Some(rt) = cx.try_global::<Runtime>() {
                rt.backend.known(known);
                rt.backend.refresh(to_check);
            }
            if first && cx.has_global::<Runtime>() {
                cx.global_mut::<Runtime>().refreshed = true;
            }
            tell_games(cx);
            if model.read(cx).loaded {
                run_waiting_plays(cx);
            }
        });
    })
    .detach();
}

fn recovered_notice(found: &[(PathBuf, Recovery)]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for (path, recovery) in found {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let count = found.iter().filter(|(p, r)| r == recovery && p.file_name() == path.file_name()).count();
        let line = match (name.as_str(), recovery, count) {
            ("accounts.json", Recovery::Restored, _) => tr("Die Accounts waren beschädigt und wurden aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht."),
            ("accounts.json", Recovery::Lost, _) => tr("Die Accounts waren beschädigt und ließen sich nicht wiederherstellen. Füg sie neu hinzu; eine Kopie liegt neben der Datei (accounts.json.damaged)."),
            ("settings.json", Recovery::Restored, _) => tr("Die Einstellungen waren beschädigt und wurden aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht."),
            ("settings.json", Recovery::Lost, _) => tr("Die Einstellungen waren beschädigt und ließen sich nicht wiederherstellen. Es gelten die Standardwerte; eine Kopie liegt neben der Datei (settings.json.damaged)."),
            ("batch.json", Recovery::Restored, 1) => tr("Ein Batch war beschädigt und wurde aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht."),
            ("batch.json", Recovery::Restored, n) => trf("%lld Batches waren beschädigt und wurden aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht.", &[&n]),
            ("batch.json", Recovery::Lost, 1) => tr("Ein Batch war beschädigt und ließ sich nicht wiederherstellen. Leg ihn neu an; eine Kopie liegt in seinem Ordner (batch.json.damaged)."),
            ("batch.json", Recovery::Lost, n) => trf("%lld Batches waren beschädigt und ließen sich nicht wiederherstellen. Leg sie neu an; Kopien liegen in ihren Ordnern (batch.json.damaged).", &[&n]),
            ("instance.json", Recovery::Restored, 1) => tr("Eine Instanz war beschädigt und wurde aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht."),
            ("instance.json", Recovery::Restored, n) => trf("%lld Instanzen waren beschädigt und wurden aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht.", &[&n]),
            ("instance.json", Recovery::Lost, 1) => tr("Eine Instanz war beschädigt und ließ sich nicht wiederherstellen. Ihre Mods und Welten bleiben in ihrem Ordner, dazu eine Kopie (instance.json.damaged)."),
            ("instance.json", Recovery::Lost, n) => trf("%lld Instanzen waren beschädigt und ließen sich nicht wiederherstellen. Ihre Mods und Welten bleiben in ihren Ordnern, dazu je eine Kopie (instance.json.damaged).", &[&n]),
            (_, Recovery::Restored, _) => trf("%@ war beschädigt und wurde aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht.", &[&name]),
            (_, Recovery::Lost, _) => trf("%@ war beschädigt und ließ sich nicht wiederherstellen. Eine Kopie liegt daneben (%@.damaged).", &[&name, &name]),
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    if lines.len() < 2 {
        return lines.pop();
    }
    let restored = found.iter().filter(|(_, r)| *r == Recovery::Restored).count();
    Some(match (restored, found.len()) {
        (r, all) if r == all => trf("%lld Dateien waren beschädigt und wurden aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht.", &[&all]),
        (0, all) => trf("%lld Dateien waren beschädigt und ließen sich nicht wiederherstellen. Kopien liegen jeweils daneben (Endung .damaged).", &[&all]),
        (r, all) => trf("%lld Dateien waren beschädigt: %lld aus der Sicherung wiederhergestellt, die übrigen nicht. Kopien liegen jeweils daneben (Endung .damaged).", &[&all, &r]),
    })
}

fn saved_history(batches: &[Batch], instances: &[Instance]) -> Vec<(String, history::Log)> {
    let mut logs = Vec::new();
    for batch in batches {
        let Some(instance) = instances.iter().find(|i| i.id == batch.instance_id) else { continue };
        for id in &batch.accounts {
            let log = history::load(&history::path(instance, *id));
            if !log.is_empty() {
                logs.push((fleet::instance_key(batch, *id), log));
            }
        }
    }
    logs
}

fn rebuild(m: &mut Model) -> Vec<Arc<RenderImage>> {
    let mut keep: HashSet<String> = HashSet::new();
    for batch in &m.batches {
        for id in &batch.accounts {
            let Some(account) = m.accounts.iter().find(|a| a.id == *id) else { continue };
            let key = fleet::instance_key(batch, *id);
            keep.insert(key.clone());
            let instance = m.games.entry(key.clone()).or_insert_with(|| Game {
                key,
                batch_id: batch.id.clone(),
                instance_id: batch.instance_id.clone(),
                account_id: *id,
                name: account.name.clone(),
                phase: Phase::Stopped,
                reconnect_attempts: 0,
                unread_whispers: 0,
                preview: None,
                preview_seq: 0,
                live: false,
                stopping: false,
            });
            instance.name = account.name.clone();
            if !instance.phase.is_running() {
                instance.instance_id.clone_from(&batch.instance_id);
            }
        }
    }
    let mut retired = Vec::new();
    let instances = &m.instances;
    let accounts = &m.accounts;
    m.games.retain(|k, i| {
        let solo = i.batch_id == fleet::solo_owner(&i.instance_id) && instances.iter().any(|made| made.id == i.instance_id) && accounts.iter().any(|a| a.id == i.account_id);
        let stays = keep.contains(k) || i.phase.is_running() || (solo && !matches!(i.phase, Phase::Stopped));
        if !stays && let Some(image) = i.preview.take() {
            retired.push(image);
        }
        stays
    });
    let instances = &m.games;
    m.history.retain(|k, _| instances.contains_key(k));
    let accounts = &m.accounts;
    m.faces.retain(|id, image| {
        let stays = accounts.iter().any(|a| a.id == *id);
        if !stays {
            retired.push(Arc::clone(image));
        }
        stays
    });
    m.order_seq += 1;
    retired
}

/// The account `made` is played with alone when none is named: the one that played it last, else the only one.
pub fn solo_account(m: &Model, made: &Instance) -> Option<uuid::Uuid> {
    made.last_account.filter(|id| m.account(*id).is_some()).or_else(|| match m.accounts.as_slice() {
        [only] => Some(only.id),
        _ => None,
    })
}

/// Plays `instance` as another start of Manycraft or a shortcut asked: with `account`, else the one that played it last. Waits for the first load when the data is not there yet, and shows the game as soon as it can be shown.
pub fn request_play(instance: String, account: Option<String>, cx: &mut App) {
    if !model::get(cx).read(cx).loaded {
        WAITING_PLAYS.lock().unwrap_or_else(|e| e.into_inner()).push((instance, account));
        return;
    }
    let account = account.as_deref().and_then(|id| id.parse().ok());
    match play_instance(&instance, account, cx) {
        Some(key) => {
            if cx.has_global::<Runtime>() {
                cx.global_mut::<Runtime>().show_when_ready = Some((key, Instant::now()));
            }
        }
        None => show_grid(cx),
    }
}

fn run_waiting_plays(cx: &mut App) {
    let waiting = std::mem::take(&mut *WAITING_PLAYS.lock().unwrap_or_else(|e| e.into_inner()));
    for (instance, account) in waiting {
        request_play(instance, account, cx);
    }
}

fn show_ready(cx: &mut App) {
    let Some((key, since)) = cx.try_global::<Runtime>().and_then(|rt| rt.show_when_ready.clone()) else { return };
    let model = model::get(cx);
    let (shown, ready) = {
        let m = model.read(cx);
        (m.playing.as_deref() == Some(key.as_str()), m.games.get(&key).is_some_and(|g| playable(&g.phase)) && !model::hint(&key).link_lost)
    };
    if !shown && !ready && since.elapsed() < SHOW_READY_FOR {
        return;
    }
    cx.global_mut::<Runtime>().show_when_ready = None;
    if ready && !shown {
        play(&key, cx);
    }
}

fn play_instance(id: &str, account: Option<uuid::Uuid>, cx: &mut App) -> Option<String> {
    let model = model::get(cx);
    let picked = {
        let m = model.read(cx);
        m.instance(id).and_then(|made| {
            let account = account.or_else(|| solo_account(m, made)).filter(|a| m.account(*a).is_some())?;
            let key = fleet::solo_key(made, account);
            let shown = m.games.get(&key).is_some_and(|g| g.phase.is_active());
            Some((key, account, shown))
        })
    };
    let Some((key, account, shown)) = picked else {
        if model.read(cx).accounts.is_empty() {
            cx.dispatch_action(&super::actions::AddAccount);
        } else {
            let id = id.to_owned();
            cx.defer(move |cx| {
                super::main_window::show_instance(&id, cx);
            });
        }
        return None;
    };
    if shown {
        play(&key, cx);
        return Some(key);
    }
    let owner = fleet::solo_owner(id);
    update_model(cx, |m, _, _| {
        let name = m.account(account).map(|a| a.name.clone()).unwrap_or_default();
        m.games.entry(key.clone()).or_insert_with(|| Game {
            key: key.clone(),
            batch_id: owner.clone(),
            instance_id: id.to_owned(),
            account_id: account,
            name,
            phase: Phase::Stopped,
            reconnect_attempts: 0,
            unread_whispers: 0,
            preview: None,
            preview_seq: 0,
            live: false,
            stopping: false,
        });
        if let Some(made) = m.instances.iter_mut().find(|made| made.id == id) {
            made.last_account = Some(account);
        }
        m.order_seq += 1;
    });
    launch(&owner, Some(std::slice::from_ref(&key)), false, cx);
    let id = id.to_owned();
    cx.background_spawn(async move {
        if let Err(e) = super::remote::remember_account(&id, account) {
            eprintln!("last account: {e:#}");
        }
    })
    .detach();
    Some(key)
}

/// Shows `text` as the notice after the next reload.
pub fn tell(text: String, cx: &mut App) {
    if cx.has_global::<Runtime>() {
        cx.global_mut::<Runtime>().notice = Some(text);
    } else {
        model::get(cx).update(cx, |m, cx| {
            m.error = Some(text);
            cx.notify();
        });
    }
}

fn clone_instance(id: &str, cx: &mut App) {
    let source = id.to_owned();
    let task = cx.background_spawn({
        let id = source.clone();
        async move { super::remote::clone_instance(&id) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(made) => {
                    let (id, name) = (made.id.clone(), made.name.clone());
                    model::get(cx).update(cx, |m, cx| {
                        m.instances.push(made);
                        cx.notify();
                    });
                    super::instance_page::rename(&id, cx);
                    super::undo::offer(super::undo::Kind::Cloned { id, name, source }, cx);
                }
                Err(e) => tell(super::forms::explain_as(tr("Nicht geklont."), &e).text, cx),
            }
            reload(cx);
        });
    })
    .detach();
}

/// Deletes instance `id` with its games and offers to bring it back; says why when it was refused.
pub fn delete_instance(id: &str, cx: &mut App) {
    let id = id.to_owned();
    let gone = id.clone();
    let name = model::get(cx).read(cx).instance(&id).map(|made| made.name.clone()).unwrap_or_default();
    let task = cx.background_spawn(async move { super::remote::delete_instance(&id) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(entry) => {
                    model::get(cx).update(cx, |m, cx| {
                        m.instances.retain(|made| made.id != gone);
                        cx.notify();
                    });
                    super::undo::offer(super::undo::Kind::Deleted { id: gone, name, entry }, cx);
                }
                Err(e) => tell(super::forms::explain_as(tr("Nicht gelöscht."), &e).text, cx),
            }
            reload(cx);
        });
    })
    .detach();
}

fn update_model(cx: &mut App, f: impl FnOnce(&mut Model, &mut Runtime, &mut App)) {
    change_model(Redraw::All, cx, f);
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Redraw {
    All,
    Chat,
    Nothing,
}

fn change_model(redraw: Redraw, cx: &mut App, f: impl FnOnce(&mut Model, &mut Runtime, &mut App)) {
    let model = model::get(cx);
    if !cx.has_global::<Runtime>() {
        return;
    }
    cx.update_global::<Runtime, _>(|rt, cx| {
        model.update(cx, |m, cx| {
            f(m, rt, cx);
            match redraw {
                Redraw::All => cx.notify(),
                Redraw::Chat => cx.emit(model::ChatArrived),
                Redraw::Nothing => {}
            }
        });
    });
    refresh_badge(cx);
}

fn redraw(updates: &[Update]) -> Redraw {
    let mut chat = false;
    for update in updates {
        match update {
            Update::Ipc(Event::Message { instance, message: FromMod::Preview { .. } }) if !model::hint(instance).link_lost => {}
            Update::Ipc(Event::Message { instance, message: FromMod::Chat { kind, .. } }) if *kind != ChatKind::Whisper && !model::hint(instance).link_lost => chat = true,
            _ => return Redraw::All,
        }
    }
    if chat && settings::active().merged_chat { Redraw::Chat } else { Redraw::Nothing }
}

fn start_keys(keys: &[String], cx: &mut App) {
    let model = model::get(cx);
    let mut by_batch: HashMap<String, Vec<String>> = HashMap::new();
    for key in keys {
        if let Some(i) = model.read(cx).games.get(key) {
            by_batch.entry(i.batch_id.clone()).or_default().push(key.clone());
        }
    }
    for (batch, keys) in by_batch {
        launch(&batch, Some(&keys), false, cx);
    }
}

fn start_batch(batch: &str, cx: &mut App) {
    launch(batch, None, false, cx);
}

fn launch(owner: &str, only: Option<&[String]>, restart: bool, cx: &mut App) {
    let alive: HashSet<String> = cx.try_global::<Runtime>().map(|rt| rt.pids.keys().cloned().collect()).unwrap_or_default();
    let model = model::get(cx);
    let (target, accounts, busy) = {
        let m = model.read(cx);
        let Some(target) = m.target(owner) else { return };
        let ids: Vec<uuid::Uuid> = match &target.batch {
            Some(batch) => batch.accounts.clone(),
            None => m.games.values().filter(|g| g.batch_id == owner).map(|g| g.account_id).collect(),
        };
        let wanted: Vec<Account> = ids
            .iter()
            .filter_map(|id| m.account(*id).cloned())
            .filter(|a| {
                let key = target.key(a.id);
                only.is_none_or(|o| o.contains(&key)) && !alive.contains(&key) && m.games.get(&key).is_some_and(|i| if only.is_some() { !i.phase.is_running() } else { !i.phase.is_active() })
            })
            .collect();
        let (busy, accounts): (Vec<Account>, Vec<Account>) = wanted.into_iter().partition(|a| m.playing_elsewhere(&target.instance.id, a.id, &target.key(a.id)).is_some());
        let busy: Vec<(String, String)> = busy.iter().filter_map(|a| m.playing_elsewhere(&target.instance.id, a.id, &target.key(a.id)).map(|place| (target.key(a.id), trf("Spielt schon in %@", &[&place])))).collect();
        (target, accounts, busy)
    };
    if !busy.is_empty() {
        update_model(cx, |m, _, _| {
            for (key, reason) in busy {
                if let Some(i) = m.games.get_mut(&key) {
                    i.phase = Phase::Failed { reason };
                }
            }
            m.order_seq += 1;
        });
    }
    if accounts.is_empty() {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    update_model(cx, |m, rt, _| {
        let runs = rt.cancel.entry(owner.to_owned()).or_default();
        runs.retain(|c| Arc::strong_count(c) > 1);
        runs.push(Arc::clone(&cancel));
        let total = accounts.len();
        for (n, account) in accounts.iter().enumerate() {
            let key = target.key(account.id);
            if !restart {
                rt.crash_restarts.remove(&key);
                *rt.reconnects.entry(key.clone()).or_default() += 1;
            }
            if let Some(i) = m.games.get_mut(&key) {
                i.phase = Phase::Starting { place: n + 1, total };
                i.reconnect_attempts = 0;
                i.stopping = false;
            }
        }
        m.order_seq += 1;
    });
    if !restart {
        warn_if_too_big(u32::try_from(accounts.len()).unwrap_or(u32::MAX), target.memory_mb(), cx);
    }
    let home = manager_place(cx).map(|p| p.frame);
    if let Some(backend) = backend(cx) {
        backend.start(target, accounts, cancel, home);
    }
}

fn warn_if_too_big(count: u32, memory_mb: u32, cx: &mut App) {
    let model = model::get(cx);
    cx.spawn(async move |cx| {
        let fit = cx.background_executor().spawn(async move { fit::fit(count, memory_mb) }).await;
        let Some(fit) = fit.filter(|f| !f.fits()) else { return };
        let _ = model.update(cx, |m, cx| {
            m.error = Some(model::fit_line(count, memory_mb, &fit));
            cx.notify();
        });
    })
    .detach();
}

/// `keys` without the accounts that need to sign in again, which a bulk start would only fail.
fn signed_in(keys: &[String], cx: &App) -> Vec<String> {
    let m = model::get(cx).read(cx);
    keys.iter().filter(|k| m.games.get(*k).is_some_and(|i| i.phase != Phase::LoginNeeded)).cloned().collect()
}

/// The keys of restart `group` once none of them is still on its way down, so they start again together.
fn restart_done(rt: &mut Runtime, group: u64) -> Option<Vec<String>> {
    if rt.restarts.values().any(|g| *g == group) { None } else { rt.restart_ready.remove(&group).filter(|keys| !keys.is_empty()) }
}

/// Takes `key` out of any restart, so it is not started again; returns the others of its group once none of them is still on its way down.
fn drop_restart(rt: &mut Runtime, key: &str) -> Vec<String> {
    for waiting in rt.restart_ready.values_mut() {
        waiting.retain(|k| k != key);
    }
    rt.restarts.remove(key).and_then(|group| restart_done(rt, group)).unwrap_or_default()
}

fn stop_keys(keys: Vec<String>, cx: &mut App) {
    let mut running = Vec::new();
    let mut ready = Vec::new();
    update_model(cx, |m, rt, _| {
        for key in &keys {
            ready.extend(drop_restart(rt, key));
            rt.reconnects.entry(key.clone()).and_modify(|g| *g += 1).or_insert(1);
            rt.joins.entry(key.clone()).and_modify(|g| *g += 1).or_insert(1);
            rt.crash_restarts.remove(key);
            rt.joined.remove(key);
            rt.phone.forget(key);
            let Some(i) = m.games.get_mut(key) else { continue };
            match i.phase {
                Phase::Starting { .. } if !rt.pids.contains_key(key) => {
                    i.phase = Phase::Stopped;
                    running.push(key.clone());
                }
                ref p if p.is_running() || rt.pids.contains_key(key) => {
                    i.stopping = true;
                    running.push(key.clone());
                }
                Phase::Crashed { .. } | Phase::Restarting { .. } | Phase::Failed { .. } | Phase::NoNetwork => i.phase = Phase::Stopped,
                _ => {}
            }
        }
        m.order_seq += 1;
    });
    ready.retain(|k| !keys.contains(k));
    if !running.is_empty()
        && let Some(backend) = backend(cx)
    {
        backend.stop(running);
    }
    if !ready.is_empty() {
        start_keys(&ready, cx);
    }
}

fn restart_keys(keys: &[String], cx: &mut App) {
    let running: Vec<String> = {
        let pids: HashSet<&String> = cx.try_global::<Runtime>().map(|rt| rt.pids.keys().collect()).unwrap_or_default();
        let m = model::get(cx).read(cx);
        keys.iter().filter(|k| pids.contains(k) || m.games.get(*k).is_some_and(|i| i.phase.is_running())).cloned().collect()
    };
    stop_keys(running.clone(), cx);
    let mut start: Vec<String> = keys.iter().filter(|k| !running.contains(k)).cloned().collect();
    update_model(cx, |m, rt, _| {
        rt.restart_groups += 1;
        let group = rt.restart_groups;
        for key in running {
            if m.games.get(&key).is_some_and(|i| i.stopping) {
                rt.restarts.insert(key, group);
            } else {
                start.push(key);
            }
        }
    });
    start_keys(&start, cx);
}

fn stop_batch(batch: &str, cx: &mut App) {
    if cx.has_global::<Runtime>()
        && let Some(runs) = cx.global_mut::<Runtime>().cancel.remove(batch)
    {
        for cancel in runs {
            cancel.store(true, Ordering::SeqCst);
        }
    }
    let keys: Vec<String> = model::get(cx).read(cx).games.values().filter(|i| i.batch_id == batch).map(|i| i.key.clone()).collect();
    stop_keys(keys, cx);
}

fn set_color(a: &SetColor, cx: &mut App) {
    let ids: Vec<uuid::Uuid> = a.accounts.iter().filter_map(|id| id.parse().ok()).collect();
    let color = a.color;
    model::get(cx).update(cx, |m, cx| {
        for account in m.accounts.iter_mut().filter(|account| ids.contains(&account.id)) {
            account.color = Some(color);
        }
        cx.notify();
    });
    let task = cx.background_spawn(async move { super::remote::set_color(&ids, color) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            if let Err(e) = result {
                eprintln!("color: {e:#}");
                let text = tr("Die Farbe wurde nicht gesichert. Versuch es noch einmal.");
                if cx.has_global::<Runtime>() {
                    cx.global_mut::<Runtime>().notice = Some(text);
                } else {
                    model::get(cx).update(cx, |m, cx| {
                        m.error = Some(text);
                        cx.notify();
                    });
                }
            }
            reload(cx);
        });
    })
    .detach();
}

fn reconnect(key: &str, cx: &mut App) {
    let key = key.to_owned();
    let mut send = false;
    update_model(cx, |m, rt, _| {
        let Some(i) = m.games.get_mut(&key) else { return };
        if !i.phase.is_running() || matches!(i.phase, Phase::Plain) {
            return;
        }
        *rt.reconnects.entry(key.clone()).or_default() += 1;
        *rt.joins.entry(key.clone()).or_default() += 1;
        i.reconnect_attempts = 0;
        i.phase = Phase::Connecting { server: server_of(&i.phase).unwrap_or_default() };
        m.order_seq += 1;
        send = true;
    });
    if send && let Some(backend) = backend(cx) {
        backend.send(&key, ToMod::Reconnect);
    }
}

fn respawn(key: &str, cx: &mut App) {
    let dead = model::get(cx).read(cx).games.get(key).is_some_and(|i| matches!(i.phase, Phase::Dead { .. }));
    if dead && let Some(backend) = backend(cx) {
        backend.send(key, ToMod::Respawn);
    }
}

fn server_of(phase: &Phase) -> Option<String> {
    match phase {
        Phase::Connecting { server } | Phase::Queue { server } | Phase::Playing { server, .. } | Phase::NetworkLost { server } => Some(server.clone()),
        Phase::Disconnected { server, .. } | Phase::Dead { server, .. } | Phase::Banned { server, .. } => server.clone(),
        _ => None,
    }
}

/// Sends a chat line or command to one running instance.
fn send_chat(key: &str, text: &str, cx: &mut App) {
    let Some(backend) = backend(cx) else { return };
    if model::get(cx).read(cx).games.get(key).is_some_and(|i| i.phase.on_server()) {
        backend.send(key, ToMod::ChatSend { text: text.to_owned() });
    }
}

/// Marks the whispers of `batch` (all instances when `None`) as seen.
pub fn clear_whispers(batch: Option<&str>, cx: &mut App) {
    let model = model::get(cx);
    if !model.read(cx).games.values().any(|i| i.unread_whispers > 0 && batch.is_none_or(|b| i.batch_id == b)) {
        return;
    }
    update_model(cx, |m, _, _| {
        for i in m.games.values_mut().filter(|i| batch.is_none_or(|b| i.batch_id == b)) {
            i.unread_whispers = 0;
        }
    });
}

fn broadcast(batch: &str, text: &str, cx: &mut App) {
    let Some(backend) = backend(cx) else { return };
    let model = model::get(cx);
    for i in model.read(cx).games.values().filter(|i| i.batch_id == batch && i.phase.on_server()) {
        backend.send(&i.key, ToMod::ChatSend { text: text.to_owned() });
    }
}

fn playable(phase: &Phase) -> bool {
    matches!(phase, Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } | Phase::NetworkLost { .. } | Phase::Disconnected { .. } | Phase::Dead { .. })
}

/// Puts the game window of `key` in the manager's place and hides the manager.
pub fn play(key: &str, cx: &mut App) {
    let model = model::get(cx);
    if let Some(i) = model.read(cx).games.get(key).filter(|i| matches!(i.phase, Phase::Plain)) {
        let text = trf("%@ läuft in einem eigenen Fenster, ohne Manycrafts Mod. Hier lässt es sich nicht zeigen.", &[&i.name]);
        model.update(cx, |m, cx| {
            m.error = Some(text);
            cx.notify();
        });
        return;
    }
    if !model.read(cx).games.get(key).is_some_and(|i| playable(&i.phase)) || model::hint(key).link_lost {
        return;
    }
    let shown = model.read(cx).playing.is_some();
    let place = if shown { follow_shown(cx) } else { manager_place(cx) };
    let Some(place) = place else { return };
    swap_to(key, place, cx);
    spread(cx);
}

fn swap_to(key: &str, place: Place, cx: &mut App) {
    let key = key.to_owned();
    let mut skipped = None;
    let mut client = place.frame;
    update_model(cx, |m, rt, _| {
        rt.place = Some(place);
        let before = m.playing.replace(key.clone());
        let mut reveal = match rt.reveal.take() {
            Some(r) if r.key != key => {
                skipped = Some(r.key);
                Reveal { key: key.clone(), hide: r.hide, manager: r.manager }
            }
            Some(r) => r,
            None => Reveal { key: key.clone(), manager: before.is_none(), hide: before.into_iter().collect() },
        };
        reveal.hide.retain(|k| *k != key);
        rt.reveal = Some(reveal);
        if let Some(i) = m.games.get_mut(&key) {
            i.unread_whispers = 0;
        }
        if let Some(pid) = rt.pids.get(&key) {
            native::allow_foreground(*pid);
            if let Some(seen) = game_window::seen(*pid).filter(|s| s.mode.is_normal() && !drive::on()) {
                rt.insets.insert(key.clone(), seen.insets);
            }
        }
        client = place::client_in(place.frame, rt.insets.get(&key).copied().unwrap_or_default());
    });
    drive::note(format!("play {key}"));
    if let Some(backend) = backend(cx) {
        backend.send(&key, ToMod::Show { x: client.x, y: client.y, w: client.w, h: client.h, mode: place.mode });
        if let Some(skipped) = skipped {
            backend.send(&skipped, ToMod::Hide);
        }
    }
}

/// Hides what the game that is now shown covers: the manager and the games shown before it. Waiting for the game keeps the screen from flashing while it resizes on its first show.
fn reveal(reveal: Reveal, cx: &mut App) {
    let playing = model::get(cx).read(cx).playing.clone();
    if let Some(backend) = backend(cx) {
        for key in reveal.hide.iter().filter(|k| playing.as_ref() != Some(*k)) {
            backend.send(key, ToMod::Hide);
        }
    }
    if reveal.manager && playing.is_some() {
        let place = cx.try_global::<Runtime>().and_then(|rt| rt.place);
        super::main_window::hide(place, cx);
    }
}

/// Hides the game that is played and brings the manager back in its place.
pub fn show_grid(cx: &mut App) {
    let place = follow_shown(cx);
    let mut previous = None;
    let mut pending = Vec::new();
    update_model(cx, |m, rt, _| {
        previous = m.playing.take();
        pending = rt.reveal.take().map(|r| r.hide).unwrap_or_default();
    });
    if let Some(backend) = backend(cx) {
        for key in pending.iter().filter(|k| previous.as_ref() != Some(*k)) {
            backend.send(key, ToMod::Hide);
        }
    }
    if let Some(previous) = &previous {
        drive::note(format!("grid {previous}"));
        if let Some(backend) = backend(cx) {
            backend.send(previous, ToMod::Hide);
        }
    }
    super::main_window::show(place, cx);
    spread(cx);
}

/// The place where the manager window is now, kept on a live monitor; the last place while it is minimized.
fn manager_place(cx: &mut App) -> Option<Place> {
    let before = cx.try_global::<Runtime>().and_then(|rt| rt.place);
    let works = game_window::work_areas();
    let place = match super::main_window::place(cx) {
        Some((frame, mode)) => place::follow(before, frame, mode, &works),
        None => before?,
    };
    Some(Place { frame: place::clamp(place.frame, &works), ..place })
}

/// The place after the game that is shown, wherever the player moved, resized, maximized or fullscreened it.
fn follow_shown(cx: &mut App) -> Option<Place> {
    let playing = model::get(cx).read(cx).playing.clone();
    let works = game_window::work_areas();
    if !cx.has_global::<Runtime>() {
        return None;
    }
    let rt = cx.global_mut::<Runtime>();
    let pid = playing.as_ref().and_then(|k| rt.pids.get(k)).filter(|_| !drive::on());
    if let Some(seen) = pid.and_then(|pid| game_window::seen(*pid)).filter(|s| s.visible) {
        if let Some(key) = playing.filter(|_| seen.mode.is_normal()) {
            rt.insets.insert(key, seen.insets);
        }
        rt.place = Some(place::follow(rt.place, seen.frame, seen.mode, &works));
    }
    if let Some(p) = &mut rt.place {
        p.frame = place::clamp(p.frame, &works);
    }
    rt.place
}

/// Moves the hidden games to the place once per change, so that a swap only shows and hides.
fn spread(cx: &mut App) {
    if drive::on() {
        return;
    }
    let playing = model::get(cx).read(cx).playing.clone();
    if !cx.has_global::<Runtime>() {
        return;
    }
    let rt = cx.global_mut::<Runtime>();
    let Some(place) = rt.place.filter(|p| rt.spread != Some(p.frame)) else { return };
    rt.spread = Some(place.frame);
    for (key, pid) in &rt.pids {
        if playing.as_ref() != Some(key) {
            game_window::place_hidden(*pid, place.frame);
        }
    }
}

/// Puts a game that came up hidden at the place as soon as its window exists, so that showing it later does not resize it.
fn place_new(key: &str, cx: &mut App) {
    let Some(pid) = cx.try_global::<Runtime>().and_then(|rt| rt.pids.get(key).copied()) else { return };
    let Some(seen) = game_window::seen(pid).filter(|_| !drive::on()) else { return };
    let place = match cx.global::<Runtime>().place {
        Some(place) => Some(place),
        None => manager_place(cx),
    };
    let playing = model::get(cx).read(cx).playing.clone();
    let rt = cx.global_mut::<Runtime>();
    rt.unplaced.remove(key);
    if seen.mode.is_normal() {
        rt.insets.insert(key.to_owned(), seen.insets);
    }
    if let Some(place) = place.filter(|_| playing.as_deref() != Some(key)) {
        game_window::place_hidden(pid, place.frame);
    }
}

fn step(action: HotkeyAction, cx: &mut App) {
    if action == HotkeyAction::Grid {
        if model::get(cx).read(cx).playing.is_some() {
            show_grid(cx);
        }
        return;
    }
    let model = model::get(cx);
    let target = {
        let m = model.read(cx);
        let order: Vec<&Game> = m.sorted(None).into_iter().filter(|i| playable(&i.phase)).collect();
        let at = m.playing.as_ref().and_then(|p| order.iter().position(|i| i.key == *p));
        let n = order.len();
        let pick = |from: usize, forward: bool, want: &dyn Fn(&Game) -> bool| -> Option<String> {
            (1..=n).map(|d| if forward { (from + d) % n } else { (from + n - d % n) % n }).map(|ix| order[ix]).find(|i| want(i)).map(|i| i.key.clone())
        };
        match (action, at) {
            (_, _) if n == 0 => None,
            (HotkeyAction::Next, Some(at)) => pick(at, true, &|_| true),
            (HotkeyAction::Prev, Some(at)) => pick(at, false, &|_| true),
            (HotkeyAction::NextAttention, Some(at)) => pick(at, true, &|i| i.phase.needs_attention()),
            (HotkeyAction::NextAttention, None) => order.iter().find(|i| i.phase.needs_attention()).map(|i| i.key.clone()),
            (_, None) => Some(order[0].key.clone()),
            _ => None,
        }
    };
    if let Some(target) = target {
        play(&target, cx);
    }
}

/// Tells the runtime which tiles are on screen, so their previews update at full rate.
pub fn set_visible(keys: HashSet<String>, cx: &mut App) {
    let Some(rt) = cx.try_global::<Runtime>() else { return };
    if rt.visible == keys {
        return;
    }
    let appeared: Vec<String> = keys.iter().filter(|k| !rt.visible.contains(*k) && rt.stale.contains_key(*k)).cloned().collect();
    *lock(&rt.shared_visible) = keys.clone();
    let model = model::get(cx);
    let paths: Vec<(String, PathBuf)> = appeared.iter().filter_map(|k| preview_path(model.read(cx), k).map(|p| (k.clone(), p))).collect();
    let rt = cx.global_mut::<Runtime>();
    rt.visible = keys;
    for (key, path) in paths {
        rt.stale.remove(&key);
        rt.read_at.insert(key.clone(), Instant::now());
        rt.backend.read_preview(key, path);
    }
}

fn preview_path(m: &Model, key: &str) -> Option<PathBuf> {
    let i = m.games.get(key)?;
    Some(fleet::preview_path(m.instance(&i.instance_id)?, i.account_id))
}

enum Deferred {
    Reconnect { key: String, delay: Duration, generation: u64 },
    Restart { key: String, delay: Duration, generation: u64 },
    Join { key: String, generation: u64, lines: Vec<String>, delay: Duration },
    Start { keys: Vec<String> },
    StartWaiting { key: String, delay: Duration },
}

#[derive(Default)]
struct Effects {
    later: Vec<Deferred>,
    hotkeys: Vec<HotkeyAction>,
    exited_playing: bool,
    plays: Vec<(String, Option<String>)>,
    retired: Vec<Arc<RenderImage>>,
    changed_history: Vec<String>,
    stop: Vec<String>,
    stills: Vec<(String, Arc<RenderImage>, bool)>,
    check_proxy: Vec<(String, String)>,
    whispers: Vec<Whisper>,
    phone: Vec<(String, Incident)>,
    placed: Vec<String>,
    game_focused: bool,
    revealed: Option<Reveal>,
    reopen: bool,
    suspect_offline: bool,
    network_back: bool,
}

fn apply_all(updates: Vec<Update>, cx: &mut App) {
    if updates.iter().all(|u| matches!(u, Update::Preview { .. })) {
        return apply_previews(updates, cx);
    }
    let mut fresh: Vec<(String, String, Alert)> = Vec::new();
    let mut fx = Effects::default();
    change_model(redraw(&updates), cx, |m, rt, _| {
        for update in updates {
            let key = match &update {
                Update::Preparing { .. } | Update::Prepared { .. } | Update::SignIn { .. } | Update::Face { .. } | Update::Reach { .. } => None,
                Update::Launched { key, .. } | Update::Failed { key, .. } | Update::Exited { key, .. } | Update::Preview { key, .. } | Update::Seed { key, .. } | Update::Still { key, .. } | Update::ProxyChecked { key, .. } => Some(key.clone()),
                Update::Ipc(Event::Connected { instance, .. } | Event::Message { instance, .. } | Event::Disconnected { instance }) => Some(instance.clone()),
                Update::Ipc(Event::Reopen | Event::Play { .. }) => None,
            };
            let before = key.as_ref().and_then(|k| m.games.get(k)).map(|i| (rank(&i.phase), i.phase.needs_attention()));
            let first_state = matches!(&update, Update::Ipc(Event::Message { message: FromMod::State { .. }, .. })) && key.as_ref().is_some_and(|k| rt.adopted.contains(k));
            let seeded = matches!(update, Update::Seed { .. }) || first_state;
            apply(m, rt, update, &mut fx);
            let Some(key) = key else { continue };
            let Some(i) = m.games.get(&key) else { continue };
            let after = (rank(&i.phase), i.phase.needs_attention());
            if before.is_some_and(|b| b.0 != after.0) {
                m.order_seq += 1;
            }
            if after.1 && !seeded && before.is_some_and(|b| !b.1) {
                fresh.push((i.batch_id.clone(), i.name.clone(), Alert::of(&i.phase)));
                if let Some(what) = phone_what(&i.phase) {
                    fx.phone.push((i.batch_id.clone(), Incident::new(&key, &i.name, what, phase_reason(&i.phase), i.phase.server())));
                }
            }
        }
    });
    for image in fx.retired {
        cx.drop_image(image, None);
    }
    for (key, image, last) in fx.stills {
        super::stills::save(&key, &image, last, cx);
    }
    save_histories(&fx.changed_history, cx);
    if !fx.stop.is_empty()
        && let Some(backend) = backend(cx)
    {
        backend.stop(fx.stop);
    }
    if let Some(backend) = backend(cx) {
        for (key, url) in fx.check_proxy {
            backend.check_proxy(key, url);
        }
    }
    if fx.suspect_offline {
        online::suspect();
    }
    if fx.network_back {
        network_back(cx);
    }
    for deferred in fx.later {
        match deferred {
            Deferred::Reconnect { key, delay, generation } => schedule_reconnect(key, delay, generation, cx),
            Deferred::Restart { key, delay, generation } => schedule_restart(key, delay, generation, cx),
            Deferred::Join { key, generation, lines, delay } => schedule_join(key, generation, lines, delay, cx),
            Deferred::Start { keys } => start_keys(&keys, cx),
            Deferred::StartWaiting { key, delay } => start_waiting(key, delay, cx),
        }
    }
    for (batch, name, kind) in fresh {
        queue_alert(batch, name, kind, cx);
    }
    for whisper in fx.whispers {
        queue_whisper(whisper, cx);
    }
    queue_phone(fx.phone, cx);
    for key in fx.placed {
        place_new(&key, cx);
    }
    if let Some(r) = fx.revealed {
        reveal(r, cx);
    }
    if fx.game_focused {
        let place = follow_shown(cx);
        super::main_window::hide(place, cx);
    }
    if fx.exited_playing || fx.reopen {
        show_grid(cx);
    }
    for (instance, account) in fx.plays {
        request_play(instance, account, cx);
    }
    show_ready(cx);
    for action in fx.hotkeys {
        step(action, cx);
    }
}

fn save_histories(keys: &[String], cx: &mut App) {
    if keys.is_empty() || !cx.has_global::<Runtime>() {
        return;
    }
    let model = model::get(cx);
    let m = model.read(cx);
    let rt = cx.global::<Runtime>();
    let Some(writer) = &rt.history else { return };
    for key in keys {
        let Some(i) = m.games.get(key) else { continue };
        let (Some(instance), Some(log)) = (m.instance(&i.instance_id), m.history.get(key)) else { continue };
        writer.save(history::path(instance, i.account_id), log.clone());
    }
}

fn apply_previews(updates: Vec<Update>, cx: &mut App) {
    let model = model::get(cx);
    let mut shown = Vec::new();
    let mut retired = Vec::new();
    model.update(cx, |m, _| {
        for update in updates {
            let Update::Preview { key, seq, image } = update else { continue };
            match m.games.get_mut(&key) {
                Some(i) if i.phase.is_running() => {
                    if let Some(old) = i.preview.replace(Arc::clone(&image)) {
                        retired.push(old);
                    }
                    i.preview_seq = seq;
                    i.live = true;
                    shown.push((key, image));
                }
                _ => retired.push(image),
            }
        }
    });
    for (key, image) in &shown {
        super::stills::save(key, image, false, cx);
    }
    super::tile::show_previews(shown, cx);
    for image in retired {
        cx.drop_image(image, None);
    }
}

fn note(m: &mut Model, fx: &mut Effects, key: &str, kind: Kind, server: Option<String>, reason: &str) {
    history::push(m.history.entry(key.to_owned()).or_default(), history::Entry::now(kind, server, reason));
    if !fx.changed_history.iter().any(|k| k == key) {
        fx.changed_history.push(key.to_owned());
    }
}

fn apply(m: &mut Model, rt: &mut Runtime, update: Update, fx: &mut Effects) {
    match update {
        Update::Preparing { batch, percent } => {
            m.preparing.insert(batch, percent);
        }
        Update::Prepared { batch } => {
            m.preparing.remove(&batch);
        }
        Update::Launched { key, plain } => {
            if let Some(i) = m.games.get_mut(&key)
                && matches!(i.phase, Phase::Starting { .. } | Phase::Failed { .. })
                && !i.stopping
            {
                i.phase = if plain { Phase::Plain } else { Phase::Starting { place: 0, total: 0 } };
            }
        }
        Update::Failed { key, reason, login, offline } => {
            if let Some(i) = m.games.get_mut(&key) {
                m.preparing.remove(&i.batch_id);
                i.phase = if login {
                    Phase::LoginNeeded
                } else if offline {
                    Phase::NoNetwork
                } else {
                    Phase::Failed { reason }
                };
            }
            if offline && !login {
                fx.later.push(Deferred::StartWaiting { key, delay: waiting_retry() });
            }
            fx.suspect_offline |= offline;
        }
        Update::Reach { online } => on_reach(m, rt, fx, online),
        Update::Seed { key, phase, attempts } => {
            if let Some(i) = m.games.get_mut(&key) {
                i.phase = phase;
                i.reconnect_attempts = attempts;
            }
        }
        Update::SignIn { account, valid } => {
            for i in m.games.values_mut().filter(|i| i.account_id == account) {
                if !valid && matches!(i.phase, Phase::Stopped | Phase::Crashed { .. } | Phase::Failed { .. }) {
                    i.phase = Phase::LoginNeeded;
                } else if valid && i.phase == Phase::LoginNeeded && !rt.pids.contains_key(&i.key) {
                    i.phase = Phase::Stopped;
                }
            }
            m.order_seq += 1;
        }
        Update::Exited { key, code } => on_exit(m, rt, fx, key, code),
        Update::Preview { key, seq, image } => match m.games.get_mut(&key) {
            Some(i) if i.phase.is_running() => {
                fx.stills.push((key, Arc::clone(&image), false));
                if let Some(old) = i.preview.replace(image) {
                    fx.retired.push(old);
                }
                i.preview_seq = seq;
                i.live = true;
            }
            _ => fx.retired.push(image),
        },
        Update::Still { key, image } => match m.games.get_mut(&key) {
            Some(i) if i.preview.is_none() && !i.live => i.preview = Some(image),
            _ => fx.retired.push(image),
        },
        Update::Face { account, image } => {
            if m.account(account).is_none() {
                fx.retired.push(image);
            } else if let Some(old) = m.faces.insert(account, image) {
                fx.retired.push(old);
            }
        }
        Update::ProxyChecked { key, fault } => {
            if m.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Connecting { .. } | Phase::Disconnected { .. })) {
                model::set_hint(&key, |h| h.proxy = fault);
            }
        }
        Update::Ipc(Event::Connected { instance, pid, .. }) => {
            model::set_hint(&instance, |h| h.link_lost = false);
            rt.pids.insert(instance.clone(), pid);
            if cfg!(target_os = "windows") && !drive::on() {
                rt.unplaced.insert(instance.clone());
            }
            for message in game_identity(m, &instance) {
                rt.backend.send(&instance, message);
            }
            tint(m, &instance, pid);
            if rt.adopted.contains(&instance) && m.playing.as_deref() != Some(instance.as_str()) {
                rt.backend.send(&instance, ToMod::Hide);
            }
            if let Some(i) = m.games.get_mut(&instance)
                && matches!(i.phase, Phase::Starting { .. })
            {
                i.phase = Phase::Menu;
            }
        }
        Update::Ipc(Event::Reopen) => fx.reopen = true,
        Update::Ipc(Event::Play { instance, account }) => fx.plays.push((instance, account)),
        Update::Ipc(Event::Disconnected { instance }) => {
            let alive = rt.pids.contains_key(&instance) && m.games.get(&instance).is_some_and(|i| !i.stopping && i.phase != Phase::Stopped);
            if alive {
                model::set_hint(&instance, |h| h.link_lost = true);
            }
        }
        Update::Ipc(Event::Message { instance, message }) => {
            model::set_hint(&instance, |h| h.link_lost = false);
            on_message(m, rt, instance, message, fx);
        }
    }
}

fn on_exit(m: &mut Model, rt: &mut Runtime, fx: &mut Effects, key: String, code: Option<i32>) {
    model::set_hint(&key, |h| *h = model::Hint::default());
    rt.pids.remove(&key);
    rt.unplaced.remove(&key);
    rt.insets.remove(&key);
    rt.joined.remove(&key);
    rt.adopted.remove(&key);
    let unknown = rt.foreign.remove(&key) && code.is_none();
    let ran = rt.started.remove(&key).map(|t| t.elapsed());
    let Some(i) = m.games.get_mut(&key) else {
        let keys = drop_restart(rt, &key);
        if !keys.is_empty() {
            fx.later.push(Deferred::Start { keys });
        }
        return;
    };
    *rt.joins.entry(key.clone()).or_default() += 1;
    let crashed = !(i.stopping || code == Some(0) || unknown || matches!(i.phase, Phase::Stopped | Phase::LoginNeeded | Phase::Restarting { .. }));
    let relaunch = !i.stopping && matches!(i.phase, Phase::Restarting { .. });
    let restart = if i.stopping { rt.restarts.remove(&key) } else { None };
    i.stopping = false;
    if i.live
        && let Some(image) = &i.preview
    {
        fx.stills.push((key.clone(), Arc::clone(image), true));
    }
    i.live = false;
    if m.playing.as_deref() == Some(key.as_str()) {
        fx.exited_playing = true;
    }
    if relaunch {
        let generation = rt.reconnects.get(&key).copied().unwrap_or_default();
        fx.later.push(Deferred::Restart { key, delay: Duration::ZERO, generation });
        return;
    }
    if !crashed {
        if !matches!(i.phase, Phase::LoginNeeded | Phase::Restarting { .. }) {
            i.phase = Phase::Stopped;
        }
        if let Some(group) = restart {
            rt.restart_ready.entry(group).or_default().push(key);
            if let Some(keys) = restart_done(rt, group) {
                fx.later.push(Deferred::Start { keys });
            }
        }
        return;
    }
    let server = server_of(&i.phase);
    let policy = &settings::active().crash_restart;
    if ran.is_some_and(|r| r >= STABLE) {
        rt.crash_restarts.remove(&key);
    }
    let tried = rt.crash_restarts.get(&key).copied().unwrap_or(0);
    let reason = tr("Unerwartet beendet");
    let banned = matches!(i.phase, Phase::Banned { .. });
    match reconnect::restart(tried, policy, reconnect::unit()) {
        Decision::Retry(_) if banned => i.phase = Phase::Crashed { code },
        Decision::Retry(delay) => {
            rt.crash_restarts.insert(key.clone(), tried + 1);
            let generation = rt.reconnects.entry(key.clone()).or_default();
            *generation += 1;
            fx.later.push(Deferred::Restart { key: key.clone(), delay, generation: *generation });
            i.phase = Phase::Restarting { attempt: tried + 1, of: policy.max_attempts };
        }
        Decision::Stop => i.phase = Phase::Crashed { code },
    }
    note(m, fx, &key, Kind::Crashed, server, &reason);
}

fn join_lines_of(m: &Model, key: &str) -> Option<(Vec<String>, Duration)> {
    let batch = m.batch(&m.games.get(key)?.batch_id)?;
    let lines = batch.join_lines();
    (!lines.is_empty()).then(|| (lines, Duration::from_millis(u64::from(batch.join_delay_ms))))
}

fn on_state(m: &mut Model, rt: &mut Runtime, fx: &mut Effects, key: String, kind: StateKind, server: Option<String>, reason: Option<String>, cause: Option<DisconnectCause>) {
    if rt.adopted.remove(&key) {
        return seed_state(m, rt, &key, kind, server, reason, cause);
    }
    if kind != StateKind::Playing {
        *rt.joins.entry(key.clone()).or_default() += 1;
    }
    let account = m.games.get(&key).and_then(|i| m.account(i.account_id));
    let offline = account.is_some_and(|a| a.kind == AccountKind::Offline);
    let direct = account.is_none_or(|a| a.proxy.is_none());
    let nearby = server.as_deref().is_some_and(local_server);
    let cut_off = m.offline && direct && !nearby;
    if matches!(kind, StateKind::Menu | StateKind::Playing) {
        model::set_hint(&key, |h| h.proxy = None);
    }
    if kind == StateKind::Disconnected
        && matches!(cause, None | Some(DisconnectCause::ServerDown | DisconnectCause::Auth | DisconnectCause::Unknown))
        && let Some(url) = m.games.get(&key).and_then(|i| m.account(i.account_id)).and_then(|a| a.proxy.clone())
    {
        fx.check_proxy.push((key.clone(), url));
    }
    let join = if kind == StateKind::Playing { join_lines_of(m, &key) } else { None };
    let Some(i) = m.games.get_mut(&key) else { return };
    if i.stopping || i.phase == Phase::Stopped {
        return;
    }
    let policy = &settings::active().reconnect;
    let server_text = server.clone().unwrap_or_default();
    let reason_text = reason.clone().unwrap_or_default();
    let mut event: Option<(Kind, Option<String>, String)> = None;
    i.phase = match kind {
        StateKind::Menu => {
            rt.joined.remove(&key);
            Phase::Menu
        }
        StateKind::Connecting => Phase::Connecting { server: server_text },
        StateKind::Queue => Phase::Queue { server: server_text },
        StateKind::Playing => {
            if let Phase::Playing { since, .. } = i.phase {
                Phase::Playing { server: server_text, since }
            } else {
                rt.started.entry(key.clone()).or_insert_with(Instant::now);
                if !matches!(i.phase, Phase::Dead { .. }) && !server_text.is_empty() && rt.joined.insert(key.clone()) {
                    event = Some((Kind::Connected, server.clone(), String::new()));
                    fx.phone.push((i.batch_id.clone(), Incident::new(&key, &i.name, What::Back, "", server.as_deref())));
                    if let Some((lines, delay)) = join {
                        let generation = *rt.joins.entry(key.clone()).or_default();
                        fx.later.push(Deferred::Join { key: key.clone(), generation, lines, delay });
                    }
                }
                Phase::Playing { server: server_text, since: Instant::now() }
            }
        }
        StateKind::Dead => Phase::Dead { server, reason: reason_text },
        StateKind::Disconnected => {
            rt.joined.remove(&key);
            if let Phase::Playing { since, .. } = i.phase
                && since.elapsed() >= STABLE
            {
                i.reconnect_attempts = 0;
            }
            let cause = cause.unwrap_or(DisconnectCause::Unknown);
            let generation = rt.reconnects.entry(key.clone()).or_default();
            *generation += 1;
            fx.suspect_offline |= direct && !nearby && matches!(cause, DisconnectCause::ServerDown | DisconnectCause::Unknown);
            let reaction = match reconnect::react(cause, i.reconnect_attempts, policy, reconnect::unit(), cut_off) {
                Reaction::Retry(_) | Reaction::WaitForNetwork if server.is_none() => Reaction::GiveUp,
                Reaction::SignInAgain if offline => Reaction::NotAllowed,
                other => other,
            };
            let kind = match cause {
                DisconnectCause::Banned => Kind::Banned,
                DisconnectCause::Kicked => Kind::Kicked,
                _ => Kind::Disconnected,
            };
            event = Some((kind, server.clone(), reason_text.clone()));
            match reaction {
                Reaction::Retry(delay) => {
                    let what = if cause == DisconnectCause::Kicked { What::Kicked } else { What::Dropped };
                    let said = if reason_text.trim().is_empty() { cause_text(cause) } else { reason_text.clone() };
                    fx.phone.push((i.batch_id.clone(), Incident::new(&key, &i.name, what, &said, server.as_deref())));
                    i.reconnect_attempts += 1;
                    fx.later.push(Deferred::Reconnect { key: key.clone(), delay, generation: *generation });
                    Phase::Connecting { server: server_text }
                }
                Reaction::WaitForNetwork => {
                    fx.later.push(Deferred::Reconnect { key: key.clone(), delay: waiting_retry(), generation: *generation });
                    Phase::NetworkLost { server: server_text }
                }
                Reaction::GiveUp | Reaction::NotAllowed => Phase::Disconnected { server, reason: reason_text },
                Reaction::Banned => Phase::Banned { server, reason: reason_text },
                Reaction::SignInAgain => {
                    fx.stop.push(key.clone());
                    let crash = &settings::active().crash_restart;
                    let tried = rt.crash_restarts.get(&key).copied().unwrap_or(0);
                    match reconnect::restart(tried, crash, 0.0) {
                        Decision::Retry(_) => {
                            rt.crash_restarts.insert(key.clone(), tried + 1);
                            Phase::Restarting { attempt: tried + 1, of: crash.max_attempts }
                        }
                        Decision::Stop => Phase::LoginNeeded,
                    }
                }
            }
        }
    };
    if let Some((kind, server, reason)) = event {
        note(m, fx, &key, kind, server, &reason);
    }
}

fn on_reach(m: &mut Model, rt: &mut Runtime, fx: &mut Effects, online: bool) {
    m.offline = !online;
    let (accounts, mut nth, mut start) = (&m.accounts, 0, Vec::new());
    for i in m.games.values_mut().filter(|i| !i.stopping) {
        let direct = accounts.iter().find(|a| a.id == i.account_id).is_none_or(|a| a.proxy.is_none());
        match &i.phase {
            Phase::Connecting { server } if !online && direct && i.reconnect_attempts > 0 && !local_server(server) => {
                let generation = rt.reconnects.entry(i.key.clone()).or_default();
                *generation += 1;
                fx.later.push(Deferred::Reconnect { key: i.key.clone(), delay: waiting_retry(), generation: *generation });
                i.reconnect_attempts -= 1;
                i.phase = Phase::NetworkLost { server: server.clone() };
            }
            Phase::NetworkLost { server } if online => {
                let generation = rt.reconnects.entry(i.key.clone()).or_default();
                *generation += 1;
                let delay = BACK_STAGGER * nth + reconnect::wait(&[BACK_WAIT_SECS], 0, 50, reconnect::unit());
                nth += 1;
                fx.later.push(Deferred::Reconnect { key: i.key.clone(), delay, generation: *generation });
                i.phase = Phase::Connecting { server: server.clone() };
            }
            Phase::NoNetwork if online => start.push(i.key.clone()),
            _ => {}
        }
    }
    if !start.is_empty() {
        fx.later.push(Deferred::Start { keys: start });
    }
    fx.network_back |= online;
    m.order_seq += 1;
}

fn waiting_retry() -> Duration {
    reconnect::wait(&[WAITING_RETRY_SECS], 0, 20, reconnect::unit())
}

fn local_server(server: &str) -> bool {
    let host = match server.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() && (!host.contains(':') || host.ends_with(']')) => host,
        _ => server,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || ip.segments()[0] & 0xfe00 == 0xfc00 || ip.segments()[0] & 0xffc0 == 0xfe80,
        Err(_) => host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".local"),
    }
}

fn network_back(cx: &mut App) {
    super::remote::network_back(cx);
}

fn seed_state(m: &mut Model, rt: &mut Runtime, key: &str, kind: StateKind, server: Option<String>, reason: Option<String>, cause: Option<DisconnectCause>) {
    let Some(i) = m.games.get_mut(key) else { return };
    if i.stopping || !i.phase.is_running() {
        return;
    }
    if matches!(kind, StateKind::Menu | StateKind::Disconnected) {
        rt.joined.remove(key);
    }
    let (server_text, reason) = (server.clone().unwrap_or_default(), reason.unwrap_or_default());
    i.phase = match kind {
        StateKind::Menu => Phase::Menu,
        StateKind::Connecting => Phase::Connecting { server: server_text },
        StateKind::Queue => Phase::Queue { server: server_text },
        StateKind::Playing => Phase::Playing { server: server_text, since: Instant::now() },
        StateKind::Dead => Phase::Dead { server, reason },
        StateKind::Disconnected if cause == Some(DisconnectCause::Banned) => Phase::Banned { server, reason },
        StateKind::Disconnected => Phase::Disconnected { server, reason },
    };
}

fn adopt_games(m: &mut Model, rt: &mut Runtime, adopted: Vec<fleet::Adoption>) {
    let (mut placed, mut dropped) = (Vec::new(), Vec::new());
    for a in adopted {
        if a.batch.is_none() && !m.games.contains_key(&a.key) {
            let Some(name) = m.account(a.account).map(|account| account.name.clone()) else {
                dropped.push(a.key);
                continue;
            };
            m.games.insert(a.key.clone(), Game {
                key: a.key.clone(),
                batch_id: fleet::solo_owner(&a.instance),
                instance_id: a.instance.clone(),
                account_id: a.account,
                name,
                phase: Phase::Stopped,
                reconnect_attempts: 0,
                unread_whispers: 0,
                preview: None,
                preview_seq: 0,
                live: false,
                stopping: false,
            });
        }
        let Some(i) = m.games.get_mut(&a.key) else {
            dropped.push(a.key);
            continue;
        };
        i.phase = Phase::Menu;
        rt.pids.insert(a.key.clone(), a.pid);
        rt.joined.insert(a.key.clone());
        rt.started.insert(a.key.clone(), Instant::now());
        rt.adopted.insert(a.key.clone());
        rt.foreign.insert(a.key.clone());
        if cfg!(target_os = "windows") && !drive::on() {
            rt.unplaced.insert(a.key.clone());
        }
        model::set_hint(&a.key, |h| h.link_lost = true);
        placed.push(a.key);
    }
    rt.backend.settle(&placed, &dropped);
    m.order_seq += 1;
}

fn on_message(m: &mut Model, rt: &mut Runtime, key: String, message: FromMod, fx: &mut Effects) {
    let playing = m.playing.as_deref() == Some(key.as_str());
    if rt.unplaced.contains(&key) {
        fx.placed.push(key.clone());
    }
    match message {
        FromMod::State { kind, server, reason, cause } => on_state(m, rt, fx, key, kind, server, reason, cause),
        FromMod::Chat { kind, from, text } => {
            let whisper = kind == ChatKind::Whisper;
            if whisper
                && !playing
                && settings::active().merged_chat
                && let Some(i) = m.games.get_mut(&key)
            {
                i.unread_whispers += 1;
            }
            let (account, batch) = m.games.get(&key).map(|i| (i.name.clone(), i.batch_id.clone())).unwrap_or_default();
            if whisper && !playing {
                fx.whispers.push(Whisper { account: account.clone(), from: from.clone(), text: text.clone() });
            }
            if settings::active().merged_chat {
                let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                let server = m.games.get(&key).and_then(|i| i.phase.server().map(str::to_owned));
                let shared = !whisper && !(from.is_none() && text.contains(&account));
                m.receive_chat(ChatLine { instance: key, account, batch, also: Vec::new(), at, whisper, from, text }, server.as_deref(), shared, Instant::now(), KEPT_CHAT);
            }
        }
        FromMod::Preview { .. } => {
            let now = Instant::now();
            let recent = rt.read_at.get(&key).is_some_and(|t| now.duration_since(*t) < OFFSCREEN_PREVIEW);
            if rt.visible.contains(&key) || !recent {
                if let Some(path) = preview_path(m, &key) {
                    rt.stale.remove(&key);
                    rt.read_at.insert(key.clone(), now);
                    rt.backend.read_preview(key, path);
                }
            } else {
                rt.stale.insert(key, now);
            }
        }
        FromMod::Hotkey { action } => {
            if playing || rt.reveal.as_ref().is_some_and(|r| r.hide.contains(&key)) {
                fx.hotkeys.push(action);
            } else if action == HotkeyAction::Grid {
                rt.backend.send(&key, ToMod::Hide);
            }
        }
        FromMod::Window { visible: true, focused } => {
            if let Some(pid) = rt.pids.get(&key) {
                tint(m, &key, *pid);
            }
            fx.game_focused |= playing && focused;
            if rt.reveal.as_ref().is_some_and(|r| r.key == key) {
                fx.revealed = rt.reveal.take();
            }
        }
        FromMod::Window { .. } | FromMod::Hello { .. } | FromMod::Options { .. } | FromMod::Servers => {}
    }
}

fn game_identity(m: &Model, key: &str) -> Vec<ToMod> {
    let Some(i) = m.games.get(key) else { return Vec::new() };
    let (Some(account), Some(target)) = (m.accounts.iter().find(|a| a.id == i.account_id), m.target(&i.batch_id)) else { return Vec::new() };
    fleet::identity_messages(&target, account, settings::active().game_badge)
}

fn tint(m: &Model, key: &str, pid: u32) {
    if let Some(account) = m.games.get(key).and_then(|i| m.accounts.iter().find(|a| a.id == i.account_id)) {
        crate::core::game_window::tint(pid, account);
    }
}

/// Keeps the computer awake while a game runs or waits to start again, as far as the settings allow.
pub fn keep_awake(cx: &App) {
    let busy = model::get(cx).read(cx).games.values().any(|i| i.phase.is_active());
    awake::set(awake::wanted(settings::active().prevent_sleep, busy));
}

/// Tells every connected game its identity, badge and background level again, after batches, accounts or settings changed.
pub fn tell_games(cx: &mut App) {
    let Some(rt) = cx.try_global::<Runtime>() else { return };
    let m = model::get(cx).read(cx);
    for key in rt.pids.keys() {
        for message in game_identity(m, key) {
            rt.backend.send(key, message);
        }
    }
}

fn start_waiting(key: String, delay: Duration, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        cx.update(|cx| {
            if model::get(cx).read(cx).games.get(&key).is_some_and(|i| i.phase == Phase::NoNetwork && !i.stopping) {
                start_keys(std::slice::from_ref(&key), cx);
            }
        });
    })
    .detach();
}

fn schedule_reconnect(key: String, delay: Duration, generation: u64, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        cx.update(|cx| {
            let Some(rt) = cx.try_global::<Runtime>() else { return };
            if rt.reconnects.get(&key) != Some(&generation) {
                return;
            }
            let model = model::get(cx);
            if model.read(cx).games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Connecting { .. } | Phase::NetworkLost { .. }) && !i.stopping) {
                rt.backend.send(&key, ToMod::Reconnect);
            }
        });
    })
    .detach();
}

fn schedule_restart(key: String, delay: Duration, generation: u64, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        cx.update(|cx| {
            let Some(rt) = cx.try_global::<Runtime>() else { return };
            if rt.reconnects.get(&key) != Some(&generation) {
                return;
            }
            let model = model::get(cx);
            let batch = {
                let m = model.read(cx);
                m.games.get(&key).filter(|i| matches!(i.phase, Phase::Restarting { .. }) && !i.stopping).map(|i| i.batch_id.clone())
            };
            if let Some(batch) = batch {
                launch(&batch, Some(std::slice::from_ref(&key)), true, cx);
            }
        });
    })
    .detach();
}

fn schedule_join(key: String, generation: u64, lines: Vec<String>, delay: Duration, cx: &mut App) {
    cx.spawn(async move |cx| {
        for line in lines {
            cx.background_executor().timer(delay).await;
            let sent = cx.update(|cx| {
                let Some(rt) = cx.try_global::<Runtime>() else { return false };
                if rt.joins.get(&key) != Some(&generation) {
                    return false;
                }
                let model = model::get(cx);
                if !model.read(cx).games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Playing { .. }) && !i.stopping) {
                    return false;
                }
                rt.backend.send(&key, ToMod::ChatSend { text: line });
                true
            });
            if !sent {
                return;
            }
        }
    })
    .detach();
}

fn queue_alert(batch: String, name: String, kind: Alert, cx: &mut App) {
    if !cx.has_global::<Runtime>() {
        return;
    }
    let rt = cx.global_mut::<Runtime>();
    let list = rt.alerts.entry(batch.clone()).or_default();
    let first = list.is_empty();
    list.push((name, kind));
    if !first {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(ALERT_AFTER).await;
        cx.update(|cx| flush_alert(&batch, cx));
    })
    .detach();
}

fn flush_alert(batch_id: &str, cx: &mut App) {
    if !cx.has_global::<Runtime>() {
        return;
    }
    if !settings::active().notifications.attention {
        cx.global_mut::<Runtime>().alerts.remove(batch_id);
        return;
    }
    let Some(list) = cx.global_mut::<Runtime>().alerts.remove(batch_id) else { return };
    let model = model::get(cx);
    let (names, kinds, batch_name) = {
        let m = model.read(cx);
        let open: Vec<&(String, Alert)> = list.iter().filter(|(name, _)| m.games.values().any(|i| i.batch_id == batch_id && i.name == *name && i.phase.needs_attention())).collect();
        let mut seen = HashSet::new();
        let names: Vec<String> = open.iter().filter(|(n, _)| seen.insert(n.clone())).map(|(n, _)| n.clone()).collect();
        let kinds: HashSet<Alert> = open.iter().map(|(_, k)| *k).collect();
        (names, kinds, m.batch(batch_id).map(|b| b.name.clone()).unwrap_or_default())
    };
    if names.is_empty() || (super::main_window::is_active(cx) && model.read(cx).playing.is_none()) {
        return;
    }
    let n = names.len();
    let what = match (kinds.len(), kinds.iter().next()) {
        (1, Some(Alert::Disconnected)) => trf(if n == 1 { "%lld Account getrennt" } else { "%lld Accounts getrennt" }, &[&n]),
        (1, Some(Alert::Banned)) => trf(if n == 1 { "%lld Account gesperrt" } else { "%lld Accounts gesperrt" }, &[&n]),
        (1, Some(Alert::Crashed)) => trf(if n == 1 { "%lld Account abgestürzt" } else { "%lld Accounts abgestürzt" }, &[&n]),
        (1, Some(Alert::Login)) => trf(if n == 1 { "%lld Account muss sich anmelden" } else { "%lld Accounts müssen sich anmelden" }, &[&n]),
        _ => trf(if n == 1 { "%lld Account braucht dich" } else { "%lld Accounts brauchen dich" }, &[&n]),
    };
    let title = format!("{what} · {batch_name}");
    let mut body = names.iter().take(ALERT_NAMES).cloned().collect::<Vec<_>>().join(", ");
    if n > ALERT_NAMES {
        body = trf("%@ und %lld weitere", &[&body, &(n - ALERT_NAMES)]);
    }
    if drive::fenced("notify", format!("{title} | {body}")) {
        return;
    }
    cx.show_system_notification(SystemNotification { tag: format!("attention-{batch_id}").into(), title: title.into(), body: body.into(), actions: Vec::new() });
}

/// A whisper that reached an instance nobody is playing.
struct Whisper {
    account: String,
    from: Option<String>,
    text: String,
}

const WHISPER_TAG: &str = "whisper";
const WHISPER_PREVIEW: usize = 90;

fn queue_whisper(whisper: Whisper, cx: &mut App) {
    let wanted = {
        let s = settings::active();
        s.notifications.whispers || (s.phone.enabled && s.phone.whispers)
    };
    if !cx.has_global::<Runtime>() || !wanted {
        return;
    }
    let rt = cx.global_mut::<Runtime>();
    let first = rt.whispers.is_empty();
    rt.whispers.push(whisper);
    if !first {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(ALERT_AFTER).await;
        cx.update(flush_whispers);
    })
    .detach();
}

fn flush_whispers(cx: &mut App) {
    if !cx.has_global::<Runtime>() {
        return;
    }
    let list = std::mem::take(&mut cx.global_mut::<Runtime>().whispers);
    let phone = settings::active().phone.clone();
    if phone.enabled && phone.whispers {
        let lines: Vec<(String, Option<String>, String)> = list.iter().map(|w| (w.account.clone(), w.from.clone(), w.text.clone())).collect();
        if let Some(text) = phone::whispers(&lines) {
            send_phone(text, cx);
        }
    }
    if list.is_empty() || !settings::active().notifications.whispers || (super::main_window::is_active(cx) && model::get(cx).read(cx).playing.is_none()) {
        return;
    }
    let n = list.len();
    let (title, body) = if n == 1 {
        let w = &list[0];
        let mut text: String = w.text.chars().take(WHISPER_PREVIEW).collect();
        if w.text.chars().count() > WHISPER_PREVIEW {
            text.push('…');
        }
        let who = w.from.clone().unwrap_or_else(|| tr("Jemand"));
        (trf("%@ flüstert · %@", &[&who, &w.account]), text)
    } else {
        let mut seen = HashSet::new();
        let names: Vec<&str> = list.iter().filter(|w| seen.insert(w.account.as_str())).map(|w| w.account.as_str()).collect();
        let mut body = names.iter().take(ALERT_NAMES).copied().collect::<Vec<_>>().join(", ");
        if names.len() > ALERT_NAMES {
            body = trf("%@ und %lld weitere", &[&body, &(names.len() - ALERT_NAMES)]);
        }
        (trf("%lld Flüsternachrichten", &[&n]), body)
    };
    if drive::fenced("notify", format!("{title} | {body}")) {
        return;
    }
    cx.show_system_notification(SystemNotification { tag: WHISPER_TAG.into(), title: title.into(), body: body.into(), actions: Vec::new() });
}

fn phone_what(phase: &Phase) -> Option<What> {
    Some(match phase {
        Phase::Disconnected { .. } => What::GaveUp,
        Phase::Banned { .. } => What::Banned,
        Phase::LoginNeeded => What::Login,
        Phase::Crashed { .. } => What::Crashed,
        Phase::Failed { .. } => What::Failed,
        Phase::Dead { .. } => What::Died,
        _ => return None,
    })
}

fn phase_reason(phase: &Phase) -> &str {
    match phase {
        Phase::Disconnected { reason, .. } | Phase::Banned { reason, .. } => reason,
        _ => "",
    }
}

fn cause_text(cause: DisconnectCause) -> String {
    match cause {
        DisconnectCause::ServerDown => tr("Server nicht erreichbar"),
        DisconnectCause::ServerFull => tr("Server voll"),
        _ => String::new(),
    }
}

fn queue_phone(incidents: Vec<(String, Incident)>, cx: &mut App) {
    if incidents.is_empty() || !settings::active().phone.enabled || !cx.has_global::<Runtime>() {
        return;
    }
    let now = Instant::now();
    let rt = cx.global_mut::<Runtime>();
    for (batch, incident) in incidents {
        rt.phone.push(&batch, incident, now);
    }
    if let Some(due) = rt.phone.next_due() {
        flush_phone_at(due, cx);
    }
}

fn flush_phone_at(due: Instant, cx: &mut App) {
    let rt = cx.global_mut::<Runtime>();
    if rt.phone_at.is_some_and(|at| at <= due) {
        return;
    }
    rt.phone_at = Some(due);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(due.saturating_duration_since(Instant::now())).await;
        cx.update(|cx| {
            if cx.has_global::<Runtime>() && cx.global::<Runtime>().phone_at == Some(due) {
                cx.global_mut::<Runtime>().phone_at = None;
            }
            flush_phone(cx);
        });
    })
    .detach();
}

fn flush_phone(cx: &mut App) {
    if !cx.has_global::<Runtime>() {
        return;
    }
    let mut coalescer = std::mem::take(&mut cx.global_mut::<Runtime>().phone);
    let model = model::get(cx);
    let due: Vec<(String, Vec<Incident>)> = {
        let m = model.read(cx);
        let current = |e: &Incident| {
            m.games.get(&e.key).is_some_and(|i| {
                !i.stopping
                    && match e.what {
                        _ if e.healed => i.phase.on_server(),
                        What::Back => i.phase.on_server(),
                        What::Dropped | What::Kicked => !i.phase.on_server() && i.phase.is_active(),
                        _ => i.phase.needs_attention(),
                    }
            })
        };
        let batches = coalescer.take_due(Instant::now(), current);
        batches.into_iter().map(|(id, list)| (m.batch(&id).map(|b| b.name.clone()).unwrap_or_default(), list)).collect()
    };
    let next = coalescer.next_due();
    cx.global_mut::<Runtime>().phone = coalescer;
    if let Some(next) = next {
        flush_phone_at(next, cx);
    }
    let phone = settings::active().phone.clone();
    if !phone.enabled {
        return;
    }
    for (batch, list) in due {
        if let Some(text) = phone::format(&batch, &list, phone.reconnected) {
            send_phone(text, cx);
        }
    }
}

fn send_phone(text: String, cx: &mut App) {
    if !cx.has_global::<Runtime>() {
        return;
    }
    drive::note(format!("phone {}", text.replace('\n', " | ")));
    if cx.global::<Runtime>().outbox.is_none() {
        let (tx, rx) = async_channel::unbounded::<phone::Outcome>();
        cx.global_mut::<Runtime>().outbox = Some(phone::Outbox::start(move |outcome| {
            let _ = tx.send_blocking(outcome);
        }));
        cx.spawn(async move |cx| {
            while let Ok(outcome) = rx.recv().await {
                cx.update(|cx| {
                    let trouble = match outcome {
                        phone::Outcome::Arrived => None,
                        phone::Outcome::Lost(why) => Some(why),
                    };
                    set_phone_trouble(trouble, cx);
                });
            }
        })
        .detach();
    }
    let Some(Err(text)) = cx.global::<Runtime>().outbox.as_ref().map(|o| o.send(text)) else { return };
    cx.global_mut::<Runtime>().outbox = None;
    send_phone(text, cx);
}

/// Sets or clears why the last message to the phone did not arrive, as Settings shows it.
pub fn set_phone_trouble(trouble: Option<String>, cx: &mut App) {
    model::get(cx).update(cx, |m, cx| {
        if m.phone_trouble != trouble {
            m.phone_trouble = trouble;
            cx.notify();
        }
    });
}

fn refresh_badge(cx: &mut App) {
    let count = model::get(cx).read(cx).attention_count(None);
    if !cx.has_global::<Runtime>() {
        return;
    }
    let rt = cx.global_mut::<Runtime>();
    if rt.badge == count {
        return;
    }
    rt.badge = count;
    if drive::fenced("badge", count) {
        return;
    }
    native::set_badge(count);
}

/// The ban that is still the latest event of a stopped instance, as its status line, so a stop does not hide it.
pub fn stopped_ban(i: &Game, m: &Model) -> Option<String> {
    if i.phase != Phase::Stopped || i.stopping {
        return None;
    }
    let last = m.history.get(&i.key)?.back()?;
    (last.kind == Kind::Banned).then(|| format!("{} · {}", tr("Gestoppt"), model::banned_status(&last.reason)))
}

/// The phase of an instance as one line of German for its tile.
pub fn status(i: &Game, preparing: Option<u8>) -> String {
    if i.stopping {
        return tr("Wird gestoppt…");
    }
    let hint = model::hint(&i.key);
    if let Some(fault) = hint.proxy
        && matches!(i.phase, Phase::Connecting { .. } | Phase::Disconnected { .. })
    {
        return model::proxy_status(fault);
    }
    if hint.link_lost && i.phase.is_running() {
        return model::link_status();
    }
    match &i.phase {
        Phase::Stopped => tr("Gestoppt"),
        Phase::Starting { .. } if preparing.is_some() => trf("Lädt Minecraft · %lld %%", &[&preparing.unwrap_or(0)]),
        Phase::Restarting { attempt, of } => model::restart_status(*attempt, *of),
        Phase::Starting { place, total } if *total > 1 && *place > 0 => trf("Wartet auf Start (%lld von %lld)", &[place, total]),
        Phase::Starting { .. } => tr("Startet…"),
        Phase::Plain => tr("Läuft · ohne Vorschau"),
        Phase::Menu => tr("Im Hauptmenü"),
        Phase::Connecting { .. } if i.reconnect_attempts > 0 => trf("Verbindet neu · Versuch %lld von %lld", &[&i.reconnect_attempts, &settings::active().reconnect.max_attempts]),
        Phase::Connecting { server } if server.is_empty() => tr("Verbindet…"),
        Phase::Connecting { server } => trf("Verbindet mit %@…", &[server]),
        Phase::Queue { .. } => tr("In der Warteschlange"),
        Phase::Playing { server, .. } if server.is_empty() => tr("Spielt"),
        Phase::Playing { server, .. } => trf("Auf %@", &[server]),
        Phase::Disconnected { reason, .. } if reason.is_empty() => tr("Getrennt"),
        Phase::Disconnected { reason, .. } => trf("Getrennt · %@", &[reason]),
        Phase::Dead { .. } => tr("Gestorben"),
        Phase::Crashed { .. } => tr("Abgestürzt"),
        Phase::Banned { reason, .. } => model::banned_status(reason),
        Phase::LoginNeeded => tr("Anmeldung nötig"),
        Phase::Failed { reason } => trf("Start fehlgeschlagen · %@", &[reason]),
        Phase::NetworkLost { .. } => tr("Keine Verbindung · verbindet sich wieder, sobald das Internet zurück ist"),
        Phase::NoNetwork => tr("Keine Verbindung · startet, sobald das Internet zurück ist"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::{model, ChatKind, Fault, AccountKind, Account, AuthError, Backend, Batch, Deferred, DisconnectCause, Effects, Event, FromMod, HotkeyAction, Game, Instant, Kind, Model, Phase, Redraw, Reveal, Runtime, StateKind, ToMod, Update, What, adopt_games, apply, fleet, local_server, needs_sign_in, playable, server_of, redraw, render_image, status};

    fn instance(phase: Phase) -> Game {
        Game { key: "k".into(), batch_id: "b".into(), instance_id: "b".into(), account_id: uuid::Uuid::nil(), name: "Alex".into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, live: false, stopping: false }
    }

    #[test]
    fn status_lines_name_the_phase() {
        crate::core::i18n::init("de");
        assert_eq!(status(&instance(Phase::Stopped), None), "Gestoppt");
        assert_eq!(status(&instance(Phase::Starting { place: 12, total: 50 }), None), "Wartet auf Start (12 von 50)");
        assert_eq!(status(&instance(Phase::Starting { place: 0, total: 0 }), None), "Startet…");
        assert_eq!(status(&instance(Phase::Starting { place: 1, total: 4 }), Some(45)), "Lädt Minecraft · 45 %");
        assert_eq!(status(&instance(Phase::Playing { server: "mc.example".into(), since: Instant::now() }), None), "Auf mc.example");
        assert_eq!(status(&instance(Phase::Crashed { code: Some(1) }), None), "Abgestürzt");
        let mut again = instance(Phase::Connecting { server: "s".into() });
        again.reconnect_attempts = 2;
        assert_eq!(status(&again, None), "Verbindet neu · Versuch 2 von 5");
        let mut stopping = instance(Phase::Menu);
        stopping.stopping = true;
        assert_eq!(status(&stopping, None), "Wird gestoppt…");
    }

    #[test]
    fn rgba_frames_become_bgra_images() {
        let image = render_image(1, 1, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(image.as_bytes(0).unwrap(), &[3, 2, 1, 4]);
        assert!(render_image(2, 2, vec![0; 4]).is_none());
    }

    #[test]
    fn sign_in_errors_are_found_through_context() {
        let e = anyhow::Error::from(AuthError::SignInAgain).context("cannot start AltOne");
        assert!(needs_sign_in(&e));
        assert!(!needs_sign_in(&anyhow::anyhow!("no java")));
    }

    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<(String, ToMod)>>);

    impl Backend for Recorder {
        fn start(&self, _: fleet::Target, _: Vec<Account>, _: Arc<AtomicBool>, _: Option<crate::core::place::Rect>) {}
        fn stop(&self, _: Vec<String>) {}
        fn send(&self, key: &str, message: ToMod) {
            self.0.lock().unwrap().push((key.to_owned(), message));
        }
        fn read_preview(&self, _: String, _: PathBuf) {}
        fn quit(&self) {}
    }

    #[test]
    fn closing_a_game_that_is_not_played_hides_only_it() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let recorder = Arc::new(Recorder::default());
        rt.backend = recorder.clone();
        m.playing = Some("other".into());
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Ipc(Event::Message { instance: key.clone(), message: FromMod::Hotkey { action: HotkeyAction::Grid } }), &mut fx);
        assert!(fx.hotkeys.is_empty());
        assert_eq!(*recorder.0.lock().unwrap(), [(key, ToMod::Hide)]);
        assert_eq!(m.playing.as_deref(), Some("other"));
    }

    #[test]
    fn preview_notices_and_public_chat_redraw_at_most_the_chat() {
        let message = |instance: &str, message| Update::Ipc(Event::Message { instance: instance.into(), message });
        let chat = |instance: &str, kind| message(instance, FromMod::Chat { kind, from: None, text: "hi".into() });
        let notice = |instance: &str| message(instance, FromMod::Preview { seq: 2, w: 1, h: 1 });
        assert_eq!(redraw(&[notice("redraw-a"), notice("redraw-b")]), Redraw::Nothing);
        assert_eq!(redraw(&[notice("redraw-a"), chat("redraw-b", ChatKind::Player), chat("redraw-a", ChatKind::System)]), Redraw::Chat);
        assert_eq!(redraw(&[chat("redraw-a", ChatKind::Player), chat("redraw-a", ChatKind::Whisper)]), Redraw::All);
        assert_eq!(redraw(&[notice("redraw-a"), message("redraw-a", FromMod::State { kind: StateKind::Playing, server: None, reason: None, cause: None })]), Redraw::All);
        model::set_hint("redraw-lost", |h| h.link_lost = true);
        assert_eq!(redraw(&[notice("redraw-lost")]), Redraw::All);
        assert_eq!(redraw(&[chat("redraw-lost", ChatKind::Player)]), Redraw::All);
    }

    #[test]
    fn the_shown_game_reveals_once() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.playing = Some(key.clone());
        rt.reveal = Some(Reveal { key: key.clone(), hide: vec!["before".into()], manager: true });
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Ipc(Event::Message { instance: "before".into(), message: FromMod::Window { visible: true, focused: false } }), &mut fx);
        assert!(fx.revealed.is_none() && rt.reveal.is_some());
        apply(&mut m, &mut rt, Update::Ipc(Event::Message { instance: key.clone(), message: FromMod::Window { visible: true, focused: false } }), &mut fx);
        let shown = fx.revealed.expect("no reveal");
        assert_eq!((shown.key, shown.hide, shown.manager), (key, vec!["before".to_owned()], true));
        assert!(rt.reveal.is_none());
    }

    #[test]
    fn hotkeys_from_the_game_still_covering_count() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.playing = Some("next".into());
        rt.reveal = Some(Reveal { key: "next".into(), hide: vec![key.clone()], manager: false });
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Ipc(Event::Message { instance: key, message: FromMod::Hotkey { action: HotkeyAction::Grid } }), &mut fx);
        assert_eq!(fx.hotkeys, [HotkeyAction::Grid]);
    }

    struct Quiet;

    impl Backend for Quiet {
        fn start(&self, _: fleet::Target, _: Vec<Account>, _: Arc<AtomicBool>, _: Option<crate::core::place::Rect>) {}
        fn stop(&self, _: Vec<String>) {}
        fn send(&self, _: &str, _: ToMod) {}
        fn read_preview(&self, _: String, _: PathBuf) {}
        fn quit(&self) {}
    }

    fn runtime() -> Runtime {
        Runtime {
            backend: Arc::new(Quiet),
            cancel: HashMap::new(),
            visible: HashSet::new(),
            shared_visible: Arc::default(),
            stale: HashMap::new(),
            read_at: HashMap::new(),
            reconnects: HashMap::new(),
            joins: HashMap::new(),
            crash_restarts: HashMap::new(),
            started: HashMap::new(),
            joined: HashSet::new(),
            history: None,
            refreshed: false,
            show_when_ready: None,
            alerts: HashMap::new(),
            pids: HashMap::new(),
            restarts: HashMap::new(),
            restart_ready: HashMap::new(),
            restart_groups: 0,
            place: None,
            insets: HashMap::new(),
            unplaced: HashSet::new(),
            spread: None,
            reveal: None,
            badge: 0,
            notice: None,
            whispers: Vec::new(),
            phone: super::phone::Coalescer::default(),
            outbox: None,
            phone_at: None,
            adopted: HashSet::new(),
            foreign: HashSet::new(),
        }
    }

    fn setup(phase: Phase, kind: AccountKind, join: &[&str]) -> (Model, Runtime, String) {
        let id = uuid::Uuid::from_u128(7);
        let made = crate::core::instance::Instance::new("b", "26.3", crate::core::instance::Loader::default());
        let mut batch = Batch::new("b", &made.id);
        batch.accounts = vec![id];
        batch.join_commands = join.iter().map(|s| (*s).to_owned()).collect();
        let key = fleet::instance_key(&batch, id);
        let mut m = Model::default();
        m.accounts.push(Account { id, name: "AltOne".into(), kind, proxy: None, color: None });
        m.games.insert(key.clone(), Game { key: key.clone(), batch_id: batch.id.clone(), instance_id: made.id.clone(), account_id: id, name: "AltOne".into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, live: false, stopping: false });
        m.batches.push(batch);
        m.instances.push(made);
        (m, runtime(), key)
    }

    fn send_state(m: &mut Model, rt: &mut Runtime, fx: &mut Effects, key: &str, kind: StateKind, cause: Option<DisconnectCause>) {
        let message = FromMod::State { kind, server: Some("mc.example:25565".into()), reason: Some("why".into()), cause };
        apply(m, rt, Update::Ipc(Event::Message { instance: key.to_owned(), message }), fx);
    }

    fn dropped(cause: DisconnectCause) -> (Phase, Effects, Model) {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(cause));
        (m.games[&key].phase.clone(), fx, m)
    }

    #[test]
    fn a_ban_stops_for_good_and_is_remembered() {
        let (phase, fx, m) = dropped(DisconnectCause::Banned);
        assert_eq!(phase, Phase::Banned { server: Some("mc.example:25565".into()), reason: "why".into() });
        assert!(fx.later.is_empty() && fx.stop.is_empty());
        assert!(phase.needs_attention());
        let log = m.history.values().next().unwrap();
        assert_eq!(log.back().unwrap().kind, Kind::Banned);
        assert_eq!(fx.changed_history.len(), 1);
    }

    #[test]
    fn server_down_backs_off_five_times_then_needs_attention() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        for attempt in 1..=5 {
            let mut fx = Effects::default();
            send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
            assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }));
            assert_eq!(m.games[&key].reconnect_attempts, attempt);
            let Some(Deferred::Reconnect { delay, .. }) = fx.later.pop() else { panic!("no reconnect scheduled") };
            let base = [10.0, 30.0, 90.0, 300.0, 300.0][attempt as usize - 1];
            assert!(delay.as_secs_f64() >= base * 0.75 - 0.01 && delay.as_secs_f64() <= base * 1.25 + 0.01, "{delay:?} for try {attempt}");
        }
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(matches!(m.games[&key].phase, Phase::Disconnected { .. }) && fx.later.is_empty());
        assert_eq!(m.history[&key].len(), 6);
    }

    #[test]
    fn without_network_a_drop_waits_and_resumes_by_itself_when_it_is_back() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        apply(&mut m, &mut rt, Update::Reach { online: false }, &mut Effects::default());
        for _ in 0..8 {
            let mut fx = Effects::default();
            send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
            assert_eq!(m.games[&key].phase, Phase::NetworkLost { server: "mc.example:25565".into() });
            assert!(fx.suspect_offline);
            let [Deferred::Reconnect { delay, .. }] = fx.later.as_slice() else { panic!("no slow retry while waiting") };
            assert!(delay.as_secs() >= 48, "{delay:?}");
        }
        assert_eq!(m.games[&key].reconnect_attempts, 0);
        assert!(m.games[&key].phase.is_running() && !m.games[&key].phase.needs_attention());
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Reach { online: true }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::Connecting { server: "mc.example:25565".into() });
        assert!(fx.network_back);
        let Some(Deferred::Reconnect { generation, .. }) = fx.later.pop() else { panic!("no reconnect scheduled") };
        assert_eq!(rt.reconnects.get(&key), Some(&generation));
    }

    #[test]
    fn losing_the_network_gives_back_the_try_a_pending_reconnect_spent() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        send_state(&mut m, &mut rt, &mut Effects::default(), &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert_eq!(m.games[&key].reconnect_attempts, 1);
        let before = rt.reconnects.get(&key).copied();
        apply(&mut m, &mut rt, Update::Reach { online: false }, &mut Effects::default());
        assert_eq!(m.games[&key].phase, Phase::NetworkLost { server: "mc.example:25565".into() });
        assert_eq!(m.games[&key].reconnect_attempts, 0);
        assert_ne!(rt.reconnects.get(&key).copied(), before);
    }

    #[test]
    fn a_server_on_this_computer_or_in_the_home_network_does_not_wait_for_the_internet() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        apply(&mut m, &mut rt, Update::Reach { online: false }, &mut Effects::default());
        let mut fx = Effects::default();
        let message = FromMod::State { kind: StateKind::Disconnected, server: Some("192.168.1.20:25565".into()), reason: None, cause: Some(DisconnectCause::ServerDown) };
        apply(&mut m, &mut rt, Update::Ipc(Event::Message { instance: key.clone(), message }), &mut fx);
        assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }) && !fx.suspect_offline);
        assert_eq!(m.games[&key].reconnect_attempts, 1);
        apply(&mut m, &mut rt, Update::Reach { online: false }, &mut Effects::default());
        assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }));
    }

    #[test]
    fn local_servers_are_told_apart_from_the_internet() {
        for near in ["localhost", "localhost:25565", "127.0.0.1:25565", "10.0.0.5", "192.168.1.20:25565", "172.16.3.4:1", "[::1]:25565", "fd00::1", "minecraft.local"] {
            assert!(local_server(near), "{near}");
        }
        for far in ["mc.example:25565", "8.8.8.8:25565", "[2001:db8::1]:25565", "localhost.example.com", ""] {
            assert!(!local_server(far), "{far}");
        }
    }

    #[test]
    fn a_waiting_game_can_be_played_and_a_waiting_start_can_be_stopped() {
        assert!(playable(&Phase::NetworkLost { server: "s".into() }));
        assert_eq!(server_of(&Phase::NetworkLost { server: "s".into() }).as_deref(), Some("s"));
        assert!(Phase::NoNetwork.stoppable() && !Phase::NoNetwork.is_active());
    }

    #[test]
    fn a_ban_is_a_ban_even_without_network() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        apply(&mut m, &mut rt, Update::Reach { online: false }, &mut Effects::default());
        send_state(&mut m, &mut rt, &mut Effects::default(), &key, StateKind::Disconnected, Some(DisconnectCause::Banned));
        assert!(matches!(m.games[&key].phase, Phase::Banned { .. }));
    }

    #[test]
    fn a_proxied_account_does_not_wait_for_the_network() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        m.accounts[0].proxy = Some("socks5://127.0.0.1:1080".into());
        apply(&mut m, &mut rt, Update::Reach { online: false }, &mut Effects::default());
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }) && !fx.suspect_offline);
    }

    #[test]
    fn a_start_without_connection_waits_and_starts_when_it_is_back_unless_stopped() {
        let (mut m, mut rt, key) = setup(Phase::Starting { place: 1, total: 1 }, AccountKind::Microsoft, &[]);
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Failed { key: key.clone(), reason: "failed to lookup address".into(), login: false, offline: true }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::NoNetwork);
        assert!(fx.suspect_offline && !m.games[&key].phase.is_running() && !m.games[&key].phase.needs_attention());
        let [Deferred::StartWaiting { key: waiting, delay }] = fx.later.as_slice() else { panic!("no slow retry for a waiting start") };
        assert!(waiting == &key && delay.as_secs() >= 48);
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Reach { online: true }, &mut fx);
        assert!(matches!(fx.later.as_slice(), [Deferred::Start { keys }] if keys == &[key.clone()]));
        apply(&mut m, &mut rt, Update::Failed { key: key.clone(), reason: "bad jar".into(), login: false, offline: false }, &mut Effects::default());
        assert!(matches!(m.games[&key].phase, Phase::Failed { .. }));
    }

    #[test]
    fn a_kick_waits_longer_than_a_server_that_is_down() {
        let (phase, mut fx, _) = dropped(DisconnectCause::Kicked);
        assert!(matches!(phase, Phase::Connecting { .. }));
        let Some(Deferred::Reconnect { delay, .. }) = fx.later.pop() else { panic!("no reconnect scheduled") };
        assert!(delay.as_secs() >= 45);
    }

    #[test]
    fn a_drop_that_reconnects_tells_the_phone_and_so_does_the_return() {
        let (_, fx, _) = dropped(DisconnectCause::Kicked);
        let [(_, kicked)] = fx.phone.as_slice() else { panic!("{} phone incidents", fx.phone.len()) };
        assert_eq!((kicked.what, kicked.name.as_str(), kicked.reason.as_str(), kicked.server.as_deref()), (What::Kicked, "AltOne", "why", Some("mc.example:25565")));
        let (_, fx, _) = dropped(DisconnectCause::ServerDown);
        assert_eq!(fx.phone.first().map(|(_, i)| i.what), Some(What::Dropped));
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.phone.first().map(|(_, i)| i.what), Some(What::Back));
    }

    #[test]
    fn a_whitelist_refusal_is_left_alone() {
        let (phase, fx, _) = dropped(DisconnectCause::Whitelist);
        assert!(matches!(phase, Phase::Disconnected { .. }) && fx.later.is_empty());
    }

    #[test]
    fn a_rejected_sign_in_restarts_to_refresh_then_asks_for_a_new_one() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        for attempt in 1..=3 {
            let mut fx = Effects::default();
            send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
            assert_eq!(m.games[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert_eq!(fx.stop.len(), 1);
            assert!(fx.later.is_empty());
            let mut exit = Effects::default();
            apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut exit);
            assert_eq!(m.games[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert!(matches!(exit.later.pop(), Some(Deferred::Restart { .. })));
            m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert_eq!(m.games[&key].phase, Phase::LoginNeeded);
        assert!(fx.later.is_empty());
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert!(matches!(m.games[&key].phase, Phase::Disconnected { .. }) && fx.stop.is_empty());
    }

    #[test]
    fn the_same_account_signed_in_elsewhere_is_left_alone() {
        let (phase, fx, _) = dropped(DisconnectCause::Duplicate);
        assert!(matches!(phase, Phase::Disconnected { .. }) && fx.later.is_empty());
    }

    #[test]
    fn a_banned_instance_that_dies_stays_crashed_and_does_not_rejoin() {
        let (mut m, mut rt, key) = setup(Phase::Banned { server: None, reason: "x".into() }, AccountKind::Microsoft, &[]);
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::Crashed { code: Some(1) });
        assert!(fx.later.is_empty());
    }

    #[test]
    fn a_server_switch_does_not_send_the_join_commands_again() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &["/login pw"]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.later.len(), 1);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Connecting, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.later.len(), 1);
        assert_eq!(m.history[&key].len(), 1);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.later.iter().filter(|d| matches!(d, Deferred::Join { .. })).count(), 2);
    }

    #[test]
    fn leaving_to_the_menu_and_rejoining_sends_the_join_commands_again() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &["/login pw"]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Menu, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Connecting, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.later.iter().filter(|d| matches!(d, Deferred::Join { .. })).count(), 2);
        assert_eq!(m.history[&key].iter().filter(|e| e.kind == Kind::Connected).count(), 2);
    }

    #[test]
    fn a_late_exit_relaunches_only_after_the_old_process_ended() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        apply(&mut m, &mut rt, Update::Ipc(Event::Connected { instance: key.clone(), pid: 9, mod_version: String::new() }), &mut Effects::default());
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert_eq!(m.games[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
        assert_eq!(fx.stop, [key.clone()]);
        assert!(fx.later.is_empty());
        assert!(rt.pids.contains_key(&key));
        let mut exit = Effects::default();
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut exit);
        assert!(!rt.pids.contains_key(&key));
        assert_eq!(m.games[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
        let Some(Deferred::Restart { delay, generation, .. }) = exit.later.pop() else { panic!("no restart scheduled") };
        assert_eq!(delay, Duration::ZERO);
        assert_eq!(generation, rt.reconnects[&key]);
        assert!(exit.later.is_empty());
    }

    #[test]
    fn stability_counts_from_the_first_join_not_from_the_start() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        apply(&mut m, &mut rt, Update::Ipc(Event::Connected { instance: key.clone(), pid: 1, mod_version: String::new() }), &mut Effects::default());
        assert!(!rt.started.contains_key(&key));
        send_state(&mut m, &mut rt, &mut Effects::default(), &key, StateKind::Playing, None);
        assert!(rt.started.contains_key(&key));
    }

    #[test]
    fn a_manual_exit_after_a_rejected_sign_in_keeps_the_login_state() {
        let (mut m, mut rt, key) = setup(Phase::LoginNeeded, AccountKind::Microsoft, &[]);
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(0) }, &mut Effects::default());
        assert_eq!(m.games[&key].phase, Phase::LoginNeeded);
    }

    #[test]
    fn join_commands_go_out_once_per_join_and_not_after_a_respawn() {
        let (mut m, mut rt, key) = setup(Phase::Connecting { server: "mc.example:25565".into() }, AccountKind::Offline, &["/login pw", "  ", "/home"]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        let Some(Deferred::Join { lines, delay, generation, .. }) = fx.later.pop() else { panic!("no join scheduled") };
        assert_eq!(lines, ["/login pw", "/home"]);
        assert_eq!(delay, Duration::from_millis(2000));
        assert_eq!(rt.joins[&key], generation);
        assert_eq!(m.history[&key].back().unwrap().kind, Kind::Connected);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Dead, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert!(fx.later.is_empty());
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Menu, None);
        assert_ne!(rt.joins[&key], generation);
    }

    fn adopted(phase: Phase) -> (Model, Runtime, String) {
        let (mut m, mut rt, key) = setup(phase, AccountKind::Offline, &["/login pw"]);
        let (account, batch) = (m.games[&key].account_id, m.games[&key].batch_id.clone());
        let instance = m.games[&key].instance_id.clone();
        adopt_games(&mut m, &mut rt, vec![fleet::Adoption { key: key.clone(), batch: Some(batch), instance, account, pid: 41 }]);
        (m, rt, key)
    }

    #[test]
    fn an_adopted_game_runs_without_contact_until_its_mod_reports() {
        let (m, rt, key) = adopted(Phase::Stopped);
        assert_eq!(m.games[&key].phase, Phase::Menu);
        assert!(m.games[&key].phase.is_running());
        assert_eq!(rt.pids[&key], 41);
        assert!(model::hint(&key).link_lost);
        let (mut m, mut rt, solo) = adopted(Phase::Stopped);
        let account = m.games[&solo].account_id;
        let made = m.instances[0].clone();
        let key = fleet::solo_key(&made, account);
        adopt_games(&mut m, &mut rt, vec![fleet::Adoption { key: key.clone(), batch: None, instance: made.id.clone(), account, pid: 42 }]);
        assert_eq!(m.games[&key].batch_id, fleet::solo_owner(&made.id));
        assert_eq!(m.games[&key].phase, Phase::Menu);
    }

    #[test]
    fn an_adopted_game_that_plays_on_sends_no_join_commands_and_no_news() {
        let (mut m, mut rt, key) = adopted(Phase::Stopped);
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Ipc(Event::Connected { instance: key.clone(), pid: 41, mod_version: String::new() }), &mut fx);
        assert!(!model::hint(&key).link_lost);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert!(matches!(m.games[&key].phase, Phase::Playing { .. }));
        assert!(fx.later.is_empty() && fx.phone.is_empty());
        assert!(m.history.get(&key).is_none_or(|h| h.is_empty()));
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert!(fx.later.is_empty());
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Menu, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert!(matches!(fx.later.pop(), Some(Deferred::Join { .. })), "a real rejoin later sends them again");
    }

    #[test]
    fn an_adopted_game_on_a_disconnect_screen_is_not_reconnected_by_the_restart() {
        let (mut m, mut rt, key) = adopted(Phase::Stopped);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(matches!(m.games[&key].phase, Phase::Disconnected { .. }));
        assert!(fx.later.is_empty() && fx.phone.is_empty());
        let (mut m, mut rt, key) = adopted(Phase::Stopped);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Banned));
        assert!(matches!(m.games[&key].phase, Phase::Banned { .. }));
        assert!(fx.later.is_empty());
    }

    #[test]
    fn an_adopted_game_that_quits_without_an_exit_code_is_not_a_crash() {
        let (mut m, mut rt, key) = adopted(Phase::Stopped);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: None }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(fx.later.is_empty());
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: None }, &mut fx);
        assert!(matches!(m.games[&key].phase, Phase::Crashed { .. } | Phase::Restarting { .. }), "a launched game still crashes");
    }

    #[test]
    fn adopted_games_without_a_tile_are_let_go_and_the_rest_may_talk() {
        struct Settled(std::sync::Mutex<(Vec<String>, Vec<String>)>);
        impl Backend for Settled {
            fn start(&self, _: fleet::Target, _: Vec<Account>, _: Arc<AtomicBool>, _: Option<crate::core::place::Rect>) {}
            fn stop(&self, _: Vec<String>) {}
            fn send(&self, _: &str, _: ToMod) {}
            fn read_preview(&self, _: String, _: PathBuf) {}
            fn quit(&self) {}
            fn settle(&self, placed: &[String], dropped: &[String]) {
                *self.0.lock().unwrap() = (placed.to_vec(), dropped.to_vec());
            }
        }
        let (mut m, mut rt, key) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let settled = Arc::new(Settled(std::sync::Mutex::default()));
        rt.backend = settled.clone();
        let game = &m.games[&key];
        let (batch, instance, account) = (game.batch_id.clone(), game.instance_id.clone(), game.account_id);
        let gone = fleet::Adoption { key: "removed-member".into(), batch: Some(batch.clone()), instance: instance.clone(), account, pid: 2 };
        let stranger = fleet::Adoption { key: "solo-of-deleted-account".into(), batch: None, instance: instance.clone(), account: uuid::Uuid::from_u128(99), pid: 3 };
        adopt_games(&mut m, &mut rt, vec![fleet::Adoption { key: key.clone(), batch: Some(batch), instance, account, pid: 1 }, gone, stranger]);
        let (placed, dropped) = settled.0.lock().unwrap().clone();
        assert_eq!(placed, [key.clone()]);
        assert_eq!(dropped, ["removed-member", "solo-of-deleted-account"]);
        assert!(!m.games.contains_key("removed-member") && !rt.pids.contains_key("removed-member"));
    }

    #[test]
    fn a_play_request_from_another_start_is_kept_for_the_manager() {
        let (mut m, mut rt, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let mut fx = Effects::default();
        let event = Event::Play { instance: "abc".into(), account: Some("acc".into()) };
        apply(&mut m, &mut rt, Update::Ipc(event), &mut fx);
        assert_eq!(fx.plays, vec![("abc".to_owned(), Some("acc".to_owned()))]);
        assert!(!fx.reopen);
    }

    #[test]
    fn a_second_start_brings_the_manager_back() {
        let (mut m, mut rt, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Ipc(Event::Reopen), &mut fx);
        assert!(fx.reopen);
    }

    #[test]
    fn no_join_commands_means_nothing_is_scheduled() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert!(fx.later.is_empty());
    }

    #[test]
    fn a_crash_restarts_three_times_then_stays_crashed() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        for attempt in 1..=3 {
            let mut fx = Effects::default();
            apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut fx);
            assert_eq!(m.games[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert!(!m.games[&key].phase.needs_attention());
            assert!(matches!(fx.later.pop(), Some(Deferred::Restart { .. })));
            m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::Crashed { code: Some(1) });
        assert!(fx.later.is_empty());
        assert_eq!(m.history[&key].iter().filter(|e| e.kind == Kind::Crashed).count(), 4);
    }

    #[test]
    fn a_long_run_resets_the_crash_count() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        rt.crash_restarts.insert(key.clone(), 3);
        rt.started.insert(key.clone(), Instant::now() - Duration::from_secs(120));
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut Effects::default());
        assert_eq!(m.games[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
    }

    #[test]
    fn clean_and_requested_exits_do_not_restart() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(0) }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        m.games.get_mut(&key).unwrap().stopping = true;
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(fx.later.is_empty() && m.history.is_empty());
    }

    #[test]
    fn a_requested_restart_starts_again_only_after_a_stop_exit() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let mut fx = Effects::default();
        rt.restarts.insert(key.clone(), 1);
        rt.restarts.insert("other".into(), 1);
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(0) }, &mut fx);
        assert!(fx.later.is_empty() && rt.restarts.contains_key(&key), "an exit the user did not ask for is no restart");
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        m.games.get_mut(&key).unwrap().stopping = true;
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut fx);
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(fx.later.is_empty(), "the group waits for its other account");
        rt.restarts.remove("other");
        assert_eq!(super::restart_done(&mut rt, 1), Some(vec![key.clone()]), "the group starts together once the last one is down");
        assert!(rt.restarts.is_empty() && rt.restart_ready.is_empty());
    }

    #[test]
    fn a_stopped_account_leaves_its_restart_group_and_the_rest_still_starts() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        rt.restarts.insert("a".into(), 1);
        rt.restarts.insert("b".into(), 1);
        rt.restart_ready.insert(1, vec!["c".into()]);
        rt.restarts.remove("a");
        rt.restart_ready.get_mut(&1).unwrap().push("a".into());
        assert!(super::drop_restart(&mut rt, "a").is_empty(), "b is still going down");
        assert_eq!(super::drop_restart(&mut rt, "b"), vec!["c".to_string()], "a user stop never starts a, and c goes on once b is down");
        rt.restarts.insert("gone".into(), 2);
        rt.restart_ready.insert(2, vec![key.clone()]);
        m.games.clear();
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Exited { key: "gone".into(), code: Some(0) }, &mut fx);
        assert!(matches!(fx.later.as_slice(), [Deferred::Start { keys }] if *keys == [key]), "a removed instance does not hold up its group");
    }

    #[test]
    fn a_stopped_banned_account_keeps_its_ban_in_the_status() {
        crate::core::i18n::init("de");
        let (_, _, mut m) = dropped(DisconnectCause::Banned);
        let key = m.games.keys().next().unwrap().clone();
        let mut rt = runtime();
        let mut fx = Effects::default();
        m.games.get_mut(&key).unwrap().stopping = true;
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(0) }, &mut fx);
        let i = &m.games[&key];
        assert_eq!(i.phase, Phase::Stopped);
        assert_eq!(super::stopped_ban(i, &m).as_deref(), Some("Gestoppt · Gesperrt · why"));
        crate::core::history::push(m.history.get_mut(&key).unwrap(), crate::core::history::Entry::now(Kind::Connected, None, ""));
        assert!(super::stopped_ban(&m.games[&key], &m).is_none());
    }

    #[test]
    fn a_refresh_result_moves_idle_instances_in_and_out_of_login_needed() {
        let (mut m, mut rt, key) = setup(Phase::Stopped, AccountKind::Microsoft, &[]);
        let id = uuid::Uuid::from_u128(7);
        apply(&mut m, &mut rt, Update::SignIn { account: id, valid: false }, &mut Effects::default());
        assert_eq!(m.games[&key].phase, Phase::LoginNeeded);
        apply(&mut m, &mut rt, Update::SignIn { account: id, valid: true }, &mut Effects::default());
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        apply(&mut m, &mut rt, Update::SignIn { account: id, valid: false }, &mut Effects::default());
        assert_eq!(m.games[&key].phase, Phase::Menu);
    }

    #[test]
    fn the_status_line_counts_tries_against_the_policy_and_names_a_ban() {
        crate::core::i18n::init("de");
        let mut again = instance(Phase::Connecting { server: "s".into() });
        again.reconnect_attempts = 2;
        assert_eq!(status(&again, None), "Verbindet neu · Versuch 2 von 5");
        assert_eq!(status(&instance(Phase::Banned { server: None, reason: "Cheating".into() }), None), "Gesperrt · Cheating");
        assert_eq!(status(&instance(Phase::Restarting { attempt: 1, of: 3 }), None), "Startet neu · Versuch 1 von 3");
    }

    fn rekeyed(m: &mut Model, old: &str, new: &str) -> String {
        let mut i = m.games.remove(old).unwrap();
        i.key = new.to_owned();
        m.games.insert(new.to_owned(), i);
        new.to_owned()
    }

    #[test]
    fn a_dropped_link_on_a_living_game_shows_until_it_talks_again() {
        crate::core::i18n::init("de");
        let (mut m, mut rt, old) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let key = rekeyed(&mut m, &old, "link-test");
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Ipc(Event::Disconnected { instance: key.clone() }), &mut fx);
        assert!(!model::hint(&key).link_lost, "no process known yet");
        apply(&mut m, &mut rt, Update::Ipc(Event::Connected { instance: key.clone(), pid: 9, mod_version: String::new() }), &mut fx);
        apply(&mut m, &mut rt, Update::Ipc(Event::Disconnected { instance: key.clone() }), &mut fx);
        assert!(model::hint(&key).link_lost);
        assert_eq!(status(&m.games[&key], None), "Keine Verbindung zum Spiel");
        apply(&mut m, &mut rt, Update::Ipc(Event::Connected { instance: key.clone(), pid: 9, mod_version: String::new() }), &mut fx);
        assert!(!model::hint(&key).link_lost);
        apply(&mut m, &mut rt, Update::Ipc(Event::Disconnected { instance: key.clone() }), &mut fx);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert!(!model::hint(&key).link_lost);
        assert_eq!(status(&m.games[&key], None), "Auf mc.example:25565");
    }

    #[test]
    fn a_link_that_drops_because_the_game_is_closing_is_not_news() {
        let (mut m, mut rt, old) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let key = rekeyed(&mut m, &old, "link-closing");
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Ipc(Event::Connected { instance: key.clone(), pid: 9, mod_version: String::new() }), &mut fx);
        m.games.get_mut(&key).unwrap().stopping = true;
        apply(&mut m, &mut rt, Update::Ipc(Event::Disconnected { instance: key.clone() }), &mut fx);
        assert!(!model::hint(&key).link_lost);
        m.games.get_mut(&key).unwrap().stopping = false;
        apply(&mut m, &mut rt, Update::Ipc(Event::Disconnected { instance: key.clone() }), &mut fx);
        assert!(model::hint(&key).link_lost);
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(0) }, &mut fx);
        assert_eq!(model::hint(&key), model::Hint::default());
    }

    #[test]
    fn a_connection_failure_on_a_proxied_account_asks_for_a_proxy_check() {
        crate::core::i18n::init("de");
        let (mut m, mut rt, old) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let key = rekeyed(&mut m, &old, "proxy-test");
        m.accounts[0].proxy = Some("socks5://u:p@10.0.0.1:1080".into());
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert_eq!(fx.check_proxy, [(key.clone(), "socks5://u:p@10.0.0.1:1080".to_owned())]);
        assert_eq!(status(&m.games[&key], None), "Verbindet neu · Versuch 1 von 5");
        apply(&mut m, &mut rt, Update::ProxyChecked { key: key.clone(), fault: Some(Fault::Unreachable) }, &mut fx);
        assert_eq!(status(&m.games[&key], None), "Proxy nicht erreichbar");
        apply(&mut m, &mut rt, Update::ProxyChecked { key: key.clone(), fault: Some(Fault::Rejected) }, &mut fx);
        assert_eq!(status(&m.games[&key], None), "Proxy lehnt die Anmeldung ab");
        apply(&mut m, &mut rt, Update::ProxyChecked { key: key.clone(), fault: None }, &mut fx);
        assert_eq!(status(&m.games[&key], None), "Verbindet neu · Versuch 1 von 5");
        apply(&mut m, &mut rt, Update::ProxyChecked { key: key.clone(), fault: Some(Fault::Unreachable) }, &mut fx);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(model::hint(&key).proxy, None);
    }

    #[test]
    fn proxy_checks_only_follow_connection_failures_of_proxied_accounts() {
        let (mut m, mut rt, old) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let key = rekeyed(&mut m, &old, "proxy-skip");
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(fx.check_proxy.is_empty(), "no proxy, no check");
        m.accounts[0].proxy = Some("socks5://10.0.0.1".into());
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        for cause in [DisconnectCause::Banned, DisconnectCause::Kicked, DisconnectCause::Whitelist, DisconnectCause::ServerFull, DisconnectCause::Duplicate] {
            send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(cause));
            m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        assert!(fx.check_proxy.is_empty(), "the server answered, the proxy works");
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert_eq!(fx.check_proxy.len(), 1);
    }

    #[test]
    fn a_late_proxy_result_for_a_running_game_is_ignored() {
        let (mut m, mut rt, old) = setup(Phase::Playing { server: "s".into(), since: Instant::now() }, AccountKind::Microsoft, &[]);
        let key = rekeyed(&mut m, &old, "proxy-late");
        apply(&mut m, &mut rt, Update::ProxyChecked { key: key.clone(), fault: Some(Fault::Unreachable) }, &mut Effects::default());
        assert_eq!(model::hint(&key).proxy, None);
    }

    #[test]
    fn recovery_notices_name_one_file_and_summarise_several() {
        use super::{Recovery, recovered_notice};
        let at = |file: &str, r: Recovery| (PathBuf::from("data").join(file), r);
        assert_eq!(recovered_notice(&[]), None);
        assert_eq!(recovered_notice(&[at("accounts.json", Recovery::Restored)]), Some(crate::core::i18n::tr("Die Accounts waren beschädigt und wurden aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht.")));
        let two_batches = [at("b1/batch.json", Recovery::Lost), at("b2/batch.json", Recovery::Lost)];
        assert_eq!(recovered_notice(&two_batches), Some(crate::core::i18n::trf("%lld Batches waren beschädigt und ließen sich nicht wiederherstellen. Leg sie neu an; Kopien liegen in ihren Ordnern (batch.json.damaged).", &[&2])));
        assert_eq!(recovered_notice(&[at("i1/instance.json", Recovery::Restored)]), Some(crate::core::i18n::tr("Eine Instanz war beschädigt und wurde aus der Sicherung wiederhergestellt. Die letzte Änderung fehlt vielleicht.")));
        let mixed = [at("accounts.json", Recovery::Restored), at("settings.json", Recovery::Lost), at("b1/batch.json", Recovery::Restored)];
        assert_eq!(recovered_notice(&mixed), Some(crate::core::i18n::trf("%lld Dateien waren beschädigt: %lld aus der Sicherung wiederhergestellt, die übrigen nicht. Kopien liegen jeweils daneben (Endung .damaged).", &[&3, &2])));
    }
}
