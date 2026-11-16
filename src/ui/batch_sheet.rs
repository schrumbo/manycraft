//! The sheet to create or edit a batch: instance, server, accounts and background.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::base::actions::{Cancel, Confirm};
use gpui_kit::component::{IndexPath, WindowExt};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{AddAccount, DeleteBatch, EditBatch, NewBatch, NewBatchFor, NewBatchWith, Reload, ShowBatch, StartBatch};
use super::drive::Mark;
use super::forms::{self, Problem};
use super::identity;
use super::motion::Soft;
use super::remote;
use super::sheet::{self, Dismissed, Look};
use super::theme::{Theme, control, font, iconsize, radius, space, tint};
use super::widgets::{ContainScroll, KeyNav, TakeFocus, icon, segment, segmented};
use crate::core::account::AccountKind;
use crate::core::batch::{Background, Batch};
use crate::core::i18n::{tr, trf};
use crate::core::instance::{DEFAULT_MEMORY_MB, Loader};
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
const LAUNCH_CHOICES: [u32; 5] = [0, 1, 2, 4, 8];
const SAVE_ERROR_MAX: f32 = 220.;
const LIST_MIN: f32 = 96.;
const LIST_SHARE: f32 = 0.5;

#[derive(Clone)]
struct Choice {
    id: Option<String>,
    title: String,
    line: String,
}

impl SearchableListItem for Choice {
    type Value = Option<String>;

    fn title(&self) -> SharedString {
        SharedString::from(self.title.clone())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = Theme::get(cx);
        div()
            .flex()
            .items_baseline()
            .gap(px(space::S))
            .child(div().min_w_0().truncate().child(self.title.clone()))
            .when(!self.line.is_empty(), |d| d.child(div().flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).child(self.line.clone())))
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

/// Registers the batch sheet actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &NewBatch, cx| open(None, None, None, cx));
    cx.on_action(|action: &NewBatchFor, cx| open(None, Some(action.0.clone()), None, cx));
    cx.on_action(|action: &NewBatchWith, cx| open(None, None, Some(action.0.iter().filter_map(|id| id.parse().ok()).collect()), cx));
    cx.on_action(|action: &EditBatch, cx| {
        let batch = super::model::get(cx).read(cx).batch(&action.0).cloned();
        if let Some(batch) = batch {
            open(Some(batch), None, None, cx);
        }
    });
    cx.on_action(|action: &DeleteBatch, cx| ask_delete(action.0.clone(), cx));
}

/// Why batch `id` cannot be deleted right now: while any of its games runs.
pub fn delete_blocked(id: &str, cx: &App) -> Option<String> {
    super::model::get(cx).read(cx).games.values().any(|g| g.batch_id == id && g.phase.is_active()).then(|| tr("Stoppe den Batch, bevor du ihn löschst."))
}

fn ask_delete(id: String, cx: &mut App) {
    sheet::in_window(cx, move |window, cx| {
        if sheet::busy(window, cx) {
            return;
        }
        let Some(batch) = super::model::get(cx).read(cx).batch(&id).cloned() else { return };
        confirm_delete(batch, window, cx, |_, _| {});
    });
}

fn confirm_delete(batch: Batch, window: &mut Window, cx: &mut App, deleted: impl Fn(&mut Window, &mut App) + 'static) {
    let blocked_id = batch.id.clone();
    sheet::confirm_danger(
        window,
        cx,
        trf("„%@“ löschen?", &[&batch.name]),
        tr("Der Batch verschwindet aus der Liste. Instanz, Welten und Einstellungen bleiben erhalten. Solange Manycraft offen ist, kannst du das rückgängig machen."),
        tr("Löschen"),
        move |cx| delete_blocked(&blocked_id, cx),
        move |window, cx| {
            delete(batch.clone(), cx);
            deleted(window, cx);
        },
    );
}

fn delete(batch: Batch, cx: &mut App) {
    if delete_blocked(&batch.id, cx).is_some() {
        return;
    }
    let task = cx.background_spawn({
        let batch = batch.clone();
        async move { remote::delete_batch(&batch) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(()) => {
                    super::model::get(cx).update(cx, |m, cx| {
                        m.batches.retain(|b| b.id != batch.id);
                        cx.notify();
                    });
                    super::undo::offer(super::undo::Kind::BatchDeleted { batch }, cx);
                }
                Err(e) => super::undo::note(forms::explain_as(tr("Nicht gelöscht."), &e).text, cx),
            }
            super::state::reload(cx);
        });
    })
    .detach();
}

fn open(batch: Option<Batch>, instance: Option<String>, picked: Option<HashSet<Uuid>>, cx: &mut App) {
    sheet::in_window(cx, move |window, cx| {
        if sheet::busy(window, cx) {
            return;
        }
        let view = cx.new(|cx| BatchSheet::new(batch, instance, picked, window, cx));
        sheet::show(view, sheet::WIDE, window, cx);
    });
}

pub(super) enum Load<T> {
    Loading,
    Ready(T),
    Failed(Problem),
}

struct BatchSheet {
    original: Option<Batch>,
    name: Entity<InputState>,
    name_auto: Option<String>,
    server: Entity<InputState>,
    filter: Entity<InputState>,
    instance: Option<String>,
    instance_select: Entity<SelectState<SearchableVec<Choice>>>,
    versions: Load<()>,
    versions_asked: bool,
    version_list: Vec<String>,
    version_select: Entity<SelectState<SearchableVec<String>>>,
    pending_versions: bool,
    version: String,
    inherit: bool,
    memory: u32,
    background: Background,
    kept_custom: Option<Background>,
    launch_parallel: u32,
    advanced: bool,
    memory_max: u32,
    total_ram: u64,
    free_ram: Option<u64>,
    accounts: HashSet<Uuid>,
    pick_rows: Option<PickRows>,
    rows_dirty: bool,
    skin_gen: u64,
    body_scroll: ScrollHandle,
    account_scroll: UniformListScrollHandle,
    list_max: f32,
    saving: bool,
    save_error: Option<Problem>,
    details: HashSet<&'static str>,
    tried: bool,
    focus_next: bool,
    root: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for BatchSheet {}

impl BatchSheet {
    fn new(original: Option<Batch>, preselect: Option<String>, picked: Option<HashSet<Uuid>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = super::model::get(cx).read(cx);
        let mut listed: Vec<&crate::core::instance::Instance> = model.instances.iter().collect();
        listed.sort_by_key(|i| (i.name.to_lowercase(), i.id.clone()));
        let first = listed.first().map(|i| i.id.clone());
        let instance = match (&original, preselect) {
            (Some(b), _) => Some(b.instance_id.clone()),
            (None, Some(id)) => Some(id).filter(|id| model.instance(id).is_some()).or(first),
            (None, None) => first,
        };
        let mut choices: Vec<Choice> = listed.iter().map(|i| Choice { id: Some(i.id.clone()), title: i.name.clone(), line: i.version_line() }).collect();
        choices.push(Choice { id: None, title: tr("Neue Instanz…"), line: String::new() });
        let selected = choices.iter().position(|c| c.id == instance).map(IndexPath::new);
        let many = choices.len() > FILTER_FROM;
        let taken: Vec<&str> = model.batches.iter().map(|b| b.name.as_str()).collect();
        let name_value = original.as_ref().map_or_else(|| forms::next_batch_name(&taken), |b| b.name.clone());
        let server_value = original.as_ref().and_then(|b| b.server.clone()).unwrap_or_default();
        let accounts: HashSet<Uuid> = match (&original, picked) {
            (Some(b), _) => b.accounts.iter().copied().collect(),
            (None, Some(picked)) => picked,
            (None, None) => model.accounts.iter().map(|a| a.id).collect(),
        };
        let name_auto = original.is_none().then(|| name_value.clone());
        let total_ram = forms::total_ram_mb();
        let memory_max = forms::memory_max(total_ram);
        let own = instance.as_deref().and_then(|id| model.instance(id)).map_or(DEFAULT_MEMORY_MB, |i| i.memory_mb);
        let memory = forms::memory_step(original.as_ref().and_then(|b| b.memory_override).unwrap_or(own), 0, memory_max);
        let inherit = original.as_ref().is_none_or(|b| b.memory_override.is_none());
        let instance_select = cx.new(|cx| SelectState::new(SearchableVec::new(choices), selected, window, cx).searchable(many));
        let version_select = cx.new(|cx| SelectState::new(SearchableVec::new(Vec::<String>::new()), None, window, cx).searchable(true));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. Farm")).default_value(name_value));
        let server = cx.new(|cx| InputState::new(window, cx).placeholder(tr("z. B. play.example.net")).default_value(server_value));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Accounts filtern")));
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
            }),
            cx.subscribe(&name, |s: &mut Self, name, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    s.save_error = None;
                    if s.name_auto.as_deref().is_some_and(|auto| auto != name.read(cx).value().as_ref()) {
                        s.name_auto = None;
                    }
                    cx.notify();
                }
            }),
            cx.subscribe_in(&server, window, |s: &mut Self, server, e: &InputEvent, window, cx| {
                if !matches!(e, InputEvent::Change) {
                    return;
                }
                s.save_error = None;
                if s.name_auto.is_some() {
                    let taken: Vec<String> = super::model::get(cx).read(cx).batches.iter().map(|b| b.name.clone()).collect();
                    let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
                    let next = forms::batch_name_for(&server.read(cx).value(), &taken);
                    s.name.update(cx, |n, cx| n.set_value(next.clone(), window, cx));
                    s.name_auto = Some(next);
                }
                cx.notify();
            }),
            cx.subscribe(&filter, changed),
            cx.observe(&super::model::get(cx), |s: &mut Self, _, cx| {
                s.rows_dirty = true;
                cx.notify();
            }),
            cx.subscribe_in(&instance_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<Choice>>, _, cx| {
                let SelectEvent::Confirm(Some(v)) = e else { return };
                s.pick_instance(v.clone(), cx);
            }),
            cx.subscribe_in(&version_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<String>>, _, cx| {
                let SelectEvent::Confirm(Some(v)) = e else { return };
                s.version.clone_from(v);
                s.save_error = None;
                cx.notify();
            }),
        ];
        let original_background = original.as_ref().map_or_else(Background::default, |b| b.background);
        let launch_parallel = original.as_ref().map_or(0, |b| b.launch_parallel);
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
            original,
            name,
            name_auto,
            server,
            filter,
            instance,
            instance_select,
            versions: Load::Loading,
            versions_asked: false,
            version_list: Vec::new(),
            version_select,
            pending_versions: false,
            version: String::new(),
            inherit,
            memory,
            background: original_background,
            kept_custom: None,
            launch_parallel,
            advanced: false,
            memory_max,
            total_ram,
            free_ram: None,
            accounts,
            pick_rows: None,
            rows_dirty: true,
            skin_gen: 0,
            body_scroll: ScrollHandle::new(),
            account_scroll: UniformListScrollHandle::new(),
            list_max: sheet::LIST_MAX,
            saving: false,
            save_error: None,
            details: HashSet::new(),
            tried: false,
            focus_next: true,
            root: cx.focus_handle(),
            _subs: subs,
        };
        if sheet.instance.is_none() {
            sheet.load_versions(cx);
        }
        let everyone = super::model::get(cx).read(cx).accounts.clone();
        sheet::warm_heads(everyone, cx);
        sheet
    }

    fn toggle(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if !self.details.remove(key) {
            self.details.insert(key);
        }
        cx.notify();
    }

    fn problem(&self, key: &'static str, icon_name: &str, color: Hsla, problem: &Problem, cx: &mut Context<Self>, t: &Theme) -> Div {
        sheet::problem(key, icon_name, color, problem, self.details.contains(key), cx.listener(move |s, _, _, cx| s.toggle(key, cx)), t)
    }

    fn pick_instance(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.instance = id;
        self.save_error = None;
        if self.instance.is_none() && !self.versions_asked {
            self.load_versions(cx);
        }
        cx.notify();
    }

    fn load_versions(&mut self, cx: &mut Context<Self>) {
        self.versions = Load::Loading;
        self.versions_asked = true;
        cx.notify();
        let task = cx.background_executor().spawn(async { remote::versions() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                match result {
                    Ok(list) if !list.is_empty() => {
                        if s.version.is_empty() {
                            s.version.clone_from(&list[0]);
                        }
                        s.version_list = list;
                        s.pending_versions = true;
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

    fn active(&self, cx: &App) -> bool {
        let Some(batch) = &self.original else { return false };
        super::model::get(cx).read(cx).games.values().any(|g| g.batch_id == batch.id && g.phase.is_active())
    }

    fn instance_memory(&self, cx: &App) -> u32 {
        self.instance.as_deref().and_then(|id| super::model::get(cx).read(cx).instance(id)).map_or(DEFAULT_MEMORY_MB, |i| i.memory_mb)
    }

    fn problems(&self, cx: &App) -> (Option<String>, Option<String>, Option<String>) {
        let name = forms::batch_name_problem(&self.name.read(cx).value());
        let server = forms::server_problem(self.server.read(cx).value().trim());
        let version = (self.instance.is_none() && self.version.is_empty()).then(|| tr("Wähle eine Version."));
        (name, server, version)
    }

    fn picked(&self, cx: &App) -> usize {
        super::model::get(cx).read(cx).accounts.iter().filter(|a| self.accounts.contains(&a.id)).count()
    }

    fn save(&mut self, start: bool, cx: &mut Context<Self>) {
        self.tried = true;
        let start = start && self.original.is_none() && self.picked(cx) > 0;
        let (name, server, version) = self.problems(cx);
        if self.saving || name.is_some() || server.is_some() || version.is_some() {
            cx.notify();
            return;
        }
        let accounts = super::model::get(cx).read(cx).accounts.clone();
        let edit = remote::BatchEdit {
            name: self.name.read(cx).value().trim().to_string(),
            instance: match &self.instance {
                Some(id) => remote::InstancePick::Existing(id.clone()),
                None => remote::InstancePick::New { game_version: self.version.clone(), mods: Vec::new() },
            },
            server: Some(self.server.read(cx).value().trim().to_string()).filter(|s| !s.is_empty()),
            memory_override: (!self.inherit).then_some(self.memory),
            accounts: accounts.iter().map(|a| a.id).filter(|id| self.accounts.contains(id)).collect(),
            background: self.background,
            launch_parallel: self.launch_parallel,
        };
        let id = self.original.as_ref().map(|b| b.id.clone());
        self.saving = true;
        self.save_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let mut edit = edit;
            if let remote::InstancePick::New { game_version, .. } = &edit.instance {
                let game_version = game_version.clone();
                let defaults = remote::defaults(&game_version, &Loader::default())?;
                edit.instance = remote::InstancePick::New { game_version, mods: defaults.mods };
            }
            remote::save_batch(id.as_deref(), edit)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.saving = false;
                match result {
                    Ok(batch) => {
                        cx.dispatch_action(&Reload);
                        cx.emit(Dismissed);
                        if start {
                            cx.dispatch_action(&StartBatch(batch.id.clone()));
                            cx.dispatch_action(&ShowBatch(batch.id));
                        }
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
        confirm_delete(batch, window, cx, move |_, cx| {
            let _ = sheet.update(cx, |_, cx| cx.emit(Dismissed));
        });
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !sheet::typing(window) {
            cx.propagate();
            return;
        }
        self.save(true, cx);
    }

    fn render_instance(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let active = self.active(cx);
        let caption = self.original.as_ref().filter(|b| self.instance.as_ref() != Some(&b.instance_id)).and_then(|b| super::model::get(cx).read(cx).instance(&b.instance_id).map(|i| trf("Welten und Screenshots bleiben bei %@.", &[&i.name])));
        let select = Select::new(&self.instance_select).menu_max_h(px(self.list_max)).search_placeholder(tr("Instanz suchen")).disabled(active || self.saving);
        let picker = match (active, self.saving) {
            (true, _) => sheet::unavailable_over("batch-instance-locked", select, tr("Stoppe zuerst die laufenden Spiele.")),
            (false, true) => sheet::unavailable_over("batch-instance-locked", select, tr("Wird gerade gesichert …")),
            (false, false) => div().child(select),
        };
        let mut col = div().flex().flex_col().gap(px(space::M)).child(sheet::field(tr("Instanz"), picker, caption, None, t));
        if self.instance.is_none() {
            let (_, _, version_problem) = self.problems(cx);
            let control = match &self.versions {
                Load::Failed(why) if self.version_list.is_empty() => div()
                    .flex()
                    .items_center()
                    .gap(px(space::S))
                    .child(div().flex_1().min_w_0().child(self.problem("versions-error", "circle-alert", t.danger_text, why, cx, t)))
                    .child(sheet::button("versions-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| s.load_versions(cx)))),
                Load::Loading if self.version_list.is_empty() => div().h(px(control::M)).flex().items_center().child(sheet::waiting(tr("Versionen werden geladen …"), t)),
                _ => {
                    let select = Select::new(&self.version_select).menu_max_h(px(self.list_max)).search_placeholder(tr("Version suchen")).disabled(self.saving);
                    if self.saving { sheet::unavailable_over("batch-version-locked", select, tr("Wird gerade gesichert …")) } else { div().child(select) }
                }
            };
            col = col.child(sheet::field(tr("Minecraft-Version"), control, None, version_problem.filter(|_| self.tried), t));
        }
        col
    }

    fn render_form(&mut self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let (name_problem, server_problem, _) = self.problems(cx);
        let name_shown = name_problem.filter(|_| self.tried || !self.name.read(cx).value().is_empty());
        let active = self.active(cx);
        let mut body = div().flex().flex_col().gap(px(space::M));
        if active {
            body = body.child(sheet::notice("info", t.accent, t.accent_text, tr("Server und Accounts gelten beim nächsten Start. Die Instanz wechselt nur ein gestoppter Batch.")));
        }
        let ram_here = if self.memory_open() { None } else { self.render_ram(cx, t) };
        body.child(self.render_instance(cx, t))
            .child(sheet::field(tr("Server"), Input::new(&self.server), Some(tr("Optional. Adresse mit oder ohne Port.")), server_problem, t))
            .child(self.render_accounts(cx, t))
            .children(ram_here)
            .child(sheet::field(tr("Name"), Input::new(&self.name), None, name_shown, t))
            .child(self.render_background(active, cx, t))
            .child(self.render_memory(active, cx, t))
    }

    fn memory_open(&self) -> bool {
        self.advanced || !self.inherit || self.launch_parallel != 0
    }

    fn each_memory(&self, cx: &App) -> u32 {
        if self.inherit { self.instance_memory(cx) } else { self.memory }
    }

    fn render_ram(&mut self, cx: &mut Context<Self>, t: &Theme) -> Option<Stateful<Div>> {
        let count = self.picked(cx);
        let (total, budget) = forms::memory_need(count, self.each_memory(cx), self.total_ram)?;
        let lower = forms::memory_fit(count, self.memory_max);
        let need = trf("%lld Accounts brauchen zusammen bis zu %@. Dieser Rechner hat neben dem System nur %@.", &[&count, &forms::memory_label(total), &forms::memory_label(budget)]);
        let text = if lower.is_some() { need } else { format!("{need} {}", tr("Wähle weniger Accounts.")) };
        let fix = lower.map(|each| {
            sheet::button("memory-lower", trf("Auf %@ pro Account senken", &[&forms::memory_label(each)]), None, Look::Secondary, false, false, t).on_click(cx.listener(move |s, _, _, cx| {
                if s.saving {
                    return;
                }
                s.inherit = false;
                s.memory = each;
                cx.notify();
            }))
        });
        Some(div().id("ram-over").mark("warning", text.clone()).flex().flex_col().gap(px(space::S)).child(sheet::notice("triangle-alert", t.attention, t.attention_text, text)).children(fix.map(|b| div().flex().child(b))))
    }

    fn render_memory(&mut self, active: bool, cx: &mut Context<Self>, t: &Theme) -> Div {
        let count = self.accounts.len().max(1);
        let each = self.each_memory(cx);
        let total = u64::from(each) * count as u64;
        let total_label = forms::memory_label(u32::try_from(total).unwrap_or(u32::MAX));
        let at_min = self.memory <= forms::MEMORY_MIN;
        let at_max = self.memory >= self.memory_max;
        let open = self.memory_open();
        let label = if open { tr("Erweitert") } else { trf("Erweitert · %@ Arbeitsspeicher pro Account", &[&forms::memory_label(each)]) };
        let disclosure = div()
            .id("batch-advanced")
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
            .child(label)
            .on_click(cx.listener(move |s, _, _, cx| {
                s.advanced = !open;
                cx.notify();
            }));
        let col = div().flex().flex_col().gap(px(space::S)).child(disclosure);
        if !open {
            return col;
        }
        let like = trf("Wie die Instanz (%@)", &[&forms::memory_label(self.instance_memory(cx))]);
        let toggle = |s: &mut Self, cx: &mut Context<Self>| {
            if s.saving {
                return;
            }
            s.inherit = !s.inherit;
            if !s.inherit {
                s.memory = forms::memory_step(s.instance_memory(cx), 0, s.memory_max);
            }
            cx.notify();
        };
        let inherit = div()
            .id("batch-memory-inherit")
            .mark("checkbox", like.clone())
            .self_start()
            .h(px(control::S))
            .px(px(space::XS))
            .ml(px(-space::XS))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .rounded(px(radius::INNER))
            .cursor_pointer()
            .key_nav()
            .soft(t.ink.opacity(tint::HOVER), radius::INNER)
            .on_click(cx.listener(move |s, _, _, cx| toggle(s, cx)))
            .child(Checkbox::new("batch-memory-inherit-check").checked(self.inherit).tab_stop(false).on_click(cx.listener(move |s, _: &bool, _, cx| {
                toggle(s, cx);
                cx.stop_propagation();
            })))
            .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(like));
        let step = |id: &'static str, icon_name: &'static str, label: String, reason: Option<String>, delta: i32, cx: &mut Context<Self>| {
            if let Some(reason) = reason {
                return sheet::stepper_end(div().id(SharedString::from(format!("{id}-end"))).size(px(control::M)).flex().items_center().justify_center().rounded(px(radius::INNER)), icon(icon_name, iconsize::M, t.tertiary), reason);
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
        let line = if count > 1 {
            sheet::note(Some(trf("Pro Account. %lld Accounts brauchen zusammen bis zu %@.", &[&count, &total_label])), None, t)
        } else {
            sheet::note(Some(tr("Pro Account.")), None, t)
        };
        let manual = (!self.inherit).then(|| div().flex().flex_col().gap(px(space::XS)).child(sheet::label(tr("Arbeitsspeicher"), t)).child(stepper));
        col.child(div().pl(px(space::L)).flex().flex_col().gap(px(space::XS)).child(inherit).children(manual).child(line.min_h(px(font::CAPTION * font::LINE * 2.)))).children(self.render_ram(cx, t)).child(self.render_launch(active, cx, t))
    }

    fn render_launch(&self, running: bool, cx: &mut Context<Self>, t: &Theme) -> Div {
        let busy = self.saving;
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let auto = crate::core::stagger::auto_parallel(cores, self.free_ram);
        let hint = match self.launch_parallel {
            0 => trf("Automatisch: höchstens %lld Spiele gleichzeitig, passend zu Prozessor und freiem Arbeitsspeicher. Die übrigen warten.", &[&auto]),
            1 => tr("Ein Spiel startet nach dem anderen. Das schont den Rechner am meisten und dauert am längsten."),
            n => trf("Höchstens %lld Spiele starten gleichzeitig. Mehr geht schneller, der Rechner reagiert dann aber träger. Die übrigen warten.", &[&n]),
        };
        let hint = if running { format!("{hint} {}", tr("Gilt beim nächsten Start.")) } else { hint };
        let segments = LAUNCH_CHOICES.iter().enumerate().map(|(n, count)| {
            let label = if *count == 0 { tr("Automatisch") } else { count.to_string() };
            segment(("launch-parallel", n), label, *count == self.launch_parallel, !busy, t)
        });
        let pick = cx.listener(|s, n: &usize, _, cx| {
            if s.saving {
                return;
            }
            s.launch_parallel = LAUNCH_CHOICES.get(*n).copied().unwrap_or(0);
            s.save_error = None;
            cx.notify();
        });
        let control = segmented("batch-launch-parallel", segments, move |n, window, cx| pick(&n, window, cx), t).when(busy, |d| d.opacity(tint::DISABLED));
        let control = if busy { sheet::unavailable_over("batch-launch-parallel-locked", control, tr("Wird gerade gesichert …")).into_any_element() } else { control.into_any_element() };
        div()
            .pl(px(space::L))
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(sheet::label(tr("Gleichzeitig starten"), t))
            .child(div().flex().child(control))
            .child(sheet::note(Some(hint), None, t).line_height(px(font::CAPTION * font::LINE)).min_h(px(font::CAPTION * font::LINE * 2.)))
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
            Background::Custom { fps, sight } => {
                let line = match forms::custom_cost(fps) {
                    forms::Cost::Little => tr("Kostet wenig: Das Spiel zeichnet im Hintergrund nur für die Vorschau."),
                    forms::Cost::Noticeable => tr("Kostet spürbar: Das Spiel zeichnet im Hintergrund weiter."),
                    forms::Cost::Much => tr("Kostet viel, fast so viel wie ein offenes Spiel."),
                };
                if sight >= forms::FAR_SIGHT { format!("{line} {}", tr("Die große Sichtweite kostet zusätzlich.")) } else { line }
            }
        };
        let busy = self.saving;
        let levels = Background::PRESETS.into_iter().chain([self.background.to_custom()]);
        let segments = levels.enumerate().map(|(n, level)| segment(("background", n), name(level), std::mem::discriminant(&self.background) == std::mem::discriminant(&level), !busy, t));
        let pick = cx.listener(|s, n: &usize, _, cx| {
            if s.saving {
                return;
            }
            (s.background, s.kept_custom) = forms::pick_background(s.background, s.kept_custom, *n);
            s.save_error = None;
            cx.notify();
        });
        let control = segmented("batch-background", segments, move |n, window, cx| pick(&n, window, cx), t).when(busy, |d| d.opacity(tint::DISABLED));
        let control = if busy { sheet::unavailable_over("batch-background-locked", control, tr("Wird gerade gesichert …")).into_any_element() } else { control.into_any_element() };
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
        let shown = |v: u8| match dial {
            Dial::Fps => v.to_string(),
            Dial::Sight => trf("%lld Chunks", &[&v]),
        };
        let step = |icon_name: &'static str, label: String, up: bool, cx: &mut Context<Self>| {
            let id = SharedString::from(format!("background-{id}-{}", if up { "more" } else { "less" }));
            let reason = if busy {
                Some(tr("Wird gerade gesichert …"))
            } else if forms::ladder_step(value, up, steps) == value {
                let end = shown((if up { steps.last() } else { steps.first() }).copied().unwrap_or(value));
                Some(if up { trf("Mehr geht nicht: höchstens %@.", &[&end]) } else { trf("Weniger geht nicht: mindestens %@.", &[&end]) })
            } else {
                None
            };
            if let Some(reason) = reason {
                return sheet::stepper_end(div().id(SharedString::from(format!("{id}-end"))).size(px(control::M)).flex().items_center().justify_center().rounded(px(radius::INNER)), icon(icon_name, iconsize::M, t.tertiary), reason);
            }
            sheet::icon_button(id, icon_name, label, t.secondary, t).on_click(cx.listener(move |s, _, _, cx| {
                if s.saving {
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
        let header = header.child(sheet::button_unless("accounts-all", if all { unpick } else { pick }, None, Look::Plain, ids.is_empty().then(|| trf("Kein Account passt zu „%@“.", &[&raw_filter]).into()), false, t).on_click(cx.listener(move |s, _, _, cx| {
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
            .child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Ändern sich Einstellungen, Ressourcenpakete oder Serverliste in einem Spiel, ziehen alle Spiele dieser Instanz mit, auch in anderen Batches.")))
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
            if self.original.is_none() {
                self.server.update(cx, |s, cx| s.take_focus(window, cx));
            } else {
                self.name.update(cx, |s, cx| {
                    let end = s.value().len();
                    s.take_focus(window, cx);
                    s.set_selected_range(0..end, cx);
                });
            }
        }
    }
}

pub(super) fn body_max(viewport: f32) -> f32 {
    (viewport - 2. * sheet::MARGIN - CHROME_HEIGHT).max(160.)
}

pub(super) fn list_max(viewport: f32) -> f32 {
    (body_max(viewport) * LIST_SHARE).clamp(LIST_MIN, sheet::LIST_MAX)
}

impl Render for BatchSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_focus(window, cx);
        self.list_max = list_max(window.viewport_size().height.as_f32());
        let t = Theme::get(cx);
        let editing = self.original.is_some();
        let (title, subtitle) = if editing { (tr("Batch bearbeiten"), None) } else { (tr("Batch erstellen"), Some(tr("Ein Batch schickt mehrere Accounts mit derselben Instanz los."))) };
        let body = self.render_form(cx, &t);
        let max_body = body_max(window.viewport_size().height.as_f32());
        let running = self.active(cx);
        let leading = if editing {
            let why = if running { Some(tr("Stoppe den Batch, bevor du ihn löschst.")) } else { self.saving.then(|| tr("Wird gerade gesichert …")) };
            let delete = sheet::button_unless("batch-delete", tr("Batch löschen…"), None, Look::Danger, why.clone().map(Into::into), false, &t);
            let delete = if why.is_some() { delete } else { delete.on_click(cx.listener(|s, _, window, cx| s.ask_delete(window, cx))) };
            vec![delete.into_any_element()]
        } else {
            Vec::new()
        };
        let (name_problem, server_problem, version_problem) = self.problems(cx);
        let invalid = name_problem.or(server_problem).or(version_problem);
        let mut trailing = Vec::new();
        if let Some(e) = &self.save_error {
            trailing.push(div().max_w(px(SAVE_ERROR_MAX)).child(self.problem("save-error", "circle-alert", t.danger_text, e, cx, &t)).into_any_element());
        }
        trailing.push(sheet::button_unless("batch-cancel", tr("Abbrechen"), None, Look::Secondary, self.saving.then(|| tr("Wird gerade gesichert …").into()), false, &t).on_click(cx.listener(|s, _, _, cx| if !s.saving { cx.emit(Dismissed) })).into_any_element());
        let starts = !editing && self.picked(cx) > 0;
        if starts {
            trailing.push(sheet::button_unless("batch-create-only", tr("Nur erstellen"), None, Look::Secondary, invalid.clone().or_else(|| self.saving.then(|| tr("Wird gerade gesichert …"))).map(Into::into), false, &t).on_click(cx.listener(|s, _, _, cx| s.save(false, cx))).into_any_element());
        }
        let save_label = if editing { tr("Sichern") } else if starts { tr("Erstellen und starten") } else { tr("Erstellen") };
        trailing.push(sheet::button_unless("batch-save", save_label, None, Look::Primary, invalid.map(Into::into), self.saving, &t).on_click(cx.listener(|s, _, _, cx| s.save(true, cx))).into_any_element());
        div()
            .id("batch-sheet")
            .track_focus(&self.root)
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .on_action(cx.listener(|s, _: &Confirm, window, cx| s.confirm(window, cx)))
            .on_action(cx.listener(|s, _: &Cancel, _, cx| {
                if !s.saving {
                    cx.propagate();
                }
            }))
            .child(sheet::header(title, subtitle, &t))
            .child(div().id("batch-body").track_scroll(&self.body_scroll).max_h(px(max_body)).overflow_y_scroll().px(px(space::XL)).pb(px(space::L)).child(body))
            .child(sheet::footer(leading, trailing, &t))
    }
}

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
