//! The updater as the app shows it: the daily check, the download in the background, the restart that installs, going back.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::*;

use super::actions::{CancelUpdate, CheckForUpdates, DismissNews, PreviousVersion, RestartToUpdate};
use super::model::{self, Phase, UpdateStatus, Updates};
use super::sheet;
use crate::core::i18n::{tr, trf};
use crate::core::settings;
use crate::core::updater::{self, Kind, Release, Skip, Trouble, install, release};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const EVERY: Duration = Duration::from_secs(3600);
const PROVEN_AFTER: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quit {
    Install,
    Back,
}

#[derive(Default)]
struct Flight {
    busy: bool,
    quit: Option<Quit>,
    cancel: Option<Arc<AtomicBool>>,
    declined: Option<String>,
}

impl Global for Flight {}

enum Msg {
    Progress(Option<u8>),
    Done(Result<String, (Option<Skip>, Option<Trouble>)>),
}

/// Starts the daily check, counts this start as working after a while, and installs a ready update when the app quits.
pub fn init(cx: &mut App) {
    cx.set_global(Flight::default());
    cx.background_spawn(async { install::hold() }).detach();
    cx.on_action(|_: &CheckForUpdates, cx| check(true, cx));
    cx.on_action(|_: &CancelUpdate, cx| cancel(cx));
    cx.on_action(|_: &RestartToUpdate, cx| sheet::in_window(cx, restart));
    cx.on_action(|_: &PreviousVersion, cx| sheet::in_window(cx, go_back));
    cx.on_action(|_: &DismissNews, cx| dismiss_news(cx));
    cx.on_app_quit(|cx| {
        on_quit(cx);
        async {}
    })
    .detach();
    let saved = cx.background_spawn(async {
        let dir = updater::dir();
        let state = updater::load(&dir);
        (updater::news(&state, VERSION), updater::previous(&state).map(|p| p.version), state.trouble, updater::ready_for(&state, &dir, VERSION).map(|r| r.version))
    });
    cx.spawn(async move |cx| {
        let (news, previous, trouble, ready) = saved.await;
        cx.update(|cx| {
            set(cx, |u| {
                u.news = news;
                u.previous = previous;
                u.trouble = trouble;
                if let Some(version) = ready {
                    u.status = UpdateStatus::Ready { version };
                }
            });
        });
        if !allowed() {
            return;
        }
        loop {
            cx.update(|cx| check(false, cx));
            cx.background_executor().timer(EVERY).await;
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(PROVEN_AFTER).await;
        cx.background_spawn(async { install::mark_proven(VERSION) }).await;
    })
    .detach();
}

/// Whether this process may ask for updates: always, except driven test instances without a test release list.
fn allowed() -> bool {
    !crate::core::drive::on() || updater::override_url().is_some()
}

fn set(cx: &mut App, change: impl FnOnce(&mut Updates)) {
    model::get(cx).update(cx, |m, cx| {
        let before = m.updates.clone();
        change(&mut m.updates);
        if m.updates != before {
            cx.notify();
        }
    });
}

fn busy(cx: &mut App, busy: bool) -> bool {
    let flight = cx.global_mut::<Flight>();
    let was = flight.busy;
    flight.busy = busy;
    was
}

/// Looks for a newer release; `asked` when the user wants an answer now, which skips the daily wait and shows progress.
pub fn check(asked: bool, cx: &mut App) {
    if !allowed() && !asked {
        return;
    }
    if busy(cx, true) {
        if asked {
            set(cx, |u| u.asked = true);
        }
        return;
    }
    if asked {
        set(cx, |u| {
            u.status = UpdateStatus::Checking;
            u.asked = true;
            u.offline = false;
        });
    }
    let (auto, prereleases) = (settings::active().auto_update, settings::active().prereleases);
    let task = cx.background_spawn(async move {
        let found = updater::check(VERSION, prereleases, asked)?;
        let dir = updater::dir();
        let ready = updater::ready_for(&updater::load(&dir), &dir, VERSION).map(|r| r.version);
        anyhow::Ok((found, install::here().kind, ready))
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            busy(cx, false);
            match result {
                Ok((None, _, _)) => set(cx, |u| {
                    u.status = UpdateStatus::Current;
                    u.asked = false;
                }),
                Ok((Some(found), _, Some(ready))) if release::same(&found.version, &ready) => set(cx, |u| {
                    u.status = UpdateStatus::Ready { version: ready };
                    u.asked = false;
                }),
                Ok((Some(found), kind, _)) => {
                    let installs = kind.installs_itself() && release::pick(&found.assets, kind, std::env::consts::ARCH).is_some();
                    set(cx, |u| u.status = UpdateStatus::Available { version: found.version.clone(), page: found.page.clone(), installs });
                    let declined = cx.global::<Flight>().declined.as_deref().is_some_and(|d| release::same(d, &found.version));
                    if installs && (asked || (auto && !declined)) {
                        download(found, kind, asked, cx);
                    } else {
                        set(cx, |u| u.asked = false);
                    }
                }
                Err(e) => {
                    eprintln!("update: {e:#}");
                    set(cx, |u| {
                        if asked {
                            u.offline = true;
                            u.status = UpdateStatus::Idle;
                        }
                        u.asked = false;
                    });
                }
            }
        });
    })
    .detach();
}

/// Downloads the release the sidebar offers, when the user clicks it.
pub fn fetch(cx: &mut App) {
    let UpdateStatus::Available { installs: true, .. } = model::get(cx).read(cx).updates.status else { return };
    check(true, cx);
}

fn download(found: Release, kind: Kind, asked: bool, cx: &mut App) {
    busy(cx, true);
    let version = found.version.clone();
    set(cx, |u| {
        u.status = UpdateStatus::Downloading { version: version.clone(), percent: None };
        u.asked = asked;
        u.cancelling = false;
    });
    let stop = Arc::new(AtomicBool::new(false));
    cx.global_mut::<Flight>().cancel = Some(stop.clone());
    let (tx, rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new().name("update-download".into()).spawn(move || {
        let mut last = None;
        let result = updater::prepare(&found, kind, asked, &stop, &mut |done, total| {
            let percent = total.filter(|t| *t > 0).map(|t| u8::try_from((done.saturating_mul(100) / t).min(100)).unwrap_or(100));
            if percent != last {
                last = percent;
                let _ = tx.send_blocking(Msg::Progress(percent));
            }
        });
        let result = result.map(|r| r.version).map_err(|e| {
            eprintln!("update: {e:#}");
            (e.downcast_ref::<Skip>().copied(), updater::load(&updater::dir()).trouble)
        });
        let _ = tx.send_blocking(Msg::Done(result));
    });
    if spawned.is_err() {
        busy(cx, false);
        cx.global_mut::<Flight>().cancel = None;
        set(cx, |u| u.status = UpdateStatus::Idle);
        return;
    }
    let page = found_page(&version, cx);
    cx.spawn(async move |cx| {
        while let Ok(msg) = rx.recv().await {
            cx.update(|cx| match msg {
                Msg::Progress(percent) => set(cx, |u| {
                    if let UpdateStatus::Downloading { percent: p, .. } = &mut u.status {
                        *p = percent;
                    }
                }),
                Msg::Done(result) => {
                    busy(cx, false);
                    let flight = cx.global_mut::<Flight>();
                    flight.cancel = None;
                    if matches!(result, Err((Some(Skip::Cancelled), _))) {
                        flight.declined = Some(version.clone());
                    }
                    set(cx, |u| {
                        u.asked = false;
                        u.cancelling = false;
                        match result {
                            Ok(version) => {
                                u.status = UpdateStatus::Ready { version };
                                u.trouble = None;
                            }
                            Err((skip, trouble)) => {
                                u.status = UpdateStatus::Available { version: version.clone(), page: page.clone(), installs: skip != Some(Skip::NoInstaller) };
                                if skip.is_none() {
                                    u.trouble = trouble;
                                }
                            }
                        }
                    });
                }
            });
        }
    })
    .detach();
}

fn cancel(cx: &mut App) {
    let Some(stop) = cx.global::<Flight>().cancel.clone() else { return };
    stop.store(true, Ordering::SeqCst);
    set(cx, |u| u.cancelling = true);
}

/// Whether a download is running that [`CancelUpdate`] can stop.
pub fn cancellable(u: &Updates) -> bool {
    matches!(u.status, UpdateStatus::Downloading { .. }) && !u.cancelling
}

/// What the sidebar's update row says, for places that show it only as a hint when the sidebar is hidden.
pub fn offer(u: &Updates) -> Option<String> {
    match &u.status {
        UpdateStatus::Ready { .. } => Some(tr("Update bereit")),
        UpdateStatus::Available { .. } => Some(tr("Update verfügbar")),
        _ => None,
    }
}

/// Why starting or going back waits while instances are still starting.
pub fn launching_line() -> String {
    tr("Das geht, sobald keine Instanz mehr startet.")
}

fn found_page(version: &str, cx: &App) -> String {
    match &model::get(cx).read(cx).updates.status {
        UpdateStatus::Available { page, .. } => page.clone(),
        _ => format!("https://github.com/schrumbo/manycraft/releases/tag/v{version}"),
    }
}

/// Instances still starting, and instances with a running game.
pub fn counts(cx: &App) -> (usize, usize) {
    let m = model::get(cx).read(cx);
    let launching = m.games.values().filter(|i| matches!(i.phase, Phase::Starting { .. })).count();
    let running = m.games.values().filter(|i| i.phase.is_running()).count();
    (launching, running)
}

fn stops(running: usize) -> String {
    if running == 1 { tr("1 laufende Instanz wird beendet.") } else { trf("%lld laufende Instanzen werden beendet.", &[&running]) }
}

fn restart(window: &mut Window, cx: &mut App) {
    if !matches!(model::get(cx).read(cx).updates.status, UpdateStatus::Ready { .. }) || sheet::busy(window, cx) {
        return;
    }
    let (launching, running) = counts(cx);
    if launching > 0 {
        return;
    }
    if running == 0 {
        quit(Quit::Install, cx);
        return;
    }
    sheet::ask_blocked(window, cx, tr("Jetzt neu starten?"), stops(running), tr("Neu starten"), blocked, |_, cx| quit_unless_launching(Quit::Install, cx));
}

fn go_back(window: &mut Window, cx: &mut App) {
    let Some(previous) = model::get(cx).read(cx).updates.previous.clone() else { return };
    if sheet::busy(window, cx) || counts(cx).0 > 0 {
        return;
    }
    let mut message = tr("Manycraft startet neu mit der vorherigen Version. Die neuere wird nicht mehr angeboten.");
    let running = counts(cx).1;
    if running > 0 {
        message = format!("{message} {}", stops(running));
    }
    sheet::ask_blocked(window, cx, trf("Zu Version %@ zurückkehren?", &[&previous]), message, tr("Zurückkehren"), blocked, |_, cx| quit_unless_launching(Quit::Back, cx));
}

fn blocked(cx: &App) -> Option<String> {
    (counts(cx).0 > 0).then(launching_line)
}

fn quit_unless_launching(how: Quit, cx: &mut App) {
    if counts(cx).0 == 0 {
        quit(how, cx);
    }
}

fn quit(how: Quit, cx: &mut App) {
    cx.global_mut::<Flight>().quit = Some(how);
    cx.quit();
}

fn on_quit(cx: &mut App) {
    let how = cx.try_global::<Flight>().and_then(|f| f.quit);
    let launching = counts(cx).0 > 0;
    if let Err(e) = install::on_exit(VERSION, how == Some(Quit::Back), how.is_some(), launching) {
        eprintln!("update: {e:#}");
    }
}

/// The line under the version in Settings that says where the updater is.
pub fn status_line(u: &Updates) -> String {
    match &u.status {
        UpdateStatus::Idle => tr("Sucht einmal am Tag nach einer neueren Version."),
        UpdateStatus::Checking => tr("Sucht …"),
        UpdateStatus::Current => tr("Das ist die neueste Version."),
        UpdateStatus::Available { version, installs: true, .. } => trf("Version %@ ist verfügbar.", &[version]),
        UpdateStatus::Available { version, installs: false, .. } => trf("Version %@ ist verfügbar. Diese Installation aktualisiert sich nicht selbst.", &[version]),
        UpdateStatus::Downloading { version, .. } if u.cancelling => trf("Laden von Version %@ wird abgebrochen …", &[version]),
        UpdateStatus::Downloading { version, percent: Some(p) } => trf("Version %@ wird geladen: %@", &[version, &format!("{p} %")]),
        UpdateStatus::Downloading { version, percent: None } => trf("Version %@ wird geladen …", &[version]),
        UpdateStatus::Ready { version } => trf("Version %@ ist bereit und wird beim Beenden installiert.", &[version]),
    }
}

/// The quiet line about the last thing that went wrong, if anything did.
pub fn trouble_line(u: &Updates) -> Option<String> {
    if u.offline {
        return Some(tr("GitHub ist gerade nicht erreichbar."));
    }
    u.trouble.map(|t| match t {
        Trouble::Download => tr("Das letzte Update ließ sich nicht laden. Neuer Versuch morgen."),
        Trouble::Damaged => tr("Das letzte Update war beschädigt und wurde gelöscht. Neuer Versuch morgen."),
        Trouble::Install => tr("Das letzte Update ließ sich nicht installieren. Neuer Versuch morgen."),
    })
}

fn dismiss_news(cx: &mut App) {
    set(cx, |u| u.news = None);
    cx.background_spawn(async {
        let result = updater::edit(&updater::dir(), |s| {
            if let Some(fresh) = s.fresh.as_mut() {
                fresh.seen = true;
            }
        });
        if let Err(e) = result {
            eprintln!("update: {e:#}");
        }
    })
    .detach();
}
