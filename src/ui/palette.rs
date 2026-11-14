//! The command palette: every command of the main window by name with its shortcut, and the names of keys as the platform writes them.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{
    AddAccount, AddMicrosoft, ClearSelection, EditBatch, MessageSelected, NewBatch, NextAttention, NextInstance, OpenChat, OpenSettings, Play, PreviousInstance, ReconnectSelected, RestartSelected, SelectAll, ShowAccounts, ShowAll, ShowBatch, SignInAgain, Start, StartAll, StartBatch,
    StartSelected, StopAll, StopBatch, StopSelected, TileLarger, TileSmaller, ToggleSidebar,
};
use super::drive::Mark;
use super::main_window::{FocusSearch, MainWindow, TileFit, TileLarge, TileMedium, TileSmall};
use super::model::{self, Phase};
use super::motion::Soft;
use super::theme::{Theme, font, iconsize, palette, radius, space, tint};
use super::widgets::icon;
use crate::core::i18n::{tr, trf};

/// Where an entry belongs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Selection,
    Commands,
    Pages,
    Batches,
    Accounts,
}

impl Group {
    fn label(self) -> String {
        match self {
            Self::Selection => tr("Auswahl"),
            Self::Commands => tr("Befehle"),
            Self::Pages => tr("Seiten"),
            Self::Batches => tr("Batches"),
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
    _typed: Subscription,
}

impl Palette {
    fn filter(&mut self, query: &str) {
        self.rows = Rc::new(rows(&self.entries, query));
        self.cursor = self.rows.iter().position(|r| matches!(r, Row::Entry(_))).unwrap_or(0);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn step(&mut self, by: isize) {
        let entries: Vec<usize> = self.rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Entry(_))).map(|(ix, _)| ix).collect();
        let Some(at) = entries.iter().position(|ix| *ix == self.cursor) else { return };
        let to = (at as isize + by).clamp(0, entries.len() as isize - 1) as usize;
        self.cursor = entries[to];
        let shown = if to == 0 { 0 } else { self.cursor };
        self.scroll.scroll_to_item(shown, ScrollStrategy::Nearest);
    }

    fn chosen(&self) -> Option<Box<dyn Action>> {
        match self.rows.get(self.cursor)? {
            Row::Entry(ix) => self.entries.get(*ix).map(|e| e.action.boxed_clone()),
            Row::Heading(_) => None,
        }
    }
}

/// Opens the palette, or closes it when it is open.
pub fn toggle(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if this.palette.is_some() {
        close(this, window, cx);
        return;
    }
    let entries = Rc::new(entries(this, cx));
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Befehl, Batch oder Account suchen")));
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
    let cursor = rows.iter().position(|r| matches!(r, Row::Entry(_))).unwrap_or(0);
    input.update(cx, |s, cx| s.focus(window, cx));
    this.palette = Some(Palette { input, entries, rows, cursor, scroll: UniformListScrollHandle::new(), _typed: typed });
    cx.notify();
}

fn close(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    this.palette = None;
    this.focus_page(window, cx);
    cx.notify();
}

fn run(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let action = this.palette.as_ref().and_then(Palette::chosen);
    close(this, window, cx);
    if let Some(action) = action {
        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
    }
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
    let mut hits: Vec<(i32, usize)> = entries.iter().enumerate().filter_map(|(ix, e)| score(query, &e.label, e.detail.as_deref().unwrap_or("")).map(|s| (s, ix))).collect();
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
    let mut rest = text.chars();
    let mut gaps = 0;
    for c in word.chars() {
        loop {
            match rest.next() {
                Some(t) if t == c => break,
                Some(_) => gaps += 1,
                None => return None,
            }
        }
    }
    Some((30 - gaps).max(1))
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

/// A menu row with its shortcut on the right in the app's own key names, for menus whose built-in key labels are English.
pub fn menu_row(text: String, keys: Option<String>) -> impl Fn(&mut Window, &mut App) -> AnyElement + 'static {
    move |_, cx| {
        let t = Theme::get(cx);
        div().flex_1().flex().items_center().gap(px(space::L)).child(div().flex_1().child(text.clone())).children(keys.clone().map(|k| div().flex_none().text_color(t.tertiary).child(k))).into_any_element()
    }
}

fn entry(group: Group, icon: &'static str, label: String, detail: Option<String>, action: Box<dyn Action>, cx: &App) -> Entry {
    let keys = shortcut(action.as_ref(), cx);
    Entry { group, icon, label, detail, keys, action }
}

fn entries(this: &MainWindow, cx: &App) -> Vec<Entry> {
    let m = model::get(cx).read(cx);
    let mut out = Vec::new();
    let chosen = this.grid.chosen();
    if !chosen.is_empty() {
        let detail = match chosen.as_slice() {
            [one] => m.instances.get(one).map(|i| i.name.clone()),
            many => Some(trf("%lld Accounts", &[&many.len()])),
        };
        for (icon, label, action) in [
            ("play", tr("Ausgewählte starten"), Box::new(StartSelected) as Box<dyn Action>),
            ("square", tr("Ausgewählte stoppen"), Box::new(StopSelected)),
            ("rotate-cw", tr("Ausgewählte neu starten"), Box::new(RestartSelected)),
            ("refresh-cw", tr("Ausgewählte neu verbinden"), Box::new(ReconnectSelected)),
            ("message-square", tr("Nachricht an Ausgewählte…"), Box::new(MessageSelected)),
            ("x", tr("Auswahl aufheben"), Box::new(ClearSelection)),
        ] {
            out.push(entry(Group::Selection, icon, label, detail.clone(), action, cx));
        }
    }
    for (icon, label, action) in [
        ("play", tr("Alle starten"), Box::new(StartAll) as Box<dyn Action>),
        ("square", tr("Alle stoppen"), Box::new(StopAll)),
        ("user-plus", tr("Account hinzufügen"), Box::new(AddAccount)),
        ("user-plus", tr("Microsoft-Account hinzufügen"), Box::new(AddMicrosoft)),
        ("plus", tr("Neuer Batch"), Box::new(NewBatch)),
        ("search", tr("Suchen"), Box::new(FocusSearch)),
        ("square-check", tr("Alles auswählen"), Box::new(SelectAll)),
        ("gamepad-2", tr("Nächste Instanz spielen"), Box::new(NextInstance)),
        ("gamepad-2", tr("Vorherige Instanz spielen"), Box::new(PreviousInstance)),
        ("triangle-alert", tr("Nächste, die dich braucht"), Box::new(NextAttention)),
        ("panel-left", tr("Seitenleiste ein- oder ausblenden"), Box::new(ToggleSidebar)),
        ("zoom-in", tr("Kacheln größer"), Box::new(TileLarger)),
        ("zoom-out", tr("Kacheln kleiner"), Box::new(TileSmaller)),
        ("layout-grid", tr("Kachelgröße: Klein"), Box::new(TileSmall)),
        ("layout-grid", tr("Kachelgröße: Mittel"), Box::new(TileMedium)),
        ("layout-grid", tr("Kachelgröße: Groß"), Box::new(TileLarge)),
        ("layout-grid", tr("Kachelgröße: Einpassen"), Box::new(TileFit)),
        ("settings", tr("Einstellungen"), Box::new(OpenSettings)),
    ] {
        out.push(entry(Group::Commands, icon, label, None, action, cx));
    }
    for (icon, label, action) in [("layout-grid", tr("Alle"), Box::new(ShowAll) as Box<dyn Action>), ("users", tr("Accounts"), Box::new(ShowAccounts)), ("messages-square", tr("Chat"), Box::new(OpenChat))] {
        out.push(entry(Group::Pages, icon, label, None, action, cx));
    }
    for b in &m.batches {
        let server = b.server.clone().filter(|s| !s.is_empty());
        out.push(entry(Group::Batches, "boxes", b.name.clone(), server.clone(), Box::new(ShowBatch(b.id.clone())), cx));
        out.push(entry(Group::Batches, "play", trf("%@ starten", &[&b.name]), server.clone(), Box::new(StartBatch(b.id.clone())), cx));
        out.push(entry(Group::Batches, "square", trf("%@ stoppen", &[&b.name]), server.clone(), Box::new(StopBatch(b.id.clone())), cx));
        out.push(entry(Group::Batches, "pencil", trf("%@ bearbeiten", &[&b.name]), server, Box::new(EditBatch(b.id.clone())), cx));
    }
    let mut instances: Vec<_> = m.instances.values().collect();
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
        out.push(Entry { group: Group::Accounts, icon, label: format!("{} — {verb}", i.name), detail, keys: None, action });
    }
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
                            div()
                                .id(("palette-row", ix))
                                .mark("row", e.label.clone())
                                .h(px(palette::ROW))
                                .mx(px(space::XS))
                                .px(px(space::S))
                                .rounded(px(radius::INNER))
                                .flex()
                                .items_center()
                                .gap(px(space::S))
                                .cursor_pointer()
                                .when(on, |d| d.bg(t.accent.opacity(tint::ATTENTION)))
                                .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                                .child(icon(e.icon, iconsize::M, if on { t.accent_text } else { t.secondary }))
                                .child(div().flex_none().max_w(relative(0.6)).truncate().text_size(px(font::BODY)).text_color(if on { t.accent_text } else { t.primary }).child(e.label.clone()))
                                .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).children(e.detail.clone()))
                                .children(e.keys.clone().map(|k| div().flex_none().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(k)))
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
                                }))
                                .into_any_element()
                        }
                    })
                })
                .collect()
        }),
    )
    .track_scroll(&p.scroll)
    .size_full();
    let body = if count == 0 {
        div().h(px(palette::ROW * 1.5)).flex().items_center().justify_center().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Keine Treffer")).into_any_element()
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
                                return close(this, window, cx);
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
                        if let Some(p) = this.palette.as_mut() {
                            p.step(by);
                            cx.notify();
                        }
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
    use super::{label, score};
    use gpui_kit::Keystroke;

    #[test]
    fn words_match_in_any_order_and_word_starts_win() {
        assert!(score("start", "Alle starten", "").is_some());
        assert!(score("alt sta", "AltOne — Starten", "Farm · Gestoppt").is_some());
        assert!(score("xyz", "Alle starten", "").is_none());
        assert!(score("farm", "AltOne — Starten", "Farm · Gestoppt").is_some());
        assert!(score("ast", "Alle starten", "").is_some(), "letters in order match");
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
