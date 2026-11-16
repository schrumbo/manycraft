use std::ops::Range;
use std::rc::Rc;

use freya::prelude::*;

use super::logs_lines::{Level, Line, on_line, utf16};
use super::shots_parts::Same;
use crate::core::i18n::{tr, trf};
use crate::ui::theme::{files, tint};
use crate::view::drive::use_spot;
use crate::view::kit::text::{caption, mono};
use crate::view::kit::use_theme;
use crate::view::theme::{Theme, font, space};

const MONO_ADVANCE: f32 = 0.62;

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
    version: u64,
    hits: Hits,
    current: Option<usize>,
    width: f32,
}

fn text(t: &Theme, line: &Line, color: Color, hits: &[(usize, Range<usize>)]) -> Element {
    if hits.is_empty() {
        return mono(t, line.text.to_string(), font::CALLOUT, color).into_element();
    }
    let ranges: Vec<(usize, usize)> = hits.iter().filter_map(|(_, r)| utf16(&line.text, r)).collect();
    let mut span = Span::new(line.text.to_string()).font_size(font::CALLOUT).color(color);
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
    let Some(line) = d.lines.0.get(item.index) else {
        return rect().key(item.index).height(Size::px(item.size)).into_element();
    };
    let color = match line.level {
        Level::Plain => t.text1,
        Level::Warn => t.attention_text,
        Level::Error => t.danger_text,
    };
    let here = on_line(&d.hits.0, item.index);
    let mut r = rect().key(item.index).width(Size::px(d.width)).height(Size::px(item.size)).padding((0., space::S)).horizontal().cross_align(Alignment::Center);
    if d.current == Some(item.index) {
        r = r.background(t.selection);
    }
    r.child(text(t, line, color, here)).into_element()
}

/// The lines of a log in a virtual list with a note above when older lines were left out; keys scroll it.
#[derive(PartialEq)]
pub struct Viewer {
    pub lines: Same<Vec<Line>>,
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
        let spot = use_spot("instance.logs.viewer", "list", &tr("Protokoll"), true);
        let controller = self.controller;
        let height = row();
        let width = self.longest as f32 * font::CALLOUT * MONO_ADVANCE + space::S * 2.;
        let data = Rows { theme: t.clone(), lines: self.lines.clone(), version: self.version, hits: self.hits.clone(), current: self.current, width };
        let keys = move |e: Event<KeyboardEventData>| {
            let mut c = controller;
            let view = c.bounds.peek().map_or(0., |(_, view)| view.height);
            let page = ((view / height).floor() - 1.).max(1.) * height;
            let y = c.position().y;
            let to = match &e.key {
                Key::Named(NamedKey::ArrowUp) => y + height,
                Key::Named(NamedKey::ArrowDown) => y - height,
                Key::Named(NamedKey::PageUp) => y + page,
                Key::Named(NamedKey::PageDown) => y - page,
                Key::Named(NamedKey::Home) => 0.,
                Key::Named(NamedKey::End) => {
                    e.stop_propagation();
                    c.scroll_to(ScrollPosition::End, Direction::Vertical);
                    return;
                }
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
            .on_sized(spot)
            .on_mouse_down(move |_| a11y.request_focus())
            .on_key_down(keys);
        if self.cut {
            frame = frame.child(rect().width(Size::fill()).padding((space::XS, space::S)).child(caption(&t, trf("Ältere Zeilen stehen nur in der Datei. Gezeigt werden die letzten %lld.", &[&files::LOG_LINES]))));
        }
        frame.child(rect().width(Size::fill()).height(Size::flex(1.)).padding((space::XS, 0.)).child(VirtualScrollView::new_with_data_controlled(data, line_at, controller).length(self.lines.0.len()).item_size(height).scroll_with_arrows(false).width(Size::fill()).height(Size::fill()))).content(Content::flex())
    }
}
