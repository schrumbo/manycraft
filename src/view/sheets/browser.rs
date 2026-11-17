//! The content browser of the instance sheets: mods, resource packs, shaders, data packs and modpacks from Modrinth,
//! popular first, search as you type, install, update and remove in place.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

mod content;
mod project;

use super::instance::{self, Load, body_max, problem_view};
use crate::core::batch::ModRef;
use crate::core::i18n::{Lang, lang, tr, trf};
use crate::core::instance::{Instance, Loader};
use crate::core::modicons;
use crate::core::modrinth::{self, Filter, Hit, Kind, Page, Search, Sort, Support, Update, Version};
use crate::core::packs::{self, Installed};
use crate::core::worlds::{self, World};
use crate::ui::catalog;
use crate::ui::forms::{self, Problem, downloads_short};
use crate::ui::remote;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::{self, Field};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::sheet::{Card, Primary};
use crate::view::kit::spinner;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::{caption, line, para, strong};
use crate::view::kit::{after, at_least, button_focused, off_ui, toast, use_theme};
use crate::view::install::{self, Source};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, motion, space};
use crate::view::ui::BrowserUndo;

pub use content::{ContentPage, content_line};
use project::ProjectPage;

const SEARCH_DELAY: Duration = Duration::from_millis(250);
const RESULTS_HEIGHT: f32 = 372.;
const RESULTS_MIN: f32 = 216.;
const ROW_H: f32 = control::L + space::M + space::S;
const SKELETON_ROWS: usize = 7;
const MORE_AHEAD: usize = 4;
const KEPT_SEARCHES: usize = 200;
const MAX_LOADED: usize = 1000;
const KEPT_ICONS: usize = 48;
const DECODERS: usize = 4;
const UNDO_KEPT: usize = 20;
const AUTHOR_MAX: f32 = 120.;
const CATEGORY_W: f32 = 168.;
const SORT_W: f32 = 120.;
const SPLIT_AT: f32 = 1100.;
const PANE_LIST: f32 = 0.8;
const PANE_MAX: f32 = 1240.;
const OTHERS: f32 = control::M * 4. + space::XS * 7. + font::CAPTION * font::LINE + font::BODY * font::LINE;

/// Shows the Mods page of instance `id`: on Mods, or on Ressourcenpakete when the loader takes no mods.
pub fn add_mods(store: Store, id: String) {
    let Some(made) = store.model.peek().instance(&id).cloned() else { return };
    let tab = if made.loader.takes_mods() { Kind::Mod } else { Kind::ResourcePack };
    content::show(store, &id, tab);
}

/// Shows the Mods page of instance `id` on the tab of `kind`.
pub fn find(store: Store, id: &str, kind: Kind) {
    if store.model.peek().instance(id).is_some() {
        content::show(store, id, kind);
    }
}

/// Shows Entdecken, where every pack installs as a new instance.
pub fn find_modpacks(store: Store) {
    crate::view::kit::sheet::close(store.ui);
    let mut page = store.ui.page;
    page.set(crate::view::ui::Page::Discover);
}

/// What the sheet or page around the browser shares with it: the instance and what is picked, the work in flight, and
/// what leaving and Fertig do.
#[derive(Clone, PartialEq)]
pub(super) struct Scope {
    pub create: bool,
    /// Shown as the Modpacks page instead of on a sheet.
    pub page: bool,
    pub made: Option<Instance>,
    pub version: String,
    pub loader: Loader,
    pub mods: State<Vec<ModRef>>,
    pub touched: Option<State<bool>>,
    pub adding: State<HashSet<String>>,
    pub updatable: HashSet<String>,
    pub updating: HashSet<String>,
    pub saving: bool,
    pub held: Option<String>,
    pub pending: bool,
    pub dirty: bool,
    pub follow: Option<(State<String>, State<Loader>)>,
    pub kind: Kind,
    pub on_update: EventHandler<String>,
    pub back: EventHandler<()>,
    pub done: EventHandler<()>,
}

impl Scope {
    fn packs(&self) -> bool {
        self.page && self.made.is_none()
    }

    fn kinds(&self) -> &'static [Kind] {
        if self.packs() {
            &[Kind::Modpack]
        } else if self.create {
            &[Kind::Mod]
        } else {
            &Kind::ALL
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Query {
    kind: Kind,
    query: String,
    version: String,
    loader: Loader,
    sort: Sort,
    category: Option<String>,
    all: bool,
}

#[derive(Clone, PartialEq, Debug)]
enum Status {
    Idle,
    Loading,
    Failed(Problem),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum More {
    Idle,
    Loading,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Install,
    Update,
    Nothing,
}

/// The project whose page is open, and the tab it was opened on.
#[derive(Clone, PartialEq)]
struct Open {
    hit: Hit,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Enter {
    OpenPage,
    RunRow,
    RunPage,
}

fn enter_route(page: bool, command: bool) -> Enter {
    match (page, command) {
        (false, false) => Enter::OpenPage,
        (false, true) => Enter::RunRow,
        (true, _) => Enter::RunPage,
    }
}

enum Notice {
    Added(Vec<String>),
    Making(String),
    NeedsShaderMod { id: &'static str, name: &'static str },
    NoShaderMod,
}

#[derive(Clone, Debug)]
enum Back {
    Mod { previous: ModRef, at: usize, added: Vec<String> },
    Pack { kind: Kind, dir: PathBuf, entry: String, current: String, previous: String },
}

#[derive(Clone, Debug)]
enum Undo {
    Removed(ModRef, usize),
    Trashed { pid: String, title: String, dir: PathBuf, entry: String },
    Switched { pid: String, title: String, number: String, extra: Vec<String>, back: Back },
}

impl Undo {
    fn pid(&self) -> &str {
        match self {
            Undo::Removed(m, _) => &m.project_id,
            Undo::Trashed { pid, .. } | Undo::Switched { pid, .. } => pid,
        }
    }

    fn touches(&self, id: &str) -> bool {
        self.pid() == id || matches!(self, Undo::Switched { back: Back::Mod { added, .. }, .. } if added.iter().any(|a| a == id))
    }

    fn title(&self) -> &str {
        match self {
            Undo::Removed(m, _) => &m.title,
            Undo::Trashed { title, .. } | Undo::Switched { title, .. } => title,
        }
    }

    fn label(&self) -> String {
        match self {
            Undo::Switched { number, .. } => trf("Wechsel zu Version %@ rückgängig", &[number]),
            _ => trf("Entfernen von %@ rückgängig", &[&self.title()]),
        }
    }
}

struct Merged {
    previous: Option<(ModRef, usize)>,
    extra: Vec<String>,
    put: Vec<String>,
}

fn merge_version(list: &mut Vec<ModRef>, found: Vec<ModRef>, id: &str) -> Merged {
    let mut merged = Merged { previous: None, extra: Vec::new(), put: Vec::new() };
    for m in found {
        match list.iter().position(|have| have.project_id == m.project_id) {
            Some(at) if m.project_id == id => {
                merged.previous = Some((list[at].clone(), at));
                merged.put.push(m.project_id.clone());
                list[at] = m;
            }
            Some(_) => {}
            None => {
                if m.project_id != id {
                    merged.extra.push(m.title.clone());
                }
                merged.put.push(m.project_id.clone());
                list.push(m);
            }
        }
    }
    merged
}

fn stepped(at: Option<usize>, count: usize, delta: i32) -> usize {
    match at {
        None if delta == 1 || delta == -1 => 0,
        None => clamp_step(0, count, delta),
        Some(at) => clamp_step(at, count, delta),
    }
}

fn version_blocked(f: &Facts) -> Option<String> {
    if f.kind == Kind::Mod && !f.takes_mods {
        return Some(trf("%@ lädt keine Mods", &[&f.loader]));
    }
    f.install_blocked().or_else(|| (f.kind == Kind::Mod && (!f.updating.is_empty() || f.saving)).then(|| tr("Eine andere Änderung läuft noch.")))
}

fn fit_name(f: &Facts) -> String {
    format!("Minecraft {} · {}", f.version, f.loader)
}

fn keep_undo(list: &mut Vec<Undo>, next: Undo) {
    list.push(next);
    if list.len() > UNDO_KEPT {
        list.remove(0);
    }
}

fn forget_undo(list: &mut Vec<Undo>, ids: &[String]) {
    list.retain(|u| !ids.iter().any(|id| u.touches(id)));
}

fn clamp_step(sel: usize, count: usize, delta: i32) -> usize {
    if count == 0 {
        return 0;
    }
    (sel as i64 + i64::from(delta)).clamp(0, count as i64 - 1) as usize
}

fn wants_more(count: usize, last: usize, has_more: bool, more: More) -> bool {
    has_more && more == More::Idle && last + MORE_AHEAD >= count
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shown {
    Failed,
    Skeleton,
    Empty,
    List { dim: bool },
}

fn shown_of(has_page: bool, visible_empty: bool, fresh: bool, status: &Status) -> Shown {
    if !has_page {
        return if matches!(status, Status::Failed(_)) { Shown::Failed } else { Shown::Skeleton };
    }
    if visible_empty {
        return Shown::Empty;
    }
    Shown::List { dim: !fresh || matches!(status, Status::Loading) }
}

fn results_height(store: Store, kind: Kind) -> f32 {
    let target_h = if kind == Kind::DataPack { control::M + space::XS } else { 0. };
    (body_max(store) - OTHERS - target_h).clamp(RESULTS_MIN, RESULTS_HEIGHT)
}

#[derive(Clone)]
struct Shelf {
    files: Load<Vec<Installed>>,
    updates: Vec<Update>,
}

fn widens(kind: Kind) -> bool {
    matches!(kind, Kind::Mod | Kind::Modpack)
}

fn filter_for(kind: Kind, version: &str, loader: &Loader) -> Filter {
    Filter::of(kind, version, loader, None)
}

fn search_of(key: &Query) -> Search {
    Search { filter: filter_for(key.kind, &key.version, &key.loader), query: key.query.clone(), sort: key.sort, category: key.category.clone(), all: key.all }
}

/// What the search knows: the tab and its filters, the answers kept for this session, the rows shown and the one picked.
struct Core {
    kind: Kind,
    sort: Sort,
    category: Option<String>,
    all: bool,
    wanted: Option<Query>,
    shown: Option<Query>,
    status: Status,
    more: More,
    cache: HashMap<Query, Rc<Page>>,
    generation: u64,
    epoch: u64,
    sel: usize,
    visible: Rc<Vec<usize>>,
}

impl Core {
    fn new(kind: Kind) -> Self {
        Self { kind, sort: Sort::Relevance, category: None, all: false, wanted: None, shown: None, status: Status::Idle, more: More::Idle, cache: HashMap::new(), generation: 0, epoch: 0, sel: 0, visible: Rc::new(Vec::new()) }
    }

    fn sort_for(&self, query: &str) -> Sort {
        if query.is_empty() && self.sort == Sort::Relevance { Sort::Downloads } else { self.sort }
    }

    fn key(&self, typed: &str, version: &str, loader: &Loader) -> Query {
        let query = typed.trim().to_lowercase();
        let sort = self.sort_for(&query);
        Query { kind: self.kind, query, version: version.to_owned(), loader: loader.with_version(None), sort, category: self.category.clone(), all: self.all && widens(self.kind) }
    }

    fn fresh(&self) -> bool {
        self.shown.is_some() && self.shown == self.wanted
    }

    fn page(&self) -> Option<&Rc<Page>> {
        self.shown.as_ref().and_then(|k| self.cache.get(k))
    }

    fn reindex(&mut self) {
        let filter = self.shown.as_ref().map(|k| (filter_for(k.kind, &k.version, &k.loader), k.all));
        let visible = self.page().map_or_else(Vec::new, |p| p.hits.iter().enumerate().filter(|(_, h)| filter.as_ref().is_none_or(|(f, all)| *all || h.fits(f) == Support::Fits)).map(|(i, _)| i).collect());
        self.visible = Rc::new(visible);
    }

    fn stale(&self) -> bool {
        matches!(self.status, Status::Failed(_)) || self.page().is_some_and(|p| p.saved.is_some())
    }

    fn forget_saved(&mut self) {
        self.cache.retain(|_, p| p.saved.is_none());
        if self.page().is_none() {
            self.shown = None;
            self.wanted = None;
        }
    }

    fn has_more(&self) -> bool {
        self.fresh() && self.page().is_some_and(|p| p.hits.len() < p.total.min(MAX_LOADED))
    }

    fn show(&mut self, key: Query) {
        self.shown = Some(key);
        self.reindex();
        self.status = Status::Idle;
        self.sel = 0;
        self.epoch += 1;
    }

    fn keep(&mut self, key: Query, page: Page) {
        if self.cache.len() >= KEPT_SEARCHES {
            let shown = self.shown.clone();
            self.cache.retain(|k, _| Some(k) == shown.as_ref());
        }
        self.cache.entry(key).or_insert_with(|| Rc::new(page));
    }

    fn append(&mut self, key: &Query, offset: usize, next: Page) {
        let Some(page) = self.cache.get_mut(key) else { return };
        if page.hits.len() != offset {
            return;
        }
        let page = Rc::make_mut(page);
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

/// Values by key, the least recently used dropped beyond `cap`.
struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    clock: u64,
    cap: usize,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Lru<K, V> {
    fn new(cap: usize) -> Self {
        Self { map: HashMap::new(), clock: 0, cap }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.map.get_mut(key)?;
        entry.1 = clock;
        Some(entry.0.clone())
    }

    fn put(&mut self, key: K, value: V) {
        self.clock += 1;
        self.map.insert(key, (value, self.clock));
        while self.map.len() > self.cap {
            let Some(oldest) = self.map.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { return };
            self.map.remove(&oldest);
        }
    }
}

thread_local! {
    static ICONS: RefCell<Lru<String, ImageHandle>> = RefCell::new(Lru::new(KEPT_ICONS));
}

static GATE: LazyLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> = LazyLock::new(|| async_channel::bounded(DECODERS));

fn cached(pid: &str) -> Option<ImageHandle> {
    ICONS.with(|i| i.borrow_mut().get(&pid.to_owned()))
}

fn remember(pid: String, handle: ImageHandle) {
    ICONS.with(|i| i.borrow_mut().put(pid, handle));
}

struct Slot;

impl Slot {
    async fn take() -> Self {
        let _ = GATE.0.send(()).await;
        Self
    }

    #[cfg(test)]
    fn try_take() -> Option<Self> {
        GATE.0.try_send(()).ok().map(|()| Self)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = GATE.1.try_recv();
    }
}

fn decode(path: &std::path::Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// The icon of a project: fetched and decoded off the UI thread once, kept by project id; a tile while it loads.
#[derive(PartialEq)]
struct Face {
    pid: String,
    url: Option<String>,
}

impl Component for Face {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut shown = use_state(|| cached(&self.pid));
        use_side_effect_with_deps(&(self.pid.clone(), self.url.clone()), move |(pid, url)| {
            if let Some(handle) = cached(pid) {
                shown.set(Some(handle));
                return;
            }
            shown.set(None);
            let Some(url) = url.clone() else { return };
            let pid = pid.clone();
            spawn(async move {
                let slot = Slot::take().await;
                let wanted = vec![(pid.clone(), url)];
                let decoded = off_ui(move || {
                    let _slot = slot;
                    modicons::prepare(&wanted);
                    modicons::icon(&wanted[0].0).and_then(|path| decode(&path))
                })
                .await
                .flatten();
                let Some((w, h, rgba)) = decoded else { return };
                if let Some(handle) = ImageHandle::from_rgba(w, h, Bytes::from(rgba), AlphaType::Unpremul) {
                    remember(pid, handle.clone());
                    shown.set(Some(handle));
                }
            });
        });
        let tile = rect().width(Size::px(control::L)).height(Size::px(control::L)).corner_radius(t.shape.control).overflow(Overflow::Clip).center();
        match shown.read().clone() {
            Some(handle) => tile.child(image(handle).width(Size::px(control::L)).height(Size::px(control::L)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center)),
            None => tile.background(t.raised).child(icon(&t, Icon::Package, iconsize::M, t.text3)),
        }
    }
}

fn kind_name(kind: Kind) -> String {
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

fn search_problem(e: &anyhow::Error) -> Problem {
    if remote::is_network(e) {
        Problem { text: tr("Keine Verbindung zu Modrinth. Prüf dein Internet und versuch es nochmal."), detail: Some(format!("{e:#}")) }
    } else {
        forms::explain_as(tr("Die Suche hat nicht geklappt."), e)
    }
}

fn saved_when(at: SystemTime) -> String {
    let at = chrono::DateTime::<chrono::Local>::from(at);
    if at.date_naive() == chrono::Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else if lang() == Lang::En {
        at.format("%b %-d").to_string()
    } else {
        at.format("%-d.%-m.").to_string()
    }
}

fn enter_key(command: bool) -> String {
    match (cfg!(target_os = "macos"), command) {
        (true, false) => "Return".to_owned(),
        (true, true) => "⌘↩".to_owned(),
        (false, false) => tr("Eingabe"),
        (false, true) => format!("{}+{}", tr("Strg"), tr("Eingabe")),
    }
}

fn run_hint(act: Act, modpack: bool, command: bool) -> String {
    let key = enter_key(command);
    match act {
        Act::Install if modpack => trf("%@ installiert es.", &[&key]),
        Act::Install => trf("%@ fügt es hinzu.", &[&key]),
        Act::Update => trf("%@ aktualisiert es.", &[&key]),
        Act::Nothing => String::new(),
    }
}

fn hint_of(title: &str, act: Act, modpack: bool) -> String {
    let opens = trf("%@ öffnet „%@“.", &[&enter_key(false), &title]);
    format!("{opens} {}", run_hint(act, modpack, true)).trim().to_owned()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pack {
    None,
    Loading,
    Failed,
    Ready,
}

/// Everything a row needs to know to say what it offers.
#[derive(Clone, PartialEq)]
struct Facts {
    kind: Kind,
    version: String,
    loader: String,
    takes_mods: bool,
    filter: Filter,
    have: HashSet<String>,
    updatable: HashSet<String>,
    adding: HashSet<String>,
    updating: HashSet<String>,
    restoring: HashSet<String>,
    saving: bool,
    held: Option<String>,
    pack: Pack,
    global_needed: bool,
    pending: bool,
}

impl Facts {
    fn install_blocked(&self) -> Option<String> {
        match self.kind {
            Kind::Mod => {
                if self.version.is_empty() {
                    Some(tr("Wähle zuerst eine Version."))
                } else {
                    self.held.clone()
                }
            }
            Kind::Modpack => self.pending.then(|| tr("Sichere zuerst die Änderungen.")),
            _ if self.global_needed => Some(tr("Für neue Welten braucht es Global Packs.")),
            _ => match self.pack {
                Pack::None | Pack::Loading => Some(tr("Einen Moment, die Instanz wird gelesen …")),
                Pack::Failed => Some(tr("Erst muss klar sein, was schon installiert ist.")),
                Pack::Ready => self.held.clone(),
            },
        }
    }
}

fn act_of(hit: &Hit, f: &Facts) -> Act {
    let pid = &hit.project_id;
    if f.adding.contains(pid) {
        return Act::Nothing;
    }
    let fits = hit.fits(&f.filter) == Support::Fits;
    match f.kind {
        Kind::Mod => {
            if f.have.contains(pid) {
                return if f.updatable.contains(pid) && f.updating.is_empty() && !f.saving && f.held.is_none() { Act::Update } else { Act::Nothing };
            }
            if fits && f.takes_mods && f.install_blocked().is_none() { Act::Install } else { Act::Nothing }
        }
        _ => {
            if f.kind != Kind::Modpack {
                if f.pack == Pack::None {
                    return Act::Nothing;
                }
                if f.have.contains(pid) {
                    return if f.updatable.contains(pid) && f.held.is_none() { Act::Update } else { Act::Nothing };
                }
            }
            if fits && f.install_blocked().is_none() { Act::Install } else { Act::Nothing }
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
struct Refresh {
    busy: bool,
    blocked: Option<String>,
}

#[derive(Clone, PartialEq, Debug)]
enum Trail {
    Busy { label: String, reason: String },
    Present { update: Option<Refresh>, remove: Option<String> },
    Note(String),
    Install { label: String, blocked: Option<String> },
}

fn trail_of(hit: &Hit, f: &Facts) -> Trail {
    let pid = &hit.project_id;
    let installed = f.kind != Kind::Modpack && f.have.contains(pid);
    let adding = f.adding.contains(pid);
    let busy = adding || (f.kind == Kind::Mod && f.updating.contains(pid));
    let label = if f.kind == Kind::Modpack { tr("Installieren") } else { tr("Hinzufügen") };
    if adding && !installed {
        let reason = if f.restoring.contains(pid) { trf("„%@“ wird gerade geändert.", &[&hit.title]) } else { tr("Wird hinzugefügt …") };
        return Trail::Busy { label, reason };
    }
    if installed {
        let other = (f.kind == Kind::Mod && (!f.updating.is_empty() || f.saving)).then(|| tr("Eine andere Änderung läuft noch."));
        let update = (f.updatable.contains(pid) || busy).then(|| Refresh { busy, blocked: if busy { None } else { other.clone().or_else(|| f.held.clone()) } });
        let remove = if busy {
            Some(trf("„%@“ wird gerade geändert.", &[&hit.title]))
        } else if f.saving {
            Some(tr("Eine andere Änderung läuft noch."))
        } else {
            f.held.clone()
        };
        return Trail::Present { update, remove };
    }
    let support = hit.fits(&f.filter);
    if support != Support::Fits {
        return Trail::Note(match (support, f.kind) {
            (Support::OtherVersion, _) => trf("Nicht für Minecraft %@", &[&f.version]),
            (_, Kind::Shader) => tr("Nur für OptiFine"),
            _ => trf("Nicht für %@", &[&f.loader]),
        });
    }
    if f.kind == Kind::Mod && !f.takes_mods {
        return Trail::Note(trf("%@ lädt keine Mods", &[&f.loader]));
    }
    Trail::Install { label, blocked: f.install_blocked() }
}

fn val<T: Clone + 'static>(state: State<T>, subscribe: bool) -> T {
    if subscribe { state.read().clone() } else { state.peek().clone() }
}

/// What the scope says now, for code that outlives the render that made its `Flow`.
#[derive(Clone, PartialEq)]
struct Live {
    version: String,
    loader: Loader,
    held: Option<String>,
    dirty: bool,
}

/// The browser and what it works on, shared by its parts.
#[derive(Clone, PartialEq)]
struct Flow {
    store: Store,
    scope: Rc<Scope>,
    live: State<Live>,
    core: State<Core>,
    query: State<String>,
    error: State<Option<Problem>>,
    notice: State<Option<Notice>>,
    undos: State<Vec<Undo>>,
    restoring: State<HashSet<String>>,
    versioning: State<HashMap<String, String>>,
    shelves: State<HashMap<PathBuf, Shelf>>,
    worlds: State<Option<Vec<World>>>,
    target: State<Option<PathBuf>>,
    details: State<HashSet<String>>,
    open: State<Option<Open>>,
    list: ScrollController,
    list_epoch: State<u64>,
    body: ScrollController,
    search: AccessibilityId,
    room: State<f32>,
}

impl Flow {
    fn alive(&self) -> bool {
        self.core.try_peek().is_some()
    }

    fn focus(&self) {
        self.search.request_focus();
    }

    fn key(&self) -> Query {
        let live = self.live.peek();
        self.core.peek().key(&self.query.peek(), &live.version, &live.loader)
    }

    fn filter(&self) -> Filter {
        let live = self.live.peek();
        filter_for(self.core.peek().kind, &live.version, &live.loader)
    }

    fn rows(&self) -> i32 {
        let kind = self.core.peek().kind;
        let height = if self.scope.page { *self.room.peek() } else { results_height(self.store, kind) };
        ((height / ROW_H) as i32).max(1)
    }

    fn undoable(&self) -> bool {
        !self.undos.peek().is_empty()
    }

    fn keep_undo(&self, next: Undo) {
        let mut undos = self.undos;
        keep_undo(&mut undos.write(), next);
    }

    fn forget_undo(&self, ids: &[String]) {
        let mut undos = self.undos;
        if undos.peek().iter().any(|u| ids.iter().any(|id| u.touches(id))) {
            forget_undo(&mut undos.write(), ids);
        }
    }

    fn instance_dir(&self) -> Option<PathBuf> {
        self.scope.made.as_ref().filter(|_| !self.scope.create).map(Instance::dir)
    }

    fn pack_dir(&self, subscribe: bool) -> Option<PathBuf> {
        let kind = if subscribe { self.core.read().kind } else { self.core.peek().kind };
        packs::pack_dir(&self.instance_dir()?, kind, val(self.target, subscribe).as_deref())
    }

    fn global_packs(&self, subscribe: bool) -> bool {
        let local = self.scope.made.as_ref().map_or(&[][..], |m| m.local_mods.as_slice());
        packs::loads_global_packs(&val(self.scope.mods, subscribe), local)
    }

    fn shelf_of(&self, dir: &PathBuf, subscribe: bool) -> Option<Shelf> {
        let shelves = if subscribe { self.shelves.read() } else { self.shelves.peek() };
        shelves.get(dir).cloned()
    }

    fn facts(&self, subscribe: bool) -> Facts {
        let kind = if subscribe { self.core.read().kind } else { self.core.peek().kind };
        let dir = self.pack_dir(subscribe);
        let shelf = dir.as_ref().and_then(|d| self.shelf_of(d, subscribe));
        let (pack, have, updatable) = match kind {
            Kind::Mod => (Pack::None, val(self.scope.mods, subscribe).iter().map(|m| m.project_id.clone()).collect(), self.scope.updatable.clone()),
            Kind::Modpack => (Pack::None, HashSet::new(), HashSet::new()),
            _ => match shelf {
                None => (if dir.is_some() { Pack::Loading } else { Pack::None }, HashSet::new(), HashSet::new()),
                Some(Shelf { files, updates }) => {
                    let updatable = updates.iter().map(|u| u.current.project_id.clone()).collect();
                    match files {
                        Load::Loading => (Pack::Loading, HashSet::new(), updatable),
                        Load::Failed(_) => (Pack::Failed, HashSet::new(), updatable),
                        Load::Ready(files) => (Pack::Ready, files.iter().map(|i| i.found.project_id.clone()).collect(), updatable),
                    }
                }
            },
        };
        let global_needed = kind == Kind::DataPack && val(self.target, subscribe).is_none() && !self.global_packs(subscribe);
        Facts {
            kind,
            version: self.scope.version.clone(),
            loader: self.scope.loader.name().to_owned(),
            takes_mods: self.scope.loader.takes_mods(),
            filter: filter_for(kind, &self.scope.version, &self.scope.loader),
            have,
            updatable,
            adding: val(self.scope.adding, subscribe),
            updating: self.scope.updating.clone(),
            restoring: val(self.restoring, subscribe),
            saving: self.scope.saving,
            held: self.scope.held.clone(),
            pack,
            global_needed,
            pending: self.scope.pending,
        }
    }

    fn act(&self, hit: &Hit) -> Act {
        act_of(hit, &self.facts(false))
    }

    fn selected(&self) -> Option<(Hit, Act)> {
        let hit = {
            let c = self.core.peek();
            if !c.fresh() {
                return None;
            }
            let page = c.page()?;
            let at = *c.visible.get(c.sel)?;
            page.hits.get(at)?.clone()
        };
        let act = self.act(&hit);
        Some((hit, act))
    }

    fn refresh(&self, debounce: bool) {
        let key = self.key();
        let mut core = self.core;
        {
            let c = core.peek();
            if debounce && c.wanted.as_ref() == Some(&key) {
                return;
            }
        }
        if debounce && self.page_open() {
            let mut open = self.open;
            open.set(None);
        }
        let generation = {
            let mut c = core.write();
            c.generation += 1;
            c.more = More::Idle;
            c.wanted = Some(key.clone());
            c.generation
        };
        if core.peek().cache.contains_key(&key) {
            core.write().show(key);
            return;
        }
        let wait = debounce && !key.query.is_empty();
        if !wait {
            core.write().status = Status::Loading;
        }
        let flow = self.clone();
        spawn_in_window(async move {
            if wait {
                after(SEARCH_DELAY).await;
                if !flow.alive() || flow.core.peek().generation != generation {
                    return;
                }
                let mut core = flow.core;
                core.write().status = Status::Loading;
            }
            let search = search_of(&key);
            let result = off_ui(move || catalog::search(&search, 0)).await;
            if !flow.alive() {
                return;
            }
            let mut core = flow.core;
            let mut c = core.write();
            let current = c.generation == generation;
            match result {
                Some(Ok(page)) => {
                    c.keep(key.clone(), page);
                    if current {
                        c.show(key);
                    }
                }
                Some(Err(e)) if current => {
                    c.status = Status::Failed(search_problem(&e));
                    c.shown = None;
                }
                None if current => {
                    c.status = Status::Failed(Problem::plain(forms::unexplained()));
                    c.shown = None;
                }
                _ => {}
            }
        });
    }

    fn more(&self) {
        let (key, offset) = {
            let c = self.core.peek();
            let Some(key) = c.shown.clone() else { return };
            if c.more != More::Idle || !c.has_more() {
                return;
            }
            let Some(page) = c.page() else { return };
            (key, page.hits.len())
        };
        let mut core = self.core;
        let generation = {
            let mut c = core.write();
            c.more = More::Loading;
            c.generation
        };
        let flow = self.clone();
        spawn_in_window(async move {
            let search = search_of(&key);
            let result = off_ui(move || catalog::search(&search, offset)).await;
            if !flow.alive() {
                return;
            }
            let mut core = flow.core;
            let mut c = core.write();
            let current = c.generation == generation;
            match result {
                Some(Ok(next)) => {
                    c.append(&key, offset, next);
                    if current {
                        c.more = More::Idle;
                    }
                }
                _ if current => c.more = More::Failed,
                _ => {}
            }
        });
    }

    fn retry(&self) {
        if !self.alive() {
            return;
        }
        let mut core = self.core;
        if core.peek().stale() {
            core.write().forget_saved();
            self.refresh(false);
        }
        if core.peek().more == More::Failed {
            core.write().more = More::Idle;
            self.more();
        }
        if let Some(dir) = self.pack_dir(false)
            && matches!(self.shelf_of(&dir, false), Some(Shelf { files: Load::Failed(_), .. }))
        {
            let mut shelves = self.shelves;
            shelves.write().remove(&dir);
            self.shelf();
        }
    }

    fn tab(&self, kind: Kind) {
        if !self.scope.kinds().contains(&kind) {
            return;
        }
        if self.core.peek().kind != kind {
            let mut open = self.open;
            if open.peek().is_some() {
                open.set(None);
            }
            let mut core = self.core;
            {
                let mut c = core.write();
                c.kind = kind;
                c.category = None;
            }
            let mut error = self.error;
            error.set(None);
            let mut notice = self.notice;
            if notice.peek().is_some() {
                notice.set(None);
            }
            self.refresh(false);
            self.shelf();
        }
        self.focus();
    }

    fn category(&self, category: Option<String>) {
        let mut core = self.core;
        core.write().category = category;
        self.refresh(false);
        self.focus();
    }

    fn sort(&self, sort: Sort) {
        let mut core = self.core;
        core.write().sort = sort;
        self.refresh(false);
        self.focus();
    }

    fn all(&self, on: bool) {
        let mut core = self.core;
        core.write().all = on;
        self.refresh(false);
        self.focus();
    }

    fn cancel(&self) {
        let mut query = self.query;
        if self.page_open() {
            self.close_page();
        } else if query.peek().is_empty() {
            self.scope.back.call(());
        } else {
            query.set(String::new());
        }
    }

    fn page_open(&self) -> bool {
        self.open.peek().is_some()
    }

    fn paned(&self) -> bool {
        self.scope.page && self.store.ui.window.peek().0 >= SPLIT_AT
    }

    fn open_page(&self, hit: Hit) {
        let kind = self.core.peek().kind;
        let mut body = self.body;
        body.scroll_to_y(0);
        let mut open = self.open;
        open.set(Some(Open { hit, kind }));
        self.focus();
    }

    fn close_page(&self) {
        let mut open = self.open;
        if open.peek().is_some() {
            open.set(None);
        }
        self.focus();
    }

    fn open_selected(&self) {
        if let Some((hit, _)) = self.selected() {
            self.open_page(hit);
        }
    }

    fn page_primary(&self) {
        let Some(open) = self.open.peek().clone() else { return };
        let act = self.act(&open.hit);
        self.run(open.hit, act);
        self.focus();
    }

    fn enter(&self, command: bool) {
        match enter_route(self.page_open(), command) {
            Enter::OpenPage => self.open_selected(),
            Enter::RunRow => self.act_selected(),
            Enter::RunPage => self.page_primary(),
        }
    }

    fn other_field_typing(&self) -> bool {
        let focused = *Platform::get().current_window().focused_accessibility_id.peek();
        field::typing() && focused != self.search
    }

    fn scroll_page(&self, lines: f32, pages: f32) {
        let mut body = self.body;
        let viewport = body.bounds.peek().map_or(0., |(_, viewport)| viewport.height);
        let step = lines * frame::LINE_STEP + pages * (viewport - frame::LINE_STEP).max(frame::LINE_STEP);
        body.scroll_to_y((body.position().y - step) as i32);
    }

    fn step(&self, delta: i32) {
        let count = self.core.peek().visible.len();
        if count == 0 {
            return;
        }
        let at = clamp_step(self.core.peek().sel, count, delta);
        let mut core = self.core;
        core.write().sel = at;
        if at + 1 >= count {
            self.more();
        }
    }

    fn pick(&self, ix: usize) {
        let mut core = self.core;
        core.write().sel = ix;
    }

    fn pick_row(&self, ix: Option<usize>) {
        if let Some(ix) = ix {
            self.pick(ix);
        }
    }

    fn installed_version(&self, pid: &str, subscribe: bool) -> Option<String> {
        let kind = if subscribe { self.core.read().kind } else { self.core.peek().kind };
        match kind {
            Kind::Mod => val(self.scope.mods, subscribe).iter().find(|m| m.project_id == pid).map(|m| m.version_id.clone()),
            Kind::Modpack => None,
            _ => {
                let dir = self.pack_dir(subscribe)?;
                match self.shelf_of(&dir, subscribe)?.files {
                    Load::Ready(files) => files.into_iter().find(|i| i.found.project_id == pid).map(|i| i.found.version_id),
                    _ => None,
                }
            }
        }
    }

    fn act_selected(&self) {
        let Some((hit, act)) = self.selected() else { return };
        self.run(hit, act);
        self.focus();
    }

    fn run(&self, hit: Hit, act: Act) {
        let kind = self.core.peek().kind;
        match (kind, act) {
            (_, Act::Nothing) => {}
            (Kind::Mod, Act::Install) => self.install_mod(hit),
            (Kind::Mod, Act::Update) => self.scope.on_update.call(hit.project_id),
            (Kind::Modpack, _) => self.modpack(hit),
            (_, Act::Install) => self.pack(hit, false),
            (_, Act::Update) => self.pack(hit, true),
        }
    }

    fn mark(&self, pid: &str, on: bool) -> bool {
        let mut adding = self.scope.adding;
        let Some(mut set) = adding.try_write() else { return false };
        if on {
            set.insert(pid.to_owned());
        } else {
            set.remove(pid);
        }
        true
    }

    fn set_version_busy(&self, pid: &str, version: Option<&str>) {
        let mut state = self.versioning;
        let Some(mut map) = state.try_write() else { return };
        match version {
            Some(v) => map.insert(pid.to_owned(), v.to_owned()),
            None => map.remove(pid),
        };
    }

    fn add_version(&self, hit: &Hit, version: &Version) {
        let f = self.facts(false);
        let pid = &hit.project_id;
        if f.kind == Kind::Modpack || version.project_id != *pid || f.adding.contains(pid) || !modrinth::version_fits(version, &f.filter) || version_blocked(&f).is_some() {
            return;
        }
        if self.installed_version(pid, false).as_deref() == Some(version.id.as_str()) {
            return;
        }
        if f.kind == Kind::Mod { self.mod_version(hit, version.clone()) } else { self.pack_version(hit, version.clone()) }
    }

    fn mod_version(&self, hit: &Hit, version: Version) {
        let (id, title) = (hit.project_id.clone(), hit.title.clone());
        self.mark(&id, true);
        self.set_version_busy(&id, Some(&version.id));
        self.clear_error();
        let mut notice = self.notice;
        if matches!(notice.peek().as_ref(), Some(Notice::Added(_) | Notice::NeedsShaderMod { .. })) {
            notice.set(None);
        }
        let flow = self.clone();
        let (game, loader) = {
            let live = self.live.peek();
            (live.version.clone(), live.loader.clone())
        };
        spawn_in_window(async move {
            let (asked_game, asked_loader, name, number) = (game.clone(), loader.clone(), title.clone(), version.version_number.clone());
            let result = off_ui(move || remote::resolve_version(&version, &name, &asked_game, &asked_loader)).await;
            flow.set_version_busy(&id, None);
            if !flow.mark(&id, false) {
                return;
            }
            let mut mods = flow.scope.mods;
            if mods.try_peek().is_none() {
                return;
            }
            let moved = flow.scope.follow.as_ref().is_some_and(|(v, l)| v.try_peek().is_none_or(|v| *v != game) || l.try_peek().is_none_or(|l| *l != loader));
            if moved {
                return;
            }
            match result {
                Some(Ok(found)) => {
                    let resolved: Vec<String> = found.iter().map(|m| m.project_id.clone()).collect();
                    let merged = merge_version(&mut mods.write(), found, &id);
                    if let Some(mut touched) = flow.scope.touched.filter(|t| t.try_peek().is_some()) {
                        touched.set(true);
                    }
                    if flow.alive() {
                        flow.forget_undo(&resolved);
                        let added: Vec<String> = merged.put.iter().filter(|p| **p != id).cloned().collect();
                        if let Some((previous, at)) = merged.previous {
                            flow.keep_undo(Undo::Switched { pid: id.clone(), title, number, extra: merged.extra, back: Back::Mod { previous, at, added } });
                        } else if !merged.extra.is_empty() && !flow.undoable() {
                            notice.set(Some(Notice::Added(merged.extra)));
                        }
                    }
                }
                other => {
                    if flow.alive() {
                        flow.fail::<Vec<ModRef>>(trf("„%@“ ließ sich nicht hinzufügen.", &[&title]), other);
                    }
                }
            }
        });
    }

    fn pack_version(&self, hit: &Hit, version: Version) {
        let (Some(dir), Some(instance_dir)) = (self.pack_dir(false), self.instance_dir()) else { return };
        let (id, title) = (hit.project_id.clone(), hit.title.clone());
        let kind = self.core.peek().kind;
        let have = self.installed_in(&dir, &id);
        let filter = self.filter();
        self.mark(&id, true);
        self.set_version_busy(&id, Some(&version.id));
        self.clear_error();
        let flow = self.clone();
        spawn_in_window(async move {
            let (asked, name, number) = (dir.clone(), title.clone(), version.version_number.clone());
            let replacing = have.as_ref().map(|i| i.file.clone());
            let result = off_ui(move || catalog::install_version(&instance_dir, &asked, &name, &version, replacing.as_deref(), &filter)).await;
            flow.set_version_busy(&id, None);
            flow.mark(&id, false);
            if !flow.alive() {
                return;
            }
            let Some((placed, entry)) = flow.fail(trf("„%@“ ließ sich nicht hinzufügen.", &[&title]), result) else { return };
            flow.forget_undo(std::slice::from_ref(&id));
            let replaced = have.is_some();
            if let (Some(have), Some(entry)) = (have, entry) {
                flow.keep_undo(Undo::Switched { pid: id.clone(), title, number, extra: Vec::new(), back: Back::Pack { kind, dir: dir.clone(), entry, current: placed.file.clone(), previous: have.file } });
            }
            let mut shelves = flow.shelves;
            if let Some(shelf) = shelves.write().get_mut(&dir) {
                if let Load::Ready(files) = &mut shelf.files {
                    files.retain(|i| i.found.project_id != id);
                    files.push(placed);
                }
                shelf.updates.retain(|u| u.current.project_id != id);
            }
            if kind == Kind::Shader && !replaced {
                flow.offer_shader_mod();
            }
        });
    }

    fn clear_error(&self) {
        let mut error = self.error;
        error.set(None);
    }

    fn fail<T>(&self, what: String, result: Option<anyhow::Result<T>>) -> Option<T> {
        let mut error = self.error;
        match result {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => {
                error.set(Some(forms::explain_as(what, &e)));
                None
            }
            None => {
                error.set(Some(Problem::plain(forms::unexplained())));
                None
            }
        }
    }

    fn install_mod(&self, hit: Hit) {
        if self.core.peek().kind != Kind::Mod || self.act(&hit) != Act::Install {
            return;
        }
        self.add_mod(hit.project_id, hit.title);
    }

    fn add_mod(&self, id: String, title: String) {
        self.mark(&id, true);
        self.clear_error();
        let mut notice = self.notice;
        if matches!(notice.peek().as_ref(), Some(Notice::Added(_) | Notice::NeedsShaderMod { .. })) {
            notice.set(None);
        }
        let flow = self.clone();
        let (version, loader) = {
            let live = self.live.peek();
            (live.version.clone(), live.loader.clone())
        };
        spawn_in_window(async move {
            let ids = vec![id.clone()];
            let (asked_version, asked_loader) = (version.clone(), loader.clone());
            let result = off_ui(move || remote::resolve(&ids, &asked_version, &asked_loader)).await;
            if !flow.mark(&id, false) {
                return;
            }
            let mut mods = flow.scope.mods;
            if mods.try_peek().is_none() {
                return;
            }
            let moved = flow.scope.follow.as_ref().is_some_and(|(v, l)| v.try_peek().is_none_or(|v| *v != version) || l.try_peek().is_none_or(|l| *l != loader));
            if moved {
                return;
            }
            match result {
                Some(Ok(found)) => {
                    let resolved: Vec<String> = found.iter().map(|m| m.project_id.clone()).collect();
                    let mut extra = Vec::new();
                    {
                        let mut list = mods.write();
                        for m in found {
                            if !list.iter().any(|have| have.project_id == m.project_id) {
                                if m.project_id != id {
                                    extra.push(m.title.clone());
                                }
                                list.push(m);
                            }
                        }
                    }
                    if let Some(mut touched) = flow.scope.touched.filter(|t| t.try_peek().is_some()) {
                        touched.set(true);
                    }
                    if flow.alive() {
                        flow.forget_undo(&resolved);
                        if !extra.is_empty() && !flow.undoable() {
                            notice.set(Some(Notice::Added(extra)));
                        }
                    }
                }
                other => {
                    if flow.alive() {
                        flow.fail::<Vec<ModRef>>(trf("„%@“ ließ sich nicht hinzufügen.", &[&title]), other);
                    }
                }
            }
        });
    }

    fn pack(&self, hit: Hit, update: bool) {
        if self.act(&hit) != if update { Act::Update } else { Act::Install } {
            return;
        }
        let (Some(dir), Some(instance_dir)) = (self.pack_dir(false), self.instance_dir()) else { return };
        let kind = self.core.peek().kind;
        let id = hit.project_id.clone();
        let mut pending = if update { self.shelf_of(&dir, false).and_then(|s| s.updates.iter().find(|u| u.current.project_id == id).cloned()) } else { None };
        if let Some(u) = pending.as_mut() {
            let Some(have) = self.installed_in(&dir, &id) else { return };
            u.current.file_name = have.file;
        }
        let filter = self.filter();
        self.mark(&id, true);
        self.clear_error();
        let flow = self.clone();
        let title = hit.title.clone();
        spawn_in_window(async move {
            let (asked, name) = (dir.clone(), title.clone());
            let work = id.clone();
            let result = off_ui(move || catalog::install(&instance_dir, &asked, &work, &name, &filter, pending.as_ref())).await;
            flow.mark(&id, false);
            if !flow.alive() {
                return;
            }
            let what = if update { trf("„%@“ ließ sich nicht aktualisieren.", &[&title]) } else { trf("„%@“ ließ sich nicht hinzufügen.", &[&title]) };
            if let Some(placed) = flow.fail(what, result) {
                flow.forget_undo(std::slice::from_ref(&id));
                let mut shelves = flow.shelves;
                if let Some(shelf) = shelves.write().get_mut(&dir) {
                    if let Load::Ready(files) = &mut shelf.files {
                        files.retain(|i| i.found.project_id != id);
                        files.push(placed);
                    }
                    shelf.updates.retain(|u| u.current.project_id != id);
                }
                if kind == Kind::Shader && !update {
                    flow.offer_shader_mod();
                }
            }
        });
    }

    fn installed_in(&self, dir: &PathBuf, pid: &str) -> Option<Installed> {
        match self.shelf_of(dir, false)?.files {
            Load::Ready(files) => files.into_iter().find(|i| i.found.project_id == pid),
            _ => None,
        }
    }

    fn offer_shader_mod(&self) {
        let local = self.scope.made.as_ref().map_or(&[][..], |m| m.local_mods.as_slice());
        let mut notice = self.notice;
        if packs::shader_loader(&self.scope.mods.peek(), local).is_some() || self.undoable() {
            return;
        }
        notice.set(Some(match packs::shader_mod(&self.scope.loader) {
            Some((id, name)) => Notice::NeedsShaderMod { id, name },
            None => Notice::NoShaderMod,
        }));
    }

    fn modpack(&self, hit: Hit) {
        let f = self.facts(false);
        if f.kind != Kind::Modpack || act_of(&hit, &f) != Act::Install {
            return;
        }
        let filter = if self.core.peek().all { Filter::any(Kind::Modpack) } else { self.filter() };
        let source = Source::Pack { project: hit.project_id.clone(), title: hit.title.clone(), icon: hit.icon_url.clone(), filter };
        if self.scope.page || !self.live.peek().dirty {
            crate::view::kit::sheet::close(self.store.ui);
            install::start(self.store, vec![source]);
        } else {
            install::begin(self.store, vec![source]);
            let mut notice = self.notice;
            notice.set(Some(Notice::Making(hit.title.clone())));
        }
    }

    fn remove(&self, pid: &str) {
        if self.core.peek().kind != Kind::Mod {
            self.remove_pack(pid);
            return;
        }
        let mut mods = self.scope.mods;
        let Some(at) = mods.peek().iter().position(|m| m.project_id == pid) else { return };
        let removed = mods.write().remove(at);
        if let Some(mut touched) = self.scope.touched {
            touched.set(true);
        }
        self.clear_error();
        let mut notice = self.notice;
        if notice.peek().is_some() {
            notice.set(None);
        }
        self.keep_undo(Undo::Removed(removed, at));
    }

    fn remove_pack(&self, pid: &str) {
        let (Some(dir), Some(instance_dir)) = (self.pack_dir(false), self.instance_dir()) else { return };
        let Some(found) = self.installed_in(&dir, pid) else { return };
        let id = pid.to_owned();
        self.mark(&id, true);
        self.clear_error();
        let flow = self.clone();
        spawn_in_window(async move {
            let (asked, file) = (dir.clone(), found.file.clone());
            let result = off_ui(move || catalog::remove(&instance_dir, &asked, &file)).await;
            flow.mark(&id, false);
            if !flow.alive() {
                return;
            }
            let title = found.found.title.clone();
            if let Some(entry) = flow.fail(trf("„%@“ ließ sich nicht entfernen.", &[&title]), result) {
                let mut shelves = flow.shelves;
                if let Some(shelf) = shelves.write().get_mut(&dir) {
                    if let Load::Ready(files) = &mut shelf.files {
                        files.retain(|i| i.found.project_id != id);
                    }
                    shelf.updates.retain(|u| u.current.project_id != id);
                }
                let mut notice = flow.notice;
                if notice.peek().is_some() {
                    notice.set(None);
                }
                flow.keep_undo(Undo::Trashed { pid: id, title, dir, entry });
            }
        });
    }

    fn undo(&self) {
        if !self.alive() {
            return;
        }
        let mut undos = self.undos;
        let Some(next) = undos.peek().last().cloned() else { return };
        let held = self.live.peek().held.clone();
        if let Some(why) = held {
            toast::show(self.store.ui, why, None);
            return;
        }
        if self.scope.adding.peek().contains(next.pid()) {
            toast::show(self.store.ui, tr("Wird hinzugefügt …"), None);
            return;
        }
        let again = next.clone();
        match next {
            Undo::Removed(m, at) => {
                undos.write().pop();
                let mut mods = self.scope.mods;
                if mods.try_peek().is_some() && !mods.peek().iter().any(|have| have.project_id == m.project_id) {
                    let mut list = mods.write();
                    let at = at.min(list.len());
                    list.insert(at, m);
                }
            }
            Undo::Switched { pid, back: Back::Mod { previous, at, added }, .. } => {
                undos.write().pop();
                let mut mods = self.scope.mods;
                if mods.try_peek().is_some() {
                    let mut list = mods.write();
                    match list.iter().position(|m| m.project_id == pid) {
                        Some(i) => list[i] = previous,
                        None => {
                            let at = at.min(list.len());
                            list.insert(at, previous);
                        }
                    }
                    list.retain(|m| m.project_id == pid || !added.contains(&m.project_id));
                }
            }
            Undo::Switched { pid, title, back: Back::Pack { kind, dir, entry, current, previous }, .. } => {
                let Some(instance_dir) = self.instance_dir() else { return };
                undos.write().pop();
                self.mark(&pid, true);
                let mut restoring = self.restoring;
                restoring.write().insert(pid.clone());
                let flow = self.clone();
                spawn_in_window(async move {
                    let asked = dir.clone();
                    let result = off_ui(move || catalog::switch_back(&instance_dir, &asked, kind, &current, &entry, &previous)).await;
                    flow.mark(&pid, false);
                    if !flow.alive() {
                        return;
                    }
                    let mut restoring = flow.restoring;
                    restoring.write().remove(&pid);
                    let mut shelves = flow.shelves;
                    shelves.write().remove(&dir);
                    flow.shelf();
                    if flow.fail(trf("„%@“ ließ sich nicht zurückholen.", &[&title]), result).is_none() {
                        flow.keep_undo(again);
                    }
                });
            }
            Undo::Trashed { pid, title, dir, entry } => {
                let Some(instance_dir) = self.instance_dir() else { return };
                undos.write().pop();
                self.mark(&pid, true);
                let mut restoring = self.restoring;
                restoring.write().insert(pid.clone());
                let flow = self.clone();
                spawn_in_window(async move {
                    let result = off_ui(move || catalog::put_back(&instance_dir, &entry)).await;
                    flow.mark(&pid, false);
                    if !flow.alive() {
                        return;
                    }
                    let mut restoring = flow.restoring;
                    restoring.write().remove(&pid);
                    let mut shelves = flow.shelves;
                    shelves.write().remove(&dir);
                    flow.shelf();
                    flow.fail(trf("„%@“ ließ sich nicht zurückholen.", &[&title]), result);
                });
            }
        }
    }

    fn load_worlds(&self) {
        let mut worlds = self.worlds;
        if worlds.peek().is_some() {
            return;
        }
        let Some(dir) = self.instance_dir() else { return };
        worlds.set(Some(Vec::new()));
        let flow = self.clone();
        spawn_in_window(async move {
            let list = off_ui(move || {
                let mut list = worlds::list(&dir);
                list.sort_by_key(|w| std::cmp::Reverse(w.level.last_played.unwrap_or(0)));
                list
            })
            .await
            .unwrap_or_default();
            if !flow.alive() {
                return;
            }
            let mut target = flow.target;
            if target.peek().is_none() && !flow.global_packs(false) {
                target.set(list.first().map(|w| w.dir.clone()));
            }
            let mut worlds = flow.worlds;
            worlds.set(Some(list));
            flow.shelf();
        });
    }

    fn shelf(&self) {
        let kind = self.core.peek().kind;
        if kind == Kind::DataPack && self.worlds.peek().is_none() {
            self.load_worlds();
            return;
        }
        if widens(kind) {
            return;
        }
        let Some(dir) = self.pack_dir(false) else { return };
        if matches!(self.shelf_of(&dir, false), Some(Shelf { files: Load::Ready(_) | Load::Loading, .. })) {
            return;
        }
        let mut shelves = self.shelves;
        shelves.write().insert(dir.clone(), Shelf { files: Load::Loading, updates: Vec::new() });
        let filter = self.filter();
        let flow = self.clone();
        spawn_in_window(async move {
            let asked = dir.clone();
            let result = off_ui(move || {
                let files = catalog::installed(&asked)?;
                let current: Vec<ModRef> = files.iter().map(|i| i.found.clone()).collect();
                let updates = if current.is_empty() { Vec::new() } else { catalog::updates(&filter, &current).unwrap_or_default() };
                anyhow::Ok((files, updates))
            })
            .await;
            if !flow.alive() {
                return;
            }
            let shelf = match result {
                Some(Ok((files, updates))) => Shelf { files: Load::Ready(files), updates },
                Some(Err(e)) if remote::is_network(&e) => Shelf { files: Load::Failed(search_problem(&e)), updates: Vec::new() },
                Some(Err(e)) => Shelf { files: Load::Failed(forms::explain_as(tr("Was installiert ist, ließ sich nicht lesen."), &e)), updates: Vec::new() },
                None => Shelf { files: Load::Failed(Problem::plain(forms::unexplained())), updates: Vec::new() },
            };
            let mut shelves = flow.shelves;
            shelves.write().insert(dir, shelf);
        });
    }

    fn choose_target(&self, target: Option<PathBuf>) {
        let mut state = self.target;
        state.set(target);
        self.shelf();
        self.focus();
    }
}

/// The browser as the whole sheet: tabs, search, results and what the last action did, with Zurück and Fertig below.
#[derive(PartialEq)]
pub(super) struct Browser {
    pub scope: Scope,
}

/// The Modpacks page: Modrinth's packs for any version and loader, each installed as a new instance.
#[derive(PartialEq)]
pub struct PacksPage;

impl Component for PacksPage {
    fn render(&self) -> impl IntoElement {
        let mods = use_state(Vec::new);
        let adding = use_state(HashSet::new);
        let scope = Scope {
            create: false,
            page: true,
            made: None,
            version: String::new(),
            loader: Loader::Vanilla,
            mods,
            touched: None,
            adding,
            updatable: HashSet::new(),
            updating: HashSet::new(),
            saving: false,
            held: None,
            pending: false,
            dirty: false,
            follow: None,
            kind: Kind::Modpack,
            on_update: EventHandler::new(|_| {}),
            back: EventHandler::new(|_| {}),
            done: EventHandler::new(|_| {}),
        };
        Browser { scope }
    }
}

impl Component for Browser {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let query = use_state(String::new);
        let search = use_hook(AccessibilityId::new_unique);
        let kind = self.scope.kind;
        let paged = self.scope.page;
        let packs = self.scope.packs();
        let live = use_reactive(&Live { version: self.scope.version.clone(), loader: self.scope.loader.clone(), held: self.scope.held.clone(), dirty: self.scope.dirty });
        let flow = Flow {
            store,
            scope: Rc::new(self.scope.clone()),
            live,
            core: use_state(move || Core { all: packs, ..Core::new(kind) }),
            query,
            error: use_state(|| None),
            notice: use_state(|| None),
            undos: use_state(Vec::new),
            restoring: use_state(HashSet::new),
            versioning: use_state(HashMap::new),
            shelves: use_state(HashMap::new),
            worlds: use_state(|| None),
            target: use_state(|| None),
            details: use_state(HashSet::new),
            open: use_state(|| None),
            list: use_scroll_controller(ScrollConfig::default),
            list_epoch: use_state(|| 0),
            body: use_scroll_controller(ScrollConfig::default),
            search,
            room: use_state(|| RESULTS_HEIGHT),
        };
        let typed = query.read().trim().to_lowercase();
        let again = flow.clone();
        let fresh_deps = (typed.clone(), self.scope.version.clone(), self.scope.loader.with_version(None));
        use_side_effect_with_deps(&fresh_deps, move |_| again.refresh(true));
        let top = flow.undos.read().last().map(Undo::label);
        let undo_run = use_hook({
            let flow = flow.clone();
            move || EventHandler::new(move |_| flow.undo())
        });
        let mut slot = store.ui.browser_undo;
        use_side_effect_with_deps(&top, move |top| slot.set(top.clone().map(|label| BrowserUndo { label, run: undo_run.clone() })));
        use_drop(move || slot.set(None));
        let shelf = flow.clone();
        use_hook(move || shelf.shelf());
        let back = *store.back.read();
        let retry = flow.clone();
        use_side_effect_with_deps(&back, move |_| retry.retry());

        let (kind, sort, category, all, total, status, more, sel, epoch, has_more, saved, page, visible, fresh) = {
            let c = flow.core.read();
            (c.kind, c.sort_for(&typed), c.category.clone(), c.all && widens(c.kind), c.page().map(|p| p.total), c.status.clone(), c.more, c.sel, c.epoch, c.has_more(), c.page().and_then(|p| p.saved), c.page().cloned(), c.visible.clone(), c.fresh())
        };
        let facts = Rc::new(flow.facts(true));
        let open = flow.open.read().clone();
        let pane = paged && store.ui.window.read().0 >= SPLIT_AT;
        let detail = open.is_some() && !pane;
        let selected = flow.selected_view(&facts, fresh, sel, &page, &visible, detail);

        let mut column = rect().width(Size::fill()).spacing(space::XS).on_global_key_down({
            let flow = flow.clone();
            move |e: Event<KeyboardEventData>| {
                if flow.store.ui.menu.peek().is_some() || *flow.store.ui.palette.peek() || flow.other_field_typing() {
                    return;
                }
                if drive_rows(&flow, &e.key, e.modifiers) {
                    e.stop_propagation();
                    e.prevent_default();
                }
            }
        });
        let kinds = flow.scope.kinds();
        if kinds.len() > 1 {
            let segments = kinds.iter().map(|k| Segment { id: format!("browser.tab.{}", k.project_type()), label: kind_name(*k), tip: None }).collect();
            let at = kinds.iter().position(|k| *k == kind).unwrap_or(0);
            let pick = flow.clone();
            column = column.child(rect().padding((0., 0., space::XS, 0.)).child(rect().horizontal().child(Segmented::new(tr("Inhalt"), segments, at).on_choose(move |i| {
                if let Some(k) = pick.scope.kinds().get(i) {
                    pick.tab(*k);
                }
            }))));
        }
        let claimer = flow.clone();
        let field = Field::new("browser.search", query).placeholder(placeholder_text(kind)).glyph(Icon::Search).auto_focus().a11y(search).claim(move |key, modifiers| drive_rows(&claimer, key, modifiers));
        column = column.child(field);
        if open.is_none() {
            column = column.child(rect().width(Size::fill()).height(Size::px(font::CAPTION * font::LINE)).child(caption(&t, selected.unwrap_or_default())));
            if kind == Kind::DataPack {
                column = column.child(target_row(&t, &flow, &facts));
            }
            column = column.child(controls(&t, &flow, kind, &typed, category.as_deref(), sort, all, if fresh { total } else { None }));
        }
        let mut room = flow.room;
        let (listed, aside) = if pane { (None, open) } else { (open, None) };
        let project = |open: Open, pane: bool| {
            let pid = open.hit.project_id.clone();
            let have = flow.installed_version(&pid, true);
            let mods: HashSet<String> = val(flow.scope.mods, true).iter().map(|m| m.project_id.clone()).collect();
            rect().key(pid).width(Size::fill()).height(Size::fill()).child(ProjectPage { flow: flow.clone(), hit: open.hit, kind: open.kind, facts: facts.clone(), have, mods: Rc::new(mods), pane }).into_element()
        };
        let well = |t: &Theme| rect().corner_radius(t.shape.tile).overflow(Overflow::Clip).border(Border::new().fill(t.separator).width(frame::HAIRLINE).alignment(BorderAlignment::Inner));
        let aside = aside.map(|open| well(&t).width(Size::flex(1.)).height(Size::fill()).child(project(open, true)).into_element());
        let region = rect()
            .width(Size::fill())
            .height(if paged { Size::flex(1.) } else { Size::px(results_height(store, kind)) })
            .on_sized(move |e: Event<SizedEventData>| {
                if paged {
                    room.set_if_modified(e.area.height());
                }
            })
            .corner_radius(t.shape.tile)
            .overflow(Overflow::Clip)
            .border(Border::new().fill(t.separator).width(frame::HAIRLINE).alignment(BorderAlignment::Inner))
            .child(match listed {
                Some(open) => project(open, false),
                None => results(&t, &flow, &facts, kind, &typed, category.is_some(), all, status, more, sel, epoch, has_more, page, visible, fresh),
            });
        column = column.child(region);
        column = column.child(at_least(control::M, notice_row(&t, &flow, saved)).width(Size::fill()));
        if paged {
            let (esc, enter, arrive) = (flow.clone(), flow.clone(), flow.clone());
            use_hook(move || {
                spawn_in_window(async move {
                    after(motion::FAST).await;
                    if arrive.alive() {
                        arrive.focus();
                    }
                });
            });
            return rect()
                .width(Size::fill())
                .height(Size::fill())
                .cross_align(Alignment::Center)
                .padding((space::S, space::L, space::M, space::L))
                .on_global_key_down(move |e: Event<KeyboardEventData>| {
                    if enter.store.ui.layered() {
                        return;
                    }
                    match &e.key {
                        Key::Named(NamedKey::Escape) if esc.page_open() || !esc.query.peek().is_empty() || !esc.scope.packs() => {
                            e.stop_propagation();
                            esc.cancel();
                        }
                        Key::Named(NamedKey::Escape) => {
                            e.stop_propagation();
                            let mut page = esc.store.ui.page;
                            page.set(crate::view::ui::Page::Grid(crate::view::ui::Filter::All));
                        }
                        Key::Named(NamedKey::Enter) if e.modifiers.is_empty() && (field::typing() || enter.page_open() && !enter.other_field_typing() && !button_focused()) => {
                            e.stop_propagation();
                            enter.enter(false);
                        }
                        _ => {}
                    }
                })
                .child(match aside {
                    Some(aside) => rect()
                        .width(Size::fill())
                        .max_width(Size::px(PANE_MAX))
                        .height(Size::fill())
                        .horizontal()
                        .spacing(space::M)
                        .content(Content::flex())
                        .child(column.width(Size::flex(PANE_LIST)).height(Size::fill()).content(Content::flex()))
                        .child(aside)
                        .into_element(),
                    None => column.max_width(Size::px(frame::CONTENT_MAX)).height(Size::fill()).content(Content::flex()).into_element(),
                })
                .into_element();
        }

        let done = self.scope.done.clone();
        let title = if kind == Kind::Modpack {
            tr("Neue Instanz aus Modpack")
        } else if let Some(made) = self.scope.made.as_ref().filter(|_| !self.scope.create) {
            trf("Zu %@ hinzufügen", &[&made.name])
        } else {
            tr("Von Modrinth hinzufügen")
        };
        let subtitle = trf("Von Modrinth für Minecraft %@ · %@", &[&self.scope.version, &self.scope.loader.name()]);
        let primary = Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| done.call(())) };
        let (esc, enter) = (flow.clone(), flow.clone());
        Card::new("browser", title)
            .message(subtitle)
            .width(frame::SHEET)
            .body(column)
            .primary(primary)
            .primary_id("browser.done")
            .inform()
            .on_escape(move |_| esc.cancel())
            .on_enter(move |_| {
                if field::typing() || enter.page_open() && !enter.other_field_typing() && !button_focused() {
                    enter.enter(false);
                }
            })
            .into_element()
    }
}

fn drive_rows(flow: &Flow, key: &Key, modifiers: Modifiers) -> bool {
    let primary = modifiers.intersects(Modifiers::CONTROL | Modifiers::META) && !modifiers.intersects(Modifiers::SHIFT | Modifiers::ALT);
    let page = flow.page_open();
    match key {
        Key::Named(named @ (NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::PageUp | NamedKey::PageDown)) if modifiers.is_empty() && !crate::view::kit::vertical_arrows() => {
            if page && flow.paned() && matches!(named, NamedKey::ArrowUp | NamedKey::ArrowDown) {
                flow.step(if *named == NamedKey::ArrowUp { -1 } else { 1 });
                flow.open_selected();
                return true;
            }
            if page {
                match named {
                    NamedKey::ArrowUp => flow.scroll_page(-1., 0.),
                    NamedKey::ArrowDown => flow.scroll_page(1., 0.),
                    NamedKey::PageUp => flow.scroll_page(0., -1.),
                    _ => flow.scroll_page(0., 1.),
                }
                return true;
            }
            let rows = flow.rows();
            flow.step(match named {
                NamedKey::ArrowUp => -1,
                NamedKey::ArrowDown => 1,
                NamedKey::PageUp => -rows,
                _ => rows,
            });
            true
        }
        Key::Named(NamedKey::Home) if primary && !page => {
            flow.step(i32::MIN);
            true
        }
        Key::Named(NamedKey::End) if primary && !page => {
            flow.step(i32::MAX);
            true
        }
        Key::Named(NamedKey::Enter) if primary && !button_focused() => {
            flow.enter(true);
            true
        }
        Key::Character(c) if primary && c.eq_ignore_ascii_case("z") && flow.undoable() => {
            flow.undo();
            true
        }
        _ => false,
    }
}

impl Flow {
    fn selected_view(&self, facts: &Facts, fresh: bool, sel: usize, page: &Option<Rc<Page>>, visible: &[usize], open: bool) -> Option<String> {
        if !fresh || open {
            return None;
        }
        let hit = page.as_ref()?.hits.get(*visible.get(sel)?)?;
        Some(hint_of(&hit.title, act_of(hit, facts), facts.kind == Kind::Modpack))
    }
}

fn target_row(t: &Theme, flow: &Flow, facts: &Facts) -> Element {
    let worlds = val(flow.worlds, true).unwrap_or_default();
    let target = val(flow.target, true);
    let accounts = flow.store.model.read().accounts.iter().map(|a| (a.id, a.name.clone())).collect::<Vec<_>>();
    let several = worlds.iter().map(|w| w.account).collect::<HashSet<_>>().len() > 1;
    let label_of = |w: &World| {
        if several {
            let who = accounts.iter().find(|(id, _)| *id == w.account).map_or_else(|| tr("Entfernter Account"), |(_, n)| n.clone());
            format!("{} · {who}", w.name())
        } else {
            w.name().to_owned()
        }
    };
    let mut options: Vec<String> = worlds.iter().map(label_of).collect();
    options.push(tr("Für neue Welten"));
    let chosen = target.as_ref().and_then(|d| worlds.iter().position(|w| &w.dir == d)).unwrap_or(worlds.len());
    let dirs: Vec<Option<PathBuf>> = worlds.iter().map(|w| Some(w.dir.clone())).chain(std::iter::once(None)).collect();
    let pick = flow.clone();
    let select = Select::new("browser.target", tr("Hinzufügen zu"), options, chosen).width(frame::PICK_W).on_choose(move |i| {
        if let Some(dir) = dirs.get(i) {
            pick.choose_target(dir.clone());
        }
    });
    let mut row = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(at_least(control::M, line(t, tr("Hinzufügen zu"), font::CALLOUT, t.text2))).child(select);
    if facts.global_needed {
        let takes = flow.scope.loader.takes_mods();
        let text = if takes { tr("Dafür braucht es den Mod Global Packs.") } else { tr("Vanilla lädt Datenpakete nur pro Welt.") };
        row = row.child(rect().width(Size::flex(1.)).child(line(t, text, font::CALLOUT, t.text2)));
        if takes {
            let busy = facts.adding.contains(packs::GLOBAL_PACKS);
            let add = flow.clone();
            row = row.child(Button::new("browser.globalpacks", tr("Global Packs hinzufügen")).glyph(Icon::Plus).busy(busy).disabled(busy.then(|| tr("Wird hinzugefügt …")).or_else(|| facts.held.clone())).on_press(move |_| {
                add.add_mod(packs::GLOBAL_PACKS.to_owned(), "Global Packs".to_owned());
                add.focus();
            }));
        }
    }
    row.into_element()
}

#[allow(clippy::too_many_arguments)]
fn controls(t: &Theme, flow: &Flow, kind: Kind, typed: &str, category: Option<&str>, sort: Sort, all: bool, total: Option<usize>) -> Element {
    let caption_text = if typed.is_empty() && category.is_none() {
        tr("Beliebt")
    } else if let Some(total) = total {
        trf("%lld Treffer", &[&total])
    } else {
        String::new()
    };
    let mut named: Vec<(String, &'static str)> = kind.categories().iter().map(|c| (category_name(c), *c)).collect();
    named.sort_by_cached_key(|(n, _)| n.to_lowercase());
    let mut options = vec![tr("Alle Kategorien")];
    let mut slugs: Vec<Option<&'static str>> = vec![None];
    for (name, slug) in &named {
        options.push(name.clone());
        slugs.push(Some(slug));
    }
    let chosen = slugs.iter().position(|s| *s == category).unwrap_or(0);
    let pick = flow.clone();
    let category_select = Select::new("browser.category", tr("Kategorie"), options, chosen).width(CATEGORY_W).on_choose(move |i| {
        if let Some(slug) = slugs.get(i) {
            pick.category(slug.map(str::to_owned));
        }
    });
    let sorts: Vec<String> = Sort::ALL.iter().map(|s| sort_name(*s)).collect();
    let at = Sort::ALL.iter().position(|s| *s == sort).unwrap_or(0);
    let order = flow.clone();
    let sort_select = Select::new("browser.sort", tr("Sortierung"), sorts, at).width(SORT_W).on_choose(move |i| {
        if let Some(s) = Sort::ALL.get(i) {
            order.sort(*s);
        }
    });
    let mut row = rect()
        .width(Size::fill())
        .height(Size::px(control::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .content(Content::flex())
        .child(rect().width(Size::flex(1.)).child(line(t, caption_text, font::CALLOUT, t.text2).font_weight(FontWeight::MEDIUM)))
        .child(category_select)
        .child(sort_select);
    if widens(kind) && !flow.scope.packs() {
        let (flip, label) = (flow.clone(), flow.clone());
        row = row.child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::S)
                .child(Switch::new("browser.all", tr("Alle zeigen"), all).on_toggle(move |on| flip.all(on)))
                .child(line(t, tr("Alle zeigen"), font::CALLOUT, t.text2).cursor(CursorIcon::Pointer).on_press(move |_| label.all(!all))),
        );
    }
    row.into_element()
}

fn skeleton(t: &Theme) -> Element {
    let bar = |w: f32| rect().width(Size::px(w)).height(Size::px(frame::BAR_H)).corner_radius(t.shape.small).background(t.hover);
    rect()
        .width(Size::fill())
        .height(Size::px(ROW_H))
        .padding((0., space::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .child(rect().width(Size::px(control::L)).height(Size::px(control::L)).corner_radius(t.shape.control).background(t.hover))
        .child(rect().spacing(space::S).child(bar(frame::BAR_NAME)).child(bar(frame::BAR_TEXT)))
        .into_element()
}

fn status_line(t: &Theme, glyph: Icon, color: Color, text: String) -> Element {
    rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(icon(t, glyph, iconsize::S, color)).child(line(t, text, font::CALLOUT, color)).into_element()
}

#[allow(clippy::too_many_arguments)]
fn results(t: &Theme, flow: &Flow, facts: &Rc<Facts>, kind: Kind, typed: &str, categorised: bool, all: bool, status: Status, more: More, sel: usize, epoch: u64, has_more: bool, page: Option<Rc<Page>>, visible: Rc<Vec<usize>>, fresh: bool) -> Element {
    if kind == Kind::Mod && !facts.takes_mods {
        return empty(t, flow, facts, typed, categorised, all);
    }
    let shown = shown_of(page.is_some(), visible.is_empty(), fresh, &status);
    let (Some(page), Shown::List { dim }) = (page, shown) else {
        return match (shown, status) {
            (Shown::Failed, Status::Failed(why)) => {
                let retry = flow.clone();
                rect()
                    .width(Size::fill())
                    .padding(space::M)
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .content(Content::flex())
                    .child(rect().width(Size::flex(1.)).child(problem_view(t, "browser.error", Icon::WifiOff, &why, flow.details)))
                    .child(Button::new("browser.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| retry.refresh(false)))
                    .into_element()
            }
            (Shown::Empty, _) => empty(t, flow, facts, typed, categorised, all),
            _ => rect().width(Size::fill()).children((0..SKELETON_ROWS).map(|_| skeleton(t))).into_element(),
        };
    };
    let list = ResultList { view: View { flow: flow.clone(), facts: facts.clone(), page, visible, sel, more, has_more, epoch } };
    rect().width(Size::fill()).height(Size::fill()).padding(space::XXS).opacity(if dim { t.hidden_dim } else { 1. }).interactive(!dim).child(list).into_element()
}

fn empty(t: &Theme, flow: &Flow, facts: &Facts, typed: &str, categorised: bool, all: bool) -> Element {
    let what = kind_name(facts.kind);
    let all = all && widens(facts.kind);
    let text = if facts.kind == Kind::Mod && !facts.takes_mods {
        trf("%@ lädt keine Mods. Ressourcenpakete, Shader und Datenpakete gehen trotzdem.", &[&facts.loader])
    } else if categorised {
        trf("Keine %@ in dieser Kategorie. Wähle „Alle Kategorien“.", &[&what])
    } else {
        match (typed.is_empty(), all) {
            (true, false) => trf("Keine %@ für Minecraft %@ gefunden.", &[&what, &facts.version]),
            (true, true) => trf("Keine %@ gefunden.", &[&what]),
            (false, false) => trf("Keine %@ zu „%@“ für %@. Versuche einen anderen Suchbegriff.", &[&what, &typed, &facts.version]),
            (false, true) => trf("Keine %@ zu „%@“ gefunden. Versuche einen anderen Suchbegriff.", &[&what, &typed]),
        }
    };
    let mut column = rect()
        .width(Size::fill())
        .height(Size::fill())
        .padding(space::L)
        .center()
        .spacing(space::M)
        .child(icon(t, Icon::Search, iconsize::L, t.text3))
        .child(rect().max_width(Size::px(control::READING)).child(para(t, text, font::CALLOUT, t.text2).text_align(TextAlign::Center)));
    if !all && widens(facts.kind) && !categorised && facts.takes_mods {
        let widen = flow.clone();
        column = column.child(Button::new("browser.all.show", tr("Alle zeigen")).on_press(move |_| widen.all(true)));
    }
    if !widens(facts.kind) && typed.is_empty() {
        if categorised {
            let widen = flow.clone();
            column = column.child(Button::new("browser.category.clear", tr("Alle Kategorien")).on_press(move |_| widen.category(None)));
        } else {
            column = column.child(caption(t, tr("Wähle eine andere Minecraft-Version, um mehr zu finden.")));
        }
    }
    column.into_element()
}

fn switched_text(title: &str, number: &str, extra: &[String]) -> String {
    if extra.is_empty() {
        trf("„%@“ ist jetzt Version %@.", &[&title, &number])
    } else {
        trf("„%@“ ist jetzt Version %@. Auch hinzugefügt: %@.", &[&title, &number, &extra.join(", ")])
    }
}

fn notice_row(t: &Theme, flow: &Flow, saved: Option<SystemTime>) -> Element {
    let row = || rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex());
    let fill = |e: Element| rect().width(Size::flex(1.)).child(e);
    let note = |glyph: Icon, color: Color, text: String| fill(status_line(t, glyph, color, text));
    let undo = |flow: &Flow| {
        let run = flow.clone();
        Button::new("browser.undo", tr("Rückgängig")).plain().glyph(Icon::Undo).disabled(flow.scope.held.clone()).on_press(move |_| {
            run.undo();
            run.focus();
        })
    };
    if let Some(e) = val(flow.error, true) {
        return row().child(fill(problem_view(t, "browser.add", Icon::CircleAlert, &e, flow.details))).into_element();
    }
    let notice = flow.notice.read();
    let undos = flow.undos.read();
    if let Some(Notice::Making(title)) = notice.as_ref() {
        return row().child(note(Icon::CircleCheck, t.success_text, trf("„%@“ wird als neue Instanz angelegt. Den Fortschritt siehst du in der Seitenleiste.", &[title]))).into_element();
    }
    if let Some(top) = undos.last() {
        return match top {
            Undo::Removed(m, _) => row().child(note(Icon::Check, t.text2, trf("„%@“ entfernt.", &[&m.title]))).child(undo(flow)).into_element(),
            Undo::Trashed { title, .. } => row().child(note(Icon::Trash, t.text2, trf("„%@“ liegt im Papierkorb der Instanz.", &[title]))).child(undo(flow)).into_element(),
            Undo::Switched { title, number, extra, .. } => row().child(note(Icon::Check, t.text2, switched_text(title, number, extra))).child(undo(flow)).into_element(),
        };
    }
    if notice.is_none()
        && let Some(dir) = flow.pack_dir(true)
        && let Some(Shelf { files: Load::Failed(e), .. }) = flow.shelf_of(&dir, true)
    {
        let retry = flow.clone();
        return row()
            .child(fill(problem_view(t, "browser.shelf", Icon::CircleAlert, &e, flow.details)))
            .child(Button::new("browser.shelf.retry", tr("Erneut versuchen")).plain().glyph(Icon::Refresh).on_press(move |_| {
                let mut shelves = retry.shelves;
                shelves.write().remove(&dir);
                retry.shelf();
            }))
            .into_element();
    }
    match notice.as_ref() {
        Some(Notice::Added(titles)) => row().child(note(Icon::CircleCheck, t.success_text, trf("Auch hinzugefügt: %@.", &[&titles.join(", ")]))).into_element(),
        Some(Notice::Making(_)) => row().into_element(),
        Some(Notice::NeedsShaderMod { id, name }) => {
            let (id, name) = (*id, *name);
            let busy = val(flow.scope.adding, true).contains(id);
            let add = flow.clone();
            row()
                .child(note(Icon::Info, t.text2, trf("Shader laufen erst mit %@.", &[&name])))
                .child(Button::new("browser.shadermod", trf("%@ hinzufügen", &[&name])).glyph(Icon::Plus).busy(busy).disabled(busy.then(|| tr("Wird hinzugefügt …")).or_else(|| flow.scope.held.clone())).on_press(move |_| {
                    add.add_mod(id.to_owned(), name.to_owned());
                    add.focus();
                }))
                .into_element()
        }
        Some(Notice::NoShaderMod) => row().child(note(Icon::Info, t.text2, trf("Shader brauchen Iris, und Iris braucht Fabric oder Quilt. %@ ist keins von beiden.", &[&flow.scope.loader.name()]))).into_element(),
        None => match saved {
            Some(at) => {
                let retry = flow.clone();
                row()
                    .child(note(Icon::WifiOff, t.text2, trf("Keine Verbindung · Ergebnisse von %@", &[&saved_when(at)])))
                    .child(Button::new("browser.saved.retry", tr("Erneut versuchen")).plain().glyph(Icon::Refresh).on_press(move |_| {
                        let mut core = retry.core;
                        core.write().forget_saved();
                        retry.refresh(false);
                    }))
                    .into_element()
            }
            None => row().into_element(),
        },
    }
}

#[derive(Clone, PartialEq)]
struct View {
    flow: Flow,
    facts: Rc<Facts>,
    page: Rc<Page>,
    visible: Rc<Vec<usize>>,
    sel: usize,
    more: More,
    has_more: bool,
    epoch: u64,
}

impl View {
    fn item(&self, index: usize) -> Element {
        let count = self.visible.len();
        if index >= count {
            return rect().key("more").width(Size::fill()).height(Size::px(ROW_H)).child(Footer { flow: self.flow.clone(), failed: self.more == More::Failed }).into_element();
        }
        let Some(hit) = self.visible.get(index).and_then(|at| self.page.hits.get(*at)) else {
            return rect().height(Size::px(ROW_H)).into_element();
        };
        rect()
            .key(hit.project_id.clone())
            .width(Size::fill())
            .height(Size::px(ROW_H))
            .child(ResultRow { flow: self.flow.clone(), facts: self.facts.clone(), hit: hit.clone(), ix: index, selected: index == self.sel })
            .into_element()
    }
}

#[derive(PartialEq)]
struct ResultList {
    view: View,
}

impl Component for ResultList {
    fn render(&self) -> impl IntoElement {
        let scroll = self.view.flow.list;
        let mut seen = self.view.flow.list_epoch;
        let mut viewport = use_state(|| 0f32);
        let (sel, epoch) = (self.view.sel, self.view.epoch);
        use_side_effect_with_deps(&epoch, move |epoch| {
            if *seen.peek() != *epoch {
                seen.set(*epoch);
                let mut scroll = scroll;
                scroll.scroll_to_y(0);
            }
        });
        use_side_effect_with_deps(&sel, move |at| {
            let mut scroll = scroll;
            let top = *at as f32 * ROW_H;
            let bottom = top + ROW_H;
            let shown = -scroll.position().y;
            let h = *viewport.peek();
            if top < shown {
                scroll.scroll_to_y(-(top as i32));
            } else if h > 0. && bottom > shown + h {
                scroll.scroll_to_y(-((bottom - h) as i32));
            }
        });
        let (flow, count) = (self.view.flow.clone(), self.view.visible.len());
        use_side_effect_with_deps(&(count, self.view.has_more, self.view.more), move |(count, has_more, more)| {
            let shown = -scroll.position().y;
            let h = *viewport.read();
            if h <= 0. || !flow.alive() {
                return;
            }
            let last = ((shown + h) / ROW_H).ceil().max(0.) as usize;
            if wants_more(*count, last, *has_more, *more) {
                flow.more();
            }
        });
        let items = count + usize::from(self.view.has_more);
        VirtualScrollView::new_with_data_controlled(self.view.clone(), |item: VirtualItem, view: &View| view.item(item.index), scroll)
            .length(items)
            .item_size(ROW_H)
            .scroll_with_arrows(false)
            .width(Size::fill())
            .height(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| viewport.set_if_modified(e.area.height()))
    }
}

#[derive(PartialEq)]
struct Footer {
    flow: Flow,
    failed: bool,
}

impl Component for Footer {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let flow = self.flow.clone();
        use_hook(move || {
            spawn_in_window(async move {
                if flow.alive() {
                    flow.more();
                }
            });
        });
        if !self.failed {
            return skeleton(&t);
        }
        let retry = self.flow.clone();
        rect()
            .width(Size::fill())
            .height(Size::px(ROW_H))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .content(Content::flex())
            .child(rect().width(Size::flex(1.)).child(line(&t, tr("Weitere Ergebnisse ließen sich nicht laden."), font::CALLOUT, t.text2)))
            .child(Button::new("browser.more.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| {
                let mut core = retry.core;
                core.write().more = More::Idle;
                retry.more();
            }))
            .into_element()
    }
}

#[derive(PartialEq)]
struct ResultRow {
    flow: Flow,
    facts: Rc<Facts>,
    hit: Hit,
    ix: usize,
    selected: bool,
}

impl Component for ResultRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let hit = &self.hit;
        let pid = hit.project_id.clone();
        let spot = use_spot(&format!("browser.row.{pid}"), "row", &hit.title, true);
        let trail = trail_of(hit, &self.facts);
        let installed = self.facts.kind != Kind::Modpack && self.facts.have.contains(&pid);
        let dim = !installed && hit.fits(&self.facts.filter) != Support::Fits;

        let mut head = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(line(&t, hit.title.clone(), font::BODY, t.text1).font_weight(FontWeight::SEMI_BOLD).width(Size::flex(1.)));
        if !hit.author.is_empty() {
            head = head.child(rect().max_width(Size::px(AUTHOR_MAX)).child(caption(&t, trf("von %@", &[&hit.author]))));
        }
        head = head.child(rect().horizontal().cross_align(Alignment::Center).spacing(space::XXS).child(icon(&t, Icon::Download, iconsize::XS, t.text3)).child(caption(&t, downloads_short(hit.downloads)).font_tabular()));
        let text = rect()
            .width(Size::flex(1.))
            .spacing(space::XXS)
            .opacity(if dim { t.hidden_dim } else { 1. })
            .child(head)
            .child(line(&t, hit.description.clone(), font::CALLOUT, t.text2).width(Size::fill()));

        let flow = self.flow.clone();
        let (ix, press_hit) = (self.ix, hit.clone());
        let on_row = {
            let hit = hit.clone();
            move || {
                flow.pick(ix);
                flow.open_page(hit.clone());
            }
        };
        let fill = if self.selected {
            t.selection
        } else if *pressed.read() {
            t.press
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let click = on_row.clone();
        let row = rect()
            .width(Size::fill())
            .height(Size::px(ROW_H))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .corner_radius(t.shape.control)
            .background(fill)
            .cursor(CursorIcon::Pointer)
            .content(Content::flex())
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |_| pressed.set(true))
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_press(move |_| click())
            .child(Face { pid: pid.clone(), url: hit.icon_url.clone() })
            .child(text)
            .child(trail_view(&t, &self.flow, &press_hit, &trail, Some(ix), None));
        row
    }
}

fn trail_view(t: &Theme, flow: &Flow, hit: &Hit, trail: &Trail, ix: Option<usize>, hint: Option<String>) -> Element {
    let pid = hit.project_id.clone();
    let act = {
        let (flow, hit) = (flow.clone(), hit.clone());
        move || {
            flow.pick_row(ix);
            let act = flow.act(&hit);
            flow.run(hit.clone(), act);
            flow.focus();
        }
    };
    let row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
    match trail {
        Trail::Busy { label, reason } => row.child(Button::new(format!("browser.act.{pid}"), label.clone()).glyph(Icon::Plus).busy(true).disabled(Some(reason.clone()))).into_element(),
        Trail::Note(text) => row.child(line(t, text.clone(), font::CALLOUT, t.text3)).into_element(),
        Trail::Install { label, blocked } => {
            let mut add = Button::new(format!("browser.act.{pid}"), label.clone()).glyph(Icon::Plus).disabled(blocked.clone()).on_press(move |_| act());
            if let Some(hint) = hint {
                add = add.tip(hint);
            }
            row.child(add).into_element()
        }
        Trail::Present { update, remove } => {
            let first: Element = match update {
                Some(Refresh { busy, blocked }) => {
                    let mut update = Button::new(format!("browser.act.{pid}"), tr("Aktualisieren")).primary().glyph(Icon::Refresh).busy(*busy).disabled(blocked.clone().or_else(|| busy.then(|| tr("Wird gerade geändert.")))).on_press(move |_| act());
                    if let Some(hint) = hint {
                        update = update.tip(hint);
                    }
                    update.into_element()
                }
                None => status_line(t, Icon::Check, t.success_text, tr("Hinzugefügt")),
            };
            let (flow, id) = (flow.clone(), pid.clone());
            let gone = Button::new(format!("browser.remove.{pid}"), tr("Entfernen")).tip(trf("„%@“ entfernen", &[&hit.title])).disabled(remove.clone()).on_press(move |_| {
                flow.pick_row(ix);
                flow.remove(&id);
                flow.focus();
            });
            row.child(first).child(gone).into_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_opens_the_page_and_the_command_key_runs_the_row() {
        assert_eq!(enter_route(false, false), Enter::OpenPage);
        assert_eq!(enter_route(false, true), Enter::RunRow);
        assert_eq!(enter_route(true, false), Enter::RunPage);
        assert_eq!(enter_route(true, true), Enter::RunPage);
    }

    #[test]
    fn the_hint_names_both_keys_and_the_action() {
        let opens = trf("%@ öffnet „%@“.", &[&enter_key(false), &"Sodium"]);
        assert_eq!(hint_of("Sodium", Act::Install, false), format!("{opens} {}", trf("%@ fügt es hinzu.", &[&enter_key(true)])));
        assert_eq!(hint_of("Sodium", Act::Install, true), format!("{opens} {}", trf("%@ installiert es.", &[&enter_key(true)])));
        assert_eq!(hint_of("Sodium", Act::Update, false), format!("{opens} {}", trf("%@ aktualisiert es.", &[&enter_key(true)])));
        assert_eq!(hint_of("Sodium", Act::Nothing, false), opens);
    }

    fn hit(id: &str, versions: &[&str], categories: &[&str]) -> Hit {
        Hit { title: id.to_owned(), slug: id.to_owned(), project_id: id.to_owned(), description: String::new(), downloads: 0, icon_url: None, author: String::new(), versions: versions.iter().map(|v| (*v).to_owned()).collect(), categories: categories.iter().map(|c| (*c).to_owned()).collect() }
    }

    fn page(ids: &[&str], total: usize) -> Page {
        Page { hits: ids.iter().map(|i| hit(i, &["1.21"], &["fabric"])).collect(), total, saved: None }
    }

    fn key(core: &Core, query: &str) -> Query {
        core.key(query, "1.21", &Loader::Fabric(None))
    }

    fn facts(kind: Kind) -> Facts {
        Facts {
            kind,
            version: "1.21".into(),
            loader: "Fabric".into(),
            takes_mods: true,
            filter: filter_for(kind, "1.21", &Loader::Fabric(None)),
            have: HashSet::new(),
            updatable: HashSet::new(),
            adding: HashSet::new(),
            updating: HashSet::new(),
            restoring: HashSet::new(),
            saving: false,
            held: None,
            pack: Pack::Ready,
            global_needed: false,
            pending: false,
        }
    }

    fn fitting(kind: Kind, id: &str) -> Hit {
        match kind {
            Kind::ResourcePack => hit(id, &["1.21"], &["minecraft"]),
            Kind::DataPack => hit(id, &["1.21"], &["datapack"]),
            Kind::Shader => hit(id, &["1.21"], &["iris"]),
            _ => hit(id, &["1.21"], &["fabric"]),
        }
    }

    #[test]
    fn a_search_is_keyed_by_the_trimmed_lowercase_query() {
        let core = Core::new(Kind::Mod);
        assert_eq!(key(&core, "  SoDiuM "), key(&core, "sodium"));
        assert_ne!(key(&core, "sodium"), key(&core, "lithium"));
        assert_eq!(key(&core, "Ärger ").query, "ärger");
    }

    #[test]
    fn the_loader_version_does_not_split_searches() {
        let core = Core::new(Kind::Mod);
        assert_eq!(core.key("a", "1.21", &Loader::Fabric(Some("0.1".into()))), core.key("a", "1.21", &Loader::Fabric(None)));
    }

    #[test]
    fn an_empty_query_sorts_by_downloads_unless_another_order_is_picked() {
        let mut core = Core::new(Kind::Mod);
        assert_eq!(core.sort_for(""), Sort::Downloads);
        assert_eq!(core.sort_for("x"), Sort::Relevance);
        core.sort = Sort::Newest;
        assert_eq!(core.sort_for(""), Sort::Newest);
    }

    #[test]
    fn all_only_widens_mods_and_modpacks() {
        let mut core = Core::new(Kind::ResourcePack);
        core.all = true;
        assert!(!key(&core, "").all);
        core.kind = Kind::Mod;
        assert!(key(&core, "").all);
    }

    #[test]
    fn showing_a_page_picks_the_first_row_and_counts_a_new_epoch() {
        let mut core = Core::new(Kind::Mod);
        let k = key(&core, "");
        core.wanted = Some(k.clone());
        core.keep(k.clone(), page(&["a", "b"], 2));
        core.sel = 1;
        core.status = Status::Loading;
        core.show(k);
        assert_eq!((core.sel, core.epoch, core.status.clone()), (0, 1, Status::Idle));
        assert!(core.fresh());
        assert_eq!(core.visible.as_slice(), &[0, 1]);
    }

    #[test]
    fn rows_that_do_not_fit_stay_hidden_until_all_is_on() {
        let mut core = Core::new(Kind::Mod);
        let k = key(&core, "");
        core.keep(k.clone(), Page { hits: vec![hit("a", &["1.21"], &["fabric"]), hit("b", &["1.20"], &["fabric"]), hit("c", &["1.21"], &["forge"])], total: 3, saved: None });
        core.show(k);
        assert_eq!(core.visible.as_slice(), &[0]);
        core.all = true;
        let wide = key(&core, "");
        core.keep(wide.clone(), Page { hits: vec![hit("a", &["1.21"], &["fabric"]), hit("b", &["1.20"], &["fabric"])], total: 2, saved: None });
        core.show(wide);
        assert_eq!(core.visible.as_slice(), &[0, 1]);
    }

    #[test]
    fn more_results_are_added_once_and_the_total_follows() {
        let mut core = Core::new(Kind::Mod);
        let k = key(&core, "");
        core.keep(k.clone(), page(&["a", "b"], 5));
        core.show(k.clone());
        assert!(core.has_more() || !core.fresh());
        core.wanted = Some(k.clone());
        assert!(core.has_more());
        core.append(&k, 2, page(&["b", "c"], 5));
        let p = core.page().map(|p| (p.hits.len(), p.total));
        assert_eq!(p, Some((3, 5)));
        assert_eq!(core.visible.len(), 3);
    }

    #[test]
    fn a_page_with_nothing_new_ends_the_list() {
        let mut core = Core::new(Kind::Mod);
        let k = key(&core, "");
        core.wanted = Some(k.clone());
        core.keep(k.clone(), page(&["a", "b"], 9));
        core.show(k.clone());
        core.append(&k, 2, page(&["a"], 9));
        assert!(!core.has_more());
    }

    #[test]
    fn a_page_for_a_stale_offset_is_ignored() {
        let mut core = Core::new(Kind::Mod);
        let k = key(&core, "");
        core.keep(k.clone(), page(&["a", "b"], 9));
        core.append(&k, 1, page(&["c"], 9));
        assert_eq!(core.cache.get(&k).map(|p| p.hits.len()), Some(2));
    }

    #[test]
    fn nothing_more_is_loaded_past_the_cap_or_the_total() {
        let mut core = Core::new(Kind::Mod);
        let k = key(&core, "");
        core.wanted = Some(k.clone());
        core.keep(k.clone(), page(&["a"], 1));
        core.show(k.clone());
        assert!(!core.has_more());
        core.cache.insert(k.clone(), Rc::new(Page { hits: (0..MAX_LOADED).map(|i| hit(&i.to_string(), &["1.21"], &["fabric"])).collect(), total: MAX_LOADED + 50, saved: None }));
        assert!(!core.has_more());
    }

    #[test]
    fn saved_pages_are_forgotten_and_the_view_is_reset_when_one_was_shown() {
        let mut core = Core::new(Kind::Mod);
        let k = key(&core, "");
        core.keep(k.clone(), Page { saved: Some(SystemTime::now()), ..page(&["a"], 1) });
        core.wanted = Some(k.clone());
        core.show(k);
        assert!(core.stale());
        core.forget_saved();
        assert!(core.page().is_none() && core.shown.is_none() && core.wanted.is_none());
        assert!(!core.stale());
    }

    #[test]
    fn a_failed_search_is_stale_so_the_connection_coming_back_retries_it() {
        let mut core = Core::new(Kind::Mod);
        core.status = Status::Failed(Problem::plain("x"));
        assert!(core.stale());
    }

    #[test]
    fn the_kept_answers_are_bounded_and_keep_the_shown_one() {
        let mut core = Core::new(Kind::Mod);
        let shown = key(&core, "shown");
        core.keep(shown.clone(), page(&["a"], 1));
        core.show(shown.clone());
        for i in 0..KEPT_SEARCHES + 5 {
            core.keep(key(&core, &format!("q{i}")), page(&["a"], 1));
        }
        assert!(core.cache.len() <= KEPT_SEARCHES);
        assert!(core.cache.contains_key(&shown));
    }

    #[test]
    fn the_icon_cache_drops_the_least_recently_used() {
        let mut lru: Lru<String, u32> = Lru::new(2);
        lru.put("a".into(), 1);
        lru.put("b".into(), 2);
        assert_eq!(lru.get(&"a".to_owned()), Some(1));
        lru.put("c".into(), 3);
        assert_eq!(lru.get(&"b".to_owned()), None);
        assert_eq!(lru.get(&"a".to_owned()), Some(1));
        assert_eq!(lru.get(&"c".to_owned()), Some(3));
    }

    #[test]
    fn a_mod_that_fits_is_installed_and_one_that_is_there_is_not() {
        let mut f = facts(Kind::Mod);
        let h = fitting(Kind::Mod, "a");
        assert_eq!(act_of(&h, &f), Act::Install);
        f.have.insert("a".into());
        assert_eq!(act_of(&h, &f), Act::Nothing);
    }

    #[test]
    fn no_mod_is_installed_before_a_version_is_chosen() {
        let mut f = facts(Kind::Mod);
        f.version.clear();
        let h = fitting(Kind::Mod, "a");
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Hinzufügen"), blocked: Some(tr("Wähle zuerst eine Version.")) });
    }

    #[test]
    fn a_mod_with_an_update_updates_unless_something_else_runs() {
        let mut f = facts(Kind::Mod);
        let h = fitting(Kind::Mod, "a");
        f.have.insert("a".into());
        f.updatable.insert("a".into());
        assert_eq!(act_of(&h, &f), Act::Update);
        f.saving = true;
        assert_eq!(act_of(&h, &f), Act::Nothing);
        f.saving = false;
        f.updating.insert("b".into());
        assert_eq!(act_of(&h, &f), Act::Nothing);
    }

    #[test]
    fn a_row_being_worked_on_offers_nothing() {
        let mut f = facts(Kind::Mod);
        f.adding.insert("a".into());
        assert_eq!(act_of(&fitting(Kind::Mod, "a"), &f), Act::Nothing);
    }

    #[test]
    fn a_loader_without_mods_installs_no_mod() {
        let mut f = facts(Kind::Mod);
        f.takes_mods = false;
        assert_eq!(act_of(&fitting(Kind::Mod, "a"), &f), Act::Nothing);
        assert_eq!(trail_of(&fitting(Kind::Mod, "a"), &f), Trail::Note(trf("%@ lädt keine Mods", &[&"Fabric"])));
    }

    #[test]
    fn a_misfit_says_why() {
        let f = facts(Kind::Mod);
        let other_version = hit("a", &["1.20"], &["fabric"]);
        let other_loader = hit("b", &["1.21"], &["forge"]);
        assert_eq!(trail_of(&other_version, &f), Trail::Note(trf("Nicht für Minecraft %@", &[&"1.21"])));
        assert_eq!(trail_of(&other_loader, &f), Trail::Note(trf("Nicht für %@", &[&"Fabric"])));
        let shader = facts(Kind::Shader);
        assert_eq!(trail_of(&hit("c", &["1.21"], &["optifine"]), &shader), Trail::Note(tr("Nur für OptiFine")));
    }

    #[test]
    fn packs_wait_for_what_is_installed_before_they_install() {
        let mut f = facts(Kind::ResourcePack);
        let h = fitting(Kind::ResourcePack, "a");
        f.pack = Pack::Loading;
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Hinzufügen"), blocked: Some(tr("Einen Moment, die Instanz wird gelesen …")) });
        f.pack = Pack::Failed;
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Hinzufügen"), blocked: Some(tr("Erst muss klar sein, was schon installiert ist.")) });
        f.pack = Pack::Ready;
        assert_eq!(act_of(&h, &f), Act::Install);
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Hinzufügen"), blocked: None });
    }

    #[test]
    fn data_packs_for_new_worlds_need_global_packs() {
        let mut f = facts(Kind::DataPack);
        let h = fitting(Kind::DataPack, "a");
        f.global_needed = true;
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Hinzufügen"), blocked: Some(tr("Für neue Welten braucht es Global Packs.")) });
    }

    #[test]
    fn an_installed_pack_updates_when_the_shelf_knows_a_newer_file() {
        let mut f = facts(Kind::ResourcePack);
        let h = fitting(Kind::ResourcePack, "a");
        f.have.insert("a".into());
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Present { update: None, remove: None });
        f.updatable.insert("a".into());
        assert_eq!(act_of(&h, &f), Act::Update);
        assert_eq!(trail_of(&h, &f), Trail::Present { update: Some(Refresh { busy: false, blocked: None }), remove: None });
    }

    #[test]
    fn modpacks_install_as_instances_and_wait_for_unsaved_changes() {
        let mut f = facts(Kind::Modpack);
        let h = fitting(Kind::Modpack, "a");
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Installieren"), blocked: None });
        assert_eq!(act_of(&h, &f), Act::Install);
        f.pending = true;
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Installieren"), blocked: Some(tr("Sichere zuerst die Änderungen.")) });
    }

    #[test]
    fn busy_rows_say_so_and_installed_rows_lock_while_something_else_runs() {
        let mut f = facts(Kind::Mod);
        let h = fitting(Kind::Mod, "a");
        f.adding.insert("a".into());
        assert_eq!(trail_of(&h, &f), Trail::Busy { label: tr("Hinzufügen"), reason: tr("Wird hinzugefügt …") });
        f.adding.clear();
        f.have.insert("a".into());
        f.updatable.insert("a".into());
        f.saving = true;
        let blocked = Some(tr("Eine andere Änderung läuft noch."));
        assert_eq!(trail_of(&h, &f), Trail::Present { update: Some(Refresh { busy: false, blocked: blocked.clone() }), remove: blocked });
        f.saving = false;
        f.updating.insert("a".into());
        assert_eq!(trail_of(&h, &f), Trail::Present { update: Some(Refresh { busy: true, blocked: None }), remove: Some(trf("„%@“ wird gerade geändert.", &[&"a"])) });
    }

    #[test]
    fn a_held_instance_blocks_adding_updating_and_removing() {
        let held = Some(tr("Wird wiederhergestellt …"));
        let mut f = facts(Kind::Mod);
        f.held = held.clone();
        let h = fitting(Kind::Mod, "a");
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Install { label: tr("Hinzufügen"), blocked: held.clone() });
        f.have.insert("a".into());
        f.updatable.insert("a".into());
        assert_eq!(act_of(&h, &f), Act::Nothing);
        assert_eq!(trail_of(&h, &f), Trail::Present { update: Some(Refresh { busy: false, blocked: held.clone() }), remove: held.clone() });
        let mut packs = facts(Kind::ResourcePack);
        packs.held = held.clone();
        let p = fitting(Kind::ResourcePack, "p");
        assert_eq!(act_of(&p, &packs), Act::Nothing);
        packs.have.insert("p".into());
        packs.updatable.insert("p".into());
        assert_eq!(act_of(&p, &packs), Act::Nothing);
    }

    #[test]
    fn a_held_instance_does_not_block_modpacks_which_make_a_new_one() {
        let mut f = facts(Kind::Modpack);
        f.held = Some("held".into());
        assert_eq!(act_of(&fitting(Kind::Modpack, "a"), &f), Act::Install);
    }

    #[test]
    fn a_pack_being_put_back_says_it_is_being_changed() {
        let mut f = facts(Kind::ResourcePack);
        let h = fitting(Kind::ResourcePack, "a");
        f.adding.insert("a".into());
        f.restoring.insert("a".into());
        assert_eq!(trail_of(&h, &f), Trail::Busy { label: tr("Hinzufügen"), reason: trf("„%@“ wird gerade geändert.", &[&"a"]) });
    }

    fn removed(id: &str) -> Undo {
        Undo::Removed(ModRef { project_id: id.into(), version_id: "v".into(), title: id.into(), file_name: String::new(), url: String::new(), sha1: String::new() }, 0)
    }

    fn trashed(id: &str) -> Undo {
        Undo::Trashed { pid: id.into(), title: id.into(), dir: PathBuf::new(), entry: String::new() }
    }

    #[test]
    fn removals_undo_newest_first_and_are_bounded() {
        let mut list = Vec::new();
        for i in 0..UNDO_KEPT + 3 {
            keep_undo(&mut list, removed(&i.to_string()));
        }
        assert_eq!(list.len(), UNDO_KEPT);
        assert_eq!(list.last().map(Undo::pid), Some("22"));
        assert_eq!(list.first().map(Undo::pid), Some("3"));
    }

    #[test]
    fn adding_a_project_forgets_its_removal() {
        let mut list = vec![removed("a"), trashed("b"), removed("c")];
        forget_undo(&mut list, &["b".to_owned(), "x".to_owned()]);
        assert_eq!(list.iter().map(Undo::pid).collect::<Vec<_>>(), ["a", "c"]);
        forget_undo(&mut list, &["a".to_owned(), "c".to_owned()]);
        assert!(list.is_empty());
    }

    #[test]
    fn the_undo_label_names_what_it_undoes() {
        assert_eq!(removed("Sodium").label(), trf("Entfernen von %@ rückgängig", &[&"Sodium"]));
        assert_eq!(trashed("Fresh").label(), trf("Entfernen von %@ rückgängig", &[&"Fresh"]));
    }

    fn mod_ref(project: &str, version: &str) -> ModRef {
        ModRef { project_id: project.into(), version_id: version.into(), title: project.into(), file_name: String::new(), url: String::new(), sha1: String::new() }
    }

    #[test]
    fn switching_replaces_the_mod_in_place_and_adds_only_missing_dependencies() {
        let mut list = vec![mod_ref("lib", "l1"), mod_ref("a", "a1"), mod_ref("z", "z1")];
        let found = vec![mod_ref("a", "a2"), mod_ref("lib", "l2"), mod_ref("new", "n1")];
        let merged = merge_version(&mut list, found, "a");
        assert_eq!(list.iter().map(|m| m.version_id.as_str()).collect::<Vec<_>>(), ["l1", "a2", "z1", "n1"]);
        assert_eq!(merged.previous.map(|(m, at)| (m.version_id, at)), Some(("a1".to_owned(), 1)));
        assert_eq!(merged.extra, ["new"]);
        assert_eq!(merged.put, ["a", "new"]);
    }

    #[test]
    fn a_plain_add_of_a_version_keeps_no_previous_one() {
        let mut list = vec![mod_ref("z", "z1")];
        let merged = merge_version(&mut list, vec![mod_ref("a", "a2"), mod_ref("lib", "l1")], "a");
        assert!(merged.previous.is_none());
        assert_eq!(merged.extra, ["lib"]);
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn the_switch_undo_names_the_version_switched_to() {
        let switched = Undo::Switched { pid: "a".into(), title: "Sodium".into(), number: "2.0".into(), extra: Vec::new(), back: Back::Mod { previous: mod_ref("a", "a1"), at: 0, added: Vec::new() } };
        assert_eq!(switched.label(), trf("Wechsel zu Version %@ rückgängig", &[&"2.0"]));
        assert_eq!((switched.pid(), switched.title()), ("a", "Sodium"));
        let mut list = vec![switched];
        forget_undo(&mut list, &["a".to_owned()]);
        assert!(list.is_empty());
    }

    #[test]
    fn needing_a_dependency_a_switch_added_forgets_that_switch() {
        let switched = Undo::Switched { pid: "a".into(), title: "A".into(), number: "2.0".into(), extra: vec!["Lib".into()], back: Back::Mod { previous: mod_ref("a", "a1"), at: 0, added: vec!["lib".into()] } };
        let mut list = vec![switched, removed("c")];
        forget_undo(&mut list, &["b".to_owned()]);
        assert_eq!(list.len(), 2);
        forget_undo(&mut list, &["b".to_owned(), "lib".to_owned()]);
        assert_eq!(list.iter().map(Undo::pid).collect::<Vec<_>>(), ["c"]);
    }

    #[test]
    fn the_switch_notice_names_the_dependencies_it_added() {
        assert_eq!(switched_text("Sodium", "2.0", &[]), trf("„%@“ ist jetzt Version %@.", &[&"Sodium", &"2.0"]));
        assert_eq!(switched_text("Sodium", "2.0", &["Lib".to_owned(), "Api".to_owned()]), trf("„%@“ ist jetzt Version %@. Auch hinzugefügt: %@.", &[&"Sodium", &"2.0", &"Lib, Api"]));
    }

    #[test]
    fn the_version_cursor_starts_on_the_first_row_and_stays_inside() {
        assert_eq!(stepped(None, 5, 1), 0);
        assert_eq!(stepped(None, 5, -1), 0);
        assert_eq!(stepped(None, 5, i32::MAX), 4);
        assert_eq!(stepped(Some(2), 5, 1), 3);
        assert_eq!(stepped(Some(2), 5, -9), 0);
        assert_eq!(stepped(Some(4), 5, 1), 4);
        assert_eq!(stepped(None, 0, i32::MAX), 0);
    }

    #[test]
    fn a_version_waits_for_what_blocks_installing() {
        let mut f = facts(Kind::Mod);
        assert_eq!(version_blocked(&f), None);
        f.saving = true;
        assert_eq!(version_blocked(&f), Some(tr("Eine andere Änderung läuft noch.")));
        f.saving = false;
        f.takes_mods = false;
        assert_eq!(version_blocked(&f), Some(trf("%@ lädt keine Mods", &[&"Fabric"])));
        assert_eq!(fit_name(&f), "Minecraft 1.21 · Fabric");
        let mut packs = facts(Kind::ResourcePack);
        packs.pack = Pack::Loading;
        assert!(version_blocked(&packs).is_some());
    }

    #[test]
    fn stepping_clamps_to_the_rows_shown() {
        assert_eq!(clamp_step(3, 10, 1), 4);
        assert_eq!(clamp_step(3, 10, -9), 0);
        assert_eq!(clamp_step(3, 10, i32::MIN), 0);
        assert_eq!(clamp_step(3, 10, i32::MAX), 9);
        assert_eq!(clamp_step(0, 0, 1), 0);
    }

    #[test]
    fn more_is_asked_for_once_the_end_is_near_and_nothing_loads() {
        assert!(wants_more(20, 17, true, More::Idle));
        assert!(!wants_more(20, 10, true, More::Idle));
        assert!(!wants_more(20, 19, false, More::Idle));
        assert!(!wants_more(20, 19, true, More::Loading));
        assert!(!wants_more(20, 19, true, More::Failed));
    }

    #[test]
    fn the_last_page_stays_dimmed_while_the_next_one_is_awaited() {
        assert_eq!(shown_of(true, false, false, &Status::Idle), Shown::List { dim: true });
        assert_eq!(shown_of(true, false, false, &Status::Loading), Shown::List { dim: true });
        assert_eq!(shown_of(true, false, true, &Status::Idle), Shown::List { dim: false });
        assert_eq!(shown_of(false, true, false, &Status::Idle), Shown::Skeleton);
        assert_eq!(shown_of(false, true, false, &Status::Loading), Shown::Skeleton);
        assert_eq!(shown_of(false, true, false, &Status::Failed(Problem::plain("x"))), Shown::Failed);
        assert_eq!(shown_of(true, true, true, &Status::Idle), Shown::Empty);
        assert_eq!(shown_of(true, true, false, &Status::Idle), Shown::Empty);
    }

    #[test]
    fn icon_fetches_stay_bounded_until_the_thread_returns() {
        let taken: Vec<Slot> = (0..DECODERS).filter_map(|_| Slot::try_take()).collect();
        assert_eq!(taken.len(), DECODERS);
        assert!(Slot::try_take().is_none());
        let mut taken = taken;
        let last = taken.pop();
        std::thread::spawn(move || drop(last)).join().unwrap();
        let again = Slot::try_take();
        assert!(again.is_some());
        assert!(Slot::try_take().is_none());
    }
}
