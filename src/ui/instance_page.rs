//! The page of one instance: what it plays, its batches, mods and settings.

use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::base::input::Escape;
use gpui_kit::component::{FocusableExt, IndexPath, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{AddAccount, AddMods, CloneInstance, DeleteInstance, NewBatchFor, OpenInstanceFolder, OpenInstanceSetting, Play, PlayInstance, Reload, RenameCurrent, RenameInstance, ResetInstance, ShowAll, ShowBatch, ShowInstance, StartBatch};
use super::batch_sheet::Load;
use super::drive::Mark;
use super::forms::{self, Problem};
use super::loader_version::{self, LoaderVersion, Picked};
use super::model::{self, Game, Phase};
use super::motion::Soft;
use super::remote::{self, InstanceEdit};
use super::sheet::{self, Look};
use super::undo::Edit;
use super::versions::{self as picker, VersionItem};
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tile, tint};
use super::toolbar;
use super::widgets::{KeyNav, TakeFocus, icon, segment, segmented, tip};
use crate::core::batch::ModRef;
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Instance, LOADERS, Loader};
use crate::core::options::check_jvm_args;
use crate::core::versions::GameVersion;

#[path = "instance_options.rs"]
mod options;

use options::{OptionChange, Options};

const PAGE_MAX: f32 = 760.;
const VERSION_WIDTH: f32 = 168.;
const ICON_TILE: f32 = control::L + space::XS;

/// Registers the instance page's delete confirmation and the rename that starts from outside the page.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &DeleteInstance, cx| {
        let id = a.0.clone();
        sheet::in_window(cx, move |window, cx| ask_delete(&id, window, cx));
    });
    cx.on_action(|a: &RenameInstance, cx| {
        let id = a.0.clone();
        cx.defer(move |cx| rename(&id, cx));
    });
    cx.on_action(|a: &OpenInstanceSetting, cx| {
        let (id, setting) = (a.instance.clone(), a.setting.clone());
        cx.defer(move |cx| open_setting(&id, &setting, cx));
    });
    cx.bind_keys([KeyBinding::new("f2", RenameCurrent, Some(PAGE_CONTEXT))]);
}

/// Shows the page of instance `id` and starts renaming it there.
pub fn rename(id: &str, cx: &mut App) {
    cx.set_global(WantsRename(id.to_owned()));
    if !super::main_window::show_instance(id, cx) {
        cx.remove_global::<WantsRename>();
        return;
    }
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |_, cx| cx.notify());
    }
}

/// Shows the settings of instance `id` with "Erweitert" open and `setting` focused.
pub fn open_setting(id: &str, setting: &str, cx: &mut App) {
    cx.set_global(WantsSetting(id.to_owned(), setting.to_owned()));
    if !super::main_window::show_instance(id, cx) {
        cx.remove_global::<WantsSetting>();
        return;
    }
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |_, cx| cx.notify());
    }
}

struct WantsSetting(String, String);

impl Global for WantsSetting {}

/// Moves the keyboard to the instance page, where F2 renames.
pub fn focus(window: &mut Window, cx: &mut App) -> bool {
    let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) else { return false };
    let root = page.read(cx).root.clone();
    window.focus(&root, cx);
    true
}

struct WantsRename(String);

impl Global for WantsRename {}

fn ask_delete(id: &str, window: &mut Window, cx: &mut App) {
    if sheet::busy(window, cx) {
        return;
    }
    let (name, users) = {
        let m = model::get(cx).read(cx);
        let Some(made) = m.instance(id) else { return };
        let users: Vec<String> = m.batches.iter().filter(|b| b.instance_id == made.id).map(|b| b.name.clone()).collect();
        (made.name.clone(), users)
    };
    if !users.is_empty() {
        let (title, why) = (trf("„%@“ lässt sich nicht löschen.", &[&name]), crate::core::fleet::used_by(&users));
        let done = cx.focus_handle();
        let first = done.clone();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let rings = super::theme::rings(cx);
            alert.title(title.clone()).description(why.clone()).on_ok(sheet::pass_enter).footer(DialogFooter::new().child(Button::new("alert-done").label(tr("Fertig")).focus_ring(rings).track_focus(&done).on_click(|_, window, cx| window.close_dialog(cx))))
        });
        first.focus(window, cx);
        return;
    }
    let id = id.to_owned();
    sheet::confirm_danger(window, cx, trf("„%@“ löschen?", &[&name]), tr("Mods, Welten und Screenshots dieser Instanz werden gelöscht. Solange Manycraft offen ist, kannst du das rückgängig machen."), tr("Löschen"), |_| None, move |window, cx| {
        let next = after(&model::get(cx).read(cx).instances, &id);
        match next {
            Some(next) => window.dispatch_action(Box::new(ShowInstance(next)), cx),
            None => window.dispatch_action(Box::new(ShowAll), cx),
        }
        super::state::delete_instance(&id, cx);
    });
}

fn after(instances: &[Instance], id: &str) -> Option<String> {
    let mut order: Vec<&Instance> = instances.iter().collect();
    order.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    let at = order.iter().position(|made| made.id == id)?;
    order.get(at + 1).or_else(|| at.checked_sub(1).and_then(|before| order.get(before))).map(|made| made.id.clone())
}

/// Puts removed mod `mod_ref` back into instance `id` at `index`. `base` is the instance as it was right after the
/// removal, so a change of the mods, version or loader since then is refused for good instead of merged. Says why when
/// it cannot, and offers a retry in its old place only when that may help.
pub fn restore_mod(id: &str, mod_ref: ModRef, index: usize, base: InstanceEdit, at: Instant, cx: &mut App) {
    let again = |mod_ref: ModRef, base: InstanceEdit| super::undo::Kind::ModRemoved { instance_id: id.to_owned(), mod_ref, index, base };
    let (made, running) = {
        let m = model::get(cx).read(cx);
        (m.instance(id).cloned(), m.games.values().any(|g| g.instance_id == id && g.phase.is_active()))
    };
    let Some(made) = made else {
        super::undo::note(tr("Diese Instanz gibt es nicht mehr."), cx);
        super::state::reload(cx);
        return;
    };
    if made.mods.iter().any(|m| m.project_id == mod_ref.project_id) {
        super::undo::note(trf("„%@“ ist schon wieder da.", &[&mod_ref.title]), cx);
        super::state::reload(cx);
        return;
    }
    if made.game_version != base.game_version || !made.loader.same_kind(&base.loader) {
        super::undo::note(trf("„%@“ passt nicht mehr zu dieser Version.", &[&mod_ref.title]), cx);
        super::state::reload(cx);
        return;
    }
    if made.mods != base.mods {
        super::undo::note(trf("„%@“ lässt sich nicht mehr zurückholen: Die Mods wurden inzwischen geändert.", &[&mod_ref.title]), cx);
        super::state::reload(cx);
        return;
    }
    if running {
        super::undo::note(tr("Gerade laufen Spiele. Stoppe sie zuerst."), cx);
        super::undo::retry(again(mod_ref, base), at, cx);
        super::state::reload(cx);
        return;
    }
    let mut edit = base.clone();
    edit.mods.insert(index.min(edit.mods.len()), mod_ref.clone());
    let instance = id.to_owned();
    let task = cx.background_spawn({
        let (base, instance) = (base.clone(), instance.clone());
        async move { remote::save_instance(&instance, &base, edit) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(saved) => model::get(cx).update(cx, |m, cx| {
                    if let Some(slot) = m.instances.iter_mut().find(|made| made.id == saved.id) {
                        *slot = saved;
                    }
                    cx.notify();
                }),
                Err(e) => {
                    super::undo::note(forms::explain_as(tr("Nicht gespeichert."), &e).text, cx);
                    if !super::undo::permanent(&e) {
                        super::undo::retry(super::undo::Kind::ModRemoved { instance_id: instance, mod_ref, index, base }, at, cx);
                    }
                }
            }
            super::state::reload(cx);
        });
    })
    .detach();
}

const PAGE_CONTEXT: &str = "InstancePage";

struct Page(Entity<InstancePage>);

impl Global for Page {}

/// The page of instance `id`.
pub fn page(id: &str, window: &mut Window, cx: &mut App) -> AnyElement {
    let view = match cx.try_global::<Page>() {
        Some(page) => page.0.clone(),
        None => {
            let view = cx.new(|cx| InstancePage::new(id, window, cx));
            cx.set_global(Page(view.clone()));
            if !window.has_active_dialog(cx) {
                let root = view.read(cx).root.clone();
                window.focus(&root, cx);
            }
            view
        }
    };
    view.update(cx, |s, cx| s.bind(id, cx));
    view.into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Mods,
    Worlds,
    Shots,
    Logs,
    Settings,
}

const TABS: [Tab; 5] = [Tab::Mods, Tab::Worlds, Tab::Shots, Tab::Logs, Tab::Settings];

impl Tab {
    fn label(self) -> String {
        match self {
            Self::Mods => tr("Mods"),
            Self::Worlds => tr("Welten"),
            Self::Shots => tr("Screenshots"),
            Self::Logs => tr("Protokolle"),
            Self::Settings => tr("Einstellungen"),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Mods => "mods",
            Self::Worlds => "worlds",
            Self::Shots => "screenshots",
            Self::Logs => "logs",
            Self::Settings => "settings",
        }
    }
}

struct WantsTab(String, Tab);

impl Global for WantsTab {}

/// Shows the page of instance `id` on tab `key` ("worlds", "screenshots", "logs", "mods" or "settings").
pub fn show_tab(id: &str, key: &str, cx: &mut App) {
    let Some(tab) = TABS.into_iter().find(|t| t.key() == key) else { return };
    cx.set_global(WantsTab(id.to_owned(), tab));
    if !super::main_window::show_instance(id, cx) {
        cx.remove_global::<WantsTab>();
        return;
    }
    if let Some(page) = cx.try_global::<Page>().map(|p| p.0.clone()) {
        page.update(cx, |_, cx| cx.notify());
    }
}

/// The key of the tab the instance page shows, while one is shown.
pub fn tab_key(cx: &App) -> Option<&'static str> {
    cx.try_global::<Page>().map(|p| p.0.read(cx).tab.key())
}

#[derive(Clone, Debug, PartialEq)]
enum Change {
    Name(String),
    Memory(u32),
    JvmArgs(Vec<String>),
    LoaderVersion(Option<String>),
    RemoveMod(String),
    Option(OptionChange),
}

impl Change {
    fn apply(&self, edit: &mut InstanceEdit) {
        match self {
            Self::Name(name) => edit.name.clone_from(name),
            Self::Memory(mb) => edit.memory_mb = *mb,
            Self::JvmArgs(args) => edit.jvm_args.clone_from(args),
            Self::LoaderVersion(version) => edit.loader = edit.loader.with_version(version.clone()),
            Self::RemoveMod(project) => edit.mods.retain(|m| m.project_id != *project),
            Self::Option(change) => change.apply(&mut edit.options),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Switch {
    Version(String),
    Loader(Loader),
}

#[derive(Clone, Debug, PartialEq)]
enum Job {
    Save(Change),
    Switch(Switch),
}

struct Busy {
    id: String,
    text: String,
}

fn loader_key(made: &Instance) -> LoaderKey {
    (made.id.clone(), made.game_version.clone(), made.loader.name().to_owned(), made.loader.version().map(str::to_owned))
}

fn mods_after_loader(from: &Loader, to: &Loader, mods: Vec<ModRef>) -> Vec<ModRef> {
    let fabric_like = |l: &Loader| matches!(l, Loader::Fabric(_) | Loader::Quilt(_));
    if fabric_like(from) && fabric_like(to) { mods } else { Vec::new() }
}

fn split_args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

type LoaderKey = (String, String, String, Option<String>);

struct Facts {
    made: Instance,
    batches: Vec<(String, String)>,
    accounts: Vec<(Uuid, String)>,
    solo: Option<Uuid>,
    shown: Option<String>,
    running: bool,
    plain: Vec<Uuid>,
}

struct InstancePage {
    id: String,
    tab: Tab,
    entered: bool,
    advanced: bool,
    versions: Load<()>,
    version_list: Vec<GameVersion>,
    list_gen: u64,
    everything: bool,
    supported: Option<(String, Option<HashSet<String>>)>,
    supported_asked: Option<String>,
    version_select: Entity<SelectState<SearchableVec<VersionItem>>>,
    select_target: Option<String>,
    select_applied: Option<(String, u64, bool, bool)>,
    opts: Options,
    jvm: Entity<InputState>,
    loader_version: Entity<LoaderVersion>,
    name: Entity<InputState>,
    renaming: bool,
    rename_saving: bool,
    loader_focus: FocusHandle,
    focus_name: bool,
    removing: Option<(ModRef, usize)>,
    changing: Option<(InstanceEdit, Option<Edit>)>,
    loader_news: Option<(LoaderKey, Option<String>)>,
    loader_asked: Option<LoaderKey>,
    synced: Option<(String, String, String)>,
    queue: VecDeque<(String, Job)>,
    in_flight: Option<(String, Job)>,
    switching: Option<Busy>,
    notes: Vec<String>,
    error: Option<Problem>,
    rename_error: Option<Problem>,
    rename_error_for: String,
    details: HashSet<&'static str>,
    memory_max: u32,
    scroll: ScrollHandle,
    root: FocusHandle,
    _subs: Vec<Subscription>,
}

impl InstancePage {
    fn new(id: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let version_select = cx.new(|cx| SelectState::new(SearchableVec::new(Vec::<VersionItem>::new()), None, window, cx).searchable(true));
        let (opts, opts_subs) = Options::new(window, cx);
        let jvm = cx.new(|cx| InputState::new(window, cx));
        let loader_version = cx.new(|cx| LoaderVersion::new(window, cx));
        let name = cx.new(|cx| InputState::new(window, cx));
        let committed = |s: &mut Self, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<Self>| {
            if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                s.commit_inputs(cx);
            }
        };
        let subs = vec![
            cx.observe_global::<remote::Back>(|s, cx| {
                if matches!(s.versions, Load::Failed(_)) {
                    s.load_versions(cx);
                }
            }),
            cx.observe(&model::get(cx), |_, _, cx| cx.notify()),
            cx.subscribe(&jvm, committed),
            cx.subscribe(&loader_version, |s: &mut Self, picker, _: &Picked, cx| {
                if s.instance(cx).is_some_and(|made| made.loader.takes_mods()) {
                    let pinned = picker.read(cx).pinned();
                    s.enqueue(Job::Save(Change::LoaderVersion(pinned)), cx);
                }
            }),
            cx.subscribe_in(&name, window, |s: &mut Self, _, e: &InputEvent, window, cx| match e {
                InputEvent::PressEnter { .. } => {
                    s.commit_rename(cx);
                    if s.renaming && s.rename_error.is_some() {
                        s.focus_name = true;
                        cx.notify();
                        return;
                    }
                    let root = s.root.clone();
                    window.on_next_frame(move |window, cx| window.focus(&root, cx));
                }
                InputEvent::Blur => s.commit_rename(cx),
                InputEvent::Change if s.rename_error.is_some() && s.name.read(cx).value().as_ref() != s.rename_error_for => {
                    s.rename_error = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&version_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<VersionItem>>, window, cx| {
                let SelectEvent::Confirm(Some(v)) = e else { return };
                s.pick_version(v.clone(), window, cx);
            }),
        ]
        .into_iter()
        .chain(opts_subs)
        .collect();
        let mut page = Self {
            id: id.to_owned(),
            tab: Tab::Mods,
            entered: true,
            advanced: false,
            versions: Load::Loading,
            version_list: Vec::new(),
            list_gen: 0,
            everything: false,
            supported: None,
            supported_asked: None,
            version_select,
            select_target: None,
            select_applied: None,
            opts,
            jvm,
            loader_version,
            name,
            renaming: false,
            rename_saving: false,
            loader_focus: cx.focus_handle().tab_stop(true),
            focus_name: false,
            removing: None,
            changing: None,
            loader_news: None,
            loader_asked: None,
            synced: None,
            queue: VecDeque::new(),
            in_flight: None,
            switching: None,
            notes: Vec::new(),
            error: None,
            rename_error: None,
            rename_error_for: String::new(),
            details: HashSet::new(),
            memory_max: forms::memory_max(forms::total_ram_mb()),
            scroll: ScrollHandle::new(),
            root: cx.focus_handle(),
            _subs: subs,
        };
        page.load_versions(cx);
        page
    }

    fn bind(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.id == id {
            return;
        }
        self.id = id.to_owned();
        self.entered = true;
        self.renaming = false;
        self.rename_saving = false;
        self.rename_error = None;
        self.advanced = false;
        self.opts.rebind();
        self.select_target = None;
        self.select_applied = None;
        self.loader_asked = None;
        self.loader_news = None;
        self.notes.clear();
        self.error = None;
        self.details.clear();
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(made) = self.instance(cx) else { return };
        if self.renaming {
            self.focus_name = true;
            cx.notify();
            return;
        }
        self.rename_error = None;
        self.renaming = true;
        self.rename_saving = false;
        self.name.update(cx, |s, cx| s.set_value(made.name, window, cx));
        self.focus_name = true;
        cx.notify();
    }

    fn end_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = false;
        self.rename_saving = false;
        self.rename_error = None;
        cx.notify();
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.renaming {
            cx.propagate();
            return;
        }
        self.end_rename(cx);
        window.focus(&self.root, cx);
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        if !self.renaming || self.rename_saving {
            return;
        }
        let Some(made) = self.instance(cx) else { return };
        let typed = self.name.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            self.rename_error = Some(Problem::plain(tr("Der Name darf nicht leer sein.")));
            self.rename_error_for = self.name.read(cx).value().to_string();
            cx.notify();
            return;
        }
        if typed == made.name {
            self.end_rename(cx);
            return;
        }
        self.rename_saving = true;
        self.rename_error = None;
        self.enqueue(Job::Save(Change::Name(typed)), cx);
    }

    fn toggle(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if !self.details.remove(key) {
            self.details.insert(key);
        }
        cx.notify();
    }

    fn facts(&self, cx: &App) -> Option<Facts> {
        let m = model::get(cx).read(cx);
        let made = m.instance(&self.id)?.clone();
        let batches = m.batches.iter().filter(|b| b.instance_id == made.id).map(|b| (b.id.clone(), b.name.clone())).collect();
        let accounts = m.accounts.iter().map(|a| (a.id, a.name.clone())).collect();
        let live: Vec<&Game> = m.solo_games(&made.id).filter(|g| g.phase.is_active()).collect();
        let plain: Vec<Uuid> = live.iter().filter(|g| matches!(g.phase, Phase::Plain)).map(|g| g.account_id).collect();
        let solo = super::state::solo_account(m, &made).filter(|a| !plain.contains(a));
        let live: Vec<&Game> = live.into_iter().filter(|g| super::tile::playable(&g.phase)).collect();
        let shown = live.iter().find(|g| Some(g.account_id) == solo).or(live.first()).map(|g| g.key.clone());
        let running = m.games.values().any(|g| g.instance_id == made.id && g.phase.is_active());
        Some(Facts { made, batches, accounts, solo, shown, running, plain })
    }

    fn instance(&self, cx: &App) -> Option<Instance> {
        model::get(cx).read(cx).instance(&self.id).cloned()
    }

    fn running(&self, cx: &App) -> bool {
        model::get(cx).read(cx).games.values().any(|g| g.instance_id == self.id && g.phase.is_active())
    }

    fn switching_now(&self) -> bool {
        self.switching.as_ref().is_some_and(|b| b.id == self.id)
    }

    fn blocked(&self, cx: &App) -> bool {
        self.switching_now() || self.running(cx)
    }

    fn load_versions(&mut self, cx: &mut Context<Self>) {
        self.versions = Load::Loading;
        cx.notify();
        let task = cx.background_executor().spawn(async { remote::catalog() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                match result {
                    Ok(list) if !list.is_empty() => {
                        s.version_list = list;
                        s.list_gen += 1;
                        s.versions = Load::Ready(());
                    }
                    Ok(_) => s.versions = Load::Failed(Problem::plain(tr("Mojang hat keine Versionen geliefert. Versuche es später noch einmal."))),
                    Err(e) => s.versions = Load::Failed(forms::explain_as(tr("Versionen nicht geladen."), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn check_supported(&mut self, made: &Instance, cx: &mut Context<Self>) {
        let kind = made.loader.name().to_owned();
        if self.supported_asked.as_ref() == Some(&kind) {
            return;
        }
        self.supported_asked = Some(kind.clone());
        let loader = made.loader.clone();
        let task = cx.background_executor().spawn(async move { remote::supported_versions(&loader) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.supported_asked.as_ref() == Some(&kind) {
                    s.supported = Some((kind, found));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn sync(&mut self, made: &Instance, window: &mut Window, cx: &mut Context<Self>) {
        self.check_supported(made, cx);
        let want = self.select_target.clone().unwrap_or_else(|| made.game_version.clone());
        let supported = self.supported.as_ref().filter(|(kind, _)| kind == made.loader.name()).and_then(|(_, s)| s.as_ref());
        let key = (want.clone(), self.list_gen, self.everything, supported.is_some());
        if self.select_applied.as_ref() != Some(&key) {
            self.select_applied = Some(key);
            let list = picker::items(&self.version_list, self.everything, supported, &want);
            let at = list.iter().position(|v| v.value() == &want);
            self.version_select.update(cx, |s, cx| {
                s.set_items(SearchableVec::new(list), window, cx);
                if let Some(at) = at {
                    s.set_selected_index(Some(IndexPath::new(at)), window, cx);
                }
            });
        }
        let shown = (made.id.clone(), made.jvm_args.join(" "), made.loader.version().unwrap_or_default().to_owned());
        if self.synced.as_ref() != Some(&shown) {
            let jvm = shown.1.clone();
            self.synced = Some(shown);
            self.jvm.update(cx, |s, cx| s.set_value(jvm, window, cx));
        }
        self.loader_version.update(cx, |p, cx| p.show(&made.loader, &made.game_version, cx));
        self.sync_options(made, window, cx);
    }

    fn commit_inputs(&mut self, cx: &mut Context<Self>) {
        let Some((id, jvm, _)) = self.synced.clone() else { return };
        if id != self.id {
            return;
        }
        let args = split_args(&self.jvm.read(cx).value());
        self.opts.jvm_problem = check_jvm_args(&args).err();
        if self.opts.jvm_problem.is_none() && args != split_args(&jvm) {
            self.enqueue(Job::Save(Change::JvmArgs(args)), cx);
        }
        cx.notify();
    }

    fn enqueue(&mut self, job: Job, cx: &mut Context<Self>) {
        let entry = (self.id.clone(), job);
        if self.queue.back().or(self.in_flight.as_ref()) == Some(&entry) {
            return;
        }
        self.error = None;
        self.details.remove("error");
        self.queue.push_back(entry);
        self.pump(cx);
        cx.notify();
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        while self.in_flight.is_none() {
            let Some((id, job)) = self.queue.pop_front() else { return };
            let Some(made) = model::get(cx).read(cx).instance(&id).cloned() else { continue };
            self.in_flight = Some((id.clone(), job.clone()));
            match job {
                Job::Save(change) => {
                    self.changing = Some((InstanceEdit::of(&made), matches!(change, Change::Name(_)).then_some(Edit::Renamed)));
                    self.removing = match &change {
                        Change::RemoveMod(project) => made.mods.iter().position(|m| m.project_id == *project).map(|at| (made.mods[at].clone(), at)),
                        _ => None,
                    };
                    let base = InstanceEdit::of(&made);
                    let mut edit = base.clone();
                    change.apply(&mut edit);
                    let task = cx.background_executor().spawn({
                        let id = id.clone();
                        async move { remote::save_instance(&id, &base, edit).map(|saved| (saved, Vec::new())) }
                    });
                    self.finish_with(id, task, None, cx);
                }
                Job::Switch(switch) => {
                    self.removing = None;
                    let to = match &switch {
                        Switch::Version(v) => trf("Minecraft %@", &[v]),
                        Switch::Loader(l) => l.name().to_owned(),
                    };
                    self.changing = Some((InstanceEdit::of(&made), Some(Edit::Switched(to))));
                    let text = match &switch {
                        Switch::Version(v) => trf("Mods werden für %@ angepasst …", &[v]),
                        Switch::Loader(l) => trf("Auf %@ wird umgestellt …", &[&l.name()]),
                    };
                    self.switching = Some(Busy { id: id.clone(), text });
                    let target = match &switch {
                        Switch::Version(v) => Some(v.clone()),
                        Switch::Loader(_) => None,
                    };
                    let task = cx.background_executor().spawn(async move { switched(&made, &switch) });
                    self.finish_with(id, task, Some(target), cx);
                }
            }
        }
    }

    fn finish_with(&mut self, id: String, task: Task<anyhow::Result<(Instance, Vec<String>)>>, switch: Option<Option<String>>, cx: &mut Context<Self>) {
        let switched = switch.is_some();
        if let Some(target) = switch {
            self.select_target = target;
            self.select_applied = None;
        }
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| s.finished(&id, switched, result, cx));
        })
        .detach();
    }

    fn finished(&mut self, id: &str, switched: bool, result: anyhow::Result<(Instance, Vec<String>)>, cx: &mut Context<Self>) {
        let renamed = matches!(self.in_flight.take(), Some((_, Job::Save(Change::Name(_)))));
        let removed = self.removing.take();
        let changed = self.changing.take();
        if switched && self.switching.as_ref().is_some_and(|b| b.id == id) {
            self.switching = None;
        }
        match result {
            Ok((saved, notes)) => {
                let kept = removed.is_some().then(|| InstanceEdit::of(&saved));
                let edited = changed.and_then(|(before, what)| {
                    let after = InstanceEdit::of(&saved);
                    what.filter(|_| before != after).map(|what| super::undo::Kind::Edited { instance_id: id.to_owned(), name: saved.name.clone(), what, before, after })
                });
                model::get(cx).update(cx, |m, cx| {
                    if let Some(slot) = m.instances.iter_mut().find(|made| made.id == saved.id) {
                        *slot = saved;
                    }
                    cx.notify();
                });
                if switched && id == self.id {
                    self.notes = notes;
                }
                if renamed && self.rename_saving {
                    self.end_rename(cx);
                }
                if let (Some((mod_ref, index)), Some(base)) = (removed, kept) {
                    super::undo::offer(super::undo::Kind::ModRemoved { instance_id: id.to_owned(), mod_ref, index, base }, cx);
                }
                if let Some(kind) = edited {
                    super::undo::offer(kind, cx);
                }
                cx.dispatch_action(&Reload);
            }
            Err(e) => {
                let problem = forms::explain_as(tr("Nicht gespeichert."), &e);
                let rename_failed = renamed && self.rename_saving;
                if rename_failed {
                    self.rename_saving = false;
                    self.focus_name = true;
                }
                self.queue.retain(|(queued, _)| queued != id);
                if id == self.id {
                    if rename_failed {
                        self.rename_error_for = self.name.read(cx).value().to_string();
                        self.rename_error = Some(problem);
                    } else {
                        self.error = Some(problem);
                    }
                }
                self.synced = None;
            }
        }
        if switched {
            self.select_target = None;
            self.select_applied = None;
        }
        self.pump(cx);
        cx.notify();
    }

    fn pending_memory(&self) -> Option<u32> {
        self.queue.iter().rev().chain(self.in_flight.iter()).filter(|(id, _)| *id == self.id).find_map(|(_, job)| match job {
            Job::Save(Change::Memory(mb)) => Some(*mb),
            _ => None,
        })
    }

    fn step_memory(&mut self, delta: i32, cx: &mut Context<Self>) {
        let Some(made) = self.instance(cx) else { return };
        let now = self.pending_memory().unwrap_or(made.memory_mb);
        let next = forms::memory_step(now, delta, self.memory_max);
        if next != now {
            self.enqueue(Job::Save(Change::Memory(next)), cx);
        }
    }

    fn remove_mod(&mut self, project: &str, cx: &mut Context<Self>) {
        if self.blocked(cx) {
            return;
        }
        self.enqueue(Job::Save(Change::RemoveMod(project.to_owned())), cx);
    }

    fn pick_version(&mut self, version: String, window: &mut Window, cx: &mut Context<Self>) {
        self.select_applied = None;
        cx.notify();
        let Some(made) = self.instance(cx) else { return };
        if version == made.game_version || self.blocked(cx) {
            return;
        }
        self.ask_switch(Switch::Version(version), window, cx);
    }

    fn pick_loader(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(made), Some(loader)) = (self.instance(cx), LOADERS.get(n)) else { return };
        if made.loader.same_kind(loader) || self.blocked(cx) {
            return;
        }
        window.focus(&self.loader_focus, cx);
        self.ask_switch(Switch::Loader(loader.clone()), window, cx);
    }

    fn ask_switch(&mut self, switch: Switch, window: &mut Window, cx: &mut Context<Self>) {
        if self.instance(cx).is_some_and(|made| made.mods.is_empty()) {
            self.notes.clear();
            self.enqueue(Job::Switch(switch), cx);
            return;
        }
        let title = match &switch {
            Switch::Version(v) => trf("Auf Minecraft %@ umstellen?", &[v]),
            Switch::Loader(l) => trf("Auf %@ umstellen?", &[&l.name()]),
        };
        let page = cx.entity().downgrade();
        sheet::ask_blocked(window, cx, title, tr("Mods, die es dafür nicht gibt, werden entfernt. Welten bleiben."), tr("Umstellen"), |_| None, move |_, cx| {
            let switch = switch.clone();
            let _ = page.update(cx, |s, cx| {
                if !s.blocked(cx) {
                    s.notes.clear();
                    s.enqueue(Job::Switch(switch), cx);
                }
            });
        });
    }

    fn header(&mut self, f: &Facts, cx: &mut Context<Self>, t: &Theme) -> Div {
        let made = &f.made;
        let icon_tile = div().size(px(ICON_TILE)).flex_none().rounded(px(radius::CONTROL)).bg(t.raised).flex().items_center().justify_center().child(icon("package", iconsize::L, t.tertiary));
        let name = if self.renaming {
            let typed = self.name.read(cx).value().chars().count() as f32;
            let width = (typed * font::TITLE3 * control::RENAME_GLYPH + space::XL).clamp(control::RENAME_MIN, control::RENAME_MAX);
            div()
                .id("instance-title-edit")
                .mark("textbox", tr("Name"))
                .self_start()
                .w(px(width))
                .max_w_full()
                .h(px(font::TITLE3 * font::LINE))
                .ml(px(-space::XS))
                .px(px(space::XS))
                .rounded(px(radius::SMALL))
                .bg(t.window)
                .shadow(vec![BoxShadow { color: super::theme::ring::color(), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(super::theme::ring::INSET), inset: true }])
                .text_size(px(font::TITLE3))
                .font_weight(FontWeight::SEMIBOLD)
                .on_action(cx.listener(|s, _: &Escape, window, cx| s.cancel_rename(window, cx)))
                .child(Styled::h(Input::new(&self.name).appearance(false).bordered(false).focus_bordered(false).p_0().text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD), px(font::TITLE3 * font::LINE)))
                .into_any_element()
        } else {
            div()
                .id("instance-title")
                .mark("heading", made.name.clone())
                .self_start()
                .max_w_full()
                .ml(px(-space::XS))
                .px(px(space::XS))
                .truncate()
                .text_size(px(font::TITLE3))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .when(!super::widgets::menu_open("instance-title", cx), |d| d.tooltip(tip(tr("Doppelklicken zum Umbenennen"))))
                .soft(t.ink.opacity(tint::HOVER), radius::SMALL)
                .on_click(cx.listener(|s, e: &ClickEvent, window, cx| {
                    if e.click_count() == 2 {
                        s.begin_rename(window, cx);
                    }
                }))
                .child(made.name.clone())
                .context_menu({
                    let id = made.id.clone();
                    move |menu, window, cx| super::widgets::menu_of("instance-title", super::sidebar::instance_menu(menu, &id, cx), window, cx)
                })
                .into_any_element()
        };
        let title = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(name)
            .child(match self.rename_error.as_ref().filter(|_| self.renaming) {
                Some(e) => {
                    let said = match &e.detail {
                        Some(detail) => format!("{}\n{detail}", e.text),
                        None => e.text.clone(),
                    };
                    div().id("instance-rename-error").mark("notice", e.text.clone()).truncate().text_size(px(font::CALLOUT)).text_color(t.danger_text).tooltip(tip(said)).child(e.text.clone())
                }
                None => div().id("instance-version").truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(made.version_line()),
            });
        div()
            .h(px(shell::TOOLBAR))
            .flex_none()
            .pl(px(toolbar::lead()))
            .pr(px(tile::INSET))
            .flex()
            .items_center()
            .gap(px(space::S))
            .border_b_1()
            .border_color(t.separator)
            .child(toolbar::slot())
            .child(icon_tile)
            .child(title)
            .child(div().flex_none().flex().items_center().gap(px(space::S)).child(self.batch_control(f, cx, t)).child(self.play_control(f, cx, t)))
    }

    fn play_control(&self, f: &Facts, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let made = &f.made;
        if f.accounts.is_empty() {
            return sheet::button("play-add-account", tr("Account hinzufügen"), Some("plus"), Look::Primary, false, false, t).on_click(|_, window, cx| window.dispatch_action(Box::new(AddAccount), cx)).into_any_element();
        }
        let name = f.solo.and_then(|a| f.accounts.iter().find(|(id, _)| *id == a)).map(|(_, name)| name.clone());
        let (label, action): (String, Option<Box<dyn Action>>) = match (&f.shown, &name) {
            (Some(key), _) => (tr("Zeigen"), Some(Box::new(Play(key.clone())))),
            (None, Some(name)) => (trf("Spielen als %@", &[name]), Some(Box::new(PlayInstance { instance: made.id.clone(), account: f.solo.map(|a| a.to_string()) }))),
            (None, None) => (tr("Spielen"), None),
        };
        let rings = super::theme::rings(cx);
        let (instance, accounts, current, plain) = (made.id.clone(), f.accounts.clone(), f.solo, f.plain.clone());
        let menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let mut menu = menu.scrollable(true);
            for (account, name) in &accounts {
                let action = Box::new(PlayInstance { instance: instance.clone(), account: Some(account.to_string()) });
                menu = if plain.contains(account) {
                    menu.menu_element_with_disabled(action, true, super::palette::menu_row(name.clone(), Some(tr("Läuft · ohne Vorschau")), true))
                } else {
                    menu.menu_with_check(name.clone(), current == Some(*account), action)
                };
            }
            menu
        };
        let words = |label: String| div().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).child(label);
        let Some(action) = action else {
            let button = Button::new("play-menu").primary().small().h(px(control::M)).px(px(space::M)).focus_ring(rings).dropdown_caret(true).child(words(label.clone())).dropdown_menu(menu);
            return div().id("play").mark("button", label).flex_none().child(button).into_any_element();
        };
        if f.accounts.len() < 2 {
            return sheet::button("play", label, None, Look::Primary, false, false, t).on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)).into_any_element();
        }
        let main = Button::new("play-main")
            .primary()
            .small()
            .h(px(control::M))
            .px(px(space::M))
            .rounded_tr(px(0.))
            .rounded_br(px(0.))
            .focus_ring(rings)
            .child(words(label.clone()))
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx));
        let chevron_label = tr("Anderer Account");
        let chevron = Button::new("play-menu")
            .primary()
            .small()
            .h(px(control::M))
            .w(px(control::M))
            .rounded_tl(px(0.))
            .rounded_bl(px(0.))
            .focus_ring(rings)
            .tooltip(chevron_label.clone())
            .child(icon("chevron-down", iconsize::S, t.on_accent))
            .dropdown_menu(menu);
        div()
            .flex()
            .flex_none()
            .gap(px(tile::HAIRLINE))
            .child(div().id("play").mark("button", label).flex_none().child(main))
            .child(div().id("play-chevron").mark("button", chevron_label).flex_none().child(chevron))
            .into_any_element()
    }

    fn batch_control(&self, f: &Facts, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let id = f.made.id.clone();
        if f.batches.is_empty() {
            return sheet::button("new-batch", tr("Neuer Batch…"), None, Look::Secondary, false, false, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(NewBatchFor(id.clone())), cx)).into_any_element();
        }
        let label = tr("Batch starten");
        let batches = f.batches.clone();
        let button = Button::new("batch-menu")
            .secondary()
            .small()
            .h(px(control::M))
            .px(px(space::M))
            .focus_ring(super::theme::rings(cx))
            .dropdown_caret(true)
            .child(div().text_size(px(font::BODY)).child(label.clone()))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true);
                for (batch, name) in &batches {
                    menu = menu.menu(name.clone(), Box::new(StartBatch(batch.clone())));
                }
                menu.separator().menu(tr("Neuer Batch mit dieser Instanz…"), Box::new(NewBatchFor(id.clone())))
            });
        div().id("batch-start").mark("button", label).flex_none().child(button).into_any_element()
    }

    fn batch_links(&self, f: &Facts, t: &Theme) -> Option<Div> {
        if f.batches.is_empty() {
            return None;
        }
        let intro = if f.batches.len() == 1 { tr("In 1 Batch:") } else { trf("In %lld Batches:", &[&f.batches.len()]) };
        let last = f.batches.len() - 1;
        let links = f.batches.iter().enumerate().map(|(at, (id, name))| {
            let target = id.clone();
            let link = div()
                .id(SharedString::from(format!("batch-link-{id}")))
                .mark("link", name.clone())
                .px(px(space::XXS))
                .rounded(px(radius::SMALL))
                .text_color(t.accent_text)
                .cursor_pointer()
                .key_nav_inside()
                .soft(t.ink.opacity(tint::HOVER), radius::SMALL)
                .child(name.clone())
                .on_click(move |_, window, cx| window.dispatch_action(Box::new(ShowBatch(target.clone())), cx));
            div().flex().child(link).when(at < last, |d| d.child(div().text_color(t.secondary).child(",")))
        });
        Some(div().flex().flex_wrap().items_center().gap(px(space::XS)).text_size(px(font::CALLOUT)).child(div().text_color(t.secondary).child(intro)).children(links))
    }

    fn tabs(&self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let segments = TABS.iter().enumerate().map(|(n, tab)| segment(("instance-tab", n), tab.label(), *tab == self.tab, true, t));
        let pick = cx.listener(|s, n: &usize, _, cx| {
            if let Some(tab) = TABS.get(*n) {
                s.entered |= s.tab != *tab;
                s.tab = *tab;
                s.scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }
        });
        div().flex().child(segmented("instance-tabs", segments, move |n, window, cx| pick(&n, window, cx), t))
    }

    fn render_mods(&mut self, f: &Facts, cx: &mut Context<Self>, t: &Theme) -> Div {
        let made = &f.made;
        let col = div().flex().flex_col().gap(px(space::M));
        if matches!(made.loader, Loader::Vanilla) {
            return col.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Vanilla lädt keine Mods.")));
        }
        let locked = self.blocked(cx);
        let switching = self.switching_now();
        let add = sheet::button_unless("mods-add", tr("Mods hinzufügen…"), Some("plus"), Look::Secondary, switching.then(|| tr("Warte, bis die Umstellung fertig ist.").into()), false, t)
            .when(!switching, |b| {
                b.on_click({
                    let id = made.id.clone();
                    move |_, window, cx| window.dispatch_action(Box::new(AddMods(id.clone())), cx)
                })
            });
        let mut col = col;
        if made.mods.is_empty() {
            col = col.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Noch keine Mods.")));
        } else {
            let mut list = div().flex().flex_col().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).overflow_hidden();
            for (at, m) in made.mods.iter().enumerate() {
                let project = m.project_id.clone();
                let remove = if locked {
                    sheet::unavailable(
                        div()
                            .id(SharedString::from(format!("mod-remove-{project}")))
                            .size(px(control::M))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(radius::INNER)),
                        icon("x", iconsize::M, t.tertiary),
                        if self.switching_now() { tr("Warte, bis die Umstellung fertig ist.") } else { tr("Stoppe zuerst die laufenden Spiele.") },
                    )
                } else {
                    sheet::icon_button(SharedString::from(format!("mod-remove-{project}")), "x", tr("Entfernen"), t.tertiary, t).on_click(cx.listener(move |s, _, _, cx| s.remove_mod(&project, cx)))
                };
                list = list.child(
                    div()
                        .when(at > 0, |d| d.border_t_1().border_color(t.separator))
                        .pl(px(space::M))
                        .pr(px(space::XS))
                        .py(px(space::XS))
                        .flex()
                        .items_center()
                        .gap(px(space::S))
                        .child(icon("package", iconsize::S, t.tertiary))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().text_size(px(font::BODY)).child(m.title.clone()))
                                .child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(m.file_name.clone())),
                        )
                        .child(remove),
                );
            }
            col = col.child(list);
        }
        if !made.local_mods.is_empty() {
            let mut own = div().flex().flex_col().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).overflow_hidden();
            for (at, name) in made.local_mods.iter().enumerate() {
                own = own.child(
                    div()
                        .when(at > 0, |d| d.border_t_1().border_color(t.separator))
                        .pl(px(space::M))
                        .pr(px(space::S))
                        .py(px(space::S))
                        .flex()
                        .items_center()
                        .gap(px(space::S))
                        .child(icon("package", iconsize::S, t.tertiary))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).child(name.clone())),
                );
            }
            col = col.child(div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Eigene Dateien"), t)).child(own));
        }
        col.child(div().flex().child(add))
    }

    fn render_settings(&mut self, f: &Facts, cx: &mut Context<Self>, t: &Theme) -> Div {
        let made = &f.made;
        let busy = self.switching_now();
        let blocked = busy || f.running;
        let why = if busy { Some(tr("Warte, bis die Umstellung fertig ist.")) } else { f.running.then(|| tr("Stoppe zuerst die laufenden Spiele.")) };
        let lock = why.clone();
        self.loader_version.update(cx, |p, cx| p.lock(lock, cx));
        let locked = |id: &'static str, control: AnyElement| match &why {
            Some(why) => sheet::unavailable_over(id, control, why.clone()).into_any_element(),
            None => control,
        };
        let listed = !self.version_list.is_empty() || !matches!(self.versions, Load::Failed(_) | Load::Loading);
        let version_control = match &self.versions {
            Load::Failed(_) if self.version_list.is_empty() => div().h(px(control::M)).flex().items_center().child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(made.game_version.clone())).into_any_element(),
            Load::Loading if self.version_list.is_empty() => div().h(px(control::M)).flex().items_center().child(sheet::waiting(tr("Versionen werden geladen …"), t)).into_any_element(),
            _ => locked("instance-version-locked", div().child(Select::new(&self.version_select).menu_width(px(control::VERSION_MENU)).menu_max_h(px(sheet::LIST_MAX)).search_placeholder(tr("Version suchen")).disabled(blocked)).into_any_element()),
        };
        let page = cx.entity().downgrade();
        let everything = listed.then(|| picker::everything_toggle("instance-versions-all", self.everything, !blocked, move |_, cx| {
            let _ = page.update(cx, |s, cx| {
                s.everything = !s.everything;
                cx.notify();
            });
        }, t));
        let version_control = div().flex().flex_col().gap(px(space::XS)).child(version_control).children(everything).into_any_element();
        let loader_focus = self.loader_focus.clone();
        let segments = LOADERS.iter().enumerate().map(|(n, l)| {
            let selected = made.loader.same_kind(l);
            segment(("instance-loader", n), l.name().to_owned(), selected, !blocked, t).when(selected && !blocked, |d| d.track_focus(&loader_focus))
        });
        let pick = cx.listener(|s, n: &usize, window, cx| s.pick_loader(*n, window, cx));
        let loader_control = locked("instance-loaders-locked", segmented("instance-loaders", segments, move |n, window, cx| pick(&n, window, cx), t).when(blocked, |d| d.opacity(tint::DISABLED)).into_any_element());
        let field = |id: &'static str, label: String, control: AnyElement, width: Option<f32>| {
            div().id(id).flex().flex_col().flex_none().gap(px(space::XS)).when_some(width, |d, w| d.w(px(w))).child(sheet::label(label, t)).child(control)
        };
        let controls = div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(px(space::L))
            .child(field("instance-version-field", tr("Minecraft-Version"), version_control, Some(VERSION_WIDTH)))
            .child(field("instance-loader-field", tr("Loader"), loader_control, None));
        let news = self.loader_update(made);
        let busy_line = match self.switching.as_ref().filter(|b| b.id == self.id) {
            Some(b) => Some(sheet::waiting(b.text.clone(), t).into_any_element()),
            None => news.map(|newest| self.loader_news_line(made, &newest, t)),
        }
        .map(|line| div().min_h(px(control::M)).flex().items_center().child(line));
        let mut col = div().flex().flex_col().gap(px(space::L)).child(div().flex().flex_col().gap(px(space::XS)).child(controls).children(busy_line));
        if let Load::Failed(why) = &self.versions {
            let retry = sheet::button("versions-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.load_versions(cx)));
            col = col.child(div().flex().items_start().gap(px(space::S)).child(div().flex_1().min_w_0().child(sheet::problem("versions-error", "circle-alert", t.danger_text, why, self.details.contains("versions-error"), cx.listener(|s, _, _, cx| s.toggle("versions-error", cx)), t))).child(retry));
        }
        if let Some(e) = &self.error {
            col = col.child(sheet::problem("instance-error", "circle-alert", t.danger_text, e, self.details.contains("instance-error"), cx.listener(|s, _, _, cx| s.toggle("instance-error", cx)), t));
        }
        for note in &self.notes {
            col = col.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(note.clone()));
        }
        col.child(self.render_advanced(made, busy, cx, t)).child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Batches mit dieser Instanz teilen Tastenbelegung und Serverliste."))).child(self.render_actions(f, t))
    }

    fn loader_update(&self, made: &Instance) -> Option<String> {
        let current = made.loader.version()?;
        match &self.loader_news {
            Some((key, Some(newest))) if *key == loader_key(made) && crate::core::install::is_newer(newest, current) => Some(newest.clone()),
            _ => None,
        }
    }

    fn loader_news_line(&self, made: &Instance, newest: &str, t: &Theme) -> AnyElement {
        let (id, to) = (made.id.clone(), newest.to_owned());
        let update = sheet::button("loader-update", tr("Aktualisieren"), None, Look::Plain, false, false, t).on_click(move |_, window, cx| super::maintenance::ask_loader(&id, &to, window, cx));
        div()
            .id("loader-news")
            .mark("status", trf("%@ %@ ist verfügbar.", &[&made.loader.name(), &newest]))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .text_size(px(font::CALLOUT))
            .text_color(t.secondary)
            .child(trf("%@ %@ ist verfügbar.", &[&made.loader.name(), &newest]))
            .child(update)
            .into_any_element()
    }

    fn check_loader(&mut self, made: &Instance, cx: &mut Context<Self>) {
        let key = loader_key(made);
        if self.loader_asked.as_ref() == Some(&key) {
            return;
        }
        self.loader_asked = Some(key.clone());
        if made.loader.version().is_none() || !matches!(made.loader, Loader::Fabric(_) | Loader::Quilt(_)) {
            self.loader_news = None;
            return;
        }
        let (loader, version) = (made.loader.clone(), made.game_version.clone());
        let task = cx.background_executor().spawn(async move { remote::newest_loader(&loader, &version) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.loader_asked.as_ref() == Some(&key) {
                    s.loader_news = Some((key, result.ok().flatten()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_memory(&self, made: &Instance, busy: bool, cx: &mut Context<Self>, t: &Theme) -> Div {
        let shown = self.pending_memory().unwrap_or(made.memory_mb);
        let step = |id: &'static str, icon_name: &'static str, label: String, reason: Option<String>, delta: i32, cx: &mut Context<Self>| {
            if let Some(reason) = reason {
                return sheet::unavailable(div().id(id).size(px(control::M)).flex().items_center().justify_center().rounded(px(radius::INNER)), icon(icon_name, iconsize::M, t.tertiary), reason);
            }
            sheet::icon_button(id, icon_name, label, t.secondary, t).on_click(cx.listener(move |s, _, _, cx| s.step_memory(delta, cx)))
        };
        let value = forms::memory_label(shown);
        let wait = || busy.then(|| tr("Warte, bis die Umstellung fertig ist."));
        let less = wait().or_else(|| (shown <= forms::MEMORY_MIN).then(|| trf("Weniger geht nicht: mindestens %@.", &[&forms::memory_label(forms::MEMORY_MIN)])));
        let more = wait().or_else(|| (shown >= self.memory_max).then(|| trf("Mehr geht nicht: höchstens %@.", &[&forms::memory_label(self.memory_max)])));
        let (less_focus, more_focus) = (self.opts.memory_less.clone(), self.opts.memory_more.clone());
        let (less_on, more_on) = (less.is_none(), more.is_none());
        self.opts.memory_less_off.set(!less_on);
        let stepper = div()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .child(step("memory-less", "minus", tr("Weniger Arbeitsspeicher"), less, -1, cx).when(less_on, |d| d.track_focus(&less_focus)))
            .child(div().id("memory-value").mark("value", value.clone()).min_w(px(control::STEPPER_VALUE)).text_center().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(value))
            .child(step("memory-more", "plus", tr("Mehr Arbeitsspeicher"), more, 1, cx).when(more_on, |d| d.track_focus(&more_focus)));
        div().flex().child(stepper)
    }

    fn render_advanced(&mut self, made: &Instance, busy: bool, cx: &mut Context<Self>, t: &Theme) -> Div {
        let open = self.advanced;
        let disclosure = div()
            .id("instance-advanced")
            .mark("disclosure", tr("Erweitert"))
            .self_start()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .h(px(control::S))
            .px(px(space::XS))
            .ml(px(-space::XS))
            .rounded(px(radius::SMALL))
            .text_size(px(font::CALLOUT))
            .text_color(t.secondary)
            .cursor_pointer()
            .key_nav()
            .soft(t.ink.opacity(tint::HOVER), radius::SMALL)
            .child(icon(if open { "chevron-down" } else { "chevron-right" }, iconsize::S, t.secondary))
            .child(tr("Erweitert"))
            .on_click(cx.listener(move |s, _, _, cx| {
                s.advanced = !open;
                cx.notify();
            }));
        let col = div().flex().flex_col().gap(px(space::S)).child(disclosure);
        if !open {
            return col;
        }
        let memory = self.render_memory(made, busy, cx, t);
        col.child(div().pl(px(space::L)).child(self.render_options(made, memory, cx, t)))
    }

    fn render_actions(&self, f: &Facts, t: &Theme) -> Div {
        let id = f.made.id.clone();
        let open = sheet::button("instance-folder", tr("Ordner öffnen"), None, Look::Secondary, false, false, t).on_click({
            let id = id.clone();
            move |_, window, cx| window.dispatch_action(Box::new(OpenInstanceFolder(id.clone())), cx)
        });
        let clone = sheet::button("instance-clone", tr("Klonen"), None, Look::Secondary, false, false, t).on_click({
            let id = id.clone();
            move |_, window, cx| window.dispatch_action(Box::new(CloneInstance(id.clone())), cx)
        });
        let reset = sheet::button_unless("instance-reset", tr("Zurücksetzen…"), None, Look::Secondary, f.running.then(|| tr("Stoppe zuerst die laufenden Spiele.").into()), false, t);
        let reset = if f.running { reset } else { reset.on_click({ let id = id.clone(); move |_, window, cx| window.dispatch_action(Box::new(ResetInstance(id.clone())), cx) }) };
        let used = !f.batches.is_empty();
        let names: Vec<String> = f.batches.iter().map(|(_, name)| name.clone()).collect();
        let delete = sheet::button_unless("instance-delete", tr("Löschen…"), None, Look::Danger, used.then(|| crate::core::fleet::used_by(&names).into()), false, t);
        let delete = if used { delete } else { delete.on_click(move |_, window, cx| window.dispatch_action(Box::new(DeleteInstance(id.clone())), cx)) };
        div().flex().flex_wrap().items_center().gap(px(space::S)).child(open).child(clone).child(reset).child(delete)
    }

    fn render_gone(&self, root: Stateful<Div>, t: &Theme) -> Stateful<Div> {
        let text = tr("Diese Instanz gibt es nicht mehr.");
        root.flex().items_center().justify_center().px(px(space::XL)).child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(space::M))
                .child(div().id("instance-gone").mark("heading", text.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).child(text))
                .child(sheet::button("instance-gone-all", tr("Zu allen Spielen"), None, Look::Primary, false, false, t).on_click(|_, window, cx| window.dispatch_action(Box::new(ShowAll), cx))),
        )
    }
}

fn switched(made: &Instance, switch: &Switch) -> anyhow::Result<(Instance, Vec<String>)> {
    let base = InstanceEdit::of(made);
    let mut edit = base.clone();
    let mut missing = Vec::new();
    match switch {
        Switch::Version(version) => {
            edit.game_version.clone_from(version);
            edit.loader = edit.loader.with_version(None);
            if edit.loader.takes_mods() && !edit.mods.is_empty() {
                let (mods, gone) = remote::move_mods(&edit.mods, version, &edit.loader)?;
                edit.mods = mods;
                missing = gone.into_iter().map(|title| trf("„%@“ gibt es nicht für %@ und wurde entfernt.", &[&title, version])).collect();
            }
        }
        Switch::Loader(loader) => {
            edit.mods = mods_after_loader(&edit.loader, loader, std::mem::take(&mut edit.mods));
            edit.loader = loader.clone();
        }
    }
    Ok((remote::save_instance(&made.id, &base, edit)?, missing))
}

impl Render for InstancePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        let root = div().id("instance-page").key_context(PAGE_CONTEXT).on_action(cx.listener(|s, _: &RenameCurrent, window, cx| s.begin_rename(window, cx))).on_action(cx.listener(|s, _: &Escape, window, cx| s.cancel_rename(window, cx))).size_full().bg(t.window).text_color(t.primary).font_family(Theme::font_family()).text_size(px(font::BODY)).track_focus(&self.root);
        let Some(f) = self.facts(cx) else { return self.render_gone(root, &t).into_any_element() };
        if let Some(WantsTab(id, tab)) = cx.try_global::<WantsTab>()
            && *id == self.id
        {
            self.entered |= self.tab != *tab;
            self.tab = *tab;
            cx.remove_global::<WantsTab>();
        }
        self.sync(&f.made, window, cx);
        self.check_loader(&f.made, cx);
        if let Some(setting) = cx.try_global::<WantsSetting>().filter(|w| w.0 == self.id).map(|w| w.1.clone()) {
            cx.remove_global::<WantsSetting>();
            self.tab = Tab::Settings;
            self.advanced = true;
            self.opts.reveal(&setting);
        }
        if cx.try_global::<WantsRename>().is_some_and(|w| w.0 == self.id) {
            cx.remove_global::<WantsRename>();
            self.begin_rename(window, cx);
        }
        if std::mem::take(&mut self.focus_name) && self.renaming {
            let input = self.name.clone();
            window.on_next_frame(move |window, cx| {
                input.update(cx, |s, cx| {
                    let end = s.value().len();
                    s.take_focus(window, cx);
                    s.set_selected_range(0..end, cx);
                })
            });
        }
        if self.opts.revealing() {
            let page = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                let _ = page.update(cx, |s, cx| s.finish_reveal(window, cx));
            });
        }
        let header = self.header(&f, cx, &t);
        let hint = super::instance_sheet::companion_hint(&f.made.loader, &f.made.game_version).map(|hint| sheet::status("info", t.secondary, hint));
        let fresh = std::mem::take(&mut self.entered);
        let files = match self.tab {
            Tab::Worlds => Some(super::instance_worlds::view(&self.id, fresh, window, cx)),
            Tab::Shots => Some(super::instance_shots::view(&self.id, fresh, window, cx)),
            Tab::Logs => Some(super::instance_logs::view(&self.id, fresh, window, cx)),
            Tab::Mods | Tab::Settings => None,
        };
        let error = self.error.clone().filter(|_| self.tab != Tab::Settings).map(|e| sheet::problem("instance-error", "circle-alert", t.danger_text, &e, self.details.contains("instance-error"), cx.listener(|s, _, _, cx| s.toggle("instance-error", cx)), &t));
        if let Some(files) = files {
            let body = div().size_full().max_w(px(PAGE_MAX)).px(px(space::XL)).py(px(space::L)).flex().flex_col().gap(px(space::M)).children(self.batch_links(&f, &t)).children(hint).child(self.tabs(cx, &t)).children(error).child(div().flex_1().min_h_0().child(files));
            return root.flex().flex_col().child(header).child(div().id("instance-body").flex_1().min_h_0().flex().justify_center().child(body)).into_any_element();
        }
        let content = match self.tab {
            Tab::Settings => self.render_settings(&f, cx, &t),
            _ => self.render_mods(&f, cx, &t),
        };
        let body = div().w_full().max_w(px(PAGE_MAX)).px(px(space::XL)).py(px(space::L)).flex().flex_col().gap(px(space::M)).children(self.batch_links(&f, &t)).children(hint).child(self.tabs(cx, &t)).children(error).child(content);
        root.flex().flex_col().child(header).child(div().id("instance-body").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).child(div().w_full().flex().justify_center().child(body))).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, InstanceEdit, Loader, ModRef, after, mods_after_loader, split_args};
    use crate::core::instance::Instance;

    fn mod_ref(id: &str) -> ModRef {
        ModRef { project_id: id.into(), version_id: "v".into(), title: id.into(), file_name: format!("{id}.jar"), url: String::new(), sha1: String::new() }
    }

    fn edit() -> InstanceEdit {
        let mut made = Instance::new("Farm", "26.2", Loader::Fabric(Some("0.17".into())));
        made.mods = vec![mod_ref("a"), mod_ref("b")];
        made.jvm_args = vec!["-Xss1m".into()];
        InstanceEdit::of(&made)
    }

    #[test]
    fn removing_a_mod_matches_its_project() {
        let mut e = edit();
        Change::RemoveMod("a".into()).apply(&mut e);
        assert_eq!(e.mods, vec![mod_ref("b")]);
        Change::RemoveMod("gone".into()).apply(&mut e);
        assert_eq!(e.mods.len(), 1);
    }

    #[test]
    fn renaming_changes_only_the_name() {
        let mut e = edit();
        Change::Name("Technik".into()).apply(&mut e);
        assert_eq!((e.name.as_str(), e.mods.len(), e.game_version.as_str(), e.memory_mb), ("Technik", 2, "26.2", edit().memory_mb));
    }

    #[test]
    fn a_change_touches_only_its_field() {
        let mut e = edit();
        Change::Memory(4096).apply(&mut e);
        Change::JvmArgs(vec!["-XX:+UseZGC".into()]).apply(&mut e);
        assert_eq!((e.memory_mb, e.jvm_args.as_slice(), e.mods.len(), e.game_version.as_str()), (4096, &["-XX:+UseZGC".to_owned()][..], 2, "26.2"));
        Change::LoaderVersion(None).apply(&mut e);
        assert_eq!(e.loader, Loader::Fabric(None));
        Change::LoaderVersion(Some("0.18".into())).apply(&mut e);
        assert_eq!(e.loader, Loader::Fabric(Some("0.18".into())));
    }

    #[test]
    fn a_loader_version_never_pins_vanilla() {
        let mut e = edit();
        e.loader = Loader::Vanilla;
        Change::LoaderVersion(Some("1".into())).apply(&mut e);
        assert_eq!(e.loader, Loader::Vanilla);
    }

    #[test]
    fn only_fabric_and_quilt_swap_with_their_mods() {
        let mods = vec![mod_ref("a")];
        let (fabric, quilt) = (Loader::Fabric(None), Loader::Quilt(None));
        assert_eq!(mods_after_loader(&fabric, &quilt, mods.clone()), mods);
        assert_eq!(mods_after_loader(&quilt, &fabric, mods.clone()), mods);
        for to in [Loader::Vanilla, Loader::Forge(None), Loader::NeoForge(None)] {
            assert!(mods_after_loader(&fabric, &to, mods.clone()).is_empty(), "{to:?}");
        }
        assert!(mods_after_loader(&Loader::Forge(None), &fabric, Vec::new()).is_empty());
    }

    #[test]
    fn arguments_split_on_any_whitespace() {
        assert_eq!(split_args("  -Xss1m \t -XX:+UseZGC\n"), ["-Xss1m", "-XX:+UseZGC"]);
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args("-Dname=Müller"), ["-Dname=Müller"]);
    }

    #[test]
    fn after_deleting_the_next_in_sidebar_order_shows_else_the_one_before_else_none() {
        let named = |name: &str| Instance::new(name, "26.3", Loader::Vanilla);
        let (a, b, c) = (named("alpha"), named("Beta"), named("gamma"));
        let all = vec![c.clone(), a.clone(), b.clone()];
        assert_eq!(after(&all, &a.id), Some(b.id.clone()));
        assert_eq!(after(&all, &b.id), Some(c.id.clone()));
        assert_eq!(after(&all, &c.id), Some(b.id.clone()));
        assert_eq!(after(&[a.clone()], &a.id), None);
        assert_eq!(after(&all, "missing"), None);
    }
}
