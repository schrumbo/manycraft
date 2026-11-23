use std::path::PathBuf;
use std::sync::Arc;

use freya::prelude::*;
use uuid::Uuid;

use super::desk::{Desk, Hung, Look, Views, handle, ticket};
use super::lane::{Lane, Op};
use super::text::problem;
use crate::core::account::{Account, AccountKind};
use crate::core::i18n::{tr, trf};
use crate::core::skin_change::{self as change, Model};
use crate::core::wardrobe::{self, Added, Fetched};
use crate::core::drive;
use crate::ui::forms::Problem;
use crate::view::kit::{off_thread, toast};
use crate::view::store::Store;

async fn ask() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new().set_title(tr("Skin wählen")).add_filter(tr("PNG-Bild"), &["png"]).pick_file().await.map(|f| f.path().to_path_buf())
}

/// Why the skin being worn cannot be removed.
pub fn keep_text() -> String {
    tr("Dieser Skin ist angezogen. Zieh erst einen anderen an.")
}

fn looks_like_link(input: &str) -> bool {
    input.contains('/') || input.contains('.')
}

/// A skin that was added, with what the shelf needs to show it at once.
type Arrived = (Added, Vec<u8>, image::RgbaImage);

fn store(checked: Vec<(Fetched, image::RgbaImage)>) -> (Vec<Arrived>, Option<anyhow::Error>) {
    let mut arrived = Vec::new();
    for (f, front) in checked {
        match wardrobe::add(&f) {
            Ok(added) => arrived.push((added, f.png, front)),
            Err(e) => return (arrived, Some(e)),
        }
    }
    (arrived, None)
}

/// What the Wardrobe page does: it holds the page's own state, such as the text being typed, and hands everything that
/// touches accounts or the library to the [Desk], which works in the shared state.
#[derive(Clone, Copy, PartialEq)]
pub struct Ctl {
    pub store: Store,
    pub shared: super::Shared,
    pub text: State<String>,
    pub choosing: State<bool>,
    pub add_problem: State<Option<Problem>>,
    pub add_details: State<bool>,
    pub anchor: State<Option<crate::view::kit::Bounds>>,
    pub details: State<bool>,
    pub grid: State<f32>,
}

impl Ctl {
    fn alive(self) -> bool {
        self.choosing.try_peek().is_some()
    }

    pub fn desk(self) -> Desk {
        Desk { store: self.store, shared: self.shared }
    }

    pub fn account(self) -> Option<Account> {
        let id = (*self.shared.chosen.peek())?;
        self.desk().account(id)
    }

    /// What the chosen account wears, with the changes still in line; redraws the page when it changes.
    pub fn look(self) -> Option<Look> {
        drop(self.shared.lanes.read());
        self.desk().shown((*self.shared.chosen.peek())?)
    }

    /// The work of the chosen account.
    pub fn lane(self) -> Lane {
        drop(self.shared.lanes.read());
        (*self.shared.chosen.peek()).map(|id| self.desk().lane(id)).unwrap_or_default()
    }

    pub fn shelf(self) -> Vec<Hung> {
        self.shared.shelf.read().clone()
    }

    pub fn views(self) -> Views {
        self.shared.views.read().clone()
    }

    pub fn adds(self) -> Vec<(u64, String)> {
        self.shared.adds.read().clone()
    }

    pub fn start(mut self) {
        let m = self.store.model.peek();
        let known = self.shared.chosen.peek().filter(|id| m.account(*id).is_some());
        let pick = known
            .or_else(|| m.accounts.iter().find(|a| a.kind == AccountKind::Microsoft).map(|a| a.id))
            .or_else(|| crate::app::reduce::active_account(&m))
            .or_else(|| m.accounts.first().map(|a| a.id));
        drop(m);
        self.shared.chosen.set(pick);
        let desk = self.desk();
        desk.redraw();
        desk.reload_shelf();
        if let Some(id) = pick {
            desk.open(id, false);
        }
    }

    pub fn choose(mut self, id: Uuid) {
        if *self.shared.chosen.peek() == Some(id) {
            return;
        }
        self.shared.chosen.set(Some(id));
        let desk = self.desk();
        desk.redraw();
        desk.open(id, false);
    }

    /// Asks Minecraft services again what the chosen account wears.
    pub fn reload(self) {
        if let Some(id) = *self.shared.chosen.peek() {
            self.desk().ask(id, Op::Refresh);
        }
    }

    pub fn wear_skin(self, file: &str) {
        let Some(account) = self.account() else { return };
        if account.kind != AccountKind::Microsoft {
            return self.desk().offline_hint();
        }
        let Some(hung) = self.shared.shelf.peek().iter().find(|h| h.skin.file == file).cloned() else { return };
        if self.look().and_then(|l| l.worn).as_deref() == Some(file) {
            return;
        }
        let Some(image) = Desk::picture(&hung.png) else {
            toast::show(self.store.ui, tr("Dieses Bild lässt sich nicht lesen."), None);
            return;
        };
        self.desk().ask(account.id, Op::Skin { file: hung.skin.file, png: hung.png, image, model: hung.skin.model });
    }

    pub fn wear_cape(self, id: Option<String>) {
        let Some(account) = self.account() else { return };
        if account.kind != AccountKind::Microsoft || self.look().is_some_and(|l| l.cape == id) {
            return;
        }
        self.desk().ask(account.id, Op::Cape(id));
    }

    pub fn add_typed(mut self) {
        let input = self.text.peek().trim().to_owned();
        if input.is_empty() {
            return;
        }
        self.text.set(String::new());
        let typed = input.clone();
        self.run_add(input.clone(), move || if looks_like_link(&input) { wardrobe::fetch_link(&input) } else { wardrobe::fetch_player(&input) }.map(|f| vec![f]), Some(typed));
    }

    pub fn choose_file(mut self) {
        if *self.choosing.peek() {
            return;
        }
        self.choosing.set(true);
        spawn_in_window(async move {
            let path = if drive::on() { drive::answer("skin file").and_then(|p| p.into_iter().next()) } else { ask().await };
            if !self.alive() {
                return;
            }
            let mut choosing = self.choosing;
            choosing.set(false);
            if let Some(path) = path {
                self.add_paths(vec![path]);
            }
        });
    }

    pub fn add_paths(self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let label = match paths.as_slice() {
            [one] => one.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            many => trf("%lld Dateien", &[&many.len()]),
        };
        self.run_add(
            label,
            move || {
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
            },
            None,
        );
    }

    fn run_add(mut self, label: String, work: impl FnOnce() -> anyhow::Result<Vec<Fetched>> + Send + 'static, typed: Option<String>) {
        let n = ticket();
        let mut adds = self.shared.adds;
        adds.write().push((n, label));
        self.add_problem.set(None);
        let Some(rx) = off_thread("wardrobe-add", move || {
            let mut checked: Vec<(Fetched, image::RgbaImage)> = Vec::new();
            for f in work()? {
                let image = change::check_png(&f.png)?;
                let (front, _) = change::front_and_back(&image, f.model);
                checked.push((f, front));
            }
            Ok::<_, anyhow::Error>(checked)
        }) else {
            adds.write().retain(|(i, _)| *i != n);
            return;
        };
        spawn_in_window(async move {
            let done = rx.recv().await;
            let desk = self.desk();
            let Ok(done) = done else {
                adds.write().retain(|(i, _)| *i != n);
                return;
            };
            let (arrived, error) = match done {
                Ok(checked) => match Desk::settle(Desk::enqueue(move || store(checked))).await {
                    Some(stored) => stored,
                    None => (Vec::new(), None),
                },
                Err(e) => (Vec::new(), Some(e)),
            };
            adds.write().retain(|(i, _)| *i != n);
            if !arrived.is_empty() {
                let mut shelf = self.shared.shelf;
                for (added, png, front) in &arrived {
                    if added.new
                        && !shelf.peek().iter().any(|h| h.skin.file == added.skin.file)
                        && let Some(thumb) = handle(front)
                    {
                        shelf.write().insert(0, Hung { skin: added.skin.clone(), thumb, png: Arc::new(png.clone()) });
                    }
                }
                desk.reload_shelf();
                let news = arrived.iter().filter(|(a, ..)| a.new).count();
                let message = match (arrived.as_slice(), news) {
                    ([(one, ..)], 1) => trf("„%@“ liegt jetzt in der Garderobe.", &[&one.skin.name]),
                    ([(one, ..)], _) => trf("„%@“ war schon in der Garderobe.", &[&one.skin.name]),
                    (_, n) => trf("%lld neue Skins in der Garderobe.", &[&n]),
                };
                toast::show(self.store.ui, message, None);
            }
            if let Some(e) = error {
                let found = problem(&e);
                if self.alive() {
                    let mut text = self.text;
                    if let Some(typed) = typed
                        && text.peek().is_empty()
                    {
                        text.set(typed);
                    }
                    let mut add_problem = self.add_problem;
                    add_problem.set(Some(found));
                } else {
                    toast::show(self.store.ui, found.text, None);
                }
            }
        });
    }

    /// Takes a skin out of the library; the toast puts it back where it was.
    pub fn remove(self, file: &str) {
        let mut shelf = self.shared.shelf;
        let Some(at) = shelf.peek().iter().position(|h| h.skin.file == file) else { return };
        if self.lane().holds(file) {
            toast::show(self.store.ui, keep_text(), None);
            return;
        }
        let hung = shelf.write().remove(at);
        let desk = self.desk();
        let (gone, name) = (hung.skin.file.clone(), hung.skin.name.clone());
        desk.library(move || wardrobe::remove(&gone));
        let store = self.store;
        toast::show_kept(
            store.ui,
            trf("„%@“ aus der Garderobe entfernt.", &[&name]),
            EventHandler::new(move |_| {
                let mut shelf = shelf;
                let back = at.min(shelf.peek().len());
                if !shelf.peek().iter().any(|h| h.skin.file == hung.skin.file) {
                    shelf.write().insert(back, hung.clone());
                }
                let (skin, png) = (hung.skin.clone(), hung.png.clone());
                desk.library(move || wardrobe::restore(&skin, &png, at));
            }),
        );
    }

    pub fn rename(self, file: &str, name: &str) {
        let (file, name) = (file.to_owned(), name.to_owned());
        let mut shelf = self.shared.shelf;
        if let Some(h) = shelf.write().iter_mut().find(|h| h.skin.file == file) {
            h.skin.name.clone_from(&name);
        }
        self.desk().library(move || wardrobe::rename(&file, &name));
    }

    pub fn set_model(self, file: &str, model: Model) {
        let file = file.to_owned();
        let mut shelf = self.shared.shelf;
        if let Some(h) = shelf.write().iter_mut().find(|h| h.skin.file == file) {
            h.skin.model = model;
            if let Some(image) = Desk::picture(&h.png)
                && let Some(thumb) = handle(&change::front_and_back(&image, model).0)
            {
                h.thumb = thumb;
            }
        }
        self.desk().library(move || wardrobe::set_model(&file, model));
    }
}
