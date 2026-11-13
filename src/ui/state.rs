//! Runs the app behind the views: loads accounts and batches, drives the fleet, turns mod events into model changes and handles the actions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::actions::{Broadcast, NextAttention, NextInstance, Play, PreviousInstance, Reconnect, Reload, ShowGrid, Start, StartAll, StartBatch, Stop, StopAll, StopBatch};
use super::model::{self, ChatLine, Instance, Model, Phase};
use super::native::{self, Rect};
use crate::core::account::{self, Account};
use crate::core::auth::AuthError;
use crate::core::batch::{self, Batch};
use crate::core::fleet::{self, Fleet};
use crate::core::i18n::{tr, trf};
use crate::core::ipc::{ChatKind, Event, FromMod, HotkeyAction, StateKind, ToMod};
use crate::core::{companion, drive, install, ipc};

const BACKOFF: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(45)];
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
}

struct Real {
    fleet: Arc<Fleet>,
    out: async_channel::Sender<Update>,
    watch: Arc<Mutex<HashSet<String>>>,
    owners: Arc<Mutex<HashMap<String, u64>>>,
    runs: AtomicU64,
    sends: mpsc::Sender<(String, ToMod)>,
    previews: mpsc::Sender<(String, PathBuf)>,
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
        Ok(Self { fleet, out, watch, owners: Arc::default(), runs: AtomicU64::new(0), sends, previews })
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
    Crashed,
    Login,
    Other,
}

impl Alert {
    fn of(phase: &Phase) -> Self {
        match phase {
            Phase::Disconnected { .. } => Self::Disconnected,
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
    alerts: HashMap<String, Vec<(String, Alert)>>,
    pids: HashMap<String, u32>,
    rect: Option<Rect>,
    badge: usize,
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
        alerts: HashMap::new(),
        pids: HashMap::new(),
        rect: None,
        badge: 0,
    });
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
    cx.on_action(|a: &Play, cx| {
        let key = a.0.clone();
        cx.defer(move |cx| play(&key, cx));
    });
    cx.on_action(|_: &ShowGrid, cx| cx.defer(show_grid));
    cx.on_action(|_: &NextInstance, cx| cx.defer(|cx| step(HotkeyAction::Next, cx)));
    cx.on_action(|_: &PreviousInstance, cx| cx.defer(|cx| step(HotkeyAction::Prev, cx)));
    cx.on_action(|_: &NextAttention, cx| cx.defer(|cx| step(HotkeyAction::NextAttention, cx)));
    cx.on_action(|_: &Reload, cx| reload(cx));
    cx.on_action(|a: &Broadcast, cx| broadcast(&a.batch, &a.text, cx));
    cx.on_system_notification_response(|response, cx| {
        let batch = response.tag.strip_prefix("attention-").map(str::to_owned);
        cx.defer(move |cx| {
            show_grid(cx);
            if let Some(batch) = batch {
                super::main_window::select_batch(&batch, cx);
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
    let task = cx.background_spawn(async { (account::load(), batch::load_all()) });
    cx.spawn(async move |cx| {
        let (accounts, batches) = task.await;
        cx.update(|cx| {
            let model = model::get(cx);
            let mut retired = Vec::new();
            model.update(cx, |m, cx| {
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
            if let Some(rt) = cx.try_global::<Runtime>() {
                rt.backend.known(known);
            }
        });
    })
    .detach();
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
        launch(&batch, Some(&keys), cx);
    }
}

fn start_batch(batch: &str, cx: &mut App) {
    launch(batch, None, cx);
}

fn launch(batch_id: &str, only: Option<&[String]>, cx: &mut App) {
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
                only.is_none_or(|o| o.contains(&key)) && m.instances.get(&key).is_some_and(|i| !i.phase.is_running())
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
            if let Some(i) = m.instances.get_mut(&fleet::instance_key(&batch, account.id)) {
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
                Phase::Crashed { .. } | Phase::Failed { .. } => i.phase = Phase::Stopped,
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
        i.reconnect_attempts = 0;
        i.phase = Phase::Connecting { server: server_of(&i.phase).unwrap_or_default() };
        m.order_seq += 1;
        send = true;
    });
    if send && let Some(backend) = backend(cx) {
        backend.send(&key, ToMod::Reconnect);
    }
}

fn server_of(phase: &Phase) -> Option<String> {
    match phase {
        Phase::Connecting { server } | Phase::Queue { server } | Phase::Playing { server, .. } => Some(server.clone()),
        Phase::Disconnected { server, .. } | Phase::Dead { server, .. } => server.clone(),
        _ => None,
    }
}

fn broadcast(batch: &str, text: &str, cx: &mut App) {
    let Some(backend) = backend(cx) else { return };
    let model = model::get(cx);
    for i in model.read(cx).instances.values().filter(|i| i.batch_id == batch && i.phase.is_running()) {
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

fn apply_all(updates: Vec<Update>, cx: &mut App) {
    if updates.iter().all(|u| matches!(u, Update::Preview { .. })) {
        return apply_previews(updates, cx);
    }
    let mut fresh: Vec<(String, String, Alert)> = Vec::new();
    let mut later: Vec<(String, Duration, u64)> = Vec::new();
    let mut hotkeys: Vec<HotkeyAction> = Vec::new();
    let mut exited_playing = false;
    let mut retired: Vec<Arc<RenderImage>> = Vec::new();
    update_model(cx, |m, rt, _| {
        for update in updates {
            let key = match &update {
                Update::Preparing { .. } | Update::Progress { .. } => None,
                Update::Launched { key } | Update::Failed { key, .. } | Update::Exited { key, .. } | Update::Preview { key, .. } | Update::Seed { key, .. } => Some(key.clone()),
                Update::Ipc(Event::Connected { instance, .. } | Event::Message { instance, .. } | Event::Disconnected { instance }) => Some(instance.clone()),
            };
            let before = key.as_ref().and_then(|k| m.instances.get(k)).map(|i| (rank(&i.phase), i.phase.needs_attention()));
            let seeded = matches!(update, Update::Seed { .. });
            apply(m, rt, update, &mut later, &mut hotkeys, &mut exited_playing, &mut retired);
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
    for image in retired {
        cx.drop_image(image, None);
    }
    for (key, delay, generation) in later {
        schedule_reconnect(key, delay, generation, cx);
    }
    for (batch, name, kind) in fresh {
        queue_alert(batch, name, kind, cx);
    }
    if exited_playing {
        show_grid(cx);
    }
    for action in hotkeys {
        step(action, cx);
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

fn apply(m: &mut Model, rt: &mut Runtime, update: Update, later: &mut Vec<(String, Duration, u64)>, hotkeys: &mut Vec<HotkeyAction>, exited_playing: &mut bool, retired: &mut Vec<Arc<RenderImage>>) {
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
        Update::Exited { key, code } => {
            rt.pids.remove(&key);
            let Some(i) = m.instances.get_mut(&key) else { return };
            i.phase = if i.stopping || code == Some(0) || i.phase == Phase::Stopped { Phase::Stopped } else { Phase::Crashed { code } };
            i.stopping = false;
            if let Some(old) = i.preview.take() {
                retired.push(old);
            }
            if m.playing.as_deref() == Some(key.as_str()) {
                *exited_playing = true;
            }
        }
        Update::Preview { key, seq, image } => match m.instances.get_mut(&key) {
            Some(i) if i.phase.is_running() => {
                if let Some(old) = i.preview.replace(image) {
                    retired.push(old);
                }
                i.preview_seq = seq;
            }
            _ => retired.push(image),
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
        Update::Ipc(Event::Message { instance, message }) => on_message(m, rt, instance, message, later, hotkeys),
    }
}

fn on_message(m: &mut Model, rt: &mut Runtime, key: String, message: FromMod, later: &mut Vec<(String, Duration, u64)>, hotkeys: &mut Vec<HotkeyAction>) {
    let playing = m.playing.as_deref() == Some(key.as_str());
    match message {
        FromMod::State { kind, server, reason } => {
            let Some(i) = m.instances.get_mut(&key) else { return };
            if i.stopping || i.phase == Phase::Stopped {
                return;
            }
            let server_text = server.clone().unwrap_or_default();
            i.phase = match kind {
                StateKind::Menu => Phase::Menu,
                StateKind::Connecting => Phase::Connecting { server: server_text },
                StateKind::Queue => Phase::Queue { server: server_text },
                StateKind::Playing => {
                    if let Phase::Playing { since, .. } = i.phase {
                        Phase::Playing { server: server_text, since }
                    } else {
                        Phase::Playing { server: server_text, since: Instant::now() }
                    }
                }
                StateKind::Dead => Phase::Dead { server, reason: reason.unwrap_or_default() },
                StateKind::Disconnected => {
                    if let Phase::Playing { since, .. } = i.phase
                        && since.elapsed() >= STABLE
                    {
                        i.reconnect_attempts = 0;
                    }
                    let attempt = i.reconnect_attempts as usize;
                    if attempt < BACKOFF.len() && server.is_some() {
                        i.reconnect_attempts += 1;
                        let generation = rt.reconnects.entry(key.clone()).or_default();
                        *generation += 1;
                        later.push((key.clone(), BACKOFF[attempt], *generation));
                        Phase::Connecting { server: server_text }
                    } else {
                        Phase::Disconnected { server, reason: reason.unwrap_or_default() }
                    }
                }
            };
        }
        FromMod::Chat { kind, from, text } => {
            let whisper = kind == ChatKind::Whisper;
            if whisper
                && !playing
                && let Some(i) = m.instances.get_mut(&key)
            {
                i.unread_whispers += 1;
            }
            if m.chat.len() >= KEPT_CHAT {
                m.chat.drain(..KEPT_CHAT / 10);
            }
            m.chat.push(ChatLine { instance: key, whisper, from, text });
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
                hotkeys.push(action);
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
        Phase::Starting { done, total } if *total > 1 => trf("Wartet · %lld/%lld gestartet", &[done, total]),
        Phase::Starting { .. } => tr("Startet…"),
        Phase::Menu => tr("Im Hauptmenü"),
        Phase::Connecting { .. } if i.reconnect_attempts > 0 => trf("Verbindet neu · Versuch %lld von %lld", &[&i.reconnect_attempts, &BACKOFF.len()]),
        Phase::Connecting { server } if server.is_empty() => tr("Verbindet…"),
        Phase::Connecting { server } => trf("Verbindet mit %@…", &[server]),
        Phase::Queue { .. } => tr("In der Warteschlange"),
        Phase::Playing { server, .. } if server.is_empty() => tr("Spielt"),
        Phase::Playing { server, .. } => trf("Auf %@", &[server]),
        Phase::Disconnected { reason, .. } if reason.is_empty() => tr("Getrennt"),
        Phase::Disconnected { reason, .. } => trf("Getrennt · %@", &[reason]),
        Phase::Dead { .. } => tr("Gestorben"),
        Phase::Crashed { .. } => tr("Abgestürzt"),
        Phase::LoginNeeded => tr("Anmeldung nötig"),
        Phase::Failed { reason } => trf("Start fehlgeschlagen · %@", &[reason]),
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthError, Instance, Instant, Phase, needs_sign_in, render_image, status};

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
        assert_eq!(status(&again, None), "Verbindet neu · Versuch 2 von 3");
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
}
