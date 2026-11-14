//! The instance grid: a mosaic of previews that fills the window edge to edge, attention first, grouped by batch when it shows several, only the rows on screen drawn.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::Sizable;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{ClearSelection, EditBatch, MessageSelected, Play, ReconnectSelected, RestartSelected, SendChatMany, StartAll, StartBatch, StartSelected, StopSelected};
use super::drive::Mark;
use super::main_window::{Filter, MainWindow};
use super::model::{self, Instance, Model, Phase};
use super::motion::{SLOW, Soft};
use super::theme::{Theme, control, font, iconsize, radius, selection, shell, space, tile, tint, with_alpha};
use super::tile::{Geometry, TileData};
use super::widgets::{KeyNav, TakeFocus, icon, primary_button, tip};
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

/// The grid's order of instances: attention first, changed only after an instance's state changed and never under the pointer.
#[derive(Default)]
struct Order {
    keys: Vec<String>,
    seq: Option<u64>,
}

impl Order {
    /// Follows `m`. Without a state change since the last sort, or while `hovered`, it only drops instances that are gone and adds new ones
    /// at the end. True when the order changed.
    fn update(&mut self, m: &Model, hovered: bool) -> bool {
        if self.seq == Some(m.order_seq) || hovered {
            if self.keys.len() != m.instances.len() || self.keys.iter().any(|k| !m.instances.contains_key(k)) {
                let known: HashSet<&str> = self.keys.iter().map(String::as_str).collect();
                let mut fresh: Vec<&Instance> = m.instances.values().filter(|i| !known.contains(i.key.as_str())).collect();
                fresh.sort_by_cached_key(|i| i.name.to_lowercase());
                let fresh: Vec<String> = fresh.into_iter().map(|i| i.key.clone()).collect();
                self.keys.retain(|k| m.instances.contains_key(k));
                self.keys.extend(fresh);
            }
            return false;
        }
        self.seq = Some(m.order_seq);
        let sorted: Vec<String> = m.sorted(None).into_iter().map(|i| i.key.clone()).collect();
        let changed = sorted != self.keys;
        self.keys = sorted;
        changed
    }

    /// Moves `key` so that filtering the order again puts it where it is in `shown`, next to a neighbour of the same `group`.
    fn follow(&mut self, shown: &[String], key: &str, group: impl Fn(&str) -> Option<String>) {
        let (Some(at), Some(from)) = (shown.iter().position(|k| k == key), self.keys.iter().position(|k| k == key)) else { return };
        let mine = group(key);
        let moved = self.keys.remove(from);
        let before = at.checked_sub(1).map(|p| shown[p].as_str()).filter(|k| group(k) == mine).and_then(|p| self.keys.iter().position(|k| k == p)).map(|i| i + 1);
        let after = shown.get(at + 1).map(String::as_str).filter(|k| group(k) == mine).and_then(|n| self.keys.iter().position(|k| k == n));
        self.keys.insert(before.or(after).unwrap_or(from).min(self.keys.len()), moved);
    }
}

/// `sorted` with `pinned` moved back to its place in `old`, kept among the keys of its `group`.
fn arrange(mut sorted: Vec<String>, old: &[String], pinned: &str, group: impl Fn(&str) -> Option<String>) -> Vec<String> {
    if let Some(was) = old.iter().position(|k| k == pinned)
        && let Some(now) = sorted.iter().position(|k| k == pinned)
    {
        let key = sorted.remove(now);
        let mine = group(&key);
        let low = sorted.iter().position(|k| group(k) == mine).unwrap_or(now);
        let high = sorted.iter().rposition(|k| group(k) == mine).map_or(low, |i| i + 1);
        sorted.insert(was.clamp(low.min(high), high).min(sorted.len()), key);
    }
    sorted
}

/// Keyboard cursor, selection, sort order and scroll position of the grid.
pub struct Grid {
    pub focus: FocusHandle,
    /// The tile under the keyboard cursor.
    pub selected: Option<String>,
    pub picks: Picks,
    pub message: Option<Message>,
    order: Order,
    hovered: bool,
    goal: Option<f32>,
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
            order: Order::default(),
            hovered: false,
            goal: None,
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
        self.goal = None;
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

/// Columns per section and tile height of the mosaic, and whether it scrolls.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub columns: Vec<usize>,
    pub height: f32,
    pub scrolls: bool,
}

/// Width of each tile in a row of `count` tiles across `width`.
pub fn tile_width(count: usize, width: f32) -> f32 {
    let count = count.max(1) as f32;
    (width - tile::HAIRLINE * (count - 1.)) / count
}

/// The mosaic for sections of `sections` tiles each in a `width` × `height` area; `headed` sections carry a heading.
/// A section of n tiles takes ceil(√n) columns and the tiles fill the area, the last row of a section stretching across.
/// When that makes tiles lower than the smallest tile, sections take more columns, up to as many as fit at the smallest width;
/// when even that is not enough, the grid scrolls.
pub fn layout(sections: &[usize], width: f32, height: f32, headed: bool) -> Layout {
    let room = height - if headed { sections.len() as f32 * tile::HEADING } else { 0. };
    let fits = (((width + tile::HAIRLINE) / (tile::MIN_W + tile::HAIRLINE)).floor() as usize).max(1);
    let columns = |least: usize| -> Vec<usize> { sections.iter().map(|&n| ((n as f32).sqrt().ceil() as usize).max(least).min(n).min(fits).max(1)).collect() };
    (1..=fits)
        .find_map(|least| {
            let columns = columns(least);
            let rows = sections.iter().zip(&columns).map(|(n, c)| n.div_ceil(*c)).sum::<usize>().max(1);
            let height = room / rows as f32 - tile::HAIRLINE;
            (height >= tile::MIN_H).then_some(Layout { columns, height, scrolls: false })
        })
        .unwrap_or_else(|| Layout { columns: columns(fits), height: (tile_width(fits, width) / tile::ASPECT).max(tile::MIN_H), scrolls: true })
}

/// The status of `i` in a word for its tile; the full line is in its tooltip.
fn word(i: &Instance, preparing: Option<u8>, banned: bool) -> String {
    if i.stopping {
        return tr("Stoppt…");
    }
    let hint = model::hint(&i.key);
    if hint.proxy.is_some() && matches!(i.phase, Phase::Connecting { .. } | Phase::Disconnected { .. }) {
        return tr("Proxy-Fehler");
    }
    if hint.link_lost && i.phase.is_running() {
        return tr("Kein Kontakt");
    }
    let tries = crate::core::settings::active().reconnect.max_attempts;
    match &i.phase {
        _ if banned => tr("Gesperrt"),
        Phase::Stopped => tr("Gestoppt"),
        Phase::Starting { .. } if preparing.is_some() => trf("Lädt %lld %%", &[&preparing.unwrap_or(0)]),
        Phase::Starting { done, total } if *total > 1 => trf("Wartet %lld/%lld", &[done, total]),
        Phase::Starting { .. } => tr("Startet…"),
        Phase::Restarting { .. } => tr("Startet neu…"),
        Phase::Menu => tr("Hauptmenü"),
        Phase::Connecting { .. } if i.reconnect_attempts > 0 => trf("Versuch %lld/%lld", &[&i.reconnect_attempts, &tries]),
        Phase::Connecting { .. } => tr("Verbindet…"),
        Phase::Queue { .. } => tr("Warteschlange"),
        Phase::Playing { .. } => tr("Spielt"),
        Phase::Disconnected { .. } => tr("Getrennt"),
        Phase::Dead { .. } => tr("Gestorben"),
        Phase::Crashed { .. } => tr("Abgestürzt"),
        Phase::Banned { .. } => tr("Gesperrt"),
        Phase::LoginNeeded => tr("Anmeldung nötig"),
        Phase::Failed { .. } => tr("Fehlgeschlagen"),
    }
}

fn data(i: &Instance, m: &Model, selected: &Option<String>, picked: &HashSet<String>) -> TileData {
    let preparing = m.preparing.get(&i.batch_id).copied();
    let ban = super::state::stopped_ban(i, m);
    TileData {
        key: i.key.clone(),
        name: i.name.clone(),
        account_id: i.account_id,
        color: m.account(i.account_id).map_or(0, |a| a.color()),
        batch_id: i.batch_id.clone(),
        phase: i.phase.clone(),
        status: word(i, preparing, ban.is_some()),
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
    let mut goal = None;
    let next = match (at, step) {
        (None, Move::Last) => last,
        (None, _) | (Some(_), Move::First) => 0,
        (Some(_), Move::Last) => last,
        (Some(a), Move::Back) => a.saturating_sub(1),
        (Some(a), Move::Forward) => (a + 1).min(last),
        (Some(a), Move::Up(n) | Move::Down(n)) => {
            let Some(row) = g.row_at(a) else { return };
            let (_, start, stop) = g.rows[row];
            let to = if matches!(step, Move::Up(_)) { row.saturating_sub(n) } else { (row + n).min(g.rows.len() - 1) };
            let (_, from, end) = g.rows[to];
            let fraction = g.goal.unwrap_or((((a - start) as f32) + 0.5) / (stop - start) as f32);
            goal = Some(fraction);
            from + column_at(fraction, end - from)
        }
    };
    let key = g.shown[next].clone();
    g.click(&key, extend, false);
    g.goal = goal;
    if let Some(row) = g.row_at(next) {
        g.reveal_row(row);
    }
    cx.notify();
}

/// The column in a row of `count` tiles at `fraction` of the width.
fn column_at(fraction: f32, count: usize) -> usize {
    ((fraction * count as f32) as usize).min(count.saturating_sub(1))
}

fn action(id: &'static str, label: String, icon_name: &str, action: Box<dyn Action>, t: &Theme) -> AnyElement {
    primary_button(id, label, icon_name, false, t).on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)).into_any_element()
}

fn empty(id: &'static str, symbol: &str, title: String, text: String, button: Option<AnyElement>, focus: &FocusHandle, t: &Theme) -> AnyElement {
    div()
        .id(id)
        .key_context("InstanceGrid")
        .track_focus(focus)
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
    let id = batch.to_owned();
    let exporting = m.exporting.contains(batch);
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
        .context_menu(move |menu, _, _| super::sidebar::batch_menu(menu, &id, running, stopped, exporting))
        .into_any_element()
}

/// Draws the grid of `batch` (all batches when `None`) with the window's filter, size and search.
pub fn render(this: &mut MainWindow, batch: Option<&str>, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = Theme::get(cx);
    let model = model::get(cx);
    let m = model.read(cx);
    let resorted = this.grid.order.update(m, this.grid.hovered);
    if resorted && this.grid.selected_in_view && this.grid.focus.is_focused(window) && window.last_input_was_keyboard() {
        this.grid.reveal = true;
    }
    let scope = (batch.map(str::to_owned), this.filter, this.query.clone());
    let same_scope = this.grid.scope.as_ref() == Some(&scope);
    this.grid.scope = Some(scope);
    let held: HashSet<&str> = if this.grid.hovered && same_scope { this.grid.shown.iter().map(String::as_str).collect() } else { HashSet::new() };
    let words = words(&this.query);
    let mut keys: Vec<String> = this.grid.order.keys.iter().filter(
|k| held.contains(k.as_str()) || m.instances.get(*k).is_some_and(|i| matches(i, batch, this.filter) && found(i, m, &words))).cloned().collect();
    drop(held);
    let batch_of = |k: &String| m.instances.get(k).map(|i| i.batch_id.clone()).unwrap_or_default();
    let headed = batch.is_none() && keys.iter().map(&batch_of).collect::<HashSet<_>>().len() > 1;
    if headed {
        let rank: HashMap<&str, usize> = m.batches.iter().enumerate().map(|(n, b)| (b.id.as_str(), n)).collect();
        keys.sort_by_key(|k| m.instances.get(k).and_then(|i| rank.get(i.batch_id.as_str()).copied()).unwrap_or(usize::MAX));
    }
    if resorted && same_scope && let Some(playing) = m.playing.as_deref() {
        let group = |k: &str| headed.then(|| batch_of(&k.to_owned()));
        keys = arrange(std::mem::take(&mut keys), &this.grid.shown, playing, group);
        this.grid.order.follow(&keys, playing, group);
    }
    if !this.grid.picks.set.is_empty() {
        let shown: HashSet<&str> = keys.iter().map(String::as_str).collect();
        if this.grid.picks.set.iter().any(|k| !shown.contains(k.as_str())) {
            this.grid.picks.set.retain(|k| shown.contains(k.as_str()));
            this.grid.settle();
        }
    }
    if keys.is_empty() {
        let resting = m.attention_count(batch) == 0;
        this.grid.shown.clear();
        this.grid.rows.clear();
        let in_scope = m.instances.values().any(|i| batch.is_none_or(|b| i.batch_id == b));
        super::tile::unhover_except(None, cx);
        super::state::set_visible(HashSet::new(), cx);
        if !in_scope {
            let edit = batch.map(|b| action("edit-batch", tr("Batch bearbeiten"), "pencil", Box::new(EditBatch(b.to_owned())), &t));
            return empty("no-accounts", "users", tr("Noch keiner da."), tr("Füg einen Account hinzu und los."), edit, &this.grid.focus, &t);
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
            return empty("no-match", "search", tr("Nichts gefunden."), trf("Zu „%@“ passt keiner.", &[&this.query]), Some(clear), &this.grid.focus, &t);
        }
        return match this.filter {
            Filter::Attention => empty("all-good", "circle-check", tr("Alles ruhig."), tr("Gerade braucht dich keiner."), None, &this.grid.focus, &t),
            Filter::Stopped => empty("none-stopped", "circle-check", tr("Nichts gestoppt."), tr("Alle laufen oder brauchen dich."), None, &this.grid.focus, &t),
            Filter::Running | Filter::All => {
                let start: Box<dyn Action> = match batch {
                    Some(b) => Box::new(StartBatch(b.to_owned())),
                    None => Box::new(StartAll),
                };
                let label = if batch.is_some() { tr("Starten") } else { tr("Alle starten") };
                let title = if resting { tr("Alle schlummern.") } else { tr("Gerade läuft keiner.") };
                empty("none-running", "play", title, tr("Starte sie, dann siehst du sie hier."), Some(action("start-empty", label, "play", start, &t)), &this.grid.focus, &t)
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
    let (width, height) = if area.width > px(0.) { (f32::from(area.width), f32::from(area.height)) } else { (f32::from(window.viewport_size().width), f32::from(window.viewport_size().height) - shell::TOOLBAR) };
    let width = width.max(1.);
    let counts: Vec<usize> = sections.iter().map(|(_, from, to)| to - from).collect();
    let Layout { columns, height: tile_h, .. } = layout(&counts, width, height.max(1.), headed);
    let row_h = tile_h + tile::HAIRLINE;
    let mut items = Vec::new();
    for (n, (_, from, to)) in sections.iter().enumerate() {
        if headed {
            items.push(Item::Heading(n));
        }
        let mut at = *from;
        while at < *to {
            items.push(Item::Row { from: at, to: (at + columns[n]).min(*to) });
            at += columns[n];
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
            let step = tile_width(to - from, width) + tile::HAIRLINE;
            for (c, k) in keys[from..to].iter().enumerate() {
                places.insert(k.clone(), (c as f32 * step, tops[ix]));
            }
        }
    }
    if resorted && !cx.reduce_motion() {
        this.grid.moves = places.iter().filter_map(|(k, new)| this.grid.places.get(k).filter(|old| *old != new).map(|old| (k.clone(), *old))).collect();
        this.grid.moved_at = (!this.grid.moves.is_empty()).then(Instant::now);
    }
    this.grid.sync(items.clone(), (row_h, width), &keys);
    this.grid.to_top = false;
    this.grid.places = places.clone();
    this.grid.shown = keys.clone();
    this.grid.rows = rows;
    this.grid.row_h = row_h;
    this.grid.tops = tops.clone();
    this.grid.headed = headed;
    let bar = if this.grid.picks.set.len() >= 2 { selection::BAR + selection::BOTTOM } else { 0. };
    this.grid.padding = bar;
    if std::mem::take(&mut this.grid.reveal)
        && let Some(row) = this.grid.selected.as_ref().and_then(|s| keys.iter().position(|k| k == s)).and_then(|at| this.grid.row_at(at))
    {
        this.grid.reveal_row(row);
    }
    let selected = if this.grid.focus.is_focused(window) { this.grid.selected.clone() } else { None };
    let picked = Rc::new(this.grid.picks.set.clone());
    let progress = this.grid.moved_at.map(|at| (at.elapsed().as_secs_f32() / SLOW.as_secs_f32()).min(1.));
    if progress.is_some_and(|p| p >= 1.) {
        this.grid.moved_at = None;
        this.grid.moves.clear();
    } else if progress.is_some() {
        window.request_animation_frame();
    }
    let ease = progress.filter(|p| *p < 1.).map(|p| 1. - (1. - p).powi(3));
    let moves = Rc::new(this.grid.moves.clone());
    let view = cx.entity().downgrade();
    let (shown, items, tops, places) = (Rc::new(keys), Rc::new(items), Rc::new(tops), Rc::new(places));
    let section_batches: Rc<Vec<String>> = Rc::new(sections.iter().map(|(b, _, _)| b.clone()).collect());
    let scrolled = this.grid.list.logical_scroll_top();
    let from = tops.get(scrolled.item_ix).copied().unwrap_or(0.) + f32::from(scrolled.offset_in_item);
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
    let pinned = pinned.flatten().map(|n| div().absolute().top_0().left_0().right_0().px(px(tile::INSET)).bg(t.window).border_b_1().border_color(t.separator).child(heading(&section_batches[n], true, m, &t)));
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
                div().px(px(tile::INSET)).child(heading(&list_batches[n], false, m, &t)).into_any_element()
            }
            Some(Item::Row { from, to }) => {
                let geometry = Geometry::new(tile_width(to - from, width), tile_h);
                let cells: Vec<TileData> = {
                    let m = model.read(cx);
                    list_shown[from..to].iter().filter_map(|k| m.instances.get(k).map(|i| data(i, m, &selected, &picked))).collect()
                };
                let cells: Vec<AnyElement> = cells
                    .into_iter()
                    .map(|d| {
                        let offset = match (moves.get(&d.key), ease, places.get(&d.key)) {
                            (Some(old), Some(e), Some(new)) => ((old.0 - new.0) * (1. - e), (old.1 - new.1) * (1. - e)),
                            _ => (0., 0.),
                        };
                        super::tile::view(d, geometry, offset, view.clone(), cx)
                    })
                    .collect();
                div().h(px(row_h)).flex().gap(px(tile::HAIRLINE)).children(cells).into_any_element()
            }
            None => div().into_any_element(),
        }
    })
    .pb(px(bar))
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
        msg.input.update(cx, |s, cx| s.take_focus(window, cx));
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
    input.update(cx, |s, cx| s.take_focus(window, cx));
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
        s.take_focus(window, cx);
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
        .shadow(vec![
            BoxShadow { color: white().opacity(tint::RIM), offset: point(px(0.), px(1.)), blur_radius: px(0.), spread_radius: px(0.), inset: true },
            BoxShadow { color: black().opacity(tint::FLOAT_SHADOW), offset: point(px(0.), px(selection::SHADOW_Y)), blur_radius: px(selection::SHADOW_BLUR), spread_radius: px(0.), inset: false },
        ])
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
    use super::{Filter, Instance, Model, Order, Phase, Picks, arrange, chosen, column_at, found, layout, matches, tile, tile_width, word, words};

    fn rows(sections: &[usize], columns: &[usize]) -> usize {
        sections.iter().zip(columns).map(|(n, c)| n.div_ceil(*c)).sum()
    }

    #[test]
    fn columns_follow_the_square_root_whatever_the_window_shape() {
        for (w, h) in [(1060., 718.), (1920., 1000.), (2400., 700.)] {
            for (n, columns) in [(1, 1), (2, 2), (3, 2), (4, 2), (5, 3), (6, 3), (7, 3), (9, 3)] {
                let l = layout(&[n], w, h, false);
                assert_eq!((l.columns, l.scrolls), (vec![columns], false), "{n} tiles in {w}x{h}");
            }
        }
        assert_eq!(layout(&[16], 1920., 1000., false).columns, [4]);
        assert_eq!(layout(&[10], 1920., 1000., false).columns, [4]);
    }

    #[test]
    fn the_last_row_stretches_across() {
        let w = 1060.;
        assert_eq!(tile_width(2, w), (w - tile::HAIRLINE) / 2., "2 = 50/50");
        assert_eq!(tile_width(1, w), w, "the third of 3 spans the whole width");
        assert_eq!(tile_width(3, w) * 3. + tile::HAIRLINE * 2., w);
    }

    #[test]
    fn a_layout_that_fits_fills_the_height_exactly() {
        for (sections, width, height, headed) in [(vec![1], 1060., 718., false), (vec![2], 1060., 718., false), (vec![9], 1060., 718., false), (vec![6, 3, 1], 1280., 752., true), (vec![7], 500., 900., false)] {
            let l = layout(&sections, width, height, headed);
            assert!(!l.scrolls, "{sections:?}");
            let headings = if headed { sections.len() as f32 * tile::HEADING } else { 0. };
            let used = headings + rows(&sections, &l.columns) as f32 * (l.height + tile::HAIRLINE);
            assert!((used - height).abs() < 0.01, "{sections:?} uses {used} of {height}");
            assert!(l.height >= tile::MIN_H && l.columns.iter().all(|c| tile_width(*c, width) >= tile::MIN_W));
        }
        assert_eq!(layout(&[6, 3, 1], 1280., 752., true).columns, [3, 2, 1], "each section takes its own columns");
    }

    #[test]
    fn low_tiles_take_more_columns_before_the_grid_scrolls() {
        let l = layout(&[9], 1060., 300., false);
        assert_eq!((l.columns.clone(), l.scrolls), (vec![5], false), "3 rows would be too low, 2 rows of 5 fit");
        assert!(l.height >= tile::MIN_H);
    }

    #[test]
    fn below_the_smallest_tile_the_grid_scrolls_instead_of_shrinking() {
        for n in [50, 200, 1000] {
            let l = layout(&[n], 1060., 718., false);
            assert!(l.scrolls, "{n}");
            assert_eq!(l.columns, [5], "as many columns as fit at the smallest width");
            assert!(tile_width(5, 1060.) >= tile::MIN_W && l.height >= tile::MIN_H);
            assert!((tile_width(5, 1060.) / l.height - tile::ASPECT).abs() < 0.01, "scrolling tiles are 16:9");
        }
        let narrow = layout(&[20], 150., 400., false);
        assert_eq!((narrow.columns.clone(), narrow.scrolls), (vec![1], true));
        assert!(narrow.height >= tile::MIN_H);
        let small = layout(&[2; 5], 1060., 718., true);
        assert!(small.scrolls && small.height < 120., "small sections scroll at the smallest tile height, not as huge tiles: {small:?}");
    }

    fn mk(phase: Phase) -> Instance {
        Instance { key: "k".into(), batch_id: "b".into(), account_id: uuid::Uuid::nil(), name: "AltOne".into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, live: false, stopping: false }
    }

    fn fleet(list: &[(&str, Phase)]) -> Model {
        let mut m = Model::default();
        for (name, phase) in list {
            m.instances.insert((*name).to_owned(), Instance { key: (*name).into(), name: (*name).into(), ..mk(phase.clone()) });
        }
        m
    }

    fn lost() -> Phase {
        Phase::Disconnected { server: None, reason: String::new() }
    }

    fn playing() -> Phase {
        Phase::Playing { server: "s".into(), since: std::time::Instant::now() }
    }

    #[test]
    fn attention_sorts_first_but_only_after_a_state_change_off_the_grid() {
        let mut m = fleet(&[("a", Phase::Stopped), ("b", playing()), ("c", lost()), ("d", playing())]);
        let mut order = Order::default();
        assert!(order.update(&m, false));
        assert_eq!(order.keys, ["c", "b", "d", "a"]);
        m.instances.get_mut("d").unwrap().phase = Phase::Crashed { code: Some(1) };
        assert!(!order.update(&m, false), "no state change counted yet");
        m.order_seq += 1;
        assert!(!order.update(&m, true), "never under the pointer");
        assert_eq!(order.keys, ["c", "b", "d", "a"]);
        m.instances.insert("e".into(), Instance { key: "e".into(), name: "e".into(), ..mk(lost()) });
        assert!(!order.update(&m, true));
        assert_eq!(order.keys, ["c", "b", "d", "a", "e"], "a new one waits at the end");
        assert!(order.update(&m, false));
        assert_eq!(order.keys, ["c", "d", "e", "b", "a"]);
    }

    #[test]
    fn the_instance_being_played_keeps_its_place_on_screen() {
        let k = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let flat = |_: &str| None;
        assert_eq!(arrange(k(&["c", "a", "b"]), &k(&["a", "b", "c"]), "b", flat), ["c", "b", "a"], "b stays second while c moves up past it");
        assert_eq!(arrange(k(&["x", "y"]), &k(&["x", "y", "z"]), "z", flat), ["x", "y"], "a pinned key that is gone changes nothing");
        let batch = |key: &str| Some(key[..1].to_owned());
        assert_eq!(arrange(k(&["a1", "a2", "b2", "b1"]), &k(&["b1", "b2", "a1", "a2"]), "b1", batch), ["a1", "a2", "b1", "b2"], "it never leaves its batch's section");
        let mut order = Order { keys: k(&["x1", "b3", "b1", "x2", "b2"]), seq: None };
        let shown = k(&["b3", "b2", "b1"]);
        order.follow(&shown, "b2", flat);
        let again: Vec<&String> = order.keys.iter().filter(|key| key.starts_with('b')).collect();
        assert_eq!(again, ["b3", "b2", "b1"], "filtering the order again keeps the pinned place");
    }

    #[test]
    fn up_and_down_keep_the_column_under_the_tile_centre() {
        let centre = |column: usize, count: usize| (column as f32 + 0.5) / count as f32;
        for from in [1, 2, 3, 5] {
            for column in 0..from {
                assert_eq!(column_at(centre(column, from), from), column, "the kept goal brings Up/Down back to column {column} of {from}");
            }
        }
        assert_eq!(column_at(centre(2, 3), 2), 1);
        assert_eq!(column_at(centre(0, 5), 2), 0);
        assert_eq!(column_at(1., 4), 3);
    }

    #[test]
    fn tile_status_is_one_word() {
        crate::core::i18n::init("de");
        assert_eq!(word(&mk(playing()), None, false), "Spielt");
        assert_eq!(word(&mk(Phase::Connecting { server: "play.example.net".into() }), None, false), "Verbindet…");
        let lost = mk(Phase::Disconnected { server: Some("s".into()), reason: "Zeitüberschreitung".into() });
        assert_eq!(word(&lost, None, false), "Getrennt");
        let mut again = mk(Phase::Connecting { server: "s".into() });
        again.reconnect_attempts = 2;
        assert_eq!(word(&again, None, false), "Versuch 2/5");
        assert_eq!(word(&mk(Phase::Starting { done: 12, total: 50 }), None, false), "Wartet 12/50");
        assert_eq!(word(&mk(Phase::Stopped), None, true), "Gesperrt", "a stop after a ban says so");
        assert_eq!(word(&mk(Phase::LoginNeeded), None, false), "Anmeldung nötig");
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
}
