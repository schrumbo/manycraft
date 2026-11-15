//! The loader version picker: always the newest stable version, or one real version of the loader for the game.

use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};

use super::remote;
use super::sheet;
use crate::core::i18n::{tr, trf};
use crate::core::install::Versions;
use crate::core::instance::Loader;

/// The user picked another version.
pub struct Picked;

/// A searchable list of the versions of one loader for one game, led by "Immer die neueste".
pub struct LoaderVersion {
    select: Entity<SelectState<SearchableVec<String>>>,
    shown: Option<(Loader, String)>,
    seen: Option<String>,
    pinned: Option<String>,
    versions: Versions,
    newest: String,
    pending: bool,
    _picked: Subscription,
}

impl EventEmitter<Picked> for LoaderVersion {}

impl LoaderVersion {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let newest = tr("Immer die neueste");
        let select = cx.new(|cx| SelectState::new(SearchableVec::new(vec![newest.clone()]), Some(IndexPath::new(0)), window, cx).searchable(true));
        let picked = cx.subscribe(&select, |s: &mut Self, _, e: &SelectEvent<SearchableVec<String>>, cx| {
            let SelectEvent::Confirm(Some(v)) = e else { return };
            let pinned = (*v != s.newest).then(|| v.clone());
            if pinned != s.pinned {
                s.pinned = pinned;
                cx.emit(Picked);
            }
        });
        Self { select, shown: None, seen: None, pinned: None, versions: Versions::default(), newest, pending: true, _picked: picked }
    }

    /// The picked version; `None` follows the newest stable one.
    pub fn pinned(&self) -> Option<String> {
        self.pinned.clone()
    }

    /// Shows the versions of `loader` for `game` and takes over its pinned version when that changed from outside.
    /// Loads the list again only when the loader kind or the game changed.
    pub fn show(&mut self, loader: &Loader, game: &str, cx: &mut Context<Self>) {
        let kind = loader.with_version(None);
        let fresh = self.shown.as_ref().is_none_or(|(k, g)| *k != kind || g != game);
        let outside = loader.version().map(str::to_owned);
        if !fresh && outside == self.seen {
            return;
        }
        self.seen.clone_from(&outside);
        self.pinned = outside;
        if fresh {
            self.shown = Some((kind.clone(), game.to_owned()));
            self.versions = Versions::default();
            if kind.takes_mods() && !game.is_empty() {
                self.load(kind, game.to_owned(), cx);
            }
        }
        self.pending = true;
        cx.notify();
    }

    fn load(&mut self, kind: Loader, game: String, cx: &mut Context<Self>) {
        let wanted = (kind.clone(), game.clone());
        let task = cx.background_executor().spawn(async move { remote::loader_versions(&kind, &game) });
        cx.spawn(async move |this, cx| {
            let Ok(versions) = task.await else { return };
            let _ = this.update(cx, |s, cx| {
                if s.shown.as_ref() == Some(&wanted) {
                    s.versions = versions;
                    s.pending = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn fill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.newest = match &self.versions.default {
            Some(v) => trf("Immer die neueste (%@)", &[v]),
            None => tr("Immer die neueste"),
        };
        let mut items = vec![self.newest.clone()];
        if let Some(pinned) = self.pinned.as_ref().filter(|p| !self.versions.list.contains(p)) {
            items.push(pinned.clone());
        }
        items.extend(self.versions.list.iter().cloned());
        let at = self.pinned.as_ref().and_then(|p| items.iter().position(|i| i == p)).unwrap_or(0);
        self.select.update(cx, |s, cx| {
            s.set_items(SearchableVec::new(items), window, cx);
            s.set_selected_index(Some(IndexPath::new(at)), window, cx);
        });
    }
}

impl Render for LoaderVersion {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending {
            self.pending = false;
            self.fill(window, cx);
        }
        div().child(Select::new(&self.select).menu_max_h(px(sheet::LIST_MAX)).search_placeholder(tr("Version suchen")))
    }
}

/// The label over the picker: "Fabric-Version".
pub fn label(loader: &Loader) -> String {
    trf("%@-Version", &[&loader.name()])
}
