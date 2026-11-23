//! Runs the app behind the views: loads accounts and batches, drives the fleet, turns mod events into model changes and handles the actions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::actions::{TakeOver, Broadcast, CancelStart, CloneInstance, NextAttention, NextInstance, OpenChat, OpenFromTray, Play, PlayInstance, PreviousInstance, Quit, QuitAll, Reconnect, ReconnectMany, Reload, Respawn, RespawnMany, RestartMany, SendChat, SendChatMany, SetColor, ShowGrid, Start, StartAll, StartAnyway, StartBatch, StartMany, Stop, StopAll, StopBatch, StopMany};
use super::model::{self, Game, Model, Phase};
use super::native;
use crate::app::effect::{Effect, Redraw, Unshown};
use crate::app::event::{Event as AppEvent, Loaded, Report};
use crate::app::reduce::{Env, reduce};
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
use crate::runner::{self, Backend};

pub use crate::app::model::{status, stopped_ban};
pub use crate::app::reduce::solo_account;

const COALESCE: Duration = Duration::from_millis(40);
const EXIT_POLL: Duration = Duration::from_millis(500);
const STAGGER_STEP: Duration = Duration::from_millis(100);
const QUIT_WAIT: Duration = Duration::from_secs(5);
const TRASH_KEPT: Duration = crate::core::bin::KEPT;

/// What the fleet reports back to the UI thread, with GPU images.
pub type Update = runner::Update<Arc<RenderImage>>;

impl runner::Pixels for Arc<RenderImage> {
    fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        render_image(width, height, rgba)
    }
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
    core: runner::Runtime,
    badge: usize,
    outbox: Option<phone::Outbox>,
    /// The games each shown notification is about, by tag, so a click opens them.
    notified: HashMap<String, Vec<String>>,
}

impl Global for Runtime {}

/// Loads saved data into the model, starts the fleet and registers the action handlers.
pub fn init(cx: &mut App) {
    let (out, updates) = async_channel::unbounded::<Update>();
    super::stills::init(out.clone(), cx);
    let shared_visible: Arc<Mutex<HashSet<String>>> = Arc::default();
    let backend = match runner::backend(out, &shared_visible) {
        Ok(backend) => backend,
        Err(e) => {
            let model = model::get(cx);
            model.update(cx, |m, cx| {
                m.error = Some(format!("{e:#}"));
                cx.notify();
            });
            return;
        }
    };
    cx.set_global(Runtime { core: runner::Runtime::new(backend, shared_visible), badge: 0, outbox: None, notified: HashMap::new() });
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
        if let Err(e) = crate::core::batch::purge_stale(TRASH_KEPT) {
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
    cx.on_action(|a: &TakeOver, cx| dispatch(AppEvent::TakeOver(a.0.clone()), cx));
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
        let tag = response.tag.to_string();
        cx.defer(move |cx| notification_clicked(&tag, cx));
    });
    cx.on_app_quit(|cx| {
        awake::set(false);
        if let Some(rt) = cx.try_global::<Runtime>() {
            match rt.core.close() {
                Ok(closing) => close_games(closing, cx),
                Err(_) if rt.core.keep_games => rt.core.backend.leave(),
                Err(_) => rt.core.backend.quit(),
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
    cx.try_global::<Runtime>().map(|rt| Arc::clone(&rt.core.backend))
}

/// Hands `event` to the application core and carries out what it asks for.
fn dispatch(event: AppEvent, cx: &mut App) {
    dispatch_with(event, Vec::new(), cx);
}

fn dispatch_with(event: AppEvent, mut images: Vec<Option<Arc<RenderImage>>>, cx: &mut App) {
    if !cx.has_global::<Runtime>() && !matches!(event, AppEvent::PhoneOutcome(_) | AppEvent::Error(_) | AppEvent::Loaded(_) | AppEvent::Recolor { .. } | AppEvent::InstanceAdded(_) | AppEvent::InstanceRemoved(_) | AppEvent::ShowGrid | AppEvent::CloseWindow | AppEvent::TrayExplained | AppEvent::Quit | AppEvent::QuitChosen { .. } | AppEvent::RestartForUpdate) {
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
    let env = runner::env();
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
    let window = if matches!(effect, Effect::Start { .. }) { super::main_window::place(cx) } else { None };
    let effect = match runtime(cx) {
        Some(rt) => match rt.core.run(effect, window) {
            Some(effect) => effect,
            None => return,
        },
        None => effect,
    };
    let backend = backend(cx);
    match effect {
        Effect::CheckFit { count, memory_mb } => warn_if_too_big(count, memory_mb, cx),
        Effect::Stopped(keys) => {
            let name = match keys.as_slice() {
                [one] => model::get(cx).read(cx).games.get(one).map(|g| g.name.clone()),
                _ => None,
            };
            super::undo::offer(super::undo::Kind::Stopped { keys, name, closed: false }, cx);
        }
        Effect::Closed(key) => {
            let name = model::get(cx).read(cx).games.get(&key).map(|g| g.name.clone());
            super::undo::offer(super::undo::Kind::Stopped { keys: vec![key], name, closed: true }, cx);
        }
        Effect::ReadMemory => read_memory(cx),
        Effect::ReadUsage(pids) => read_usage(pids, cx),
        Effect::NetworkBack => super::remote::network_back(cx),
        Effect::After { delay, event } => {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(delay).await;
                cx.update(|cx| dispatch(*event, cx));
            })
            .detach();
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
        Effect::Place(key) => place_new(&key, cx),
        Effect::Notify { tag, title, body, keys } => {
            if super::main_window::is_active(cx) && model::get(cx).read(cx).playing.is_none() {
                return;
            }
            if let Some(rt) = runtime(cx) {
                rt.notified.insert(tag.clone(), keys.clone());
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
            let place = cx.try_global::<Runtime>().and_then(|rt| rt.core.place);
            super::main_window::hide(place, cx);
        }
        Effect::ShowManager => {
            let place = cx.try_global::<Runtime>().and_then(|rt| rt.core.place);
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
        Effect::ToTray { explain: false } => super::main_window::stow(cx),
        Effect::ToTray { explain: true } => explain_tray(cx),
        Effect::AskQuit { running } => ask_quit(running, cx),
        Effect::Quit { keep } => {
            if let Some(rt) = runtime(cx) {
                rt.core.keep_games = keep;
            }
            cx.quit();
        }
        Effect::Trace(text) => drive::note(text),
        Effect::Log(text) => eprintln!("{text}"),
        _ => {}
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
    let task = cx.background_spawn(async { runner::read_data() });
    cx.spawn(async move |cx| {
        let read = task.await;
        cx.update(|cx| {
            let adopted = match backend(cx) {
                Some(backend) if read.data.is_ok() || model::get(cx).read(cx).loaded => backend.adopted(),
                _ => Vec::new(),
            };
            dispatch(AppEvent::Loaded(Box::new(read.loaded(keep, adopted))), cx);
        });
    })
    .detach();
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

/// Shows game `key` in the manager's place, following the game shown so far, `before`, or the manager window.
fn show(key: &str, before: Option<&str>, skipped: Option<String>, unshown: Unshown, cx: &mut App) {
    let (pid, before_pid) = {
        let pids = &model::get(cx).read(cx).session.pids;
        (pids.get(key).copied(), before.and_then(|b| pids.get(b).copied()))
    };
    if let Some(pid) = pid {
        native::allow_foreground(pid);
    }
    let window = if before.is_none() { super::main_window::place(cx) } else { None };
    let Some(rt) = runtime(cx) else { return };
    if !rt.core.show(key, pid, before, before_pid, skipped, window) {
        dispatch(AppEvent::NotShown(Box::new(unshown)), cx);
        return;
    }
    spread(cx);
}

/// Opens what the notification `tag` was about: its one tile, the attention tiles of its batch, or the chat.
pub fn notification_clicked(tag: &str, cx: &mut App) {
    let keys = cx.try_global::<Runtime>().and_then(|rt| rt.notified.get(tag).cloned()).unwrap_or_default();
    if let Some(route) = super::route::of_notification(tag, &keys) {
        super::route::go(route, cx);
    }
}

/// Hides the game that is played and brings the manager back in its place.
pub fn show_grid(cx: &mut App) {
    dispatch(AppEvent::ShowGrid, cx);
}

fn manager_place(cx: &mut App) -> Option<Place> {
    let window = super::main_window::place(cx);
    cx.try_global::<Runtime>()?.core.manager_place(window)
}

fn follow_shown(playing: Option<&str>, cx: &mut App) -> Option<Place> {
    let pid = playing.and_then(|k| model::get(cx).read(cx).session.pids.get(k).copied());
    runtime(cx)?.core.follow_shown(playing, pid)
}

fn spread(cx: &mut App) {
    let (playing, pids) = {
        let m = model::get(cx).read(cx);
        (m.playing.clone(), m.session.pids.clone())
    };
    if let Some(rt) = runtime(cx) {
        rt.core.spread(playing.as_deref(), &pids);
    }
}

fn place_new(key: &str, cx: &mut App) {
    let (pid, playing) = {
        let m = model::get(cx).read(cx);
        (m.session.pids.get(key).copied(), m.playing.clone())
    };
    let Some(pid) = pid else { return };
    let window = if cx.try_global::<Runtime>().is_some_and(|rt| rt.core.place.is_none()) { super::main_window::place(cx) } else { None };
    let Some(rt) = runtime(cx) else { return };
    if rt.core.place_new(key, pid, playing.as_deref(), window) {
        dispatch(AppEvent::Placed { key: key.to_owned() }, cx);
    }
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
    rt.core.share_visible(keys.clone());
    dispatch(AppEvent::Visible(keys), cx);
}

fn apply_all(updates: Vec<Update>, cx: &mut App) {
    let Some(rt) = runtime(cx) else { return };
    let runner::Reports { reports, images, faces, frames_only } = rt.core.reports(updates);
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use std::time::Instant;

    use super::{AuthError, Game, Phase, render_image, status, stopped_ban};
    use crate::runner::backend::needs_sign_in;
    use crate::app::event::Report;
    use crate::app::reduce::testing::{report, run, settings, setup, state};
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
        use crate::core::store::Recovery;
        use crate::runner::recovered_notice;
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
        let message = FromMod::State { kind: StateKind::Disconnected, server: Some("mc.example:25565".into()), reason: Some("why".into()), cause: Some(DisconnectCause::Banned), offer: None };
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
        run(&mut m, &s, Instant::now(), crate::app::event::Event::LinkLostDue { key: key.clone() });
        assert_eq!(status(&m.games[&key], None), "Keine Verbindung zum Spiel");
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        report(&mut m, &s, Report::LinkLost { key: key.clone() });
        let message = FromMod::State { kind: StateKind::Playing, server: Some("mc.example:25565".into()), reason: None, cause: None, offer: None };
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
