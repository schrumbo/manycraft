//! The sheets that create an instance and that edit the mods of one: version, loader, mods and memory.

use std::collections::HashSet;

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::IndexPath;
use gpui_kit::component::input::{Input, InputEvent, InputState, Undo};
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{AddMods, EditMods, FindModpacks, FindResourcePacks, FindShaders, ImportFromLauncher, NewInstance, NewInstanceToPlay, PlayInstance, Reload};
use super::batch_sheet::{Load, body_max, list_max};
use super::drive::Mark;
use super::forms::{self, Problem};
use super::loader_version::{self, LoaderVersion, Picked};
use super::mod_browser::Browser;
use super::motion::Soft;
use super::{remote, snapshots};
use super::sheet::{self, Dismissed, Look};
use super::theme::{Theme, control, font, iconsize, radius, space, tint, with_alpha};
use super::versions::{self as picker, VersionItem};
use super::widgets::{ContainScroll, KeyNav, TakeFocus, icon, segment, segmented};
use crate::core::batch::ModRef;
use crate::core::companion;
use crate::core::i18n::{tr, trf};
use crate::core::instance::{self as setup, DEFAULT_MEMORY_MB, LOADERS, Loader};
use crate::core::modrinth::{self, Update};
use crate::core::snapshot::Reason;
use crate::core::versions::{GameVersion, Kind};

const VERSION_WIDTH: f32 = 232.;
const SNUG_PERCENT: u64 = 80;
const SAVE_ERROR_MAX: f32 = 220.;
const DEFAULT_ROWS: f32 = 4.;
const NOTE_LINE: &str = "\u{a0}";

/// Registers the instance sheet actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &NewInstance, cx| open(Mode::Create, None, cx));
    cx.on_action(|action: &NewInstanceToPlay, cx| {
        let account = action.0.clone();
        sheet::in_window(cx, move |window, cx| {
            if sheet::busy(window, cx) {
                return;
            }
            let view = cx.new(|cx| {
                let mut sheet = InstanceSheet::new(Mode::Create, window, cx);
                sheet.play = Some(account);
                sheet
            });
            sheet::show(view, sheet::WIDE, window, cx);
        });
    });
    cx.on_action(|_: &FindModpacks, cx| open(Mode::Create, Some(modrinth::Kind::Modpack), cx));
    cx.on_action(|action: &FindResourcePacks, cx| find(&action.0, modrinth::Kind::ResourcePack, cx));
    cx.on_action(|action: &FindShaders, cx| find(&action.0, modrinth::Kind::Shader, cx));
    cx.on_action(|action: &EditMods, cx| {
        if super::model::get(cx).read(cx).instance(&action.0).is_some() {
            open(Mode::Mods(action.0.clone()), None, cx);
        }
    });
    cx.on_action(|action: &AddMods, cx| {
        let Some(made) = super::model::get(cx).read(cx).instance(&action.0).cloned() else { return };
        let tab = if made.loader.takes_mods() { modrinth::Kind::Mod } else { modrinth::Kind::ResourcePack };
        open(Mode::Mods(action.0.clone()), Some(tab), cx);
    });
}

fn find(id: &str, tab: modrinth::Kind, cx: &mut App) {
    if super::model::get(cx).read(cx).instance(id).is_some() {
        open(Mode::Mods(id.to_owned()), Some(tab), cx);
    }
}

fn open(mode: Mode, search: Option<modrinth::Kind>, cx: &mut App) {
    sheet::in_window(cx, move |window, cx| {
        if sheet::busy(window, cx) {
            return;
        }
        let view = cx.new(|cx| InstanceSheet::new(mode, window, cx));
        sheet::show(view.clone(), sheet::WIDE, window, cx);
        if let Some(tab) = search {
            view.update(cx, |s, cx| {
                s.direct = true;
                s.browser.kind = tab;
                s.open_search(true, window, cx);
                s.browse_tab(tab, window, cx);
            });
            window.on_next_frame(move |window, cx| window.on_next_frame(move |window, cx| drop(view.update(cx, |s, cx| s.focus_search(window, cx)))));
        }
    });
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Mode {
    Create,
    Mods(String),
}

pub(super) struct InstanceSheet {
    pub(super) mode: Mode,
    play: Option<Option<String>>,
    pub(super) setup: Option<setup::Instance>,
    name: Entity<InputState>,
    loader_version: Entity<LoaderVersion>,
    jvm_args: Entity<InputState>,
    pub(super) browser: Browser,
    versions: Load<()>,
    version_list: Vec<GameVersion>,
    version_select: Entity<SelectState<SearchableVec<VersionItem>>>,
    pending_versions: bool,
    everything: bool,
    supported: Option<(String, Option<HashSet<String>>)>,
    supported_asked: Option<String>,
    pub(super) version: String,
    pub(super) loader: Loader,
    memory: u32,
    advanced: bool,
    memory_max: u32,
    total_ram: u64,
    free_ram: Option<u64>,
    pub(super) mods: Vec<ModRef>,
    pub(super) mods_touched: bool,
    mods_busy: Option<String>,
    mods_error: Option<Problem>,
    notes: Vec<String>,
    pub(super) searching: bool,
    direct: bool,
    name_auto: String,
    rename: bool,
    pub(super) body_scroll: ScrollHandle,
    mods_scroll: ScrollHandle,
    local_scroll: ScrollHandle,
    pub(super) saved_scroll: Option<Point<Pixels>>,
    list_max: f32,
    imported: Option<setup::Instance>,
    pub(super) updates: Option<Load<Vec<Update>>>,
    updates_gen: u64,
    pub(super) updating: HashSet<String>,
    pub(super) applied: HashSet<String>,
    update_error: Option<Problem>,
    updates_note: Option<String>,
    pub(super) saving: bool,
    save_error: Option<Problem>,
    details: HashSet<&'static str>,
    tried: bool,
    focus_next: bool,
    focus_done: bool,
    refocus: bool,
    pub(super) root: FocusHandle,
    done_focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for InstanceSheet {}

impl InstanceSheet {
    fn new(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = super::model::get(cx).read(cx);
        let setup = match &mode {
            Mode::Mods(id) => model.instance(id).cloned(),
            Mode::Create => None,
        };
        let total_ram = forms::total_ram_mb();
        let memory_max = forms::memory_max(total_ram);
        let memory = forms::memory_step(DEFAULT_MEMORY_MB, 0, memory_max);
        let version = setup.as_ref().map(|i| i.game_version.clone()).unwrap_or_default();
        let loader = setup.as_ref().map_or_else(Loader::default, |i| i.loader.clone());
        let initial: Vec<GameVersion> = if version.is_empty() { Vec::new() } else { vec![GameVersion { id: version.clone(), kind: Kind::Release }] };
        let selected = (!initial.is_empty()).then(|| IndexPath::new(0));
        let version_select = cx.new(|cx| SelectState::new(SearchableVec::new(picker::items(&initial, true, None, &version)), selected, window, cx).searchable(true));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. Technik")));
        let loader_version = cx.new(|cx| LoaderVersion::new(window, cx));
        let jvm_args = cx.new(|cx| InputState::new(window, cx));
        let (browser, browser_sub) = Browser::new(window, cx);
        let changed = |s: &mut Self, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<Self>| {
            if matches!(e, InputEvent::Change) {
                s.save_error = None;
                cx.notify();
            }
        };
        let subs = vec![
            cx.observe_global::<remote::Back>(|s, cx| {
                if matches!(s.versions, Load::Failed(_)) {
                    s.load_versions(cx);
                }
                if s.searching && s.browser.stale() {
                    s.browser.forget_saved();
                    s.browse_refresh(false, cx);
                }
            }),
            cx.subscribe(&name, changed),
            cx.subscribe(&loader_version, |s: &mut Self, picker, _: &Picked, cx| {
                s.loader = s.loader.with_version(picker.read(cx).pinned());
                s.save_error = None;
                cx.notify();
            }),
            cx.subscribe(&jvm_args, changed),
            browser_sub,
            cx.observe(&super::model::get(cx), |s, model, cx| {
                s.follow(&model, cx);
                cx.notify();
            }),
            cx.subscribe_in(&version_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<VersionItem>>, _, cx| {
                let SelectEvent::Confirm(Some(v)) = e else { return };
                s.change_version(v.clone(), cx);
            }),
        ];
        let editing = setup.is_some();
        let free = cx.background_executor().spawn(async { crate::core::fit::system_memory().map(|m| m.free_mb) });
        cx.spawn(async move |this, cx| {
            let free_ram = free.await;
            let _ = this.update(cx, |s, cx| {
                s.free_ram = free_ram;
                cx.notify();
            });
        })
        .detach();
        let mut sheet = Self {
            mods: setup.as_ref().map(|i| i.mods.clone()).unwrap_or_default(),
            mods_touched: editing,
            mode,
            play: None,
            setup,
            name,
            loader_version,
            jvm_args,
            browser,
            versions: if editing { Load::Ready(()) } else { Load::Loading },
            version_list: initial,
            version_select,
            pending_versions: false,
            everything: false,
            supported: None,
            supported_asked: None,
            version,
            loader,
            memory,
            advanced: false,
            memory_max,
            total_ram,
            free_ram: None,
            mods_busy: None,
            mods_error: None,
            notes: Vec::new(),
            searching: false,
            direct: false,
            name_auto: String::new(),
            rename: false,
            body_scroll: ScrollHandle::new(),
            mods_scroll: ScrollHandle::new(),
            local_scroll: ScrollHandle::new(),
            saved_scroll: None,
            list_max: sheet::LIST_MAX,
            imported: None,
            updates: None,
            updates_gen: 0,
            updating: HashSet::new(),
            applied: HashSet::new(),
            update_error: None,
            updates_note: None,
            saving: false,
            save_error: None,
            details: HashSet::new(),
            tried: false,
            focus_next: true,
            focus_done: false,
            refocus: false,
            root: cx.focus_handle(),
            done_focus: cx.focus_handle(),
            _subs: subs,
        };
        if editing {
            sheet.load_updates(cx);
        } else {
            sheet.load_versions(cx);
        }
        sheet
    }

    pub(super) fn creating(&self) -> bool {
        self.mode == Mode::Create
    }

    /// Whether leaving now would drop something the user set: mods picked for a new instance, or changed mods.
    fn follow(&mut self, model: &Entity<super::model::Model>, cx: &App) {
        if self.creating() || self.pending_changes() || self.saving || !self.updating.is_empty() {
            return;
        }
        let Some(made) = &self.setup else { return };
        if let Some(now) = model.read(cx).instances.iter().find(|i| i.id == made.id)
            && now.mods != made.mods
        {
            self.mods = now.mods.clone();
            self.setup = Some(now.clone());
            self.applied.clear();
            self.updates_note = None;
        }
    }

    pub(super) fn pending_changes(&self) -> bool {
        match &self.setup {
            Some(made) if !self.creating() => made.mods != self.mods,
            _ => !self.mods.is_empty(),
        }
    }

    pub(super) fn has_mods(&self) -> bool {
        !self.creating() || self.loader.takes_mods()
    }

    fn toggle(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if !self.details.remove(key) {
            self.details.insert(key);
        }
        cx.notify();
    }

    pub(super) fn problem(&self, key: &'static str, icon_name: &str, color: Hsla, problem: &Problem, cx: &mut Context<Self>, t: &Theme) -> Div {
        sheet::problem(key, icon_name, color, problem, self.details.contains(key), cx.listener(move |s, _, _, cx| s.toggle(key, cx)), t)
    }

    fn closable(&self) -> bool {
        !self.saving && self.updating.is_empty()
    }

    fn wait_reason(&self) -> Option<String> {
        if let Some(text) = &self.mods_busy {
            return Some(text.clone());
        }
        if !self.updating.is_empty() {
            return Some(tr("Warte, bis die Updates fertig sind."));
        }
        (!self.browser.adding.is_empty()).then(|| tr("Warte, bis die Mods hinzugefügt sind."))
    }

    fn lock_reason(&self) -> Option<String> {
        self.wait_reason()
    }

    fn taken_names(&self, cx: &App) -> Vec<String> {
        super::model::get(cx).read(cx).instances.iter().map(|i| i.name.clone()).collect()
    }

    fn load_updates(&mut self, cx: &mut Context<Self>) {
        let Some(original) = self.setup.clone().filter(|i| !i.mods.is_empty()) else {
            self.updates = None;
            return;
        };
        self.updates = Some(Load::Loading);
        self.applied.clear();
        self.updates_gen += 1;
        let generation = self.updates_gen;
        cx.notify();
        let task = cx.background_executor().spawn(async move { remote::mod_updates(&original) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.updates_gen != generation {
                    return;
                }
                s.updates = Some(match result {
                    Ok(list) => Load::Ready(list),
                    Err(e) => Load::Failed(forms::explain_as(tr("Updates nicht geladen."), &e)),
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn apply_updates(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        let Some(Load::Ready(list)) = &self.updates else { return };
        let Some(id) = self.setup.as_ref().map(|i| i.id.clone()) else { return };
        let n = list.iter().filter(|u| !self.applied.contains(&u.current.project_id) && only.as_ref().is_none_or(|p| *p == u.current.project_id)).count();
        if n == 0 || !self.updating.is_empty() || self.saving {
            return;
        }
        let sheet = cx.entity().downgrade();
        snapshots::before(&id, Reason::ModUpdates(n), cx, move |cx| {
            let _ = sheet.update(cx, |s, cx| s.apply_updates_now(only, cx));
        });
    }

    fn apply_updates_now(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        let Some(Load::Ready(list)) = &self.updates else { return };
        let Some(id) = self.setup.as_ref().map(|i| i.id.clone()) else { return };
        let selection: Vec<Update> = list.iter().filter(|u| !self.applied.contains(&u.current.project_id) && only.as_ref().is_none_or(|p| *p == u.current.project_id)).cloned().collect();
        if selection.is_empty() || !self.updating.is_empty() || self.saving {
            return;
        }
        self.updating.extend(selection.iter().map(|u| u.current.project_id.clone()));
        self.update_error = None;
        self.updates_note = None;
        let running = self.running(cx);
        cx.notify();
        let sent = selection.clone();
        let task = cx.background_executor().spawn(async move { remote::apply_updates(&id, &sent, running) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.updating.clear();
                match result {
                    Ok(made) => {
                        for u in &selection {
                            if let Some(slot) = s.mods.iter_mut().find(|m| m.project_id == u.current.project_id && m.version_id == u.current.version_id) {
                                *slot = u.latest.clone();
                            }
                        }
                        s.applied.extend(selection.iter().map(|u| u.current.project_id.clone()));
                        s.refocus = true;
                        s.updates_note = Some(if running { trf("%lld Updates vorgemerkt. Sie gelten beim nächsten Start.", &[&selection.len()]) } else if selection.len() == 1 { tr("1 Mod aktualisiert.") } else { trf("%lld Mods aktualisiert.", &[&selection.len()]) });
                        s.setup = Some(made.clone());
                        super::mod_updates::applied(made, selection, cx);
                        cx.dispatch_action(&Reload);
                    }
                    Err(e) => {
                        s.update_error = Some(forms::explain_as(tr("Nicht aktualisiert."), &e));
                        s.refocus = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn running(&self, cx: &App) -> bool {
        let Some(made) = &self.setup else { return false };
        super::model::get(cx).read(cx).games.values().any(|i| i.instance_id == made.id && i.phase.is_running())
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
                        let newest = list.iter().find(|v| v.kind == Kind::Release).unwrap_or(&list[0]).id.clone();
                        s.version_list = list;
                        s.pending_versions = true;
                        s.versions = Load::Ready(());
                        if s.version.is_empty() {
                            s.change_version(newest, cx);
                        }
                    }
                    Ok(_) => s.versions = Load::Failed(Problem::plain(tr("Mojang hat keine Versionen geliefert. Versuche es später noch einmal."))),
                    Err(e) => s.versions = Load::Failed(forms::explain_as(tr("Versionen nicht geladen."), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn check_supported(&mut self, cx: &mut Context<Self>) {
        let kind = self.loader.name().to_owned();
        if !self.creating() || self.supported_asked.as_ref() == Some(&kind) {
            return;
        }
        self.supported_asked = Some(kind.clone());
        let loader = self.loader.clone();
        let task = cx.background_executor().spawn(async move { remote::supported_versions(&loader) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.supported_asked.as_ref() == Some(&kind) {
                    s.supported = Some((kind, found));
                    s.pending_versions = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn change_version(&mut self, version: String, cx: &mut Context<Self>) {
        if version == self.version || self.mods_busy.is_some() || !self.browser.adding.is_empty() {
            self.pending_versions = version != self.version;
            return;
        }
        let previous = std::mem::replace(&mut self.version, version);
        self.loader = self.loader.with_version(None);
        self.rename = true;
        self.load_mods(previous, cx);
    }

    fn load_mods(&mut self, previous: String, cx: &mut Context<Self>) {
        self.mods_error = None;
        if self.version.is_empty() {
            return;
        }
        if !self.has_mods() {
            self.mods.clear();
            self.notes.clear();
            self.mods_touched = false;
            cx.notify();
            return;
        }
        self.notes.clear();
        let version = self.version.clone();
        let touched = self.mods_touched;
        let mods = self.mods.clone();
        self.mods_busy = Some(if touched { trf("Mods werden für %@ angepasst …", &[&version]) } else { tr("Standard-Mods werden geladen …") });
        cx.notify();
        let v = version.clone();
        let loader = self.loader.clone();
        let wanted = loader.clone();
        let task = cx.background_executor().spawn(async move {
            if touched {
                remote::move_mods(&mods, &v, &wanted).map(|(mods, missing)| (mods, missing.into_iter().map(|m| trf("„%@“ gibt es nicht für %@ und wurde entfernt.", &[&m, &v])).collect::<Vec<_>>()))
            } else {
                remote::defaults(&v, &wanted).map(|d| {
                    let note = default_gaps(d.mods.len(), &d.notes, &v);
                    (d.mods, note.into_iter().collect())
                })
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.version != version || !s.loader.same_kind(&loader) {
                    return;
                }
                s.mods_busy = None;
                if !s.has_mods() {
                    cx.notify();
                    return;
                }
                match result {
                    Ok((mods, notes)) => {
                        s.mods = mods;
                        s.notes = notes;
                    }
                    Err(e) if previous.is_empty() || previous == version => {
                        s.mods.clear();
                        s.mods_error = Some(forms::explain_as(tr("Mods nicht geladen."), &e));
                    }
                    Err(e) => {
                        s.mods_error = Some(forms::explain_as(trf("Mods für %@ nicht geladen, die Version bleibt %@.", &[&version, &previous]), &e));
                        s.version = previous.clone();
                        s.rename = true;
                        s.pending_versions = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_loader(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(loader) = LOADERS.get(n) else { return };
        if self.saving || self.mods_busy.is_some() || self.loader.same_kind(loader) {
            return;
        }
        self.loader = loader.clone();
        self.save_error = None;
        self.notes.clear();
        if self.has_mods() {
            self.load_mods(self.version.clone(), cx);
        }
        cx.notify();
    }

    fn remove_mod(&mut self, project_id: &str, cx: &mut Context<Self>) {
        self.mods.retain(|m| m.project_id != project_id);
        self.mods_touched = true;
        cx.notify();
    }

    fn name_problem(&self, cx: &App) -> Option<String> {
        let taken = self.taken_names(cx);
        let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
        name_problem(&self.name.read(cx).value(), &taken)
    }

    fn edit(&self, cx: &App) -> remote::InstanceEdit {
        build_edit(
            &self.name.read(cx).value(),
            &self.version,
            &self.loader,
            self.memory,
            &self.jvm_args.read(cx).value(),
            if self.has_mods() { self.mods.clone() } else { Vec::new() },
        )
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.tried = true;
        let blocked = self.saving || self.mods_busy.is_some() || !self.updating.is_empty() || !self.browser.adding.is_empty();
        match self.mode.clone() {
            Mode::Create => {
                if blocked || self.name_problem(cx).is_some() || self.version.is_empty() {
                    cx.notify();
                    return;
                }
                self.create(cx);
            }
            Mode::Mods(id) => {
                if blocked {
                    cx.notify();
                    return;
                }
                self.save_mods(id, cx);
            }
        }
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let edit = self.edit(cx);
        self.saving = true;
        self.save_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move { remote::create_instance(edit) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.saving = false;
                match result {
                    Ok(made) => {
                        super::model::get(cx).update(cx, |m, cx| {
                            if !m.instances.iter().any(|i| i.id == made.id) {
                                m.instances.push(made.clone());
                            }
                            cx.notify();
                        });
                        cx.dispatch_action(&Reload);
                        if made.local_mods.is_empty() {
                            s.finish(made.id, cx);
                        } else {
                            s.imported = Some(made);
                            s.focus_done = true;
                        }
                    }
                    Err(e) => s.save_error = Some(forms::explain_as(tr("Nicht angelegt."), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_mods(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(current) = super::model::get(cx).read(cx).instance(&id).cloned() else {
            self.save_error = Some(Problem::plain(tr("Diese Instanz gibt es nicht mehr.")));
            cx.notify();
            return;
        };
        let base = remote::InstanceEdit::of(self.setup.as_ref().unwrap_or(&current));
        let edit = remote::InstanceEdit { mods: self.mods.clone(), ..base.clone() };
        self.saving = true;
        self.save_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move { remote::save_instance(&id, &base, edit) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.saving = false;
                match result {
                    Ok(_) => {
                        cx.dispatch_action(&Reload);
                        cx.emit(Dismissed);
                    }
                    Err(e) => s.save_error = Some(forms::explain_as(tr("Nicht gespeichert."), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn finish(&mut self, id: String, cx: &mut Context<Self>) {
        cx.emit(Dismissed);
        let play = self.play.take();
        cx.defer(move |cx| {
            super::main_window::show_instance(&id, cx);
            if let Some(account) = play {
                cx.dispatch_action(&PlayInstance { instance: id, account });
            }
        });
    }

    pub(super) fn leave_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.direct && self.wait_reason().is_none() && self.setup.as_ref().is_some_and(|made| made.mods == self.mods) {
            cx.emit(Dismissed);
        } else {
            self.open_search(false, window, cx);
        }
    }

    fn search_done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.direct || self.creating() {
            self.open_search(false, window, cx);
        } else if self.wait_reason().is_some() {
            cx.notify();
        } else if self.setup.as_ref().is_some_and(|made| made.mods == self.mods) {
            cx.emit(Dismissed);
        } else {
            self.save(cx);
        }
    }

    fn done(&mut self, cx: &mut Context<Self>) {
        match self.imported.as_ref().map(|made| made.id.clone()) {
            Some(id) => self.finish(id, cx),
            None => cx.emit(Dismissed),
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !sheet::typing(window) {
            cx.propagate();
            return;
        }
        if self.imported.is_some() {
            self.done(cx);
        } else if self.searching {
            self.browse_enter(window, cx);
        } else {
            self.save(cx);
        }
    }

    fn render_form(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let mut body = div().flex().flex_col().gap(px(space::M));
        if self.running(cx) {
            body = body.child(sheet::notice("info", t.accent, t.accent_text, tr("Diese Instanz läuft. Änderungen an den Mods gelten beim nächsten Start.")));
        }
        if !self.creating() {
            return body.child(self.render_mods(cx, t));
        }
        let name_problem = self.name_problem(cx).filter(|_| self.tried || !self.name.read(cx).value().is_empty());
        let version_problem = self.version.is_empty().then(|| tr("Wähle eine Version.")).filter(|_| self.tried);
        let version_control = match &self.versions {
            Load::Failed(why) if self.version_list.is_empty() => div()
                .flex()
                .items_center()
                .gap(px(space::S))
                .child(div().flex_1().min_w_0().child(self.problem("versions-error", "circle-alert", t.danger_text, why, cx, t)))
                .child(sheet::button("versions-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.load_versions(cx)))),
            Load::Loading if self.version_list.is_empty() => div().h(px(control::M)).flex().items_center().child(sheet::waiting(tr("Versionen werden geladen …"), t)),
            _ => {
                let lock = self.lock_reason();
                let select = Select::new(&self.version_select).menu_width(px(control::VERSION_MENU)).menu_max_h(px(self.list_max)).search_placeholder(tr("Version suchen")).disabled(lock.is_some());
                let enabled = lock.is_none();
                let select = match lock {
                    Some(why) => sheet::unavailable_over("instance-version-locked", select, why),
                    None => div().child(select),
                };
                let sheet = cx.entity().downgrade();
                let everything = picker::everything_toggle("sheet-versions-all", self.everything, enabled, move |_, cx| {
                    let _ = sheet.update(cx, |s, cx| {
                        s.everything = !s.everything;
                        s.pending_versions = true;
                        cx.notify();
                    });
                }, t);
                div().flex().flex_col().gap(px(space::XS)).child(select).child(everything)
            }
        };
        if let Some(entry) = self.render_import_entry(cx, t) {
            body = body.child(entry);
        }
        body = body
            .child(
                div()
                    .flex()
                    .gap(px(space::M))
                    .child(div().flex_1().min_w_0().child(div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Name"), t)).child(Input::new(&self.name)).child(sheet::note(None, name_problem, t).min_h(px(font::CAPTION * font::LINE * 2.)))))
                    .child(div().w(px(VERSION_WIDTH)).flex_none().child(sheet::field(tr("Minecraft-Version"), version_control, companion_hint(&self.version), version_problem, t))),
            )
            .child(self.render_loader(cx, t));
        if self.has_mods() {
            body = body.child(self.render_mods(cx, t));
        }
        body = body.child(self.render_advanced(cx, t));
        body
    }

    fn render_loader(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let lock = self.saving.then(|| tr("Wird gerade gesichert …")).or_else(|| self.lock_reason());
        let blocked = lock.is_some();
        let segments = LOADERS.iter().enumerate().map(|(n, l)| segment(("instance-loader", n), l.name().to_owned(), self.loader.same_kind(l), !blocked, t));
        let pick = cx.listener(|s, n: &usize, _, cx| s.pick_loader(*n, cx));
        let control = segmented("instance-loaders", segments, move |n, window, cx| pick(&n, window, cx), t).when(blocked, |d| d.opacity(tint::DISABLED));
        let control = match lock {
            Some(why) => sheet::unavailable_over("instance-loaders-locked", control, why).into_any_element(),
            None => control.into_any_element(),
        };
        div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Loader"), t)).child(div().flex().child(control))
    }

    fn render_advanced(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let count = 1_u32;
        let total = u64::from(self.memory);
        let over = total > self.total_ram;
        let measured = self.free_ram.map(|free| crate::core::fit::fit_in(count, self.memory, crate::core::fit::Memory { total_mb: self.total_ram, free_mb: free }));
        let snug = measured.as_ref().filter(|f| f.fits() && f.needed_mb * 100 > f.free_mb * SNUG_PERCENT).cloned();
        let tight = measured.filter(|f| !f.fits());
        let open = self.advanced || over || tight.is_some() || snug.is_some();
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
        let labelled = |label: String, input: &Entity<InputState>| div().flex().flex_col().gap(px(space::XS)).child(sheet::label(label, t)).child(Input::new(input));
        col.child(
            div()
                .pl(px(space::L))
                .flex()
                .flex_col()
                .gap(px(space::M))
                .when(self.loader.takes_mods(), |d| d.child(div().flex().flex_col().gap(px(space::XS)).child(sheet::label(loader_version::label(&self.loader), t)).child(self.loader_version.clone())))
                .child(self.render_memory(over, tight, snug, cx, t))
                .child(labelled(tr("Java-Argumente"), &self.jvm_args)),
        )
    }

    fn render_memory(&mut self, over: bool, tight: Option<crate::core::fit::Fit>, snug: Option<crate::core::fit::Fit>, cx: &mut Context<Self>, t: &Theme) -> Div {
        let at_min = self.memory <= forms::MEMORY_MIN;
        let at_max = self.memory >= self.memory_max;
        let step = |id: &'static str, icon_name: &'static str, label: String, reason: Option<String>, delta: i32, cx: &mut Context<Self>| {
            if let Some(reason) = reason {
                return sheet::unavailable(div().id(id).size(px(control::M)).flex().items_center().justify_center().rounded(px(radius::INNER)), icon(icon_name, iconsize::M, t.tertiary), reason);
            }
            sheet::icon_button(id, icon_name, label, t.secondary, t).on_click(cx.listener(move |s, _, _, cx| {
                s.memory = forms::memory_step(s.memory, delta, s.memory_max);
                cx.notify();
            }))
        };
        let stepper = div()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .child(step("memory-less", "minus", tr("Weniger Arbeitsspeicher"), at_min.then(|| trf("Weniger geht nicht: mindestens %@.", &[&forms::memory_label(forms::MEMORY_MIN)])), -1, cx))
            .child(div().id("memory-value").mark("value", forms::memory_label(self.memory)).min_w(px(control::STEPPER_VALUE)).text_center().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(forms::memory_label(self.memory)))
            .child(step("memory-more", "plus", tr("Mehr Arbeitsspeicher"), at_max.then(|| trf("Mehr geht nicht: höchstens %@.", &[&forms::memory_label(self.memory_max)])), 1, cx));
        let line = if over {
            sheet::note(None, Some(trf("Das ist mehr, als dieser Rechner hat (%@).", &[&forms::memory_label(u32::try_from(self.total_ram).unwrap_or(u32::MAX))])), t)
        } else if let Some(fit) = tight {
            sheet::note(None, Some(super::model::fit_line(1, self.memory, &fit)), t)
        } else if let Some(fit) = snug {
            sheet::note(Some(trf("%@. Das passt, wird aber knapp.", &[&super::model::fit_line(1, self.memory, &fit)])), None, t)
        } else {
            sheet::note(Some(tr("Pro Account.")), None, t)
        };
        div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Arbeitsspeicher"), t)).child(stepper).child(line.min_h(px(font::CAPTION * font::LINE * 2.)))
    }

    fn render_mods(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let busy = self.mods_busy.clone();
        let add_reason = busy.clone().or_else(|| self.version.is_empty().then(|| tr("Wähle zuerst eine Version.")));
        let header = div()
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(div().flex_1().child(sheet::label(trf("Mods (%lld)", &[&self.mods.len()]), t)))
            .child(sheet::button_unless("mods-add", tr("Mod hinzufügen"), Some("plus"), Look::Plain, add_reason.clone().map(Into::into), false, t).when(add_reason.is_none(), |b| b.on_click(cx.listener(|s, _, window, cx| s.open_search(true, window, cx)))));
        let mut col = div().flex().flex_col().gap(px(space::XS)).child(header);
        if let Some(text) = busy {
            let rows = if self.mods.is_empty() { DEFAULT_ROWS } else { self.mods.len() as f32 };
            let height = (rows * control::M + space::XXS * 2. + 2.).min(self.list_max);
            return col.child(div().h(px(height)).flex().items_center().child(sheet::waiting(text, t))).child(div().text_size(px(font::CAPTION)).child(NOTE_LINE));
        }
        if let Some(e) = &self.mods_error {
            col = col.child(self.problem("mods-error", "circle-alert", t.danger_text, e, cx, t));
        }
        if self.mods.is_empty() {
            col = col.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Keine Mods. Die Instanz startet mit %@ allein.", &[&self.loader.name()])));
        } else {
            let mut list = div().id("mod-list").flex().flex_col().max_h(px(self.list_max)).overflow_y_scroll().track_scroll(&self.mods_scroll).rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS));
            for m in &self.mods {
                let pid = m.project_id.clone();
                list = list.child(
                    div()
                        .pl(px(space::M))
                        .pr(px(space::XS))
                        .flex()
                        .items_center()
                        .gap(px(space::S))
                        .child(icon("package", iconsize::S, t.tertiary))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).child(m.title.clone()))
                        .child(sheet::icon_button(SharedString::from(format!("mod-remove-{pid}")), "x", trf("„%@“ entfernen", &[&m.title]), t.tertiary, t).on_click(cx.listener(move |s, _, _, cx| s.remove_mod(&pid, cx)))),
                );
            }
            col = col.child(div().contain_scroll(&self.mods_scroll).child(list));
        }
        for n in &self.notes {
            col = col.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(n.clone()));
        }
        if let Some(own) = self.setup.as_ref().filter(|i| !i.local_mods.is_empty()) {
            col = col
                .child(div().pt(px(space::S)).child(sheet::label(trf("Eigene Dateien (%lld)", &[&own.local_mods.len()]), t)))
                .child(local_files("local-files", &own.local_mods, &self.local_scroll, self.list_max, t))
                .child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Diese Dateien bleiben unverändert in der Instanz und werden nicht aktualisiert.")));
        }
        if let Some(updates) = self.render_updates(cx, t) {
            col = col.child(updates);
        }
        col
    }

    fn render_import_entry(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        let blocked = self.mods_busy.clone().or_else(|| self.saving.then(|| tr("Wird gerade gesichert …")));
        let card = |id: &'static str, glyph: &'static str, title: String, subtitle: String| {
            div()
                .id(id)
                .mark("option", title.clone())
                .w_full()
                .flex()
                .items_center()
                .gap(px(space::M))
                .px(px(space::M))
                .py(px(space::S))
                .rounded(px(radius::CONTROL))
                .border_1()
                .border_color(t.separator)
                .child(div().size(px(control::M)).flex_none().rounded(px(radius::INNER)).bg(with_alpha(t.accent, tint::ATTENTION)).flex().items_center().justify_center().child(icon(glyph, iconsize::M, t.accent_text)))
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title)).child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(subtitle)))
                .child(icon("chevron-right", iconsize::M, t.tertiary))
        };
        let launcher = card("launcher-pick", "download", tr("Aus anderem Launcher importieren…"), tr("Prism, Modrinth, CurseForge, ATLauncher und mehr"));
        let launcher = match blocked.clone() {
            Some(why) => sheet::unavailable_over("launcher-pick-locked", launcher.opacity(tint::DISABLED), why).into_any_element(),
            None => launcher
                .cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), radius::CONTROL)
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(Dismissed);
                    cx.defer(|cx| cx.dispatch_action(&ImportFromLauncher));
                }))
                .into_any_element(),
        };
        let row = card("file-pick", "file-down", tr("Datei importieren…"), tr("Modpack (.mrpack) oder Prism-Export"));
        let file = if let Some(why) = blocked {
            sheet::unavailable_over("file-pick-locked", row.opacity(tint::DISABLED), why).into_any_element()
        } else {
            row.cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), radius::CONTROL)
                .on_click(cx.listener(|_, _, _, cx| {
                    let sheet = cx.weak_entity();
                    super::transfer::pick(cx, move |cx| {
                        cx.defer(move |cx| {
                            let _ = sheet.update(cx, |_, cx| cx.emit(Dismissed));
                        })
                    });
                }))
                .into_any_element()
        };
        Some(div().flex().flex_col().gap(px(space::S)).child(launcher).child(file))
    }

    fn render_updates(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        let original = self.setup.as_ref()?;
        if self.mods.is_empty() && self.updates_note.is_none() {
            return None;
        }
        let running = self.running(cx);
        let mut col = div().flex().flex_col().gap(px(space::XS)).pt(px(space::S));
        if self.version != original.game_version {
            return Some(col.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Updates gibt es nach dem Sichern der neuen Version."))));
        }
        let busy = !self.updating.is_empty() || snapshots::progress(&original.id, cx).is_some();
        let count = match &self.updates {
            Some(Load::Ready(list)) => list.iter().filter(|u| !self.applied.contains(&u.current.project_id)).count(),
            _ => 0,
        };
        let mut header = div().flex().items_center().gap(px(space::S)).min_h(px(control::M)).child(div().flex_1().child(sheet::label(if count > 0 { trf("Updates (%lld)", &[&count]) } else { tr("Updates") }, t)));
        if count > 1 {
            header = header.child(sheet::button_unless("updates-all", tr("Alle aktualisieren"), Some("refresh-cw"), Look::Secondary, self.saving.then(|| tr("Wird gerade gesichert …").into()), busy, t).on_click(cx.listener(|s, _, _, cx| s.apply_updates(None, cx))));
        }
        col = col.child(header);
        if let Some(e) = &self.update_error {
            col = col.child(self.problem("update-error", "circle-alert", t.danger_text, e, cx, t));
        }
        match &self.updates {
            None => {}
            Some(Load::Loading) => col = col.child(sheet::waiting(tr("Suche nach Updates …"), t)),
            Some(Load::Failed(why)) => {
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::S))
                        .child(div().flex_1().min_w_0().child(self.problem("updates-error", "wifi-off", t.danger_text, why, cx, t)))
                        .child(sheet::button("updates-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.load_updates(cx)))),
                );
            }
            Some(Load::Ready(list)) if list.is_empty() => {
                let line = self.updates_note.clone().unwrap_or_else(|| tr("Alle Mods sind aktuell."));
                col = col.child(sheet::status("circle-check", t.success_text, line));
            }
            Some(Load::Ready(list)) => {
                let mut rows = div().flex().flex_col().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS));
                for u in list.clone() {
                    let pid = u.current.project_id.clone();
                    let working = self.updating.contains(&pid);
                    let done = self.applied.contains(&pid);
                    let action = if done {
                        sheet::status("check", t.success_text, tr("Aktualisiert")).into_any_element()
                    } else {
                        sheet::button_unless(SharedString::from(format!("update-{pid}")), tr("Aktualisieren"), None, Look::Plain, if self.saving { Some(tr("Wird gerade gesichert …").into()) } else if busy { Some(tr("Warte, bis die Updates fertig sind.").into()) } else { None }, working, t).on_click(cx.listener(move |s, _, _, cx| s.apply_updates(Some(pid.clone()), cx))).into_any_element()
                    };
                    let versions = if done { u.latest.file_name.clone() } else { format!("{} → {}", u.current.file_name, u.latest.file_name) };
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!("update-row-{}", u.current.project_id)))
                            .mark("row", u.current.title.clone())
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
                                    .child(div().truncate().text_size(px(font::BODY)).child(u.current.title.clone()))
                                    .child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(versions)),
                            )
                            .child(action),
                    );
                }
                col = col.child(rows);
                if let Some(note) = &self.updates_note {
                    col = col.child(sheet::status("circle-check", t.success_text, note.clone()));
                }
                if running {
                    col = col.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Gilt beim nächsten Start.")));
                }
            }
        }
        Some(col)
    }

    fn render_imported(&mut self, t: &Theme) -> Div {
        let Some(made) = &self.imported else { return div() };
        let matched = made.mods.len();
        let local = made.local_mods.len();
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(space::M))
            .child(sheet::status("circle-check", t.success_text, trf("„%@“ ist angelegt: %lld Mods von Modrinth, %lld eigene Dateien.", &[&made.name, &matched, &local])))
            .child(sheet::label(tr("Nicht auf Modrinth"), t));
        col = col.child(local_files("local-mods", &made.local_mods, &self.local_scroll, self.list_max, t)).child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Diese Dateien bleiben unverändert in der Instanz. Sie werden nicht automatisch aktualisiert und beim Synchronisieren nie gelöscht.")));
        col
    }

    fn apply_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.searching && self.imported.is_none() && let Some(offset) = self.saved_scroll.take() {
            self.body_scroll.set_offset(offset);
        }
        if std::mem::take(&mut self.focus_done) && self.imported.is_some() {
            window.focus(&self.done_focus, cx);
        }
        if std::mem::take(&mut self.refocus) {
            if self.searching {
                self.browse_focus(window, cx);
            } else {
                window.focus(&self.root, cx);
            }
        }
        if window.focused(cx).is_none() {
            window.focus(&self.root, cx);
        }
        let (loader, version, why) = (self.loader.clone(), self.version.clone(), self.lock_reason());
        self.loader_version.update(cx, |p, cx| {
            p.show(&loader, &version, cx);
            p.lock(why, cx);
        });
        self.check_supported(cx);
        if self.pending_versions {
            self.pending_versions = false;
            let supported = self.supported.as_ref().filter(|(kind, _)| kind == self.loader.name()).and_then(|(_, s)| s.as_ref());
            let list = picker::items(&self.version_list, self.everything, supported, &self.version);
            let at = list.iter().position(|v| v.value() == &self.version);
            self.version_select.update(cx, |s, cx| {
                s.set_items(SearchableVec::new(list), window, cx);
                if let Some(at) = at {
                    s.set_selected_index(Some(IndexPath::new(at)), window, cx);
                }
            });
        }
        if std::mem::take(&mut self.rename) && self.creating() && !self.version.is_empty() {
            let typed = self.name.read(cx).value().trim().to_string();
            if typed == self.name_auto {
                let name = trf("Minecraft %@", &[&self.version]);
                let first = typed.is_empty();
                self.name.update(cx, |s, cx| {
                    s.set_value(name.clone(), window, cx);
                    if first {
                        s.set_selected_range(0..name.len(), cx);
                    }
                });
                self.name_auto = name;
            }
        }
        if std::mem::take(&mut self.focus_next) {
            if self.creating() {
                self.name.update(cx, |s, cx| {
                    let end = s.value().len();
                    s.take_focus(window, cx);
                    s.set_selected_range(0..end, cx);
                });
            } else {
                window.focus(&self.root, cx);
            }
        }
    }
}

fn name_problem(name: &str, taken: &[&str]) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some(tr("Gib der Instanz einen Namen."));
    }
    if let Some(problem) = forms::batch_name_problem(name) {
        return Some(problem);
    }
    let lower = name.to_lowercase();
    taken.iter().any(|t| t.trim().to_lowercase() == lower).then(|| trf("Es gibt schon eine Instanz „%@“. Wähle einen anderen Namen.", &[&name]))
}

/// The quiet line under a version the companion does not reach, or nothing when games of `game` have it.
pub(super) fn companion_hint(game: &str) -> Option<String> {
    (!game.is_empty() && !companion::available(game)).then(|| trf("Hintergrundmodus ab %@", &[&companion::first()]))
}

fn build_edit(name: &str, version: &str, loader: &Loader, memory_mb: u32, args: &str, mods: Vec<ModRef>) -> remote::InstanceEdit {
    remote::InstanceEdit {
        name: name.trim().to_owned(),
        game_version: version.to_owned(),
        loader: loader.clone(),
        memory_mb,
        jvm_args: args.split_whitespace().map(str::to_owned).collect(),
        options: Default::default(),
        mods: if loader.takes_mods() { mods } else { Vec::new() },
    }
}

fn local_files(id: &'static str, names: &[String], scroll: &ScrollHandle, max: f32, t: &Theme) -> Div {
    let mut list = div().id(id).flex().flex_col().max_h(px(max)).overflow_y_scroll().track_scroll(scroll).rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS));
    for name in names {
        list = list.child(div().pl(px(space::M)).pr(px(space::XS)).py(px(space::XS)).flex().items_center().gap(px(space::S)).child(icon("package", iconsize::S, t.tertiary)).child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).child(name.clone())));
    }
    div().contain_scroll(scroll).child(list)
}

fn default_gaps(found: usize, notes: &[String], version: &str) -> Option<String> {
    let skipped = notes.iter().filter(|n| n.starts_with("skipped ")).count();
    (skipped > 0).then(|| trf("%1$lld von %2$lld Performance-Mods für %3$@ verfügbar", &[&found, &(found + skipped), &version]))
}

impl Render for InstanceSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_focus(window, cx);
        self.list_max = list_max(window.viewport_size().height.as_f32());
        self.browser.body = body_max(window.viewport_size().height.as_f32());
        let t = Theme::get(cx);
        let creating = self.creating();
        let (title, subtitle) = if self.imported.is_some() {
            (tr("Instanz erstellt"), None)
        } else if self.searching {
            let title = match self.setup.as_ref().filter(|_| !creating) {
                _ if self.browser.kind == modrinth::Kind::Modpack => tr("Neue Instanz aus Modpack"),
                Some(made) => trf("Zu %@ hinzufügen", &[&made.name]),
                None => tr("Von Modrinth hinzufügen"),
            };
            (title, Some(trf("Von Modrinth für Minecraft %@ · %@", &[&self.version, &self.loader.name()])))
        } else if let Some(made) = self.setup.as_ref().filter(|_| !creating) {
            (trf("Mods von %@", &[&made.name]), None)
        } else {
            (tr("Instanz erstellen"), None)
        };
        let body = if self.imported.is_some() {
            self.render_imported(&t)
        } else if self.searching {
            self.render_search(cx, &t)
        } else {
            self.render_form(cx, &t)
        };
        let max_body = body_max(window.viewport_size().height.as_f32());
        let (leading, trailing): (Vec<AnyElement>, Vec<AnyElement>) = if self.imported.is_some() {
            (Vec::new(), vec![sheet::button("import-done", tr("Fertig"), None, Look::Primary, false, false, &t).track_focus(&self.done_focus).on_click(cx.listener(|s, _, _, cx| s.done(cx))).into_any_element()])
        } else if self.searching {
            (
                vec![sheet::button("search-back", tr("Zurück"), Some("chevron-left"), Look::Secondary, false, false, &t).on_click(cx.listener(|s, _, window, cx| s.leave_search(window, cx))).into_any_element()],
                vec![sheet::button_unless("search-done", tr("Fertig"), None, Look::Primary, self.wait_reason().filter(|_| self.direct).map(Into::into), false, &t).on_click(cx.listener(|s, _, window, cx| s.search_done(window, cx))).into_any_element()],
            )
        } else {
            let invalid = if creating { self.name_problem(cx).or_else(|| self.version.is_empty().then(|| tr("Wähle eine Version."))) } else { None };
            let wait = invalid.or_else(|| self.wait_reason());
            let mut trailing = Vec::new();
            if let Some(e) = &self.save_error {
                trailing.push(div().max_w(px(SAVE_ERROR_MAX)).child(self.problem("save-error", "circle-alert", t.danger_text, e, cx, &t)).into_any_element());
            }
            trailing.push(sheet::button_unless("instance-cancel", tr("Abbrechen"), None, Look::Secondary, (!self.closable()).then(|| if self.saving { tr("Wird gerade gesichert …") } else { tr("Warte, bis die Updates fertig sind.") }.into()), false, &t).on_click(cx.listener(|s, _, _, cx| if s.closable() { cx.emit(Dismissed) })).into_any_element());
            trailing.push(sheet::button_unless("instance-save", if !creating { tr("Sichern") } else if self.play.is_some() { tr("Erstellen und spielen") } else { tr("Erstellen") }, None, Look::Primary, wait.map(Into::into), self.saving, &t).on_click(cx.listener(|s, _, _, cx| s.save(cx))).into_any_element());
            (Vec::new(), trailing)
        };
        let me = cx.entity().downgrade();
        div()
            .id("instance-sheet")
            .track_focus(&self.root)
            .capture_key_down(move |e: &KeyDownEvent, window, cx| {
                let handled = me.update(cx, |s, cx| s.browse_key(e.keystroke.key.as_str(), e.keystroke.modifiers, window, cx));
                if handled.unwrap_or(false) {
                    cx.stop_propagation();
                }
            })
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .capture_action(cx.listener(|s, _: &Undo, _, cx| {
                if s.browse_undo_key(cx) {
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|s, _: &Confirm, window, cx| s.confirm(window, cx)))
            .on_action(cx.listener(|s, _: &Cancel, window, cx| {
                if s.searching {
                    s.browse_cancel(window, cx);
                } else if s.imported.is_some() {
                    s.done(cx);
                } else if s.closable() {
                    cx.propagate();
                }
            }))
            .child(sheet::header(title, subtitle, &t))
            .child(div().id("instance-body").track_scroll(&self.body_scroll).max_h(px(max_body)).overflow_y_scroll().px(px(space::XL)).pb(px(space::L)).child(body))
            .child(sheet::footer(leading, trailing, &t))
    }
}

#[cfg(test)]
mod tests {
    use super::{build_edit, name_problem};
    use crate::core::batch::ModRef;
    use crate::core::instance::Loader;

    fn a_mod() -> ModRef {
        ModRef { project_id: "p".into(), version_id: "v".into(), title: "Mod".into(), file_name: "mod.jar".into(), url: String::new(), sha1: String::new() }
    }

    #[test]
    fn an_empty_name_is_a_problem() {
        assert!(name_problem("  ", &[]).is_some());
    }

    #[test]
    fn a_taken_name_is_a_problem_in_any_case() {
        assert!(name_problem("farm", &["Farm"]).is_some());
        assert!(name_problem(" ÄRGER ", &["ärger"]).is_some());
        assert!(name_problem("Farm 2", &["Farm"]).is_none());
    }

    #[test]
    fn a_long_name_is_a_problem() {
        assert!(name_problem(&"x".repeat(200), &[]).is_some());
    }

    #[test]
    fn every_mod_loader_keeps_the_mods() {
        let fabric = build_edit(" Farm ", "26.3", &Loader::Fabric(None), 2048, "", vec![a_mod()]);
        assert_eq!(fabric.name, "Farm");
        assert_eq!(fabric.mods.len(), 1);
        for loader in [Loader::Quilt(None), Loader::Forge(None), Loader::NeoForge(None)] {
            assert_eq!(build_edit("Farm", "26.3", &loader, 2048, "", vec![a_mod()]).mods.len(), 1);
        }
        assert!(build_edit("Farm", "26.3", &Loader::Vanilla, 2048, "", vec![a_mod()]).mods.is_empty());
    }

    #[test]
    fn the_picked_loader_version_is_saved_and_vanilla_takes_no_mods() {
        assert_eq!(build_edit("A", "26.3", &Loader::Fabric(Some("0.17.1".into())), 2048, "", Vec::new()).loader, Loader::Fabric(Some("0.17.1".into())));
        assert_eq!(build_edit("A", "26.3", &Loader::Fabric(None), 2048, "", Vec::new()).loader, Loader::Fabric(None));
        assert!(build_edit("A", "26.3", &Loader::Vanilla, 2048, "", vec![a_mod()]).mods.is_empty());
    }

    #[test]
    fn java_arguments_split_on_whitespace() {
        assert_eq!(build_edit("A", "26.3", &Loader::Fabric(None), 2048, " -Xss1M  -Dx=1 ", Vec::new()).jvm_args, ["-Xss1M", "-Dx=1"]);
    }
}
