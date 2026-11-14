//! Runs the app behind the views: loads accounts and batches, drives the fleet, turns mod events into model changes and handles the actions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::actions::{Broadcast, NextAttention, NextInstance, OpenChat, Play, PreviousInstance, Reconnect, Reload, Respawn, SendChat, ShowGrid, Start, StartAll, StartBatch, Stop, StopAll, StopBatch};
use super::model::{self, ChatLine, Instance, Model, Phase};
use super::native::{self, Rect};
use crate::core::account::{self, Account, AccountKind};
use crate::core::auth::{self, AuthError};
use crate::core::batch::{self, Batch};
use crate::core::fleet::{self, Fleet};
use crate::core::history::{self, Kind};
use crate::core::i18n::{tr, trf};
use crate::core::ipc::{ChatKind, DisconnectCause, Event, FromMod, HotkeyAction, StateKind, ToMod};
use crate::core::reconnect::{self, Decision, Reaction};
use crate::core::settings;
use crate::core::{companion, drive, install, ipc};

const STABLE: Duration = Duration::from_secs(60);
const COALESCE: Duration = Duration::from_millis(40);
const ALERT_AFTER: Duration = Duration::from_secs(3);
const OFFSCREEN_PREVIEW: Duration = Duration::from_secs(30);
const EXIT_POLL: Duration = Duration::from_millis(500);
const STAGGER_STEP: Duration = Duration::from_millis(100);
const KEPT_CHAT: usize = 5000;
const ALERT_NAMES: usize = 4;
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// What the fleet reports back to the UI thread.
pub enum Update {
    Preparing { batch: String, percent: u8 },
    Progress { batch: String, done: usize, total: usize },
    Launched { key: String },
    Failed { key: String, reason: String, login: bool },
    Ipc(Event),
    Exited { key: String, code: Option<i32> },
    Preview { key: String, seq: u32, image: Arc<RenderImage> },
    Seed { key: String, phase: Phase, attempts: u32 },
    /// Whether the saved sign-in of an account still works, from a token refresh.
    SignIn { account: uuid::Uuid, valid: bool },
}

/// Runs and talks to instances off the UI thread: the real fleet, or a fake one when an agent drives the app.
pub trait Backend: Send + Sync {
    /// Starts `accounts` of `batch` one after another until `cancel` is set.
    fn start(&self, batch: Batch, accounts: Vec<Account>, cancel: Arc<AtomicBool>);
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
}

impl Real {
    fn new(out: async_channel::Sender<Update>) -> anyhow::Result<Self> {
        let (fleet, events) = Fleet::new()?;
        let fleet = Arc::new(fleet);
        let forward = out.clone();
        std::thread::Builder::new().name("fleet-events".into()).spawn(move || {
            while let Ok(event) = events.recv() {
                if forward.send_blocking(Update::Ipc(event)).is_err() {
                    return;
                }
            }
        })?;
        let watch: Arc<Mutex<HashSet<String>>> = Arc::default();
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
            while let Ok(first) = wanted.recv() {
                let mut batch: HashMap<String, PathBuf> = HashMap::from([first]);
                batch.extend(wanted.try_iter());
                for (key, path) in batch {
                    let Some(preview) = ipc::read_preview(&path) else { continue };
                    let Some(image) = render_image(preview.width, preview.height, preview.rgba) else { continue };
                    if frames.send_blocking(Update::Preview { key, seq: preview.seq, image }).is_err() {
                        return;
                    }
                }
            }
        })?;
        Ok(Self { fleet, out, watch, owners: Arc::default(), runs: AtomicU64::new(0), sends, previews, refreshing: Arc::default() })
    }
}

impl Backend for Real {
    fn start(&self, batch: Batch, accounts: Vec<Account>, cancel: Arc<AtomicBool>) {
        let (fleet, out, watch, owners) = (Arc::clone(&self.fleet), self.out.clone(), Arc::clone(&self.watch), Arc::clone(&self.owners));
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut owners = lock(&owners);
            for account in &accounts {
                owners.insert(fleet::instance_key(&batch, account.id), run);
            }
        }
        let _ = std::thread::Builder::new().name("fleet-start".into()).spawn(move || {
            let mine = |key: &str| lock(&owners).get(key) == Some(&run);
            let fail_all = |reason: String| {
                for account in &accounts {
                    let key = fleet::instance_key(&batch, account.id);
                    if mine(&key) {
                        let _ = out.send_blocking(Update::Failed { key, reason: reason.clone(), login: false });
                    }
                }
            };
            let jar = match companion::jar() {
                Ok(jar) => jar,
                Err(e) => return fail_all(format!("{e:#}")),
            };
            let last = Mutex::new(u8::MAX);
            let progress = |p: install::Progress| {
                let percent = (p.done * 100).checked_div(p.total).unwrap_or(0).min(100) as u8;
                let mut last = lock(&last);
                if *last != percent {
                    *last = percent;
                    let _ = out.send_blocking(Update::Preparing { batch: batch.id.clone(), percent });
                }
            };
            let prepared = match fleet.prepare_launch(&batch, Some(&jar), &progress) {
                Ok(prepared) => prepared,
                Err(e) => return fail_all(format!("{e:#}")),
            };
            let total = accounts.len();
            let _ = out.send_blocking(Update::Progress { batch: batch.id.clone(), done: 0, total });
            for (i, account) in accounts.iter().enumerate() {
                if i > 0 {
                    let until = Instant::now() + fleet.stagger;
                    while Instant::now() < until && !cancel.load(Ordering::SeqCst) {
                        std::thread::sleep(STAGGER_STEP);
                    }
                }
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                let key = fleet::instance_key(&batch, account.id);
                if !mine(&key) {
                    let _ = out.send_blocking(Update::Progress { batch: batch.id.clone(), done: i + 1, total });
                    continue;
                }
                let update = match fleet.start_one(&prepared, account) {
                    Ok(key) => {
                        lock(&watch).insert(key.clone());
                        if !lock(&owners).contains_key(&key) {
                            fleet.stop_all(std::slice::from_ref(&key));
                        }
                        Some(Update::Launched { key })
                    }
                    Err(_) if fleet.is_running(&key) => Some(Update::Launched { key }),
                    Err(_) if !mine(&key) => None,
                    Err(e) => Some(Update::Failed { key, login: needs_sign_in(&e), reason: format!("{e:#}") }),
                };
                if let Some(update) = update {
                    let _ = out.send_blocking(update);
                }
                let _ = out.send_blocking(Update::Progress { batch: batch.id.clone(), done: i + 1, total });
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
    alerts: HashMap<String, Vec<(String, Alert)>>,
    pids: HashMap<String, u32>,
    rect: Option<Rect>,
    badge: usize,
    whispers: Vec<Whisper>,
}

impl Global for Runtime {}

/// Loads saved data into the model, starts the fleet and registers the action handlers.
pub fn init(cx: &mut App) {
    let (out, updates) = async_channel::unbounded::<Update>();
    let shared_visible: Arc<Mutex<HashSet<String>>> = Arc::default();
    let backend: Arc<dyn Backend> = if drive::on() {
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
        alerts: HashMap::new(),
        pids: HashMap::new(),
        rect: None,
        badge: 0,
        whispers: Vec::new(),
    });
    if let Err(e) = settings::activate() {
        eprintln!("settings: {e:#}");
    }
    reload(cx);
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
    cx.on_action(|_: &ShowGrid, cx| cx.defer(show_grid));
    cx.on_action(|_: &NextInstance, cx| cx.defer(|cx| step(HotkeyAction::Next, cx)));
    cx.on_action(|_: &PreviousInstance, cx| cx.defer(|cx| step(HotkeyAction::Prev, cx)));
    cx.on_action(|_: &NextAttention, cx| cx.defer(|cx| step(HotkeyAction::NextAttention, cx)));
    cx.on_action(|_: &Reload, cx| reload(cx));
    cx.on_action(|_: &OpenChat, cx| cx.defer(super::chat::open));
    cx.on_action(|a: &Broadcast, cx| broadcast(&a.batch, &a.text, cx));
    cx.on_action(|a: &SendChat, cx| send_chat(&a.key, &a.text, cx));
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
        if let Some(rt) = cx.try_global::<Runtime>() {
            for cancel in rt.cancel.values().flatten() {
                cancel.store(true, Ordering::SeqCst);
            }
            rt.backend.quit();
        }
        async {}
    })
    .detach();
}

fn batch_ids(cx: &App) -> Vec<String> {
    model::get(cx).read(cx).batches.iter().map(|b| b.id.clone()).collect()
}

fn backend(cx: &App) -> Option<Arc<dyn Backend>> {
    cx.try_global::<Runtime>().map(|rt| Arc::clone(&rt.backend))
}

/// Reads accounts and batches from disk again and rebuilds the instance list, keeping what runs.
pub fn reload(cx: &mut App) {
    let task = cx.background_spawn(async {
        let (accounts, batches) = (account::load(), batch::load_all());
        let logs = batches.as_ref().map(|batches| saved_history(batches)).unwrap_or_default();
        (accounts, batches, logs)
    });
    cx.spawn(async move |cx| {
        let (accounts, batches, logs) = task.await;
        cx.update(|cx| {
            let model = model::get(cx);
            let mut retired = Vec::new();
            model.update(cx, |m, cx| {
                for (key, log) in logs {
                    m.history.entry(key).or_insert(log);
                }
                match (accounts, batches) {
                    (Ok(accounts), Ok(batches)) => {
                        m.accounts = accounts;
                        m.batches = batches;
                        m.error = None;
                    }
                    (Err(e), _) | (_, Err(e)) => m.error = Some(format!("{e:#}")),
                }
                retired = rebuild(m);
                cx.notify();
            });
            for image in retired {
                cx.drop_image(image, None);
            }
            let present: HashSet<String> = model.read(cx).instances.keys().cloned().collect();
            super::tile::forget_except(&|k| present.contains(k), cx);
            let known: Vec<(String, String)> = model.read(cx).instances.values().map(|i| (i.key.clone(), i.name.clone())).collect();
            let first = cx.try_global::<Runtime>().is_some_and(|rt| !rt.refreshed);
            let to_check: Vec<Account> = {
                let m = model.read(cx);
                m.accounts.iter().filter(|a| first || m.instances.values().any(|i| i.account_id == a.id && i.phase == Phase::LoginNeeded)).cloned().collect()
            };
            if let Some(rt) = cx.try_global::<Runtime>() {
                rt.backend.known(known);
                rt.backend.refresh(to_check);
            }
            if first && cx.has_global::<Runtime>() {
                cx.global_mut::<Runtime>().refreshed = true;
            }
        });
    })
    .detach();
}

fn saved_history(batches: &[Batch]) -> Vec<(String, history::Log)> {
    let mut logs = Vec::new();
    for batch in batches {
        for id in &batch.accounts {
            let log = history::load(&history::path(batch, *id));
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
            let instance = m.instances.entry(key.clone()).or_insert_with(|| Instance {
                key,
                batch_id: batch.id.clone(),
                account_id: *id,
                name: account.name.clone(),
                phase: Phase::Stopped,
                reconnect_attempts: 0,
                unread_whispers: 0,
                preview: None,
                preview_seq: 0,
                stopping: false,
            });
            instance.name = account.name.clone();
        }
    }
    let mut retired = Vec::new();
    m.instances.retain(|k, i| {
        let stays = keep.contains(k) || i.phase.is_running();
        if !stays && let Some(image) = i.preview.take() {
            retired.push(image);
        }
        stays
    });
    let instances = &m.instances;
    m.history.retain(|k, _| instances.contains_key(k));
    m.order_seq += 1;
    retired
}

fn update_model(cx: &mut App, f: impl FnOnce(&mut Model, &mut Runtime, &mut App)) {
    let model = model::get(cx);
    if !cx.has_global::<Runtime>() {
        return;
    }
    cx.update_global::<Runtime, _>(|rt, cx| {
        model.update(cx, |m, cx| {
            f(m, rt, cx);
            cx.notify();
        });
    });
    refresh_badge(cx);
}

fn start_keys(keys: &[String], cx: &mut App) {
    let model = model::get(cx);
    let mut by_batch: HashMap<String, Vec<String>> = HashMap::new();
    for key in keys {
        if let Some(i) = model.read(cx).instances.get(key) {
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

fn launch(batch_id: &str, only: Option<&[String]>, restart: bool, cx: &mut App) {
    let model = model::get(cx);
    let (batch, accounts) = {
        let m = model.read(cx);
        let Some(batch) = m.batch(batch_id).cloned() else { return };
        let accounts: Vec<Account> = batch
            .accounts
            .iter()
            .filter_map(|id| m.account(*id).cloned())
            .filter(|a| {
                let key = fleet::instance_key(&batch, a.id);
                only.is_none_or(|o| o.contains(&key)) && m.instances.get(&key).is_some_and(|i| !i.phase.is_running() )
            })
            .collect();
        (batch, accounts)
    };
    if accounts.is_empty() {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    update_model(cx, |m, rt, _| {
        let runs = rt.cancel.entry(batch.id.clone()).or_default();
        runs.retain(|c| Arc::strong_count(c) > 1);
        runs.push(Arc::clone(&cancel));
        let total = accounts.len();
        for account in &accounts {
            let key = fleet::instance_key(&batch, account.id);
            if !restart {
                rt.crash_restarts.remove(&key);
                *rt.reconnects.entry(key.clone()).or_default() += 1;
            }
            if let Some(i) = m.instances.get_mut(&key) {
                i.phase = Phase::Starting { done: 0, total };
                i.reconnect_attempts = 0;
                i.stopping = false;
            }
        }
        m.order_seq += 1;
    });
    if let Some(backend) = backend(cx) {
        backend.start(batch, accounts, cancel);
    }
}

fn stop_keys(keys: Vec<String>, cx: &mut App) {
    let mut running = Vec::new();
    update_model(cx, |m, rt, _| {
        for key in &keys {
            rt.reconnects.entry(key.clone()).and_modify(|g| *g += 1).or_insert(1);
            rt.joins.entry(key.clone()).and_modify(|g| *g += 1).or_insert(1);
            rt.crash_restarts.remove(key);
            rt.joined.remove(key);
            let Some(i) = m.instances.get_mut(key) else { continue };
            match i.phase {
                Phase::Starting { .. } if !rt.pids.contains_key(key) => {
                    i.phase = Phase::Stopped;
                    running.push(key.clone());
                }
                ref p if p.is_running() || rt.pids.contains_key(key) => {
                    i.stopping = true;
                    running.push(key.clone());
                }
                Phase::Crashed { .. } | Phase::Restarting { .. } | Phase::Failed { .. } => i.phase = Phase::Stopped,
                _ => {}
            }
        }
        m.order_seq += 1;
    });
    if !running.is_empty()
        && let Some(backend) = backend(cx)
    {
        backend.stop(running);
    }
}

fn stop_batch(batch: &str, cx: &mut App) {
    if cx.has_global::<Runtime>()
        && let Some(runs) = cx.global_mut::<Runtime>().cancel.remove(batch)
    {
        for cancel in runs {
            cancel.store(true, Ordering::SeqCst);
        }
    }
    let keys: Vec<String> = model::get(cx).read(cx).instances.values().filter(|i| i.batch_id == batch).map(|i| i.key.clone()).collect();
    stop_keys(keys, cx);
}

fn reconnect(key: &str, cx: &mut App) {
    let key = key.to_owned();
    let mut send = false;
    update_model(cx, |m, rt, _| {
        let Some(i) = m.instances.get_mut(&key) else { return };
        if !i.phase.is_running() {
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
    let dead = model::get(cx).read(cx).instances.get(key).is_some_and(|i| matches!(i.phase, Phase::Dead { .. }));
    if dead && let Some(backend) = backend(cx) {
        backend.send(key, ToMod::Respawn);
    }
}

fn server_of(phase: &Phase) -> Option<String> {
    match phase {
        Phase::Connecting { server } | Phase::Queue { server } | Phase::Playing { server, .. } => Some(server.clone()),
        Phase::Disconnected { server, .. } | Phase::Dead { server, .. } | Phase::Banned { server, .. } => server.clone(),
        _ => None,
    }
}

/// Sends a chat line or command to one running instance.
fn send_chat(key: &str, text: &str, cx: &mut App) {
    let Some(backend) = backend(cx) else { return };
    if model::get(cx).read(cx).instances.get(key).is_some_and(|i| i.phase.on_server()) {
        backend.send(key, ToMod::ChatSend { text: text.to_owned() });
    }
}

/// Marks the whispers of `batch` (all instances when `None`) as seen.
pub fn clear_whispers(batch: Option<&str>, cx: &mut App) {
    let model = model::get(cx);
    if !model.read(cx).instances.values().any(|i| i.unread_whispers > 0 && batch.is_none_or(|b| i.batch_id == b)) {
        return;
    }
    update_model(cx, |m, _, _| {
        for i in m.instances.values_mut().filter(|i| batch.is_none_or(|b| i.batch_id == b)) {
            i.unread_whispers = 0;
        }
    });
}

fn broadcast(batch: &str, text: &str, cx: &mut App) {
    let Some(backend) = backend(cx) else { return };
    let model = model::get(cx);
    for i in model.read(cx).instances.values().filter(|i| i.batch_id == batch && i.phase.on_server()) {
        backend.send(&i.key, ToMod::ChatSend { text: text.to_owned() });
    }
}

fn playable(phase: &Phase) -> bool {
    matches!(phase, Phase::Menu | Phase::Connecting { .. } | Phase::Queue { .. } | Phase::Playing { .. } | Phase::Disconnected { .. } | Phase::Dead { .. })
}

/// Puts the game window of `key` in the manager's place and hides the manager.
pub fn play(key: &str, cx: &mut App) {
    let model = model::get(cx);
    if !model.read(cx).instances.get(key).is_some_and(|i| playable(&i.phase)) {
        return;
    }
    let shown = model.read(cx).playing.is_some();
    let rect = if shown { cx.try_global::<Runtime>().and_then(|rt| rt.rect) } else { super::main_window::game_rect(cx) };
    let Some(rect) = rect else { return };
    swap_to(key, rect, cx);
    if !shown {
        super::main_window::hide(cx);
    }
}

fn swap_to(key: &str, rect: Rect, cx: &mut App) {
    let key = key.to_owned();
    let mut previous = None;
    update_model(cx, |m, rt, _| {
        rt.rect = Some(rect);
        previous = m.playing.replace(key.clone()).filter(|p| *p != key);
        if let Some(i) = m.instances.get_mut(&key) {
            i.unread_whispers = 0;
        }
        if let Some(pid) = rt.pids.get(&key) {
            native::allow_foreground(*pid);
        }
    });
    drive::note(format!("play {key}"));
    if let Some(backend) = backend(cx) {
        backend.send(&key, ToMod::Show { x: rect.x, y: rect.y, w: rect.w, h: rect.h });
        if let Some(previous) = previous {
            backend.send(&previous, ToMod::Hide);
        }
    }
}

/// Hides the game that is played and brings the manager back in its place.
pub fn show_grid(cx: &mut App) {
    let mut previous = None;
    let mut rect = None;
    update_model(cx, |m, rt, _| {
        previous = m.playing.take();
        rect = rt.rect;
    });
    if let Some(previous) = &previous {
        drive::note(format!("grid {previous}"));
        if let Some(backend) = backend(cx) {
            backend.send(previous, ToMod::Hide);
        }
    }
    super::main_window::show(rect, cx);
}

fn step(action: HotkeyAction, cx: &mut App) {
    if action == HotkeyAction::Grid {
        return show_grid(cx);
    }
    let model = model::get(cx);
    let target = {
        let m = model.read(cx);
        let order: Vec<&Instance> = m.sorted(None).into_iter().filter(|i| playable(&i.phase)).collect();
        let at = m.playing.as_ref().and_then(|p| order.iter().position(|i| i.key == *p));
        let n = order.len();
        let pick = |from: usize, forward: bool, want: &dyn Fn(&Instance) -> bool| -> Option<String> {
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
    let i = m.instances.get(key)?;
    Some(fleet::preview_path(m.batch(&i.batch_id)?, i.account_id))
}

enum Deferred {
    Reconnect { key: String, delay: Duration, generation: u64 },
    Restart { key: String, delay: Duration, generation: u64 },
    Join { key: String, generation: u64, lines: Vec<String>, delay: Duration },
}

#[derive(Default)]
struct Effects {
    later: Vec<Deferred>,
    hotkeys: Vec<HotkeyAction>,
    exited_playing: bool,
    retired: Vec<Arc<RenderImage>>,
    changed_history: Vec<String>,
    stop: Vec<String>,
    whispers: Vec<Whisper>,
}

fn apply_all(updates: Vec<Update>, cx: &mut App) {
    if updates.iter().all(|u| matches!(u, Update::Preview { .. })) {
        return apply_previews(updates, cx);
    }
    let mut fresh: Vec<(String, String, Alert)> = Vec::new();
    let mut fx = Effects::default();
    update_model(cx, |m, rt, _| {
        for update in updates {
            let key = match &update {
                Update::Preparing { .. } | Update::Progress { .. } | Update::SignIn { .. } => None,
                Update::Launched { key } | Update::Failed { key, .. } | Update::Exited { key, .. } | Update::Preview { key, .. } | Update::Seed { key, .. } => Some(key.clone()),
                Update::Ipc(Event::Connected { instance, .. } | Event::Message { instance, .. } | Event::Disconnected { instance }) => Some(instance.clone()),
            };
            let before = key.as_ref().and_then(|k| m.instances.get(k)).map(|i| (rank(&i.phase), i.phase.needs_attention()));
            let seeded = matches!(update, Update::Seed { .. });
            apply(m, rt, update, &mut fx);
            let Some(key) = key else { continue };
            let Some(i) = m.instances.get(&key) else { continue };
            let after = (rank(&i.phase), i.phase.needs_attention());
            if before.is_some_and(|b| b.0 != after.0) {
                m.order_seq += 1;
            }
            if after.1 && !seeded && before.is_some_and(|b| !b.1) {
                fresh.push((i.batch_id.clone(), i.name.clone(), Alert::of(&i.phase)));
            }
        }
    });
    for image in fx.retired {
        cx.drop_image(image, None);
    }
    save_histories(&fx.changed_history, cx);
    if !fx.stop.is_empty()
        && let Some(backend) = backend(cx)
    {
        backend.stop(fx.stop);
    }
    for deferred in fx.later {
        match deferred {
            Deferred::Reconnect { key, delay, generation } => schedule_reconnect(key, delay, generation, cx),
            Deferred::Restart { key, delay, generation } => schedule_restart(key, delay, generation, cx),
            Deferred::Join { key, generation, lines, delay } => schedule_join(key, generation, lines, delay, cx),
        }
    }
    for (batch, name, kind) in fresh {
        queue_alert(batch, name, kind, cx);
    }
    for whisper in fx.whispers {
        queue_whisper(whisper, cx);
    }
    if fx.exited_playing {
        show_grid(cx);
    }
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
        let Some(i) = m.instances.get(key) else { continue };
        let (Some(batch), Some(log)) = (m.batch(&i.batch_id), m.history.get(key)) else { continue };
        writer.save(history::path(batch, i.account_id), log.clone());
    }
}

fn apply_previews(updates: Vec<Update>, cx: &mut App) {
    let model = model::get(cx);
    let mut shown = Vec::new();
    let mut retired = Vec::new();
    model.update(cx, |m, _| {
        for update in updates {
            let Update::Preview { key, seq, image } = update else { continue };
            match m.instances.get_mut(&key) {
                Some(i) if i.phase.is_running() => {
                    if let Some(old) = i.preview.replace(Arc::clone(&image)) {
                        retired.push(old);
                    }
                    i.preview_seq = seq;
                    shown.push((key, image));
                }
                _ => retired.push(image),
            }
        }
    });
    super::tile::show_previews(shown, cx);
    for image in retired {
        cx.drop_image(image, None);
    }
}

fn rank(phase: &Phase) -> u8 {
    if phase.needs_attention() {
        0
    } else if phase.is_running() {
        1
    } else {
        2
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
        Update::Progress { batch, done, total } => {
            m.preparing.remove(&batch);
            for i in m.instances.values_mut().filter(|i| i.batch_id == batch) {
                if let Phase::Starting { total: queued, .. } = i.phase
                    && queued > 0
                {
                    i.phase = Phase::Starting { done, total };
                }
            }
        }
        Update::Launched { key } => {
            if let Some(i) = m.instances.get_mut(&key)
                && matches!(i.phase, Phase::Starting { .. } | Phase::Failed { .. })
                && !i.stopping
            {
                i.phase = Phase::Starting { done: 0, total: 0 };
            }
        }
        Update::Failed { key, reason, login } => {
            if let Some(i) = m.instances.get_mut(&key) {
                m.preparing.remove(&i.batch_id);
                i.phase = if login { Phase::LoginNeeded } else { Phase::Failed { reason } };
            }
        }
        Update::Seed { key, phase, attempts } => {
            if let Some(i) = m.instances.get_mut(&key) {
                i.phase = phase;
                i.reconnect_attempts = attempts;
            }
        }
        Update::SignIn { account, valid } => {
            for i in m.instances.values_mut().filter(|i| i.account_id == account) {
                if !valid && matches!(i.phase, Phase::Stopped | Phase::Crashed { .. } | Phase::Failed { .. }) {
                    i.phase = Phase::LoginNeeded;
                } else if valid && i.phase == Phase::LoginNeeded && !rt.pids.contains_key(&i.key) {
                    i.phase = Phase::Stopped;
                }
            }
            m.order_seq += 1;
        }
        Update::Exited { key, code } => on_exit(m, rt, fx, key, code),
        Update::Preview { key, seq, image } => match m.instances.get_mut(&key) {
            Some(i) if i.phase.is_running() => {
                if let Some(old) = i.preview.replace(image) {
                    fx.retired.push(old);
                }
                i.preview_seq = seq;
            }
            _ => fx.retired.push(image),
        },
        Update::Ipc(Event::Connected { instance, pid, .. }) => {
            rt.pids.insert(instance.clone(), pid);
            if let Some(i) = m.instances.get_mut(&instance)
                && matches!(i.phase, Phase::Starting { .. })
            {
                i.phase = Phase::Menu;
            }
        }
        Update::Ipc(Event::Disconnected { .. }) => {}
        Update::Ipc(Event::Message { instance, message }) => on_message(m, rt, instance, message, fx),
    }
}

fn on_exit(m: &mut Model, rt: &mut Runtime, fx: &mut Effects, key: String, code: Option<i32>) {
    rt.pids.remove(&key);
    rt.joined.remove(&key);
    let ran = rt.started.remove(&key).map(|t| t.elapsed());
    let Some(i) = m.instances.get_mut(&key) else { return };
    *rt.joins.entry(key.clone()).or_default() += 1;
    let crashed = !(i.stopping || code == Some(0) || matches!(i.phase, Phase::Stopped | Phase::LoginNeeded | Phase::Restarting { .. }));
    i.stopping = false;
    if let Some(old) = i.preview.take() {
        fx.retired.push(old);
    }
    if m.playing.as_deref() == Some(key.as_str()) {
        fx.exited_playing = true;
    }
    if !crashed {
        if !matches!(i.phase, Phase::LoginNeeded | Phase::Restarting { .. }) {
            i.phase = Phase::Stopped;
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
    let batch = m.batch(&m.instances.get(key)?.batch_id)?;
    let lines = batch.join_lines();
    (!lines.is_empty()).then(|| (lines, Duration::from_millis(u64::from(batch.join_delay_ms))))
}

fn on_state(m: &mut Model, rt: &mut Runtime, fx: &mut Effects, key: String, kind: StateKind, server: Option<String>, reason: Option<String>, cause: Option<DisconnectCause>) {
    if kind != StateKind::Playing {
        *rt.joins.entry(key.clone()).or_default() += 1;
    }
    let offline = m.instances.get(&key).and_then(|i| m.account(i.account_id)).is_some_and(|a| a.kind == AccountKind::Offline);
    let join = if kind == StateKind::Playing { join_lines_of(m, &key) } else { None };
    let Some(i) = m.instances.get_mut(&key) else { return };
    if i.stopping || i.phase == Phase::Stopped {
        return;
    }
    let policy = &settings::active().reconnect;
    let server_text = server.clone().unwrap_or_default();
    let reason_text = reason.clone().unwrap_or_default();
    let mut event: Option<(Kind, Option<String>, String)> = None;
    i.phase = match kind {
        StateKind::Menu => Phase::Menu,
        StateKind::Connecting => Phase::Connecting { server: server_text },
        StateKind::Queue => Phase::Queue { server: server_text },
        StateKind::Playing => {
            if let Phase::Playing { since, .. } = i.phase {
                Phase::Playing { server: server_text, since }
            } else {
                rt.started.entry(key.clone()).or_insert_with(Instant::now);
                if !matches!(i.phase, Phase::Dead { .. }) && !server_text.is_empty() && rt.joined.insert(key.clone()) {
                    event = Some((Kind::Connected, server.clone(), String::new()));
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
            let reaction = match reconnect::react(cause, i.reconnect_attempts, policy, reconnect::unit()) {
                Reaction::Retry(_) if server.is_none() => Reaction::GiveUp,
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
                    i.reconnect_attempts += 1;
                    fx.later.push(Deferred::Reconnect { key: key.clone(), delay, generation: *generation });
                    Phase::Connecting { server: server_text }
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
                            fx.later.push(Deferred::Restart { key: key.clone(), delay: Duration::from_secs(5), generation: *generation });
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

fn on_message(m: &mut Model, rt: &mut Runtime, key: String, message: FromMod, fx: &mut Effects) {
    let playing = m.playing.as_deref() == Some(key.as_str());
    match message {
        FromMod::State { kind, server, reason, cause } => on_state(m, rt, fx, key, kind, server, reason, cause),
        FromMod::Chat { kind, from, text } => {
            let whisper = kind == ChatKind::Whisper;
            if whisper
                && !playing
                && settings::active().merged_chat
                && let Some(i) = m.instances.get_mut(&key)
            {
                i.unread_whispers += 1;
            }
            let (account, batch) = m.instances.get(&key).map(|i| (i.name.clone(), i.batch_id.clone())).unwrap_or_default();
            if whisper && !playing {
                fx.whispers.push(Whisper { account: account.clone(), from: from.clone(), text: text.clone() });
            }
            if settings::active().merged_chat {
                let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                m.push_chat(ChatLine { instance: key, account, batch, at, whisper, from, text }, KEPT_CHAT);
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
            if playing || action == HotkeyAction::Grid {
                fx.hotkeys.push(action);
            }
        }
        FromMod::Window { .. } | FromMod::Hello { .. } => {}
    }
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
            if model.read(cx).instances.get(&key).is_some_and(|i| matches!(i.phase, Phase::Connecting { .. }) && !i.stopping) {
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
                m.instances.get(&key).filter(|i| matches!(i.phase, Phase::Restarting { .. }) && !i.stopping).map(|i| i.batch_id.clone())
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
                if !model.read(cx).instances.get(&key).is_some_and(|i| matches!(i.phase, Phase::Playing { .. }) && !i.stopping) {
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
        let open: Vec<&(String, Alert)> = list.iter().filter(|(name, _)| m.instances.values().any(|i| i.batch_id == batch_id && i.name == *name && i.phase.needs_attention())).collect();
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
    if !cx.has_global::<Runtime>() || !settings::active().notifications.whispers {
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

/// The phase of an instance as one line of German for its tile.
pub fn status(i: &Instance, preparing: Option<u8>) -> String {
    if i.stopping {
        return tr("Wird gestoppt…");
    }
    match &i.phase {
        Phase::Stopped => tr("Gestoppt"),
        Phase::Starting { .. } if preparing.is_some() => trf("Lädt Minecraft · %lld %%", &[&preparing.unwrap_or(0)]),
        Phase::Restarting { attempt, of } => model::restart_status(*attempt, *of),
        Phase::Starting { done, total } if *total > 1 => trf("Wartet · %lld/%lld gestartet", &[done, total]),
        Phase::Starting { .. } => tr("Startet…"),
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
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::{AccountKind, Account, AuthError, Backend, Batch, Deferred, DisconnectCause, Effects, Event, FromMod, Instance, Instant, Kind, Model, Phase, Runtime, StateKind, ToMod, Update, apply, fleet, needs_sign_in, render_image, status};

    fn instance(phase: Phase) -> Instance {
        Instance { key: "k".into(), batch_id: "b".into(), account_id: uuid::Uuid::nil(), name: "Alex".into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, stopping: false }
    }

    #[test]
    fn status_lines_name_the_phase() {
        crate::core::i18n::init("de");
        assert_eq!(status(&instance(Phase::Stopped), None), "Gestoppt");
        assert_eq!(status(&instance(Phase::Starting { done: 12, total: 50 }), None), "Wartet · 12/50 gestartet");
        assert_eq!(status(&instance(Phase::Starting { done: 0, total: 0 }), None), "Startet…");
        assert_eq!(status(&instance(Phase::Starting { done: 1, total: 4 }), Some(45)), "Lädt Minecraft · 45 %");
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

    struct Quiet;

    impl Backend for Quiet {
        fn start(&self, _: Batch, _: Vec<Account>, _: Arc<AtomicBool>) {}
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
            alerts: HashMap::new(),
            pids: HashMap::new(),
            rect: None,
            badge: 0,
            whispers: Vec::new(),
        }
    }

    fn setup(phase: Phase, kind: AccountKind, join: &[&str]) -> (Model, Runtime, String) {
        let id = uuid::Uuid::from_u128(7);
        let mut batch = Batch::new("b", "26.3");
        batch.accounts = vec![id];
        batch.join_commands = join.iter().map(|s| (*s).to_owned()).collect();
        let key = fleet::instance_key(&batch, id);
        let mut m = Model::default();
        m.accounts.push(Account { id, name: "AltOne".into(), kind, proxy: None });
        m.instances.insert(key.clone(), Instance { key: key.clone(), batch_id: batch.id.clone(), account_id: id, name: "AltOne".into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, stopping: false });
        m.batches.push(batch);
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
        (m.instances[&key].phase.clone(), fx, m)
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
            assert!(matches!(m.instances[&key].phase, Phase::Connecting { .. }));
            assert_eq!(m.instances[&key].reconnect_attempts, attempt);
            let Some(Deferred::Reconnect { delay, .. }) = fx.later.pop() else { panic!("no reconnect scheduled") };
            let base = [10.0, 30.0, 90.0, 300.0, 300.0][attempt as usize - 1];
            assert!(delay.as_secs_f64() >= base * 0.75 - 0.01 && delay.as_secs_f64() <= base * 1.25 + 0.01, "{delay:?} for try {attempt}");
        }
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(matches!(m.instances[&key].phase, Phase::Disconnected { .. }) && fx.later.is_empty());
        assert_eq!(m.history[&key].len(), 6);
    }

    #[test]
    fn a_kick_waits_longer_than_a_server_that_is_down() {
        let (phase, mut fx, _) = dropped(DisconnectCause::Kicked);
        assert!(matches!(phase, Phase::Connecting { .. }));
        let Some(Deferred::Reconnect { delay, .. }) = fx.later.pop() else { panic!("no reconnect scheduled") };
        assert!(delay.as_secs() >= 45);
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
            assert_eq!(m.instances[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert_eq!(fx.stop.len(), 1);
            assert!(matches!(fx.later.pop(), Some(Deferred::Restart { .. })));
            apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut Effects::default());
            assert_eq!(m.instances[&key].phase, Phase::Restarting { attempt, of: 3 });
            m.instances.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert_eq!(m.instances[&key].phase, Phase::LoginNeeded);
        assert!(fx.later.is_empty());
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert!(matches!(m.instances[&key].phase, Phase::Disconnected { .. }) && fx.stop.is_empty());
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
        assert_eq!(m.instances[&key].phase, Phase::Crashed { code: Some(1) });
        assert!(fx.later.is_empty());
    }

    #[test]
    fn a_server_switch_does_not_send_the_join_commands_again() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Offline, &["/login pw"]);
        let mut fx = Effects::default();
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.later.len(), 1);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Menu, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Connecting, None);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.later.len(), 1);
        assert_eq!(m.history[&key].len(), 1);
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        send_state(&mut m, &mut rt, &mut fx, &key, StateKind::Playing, None);
        assert_eq!(fx.later.iter().filter(|d| matches!(d, Deferred::Join { .. })).count(), 2);
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
        assert_eq!(m.instances[&key].phase, Phase::LoginNeeded);
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
            assert_eq!(m.instances[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert!(!m.instances[&key].phase.needs_attention());
            assert!(matches!(fx.later.pop(), Some(Deferred::Restart { .. })));
            m.instances.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut fx);
        assert_eq!(m.instances[&key].phase, Phase::Crashed { code: Some(1) });
        assert!(fx.later.is_empty());
        assert_eq!(m.history[&key].iter().filter(|e| e.kind == Kind::Crashed).count(), 4);
    }

    #[test]
    fn a_long_run_resets_the_crash_count() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        rt.crash_restarts.insert(key.clone(), 3);
        rt.started.insert(key.clone(), Instant::now() - Duration::from_secs(120));
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut Effects::default());
        assert_eq!(m.instances[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
    }

    #[test]
    fn clean_and_requested_exits_do_not_restart() {
        let (mut m, mut rt, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let mut fx = Effects::default();
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(0) }, &mut fx);
        assert_eq!(m.instances[&key].phase, Phase::Stopped);
        m.instances.get_mut(&key).unwrap().phase = Phase::Menu;
        m.instances.get_mut(&key).unwrap().stopping = true;
        apply(&mut m, &mut rt, Update::Exited { key: key.clone(), code: Some(1) }, &mut fx);
        assert_eq!(m.instances[&key].phase, Phase::Stopped);
        assert!(fx.later.is_empty() && m.history.is_empty());
    }

    #[test]
    fn a_refresh_result_moves_idle_instances_in_and_out_of_login_needed() {
        let (mut m, mut rt, key) = setup(Phase::Stopped, AccountKind::Microsoft, &[]);
        let id = uuid::Uuid::from_u128(7);
        apply(&mut m, &mut rt, Update::SignIn { account: id, valid: false }, &mut Effects::default());
        assert_eq!(m.instances[&key].phase, Phase::LoginNeeded);
        apply(&mut m, &mut rt, Update::SignIn { account: id, valid: true }, &mut Effects::default());
        assert_eq!(m.instances[&key].phase, Phase::Stopped);
        m.instances.get_mut(&key).unwrap().phase = Phase::Menu;
        apply(&mut m, &mut rt, Update::SignIn { account: id, valid: false }, &mut Effects::default());
        assert_eq!(m.instances[&key].phase, Phase::Menu);
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
}
