//! Runs the Wardrobe's lanes: reads what an account wears, applies the changes in line one at a time off the UI thread and
//! keeps what the page shows in step. Everything here lives in the shared state, so work goes on when the page is left.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;
use image::RgbaImage;
use uuid::Uuid;

use super::lane::{Failure, Job, Op};
use super::text::{done_text, problem, signs_in_again};
use crate::core::account::{Account, AccountKind};
use crate::core::i18n::tr;
use crate::core::skin_change::{self as change, Model};
use crate::core::wardrobe::{self, Outfit, Skin, Step, Told};
use crate::core::skin;
use crate::view::kit::{off_thread, toast};
use crate::view::store::Store;
use crate::view::ui::Page;

use super::Shared;

const FACE_SCALE: u32 = 4;

static JOBS: AtomicU64 = AtomicU64::new(1);

/// A number that has not been used before, to tell jobs and arriving skins apart.
pub fn ticket() -> u64 {
    JOBS.fetch_add(1, Ordering::Relaxed)
}
static SHELVES: AtomicU64 = AtomicU64::new(1);
static LIBRARY_BUSY: AtomicUsize = AtomicUsize::new(0);

/// A skin of the library with its small picture and its file.
#[derive(Clone, PartialEq)]
pub struct Hung {
    pub skin: Skin,
    pub thumb: ImageHandle,
    pub png: Arc<Vec<u8>>,
}

/// A cape the account owns.
#[derive(Clone, PartialEq)]
pub struct Owns {
    pub id: String,
    pub alias: String,
    pub thumb: Option<ImageHandle>,
    pub front: Option<RgbaImage>,
}

/// What the stage shows of an account.
#[derive(Clone)]
pub struct Look {
    pub skin: Option<Arc<RgbaImage>>,
    pub model: Model,
    pub worn: Option<String>,
    pub capes: Vec<Owns>,
    pub cape: Option<String>,
    pub microsoft: bool,
    pub known: bool,
    pub fetched: u64,
}

/// What the pictures on the stage were drawn from, so they are drawn again only when it changes.
#[derive(Clone, PartialEq)]
pub struct Drawn {
    worn: Option<String>,
    cape: Option<String>,
    capes: usize,
    model: Model,
    skin: bool,
}

pub type Views = Option<(ImageHandle, ImageHandle)>;

struct Data {
    skin: Option<RgbaImage>,
    model: Model,
    worn: Option<String>,
    capes: Vec<(String, String, Option<RgbaImage>)>,
    cape: Option<String>,
    microsoft: bool,
    known: bool,
    fetched: u64,
}

fn data(account: &Account, outfit: Option<&Outfit>) -> Data {
    if account.kind != AccountKind::Microsoft {
        let own = wardrobe::default_for(account.id);
        return Data { worn: own.as_ref().map(|(image, _)| wardrobe::file_of(image)), model: own.as_ref().map(|(_, m)| *m).unwrap_or_default(), skin: own.map(|(image, _)| image), capes: Vec::new(), cape: None, microsoft: false, known: true, fetched: 0 };
    }
    let Some(outfit) = outfit else {
        return Data { skin: None, model: Model::Classic, worn: None, capes: Vec::new(), cape: None, microsoft: true, known: false, fetched: 0 };
    };
    let skin = outfit.skin.as_deref().and_then(wardrobe::texture);
    Data {
        worn: skin.as_ref().map(wardrobe::file_of),
        skin,
        model: outfit.model,
        capes: outfit.capes.iter().map(|c| (c.id.clone(), c.alias.clone(), wardrobe::texture(&c.texture).and_then(|t| change::cape_front(&t)))).collect(),
        cape: outfit.cape.clone(),
        microsoft: true,
        known: true,
        fetched: outfit.fetched,
    }
}

fn look_of(d: Data) -> Look {
    let capes = d.capes.into_iter().map(|(id, alias, front)| Owns { thumb: front.as_ref().and_then(handle), id, alias, front }).collect();
    Look { skin: d.skin.map(Arc::new), model: d.model, worn: d.worn, capes, cape: d.cape, microsoft: d.microsoft, known: d.known, fetched: d.fetched }
}

pub fn handle(image: &RgbaImage) -> Option<ImageHandle> {
    ImageHandle::from_rgba(image.width(), image.height(), Bytes::from(image.as_raw().clone()), AlphaType::Unpremul)
}

fn show_face(store: Store, id: Uuid, face: Vec<u8>) {
    let Some(image) = RgbaImage::from_raw(skin::SIDE, skin::SIDE, face) else { return };
    if let Some(shown) = handle(&change::scaled(&image, FACE_SCALE)) {
        let mut faces = store.faces;
        faces.write().insert(id, shown);
    }
}

enum Done {
    Refreshed(anyhow::Result<(Data, Option<Vec<u8>>)>),
    Worn(wardrobe::Changed, Option<Data>),
}

enum Msg {
    Step(Step),
    Done(Done),
}

fn run(account: &Account, op: Op, step: &dyn Fn(Step)) -> Done {
    let upload;
    let (skin, cape) = match &op {
        Op::Refresh => return Done::Refreshed(wardrobe::refresh(account, step).map(|t| (data(account, Some(&t.outfit)), t.face))),
        Op::Skin { png, model, .. } => {
            upload = (png.as_slice(), *model);
            (Some(upload), None)
        }
        Op::Cape(id) => (None, Some(id.as_deref())),
    };
    let changed = wardrobe::wear(account, skin, cape, step);
    let shown = changed.told.as_ref().map(|t| data(account, Some(&t.outfit)));
    Done::Worn(changed, shown)
}

type Task = Box<dyn FnOnce() + Send>;

fn library_worker() -> &'static mpsc::Sender<Task> {
    static WORKER: OnceLock<mpsc::Sender<Task>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Task>();
        let _ = std::thread::Builder::new().name("wardrobe-library".into()).spawn(move || rx.into_iter().for_each(|task| task()));
        tx
    })
}

/// The Wardrobe's data and the work on it.
#[derive(Clone, Copy, PartialEq)]
pub struct Desk {
    pub store: Store,
    pub shared: Shared,
}

impl Desk {
    pub fn account(self, id: Uuid) -> Option<Account> {
        self.store.model.peek().account(id).cloned()
    }

    fn edit<T>(self, id: Uuid, change: impl FnOnce(&mut super::lane::Lane) -> T) -> T {
        let mut lanes = self.shared.lanes;
        let out = change(lanes.write().entry(id).or_default());
        self.redraw();
        out
    }

    /// What the page shows of account `id`: its confirmed look with every change in line applied.
    pub fn shown(self, id: Uuid) -> Option<Look> {
        let microsoft = self.account(id).is_none_or(|a| a.kind == AccountKind::Microsoft);
        self.shared.lanes.peek().get(&id).and_then(|lane| lane.shown(microsoft))
    }

    pub fn lane(self, id: Uuid) -> super::lane::Lane {
        self.shared.lanes.peek().get(&id).cloned().unwrap_or_default()
    }

    /// Draws the pictures of the chosen account again when what they show changed.
    pub fn redraw(mut self) {
        let look = (*self.shared.chosen.peek()).and_then(|id| self.shown(id));
        let key = look.as_ref().map(|l| Drawn { worn: l.worn.clone(), cape: l.cape.clone(), capes: l.capes.len(), model: l.model, skin: l.skin.is_some() });
        if *self.shared.drawn.peek() == key {
            return;
        }
        let views = look.as_ref().and_then(|look| {
            let skin = look.skin.as_ref()?;
            let (front, mut back) = change::front_and_back(skin, look.model);
            if let Some(cloak) = look.cape.as_deref().and_then(|id| look.capes.iter().find(|c| c.id == id)).and_then(|c| c.front.as_ref()) {
                change::hang_cape(&mut back, cloak);
            }
            Some((handle(&front)?, handle(&back)?))
        });
        self.shared.drawn.set(key);
        self.shared.views.set(views);
    }

    /// Makes the look of `id` known: from memory when it is, else from the cache on disk, and asks Minecraft services in
    /// the background when what is known is old or `force`.
    pub fn open(self, id: Uuid, force: bool) {
        let Some(account) = self.account(id) else { return };
        let known = self.shared.lanes.peek().get(&id).and_then(|l| l.look.as_ref().map(|l| l.fetched));
        if let Some(fetched) = known {
            if stale(&account, fetched, force) {
                self.ask(id, Op::Refresh);
            }
            return;
        }
        let Some(rx) = off_thread("wardrobe-cache", move || {
            let cached = wardrobe::cached(account.id);
            let fetched = cached.as_ref().map_or(0, |o| o.fetched);
            let ask = stale(&account, fetched, force);
            (data(&account, cached.as_ref()), ask)
        }) else {
            return;
        };
        spawn_in_window(async move {
            let Ok((shown, ask)) = rx.recv().await else { return };
            self.edit(id, |lane| {
                if lane.look.is_none() {
                    lane.look = Some(look_of(shown));
                }
            });
            if ask {
                self.ask(id, Op::Refresh);
            }
        });
    }

    /// Puts a change in line for `id` and starts it when nothing runs.
    pub fn ask(self, id: Uuid, op: Op) {
        self.edit(id, |lane| lane.push(ticket(), op));
        self.pump(id);
    }

    fn watching(self, id: Uuid) -> bool {
        matches!(*self.store.ui.page.peek(), Page::Wardrobe) && *self.shared.chosen.peek() == Some(id)
    }

    fn pump(self, id: Uuid) {
        let Some(job) = self.edit(id, super::lane::Lane::next) else { return };
        let Some(account) = self.account(id) else {
            self.edit(id, |lane| lane.finish(job.id));
            return self.pump(id);
        };
        let (tx, rx) = async_channel::unbounded::<Msg>();
        let op = job.op.clone();
        let started = std::thread::Builder::new().name("wardrobe-lane".into()).spawn(move || {
            let done = run(&account, op, &|step| {
                let _ = tx.send_blocking(Msg::Step(step));
            });
            let _ = tx.send_blocking(Msg::Done(done));
        });
        if started.is_err() {
            self.edit(id, |lane| lane.finish(job.id));
            return self.pump(id);
        }
        spawn_in_window(async move {
            while let Ok(msg) = rx.recv().await {
                match msg {
                    Msg::Step(step) => self.edit(id, |lane| lane.step(job.id, step)),
                    Msg::Done(done) => {
                        self.land(id, &job, done);
                        return self.pump(id);
                    }
                }
            }
            self.edit(id, |lane| lane.finish(job.id));
            self.pump(id);
        });
    }

    fn land(self, id: Uuid, job: &Job, done: Done) {
        let Some(account) = self.account(id) else {
            self.edit(id, |lane| lane.finish(job.id));
            return;
        };
        let microsoft = account.kind == AccountKind::Microsoft;
        let watched = self.watching(id);
        match done {
            Done::Refreshed(Ok((fresh, face))) => {
                if let Some(face) = face {
                    show_face(self.store, id, face);
                }
                self.edit(id, |lane| {
                    lane.finish(job.id);
                    lane.load = None;
                    lane.look = Some(look_of(fresh));
                });
                self.reload_shelf();
            }
            Done::Refreshed(Err(e)) => {
                self.edit(id, |lane| {
                    lane.finish(job.id);
                    lane.load = Some((problem(&e), signs_in_again(&e)));
                });
            }
            Done::Worn(changed, shown) => {
                if let Some(Told { face: Some(face), .. }) = &changed.told {
                    show_face(self.store, id, face.clone());
                }
                let failure = changed.error.as_ref().map(|e| Failure { key: job.op.key(), problem: problem(e), signin: signs_in_again(e) });
                let refused = failure.as_ref().map(|f| f.problem.text.clone());
                let asked = (matches!(job.op, Op::Skin { .. }), matches!(job.op, Op::Cape(_)));
                self.edit(id, |lane| {
                    lane.finish(job.id);
                    match shown {
                        Some(shown) => lane.look = Some(look_of(shown)),
                        None if failure.is_none() => lane.bake(&job.op, microsoft),
                        None => {}
                    }
                    lane.failure = failure;
                });
                if !watched {
                    let text = refused.map_or_else(|| done_text(asked.0, asked.1), |text| text);
                    toast::show(self.store.ui, format!("{}: {text}", account.name), None);
                }
                if asked.0 {
                    self.reload_shelf();
                }
            }
        }
    }

    /// Reads the library again; skins already known are not decoded a second time.
    pub fn reload_shelf(self) {
        let generation = SHELVES.fetch_add(1, Ordering::Relaxed) + 1;
        let accounts: Vec<Account> = self.store.model.peek().accounts.iter().filter(|a| a.kind == AccountKind::Microsoft).cloned().collect();
        let known: HashSet<(String, bool)> = self.shared.shelf.peek().iter().map(|h| (h.skin.file.clone(), h.skin.model == Model::Slim)).collect();
        let Some(rx) = off_thread("wardrobe-shelf", move || {
            accounts.iter().for_each(wardrobe::adopt_recent);
            wardrobe::list()
                .into_iter()
                .map(|skin| {
                    let loaded = (!known.contains(&(skin.file.clone(), skin.model == Model::Slim))).then(|| load(&skin)).flatten();
                    (skin, loaded)
                })
                .collect::<Vec<_>>()
        }) else {
            return;
        };
        spawn_in_window(async move {
            let Ok(found) = rx.recv().await else { return };
            if generation != SHELVES.load(Ordering::Relaxed) || LIBRARY_BUSY.load(Ordering::Relaxed) > 0 {
                return;
            }
            let have: HashMap<String, Hung> = self.shared.shelf.peek().iter().map(|h| (h.skin.file.clone(), h.clone())).collect();
            let mut shelf = self.shared.shelf;
            shelf.set(
                found
                    .into_iter()
                    .filter_map(|(skin, loaded)| match loaded {
                        Some((png, front)) => Some(Hung { skin, thumb: handle(&front)?, png: Arc::new(png) }),
                        None => have.get(&skin.file).map(|h| Hung { skin, thumb: h.thumb.clone(), png: h.png.clone() }),
                    })
                    .collect(),
            );
        });
    }

    /// Runs `work` on the library after the library jobs asked before it; `done` gets what it came to on the UI thread.
    pub fn library(self, work: impl FnOnce() -> anyhow::Result<()> + Send + 'static) {
        let (tx, rx) = async_channel::bounded(1);
        LIBRARY_BUSY.fetch_add(1, Ordering::Relaxed);
        let sent = library_worker().send(Box::new(move || {
            let _ = tx.send_blocking(work());
        }));
        spawn_in_window(async move {
            let done = if sent.is_ok() { rx.recv().await.ok() } else { None };
            LIBRARY_BUSY.fetch_sub(1, Ordering::Relaxed);
            if let Some(Err(e)) = done {
                toast::show(self.store.ui, problem(&e).text, None);
            }
            if LIBRARY_BUSY.load(Ordering::Relaxed) == 0 {
                self.reload_shelf();
            }
        });
    }

    /// A skin a person asked for the first time put on: the decoded picture, or why it cannot be used.
    pub fn picture(png: &[u8]) -> Option<Arc<RgbaImage>> {
        change::check_png(png).ok().map(Arc::new)
    }

    pub fn offline_hint(self) {
        toast::show(self.store.ui, tr("Zum Anziehen brauchst du einen Microsoft-Account."), None);
    }
}

fn stale(account: &Account, fetched: u64, force: bool) -> bool {
    account.kind == AccountKind::Microsoft && (force || !wardrobe::fresh(fetched))
}

fn load(skin: &Skin) -> Option<(Vec<u8>, RgbaImage)> {
    let png = wardrobe::bytes(skin).ok()?;
    let image = change::check_png(&png).ok()?;
    let (front, _) = change::front_and_back(&image, skin.model);
    Some((png, front))
}
