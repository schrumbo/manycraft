use freya::prelude::*;

use super::{Column, Line, context, face, online_text, pick, tone_color};
use crate::core::i18n::tr;
use crate::view::commands::{Want, want_blocked};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::{Tone, dot};
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Menu};
use crate::view::kit::text::line;
use crate::view::kit::{ring, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{Theme, control, font, frame, iconsize, space};

const ROW_H: f32 = control::M + space::XS;
const KIND_W: f32 = 96.;
const ONLINE_W: f32 = 136.;

fn width(column: Column) -> Size {
    match column {
        Column::Name => Size::flex(2.),
        Column::Kind => Size::px(KIND_W),
        Column::Batches => Size::flex(1.5),
        Column::Status => Size::flex(2.5),
        Column::Proxy => Size::flex(1.5),
        Column::Online => Size::px(ONLINE_W),
    }
}

fn heading(column: Column) -> String {
    match column {
        Column::Name => tr("Name"),
        Column::Kind => tr("Art"),
        Column::Batches => tr("Batches"),
        Column::Status => tr("Status"),
        Column::Proxy => tr("Proxy"),
        Column::Online => tr("Zuletzt online"),
    }
}

const COLUMNS: [Column; 6] = [Column::Name, Column::Kind, Column::Batches, Column::Status, Column::Proxy, Column::Online];

/// The accounts as a dense table that sorts by any column, for many accounts.
#[derive(PartialEq)]
pub struct Table {
    pub lines: Vec<Line>,
}

impl Component for Table {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let shared = store.ui.accounts;
        let scroll = use_scroll_controller(ScrollConfig::default);
        let mut viewport = use_state(|| 0f32);
        let cursor = *shared.cursor.read();
        let cursor_at = cursor.and_then(|c| self.lines.iter().position(|l| l.id == c));
        use_side_effect_with_deps(&cursor_at, move |at| {
            let Some(at) = *at else { return };
            let mut scroll = scroll;
            let top = at as f32 * ROW_H;
            let shown = -(scroll.peek_position().y);
            let h = *viewport.peek();
            if top < shown {
                scroll.scroll_to_y(-(top as i32));
            } else if h > 0. && top + ROW_H > shown + h {
                scroll.scroll_to_y(-((top + ROW_H - h) as i32));
            }
        });
        let n = self.lines.len();
        let header = rect().width(Size::fill()).height(Size::px(ROW_H)).padding((0., space::S)).horizontal().cross_align(Alignment::Center).spacing(space::M).children(COLUMNS.map(|c| HeaderCell { column: c }.into_element())).content(Content::flex());
        let rows = VirtualScrollView::new_with_data_controlled(self.lines.clone(), move |item: VirtualItem, lines: &Vec<Line>| {
            let l = &lines[item.index];
            rect().key(l.id).width(Size::fill()).height(Size::px(ROW_H)).child(Row { line: l.clone(), index: item.index, cursor: cursor == Some(l.id) }).into_element()
        }, scroll)
        .length(n)
        .item_size(ROW_H)
        .width(Size::fill())
        .height(Size::fill())
        .on_sized(move |e: Event<SizedEventData>| viewport.set_if_modified(e.area.height()));
        rect()
            .width(Size::fill())
            .height(Size::fill())
            .corner_radius(t.shape.tile)
            .background(t.raised)
            .overflow(Overflow::Clip)
            .child(header)
            .child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator))
            .child(rect().width(Size::fill()).height(Size::flex(1.)).child(rows))
            .content(Content::flex())
    }
}

#[derive(PartialEq)]
struct HeaderCell {
    column: Column,
}

impl Component for HeaderCell {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut sort = store.ui.accounts.sort;
        let mut hovered = use_state(|| false);
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let column = self.column;
        let label = heading(column);
        let spot = use_spot(&format!("accounts.sort.{column:?}").to_lowercase(), "button", &label, true);
        let (by, up) = *sort.read();
        let on = by == column;
        let ink = if on { t.text1 } else if *hovered.read() { t.text1 } else { t.text2 };
        let mut cell = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::ColumnHeader)
            .a11y_alt(label.clone())
            .width(width(column))
            .height(Size::px(control::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .corner_radius(t.shape.small)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_press(move |_| {
                a11y.request_focus();
                let (by, up) = *sort.peek();
                sort.set(if by == column { (column, !up) } else { (column, true) });
            })
            .child(line(&t, label, font::CALLOUT, ink).font_weight(if on { FontWeight::SEMI_BOLD } else { FontWeight::MEDIUM }).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        if on {
            cell = cell.child(icon(&t, if up { Icon::ArrowDown } else { Icon::ArrowUp }, iconsize::XS, t.text2));
        }
        if let Some(border) = ring(&t, focus()) {
            cell = cell.border(border);
        }
        cell
    }
}

#[derive(PartialEq)]
struct Row {
    line: Line,
    index: usize,
    cursor: bool,
}

impl Component for Row {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let l = &self.line;
        let spot = use_spot(&format!("accounts.row.{}", l.name), "row", &l.name, true);
        let picked = store.ui.accounts.picked.read().contains(&l.id);
        let id = l.id;
        let fill = if picked {
            t.selection
        } else if *hovered.read() {
            t.hover
        } else if self.index % 2 == 1 {
            t.hover.with_a(t.hover.a() / 2)
        } else {
            Color::TRANSPARENT
        };
        let ink = if l.busy { t.text3 } else { t.text1 };
        let quiet = |t: &Theme, s: String| line(t, s, font::CALLOUT, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis);
        let name = rect().width(width(Column::Name)).horizontal().cross_align(Alignment::Center).spacing(space::S).child(face(&t, store, l, frame::FACE)).child(line(&t, l.name.clone(), font::BODY, ink).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        let kind = rect().width(width(Column::Kind)).child(quiet(&t, if l.microsoft { tr("Microsoft") } else { tr("Offline") }));
        let batches = rect().width(width(Column::Batches)).child(quiet(&t, if l.batches.is_empty() { "—".to_owned() } else { l.batches.join(", ") }));
        let (text, tone) = if l.busy { (tr("Wird entfernt …"), Tone::Quiet) } else { (l.status.clone(), l.tone) };
        let mut status = rect().width(width(Column::Status)).horizontal().cross_align(Alignment::Center).spacing(space::S);
        if tone != Tone::Quiet {
            status = status.child(dot(tone_color(&t, tone)));
        }
        status = status.child(line(&t, text, font::CALLOUT, if tone == Tone::Quiet { t.text2 } else { tone_color(&t, tone) }).width(Size::flex(1.)).max_lines(1).text_overflow(TextOverflow::Ellipsis)).content(Content::flex());
        if l.sign_in && !l.busy {
            status = status.child(Button::new(format!("accounts.{}.signin", l.name), tr("Anmelden")).plain().disabled(want_blocked(&Want::SignInAgain(Vec::new()))).on_press(move |_| super::sign_in(store, vec![id])));
        }
        let proxy = rect().width(width(Column::Proxy)).child(quiet(&t, l.proxy.clone().unwrap_or_else(|| "—".to_owned())));
        let online = rect().width(width(Column::Online)).child(line(&t, online_text(l.online), font::CALLOUT, if l.online == Some(u64::MAX) { t.success_text } else { t.text2 }).max_lines(1));
        let mut r = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Row)
            .a11y_alt(format!("{}, {}", l.name, l.status))
            .width(Size::fill())
            .height(Size::fill())
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .background(fill)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_press(move |_| {
                a11y.request_focus();
                pick(store, id);
            })
            .on_secondary_down(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e {
                    let at = (m.global_location.x as f32, m.global_location.y as f32);
                    menu::open(store.ui, Menu::at(context(store, id, at), at));
                }
            })
            .child(name)
            .child(kind)
            .child(batches)
            .child(status)
            .child(proxy)
            .child(online)
            .content(Content::flex());
        if let Some(border) = ring(&t, focus()) {
            r = r.border(border);
        } else if self.cursor && !picked {
            r = r.border(Border::new().fill(t.focus.with_a(90)).width(1.).alignment(BorderAlignment::Inner));
        }
        r
    }
}
