//! The sheet to create or edit a batch: version, server, mods, memory and accounts.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::{IndexPath, WindowExt};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Undo};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{AddAccount, EditBatch, NewBatch, NewBatchFromPrism, Reload};
use super::drive::Mark;
use super::forms::{self, Problem};
use super::identity;
use super::mod_browser::Browser;
use super::motion::Soft;
use super::remote;
use super::sheet::{self, Dismissed, Look};
use super::theme::{Theme, control, font, iconsize, radius, space, tint, with_alpha};
use super::widgets::{ContainScroll, KeyNav, TakeFocus, icon, segment, segmented};
use crate::core::account::{Account, AccountKind};
use crate::core::batch::{Background, Batch, ModRef};
use crate::core::i18n::{tr, trf};
use crate::core::modrinth::Update;
use crate::core::prism::Instance;
use crate::core::skins;

type PickRow = (Uuid, String, String, Option<PathBuf>, u8);

struct PickRows {
    raw_filter: String,
    total: usize,
    filtered: bool,
    ids: Rc<Vec<Uuid>>,
    rows: Rc<Vec<PickRow>>,
}

const FILTER_FROM: usize = 12;
const CHROME_HEIGHT: f32 = 150.;
const DEFAULT_TITLES: [(&str, &str); 5] = [("fabric-api", "Fabric API"), ("lithium", "Lithium"), ("ferrite-core", "FerriteCore"), ("modernfix", "ModernFix"), ("krypton", "Krypton")];

/// Registers the batch sheet actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &NewBatch, cx| open(None, cx));
    cx.on_action(|_: &NewBatchFromPrism, cx| {
        let task = cx.background_executor().spawn(async { remote::prism_instances() });
        cx.spawn(async move |cx| {
            let list = task.await.unwrap_or_default();
            cx.update(|cx| {
                sheet::in_window(cx, move |window, cx| {
                    if sheet::busy(window, cx) {
                        return;
                    }
                    let view = cx.new(|cx| BatchSheet::new(None, window, cx));
                    sheet::show(view.clone(), sheet::WIDE, window, cx);
                    if !list.is_empty() {
                        view.update(cx, |s, cx| {
                            s.prism_list = list;
                            s.open_prism(true, window, cx);
                        });
                    }
                })
            })
        })
        .detach();
    });
    cx.on_action(|action: &EditBatch, cx| {
        let batch = super::model::get(cx).read(cx).batch(&action.0).cloned();
        if let Some(batch) = batch {
            open(Some(batch), cx);
        }
    });
}

fn open(batch: Option<Batch>, cx: &mut App) {
    sheet::in_window(cx, move |window, cx| {
        if sheet::busy(window, cx) {
            return;
        }
        let view = cx.new(|cx| BatchSheet::new(batch, window, cx));
        sheet::show(view, sheet::WIDE, window, cx);
    });
}

pub(super) enum Load<T> {
    Loading,
    Ready(T),
    Failed(Problem),
}

pub(super) struct BatchSheet {
    pub(super) original: Option<Batch>,
    name: Entity<InputState>,
    server: Entity<InputState>,
    filter: Entity<InputState>,
    pub(super) browser: Browser,
    versions: Load<()>,
    version_list: Vec<String>,
    version_select: Entity<SelectState<SearchableVec<String>>>,
    pending_versions: bool,
    pub(super) version: String,
    memory: u32,
    memory_touched: bool,
    background: Background,
    advanced: bool,
    memory_max: u32,
    total_ram: u64,
    free_ram: Option<u64>,
    accounts: HashSet<Uuid>,
    pub(super) mods: Vec<ModRef>,
    pub(super) mods_touched: bool,
    mods_busy: Option<String>,
    mods_error: Option<Problem>,
    notes: Vec<String>,
    pub(super) searching: bool,
    prism_list: Vec<Instance>,
    prism_filter: Entity<InputState>,
    prism_sel: usize,
    prism_scroll: ScrollHandle,
    name_auto: String,
    pick_rows: Option<PickRows>,
    rows_dirty: bool,
    skin_gen: u64,
    pub(super) body_scroll: ScrollHandle,
    account_scroll: UniformListScrollHandle,
    mods_scroll: ScrollHandle,
    local_scroll: ScrollHandle,
    pub(super) saved_scroll: Option<Point<Pixels>>,
    list_max: f32,
    prism_choice: Option<Instance>,
    prism_picking: bool,
    imported: Option<Batch>,
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

impl EventEmitter<Dismissed> for BatchSheet {}

impl BatchSheet {
    fn new(original: Option<Batch>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = super::model::get(cx).read(cx);
        let taken: Vec<&str> = model.batches.iter().map(|b| b.name.as_str()).collect();
        let name_value = original.as_ref().map_or_else(|| forms::next_batch_name(&taken), |b| b.name.clone());
        let server_value = original.as_ref().and_then(|b| b.server.clone()).unwrap_or_default();
        let accounts: HashSet<Uuid> = match &original {
            Some(b) => b.accounts.iter().copied().collect(),
            None => model.accounts.iter().map(|a| a.id).collect(),
        };
        let total_ram = forms::total_ram_mb();
        let memory_max = forms::memory_max(total_ram);
        let memory = forms::memory_step(original.as_ref().map_or(2048, |b| b.memory_mb), 0, memory_max);
        let version = original.as_ref().map(|b| b.game_version.clone()).unwrap_or_default();
        let initial: Vec<String> = if version.is_empty() { Vec::new() } else { vec![version.clone()] };
        let selected = (!initial.is_empty()).then(|| IndexPath::new(0));
        let version_select = cx.new(|cx| SelectState::new(SearchableVec::new(initial.clone()), selected, window, cx).searchable(true));
        let name_auto = if original.is_none() { name_value.clone() } else { String::new() };
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. Farm")).default_value(name_value));
        let server = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. play.example.net")).default_value(server_value));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Accounts filtern")));
        let (browser, browser_sub) = Browser::new(window, cx);
        let prism_filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Instanzen filtern")));
        let changed = |s: &mut Self, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<Self>| {
            if matches!(e, InputEvent::Change) {
                s.save_error = None;
                cx.notify();
            }
        };
        let subs = vec![
            cx.subscribe(&name, changed),
            cx.subscribe(&server, changed),
            cx.subscribe(&filter, changed),
            browser_sub,
            cx.observe(&super::model::get(cx), |s: &mut Self, _, cx| {
                s.rows_dirty = true;
                cx.notify();
            }),
            cx.subscribe(&prism_filter, |s: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    s.prism_sel = s.first_supported(cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&version_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<String>>, _, cx| {
                let SelectEvent::Confirm(Some(v)) = e else { return };
                s.change_version(v.clone(), cx);
            }),
        ];
        let editing = original.is_some();
        let original_background = original.as_ref().map_or_else(Background::default, |b| b.background);
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
            mods: original.as_ref().map(|b| b.mods.clone()).unwrap_or_default(),
            mods_touched: editing,
            original,
            name,
            server,
            filter,
            browser,
            versions: Load::Loading,
            version_list: initial,
            version_select,
            pending_versions: false,
            version,
            memory,
            memory_touched: false,
            background: original_background,
            advanced: false,
            memory_max,
            total_ram,
            free_ram: None,
            accounts,
            mods_busy: None,
            mods_error: None,
            notes: Vec::new(),
            searching: false,
            prism_list: Vec::new(),
            prism_filter,
            prism_sel: 0,
            prism_scroll: ScrollHandle::new(),
            name_auto,
            pick_rows: None,
            rows_dirty: true,
            skin_gen: 0,
            body_scroll: ScrollHandle::new(),
            account_scroll: UniformListScrollHandle::new(),
            mods_scroll: ScrollHandle::new(),
            local_scroll: ScrollHandle::new(),
            saved_scroll: None,
            list_max: sheet::LIST_MAX,
            prism_choice: None,
            prism_picking: false,
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
        sheet.load_versions(cx);
        let everyone = super::model::get(cx).read(cx).accounts.clone();
        sheet::warm_heads(everyone, cx);
        if editing {
            sheet.load_updates(cx);
        } else {
            sheet.load_prism(cx);
        }
        sheet
    }

    fn load_prism(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async { remote::prism_instances() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if let Ok(list) = result {
                    s.prism_list = list;
                    cx.notify();
                }
            });
        })
        .detach();
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

    fn open_prism(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open && !self.prism_picking {
            self.saved_scroll = Some(self.body_scroll.offset());
        }
        self.prism_picking = open;
        self.focus_next = !open;
        if open {
            self.prism_sel = self.first_supported(cx);
            if self.prism_list.len() > PRISM_FILTER_FROM {
                self.prism_filter.update(cx, |q, cx| q.take_focus(window, cx));
            } else {
                window.focus(&self.root, cx);
            }
        }
        cx.notify();
    }

    fn prism_shown(&self, cx: &App) -> Vec<Instance> {
        let query = if self.prism_list.len() > PRISM_FILTER_FROM { self.prism_filter.read(cx).value().trim().to_lowercase() } else { String::new() };
        self.prism_list.iter().filter(|i| query.is_empty() || i.name.to_lowercase().contains(&query)).cloned().collect()
    }

    fn first_supported(&self, cx: &App) -> usize {
        self.prism_shown(cx).iter().position(|i| i.unsupported.is_none()).unwrap_or(0)
    }

    fn step_prism(&mut self, delta: i32, cx: &mut Context<Self>) {
        let shown = self.prism_shown(cx);
        let mut at = self.prism_sel.min(shown.len().saturating_sub(1)) as i32;
        loop {
            let next = at + delta;
            if next < 0 || next >= shown.len() as i32 {
                return;
            }
            at = next;
            if shown[at as usize].unsupported.is_none() {
                break;
            }
        }
        self.prism_sel = at as usize;
        self.prism_scroll.scroll_to_item(self.prism_sel);
        cx.notify();
    }

    fn choose_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.prism_shown(cx);
        if let Some(instance) = shown.get(self.prism_sel.min(shown.len().saturating_sub(1))).cloned() {
            self.choose_prism(instance, window, cx);
        }
    }

    fn closable(&self) -> bool {
        !self.saving && self.updating.is_empty()
    }

    fn choose_prism(&mut self, instance: Instance, window: &mut Window, cx: &mut Context<Self>) {
        if self.mods_busy.is_some() || self.saving {
            return;
        }
        let Some(game) = instance.game_version.clone().filter(|_| instance.unsupported.is_none()) else { return };
        let taken: Vec<String> = super::model::get(cx).read(cx).batches.iter().map(|b| b.name.clone()).collect();
        let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
        let name = forms::unique_name(&instance.name, &taken);
        let typed = self.name.read(cx).value().trim().to_string();
        if typed.is_empty() || typed == self.name_auto {
            self.name.update(cx, |s, cx| s.set_value(name.clone(), window, cx));
            self.name_auto = name;
        }
        if !self.version_list.contains(&game) {
            self.version_list.insert(0, game.clone());
        }
        self.version = game;
        self.pending_versions = true;
        self.mods.clear();
        self.notes.clear();
        self.mods_error = None;
        self.mods_touched = true;
        self.prism_choice = Some(instance);
        self.prism_picking = false;
        self.focus_next = true;
        cx.notify();
    }

    fn drop_prism(&mut self, cx: &mut Context<Self>) {
        if self.mods_busy.is_some() || self.saving || self.prism_choice.take().is_none() {
            return;
        }
        let version = std::mem::take(&mut self.version);
        self.mods_touched = false;
        self.mods.clear();
        self.change_version(version, cx);
    }

    fn load_updates(&mut self, cx: &mut Context<Self>) {
        let Some(original) = self.original.clone().filter(|b| !b.mods.is_empty()) else {
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
        let Some(id) = self.original.as_ref().map(|b| b.id.clone()) else { return };
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
                    Ok(batch) => {
                        for u in &selection {
                            if let Some(slot) = s.mods.iter_mut().find(|m| m.project_id == u.current.project_id && m.version_id == u.current.version_id) {
                                *slot = u.latest.clone();
                            }
                        }
                        s.applied.extend(selection.iter().map(|u| u.current.project_id.clone()));
                        s.refocus = true;
                        s.updates_note = Some(if running { trf("%lld Updates vorgemerkt. Sie gelten beim nächsten Start.", &[&selection.len()]) } else if selection.len() == 1 { tr("1 Mod aktualisiert.") } else { trf("%lld Mods aktualisiert.", &[&selection.len()]) });
                        s.original = Some(batch);
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
        let Some(b) = &self.original else { return false };
        super::model::get(cx).read(cx).instances.values().any(|i| i.batch_id == b.id && i.phase.is_running())
    }

    fn load_versions(&mut self, cx: &mut Context<Self>) {
        self.versions = Load::Loading;
        cx.notify();
        let task = cx.background_executor().spawn(async { remote::versions() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                match result {
                    Ok(list) if !list.is_empty() => {
                        let mut list = list;
                        if !s.version.is_empty() && !list.contains(&s.version) {
                            list.insert(0, s.version.clone());
                        }
                        s.version_list = list;
                        s.pending_versions = true;
                        s.versions = Load::Ready(());
                        if s.version.is_empty() {
                            let newest = s.version_list[0].clone();
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

    fn change_version(&mut self, version: String, cx: &mut Context<Self>) {
        if version == self.version || self.mods_busy.is_some() || !self.browser.adding.is_empty() {
            self.pending_versions = version != self.version;
            return;
        }
        let previous = std::mem::replace(&mut self.version, version.clone());
        self.mods_error = None;
        let touched = self.mods_touched;
        let mods = self.mods.clone();
        self.mods_busy = Some(if touched { trf("Mods werden für %@ angepasst …", &[&version]) } else { tr("Standard-Mods werden geladen …") });
        cx.notify();
        let v = version.clone();
        let task = cx.background_executor().spawn(async move {
            if touched {
                remote::move_mods(&mods, &v).map(|(mods, missing)| (mods, missing.into_iter().map(|m| trf("„%@“ gibt es nicht für %@ und wurde entfernt.", &[&m, &v])).collect::<Vec<_>>()))
            } else {
                remote::defaults(&v).map(|d| (d.mods, d.notes.iter().map(|n| default_note(n, &v)).collect()))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.version != version {
                    return;
                }
                s.mods_busy = None;
                match result {
                    Ok((mods, notes)) => {
                        s.mods = mods;
                        s.notes = notes;
                    }
                    Err(e) if previous.is_empty() => {
                        s.mods.clear();
                        s.mods_error = Some(forms::explain_as(tr("Mods nicht geladen."), &e));
                    }
                    Err(e) => {
                        s.mods_error = Some(forms::explain_as(trf("Mods für %@ nicht geladen, die Version bleibt %@.", &[&version, &previous]), &e));
                        s.version = previous;
                        s.pending_versions = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_mod(&mut self, project_id: &str, cx: &mut Context<Self>) {
        self.mods.retain(|m| m.project_id != project_id);
        self.mods_touched = true;
        cx.notify();
    }

    fn problems(&self, cx: &App) -> (Option<String>, Option<String>, Option<String>) {
        let name = forms::batch_name_problem(&self.name.read(cx).value());
        let server = forms::server_problem(self.server.read(cx).value().trim());
        let version = self.version.is_empty().then(|| tr("Wähle eine Version."));
        (name, server, version)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.tried = true;
        let (name, server, version) = self.problems(cx);
        if self.saving || name.is_some() || server.is_some() || version.is_some() || self.mods_busy.is_some() || !self.updating.is_empty() || !self.browser.adding.is_empty() {
            cx.notify();
            return;
        }
        let accounts: Vec<Account> = super::model::get(cx).read(cx).accounts.clone();
        let edit = remote::BatchEdit {
            name: self.name.read(cx).value().trim().to_string(),
            game_version: self.version.clone(),
            server: Some(self.server.read(cx).value().trim().to_string()).filter(|s| !s.is_empty()),
            memory_mb: (self.memory_touched || self.original.is_none()).then_some(self.memory),
            accounts: accounts.iter().map(|a| a.id).filter(|id| self.accounts.contains(id)).collect(),
            mods: self.mods.clone(),
            background: self.background,
        };
        let id = self.original.as_ref().map(|b| b.id.clone());
        self.saving = true;
        cx.notify();
        if let (None, Some(instance)) = (&self.original, self.prism_choice.clone()) {
            let task = cx.background_executor().spawn(async move { remote::import_prism(&instance, edit) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |s, cx| {
                    s.saving = false;
                    match result {
                        Ok(batch) => {
                            cx.dispatch_action(&Reload);
                            if batch.local_mods.is_empty() {
                                cx.emit(Dismissed);
                            } else {
                                s.imported = Some(batch);
                                s.focus_done = true;
                            }
                        }
                        Err(e) => s.save_error = Some(forms::explain_as(tr("Nicht übernommen."), &e)),
                    }
                    cx.notify();
                });
            })
            .detach();
            return;
        }
        let task = cx.background_executor().spawn(async move { remote::save_batch(id.as_deref(), edit) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.saving = false;
                match result {
                    Ok(()) => {
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

    fn ask_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(batch) = self.original.clone() else { return };
        let sheet = cx.entity().downgrade();
        sheet::ask(
            window,
            cx,
            trf("„%@“ löschen?", &[&batch.name]),
            tr("Der Batch verschwindet aus der Liste. Welten, Mods und Einstellungen bleiben im Instanzordner erhalten."),
            tr("Löschen"),
            move |_, cx| {
                let batch = batch.clone();
                let _ = sheet.update(cx, |s, cx| {
                    s.saving = true;
                    cx.notify();
                    let task = cx.background_executor().spawn(async move { remote::delete_batch(&batch) });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |s, cx| {
                            s.saving = false;
                            match result {
                                Ok(()) => {
                                    cx.dispatch_action(&Reload);
                                    cx.emit(Dismissed);
                                }
                                Err(e) => s.save_error = Some(forms::explain_as(tr("Nicht gelöscht."), &e)),
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                });
            },
        );
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prism_picking && !self.searching {
            self.choose_selected(window, cx);
            return;
        }
        if !sheet::typing(window) {
            cx.propagate();
            return;
        }
        if self.imported.is_some() {
            cx.emit(Dismissed);
        } else if self.searching {
            self.browse_enter(window, cx);
        } else {
            self.save(cx);
        }
    }

    fn render_form(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let (name_problem, server_problem, version_problem) = self.problems(cx);
        let name_shown = name_problem.filter(|_| self.tried || !self.name.read(cx).value().is_empty());
        let running = self.running(cx);
        let version_control = match &self.versions {
            Load::Failed(why) if self.version_list.is_empty() => div()
                .flex()
                .items_center()
                .gap(px(space::S))
                .child(div().flex_1().min_w_0().child(self.problem("versions-error", "circle-alert", t.danger_text, why, cx, t)))
                .child(sheet::button("versions-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.load_versions(cx)))),
            Load::Loading if self.version_list.is_empty() => div().h(px(control::M)).flex().items_center().child(sheet::waiting(tr("Versionen werden geladen …"), t)),
            _ => div().child(Select::new(&self.version_select).menu_max_h(px(self.list_max)).search_placeholder(tr("Version suchen")).disabled(self.mods_busy.is_some() || self.prism_choice.is_some() || !self.browser.adding.is_empty())),
        };
        let mut body = div().flex().flex_col().gap(px(space::M));
        if running {
            body = body.child(sheet::notice("info", t.accent, t.accent_text, tr("Dieser Batch läuft. Version, Server, Mods und Accounts gelten beim nächsten Start.")));
        }
        if let Some(entry) = self.render_prism_entry(cx, t) {
            body = body.child(entry);
        }
        let version_hint = self.prism_choice.is_some().then(|| tr("Kommt aus Prism."));
        body = body
            .child(sheet::field(tr("Name"), Input::new(&self.name), None, name_shown, t))
            .child(
                div()
                    .flex()
                    .gap(px(space::M))
                    .child(div().w(px(VERSION_WIDTH)).flex_none().child(sheet::field(tr("Minecraft-Version"), version_control, version_hint, version_problem.filter(|_| self.tried), t)))
                    .child(div().flex_1().min_w_0().child(sheet::field(tr("Server"), Input::new(&self.server), Some(tr("Optional. Adresse mit oder ohne Port.")), server_problem, t))),
            )
            .child(self.render_accounts(cx, t))
            .child(if let Some(instance) = self.prism_choice.clone().filter(|_| self.original.is_none()) { self.render_prism_mods(&instance, t) } else { self.render_mods(cx, t) })
            .child(self.render_background(running, cx, t))
            .child(self.render_memory(cx, t));
        body
    }

    fn render_background(&self, running: bool, cx: &mut Context<Self>, t: &Theme) -> Div {
        let name = |level: Background| match level {
            Background::Saving => tr("Sparsam"),
            Background::Balanced => tr("Ausgewogen"),
            Background::Full => tr("Volle Leistung"),
            Background::Custom { .. } => tr("Eigene"),
        };
        let cost = match self.background {
            Background::Saving => tr("Kostet kaum etwas: Das Spiel zeigt nur wenige Bilder und sieht nicht weit."),
            Background::Balanced => tr("Kostet spürbar mehr: Das Spiel läuft flüssig weiter und sieht so weit wie sonst."),
            Background::Full => tr("Kostet so viel wie ein offenes Spiel."),
            Background::Custom { fps: 1, sight } => trf("Eigene Werte: 1 Bild pro Sekunde, %lld Chunks Sichtweite.", &[&sight]),
            Background::Custom { fps, sight } => trf("Eigene Werte: %lld Bilder pro Sekunde, %lld Chunks Sichtweite.", &[&fps, &sight]),
        };
        let busy = self.saving || self.mods_busy.is_some();
        let levels = Background::PRESETS.into_iter().chain([self.background.to_custom()]);
        let segments = levels.enumerate().map(|(n, level)| segment(("background", n), name(level), std::mem::discriminant(&self.background) == std::mem::discriminant(&level), !busy, t));
        let pick = cx.listener(|s, n: &usize, _, cx| {
            if s.saving || s.mods_busy.is_some() {
                return;
            }
            s.background = Background::PRESETS.get(*n).copied().unwrap_or_else(|| s.background.to_custom());
            s.save_error = None;
            cx.notify();
        });
        let control = segmented("batch-background", segments, move |n, window, cx| pick(&n, window, cx), t).when(busy, |d| d.opacity(tint::DISABLED));
        let hint = if running { format!("{cost} {}", tr("Gilt sofort, auch für die laufenden Spiele.")) } else { cost };
        let mut col = div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Im Hintergrund"), t)).child(div().flex().child(control));
        if let Background::Custom { fps, sight } = self.background {
            col = col.child(div().pt(px(space::XS)).flex().gap(px(space::XL)).child(self.render_dial(Dial::Fps, fps, busy, cx, t)).child(self.render_dial(Dial::Sight, sight, busy, cx, t)));
        }
        col.child(sheet::note(Some(hint), None, t).line_height(px(font::CAPTION * font::LINE)).min_h(px(font::CAPTION * font::LINE * 2.)))
    }

    fn render_dial(&self, dial: Dial, value: u8, busy: bool, cx: &mut Context<Self>, t: &Theme) -> Div {
        let (id, label, value_label, less, more, steps): (&str, _, _, _, _, &[u8]) = match dial {
            Dial::Fps => ("fps", tr("Bilder pro Sekunde"), value.to_string(), tr("Weniger Bilder pro Sekunde"), tr("Mehr Bilder pro Sekunde"), &forms::FPS_STEPS),
            Dial::Sight => ("sight", tr("Sichtweite"), trf("%lld Chunks", &[&value]), tr("Weniger Sichtweite"), tr("Mehr Sichtweite"), &forms::SIGHT_STEPS),
        };
        let step = |icon_name: &'static str, label: String, up: bool, cx: &mut Context<Self>| {
            let id = SharedString::from(format!("background-{id}-{}", if up { "more" } else { "less" }));
            if busy || forms::ladder_step(value, up, steps) == value {
                return div().id(id).mark("button", label.clone()).size(px(control::M)).flex().items_center().justify_center().rounded(px(radius::INNER)).opacity(tint::DISABLED).cursor_not_allowed().key_nav().tooltip(super::widgets::tip(label)).child(icon(icon_name, iconsize::M, t.tertiary));
            }
            sheet::icon_button(id, icon_name, label, t.secondary, t).on_click(cx.listener(move |s, _, _, cx| {
                if s.saving || s.mods_busy.is_some() {
                    return;
                }
                if let Background::Custom { fps, sight } = &mut s.background {
                    let target = match dial {
                        Dial::Fps => fps,
                        Dial::Sight => sight,
                    };
                    *target = forms::ladder_step(*target, up, steps);
                    s.save_error = None;
                    cx.notify();
                }
            }))
        };
        let stepper = div()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .child(step("minus", less, false, cx))
            .child(div().id(SharedString::from(format!("background-{id}-value"))).mark("value", value_label.clone()).min_w(px(control::STEPPER_VALUE_WIDE)).text_center().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).when(busy, |d| d.opacity(tint::DISABLED)).child(value_label))
            .child(step("plus", more, true, cx));
        div().flex().flex_col().gap(px(space::XS)).child(sheet::label(label, t).when(busy, |d| d.opacity(tint::DISABLED))).child(stepper)
    }

    fn render_memory(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let count = self.accounts.len().max(1);
        let total = u64::from(self.memory) * count as u64;
        let total_label = forms::memory_label(u32::try_from(total).unwrap_or(u32::MAX));
        let over = total > self.total_ram;
        let measured = self.free_ram.map(|free| crate::core::fit::fit_in(u32::try_from(count).unwrap_or(u32::MAX), self.memory, crate::core::fit::Memory { total_mb: self.total_ram, free_mb: free }));
        let snug = measured.as_ref().filter(|f| f.fits() && f.needed_mb * 100 > f.free_mb * SNUG_PERCENT).cloned();
        let tight = measured.filter(|f| !f.fits());
        let at_min = self.memory <= forms::MEMORY_MIN;
        let at_max = self.memory >= self.memory_max;
        let open = self.advanced || over || tight.is_some() || snug.is_some();
        let label = if open { tr("Erweitert") } else { trf("Erweitert · %@ Arbeitsspeicher pro Account", &[&forms::memory_label(self.memory)]) };
        let disclosure = div()
            .id("batch-advanced")
            .mark("disclosure", tr("Erweitert"))
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
            .child(label)
            .on_click(cx.listener(move |s, _, _, cx| {
                s.advanced = !open;
                cx.notify();
            }));
        let col = div().flex().flex_col().gap(px(space::S)).child(disclosure);
        if !open {
            return col;
        }
        let step = |id: &'static str, icon_name: &'static str, label: String, limit: bool, delta: i32, cx: &mut Context<Self>| {
            if limit {
                return div().id(id).mark("button", label.clone()).size(px(control::M)).flex().items_center().justify_center().rounded(px(radius::INNER)).opacity(tint::DISABLED).cursor_not_allowed().key_nav().tooltip(super::widgets::tip(label)).child(icon(icon_name, iconsize::M, t.tertiary));
            }
            sheet::icon_button(id, icon_name, label, t.secondary, t).on_click(cx.listener(move |s, _, _, cx| {
                s.memory = forms::memory_step(s.memory, delta, s.memory_max);
                s.memory_touched = true;
                cx.notify();
            }))
        };
        let stepper = div()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .child(step("memory-less", "minus", tr("Weniger Arbeitsspeicher"), at_min, -1, cx))
            .child(div().id("memory-value").mark("value", forms::memory_label(self.memory)).min_w(px(control::STEPPER_VALUE)).text_center().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(forms::memory_label(self.memory)))
            .child(step("memory-more", "plus", tr("Mehr Arbeitsspeicher"), at_max, 1, cx));
        let line = if over {
            sheet::note(None, Some(trf("%lld Accounts brauchen zusammen bis zu %@, mehr als dieser Rechner hat (%@).", &[&count, &total_label, &forms::memory_label(u32::try_from(self.total_ram).unwrap_or(u32::MAX))])), t)
        } else if let Some(fit) = tight {
            sheet::note(None, Some(super::model::fit_line(u32::try_from(count).unwrap_or(u32::MAX), self.memory, &fit)), t)
        } else if let Some(fit) = snug {
            sheet::note(Some(trf("%@. Das passt, wird aber knapp.", &[&super::model::fit_line(u32::try_from(count).unwrap_or(u32::MAX), self.memory, &fit)])), None, t)
        } else if count > 1 {
            sheet::note(Some(trf("Pro Account. %lld Accounts brauchen zusammen bis zu %@.", &[&count, &total_label])), None, t)
        } else {
            sheet::note(Some(tr("Pro Account.")), None, t)
        };
        col.child(div().pl(px(space::L)).flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Arbeitsspeicher"), t)).child(stepper).child(line.min_h(px(font::CAPTION * font::LINE * 2.))))
    }

    fn refresh_rows(&mut self, cx: &App) {
        let raw_filter = self.filter.read(cx).value().trim().to_lowercase();
        let skin_gen = skins::generation();
        let stale = self.rows_dirty || self.skin_gen != skin_gen || self.pick_rows.as_ref().is_none_or(|c| c.raw_filter != raw_filter);
        if !stale {
            return;
        }
        self.rows_dirty = false;
        self.skin_gen = skin_gen;
        let model = super::model::get(cx).read(cx);
        let total = model.accounts.len();
        let filtered = total > FILTER_FROM && !raw_filter.is_empty();
        let shown = model.accounts.iter().filter(|a| !filtered || a.name.to_lowercase().contains(&raw_filter));
        let (ids, rows): (Vec<Uuid>, Vec<PickRow>) = shown.map(|a| (a.id, (a.id, a.name.clone(), if a.kind == AccountKind::Microsoft { tr("Microsoft") } else { tr("Offline") }, skins::head(a), a.color()))).unzip();
        self.pick_rows = Some(PickRows { raw_filter, total, filtered, ids: Rc::new(ids), rows: Rc::new(rows) });
    }

    fn render_accounts(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        self.refresh_rows(cx);
        let Some(cache) = self.pick_rows.as_ref() else { return div() };
        let (total, filtered, ids, rows, raw_filter) = (cache.total, cache.filtered, cache.ids.clone(), cache.rows.clone(), cache.raw_filter.clone());
        let picked = super::model::get(cx).read(cx).accounts.iter().filter(|a| self.accounts.contains(&a.id)).count();
        let header = div().flex().items_center().gap(px(space::S)).child(div().flex_1().child(sheet::label(trf("Accounts (%lld von %lld)", &[&picked, &total]), t)));
        if total == 0 {
            let add = sheet::button("batch-add-account", tr("Account hinzufügen"), Some("plus"), Look::Plain, false, false, t).on_click(|_, window, cx| {
                window.close_dialog(cx);
                window.dispatch_action(Box::new(AddAccount), cx);
            });
            return div().flex().flex_col().gap(px(space::XS)).child(header).child(div().flex().items_center().gap(px(space::S)).child(div().flex_1().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Noch keine Accounts. Du kannst sie später hinzufügen."))).child(add));
        }
        let all = !ids.is_empty() && ids.iter().all(|id| self.accounts.contains(id));
        let (pick, unpick) = if filtered { (tr("Alle gefilterten auswählen"), tr("Gefilterte abwählen")) } else { (tr("Alle auswählen"), tr("Keine auswählen")) };
        let toggled = ids.clone();
        let header = header.child(sheet::button("accounts-all", if all { unpick } else { pick }, None, Look::Plain, ids.is_empty(), false, t).on_click(cx.listener(move |s, _, _, cx| {
            if all {
                s.accounts.retain(|id| !toggled.contains(id));
            } else {
                s.accounts.extend(toggled.iter().copied());
            }
            cx.notify();
        })));
        let frame = div().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).contain_scroll(&self.account_scroll.0.borrow().base_handle);
        let list = if rows.is_empty() {
            frame.py(px(space::XS)).child(div().px(px(space::M)).py(px(space::S)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Kein Account passt zu „%@“.", &[&raw_filter])))
        } else {
            let count = rows.len();
            frame.child(
                uniform_list("account-list", count, cx.processor(move |s, range: std::ops::Range<usize>, _, cx| rows[range].iter().map(|(id, name, kind, head, color)| s.pick_row(*id, name.clone(), kind.clone(), head.clone(), *color, cx)).collect::<Vec<_>>()))
                    .track_scroll(&self.account_scroll)
                    .with_sizing_behavior(ListSizingBehavior::Infer)
                    .px(px(space::XS))
                    .py(px(space::XS))
                    .max_h(px(self.list_max)),
            )
        };
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(header)
            .when(total > FILTER_FROM, |d| d.child(Input::new(&self.filter)))
            .child(list)
            .child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Ändern sich Einstellungen, Ressourcenpakete oder Serverliste in einem Spiel, ziehen alle Spiele dieses Batches mit.")))
    }

    fn pick_row(&mut self, id: Uuid, name: String, kind: String, head: Option<PathBuf>, color: u8, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = Theme::get(cx);
        let checked = self.accounts.contains(&id);
        let toggle = move |s: &mut Self, cx: &mut Context<Self>| {
            if !s.accounts.remove(&id) {
                s.accounts.insert(id);
            }
            cx.notify();
        };
        div()
            .id(SharedString::from(format!("pick-row-{}", id.simple())))
            .mark("row", name.clone())
            .w_full()
            .px(px(space::S))
            .py(px(space::XS))
            .flex()
            .items_center()
            .gap(px(space::S))
            .rounded(px(radius::SMALL))
            .cursor_pointer()
            .key_nav()
            .soft(t.ink.opacity(tint::HOVER), radius::SMALL)
            .on_click(cx.listener(move |s, _, _, cx| toggle(s, cx)))
            .child(Checkbox::new(SharedString::from(format!("pick-{}", id.simple()))).checked(checked).on_click(cx.listener(move |s, _: &bool, _, cx| {
                toggle(s, cx);
                cx.stop_propagation();
            })))
            .child(identity::chip(&name, color, head.map(ImageSource::from), iconsize::L, font::CAPTION))
            .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).child(name))
            .child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(kind))
    }

    fn render_mods(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let busy = self.mods_busy.clone();
        let header = div()
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(div().flex_1().child(sheet::label(trf("Mods (%lld)", &[&self.mods.len()]), t)))
            .child(sheet::button("mods-add", tr("Mod hinzufügen"), Some("plus"), Look::Plain, self.version.is_empty() || busy.is_some(), false, t).on_click(cx.listener(|s, _, window, cx| s.open_search(true, window, cx))));
        let mut col = div().flex().flex_col().gap(px(space::XS)).child(header);
        if let Some(text) = busy {
            return col.child(div().h(px(control::L)).flex().items_center().child(sheet::waiting(text, t)));
        }
        if let Some(e) = &self.mods_error {
            col = col.child(self.problem("mods-error", "circle-alert", t.danger_text, e, cx, t));
        }
        if self.mods.is_empty() {
            col = col.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Keine Mods. Der Batch startet mit Fabric allein.")));
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
        if let Some(own) = self.original.as_ref().filter(|b| !b.local_mods.is_empty()) {
            col = col
                .child(div().pt(px(space::S)).child(sheet::label(trf("Eigene Dateien (%lld)", &[&own.local_mods.len()]), t)))
                .child(local_files("local-files", &own.local_mods, &self.local_scroll, self.list_max, t))
                .child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Diese Dateien bleiben unverändert im Batch und werden nicht aktualisiert.")));
        }
        if let Some(updates) = self.render_updates(cx, t) {
            col = col.child(updates);
        }
        col
    }

    fn render_prism_entry(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        if self.original.is_some() {
            return None;
        }
        let blocked = self.mods_busy.is_some() || self.saving;
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
        };
        let entry = match self.prism_choice.clone() {
            None if self.prism_list.is_empty() => None,
            None => {
                let row = card("prism-pick", "download", tr("Aus Prism-Instanz übernehmen"), tr("Version, Mods und Einstellungen aus Prism Launcher"));
                let row = if blocked { row.opacity(tint::DISABLED) } else { row.cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::CONTROL).on_click(cx.listener(|s, _, window, cx| s.open_prism(true, window, cx))) };
                Some(row.child(icon("chevron-right", iconsize::M, t.tertiary)))
            }
            Some(instance) => {
                let subtitle = trf("Aus Prism · Minecraft %@ · %lld Mods", &[&instance.game_version.clone().unwrap_or_default(), &instance.mods]);
                Some(card("prism-chosen", "download", instance.name.clone(), subtitle)
                    .child(sheet::icon_button("prism-change", "list", tr("Andere Instanz wählen"), t.secondary, t).on_click(cx.listener(|s, _, window, cx| s.open_prism(true, window, cx))))
                    .child(sheet::icon_button("prism-drop", "x", tr("Nicht aus Prism übernehmen"), t.secondary, t).on_click(cx.listener(|s, _, _, cx| s.drop_prism(cx)))))
            }
        };
        let file = self.prism_choice.is_none().then(|| {
            let row = card("file-pick", "file-down", tr("Aus Datei importieren…"), tr("Modpack von Modrinth oder Export aus Prism"));
            let row = if blocked {
                row.opacity(tint::DISABLED)
            } else {
                row.cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::CONTROL).on_click(cx.listener(|_, _, _, cx| {
                    let sheet = cx.weak_entity();
                    super::transfer::pick(cx, move |cx| {
                        cx.defer(move |cx| {
                            let _ = sheet.update(cx, |_, cx| cx.emit(Dismissed));
                        })
                    });
                }))
            };
            row.child(icon("chevron-right", iconsize::M, t.tertiary))
        });
        Some(div().flex().flex_col().gap(px(space::S)).children(entry).children(file))
    }

    fn render_prism_mods(&self, instance: &Instance, t: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(sheet::label(trf("Mods (%lld)", &[&instance.mods]), t))
            .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Mods und Einstellungen werden aus der Instanz kopiert; Prism bleibt unverändert. Beim Erstellen wird geprüft, welche Mods auf Modrinth liegen. Die übrigen bleiben als eigene Dateien im Batch.")))
    }

    fn render_updates(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        let original = self.original.as_ref()?;
        if self.mods.is_empty() && self.updates_note.is_none() {
            return None;
        }
        let running = self.running(cx);
        let mut col = div().flex().flex_col().gap(px(space::XS)).pt(px(space::S));
        if self.version != original.game_version {
            return Some(col.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Updates gibt es nach dem Sichern der neuen Version."))));
        }
        let busy = !self.updating.is_empty();
        let count = match &self.updates {
            Some(Load::Ready(list)) => list.iter().filter(|u| !self.applied.contains(&u.current.project_id)).count(),
            _ => 0,
        };
        let mut header = div().flex().items_center().gap(px(space::S)).min_h(px(control::M)).child(div().flex_1().child(sheet::label(if count > 0 { trf("Updates (%lld)", &[&count]) } else { tr("Updates") }, t)));
        if count > 1 {
            header = header.child(sheet::button("updates-all", tr("Alle aktualisieren"), Some("refresh-cw"), Look::Secondary, self.saving, busy, t).on_click(cx.listener(|s, _, _, cx| s.apply_updates(None, cx))));
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
                        sheet::button(SharedString::from(format!("update-{pid}")), tr("Aktualisieren"), None, Look::Plain, busy || self.saving, working, t).on_click(cx.listener(move |s, _, _, cx| s.apply_updates(Some(pid.clone()), cx))).into_any_element()
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

    fn render_prism_picker(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let shown = self.prism_shown(cx);
        let selected = self.prism_sel.min(shown.len().saturating_sub(1));
        let mut list = div().id("prism-list").flex().flex_col().gap(px(space::XXS)).max_h(px(RESULTS_MAX)).overflow_y_scroll().track_scroll(&self.prism_scroll);
        for (at, instance) in shown.iter().cloned().enumerate() {
            let reason = instance.unsupported.as_deref().map(unsupported_text);
            let ok = reason.is_none();
            let subtitle = match &reason {
                Some(r) => r.clone(),
                None => trf("Minecraft %@ · %lld Mods", &[&instance.game_version.clone().unwrap_or_default(), &instance.mods]),
            };
            let row = div()
                .id(SharedString::from(format!("prism-row-{}", instance.id)))
                .mark("result", instance.name.clone())
                .flex()
                .items_center()
                .gap(px(space::M))
                .px(px(space::M))
                .py(px(space::S))
                .rounded(px(radius::CONTROL))
                .when(at == selected, |d| d.bg(t.ink.opacity(tint::HOVER)));
            let chosen = self.prism_choice.as_ref().is_some_and(|c| c.id == instance.id);
            let row = if ok && !self.mods_busy.is_some() && !self.saving {
                let picked = instance.clone();
                row.cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::CONTROL).on_click(cx.listener(move |s, _, window, cx| s.choose_prism(picked.clone(), window, cx)))
            } else {
                row.opacity(tint::DISABLED)
            };
            list = list.child(
                row.child(icon(if ok { "package" } else { "circle-alert" }, iconsize::M, t.tertiary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(space::XXS))
                            .child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).child(instance.name.clone()))
                            .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(subtitle)),
                    )
                    .when(chosen, |d| d.child(icon("check", iconsize::S, t.success_text))),
            );
        }
        let mut col = div().flex().flex_col().gap(px(space::S));
        if self.prism_list.len() > PRISM_FILTER_FROM {
            col = col.child(Input::new(&self.prism_filter).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true));
        }
        if shown.is_empty() {
            let query = self.prism_filter.read(cx).value().trim().to_string();
            return col.child(div().py(px(space::L)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Keine Instanz passt zu „%@“. Lösche den Filter oder suche anders.", &[&query])));
        }
        col.child(list)
    }

    fn render_imported(&mut self, t: &Theme) -> Div {
        let Some(batch) = &self.imported else { return div() };
        let matched = batch.mods.len();
        let local = batch.local_mods.len();
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(space::M))
            .child(sheet::status("circle-check", t.success_text, trf("„%@“ ist angelegt: %lld Mods von Modrinth, %lld eigene Dateien.", &[&batch.name, &matched, &local])))
            .child(sheet::label(tr("Nicht auf Modrinth"), t));
        col = col.child(local_files("local-mods", &batch.local_mods, &self.local_scroll, self.list_max, t)).child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Diese Dateien bleiben unverändert im Batch. Sie werden nicht automatisch aktualisiert und beim Synchronisieren nie gelöscht.")));
        col
    }

    fn apply_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.searching && !self.prism_picking && self.imported.is_none() && let Some(offset) = self.saved_scroll.take() {
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
        if self.pending_versions {
            self.pending_versions = false;
            let list = self.version_list.clone();
            let at = list.iter().position(|v| *v == self.version);
            self.version_select.update(cx, |s, cx| {
                s.set_items(SearchableVec::new(list), window, cx);
                if let Some(at) = at {
                    s.set_selected_index(Some(IndexPath::new(at)), window, cx);
                }
            });
        }
        if std::mem::take(&mut self.focus_next) {
            self.name.update(cx, |s, cx| {
                let end = s.value().len();
                s.take_focus(window, cx);
                s.set_selected_range(0..end, cx);
            });
        }
    }
}

const VERSION_WIDTH: f32 = 168.;
const RESULTS_MAX: f32 = 340.;
const LIST_MIN: f32 = 96.;
const LIST_SHARE: f32 = 0.5;

fn body_max(viewport: f32) -> f32 {
    (viewport - 2. * sheet::MARGIN - CHROME_HEIGHT).max(160.)
}

fn list_max(viewport: f32) -> f32 {
    (body_max(viewport) * LIST_SHARE).clamp(LIST_MIN, sheet::LIST_MAX)
}
const PRISM_FILTER_FROM: usize = 6;
const SNUG_PERCENT: u64 = 80;

fn local_files(id: &'static str, names: &[String], scroll: &ScrollHandle, max: f32, t: &Theme) -> Div {
    let mut list = div().id(id).flex().flex_col().max_h(px(max)).overflow_y_scroll().track_scroll(scroll).rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS));
    for name in names {
        list = list.child(div().pl(px(space::M)).pr(px(space::XS)).py(px(space::XS)).flex().items_center().gap(px(space::S)).child(icon("package", iconsize::S, t.tertiary)).child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).child(name.clone())));
    }
    div().contain_scroll(scroll).child(list)
}

fn unsupported_text(reason: &str) -> String {
    if let Some(loader) = reason.strip_prefix("uses ").and_then(|r| r.split(',').next()) {
        trf("Nutzt %@. Nur Fabric geht.", &[&loader])
    } else if reason.contains("no mod loader") {
        tr("Läuft ohne Fabric. Nur Fabric-Instanzen gehen.")
    } else if reason.contains("Minecraft version") {
        tr("Ohne Minecraft-Version.")
    } else if reason.contains("mmc-pack") {
        tr("Die Instanz ist unvollständig oder beschädigt.")
    } else {
        tr("Diese Instanz lässt sich nicht übernehmen.")
    }
}

fn default_note(note: &str, version: &str) -> String {
    let Some(rest) = note.strip_prefix("skipped ") else { return note.to_owned() };
    let slug = rest.split(':').next().unwrap_or(rest);
    let title = DEFAULT_TITLES.iter().find(|(s, _)| *s == slug).map_or(slug, |(_, t)| t);
    trf("%@ fehlt, weil es keine Version für %@ gibt.", &[&title, &version])
}

impl Render for BatchSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_focus(window, cx);
        self.list_max = list_max(window.viewport_size().height.as_f32());
        let t = Theme::get(cx);
        let editing = self.original.is_some();
        let (title, subtitle) = if self.imported.is_some() {
            (tr("Batch erstellt"), None)
        } else if self.prism_picking {
            (tr("Aus Prism übernehmen"), Some(tr("Instanzen aus Prism Launcher. Nur Fabric-Instanzen lassen sich übernehmen.")))
        } else if self.searching {
            (tr("Mod hinzufügen"), Some(trf("Fabric-Mods von Modrinth für Minecraft %@", &[&self.version])))
        } else if editing {
            (tr("Batch bearbeiten"), None)
        } else {
            (tr("Batch erstellen"), Some(tr("Alle Accounts eines Batches spielen dieselbe Version mit denselben Mods.")))
        };
        let body = if self.imported.is_some() {
            self.render_imported(&t)
        } else if self.prism_picking {
            self.render_prism_picker(cx, &t)
        } else if self.searching {
            self.render_search(cx, &t)
        } else {
            self.render_form(cx, &t)
        };
        let max_body = body_max(window.viewport_size().height.as_f32());
        let (leading, trailing): (Vec<AnyElement>, Vec<AnyElement>) = if self.imported.is_some() {
            (Vec::new(), vec![sheet::button("import-done", tr("Fertig"), None, Look::Primary, false, false, &t).track_focus(&self.done_focus).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))).into_any_element()])
        } else if self.prism_picking {
            (vec![sheet::button("prism-back", tr("Zurück"), Some("chevron-left"), Look::Secondary, false, false, &t).on_click(cx.listener(|s, _, window, cx| s.open_prism(false, window, cx))).into_any_element()], Vec::new())
        } else if self.searching {
            (
                vec![sheet::button("search-back", tr("Zurück"), Some("chevron-left"), Look::Secondary, false, false, &t).on_click(cx.listener(|s, _, window, cx| s.open_search(false, window, cx))).into_any_element()],
                vec![sheet::button("search-done", tr("Fertig"), None, Look::Primary, false, false, &t).on_click(cx.listener(|s, _, window, cx| s.open_search(false, window, cx))).into_any_element()],
            )
        } else {
            let running = self.running(cx);
            let leading = if editing {
                let delete = sheet::button("batch-delete", tr("Batch löschen"), None, Look::Danger, running || self.saving, false, &t);
                let delete = if running { delete.tooltip(super::widgets::tip(tr("Stoppe den Batch, bevor du ihn löschst."))) } else { delete.on_click(cx.listener(|s, _, window, cx| s.ask_delete(window, cx))) };
                vec![delete.into_any_element()]
            } else {
                Vec::new()
            };
            let blocked = self.mods_busy.is_some() || !self.updating.is_empty() || !self.browser.adding.is_empty();
            let (name_problem, server_problem, version_problem) = self.problems(cx);
            let invalid = name_problem.is_some() || server_problem.is_some() || version_problem.is_some();
            let mut trailing = Vec::new();
            if let Some(e) = &self.save_error {
                trailing.push(div().max_w(px(SAVE_ERROR_MAX)).child(self.problem("save-error", "circle-alert", t.danger_text, e, cx, &t)).into_any_element());
            }
            trailing.push(sheet::button("batch-cancel", tr("Abbrechen"), None, Look::Secondary, !self.closable(), false, &t).on_click(cx.listener(|s, _, _, cx| if s.closable() { cx.emit(Dismissed) })).into_any_element());
            trailing.push(sheet::button("batch-save", if editing { tr("Sichern") } else { tr("Erstellen") }, None, Look::Primary, invalid || blocked, self.saving, &t).on_click(cx.listener(|s, _, _, cx| s.save(cx))).into_any_element());
            (leading, trailing)
        };
        let me = cx.entity().downgrade();
        div()
            .id("batch-sheet")
            .track_focus(&self.root)
            .capture_key_down(move |e: &KeyDownEvent, window, cx| {
                let handled = me.update(cx, |s, cx| s.browse_key(e.keystroke.key.as_str(), e.keystroke.modifiers, window, cx));
                if handled.unwrap_or(false) {
                    cx.stop_propagation();
                    return;
                }
                let delta = match e.keystroke.key.as_str() {
                    "up" => -1,
                    "down" => 1,
                    _ => return,
                };
                if e.keystroke.modifiers.modified() {
                    return;
                }
                let stepped = me.update(cx, |s, cx| {
                    let picking = s.prism_picking;
                    if picking {
                        s.step_prism(delta, cx);
                    }
                    picking
                });
                if stepped.unwrap_or(false) {
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
                if s.prism_picking {
                    s.open_prism(false, window, cx);
                } else if s.searching {
                    s.browse_cancel(window, cx);
                } else if s.closable() {
                    cx.propagate();
                }
            }))
            .child(sheet::header(title, subtitle, &t))
            .child(div().id("batch-body").track_scroll(&self.body_scroll).max_h(px(max_body)).overflow_y_scroll().px(px(space::XL)).pb(px(space::L)).child(body))
            .child(sheet::footer(leading, trailing, &t))
    }
}

const SAVE_ERROR_MAX: f32 = 220.;

#[derive(Clone, Copy)]
enum Dial {
    Fps,
    Sight,
}

#[cfg(test)]
mod tests {
    use super::{LIST_MIN, body_max, list_max};
    use crate::ui::sheet;

    #[test]
    fn lists_leave_room_for_the_sheet_to_scroll_in_a_small_window() {
        let body = body_max(420.);
        assert!(list_max(420.) <= body * 0.6);
        assert!(list_max(420.) < sheet::LIST_MAX);
    }

    #[test]
    fn lists_keep_their_full_height_in_a_tall_window() {
        assert_eq!(list_max(1200.), sheet::LIST_MAX);
        assert!(list_max(300.) >= LIST_MIN);
    }
}
