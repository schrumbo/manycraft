//! The page of one instance: its tabs on the left, and on the right what runs, how to play it and what to do with it.

pub(crate) mod actions;
mod crash;
mod cursor;
mod detail;
mod entry;
mod icon;
mod inspector;
mod logs;
mod logs_lines;
mod logs_select;
mod logs_viewer;
pub(crate) mod memory;
mod mods;
mod packlist;
mod play;
mod shots;
mod shots_parts;
mod shots_thumbs;
mod shots_view;
mod worlds;
pub mod links;
mod save;
mod settings;
pub(crate) mod snapshots;
mod title;

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use freya::prelude::*;
use uuid::Uuid;

pub use self::icon::InstanceIcon;
pub use self::play::{Hero, Mini, play_now};
pub use self::title::{Tabs, Title};
use self::inspector::Strip;
use super::commands::{Extra, Shortcut};
use super::icons::{Icon, icon};
use super::kit::button::Button;
use super::kit::text::{heading, secondary};
use super::kit::{after, page, use_theme};
use super::store::{Store, use_store};
use super::theme::{frame, iconsize, space};
use super::ui::{Filter, Page};
use crate::app::event::Event;
use crate::app::model::{Game, Model, Phase};
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Instance, Loader};
use crate::core::modrinth::Kind;


const TAB_DIGITS: [Code; 7] = [Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4, Code::Digit5, Code::Digit6, Code::Digit7];

/// The tabs of the instance page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Mods,
    Packs,
    Shaders,
    Worlds,
    Shots,
    Logs,
    Settings,
}

impl Tab {
    /// The tabs of an instance in order; shader packs need a loader that takes mods.
    pub fn shown(takes_mods: bool) -> Vec<Tab> {
        [Tab::Mods, Tab::Packs, Tab::Shaders, Tab::Worlds, Tab::Shots, Tab::Logs, Tab::Settings].into_iter().filter(|t| takes_mods || *t != Tab::Shaders).collect()
    }

    /// Whether the tab is a form or prose that keeps a readable measure; the others use the whole width.
    fn reads(self) -> bool {
        self == Tab::Settings
    }

    /// The id the drive tool and spots use.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Mods => "mods",
            Tab::Packs => "resourcepacks",
            Tab::Shaders => "shaders",
            Tab::Worlds => "worlds",
            Tab::Shots => "screenshots",
            Tab::Logs => "logs",
            Tab::Settings => "settings",
        }
    }

    pub fn label(self) -> String {
        match self {
            Tab::Mods => tr("Mods"),
            Tab::Packs => tr("Ressourcenpakete"),
            Tab::Shaders => tr("Shader"),
            Tab::Worlds => tr("Welten"),
            Tab::Shots => tr("Screenshots"),
            Tab::Logs => tr("Protokolle"),
            Tab::Settings => tr("Einstellungen"),
        }
    }

    fn glyph(self) -> Icon {
        match self {
            Tab::Mods => Icon::Layers,
            Tab::Packs => Icon::Palette,
            Tab::Shaders => Icon::Sparkles,
            Tab::Worlds => Icon::Grid,
            Tab::Shots => Icon::Fit,
            Tab::Logs => Icon::Rows,
            Tab::Settings => Icon::Settings,
        }
    }
}

/// Work on an instance that holds back other changes, with how far it got.
#[derive(Clone, Debug, PartialEq)]
pub struct Busy {
    pub text: String,
    pub progress: Option<f32>,
}

/// What the instance page keeps while the person looks elsewhere.
#[derive(Clone, Copy, PartialEq)]
pub struct Nav {
    pub tab: State<Tab>,
    /// The instance `tab` belongs to, and the tab each instance showed last.
    tab_for: State<Option<String>>,
    tabs: State<HashMap<String, Tab>>,
    /// The instance whose name is being edited.
    pub renaming: State<Option<String>>,
    pub rename: State<String>,
    pub rename_error: State<Option<String>>,
    /// Per instance: work that holds back changes, like a switch or a backup.
    pub busy: State<HashMap<String, Busy>>,
    /// Per instance: why its last change was not saved.
    pub error: State<HashMap<String, String>>,
    /// Per instance: what its last switch left out.
    pub notes: State<HashMap<String, Vec<String>>>,
    /// When each running game was first seen running, by key.
    pub up: State<HashMap<String, Instant>>,
    pub advanced: State<bool>,
    /// Per instance: when Play was last pressed, so a second press right after is ignored.
    pub played: State<HashMap<String, Instant>>,
    /// Counts changes to instance files on disk, so tabs read them again.
    pub changed: State<u64>,
    /// What the shown tab adds to the palette, such as commands on its selection; set through [`use_extras`].
    pub tab_extras: State<Vec<Extra>>,
    queue: State<VecDeque<(String, save::Job)>>,
    saving: State<Option<(String, save::Job)>>,
    java_picking: State<HashSet<String>>,
    java_problem: State<HashMap<String, String>>,
}

impl Nav {
    /// Fresh page state.
    pub fn create() -> Self {
        Self {
            tab: State::create(Tab::Mods),
            tab_for: State::create(None),
            tabs: State::create(HashMap::new()),
            renaming: State::create(None),
            rename: State::create(String::new()),
            rename_error: State::create(None),
            busy: State::create(HashMap::new()),
            error: State::create(HashMap::new()),
            notes: State::create(HashMap::new()),
            up: State::create(HashMap::new()),
            advanced: State::create(false),
            played: State::create(HashMap::new()),
            changed: State::create(0),
            tab_extras: State::create(Vec::new()),
            queue: State::create(VecDeque::new()),
            saving: State::create(None),
            java_picking: State::create(HashSet::new()),
            java_problem: State::create(HashMap::new()),
        }
    }
}

/// What the page shows of an instance, read from the model in one go.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub made: Instance,
    pub batches: Vec<(String, String)>,
    pub accounts: Vec<(Uuid, String)>,
    /// The account that plays it alone next.
    pub solo: Option<Uuid>,
    /// Whether `solo` is the instance's own default rather than the active account.
    pub pinned: bool,
    /// Whether any game of it runs, alone or in a batch.
    pub running: bool,
    /// Accounts whose game of it runs alone.
    pub playing: Vec<Uuid>,
    /// Running games played alone that can be stopped, by key and name.
    pub stoppable: Vec<(String, String)>,
    /// The game of it played alone that is being stopped, and whether it took too long so it can be ended at once.
    pub stopping: Option<(String, bool)>,
    /// What went wrong with its last game played alone, while none runs.
    pub trouble: Option<String>,
    /// Which end its last game played alone came to, while none runs.
    pub ended: Option<Ended>,
    /// Games played alone that are on their way: no window yet, or waiting for a restart; by key.
    pub launching: Vec<String>,
    /// How far Minecraft's files are installed for the game on its way, while they are.
    pub loading: Option<u8>,
    /// What the games played alone that run hold in memory, in MB; 0 before the first reading.
    pub memory_mb: u64,
    /// Accounts that have to sign in again before they can play.
    pub expired: Vec<Uuid>,
    /// Accounts whose game of it runs in a batch, with the batch's name; they cannot play it alone now.
    pub busy: Vec<(Uuid, String)>,
    /// The account Play starts: `solo`, or the first free one while `solo` is busy or has to sign in again.
    pub pick: Option<Uuid>,
}

/// How a game played alone ended without the person stopping it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Crashed,
    Failed,
    SignIn,
}

impl Facts {
    /// Where `account` plays this instance in a batch, if it does.
    pub fn busy_in(&self, account: Uuid) -> Option<&str> {
        self.busy.iter().find(|(a, _)| *a == account).map(|(_, place)| place.as_str())
    }

    /// The name of `account`.
    pub fn name_of(&self, account: Uuid) -> Option<&str> {
        self.accounts.iter().find(|(a, _)| *a == account).map(|(_, n)| n.as_str())
    }

    /// Whether every account is busy or already plays it alone, so none can start it.
    pub fn none_free(&self) -> bool {
        !self.accounts.is_empty() && self.accounts.iter().all(|(a, _)| self.busy_in(*a).is_some() || self.playing.contains(a))
    }
}

/// The facts of instance `id`, when it exists.
pub fn facts(m: &Model, id: &str) -> Option<Facts> {
    let made = m.instance(id)?.clone();
    let batches = m.batches.iter().filter(|b| b.instance_id == made.id).map(|b| (b.id.clone(), b.name.clone())).collect();
    let accounts = m.accounts.iter().map(|a| (a.id, a.name.clone())).collect();
    let live: Vec<&Game> = m.solo_games(&made.id).filter(|g| g.phase.is_active()).collect();
    let mut stoppable: Vec<(String, String)> = live.iter().filter(|g| !g.stopping).map(|g| (g.key.clone(), g.name.clone())).collect();
    stoppable.sort_by_key(|(_, name)| name.to_lowercase());
    let playing: Vec<Uuid> = live.iter().map(|g| g.account_id).collect();
    let stopping = live.iter().find(|g| g.stopping).map(|g| (g.key.clone(), g.overdue));
    let solo = crate::app::reduce::solo_account(m, &made);
    let pinned = made.default_account.is_some() && made.default_account == solo;
    let trouble = if live.is_empty() {
        let mut ended: Vec<&Game> = m.solo_games(&made.id).filter(|g| g.phase.needs_attention()).collect();
        ended.sort_by_key(|g| Some(g.account_id) != solo);
        ended.first().map(|g| crate::app::model::status(g, None))
    } else {
        None
    };
    let ended = if live.is_empty() {
        m.solo_games(&made.id).find(|g| Some(g.account_id) == solo && g.phase.needs_attention()).and_then(|g| match g.phase {
            Phase::Crashed { .. } => Some(Ended::Crashed),
            Phase::Failed { .. } => Some(Ended::Failed),
            Phase::LoginNeeded => Some(Ended::SignIn),
            _ => None,
        })
    } else {
        None
    };
    let launching: Vec<String> = live.iter().filter(|g| !g.stopping && matches!(g.phase, Phase::Starting { .. } | Phase::Restarting { .. })).map(|g| g.key.clone()).collect();
    let memory_mb = live.iter().filter_map(|g| m.session.usage.get(&g.key)).sum();
    let loading = loading(m, &made.id);
    let running = m.games.values().any(|g| g.instance_id == made.id && g.phase.is_active());
    let expired: Vec<Uuid> = m.accounts.iter().filter(|a| m.sign_in_expired(a.id)).map(|a| a.id).collect();
    let busy: Vec<(Uuid, String)> = m.accounts.iter().filter_map(|a| m.playing_elsewhere(&made.id, a.id, &crate::core::fleet::solo_key(&made, a.id)).map(|place| (a.id, place))).collect();
    let pick = crate::app::reduce::solo_pick(m, &made);
    Some(Facts { made, batches, accounts, solo, pinned, running, playing, stoppable, stopping, trouble, ended, launching, loading, memory_mb, expired, busy, pick })
}

/// Why changes to the instance wait: work on it, or games that run.
pub fn blocked(store: Store, id: &str) -> Option<String> {
    if store.ui.instance.busy.peek().contains_key(id) {
        return Some(tr("Warte, bis die Umstellung fertig ist."));
    }
    store.model.peek().games.values().any(|g| g.instance_id == id && g.phase.is_active()).then(|| tr("Stoppe zuerst die laufenden Spiele."))
}

/// Why the browser and the mods sheet wait on instance `id`: work on it, such as a restore, that holds it.
pub fn held(store: Store, id: &str) -> Option<String> {
    store.ui.instance.busy.read().get(id).map(|b| b.text.clone())
}

/// Draws again each full minute of the oldest running game, or of the oldest played alone when `alone`, so uptimes
/// stay current.
pub fn use_minute(store: Store, alone: bool) {
    let mut tick = use_state(|| 0u32);
    let _ = tick.read();
    use_hook(move || {
        spawn(async move {
            loop {
                let up = store.ui.instance.up;
                let since = {
                    let m = store.model.peek();
                    up.peek().iter().filter(|(k, _)| !alone || m.games.get(*k).is_some_and(Game::alone)).map(|(_, at)| *at).min()
                };
                let wait = since.map_or(60, |s: Instant| 60 - s.elapsed().as_secs() % 60);
                after(Duration::from_secs(wait) + Duration::from_millis(50)).await;
                if since.is_some() {
                    let next = tick.peek().wrapping_add(1);
                    tick.set(next);
                }
            }
        })
    });
}

/// "12 Min." under an hour, else "1:05 h", since `since`; nothing in the first minute.
pub fn uptime_text(since: Instant) -> Option<String> {
    let minutes = since.elapsed().as_secs() / 60;
    Some(match (minutes / 60, minutes % 60) {
        (0, 0) => return None,
        (0, m) => trf("%lld Min.", &[&m]),
        (h, m) => trf("%@ h", &[&format!("{h}:{m:02}")]),
    })
}

/// "Läuft seit 12 Min. · 1,8 GB" for the games of the instance played alone that run, since `since`; nothing while
/// none has a window.
pub fn running_line(f: &Facts, since: Option<Instant>) -> Option<String> {
    let running = f.stoppable.iter().filter(|(k, _)| !f.launching.contains(k)).count();
    let head = match (running, since.and_then(uptime_text)) {
        (0, _) => return None,
        (1, None) => tr("Läuft"),
        (1, Some(up)) => trf("Läuft seit %@", &[&up]),
        (n, None) => trf("%lld laufen", &[&n]),
        (n, Some(up)) => trf("%1$lld laufen seit %2$@", &[&n, &up]),
    };
    Some(if f.memory_mb > 0 { format!("{head} · {}", crate::ui::forms::memory_label(u32::try_from(f.memory_mb).unwrap_or(u32::MAX))) } else { head })
}

/// "Minecraft 26.3 · Fabric · 12 Mods" for the toolbar, without what the name already says.
pub fn subtitle(made: &Instance) -> String {
    let mut parts = Vec::new();
    if !made.name.contains(&made.game_version) {
        parts.push(trf("Minecraft %@", &[&made.game_version]));
    }
    if made.loader != Loader::Vanilla {
        parts.push(made.loader.name().to_owned());
    }
    if made.loader.takes_mods() {
        match made.mods.len() + made.local_mods.len() {
            0 => {}
            1 => parts.push(tr("1 Mod")),
            n => parts.push(trf("%lld Mods", &[&n])),
        }
    }
    parts.join(" · ")
}

/// "26.3-rc1 · Vorabversion" for the version pickers; a release is just its id.
pub fn version_text(v: &crate::core::versions::GameVersion) -> String {
    crate::ui::versions::kind_label(v.kind).map_or_else(|| v.id.clone(), |k| format!("{} · {k}", v.id))
}

/// Notes when each running game started, so the page can say how long it runs. Lives in the shell, so leaving the
/// page keeps the times.
#[derive(PartialEq)]
pub struct Uptime;

impl Component for Uptime {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut up = store.ui.instance.up;
        let running: Vec<String> = store.model.read().games.values().filter(|g| g.phase.is_running() && !matches!(g.phase, Phase::Starting { .. })).map(|g| g.key.clone()).collect();
        use_side_effect_with_deps(&running, move |running| {
            let now = Instant::now();
            let fresh = up.peek().len() != running.len() || running.iter().any(|k| !up.peek().contains_key(k));
            if fresh {
                let mut map = up.write();
                map.retain(|k, _| running.contains(k));
                for key in running {
                    map.entry(key.clone()).or_insert(now);
                }
            }
        });
        rect()
    }
}

/// The instance page.
#[derive(PartialEq)]
pub struct InstancePage {
    pub id: String,
}

impl Component for InstancePage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let takes_mods = store.model.read().instance(&self.id).is_some_and(|i| i.loader.takes_mods());
        let tab = use_remembered_tab(store, &self.id, takes_mods);
        let m = store.model.read();
        let Some(f) = facts(&m, &self.id) else {
            return gone(&t, store, m.loaded).into_element();
        };
        let room = ui.window.read().0 - ui.sidebar_room();
        let body = tab_body(&t, store, tab, f.clone());
        let body = if tab.reads() { page::reading(body).height(Size::fill()) } else { rect().width(Size::fill()).height(Size::fill()).child(body) };
        let content = rect().width(Size::fill()).height(Size::flex(1.)).main_align(Alignment::Start).child(body);
        let mut page = rect().width(Size::fill()).height(Size::fill()).child(Strip { id: self.id.clone() });
        for n in inspector::notices(&t, store, &self.id) {
            page = page.child(rect().width(Size::fill()).padding((space::S, space::L, 0., space::L)).child(n));
        }
        if room < frame::TOOLBAR_TABS {
            page = page.child(rect().width(Size::fill()).padding((space::S, space::L, 0., space::L)).cross_align(Alignment::Center).child(Tabs));
        }
        page.child(content).content(Content::flex()).into_element()
    }
}

fn tab_entry(tab: Tab, name: &str) -> String {
    match tab {
        Tab::Mods => trf("Mods von %@", &[&name]),
        Tab::Packs => trf("Ressourcenpakete von %@", &[&name]),
        Tab::Shaders => trf("Shader von %@", &[&name]),
        Tab::Worlds => trf("Welten von %@", &[&name]),
        Tab::Shots => trf("Screenshots von %@", &[&name]),
        Tab::Logs => trf("Protokolle von %@", &[&name]),
        Tab::Settings => trf("Einstellungen von %@", &[&name]),
    }
}

fn tab_body(t: &super::theme::Theme, store: Store, tab: Tab, f: Facts) -> Element {
    match tab {
        Tab::Mods if !f.made.loader.takes_mods() => {
            let mut tab = store.ui.instance.tab;
            rect()
                .width(Size::fill())
                .height(Size::fill())
                .center()
                .spacing(space::M)
                .child(icon(t, Icon::Layers, iconsize::HERO, t.text3))
                .child(heading(t, tr("Keine Mods")))
                .child(secondary(t, tr("Vanilla lädt keine Mods.")))
                .child(Button::new("instance.mods.loader", tr("Loader wählen")).on_press(move |_| tab.set(Tab::Settings)))
                .into_element()
        }
        Tab::Mods => mods::Mods { facts: f }.into_element(),
        Tab::Packs => rect().key("packs").child(packlist::Packs { facts: f, kind: Kind::ResourcePack }).into_element(),
        Tab::Shaders => rect().key("shaders").child(packlist::Packs { facts: f, kind: Kind::Shader }).into_element(),
        Tab::Worlds => worlds::Worlds { facts: f }.into_element(),
        Tab::Shots => shots::Shots { facts: f }.into_element(),
        Tab::Logs => logs::Logs { facts: f }.into_element(),
        Tab::Settings => settings::Settings { facts: f }.into_element(),
    }
}

fn moving(t: &super::theme::Theme, tab: Tab) -> Element {
    rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .child(icon(t, tab.glyph(), iconsize::HERO, t.text3))
        .child(heading(t, tab.label()))
        .child(secondary(t, tr("Dieser Bereich zieht als Nächstes in die neue Oberfläche um.")))
        .into_element()
}

fn gone(t: &super::theme::Theme, store: Store, loaded: bool) -> impl IntoElement {
    let mut ui = store.ui;
    if !loaded {
        return rect().width(Size::fill()).height(Size::fill()).center().child(secondary(t, tr("Lädt …")));
    }
    rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .child(icon(t, Icon::Folder, iconsize::HERO, t.text3))
        .child(heading(t, tr("Diese Instanz gibt es nicht mehr.")))
        .child(Button::new("instance.gone.all", tr("Zu den Instanzen")).tall().on_press(move |_| ui.page.set(Page::Library)))
}

/// The tab the page of instance `id` shows: the one it showed last, at first Mods when it takes mods and Worlds when not.
pub fn shown_tab(store: Store, id: &str, takes_mods: bool) -> Tab {
    let nav = store.ui.instance;
    let now = *nav.tab.read();
    let tab = if nav.tab_for.read().as_deref() == Some(id) { now } else { nav.tabs.peek().get(id).copied().unwrap_or(if takes_mods { Tab::Mods } else { Tab::Worlds }) };
    if tab == Tab::Shaders && !takes_mods { Tab::Worlds } else { tab }
}

fn use_remembered_tab(store: Store, id: &str, takes_mods: bool) -> Tab {
    let tab = shown_tab(store, id, takes_mods);
    let nav = store.ui.instance;
    use_side_effect_with_deps(&(id.to_owned(), tab), move |(id, tab)| {
        let (mut now, mut tab_for, mut tabs) = (nav.tab, nav.tab_for, nav.tabs);
        if tab_for.peek().as_deref() != Some(id.as_str()) {
            tab_for.set(Some(id.clone()));
            now.set_if_modified(*tab);
        }
        tabs.write().insert(id.clone(), *tab);
    });
    tab
}

/// How far the game of instance `id` played alone has loaded Minecraft before its start, while it does.
pub fn loading(m: &crate::app::model::Model, id: &str) -> Option<u8> {
    m.solo_games(id).filter(|g| matches!(g.phase, crate::app::model::Phase::Starting { .. }) && g.memory_wait.is_none()).find_map(|g| m.preparing.get(&g.batch_id).copied())
}

/// Shows the page of instance `id` on `tab`.
pub fn show_on(store: Store, id: &str, tab: Tab) {
    let nav = store.ui.instance;
    let (mut now, mut tab_for, mut tabs) = (nav.tab, nav.tab_for, nav.tabs);
    tabs.write().insert(id.to_owned(), tab);
    tab_for.set(Some(id.to_owned()));
    now.set(tab);
    let mut page = store.ui.page;
    page.set(Page::Instance(id.to_owned()));
}

/// The instance the page shows, if it shows one.
pub fn current(store: Store) -> Option<String> {
    match &*store.ui.page.peek() {
        Page::Instance(id) => store.model.peek().instance(id).map(|i| i.id.clone()),
        _ => None,
    }
}

/// What the instance page offers beyond the window's commands: in the palette, and through shortcuts while it is shown.
pub fn extras(store: Store) -> Vec<Extra> {
    let Some(id) = current(store) else { return Vec::new() };
    let Some(f) = facts(&store.model.peek(), &id) else { return Vec::new() };
    let mut list = Vec::new();
    let name = f.made.name.clone();
    let mut push = |key: &str, label: String, glyph: Icon, shortcut: Option<Shortcut>, run: EventHandler<()>| {
        list.push(Extra { id: format!("instance.{key}"), label, glyph, shortcut, run });
    };
    for (n, tab) in Tab::shown(f.made.loader.takes_mods()).into_iter().enumerate() {
        let mut nav = store.ui.instance.tab;
        let code = TAB_DIGITS[n];
        push(&format!("tab.{}", tab.key()), tab_entry(tab, &name), tab.glyph(), Some(Shortcut::alt(code)), EventHandler::new(move |_| nav.set(tab)));
    }
    let rename_id = id.clone();
    push("rename", trf("%@ umbenennen…", &[&name]), Icon::Settings, Some(Shortcut::bare(Code::F2)), EventHandler::new(move |_| actions::begin_rename(store, &rename_id)));
    let play_key = Some(Shortcut::primary(Code::Enter));
    let pick = play::pick(&f).filter(|_| f.stoppable.is_empty() && f.stopping.is_none());
    if let Some((key, _)) = f.stoppable.iter().find(|(k, _)| !f.launching.contains(k)).cloned().filter(|_| crate::core::game_window::CAN_RAISE) {
        push("raise", tr("Zum Spiel"), Icon::External, play_key, EventHandler::new(move |_| store.dispatch(Event::Raise(key.clone()))));
    }
    for (account, who) in f.accounts.iter().filter(|(a, _)| !f.playing.contains(a) && f.busy_in(*a).is_none()) {
        let (instance, account) = (id.clone(), *account);
        let first = pick == Some(account);
        if f.expired.contains(&account) {
            push(&format!("play.{account}"), trf("Anmelden als %@", &[who]), Icon::LogIn, play_key.filter(|_| first), EventHandler::new(move |_| crate::view::accounts::sign_in(store, vec![account])));
        } else {
            push(&format!("play.{account}"), trf("Spielen als %@", &[who]), Icon::Play, play_key.filter(|_| first), EventHandler::new(move |_| play::play_now(store, &instance, Some(account))));
        }
    }
    if let Some(solo) = f.solo.filter(|_| f.accounts.len() > 1) {
        let instance = id.clone();
        let (key, label, account) = if f.pinned { ("default.clear", tr("Standard-Account verwenden"), None) } else { ("default.set", trf("%@ für diese Instanz festlegen", &[&f.name_of(solo).unwrap_or_default()]), Some(solo)) };
        push(key, label, Icon::Users, None, EventHandler::new(move |_| store.dispatch(Event::SetDefaultAccount { instance: instance.clone(), account })));
    }
    let new = links::new_batch(store, &id, &name);
    if new.disabled.is_none() {
        push(new.key, new.long, new.glyph, None, new.run);
    }
    for (batch, label) in &f.batches {
        let batch = batch.clone();
        push(&format!("batch.start.{batch}"), trf("Batch „%@“ starten", &[label]), Icon::Layers, None, EventHandler::new(move |_| store.dispatch(Event::StartBatch(batch.clone()))));
    }
    if let Some((key, true)) = f.stopping.clone() {
        push("kill", tr("Sofort beenden"), Icon::Stop, Some(Shortcut::primary(Code::Period)), EventHandler::new(move |_| store.dispatch(Event::Stop(vec![key.clone()]))));
    } else if let Some((key, _)) = f.stoppable.first().cloned() {
        push("stop", tr("Stoppen"), Icon::Stop, Some(Shortcut::primary(Code::Period)), EventHandler::new(move |_| store.dispatch(Event::Stop(vec![key.clone()]))));
    }
    if f.stoppable.len() > 1 {
        let keys: Vec<String> = f.stoppable.iter().map(|(k, _)| k.clone()).collect();
        push("stop.all", trf("Alle Spiele von %@ stoppen", &[&name]), Icon::Stop, None, EventHandler::new(move |_| store.dispatch(Event::Stop(keys.clone()))));
    }
    for item in actions::all(store, &f).into_iter().filter(|a| a.disabled.is_none()) {
        push(item.key, item.long, item.glyph, None, item.run);
    }
    list.extend(store.ui.instance.tab_extras.peek().iter().cloned());
    list
}

/// Offers `extras` in the palette, and their shortcuts, while the calling tab is shown.
pub fn use_extras(extras: Vec<Extra>) {
    let store = use_store();
    let mut tab_extras = store.ui.instance.tab_extras;
    let ids: Vec<(String, String, Option<Shortcut>)> = extras.iter().map(|x| (x.id.clone(), x.label.clone(), x.shortcut)).collect();
    let mut latest = use_state(Vec::<Extra>::new);
    latest.set_if_modified(extras);
    use_side_effect_with_deps(&ids, move |_| tab_extras.set(latest.peek().clone()));
    use_drop(move || tab_extras.set(Vec::new()));
}

#[cfg(test)]
mod tests {
    use super::{Ended, Facts, running_line, version_text};
    use crate::core::versions::{GameVersion, Kind};

    fn facts(stoppable: &[&str], launching: &[&str], memory_mb: u64) -> Facts {
        Facts {
            made: crate::core::instance::Instance::new("A", "26.3", crate::core::instance::Loader::Vanilla),
            batches: Vec::new(),
            accounts: Vec::new(),
            solo: None,
            pinned: false,
            running: true,
            playing: Vec::new(),
            stoppable: stoppable.iter().map(|k| ((*k).to_owned(), String::new())).collect(),
            stopping: None,
            trouble: None,
            ended: None::<Ended>,
            launching: launching.iter().map(|k| (*k).to_owned()).collect(),
            loading: None,
            memory_mb,
            expired: Vec::new(),
            busy: Vec::new(),
            pick: None,
        }
    }

    #[test]
    fn the_running_line_names_uptime_and_memory_only_for_games_with_a_window() {
        assert_eq!(running_line(&facts(&[], &[], 0), None), None);
        assert_eq!(running_line(&facts(&["k"], &["k"], 900), None), None);
        assert_eq!(running_line(&facts(&["k"], &[], 0), None), Some(crate::core::i18n::tr("Läuft")));
        assert!(running_line(&facts(&["k"], &[], 1843), None).is_some_and(|t| t.ends_with(" GB") && t.contains(" · ")));
        assert!(running_line(&facts(&["a", "b"], &[], 0), None).is_some_and(|t| t.contains('2')));
    }

    #[test]
    fn every_kind_has_one_name_in_every_picker() {
        let text = |id: &str, kind| version_text(&GameVersion { id: id.into(), kind });
        assert_eq!(text("26.3", Kind::Release), "26.3");
        assert_eq!(text("26.4-snapshot-1", Kind::Snapshot), "26.4-snapshot-1 · Snapshot");
        assert_eq!(text("26.3-rc1", Kind::PreRelease), "26.3-rc1 · Vorabversion");
        assert_eq!(text("b1.7.3", Kind::OldBeta), "b1.7.3 · Alte Beta");
        assert_eq!(text("a1.2.6", Kind::OldAlpha), "a1.2.6 · Alte Alpha");
    }
}