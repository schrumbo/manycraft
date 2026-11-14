//! The instance grid: tiles with previews, sorted attention first, grouped by batch when it shows several, only the rows on screen drawn.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::Sizable;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{ClearSelection, EditBatch, MessageSelected, Play, ReconnectSelected, RestartSelected, SendChatMany, StartAll, StartBatch, StartSelected, StopBatch, StopSelected};
use super::drive::Mark;
use super::main_window::{Filter, MainWindow, TileSize};
use super::model::{self, Instance, Model, Phase};
use super::motion::{SLOW, Soft};
use super::theme::{Theme, control, font, iconsize, radius, selection, shell, space, tile, tint, with_alpha};
use super::tile::{Geometry, TileData};
use super::widgets::{KeyNav, icon, primary_button, tip};
use crate::core::i18n::{tr, trf};

/// One line of the grid: a batch heading, or a row of the shown tiles `from..to`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Item {
    Heading(usize),
    Row { from: usize, to: usize },
}

/// How the keyboard moves the selection.
#[derive(Clone, Copy)]
enum Move {
    Back,
    Forward,
    Up(usize),
    Down(usize),
    First,
    Last,
}

/// The message field of the selection bar.
pub struct Message {
    pub input: Entity<InputState>,
    _typed: Subscription,
}

/// The tiles selected together, never just one, and the tile a range starts from.
#[derive(Default)]
pub struct Picks {
    pub set: HashSet<String>,
    anchor: Option<String>,
}

impl Picks {
    /// Applies a click on `key` among `shown` with the cursor on `cursor`: alone, toggled with `toggle`, or with `range` from the anchor; returns where the cursor goes.
    fn click(&mut self, shown: &[String], cursor: Option<&String>, key: &str, range: bool, toggle: bool) -> String {
        if range {
            let anchor = self.anchor.clone().filter(|a| shown.contains(a)).or_else(|| cursor.filter(|c| shown.contains(c)).cloned()).unwrap_or_else(|| key.to_owned());
            let at = |k: &str| shown.iter().position(|s| s == k);
            self.set = match (at(&anchor), at(key)) {
                (Some(a), Some(b)) => shown[a.min(b)..=a.max(b)].iter().cloned().collect(),
                _ => HashSet::new(),
            };
            self.anchor = Some(anchor);
        } else if toggle {
            if self.set.is_empty() {
                self.set = cursor.filter(|k| shown.contains(k)).cloned().into_iter().collect();
            }
            if !self.set.remove(key) {
                self.set.insert(key.to_owned());
            }
            self.anchor = Some(key.to_owned());
        } else {
            self.set.clear();
            self.anchor = Some(key.to_owned());
        }
        self.settle().unwrap_or_else(|| key.to_owned())
    }

    /// Turns a selection of one into just the cursor; returns that tile.
    fn settle(&mut self) -> Option<String> {
        if self.set.len() == 1 { self.set.drain().next() } else { None }
    }
}

/// The tiles the selection actions act on: the selected ones in grid order, or the cursor's tile when none are.
fn chosen(shown: &[String], set: &HashSet<String>, cursor: Option<&String>) -> Vec<String> {
    if set.is_empty() { cursor.filter(|k| shown.contains(k)).cloned().into_iter().collect() } else { shown.iter().filter(|k| set.contains(*k)).cloned().collect() }
}

/// Keyboard cursor, selection, sort order and scroll position of the grid.
pub struct Grid {
    pub focus: FocusHandle,
    /// The tile under the keyboard cursor.
    pub selected: Option<String>,
    pub picks: Picks,
    pub message: Option<Message>,
    order: Vec<String>,
    seq: u64,
    hovered: bool,
    list: ListState,
    items: Vec<Item>,
    heights: (f32, f32),
    to_top: bool,
    reveal: bool,
    selected_in_view: bool,
    area: Rc<Cell<Size<Pixels>>>,
    shown: Vec<String>,
    rows: Vec<(usize, usize, usize)>,
    tops: Vec<f32>,
    row_h: f32,
    padding: f32,
    headed: bool,
    places: HashMap<String, (f32, f32)>,
    moves: HashMap<String, (f32, f32)>,
    moved_at: Option<Instant>,
    scope: Option<(Option<String>, Filter, String)>,
}

impl Grid {
    pub fn new(cx: &mut App) -> Self {
        Self {
            scope: None,
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            selected: None,
            picks: Picks::default(),
            message: None,
            order: Vec::new(),
            seq: u64::MAX,
            hovered: false,
            list: ListState::new(0, ListAlignment::Top, px(tile::OVERDRAW)),
            items: Vec::new(),
            heights: (0., 0.),
            to_top: false,
            reveal: false,
            selected_in_view: false,
            area: Rc::default(),
            shown: Vec::new(),
            rows: Vec::new(),
            tops: Vec::new(),
            row_h: 1.,
            padding: 0.,
            headed: false,
            places: HashMap::new(),
            moves: HashMap::new(),
            moved_at: None,
        }
    }

    /// Scrolls back to the first row, for a new filter, search or page.
    pub fn reset_scroll(&mut self) {
        self.to_top = true;
        self.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) });
    }

    /// Selects `key` and scrolls it into view, as when its game hands the screen back.
    pub fn reveal(&mut self, key: String) {
        self.selected = Some(key);
        self.reveal = true;
    }

    /// The tiles the selection actions act on: the selected ones in grid order, or the cursor's tile when none are.
    pub fn chosen(&self) -> Vec<String> {
        chosen(&self.shown, &self.picks.set, self.selected.as_ref())
    }

    /// Selects `key` as a click does: alone, toggled in or out with `toggle`, or with `range` from the anchor to it.
    pub fn click(&mut self, key: &str, range: bool, toggle: bool) {
        self.selected = Some(self.picks.click(&self.shown, self.selected.as_ref(), key, range, toggle));
        self.settle();
    }

    /// Selects every tile shown.
    pub fn select_all(&mut self) {
        self.picks.set = self.shown.iter().cloned().collect();
        self.settle();
    }

    /// Drops the selection and the cursor; false when there was neither.
    pub fn clear_selection(&mut self) -> bool {
        self.message = None;
        let picked = !std::mem::take(&mut self.picks.set).is_empty();
        self.picks.anchor = None;
        self.selected.take().is_some() || picked
    }

    /// Forgets cursor and selection, for another page.
    pub fn forget(&mut self) {
        self.selected = None;
        self.picks = Picks::default();
        self.message = None;
    }

    fn settle(&mut self) {
        if let Some(one) = self.picks.settle() {
            self.selected = Some(one);
        }
        if self.picks.set.len() < 2 {
            self.message = None;
        }
    }

    fn sort(&mut self, m: &Model) -> bool {
        if self.seq == m.order_seq || self.hovered {
            if self.order.len() != m.instances.len() || self.order.iter().any(|k| !m.instances.contains_key(k)) {
                let known: HashSet<&str> = self.order.iter().map(String::as_str).collect();
                let mut fresh: Vec<&Instance> = m.instances.values().filter(|i| !known.contains(i.key.as_str())).collect();
                fresh.sort_by_cached_key(|i| i.name.to_lowercase());
                let fresh: Vec<String> = fresh.into_iter().map(|i| i.key.clone()).collect();
                self.order.retain(|k| m.instances.contains_key(k));
                self.order.extend(fresh);
            }
            return false;
        }
        self.seq = m.order_seq;
        let sorted: Vec<String> = m.sorted(None).into_iter().map(|i| i.key.clone()).collect();
        let changed = sorted != self.order;
        self.order = sorted;
        changed
    }

    fn row_at(&self, at: usize) -> Option<usize> {
        self.rows.iter().position(|(_, from, to)| (*from..*to).contains(&at))
    }

    /// Scrolls just far enough to show the tiles of `row` whole, clear of a pinned heading.
    fn reveal_row(&mut self, row: usize) {
        let Some(&(ix, _, _)) = self.rows.get(row) else { return };
        let Some(&y) = self.tops.get(ix) else { return };
        let first = ix > 0 && matches!(self.items.get(ix - 1), Some(Item::Heading(_)));
        let top = if first { self.tops[ix - 1] } else if self.headed { y - tile::HEADING } else { y };
        let bottom = y + self.row_h;
        let view = (f32::from(self.area.get().height) - self.padding).max(self.row_h);
        let now = self.list.logical_scroll_top();
        let scroll = self.tops.get(now.item_ix).copied().unwrap_or(0.) + f32::from(now.offset_in_item);
        let goal = if top < scroll {
            top.max(0.)
        } else if bottom > scroll + view {
            (bottom - view).min(top)
        } else {
            return;
        };
        let at = self.tops.iter().rposition(|t| *t <= goal).unwrap_or(0);
        self.list.scroll_to(ListOffset { item_ix: at, offset_in_item: px(goal - self.tops[at]) });
    }

    /// Hands the list its new lines, keeping the first tile on screen where it was.
    fn sync(&mut self, items: Vec<Item>, heights: (f32, f32), keys: &[String]) {
        if self.items == items && self.heights == heights {
            return;
        }
        let top = self.list.logical_scroll_top();
        let anchor = (!self.to_top && (top.item_ix > 0 || top.offset_in_item > px(0.)))
            .then(|| {
                self.items.iter().enumerate().skip(top.item_ix).find_map(|(ix, item)| match item {
                    Item::Row { from, .. } => self.shown.get(*from).map(|k| (k.clone(), if ix == top.item_ix && self.heights == heights { top.offset_in_item } else { px(0.) })),
                    Item::Heading(_) => None,
                })
            })
            .flatten();
        self.list.reset(items.len());
        let target = anchor.and_then(|(key, offset)| {
            let at = keys.iter().position(|k| *k == key)?;
            let ix = items.iter().position(|i| matches!(i, Item::Row { from, to } if (*from..*to).contains(&at)))?;
            Some(if ix > 0 && offset == px(0.) && matches!(items[ix - 1], Item::Heading(_)) { ListOffset { item_ix: ix - 1, offset_in_item: px(0.) } } else { ListOffset { item_ix: ix, offset_in_item: offset } })
        });
        self.list.scroll_to(target.unwrap_or(ListOffset { item_ix: 0, offset_in_item: px(0.) }));
        self.items = items;
        self.heights = heights;
    }
}

/// Whether `i` belongs to `batch`, any when `None`, and shows under `filter`.
pub fn matches(i: &Instance, batch: Option<&str>, filter: Filter) -> bool {
    batch.is_none_or(|b| i.batch_id == b)
        && match filter {
            Filter::All => true,
            Filter::Attention => i.phase.needs_attention(),
            Filter::Running => i.phase.is_active() && !i.phase.needs_attention(),
            Filter::Stopped => !i.phase.is_active() && !i.phase.needs_attention(),
        }
}

/// The search words of `query`, lowercase.
pub fn words(query: &str) -> Vec<String> {
    query.to_lowercase().split_whitespace().map(str::to_owned).collect()
}

/// Whether each of the search `words` occurs in the account name, batch, server or status of `i`.
pub fn found(i: &Instance, m: &Model, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let batch = m.batch(&i.batch_id);
    let server = i.phase.server().or_else(|| batch.and_then(|b| b.server.as_deref())).unwrap_or_default();
    let mut text = [i.name.as_str(), batch.map(|b| b.name.as_str()).unwrap_or_default(), server, &super::state::status(i, None)].join(" ");
    if let Some(ban) = super::state::stopped_ban(i, m) {
        text.push(' ');
        text.push_str(&ban);
    }
    let state = if i.phase.needs_attention() {
        "braucht dich"
    } else if i.phase.is_active() {
        "läuft"
    } else {
        "gestoppt"
    };
    text.push(' ');
    text.push_str(&tr(state));
    if matches!(i.phase, Phase::Playing { .. }) {
        text.push(' ');
        text.push_str(&tr("spielt"));
    }
    let text = text.to_lowercase();
    words.iter().all(|w| text.contains(w.as_str()))
}

/// Columns, tile width and the space between tiles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub columns: usize,
    pub width: f32,
    pub gap: f32,
}

/// The layout for sections of `sections` tiles each in a `width` × `height` area; `headed` sections carry a heading.
pub fn layout(size: TileSize, sections: &[usize], width: f32, height: f32, headed: bool) -> Layout {
    let fit = |min: f32| {
        let gap = Geometry::gap_for(min);
        let columns = (((width + gap) / (min + gap)).floor() as usize).max(1);
        Layout { columns, width: ((width - gap * (columns - 1) as f32) / columns as f32).max(1.), gap }
    };
    match size {
        TileSize::Small => fit(tile::SMALL),
        TileSize::Medium => fit(tile::MEDIUM),
        TileSize::Large => fit(tile::LARGE),
        TileSize::Fit => {
            let count: usize = sections.iter().sum::<usize>().max(1);
            let headings = if headed { sections.len() as f32 * tile::HEADING } else { 0. };
            let best = (1..=count)
                .map(|columns| {
                    let rows = sections.iter().map(|n| n.div_ceil(columns)).sum::<usize>().max(1) as f32;
                    let size = |gap: f32, strip: f32| {
                        let by_width = (width - gap * (columns - 1) as f32) / columns as f32;
                        let row_h = (height - headings - gap * (rows - 1.)) / rows;
                        by_width.min((row_h - strip) * 16. / 9.)
                    };
                    let mut layout = Layout { columns, width: size(tile::GAP, tile::STRIP), gap: tile::GAP };
                    if layout.width < tile::MEDIUM {
                        layout = Layout { columns, width: size(tile::GAP_SMALL, tile::STRIP_SMALL).min(tile::MEDIUM - 1.), gap: tile::GAP_SMALL };
                    }
                    if layout.width < tile::TINY {
                        layout = Layout { columns, width: size(tile::GAP_TINY, tile::STRIP_TINY).min(tile::TINY - 1.), gap: tile::GAP_TINY };
                    }
                    Layout { width: layout.width.floor(), ..layout }
                })
                .max_by(|a, b| a.width.total_cmp(&b.width))
                .unwrap_or(Layout { columns: 1, width, gap: tile::GAP });
            if best.width >= tile::FIT_MIN { best } else { fit(tile::FIT_MIN) }
        }
    }
}

/// The status line of `i` as it fits a tile; `terse` keeps only its first words.
fn line(i: &Instance, preparing: Option<u8>, terse: bool) -> String {
    let full = || super::state::status(i, preparing);
    let tries = crate::core::settings::active().reconnect.max_attempts;
    if i.stopping {
        return if terse { tr("Stoppt…") } else { full() };
    }
    match &i.phase {
        Phase::Starting { .. } if terse && preparing.is_some() => trf("Lädt %lld %%", &[&preparing.unwrap_or(0)]),
        Phase::Starting { done, total } if terse && *total > 1 => trf("Wartet %lld/%lld", &[done, total]),
        Phase::Menu if terse => tr("Hauptmenü"),
        Phase::Connecting { .. } if i.reconnect_attempts > 0 && terse => trf("Versuch %lld/%lld", &[&i.reconnect_attempts, &tries]),
        Phase::Connecting { .. } if i.reconnect_attempts > 0 => trf("Verbindet neu · %lld/%lld", &[&i.reconnect_attempts, &tries]),
        Phase::Connecting { .. } => tr("Verbindet…"),
        Phase::Queue { .. } if terse => tr("Warteschlange"),
        Phase::Playing { .. } => tr("Spielt"),
        _ => full(),
    }
}

fn data(i: &Instance, m: &Model, selected: &Option<String>, picked: &HashSet<String>, terse: bool) -> TileData {
    let preparing = m.preparing.get(&i.batch_id).copied();
    let ban = super::state::stopped_ban(i, m);
    TileData {
        key: i.key.clone(),
        name: i.name.clone(),
        account_id: i.account_id,
        batch_id: i.batch_id.clone(),
        phase: i.phase.clone(),
        status: ban.clone().unwrap_or_else(|| line(i, preparing, terse)),
        detail: ban.unwrap_or_else(|| super::state::status(i, preparing)),
        stopping: i.stopping,
        whispers: i.unread_whispers,
        preview: i.preview.clone(),
        live: i.live,
        face: m.faces.get(&i.account_id).cloned(),
        selected: selected.as_deref() == Some(i.key.as_str()),
        picked: picked.contains(&i.key),
        playing: m.playing.as_deref() == Some(i.key.as_str()),
    }
}

fn move_selection(this: &mut MainWindow, step: Move, extend: bool, cx: &mut Context<MainWindow>) {
    let g = &mut this.grid;
    let last = match g.shown.len() {
        0 => return,
        n => n - 1,
    };
    let at = g.selected.as_ref().and_then(|s| g.shown.iter().position(|k| k == s));
    let next = match (at, step) {
        (None, Move::Last) => last,
        (None, _) | (Some(_), Move::First) => 0,
        (Some(_), Move::Last) => last,
        (Some(a), Move::Back) => a.saturating_sub(1),
        (Some(a), Move::Forward) => (a + 1).min(last),
        (Some(a), Move::Up(n) | Move::Down(n)) => {
            let Some(row) = g.row_at(a) else { return };
            let column = a - g.rows[row].1;
            let to = if matches!(step, Move::Up(_)) { row.saturating_sub(n) } else { (row + n).min(g.rows.len() - 1) };
            let (_, from, end) = g.rows[to];
            from + column.min(end - from - 1)
        }
    };
    let key = g.shown[next].clone();
    g.click(&key, extend, false);
    if let Some(row) = g.row_at(next) {
        g.reveal_row(row);
    }
    cx.notify();
}

fn action(id: &'static str, label: String, icon_name: &str, action: Box<dyn Action>, t: &Theme) -> AnyElement {
    primary_button(id, label, icon_name, false, t).on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)).into_any_element()
}

fn empty(id: &'static str, symbol: &str, title: String, text: String, button: Option<AnyElement>, t: &Theme) -> AnyElement {
    div()
        .id(id)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(space::S))
        .px(px(space::XL))
        .child(icon(symbol, iconsize::HERO, t.tertiary))
        .child(div().id(SharedString::from(format!("{id}-title"))).mark("heading", title.clone()).mt(px(space::S)).text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
        .child(div().max_w(px(control::READING)).text_size(px(font::CALLOUT)).text_color(t.secondary).text_center().child(text))
        .when_some(button, |d, button| d.child(div().mt(px(space::S)).child(button)))
        .into_any_element()
}

/// The heading over a batch's tiles in the grid of all batches: name, server, how many run, and what needs attention.
fn heading(batch: &str, pinned: bool, m: &Model, t: &Theme) -> AnyElement {
    let name = m.batch(batch).map(|b| b.name.clone()).unwrap_or_default();
    let server = m.batch(batch).and_then(|b| b.server.clone()).filter(|s| !s.is_empty());
    let mine = || m.instances.values().filter(|i| i.batch_id == batch);
    let (total, running) = (mine().count(), mine().filter(|i| i.phase.is_active()).count());
    let attention = m.attention_count(Some(batch));
    let fine = mine().filter(|i| i.phase.is_active() && !i.phase.needs_attention()).count();
    let state = if running == 0 { tr("gestoppt") } else { trf("%lld von %lld laufen", &[&fine, &total]) };
    let summary = [server, Some(state)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let (start, stop, edit) = (batch.to_owned(), batch.to_owned(), batch.to_owned());
    let stopped = total - running;
    div()
        .id(SharedString::from(format!("heading-{batch}")))
        .mark("heading", name.clone())
        .h(px(tile::HEADING))
        .flex()
        .when(pinned, |d| d.items_center())
        .when(!pinned, |d| d.items_end().pb(px(space::S)))
        .gap(px(space::S))
        .child(div().min_w_0().truncate().text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(name))
        .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(summary))
        .when(attention > 0, |d| {
            d.child(
                div()
                    .h(px(tile::BADGE))
                    .min_w(px(tile::BADGE))
                    .px(px(space::XS))
                    .flex_none()
                    .rounded(px(radius::FULL))
                    .bg(t.attention)
                    .text_color(t.on_attention)
                    .text_size(px(font::CAPTION2))
                    .font_weight(FontWeight::BOLD)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(attention.to_string()),
            )
        })
        .context_menu(move |menu, _, _| {
            menu.menu_with_disabled(tr("Starten"), Box::new(StartBatch(start.clone())), stopped == 0)
                .menu_with_disabled(tr("Stoppen"), Box::new(StopBatch(stop.clone())), running == 0)
                .separator()
                .menu(tr("Batch bearbeiten"), Box::new(EditBatch(edit.clone())))
        })
        .into_any_element()
}

/// Draws the grid of `batch` (all batches when `None`) with the window's filter, size and search.
pub fn render(this: &mut MainWindow, batch: Option<&str>, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = Theme::get(cx);
    let model = model::get(cx);
    let m = model.read(cx);
    let resorted = this.grid.sort(m);
    if resorted && this.grid.selected_in_view && this.grid.focus.is_focused(window) && window.last_input_was_keyboard() {
        this.grid.reveal = true;
    }
    let scope = (batch.map(str::to_owned), this.filter, this.query.clone());
    let same_scope = this.grid.scope.as_ref() == Some(&scope);
    this.grid.scope = Some(scope);
    let held: HashSet<&str> = if this.grid.hovered && same_scope { this.grid.shown.iter().map(String::as_str).collect() } else { HashSet::new() };
    let words = words(&this.query);
    let mut keys: Vec<String> = this.grid.order.iter().filter(|k| held.contains(k.as_str()) || m.instances.get(*k).is_some_and(|i| matches(i, batch, this.filter) && found(i, m, &words))).cloned().collect();
    drop(held);
    let batch_of = |k: &String| m.instances.get(k).map(|i| i.batch_id.clone()).unwrap_or_default();
    let headed = batch.is_none() && keys.iter().map(&batch_of).collect::<HashSet<_>>().len() > 1;
    if headed {
        let rank: HashMap<&str, usize> = m.batches.iter().enumerate().map(|(n, b)| (b.id.as_str(), n)).collect();
        keys.sort_by_key(|k| m.instances.get(k).and_then(|i| rank.get(i.batch_id.as_str()).copied()).unwrap_or(usize::MAX));
    }
    if !this.grid.picks.set.is_empty() {
        let shown: HashSet<&str> = keys.iter().map(String::as_str).collect();
        if this.grid.picks.set.iter().any(|k| !shown.contains(k.as_str())) {
            this.grid.picks.set.retain(|k| shown.contains(k.as_str()));
            this.grid.settle();
        }
    }
    if keys.is_empty() {
        this.grid.shown.clear();
        this.grid.rows.clear();
        let in_scope = m.instances.values().any(|i| batch.is_none_or(|b| i.batch_id == b));
        super::tile::unhover_except(None, cx);
        super::state::set_visible(HashSet::new(), cx);
        if !in_scope {
            let edit = batch.map(|b| action("edit-batch", tr("Batch bearbeiten"), "pencil", Box::new(EditBatch(b.to_owned())), &t));
            return empty("no-accounts", "users", tr("Keine Accounts in diesem Batch"), tr("Füge dem Batch Accounts hinzu, um sie hier zu starten."), edit, &t);
        }
        if !this.query.is_empty() {
            let clear = primary_button("clear-search", tr("Suche löschen"), "x", false, &t)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.search.update(cx, |s, cx| s.set_value("", window, cx));
                    this.query.clear();
                    this.grid.reset_scroll();
                    cx.notify();
                }))
                .into_any_element();
            return empty("no-match", "search", tr("Keine Treffer"), trf("Kein Account passt zu „%@“.", &[&this.query]), Some(clear), &t);
        }
        return match this.filter {
            Filter::Attention => empty("all-good", "circle-check", tr("Alles in Ordnung"), tr("Kein Account braucht dich gerade."), None, &t),
            Filter::Stopped => empty("none-stopped", "circle-check", tr("Alles läuft"), tr("Kein Account ist gestoppt."), None, &t),
            Filter::Running | Filter::All => {
                let start: Box<dyn Action> = match batch {
                    Some(b) => Box::new(StartBatch(b.to_owned())),
                    None => Box::new(StartAll),
                };
                let label = if batch.is_some() { tr("Starten") } else { tr("Alle starten") };
                empty("none-running", "play", tr("Nichts läuft"), tr("Starte die Accounts, um sie hier zu sehen."), Some(action("start-empty", label, "play", start, &t)), &t)
            }
        };
    }
    let mut sections: Vec<(String, usize, usize)> = Vec::new();
    for (n, k) in keys.iter().enumerate() {
        let b = if headed { batch_of(k) } else { String::new() };
        match sections.last_mut() {
            Some((last, _, to)) if *last == b => *to = n + 1,
            _ => sections.push((b, n, n + 1)),
        }
    }
    let area = this.grid.area.get();
    let inset = tile::INSET;
    let (width, height) = if area.width > px(0.) { (f32::from(area.width) - inset * 2., f32::from(area.height) - inset * 2.) } else { (f32::from(window.viewport_size().width) - inset * 2., f32::from(window.viewport_size().height) - inset * 2. - super::theme::shell::TOOLBAR) };
    let counts: Vec<usize> = sections.iter().map(|(_, from, to)| to - from).collect();
    let Layout { columns, width: tile_w, gap } = layout(this.size, &counts, width.max(tile::FIT_MIN), height.max(1.), headed);
    let geometry = Geometry::new(tile_w);
    let row_h = geometry.height() + gap;
    let lead = if this.size == TileSize::Fit { ((width - columns as f32 * tile_w - gap * (columns - 1) as f32) / 2.).max(0.).floor() } else { 0. };
    let mut items = Vec::new();
    for (n, (_, from, to)) in sections.iter().enumerate() {
        if headed {
            items.push(Item::Heading(n));
        }
        let mut at = *from;
        while at < *to {
            items.push(Item::Row { from: at, to: (at + columns).min(*to) });
            at += columns;
        }
    }
    let mut tops = Vec::with_capacity(items.len());
    let mut y = 0.;
    for item in &items {
        tops.push(y);
        y += if matches!(item, Item::Heading(_)) { tile::HEADING } else { row_h };
    }
    let mut places = HashMap::with_capacity(keys.len());
    let mut rows = Vec::new();
    for (ix, item) in items.iter().enumerate() {
        if let Item::Row { from, to } = *item {
            rows.push((ix, from, to));
            for (c, k) in keys[from..to].iter().enumerate() {
                places.insert(k.clone(), (c as f32 * (tile_w + gap), tops[ix]));
            }
        }
    }
    if resorted && !cx.reduce_motion() {
        this.grid.moves = places.iter().filter_map(|(k, new)| this.grid.places.get(k).filter(|old| *old != new).map(|old| (k.clone(), *old))).collect();
        this.grid.moved_at = (!this.grid.moves.is_empty()).then(Instant::now);
    }
    this.grid.sync(items.clone(), (row_h, tile_w), &keys);
    this.grid.to_top = false;
    this.grid.places = places.clone();
    this.grid.shown = keys.clone();
    this.grid.rows = rows;
    this.grid.row_h = row_h;
    this.grid.tops = tops.clone();
    this.grid.headed = headed;
    this.grid.padding = inset + (inset - gap).max(0.);
    if std::mem::take(&mut this.grid.reveal)
        && let Some(row) = this.grid.selected.as_ref().and_then(|s| keys.iter().position(|k| k == s)).and_then(|at| this.grid.row_at(at))
    {
        this.grid.reveal_row(row);
    }
    let selected = if this.grid.focus.is_focused(window) { this.grid.selected.clone() } else { None };
    let picked = Rc::new(this.grid.picks.set.clone());
    let bar = if this.grid.picks.set.len() >= 2 { selection::BAR + selection::BOTTOM } else { 0. };
    let progress = this.grid.moved_at.map(|at| (at.elapsed().as_secs_f32() / SLOW.as_secs_f32()).min(1.));
    if progress.is_some_and(|p| p >= 1.) {
        this.grid.moved_at = None;
        this.grid.moves.clear();
    } else if progress.is_some() {
        window.request_animation_frame();
    }
    let ease = progress.filter(|p| *p < 1.).map(|p| 1. - (1. - p).powi(3));
    let moves = Rc::new(this.grid.moves.clone());
    let focused = this.grid.focus.is_focused(window) && window.last_input_was_keyboard();
    let view = cx.entity().downgrade();
    let (shown, items, tops, places) = (Rc::new(keys), Rc::new(items), Rc::new(tops), Rc::new(places));
    let section_batches: Rc<Vec<String>> = Rc::new(sections.iter().map(|(b, _, _)| b.clone()).collect());
    let scrolled = this.grid.list.logical_scroll_top();
    let from = tops.get(scrolled.item_ix).copied().unwrap_or(0.) + f32::from(scrolled.offset_in_item) - inset;
    let to = from + f32::from(this.grid.area.get().height).max(row_h);
    let visible: HashSet<String> = items
        .iter()
        .zip(tops.iter())
        .filter(|(_, y)| **y < to && **y + row_h > from)
        .flat_map(|(i, _)| match *i {
            Item::Row { from, to } => shown[from..to].to_vec(),
            Item::Heading(_) => Vec::new(),
        })
        .collect();
    this.grid.selected_in_view = this.grid.selected.as_ref().is_some_and(|s| visible.contains(s));
    let pinned = headed.then(|| {
        let top = scrolled;
        let at_heading = matches!(items.get(top.item_ix), Some(Item::Heading(_))) && top.offset_in_item <= px(0.);
        let section = items.iter().take(top.item_ix + 1).filter_map(|i| if let Item::Heading(n) = i { Some(*n) } else { None }).last();
        section.filter(|_| !at_heading && (top.item_ix > 0 || top.offset_in_item > px(0.)))
    });
    let pinned = pinned.flatten().map(|n| {
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .pl(px(inset + lead))
            .pr(px(inset))
            .bg(t.window)
            .border_b_1()
            .border_color(t.separator)
            .child(heading(&section_batches[n], true, m, &t))
    });
    let can_play: HashMap<String, bool> = shown.iter().filter_map(|k| m.instances.get(k)).map(|i| (i.key.clone(), super::tile::playable(&i.phase))).collect();
    if super::tile::hovered(cx).is_some_and(|k| !visible.contains(&k)) {
        super::tile::unhover_except(None, cx);
    }
    super::state::set_visible(visible, cx);
    let (list_items, list_shown, list_batches) = (Rc::clone(&items), Rc::clone(&shown), Rc::clone(&section_batches));
    let list = list(this.grid.list.clone(), move |ix, _, cx| {
        let t = Theme::get(cx);
        let model = model::get(cx);
        match list_items.get(ix).copied() {
            Some(Item::Heading(n)) => {
                let m = model.read(cx);
                div().pl(px(inset + lead)).pr(px(inset)).child(heading(&list_batches[n], false, m, &t)).into_any_element()
            }
            Some(Item::Row { from, to }) => {
                let cells: Vec<TileData> = {
                    let m = model.read(cx);
                    list_shown[from..to].iter().filter_map(|k| m.instances.get(k).map(|i| data(i, m, &selected, &picked, geometry.dense))).collect()
                };
                let cells: Vec<AnyElement> = cells
                    .into_iter()
                    .map(|d| {
                        let offset = match (moves.get(&d.key), ease, places.get(&d.key)) {
                            (Some(old), Some(e), Some(new)) => ((old.0 - new.0) * (1. - e), (old.1 - new.1) * (1. - e)),
                            _ => (0., 0.),
                        };
                        super::tile::view(d, geometry, offset, focused, view.clone(), cx)
                    })
                    .collect();
                div().h(px(row_h)).pl(px(inset + lead)).flex().gap(px(gap)).children(cells).into_any_element()
            }
            None => div().into_any_element(),
        }
    })
    .pt(px(inset))
    .pb(px((inset - gap).max(0.) + bar))
    .size_full();
    let measure = {
        let area = Rc::clone(&this.grid.area);
        let view = cx.entity().downgrade();
        canvas(
            move |bounds, window, _| {
                if area.get() != bounds.size {
                    area.set(bounds.size);
                    let view = view.clone();
                    window.on_next_frame(move |_, cx| {
                        view.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    };
    div()
        .id("grid")
        .key_context("InstanceGrid")
        .track_focus(&this.grid.focus)
        .relative()
        .size_full()
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            this.grid.hovered = *hovered;
            if !*hovered {
                cx.notify();
            }
        }))
        .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
            let m = &e.keystroke.modifiers;
            if matches!(e.keystroke.key.as_str(), "enter" | "space" | "delete") && (!this.grid.focus.is_focused(window) || m.control || m.alt || m.platform) {
                return;
            }
            let page = (f32::from(this.grid.area.get().height) / this.grid.row_h).floor().max(1.) as usize;
            let step = match e.keystroke.key.as_str() {
                "left" => Move::Back,
                "right" => Move::Forward,
                "up" => Move::Up(1),
                "down" => Move::Down(1),
                "pageup" => Move::Up(page),
                "pagedown" => Move::Down(page),
                "home" => Move::First,
                "end" => Move::Last,
                "enter" => {
                    if let Some(key) = this.grid.selected.clone().filter(|k| this.grid.shown.contains(k)) {
                        if can_play.get(&key).copied().unwrap_or(false) {
                            window.dispatch_action(Box::new(Play(key)), cx);
                        } else {
                            super::tile::primary_action(&key, window, cx);
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                "space" | "delete" => {
                    if e.keystroke.key == "delete" || e.keystroke.modifiers.shift {
                        window.dispatch_action(Box::new(StopSelected), cx);
                    } else if let Some(key) = this.grid.selected.clone().filter(|k| this.grid.shown.contains(k)) {
                        super::tile::primary_action(&key, window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => return,
            };
            cx.stop_propagation();
            move_selection(this, step, e.keystroke.modifiers.shift, cx);
        }))
        .child(measure)
        .child(list)
        .children(pinned)
        .into_any_element()
}

/// How many of `keys` each selection action would act on: start, stop, restart, reconnect, message.
fn able(keys: &[String], m: &Model) -> [usize; 5] {
    let mut n = [0; 5];
    for i in keys.iter().filter_map(|k| m.instances.get(k)) {
        let running = i.phase.is_running();
        n[0] += usize::from(!running && !i.stopping && i.phase != Phase::LoginNeeded);
        n[1] += usize::from(i.phase.is_active() && !i.stopping);
        n[2] += usize::from(!i.stopping && i.phase != Phase::LoginNeeded);
        n[3] += usize::from(running && !i.stopping && !matches!(i.phase, Phase::Starting { .. }));
        n[4] += usize::from(i.phase.on_server());
    }
    n
}

/// What the selection actions offer for `keys`: label, icon, the action, and how many of them it acts on.
pub fn offers(keys: &[String], m: &Model) -> [(String, &'static str, Box<dyn Action>, usize); 5] {
    let n = able(keys, m);
    [
        (tr("Starten"), "play", Box::new(StartSelected), n[0]),
        (tr("Stoppen"), "square", Box::new(StopSelected), n[1]),
        (tr("Neu starten"), "rotate-cw", Box::new(RestartSelected), n[2]),
        (tr("Neu verbinden"), "refresh-cw", Box::new(ReconnectSelected), n[3]),
        (tr("Nachricht senden…"), "message-square", Box::new(MessageSelected), n[4]),
    ]
}

/// Opens the message field of the selection bar.
pub fn open_message(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if this.grid.picks.set.len() < 2 {
        return;
    }
    if let Some(msg) = &this.grid.message {
        msg.input.update(cx, |s, cx| s.focus(window, cx));
        return;
    }
    let on = on_server(this, cx).len();
    if on == 0 {
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(trf("Nachricht an %lld Accounts…", &[&on])));
    let typed = cx.subscribe_in(&input, window, |this: &mut MainWindow, _: &Entity<InputState>, e: &InputEvent, window, cx| {
        if matches!(e, InputEvent::PressEnter { .. }) {
            send_message(this, window, cx);
        } else if matches!(e, InputEvent::Change) {
            cx.notify();
        }
    });
    input.update(cx, |s, cx| s.focus(window, cx));
    this.grid.message = Some(Message { input, _typed: typed });
    cx.notify();
}

fn on_server(this: &MainWindow, cx: &App) -> Vec<String> {
    let m = model::get(cx).read(cx);
    this.grid.chosen().into_iter().filter(|k| m.instances.get(k).is_some_and(|i| i.phase.on_server())).collect()
}

/// Sends what the user typed in the message field to the selected accounts that are on a server, and keeps the field open for the next one.
fn send_message(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(input) = this.grid.message.as_ref().map(|m| m.input.clone()) else { return };
    let text = input.read(cx).value().trim().to_owned();
    let keys = on_server(this, cx);
    if text.is_empty() || keys.is_empty() {
        return;
    }
    let sent = keys.len();
    window.dispatch_action(Box::new(SendChatMany { keys, text }), cx);
    input.update(cx, |s, cx| {
        s.set_value("", window, cx);
        s.set_placeholder(trf("An %lld Accounts gesendet · noch eine Nachricht…", &[&sent]), window, cx);
        s.focus(window, cx);
    });
    cx.notify();
}

/// Closes the message field and hands the keyboard back to the grid.
pub fn close_message(this: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if this.grid.message.take().is_some() {
        window.focus(&this.grid.focus, cx);
        cx.notify();
    }
}

fn round(id: &'static str, label: String, symbol: &'static str, t: &Theme) -> Stateful<Div> {
    still(id, label, symbol, t).cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::FULL)
}

fn still(id: &'static str, label: String, symbol: &'static str, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .mark("button", label)
        .size(px(control::M))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(radius::FULL))
        .child(icon(symbol, iconsize::S, t.secondary))
}

#[allow(clippy::too_many_arguments)]
fn bar_button(id: &'static str, label: String, icon_name: &'static str, action: Box<dyn Action>, hint: String, enabled: bool, compact: bool, t: &Theme) -> AnyElement {
    let shown = (!compact).then(|| label.clone());
    let button = div()
        .id(id)
        .mark("button", label)
        .h(px(control::M))
        .when(compact, |d| d.w(px(control::M)).justify_center())
        .when(!compact, |d| d.px(px(space::S)))
        .rounded(px(radius::FULL))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::XS))
        .text_size(px(font::CALLOUT))
        .font_weight(FontWeight::MEDIUM)
        .tooltip(tip(hint));
    if !enabled {
        return button.opacity(tint::DISABLED).child(icon(icon_name, iconsize::S, t.primary)).children(shown).into_any_element();
    }
    button
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(tint::HOVER), radius::FULL)
        .child(icon(icon_name, iconsize::S, t.primary))
        .children(shown)
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        .into_any_element()
}

/// The floating bar over a selection of several tiles: how many, what can happen to them, and a field to message them.
pub fn selection_bar(this: &mut MainWindow, window: &Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let count = this.grid.picks.set.len();
    if count < 2 {
        return None;
    }
    let compact = this.content_width(window) < shell::COMPACT;
    let t = Theme::get(cx);
    let keys = this.grid.chosen();
    let offers = offers(&keys, model::get(cx).read(cx));
    let total = keys.len();
    let mut capsule = div()
        .id("selection-bar")
        .mark("selection", trf("%lld ausgewählt", &[&count]))
        .h(px(selection::BAR))
        .max_w_full()
        .pl(px(space::M))
        .pr(px(space::XS))
        .flex()
        .items_center()
        .gap(px(space::XS))
        .rounded(px(radius::FULL))
        .bg(with_alpha(t.material, tint::FLOATING))
        .border_1()
        .border_color(t.separator)
        .shadow_lg()
        .occlude()
        .child(div().flex_none().pr(px(space::S)).text_size(px(font::CALLOUT)).font_weight(FontWeight::SEMIBOLD).child(trf("%lld ausgewählt", &[&count])));
    match &this.grid.message {
        Some(msg) => {
            let close = tr("Nachricht schließen");
            let esc = Keystroke::parse("escape").map(|k| super::palette::label(&k)).unwrap_or_default();
            capsule = capsule
                .child(
                    div()
                        .w(px(selection::FIELD))
                        .min_w_0()
                        .flex_shrink(1.)
                        .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                            if e.keystroke.key == "escape" {
                                cx.stop_propagation();
                                close_message(this, window, cx);
                            }
                        }))
                        .child(Input::new(&msg.input).small()),
                )
                .child({
                    let send = tr("Senden");
                    let enter = Keystroke::parse("enter").map(|k| super::palette::label(&k)).unwrap_or_default();
                    let hint = format!("{send} · {enter}");
                    if msg.input.read(cx).value().trim().is_empty() {
                        still("selection-send", send, "send", &t).opacity(tint::DISABLED).tooltip(tip(hint))
                    } else {
                        round("selection-send", send, "send", &t).tooltip(tip(hint)).on_click(cx.listener(|this, _: &ClickEvent, window, cx| send_message(this, window, cx)))
                    }
                })
                .child(round("selection-message-close", close.clone(), "x", &t).tooltip(tip(format!("{close} · {esc}"))).on_click(cx.listener(|this, _: &ClickEvent, window, cx| close_message(this, window, cx))));
        }
        None => {
            let ids = ["selection-start", "selection-stop", "selection-restart", "selection-reconnect", "selection-message"];
            for ((label, icon_name, action, n), id) in offers.into_iter().zip(ids) {
                let hint = super::palette::hint(trf("%@: %lld von %lld", &[&label, &n, &total]), action.as_ref(), cx);
                capsule = capsule.child(bar_button(id, label, icon_name, action, hint, n > 0, compact, &t));
            }
            let clear = tr("Auswahl aufheben");
            capsule = capsule
                .child(div().w(px(1.)).h(px(control::S)).mx(px(space::XXS)).bg(t.separator))
                .child(round("selection-clear", clear.clone(), "x", &t).tooltip(tip(super::palette::hint(clear, &ClearSelection, cx))).on_click(|_, window, cx| window.dispatch_action(Box::new(ClearSelection), cx)));
        }
    }
    Some(div().absolute().left_0().right_0().bottom(px(selection::BOTTOM)).px(px(space::L)).flex().justify_center().child(capsule).into_any_element())
}

#[cfg(test)]
mod tests {
    use super::{Filter, Instance, Layout, Model, Phase, Picks, TileSize, chosen, found, layout, line, matches, tile, words};
    use crate::ui::tile::Geometry;

    #[test]
    fn sizes_fill_the_width() {
        let Layout { columns, width, gap } = layout(TileSize::Medium, &[50], 1000., 600., false);
        assert_eq!(columns, 3);
        assert!((width * 3. + gap * 2. - 1000.).abs() < 0.01);
        assert_eq!(layout(TileSize::Large, &[50], 200., 600., false).columns, 1);
        assert!(layout(TileSize::Small, &[50], 1000., 600., false).columns > columns);
    }

    #[test]
    fn fit_shows_every_tile_without_scrolling_until_tiles_get_too_small() {
        for (sections, width, height, headed) in [(vec![1], 1000., 600., false), (vec![6], 1000., 600., false), (vec![50], 1400., 800., false), (vec![7], 500., 900., false), (vec![20, 9, 3], 1200., 800., true)] {
            let Layout { columns, width: w, gap } = layout(TileSize::Fit, &sections, width, height, headed);
            let rows = sections.iter().map(|n: &usize| n.div_ceil(columns)).sum::<usize>() as f32;
            let headings = if headed { sections.len() as f32 * tile::HEADING } else { 0. };
            assert_eq!(gap, Geometry::gap_for(w), "{sections:?} uses the gap of its tile size");
            assert!(columns as f32 * w + (columns - 1) as f32 * gap <= width + 0.5, "{sections:?} overflows the width");
            assert!(headings + rows * (w * 9. / 16. + Geometry::strip_for(w)) + (rows - 1.) * gap <= height + 1., "{sections:?} overflows the height");
        }
        assert_eq!(layout(TileSize::Fit, &[1], 1000., 600., false).columns, 1);
        assert!(layout(TileSize::Fit, &[500], 1000., 600., false).width >= tile::FIT_MIN);
    }

    fn mk(phase: Phase) -> Instance {
        Instance { key: "k".into(), batch_id: "b".into(), account_id: uuid::Uuid::nil(), name: "AltOne".into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, live: false, stopping: false }
    }

    #[test]
    fn filters_split_attention_running_and_stopped() {
        let lost = mk(Phase::Disconnected { server: None, reason: String::new() });
        let menu = mk(Phase::Menu);
        let off = mk(Phase::Stopped);
        assert!(matches(&lost, None, Filter::Attention) && !matches(&lost, None, Filter::Running));
        assert!(matches(&menu, None, Filter::Running) && !matches(&menu, None, Filter::Stopped));
        assert!(matches(&off, Some("b"), Filter::Stopped) && !matches(&off, Some("c"), Filter::All));
        let again = mk(Phase::Restarting { attempt: 1, of: 3 });
        assert!(matches(&again, None, Filter::Running) && !matches(&again, None, Filter::Stopped));
    }

    #[test]
    fn search_matches_name_server_and_status_words() {
        crate::core::i18n::init("de");
        let m = Model::default();
        let lost = mk(Phase::Disconnected { server: Some("play.example.net".into()), reason: "Zeitüberschreitung".into() });
        assert!(found(&lost, &m, &words("alt")));
        assert!(found(&lost, &m, &words("GETRENNT example")));
        assert!(found(&lost, &m, &words("braucht")));
        assert!(!found(&lost, &m, &words("alt gesperrt")));
        assert!(!found(&mk(Phase::Stopped), &m, &words("two")));
        assert!(found(&mk(Phase::Banned { server: None, reason: "griefing".into() }), &m, &words("gesperrt")));
        assert!(found(&mk(Phase::Stopped), &m, &words("gestoppt")));
        assert!(found(&mk(Phase::Playing { server: "s".into(), since: std::time::Instant::now() }), &m, &words("spielt läuft")));
    }

    #[test]
    fn clicks_select_one_toggle_or_a_range() {
        let shown: Vec<String> = ["a", "b", "c", "d", "e"].map(str::to_owned).to_vec();
        let pick = |p: &Picks, cursor: &String| chosen(&shown, &p.set, Some(cursor));
        let mut p = Picks::default();
        let mut cursor = p.click(&shown, None, "b", false, false);
        assert!(p.set.is_empty() && pick(&p, &cursor) == ["b"]);
        cursor = p.click(&shown, Some(&cursor), "d", true, false);
        assert_eq!(pick(&p, &cursor), ["b", "c", "d"]);
        cursor = p.click(&shown, Some(&cursor), "a", true, false);
        assert_eq!(pick(&p, &cursor), ["a", "b"], "a range keeps its anchor");
        cursor = p.click(&shown, Some(&cursor), "e", false, true);
        assert_eq!(pick(&p, &cursor), ["a", "b", "e"]);
        cursor = p.click(&shown, Some(&cursor), "a", false, true);
        cursor = p.click(&shown, Some(&cursor), "b", false, true);
        assert!(p.set.is_empty() && pick(&p, &cursor) == ["e"], "one left is just the cursor");
        cursor = p.click(&shown, Some(&cursor), "c", false, true);
        assert_eq!(pick(&p, &cursor), ["c", "e"], "a toggle starts from the cursor's tile");
    }

    #[test]
    fn tile_status_drops_the_server_and_shortens_on_dense_tiles() {
        crate::core::i18n::init("de");
        let playing = mk(Phase::Playing { server: "play.example.net".into(), since: std::time::Instant::now() });
        assert_eq!(line(&playing, None, false), "Spielt");
        assert_eq!(line(&mk(Phase::Connecting { server: "play.example.net".into() }), None, false), "Verbindet…");
        let lost = mk(Phase::Disconnected { server: Some("s".into()), reason: "Zeitüberschreitung".into() });
        assert_eq!(line(&lost, None, false), "Getrennt · Zeitüberschreitung");
        assert_eq!(line(&lost, None, true), "Getrennt · Zeitüberschreitung");
        let mut again = mk(Phase::Connecting { server: "s".into() });
        again.reconnect_attempts = 2;
        assert_eq!(line(&again, None, false), "Verbindet neu · 2/5");
        assert_eq!(line(&again, None, true), "Versuch 2/5");
        assert_eq!(line(&mk(Phase::Starting { done: 12, total: 50 }), None, true), "Wartet 12/50");
    }
}
