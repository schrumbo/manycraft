//! The command palette: every command of the main window by name with its shortcut, and the names of keys as the platform writes them.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{
    AddAccount, AddMicrosoft, CancelUpdate, ChangeSkin, CheckForUpdates, ClearSelection, DismissNews, CancelImport, ExportBatch, ImportBatch, OpenBatchFolder, OpenGameFolder, PickColor, PreviousVersion, RestartToUpdate, SetColor, EditBatch, ImportPrismAccounts, ImportFromLauncher, PickShortcutInstance, SignInEach, MessageSelected, NewBatch, NextAttention, NextInstance, OpenChat, OpenSettings, OpenSettingsAt, Play, PreviousInstance, ReconnectMany, ReconnectSelected, RestartSelected, SelectAll, ShowAccounts, ShowAll, ShowBatch, SignInAgain, Start, StartAll, StartBatch, StartSelected, StopAll, StopBatch, StopSelected, ToggleFullscreen, ToggleSidebar, Undo, Quit, CancelStart, StartAnyway, DeleteBatch,
    AddMods, CloneInstance, DeleteInstance, EditMods, FindModpacks, FindResourcePacks, FindShaders, ResetInstance, UpdateLoader, ExportInstance, NewBatchFor, NewInstance, OpenInstanceFolder, OpenInstanceSetting, PlayInstance, RenameInstance, ShowInstance, CreateShortcut, ShowShortcuts,
};
use super::drive::Mark;
use super::main_window::{FocusSearch, MainWindow, Page};
use super::model::{self, Phase};
use super::motion::Soft;
use super::route::Section;
use super::theme::{Theme, font, iconsize, palette, radius, space, tint};
use super::widgets::{TakeFocus, icon};

actions!(palette, [PaletteNext, PalettePrevious]);
use crate::core::i18n::{tr, trf};
use crate::core::instance::Loader;

/// Where an entry belongs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Selection,
    Recent,
    Commands,
    Pages,
    Batches,
    Instances,
    Accounts,
}

impl Group {
    fn label(self) -> String {
        match self {
            Self::Selection => tr("Auswahl"),
            Self::Recent => tr("Zuletzt verwendet"),
            Self::Commands => tr("Befehle"),
            Self::Pages => tr("Seiten"),
            Self::Batches => tr("Batches"),
            Self::Instances => tr("Instanzen"),
            Self::Accounts => tr("Accounts"),
        }
    }
}

/// One command the palette offers.
pub struct Entry {
    pub group: Group,
    pub icon: &'static str,
    pub label: String,
    pub detail: Option<String>,
    pub keys: Option<String>,
    pub action: Box<dyn Action>,
    /// Whether the command can run now; a disabled one shows why in its detail and does nothing.
    pub enabled: bool,
    /// Whether it starts, stops or deletes many games or a whole instance at once, so the cursor never starts on it.
    pub mass: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    Heading(Group),
    Entry(usize),
}

/// The open palette: its field, its entries and the highlighted row.
pub struct Palette {
    pub input: Entity<InputState>,
    entries: Rc<Vec<Entry>>,
    rows: Rc<Vec<Row>>,
    cursor: usize,
    scroll: UniformListScrollHandle,
    nested: bool,
    _typed: Subscription,
}

impl Palette {
    fn filter(&mut self, query: &str) {
        self.rows = Rc::new(rows(&self.entries, query));
        self.cursor = first_pick(&self.entries, &self.rows).unwrap_or(self.rows.len());
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn step(&mut self, by: isize) {
        let entries: Vec<usize> = self.rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Entry(_))).map(|(ix, _)| ix).collect();
        if entries.is_empty() {
            return;
        }
        let to = match entries.iter().position(|ix| *ix == self.cursor) {
            Some(at) => (at as isize + by).clamp(0, entries.len() as isize - 1) as usize,
            None if by > 0 => 0,
            None => entries.len() - 1,
        };
        self.cursor = entries[to];
        let shown = if to == 0 { 0 } else { self.cursor };
        self.scroll.scroll_to_item(shown, ScrollStrategy::Nearest);
    }

    fn chosen(&self) -> Option<Box<dyn Action>> {
        match self.rows.get(self.cursor)? {
            Row::Entry(ix) => self.entries.get(*ix).filter(|e| e.enabled).map(|e| e.action.boxed_clone()),
            Row::Heading(_) => None,
        }
    }

    fn blocked(&self) -> bool {
        match self.rows.get(self.cursor) {
            Some(Row::Entry(ix)) => self.entries.get(*ix).is_some_and(|e| !e.enabled),
            Some(Row::Heading(_)) => false,
            None => !self.rows.is_empty(),
        }
    }
}

/// Opens the palette, or closes it when it is open.
pub fn toggle(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if this.palette.is_some() {
        close(this, window, cx);
        return;
    }
    let entries = entries(this, cx);
    open(this, entries, tr("Befehl, Batch oder Account suchen"), false, window, cx);
}

/// Opens the palette on the colours of account `id`, so the keyboard picks one like the colour menu does.
pub fn colors(this: &mut MainWindow, id: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    let current = model::get(cx).read(cx).accounts.iter().find(|a| a.id.to_string() == id).map(|a| (a.name.clone(), a.color()));
    let Some((name, current)) = current else { return };
    let entries = (0..crate::core::account::COLORS.len() as u8)
        .map(|color| {
            let mut e = entry(Group::Accounts, "palette", super::identity::name(color), None, Box::new(SetColor { accounts: vec![id.to_owned()], color }), cx);
            if color == current {
                e.detail = Some(tr("Aktuelle Farbe"));
                e.enabled = false;
            }
            e
        })
        .collect();
    open(this, entries, trf("Farbe für %@ suchen", &[&name]), true, window, cx);
}

fn open(this: &mut MainWindow, entries: Vec<Entry>, placeholder: String, nested: bool, window: &mut Window, cx: &mut Context<MainWindow>) {
    this.overlay = false;
    this.shortcuts = false;
    let entries = Rc::new(entries);
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    let me = cx.entity().downgrade();
    let app: &mut App = cx;
    let typed = app.subscribe(&input, move |s, e: &InputEvent, cx| {
        if let InputEvent::Change = e {
            let query = s.read(cx).value().to_string();
            let _ = me.update(cx, |this, cx| {
                if let Some(p) = this.palette.as_mut() {
                    p.filter(&query);
                    cx.notify();
                }
            });
        }
    });
    let rows = Rc::new(rows(&entries, ""));
    let cursor = first_pick(&entries, &rows).unwrap_or(rows.len());
    input.update(cx, |s, cx| s.take_focus(window, cx));
    this.palette = Some(Palette { input, entries, rows, cursor, scroll: UniformListScrollHandle::new(), nested, _typed: typed });
    cx.notify();
}

fn close(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    this.palette = None;
    this.focus_page(window, cx);
    cx.notify();
}

fn pick_shortcut_instance(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let m = model::get(cx).read(cx);
    let entries: Vec<Entry> = m
        .instances
        .iter()
        .map(|made| Entry {
            group: Group::Instances,
            icon: "link",
            label: made.name.clone(),
            detail: Some(made.version_line()),
            keys: None,
            action: Box::new(CreateShortcut { instance: made.id.clone(), account: made.last_account.map(|a| a.to_string()) }),
            enabled: true,
            mass: false,
        })
        .collect();
    let Some(p) = this.palette.as_mut() else { return };
    p.rows = Rc::new(rows(&entries, ""));
    p.entries = Rc::new(entries);
    p.cursor = first_pick(&p.entries, &p.rows).unwrap_or(p.rows.len());
    p.scroll.scroll_to_item(0, ScrollStrategy::Top);
    p.input.update(cx, |s, cx| s.set_value("", window, cx));
    cx.notify();
}

fn run(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if this.palette.as_ref().is_some_and(Palette::blocked) {
        return;
    }
    let action = this.palette.as_ref().and_then(Palette::chosen);
    if let Some(action) = action.as_ref().filter(|_| this.palette.as_ref().is_some_and(|p| !p.nested)) {
        remember(action.as_ref(), cx);
    }
    if action.as_ref().is_some_and(|a| a.as_any().is::<PickShortcutInstance>()) {
        pick_shortcut_instance(this, window, cx);
        return;
    }
    close(this, window, cx);
    if let Some(action) = action {
        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
    }
}

/// How many commands the palette keeps as recently used.
const RECENT_MAX: usize = 5;

struct Recent(Vec<Box<dyn Action>>);

impl Global for Recent {}

fn remember(action: &dyn Action, cx: &mut App) {
    if !cx.has_global::<Recent>() {
        cx.set_global(Recent(Vec::new()));
    }
    let recent = &mut cx.global_mut::<Recent>().0;
    recent.retain(|a| !a.partial_eq(action));
    recent.insert(0, action.boxed_clone());
    recent.truncate(RECENT_MAX);
}

/// Whether `action` starts, stops or deletes many games or a whole instance; selection commands count when `several` are chosen.
fn mass(action: &dyn Action, several: bool) -> bool {
    let a = action.as_any();
    a.is::<StartAll>() || a.is::<StopAll>() || a.is::<CancelStart>() || a.is::<StartBatch>() || a.is::<StopBatch>() || a.is::<StartAnyway>() || a.is::<DeleteInstance>() || a.is::<ResetInstance>()
        || (several && (a.is::<StartSelected>() || a.is::<StopSelected>() || a.is::<RestartSelected>()))
}

/// The row the cursor starts on: the first entry that can run and is no mass action; none when every hit is one.
fn first_pick(entries: &[Entry], rows: &[Row]) -> Option<usize> {
    rows.iter().position(|r| matches!(r, Row::Entry(ix) if entries[*ix].enabled && !entries[*ix].mass))
}

/// The rows for `query`: every entry under its group heading when it is empty, otherwise the hits, best first.
fn rows(entries: &[Entry], query: &str) -> Vec<Row> {
    if query.trim().is_empty() {
        let mut rows = Vec::with_capacity(entries.len() + 5);
        for (ix, e) in entries.iter().enumerate() {
            if ix == 0 || entries[ix - 1].group != e.group {
                rows.push(Row::Heading(e.group));
            }
            rows.push(Row::Entry(ix));
        }
        return rows;
    }
    let mut hits: Vec<(i32, usize)> = entries.iter().enumerate().filter(|(_, e)| e.group != Group::Recent).filter_map(|(ix, e)| score(query, &e.label, e.detail.as_deref().unwrap_or("")).map(|s| (s, ix))).collect();
    hits.sort_by_key(|(s, ix)| (-s, *ix));
    hits.into_iter().map(|(_, ix)| Row::Entry(ix)).collect()
}

/// How well `query` matches an entry; every word must match its label or, weighing less, its detail, in order of its letters.
pub fn score(query: &str, label: &str, detail: &str) -> Option<i32> {
    let (label, detail) = (label.to_lowercase(), detail.to_lowercase());
    query.to_lowercase().split_whitespace().try_fold(0, |total, word| Some(total + word_score(word, &label).map(|s| s * 2).or_else(|| word_score(word, &detail))?))
}

fn word_score(word: &str, text: &str) -> Option<i32> {
    if let Some(at) = text.find(word) {
        let starts_word = text.get(..at).and_then(|before| before.chars().next_back()).is_none_or(|c| !c.is_alphanumeric());
        let base = if at == 0 { 100 } else if starts_word { 80 } else { 50 };
        return Some(base - (at.min(40) as i32) / 4);
    }
    let chars: Vec<char> = text.chars().collect();
    let first = word.chars().next()?;
    let limit = word.chars().count() * 2;
    (0..chars.len())
        .filter(|&at| chars[at] == first && (at == 0 || !chars[at - 1].is_alphanumeric()))
        .filter_map(|at| {
            let mut rest = chars[at + 1..].iter();
            let mut gaps = 0;
            for c in word.chars().skip(1) {
                loop {
                    match rest.next() {
                        Some(t) if *t == c => break,
                        Some(_) => gaps += 1,
                        None => return None,
                    }
                }
            }
            (gaps <= limit).then(|| (30 - gaps as i32).max(1))
        })
        .max()
}

/// A keystroke as the platform writes it, with German key names: ⌃⌥⇧⌘K on the Mac, Strg+Alt+Umschalt+K elsewhere.
pub fn label(k: &Keystroke) -> String {
    let mac = cfg!(target_os = "macos");
    let m = &k.modifiers;
    let key = match k.key.as_str() {
        "enter" => if mac { "↩".to_owned() } else { tr("Eingabe") },
        "escape" => if mac { "⎋".to_owned() } else { "Esc".to_owned() },
        "backspace" => if mac { "⌫".to_owned() } else { tr("Rücktaste") },
        "delete" => if mac { "⌦".to_owned() } else { tr("Entf") },
        "space" => tr("Leertaste"),
        "tab" => if mac { "⇥".to_owned() } else { "Tab".to_owned() },
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "pageup" => tr("Bild auf"),
        "pagedown" => tr("Bild ab"),
        "home" => tr("Pos1"),
        "end" => tr("Ende"),
        "." if !mac => tr("Punkt"),
        "," if !mac => tr("Komma"),
        "+" if !mac => tr("Plus"),
        "-" if !mac => tr("Minus"),
        other => other.to_uppercase(),
    };
    if mac {
        let mut out = String::new();
        for (on, glyph) in [(m.control, "⌃"), (m.alt, "⌥"), (m.shift, "⇧"), (m.platform, "⌘")] {
            if on {
                out.push_str(glyph);
            }
        }
        out + &key
    } else {
        let mut parts: Vec<String> = [(m.control, tr("Strg")), (m.alt, "Alt".to_owned()), (m.shift, tr("Umschalt")), (m.platform, "Win".to_owned())].into_iter().filter(|(on, _)| *on).map(|(_, name)| name).collect();
        parts.push(key);
        parts.join("+")
    }
}

/// The shortcut bound to `action` in any context, as `label` writes it.
pub fn shortcut(action: &dyn Action, cx: &App) -> Option<String> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let binding = keymap.bindings_for_action(action).last()?;
    binding.keystrokes().first().map(|k| label(k.as_keystroke()))
}

/// `text` followed by the shortcut of `action`, for tooltips.
pub fn hint(text: String, action: &dyn Action, cx: &App) -> String {
    match shortcut(action, cx) {
        Some(keys) => format!("{text} · {keys}"),
        None => text,
    }
}

fn step(this: &mut MainWindow, by: isize, cx: &mut Context<MainWindow>) {
    if let Some(p) = this.palette.as_mut() {
        p.step(by);
        cx.notify();
    }
}

/// A menu row with its shortcut on the right in the app's own key names, for menus whose built-in key labels are English; a `disabled` row is dimmed.
pub fn menu_row(text: String, keys: Option<String>, disabled: bool) -> impl Fn(&mut Window, &mut App) -> AnyElement + 'static {
    move |_, cx| {
        let t = Theme::get(cx);
        div()
            .flex_1()
            .flex()
            .items_center()
            .gap(px(space::L))
            .when(disabled, |d| d.text_color(t.tertiary))
            .child(div().flex_1().child(text.clone()))
            .children(keys.clone().map(|k| div().flex_none().text_color(t.tertiary).child(k)))
            .into_any_element()
    }
}

fn entry(group: Group, icon: &'static str, label: String, detail: Option<String>, action: Box<dyn Action>, cx: &App) -> Entry {
    let keys = shortcut(action.as_ref(), cx);
    Entry { group, icon, label, detail, keys, action, enabled: true, mass: false }
}

fn entries(this: &MainWindow, cx: &App) -> Vec<Entry> {
    let m = model::get(cx).read(cx);
    let mut out = Vec::new();
    let grid = matches!(this.page, Page::Grid(_));
    let chosen = if grid { this.grid.chosen() } else { Vec::new() };
    if !chosen.is_empty() {
        let detail = match chosen.as_slice() {
            [one] => m.games.get(one).map(|i| i.name.clone()),
            many => Some(trf("%lld Accounts", &[&many.len()])),
        };
        let n = super::grid::offers(&chosen, m).map(|o| o.able);
        let several = chosen.len() > 1;
        for (icon, label, action, able) in [
            ("play", tr("Ausgewählte starten"), Box::new(StartSelected) as Box<dyn Action>, n[0] > 0),
            ("square", tr("Ausgewählte stoppen"), Box::new(StopSelected), n[1] > 0),
            ("rotate-cw", tr("Ausgewählte neu starten"), Box::new(RestartSelected), n[2] > 0),
            ("refresh-cw", tr("Ausgewählte neu verbinden"), Box::new(ReconnectSelected), n[3] > 0),
            ("message-square", tr("Nachricht senden…"), Box::new(MessageSelected), several && n[4] > 0),
            ("x", tr("Auswahl aufheben"), Box::new(ClearSelection), true),
            ("folder-open", tr("Spielordner öffnen"), Box::new(OpenGameFolder(chosen[0].clone())), !several),
            ("rotate-ccw-clock", tr("Verlauf"), Box::new(super::actions::ShowHistory(chosen[0].clone())), !several),
        ] {
            if able {
                out.push(entry(Group::Selection, icon, label, detail.clone(), action, cx));
            }
        }
        let (accounts, current) = super::tile::accounts_of(&chosen, cx);
        for color in (0..crate::core::account::COLORS.len() as u8).filter(|c| current != Some(*c)) {
            let label = trf("Farbe: %@", &[&super::identity::name(color)]);
            out.push(entry(Group::Selection, "palette", label, detail.clone(), Box::new(SetColor { accounts: accounts.clone(), color }), cx));
        }
    }
    if let Page::Instance(id) = &this.page {
        out.extend(super::instance_files::palette(id, cx));
    }
    let stopped = m.games.values().any(|i| !i.phase.is_active());
    let active = m.games.values().any(|i| i.phase.stoppable() && !i.stopping);
    let playable = m.games.values().any(|i| super::tile::playable(&i.phase));
    let attention = m.attention_count(None) > 0;
    let ready = matches!(m.updates.status, model::UpdateStatus::Ready { .. }) && super::updates::counts(cx).0 == 0;
    let run = m.run(None);
    for (icon, label, action, able) in [
        ("play", tr("Alle starten"), Box::new(StartAll) as Box<dyn Action>, stopped),
        ("square", tr("Alle stoppen"), Box::new(StopAll), active),
        ("x", tr("Start abbrechen"), Box::new(CancelStart(None)), run.is_some()),
        ("play", tr("Trotzdem alle starten"), Box::new(StartAnyway(None)), run.is_some_and(|r| r.memory > 0)),
        ("user-plus", tr("Account hinzufügen"), Box::new(AddAccount), true),
        ("user-plus", tr("Microsoft-Account hinzufügen"), Box::new(AddMicrosoft), true),
        ("plus", tr("Neuer Batch"), Box::new(NewBatch), true),
        ("plus", tr("Neue Instanz"), Box::new(NewInstance), true),
        ("search", tr("Modpacks suchen…"), Box::new(FindModpacks), true),
        ("search", tr("Suchen"), Box::new(FocusSearch), this.can_search(cx)),
        ("square-check", tr("Alles auswählen"), Box::new(SelectAll), grid && !m.games.is_empty()),
        ("gamepad-2", tr("Nächsten Account spielen"), Box::new(NextInstance), playable),
        ("gamepad-2", tr("Vorherigen Account spielen"), Box::new(PreviousInstance), playable),
        ("triangle-alert", tr("Nächsten, der dich braucht"), Box::new(NextAttention), attention),
        ("panel-left", tr("Seitenleiste ein- oder ausblenden"), Box::new(ToggleSidebar), true),
        ("maximize", tr("Vollbild ein- oder ausschalten"), Box::new(ToggleFullscreen), true),
        ("settings", tr("Einstellungen"), Box::new(OpenSettings), true),
        ("smartphone", tr("Einstellungen: Aufs Handy"), Box::new(OpenSettingsAt(Section::Phone)), true),
        ("refresh-cw", tr("Einstellungen: Automatisch neu verbinden"), Box::new(OpenSettingsAt(Section::Reconnect)), true),
        ("keyboard", tr("Tastenkürzel"), Box::new(ShowShortcuts), true),
        ("refresh-cw", tr("Nach Updates suchen"), Box::new(CheckForUpdates), true),
        ("x", tr("Laden abbrechen"), Box::new(CancelUpdate), super::updates::cancellable(&m.updates)),
        ("download", tr("Neu starten und Update installieren"), Box::new(RestartToUpdate), ready),
        ("rotate-ccw-clock", tr("Zur vorherigen Version zurückkehren"), Box::new(PreviousVersion), m.updates.previous.is_some()),
        ("x", tr("Hinweis zur neuen Version ausblenden"), Box::new(DismissNews), m.updates.news.is_some()),
        ("download", tr("Accounts aus Prism übernehmen"), Box::new(ImportPrismAccounts), this.prism.1 > 0),
        ("power", tr("Manycraft beenden"), Box::new(Quit), true),
    ] {
        if able {
            out.push(entry(Group::Commands, icon, label, None, action, cx));
        }
    }
    let scope = if let Page::Grid(batch) = &this.page { batch.clone() } else { None };
    for (need, count, fix) in super::needs::fixes(scope.as_deref(), m) {
        if let Some((label, action)) = fix {
            let (_, _, icon) = super::needs::look(need, &Theme::get(cx));
            out.push(entry(Group::Commands, icon, label, Some(super::needs::label(need, count)), action, cx));
        }
    }
    let outages: Vec<String> = match scope.as_deref() {
        Some(b) => vec![b.to_owned()],
        None => m.batches.iter().map(|b| b.id.clone()).collect(),
    };
    for id in outages {
        if let Some(outage) = m.outage(&id) {
            let name = m.batch(&id).map(|b| b.name.clone());
            out.push(entry(Group::Commands, "refresh-cw", tr("Jetzt alle verbinden"), Some(super::needs::outage_line(&outage, name.as_deref(), m)), Box::new(ReconnectMany(outage.keys)), cx));
        }
    }
    if let Some(newest) = super::undo::newest(cx) {
        out.push(entry(Group::Commands, "undo-2", tr("Rückgängig"), Some(newest), Box::new(Undo), cx));
    }
    if matches!(this.page, Page::Instance(_)) {
        for c in super::mod_list::commands(cx) {
            let mut e = entry(Group::Commands, c.icon, c.label, c.blocked.clone(), c.action, cx);
            e.enabled = c.blocked.is_none();
            out.push(e);
        }
    }
    out.push(entry(Group::Commands, "download", tr("Instanz importieren…"), Some(tr("Modpack (.mrpack) oder Prism-Export")), Box::new(ImportBatch), cx));
    match &this.page {
        Page::Instance(id) if m.instance(id).is_some() => {
            let made = m.instance(id).map(|made| (made.name.clone(), made.last_account));
            if let Some((name, account)) = made {
                out.push(entry(Group::Commands, "link", trf("Verknüpfung für %@ erstellen", &[&name]), None, Box::new(CreateShortcut { instance: id.clone(), account: account.map(|a| a.to_string()) }), cx));
            }
        }
        _ => {}
    }
    if !m.instances.is_empty() {
        out.push(entry(Group::Commands, "link", tr("Verknüpfung erstellen…"), Some(tr("Instanz wählen")), Box::new(PickShortcutInstance), cx));
    }
    out.push(entry(Group::Commands, "download", tr("Aus anderem Launcher importieren…"), Some(tr("Prism, Modrinth, CurseForge und mehr")), Box::new(ImportFromLauncher), cx));
    for i in m.imports.iter().filter(|i| !i.cancel.load(std::sync::atomic::Ordering::SeqCst)) {
        out.push(entry(Group::Commands, "x", tr("Import abbrechen"), Some(i.name.clone()), Box::new(CancelImport(i.id)), cx));
    }
    let waiting = super::accounts::waiting(cx);
    if waiting.len() > 1 {
        let detail = trf("%lld Accounts", &[&waiting.len()]);
        out.push(entry(Group::Commands, "log-in", tr("Alle nacheinander anmelden"), Some(detail), Box::new(SignInEach(waiting)), cx));
    }
    for (icon, label, action) in [("layout-grid", tr("Alle"), Box::new(ShowAll) as Box<dyn Action>), ("users", tr("Accounts"), Box::new(ShowAccounts)), ("messages-square", tr("Chat"), Box::new(OpenChat))] {
        out.push(entry(Group::Pages, icon, label, None, action, cx));
    }
    let shown = if let Page::Grid(Some(b)) = &this.page { Some(b.as_str()) } else { None };
    let mut batches: Vec<_> = m.batches.iter().collect();
    batches.sort_by_key(|b| Some(b.id.as_str()) != shown);
    for b in batches {
        let server = b.server.clone().filter(|s| !s.is_empty());
        out.push(entry(Group::Batches, "boxes", b.name.clone(), server.clone(), Box::new(ShowBatch(b.id.clone())), cx));
        out.push(entry(Group::Batches, "play", trf("%@ starten", &[&b.name]), server.clone(), Box::new(StartBatch(b.id.clone())), cx));
        out.push(entry(Group::Batches, "square", trf("%@ stoppen", &[&b.name]), server.clone(), Box::new(StopBatch(b.id.clone())), cx));
        out.push(entry(Group::Batches, "pencil", trf("%@ bearbeiten", &[&b.name]), server.clone(), Box::new(EditBatch(b.id.clone())), cx));
        out.push(entry(Group::Batches, "folder-open", trf("Ordner von %@ öffnen", &[&b.name]), server.clone(), Box::new(OpenBatchFolder(b.id.clone())), cx));
        if !m.exporting.contains(&b.instance_id) {
            out.push(entry(Group::Batches, "upload", trf("%@ als .mrpack exportieren…", &[&b.name]), server.clone(), Box::new(ExportBatch(b.id.clone())), cx));
        }
        out.push(entry(Group::Batches, "trash-2", trf("%@ löschen…", &[&b.name]), server, Box::new(DeleteBatch(b.id.clone())), cx));
    }
    if let Page::Instance(shown) = &this.page
        && let Some(made) = m.instances.iter().find(|i| &i.id == shown)
    {
        let settings = [
            ("memory-stick", tr("Arbeitsspeicher einstellen"), "memory", true),
            ("coffee", tr("Java wählen"), "java", true),
            ("terminal", tr("Java-Argumente bearbeiten"), "jvm_args", true),
            ("terminal", tr("Spielargumente bearbeiten"), "game_args", true),
            ("app-window", tr("Fenstergröße einstellen"), "window", true),
            ("cpu", tr("Grafikkarte wählen"), "gpu", cfg!(windows)),
            ("monitor", tr("Grafikschnittstelle wählen"), "renderer", crate::core::options::renderer_choice(&made.game_version)),
        ];
        for (icon, label, setting, able) in settings {
            if able {
                out.push(entry(Group::Instances, icon, label, Some(made.name.clone()), Box::new(OpenInstanceSetting { instance: made.id.clone(), setting: setting.to_owned() }), cx));
            }
        }
    }
    for made in &m.instances {
        let id = made.id.clone();
        let detail = Some(made.version_line());
        let name = &made.name;
        out.push(entry(Group::Instances, "package", trf("Instanz %@ öffnen", &[name]), detail.clone(), Box::new(ShowInstance(id.clone())), cx));
        out.push(entry(Group::Instances, "play", trf("%@ spielen", &[name]), detail.clone(), Box::new(PlayInstance { instance: id.clone(), account: None }), cx));
        out.push(entry(Group::Instances, "boxes", trf("Neuer Batch mit %@…", &[name]), detail.clone(), Box::new(NewBatchFor(id.clone())), cx));
        if made.loader.takes_mods() {
            out.push(entry(Group::Instances, "plus", trf("Mods zu %@ hinzufügen…", &[name]), detail.clone(), Box::new(AddMods(id.clone())), cx));
            out.push(entry(Group::Instances, "package", trf("Mods von %@ verwalten…", &[name]), detail.clone(), Box::new(EditMods(id.clone())), cx));
        }
        out.push(entry(Group::Instances, "image", tr("Ressourcenpakete suchen…"), Some(name.clone()), Box::new(FindResourcePacks(id.clone())), cx));
        out.push(entry(Group::Instances, "sun", tr("Shader suchen…"), Some(name.clone()), Box::new(FindShaders(id.clone())), cx));
        if matches!(made.loader, Loader::Fabric(Some(_)) | Loader::Quilt(Some(_))) {
            out.push(entry(Group::Instances, "refresh-cw", trf("Loader von %@ aktualisieren…", &[name]), detail.clone(), Box::new(UpdateLoader(id.clone())), cx));
        }
        let mut reset = entry(Group::Instances, "rotate-ccw", trf("%@ zurücksetzen…", &[name]), detail.clone(), Box::new(ResetInstance(id.clone())), cx);
        if m.games.values().any(|g| g.instance_id == made.id && g.phase.is_active()) {
            reset.detail = Some(tr("Stoppe zuerst die laufenden Spiele."));
            reset.enabled = false;
        }
        out.push(reset);
        out.push(entry(Group::Instances, "pencil", trf("%@ umbenennen…", &[name]), detail.clone(), Box::new(RenameInstance(id.clone())), cx));
        out.push(entry(Group::Instances, "copy", trf("%@ klonen", &[name]), detail.clone(), Box::new(CloneInstance(id.clone())), cx));
        out.push(entry(Group::Instances, "folder-open", trf("Ordner von %@ öffnen", &[name]), detail.clone(), Box::new(OpenInstanceFolder(id.clone())), cx));
        out.extend(super::instance_files::tab_entries(made, cx));
        out.push(entry(Group::Instances, "upload", trf("%@ als .mrpack exportieren…", &[name]), detail.clone(), Box::new(ExportInstance(id.clone())), cx));
        let mut delete = entry(Group::Instances, "trash", trf("%@ löschen…", &[name]), detail, Box::new(DeleteInstance(id)), cx);
        let users: Vec<String> = m.batches.iter().filter(|b| b.instance_id == made.id).map(|b| b.name.clone()).collect();
        if !users.is_empty() {
            delete.detail = Some(crate::core::fleet::used_by(&users));
            delete.enabled = false;
        }
        out.push(delete);
    }
    for a in &m.accounts {
        out.push(entry(Group::Accounts, "palette", trf("%@ — Farbe ändern…", &[&a.name]), Some(super::identity::name(a.color())), Box::new(PickColor(a.id.to_string())), cx));
        if a.kind == crate::core::account::AccountKind::Microsoft {
            out.push(entry(Group::Accounts, "shirt", trf("%@ — Skin ändern", &[&a.name]), None, Box::new(ChangeSkin(a.id.to_string())), cx));
        }
    }
    let mut instances: Vec<_> = m.games.values().collect();
    instances.sort_by_cached_key(|i| (i.name.to_lowercase(), m.batch(&i.batch_id).map(|b| b.name.to_lowercase())));
    for i in instances {
        let batch = m.batch(&i.batch_id).map(|b| b.name.clone()).unwrap_or_default();
        let detail = Some(format!("{batch} · {}", super::state::status(i, None)));
        let (icon, verb, action): (&'static str, String, Box<dyn Action>) = if super::tile::playable(&i.phase) {
            ("gamepad-2", tr("Spielen"), Box::new(Play(i.key.clone())))
        } else if i.phase == Phase::LoginNeeded {
            ("log-in", tr("Anmelden"), Box::new(SignInAgain(i.account_id.to_string())))
        } else {
            ("play", tr("Starten"), Box::new(Start(i.key.clone())))
        };
        out.push(Entry { group: Group::Accounts, icon, label: format!("{} — {verb}", i.name), detail, keys: None, action, enabled: true, mass: false });
    }
    let several = chosen.len() > 1;
    for e in &mut out {
        e.mass = mass(e.action.as_ref(), several);
    }
    let recent: Vec<Entry> = cx
        .try_global::<Recent>()
        .map(|r| {
            r.0.iter()
                .filter_map(|a| out.iter().find(|e| e.enabled && e.action.partial_eq(a.as_ref())))
                .map(|e| Entry { group: Group::Recent, icon: e.icon, label: e.label.clone(), detail: e.detail.clone(), keys: e.keys.clone(), action: e.action.boxed_clone(), enabled: true, mass: e.mass })
                .collect()
        })
        .unwrap_or_default();
    let at = out.iter().take_while(|e| e.group == Group::Selection).count();
    out.splice(at..at, recent);
    out
}

/// Draws the open palette over the window.
pub fn render(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let p = this.palette.as_ref()?;
    let t = Theme::get(cx);
    let viewport = window.viewport_size();
    let width = palette::WIDTH.min(f32::from(viewport.width) - space::L * 2.);
    let left = ((f32::from(viewport.width) - width) / 2.).max(space::L);
    let count = p.rows.len();
    let visible = (count as f32).min(palette::ROWS);
    let list = uniform_list(
        "palette-list",
        count,
        cx.processor(move |this: &mut MainWindow, range: Range<usize>, _, cx| {
            let t = Theme::get(cx);
            let Some(p) = this.palette.as_ref() else { return Vec::new() };
            let (rows, entries, cursor) = (Rc::clone(&p.rows), Rc::clone(&p.entries), p.cursor);
            range
                .filter_map(|ix| {
                    Some(match *rows.get(ix)? {
                        Row::Heading(g) => div().id(("palette-row", ix)).h(px(palette::ROW)).px(px(space::M)).pb(px(space::XS)).flex().items_end().text_size(px(font::CAPTION)).font_weight(FontWeight::SEMIBOLD).text_color(t.tertiary).child(g.label()).into_any_element(),
                        Row::Entry(n) => {
                            let e = entries.get(n)?;
                            let on = ix == cursor;
                            let row = div()
                                .id(("palette-row", ix))
                                .mark("row", e.label.clone())
                                .w_full()
                                .h(px(palette::ROW))
                                .px(px(space::S))
                                .rounded(px(radius::INNER))
                                .flex()
                                .items_center()
                                .gap(px(space::S))
                                .when(e.enabled, |d| d.cursor_pointer())
                                .when(!e.enabled, |d| d.cursor_not_allowed())
                                .when(on, |d| d.bg(t.highlight.opacity(tint::SELECTED)))
                                .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                                .child(icon(e.icon, iconsize::M, if !e.enabled { t.tertiary } else if on { t.accent_text } else { t.secondary }))
                                .child(div().flex_none().max_w(relative(palette::LABEL)).truncate().text_size(px(font::BODY)).text_color(if !e.enabled { t.tertiary } else if on { t.accent_text } else { t.primary }).child(e.label.clone()))
                                .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(if !e.enabled { t.tertiary } else if on { t.accent_text } else { t.secondary }).children(e.detail.clone()))
                                .children(e.keys.clone().map(|k| div().flex_none().min_w(px(palette::KEYS)).text_right().text_size(px(font::CALLOUT)).text_color(if on { t.accent_text } else { t.tertiary }).child(k)))
                                .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                                    if let Some(p) = this.palette.as_mut()
                                        && p.cursor != ix
                                    {
                                        p.cursor = ix;
                                        cx.notify();
                                    }
                                }))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    if let Some(p) = this.palette.as_mut() {
                                        p.cursor = ix;
                                    }
                                    run(this, window, cx);
                                }));
                            div().w_full().px(px(space::XS)).child(row).into_any_element()
                        }
                    })
                })
                .collect()
        }),
    )
    .track_scroll(&p.scroll)
    .size_full();
    let body = if count == 0 {
        div().h(px(palette::EMPTY)).flex().items_center().justify_center().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Keine Treffer")).into_any_element()
    } else {
        div().h(px(visible * palette::ROW + space::XS * 2.)).py(px(space::XS)).child(list).into_any_element()
    };
    Some(
        div()
            .id("palette-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| close(this, window, cx)))
            .child(
                div()
                    .id("palette")
                    .mark("palette", tr("Befehle"))
                    .key_context("Palette")
                    .on_action(cx.listener(|this, _: &PaletteNext, _, cx| step(this, 1, cx)))
                    .on_action(cx.listener(|this, _: &PalettePrevious, _, cx| step(this, -1, cx)))
                    .absolute()
                    .top(px(palette::TOP))
                    .left(px(left))
                    .w(px(width))
                    .bg(t.content)
                    .rounded(px(radius::CARD))
                    .border_1()
                    .border_color(t.separator)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                        let page = palette::ROWS as isize - 1;
                        let by = match e.keystroke.key.as_str() {
                            "escape" => {
                                cx.stop_propagation();
                                if this.palette.as_ref().is_some_and(|p| p.nested) {
                                    this.palette = None;
                                    return toggle(this, window, cx);
                                }
                                return close(this, window, cx);
                            }
                            "backspace" if this.palette.as_ref().is_some_and(|p| p.nested && p.input.read(cx).value().is_empty()) => {
                                cx.stop_propagation();
                                this.palette = None;
                                return toggle(this, window, cx);
                            }
                            "enter" => {
                                cx.stop_propagation();
                                return run(this, window, cx);
                            }
                            "up" => -1,
                            "down" => 1,
                            "pageup" => -page,
                            "pagedown" => page,
                            _ => return,
                        };
                        cx.stop_propagation();
                        step(this, by, cx);
                    }))
                    .child(div().h(px(palette::FIELD)).px(px(space::M)).flex().items_center().gap(px(space::S)).child(icon("search", iconsize::M, t.tertiary)).child(div().flex_1().min_w_0().text_size(px(font::TITLE3)).child(Input::new(&p.input).appearance(false))))
                    .child(div().h(px(1.)).bg(t.separator))
                    .child(body),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Entry, Group, Row, first_pick, label, mass, rows, score};
    use crate::ui::actions::{CancelStart, NewBatch, StartAll, StartBatch, StartSelected, StopAll, StopBatch};
    use gpui_kit::Action;

    #[test]
    fn the_cursor_never_starts_on_a_mass_action() {
        let entry = |label: &str, action: Box<dyn Action>| {
            let mass = mass(action.as_ref(), false);
            Entry { group: Group::Commands, icon: "play", label: label.into(), detail: None, keys: None, action, enabled: true, mass }
        };
        let entries = vec![entry("Alle starten", Box::new(StartAll)), entry("Alle stoppen", Box::new(StopAll)), entry("Test starten", Box::new(StartBatch("b".into()))), entry("Neuer Batch", Box::new(NewBatch))];
        for query in ["", "a", "alle", "start", "st", "neu", "batch"] {
            let rows = rows(&entries, query);
            if let Some(Row::Entry(ix)) = first_pick(&entries, &rows).map(|at| rows[at]) {
                assert!(!entries[ix].mass, "{query:?} starts on {}", entries[ix].label);
            }
        }
        assert_eq!(first_pick(&entries, &rows(&entries, "alle st")), None, "only mass hits: Enter does nothing until an arrow picks one");
        assert!(mass(&StopBatch("b".into()), false) && mass(&StartSelected, true));
        assert!(!mass(&StartSelected, false), "one chosen game is no mass action");
        assert!(mass(&CancelStart(None), false), "cancelling stops every game still in line");
    }
    use gpui_kit::Keystroke;

    #[test]
    fn words_match_in_any_order_and_word_starts_win() {
        assert!(score("start", "Alle starten", "").is_some());
        assert!(score("alt sta", "AltOne — Starten", "Farm · Gestoppt").is_some());
        assert!(score("nvb", "Neu verbinden", "").is_some());
        assert!(score("xyz", "Alle starten", "").is_none());
        assert!(score("farm", "AltOne — Starten", "Farm · Gestoppt").is_some());
        assert!(score("alst", "Alle starten", "").is_some(), "letters in order from a word start match");
        assert!(score("seite", "Test bearbeiten", "").is_none(), "scattered letters do not");
        assert!(score("st", "Stoppen", "") > score("st", "Ausgewählte stoppen", ""));
        assert!(score("stop", "Alle stoppen", "") > score("stop", "Batch", "Alle stoppen"), "the label weighs more than the detail");
    }

    #[test]
    fn keys_use_the_platform_notation_and_german_names() {
        crate::core::i18n::init("de");
        let k = |s: &'static str| Keystroke::parse(s).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(label(&k("cmd-shift-r")), "⇧⌘R");
            assert_eq!(label(&k("ctrl-alt-right")), "⌃⌥→");
            assert_eq!(label(&k("cmd-enter")), "⌘↩");
        } else {
            assert_eq!(label(&k("ctrl-shift-r")), "Strg+Umschalt+R");
            assert_eq!(label(&k("ctrl-alt-right")), "Strg+Alt+→");
            assert_eq!(label(&k("ctrl-enter")), "Strg+Eingabe");
            assert_eq!(label(&k("escape")), "Esc");
        }
    }
}
