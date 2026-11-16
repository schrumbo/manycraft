//! The updater in the Freya window: the hourly check, the download in the background, the restart that installs, going
//! back to the previous version.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use freya::prelude::*;

use super::kit::{after, off_thread};
use super::store::Store;
use crate::app::event::Event;
use crate::app::model::{Phase, UpdateStatus, Updates};
use crate::core::i18n::{tr, trf};
use crate::core::settings;
use crate::core::updater::{self, Kind, Release, Skip, Trouble, install, release};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const EVERY: Duration = Duration::from_secs(3600);
const PROVEN_AFTER: Duration = Duration::from_secs(10);
const ACKNOWLEDGED_FOR: Duration = Duration::from_secs(4);

/// How the app quits for the updater: to install the ready update, or to go back to the previous version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quit {
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

thread_local! {
    static FLIGHT: RefCell<Flight> = RefCell::default();
}

enum Msg {
    Progress(Option<u8>),
    Done(Result<String, (Option<Skip>, Option<Trouble>)>),
}

/// Starts the hourly check, counts this start as working after a while, and reads what the last run kept.
pub fn start(store: Store) {
    let _ = std::thread::Builder::new().name("update-hold".into()).spawn(install::hold);
    let Some(saved) = off_thread("update-state", || {
        let dir = updater::dir();
        let state = updater::load(&dir);
        (updater::news(&state, VERSION), updater::previous(&state).map(|p| p.version), state.trouble, updater::ready_for(&state, &dir, VERSION).map(|r| r.version), state.declined)
    }) else {
        return;
    };
    spawn_in_window(async move {
        let Ok((news, previous, trouble, ready, declined)) = saved.recv().await else { return };
        FLIGHT.with(|f| f.borrow_mut().declined = declined);
        set(store, |u| {
            u.news = news;
            u.previous = previous;
            u.trouble = trouble;
            if let Some(version) = ready {
                u.status = UpdateStatus::Ready { version };
            }
        });
        if !allowed() {
            return;
        }
        loop {
            check(store, false);
            after(EVERY).await;
        }
    });
    spawn_in_window(async move {
        after(PROVEN_AFTER).await;
        let _ = std::thread::Builder::new().name("update-proven".into()).spawn(|| install::mark_proven(VERSION));
    });
}

fn allowed() -> bool {
    !crate::core::drive::on() || updater::override_url().is_some()
}

fn set(store: Store, change: impl FnOnce(&mut Updates)) {
    let mut next = store.model.peek().updates.clone();
    change(&mut next);
    if next != store.model.peek().updates {
        store.dispatch(Event::Updates(Box::new(next)));
    }
}

fn busy(busy: bool) -> bool {
    FLIGHT.with(|f| std::mem::replace(&mut f.borrow_mut().busy, busy))
}

/// Loads the update the check found, when it installs here.
pub fn fetch(store: Store) {
    if matches!(store.model.peek().updates.status, UpdateStatus::Available { installs: true, .. }) {
        check(store, true);
    }
}

/// Looks for a newer release; `asked` when the person wants an answer now, which skips the daily wait and shows progress.
pub fn check(store: Store, asked: bool) {
    if !allowed() && !asked {
        return;
    }
    if busy(true) {
        if asked {
            set(store, |u| u.asked = true);
        }
        return;
    }
    if asked {
        set(store, |u| {
            u.status = UpdateStatus::Checking;
            u.asked = true;
            u.offline = false;
        });
    }
    let (auto, prereleases) = (settings::active().auto_update, settings::active().prereleases);
    let Some(rx) = off_thread("update-check", move || {
        let found = updater::check(VERSION, prereleases, asked)?;
        let dir = updater::dir();
        let ready = updater::ready_for(&updater::load(&dir), &dir, VERSION).map(|r| r.version);
        anyhow::Ok((found, install::here().kind, ready))
    }) else {
        busy(false);
        return;
    };
    spawn_in_window(async move {
        let Ok(result) = rx.recv().await else {
            busy(false);
            return;
        };
        busy(false);
        match result {
            Ok((None, _, _)) => set(store, |u| {
                u.status = UpdateStatus::Current;
                u.asked = false;
            }),
            Ok((Some(found), _, Some(ready))) if release::same(&found.version, &ready) => set(store, |u| {
                u.status = UpdateStatus::Ready { version: ready };
                u.asked = false;
            }),
            Ok((Some(found), kind, _)) => {
                let installs = kind.installs_itself() && release::pick(&found.assets, kind, std::env::consts::ARCH).is_some();
                set(store, |u| u.status = UpdateStatus::Available { version: found.version.clone(), page: found.page.clone(), installs });
                let declined = FLIGHT.with(|f| f.borrow().declined.as_deref().is_some_and(|d| release::same(d, &found.version)));
                if installs && (asked || (auto && !declined)) {
                    download(store, found, kind, asked);
                } else {
                    set(store, |u| u.asked = false);
                }
            }
            Err(e) => {
                eprintln!("update: {e:#}");
                set(store, |u| {
                    if asked {
                        u.offline = true;
                        u.status = UpdateStatus::Idle;
                    }
                    u.asked = false;
                });
            }
        }
    });
}

fn download(store: Store, found: Release, kind: Kind, asked: bool) {
    busy(true);
    let version = found.version.clone();
    let page = found.page.clone();
    set(store, |u| {
        u.status = UpdateStatus::Downloading { version: version.clone(), percent: None };
        u.asked = asked;
        u.cancelling = false;
        u.cancelled = false;
    });
    let stop = Arc::new(AtomicBool::new(false));
    FLIGHT.with(|f| f.borrow_mut().cancel = Some(stop.clone()));
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
        busy(false);
        FLIGHT.with(|f| f.borrow_mut().cancel = None);
        set(store, |u| u.status = UpdateStatus::Idle);
        return;
    }
    spawn_in_window(async move {
        while let Ok(msg) = rx.recv().await {
            match msg {
                Msg::Progress(percent) => set(store, |u| {
                    if let UpdateStatus::Downloading { percent: p, .. } = &mut u.status {
                        *p = percent;
                    }
                }),
                Msg::Done(result) => {
                    busy(false);
                    let cancelled = matches!(result, Err((Some(Skip::Cancelled), _)));
                    FLIGHT.with(|f| {
                        let mut f = f.borrow_mut();
                        f.cancel = None;
                        if cancelled {
                            f.declined = Some(version.clone());
                        }
                    });
                    if cancelled {
                        remember_declined(Some(version.clone()));
                        spawn_in_window(async move {
                            after(ACKNOWLEDGED_FOR).await;
                            set(store, |u| u.cancelled = false);
                        });
                    }
                    set(store, |u| {
                        u.asked = false;
                        u.cancelling = false;
                        u.cancelled = cancelled;
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
            }
        }
    });
}

fn remember_declined(version: Option<String>) {
    let _ = std::thread::Builder::new().name("update-declined".into()).spawn(move || {
        if let Err(e) = updater::edit(&updater::dir(), |s| s.declined = version) {
            eprintln!("update: {e:#}");
        }
    });
}

/// Stops the running download.
pub fn cancel(store: Store) {
    let Some(stop) = FLIGHT.with(|f| f.borrow().cancel.clone()) else { return };
    stop.store(true, Ordering::SeqCst);
    set(store, |u| u.cancelling = true);
}

/// Hides the note about the update just installed.
pub fn dismiss_news(store: Store) {
    set(store, |u| u.news = None);
    let _ = std::thread::Builder::new().name("update-news".into()).spawn(|| {
        let result = updater::edit(&updater::dir(), |s| {
            if let Some(fresh) = s.fresh.as_mut() {
                fresh.seen = true;
            }
        });
        if let Err(e) = result {
            eprintln!("update: {e:#}");
        }
    });
}

/// Games still starting, and games with a running process.
pub fn counts(store: Store) -> (usize, usize) {
    let m = store.model.peek();
    let launching = m.games.values().filter(|g| matches!(g.phase, Phase::Starting { .. })).count();
    let running = m.games.values().filter(|g| g.phase.is_running()).count();
    (launching, running)
}

/// Why restarting or going back waits while games are still starting.
pub fn launching_line() -> String {
    tr("Das geht, sobald keine Instanz mehr startet.")
}

/// Why restarting or going back must wait now, if it must.
pub fn blocked(store: Store) -> Option<String> {
    (counts(store).0 > 0).then(launching_line)
}

/// Quits the way `how` says, unless a game is still starting.
pub fn quit(store: Store, how: Quit) {
    if counts(store).0 > 0 {
        return;
    }
    FLIGHT.with(|f| f.borrow_mut().quit = Some(how));
    match how {
        Quit::Install => store.dispatch(Event::RestartForUpdate),
        Quit::Back => store.dispatch(Event::QuitChosen { keep: false }),
    }
}

/// Installs a ready update or goes back as asked, as the window closes.
pub fn on_quit(store: Store) {
    let how = FLIGHT.with(|f| f.borrow().quit);
    let launching = counts(store).0 > 0;
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

/// The installed version.
pub fn version() -> &'static str {
    VERSION
}
