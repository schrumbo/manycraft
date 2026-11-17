use freya::prelude::*;

use super::card::state_mark;
use super::{Line, ago};
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::view::drive::use_spot;
use crate::view::icons::Icon;
use crate::view::instance::{InstanceIcon, Mini};
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Menu};
use crate::view::kit::text::{body, caption, hairline, line as text_line};
use crate::view::kit::{ring, use_theme};
use crate::view::shell::instance_items;
use crate::view::store::use_store;
use crate::view::theme::{font, space};
use crate::view::ui::Page;

const ROW: f32 = 44.;
const FACE: f32 = 28.;
const NARROW: f32 = 96.;
const WIDE: f32 = 128.;

/// The instances of a group as rows: name, version, loader, group and when it was played last.
#[derive(PartialEq)]
pub struct Table {
    pub lines: Vec<Line>,
}

impl Component for Table {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let head = rect()
            .width(Size::fill())
            .padding((space::XS, space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .child(rect().width(Size::flex(1.)).child(caption(&t, tr("Name"))))
            .child(rect().width(Size::px(NARROW)).child(caption(&t, tr("Version"))))
            .child(rect().width(Size::px(NARROW)).child(caption(&t, tr("Loader"))))
            .child(rect().width(Size::px(WIDE)).child(caption(&t, tr("Gruppe"))))
            .child(rect().width(Size::px(WIDE)).child(caption(&t, tr("Zuletzt gespielt"))))
            .child(rect().width(Size::px(FACE + space::S)))
            .content(Content::flex());
        let rows = rect().width(Size::fill()).spacing(space::XXS).children(self.lines.iter().map(|l| rect().key(&l.id).width(Size::fill()).child(RowLine { line: l.clone() }).into_element()));
        rect().width(Size::fill()).spacing(space::XXS).child(head).child(hairline(&t)).child(rows)
    }
}

#[derive(PartialEq)]
struct RowLine {
    line: Line,
}

impl Component for RowLine {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let l = &self.line;
        let spot = use_spot(&format!("library.row.{}", l.id), "button", &l.name, true);
        let (open_id, menu_id) = (l.id.clone(), l.id.clone());
        let played = l.played.map(ago).unwrap_or_default();
        let shown = *hovered.read() || focus() != Focus::Not;
        let mut row = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(format!("{}, {}", l.name, l.what))
            .width(Size::fill())
            .height(Size::px(ROW))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .corner_radius(t.shape.control)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_all_press(move |ev: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*ev
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
                let mut page = store.ui.page;
                page.set(Page::Instance(open_id.clone()));
            })
            .on_secondary_down(move |ev: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*ev {
                    menu::open(store.ui, Menu::at(instance_items(store, &menu_id), (m.global_location.x as f32, m.global_location.y as f32)));
                }
            });
        if let Some(border) = ring(&t, focus()) {
            row = row.border(border);
        } else if shown {
            row = row.background(t.hover);
        }
        let mut name = rect().width(Size::flex(1.)).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex());
        name = name.child(InstanceIcon { id: l.id.clone(), name: l.name.clone(), picture: l.picture.clone(), side: FACE });
        name = name.child(rect().width(Size::flex(1.)).child(body(&t, l.name.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis)));
        if let Some(mark) = state_mark(&t, l) {
            name = name.child(mark);
        }
        let cell = |w: f32, s: String| rect().width(Size::px(w)).child(text_line(&t, s, font::CALLOUT, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        let action = if shown { Mini { id: l.id.clone(), name: l.name.clone(), spot: "library.row".into(), plain: true }.into_element() } else { rect().into_element() };
        row.child(name).child(cell(NARROW, l.game_version.clone())).child(cell(NARROW, l.loader.clone())).child(cell(WIDE, l.group.clone())).child(cell(WIDE, played)).child(rect().width(Size::px(FACE + space::S)).child(action)).content(Content::flex())
    }
}
