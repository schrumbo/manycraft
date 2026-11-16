//! Carries out what the app core asks for, for every shell: the fleet, the start runs, the history writer and where
//! games go when they are shown. A shell owns one [`Runtime`], hands it the effects of each reduce and does itself only
//! what needs its toolkit: images, windows, sheets and notifications.

pub mod backend;
pub mod fake;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use backend::{Backend, Pixels, Real, Update};
use backend::lock;

use crate::app::effect::Effect;
use crate::app::event::{Loaded, Report};
use crate::app::reduce::Env;
use crate::core::game_window;
use crate::core::account::{self, Account};
use crate::core::batch::{self, Batch};
use crate::core::fleet::{self as fleet_mod, Adoption};
use crate::core::history;
use crate::core::i18n::{tr, trf};
use crate::core::instance::{self, Instance};
use crate::core::store::{self, Recovery};
use crate::core::ipc::{Event, ToMod};
use crate::core::place::{self, Insets, Mode, Place, Rect};
use crate::core::{drive, online, settings};

/// The state behind the effects that is the same in every shell.
pub struct Runtime {
    pub backend: Arc<dyn Backend>,
    cancel: HashMap<String, Vec<Arc<AtomicBool>>>,
    shared_visible: Arc<Mutex<HashSet<String>>>,
    history: Option<history::Writer>,
    /// Where the shown game and the manager window are, the one place they share.
    pub place: Option<Place>,
    insets: HashMap<String, Insets>,
    spread: Option<Rect>,
    /// Whether the games keep running when Manycraft quits now; true unless the user chose to end them, so a quit from outside keeps them.
    pub keep_games: bool,
}

/// The fleet for this run: the fake one when an agent drives the app, else the real one.
pub fn backend<P: Pixels>(out: async_channel::Sender<Update<P>>, visible: &Arc<Mutex<HashSet<String>>>) -> anyhow::Result<Arc<dyn Backend>> {
    if drive::on() && !drive::real() {
        return Ok(Arc::new(fake::Fake::new(out, Arc::clone(visible))));
    }
    Ok(Arc::new(Real::new(out)?))
}

/// The clock, settings and randomness one reduce sees.
pub fn env() -> Env<'static> {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    Env::new(Instant::now(), unix, uuid::Uuid::new_v4().as_u64_pair().0, settings::active(), cfg!(target_os = "windows") && !drive::on())
}

/// A batch of fleet updates turned into reports for the core, with the images the reports point into.
pub struct Reports<P> {
    pub reports: Vec<Report>,
    pub images: Vec<Option<P>>,
    pub faces: Vec<(uuid::Uuid, P)>,
    /// Whether the batch held only preview frames, which a shell may take on a fast path.
    pub frames_only: bool,
}

impl Runtime {
    /// A runtime around `backend`; `shared_visible` is the set the fake fleet reads to animate visible tiles.
    pub fn new(backend: Arc<dyn Backend>, shared_visible: Arc<Mutex<HashSet<String>>>) -> Self {
        Self { backend, cancel: HashMap::new(), shared_visible, history: history::Writer::start().ok(), place: None, insets: HashMap::new(), spread: None, keep_games: true }
    }

    /// Turns fleet updates into reports, forgetting the window insets of games that ended.
    pub fn reports<P>(&mut self, updates: Vec<Update<P>>) -> Reports<P> {
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
                    self.insets.remove(&key);
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
        Reports { reports, images, faces, frames_only }
    }

    /// Carries out `effect` when it needs no toolkit and gives it back otherwise. `window` is where the manager window is now.
    pub fn run(&mut self, effect: Effect, window: Option<(Rect, Mode)>) -> Option<Effect> {
        match effect {
            Effect::Redraw(_) => {}
            Effect::Send { key, message } => self.backend.send(&key, message),
            Effect::Start { owner, target, accounts } => {
                let cancel = Arc::new(AtomicBool::new(false));
                let runs = self.cancel.entry(owner).or_default();
                runs.retain(|c| Arc::strong_count(c) > 1);
                runs.push(Arc::clone(&cancel));
                let home = self.manager_place(window).map(|p| p.frame);
                self.backend.start(target, accounts, cancel, home);
            }
            Effect::CancelStarts(owner) => {
                for cancel in self.cancel.remove(&owner).unwrap_or_default() {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            Effect::Stop(keys) => self.backend.stop(keys),
            Effect::ReadPreview { key, path } => self.backend.read_preview(key, path),
            Effect::ReleasePreview(key) => self.backend.release_preview(key),
            Effect::CheckProxy { key, url } => self.backend.check_proxy(key, url),
            Effect::SuspectOffline => online::suspect(),
            Effect::SaveHistory { path, log } => {
                if let Some(writer) = &self.history {
                    writer.save(path, log);
                }
            }
            Effect::Settle { placed, dropped } => self.backend.settle(&placed, &dropped),
            Effect::Known(known) => self.backend.known(known),
            Effect::Refresh(accounts) => self.backend.refresh(accounts),
            Effect::RememberTrayExplained => {
                settings::set(settings::Settings { tray_explained: true, ..settings::active().clone() });
                std::thread::spawn(|| {
                    if let Err(e) = settings::persist() {
                        eprintln!("settings: {e:#}");
                    }
                });
            }
            Effect::Trace(text) => drive::note(text),
            Effect::Log(text) => eprintln!("{text}"),
            other => return Some(other),
        }
        None
    }

    /// Stops every start run, for quitting.
    pub fn cancel_all(&self) {
        for cancel in self.cancel.values().flatten() {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Tells the fake fleet which tiles are on screen.
    pub fn share_visible(&self, keys: HashSet<String>) {
        *lock(&self.shared_visible) = keys;
    }

    /// The place where the manager window is now, kept on a live monitor; the last place while it is minimized.
    pub fn manager_place(&self, window: Option<(Rect, Mode)>) -> Option<Place> {
        let works = game_window::work_areas();
        let place = match window {
            Some((frame, mode)) => place::follow(self.place, frame, mode, &works),
            None => self.place?,
        };
        Some(Place { frame: place::clamp(place.frame, &works), ..place })
    }

    /// The place after the game `playing` with process `pid`, wherever the player moved, resized, maximized or fullscreened it.
    pub fn follow_shown(&mut self, playing: Option<&str>, pid: Option<u32>) -> Option<Place> {
        let pid = pid.filter(|_| !drive::on());
        let works = game_window::work_areas();
        if let Some(seen) = pid.and_then(game_window::seen).filter(|s| s.visible) {
            if let Some(key) = playing.filter(|_| seen.mode.is_normal()) {
                self.insets.insert(key.to_owned(), seen.insets);
            }
            self.place = Some(place::follow(self.place, seen.frame, seen.mode, &works));
        }
        if let Some(p) = &mut self.place {
            p.frame = place::clamp(p.frame, &works);
        }
        self.place
    }

    /// Shows game `key` with process `pid` in the manager's place: the place of the game shown so far (`before`, process
    /// `before_pid`) or of the manager `window`. False when there is no place to show it in.
    pub fn show(&mut self, key: &str, pid: Option<u32>, before: Option<&str>, before_pid: Option<u32>, skipped: Option<String>, window: Option<(Rect, Mode)>) -> bool {
        let place = if before.is_some() { self.follow_shown(before, before_pid) } else { self.manager_place(window) };
        let Some(place) = place else { return false };
        self.place = Some(place);
        if let Some(pid) = pid
            && let Some(seen) = game_window::seen(pid).filter(|s| s.mode.is_normal() && !drive::on())
        {
            self.insets.insert(key.to_owned(), seen.insets);
        }
        let client = place::client_in(place.frame, self.insets.get(key).copied().unwrap_or_default());
        drive::note(format!("play {key}"));
        self.backend.send(key, ToMod::Show { x: client.x, y: client.y, w: client.w, h: client.h, mode: place.mode });
        if let Some(skipped) = skipped {
            self.backend.send(&skipped, ToMod::Hide);
        }
        true
    }

    /// Moves the hidden games to the place once per change, so that a swap only shows and hides.
    pub fn spread(&mut self, playing: Option<&str>, pids: &HashMap<String, u32>) {
        if drive::on() {
            return;
        }
        let Some(place) = self.place.filter(|p| self.spread != Some(p.frame)) else { return };
        self.spread = Some(place.frame);
        for (key, pid) in pids {
            if playing != Some(key.as_str()) {
                game_window::place_hidden(*pid, place.frame);
            }
        }
    }

    /// Puts a game that came up hidden at the place as soon as its window exists, so that showing it later does not resize
    /// it. False while its window is not there yet.
    pub fn place_new(&mut self, key: &str, pid: u32, playing: Option<&str>, window: Option<(Rect, Mode)>) -> bool {
        let Some(seen) = game_window::seen(pid).filter(|_| !drive::on()) else { return false };
        let place = self.place.or_else(|| self.manager_place(window));
        if seen.mode.is_normal() {
            self.insets.insert(key.to_owned(), seen.insets);
        }
        if let Some(place) = place.filter(|_| playing != Some(key)) {
            game_window::place_hidden(pid, place.frame);
        }
        true
    }

    /// Ends or leaves the games at quit, as the user chose, on a thread the shell may wait for.
    pub fn close(&self) -> std::io::Result<std::thread::JoinHandle<()>> {
        self.cancel_all();
        let (backend, keep) = (Arc::clone(&self.backend), self.keep_games);
        std::thread::Builder::new().name("quit".into()).spawn(move || if keep { backend.leave() } else { backend.quit() })
    }
}

/// The saved data as read from disk, ready to become a [`Loaded`] on the UI thread.
pub struct Data {
    pub data: Result<(Vec<Account>, Vec<Batch>, Vec<Instance>), String>,
    pub logs: Vec<(String, history::Log)>,
    pub problems: Vec<String>,
    pub recovered: Option<String>,
}

impl Data {
    /// The load event, keeping the message already shown when `keep`, with the games an earlier manager left running.
    pub fn loaded(self, keep: bool, adopted: Vec<Adoption>) -> Loaded {
        Loaded { data: self.data, logs: self.logs, problems: self.problems, recovered: self.recovered, keep, adopted }
    }
}

/// Moves old data, then reads accounts, batches, instances and their event logs; blocks, so it runs off the UI thread.
pub fn read_data() -> Data {
    let moved = crate::core::migrate::run();
    let (accounts, batches, instances) = (account::load(), batch::load_all(), instance::load_all());
    let logs = match (&batches, &instances) {
        (Ok(batches), Ok(instances)) => saved_history(batches, instances),
        _ => Vec::new(),
    };
    let data = match (accounts, batches.and_then(|b| instances.map(|i| (b, i)))) {
        (Ok(accounts), Ok((batches, instances))) => Ok((accounts, batches, instances)),
        (Err(e), _) | (_, Err(e)) => Err(untouched(&e).unwrap_or_else(|| format!("{e:#}"))),
    };
    Data { data, logs, problems: moved.problems, recovered: recovered_notice(&store::take_recovered()) }
}

/// The plain German for a file of Manycraft that a newer version or a hand edit left in a shape this version cannot read.
pub fn untouched(e: &anyhow::Error) -> Option<String> {
    let found = e.chain().find_map(|c| c.downcast_ref::<crate::core::store::Untouched>())?;
    let name = found.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Some(match name.as_str() {
        "accounts.json" => tr("Die Accounts stammen von einer neueren Version von Manycraft oder wurden von Hand geändert. Manycraft lässt die Datei unverändert. Nutze wieder die neuere Version oder mach die Änderung rückgängig."),
        "batch.json" => tr("Ein Batch stammt von einer neueren Version von Manycraft oder wurde von Hand geändert. Manycraft lässt die Datei unverändert. Nutze wieder die neuere Version oder mach die Änderung rückgängig."),
        "instance.json" => tr("Eine Instanz stammt von einer neueren Version von Manycraft oder wurde von Hand geändert. Manycraft lässt die Datei unverändert. Nutze wieder die neuere Version oder mach die Änderung rückgängig."),
        "settings.json" => tr("Die Einstellungen stammen von einer neueren Version oder wurden von Hand geändert. Es gelten die Standardwerte. Änderst du etwas, legt Manycraft die alte Datei daneben (settings.json.damaged)."),
        _ => trf("%@ stammt von einer neueren Version von Manycraft oder wurde von Hand geändert. Manycraft lässt die Datei unverändert. Nutze wieder die neuere Version oder mach die Änderung rückgängig.", &[&name]),
    })
}

/// What was damaged and restored or lost, as one notice.
pub fn recovered_notice(found: &[(PathBuf, Recovery)]) -> Option<String> {
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
                logs.push((fleet_mod::instance_key(batch, *id), log));
            }
        }
    }
    logs
}
