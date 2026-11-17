//! The Mods page of an instance: the content browser on its own page, where every pick is added at once.

use std::collections::HashSet;

use freya::prelude::*;

use super::{Browser, Scope};
use crate::app::event::Event as AppEvent;
use crate::core::batch::ModRef;
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::core::modrinth::{Kind, Update};
use crate::core::snapshot::Reason;
use crate::ui::forms::{self, Problem};
use crate::ui::remote::{self, InstanceEdit};
use crate::view::instance::snapshots;
use crate::view::kit::{off_ui, toast};
use crate::view::store::{Store, use_store};
use crate::view::ui::Page;

/// The second line of the Mods page's title: version and loader, and that changes wait for the next start while it runs.
pub fn content_line(made: &Instance, running: bool) -> String {
    let base = trf("Von Modrinth für Minecraft %@ · %@", &[&made.game_version, &made.loader.name()]);
    if running { format!("{base} · {}", tr("Gilt beim nächsten Start")) } else { base }
}

/// Shows the Mods page of instance `id` on the tab of `kind`.
pub fn show(store: Store, id: &str, kind: Kind) {
    crate::view::kit::sheet::close(store.ui);
    let mut page = store.ui.page;
    page.set(Page::Browse(id.to_owned(), kind));
}

fn running(store: Store, id: &str) -> bool {
    store.model.peek().games.values().any(|g| g.instance_id == id && g.phase.is_active())
}

fn titles(mods: &[ModRef]) -> String {
    mods.iter().map(|m| m.title.as_str()).collect::<Vec<_>>().join(", ")
}

fn offer_added(store: Store, id: String, added: Vec<ModRef>) {
    let text = trf("%@ hinzugefügt.", &[&titles(&added)]);
    toast::show(store.ui, text, Some(EventHandler::new(move |_| take_back(store, id.clone(), added.clone()))));
}

fn take_back(store: Store, id: String, added: Vec<ModRef>) {
    let Some(made) = store.model.peek().instance(&id).cloned() else { return };
    let before = InstanceEdit::of(&made);
    let edit = InstanceEdit { mods: made.mods.iter().filter(|m| !added.iter().any(|a| a.project_id == m.project_id)).cloned().collect(), ..before.clone() };
    spawn_in_window(async move {
        let work = id.clone();
        match off_ui(move || remote::save_instance(&work, &before, edit)).await {
            Some(Ok(made)) => {
                store.dispatch(AppEvent::InstanceChanged(made));
                store.reload(true);
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Nicht zurückgestellt."), &e).text, None);
            }
            None => {
                toast::show(store.ui, forms::unexplained(), None);
            }
        }
    });
}

#[derive(Clone, Copy)]
struct Keep {
    store: Store,
    id: State<String>,
    mods: State<Vec<ModRef>>,
    saved: State<Vec<ModRef>>,
    adding: State<HashSet<String>>,
    saving: State<bool>,
    updates: State<Vec<Update>>,
    updating: State<HashSet<String>>,
}

impl Keep {
    fn alive(self) -> bool {
        self.mods.try_peek().is_some()
    }

    fn follow(mut self, now: &Instance) {
        if *self.saving.peek() || !self.updating.peek().is_empty() {
            return;
        }
        if now.mods != *self.saved.peek() && *self.mods.peek() == *self.saved.peek() {
            self.mods.set(now.mods.clone());
            self.saved.set(now.mods.clone());
        }
    }

    fn commit(mut self) {
        if *self.saving.peek() || !self.adding.peek().is_empty() || !self.updating.peek().is_empty() {
            return;
        }
        let (local, saved) = (self.mods.peek().clone(), self.saved.peek().clone());
        if local == saved {
            return;
        }
        let id = self.id.peek().clone();
        let store = self.store;
        let Some(made) = store.model.peek().instance(&id).cloned() else { return };
        let before = InstanceEdit { mods: saved.clone(), ..InstanceEdit::of(&made) };
        let edit = InstanceEdit { mods: local.clone(), ..before.clone() };
        let added: Vec<ModRef> = local.iter().filter(|m| !saved.iter().any(|s| s.project_id == m.project_id)).cloned().collect();
        self.saving.set(true);
        spawn_in_window(async move {
            let work = id.clone();
            let result = off_ui(move || remote::save_instance(&work, &before, edit)).await;
            let alive = self.alive();
            if alive {
                self.saving.set(false);
            }
            match result {
                Some(Ok(made)) => {
                    if alive {
                        self.saved.set(local);
                    }
                    store.dispatch(AppEvent::InstanceChanged(made));
                    store.reload(true);
                    if !added.is_empty() {
                        offer_added(store, id, added);
                    }
                }
                other => {
                    let problem = match other {
                        Some(Err(e)) => forms::explain_as(tr("Nicht gespeichert."), &e),
                        _ => Problem::plain(forms::unexplained()),
                    };
                    toast::show(store.ui, problem.text, None);
                    if alive {
                        let saved = self.saved.peek().clone();
                        self.mods.set(saved);
                    }
                }
            }
        });
    }

    fn load_updates(mut self) {
        let Some(made) = self.store.model.peek().instance(&self.id.peek()).cloned() else { return };
        if made.mods.is_empty() {
            return;
        }
        spawn_in_window(async move {
            let result = off_ui(move || remote::mod_updates(&made)).await;
            if self.alive()
                && let Some(Ok(list)) = result
            {
                self.updates.set(list);
            }
        });
    }

    fn update(self, pid: String) {
        let Some(found) = self.updates.peek().iter().find(|u| u.current.project_id == pid).cloned() else { return };
        if *self.saving.peek() || !self.updating.peek().is_empty() || *self.mods.peek() != *self.saved.peek() {
            return;
        }
        let (id, store) = (self.id.peek().clone(), self.store);
        let (back, work) = (id.clone(), id.clone());
        snapshots::before_returning(store, &id, Reason::ModUpdates(1), move || show(store, &back, Kind::Mod), move || self.apply(work, found));
    }

    fn apply(mut self, id: String, found: Update) {
        let store = self.store;
        let pid = found.current.project_id.clone();
        if self.alive() {
            self.updating.write().insert(pid.clone());
        }
        let live = running(store, &id);
        spawn_in_window(async move {
            let (work, sent) = (id.clone(), vec![found.clone()]);
            let result = off_ui(move || remote::apply_updates(&work, &sent, live)).await;
            let alive = self.alive();
            if alive {
                self.updating.write().remove(&pid);
            }
            match result {
                Some(Ok(made)) => {
                    if alive {
                        self.updates.write().retain(|u| u.current.project_id != pid);
                        self.mods.set(made.mods.clone());
                        self.saved.set(made.mods.clone());
                    }
                    store.dispatch(AppEvent::InstanceChanged(made));
                    store.reload(true);
                    toast::show(store.ui, if live { tr("1 Update vorgemerkt. Es gilt beim nächsten Start.") } else { trf("%@ aktualisiert.", &[&trf("„%@“", &[&found.current.title])]) }, None);
                }
                Some(Err(e)) => {
                    toast::show(store.ui, forms::explain_as(tr("Nicht aktualisiert."), &e).text, None);
                }
                None => {
                    toast::show(store.ui, forms::unexplained(), None);
                }
            }
        });
    }
}

/// The Mods page of instance `id`: mods, resource packs, shaders and data packs from Modrinth, each added at once.
#[derive(PartialEq)]
pub struct ContentPage {
    pub id: String,
    pub kind: Kind,
}

impl Component for ContentPage {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let made = store.model.read().instance(&self.id).cloned();
        let first = made.as_ref().map(|m| m.mods.clone()).unwrap_or_default();
        let keep = Keep {
            store,
            id: use_reactive(&self.id),
            mods: use_state({
                let first = first.clone();
                move || first
            }),
            saved: use_state(move || first),
            adding: use_state(HashSet::new),
            saving: use_state(|| false),
            updates: use_state(Vec::new),
            updating: use_state(HashSet::new),
        };
        use_hook(move || keep.load_updates());
        let watched = self.id.clone();
        use_side_effect(move || {
            let now = store.model.read().instance(&watched).cloned();
            if let Some(now) = now {
                keep.follow(&now);
            }
        });
        use_side_effect(move || {
            let _ = (keep.mods.read(), keep.adding.read(), keep.saving.read(), keep.updating.read());
            keep.commit();
        });
        let Some(made) = made else { return rect().into_element() };
        let held = crate::view::instance::held(store, &made.id);
        let saving = *keep.saving.read();
        let pending = *keep.mods.read() != *keep.saved.read();
        let back = made.id.clone();
        let scope = Scope {
            create: false,
            page: true,
            made: Some(made.clone()),
            version: made.game_version.clone(),
            loader: made.loader.clone(),
            mods: keep.mods,
            touched: None,
            adding: keep.adding,
            updatable: keep.updates.read().iter().map(|u| u.current.project_id.clone()).collect(),
            updating: keep.updating.read().clone(),
            saving,
            held,
            pending,
            dirty: false,
            follow: None,
            kind: self.kind,
            on_update: EventHandler::new(move |pid: String| keep.update(pid)),
            back: EventHandler::new(move |_| {
                let mut page = store.ui.page;
                page.set(Page::Instance(back.clone()));
            }),
            done: EventHandler::new(|_| {}),
        };
        Browser { scope }.into_element()
    }
}
