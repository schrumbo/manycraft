//! The content browser of the instance sheet: mods, resource packs, shaders, data packs and modpacks from Modrinth,
//! popular first, search as you type, install, update and remove in place, a page per project.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui_kit::component::{FocusableExt, Sizable};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::batch_sheet::Load;
use super::catalog;
use super::drive::Mark;
use super::forms::{self, Problem};
use super::instance_sheet::{InstanceSheet, Mode};
use super::motion::Soft;
use super::project_page::Project;
use super::remote;
use super::sheet::{self, Look};
use super::theme::{self, Theme, control, font, iconsize, radius, space, tint};
use super::widgets::{KeyNav, TakeFocus, icon, segment, segmented};
use crate::core::batch::ModRef;
use crate::core::packs::{self, Installed};
use crate::core::i18n::{Lang, lang, tr, trf};
use crate::core::instance::Loader;
use crate::core::modicons;
use crate::core::modrinth::{Filter, Hit, Kind, Page, Search, Sort, Support, Update};
use crate::core::worlds::{self, World};

const SEARCH_DELAY: Duration = Duration::from_millis(250);
pub(super) const RESULTS_HEIGHT: f32 = 372.;
const ROW_HEIGHT: f32 = 56.;
const CONTROLS_HEIGHT: f32 = 32.;
const SKELETON_ROWS: usize = 7;
const MORE_AHEAD: usize = 4;
const ICON_CHUNK: usize = 6;
const KEPT_SEARCHES: usize = 200;
const MAX_LOADED: usize = 1000;
const KEPT_ICONS: usize = 48;
const NAME_BAR: f32 = 120.;
const TEXT_BAR: f32 = 220.;
const BAR_HEIGHT: f32 = 10.;
const MENU_MAX: f32 = 320.;
const CHROME: f32 = 184.;
const RESULTS_MIN: f32 = 216.;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    kind: Kind,
    query: String,
    version: String,
    loader: Loader,
    sort: Sort,
    category: Option<String>,
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
    Trashed { title: String, dir: PathBuf, entry: String },
    NeedsShaderMod { id: &'static str, name: &'static str },
    NoShaderMod,
    Importing(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Act {
    Install,
    Update,
    Nothing,
}

/// The pack files in one folder, as far as Modrinth knows them, and newer files for them.
struct Shelf {
    files: Load<Vec<Installed>>,
    updates: Vec<Update>,
}

/// What the browser shows and remembers: the tab, the query, the answers kept for this session, the highlighted row,
/// the packs in each folder, the open project page.
pub(super) struct Browser {
    pub(super) query: Entity<InputState>,
    pub(super) kind: Kind,
    sort: Sort,
    category: Option<String>,
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
    pub(super) error: Option<Problem>,
    visible: Vec<usize>,
    scroll: UniformListScrollHandle,
    pub(super) icons: Entity<IconCache>,
    worlds: Option<Vec<World>>,
    target: Option<PathBuf>,
    shelves: HashMap<PathBuf, Shelf>,
    pub(super) project: Option<Project>,
    pub(super) body: f32,
}

/// The decoded pictures the browser drew last, at most `KEPT_ICONS`; all of them leave memory when the browser closes.
pub(super) struct IconCache {
    items: HashMap<Resource, (ImageCacheItem, u64)>,
    tick: u64,
}

impl IconCache {
    pub(super) fn new(cx: &mut App) -> Entity<Self> {
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

/// The tab name of `kind`.
pub(super) fn kind_name(kind: Kind) -> String {
    match kind {
        Kind::Mod => tr("Mods"),
        Kind::ResourcePack => tr("Ressourcenpakete"),
        Kind::Shader => tr("Shader"),
        Kind::DataPack => tr("Datenpakete"),
        Kind::Modpack => tr("Modpacks"),
    }
}

fn placeholder_text(kind: Kind) -> String {
    match kind {
        Kind::Mod => tr("Mods suchen, z. B. Sodium"),
        Kind::ResourcePack => tr("Ressourcenpakete suchen, z. B. Fresh Animations"),
        Kind::Shader => tr("Shader suchen, z. B. Complementary"),
        Kind::DataPack => tr("Datenpakete suchen, z. B. Terralith"),
        Kind::Modpack => tr("Modpacks suchen, z. B. Fabulously Optimized"),
    }
}

fn category_name(slug: &str) -> String {
    match slug {
        "adventure" => tr("Abenteuer"),
        "decoration" => tr("Dekoration"),
        "economy" => tr("Wirtschaft"),
        "equipment" => tr("Ausrüstung"),
        "food" => tr("Essen"),
        "game-mechanics" => tr("Spielmechanik"),
        "library" => tr("Bibliothek"),
        "magic" => tr("Magie"),
        "management" => tr("Verwaltung"),
        "minigame" => tr("Minispiel"),
        "mobs" => tr("Kreaturen"),
        "optimization" => tr("Leistung"),
        "social" => tr("Gemeinschaft"),
        "storage" => tr("Lager"),
        "technology" => tr("Technik"),
        "transportation" => tr("Transport"),
        "utility" => tr("Werkzeuge"),
        "worldgen" => tr("Weltgenerierung"),
        "combat" => tr("Kampf"),
        "modded" => tr("Für Mods"),
        "realistic" => tr("Realistisch"),
        "simplistic" => tr("Schlicht"),
        "themed" => tr("Thematisch"),
        "tweaks" => tr("Feinschliff"),
        "vanilla-like" => tr("Wie Vanilla"),
        "cartoon" => tr("Comic"),
        "fantasy" => tr("Fantasy"),
        "semi-realistic" => tr("Halb realistisch"),
        "challenging" => tr("Herausfordernd"),
        "kitchen-sink" => tr("Von allem etwas"),
        "lightweight" => tr("Leichtgewichtig"),
        "multiplayer" => tr("Mehrspieler"),
        "quests" => tr("Aufgaben"),
        other => other.to_owned(),
    }
}

fn sort_name(sort: Sort) -> String {
    match sort {
        Sort::Relevance => tr("Relevanz"),
        Sort::Downloads => tr("Downloads"),
        Sort::Updated => tr("Aktualisiert"),
        Sort::Newest => tr("Neu"),
    }
}

impl Browser {
    pub(super) fn new(window: &mut Window, cx: &mut Context<InstanceSheet>) -> (Self, Subscription) {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder_text(Kind::Mod)));
        let sub = cx.subscribe(&query, |s: &mut InstanceSheet, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                s.browse_refresh(true, cx);
            }
        });
        let browser = Self {
            query,
            kind: Kind::Mod,
            sort: Sort::Relevance,
            category: None,
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
            worlds: None,
            target: None,
            shelves: HashMap::new(),
            project: None,
            body: RESULTS_HEIGHT + CHROME,
        };
        (browser, sub)
    }

    fn sort_for(&self, query: &str) -> Sort {
        if query.is_empty() && self.sort == Sort::Relevance { Sort::Downloads } else { self.sort }
    }

    fn key(&self, version: &str, loader: &Loader, cx: &App) -> Key {
        let query = self.query.read(cx).value().trim().to_lowercase();
        let sort = self.sort_for(&query);
        Key { kind: self.kind, query, version: version.to_owned(), loader: loader.with_version(None), sort, category: self.category.clone(), all: self.all && widens(self.kind) }
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
        let filter = self.shown.as_ref().map(|k| (filter_for(k.kind, &k.version, &k.loader), k.all));
        let visible = self.page().map_or_else(Vec::new, |p| p.hits.iter().enumerate().filter(|(_, h)| filter.as_ref().is_none_or(|(f, all)| *all || h.fits(f) == Support::Fits)).map(|(i, _)| i).collect());
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
        self.shelves.clear();
        self.worlds = None;
        self.project = None;
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

    fn installed_in(&self, dir: &PathBuf, project_id: &str) -> Option<&Installed> {
        match self.shelves.get(dir).map(|s| &s.files) {
            Some(Load::Ready(files)) => files.iter().find(|i| i.found.project_id == project_id),
            _ => None,
        }
    }

    fn pack_update(&self, dir: &PathBuf, project_id: &str) -> Option<&Update> {
        self.shelves.get(dir)?.updates.iter().find(|u| u.current.project_id == project_id)
    }
}

fn widens(kind: Kind) -> bool {
    matches!(kind, Kind::Mod | Kind::Modpack)
}

/// The filter the browser searches `kind` with for an instance of `version` and `loader`.
pub(super) fn filter_for(kind: Kind, version: &str, loader: &Loader) -> Filter {
    Filter::of(kind, version, loader, None)
}

fn search_problem(e: &anyhow::Error) -> Problem {
    if remote::is_network(e) {
        Problem { text: tr("Keine Verbindung zu Modrinth. Prüf dein Internet und versuch es nochmal."), detail: Some(format!("{e:#}")) }
    } else {
        forms::explain_as(tr("Die Suche hat nicht geklappt."), e)
    }
}

impl InstanceSheet {
    /// The tabs this sheet offers: packs need an instance to go into.
    pub(super) fn browse_kinds(&self) -> &'static [Kind] {
        match self.mode {
            Mode::Create => &[Kind::Mod, Kind::Modpack],
            Mode::Mods(_) => &Kind::ALL,
        }
    }

    pub(super) fn open_search(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open && !self.searching {
            self.saved_scroll = Some(self.body_scroll.offset());
        }
        self.searching = open;
        self.browser.error = None;
        self.browser.notice = None;
        self.browser.project = None;
        if open {
            self.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
            self.browse_refresh(false, cx);
            self.browse_shelf(cx);
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

    pub(super) fn browse_tab(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        if !self.browse_kinds().contains(&kind) {
            return;
        }
        let changed = self.browser.kind != kind;
        self.browser.project = None;
        if changed {
            self.browser.kind = kind;
            self.browser.category = None;
            self.browser.error = None;
            if !matches!(self.browser.notice, Some(Notice::Removed(..) | Notice::Trashed { .. })) {
                self.browser.notice = None;
            }
            self.browse_refresh(false, cx);
            self.browse_shelf(cx);
        }
        self.browser.query.update(cx, |q, cx| q.set_placeholder(placeholder_text(kind), window, cx));
        self.browse_focus(window, cx);
        cx.notify();
    }

    pub(super) fn browse_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.project.is_some() {
            self.close_project(window, cx);
        } else if self.browser.query.read(cx).value().is_empty() {
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
        let command = modifiers.secondary(cx) && !modifiers.shift && !modifiers.alt;
        match key {
            "up" | "down" if !modifiers.modified() && self.browser.project.is_none() => {
                self.browse_step(if key == "up" { -1 } else { 1 }, cx);
                true
            }
            "up" | "down" | "pageup" | "pagedown" if !modifiers.modified() && self.browser.project.is_some() => {
                let sign = if key.ends_with("up") { -1. } else { 1. };
                if key.starts_with("page") { self.project_scroll(0., sign, cx) } else { self.project_scroll(sign, 0., cx) }
                true
            }
            "f" if command => {
                self.browse_focus(window, cx);
                true
            }
            "enter" if command && self.browser.project.is_none() => {
                self.browse_act_selected(window, cx);
                true
            }
            "1" | "2" | "3" | "4" | "5" if command => {
                let at = key.parse::<usize>().unwrap_or(1) - 1;
                if let Some(kind) = self.browse_kinds().get(at).copied() {
                    self.browse_tab(kind, window, cx);
                }
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
        if self.browser.project.is_some() {
            self.project_primary(window, cx);
            return;
        }
        let Some((hit, _)) = self.browse_selected() else { return };
        self.open_project(hit, window, cx);
    }

    fn browse_act_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((hit, act)) = self.browse_selected() else { return };
        self.browse_run(hit, act, cx);
        self.browse_focus(window, cx);
    }

    pub(super) fn browse_run(&mut self, hit: Hit, act: Act, cx: &mut Context<Self>) {
        match (self.browser.kind, act) {
            (_, Act::Nothing) => {}
            (Kind::Mod, Act::Install) => self.browse_install(hit, cx),
            (Kind::Mod, Act::Update) => self.apply_updates(Some(hit.project_id), cx),
            (Kind::Modpack, _) => self.browse_modpack(hit, cx),
            (_, Act::Install) => self.browse_pack(hit, false, cx),
            (_, Act::Update) => self.browse_pack(hit, true, cx),
        }
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

    pub(super) fn browse_filter(&self) -> Filter {
        filter_for(self.browser.kind, &self.version, &self.loader)
    }

    /// The folder packs of the open tab go into, when the tab has one.
    pub(super) fn pack_dir(&self) -> Option<PathBuf> {
        let made = self.setup.as_ref().filter(|_| !self.creating())?;
        packs::pack_dir(&made.dir(), self.browser.kind, self.browser.target.as_deref())
    }

    fn global_packs(&self) -> bool {
        let local = self.setup.as_ref().map_or(&[][..], |m| m.local_mods.as_slice());
        packs::loads_global_packs(&self.mods, local)
    }

    fn needs_global_packs(&self) -> bool {
        self.browser.kind == Kind::DataPack && self.browser.target.is_none() && !self.global_packs()
    }

    pub(super) fn browse_act(&self, hit: &Hit) -> Act {
        if self.browser.adding.contains(&hit.project_id) {
            return Act::Nothing;
        }
        let fits = hit.fits(&self.browse_filter()) == Support::Fits;
        match self.browser.kind {
            Kind::Mod => {
                if self.mods.iter().any(|m| m.project_id == hit.project_id) {
                    return if self.browse_update_for(&hit.project_id) && self.updating.is_empty() && !self.saving { Act::Update } else { Act::Nothing };
                }
                if fits && self.loader.takes_mods() { Act::Install } else { Act::Nothing }
            }
            Kind::Modpack => {
                if fits { Act::Install } else { Act::Nothing }
            }
            _ => {
                let Some(dir) = self.pack_dir() else { return Act::Nothing };
                if self.browser.installed_in(&dir, &hit.project_id).is_some() {
                    return if self.browser.pack_update(&dir, &hit.project_id).is_some() { Act::Update } else { Act::Nothing };
                }
                if fits && !self.needs_global_packs() && matches!(self.browser.shelves.get(&dir).map(|s| &s.files), Some(Load::Ready(_) | Load::Failed(_))) { Act::Install } else { Act::Nothing }
            }
        }
    }

    pub(super) fn browse_refresh(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let key = self.browser.key(&self.version, &self.loader, cx);
        if debounce && self.browser.wanted.as_ref() == Some(&key) {
            return;
        }
        if debounce {
            self.browser.project = None;
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
            let search = search_of(&key);
            let result = cx.background_executor().spawn(async move { catalog::search(&search, 0) }).await;
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
        let search = search_of(&key);
        let task = cx.background_executor().spawn(async move { catalog::search(&search, offset) });
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

    fn browse_worlds(&mut self, cx: &mut Context<Self>) {
        if self.browser.worlds.is_some() {
            return;
        }
        let Some(dir) = self.setup.as_ref().filter(|_| !self.creating()).map(setup_dir) else { return };
        self.browser.worlds = Some(Vec::new());
        let task = cx.background_executor().spawn(async move {
            let mut list = worlds::list(&dir);
            list.sort_by_key(|w| std::cmp::Reverse(w.level.last_played.unwrap_or(0)));
            list
        });
        cx.spawn(async move |this, cx| {
            let list = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.browser.target.is_none() && !s.global_packs() {
                    s.browser.target = list.first().map(|w| w.dir.clone());
                }
                s.browser.worlds = Some(list);
                s.browse_shelf(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn browse_shelf(&mut self, cx: &mut Context<Self>) {
        if self.browser.kind == Kind::DataPack && self.browser.worlds.is_none() {
            self.browse_worlds(cx);
            return;
        }
        if matches!(self.browser.kind, Kind::Mod | Kind::Modpack) {
            return;
        }
        let Some(dir) = self.pack_dir() else { return };
        if matches!(self.browser.shelves.get(&dir).map(|s| &s.files), Some(Load::Ready(_) | Load::Loading)) {
            return;
        }
        self.browser.shelves.insert(dir.clone(), Shelf { files: Load::Loading, updates: Vec::new() });
        let filter = self.browse_filter();
        let asked = dir.clone();
        let task = cx.background_executor().spawn(async move {
            let files = catalog::installed(&asked)?;
            let current: Vec<ModRef> = files.iter().map(|i| i.found.clone()).collect();
            let updates = if current.is_empty() { Vec::new() } else { catalog::updates(&filter, &current).unwrap_or_default() };
            anyhow::Ok((files, updates))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                let shelf = match result {
                    Ok((files, updates)) => Shelf { files: Load::Ready(files), updates },
                    Err(e) if remote::is_network(&e) => Shelf { files: Load::Failed(search_problem(&e)), updates: Vec::new() },
                    Err(e) => Shelf { files: Load::Failed(forms::explain_as(tr("Was installiert ist, ließ sich nicht lesen."), &e)), updates: Vec::new() },
                };
                s.browser.shelves.insert(dir, shelf);
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_target(&mut self, target: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.target = target;
        self.browse_shelf(cx);
        self.browse_focus(window, cx);
        cx.notify();
    }

    fn browse_install(&mut self, hit: Hit, cx: &mut Context<Self>) {
        if self.browser.kind != Kind::Mod || self.browse_act(&hit) != Act::Install {
            return;
        }
        self.add_mod(hit.project_id.clone(), hit.title.clone(), cx);
    }

    /// Adds the mod `id` with what it needs to the pending mods.
    pub(super) fn add_mod(&mut self, id: String, title: String, cx: &mut Context<Self>) {
        self.browser.adding.insert(id.clone());
        self.browser.error = None;
        if matches!(self.browser.notice, Some(Notice::Added(_) | Notice::NeedsShaderMod { .. })) {
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
                        if !extra.is_empty() && !matches!(s.browser.notice, Some(Notice::Removed(..) | Notice::Trashed { .. })) {
                            s.browser.notice = Some(Notice::Added(extra));
                        }
                    }
                    Err(e) => s.browser.error = Some(forms::explain_as(trf("„%@“ ließ sich nicht hinzufügen.", &[&title]), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_pack(&mut self, hit: Hit, update: bool, cx: &mut Context<Self>) {
        let wanted = if update { Act::Update } else { Act::Install };
        if self.browse_act(&hit) != wanted {
            return;
        }
        let (Some(dir), Some(made)) = (self.pack_dir(), self.setup.as_ref()) else { return };
        let instance_dir = made.dir();
        let kind = self.browser.kind;
        let mut pending = if update { self.browser.pack_update(&dir, &hit.project_id).cloned() } else { None };
        if let Some(u) = pending.as_mut() {
            let Some(have) = self.browser.installed_in(&dir, &hit.project_id) else { return };
            u.current.file_name = have.file.clone();
        }
        let filter = self.browse_filter();
        let id = hit.project_id.clone();
        self.browser.adding.insert(id.clone());
        self.browser.error = None;
        cx.notify();
        let (asked, title) = (dir.clone(), hit.title.clone());
        let task = cx.background_executor().spawn(async move { catalog::install(&instance_dir, &asked, &id, &title, &filter, pending.as_ref()) });
        let id = hit.project_id.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.browser.adding.remove(&id);
                match result {
                    Ok(placed) => {
                        if let Some(shelf) = s.browser.shelves.get_mut(&dir) {
                            if let Load::Ready(files) = &mut shelf.files {
                                files.retain(|i| i.found.project_id != id);
                                files.push(placed);
                            }
                            shelf.updates.retain(|u| u.current.project_id != id);
                        }
                        if kind == Kind::Shader && !update {
                            s.offer_shader_mod();
                        }
                    }
                    Err(e) => {
                        let text = if update { trf("„%@“ ließ sich nicht aktualisieren.", &[&hit.title]) } else { trf("„%@“ ließ sich nicht installieren.", &[&hit.title]) };
                        s.browser.error = Some(forms::explain_as(text, &e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn offer_shader_mod(&mut self) {
        let local = self.setup.as_ref().map_or(&[][..], |m| m.local_mods.as_slice());
        if packs::shader_loader(&self.mods, local).is_some() || matches!(self.browser.notice, Some(Notice::Removed(..) | Notice::Trashed { .. })) {
            return;
        }
        self.browser.notice = Some(match packs::shader_mod(&self.loader) {
            Some((id, name)) => Notice::NeedsShaderMod { id, name },
            None => Notice::NoShaderMod,
        });
    }

    fn browse_remove_pack(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let (Some(dir), Some(made)) = (self.pack_dir(), self.setup.as_ref()) else { return };
        let Some(found) = self.browser.installed_in(&dir, project_id).cloned() else { return };
        let instance_dir = made.dir();
        let id = project_id.to_owned();
        self.browser.adding.insert(id.clone());
        self.browser.error = None;
        cx.notify();
        let asked = dir.clone();
        let file = found.file.clone();
        let task = cx.background_executor().spawn(async move { catalog::remove(&instance_dir, &asked, &file) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.browser.adding.remove(&id);
                match result {
                    Ok(entry) => {
                        if let Some(shelf) = s.browser.shelves.get_mut(&dir) {
                            if let Load::Ready(files) = &mut shelf.files {
                                files.retain(|i| i.found.project_id != id);
                            }
                            shelf.updates.retain(|u| u.current.project_id != id);
                        }
                        s.browser.notice = Some(Notice::Trashed { title: found.found.title.clone(), dir: dir.clone(), entry });
                    }
                    Err(e) => s.browser.error = Some(forms::explain_as(trf("„%@“ ließ sich nicht entfernen.", &[&found.found.title]), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_modpack(&mut self, hit: Hit, cx: &mut Context<Self>) {
        if self.browser.kind != Kind::Modpack || self.browse_act(&hit) != Act::Install {
            return;
        }
        let filter = if self.browser.all { Filter::any(Kind::Modpack) } else { self.browse_filter() };
        let id = hit.project_id.clone();
        self.browser.adding.insert(id.clone());
        self.browser.error = None;
        cx.notify();
        let title = hit.title.clone();
        let task = cx.background_executor().spawn(async move { catalog::fetch_modpack(&id, &title, &filter) });
        let id = hit.project_id.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.browser.adding.remove(&id);
                match result {
                    Ok(path) => {
                        cx.defer(move |cx| super::transfer::start(path, cx));
                        if s.pending_changes() {
                            s.browser.notice = Some(Notice::Importing(hit.title.clone()));
                        } else {
                            cx.emit(super::sheet::Dismissed);
                        }
                    }
                    Err(e) => s.browser.error = Some(forms::explain_as(trf("„%@“ ließ sich nicht laden.", &[&hit.title]), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_remove(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.browser.kind != Kind::Mod {
            self.browse_remove_pack(project_id, cx);
            return;
        }
        let Some(at) = self.mods.iter().position(|m| m.project_id == project_id) else { return };
        let removed = self.mods.remove(at);
        self.mods_touched = true;
        self.browser.error = None;
        self.browser.notice = Some(Notice::Removed(removed, at));
        cx.notify();
    }

    pub(super) fn browse_undo_key(&mut self, cx: &mut Context<Self>) -> bool {
        let pending = self.searching && matches!(self.browser.notice, Some(Notice::Removed(..) | Notice::Trashed { .. }));
        if pending {
            self.browse_undo(cx);
        }
        pending
    }

    fn browse_undo(&mut self, cx: &mut Context<Self>) {
        match self.browser.notice.take() {
            Some(Notice::Removed(m, at)) => {
                if !self.mods.iter().any(|have| have.project_id == m.project_id) {
                    self.mods.insert(at.min(self.mods.len()), m);
                }
            }
            Some(Notice::Trashed { title, dir, entry }) => {
                let Some(instance_dir) = self.setup.as_ref().map(setup_dir) else { return };
                let task = cx.background_executor().spawn(async move { catalog::put_back(&instance_dir, &entry) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |s, cx| {
                        s.browser.shelves.remove(&dir);
                        s.browse_shelf(cx);
                        if let Err(e) = result {
                            s.browser.error = Some(forms::explain_as(trf("„%@“ ließ sich nicht zurückholen.", &[&title]), &e));
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn face(&self, project_id: &str, t: &Theme) -> AnyElement {
        match modicons::icon(project_id) {
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
        }
    }

    fn browse_row(&mut self, ix: usize, hit: Hit, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pid = hit.project_id.clone();
        let installed = self.is_installed(&pid);
        let dim = !installed && hit.fits(&self.browse_filter()) != Support::Fits;
        let selected = ix == self.browser.sel && self.browser.fresh();
        let face = self.face(&pid, t);
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
        let trailing = self.browse_trailing(Some(ix), &hit, t, cx);
        let open = hit.clone();
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
            .cursor_pointer()
            .when(selected, |d| d.bg(t.highlight.opacity(tint::SELECTED)))
            .when(!selected, |d| d.soft(t.ink.opacity(tint::HOVER), radius::INNER))
            .on_click(cx.listener(move |s, _, window, cx| {
                s.browser.sel = ix;
                s.open_project(open.clone(), window, cx);
            }))
            .child(face)
            .child(text)
            .child(trailing)
            .into_any_element()
    }

    fn is_installed(&self, project_id: &str) -> bool {
        match self.browser.kind {
            Kind::Mod => self.mods.iter().any(|m| m.project_id == project_id),
            Kind::Modpack => false,
            _ => self.pack_dir().is_some_and(|dir| self.browser.installed_in(&dir, project_id).is_some()),
        }
    }

    /// The Modrinth version id of the installed file of a project, when it is installed.
    pub(super) fn installed_version(&self, project_id: &str) -> Option<String> {
        match self.browser.kind {
            Kind::Mod => self.mods.iter().find(|m| m.project_id == project_id).map(|m| m.version_id.clone()),
            Kind::Modpack => None,
            _ => self.pack_dir().and_then(|dir| self.browser.installed_in(&dir, project_id).map(|i| i.found.version_id.clone())),
        }
    }

    fn has_update(&self, project_id: &str) -> bool {
        match self.browser.kind {
            Kind::Mod => self.browse_update_for(project_id),
            Kind::Modpack => false,
            _ => self.pack_dir().is_some_and(|dir| self.browser.pack_update(&dir, project_id).is_some()),
        }
    }

    /// The buttons or state at the end of a result row, or on a project page when `ix` is `None`.
    pub(super) fn browse_trailing(&mut self, ix: Option<usize>, hit: &Hit, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pid = hit.project_id.clone();
        let kind = self.browser.kind;
        let row = div().flex().flex_none().items_center().gap(px(space::S));
        let install_label = if kind == Kind::Modpack { tr("Als Instanz installieren") } else { tr("Installieren") };
        let busy = self.browser.adding.contains(&pid) || (kind == Kind::Mod && self.updating.contains(&pid));
        let pick = move |s: &mut Self| {
            if let Some(ix) = ix {
                s.browser.sel = ix;
            }
        };
        if self.browser.adding.contains(&pid) && !self.is_installed(&pid) {
            return row.child(sheet::button(SharedString::from(format!("mod-install-{pid}")), install_label, Some("plus"), Look::Secondary, false, true, t)).into_any_element();
        }
        if self.is_installed(&pid) {
            let first = if self.has_update(&pid) || busy {
                let update = hit.clone();
                let blocked = (kind == Kind::Mod && !busy && (!self.updating.is_empty() || self.saving)).then(|| SharedString::from(tr("Eine andere Änderung läuft noch.")));
                sheet::button_unless(SharedString::from(format!("mod-update-{pid}")), tr("Aktualisieren"), Some("refresh-cw"), Look::Primary, blocked, busy, t)
                    .on_click(cx.listener(move |s, _, window, cx| {
                        pick(s);
                        let act = s.browse_act(&update);
                        if act == Act::Update {
                            s.browse_run(update.clone(), act, cx);
                        }
                        s.refocus_browser(window, cx);
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
                    sheet::button_unless(
                        SharedString::from(format!("mod-remove-{pid}")),
                        tr("Entfernen"),
                        None,
                        Look::Secondary,
                        (busy || self.saving).then(|| SharedString::from(if busy { trf("„%@“ wird gerade geändert.", &[&title]) } else { tr("Eine andere Änderung läuft noch.") })),
                        false,
                        t,
                    )
                    .when(!busy && !self.saving, |b| b.tooltip(super::widgets::tip(trf("„%@“ entfernen", &[&title]))))
                        .on_click(cx.listener(move |s, _, window, cx| {
                            pick(s);
                            s.browse_remove(&remove, cx);
                            s.refocus_browser(window, cx);
                            cx.stop_propagation();
                        })),
                )
                .into_any_element();
        }
        let support = hit.fits(&self.browse_filter());
        if support != Support::Fits {
            let text = match (support, kind) {
                (Support::OtherVersion, _) => trf("Nicht für Minecraft %@", &[&self.version]),
                (_, Kind::Shader) => tr("Nur für OptiFine"),
                _ => trf("Nicht für %@", &[&self.loader.name()]),
            };
            return row.child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(text)).into_any_element();
        }
        if kind == Kind::Mod && !self.loader.takes_mods() {
            return row.child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(trf("%@ lädt keine Mods", &[&self.loader.name()]))).into_any_element();
        }
        let reason = self.install_blocked();
        let install = hit.clone();
        row.child(sheet::button_unless(SharedString::from(format!("mod-install-{pid}")), install_label, Some("plus"), Look::Secondary, reason.map(Into::into), false, t).on_click(cx.listener(move |s, _, window, cx| {
            pick(s);
            let act = s.browse_act(&install);
            s.browse_run(install.clone(), act, cx);
            s.refocus_browser(window, cx);
            cx.stop_propagation();
        })))
        .into_any_element()
    }

    fn install_blocked(&self) -> Option<String> {
        match self.browser.kind {
            Kind::Mod | Kind::Modpack => None,
            _ if self.needs_global_packs() => Some(tr("Für neue Welten braucht es Global Packs.")),
            _ => match self.pack_dir().and_then(|d| self.browser.shelves.get(&d)).map(|s| &s.files) {
                Some(Load::Loading) | None => Some(tr("Einen Moment, die Instanz wird gelesen …")),
                Some(Load::Failed(_)) => Some(tr("Erst muss klar sein, was schon installiert ist.")),
                Some(Load::Ready(_)) => None,
            },
        }
    }

    pub(super) fn refocus_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.query.update(cx, |q, cx| q.take_focus(window, cx));
    }

    fn browse_footer(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let row = div().id("mod-more").h(px(ROW_HEIGHT)).w_full().px(px(space::M)).flex().items_center();
        if matches!(self.browser.more, More::Failed) {
            return row
                .gap(px(space::S))
                .child(div().flex_1().min_w_0().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Weitere Ergebnisse ließen sich nicht laden.")))
                .child(sheet::button("mods-more-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| {
                    s.browser.more = More::Idle;
                    s.browse_more(cx);
                })))
                .into_any_element();
        }
        row.child(skeleton_row(t)).into_any_element()
    }

    fn render_tabs(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Stateful<Div>> {
        let kinds = self.browse_kinds();
        if kinds.len() < 2 {
            return None;
        }
        let current = self.browser.kind;
        let segments: Vec<Stateful<Div>> = kinds
            .iter()
            .enumerate()
            .map(|(n, k)| segment(SharedString::from(format!("tab-{}", k.project_type())), kind_name(*k), *k == current, true, t).tooltip(super::widgets::tip(format!("{} · {}", kind_name(*k), keys(&(n + 1).to_string(), true)))))
            .collect();
        let pick = cx.listener(move |s, n: &usize, window, cx| {
            if let Some(kind) = s.browse_kinds().get(*n).copied() {
                s.browse_tab(kind, window, cx);
            }
        });
        Some(segmented("content-tabs", segments, move |n, window, cx| pick(&n, window, cx), t).w_full())
    }

    fn popup(&self, id: &'static str, label: String, items: Vec<(String, bool, Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>)>, cx: &mut Context<Self>) -> Stateful<Div> {
        let me = cx.entity().downgrade();
        let items: Vec<(String, bool, Arc<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>)> = items.into_iter().map(|(l, c, f)| (l, c, Arc::from(f))).collect();
        let button = Button::new(SharedString::from(format!("{id}-button")))
            .secondary()
            .small()
            .h(px(control::M))
            .px(px(space::S))
            .focus_ring(theme::rings(cx))
            .dropdown_caret(true)
            .child(div().text_size(px(font::CALLOUT)).child(label.clone()))
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(MENU_MAX));
                for (text, checked, run) in &items {
                    let (me, run) = (me.clone(), run.clone());
                    menu = menu.item(PopupMenuItem::new(text.clone()).checked(*checked).on_click(move |_, window, cx| {
                        let _ = me.update(cx, |s, cx| run(s, window, cx));
                    }));
                }
                menu
            });
        div().id(id).mark("button", label).flex_none().child(button)
    }

    fn render_controls(&mut self, query: &str, cx: &mut Context<Self>, t: &Theme) -> Div {
        let kind = self.browser.kind;
        let caption = if query.is_empty() && self.browser.category.is_none() {
            tr("Beliebt")
        } else if let Some(total) = self.browser.page().filter(|_| self.browser.fresh()).map(|p| p.total) {
            trf("%lld Treffer", &[&total])
        } else {
            String::new()
        };
        let category_label = self.browser.category.as_deref().map_or_else(|| tr("Alle Kategorien"), category_name);
        let mut categories: Vec<(String, bool, Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>)> = vec![(tr("Alle Kategorien"), self.browser.category.is_none(), Box::new(|s: &mut Self, window: &mut Window, cx: &mut Context<Self>| s.browse_category(None, window, cx)))];
        let mut named: Vec<(String, &'static str)> = kind.categories().iter().map(|c| (category_name(c), *c)).collect();
        named.sort_by_cached_key(|(n, _)| n.to_lowercase());
        for (name, slug) in named {
            let checked = self.browser.category.as_deref() == Some(slug);
            categories.push((name, checked, Box::new(move |s: &mut Self, window: &mut Window, cx: &mut Context<Self>| s.browse_category(Some(slug.to_owned()), window, cx))));
        }
        let sort = self.browser.sort_for(query);
        let sorts = Sort::ALL.iter().map(|o| {
            let o = *o;
            (sort_name(o), o == sort, Box::new(move |s: &mut Self, window: &mut Window, cx: &mut Context<Self>| s.browse_sort(o, window, cx)) as Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>)
        });
        let all = self.browser.all;
        let toggle = |s: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            s.browser.all = !s.browser.all;
            s.browse_refresh(false, cx);
            s.browse_focus(window, cx);
        };
        let widen = widens(kind).then(|| {
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
                .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Alle zeigen")))
        });
        div()
            .h(px(CONTROLS_HEIGHT))
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).font_weight(FontWeight::MEDIUM).text_color(t.secondary).child(caption))
            .child(self.popup("content-category", category_label, categories, cx))
            .child(self.popup("content-sort", sort_name(sort), sorts.collect(), cx))
            .children(widen)
    }

    fn browse_category(&mut self, category: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.category = category;
        self.browse_refresh(false, cx);
        self.browse_focus(window, cx);
    }

    fn browse_sort(&mut self, sort: Sort, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.sort = sort;
        self.browse_refresh(false, cx);
        self.browse_focus(window, cx);
    }

    fn render_target(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        if self.browser.kind != Kind::DataPack {
            return None;
        }
        let worlds = self.browser.worlds.clone().unwrap_or_default();
        let several = worlds.iter().map(|w| w.account).collect::<HashSet<_>>().len() > 1;
        let label_of = |w: &World, cx: &App| if several { format!("{} · {}", w.name(), super::instance_files::account_name(w.account, cx)) } else { w.name().to_owned() };
        let current = match &self.browser.target {
            Some(dir) => worlds.iter().find(|w| &w.dir == dir).map_or_else(|| tr("Welt"), |w| label_of(w, cx)),
            None => tr("Für neue Welten"),
        };
        let mut items: Vec<(String, bool, Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>)> = Vec::new();
        for w in &worlds {
            let dir = w.dir.clone();
            let checked = self.browser.target.as_ref() == Some(&w.dir);
            items.push((label_of(w, cx), checked, Box::new(move |s: &mut Self, window: &mut Window, cx: &mut Context<Self>| s.browse_target(Some(dir.clone()), window, cx))));
        }
        items.push((tr("Für neue Welten"), self.browser.target.is_none(), Box::new(|s: &mut Self, window: &mut Window, cx: &mut Context<Self>| s.browse_target(None, window, cx))));
        let mut row = div().min_h(px(CONTROLS_HEIGHT)).flex().items_center().gap(px(space::S)).child(div().flex_none().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Installieren in"))).child(self.popup("content-target", current, items, cx));
        if self.needs_global_packs() {
            let text = if self.loader.takes_mods() { tr("Dafür braucht es den Mod Global Packs.") } else { tr("Vanilla lädt Datenpakete nur pro Welt.") };
            row = row.child(div().flex_1().min_w_0().text_size(px(font::CALLOUT)).text_color(t.secondary).child(text));
            if self.loader.takes_mods() {
                let busy = self.browser.adding.contains(packs::GLOBAL_PACKS);
                row = row.child(sheet::button("add-global-packs", tr("Global Packs hinzufügen"), Some("plus"), Look::Secondary, false, busy, t).on_click(cx.listener(|s, _, window, cx| {
                    s.add_mod(packs::GLOBAL_PACKS.to_owned(), "Global Packs".to_owned(), cx);
                    s.browse_focus(window, cx);
                })));
            }
        } else {
            row = row.child(div().flex_1());
        }
        Some(row)
    }

    pub(super) fn render_search(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let query = self.browser.query.read(cx).value().trim().to_string();
        let tabs = self.render_tabs(cx, t);
        let field = Input::new(&self.browser.query).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true);
        let hint = match (self.browser.project.is_some(), self.browse_selected()) {
            (false, Some((hit, act))) => {
                let command = keys("enter", true);
                let run = match act {
                    Act::Install if self.browser.kind == Kind::Modpack => trf("%@ legt es als Instanz an.", &[&command]),
                    Act::Install => trf("%@ installiert es.", &[&command]),
                    Act::Update => trf("%@ aktualisiert es.", &[&command]),
                    Act::Nothing => String::new(),
                };
                format!("{} {run}", trf("%@ öffnet „%@“.", &[&keys("enter", false), &hit.title])).trim().to_owned()
            }
            _ => String::new(),
        };
        let hint = div().h(px(font::CAPTION * font::LINE)).truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(hint);
        let target_row = if self.browser.kind == Kind::DataPack && self.browser.project.is_none() { CONTROLS_HEIGHT + space::XS } else { 0. };
        let height = (self.browser.body - CHROME - target_row).clamp(RESULTS_MIN, RESULTS_HEIGHT);
        let region = div().h(px(height)).flex().flex_col().overflow_hidden().rounded(px(radius::CONTROL)).border_1().border_color(t.separator);
        let region = if self.browser.project.is_some() {
            region.child(self.render_project(cx, t))
        } else {
            region.py(px(space::XXS)).child(self.render_results(&query, cx, t))
        };
        let controls = self.browser.project.is_none().then(|| self.render_controls(&query, cx, t));
        let target = self.browser.project.is_none().then(|| self.render_target(cx, t)).flatten();
        let bottom = self.render_notice(cx, t);
        div().flex().flex_col().gap(px(space::XS)).children(tabs.map(|tabs| div().pb(px(space::XS)).child(tabs))).child(field).child(hint).children(target).children(controls).child(region).child(bottom)
    }

    fn render_results(&mut self, query: &str, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        if self.browser.kind == Kind::Mod && !self.loader.takes_mods() {
            return self.render_empty(query, cx, t);
        }
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
            return self.render_empty(query, cx, t);
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

    fn render_empty(&mut self, query: &str, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let version = self.version.clone();
        let all = self.browser.all && widens(self.browser.kind);
        let what = kind_name(self.browser.kind);
        let text = if self.browser.kind == Kind::Mod && !self.loader.takes_mods() {
            trf("%@ lädt keine Mods. Ressourcenpakete, Shader und Datenpakete gehen trotzdem.", &[&self.loader.name()])
        } else if self.browser.category.is_some() {
            trf("Keine %@ in dieser Kategorie. Wähle „Alle Kategorien“.", &[&what])
        } else {
            match (query.is_empty(), all) {
                (true, false) => trf("Keine %@ für Minecraft %@ gefunden.", &[&what, &version]),
                (true, true) => trf("Keine %@ gefunden.", &[&what]),
                (false, false) => trf("Keine %@ zu „%@“ für %@. Versuche einen anderen Suchbegriff.", &[&what, &query, &version]),
                (false, true) => trf("Keine %@ zu „%@“ gefunden. Versuche einen anderen Suchbegriff.", &[&what, &query]),
            }
        };
        let widen = (!all && widens(self.browser.kind) && self.browser.category.is_none() && self.loader.takes_mods()).then(|| {
            sheet::button("search-all", tr("Alle zeigen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, window, cx| {
                s.browser.all = true;
                s.browse_refresh(false, cx);
                s.browse_focus(window, cx);
            }))
        });
        div()
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
            .into_any_element()
    }

    fn render_notice(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let line = div().min_h(px(control::M)).flex().items_center().gap(px(space::S));
        if let Some(e) = self.browser.error.clone() {
            return line.child(div().flex_1().min_w_0().child(self.problem("add-error", "circle-alert", t.danger_text, &e, cx, t)));
        }
        if self.browser.notice.is_none()
            && let Some(dir) = self.pack_dir()
            && let Some(Load::Failed(e)) = self.browser.shelves.get(&dir).map(|s| &s.files)
        {
            let e = e.clone();
            return line.child(div().flex_1().min_w_0().child(self.problem("shelf-error", "circle-alert", t.danger_text, &e, cx, t))).child(sheet::button("shelf-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Plain, false, false, t).on_click(cx.listener(move |s, _, _, cx| {
                s.browser.shelves.remove(&dir);
                s.browse_shelf(cx);
            })));
        }
        let undo = |cx: &mut Context<Self>, t: &Theme| {
            sheet::button("mod-undo", tr("Rückgängig"), Some("undo-2"), Look::Plain, false, false, t).on_click(cx.listener(|s, _, window, cx| {
                s.browse_undo(cx);
                s.refocus_browser(window, cx);
            }))
        };
        match &self.browser.notice {
            Some(Notice::Added(titles)) => line.child(sheet::status("circle-check", t.success_text, trf("Auch installiert: %@.", &[&titles.join(", ")]))),
            Some(Notice::Removed(m, _)) => line.child(sheet::status("check", t.secondary, trf("„%@“ entfernt.", &[&m.title]))).child(undo(cx, t)),
            Some(Notice::Trashed { title, .. }) => line.child(sheet::status("trash-2", t.secondary, trf("„%@“ liegt im Papierkorb der Instanz.", &[title]))).child(undo(cx, t)),
            Some(Notice::NeedsShaderMod { id, name }) => {
                let (id, name) = (*id, *name);
                let busy = self.browser.adding.contains(id);
                line.child(div().flex_1().min_w_0().child(sheet::status("info", t.secondary, trf("Shader laufen erst mit %@.", &[&name])))).child(sheet::button("add-shader-mod", trf("%@ hinzufügen", &[&name]), Some("plus"), Look::Secondary, false, busy, t).on_click(cx.listener(move |s, _, window, cx| {
                    s.add_mod(id.to_owned(), name.to_owned(), cx);
                    s.refocus_browser(window, cx);
                })))
            }
            Some(Notice::NoShaderMod) => line.child(sheet::status("info", t.secondary, trf("Shader brauchen Iris, und Iris braucht einen Mod-Loader. %@ hat keinen.", &[&self.loader.name()]))),
            Some(Notice::Importing(title)) => line.child(sheet::status("circle-check", t.success_text, trf("„%@“ wird als neue Instanz angelegt. Den Fortschritt siehst du in der Seitenleiste.", &[title]))),
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

/// `key`, with Cmd on the Mac or Ctrl elsewhere when `secondary`, as the platform writes it.
pub(super) fn keys(key: &str, secondary: bool) -> String {
    let mac = cfg!(target_os = "macos");
    let modifiers = Modifiers { control: secondary && !mac, platform: secondary && mac, ..Modifiers::default() };
    super::palette::label(&Keystroke { modifiers, key: key.to_owned(), key_char: None })
}

fn setup_dir(made: &crate::core::instance::Instance) -> PathBuf {
    made.dir()
}

fn search_of(key: &Key) -> Search {
    Search { filter: filter_for(key.kind, &key.version, &key.loader), query: key.query.clone(), sort: key.sort, category: key.category.clone(), all: key.all }
}

/// "Keine Verbindung · Ergebnisse von 14:03" for a page saved at `at`.
pub(super) fn saved_text(at: SystemTime) -> String {
    trf("Keine Verbindung · Ergebnisse von %@", &[&saved_when(at)])
}

/// When something was saved: the time today, else the day.
pub(super) fn saved_when(at: SystemTime) -> String {
    let at = chrono::DateTime::<chrono::Local>::from(at);
    if at.date_naive() == chrono::Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else if lang() == Lang::En {
        at.format("%b %-d").to_string()
    } else {
        at.format("%-d.%-m.").to_string()
    }
}

/// The square stand-in for a project without an icon.
pub(super) fn placeholder(t: &Theme) -> Div {
    div().size(px(control::L)).flex_none().rounded(px(radius::INNER)).bg(t.raised).flex().items_center().justify_center().child(icon("package", iconsize::M, t.tertiary))
}

/// A grey row standing in for a result that is still loading.
pub(super) fn skeleton_row(t: &Theme) -> Div {
    let bar = |width: f32| div().w(px(width)).h(px(BAR_HEIGHT)).rounded(px(radius::SMALL)).bg(t.ink.opacity(tint::HOVER));
    div()
        .flex()
        .items_center()
        .gap(px(space::M))
        .child(div().size(px(control::L)).flex_none().rounded(px(radius::INNER)).bg(t.ink.opacity(tint::HOVER)))
        .child(div().flex().flex_col().gap(px(space::S)).child(bar(NAME_BAR)).child(bar(TEXT_BAR)))
}
