//! Runs the app behind the views: loads accounts and batches, drives the fleet, turns mod events into model changes and handles the actions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::actions::{Broadcast, CancelStart, CloneInstance, NextAttention, NextInstance, OpenChat, OpenFromTray, Play, PlayInstance, PreviousInstance, Quit, QuitAll, Reconnect, ReconnectMany, Reload, Respawn, RespawnMany, RestartMany, SendChat, SendChatMany, SetColor, ShowGrid, Start, StartAll, StartAnyway, StartBatch, StartMany, Stop, StopAll, StopBatch, StopMany};
use super::model::{self, Game, Model, Phase};
use super::native;
use crate::app::effect::{Effect, Redraw, Unshown};
use crate::app::event::{Event as AppEvent, Loaded, Report};
use crate::app::reduce::{Env, WHISPER_TAG, reduce};
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
use crate::core::ipc::{Event, FromMod, HotkeyAction, ToMod};
use crate::core::place::{self, Insets, Place, Rect};
use crate::core::proxy::{self, Fault};
use crate::core::phone;
use crate::core::settings;
use crate::core::store::{self, Recovery};
use crate::core::{adopt, content, drive, install, ipc, net, online};

pub use crate::app::reduce::solo_account;

const COALESCE: Duration = Duration::from_millis(40);
const EXIT_POLL: Duration = Duration::from_millis(500);
const STAGGER_STEP: Duration = Duration::from_millis(100);
const QUIT_WAIT: Duration = Duration::from_secs(5);
const TRASH_KEPT: Duration = Duration::from_secs(600);

enum PreviewJob {
    Read(String, PathBuf),
    Release(String),
}

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
    previews: mpsc::Sender<PreviewJob>,
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
        let (previews, wanted) = mpsc::channel::<PreviewJob>();
        let frames = out.clone();
        std::thread::Builder::new().name("previews".into()).spawn(move || {
            let mut reader = ipc::PreviewReader::default();
            let mut changes = ipc::PreviewChanges::default();
            while let Ok(first) = wanted.recv() {
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
                    let Some(image) = render_image(preview.width, preview.height, preview.rgba) else { continue };
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
        std::thread::Builder::new().name("fleet-send".into()).spawn(move || {
            while let Ok((key, message)) = outgoing.recv() {
                let _ = sender.send(&key, &message);
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

    fn leave(&self) {
        self.fleet.close();
        let until = Instant::now() + QUIT_WAIT;
        while self.fleet.launching() && Instant::now() < until {
            std::thread::sleep(STAGGER_STEP);
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

struct Runtime {
    backend: Arc<dyn Backend>,
    cancel: HashMap<String, Vec<Arc<AtomicBool>>>,
    shared_visible: Arc<Mutex<HashSet<String>>>,
    history: Option<history::Writer>,
    place: Option<Place>,
    insets: HashMap<String, Insets>,
    spread: Option<Rect>,
    badge: usize,
    outbox: Option<phone::Outbox>,
    /// Whether the games keep running when Manycraft quits now; true unless the user chose to end them, so a quit from outside keeps them.
    keep_games: bool,
}

impl Global for Runtime {}

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
    cx.set_global(Runtime { backend, cancel: HashMap::new(), shared_visible, history: history::Writer::start().ok(), place: None, insets: HashMap::new(), spread: None, badge: 0, outbox: None, keep_games: true });
    if let Err(e) = settings::activate() {
        eprintln!("settings: {e:#}");
        dispatch(AppEvent::Notice(super::forms::untouched(&e).unwrap_or_else(|| format!("{e:#}"))), cx);
    }
    cx.background_spawn(async {
        if !crate::core::updater::install::running_alone() {
            return;
        }
        if let Err(e) = instance::purge_stale(TRASH_KEPT) {
            eprintln!("trash: {e:#}");
        }
        if let Err(e) = content::purge_stale(TRASH_KEPT) {
            eprintln!("mod trash: {e:#}");
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
    cx.on_action(|a: &Start, cx| dispatch(AppEvent::Start(vec![a.0.clone()]), cx));
    cx.on_action(|a: &Stop, cx| dispatch(AppEvent::Stop(vec![a.0.clone()]), cx));
    cx.on_action(|a: &StartBatch, cx| dispatch(AppEvent::StartBatch(a.0.clone()), cx));
    cx.on_action(|a: &StopBatch, cx| dispatch(AppEvent::StopBatch(a.0.clone()), cx));
    cx.on_action(|_: &StartAll, cx| dispatch(AppEvent::StartAll, cx));
    cx.on_action(|_: &StopAll, cx| dispatch(AppEvent::StopAll, cx));
    cx.on_action(|_: &Quit, cx| cx.defer(|cx| dispatch(AppEvent::Quit, cx)));
    cx.on_action(|_: &QuitAll, cx| cx.defer(|cx| dispatch(AppEvent::QuitChosen { keep: false }, cx)));
    cx.on_action(|_: &OpenFromTray, cx| {
        cx.defer(|cx| {
            super::main_window::unstow(cx);
            show_grid(cx);
        })
    });
    cx.on_action(|a: &CancelStart, cx| dispatch(AppEvent::CancelStart(a.0.clone()), cx));
    cx.on_action(|a: &StartAnyway, cx| dispatch(AppEvent::StartAnyway(a.0.clone()), cx));
    cx.on_action(|a: &Reconnect, cx| dispatch(AppEvent::Reconnect(a.0.clone()), cx));
    cx.on_action(|a: &Respawn, cx| dispatch(AppEvent::Respawn(a.0.clone()), cx));
    cx.on_action(|a: &RespawnMany, cx| {
        for key in &a.0 {
            dispatch(AppEvent::Respawn(key.clone()), cx);
        }
    });
    cx.on_action(|a: &Play, cx| {
        let key = a.0.clone();
        cx.defer(move |cx| dispatch(AppEvent::Play(key), cx));
    });
    cx.on_action(|a: &PlayInstance, cx| {
        let instance = a.instance.clone();
        let account = a.account.as_deref().and_then(|id| id.parse().ok());
        cx.defer(move |cx| dispatch(AppEvent::PlayInstance { instance, account }, cx));
    });
    cx.on_action(|a: &CloneInstance, cx| clone_instance(&a.0, cx));
    cx.on_action(|_: &ShowGrid, cx| cx.defer(show_grid));
    cx.on_action(|_: &NextInstance, cx| cx.defer(|cx| dispatch(AppEvent::Step(HotkeyAction::Next), cx)));
    cx.on_action(|_: &PreviousInstance, cx| cx.defer(|cx| dispatch(AppEvent::Step(HotkeyAction::Prev), cx)));
    cx.on_action(|_: &NextAttention, cx| cx.defer(|cx| dispatch(AppEvent::Step(HotkeyAction::NextAttention), cx)));
    cx.on_action(|_: &Reload, cx| reload(cx));
    cx.on_action(|_: &OpenChat, cx| cx.defer(super::chat::open));
    cx.on_action(|a: &Broadcast, cx| dispatch(AppEvent::Broadcast { batch: a.batch.clone(), text: a.text.clone() }, cx));
    cx.on_action(|a: &SendChat, cx| dispatch(AppEvent::SendChat { key: a.key.clone(), text: a.text.clone() }, cx));
    cx.on_action(|a: &StartMany, cx| dispatch(AppEvent::StartMany(a.0.clone()), cx));
    cx.on_action(|a: &StopMany, cx| dispatch(AppEvent::Stop(a.0.clone()), cx));
    cx.on_action(|a: &RestartMany, cx| dispatch(AppEvent::Restart(a.0.clone()), cx));
    cx.on_action(|a: &ReconnectMany, cx| dispatch(AppEvent::ReconnectMany(a.0.clone()), cx));
    cx.on_action(|a: &SetColor, cx| set_color(a, cx));
    cx.on_action(|a: &SendChatMany, cx| {
        for key in &a.keys {
            dispatch(AppEvent::SendChat { key: key.clone(), text: a.text.clone() }, cx);
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
            let (backend, keep) = (Arc::clone(&rt.backend), rt.keep_games);
            let end = move |backend: &dyn Backend| if keep { backend.leave() } else { backend.quit() };
            match std::thread::Builder::new().name("quit".into()).spawn(move || end(&*backend)) {
                Ok(closing) => close_games(closing, cx),
                Err(_) => end(&*rt.backend),
            }
        }
        async {}
    })
    .detach();
}

/// Quits for the restart that installs an update, keeping the games for the new version to find again.
pub fn restart_for_update(cx: &mut App) {
    dispatch(AppEvent::RestartForUpdate, cx);
}

/// Quits and ends every game, for going back to a version that may not find them again.
pub fn quit_ending_games(cx: &mut App) {
    dispatch(AppEvent::QuitChosen { keep: false }, cx);
}

/// Closing the main window: it quits, or goes to the notification area while games run.
pub fn close_window(cx: &mut App) {
    dispatch(AppEvent::CloseWindow, cx);
}

fn explain_tray(cx: &mut App) {
    if !super::tray::show(cx) {
        super::main_window::stow(cx);
        return;
    }
    super::main_window::in_main(cx, |window, cx| {
        if super::sheet::busy(window, cx) {
            cx.defer(super::main_window::stow);
            return;
        }
        let place = if cfg!(target_os = "windows") { tr("Solange Spiele laufen, findest du Manycraft im Infobereich der Taskleiste.") } else { tr("Solange Spiele laufen, findest du Manycraft im Infobereich.") };
        super::sheet::inform(window, cx, tr("Manycraft läuft weiter"), place, tr("Verstanden"), |_, cx| cx.defer(|cx| dispatch(AppEvent::TrayExplained, cx)));
    });
}

fn ask_quit(running: usize, cx: &mut App) {
    if super::tray::shown(cx) {
        super::main_window::unstow(cx);
    } else {
        super::main_window::show(None, cx);
    }
    super::main_window::in_main(cx, move |window, cx| {
        if super::sheet::busy(window, cx) {
            return;
        }
        let title = if running == 1 { tr("1 Spiel läuft noch") } else { trf("%lld Spiele laufen noch", &[&running]) };
        super::sheet::choose(
            window,
            cx,
            title,
            if running == 1 { tr("Beim nächsten Start findet Manycraft es wieder.") } else { tr("Beim nächsten Start findet Manycraft sie wieder.") },
            tr("Alle beenden"),
            |_, cx| cx.defer(|cx| dispatch(AppEvent::QuitChosen { keep: false }, cx)),
            tr("Weiterlaufen lassen"),
            |_, cx| cx.defer(|cx| dispatch(AppEvent::QuitChosen { keep: true }, cx)),
        );
    });
}

fn close_games(closing: std::thread::JoinHandle<()>, cx: &mut App) {
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| native::hide(window, None));
    }
    let _ = closing.join();
}

fn runtime(cx: &mut App) -> Option<&mut Runtime> {
    if cx.has_global::<Runtime>() { Some(cx.global_mut::<Runtime>()) } else { None }
}

fn backend(cx: &App) -> Option<Arc<dyn Backend>> {
    cx.try_global::<Runtime>().map(|rt| Arc::clone(&rt.backend))
}

/// Hands `event` to the application core and carries out what it asks for.
fn dispatch(event: AppEvent, cx: &mut App) {
    dispatch_with(event, Vec::new(), cx);
}

fn dispatch_with(event: AppEvent, mut images: Vec<Option<Arc<RenderImage>>>, cx: &mut App) {
    if !cx.has_global::<Runtime>() && !matches!(event, AppEvent::PhoneOutcome(_) | AppEvent::Error(_) | AppEvent::Loaded(_) | AppEvent::Recolor { .. } | AppEvent::ClearWhispers(_) | AppEvent::InstanceAdded(_) | AppEvent::InstanceRemoved(_) | AppEvent::ShowGrid | AppEvent::CloseWindow | AppEvent::TrayExplained | AppEvent::Quit | AppEvent::QuitChosen { .. } | AppEvent::RestartForUpdate) {
        return;
    }
    let effects = reduce_now(event, cx);
    refresh_badge(cx);
    for effect in effects {
        run(effect, &mut images, cx);
    }
    for image in images.into_iter().flatten() {
        cx.drop_image(image, None);
    }
}

fn reduce_now(event: AppEvent, cx: &mut App) -> Vec<Effect> {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let env = Env::new(Instant::now(), unix, uuid::Uuid::new_v4().as_u64_pair().0, settings::active(), cfg!(target_os = "windows") && !drive::on());
    model::get(cx).update(cx, |m, cx| {
        let effects = reduce(&mut m.app, &env, event);
        match effects.iter().filter_map(|e| if let Effect::Redraw(r) = e { Some(*r) } else { None }).max() {
            Some(Redraw::All) => cx.notify(),
            Some(Redraw::Chat) => cx.emit(model::ChatArrived),
            _ => {}
        }
        effects
    })
}

fn run(effect: Effect, images: &mut [Option<Arc<RenderImage>>], cx: &mut App) {
    let backend = backend(cx);
    match effect {
        Effect::Redraw(_) => {}
        Effect::Send { key, message } => {
            if let Some(backend) = backend {
                backend.send(&key, message);
            }
        }
        Effect::Start { owner, target, accounts } => {
            let cancel = Arc::new(AtomicBool::new(false));
            if let Some(rt) = runtime(cx) {
                let runs = rt.cancel.entry(owner).or_default();
                runs.retain(|c| Arc::strong_count(c) > 1);
                runs.push(Arc::clone(&cancel));
            }
            let home = manager_place(cx).map(|p| p.frame);
            if let Some(backend) = backend {
                backend.start(target, accounts, cancel, home);
            }
        }
        Effect::CancelStarts(owner) => {
            if let Some(runs) = runtime(cx).and_then(|rt| rt.cancel.remove(&owner)) {
                for cancel in runs {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
        }
        Effect::CheckFit { count, memory_mb } => warn_if_too_big(count, memory_mb, cx),
        Effect::Stop(keys) => {
            if let Some(backend) = backend {
                backend.stop(keys);
            }
        }
        Effect::Stopped(keys) => {
            let name = match keys.as_slice() {
                [one] => model::get(cx).read(cx).games.get(one).map(|g| g.name.clone()),
                _ => None,
            };
            super::undo::offer(super::undo::Kind::Stopped { keys, name }, cx);
        }
        Effect::ReadMemory => read_memory(cx),
        Effect::ReadUsage(pids) => read_usage(pids, cx),
        Effect::SaveMeasured(seen) => {
            cx.background_spawn(async move {
                if let Err(e) = crate::core::measured::save(&seen) {
                    eprintln!("measured: {e:#}");
                }
            })
            .detach();
        }
        Effect::ReadPreview { key, path } => {
            if let Some(backend) = backend {
                backend.read_preview(key, path);
            }
        }
        Effect::ReleasePreview(key) => {
            if let Some(backend) = backend {
                backend.release_preview(key);
            }
        }
        Effect::CheckProxy { key, url } => {
            if let Some(backend) = backend {
                backend.check_proxy(key, url);
            }
        }
        Effect::SuspectOffline => online::suspect(),
        Effect::NetworkBack => super::remote::network_back(cx),
        Effect::After { delay, event } => {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(delay).await;
                cx.update(|cx| dispatch(*event, cx));
            })
            .detach();
        }
        Effect::SaveHistory { path, log } => {
            if let Some(writer) = cx.try_global::<Runtime>().and_then(|rt| rt.history.as_ref()) {
                writer.save(path, log);
            }
        }
        Effect::ShowFrame { key, image, still } => {
            let Some(image) = images.get_mut(image).and_then(Option::take) else { return };
            let old = model::get(cx).update(cx, |m, _| m.previews.insert(key.clone(), Arc::clone(&image)));
            if let Some(old) = old {
                cx.drop_image(old, None);
            }
            if let Some(last) = still {
                super::stills::save(&key, &image, last, cx);
            }
        }
        Effect::DropImage(image) => {
            if let Some(image) = images.get_mut(image).and_then(Option::take) {
                cx.drop_image(image, None);
            }
        }
        Effect::SaveStill(key) => {
            if let Some(image) = model::get(cx).read(cx).previews.get(&key).cloned() {
                super::stills::save(&key, &image, true, cx);
            }
        }
        Effect::Tint { key, pid } => {
            let m = model::get(cx).read(cx);
            if let Some(account) = m.games.get(&key).and_then(|i| m.account(i.account_id)) {
                game_window::tint(pid, account);
            }
        }
        Effect::Settle { placed, dropped } => {
            if let Some(backend) = backend {
                backend.settle(&placed, &dropped);
            }
        }
        Effect::Place(key) => place_new(&key, cx),
        Effect::Notify { tag, title, body } => {
            if super::main_window::is_active(cx) && model::get(cx).read(cx).playing.is_none() {
                return;
            }
            if drive::fenced("notify", format!("{title} | {body}")) {
                return;
            }
            cx.show_system_notification(SystemNotification { tag: tag.into(), title: title.into(), body: body.into(), actions: Vec::new() });
        }
        Effect::Phone(text) => send_phone(text, cx),
        Effect::Follow(key) => {
            follow_shown(key.as_deref(), cx);
        }
        Effect::Show { key, before, skipped, unshown } => show(&key, before.as_deref(), skipped, *unshown, cx),
        Effect::HideManager => {
            let place = cx.try_global::<Runtime>().and_then(|rt| rt.place);
            super::main_window::hide(place, cx);
        }
        Effect::ShowManager => {
            let place = cx.try_global::<Runtime>().and_then(|rt| rt.place);
            super::main_window::show(place, cx);
            spread(cx);
        }
        Effect::AddAccount => cx.dispatch_action(&super::actions::AddAccount),
        Effect::ShowInstance(id) => cx.defer(move |cx| {
            super::main_window::show_instance(&id, cx);
        }),
        Effect::RememberAccount { instance, account } => {
            cx.background_spawn(async move {
                if let Err(e) = super::remote::remember_account(&instance, account) {
                    eprintln!("last account: {e:#}");
                }
            })
            .detach();
        }
        Effect::SaveColor { ids, color } => save_color(ids, color, cx),
        Effect::Retain => retain(cx),
        Effect::LoadStills(bare) => {
            let faceless: Vec<Account> = {
                let m = model::get(cx).read(cx);
                m.accounts.iter().filter(|a| !m.faces.contains_key(&a.id)).cloned().collect()
            };
            super::stills::load(bare, faceless, cx);
        }
        Effect::Known(known) => {
            if let Some(backend) = backend {
                backend.known(known);
            }
        }
        Effect::Refresh(accounts) => {
            if let Some(backend) = backend {
                backend.refresh(accounts);
            }
        }
        Effect::ToTray { explain: false } => super::main_window::stow(cx),
        Effect::ToTray { explain: true } => explain_tray(cx),
        Effect::RememberTrayExplained => {
            settings::set(settings::Settings { tray_explained: true, ..settings::active().clone() });
            cx.background_spawn(async {
                if let Err(e) = settings::persist() {
                    eprintln!("settings: {e:#}");
                }
            })
            .detach();
        }
        Effect::AskQuit { running } => ask_quit(running, cx),
        Effect::Quit { keep } => {
            if let Some(rt) = runtime(cx) {
                rt.keep_games = keep;
            }
            cx.quit();
        }
        Effect::Trace(text) => drive::note(text),
        Effect::Log(text) => eprintln!("{text}"),
    }
}

/// Lets go of the images of games and accounts that are gone, and of what the tiles and stills kept for them.
fn retain(cx: &mut App) {
    let model = model::get(cx);
    let mut retired = Vec::new();
    model.update(cx, |m, _| {
        let frames = &m.app.frames;
        m.previews.retain(|k, image| {
            let stays = frames.contains_key(k);
            if !stays {
                retired.push(Arc::clone(image));
            }
            stays
        });
        let accounts = &m.app.accounts;
        m.faces.retain(|id, image| {
            let stays = accounts.iter().any(|a| a.id == *id);
            if !stays {
                retired.push(Arc::clone(image));
            }
            stays
        });
    });
    for image in retired {
        cx.drop_image(image, None);
    }
    let present: HashSet<String> = model.read(cx).games.keys().cloned().collect();
    super::tile::forget_except(&|k| present.contains(k), cx);
    super::stills::forget_except(&|k| present.contains(k), cx);
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
        (accounts, loaded, logs, moved.problems, store::take_recovered(), crate::core::measured::load())
    });
    cx.spawn(async move |cx| {
        let (accounts, batches, logs, problems, recovered, measured) = task.await;
        cx.update(|cx| {
            let data = match (accounts, batches) {
                (Ok(accounts), Ok((batches, instances))) => Ok((accounts, batches, instances)),
                (Err(e), _) | (_, Err(e)) => Err(super::forms::untouched(&e).unwrap_or_else(|| format!("{e:#}"))),
            };
            let adopted = match backend(cx) {
                Some(backend) if data.is_ok() || model::get(cx).read(cx).loaded => backend.adopted(),
                _ => Vec::new(),
            };
            let recovered = recovered_notice(&recovered);
            dispatch(AppEvent::Loaded(Box::new(Loaded { data, logs, problems, recovered, keep, adopted, measured })), cx);
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

/// Plays `instance` as another start of Manycraft or a shortcut asked: with `account`, else the one that played it last. Waits for the first load when the data is not there yet, and shows the game as soon as it can be shown.
pub fn request_play(instance: String, account: Option<String>, cx: &mut App) {
    dispatch(AppEvent::RequestPlay { instance, account }, cx);
}

/// Shows `text` as the notice after the next reload.
pub fn tell(text: String, cx: &mut App) {
    dispatch(if cx.has_global::<Runtime>() { AppEvent::Notice(text) } else { AppEvent::Error(text) }, cx);
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
                    dispatch(AppEvent::InstanceAdded(made), cx);
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
                    dispatch(AppEvent::InstanceRemoved(gone.clone()), cx);
                    super::undo::offer(super::undo::Kind::Deleted { id: gone, name, entry }, cx);
                }
                Err(e) => tell(super::forms::explain_as(tr("Nicht gelöscht."), &e).text, cx),
            }
            reload(cx);
        });
    })
    .detach();
}

fn read_usage(pids: Vec<(String, u32)>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let pretend = drive::on();
        let readings = cx.background_executor().spawn(async move { pids.into_iter().filter_map(|(key, pid)| fit::private_mb(pid).or_else(|| (pretend && pid == 0).then_some(1434)).map(|mb| (key, mb))).collect::<Vec<_>>() }).await;
        cx.update(|cx| dispatch(AppEvent::Usage(readings), cx));
    })
    .detach();
}

fn read_memory(cx: &mut App) {
    if let Some(pretend) = drive::memory() {
        let m = model::get(cx).read(cx);
        let used: u64 = m.games.values().filter(|g| g.phase.has_process() && !matches!(g.phase, Phase::Starting { .. })).filter_map(|g| m.target(&g.batch_id)).map(|t| u64::from(t.memory_mb())).sum();
        let memory = fit::Memory { total_mb: pretend.total_mb, free_mb: pretend.free_mb.saturating_sub(used) };
        cx.defer(move |cx| dispatch(AppEvent::Memory(Some(memory)), cx));
        return;
    }
    cx.spawn(async move |cx| {
        let memory = cx.background_executor().spawn(async move { fit::system_memory() }).await;
        cx.update(|cx| dispatch(AppEvent::Memory(memory), cx));
    })
    .detach();
}

/// Starts again the games a stop of many ended, staggered and as far as memory allows.
pub(super) fn start_again(keys: Vec<String>, cx: &mut App) {
    dispatch(AppEvent::StartAgain(keys), cx);
}

fn warn_if_too_big(count: u32, memory_mb: u32, cx: &mut App) {
    cx.spawn(async move |cx| {
        let fit = cx.background_executor().spawn(async move { fit::fit(count, memory_mb) }).await;
        let Some(fit) = fit.filter(|f| !f.fits()) else { return };
        cx.update(|cx| dispatch(AppEvent::Error(model::fit_line(count, memory_mb, &fit)), cx));
    })
    .detach();
}

fn set_color(a: &SetColor, cx: &mut App) {
    let ids: Vec<uuid::Uuid> = a.accounts.iter().filter_map(|id| id.parse().ok()).collect();
    let before: Vec<(String, u8)> = {
        let m = model::get(cx).read(cx);
        ids.iter().filter_map(|id| m.account(*id)).filter(|account| account.color() != a.color).map(|account| (account.id.to_string(), account.color())).collect()
    };
    if before.is_empty() {
        return;
    }
    let name = match before.as_slice() {
        [(id, _)] => model::get(cx).read(cx).accounts.iter().find(|account| account.id.to_string() == *id).map(|account| account.name.clone()),
        _ => None,
    };
    recolor(ids, a.color, cx);
    super::undo::offer(super::undo::Kind::Colored { before, color: a.color, name }, cx);
}

/// Gives the accounts `ids` the colour `color` and saves it, without an undo offer.
pub(super) fn recolor(ids: Vec<uuid::Uuid>, color: u8, cx: &mut App) {
    dispatch(AppEvent::Recolor { ids, color }, cx);
}

fn save_color(ids: Vec<uuid::Uuid>, color: u8, cx: &mut App) {
    let task = cx.background_spawn(async move { super::remote::set_color(&ids, color) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            if let Err(e) = result {
                eprintln!("color: {e:#}");
                tell(tr("Die Farbe wurde nicht gesichert. Versuch es noch einmal."), cx);
            }
            reload(cx);
        });
    })
    .detach();
}

/// Marks the whispers of `batch` (all instances when `None`) as seen.
pub fn clear_whispers(batch: Option<&str>, cx: &mut App) {
    dispatch(AppEvent::ClearWhispers(batch.map(str::to_owned)), cx);
}

/// Shows game `key` in the manager's place, following the game shown so far, `before`, or the manager window.
fn show(key: &str, before: Option<&str>, skipped: Option<String>, unshown: Unshown, cx: &mut App) {
    let place = if before.is_some() { follow_shown(before, cx) } else { manager_place(cx) };
    let Some(place) = place else {
        dispatch(AppEvent::NotShown(Box::new(unshown)), cx);
        return;
    };
    let pid = model::get(cx).read(cx).session.pids.get(key).copied();
    let Some(rt) = runtime(cx) else { return };
    rt.place = Some(place);
    if let Some(pid) = pid {
        native::allow_foreground(pid);
        if let Some(seen) = game_window::seen(pid).filter(|s| s.mode.is_normal() && !drive::on()) {
            rt.insets.insert(key.to_owned(), seen.insets);
        }
    }
    let client = place::client_in(place.frame, rt.insets.get(key).copied().unwrap_or_default());
    drive::note(format!("play {key}"));
    if let Some(backend) = backend(cx) {
        backend.send(key, ToMod::Show { x: client.x, y: client.y, w: client.w, h: client.h, mode: place.mode });
        if let Some(skipped) = skipped {
            backend.send(&skipped, ToMod::Hide);
        }
    }
    spread(cx);
}

/// Hides the game that is played and brings the manager back in its place.
pub fn show_grid(cx: &mut App) {
    dispatch(AppEvent::ShowGrid, cx);
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

/// The place after the game `playing`, wherever the player moved, resized, maximized or fullscreened it.
fn follow_shown(playing: Option<&str>, cx: &mut App) -> Option<Place> {
    let pid = playing.and_then(|k| model::get(cx).read(cx).session.pids.get(k).copied()).filter(|_| !drive::on());
    let works = game_window::work_areas();
    let rt = runtime(cx)?;
    if let Some(seen) = pid.and_then(game_window::seen).filter(|s| s.visible) {
        if let Some(key) = playing.filter(|_| seen.mode.is_normal()) {
            rt.insets.insert(key.to_owned(), seen.insets);
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
    let (playing, pids) = {
        let m = model::get(cx).read(cx);
        (m.playing.clone(), m.session.pids.clone())
    };
    let Some(rt) = runtime(cx) else { return };
    let Some(place) = rt.place.filter(|p| rt.spread != Some(p.frame)) else { return };
    rt.spread = Some(place.frame);
    for (key, pid) in &pids {
        if playing.as_ref() != Some(key) {
            game_window::place_hidden(*pid, place.frame);
        }
    }
}

/// Puts a game that came up hidden at the place as soon as its window exists, so that showing it later does not resize it.
fn place_new(key: &str, cx: &mut App) {
    let Some(pid) = model::get(cx).read(cx).session.pids.get(key).copied() else { return };
    let Some(seen) = game_window::seen(pid).filter(|_| !drive::on()) else { return };
    let place = match cx.try_global::<Runtime>().and_then(|rt| rt.place) {
        Some(place) => Some(place),
        None => manager_place(cx),
    };
    let playing = model::get(cx).read(cx).playing.clone();
    if let Some(rt) = runtime(cx)
        && seen.mode.is_normal()
    {
        rt.insets.insert(key.to_owned(), seen.insets);
    }
    if let Some(place) = place.filter(|_| playing.as_deref() != Some(key)) {
        game_window::place_hidden(pid, place.frame);
    }
    dispatch(AppEvent::Placed { key: key.to_owned() }, cx);
}

/// Tells the core where the user looks, so previews refresh fast where it matters and pause where nobody sees them.
pub fn watch(change: impl FnOnce(&mut crate::app::session::Watch), cx: &mut App) {
    if cx.try_global::<Runtime>().is_none() {
        return;
    }
    let mut next = model::get(cx).read(cx).session.watch.clone();
    change(&mut next);
    if model::get(cx).read(cx).session.watch != next {
        dispatch(AppEvent::Watch(next), cx);
    }
}

/// Works out the preview rates again, after their setting changed.
pub fn replan(cx: &mut App) {
    let watch = model::get(cx).read(cx).session.watch.clone();
    dispatch(AppEvent::Watch(watch), cx);
}

/// Tells the runtime which tiles are on screen, so their previews update at full rate.
pub fn set_visible(keys: HashSet<String>, cx: &mut App) {
    let Some(rt) = cx.try_global::<Runtime>() else { return };
    if model::get(cx).read(cx).session.visible == keys {
        return;
    }
    *lock(&rt.shared_visible) = keys.clone();
    dispatch(AppEvent::Visible(keys), cx);
}

fn apply_all(updates: Vec<Update>, cx: &mut App) {
    let frames_only = updates.iter().all(|u| matches!(u, Update::Preview { .. }));
    let mut images = Vec::new();
    let mut reports = Vec::new();
    let mut faces = Vec::new();
    for update in updates {
        reports.push(match update {
            Update::Face { account, image } => {
                faces.push((account, image));
                continue;
            }
            Update::Preview { key, seq, image } => {
                images.push(Some(image));
                Report::Frame { key, seq, image: images.len() - 1 }
            }
            Update::Still { key, image } => {
                images.push(Some(image));
                Report::Still { key, image: images.len() - 1 }
            }
            Update::Exited { key, code } => {
                if let Some(rt) = runtime(cx) {
                    rt.insets.remove(&key);
                }
                Report::Exited { key, code }
            }
            Update::Preparing { batch, percent } => Report::Preparing { batch, percent },
            Update::Prepared { batch } => Report::Prepared { batch },
            Update::Launched { key, plain } => Report::Launched { key, plain },
            Update::Failed { key, reason, login, offline } => Report::Failed { key, reason, login, offline },
            Update::Reach { online } => Report::Reach { online },
            Update::Seed { key, phase, attempts } => Report::Seed { key, phase, attempts },
            Update::SignIn { account, valid } => Report::SignIn { account, valid },
            Update::ProxyChecked { key, fault } => Report::ProxyChecked { key, fault },
            Update::Ipc(Event::Connected { instance, pid, .. }) => Report::Connected { key: instance, pid },
            Update::Ipc(Event::Message { instance, message }) => Report::Mod { key: instance, message },
            Update::Ipc(Event::Disconnected { instance }) => Report::LinkLost { key: instance },
            Update::Ipc(Event::Reopen) => Report::Reopen,
            Update::Ipc(Event::Play { instance, account }) => Report::PlayRequest { instance, account },
        });
    }
    if frames_only {
        return show_frames(reports, images, cx);
    }
    if !faces.is_empty() {
        apply_faces(faces, cx);
    }
    dispatch_with(AppEvent::Reports(reports), images, cx);
}

fn apply_faces(faces: Vec<(uuid::Uuid, Arc<RenderImage>)>, cx: &mut App) {
    let mut retired = Vec::new();
    model::get(cx).update(cx, |m, cx| {
        for (account, image) in faces {
            if m.account(account).is_none() {
                retired.push(image);
            } else if let Some(old) = m.faces.insert(account, image) {
                retired.push(old);
            }
        }
        cx.notify();
    });
    for image in retired {
        cx.drop_image(image, None);
    }
}

fn show_frames(reports: Vec<Report>, mut images: Vec<Option<Arc<RenderImage>>>, cx: &mut App) {
    if !cx.has_global::<Runtime>() {
        return;
    }
    let effects = reduce_now(AppEvent::Reports(reports), cx);
    let model = model::get(cx);
    let mut shown = Vec::new();
    let mut retired = Vec::new();
    model.update(cx, |m, _| {
        for effect in effects {
            match effect {
                Effect::ShowFrame { key, image, .. } => {
                    let Some(image) = images.get_mut(image).and_then(Option::take) else { continue };
                    if let Some(old) = m.previews.insert(key.clone(), Arc::clone(&image)) {
                        retired.push(old);
                    }
                    shown.push((key, image));
                }
                Effect::DropImage(image) => retired.extend(images.get_mut(image).and_then(Option::take)),
                _ => {}
            }
        }
    });
    for (key, image) in &shown {
        super::stills::save(key, image, false, cx);
    }
    super::tile::show_previews(shown, cx);
    for image in retired.into_iter().chain(images.into_iter().flatten()) {
        cx.drop_image(image, None);
    }
}

/// Keeps the computer awake while a game runs or waits to start again, as far as the settings allow.
pub fn keep_awake(cx: &App) {
    let busy = model::get(cx).read(cx).games.values().any(|i| i.phase.is_active());
    awake::set(awake::wanted(settings::active().prevent_sleep, busy));
}

/// Tells every connected game its identity, badge and background level again, after batches, accounts or settings changed.
pub fn tell_games(cx: &mut App) {
    dispatch(AppEvent::TellGames, cx);
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
    dispatch(AppEvent::PhoneOutcome(trouble), cx);
}

fn refresh_badge(cx: &mut App) {
    let count = model::get(cx).read(cx).attention_count(None);
    let Some(rt) = runtime(cx) else { return };
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
    let hint = i.hint;
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
        Phase::Starting { .. } if preparing.is_some() && i.memory_wait.is_none() => trf("Lädt Minecraft · %lld %%", &[&preparing.unwrap_or(0)]),
        Phase::Restarting { attempt, of } => model::restart_status(*attempt, *of),
        Phase::Starting { .. } if i.memory_wait.is_some() => tr("Wartet auf Arbeitsspeicher"),
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
    use std::path::PathBuf;

    use std::time::Instant;

    use super::{AuthError, Game, Phase, needs_sign_in, render_image, status, stopped_ban};
    use crate::app::event::Report;
    use crate::app::reduce::testing::{report, settings, setup, state};
    use crate::core::account::AccountKind;
    use crate::core::history::{self, Entry, Kind};
    use crate::core::ipc::{DisconnectCause, FromMod, StateKind};
    use crate::core::proxy::Fault;

    fn instance(phase: Phase) -> Game {
        Game { phase, ..Game::new("k".into(), "b".into(), "b".into(), uuid::Uuid::nil(), "Alex".into()) }
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

    #[test]
    fn the_status_line_counts_tries_against_the_policy_and_names_a_ban() {
        crate::core::i18n::init("de");
        let mut again = instance(Phase::Connecting { server: "s".into() });
        again.reconnect_attempts = 2;
        assert_eq!(status(&again, None), "Verbindet neu · Versuch 2 von 5");
        assert_eq!(status(&instance(Phase::Banned { server: None, reason: "Cheating".into() }), None), "Gesperrt · Cheating");
        assert_eq!(status(&instance(Phase::Restarting { attempt: 1, of: 3 }), None), "Startet neu · Versuch 1 von 3");
    }

    #[test]
    fn a_stopped_banned_account_keeps_its_ban_in_the_status() {
        crate::core::i18n::init("de");
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let message = FromMod::State { kind: StateKind::Disconnected, server: Some("mc.example:25565".into()), reason: Some("why".into()), cause: Some(DisconnectCause::Banned) };
        report(&mut m, &s, Report::Mod { key: key.clone(), message });
        m.games.get_mut(&key).unwrap().stopping = true;
        report(&mut m, &s, Report::Exited { key: key.clone(), code: Some(0) });
        let mut ui = crate::ui::model::Model { app: m, ..Default::default() };
        assert_eq!(ui.games[&key].phase, Phase::Stopped);
        assert_eq!(stopped_ban(&ui.games[&key], &ui).as_deref(), Some("Gestoppt · Gesperrt · why"));
        history::push(ui.app.history.get_mut(&key).unwrap(), Entry::now(Kind::Connected, None, ""));
        assert!(stopped_ban(&ui.games[&key], &ui).is_none());
    }

    #[test]
    fn a_dropped_link_on_a_living_game_shows_until_it_talks_again() {
        crate::core::i18n::init("de");
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        report(&mut m, &s, Report::LinkLost { key: key.clone() });
        assert_eq!(status(&m.games[&key], None), "Keine Verbindung zum Spiel");
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        report(&mut m, &s, Report::LinkLost { key: key.clone() });
        let message = FromMod::State { kind: StateKind::Playing, server: Some("mc.example:25565".into()), reason: None, cause: None };
        report(&mut m, &s, Report::Mod { key: key.clone(), message });
        assert_eq!(status(&m.games[&key], None), "Auf mc.example:25565");
    }

    #[test]
    fn a_connection_failure_on_a_proxied_account_asks_for_a_proxy_check() {
        crate::core::i18n::init("de");
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        m.accounts[0].proxy = Some("socks5://u:p@10.0.0.1:1080".into());
        report(&mut m, &s, state(&key, StateKind::Disconnected, Some("mc.example:25565"), Some(DisconnectCause::ServerDown)));
        assert_eq!(status(&m.games[&key], None), "Verbindet neu · Versuch 1 von 5");
        for (fault, line) in [(Some(Fault::Unreachable), "Proxy nicht erreichbar"), (Some(Fault::Rejected), "Proxy lehnt die Anmeldung ab"), (None, "Verbindet neu · Versuch 1 von 5")] {
            report(&mut m, &s, Report::ProxyChecked { key: key.clone(), fault });
            assert_eq!(status(&m.games[&key], None), line);
        }
    }
}
