//! The sheet to create or edit a batch: version, server, mods, memory and accounts.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::{IndexPath, WindowExt};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{AddAccount, EditBatch, NewBatch, Reload};
use super::drive::Mark;
use super::forms;
use super::motion::Soft;
use super::remote;
use super::sheet::{self, Dismissed, Look};
use super::theme::{Theme, control, font, iconsize, radius, space, tint, with_alpha};
use super::widgets::{KeyNav, icon};
use crate::core::account::{Account, AccountKind};
use crate::core::batch::{Batch, ModRef};
use crate::core::i18n::{tr, trf};
use crate::core::modrinth::{Hit, Update};
use crate::core::prism::Instance;

const SEARCH_DELAY: Duration = Duration::from_millis(250);
const FILTER_FROM: usize = 12;
const CHROME_HEIGHT: f32 = 150.;
const DEFAULT_TITLES: [(&str, &str); 5] = [("fabric-api", "Fabric API"), ("lithium", "Lithium"), ("ferrite-core", "FerriteCore"), ("modernfix", "ModernFix"), ("krypton", "Krypton")];

/// Registers the batch sheet actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &NewBatch, cx| open(None, cx));
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

enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

struct BatchSheet {
    original: Option<Batch>,
    name: Entity<InputState>,
    server: Entity<InputState>,
    filter: Entity<InputState>,
    query: Entity<InputState>,
    versions: Load<()>,
    version_list: Vec<String>,
    version_select: Entity<SelectState<SearchableVec<String>>>,
    pending_versions: bool,
    version: String,
    memory: u32,
    memory_touched: bool,
    advanced: bool,
    memory_max: u32,
    total_ram: u64,
    accounts: HashSet<Uuid>,
    mods: Vec<ModRef>,
    mods_touched: bool,
    mods_busy: Option<String>,
    mods_error: Option<String>,
    notes: Vec<String>,
    searching: bool,
    results: Option<Load<Vec<Hit>>>,
    search_gen: u64,
    adding: HashSet<String>,
    add_error: Option<String>,
    prism_list: Vec<Instance>,
    prism_choice: Option<Instance>,
    prism_picking: bool,
    imported: Option<Batch>,
    updates: Option<Load<Vec<Update>>>,
    updates_gen: u64,
    updating: HashSet<String>,
    update_error: Option<String>,
    updates_note: Option<String>,
    saving: bool,
    save_error: Option<String>,
    tried: bool,
    focus_next: bool,
    root: FocusHandle,
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
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. Farm")).default_value(name_value));
        let server = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. play.example.net")).default_value(server_value));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Accounts filtern")));
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Mods suchen, z. B. Sodium")));
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
            cx.subscribe(&query, |s: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    s.schedule_search(cx);
                }
            }),
            cx.subscribe_in(&version_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<String>>, _, cx| {
                let SelectEvent::Confirm(Some(v)) = e else { return };
                s.change_version(v.clone(), cx);
            }),
        ];
        let editing = original.is_some();
        let mut sheet = Self {
            mods: original.as_ref().map(|b| b.mods.clone()).unwrap_or_default(),
            mods_touched: editing,
            original,
            name,
            server,
            filter,
            query,
            versions: Load::Loading,
            version_list: initial,
            version_select,
            pending_versions: false,
            version,
            memory,
            memory_touched: false,
            advanced: false,
            memory_max,
            total_ram,
            accounts,
            mods_busy: None,
            mods_error: None,
            notes: Vec::new(),
            searching: false,
            results: None,
            search_gen: 0,
            adding: HashSet::new(),
            add_error: None,
            prism_list: Vec::new(),
            prism_choice: None,
            prism_picking: false,
            imported: None,
            updates: None,
            updates_gen: 0,
            updating: HashSet::new(),
            update_error: None,
            updates_note: None,
            saving: false,
            save_error: None,
            tried: false,
            focus_next: true,
            root: cx.focus_handle(),
            _subs: subs,
        };
        sheet.load_versions(cx);
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

    fn open_prism(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.prism_picking = open;
        self.focus_next = !open;
        if open {
            window.focus(&self.root, cx);
        }
        cx.notify();
    }

    fn choose_prism(&mut self, instance: Instance, window: &mut Window, cx: &mut Context<Self>) {
        let Some(game) = instance.game_version.clone().filter(|_| instance.unsupported.is_none()) else { return };
        let taken: Vec<String> = super::model::get(cx).read(cx).batches.iter().map(|b| b.name.clone()).collect();
        let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
        let name = forms::unique_name(&instance.name, &taken);
        self.name.update(cx, |s, cx| s.set_value(name, window, cx));
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
        if self.prism_choice.take().is_none() {
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
                    Err(e) => Load::Failed(e.root_cause().to_string()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_updates(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        let Some(Load::Ready(list)) = &self.updates else { return };
        let Some(id) = self.original.as_ref().map(|b| b.id.clone()) else { return };
        let selection: Vec<Update> = list.iter().filter(|u| only.as_ref().is_none_or(|p| *p == u.current.project_id)).cloned().collect();
        if selection.is_empty() || !self.updating.is_empty() {
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
                        if let Some(Load::Ready(list)) = &mut s.updates {
                            list.retain(|u| !selection.contains(u));
                        }
                        s.updates_note = Some(if running { trf("%lld Updates vorgemerkt. Sie gelten beim nächsten Start.", &[&selection.len()]) } else if selection.len() == 1 { tr("1 Mod aktualisiert.") } else { trf("%lld Mods aktualisiert.", &[&selection.len()]) });
                        s.original = Some(batch);
                        cx.dispatch_action(&Reload);
                    }
                    Err(e) => s.update_error = Some(trf("Nicht aktualisiert: %@", &[&e.root_cause()])),
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
                    Ok(_) => s.versions = Load::Failed(tr("Mojang hat keine Versionen geliefert.")),
                    Err(e) => s.versions = Load::Failed(e.root_cause().to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn change_version(&mut self, version: String, cx: &mut Context<Self>) {
        if version == self.version || self.mods_busy.is_some() {
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
                        s.mods_error = Some(trf("Mods nicht geladen: %@", &[&e.root_cause()]));
                    }
                    Err(e) => {
                        s.mods_error = Some(trf("Mods für %@ nicht geladen, die Version bleibt %@: %@", &[&version, &previous, &e.root_cause()]));
                        s.version = previous;
                        s.pending_versions = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search_gen += 1;
        let generation = self.search_gen;
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.results = None;
            cx.notify();
            return;
        }
        let version = self.version.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            let current = this.update(cx, |s, cx| {
                let current = s.search_gen == generation;
                if current && !matches!(s.results, Some(Load::Ready(_))) {
                    s.results = Some(Load::Loading);
                    cx.notify();
                }
                current
            });
            if !matches!(current, Ok(true)) {
                return;
            }
            let result = cx.background_executor().spawn(async move { remote::search(&query, &version) }).await;
            let _ = this.update(cx, |s, cx| {
                if s.search_gen != generation {
                    return;
                }
                s.results = Some(match result {
                    Ok(hits) => Load::Ready(hits),
                    Err(e) => Load::Failed(e.root_cause().to_string()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn add_mod(&mut self, hit: &Hit, cx: &mut Context<Self>) {
        if self.adding.contains(&hit.project_id) || self.mods.iter().any(|m| m.project_id == hit.project_id) {
            return;
        }
        self.adding.insert(hit.project_id.clone());
        self.add_error = None;
        cx.notify();
        let (id, title, version) = (hit.project_id.clone(), hit.title.clone(), self.version.clone());
        let ids = vec![id.clone()];
        let task = cx.background_executor().spawn(async move { remote::resolve(&ids, &version) });
        let version = self.version.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.adding.remove(&id);
                if s.version != version {
                    return;
                }
                match result {
                    Ok(found) => {
                        s.mods_touched = true;
                        for m in found {
                            if !s.mods.iter().any(|have| have.project_id == m.project_id) {
                                s.mods.push(m);
                            }
                        }
                    }
                    Err(e) => s.add_error = Some(trf("„%@“ ließ sich nicht hinzufügen: %@", &[&title, &e.root_cause()])),
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

    fn open_search(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = open;
        self.add_error = None;
        if open {
            self.query.update(cx, |q, cx| q.focus(window, cx));
        } else {
            window.focus(&self.root, cx);
        }
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
        if self.saving || name.is_some() || server.is_some() || version.is_some() || self.mods_busy.is_some() || !self.updating.is_empty() {
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
                            }
                        }
                        Err(e) => s.save_error = Some(trf("Nicht übernommen: %@", &[&e.root_cause()])),
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
                    Err(e) => s.save_error = Some(trf("Nicht gespeichert: %@", &[&e])),
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
                                Err(e) => s.save_error = Some(trf("Nicht gelöscht: %@", &[&e])),
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
        if !sheet::typing(window) {
            cx.propagate();
            return;
        }
        if self.imported.is_some() {
            cx.emit(Dismissed);
        } else if !self.searching && !self.prism_picking {
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
                .child(div().flex_1().min_w_0().child(sheet::status("circle-alert", t.danger_text, trf("Versionen nicht geladen: %@", &[why]))))
                .child(sheet::button("versions-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.load_versions(cx)))),
            Load::Loading if self.version_list.is_empty() => div().h(px(control::M)).flex().items_center().child(sheet::waiting(tr("Versionen werden geladen …"), t)),
            _ => div().child(Select::new(&self.version_select).menu_max_h(px(sheet::LIST_MAX)).search_placeholder(tr("Version suchen")).disabled(self.mods_busy.is_some() || self.prism_choice.is_some())),
        };
        let mut body = div().flex().flex_col().gap(px(space::M));
        if running {
            body = body.child(sheet::notice("info", t.accent, t.accent_text, tr("Dieser Batch läuft. Änderungen gelten beim nächsten Start.")));
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
            .child(self.render_memory(cx, t));
        body
    }

    fn render_memory(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let count = self.accounts.len().max(1);
        let total = u64::from(self.memory) * count as u64;
        let total_label = forms::memory_label(u32::try_from(total).unwrap_or(u32::MAX));
        let over = total > self.total_ram;
        let at_min = self.memory <= forms::MEMORY_MIN;
        let at_max = self.memory >= self.memory_max;
        let open = self.advanced || over;
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
                return div().id(id).mark("button", label.clone()).size(px(control::M)).flex().items_center().justify_center().opacity(tint::DISABLED).tooltip(super::widgets::tip(label)).child(icon(icon_name, iconsize::M, t.tertiary));
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
            .child(div().id("memory-value").mark("value", forms::memory_label(self.memory)).min_w(px(MEMORY_VALUE_WIDTH)).text_center().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(forms::memory_label(self.memory)))
            .child(step("memory-more", "plus", tr("Mehr Arbeitsspeicher"), at_max, 1, cx));
        let line = if over {
            sheet::note(None, Some(trf("%lld Accounts brauchen zusammen bis zu %@, mehr als dieser Rechner hat (%@).", &[&count, &total_label, &forms::memory_label(u32::try_from(self.total_ram).unwrap_or(u32::MAX))])), t)
        } else if count > 1 {
            sheet::note(Some(trf("Pro Account. %lld Accounts brauchen zusammen bis zu %@.", &[&count, &total_label])), None, t)
        } else {
            sheet::note(Some(tr("Pro Account.")), None, t)
        };
        col.child(div().pl(px(space::L)).flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Arbeitsspeicher"), t)).child(stepper).child(line))
    }

    fn render_accounts(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let accounts = super::model::get(cx).read(cx).accounts.clone();
        let header = div().flex().items_center().gap(px(space::S)).child(div().flex_1().child(sheet::label(trf("Accounts (%lld von %lld)", &[&accounts.iter().filter(|a| self.accounts.contains(&a.id)).count(), &accounts.len()]), t)));
        if accounts.is_empty() {
            let add = sheet::button("batch-add-account", tr("Account hinzufügen"), Some("plus"), Look::Plain, false, false, t).on_click(|_, window, cx| {
                window.close_dialog(cx);
                window.dispatch_action(Box::new(AddAccount), cx);
            });
            return div().flex().flex_col().gap(px(space::XS)).child(header).child(div().flex().items_center().gap(px(space::S)).child(div().flex_1().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Noch keine Accounts. Du kannst sie später hinzufügen."))).child(add));
        }
        let filter = if accounts.len() > FILTER_FROM { self.filter.read(cx).value().trim().to_lowercase() } else { String::new() };
        let shown: Vec<&Account> = accounts.iter().filter(|a| filter.is_empty() || a.name.to_lowercase().contains(&filter)).collect();
        let ids: Vec<Uuid> = shown.iter().map(|a| a.id).collect();
        let all = !ids.is_empty() && ids.iter().all(|id| self.accounts.contains(id));
        let (pick, unpick) = if filter.is_empty() { (tr("Alle auswählen"), tr("Keine auswählen")) } else { (tr("Alle gefilterten auswählen"), tr("Gefilterte abwählen")) };
        let header = header.child(sheet::button("accounts-all", if all { unpick } else { pick }, None, Look::Plain, ids.is_empty(), false, t).on_click(cx.listener(move |s, _, _, cx| {
            if all {
                s.accounts.retain(|id| !ids.contains(id));
            } else {
                s.accounts.extend(ids.iter().copied());
            }
            cx.notify();
        })));
        let frame = div().rounded(px(radius::CONTROL)).border_1().border_color(t.separator);
        let list = if shown.is_empty() {
            frame.py(px(space::XS)).child(div().px(px(space::M)).py(px(space::S)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Kein Account passt zu „%@“.", &[&filter])))
        } else {
            let rows: Vec<(Uuid, String, String)> = shown.iter().map(|a| (a.id, a.name.clone(), if a.kind == AccountKind::Microsoft { tr("Microsoft") } else { tr("Offline") })).collect();
            let count = rows.len();
            frame.child(
                uniform_list("account-list", count, cx.processor(move |s, range: std::ops::Range<usize>, _, cx| rows[range].iter().map(|(id, name, kind)| s.pick_row(*id, name.clone(), kind.clone(), cx)).collect::<Vec<_>>()))
                    .with_sizing_behavior(ListSizingBehavior::Infer)
                    .px(px(space::XS))
                    .py(px(space::XS))
                    .max_h(px(sheet::LIST_MAX)),
            )
        };
        div().flex().flex_col().gap(px(space::XS)).child(header).when(accounts.len() > FILTER_FROM, |d| d.child(Input::new(&self.filter))).child(list)
    }

    fn pick_row(&mut self, id: Uuid, name: String, kind: String, cx: &mut Context<Self>) -> Stateful<Div> {
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
            .w_full()
            .px(px(space::S))
            .py(px(space::XS))
            .flex()
            .items_center()
            .gap(px(space::S))
            .rounded(px(radius::SMALL))
            .cursor_pointer()
            .soft(t.ink.opacity(tint::HOVER), radius::SMALL)
            .on_click(cx.listener(move |s, _, _, cx| toggle(s, cx)))
            .child(Checkbox::new(SharedString::from(format!("pick-{}", id.simple()))).checked(checked).label(name).on_click(cx.listener(move |s, _: &bool, _, cx| {
                toggle(s, cx);
                cx.stop_propagation();
            })))
            .child(div().flex_1())
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
            col = col.child(sheet::status("circle-alert", t.danger_text, trf("Mods nicht geladen: %@", &[e])));
        }
        if self.mods.is_empty() {
            col = col.child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Keine Mods. Der Batch startet mit Fabric allein.")));
        } else {
            let mut list = div().id("mod-list").flex().flex_col().max_h(px(sheet::LIST_MAX)).overflow_y_scroll().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS));
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
            col = col.child(list);
        }
        for n in &self.notes {
            col = col.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(n.clone()));
        }
        if let Some(updates) = self.render_updates(cx, t) {
            col = col.child(updates);
        }
        col
    }

    fn render_prism_entry(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        if self.prism_list.is_empty() || self.original.is_some() {
            return None;
        }
        let blocked = self.mods_busy.is_some() || self.saving;
        let card = |id: &'static str, title: String, subtitle: String| {
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
                .child(div().size(px(control::M)).flex_none().rounded(px(radius::INNER)).bg(with_alpha(t.accent, tint::ATTENTION)).flex().items_center().justify_center().child(icon("download", iconsize::M, t.accent_text)))
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title)).child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(subtitle)))
        };
        let entry = match self.prism_choice.clone() {
            None => {
                let row = card("prism-pick", tr("Aus Prism-Instanz übernehmen"), tr("Version, Mods und Einstellungen aus Prism Launcher"));
                let row = if blocked { row.opacity(tint::DISABLED) } else { row.cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::CONTROL).on_click(cx.listener(|s, _, window, cx| s.open_prism(true, window, cx))) };
                row.child(icon("chevron-right", iconsize::M, t.tertiary))
            }
            Some(instance) => {
                let subtitle = trf("Aus Prism · Minecraft %@ · %lld Mods", &[&instance.game_version.clone().unwrap_or_default(), &instance.mods]);
                card("prism-chosen", instance.name.clone(), subtitle)
                    .child(sheet::icon_button("prism-change", "list", tr("Andere Instanz wählen"), t.secondary, t).on_click(cx.listener(|s, _, window, cx| s.open_prism(true, window, cx))))
                    .child(sheet::icon_button("prism-drop", "x", tr("Nicht aus Prism übernehmen"), t.secondary, t).on_click(cx.listener(|s, _, _, cx| s.drop_prism(cx))))
            }
        };
        Some(div().child(entry))
    }

    fn render_prism_mods(&self, instance: &Instance, t: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(sheet::label(trf("Mods (%lld)", &[&instance.mods]), t))
            .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Mods, Config und Einstellungen werden aus der Instanz kopiert; Prism bleibt unverändert. Beim Erstellen wird geprüft, welche Mods auf Modrinth liegen. Die übrigen bleiben als eigene Dateien im Batch.")))
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
            Some(Load::Ready(list)) => list.len(),
            _ => 0,
        };
        let mut header = div().flex().items_center().gap(px(space::S)).child(div().flex_1().child(sheet::label(if count > 0 { trf("Updates (%lld)", &[&count]) } else { tr("Updates") }, t)));
        if count > 1 {
            header = header.child(sheet::button("updates-all", tr("Alle aktualisieren"), Some("refresh-cw"), Look::Secondary, false, busy, t).on_click(cx.listener(|s, _, _, cx| s.apply_updates(None, cx))));
        }
        col = col.child(header);
        if let Some(note) = &self.updates_note {
            col = col.child(sheet::status("circle-check", t.success_text, note.clone()));
        }
        if let Some(e) = &self.update_error {
            col = col.child(sheet::status("circle-alert", t.danger_text, e.clone()));
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
                        .child(div().flex_1().min_w_0().child(sheet::status("wifi-off", t.danger_text, trf("Updates nicht geladen: %@", &[why]))))
                        .child(sheet::button("updates-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.load_updates(cx)))),
                );
            }
            Some(Load::Ready(list)) if list.is_empty() => {
                if self.updates_note.is_none() {
                    col = col.child(sheet::status("circle-check", t.success_text, tr("Alle Mods sind aktuell.")));
                }
            }
            Some(Load::Ready(list)) => {
                let mut rows = div().flex().flex_col().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS));
                for u in list.clone() {
                    let pid = u.current.project_id.clone();
                    let working = self.updating.contains(&pid);
                    let action = sheet::button(SharedString::from(format!("update-{pid}")), tr("Aktualisieren"), None, Look::Plain, busy, working, t).on_click(cx.listener(move |s, _, _, cx| s.apply_updates(Some(pid.clone()), cx)));
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
                                    .child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(format!("{} → {}", u.current.file_name, u.latest.file_name))),
                            )
                            .child(action),
                    );
                }
                col = col.child(rows);
                if running {
                    col = col.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Gilt beim nächsten Start.")));
                }
            }
        }
        Some(col)
    }

    fn render_prism_picker(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let mut list = div().id("prism-list").flex().flex_col().gap(px(space::XXS)).max_h(px(RESULTS_MAX)).overflow_y_scroll();
        for instance in self.prism_list.clone() {
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
                .rounded(px(radius::CONTROL));
            let chosen = self.prism_choice.as_ref().is_some_and(|c| c.id == instance.id);
            let row = if ok {
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
        div().flex().flex_col().gap(px(space::S)).child(list)
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
        let mut list = div().id("local-mods").flex().flex_col().max_h(px(sheet::LIST_MAX)).overflow_y_scroll().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).py(px(space::XXS));
        for name in &batch.local_mods {
            list = list.child(div().pl(px(space::M)).pr(px(space::XS)).py(px(space::XS)).flex().items_center().gap(px(space::S)).child(icon("package", iconsize::S, t.tertiary)).child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).child(name.clone())));
        }
        col = col.child(list).child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Diese Dateien bleiben unverändert im Batch. Sie werden nicht automatisch aktualisiert und beim Synchronisieren nie gelöscht.")));
        col
    }

    fn render_search(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let field = Input::new(&self.query).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true);
        let mut col = div().flex().flex_col().gap(px(space::M)).child(field);
        if let Some(e) = &self.add_error {
            col = col.child(sheet::status("circle-alert", t.danger_text, e.clone()));
        }
        let query = self.query.read(cx).value().trim().to_string();
        let content = match &self.results {
            None => div().py(px(space::L)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Suche nach Fabric-Mods auf Modrinth, die es für %@ gibt.", &[&self.version])),
            Some(Load::Loading) => div().py(px(space::L)).child(sheet::waiting(tr("Suche läuft …"), t)),
            Some(Load::Failed(why)) => div()
                .py(px(space::S))
                .flex()
                .items_center()
                .gap(px(space::S))
                .child(div().flex_1().min_w_0().child(sheet::status("wifi-off", t.danger_text, trf("Modrinth ist nicht erreichbar: %@", &[why]))))
                .child(sheet::button("search-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.schedule_search(cx)))),
            Some(Load::Ready(hits)) if hits.is_empty() => div().py(px(space::L)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(trf("Keine Mods zu „%@“ für %@. Versuche einen anderen Suchbegriff.", &[&query, &self.version])),
            Some(Load::Ready(hits)) => {
                let mut list = div().id("results").flex().flex_col().gap(px(space::XXS)).max_h(px(RESULTS_MAX)).overflow_y_scroll();
                for hit in hits.clone() {
                    let added = self.mods.iter().any(|m| m.project_id == hit.project_id);
                    let adding = self.adding.contains(&hit.project_id);
                    let trailing = if added {
                        sheet::status("check", t.success_text, tr("Hinzugefügt"))
                    } else if adding {
                        sheet::waiting(tr("Wird hinzugefügt …"), t)
                    } else {
                        div().flex().items_center().gap(px(space::XXS)).text_size(px(font::CALLOUT)).text_color(t.accent_text).child(icon("plus", iconsize::S, t.accent_text)).child(tr("Hinzufügen"))
                    };
                    let row = div()
                        .id(SharedString::from(format!("hit-{}", hit.project_id)))
                        .mark("result", hit.title.clone())
                        .flex()
                        .items_center()
                        .gap(px(space::M))
                        .px(px(space::M))
                        .py(px(space::S))
                        .rounded(px(radius::CONTROL));
                    let (title, description, downloads) = (hit.title.clone(), hit.description.clone(), hit.downloads);
                    let row = if added || adding { row } else { row.cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::CONTROL).on_click(cx.listener(move |s, _, window, cx| {
                        s.add_mod(&hit, cx);
                        s.query.update(cx, |q, cx| q.focus(window, cx));
                    })) };
                    list = list.child(
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(space::XXS))
                                .child(div().flex().items_baseline().gap(px(space::S)).child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).child(title)).child(div().flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).child(forms::downloads_label(downloads))))
                                .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).line_clamp(2).child(description)),
                        )
                        .child(div().flex_none().child(trailing)),
                    );
                }
                div().child(list)
            }
        };
        col.child(content)
    }

    fn apply_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                s.focus(window, cx);
                s.set_selected_range(0..end, cx);
            });
        }
    }
}

const VERSION_WIDTH: f32 = 168.;
const MEMORY_VALUE_WIDTH: f32 = 64.;
const RESULTS_MAX: f32 = 340.;

fn unsupported_text(reason: &str) -> String {
    if let Some(loader) = reason.strip_prefix("uses ").and_then(|r| r.split(',').next()) {
        trf("Nutzt %@. Nur Fabric geht.", &[&loader])
    } else if reason.contains("no mod loader") {
        tr("Ohne Mod-Loader. Nur Fabric-Instanzen gehen.")
    } else if reason.contains("Minecraft version") {
        tr("Ohne Minecraft-Version.")
    } else if reason.contains("mmc-pack") {
        tr("Die Instanz ist unvollständig oder beschädigt.")
    } else {
        trf("Nicht möglich: %@", &[&reason])
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
        let max_body = (window.viewport_size().height.as_f32() - 2. * sheet::MARGIN - CHROME_HEIGHT).max(160.);
        let (leading, trailing): (Vec<AnyElement>, Vec<AnyElement>) = if self.imported.is_some() {
            (Vec::new(), vec![sheet::button("import-done", tr("Fertig"), None, Look::Primary, false, false, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))).into_any_element()])
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
            let blocked = self.mods_busy.is_some() || !self.updating.is_empty();
            let (name_problem, server_problem, version_problem) = self.problems(cx);
            let invalid = name_problem.is_some() || server_problem.is_some() || version_problem.is_some();
            let mut trailing = Vec::new();
            if let Some(e) = &self.save_error {
                trailing.push(div().max_w(px(SAVE_ERROR_MAX)).child(sheet::status("circle-alert", t.danger_text, e.clone())).into_any_element());
            }
            trailing.push(sheet::button("batch-cancel", tr("Abbrechen"), None, Look::Secondary, false, false, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))).into_any_element());
            trailing.push(sheet::button("batch-save", if editing { tr("Sichern") } else { tr("Erstellen") }, None, Look::Primary, invalid || blocked, self.saving, &t).on_click(cx.listener(|s, _, _, cx| s.save(cx))).into_any_element());
            (leading, trailing)
        };
        div()
            .id("batch-sheet")
            .track_focus(&self.root)
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .on_action(cx.listener(|s, _: &Confirm, window, cx| s.confirm(window, cx)))
            .on_action(cx.listener(|s, _: &Cancel, window, cx| {
                if s.prism_picking {
                    s.open_prism(false, window, cx);
                } else if s.searching {
                    s.open_search(false, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .child(sheet::header(title, subtitle, &t))
            .child(div().id("batch-body").max_h(px(max_body)).overflow_y_scroll().px(px(space::XL)).pb(px(space::L)).child(body))
            .child(sheet::footer(leading, trailing, &t))
    }
}

const SAVE_ERROR_MAX: f32 = 220.;
