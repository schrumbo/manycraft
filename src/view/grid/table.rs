//! The grid as a table: one row per game with its status, reason, server, time and attempts, sortable by any column, the
//! same selection, menu and keys as the tiles, and the preview of the row under the pointer.

use std::cmp::Ordering;
use std::time::Instant;

use freya::prelude::*;

use super::tile::{self, Track};
use super::words;
use crate::app::model::{Game, Model, Phase};
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::menu::{self, Menu};
use crate::view::kit::text::line;
use crate::view::kit::use_theme;
use crate::view::store::{use_preview, use_store};
use crate::view::theme::{Theme, font, frame, iconsize, space};

/// A column of the table; `Order` is the core's own order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Order,
    Account,
    Batch,
    Status,
    Reason,
    Server,
    Since,
    Attempts,
}

const SINCE_W: f32 = 112.;
const STATUS_W: f32 = 136.;
const ATTEMPTS_W: f32 = 64.;
const FLEX_MIN: f32 = 112.;
/// The width of the preview of the row under the pointer.
pub const PREVIEW_W: f32 = 192.;

impl Column {
    const SHOWN: [Column; 7] = [Column::Account, Column::Batch, Column::Status, Column::Reason, Column::Server, Column::Since, Column::Attempts];

    fn label(self) -> String {
        match self {
            Column::Order => String::new(),
            Column::Account => tr("Account"),
            Column::Batch => tr("Batch"),
            Column::Status => tr("Status"),
            Column::Reason => tr("Grund"),
            Column::Server => tr("Server"),
            Column::Since => tr("Seit"),
            Column::Attempts => tr("Versuche"),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Column::Order => "order",
            Column::Account => "account",
            Column::Batch => "batch",
            Column::Status => "status",
            Column::Reason => "reason",
            Column::Server => "server",
            Column::Since => "since",
            Column::Attempts => "attempts",
        }
    }

    fn width(self) -> Size {
        match self {
            Column::Account | Column::Reason => Size::flex(2.),
            Column::Server => Size::flex(1.5),
            Column::Since => Size::px(SINCE_W),
            Column::Attempts => Size::px(ATTEMPTS_W),
            Column::Status => Size::px(STATUS_W),
            _ => Size::flex(1.2),
        }
    }

    fn numeric(self) -> bool {
        matches!(self, Column::Since | Column::Attempts)
    }

    fn least(self) -> f32 {
        match self {
            Column::Since => SINCE_W,
            Column::Attempts => ATTEMPTS_W,
            Column::Status => STATUS_W,
            _ => FLEX_MIN,
        }
    }

    /// The columns that fit `width`: account and status always stay, the rest drops from the least important.
    fn fitting(width: f32, batches: bool) -> Vec<Column> {
        let mut columns: Vec<Column> = Column::SHOWN.into_iter().filter(|c| batches || *c != Column::Batch).collect();
        let needs = |columns: &[Column]| columns.iter().map(|c| c.least()).sum::<f32>() + space::L * 2.;
        for dropped in [Column::Attempts, Column::Reason, Column::Server, Column::Since, Column::Batch] {
            if needs(&columns) <= width {
                break;
            }
            columns.retain(|c| *c != dropped);
        }
        columns
    }
}

fn server(g: &Game, m: &Model) -> String {
    g.phase.server().or_else(|| m.batch(&g.batch_id).and_then(|b| b.server.as_deref())).unwrap_or_default().to_owned()
}

fn since(g: &Game, m: &Model) -> u64 {
    words::last_change(&g.key, m).map_or(0, |e| e.at)
}

/// `keys` in the order of `column`, reversed when `down`; the core's order breaks ties.
pub fn sort(m: &Model, keys: &mut [String], (column, down): (Column, bool)) {
    if column == Column::Order {
        if down {
            keys.reverse();
        }
        return;
    }
    let place: std::collections::HashMap<&str, usize> = keys.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    let text = |g: &Game| -> String {
        match column {
            Column::Account => g.name.to_lowercase(),
            Column::Batch => m.batch(&g.batch_id).map(|b| b.name.to_lowercase()).unwrap_or_default(),
            Column::Status => words::word(g, m).to_lowercase(),
            Column::Reason => words::reason(g, m).unwrap_or_default().to_lowercase(),
            Column::Server => server(g, m).to_lowercase(),
            _ => String::new(),
        }
    };
    let mut decorated: Vec<(String, u64, usize, String)> = keys
        .iter()
        .filter_map(|k| m.games.get(k))
        .map(|g| {
            let n = match column {
                Column::Since => since(g, m),
                Column::Attempts => u64::from(g.reconnect_attempts),
                _ => 0,
            };
            (text(g), n, place.get(g.key.as_str()).copied().unwrap_or(usize::MAX), g.key.clone())
        })
        .collect();
    decorated.sort_by(|a, b| {
        let by = a.0.cmp(&b.0).then(a.1.cmp(&b.1));
        let by = if down { by.reverse() } else { by };
        let empty = |s: &(String, u64, usize, String)| column != Column::Since && column != Column::Attempts && s.0.is_empty();
        match (empty(a), empty(b)) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => by.then(a.2.cmp(&b.2)),
        }
    });
    for (slot, (.., key)) in keys.iter_mut().zip(decorated) {
        *slot = key;
    }
}

/// The table of `keys`, scrolled to `top`, `h` high.
#[derive(PartialEq)]
pub struct Table {
    pub keys: Vec<String>,
    pub top: f32,
    pub h: f32,
    pub w: f32,
    pub batches: bool,
}

/// The height of a row and of the header.
pub const ROW: f32 = frame::ROW + space::XS;

impl Component for Table {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut hovered = use_state(|| None::<(String, f32)>);
        use_side_effect(move || {
            let now = hovered.read().as_ref().map(|(k, _)| k.clone());
            store.set_visible(now.iter().cloned().collect());
            store.watch(|w| w.hovered = now);
        });
        use_drop(move || store.watch(|w| w.hovered = None));
        let sort = *store.ui.sort.read();
        let columns = Column::fitting(self.w, self.batches);
        let first = ((self.top / ROW).floor() as usize).min(self.keys.len());
        let last = (((self.top + self.h) / ROW).ceil() as usize + 1).min(self.keys.len());
        let mut body = rect().width(Size::fill()).height(Size::fill()).overflow(Overflow::Clip).on_pointer_out(move |_| hovered.set_if_modified(None));
        for i in first..last {
            let key = self.keys[i].clone();
            let y = ROW + i as f32 * ROW - self.top;
            let hover_key = key.clone();
            body = body.child(
                rect()
                    .key(&key)
                    .position(Position::new_absolute().left(0.).top(y))
                    .width(Size::fill())
                    .on_pointer_over(move |_| hovered.set_if_modified(Some((hover_key.clone(), y))))
                    .child(Row { key, index: i, columns: columns.clone() }),
            );
        }
        body = body.child(header(&t, &columns, sort));
        let history = store.ui.history.read().clone();
        if let Some((key, y)) = history.as_ref().and_then(|k| self.keys.iter().position(|x| x == k).map(|i| (k.clone(), ROW + (i + 1) as f32 * ROW - self.top))) {
            body = body.child(rect().position(Position::new_absolute().right(space::L).top(y.clamp(ROW, (self.h - ROW).max(ROW)))).child(tile::History { key }));
        } else if let Some((key, y)) = hovered.read().clone() {
            body = body.child(rect().position(Position::new_absolute().right(space::L).top((y + ROW).min((self.h - PREVIEW_W * 9. / 16. - space::S).max(ROW)))).key(&key).child(Peek { key }));
        }
        body
    }
}

fn header(t: &Theme, columns: &[Column], (by, down): (Column, bool)) -> Rect {
    let cells = columns.iter().map(|&c| HeaderCell { column: c, on: c == by, down }.into_element());
    rect()
        .position(Position::new_absolute().left(0.).top(0.))
        .width(Size::fill())
        .height(Size::px(ROW))
        .horizontal()
        .padding((0., space::L))
        .background(t.surface)
        .border(Border::new().fill(t.separator).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner))
        .children(cells)
        .content(Content::flex())
}

#[derive(PartialEq)]
struct HeaderCell {
    column: Column,
    on: bool,
    down: bool,
}

impl Component for HeaderCell {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let (c, on) = (self.column, self.on);
        let label = c.label();
        let spot = use_spot(&format!("table.head.{}", c.id()), "button", &label, true);
        let mut cell = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::ColumnHeader)
            .a11y_alt(label.clone())
            .width(c.width())
            .height(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .main_align(if c.numeric() { Alignment::End } else { Alignment::Start })
            .spacing(space::XXS)
            .padding((0., space::S))
            .corner_radius(t.shape.control)
            .background(if *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_press(move |_| {
                let mut sort = store.ui.sort;
                let (now, was_down) = *sort.peek();
                sort.set(if now != c {
                    (c, c.numeric())
                } else if was_down != c.numeric() {
                    (Column::Order, false)
                } else {
                    (c, !was_down)
                });
            })
            .child(line(&t, label, font::CAPTION, if on { t.text1 } else { t.text2 }).font_weight(if on { FontWeight::SEMI_BOLD } else { FontWeight::MEDIUM }).max_lines(1));
        if on {
            cell = cell.child(icon(&t, if self.down { Icon::ChevronDown } else { Icon::ChevronUp }, iconsize::XS, t.text2));
        }
        if let Some(border) = crate::view::kit::ring(&t, focus()) {
            cell = cell.border(border);
        }
        cell
    }
}

#[derive(PartialEq)]
struct Row {
    key: String,
    index: usize,
    columns: Vec<Column>,
}

impl Component for Row {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut track = use_state(Track::default);
        super::use_node(&self.key, a11y);
        let m = store.model.read();
        let Some(g) = m.games.get(&self.key) else { return rect().into_element() };
        let selected = store.ui.selection.read().contains(&self.key);
        let face = store.faces.read().get(&g.account_id).cloned();
        let word = words::word(g, &m);
        let reason = words::reason(g, &m);
        let spot = use_spot(&format!("row.{}", self.key), "row", &format!("{} · {word}", g.name), true);
        let severe = matches!(g.phase, Phase::LoginNeeded | Phase::Banned { .. });
        let tone = if severe { t.danger_text } else if g.needs_attention() { t.attention_text } else { t.text2 };
        let ink = if selected { t.accent_text } else { t.text1 };
        let cell = |c: Column| -> Element {
            let text = match c {
                Column::Account => g.name.clone(),
                Column::Batch => m.batch(&g.batch_id).map(|b| b.name.clone()).unwrap_or_default(),
                Column::Status => word.clone(),
                Column::Reason => reason.clone().unwrap_or_default(),
                Column::Server => server(g, &m),
                Column::Since => words::last_change(&g.key, &m).map(|e| words::when(e.at)).unwrap_or_default(),
                Column::Attempts => if g.reconnect_attempts > 0 { g.reconnect_attempts.to_string() } else { String::new() },
                Column::Order => String::new(),
            };
            let color = match c {
                Column::Status => tone,
                Column::Account => ink,
                _ => t.text2,
            };
            let mut r = rect().width(c.width()).height(Size::fill()).horizontal().cross_align(Alignment::Center).main_align(if c.numeric() { Alignment::End } else { Alignment::Start }).padding((0., space::S)).spacing(space::S);
            if c == Column::Account {
                r = r.child(tile::face_view(&t, m.account(g.account_id), face.clone(), matches!(g.phase, Phase::Playing { .. }) && !g.stopping));
            }
            let weight = if c == Column::Account { FontWeight::MEDIUM } else { FontWeight::NORMAL };
            r.child(line(&t, text, font::CALLOUT, color).font_weight(weight).max_lines(1).text_overflow(TextOverflow::Ellipsis)).into_element()
        };
        let (key, index) = (self.key.clone(), self.index);
        let menu_key = key.clone();
        let fill = if selected { t.selection } else if *hovered.read() { t.hover } else { Color::TRANSPARENT };
        let mut row = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Row)
            .a11y_alt(format!("{}, {word}", g.name))
            .width(Size::fill())
            .height(Size::px(ROW))
            .padding((0., space::L))
            .horizontal()
            .background(fill)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .cursor(CursorIcon::Pointer)
            .on_mouse_down(move |e: Event<MouseEventData>| e.stop_propagation())
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                a11y.request_focus();
                if let PressEventData::Keyboard(k) = &*e {
                    tile::keyed(store, &key, k);
                } else if track.write().double(Instant::now()) {
                    tile::primary(store, &key);
                } else {
                    tile::pick(store, &key, index);
                }
            })
            .on_secondary_down(move |e: Event<PressEventData>| {
                e.stop_propagation();
                if let PressEventData::Mouse(m) = &*e {
                    let at = (m.global_location.x as f32, m.global_location.y as f32);
                    menu::open(store.ui, Menu::at(tile::context(store, &menu_key), at));
                }
            })
            .children(self.columns.iter().map(|&c| cell(c)))
            .content(Content::flex());
        if let Some(border) = crate::view::kit::ring(&t, focus()) {
            row = row.border(border);
        }
        row.into_element()
    }
}

#[derive(PartialEq)]
struct Peek {
    key: String,
}

impl Component for Peek {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let preview = use_preview(&self.key);
        let Some(handle) = preview else { return rect().into_element() };
        let h = PREVIEW_W * 9. / 16.;
        let mut card = rect().width(Size::px(PREVIEW_W)).height(Size::px(h)).corner_radius(t.shape.tile).overflow(Overflow::Clip).background(t.well).layer(Layer::Overlay);
        if let Some(shadow) = t.elevation.card.clone() {
            card = card.shadow(shadow);
        }
        card.child(image(handle).width(Size::px(PREVIEW_W)).height(Size::px(h)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center)).into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::Column;

    #[test]
    fn a_narrow_table_keeps_account_and_status_and_drops_the_rest_in_order() {
        let wide = Column::fitting(1200., true);
        assert_eq!(wide, Column::SHOWN);
        let no_batch = Column::fitting(1200., false);
        assert!(!no_batch.contains(&Column::Batch));
        let mid = Column::fitting(700., true);
        assert!(!mid.contains(&Column::Attempts) && mid.contains(&Column::Account) && mid.contains(&Column::Status));
        for width in [0., 200., 428.] {
            let narrow = Column::fitting(width, true);
            assert!(narrow.contains(&Column::Account) && narrow.contains(&Column::Status), "{width}");
        }
        assert_eq!(Column::fitting(0., true), [Column::Account, Column::Status]);
    }
}