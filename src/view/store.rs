//! The app model in the Freya window and the glue to the shared runner: events go through the core's reduce, effects
//! through the [`Runtime`](crate::runner::Runtime), and only what needs Freya is done here.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;
use uuid::Uuid;

use super::kit::{after, sheet, toast};
use super::native;
use super::ui::{Page, Ui};
use crate::app::effect::{Effect, Redraw};
use crate::app::event::Event;
use crate::app::model::{Model, Phase};
use crate::app::reduce::reduce;
use crate::core::i18n::{tr, trf};
use crate::core::{drive, fit, game_window, settings};
use crate::runner::{self, Pixels, Runtime, Update};

const COALESCE: Duration = Duration::from_millis(40);

/// An RGBA frame on its way from the fleet threads to the UI thread.
pub struct Raw {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Pixels for Raw {
    fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        (rgba.len() == (width as usize) * (height as usize) * 4).then_some(Self { width, height, rgba })
    }
}

impl Raw {
    fn image(self) -> Option<ImageHandle> {
        ImageHandle::from_rgba(self.width, self.height, Bytes::from(self.rgba), AlphaType::Unpremul)
    }
}

/// The model, the images it points to and the runtime behind them, shared through the context.
#[derive(Clone, Copy, PartialEq)]
pub struct Store {
    pub model: State<Model>,
    /// The newest preview frame of each game.
    pub previews: State<HashMap<String, ImageHandle>>,
    /// The skin face of each account.
    pub faces: State<HashMap<Uuid, ImageHandle>>,
    pub ui: Ui,
    runtime: State<Option<Rc<RefCell<Runtime>>>>,
}

impl Store {
    /// Starts the fleet and reads the saved data; fleet updates arrive in the window's task loop.
    pub fn create(ui: Ui) -> (Self, Option<async_channel::Receiver<Update<Raw>>>) {
        let mut store = Self { model: State::create(Model::default()), previews: State::create(HashMap::new()), faces: State::create(HashMap::new()), ui, runtime: State::create(None) };
        let (out, updates) = async_channel::unbounded::<Update<Raw>>();
        let visible: Arc<Mutex<HashSet<String>>> = Arc::default();
        match runner::backend(out, &visible) {
            Ok(backend) => store.runtime.set(Some(Rc::new(RefCell::new(Runtime::new(backend, visible))))),
            Err(e) => {
                store.model.write().error = Some(format!("{e:#}"));
                return (store, None);
            }
        }
        if let Err(e) = settings::activate() {
            eprintln!("settings: {e:#}");
            store.dispatch(Event::Notice(runner::untouched(&e).unwrap_or_else(|| format!("{e:#}"))));
        }
        (store, Some(updates))
    }

    fn runtime(&self) -> Option<Rc<RefCell<Runtime>>> {
        self.runtime.peek().clone()
    }

    /// Reads accounts, batches and instances again and hands them to the core.
    pub fn reload(self, keep: bool) {
        let (tx, rx) = async_channel::bounded(1);
        let _ = std::thread::Builder::new().name("reload".into()).spawn(move || {
            let _ = tx.send_blocking(runner::read_data());
        });
        spawn_in_window(async move {
            let Ok(read) = rx.recv().await else { return };
            let adopted = match self.runtime() {
                Some(rt) if read.data.is_ok() || self.model.peek().loaded => rt.borrow().backend.adopted(),
                _ => Vec::new(),
            };
            self.dispatch(Event::Loaded(Box::new(read.loaded(keep, adopted))));
        });
    }

    /// Takes in fleet updates until the fleet stops, a batch at a time.
    pub async fn pump(self, updates: async_channel::Receiver<Update<Raw>>) {
        while let Ok(first) = updates.recv().await {
            let mut batch = vec![first];
            while let Ok(next) = updates.try_recv() {
                batch.push(next);
            }
            self.apply(batch);
            after(COALESCE).await;
        }
    }

    fn apply(mut self, updates: Vec<Update<Raw>>) {
        let Some(rt) = self.runtime() else { return };
        let runner::Reports { reports, images, faces, .. } = rt.borrow_mut().reports(updates);
        if !faces.is_empty() {
            let known: HashSet<Uuid> = self.model.peek().accounts.iter().map(|a| a.id).collect();
            let mut map = self.faces.write();
            for (account, raw) in faces {
                if known.contains(&account)
                    && let Some(image) = raw.image()
                {
                    map.insert(account, image);
                }
            }
        }
        self.dispatch_with(Event::Reports(reports), images);
    }

    /// Hands `event` to the core and carries out what it asks for.
    pub fn dispatch(self, event: Event) {
        self.dispatch_with(event, Vec::new());
    }

    fn dispatch_with(mut self, event: Event, mut images: Vec<Option<Raw>>) {
        let env = runner::env();
        let reports = matches!(event, Event::Reports(_));
        let mut effects = Vec::new();
        self.model.into_writable().write_if(|mut model| {
            effects = reduce(&mut model, &env, event);
            !reports || effects.iter().any(|e| matches!(e, Effect::Redraw(Redraw::All)))
        });
        for effect in effects {
            self.run(effect, &mut images);
        }
    }

    fn run(mut self, effect: Effect, images: &mut [Option<Raw>]) {
        let runtime = self.runtime();
        let effect = match &runtime {
            Some(rt) => {
                let window = if matches!(effect, Effect::Start { .. }) { native::place() } else { None };
                let Some(effect) = rt.borrow_mut().run(effect, window) else { return };
                effect
            }
            None => effect,
        };
        match effect {
            Effect::After { delay, event } => {
                spawn_in_window(async move {
                    after(delay).await;
                    self.dispatch(*event);
                });
            }
            Effect::ShowFrame { key, image, .. } => {
                if let Some(image) = images.get_mut(image).and_then(Option::take).and_then(Raw::image) {
                    self.previews.write().insert(key, image);
                }
            }
            Effect::DropImage(image) => drop(images.get_mut(image).and_then(Option::take)),
            Effect::Show { key, before, skipped, unshown } => {
                let (pid, before_pid) = {
                    let model = self.model.peek();
                    (model.session.pids.get(&key).copied(), before.as_ref().and_then(|b| model.session.pids.get(b).copied()))
                };
                let Some(rt) = runtime else { return };
                let window = if before.is_none() { native::place() } else { None };
                let shown = rt.borrow_mut().show(&key, pid, before.as_deref(), before_pid, skipped, window);
                if shown {
                    self.spread(&rt);
                } else {
                    self.dispatch(Event::NotShown(Box::new(*unshown)));
                }
            }
            Effect::HideManager => native::hide(runtime.and_then(|rt| rt.borrow().place)),
            Effect::ShowManager => {
                native::show(runtime.as_ref().and_then(|rt| rt.borrow().place));
                if let Some(rt) = runtime {
                    self.spread(&rt);
                }
            }
            Effect::Place(key) => {
                let (pid, playing) = {
                    let model = self.model.peek();
                    (model.session.pids.get(&key).copied(), model.playing.clone())
                };
                let (Some(pid), Some(rt)) = (pid, runtime) else { return };
                let window = if rt.borrow().place.is_none() { native::place() } else { None };
                if rt.borrow_mut().place_new(&key, pid, playing.as_deref(), window) {
                    self.dispatch(Event::Placed { key });
                }
            }
            Effect::Follow(key) => {
                let pid = key.as_ref().and_then(|k| self.model.peek().session.pids.get(k).copied());
                let Some(rt) = runtime else { return };
                rt.borrow_mut().follow_shown(key.as_deref(), pid);
            }
            Effect::Tint { key, pid } => {
                let model = self.model.peek();
                if let Some(account) = model.games.get(&key).and_then(|g| model.account(g.account_id)) {
                    game_window::tint(pid, account);
                }
            }
            Effect::Stopped(keys) => {
                let text = match keys.as_slice() {
                    [one] => self.model.peek().games.get(one).map_or_else(|| tr("Gestoppt"), |g| trf("%@ gestoppt", &[&g.name])),
                    many => trf("%lld gestoppt", &[&many.len()]),
                };
                toast::show(self.ui, text, Some(EventHandler::new(move |_| self.dispatch(Event::StartAgain(keys.clone())))));
            }
            Effect::ReadMemory => self.read_memory(),
            Effect::Retain => {
                let frames: HashSet<String> = self.model.peek().frames.keys().cloned().collect();
                self.previews.write().retain(|k, _| frames.contains(k));
                let accounts: HashSet<Uuid> = self.model.peek().accounts.iter().map(|a| a.id).collect();
                self.faces.write().retain(|id, _| accounts.contains(id));
            }
            Effect::ShowInstance(id) => self.ui.page.set(Page::Instance(id)),
            Effect::SaveColor { ids, color } => {
                let (tx, rx) = async_channel::bounded(1);
                let _ = std::thread::Builder::new().name("color".into()).spawn(move || {
                    let _ = tx.send_blocking(crate::ui::remote::set_color(&ids, color));
                });
                spawn_in_window(async move {
                    if let Ok(Err(e)) = rx.recv().await {
                        eprintln!("color: {e:#}");
                        toast::show(self.ui, tr("Die Farbe wurde nicht gesichert. Versuch es noch einmal."), None);
                    }
                    self.reload(true);
                });
            }
            Effect::AddAccount => self.ui.page.set(Page::Accounts),
            Effect::AskQuit { running } => self.ask_quit(running),
            Effect::Quit { keep } => {
                if let Some(rt) = runtime {
                    rt.borrow_mut().keep_games = keep;
                }
                Platform::get().exit();
            }
            Effect::ToTray { .. } => native::hide(None),
            Effect::Notify { title, body, .. } => drive::note(format!("notify {title} | {body}")),
            Effect::RememberAccount { instance, account } => {
                std::thread::spawn(move || {
                    if let Err(e) = crate::ui::remote::remember_account(&instance, account) {
                        eprintln!("last account: {e:#}");
                    }
                });
            }
            other => drive::note(format!("freya: not carried out yet: {}", effect_name(&other))),
        }
    }

    fn spread(&self, rt: &Rc<RefCell<Runtime>>) {
        let (playing, pids) = {
            let model = self.model.peek();
            (model.playing.clone(), model.session.pids.clone())
        };
        rt.borrow_mut().spread(playing.as_deref(), &pids);
    }

    fn read_memory(self) {
        if let Some(pretend) = drive::memory() {
            let used: u64 = {
                let m = self.model.peek();
                m.games.values().filter(|g| g.phase.has_process() && !matches!(g.phase, Phase::Starting { .. })).filter_map(|g| m.target(&g.batch_id)).map(|t| u64::from(t.memory_mb())).sum()
            };
            let memory = fit::Memory { total_mb: pretend.total_mb, free_mb: pretend.free_mb.saturating_sub(used) };
            spawn_in_window(async move { self.dispatch(Event::Memory(Some(memory))) });
            return;
        }
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(fit::system_memory());
        });
        spawn_in_window(async move {
            if let Ok(memory) = rx.recv().await {
                self.dispatch(Event::Memory(memory));
            }
        });
    }

    fn ask_quit(self, running: usize) {
        let title = if running == 1 { tr("1 Spiel läuft noch") } else { trf("%lld Spiele laufen noch", &[&running]) };
        let message = if running == 1 { tr("Beim nächsten Start findet Manycraft es wieder.") } else { tr("Beim nächsten Start findet Manycraft sie wieder.") };
        let end = sheet::Primary { label: tr("Alle beenden"), disabled: None, danger: true, run: EventHandler::new(move |_| {
            sheet::close(self.ui);
            self.dispatch(Event::QuitChosen { keep: false });
        }) };
        let keep = sheet::Primary { label: tr("Weiterlaufen lassen"), disabled: None, danger: false, run: EventHandler::new(move |_| {
            sheet::close(self.ui);
            self.dispatch(Event::QuitChosen { keep: true });
        }) };
        sheet::open(self.ui, sheet::Card::new("quit", title).message(message).secondary(end).primary(keep).width(super::theme::frame::SHEET * 0.8));
    }

    /// Tells the core and the fake fleet which games have a tile on screen.
    pub fn set_visible(self, keys: HashSet<String>) {
        if self.model.peek().session.visible == keys {
            return;
        }
        if let Some(rt) = self.runtime() {
            rt.borrow().share_visible(keys.clone());
        }
        self.dispatch(Event::Visible(keys));
    }

    /// Ends or leaves the games as the user chose, at the end of the run.
    pub fn close(self) {
        if let Some(rt) = self.runtime() {
            let rt = rt.borrow();
            match rt.close() {
                Ok(closing) => drop(closing.join()),
                Err(_) if rt.keep_games => rt.backend.leave(),
                Err(_) => rt.backend.quit(),
            }
        }
    }
}

/// The context hook for the store.
pub fn use_store() -> Store {
    use_consume::<Store>()
}

fn effect_name(effect: &Effect) -> String {
    let text = format!("{effect:?}");
    text.split([' ', '(', '{']).next().unwrap_or_default().to_owned()
}
