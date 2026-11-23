use std::path::PathBuf;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;
use image::RgbaImage;
use uuid::Uuid;

use super::text::{done_text, problem, signs_in_again};
use crate::core::account::{Account, AccountKind};
use crate::core::i18n::{tr, trf};
use crate::core::skin_change::{self as change, Model};
use crate::core::wardrobe::{self, Fetched, Outfit, Skin, Told};
use crate::core::{drive, skin};
use crate::ui::forms::Problem;
use crate::view::kit::{off_thread, toast};
use crate::view::store::Store;

const FACE_SCALE: u32 = 4;

/// A skin of the library with its small picture.
#[derive(Clone, PartialEq)]
pub struct Hung {
    pub skin: Skin,
    pub thumb: ImageHandle,
}

/// A cape the account owns.
#[derive(Clone, PartialEq)]
pub struct Owns {
    pub id: String,
    pub alias: String,
    pub thumb: Option<ImageHandle>,
    pub front: Option<RgbaImage>,
}

/// What the stage shows of the chosen account.
#[derive(Clone)]
pub struct Look {
    pub skin: Option<RgbaImage>,
    pub model: Model,
    pub worn: Option<String>,
    pub capes: Vec<Owns>,
    pub cape: Option<String>,
    pub microsoft: bool,
}

/// Where the account's outfit comes from right now.
#[derive(Clone, PartialEq)]
pub enum Phase {
    Idle,
    Refreshing,
    Failed(Problem, bool),
}

pub type Views = Option<(ImageHandle, ImageHandle)>;

struct Data {
    skin: Option<RgbaImage>,
    model: Model,
    worn: Option<String>,
    capes: Vec<(String, String, Option<RgbaImage>)>,
    cape: Option<String>,
    microsoft: bool,
}

fn data(account: &Account, outfit: Option<&Outfit>) -> Data {
    if account.kind != AccountKind::Microsoft {
        let own = wardrobe::default_for(account.id);
        return Data { worn: own.as_ref().map(|(image, _)| wardrobe::file_of(image)), model: own.as_ref().map(|(_, m)| *m).unwrap_or_default(), skin: own.map(|(image, _)| image), capes: Vec::new(), cape: None, microsoft: false };
    }
    let Some(outfit) = outfit else {
        return Data { skin: None, model: Model::Classic, worn: None, capes: Vec::new(), cape: None, microsoft: true };
    };
    let skin = outfit.skin.as_deref().and_then(wardrobe::texture);
    Data {
        worn: skin.as_ref().map(wardrobe::file_of),
        skin,
        model: outfit.model,
        capes: outfit.capes.iter().map(|c| (c.id.clone(), c.alias.clone(), wardrobe::texture(&c.texture).and_then(|t| change::cape_front(&t)))).collect(),
        cape: outfit.cape.clone(),
        microsoft: true,
    }
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

async fn ask() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new().set_title(tr("Skin wählen")).add_filter(tr("PNG-Bild"), &["png"]).pick_file().await.map(|f| f.path().to_path_buf())
}

fn looks_like_link(input: &str) -> bool {
    input.contains('/') || input.contains('.')
}

/// Everything the Wardrobe page does: it holds the page's state and runs the work off the UI thread.
#[derive(Clone, Copy, PartialEq)]
pub struct Ctl {
    pub store: Store,
    pub chosen: State<Option<Uuid>>,
    pub shelf: State<Vec<Hung>>,
    pub look: State<Option<Look>>,
    pub phase: State<Phase>,
    pub views: State<Views>,
    pub busy: State<Option<String>>,
    pub text: State<String>,
    pub adding: State<bool>,
    pub add_problem: State<Option<Problem>>,
    pub error: State<Option<(Problem, bool)>>,
    pub anchor: State<Option<crate::view::kit::Bounds>>,
    pub details: State<bool>,
    pub add_details: State<bool>,
    pub grid: State<f32>,
}

impl Ctl {
    fn alive(self) -> bool {
        self.look.try_peek().is_some()
    }

    pub fn account(self) -> Option<Account> {
        let id = (*self.chosen.peek())?;
        self.store.model.peek().account(id).cloned()
    }

    fn still(self, id: Uuid) -> bool {
        self.alive() && *self.chosen.peek() == Some(id)
    }

    pub fn start(mut self) {
        let m = self.store.model.peek();
        let known = self.chosen.peek().filter(|id| m.account(*id).is_some());
        let pick = known
            .or_else(|| m.accounts.iter().find(|a| a.kind == AccountKind::Microsoft).map(|a| a.id))
            .or_else(|| crate::app::reduce::active_account(&m))
            .or_else(|| m.accounts.first().map(|a| a.id));
        drop(m);
        self.chosen.set(pick);
        self.reload_shelf();
        self.load(false);
    }

    pub fn choose(mut self, id: Uuid) {
        if *self.chosen.peek() == Some(id) {
            return;
        }
        self.chosen.set(Some(id));
        self.error.set(None);
        self.phase.set(Phase::Idle);
        self.load(false);
    }

    pub fn reload_shelf(self) {
        let accounts: Vec<Account> = self.store.model.peek().accounts.iter().filter(|a| a.kind == AccountKind::Microsoft).cloned().collect();
        let Some(rx) = off_thread("wardrobe-shelf", move || {
            accounts.iter().for_each(wardrobe::adopt_recent);
            wardrobe::list().into_iter().filter_map(|s| {
                let image = wardrobe::image(&s)?;
                let (front, _) = change::front_and_back(&image, s.model);
                Some((s, front))
            }).collect::<Vec<_>>()
        }) else {
            return;
        };
        spawn_in_window(async move {
            let Ok(found) = rx.recv().await else { return };
            if !self.alive() {
                return;
            }
            let mut shelf = self.shelf;
            shelf.set(found.into_iter().filter_map(|(skin, front)| Some(Hung { skin, thumb: handle(&front)? })).collect());
        });
    }

    fn set_look(mut self, d: Data) {
        let capes = d.capes.into_iter().map(|(id, alias, front)| Owns { thumb: front.as_ref().and_then(handle), id, alias, front }).collect();
        self.look.set(Some(Look { skin: d.skin, model: d.model, worn: d.worn, capes, cape: d.cape, microsoft: d.microsoft }));
        self.redraw();
    }

    pub fn redraw(mut self) {
        let views = self.look.peek().as_ref().and_then(|look| {
            let skin = look.skin.as_ref()?;
            let (front, mut back) = change::front_and_back(skin, look.model);
            if let Some(cloak) = look.cape.as_deref().and_then(|id| look.capes.iter().find(|c| c.id == id)).and_then(|c| c.front.as_ref()) {
                change::hang_cape(&mut back, cloak);
            }
            Some((handle(&front)?, handle(&back)?))
        });
        self.views.set(views);
    }

    pub fn load(mut self, force: bool) {
        let Some(account) = self.account() else {
            self.look.set(None);
            self.views.set(None);
            return;
        };
        let id = account.id;
        let Some(rx) = off_thread("wardrobe-cache", move || {
            let cached = wardrobe::cached(account.id);
            let stale = account.kind == AccountKind::Microsoft && (force || cached.as_ref().is_none_or(|o| !o.fresh()));
            (data(&account, cached.as_ref()), stale.then_some(account))
        }) else {
            return;
        };
        spawn_in_window(async move {
            let Ok((shown, ask)) = rx.recv().await else { return };
            if !self.still(id) {
                return;
            }
            self.set_look(shown);
            let Some(account) = ask else { return };
            self.refresh(account);
        });
    }

    fn refresh(mut self, account: Account) {
        let id = account.id;
        self.phase.set(Phase::Refreshing);
        let Some(rx) = off_thread("wardrobe-refresh", move || wardrobe::refresh(&account).map(|told| (data(&account, Some(&told.outfit)), told.face))) else {
            self.phase.set(Phase::Idle);
            return;
        };
        spawn_in_window(async move {
            let Ok(done) = rx.recv().await else { return };
            if !self.alive() {
                return;
            }
            match done {
                Ok((fresh, face)) => {
                    if let Some(face) = face {
                        show_face(self.store, id, face);
                    }
                    if self.still(id) {
                        self.set_look(fresh);
                        let mut phase = self.phase;
                        phase.set(Phase::Idle);
                        self.reload_shelf();
                    }
                }
                Err(e) if self.still(id) => {
                    let mut phase = self.phase;
                    phase.set(Phase::Failed(problem(&e), signs_in_again(&e)));
                }
                Err(_) => {}
            }
        });
    }

    pub fn can_wear(self) -> bool {
        self.busy.peek().is_none() && self.look.peek().as_ref().is_some_and(|l| l.microsoft)
    }

    pub fn wear_skin(self, file: &str) {
        if !self.can_wear() {
            return;
        }
        let Some(skin) = self.shelf.peek().iter().find(|h| h.skin.file == file).map(|h| h.skin.clone()) else { return };
        let worn = self.look.peek().as_ref().and_then(|l| l.worn.clone());
        if worn.as_deref() == Some(file) {
            return;
        }
        self.wear(format!("skin:{file}"), move |account| wardrobe::wear(account, Some((&skin, skin.model)), None), true, false);
    }

    pub fn wear_cape(self, id: Option<String>) {
        if !self.can_wear() || self.look.peek().as_ref().is_some_and(|l| l.cape == id) {
            return;
        }
        let key = format!("cape:{}", id.as_deref().unwrap_or(""));
        self.wear(key, move |account| wardrobe::wear(account, None, Some(id.as_deref())), false, true);
    }

    fn wear(mut self, key: String, work: impl FnOnce(&Account) -> wardrobe::Changed + Send + 'static, skin_asked: bool, cape_asked: bool) {
        let Some(account) = self.account() else { return };
        let id = account.id;
        self.busy.set(Some(key));
        self.error.set(None);
        let Some(rx) = off_thread("wardrobe-wear", move || {
            let changed = work(&account);
            let shown = changed.told.as_ref().map(|t| data(&account, Some(&t.outfit)));
            (changed, shown)
        }) else {
            self.busy.set(None);
            self.error.set(Some((Problem::plain(tr("Die Änderung lässt sich gerade nicht senden. Versuch es noch einmal.")), false)));
            return;
        };
        spawn_in_window(async move {
            let Ok((changed, shown)) = rx.recv().await else { return };
            if let Some(Told { face: Some(face), .. }) = &changed.told {
                show_face(self.store, id, face.clone());
            }
            if !self.alive() {
                let text = changed.error.as_ref().map_or_else(|| done_text(skin_asked, cape_asked), |e| problem(e).text);
                toast::show(self.store.ui, text, None);
                return;
            }
            let mut busy = self.busy;
            busy.set(None);
            if self.still(id) {
                if let Some(shown) = shown {
                    self.set_look(shown);
                }
            }
            match changed.error {
                Some(e) => {
                    let mut error = self.error;
                    error.set(Some((problem(&e), signs_in_again(&e))));
                }
                None => {
                    toast::show(self.store.ui, done_text(skin_asked, cape_asked), None);
                }
            }
            if skin_asked {
                self.reload_shelf();
            }
        });
    }

    pub fn add_typed(self) {
        let input = self.text.peek().trim().to_owned();
        if input.is_empty() || *self.adding.peek() {
            return;
        }
        self.run_add(move || if looks_like_link(&input) { wardrobe::fetch_link(&input) } else { wardrobe::fetch_player(&input) }.map(|f| vec![f]), true);
    }

    pub fn choose_file(mut self) {
        if *self.adding.peek() {
            return;
        }
        self.adding.set(true);
        spawn_in_window(async move {
            let path = if drive::on() { drive::answer("skin file").and_then(|p| p.into_iter().next()) } else { ask().await };
            if !self.alive() {
                return;
            }
            let mut adding = self.adding;
            adding.set(false);
            if let Some(path) = path {
                self.add_paths(vec![path]);
            }
        });
    }

    pub fn add_paths(self, paths: Vec<PathBuf>) {
        if paths.is_empty() || *self.adding.peek() {
            return;
        }
        self.run_add(move || {
            let mut found = Vec::new();
            let mut first_error = None;
            for path in paths {
                match wardrobe::from_file(&path) {
                    Ok(f) => found.push(f),
                    Err(e) if first_error.is_none() => first_error = Some(e),
                    Err(_) => {}
                }
            }
            match (found.is_empty(), first_error) {
                (true, Some(e)) => Err(e),
                _ => Ok(found),
            }
        }, false);
    }

    fn run_add(mut self, work: impl FnOnce() -> anyhow::Result<Vec<Fetched>> + Send + 'static, typed: bool) {
        self.adding.set(true);
        self.add_problem.set(None);
        let Some(rx) = off_thread("wardrobe-add", move || {
            let fetched = work()?;
            let mut added = Vec::new();
            for f in &fetched {
                added.push(wardrobe::add(f)?);
            }
            Ok::<_, anyhow::Error>(added)
        }) else {
            self.adding.set(false);
            return;
        };
        spawn_in_window(async move {
            let Ok(done) = rx.recv().await else { return };
            if !self.alive() {
                return;
            }
            let mut adding = self.adding;
            adding.set(false);
            match done {
                Ok(added) => {
                    let mut text = self.text;
                    if typed {
                        text.set(String::new());
                    }
                    self.reload_shelf();
                    let news = added.iter().filter(|a| a.new).count();
                    let message = match (added.as_slice(), news) {
                        ([one], 1) => trf("„%@“ liegt jetzt in der Garderobe.", &[&one.skin.name]),
                        ([one], _) => trf("„%@“ war schon in der Garderobe.", &[&one.skin.name]),
                        (_, n) => trf("%lld neue Skins in der Garderobe.", &[&n]),
                    };
                    toast::show(self.store.ui, message, None);
                }
                Err(e) => {
                    let mut add_problem = self.add_problem;
                    add_problem.set(Some(problem(&e)));
                }
            }
        });
    }

    pub fn remove(self, file: String) {
        let Some(hung) = self.shelf.peek().iter().find(|h| h.skin.file == file).cloned() else { return };
        let Ok(png) = wardrobe::bytes(&hung.skin) else { return };
        if wardrobe::remove(&file).is_err() {
            return;
        }
        self.reload_shelf();
        let skin = hung.skin;
        let name = skin.name.clone();
        toast::show(
            self.store.ui,
            trf("„%@“ aus der Garderobe entfernt.", &[&name]),
            Some(EventHandler::new(move |_| {
                let restored = Fetched { png: png.clone(), model: skin.model, name: skin.name.clone(), origin: skin.origin, from: skin.from.clone() };
                let _ = wardrobe::add(&restored);
                if self.alive() {
                    self.reload_shelf();
                }
            })),
        );
    }

    pub fn rename(self, file: &str, name: &str) {
        let _ = wardrobe::rename(file, name);
        self.reload_shelf();
    }

    pub fn set_model(self, file: &str, model: Model) {
        let _ = wardrobe::set_model(file, model);
        self.reload_shelf();
    }
}
