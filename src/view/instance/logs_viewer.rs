use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use freya::prelude::*;

use super::logs_lines::{Level, Line, on_line, utf16, wrap_rows};
use super::shots_parts::Same;
use crate::core::i18n::{tr, trf};
use crate::ui::theme::{files, tint};
use crate::view::drive::use_spot;
use crate::view::kit::text::{caption, mono};
use crate::view::kit::use_theme;
use crate::view::theme::{Theme, font, space};

const MONO_ADVANCE: f32 = 0.62;

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

#[derive(PartialEq)]
struct Rows {
    theme: Rc<Theme>,
    lines: Same<Vec<Line>>,
    rows: Option<Same<Vec<(usize, Range<usize>)>>>,
    version: u64,
    hits: Hits,
    current: Option<usize>,
    width: f32,
}

fn text(t: &Theme, shown: &str, color: Color, hits: &[Range<usize>]) -> Element {
    if hits.is_empty() {
        return mono(t, shown.to_owned(), font::CALLOUT, color).into_element();
    }
    let ranges: Vec<(usize, usize)> = hits.iter().filter_map(|r| utf16(shown, r)).collect();
    let mut span = Span::new(shown.to_owned()).font_size(font::CALLOUT).color(color);
    if let Some(f) = &t.font {
        span = span.font_family(f.clone());
    }
    for family in crate::view::theme::faces::MONO {
        span = span.font_family(family);
    }
    paragraph().max_lines(1).highlight_color(t.attention_text.with_a((tint::SELECTED * 255.) as u8)).highlights(ranges).span(span).into_element()
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
    let shown = line.text.get(part).unwrap_or_default();
    let width = if d.rows.is_some() { Size::fill() } else { Size::px(d.width) };
    let mut r = rect().key(item.index).width(width).height(Size::px(item.size)).padding((0., space::S)).horizontal().cross_align(Alignment::Center);
    if d.current == Some(at) {
        r = r.background(t.selection);
    }
    r.child(text(t, shown, color, &here)).into_element()
}

/// The lines of a log in a virtual list with a note above when older lines were left out; arrow keys scroll it.
/// With `wrap`, long lines break at the width the viewer measures into `cols`.
#[derive(PartialEq)]
pub struct Viewer {
    pub lines: Same<Vec<Line>>,
    pub wrap: bool,
    pub cols: State<usize>,
    pub version: u64,
    pub longest: usize,
    pub hits: Hits,
    pub current: Option<usize>,
    pub cut: bool,
    pub controller: ScrollController,
}

impl Component for Viewer {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let mut spot = use_spot("instance.logs.viewer", "list", &tr("Protokoll"), true);
        let controller = self.controller;
        let height = row();
        let width = self.longest as f32 * font::CALLOUT * MONO_ADVANCE + space::S * 2.;
        let mut cols = self.cols;
        let fit = *cols.read();
        let cache = use_hook(|| Rc::new(RefCell::new(None::<((usize, u64, usize), Same<Vec<(usize, Range<usize>)>>)>)));
        let rows = (self.wrap && fit > 0).then(|| {
            let key = (Arc::as_ptr(&self.lines.0) as *const () as usize, self.version, fit);
            let mut held = cache.borrow_mut();
            match held.as_ref() {
                Some((have, rows)) if *have == key => rows.clone(),
                _ => {
                    let rows = Same(Arc::new(wrap_rows(&self.lines.0, fit)));
                    *held = Some((key, rows.clone()));
                    rows
                }
            }
        });
        let count = rows.as_ref().map_or(self.lines.0.len(), |r| r.0.len());
        let data = Rows { theme: t.clone(), lines: self.lines.clone(), rows, version: self.version, hits: self.hits.clone(), current: self.current, width };
        let keys = move |e: Event<KeyboardEventData>| {
            let mut c = controller;
            let view = c.bounds.peek().map_or(0., |(_, view)| view.height);
            let page = ((view / height).floor() - 1.).max(1.) * height;
            let y = c.position().y;
            let to = match &e.key {
                Key::Named(NamedKey::ArrowUp) => y + height,
                Key::Named(NamedKey::ArrowDown) => y - height,
                _ => return,
            };
            e.stop_propagation();
            c.scroll_to_y(to as i32);
        };
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
            .on_sized(move |e: Event<SizedEventData>| {
                cols.set_if_modified(columns(e.area.width()));
                spot(e);
            })
            .on_mouse_down(move |_| a11y.request_focus())
            .on_key_down(keys);
        if self.cut {
            frame = frame.child(rect().width(Size::fill()).padding((space::XS, space::S)).child(caption(&t, trf("Ältere Zeilen stehen nur in der Datei. Gezeigt werden die letzten %lld.", &[&files::LOG_LINES]))));
        }
        frame.child(rect().width(Size::fill()).height(Size::flex(1.)).padding((space::XS, 0.)).child(VirtualScrollView::new_with_data_controlled(data, line_at, controller).length(count).item_size(height).scroll_with_arrows(false).width(Size::fill()).height(Size::fill()))).content(Content::flex())
    }
}
