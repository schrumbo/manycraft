//! The loader version picker: always the newest stable version, or one real version of the loader for the game.

use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};

use super::forms;
use super::remote;
use super::sheet;
use super::theme::{Theme, space};
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
    loading: bool,
    failed: Option<String>,
    locked: Option<String>,
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
        Self { select, shown: None, seen: None, pinned: None, versions: Versions::default(), newest, pending: true, loading: false, failed: None, locked: None, _picked: picked }
    }

    /// The picked version; `None` follows the newest stable one.
    pub fn pinned(&self) -> Option<String> {
        self.pinned.clone()
    }

    /// Locks the picker with the reason shown on it, or unlocks it with `None`.
    pub fn lock(&mut self, why: Option<String>, cx: &mut Context<Self>) {
        if self.locked != why {
            self.locked = why;
            cx.notify();
        }
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
            self.failed = None;
            self.loading = false;
            if kind.takes_mods() && !game.is_empty() {
                self.load(kind, game.to_owned(), cx);
            }
        }
        self.pending = true;
        cx.notify();
    }

    fn load(&mut self, kind: Loader, game: String, cx: &mut Context<Self>) {
        let wanted = (kind.clone(), game.clone());
        self.loading = true;
        self.failed = None;
        let task = cx.background_executor().spawn(async move { remote::loader_versions(&kind, &game) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.shown.as_ref() != Some(&wanted) {
                    return;
                }
                s.loading = false;
                match result {
                    Ok(versions) => {
                        s.failed = None;
                        s.versions = versions;
                        s.pending = true;
                    }
                    Err(e) => s.failed = Some(forms::explain_as(tr("Versionen nicht geladen."), &e).text),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if let Some((kind, game)) = self.shown.clone() {
            self.load(kind, game, cx);
            cx.notify();
        }
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
        let t = Theme::get(cx);
        let select = Select::new(&self.select).menu_max_h(px(sheet::LIST_MAX)).search_placeholder(tr("Version suchen")).disabled(self.locked.is_some());
        let control = match &self.locked {
            Some(why) => sheet::unavailable_over("loader-version-locked", select, why.clone()),
            None => div().child(select),
        };
        let below = if let Some(why) = &self.failed {
            Some(
                div()
                    .flex()
                    .items_start()
                    .gap(px(space::S))
                    .child(div().flex_1().min_w_0().child(sheet::note(None, Some(why.clone()), &t)))
                    .child(sheet::button("loader-versions-retry", tr("Erneut versuchen"), Some("refresh-cw"), sheet::Look::Secondary, false, false, &t).on_click(cx.listener(|s, _, _, cx| s.retry(cx))))
                    .into_any_element(),
            )
        } else {
            self.loading.then(|| sheet::waiting(tr("Versionen werden geladen …"), &t).into_any_element())
        };
        div().flex().flex_col().gap(px(space::XS)).child(control).children(below)
    }
}

/// The label over the picker: "Fabric-Version".
pub fn label(loader: &Loader) -> String {
    trf("%@-Version", &[&loader.name()])
}
