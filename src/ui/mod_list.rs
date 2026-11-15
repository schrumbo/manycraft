//! The Mods tab of an instance: every mod with its state, switching mods on and off with what they need, duplicates,
//! selection, filter and sort, the copied list, dropped jars and the troubleshooter that finds a broken mod.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_kit::component::{FocusableExt, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::AddMods;
use super::drive::Mark;
use super::forms::{self, Problem};
use super::mod_browser::IconCache;
use super::model;
use super::motion::Soft;
use super::sheet::{self, Look};
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tint};
use super::widgets::{TakeFocus, icon, tip};
use crate::core::content::{self, Answer, Bisect, Entry, Outcome, Scan, Source, Trashed};
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;

actions!(mods, [FilterMods, SelectAllMods, ToggleMods, EnableMods, DisableMods, DeleteMods, CheckModUpdates, CopyModList, FindBrokenMod, FixDuplicates, SortModsByName, SortModsByAdded, SortModsByUpdated]);

const LIST: &str = "ModList";
const ROW: f32 = control::L + space::S;
const FACE: f32 = control::M;
const NAMES_SHOWN: usize = 4;

/// Registers the Mods tab's commands and their keys; they act on the instance page that is showing.
pub fn init(cx: &mut App) {
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    let k = |key: &str| -> &'static str { Box::leak(format!("{m}-{key}").into_boxed_str()) };
    cx.bind_keys([
        KeyBinding::new(k("f"), FilterMods, Some(super::instance_page::PAGE_CONTEXT)),
        KeyBinding::new(k("a"), SelectAllMods, Some(LIST)),
        KeyBinding::new("space", ToggleMods, Some(LIST)),
        KeyBinding::new(if cfg!(target_os = "macos") { "delete" } else { "backspace" }, DeleteMods, Some(LIST)),
        KeyBinding::new(if cfg!(target_os = "macos") { "backspace" } else { "delete" }, DeleteMods, Some(LIST)),
    ]);
    on(cx, |s, _: &FilterMods, window, cx| s.focus_filter(window, cx));
    on(cx, |s, _: &SelectAllMods, _, cx| s.select_all(cx));
    on(cx, |s, _: &ToggleMods, window, cx| s.toggle_selected(window, cx));
    on(cx, |s, _: &EnableMods, window, cx| s.switch(s.targets(), true, window, cx));
    on(cx, |s, _: &DisableMods, window, cx| s.switch(s.targets(), false, window, cx));
    on(cx, |s, _: &DeleteMods, _, cx| s.trash(s.targets(), None, cx));
    on(cx, |s, _: &CheckModUpdates, window, cx| s.check_updates(window, cx));
    on(cx, |s, _: &CopyModList, _, cx| s.copy_list(cx));
    on(cx, |s, _: &FindBrokenMod, window, cx| s.ask_bisect(window, cx));
    on(cx, |s, _: &FixDuplicates, _, cx| s.fix_all(cx));
    on(cx, |s, _: &SortModsByName, _, cx| s.sort_by(Sort::Name, cx));
    on(cx, |s, _: &SortModsByAdded, _, cx| s.sort_by(Sort::Added, cx));
    on(cx, |s, _: &SortModsByUpdated, _, cx| s.sort_by(Sort::Updated, cx));
}

fn on<A: Action>(cx: &mut App, f: impl Fn(&mut ModList, &A, &mut Window, &mut Context<ModList>) + Clone + 'static) {
    cx.on_action(move |a: &A, cx| {
        let Some(list) = current(cx) else { return };
        super::instance_page::show_mods(cx);
        let (f, a) = (f.clone(), a.boxed_clone());
        sheet::in_window(cx, move |window, cx| {
            let Some(a) = a.as_any().downcast_ref::<A>() else { return };
            list.update(cx, |s, cx| f(s, a, window, cx));
        });
    });
}

struct Current(WeakEntity<ModList>);

impl Global for Current {}

fn current(cx: &App) -> Option<Entity<ModList>> {
    cx.try_global::<Current>().and_then(|c| c.0.upgrade())
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Sort {
    #[default]
    Name,
    Added,
    Updated,
}

impl Sort {
    fn label(self) -> String {
        match self {
            Self::Name => tr("Name"),
            Self::Added => tr("Hinzugefügt"),
            Self::Updated => tr("Aktualisiert"),
        }
    }
}

struct Sorted(Sort);

impl Global for Sorted {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    Own,
    Mod(usize),
}

#[derive(Default)]
struct Facts {
    older: HashMap<String, Vec<String>>,
    twin: HashMap<String, String>,
    needs: HashMap<String, Vec<String>>,
    dups: usize,
}

/// One palette entry of the Mods tab: icon, label, action and why it cannot run now.
pub struct Command {
    pub icon: &'static str,
    pub label: String,
    pub action: Box<dyn Action>,
    pub blocked: Option<String>,
}

/// The Mods tab of the instance page.
pub struct ModList {
    id: String,
    scan: Scan,
    sort: Sort,
    hold: Option<String>,
    scanned: bool,
    key: String,
    ticket: u64,
    error: Option<Problem>,
    error_open: bool,
    failed: bool,
    reveal: Option<String>,
    busy: bool,
    filter: Entity<InputState>,
    query: String,
    rows: Vec<Row>,
    facts: Facts,
    selected: HashSet<String>,
    anchor: Option<String>,
    cursor: Option<String>,
    bisect: Option<Bisect>,
    bisecting: bool,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    icons: Entity<IconCache>,
    _subs: Vec<Subscription>,
}

/// The Mods tab for instance `id`; the page keeps it and rebinds it when it shows another instance.
pub fn new(id: &str, window: &mut Window, cx: &mut App) -> Entity<ModList> {
    let list = cx.new(|cx| {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Mods filtern")));
        let subs = vec![
            cx.observe(&model::get(cx), |s: &mut ModList, _, cx| s.model_changed(cx)),
            cx.subscribe_in(&filter, window, |s: &mut ModList, input, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    s.query = input.read(cx).value().trim().to_lowercase();
                    s.rebuild();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    let focus = s.focus.clone();
                    window.on_next_frame(move |window, cx| window.focus(&focus, cx));
                    if s.cursor.is_none() {
                        s.step(1, false, cx);
                    }
                }
                _ => {}
            }),
            cx.observe_window_activation(window, |s: &mut ModList, window, cx| {
                if window.is_window_active() {
                    s.rescan(cx);
                }
            }),
        ];
        let mut list = ModList {
            id: id.to_owned(),
            scan: Scan::default(),
            sort: sorting(cx),
            hold: None,
            scanned: false,
            key: String::new(),
            ticket: 0,
            error: None,
            error_open: false,
            busy: false,
            filter,
            query: String::new(),
            rows: Vec::new(),
            facts: Facts::default(),
            selected: HashSet::new(),
            anchor: None,
            cursor: None,
            bisect: None,
            bisecting: false,
            failed: false,
            reveal: None,
            focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            icons: IconCache::new(cx),
            _subs: subs,
        };
        list.reset(cx);
        list
    });
    cx.set_global(Current(list.downgrade()));
    list
}

/// What the palette offers on the Mods tab of the instance page that shows.
pub fn commands(cx: &App) -> Vec<Command> {
    let Some(list) = current(cx) else { return Vec::new() };
    let s = list.read(cx);
    let Some(made) = s.instance(cx) else { return Vec::new() };
    if !made.loader.takes_mods() {
        return Vec::new();
    }
    let locked = s.locked(cx);
    let chosen = s.targets();
    let none = (chosen.is_empty()).then(|| tr("Wähle zuerst Mods aus."));
    let empty = s.scan.entries.is_empty().then(|| tr("Noch keine Mods."));
    let mut out = vec![
        Command { icon: "search", label: tr("Mods filtern"), action: Box::new(FilterMods), blocked: None },
        Command { icon: "square-check", label: tr("Alle Mods auswählen"), action: Box::new(SelectAllMods), blocked: empty.clone() },
        Command { icon: "toggle-right", label: tr("Ausgewählte Mods einschalten"), action: Box::new(EnableMods), blocked: none.clone().or(locked.clone()) },
        Command { icon: "toggle-left", label: tr("Ausgewählte Mods ausschalten"), action: Box::new(DisableMods), blocked: none.clone().or(locked.clone()) },
        Command { icon: "trash", label: tr("Ausgewählte Mods löschen"), action: Box::new(DeleteMods), blocked: none.or(locked.clone()) },
        Command { icon: "refresh-cw", label: tr("Mod-Updates prüfen…"), action: Box::new(CheckModUpdates), blocked: made.mods.is_empty().then(|| tr("Keine Mods von Modrinth.")) },
        Command { icon: "copy", label: tr("Modliste kopieren"), action: Box::new(CopyModList), blocked: empty.clone() },
        Command { icon: "bug", label: tr("Fehlerhaften Mod finden…"), action: Box::new(FindBrokenMod), blocked: s.bisect.as_ref().map(|_| tr("Die Fehlersuche läuft schon.")).or(empty) },
    ];
    if s.facts.dups > 0 {
        out.push(Command { icon: "copy-x", label: tr("Doppelte Mods bereinigen"), action: Box::new(FixDuplicates), blocked: locked });
    }
    for (sort, label, action) in [(Sort::Name, tr("Mods nach Name sortieren"), Box::new(SortModsByName) as Box<dyn Action>), (Sort::Added, tr("Mods nach Hinzufügedatum sortieren"), Box::new(SortModsByAdded)), (Sort::Updated, tr("Mods nach Aktualisierungsdatum sortieren"), Box::new(SortModsByUpdated))] {
        if sort != sorting(cx) {
            out.push(Command { icon: "arrow-up-down", label, action, blocked: None });
        }
    }
    out
}

fn sorting(cx: &App) -> Sort {
    cx.try_global::<Sorted>().map(|s| s.0).unwrap_or_default()
}

fn key(made: &Instance) -> String {
    let mut parts: Vec<String> = made.mods.iter().map(|m| m.file_name.to_lowercase()).collect();
    parts.push("|".into());
    parts.extend(made.local_mods.iter().map(|n| n.to_lowercase()));
    parts.push("|".into());
    parts.extend(made.disabled.iter().map(|n| n.to_lowercase()));
    parts.join("\n")
}

fn fallback(made: &Instance) -> Vec<Entry> {
    let off = |file: &str| made.disabled.iter().any(|d| d.eq_ignore_ascii_case(file));
    let listed = made.mods.iter().map(|m| Entry {
        file: m.file_name.clone(),
        on_disk: true,
        enabled: !off(&m.file_name),
        source: Source::Modrinth { project_id: m.project_id.clone(), title: m.title.clone() },
        meta: None,
        icon: None,
        added: None,
        updated: None,
        size: 0,
    });
    let own = made.local_mods.iter().map(|n| Entry { file: n.clone(), on_disk: true, enabled: !off(n), source: Source::Own, meta: None, icon: None, added: None, updated: None, size: 0 });
    listed.chain(own).collect()
}

fn quoted(name: &str) -> String {
    trf("„%@“", &[&name])
}

fn listing(names: &[String]) -> String {
    let shown: Vec<String> = names.iter().take(NAMES_SHOWN).map(|n| quoted(n)).collect();
    match names.len().checked_sub(NAMES_SHOWN).filter(|more| *more > 0) {
        Some(1) => trf("%@ und 1 weiterer", &[&shown.join(", ")]),
        Some(more) => trf("%@ und %lld weitere", &[&shown.join(", "), &more]),
        None => shown.join(", "),
    }
}

fn count_mods(n: usize) -> String {
    if n == 1 { tr("1 Mod") } else { trf("%lld Mods", &[&n]) }
}

impl ModList {
    /// Shows instance `id` instead, with nothing selected and no filter.
    pub fn bind(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.id == id {
            return;
        }
        self.id = id.to_owned();
        self.filter.update(cx, |f, cx| f.set_value("", window, cx));
        self.query.clear();
        self.reset(cx);
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.selected.clear();
        self.anchor = None;
        self.cursor = None;
        self.error = None;
        self.failed = false;
        self.reveal = None;
        self.scanned = false;
        self.bisect = None;
        self.scan = Scan { entries: self.instance(cx).map(|made| fallback(&made)).unwrap_or_default(), ..Scan::default() };
        self.analyze();
        self.rebuild();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.rescan(cx);
    }

    fn instance(&self, cx: &App) -> Option<Instance> {
        model::get(cx).read(cx).instance(&self.id).cloned()
    }

    fn running(&self, cx: &App) -> bool {
        model::get(cx).read(cx).games.values().any(|g| g.instance_id == self.id && g.phase.is_active())
    }

    /// Locks every change of the list for `reason`, e.g. while the page moves the instance to another version.
    pub fn hold(&mut self, reason: Option<String>, cx: &mut Context<Self>) {
        if self.hold != reason {
            self.hold = reason;
            cx.notify();
        }
    }

    fn locked(&self, cx: &App) -> Option<String> {
        if let Some(why) = &self.hold {
            return Some(why.clone());
        }
        if !self.scanned && self.error.is_some() && !self.failed {
            return Some(tr("Die Mods ließen sich nicht lesen."));
        }
        if self.bisecting || self.bisect.as_ref().is_some_and(|b| b.outcome.is_none()) {
            return Some(tr("Beende zuerst die Fehlersuche."));
        }
        self.running(cx).then(|| tr("Stoppe zuerst die laufenden Spiele."))
    }

    fn model_changed(&mut self, cx: &mut Context<Self>) {
        let Some(made) = self.instance(cx) else { return };
        if key(&made) != self.key {
            self.rescan(cx);
        }
        cx.notify();
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(made) = self.instance(cx) else { return };
        if !made.loader.takes_mods() {
            return;
        }
        self.key = key(&made);
        self.ticket += 1;
        let (ticket, id) = (self.ticket, self.id.clone());
        let task = cx.background_spawn(async move { content::scan(&id).map(|(made, scan)| (made, scan, content::load(&id))) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| s.scanned(ticket, result, cx));
        })
        .detach();
    }

    fn scanned(&mut self, ticket: u64, result: anyhow::Result<(Instance, Scan, Option<Bisect>)>, cx: &mut Context<Self>) {
        if ticket != self.ticket {
            return;
        }
        match result {
            Ok((made, scan, bisect)) => {
                self.key = key(&made);
                put(made, cx);
                self.scan = scan;
                self.analyze();
                self.bisect = bisect;
                self.scanned = true;
                if !self.failed {
                    self.error = None;
                }
                let known: HashSet<&str> = self.scan.entries.iter().map(|e| e.file.as_str()).collect();
                self.selected.retain(|f| known.contains(f.as_str()));
                self.rebuild();
                if let Some(file) = self.reveal.take().and_then(|name| self.scan.entries.iter().find(|e| e.file.eq_ignore_ascii_case(&name)).map(|e| e.file.clone())) {
                    self.go_to(&file, false, cx);
                }
            }
            Err(e) => {
                self.error = Some(forms::explain_as(tr("Die Mods ließen sich nicht lesen."), &e));
                self.failed = false;
                self.scanned = false;
            }
        }
        cx.notify();
    }

    fn fail(&mut self, problem: Problem) {
        self.error = Some(problem);
        self.failed = true;
    }

    fn settle(&mut self) {
        if self.failed {
            self.error = None;
            self.failed = false;
        }
    }

    fn analyze(&mut self) {
        let mut facts = Facts::default();
        let on: Vec<Entry> = self.scan.entries.iter().filter(|e| e.enabled).cloned().collect();
        for dup in content::duplicates(&on) {
            facts.dups += 1;
            let older = dup.files[1..].to_vec();
            for file in &dup.files {
                facts.older.insert(file.clone(), older.clone());
                if let Some(twin) = dup.files.iter().find(|f| *f != file) {
                    facts.twin.insert(file.clone(), twin.clone());
                }
            }
        }
        if self.scanned {
            for e in self.scan.entries.iter().filter(|e| e.enabled && e.on_disk) {
                let missing = content::missing_for(&self.scan, std::slice::from_ref(&e.file));
                if !missing.is_empty() {
                    facts.needs.insert(e.file.clone(), missing);
                }
            }
        }
        self.facts = facts;
    }

    fn rebuild(&mut self) {
        let q = self.query.as_str();
        let fits = |e: &Entry| q.is_empty() || e.name().to_lowercase().contains(q) || e.file.to_lowercase().contains(q) || e.id().is_some_and(|id| id.contains(q));
        let mut listed: Vec<usize> = Vec::new();
        let mut own: Vec<usize> = Vec::new();
        for (at, e) in self.scan.entries.iter().enumerate().filter(|(_, e)| fits(e)) {
            match e.source {
                Source::Modrinth { .. } => listed.push(at),
                Source::Own => own.push(at),
            }
        }
        let sort = self.sort;
        let entries = &self.scan.entries;
        let order = |list: &mut Vec<usize>| match sort {
            Sort::Name => list.sort_by_cached_key(|at| (entries[*at].name().to_lowercase(), entries[*at].file.to_lowercase())),
            Sort::Added => list.sort_by(|a, b| entries[*b].added.cmp(&entries[*a].added).then_with(|| entries[*a].file.cmp(&entries[*b].file))),
            Sort::Updated => list.sort_by(|a, b| entries[*b].updated.cmp(&entries[*a].updated).then_with(|| entries[*a].file.cmp(&entries[*b].file))),
        };
        order(&mut listed);
        order(&mut own);
        let mut rows: Vec<Row> = listed.into_iter().map(Row::Mod).collect();
        if !own.is_empty() {
            rows.push(Row::Own);
            rows.extend(own.into_iter().map(Row::Mod));
        }
        self.rows = rows;
    }

    fn visible(&self) -> Vec<String> {
        self.rows.iter().filter_map(|r| match *r {
            Row::Mod(at) => Some(self.scan.entries[at].file.clone()),
            Row::Own => None,
        }).collect()
    }

    fn entry(&self, file: &str) -> Option<&Entry> {
        self.scan.entries.iter().find(|e| e.file == file)
    }

    fn targets(&self) -> Vec<String> {
        self.visible().into_iter().filter(|f| self.selected.contains(f)).collect()
    }

    fn label_of(&self, files: &[String]) -> String {
        match files {
            [one] => quoted(&self.entry(one).map(Entry::name).unwrap_or_else(|| one.clone())),
            many => count_mods(many.len()),
        }
    }

    fn names(&self, files: &[String]) -> Vec<String> {
        let mut names: Vec<String> = files.iter().map(|f| self.entry(f).map(Entry::name).unwrap_or_else(|| f.clone())).collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    fn sort_by(&mut self, sort: Sort, cx: &mut Context<Self>) {
        self.sort = sort;
        cx.set_global(Sorted(sort));
        self.rebuild();
        cx.notify();
    }

    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        super::instance_page::show_mods(cx);
        let filter = self.filter.clone();
        window.on_next_frame(move |window, cx| {
            filter.update(cx, |f, cx| {
                let end = f.value().len();
                f.take_focus(window, cx);
                f.set_selected_range(0..end, cx);
            })
        });
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |f, cx| f.set_value("", window, cx));
        self.query.clear();
        self.rebuild();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let all = self.visible();
        self.cursor = all.last().cloned();
        self.anchor = all.first().cloned();
        self.selected = all.into_iter().collect();
        cx.notify();
    }

    fn click(&mut self, file: &str, e: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let m = e.modifiers();
        if m.shift {
            self.extend_to(file);
        } else if if cfg!(target_os = "macos") { m.platform } else { m.control } {
            if !self.selected.remove(file) {
                self.selected.insert(file.to_owned());
            }
            self.anchor = Some(file.to_owned());
        } else {
            self.selected = HashSet::from([file.to_owned()]);
            self.anchor = Some(file.to_owned());
        }
        self.cursor = Some(file.to_owned());
        cx.notify();
    }

    fn extend_to(&mut self, file: &str) {
        let order = self.visible();
        let anchor = self.anchor.clone().filter(|a| order.contains(a)).unwrap_or_else(|| file.to_owned());
        let (Some(a), Some(b)) = (order.iter().position(|f| *f == anchor), order.iter().position(|f| f == file)) else { return };
        let (lo, hi) = (a.min(b), a.max(b));
        self.selected = order[lo..=hi].iter().cloned().collect();
        self.anchor = Some(anchor);
    }

    fn step(&mut self, delta: i64, extend: bool, cx: &mut Context<Self>) {
        let order = self.visible();
        if order.is_empty() {
            return;
        }
        let at = self.cursor.as_ref().and_then(|c| order.iter().position(|f| f == c));
        let next = match at {
            Some(at) => (at as i64 + delta).clamp(0, order.len() as i64 - 1) as usize,
            None if delta < 0 => order.len() - 1,
            None => 0,
        };
        self.go_to(&order[next].clone(), extend, cx);
    }

    fn go_to(&mut self, file: &str, extend: bool, cx: &mut Context<Self>) {
        if extend {
            self.extend_to(file);
        } else {
            self.selected = HashSet::from([file.to_owned()]);
            self.anchor = Some(file.to_owned());
        }
        self.cursor = Some(file.to_owned());
        if let Some(row) = self.rows.iter().position(|r| matches!(*r, Row::Mod(at) if self.scan.entries[at].file == file)) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn key_down(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = e.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        let page = 10;
        match e.keystroke.key.as_str() {
            "up" => self.step(-1, m.shift, cx),
            "down" => self.step(1, m.shift, cx),
            "pageup" => self.step(-page, m.shift, cx),
            "pagedown" => self.step(page, m.shift, cx),
            "home" => self.step(i64::MIN / 2, m.shift, cx),
            "end" => self.step(i64::MAX / 2, m.shift, cx),
            "escape" if !self.selected.is_empty() => {
                self.selected.clear();
                cx.notify();
            }
            "escape" if !self.query.is_empty() => self.clear_filter(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn toggle_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let files = self.targets();
        if files.is_empty() {
            return;
        }
        let all_on = files.iter().all(|f| self.entry(f).is_some_and(|e| e.enabled));
        self.switch(files, !all_on, window, cx);
    }

    fn switch(&mut self, files: Vec<String>, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let files: Vec<String> = files.into_iter().filter(|f| self.entry(f).is_some_and(|e| e.enabled != on)).collect();
        if files.is_empty() || self.busy {
            return;
        }
        if let Some(why) = self.locked(cx) {
            super::undo::note(why, cx);
            return;
        }
        if !self.scanned {
            return;
        }
        if !self.scanned {
            return;
        }
        let scan = &self.scan;
        if on {
            let missing = content::missing_for(scan, &files);
            if !missing.is_empty() {
                self.ask_with(files, missing, window, cx);
                return;
            }
        } else {
            let needed = content::needed_by(scan, &files);
            if !needed.is_empty() {
                let shown: Vec<String> = needed.iter().map(|n| if n == "Manycraft" { tr("Manycrafts Mod") } else { n.clone() }).collect();
                let title = match files.as_slice() {
                    [_] => trf("%@ ausschalten?", &[&self.label_of(&files)]),
                    many => trf("%lld Mods ausschalten?", &[&many.len()]),
                };
                let message = if shown.len() == 1 { trf("Dann startet %@ nicht mehr.", &[&listing(&shown)]) } else { trf("Dann starten %@ nicht mehr.", &[&listing(&shown)]) };
                let me = cx.entity().downgrade();
                sheet::confirm(window, cx, title, message, tr("Ausschalten"), |_| None, move |_, cx| {
                    let files = files.clone();
                    let _ = me.update(cx, |s, cx| s.apply_switch(files, false, cx));
                });
                return;
            }
        }
        self.apply_switch(files, on, cx);
    }

    fn ask_with(&mut self, files: Vec<String>, missing: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if sheet::busy(window, cx) {
            return;
        }
        let needed = self.names(&missing);
        let title = match needed.as_slice() {
            [one] => trf("Auch „%@“ einschalten?", &[one]),
            many => trf("Auch %lld weitere Mods einschalten?", &[&many.len()]),
        };
        let message = match files.as_slice() {
            [_] => trf("Ohne %@ startet %@ nicht.", &[&listing(&needed), &self.label_of(&files)]),
            _ => trf("Ohne %@ starten diese Mods nicht.", &[&listing(&needed)]),
        };
        let only = match files.as_slice() {
            [_] => trf("Nur %@", &[&self.label_of(&files)]),
            _ => tr("Nur diese"),
        };
        let all = if files.len() + missing.len() == 2 { tr("Beide einschalten") } else { tr("Alle einschalten") };
        let me = cx.entity().downgrade();
        let accept = cx.focus_handle();
        let first = accept.clone();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let rings = super::theme::rings(cx);
            let (me_only, me_all, files_only) = (me.clone(), me.clone(), files.clone());
            let both: Vec<String> = files.iter().chain(missing.iter()).cloned().collect();
            alert.title(title.clone()).description(message.clone()).on_ok(sheet::pass_enter).footer(
                DialogFooter::new()
                    .child(Button::new("alert-cancel").label(tr("Abbrechen")).focus_ring(rings).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("alert-only").label(only.clone()).focus_ring(rings).on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let files = files_only.clone();
                        let _ = me_only.update(cx, |s, cx| s.apply_switch(files, true, cx));
                    }))
                    .child(Button::new("alert-ok").primary().label(all.clone()).focus_ring(rings).track_focus(&accept).on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let files = both.clone();
                        let _ = me_all.update(cx, |s, cx| s.apply_switch(files, true, cx));
                    })),
            )
        });
        first.focus(window, cx);
    }

    fn apply_switch(&mut self, files: Vec<String>, on: bool, cx: &mut Context<Self>) {
        if self.busy || self.locked(cx).is_some() {
            return;
        }
        let label = self.label_of(&files);
        let id = self.id.clone();
        self.busy = true;
        self.ticket += 1;
        self.settle();
        for e in self.scan.entries.iter_mut().filter(|e| files.contains(&e.file)) {
            e.enabled = on;
        }
        cx.notify();
        let task = cx.background_spawn({
            let (id, files) = (id.clone(), files.clone());
            async move { content::set_enabled(&id, &files, on) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy = false;
                match result {
                    Ok(made) => {
                        let culprit = !on && s.id == id && s.bisect.as_ref().is_some_and(|b| matches!(&b.outcome, Some(Outcome::Found(f)) if files.contains(f)));
                        put(made, cx);
                        super::undo::offer(super::undo::Kind::ModsSwitched { instance_id: id, files, on, label }, cx);
                        if culprit {
                            s.bisect_step(Step::Dismiss, cx);
                        }
                    }
                    Err(e) if s.id == id => s.fail(forms::explain_as(tr("Nicht gespeichert."), &e)),
                    Err(_) => {}
                }
                s.rescan(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn trash(&mut self, files: Vec<String>, label: Option<String>, cx: &mut Context<Self>) {
        if files.is_empty() || self.busy {
            return;
        }
        if let Some(why) = self.locked(cx) {
            super::undo::note(why, cx);
            return;
        }
        if !self.scanned {
            return;
        }
        let label = label.unwrap_or_else(|| self.label_of(&files));
        let id = self.id.clone();
        let next = self.after(&files);
        self.busy = true;
        self.ticket += 1;
        self.settle();
        cx.notify();
        let task = cx.background_spawn({
            let (id, files) = (id.clone(), files.clone());
            async move { content::trash(&id, &files) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy = false;
                match result {
                    Ok((made, trashed)) => {
                        if s.id == id {
                            s.scan.entries.retain(|e| !files.contains(&e.file));
                            s.selected.clear();
                            s.cursor = None;
                            s.rebuild();
                            if let Some(next) = next {
                                s.go_to(&next, false, cx);
                            }
                        }
                        put(made, cx);
                        super::undo::offer(super::undo::Kind::ModsTrashed { instance_id: id, trashed, label }, cx);
                    }
                    Err(e) if s.id == id => s.fail(forms::explain_as(tr("Nicht gelöscht."), &e)),
                    Err(_) => {}
                }
                s.rescan(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn after(&self, gone: &[String]) -> Option<String> {
        let order = self.visible();
        let last = order.iter().rposition(|f| gone.contains(f))?;
        order[last + 1..].iter().chain(order[..last].iter().rev()).find(|f| !gone.contains(f)).cloned()
    }

    fn fix(&mut self, file: &str, cx: &mut Context<Self>) {
        let Some(older) = self.facts.older.get(file).cloned() else { return };
        let label = match older.as_slice() {
            [one] => match self.entry(one) {
                Some(e) => quoted(&format!("{} {}", e.name(), e.version().unwrap_or_default()).trim_end().to_owned()),
                None => quoted(one),
            },
            many => count_mods(many.len()),
        };
        self.trash(older, Some(label), cx);
    }

    fn fix_all(&mut self, cx: &mut Context<Self>) {
        let mut older: Vec<String> = self.facts.older.values().flatten().cloned().collect();
        older.sort();
        older.dedup();
        self.trash(older, None, cx);
    }

    fn copy_list(&mut self, cx: &mut Context<Self>) {
        let mut lines: Vec<(String, String)> = self
            .scan
            .entries
            .iter()
            .map(|e| {
                let mut line = match e.version() {
                    Some(v) if !v.is_empty() => format!("{} {v}", e.name()),
                    _ => e.name(),
                };
                if !e.enabled {
                    line.push_str(&trf(" (%@)", &[&tr("aus")]));
                }
                (e.name().to_lowercase(), line)
            })
            .collect();
        if lines.is_empty() {
            return;
        }
        lines.sort();
        let n = lines.len();
        let text = lines.into_iter().map(|(_, l)| l).collect::<Vec<_>>().join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        super::undo::note(if n == 1 { tr("Liste mit 1 Mod kopiert.") } else { trf("Liste mit %lld Mods kopiert.", &[&n]) }, cx);
    }

    fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        super::mod_updates::open(&self.id, window, cx);
    }

    fn dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let (jars, other): (Vec<PathBuf>, Vec<PathBuf>) = paths.paths().iter().cloned().partition(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jar")));
        if !other.is_empty() {
            super::undo::note(tr("Hier lassen sich nur .jar-Dateien ablegen."), cx);
        }
        if jars.is_empty() {
            return;
        }
        self.settle();
        let added: Vec<String> = jars.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        let id = self.id.clone();
        let task = cx.background_spawn({
            let id = id.clone();
            async move { content::add_files(&id, &jars) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                match result {
                    Ok((made, taken)) => {
                        let fresh: Vec<String> = added.into_iter().filter(|name| !taken.contains(name)).collect();
                        if s.id == id {
                            s.reveal = fresh.first().cloned();
                        }
                        put(made, cx);
                        if !taken.is_empty() {
                            super::undo::note(trf("Schon da: %@.", &[&listing(&taken)]), cx);
                        }
                        if !fresh.is_empty() {
                            let label = if let [one] = fresh.as_slice() { quoted(one) } else { count_mods(fresh.len()) };
                            super::undo::offer(super::undo::Kind::ModsAdded { instance_id: id.clone(), files: fresh, label }, cx);
                        }
                    }
                    Err(e) if s.id == id => s.fail(forms::explain_as(tr("Nicht hinzugefügt."), &e)),
                    Err(_) => {}
                }
                s.rescan(cx);
            });
        })
        .detach();
    }

    fn ask_bisect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bisect.is_some() {
            return;
        }
        if let Some(why) = self.running(cx).then(|| tr("Stoppe zuerst die laufenden Spiele.")) {
            super::undo::note(why, cx);
            return;
        }
        let on = self.scan.entries.iter().filter(|e| e.enabled && e.on_disk).count();
        if on < 2 {
            super::undo::note(tr("Zum Eingrenzen müssen mindestens zwei Mods an sein."), cx);
            return;
        }
        let rounds = (usize::BITS - (on - 1).leading_zeros()) as usize;
        let me = cx.entity().downgrade();
        let message = trf("Manycraft schaltet in jeder Runde einen Teil der Mods aus. Du startest das Spiel und sagst, ob der Fehler noch da ist. Nach etwa %lld Runden steht fest, welcher Mod es ist. Danach ist alles wieder wie vorher.", &[&rounds]);
        sheet::confirm(window, cx, tr("Fehlerhaften Mod finden?"), message, tr("Starten"), |_| None, move |_, cx| {
            let _ = me.update(cx, |s, cx| s.bisect_step(Step::Start, cx));
        });
    }

    fn bisect_step(&mut self, step: Step, cx: &mut Context<Self>) {
        if self.bisecting {
            return;
        }
        if !matches!(step, Step::Dismiss) && self.running(cx) {
            super::undo::note(tr("Beende zuerst das Spiel."), cx);
            return;
        }
        self.bisecting = true;
        self.settle();
        cx.notify();
        let id = self.id.clone();
        let task = cx.background_spawn({
            let id = id.clone();
            async move {
            match step {
                Step::Start => content::start(&id).map(|(made, b)| (Some(made), Some(b))),
                Step::Answer(answer) => content::answer(&id, answer).map(|(made, b)| (Some(made), Some(b))),
                Step::Stop => content::stop(&id).map(|made| (Some(made), None)),
                Step::Dismiss => content::dismiss(&id).map(|_| (None, None)),
            }
        }});
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.bisecting = false;
                match result {
                    Ok((made, bisect)) => {
                        if s.id == id {
                            s.bisect = bisect;
                        }
                        if let Some(made) = made {
                            put(made, cx);
                        }
                    }
                    Err(e) if s.id == id => s.fail(forms::explain_as(tr("Die Fehlersuche hat nicht geklappt."), &e)),
                    Err(_) => {}
                }
                s.rescan(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_bisect(&self, b: &Bisect, cx: &mut Context<Self>, t: &Theme) -> Stateful<Div> {
        let running = self.running(cx);
        let wait = running.then(|| SharedString::from(tr("Beende zuerst das Spiel.")));
        let card = div().id("bisect").flex().flex_col().gap(px(space::S)).p(px(space::M)).rounded(px(radius::CARD)).bg(t.raised).border_1().border_color(t.separator);
        let title = |text: String, glyph: &str, color: Hsla| div().flex().items_center().gap(px(space::S)).child(icon(glyph, iconsize::M, color)).child(div().flex_1().min_w_0().text_size(px(font::BODY)).font_weight(FontWeight::SEMIBOLD).child(text));
        let line = |text: String| div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(text);
        match &b.outcome {
            None => {
                let off = b.off.len();
                let total = self.scan.entries.len();
                let heading = trf("Fehlersuche · Runde %lld von etwa %lld", &[&(b.round + 1), &b.rounds.max(b.round + 1)]);
                let cancel = sheet::button("bisect-stop", tr("Abbrechen"), None, Look::Plain, false, false, t).on_click(cx.listener(|s, _, _, cx| s.bisect_step(Step::Stop, cx)));
                let still = sheet::button_unless("bisect-still", tr("Fehler ist noch da"), None, Look::Secondary, wait.clone(), self.bisecting, t).when(!running, |b| b.on_click(cx.listener(|s, _, _, cx| s.bisect_step(Step::Answer(Answer::Still), cx))));
                let gone = sheet::button_unless("bisect-gone", tr("Fehler ist weg"), None, Look::Secondary, wait, self.bisecting, t).when(!running, |b| b.on_click(cx.listener(|s, _, _, cx| s.bisect_step(Step::Answer(Answer::Gone), cx))));
                card.mark("region", heading.clone())
                    .child(div().flex().items_center().gap(px(space::S)).child(div().flex_1().min_w_0().child(title(heading, "bug", t.accent_text))).child(cancel))
                    .child(line(trf("%lld von %lld Mods sind gerade aus. Starte das Spiel und sieh nach, ob der Fehler noch auftritt.", &[&off, &total])))
                    .child(div().flex().flex_wrap().gap(px(space::S)).child(still).child(gone))
            }
            Some(outcome) => {
                let done = sheet::button("bisect-done", tr("Fertig"), None, Look::Secondary, false, self.bisecting, t).on_click(cx.listener(|s, _, _, cx| s.bisect_step(Step::Dismiss, cx)));
                let (heading, glyph, color) = match outcome {
                    Outcome::Found(file) => (trf("%@ verursacht den Fehler.", &[&self.label_of(std::slice::from_ref(file))]), "circle-check", t.success_text),
                    Outcome::Several(files) => (trf("Einer dieser Mods verursacht den Fehler: %@.", &[&listing(&self.names(files))]), "circle-question-mark", t.attention_text),
                    Outcome::Nothing => (tr("Kein Mod verursacht den Fehler."), "circle-question-mark", t.secondary),
                };
                let culprit = match outcome {
                    Outcome::Found(file) if self.entry(file).is_some_and(|e| e.enabled) => {
                        let file = file.clone();
                        Some(sheet::button("bisect-off", trf("%@ ausschalten", &[&self.label_of(std::slice::from_ref(&file))]), None, Look::Primary, false, false, t).on_click(cx.listener(move |s, _, window, cx| {
                            s.switch(vec![file.clone()], false, window, cx);
                        })))
                    }
                    _ => None,
                };
                let rest = match outcome {
                    Outcome::Nothing => tr("Der Fehler liegt woanders. Alle Mods sind wieder wie vorher."),
                    _ => tr("Alle Mods sind wieder wie vorher."),
                };
                card.mark("region", heading.clone()).child(title(heading, glyph, color)).child(line(rest)).child(div().flex().flex_wrap().gap(px(space::S)).children(culprit).child(done))
            }
        }
    }

    fn render_toolbar(&self, made: &Instance, cx: &mut Context<Self>, t: &Theme) -> Div {
        let sort = self.sort;
        let rings = super::theme::rings(cx);
        let sorter = Button::new("mods-sort")
            .secondary()
            .small()
            .h(px(control::M))
            .px(px(space::M))
            .focus_ring(rings)
            .dropdown_caret(true)
            .tooltip(tr("Sortieren"))
            .child(div().flex().items_center().gap(px(space::XS)).text_size(px(font::BODY)).child(icon("arrow-up-down", iconsize::S, t.secondary)).child(sort.label()))
            .dropdown_menu(move |menu, _, _| {
                menu.menu_with_check(Sort::Name.label(), sort == Sort::Name, Box::new(SortModsByName)).menu_with_check(Sort::Added.label(), sort == Sort::Added, Box::new(SortModsByAdded)).menu_with_check(Sort::Updated.label(), sort == Sort::Updated, Box::new(SortModsByUpdated))
            });
        let dups = self.facts.dups > 0;
        let bisecting = self.bisect.is_some();
        let more = Button::new("mods-more")
            .secondary()
            .small()
            .h(px(control::M))
            .w(px(control::M))
            .focus_ring(rings)
            .tooltip(tr("Mehr"))
            .child(icon("ellipsis", iconsize::M, t.secondary))
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.menu(tr("Modliste kopieren"), Box::new(CopyModList)).menu_with_disabled(tr("Fehlerhaften Mod finden…"), Box::new(FindBrokenMod), bisecting);
                if dups { menu.separator().menu(tr("Doppelte Mods bereinigen"), Box::new(FixDuplicates)) } else { menu }
            });
        let updates = sheet::button_unless("mods-updates", tr("Updates prüfen…"), None, Look::Secondary, made.mods.is_empty().then(|| SharedString::from(tr("Keine Mods von Modrinth."))), false, t).when(!made.mods.is_empty(), |b| b.on_click(cx.listener(|s, _, window, cx| s.check_updates(window, cx))));
        let id = made.id.clone();
        let add = sheet::button("mods-add", tr("Mods hinzufügen…"), Some("plus"), Look::Secondary, false, false, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(AddMods(id.clone())), cx));
        let field = div().id("mods-filter").mark("textbox", tr("Mods filtern")).flex_1().min_w(px(shell::SEARCH_MIN)).on_action(cx.listener(|s, _: &gpui_kit::base::input::Escape, window, cx| {
            if s.query.is_empty() {
                window.focus(&s.focus, cx);
            } else {
                s.clear_filter(window, cx);
            }
        })).on_key_down(cx.listener(|s, e: &KeyDownEvent, window, cx| {
            if e.keystroke.key == "down" && !e.keystroke.modifiers.modified() {
                window.focus(&s.focus, cx);
                if s.cursor.is_none() {
                    s.step(1, false, cx);
                }
                cx.stop_propagation();
            }
        })).child(Input::new(&self.filter).prefix(icon("search", iconsize::S, t.tertiary)).cleanable(true));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(space::S))
            .child(field)
            .child(div().id("mods-sort-wrap").mark("button", tr("Sortieren")).flex_none().child(sorter))
            .child(updates)
            .child(div().id("mods-more-wrap").mark("button", tr("Mehr")).flex_none().child(more))
            .child(add)
    }

    fn render_count(&self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let total = self.scan.entries.len();
        let shown = self.rows.iter().filter(|r| matches!(r, Row::Mod(_))).count();
        let chosen = self.targets();
        let line = div().h(px(control::M)).flex().items_center().gap(px(space::S));
        let caption = |text: String| div().id("mods-count").mark("status", text.clone()).flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(text);
        if chosen.len() > 1 {
            let locked = self.locked(cx).map(SharedString::from);
            let any_off = chosen.iter().any(|f| self.entry(f).is_some_and(|e| !e.enabled));
            let any_on = chosen.iter().any(|f| self.entry(f).is_some_and(|e| e.enabled));
            let bulk = |id: &'static str, label: String, look: Look, show: bool, action: Box<dyn Action>| {
                show.then(|| sheet::button_unless(id, label, None, look, locked.clone(), self.busy, t).when(locked.is_none(), |b| b.on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))))
            };
            return line
                .child(caption(trf("%lld ausgewählt", &[&chosen.len()])))
                .children(bulk("mods-enable", tr("Einschalten"), Look::Plain, any_off, Box::new(EnableMods)))
                .children(bulk("mods-disable", tr("Ausschalten"), Look::Plain, any_on, Box::new(DisableMods)))
                .children(bulk("mods-delete", tr("Löschen"), Look::Danger, true, Box::new(DeleteMods)));
        }
        let text = if !self.query.is_empty() {
            trf("%lld von %@", &[&shown, &count_mods(total)])
        } else {
            let mut parts = vec![count_mods(total)];
            let off = self.scan.entries.iter().filter(|e| !e.enabled).count();
            if off > 0 {
                parts.push(trf("%lld aus", &[&off]));
            }
            if self.facts.dups > 0 {
                parts.push(if self.facts.dups == 1 { tr("1 doppelt") } else { trf("%lld doppelt", &[&self.facts.dups]) });
            }
            parts.join(" · ")
        };
        line.child(caption(text))
    }

    fn render_row(&self, ix: usize, row: Row, has_focus: bool, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let Row::Mod(at) = row else {
            return div().h(px(ROW)).px(px(space::M)).pb(px(space::XS)).flex().items_end().child(sheet::label(tr("Eigene Dateien"), t)).into_any_element();
        };
        let e = &self.scan.entries[at];
        let file = e.file.clone();
        let selected = self.selected.contains(&file);
        let focused = has_focus && self.cursor.as_deref() == Some(file.as_str());
        let name = e.name();
        let locked = self.locked(cx);
        let face = match &e.icon {
            Some(path) => {
                let (a, b) = (*t, *t);
                img(path.clone()).image_cache(&self.icons).size(px(FACE)).flex_none().rounded(px(radius::INNER)).object_fit(ObjectFit::Cover).with_loading(move || glyph(&a).into_any_element()).with_fallback(move || glyph(&b).into_any_element()).into_any_element()
            }
            None => glyph(t).into_any_element(),
        };
        let face = div().flex_none().when(!e.enabled, |d| d.opacity(tint::DISABLED)).child(face);
        let dim = !e.enabled;
        let (caption, warn): (String, bool) = if let Some(twin) = self.facts.twin.get(&file).and_then(|f| self.entry(f)) {
            (trf("Doppelt: auch %@ ist da.", &[&twin.version().filter(|v| !v.is_empty()).map(str::to_owned).unwrap_or_else(|| twin.file.clone())]), true)
        } else if let Some(missing) = self.facts.needs.get(&file) {
            (trf("Braucht %@, ist aus.", &[&listing(&self.names(missing))]), true)
        } else if !e.enabled {
            (trf("Aus · %@", &[&e.file]), false)
        } else if !e.on_disk && self.scanned {
            (tr("Kommt beim nächsten Start dazu."), false)
        } else {
            (e.file.clone(), false)
        };
        let version = e.version().filter(|v| !v.is_empty()).map(|v| div().flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).child(v.to_owned()));
        let text = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(div().flex().items_baseline().gap(px(space::S)).when(dim, |d| d.opacity(tint::DISABLED)).child(div().min_w_0().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(name.clone())).children(version))
            .child(div().truncate().text_size(px(font::CAPTION)).text_color(if warn { t.attention_text } else { t.tertiary }).child(caption));
        let trailing = if let Some(older) = self.facts.older.get(&file) {
            let target = file.clone();
            let newest = !older.contains(&file);
            let label = if newest { tr("Ältere Version löschen") } else { tr("Löschen") };
            Some(sheet::button_unless(SharedString::from(format!("mod-fix-{ix}")), label, None, Look::Plain, locked.clone().map(SharedString::from), false, t).when(locked.is_none(), |b| {
                b.on_click(cx.listener(move |s, _, _, cx| {
                    if newest {
                        s.fix(&target, cx);
                    } else {
                        s.trash(vec![target.clone()], None, cx);
                    }
                }))
            }))
        } else if let Some(missing) = self.facts.needs.get(&file) {
            let missing = missing.clone();
            Some(sheet::button_unless(SharedString::from(format!("mod-needs-{ix}")), tr("Einschalten"), None, Look::Plain, locked.clone().map(SharedString::from), false, t).when(locked.is_none(), |b| b.on_click(cx.listener(move |s, _, window, cx| s.switch(missing.clone(), true, window, cx)))))
        } else {
            None
        };
        let check_file = file.clone();
        let check = Checkbox::new(SharedString::from(format!("mod-on-{ix}"))).checked(e.enabled).tab_stop(false).on_click(cx.listener(move |s, on: &bool, window, cx| {
            cx.stop_propagation();
            s.switch(vec![check_file.clone()], *on, window, cx);
        }));
        let check = match locked {
            Some(why) => sheet::unavailable_over(SharedString::from(format!("mod-on-locked-{ix}")), check, why).into_any_element(),
            None => div().id(SharedString::from(format!("mod-on-wrap-{ix}"))).mark("checkbox", name.clone()).flex_none().child(check).into_any_element(),
        };
        let (click_file, menu_file) = (file.clone(), file.clone());
        let me = cx.entity().downgrade();
        div()
            .id(SharedString::from(format!("mod-row-{}", file.to_lowercase())))
            .mark("row", name)
            .h(px(ROW))
            .w_full()
            .px(px(space::M))
            .flex()
            .items_center()
            .gap(px(space::M))
            .rounded(px(radius::INNER))
            .when(selected, |d| d.bg(t.highlight.opacity(tint::SELECTED)))
            .when(focused && !selected, |d| d.border_1().border_color(t.highlight))
            .cursor_pointer()
            .soft(t.ink.opacity(tint::HOVER), radius::INNER)
            .on_click(cx.listener(move |s, e: &ClickEvent, window, cx| s.click(&click_file, e, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |s, _, window, cx| {
                if !s.selected.contains(&menu_file) {
                    s.selected = HashSet::from([menu_file.clone()]);
                    s.anchor = Some(menu_file.clone());
                }
                s.cursor = Some(menu_file.clone());
                window.focus(&s.focus, cx);
                cx.notify();
            }))
            .child(check)
            .child(face)
            .child(text)
            .children(trailing)
            .context_menu(move |menu, _, cx| row_menu(menu, &me, cx))
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        if self.rows.is_empty() {
            let text = trf("Kein Mod passt zu „%@“.", &[&self.filter.read(cx).value()]);
            let clear = sheet::button("mods-filter-clear", tr("Filter löschen"), None, Look::Plain, false, false, t).on_click(cx.listener(|s, _, window, cx| s.clear_filter(window, cx)));
            return div().size_full().flex().flex_col().items_center().justify_center().gap(px(space::S)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(icon("search-x", iconsize::L, t.tertiary)).child(div().id("mods-none").mark("status", text.clone()).child(text)).child(clear).into_any_element();
        }
        uniform_list(
            "mods-list",
            self.rows.len(),
            cx.processor(move |s: &mut ModList, range: std::ops::Range<usize>, window, cx| {
                let t = Theme::get(cx);
                let has_focus = s.focus.is_focused(window);
                let rows: Vec<(usize, Row)> = s.rows[range.clone()].iter().copied().enumerate().map(|(n, r)| (range.start + n, r)).collect();
                rows.into_iter().map(|(ix, r)| s.render_row(ix, r, has_focus, cx, &t)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full()
        .px(px(space::XXS))
        .py(px(space::XXS))
        .into_any_element()
    }

    fn render_empty(&self, made: &Instance, t: &Theme) -> Div {
        let id = made.id.clone();
        let add = sheet::button("mods-add", tr("Mods hinzufügen…"), Some("plus"), Look::Primary, false, false, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(AddMods(id.clone())), cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(space::M))
            .text_center()
            .child(icon("package", iconsize::HERO, t.tertiary))
            .child(div().id("mods-empty").mark("heading", tr("Noch keine Mods")).text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).child(tr("Noch keine Mods")))
            .child(div().max_w(px(control::READING)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Füge Mods von Modrinth hinzu oder zieh Mod-Dateien hierher.")))
            .child(add)
    }
}

fn row_menu(menu: PopupMenu, me: &WeakEntity<ModList>, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let Some(list) = me.upgrade() else { return menu };
    let s = list.read(cx);
    let chosen = s.targets();
    let any_off = chosen.iter().any(|f| s.entry(f).is_some_and(|e| !e.enabled));
    let any_on = chosen.iter().any(|f| s.entry(f).is_some_and(|e| e.enabled));
    let dup = chosen.iter().any(|f| s.facts.older.contains_key(f));
    let locked = s.locked(cx).is_some();
    let mut menu = menu;
    if any_off {
        menu = menu.menu_with_disabled(tr("Einschalten"), Box::new(EnableMods), locked);
    }
    if any_on {
        menu = menu.menu_with_disabled(tr("Ausschalten"), Box::new(DisableMods), locked);
    }
    if dup {
        menu = menu.menu_with_disabled(tr("Doppelte Mods bereinigen"), Box::new(FixDuplicates), locked);
    }
    menu.separator().menu(tr("Modliste kopieren"), Box::new(CopyModList)).separator().menu_with_disabled(if chosen.len() > 1 { trf("%lld Mods löschen", &[&chosen.len()]) } else { tr("Löschen") }, Box::new(DeleteMods), locked)
}

#[derive(Clone, Copy)]
enum Step {
    Start,
    Answer(Answer),
    Stop,
    Dismiss,
}

fn glyph(t: &Theme) -> Div {
    div().size(px(FACE)).flex_none().rounded(px(radius::INNER)).bg(t.raised).flex().items_center().justify_center().child(icon("package", iconsize::S, t.tertiary))
}

pub(super) fn put(made: Instance, cx: &mut App) {
    model::get(cx).update(cx, |m, cx| {
        if let Some(slot) = m.instances.iter_mut().find(|i| i.id == made.id)
            && *slot != made
        {
            *slot = made;
            cx.notify();
        }
    });
}

/// Switches the mods `files` of instance `id` back after an undo; says why when it cannot and offers a retry when that may help.
pub fn undo_switch(id: String, files: Vec<String>, on: bool, label: String, at: Instant, cx: &mut App) {
    let task = cx.background_spawn({
        let (id, files) = (id.clone(), files.clone());
        async move { content::set_enabled(&id, &files, !on) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(made) => put(made, cx),
            Err(e) => {
                super::undo::note(forms::explain_as(tr("Nicht zurückgestellt."), &e).text, cx);
                if !super::undo::permanent(&e) {
                    super::undo::retry(super::undo::Kind::ModsSwitched { instance_id: id, files, on, label }, at, cx);
                }
            }
        });
    })
    .detach();
}

/// Puts deleted mods back after an undo; says why when it cannot and offers a retry when that may help.
pub fn undo_add(id: String, files: Vec<String>, label: String, at: Instant, cx: &mut App) {
    let task = cx.background_spawn({
        let (id, files) = (id.clone(), files.clone());
        async move { content::trash(&id, &files) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok((made, _)) => put(made, cx),
            Err(e) => {
                super::undo::note(forms::explain_as(tr("Nicht entfernt."), &e).text, cx);
                if !super::undo::permanent(&e) {
                    super::undo::retry(super::undo::Kind::ModsAdded { instance_id: id, files, label }, at, cx);
                }
            }
        });
    })
    .detach();
}

pub fn undo_trash(id: String, trashed: Trashed, label: String, at: Instant, cx: &mut App) {
    let task = cx.background_spawn({
        let (id, trashed) = (id.clone(), trashed.clone());
        async move { content::untrash(&id, &trashed) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(made) => put(made, cx),
            Err(e) => {
                super::undo::note(forms::explain_as(tr("Nicht zurückgeholt."), &e).text, cx);
                if !super::undo::permanent(&e) {
                    super::undo::retry(super::undo::Kind::ModsTrashed { instance_id: id, trashed, label }, at, cx);
                }
            }
        });
    })
    .detach();
}

impl Render for ModList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        let root = div().id("mods-tab").size_full().flex().flex_col().gap(px(space::S));
        let Some(made) = self.instance(cx) else { return root };
        let root = root
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(t.accent.opacity(tint::DROP)).rounded(px(radius::CARD)))
            .on_drop(cx.listener(|s, paths: &ExternalPaths, _, cx| {
                s.dropped(paths, cx);
                cx.stop_propagation();
            }));
        let error = self.error.as_ref().map(|e| sheet::problem("mods-error", "circle-alert", t.danger_text, e, self.error_open, cx.listener(|s, _, _, cx| {
            s.error_open = !s.error_open;
            cx.notify();
        }), &t));
        let retry = self.error.as_ref().filter(|_| !self.failed).map(|_| sheet::button("mods-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, &t).on_click(cx.listener(|s, _, _, cx| s.rescan(cx))));
        let problems = error.map(|e| div().flex().items_start().gap(px(space::S)).child(div().flex_1().min_w_0().child(e)).children(retry));
        let bisect = self.bisect.clone().map(|b| self.render_bisect(&b, cx, &t));
        if self.scan.entries.is_empty() && self.bisect.is_none() {
            let body = if self.error.is_some() && !self.failed { div().into_any_element() } else if self.scanned { self.render_empty(&made, &t).into_any_element() } else { div().size_full().flex().items_center().justify_center().child(sheet::waiting(tr("Mods werden gelesen …"), &t)).into_any_element() };
            return root.children(problems).child(div().flex_1().min_h_0().child(body));
        }
        let list = div()
            .id("mods-region")
            .key_context(LIST)
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(t.separator)
            .overflow_hidden()
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, window, cx| s.key_down(e, window, cx)))
            .child(self.render_list(cx, &t));
        root.children(bisect).children(problems).child(self.render_toolbar(&made, cx, &t)).child(self.render_count(cx, &t)).child(list)
    }
}
