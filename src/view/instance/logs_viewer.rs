use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use freya::prelude::*;
use freya_core::elements::paragraph::{ParagraphCursorExt, ParagraphHolderInner};

use super::logs_lines::{Level, Line, on_line, utf16, wrap_rows};
use super::logs_select::{Selection, Spot, byte_at};
use super::shots_parts::Same;
use crate::core::i18n::{tr, trf};
use crate::ui::theme::{files, tint};
use crate::view::commands::Shortcut;
use crate::view::drive::use_spot;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::caption;
use crate::view::kit::{after, toast, use_theme};
use crate::view::native;
use crate::view::store::use_store;
use crate::view::theme::{Theme, font, space};
use crate::view::ui::Ui;

const MONO_ADVANCE: f32 = 0.62;
const SETTLE: Duration = Duration::from_millis(150);
const MULTI_CLICK: Duration = Duration::from_millis(400);
const AUTOSCROLL: Duration = Duration::from_millis(40);
const KEPT_ROWS: usize = 256;

/// How many characters of the log font fit in `width`.
pub fn columns(width: f32) -> usize {
    ((width - space::S * 2.) / (font::CALLOUT * MONO_ADVANCE)).floor().max(1.) as usize
}

/// The height of one line.
pub fn row() -> f32 {
    (font::CALLOUT * font::LINE).ceil()
}

/// Hits of a search: the line and the byte range of each, in line order.
pub type Hits = Same<Vec<(usize, Range<usize>)>>;

/// The wrapped rows of a log, kept between frames so appended lines wrap alone.
#[derive(Clone, Default)]
pub struct Wrapped(Rc<RefCell<Option<Kept>>>);

struct Kept {
    cols: usize,
    lines: usize,
    last: Option<Arc<str>>,
    rows: Same<Vec<(usize, Range<usize>)>>,
}

impl Wrapped {
    /// The first row of line `at` at `cols` characters, when those rows are kept.
    pub fn row_of(&self, at: usize, cols: usize) -> Option<usize> {
        self.0.borrow().as_ref().filter(|k| k.cols == cols).map(|k| k.rows.0.partition_point(|(l, _)| *l < at))
    }

    fn rows(&self, lines: &[Line], cols: usize) -> Same<Vec<(usize, Range<usize>)>> {
        let mut held = self.0.borrow_mut();
        let last = lines.last().map(|l| l.text.clone());
        if let Some(k) = held.as_mut().filter(|k| k.cols == cols)
            && let Some(now) = k.last.as_ref().and_then(|old| lines.iter().rposition(|l| Arc::ptr_eq(&l.text, old)))
            && now < k.lines
        {
            let shift = k.lines - 1 - now;
            if shift > 0 || now + 1 < lines.len() {
                let rows = Arc::make_mut(&mut k.rows.0);
                if shift > 0 {
                    let gone = rows.partition_point(|(l, _)| *l < shift);
                    rows.drain(..gone);
                    rows.iter_mut().for_each(|(l, _)| *l -= shift);
                }
                rows.extend(wrap_rows(&lines[now + 1..], cols).into_iter().map(|(l, r)| (l + now + 1, r)));
                k.lines = lines.len();
                k.last = last;
            }
            return k.rows.clone();
        }
        let rows = Same(Arc::new(wrap_rows(lines, cols)));
        *held = Some(Kept { cols, lines: lines.len(), last, rows: rows.clone() });
        rows
    }
}

#[derive(Clone, Default)]
struct Holders(Rc<RefCell<HashMap<usize, ParagraphHolder>>>);

impl PartialEq for Holders {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Holders {
    fn holder(&self, row: usize) -> ParagraphHolder {
        let mut rows = self.0.borrow_mut();
        if rows.len() > KEPT_ROWS {
            rows.retain(|at, _| at.abs_diff(row) < KEPT_ROWS / 2);
        }
        rows.entry(row).or_default().clone()
    }

    fn units_at(&self, row: usize, x: f32) -> Option<usize> {
        let rows = self.0.borrow();
        let inner = rows.get(&row)?.0.borrow();
        let ParagraphHolderInner { paragraph, scale_factor, .. } = inner.as_ref()?;
        Some(paragraph.cursor_index_at_point((f64::from(x) * scale_factor, 1.)))
    }
}

#[derive(PartialEq)]
struct Rows {
    theme: Rc<Theme>,
    lines: Same<Vec<Line>>,
    rows: Option<Same<Vec<(usize, Range<usize>)>>>,
    version: u64,
    hits: Hits,
    current: Option<usize>,
    width: f32,
    sel: Option<Selection>,
    holders: Holders,
}

fn text(t: &Theme, shown: &str, color: Color, hits: &[Range<usize>], picked: Option<Range<usize>>, holder: ParagraphHolder) -> Element {
    let (marks, wash) = match picked {
        Some(range) => (vec![range], t.accent),
        None => (hits.to_vec(), t.attention_text),
    };
    let ranges: Vec<(usize, usize)> = marks.iter().filter_map(|r| utf16(shown, r)).collect();
    let mut span = Span::new(shown.to_owned()).font_size(font::CALLOUT).color(color);
    if let Some(f) = &t.font {
        span = span.font_family(f.clone());
    }
    for family in crate::view::theme::faces::MONO {
        span = span.font_family(family);
    }
    paragraph().max_lines(1).holder(holder).highlight_color(wash.with_a((tint::SELECTED * 255.) as u8)).highlights(ranges).span(span).into_element()
}

fn line_at(item: VirtualItem, d: &Rows) -> Element {
    let t = &d.theme;
    let (at, part) = match &d.rows {
        Some(rows) => match rows.0.get(item.index) {
            Some((at, part)) => (*at, Some(part.clone())),
            None => (usize::MAX, None),
        },
        None => (item.index, None),
    };
    let Some(line) = d.lines.0.get(at) else {
        return rect().key(item.index).height(Size::px(item.size)).into_element();
    };
    let color = match line.level {
        Level::Plain => t.text1,
        Level::Warn => t.attention_text,
        Level::Error => t.danger_text,
    };
    let part = part.unwrap_or(0..line.text.len());
    let here: Vec<Range<usize>> = on_line(&d.hits.0, at).iter().filter(|(_, r)| r.start < part.end && r.end > part.start).map(|(_, r)| r.start.max(part.start) - part.start..r.end.min(part.end) - part.start).collect();
    let picked = d.sel.and_then(|s| s.on_row(at, &part));
    let shown = line.text.get(part).unwrap_or_default();
    let holder = d.holders.holder(item.index);
    let width = if d.rows.is_some() { Size::fill() } else { Size::px(d.width) };
    let mut r = rect().key(item.index).width(width).height(Size::px(item.size)).padding((0., space::S)).horizontal().cross_align(Alignment::Center);
    if d.current == Some(at) {
        r = r.background(t.selection);
    }
    r.child(text(t, shown, color, &here, picked, holder)).into_element()
}

/// Puts the selected text on the clipboard.
pub(super) fn copy_selection(ui: Ui, lines: &[Line], sel: &Selection) {
    if !native::copy_text(sel.text(lines)) {
        toast::show(ui, tr("Das Protokoll wurde nicht kopiert."), None);
    }
}

/// Finds the place in the log under a screen point from what the list last measured and drew.
#[derive(Clone)]
struct Probe {
    lines: Same<Vec<Line>>,
    rows: Option<Same<Vec<(usize, Range<usize>)>>>,
    holders: Holders,
    area: Rc<Cell<[f32; 4]>>,
    controller: ScrollController,
}

impl Probe {
    fn count(&self) -> usize {
        self.rows.as_ref().map_or(self.lines.0.len(), |r| r.0.len())
    }

    fn row(&self, index: usize) -> Option<(usize, Range<usize>)> {
        match &self.rows {
            Some(rows) => rows.0.get(index).cloned(),
            None => Some((index, 0..self.lines.0.get(index)?.text.len())),
        }
    }

    fn locate(&self, x: f32, y: f32) -> Option<Spot> {
        let [left, top, _, height] = self.area.get();
        let last = self.count().checked_sub(1)?;
        let scroll = self.controller.peek_position();
        let inside = (y - top).clamp(0., (height - 1.).max(0.));
        let index = (((inside - scroll.y) / row()).floor().max(0.) as usize).min(last);
        let (line, part) = self.row(index)?;
        let shown = self.lines.0.get(line)?.text.get(part.clone())?;
        let units = self.holders.units_at(index, x - left - space::S - scroll.x)?;
        Some(Spot { line, at: part.start + byte_at(shown, units) })
    }
}

/// What the pointer does in the log: select by dragging, by double and triple click and with Shift, and open the menu.
#[derive(Clone)]
struct Pointer {
    probe: Probe,
    sel: State<Option<Selection>>,
    ui: Ui,
    shift: Rc<Cell<bool>>,
    last: Rc<Cell<(f32, f32)>>,
    clicks: Rc<Cell<Option<(Instant, usize, u8)>>>,
    epoch: Rc<Cell<u64>>,
    held: Rc<Cell<u64>>,
}

impl Pointer {
    fn click(&self, line: usize) -> u8 {
        let now = Instant::now();
        let n = match self.clicks.get() {
            Some((at, was, n)) if was == line && now - at < MULTI_CLICK => n % 3 + 1,
            _ => 1,
        };
        self.clicks.set(Some((now, line, n)));
        n
    }

    fn extend(&self, x: f32, y: f32) {
        let Some(head) = self.probe.locate(x, y) else { return };
        let mut sel = self.sel;
        let next = (*self.sel.peek()).map(|s| Selection { head, ..s });
        sel.set_if_modified(next);
    }

    fn down(&self, e: &PointerEventData) {
        let PointerEventData::Mouse(m) = e else { return };
        let (x, y) = (m.global_location.x as f32, m.global_location.y as f32);
        let Some(spot) = self.probe.locate(x, y) else { return };
        let mut sel = self.sel;
        match m.button {
            Some(MouseButton::Left) => {
                let lines = &self.probe.lines.0;
                let clicks = self.click(spot.line);
                let next = match (clicks, self.shift.get(), *self.sel.peek()) {
                    (1, true, Some(s)) => Selection { head: spot, ..s },
                    (1, ..) => Selection::caret(spot),
                    (2, ..) => Selection::word(lines, spot),
                    _ => Selection::line(lines, spot.line),
                };
                sel.set_if_modified(Some(next));
                if clicks == 1 {
                    self.last.set((x, y));
                    let epoch = self.epoch.get() + 1;
                    self.epoch.set(epoch);
                    self.held.set(epoch);
                    self.autoscroll(epoch);
                }
            }
            Some(MouseButton::Right) => {
                if !self.sel.peek().is_some_and(|s| s.covers(spot)) {
                    sel.set_if_modified(None);
                }
                self.open_menu((x, y), spot.line);
            }
            _ => {}
        }
    }

    fn moved(&self, x: f32, y: f32) {
        if self.held.get() != 0 {
            self.last.set((x, y));
            self.extend(x, y);
        }
    }

    fn up(&self) {
        if self.held.replace(0) != 0 && self.sel.peek().is_some_and(|s| s.is_empty()) {
            let mut sel = self.sel;
            sel.set(None);
        }
    }

    fn autoscroll(&self, epoch: u64) {
        let this = self.clone();
        spawn_in_window(async move {
            while this.held.get() == epoch {
                after(AUTOSCROLL).await;
                if this.sel.try_peek().is_none() {
                    break;
                }
                let (x, y) = this.last.get();
                let [_, top, _, height] = this.probe.area.get();
                let beyond = if y < top {
                    y - top
                } else if y > top + height {
                    y - top - height
                } else {
                    continue;
                };
                let rows = (beyond.abs() / row()).clamp(1., 8.).round();
                let mut controller = this.probe.controller;
                let at = controller.peek_position().y;
                controller.scroll_to_y((at - beyond.signum() * rows * row()).min(0.) as i32);
                this.extend(x, y);
            }
        });
    }

    fn open_menu(&self, at: (f32, f32), line: usize) {
        let held = (*self.sel.peek()).filter(|s| !s.is_empty());
        let (ui, lines, sel) = (self.ui, self.probe.lines.clone(), self.sel);
        let (copy_lines, copy_line) = (lines.clone(), lines.clone());
        let mut all = sel;
        let items = vec![
            Item::new("instance.logs.menu.copy", tr("Kopieren"), move |_| {
                if let Some(s) = held {
                    copy_selection(ui, &copy_lines.0, &s);
                }
            })
            .shortcut(Shortcut::primary(Code::KeyC).label())
            .disabled(held.is_none()),
            Item::new("instance.logs.menu.line", tr("Zeile kopieren"), move |_| copy_selection(ui, &copy_line.0, &Selection::line(&copy_line.0, line))),
            Item::Separator,
            Item::new("instance.logs.menu.all", tr("Alles auswählen"), move |_| all.set(Selection::all(&lines.0))).shortcut(Shortcut::primary(Code::KeyA).label()),
        ];
        menu::open(self.ui, Menu::at(items, at));
    }
}

/// The lines of a log in a virtual list with a note above when older lines were left out; arrow keys scroll it.
/// With `wrap`, long lines break at the width the viewer measures into `cols`. The text can be selected with the pointer
/// into `sel`; a selection wins over search hits on the rows it covers.
#[derive(PartialEq)]
pub struct Viewer {
    pub lines: Same<Vec<Line>>,
    pub wrap: bool,
    pub cols: State<usize>,
    pub stick: State<bool>,
    pub wrapped: State<Wrapped>,
    pub version: u64,
    pub longest: usize,
    pub hits: Hits,
    pub current: Option<usize>,
    pub cut: bool,
    pub controller: ScrollController,
    pub sel: State<Option<Selection>>,
}

impl Component for Viewer {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let store = use_store();
        let mut spot = use_spot("instance.logs.viewer", "list", &tr("Protokoll"), true);
        let controller = self.controller;
        let height = row();
        let width = self.longest as f32 * font::CALLOUT * MONO_ADVANCE + space::S * 2.;
        let mut cols = self.cols;
        let mut stick = self.stick;
        let fit = *cols.read();
        let measured = use_hook(|| Rc::new(Cell::new(0usize)));
        let holders = use_hook(Holders::default);
        let area = use_hook(|| Rc::new(Cell::new([0.; 4])));
        let shift = use_hook(|| Rc::new(Cell::new(false)));
        let last = use_hook(|| Rc::new(Cell::new((0., 0.))));
        let clicks = use_hook(|| Rc::new(Cell::new(None)));
        let epoch = use_hook(|| Rc::new(Cell::new(0u64)));
        let held = use_hook(|| Rc::new(Cell::new(0u64)));
        use_drop({
            let held = held.clone();
            move || held.set(0)
        });
        let rows = (self.wrap && fit > 0).then(|| self.wrapped.peek().rows(&self.lines.0, fit));
        let count = rows.as_ref().map_or(self.lines.0.len(), |r| r.0.len());
        use_side_effect_with_deps(&count, move |count| {
            let mut c = controller;
            let bounds = *c.bounds.peek();
            if let (true, Some((mut content, view))) = (*stick.peek(), bounds) {
                content.height = *count as f32 * height;
                c.apply_layout(content, view);
            }
        });
        let sel = self.sel;
        let data = Rows { theme: t.clone(), lines: self.lines.clone(), rows: rows.clone(), version: self.version, hits: self.hits.clone(), current: self.current, width, sel: *sel.read(), holders: holders.clone() };
        let pointer = Pointer { probe: Probe { lines: self.lines.clone(), rows, holders, area: area.clone(), controller }, sel, ui: store.ui, shift: shift.clone(), last, clicks, epoch, held };
        let keys = move |e: Event<KeyboardEventData>| {
            let mut c = controller;
            let y = c.position().y;
            let to = match &e.key {
                Key::Named(NamedKey::ArrowUp) => y + height,
                Key::Named(NamedKey::ArrowDown) => y - height,
                Key::Named(NamedKey::Escape) if sel.peek().is_some() => {
                    let mut sel = sel;
                    sel.set(None);
                    e.stop_propagation();
                    return;
                }
                _ => return,
            };
            e.stop_propagation();
            c.scroll_to_y(to as i32);
        };
        let (down, moved, up) = (pointer.clone(), pointer.clone(), pointer);
        let (shift_down, shift_up) = (shift.clone(), shift);
        let mut frame = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::List)
            .a11y_alt(tr("Protokoll"))
            .width(Size::fill())
            .height(Size::fill())
            .background(t.well)
            .corner_radius(t.shape.control)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
            .overflow(Overflow::Clip)
            .cursor(CursorIcon::Text)
            .on_sized(move |e: Event<SizedEventData>| {
                let fit = columns(e.area.width());
                let was = measured.replace(fit);
                if fit != *cols.peek() && fit != was {
                    let measured = measured.clone();
                    let settle = if *cols.peek() == 0 { Duration::ZERO } else { SETTLE };
                    spawn_in_window(async move {
                        after(settle).await;
                        if measured.get() != fit || cols.try_peek().is_none_or(|c| *c == fit) {
                            return;
                        }
                        if super::logs::at_end(controller) {
                            stick.set(true);
                        }
                        cols.set(fit);
                    });
                }
                spot(e);
            })
            .on_mouse_down(move |_| a11y.request_focus())
            .on_pointer_down(move |e: Event<PointerEventData>| down.down(&e))
            .on_global_pointer_move(move |e: Event<PointerEventData>| {
                let at = e.global_location();
                moved.moved(at.x as f32, at.y as f32);
            })
            .on_global_pointer_up(move |_: Event<PointerEventData>| up.up())
            .on_global_key_down(move |e: Event<KeyboardEventData>| shift_down.set(e.modifiers.contains(Modifiers::SHIFT)))
            .on_global_key_up(move |e: Event<KeyboardEventData>| shift_up.set(e.modifiers.contains(Modifiers::SHIFT)))
            .on_key_down(keys);
        if self.cut {
            frame = frame.child(rect().width(Size::fill()).padding((space::XS, space::S)).child(caption(&t, trf("Ältere Zeilen stehen nur in der Datei. Gezeigt werden die letzten %lld.", &[&files::LOG_LINES]))));
        }
        let list = rect()
            .width(Size::fill())
            .height(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| area.set([e.area.min_x(), e.area.min_y(), e.area.width(), e.area.height()]))
            .child(VirtualScrollView::new_with_data_controlled(data, line_at, controller).length(count).item_size(height).scroll_with_arrows(false).width(Size::fill()).height(Size::fill()));
        frame.child(rect().width(Size::fill()).height(Size::flex(1.)).padding((space::XS, 0.)).child(list)).content(Content::flex())
    }
}

#[cfg(test)]
mod tests {
    use super::super::logs_lines::split;
    use super::*;

    #[test]
    fn kept_rows_follow_appends_and_a_dropped_front_like_a_fresh_wrap() {
        let kept = Wrapped::default();
        let mut lines = split("one two three four\nfive\nsix seven eight nine ten");
        assert_eq!(kept.rows(&lines, 8).0.as_slice(), wrap_rows(&lines, 8).as_slice());
        lines.extend(split("eleven twelve thirteen\nfourteen"));
        assert_eq!(kept.rows(&lines, 8).0.as_slice(), wrap_rows(&lines, 8).as_slice());
        lines.drain(..2);
        lines.extend(split("fifteen sixteen"));
        assert_eq!(kept.rows(&lines, 8).0.as_slice(), wrap_rows(&lines, 8).as_slice());
        assert_eq!(kept.row_of(2, 8), wrap_rows(&lines, 8).iter().position(|(l, _)| *l == 2));
        assert_eq!(kept.rows(&lines, 5).0.as_slice(), wrap_rows(&lines, 5).as_slice());
        let fresh = split("one two three four\nfive");
        assert_eq!(kept.rows(&fresh, 5).0.as_slice(), wrap_rows(&fresh, 5).as_slice());
    }
}
