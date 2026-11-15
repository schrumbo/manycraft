//! The mod browser of the instance sheet: popular mods first, search as you type, install, update and remove in place.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::batch_sheet::Load;
use super::instance_sheet::InstanceSheet;
use super::drive::Mark;
use super::forms::{self, Problem};
use super::motion::Soft;
use super::remote;
use super::sheet::{self, Look};
use super::theme::{Theme, control, font, iconsize, radius, space, tint};
use super::widgets::{KeyNav, TakeFocus, icon, segment, segmented};
use crate::core::batch::ModRef;
use crate::core::i18n::{Lang, lang, tr, trf};
use crate::core::instance::Loader;
use crate::core::modicons;
use crate::core::modrinth::{Hit, Page, Sort, Support};

const SEARCH_DELAY: Duration = Duration::from_millis(250);
const RESULTS_HEIGHT: f32 = 372.;
const ROW_HEIGHT: f32 = 56.;
const CONTROLS_HEIGHT: f32 = 32.;
const SKELETON_ROWS: usize = 7;
const MORE_AHEAD: usize = 4;
const ICON_CHUNK: usize = 6;
const KEPT_SEARCHES: usize = 200;
const MAX_LOADED: usize = 300;
const KEPT_ICONS: usize = 48;
const NAME_BAR: f32 = 120.;
const TEXT_BAR: f32 = 220.;
const BAR_HEIGHT: f32 = 10.;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    query: String,
    version: String,
    loader: Loader,
    sort: Sort,
    all: bool,
}

enum Status {
    Idle,
    Loading,
    Failed(Problem),
}

enum More {
    Idle,
    Loading,
    Failed,
}

enum Notice {
    Added(Vec<String>),
    Removed(ModRef, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Install,
    Update,
    Nothing,
}

/// What the mod browser shows and remembers: the query, the answers kept for this session, the highlighted row.
pub(super) struct Browser {
    query: Entity<InputState>,
    sort: Sort,
    all: bool,
    wanted: Option<Key>,
    shown: Option<Key>,
    status: Status,
    more: More,
    cache: HashMap<Key, Page>,
    generation: u64,
    sel: usize,
    pub(super) adding: HashSet<String>,
    notice: Option<Notice>,
    error: Option<Problem>,
    visible: Vec<usize>,
    scroll: UniformListScrollHandle,
    icons: Entity<IconCache>,
}

/// The decoded icons the browser drew last, at most `KEPT_ICONS`; all of them leave memory when the browser closes.
pub(super) struct IconCache {
    items: HashMap<Resource, (ImageCacheItem, u64)>,
    tick: u64,
}

impl IconCache {
    fn new(cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| Self { items: HashMap::new(), tick: 0 });
        cx.observe_release(&cache, |cache, cx| {
            for (_, (item, _)) in cache.items.drain() {
                if let Some(Ok(image)) = item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        cache
    }

    fn clear(&mut self, window: &mut Window, cx: &mut App) {
        for (_, (item, _)) in self.items.drain() {
            if let Some(Ok(image)) = item.get() {
                cx.drop_image(image, Some(&mut *window));
            }
        }
    }
}

impl ImageCache for IconCache {
    fn load(&mut self, resource: &Resource, window: &mut Window, cx: &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.items.entry(resource.clone()).or_insert_with(|| (ImageCacheItem::new(resource, cx), tick));
        entry.1 = tick;
        let image = entry.0.use_image(window);
        while self.items.len() > KEPT_ICONS {
            let Some(oldest) = self.items.iter().min_by_key(|(_, (_, used))| *used).map(|(r, _)| r.clone()) else { break };
            if let Some((item, _)) = self.items.remove(&oldest)
                && let Some(Ok(old)) = item.get()
            {
                cx.drop_image(old, Some(&mut *window));
            }
        }
        image
    }
}

impl Browser {
    pub(super) fn new(window: &mut Window, cx: &mut Context<InstanceSheet>) -> (Self, Subscription) {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Mods suchen, z. B. Sodium")));
        let sub = cx.subscribe(&query, |s: &mut InstanceSheet, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                s.browse_refresh(true, cx);
            }
        });
        let browser = Self {
            query,
            sort: Sort::Relevance,
            all: false,
            wanted: None,
            shown: None,
            status: Status::Idle,
            more: More::Idle,
            cache: HashMap::new(),
            generation: 0,
            sel: 0,
            adding: HashSet::new(),
            notice: None,
            error: None,
            visible: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            icons: IconCache::new(cx),
        };
        (browser, sub)
    }

    fn key(&self, version: &str, loader: &Loader, cx: &App) -> Key {
        let query = self.query.read(cx).value().trim().to_lowercase();
        let sort = if query.is_empty() { Sort::Downloads } else { self.sort };
        Key { query, version: version.to_owned(), loader: loader.with_version(None), sort, all: self.all }
    }

    fn fresh(&self) -> bool {
        self.shown.is_some() && self.shown == self.wanted
    }

    fn page(&self) -> Option<&Page> {
        self.shown.as_ref().and_then(|k| self.cache.get(k))
    }

    fn rows(&self) -> impl Iterator<Item = &Hit> {
        let hits = self.page().map_or(&[][..], |p| p.hits.as_slice());
        self.visible.iter().filter_map(move |i| hits.get(*i))
    }

    fn reindex(&mut self) {
        let (version, loader, all) = self.shown.as_ref().map_or(("", &Loader::Vanilla, true), |k| (k.version.as_str(), &k.loader, k.all));
        let visible = self.page().map_or_else(Vec::new, |p| p.hits.iter().enumerate().filter(|(_, h)| all || h.support(version, loader) == Support::Fits).map(|(i, _)| i).collect());
        self.visible = visible;
    }

    pub(super) fn stale(&self) -> bool {
        matches!(self.status, Status::Failed(_)) || self.page().is_some_and(|p| p.saved.is_some())
    }

    pub(super) fn forget_saved(&mut self) {
        self.cache.retain(|_, p| p.saved.is_none());
        if self.page().is_none() {
            self.shown = None;
            self.wanted = None;
        }
    }

    fn forget(&mut self) {
        self.cache.clear();
        self.visible.clear();
        self.shown = None;
        self.wanted = None;
        self.status = Status::Idle;
        self.more = More::Idle;
        self.generation += 1;
    }

    fn has_more(&self) -> bool {
        self.fresh() && self.page().is_some_and(|p| p.hits.len() < p.total.min(MAX_LOADED))
    }

    fn show(&mut self, key: Key) {
        self.shown = Some(key);
        self.reindex();
        self.status = Status::Idle;
        self.sel = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn keep(&mut self, key: Key, page: Page) {
        if self.cache.len() >= KEPT_SEARCHES {
            let shown = self.shown.clone();
            self.cache.retain(|k, _| Some(k) == shown.as_ref());
        }
        self.cache.entry(key).or_insert(page);
    }

    fn append(&mut self, key: &Key, offset: usize, next: Page) {
        let Some(page) = self.cache.get_mut(key) else { return };
        if page.hits.len() != offset {
            return;
        }
        let mut known: HashSet<String> = page.hits.iter().map(|h| h.project_id.clone()).collect();
        for hit in next.hits {
            if known.insert(hit.project_id.clone()) {
                page.hits.push(hit);
            }
        }
        page.total = if page.hits.len() == offset { offset } else { next.total };
        if self.shown.as_ref() == Some(key) {
            self.reindex();
        }
    }
}

fn search_problem(e: &anyhow::Error) -> Problem {
    if remote::is_network(e) {
        Problem { text: tr("Keine Verbindung zu Modrinth. Prüf dein Internet und versuch es nochmal."), detail: Some(format!("{e:#}")) }
    } else {
        forms::explain_as(tr("Die Suche hat nicht geklappt."), e)
    }
}

impl InstanceSheet {
    pub(super) fn open_search(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open && !self.searching {
            self.saved_scroll = Some(self.body_scroll.offset());
        }
        self.searching = open;
        self.browser.error = None;
        self.browser.notice = None;
        if open {
            self.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
            self.browse_refresh(false, cx);
        } else {
            self.browser.forget();
            self.browser.icons.update(cx, |icons, cx| icons.clear(window, cx));
            window.focus(&self.root, cx);
        }
        cx.notify();
    }

    pub(super) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
    }

    pub(super) fn browse_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.query.read(cx).value().is_empty() {
            self.leave_search(window, cx);
        } else {
            self.browser.query.update(cx, |q, cx| q.set_value("", window, cx));
            self.browse_refresh(false, cx);
        }
    }

    pub(super) fn browse_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.query.update(cx, |q, cx| {
            let end = q.value().len();
            q.take_focus(window, cx);
            q.set_selected_range(0..end, cx);
        });
    }

    pub(super) fn browse_key(&mut self, key: &str, modifiers: Modifiers, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.searching {
            return false;
        }
        match key {
            "up" | "down" if !modifiers.modified() => {
                self.browse_step(if key == "up" { -1 } else { 1 }, cx);
                true
            }
            "f" if modifiers.secondary(cx) && !modifiers.shift && !modifiers.alt => {
                self.browse_focus(window, cx);
                true
            }
            _ => false,
        }
    }

    fn browse_step(&mut self, delta: i32, cx: &mut Context<Self>) {
        let count = self.browser.rows().count();
        if count == 0 {
            return;
        }
        let at = (self.browser.sel as i32 + delta).clamp(0, count as i32 - 1) as usize;
        self.browser.sel = at;
        self.browser.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
        if at + 1 >= count {
            self.browse_more(cx);
        }
        cx.notify();
    }

    pub(super) fn browse_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((hit, act)) = self.browse_selected() else { return };
        match act {
            Act::Install => self.browse_install(hit, cx),
            Act::Update => self.apply_updates(Some(hit.project_id), cx),
            Act::Nothing => return,
        }
        self.browse_focus(window, cx);
    }

    fn browse_selected(&self) -> Option<(Hit, Act)> {
        if !self.browser.fresh() {
            return None;
        }
        let hit = self.browser.rows().nth(self.browser.sel)?.clone();
        let act = self.browse_act(&hit);
        Some((hit, act))
    }

    fn browse_update_for(&self, project_id: &str) -> bool {
        let Some(original) = &self.setup else { return false };
        let Some(Load::Ready(list)) = &self.updates else { return false };
        self.version == original.game_version && !self.applied.contains(project_id) && list.iter().any(|u| u.current.project_id == project_id)
    }

    fn browse_act(&self, hit: &Hit) -> Act {
        if self.browser.adding.contains(&hit.project_id) {
            return Act::Nothing;
        }
        if self.mods.iter().any(|m| m.project_id == hit.project_id) {
            return if self.browse_update_for(&hit.project_id) && self.updating.is_empty() && !self.saving { Act::Update } else { Act::Nothing };
        }
        if hit.support(&self.version, &self.loader) == Support::Fits { Act::Install } else { Act::Nothing }
    }

    pub(super) fn browse_refresh(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let key = self.browser.key(&self.version, &self.loader, cx);
        if debounce && self.browser.wanted.as_ref() == Some(&key) {
            return;
        }
        self.browser.generation += 1;
        let generation = self.browser.generation;
        self.browser.more = More::Idle;
        self.browser.wanted = Some(key.clone());
        if self.browser.cache.contains_key(&key) {
            self.browser.show(key);
            self.warm_icons(cx);
            cx.notify();
            return;
        }
        let wait = debounce && !key.query.is_empty();
        if !wait {
            self.browser.status = Status::Loading;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            if wait {
                cx.background_executor().timer(SEARCH_DELAY).await;
                let current = this.update(cx, |s, cx| {
                    let current = s.browser.generation == generation;
                    if current {
                        s.browser.status = Status::Loading;
                        cx.notify();
                    }
                    current
                });
                if !matches!(current, Ok(true)) {
                    return;
                }
            }
            let asked = key.clone();
            let result = cx.background_executor().spawn(async move { remote::search_page(&asked.query, &asked.version, &asked.loader, asked.sort, asked.all, 0) }).await;
            let _ = this.update(cx, |s, cx| {
                let current = s.browser.generation == generation;
                match result {
                    Ok(page) => {
                        s.browser.keep(key.clone(), page);
                        if current {
                            s.browser.show(key);
                            s.warm_icons(cx);
                        }
                    }
                    Err(e) if current => {
                        s.browser.status = Status::Failed(search_problem(&e));
                        s.browser.shown = None;
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_more(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.browser.shown.clone() else { return };
        if !matches!(self.browser.more, More::Idle) || !self.browser.has_more() {
            return;
        }
        let Some(offset) = self.browser.page().map(|p| p.hits.len()) else { return };
        self.browser.more = More::Loading;
        let generation = self.browser.generation;
        cx.notify();
        let asked = key.clone();
        let task = cx.background_executor().spawn(async move { remote::search_page(&asked.query, &asked.version, &asked.loader, asked.sort, asked.all, offset) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                let current = s.browser.generation == generation;
                match result {
                    Ok(next) => {
                        s.browser.append(&key, offset, next);
                        if current {
                            s.browser.more = More::Idle;
                            s.warm_icons(cx);
                        }
                    }
                    Err(_) if current => s.browser.more = More::Failed,
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn warm_icons(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<(String, String)> = self.browser.rows().filter(|h| modicons::icon(&h.project_id).is_none()).filter_map(|h| h.icon_url.clone().map(|url| (h.project_id.clone(), url))).collect();
        if wanted.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            for chunk in wanted.chunks(ICON_CHUNK) {
                let chunk = chunk.to_vec();
                let changed = cx.background_executor().spawn(async move { modicons::prepare(&chunk) }).await;
                if changed && this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn browse_install(&mut self, hit: Hit, cx: &mut Context<Self>) {
        if self.browse_act(&hit) != Act::Install {
            return;
        }
        let id = hit.project_id.clone();
        self.browser.adding.insert(id.clone());
        self.browser.error = None;
        if matches!(self.browser.notice, Some(Notice::Added(_))) {
            self.browser.notice = None;
        }
        cx.notify();
        let version = self.version.clone();
        let ids = vec![id.clone()];
        let asked = version.clone();
        let loader = self.loader.clone();
        let asked_loader = loader.clone();
        let task = cx.background_executor().spawn(async move { remote::resolve(&ids, &asked, &asked_loader) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.browser.adding.remove(&id);
                if s.version != version || !s.loader.same_kind(&loader) {
                    return;
                }
                match result {
                    Ok(found) => {
                        s.mods_touched = true;
                        let mut extra = Vec::new();
                        for m in found {
                            if !s.mods.iter().any(|have| have.project_id == m.project_id) {
                                if m.project_id != id {
                                    extra.push(m.title.clone());
                                }
                                s.mods.push(m);
                            }
                        }
                        if !extra.is_empty() && !matches!(s.browser.notice, Some(Notice::Removed(..))) {
                            s.browser.notice = Some(Notice::Added(extra));
                        }
                    }
                    Err(e) => s.browser.error = Some(forms::explain_as(trf("„%@“ ließ sich nicht hinzufügen.", &[&hit.title]), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_remove(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let Some(at) = self.mods.iter().position(|m| m.project_id == project_id) else { return };
        let removed = self.mods.remove(at);
        self.mods_touched = true;
        self.browser.error = None;
        self.browser.notice = Some(Notice::Removed(removed, at));
        cx.notify();
    }

    pub(super) fn browse_undo_key(&mut self, cx: &mut Context<Self>) -> bool {
        let pending = self.searching && matches!(self.browser.notice, Some(Notice::Removed(..)));
        if pending {
            self.browse_undo(cx);
        }
        pending
    }

    fn browse_undo(&mut self, cx: &mut Context<Self>) {
        if let Some(Notice::Removed(m, at)) = self.browser.notice.take() {
            if !self.mods.iter().any(|have| have.project_id == m.project_id) {
                self.mods.insert(at.min(self.mods.len()), m);
            }
        }
        cx.notify();
    }

    fn browse_row(&mut self, ix: usize, hit: Hit, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pid = hit.project_id.clone();
        let installed = self.mods.iter().any(|m| m.project_id == pid);
        let support = hit.support(&self.version, &self.loader);
        let dim = !installed && support != Support::Fits;
        let selected = ix == self.browser.sel && self.browser.fresh();
        let face = match modicons::icon(&pid) {
            Some(path) => {
                let (shown, failed) = (*t, *t);
                img(path)
                    .image_cache(&self.browser.icons)
                    .size(px(control::L))
                    .flex_none()
                    .rounded(px(radius::INNER))
                    .object_fit(ObjectFit::Cover)
                    .with_loading(move || placeholder(&shown).into_any_element())
                    .with_fallback(move || placeholder(&failed).into_any_element())
                    .into_any_element()
            }
            None => placeholder(t).into_any_element(),
        };
        let author = (!hit.author.is_empty()).then(|| div().min_w_0().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(trf("von %@", &[&hit.author])));
        let line = div()
            .flex()
            .items_baseline()
            .gap(px(space::S))
            .child(div().min_w_0().truncate().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).child(hit.title.clone()))
            .children(author)
            .child(div().flex().flex_none().items_center().gap(px(space::XXS)).text_size(px(font::CAPTION)).text_color(t.tertiary).child(icon("download", iconsize::XS, t.tertiary)).child(forms::downloads_short(hit.downloads)));
        let text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(space::XXS))
            .when(dim, |d| d.opacity(tint::DISABLED))
            .child(line)
            .child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(hit.description.clone()));
        let trailing = self.browse_trailing(ix, &hit, installed, support, t, cx);
        div()
            .id(SharedString::from(format!("mod-row-{pid}")))
            .mark("result", hit.title.clone())
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(space::M))
            .flex()
            .items_center()
            .gap(px(space::M))
            .rounded(px(radius::INNER))
            .when(selected, |d| d.bg(t.highlight.opacity(tint::SELECTED)))
            .child(face)
            .child(text)
            .child(trailing)
            .into_any_element()
    }

    fn browse_trailing(&mut self, ix: usize, hit: &Hit, installed: bool, support: Support, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pid = hit.project_id.clone();
        let row = div().flex().flex_none().items_center().gap(px(space::S));
        if self.browser.adding.contains(&pid) {
            return row.child(sheet::button(SharedString::from(format!("mod-install-{pid}")), tr("Installieren"), Some("plus"), Look::Secondary, false, true, t)).into_any_element();
        }
        if installed {
            let working = self.updating.contains(&pid);
            let first = if self.browse_update_for(&pid) || working {
                let update = hit.project_id.clone();
                sheet::button(SharedString::from(format!("mod-update-{pid}")), tr("Aktualisieren"), Some("refresh-cw"), Look::Primary, !self.updating.is_empty() || self.saving, working, t)
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.browser.sel = ix;
                        s.apply_updates(Some(update.clone()), cx);
                        s.browse_focus(window, cx);
                        cx.stop_propagation();
                    }))
                    .into_any_element()
            } else {
                sheet::status("check", t.success_text, tr("Installiert")).into_any_element()
            };
            let remove = pid.clone();
            let title = hit.title.clone();
            return row
                .child(first)
                .child(
                    sheet::button(SharedString::from(format!("mod-remove-{pid}")), tr("Entfernen"), None, Look::Secondary, working || self.saving, false, t)
                        .tooltip(super::widgets::tip(trf("„%@“ entfernen", &[&title])))
                        .on_click(cx.listener(move |s, _, window, cx| {
                            s.browser.sel = ix;
                            s.browse_remove(&remove, cx);
                            s.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
                            cx.stop_propagation();
                        })),
                )
                .into_any_element();
        }
        match support {
            Support::Fits => {
                let hit = hit.clone();
                row.child(
                    sheet::button(SharedString::from(format!("mod-install-{pid}")), tr("Installieren"), Some("plus"), Look::Secondary, false, false, t).on_click(cx.listener(move |s, _, window, cx| {
                        s.browser.sel = ix;
                        s.browse_install(hit.clone(), cx);
                        s.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
                        cx.stop_propagation();
                    })),
                )
                .into_any_element()
            }
            Support::OtherVersion => row.child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(trf("Nicht für Minecraft %@", &[&self.version]))).into_any_element(),
            Support::OtherLoader => row.child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(tr("Nicht für Fabric"))).into_any_element(),
        }
    }

    fn browse_footer(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let row = div().id("mod-more").h(px(ROW_HEIGHT)).w_full().px(px(space::M)).flex().items_center();
        if matches!(self.browser.more, More::Failed) {
            return row
                .gap(px(space::S))
                .child(div().flex_1().min_w_0().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Weitere Mods ließen sich nicht laden.")))
                .child(sheet::button("mods-more-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| {
                    s.browser.more = More::Idle;
                    s.browse_more(cx);
                })))
                .into_any_element();
        }
        row.child(skeleton_row(t)).into_any_element()
    }

    pub(super) fn render_search(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let query = self.browser.query.read(cx).value().trim().to_string();
        let field = Input::new(&self.browser.query).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true);
        let hint = match self.browse_selected() {
            Some((hit, Act::Install)) => trf("Enter installiert „%@“.", &[&hit.title]),
            Some((hit, Act::Update)) => trf("Enter aktualisiert „%@“.", &[&hit.title]),
            _ => String::new(),
        };
        let hint = div().h(px(font::CAPTION * font::LINE)).text_size(px(font::CAPTION)).text_color(t.tertiary).child(hint);
        let caption = if query.is_empty() {
            tr("Beliebt")
        } else if let Some(total) = self.browser.page().filter(|_| self.browser.fresh()).map(|p| p.total) {
            trf("%lld Treffer", &[&total])
        } else {
            String::new()
        };
        let sorting = (!query.is_empty()).then(|| {
            let sort = self.browser.sort;
            let pick = cx.listener(|s, n: &usize, window, cx| {
                s.browser.sort = if *n == 0 { Sort::Relevance } else { Sort::Downloads };
                s.browse_refresh(false, cx);
                if !window.last_input_was_keyboard() {
                    s.browse_focus(window, cx);
                }
            });
            segmented("mods-sort", [segment("sort-relevance", tr("Relevanz"), sort == Sort::Relevance, true, t), segment("sort-downloads", tr("Downloads"), sort == Sort::Downloads, true, t)], move |n, window, cx| pick(&n, window, cx), t)
        });
        let all = self.browser.all;
        let toggle = |s: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            s.browser.all = !s.browser.all;
            s.browse_refresh(false, cx);
            s.browse_focus(window, cx);
        };
        let controls = div()
            .h(px(CONTROLS_HEIGHT))
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).font_weight(FontWeight::MEDIUM).text_color(t.secondary).child(caption))
            .children(sorting)
            .child(
                div()
                    .id("mods-all")
                    .h(px(control::S))
                    .px(px(space::XS))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(space::XS))
                    .rounded(px(radius::INNER))
                    .cursor_pointer()
                    .key_nav()
                    .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                    .on_click(cx.listener(move |s, _, window, cx| toggle(s, window, cx)))
                    .child(Checkbox::new("mods-all-check").checked(all).tab_stop(false).on_click(cx.listener(move |s, _: &bool, window, cx| {
                        toggle(s, window, cx);
                        cx.stop_propagation();
                    })))
                    .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Alle zeigen"))),
            );
        let content = self.render_results(&query, cx, t);
        let region = div().h(px(RESULTS_HEIGHT)).flex().flex_col().overflow_hidden().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS)).child(content);
        let bottom = self.render_notice(cx, t);
        div().flex().flex_col().gap(px(space::XS)).child(field).child(hint).child(controls).child(region).child(bottom)
    }

    fn render_results(&mut self, query: &str, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        if self.browser.shown.is_none() || self.browser.page().is_none() {
            return match &self.browser.status {
                Status::Failed(why) => div()
                    .p(px(space::M))
                    .flex()
                    .items_center()
                    .gap(px(space::S))
                    .child(div().flex_1().min_w_0().child(self.problem("search-error", "wifi-off", t.danger_text, why, cx, t)))
                    .child(sheet::button("search-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.browse_refresh(false, cx))))
                    .into_any_element(),
                _ => div().flex().flex_col().children((0..SKELETON_ROWS).map(|_| div().h(px(ROW_HEIGHT)).w_full().px(px(space::M)).flex().items_center().child(skeleton_row(t)))).into_any_element(),
            };
        }
        let count = self.browser.rows().count();
        if count == 0 {
            let version = self.version.clone();
            let all = self.browser.all;
            let text = match (query.is_empty(), all) {
                (true, false) => trf("Keine Mods für Minecraft %@ gefunden.", &[&version]),
                (true, true) => tr("Keine Mods gefunden."),
                (false, false) => trf("Keine Mods zu „%@“ für %@. Versuche einen anderen Suchbegriff.", &[&query, &version]),
                (false, true) => trf("Keine Mods zu „%@“ gefunden. Versuche einen anderen Suchbegriff.", &[&query]),
            };
            let widen = (!all).then(|| sheet::button("search-all", tr("Alle zeigen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, window, cx| {
                s.browser.all = true;
                s.browse_refresh(false, cx);
                s.browse_focus(window, cx);
            })));
            return div()
                .size_full()
                .p(px(space::L))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::M))
                .text_size(px(font::CALLOUT))
                .text_color(t.secondary)
                .child(icon("search-x", iconsize::L, t.tertiary))
                .child(div().max_w(px(control::READING)).text_center().child(text))
                .children(widen)
                .into_any_element();
        }
        let items = count + usize::from(self.browser.has_more());
        let loading = matches!(self.browser.status, Status::Loading);
        let me = cx.entity().downgrade();
        uniform_list(
            "mod-results",
            items,
            cx.processor(move |s: &mut InstanceSheet, range: Range<usize>, _, cx| {
                let t = Theme::get(cx);
                let count = s.browser.rows().count();
                let hits: Vec<(usize, Hit)> = s.browser.rows().enumerate().skip(range.start).take(range.len()).map(|(ix, h)| (ix, h.clone())).collect();
                let mut out: Vec<AnyElement> = hits.into_iter().map(|(ix, hit)| s.browse_row(ix, hit, &t, cx)).collect();
                if range.end > count {
                    out.push(s.browse_footer(&t, cx));
                }
                if range.end + MORE_AHEAD > count && s.browser.has_more() && matches!(s.browser.more, More::Idle) {
                    let me = me.clone();
                    cx.defer(move |cx| drop(me.update(cx, |s, cx| s.browse_more(cx))));
                }
                out
            }),
        )
        .track_scroll(&self.browser.scroll)
        .flex_1()
        .min_h_0()
        .px(px(space::XXS))
        .when(loading, |l| l.opacity(tint::DISABLED))
        .into_any_element()
    }

    fn render_notice(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let line = div().min_h(px(control::M)).flex().items_center().gap(px(space::S));
        if let Some(e) = self.browser.error.clone() {
            return line.child(div().flex_1().min_w_0().child(self.problem("add-error", "circle-alert", t.danger_text, &e, cx, t)));
        }
        match &self.browser.notice {
            Some(Notice::Added(titles)) => line.child(sheet::status("circle-check", t.success_text, trf("Auch installiert: %@.", &[&titles.join(", ")]))),
            Some(Notice::Removed(m, _)) => line
                .child(sheet::status("check", t.secondary, trf("„%@“ entfernt.", &[&m.title])))
                .child(sheet::button("mod-undo", tr("Rückgängig"), Some("undo-2"), Look::Plain, false, false, t).on_click(cx.listener(|s, _, window, cx| {
                    s.browse_undo(cx);
                    s.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
                }))),
            None => match self.browser.page().and_then(|p| p.saved) {
                Some(at) => line.child(sheet::status("wifi-off", t.secondary, saved_text(at))).child(sheet::button("search-saved-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Plain, false, false, t).on_click(cx.listener(|s, _, _, cx| {
                    s.browser.forget_saved();
                    s.browse_refresh(false, cx);
                }))),
                None => line,
            },
        }
    }
}

fn saved_text(at: SystemTime) -> String {
    let at = chrono::DateTime::<chrono::Local>::from(at);
    let when = if at.date_naive() == chrono::Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else if lang() == Lang::En {
        at.format("%b %-d").to_string()
    } else {
        at.format("%-d.%-m.").to_string()
    };
    trf("Keine Verbindung · Ergebnisse von %@", &[&when])
}

fn placeholder(t: &Theme) -> Div {
    div().size(px(control::L)).flex_none().rounded(px(radius::INNER)).bg(t.raised).flex().items_center().justify_center().child(icon("package", iconsize::M, t.tertiary))
}

fn skeleton_row(t: &Theme) -> Div {
    let bar = |width: f32| div().w(px(width)).h(px(BAR_HEIGHT)).rounded(px(radius::SMALL)).bg(t.ink.opacity(tint::HOVER));
    div()
        .flex()
        .items_center()
        .gap(px(space::M))
        .child(div().size(px(control::L)).flex_none().rounded(px(radius::INNER)).bg(t.ink.opacity(tint::HOVER)))
        .child(div().flex().flex_col().gap(px(space::S)).child(bar(NAME_BAR)).child(bar(TEXT_BAR)))
}
