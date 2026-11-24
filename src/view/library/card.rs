use freya::prelude::*;

use super::Line;
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::view::drive::use_spot;
use crate::view::icons::Icon;
use crate::view::install::{self, Install, Stage};
use crate::view::instance::{InstanceIcon, Mini};
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::progress::Wait;
use crate::view::kit::text::{body, caption};
use crate::view::kit::{ring, use_theme};
use crate::view::shell::instance_items;
use crate::view::store::use_store;
use crate::view::theme::{frame, iconsize, space};
use crate::view::ui::Page;

/// The width of one instance card.
pub const CARD_W: f32 = 168.;
const PICTURE: f32 = 96.;

/// A wrapped run of instance cards.
#[derive(PartialEq)]
pub struct Wall {
    pub lines: Vec<Line>,
}

impl Component for Wall {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        rect().width(Size::fill()).horizontal().spacing(t.grid_gap).content(Content::wrap_spacing(t.grid_gap)).children(self.lines.iter().map(|l| rect().key(&l.id).child(Card { line: l.clone() }).into_element()))
    }
}

/// One instance as a card: its picture, name, what it plays and whether it runs; hovering offers play or stop.
#[derive(PartialEq)]
pub struct Card {
    pub line: Line,
}

impl Component for Card {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let l = &self.line;
        let spot = use_spot(&format!("library.card.{}", l.id), "button", &l.name, true);
        let (open_id, menu_id) = (l.id.clone(), l.id.clone());
        let mut card = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(format!("{}, {}", l.name, l.what))
            .width(Size::px(CARD_W))
            .padding(space::M)
            .spacing(space::S)
            .cross_align(Alignment::Center)
            .background(t.raised)
            .corner_radius(t.shape.tile)
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
        if let Some(shadow) = t.elevation.card.clone() {
            card = card.shadow(shadow);
        }
        if let Some(border) = ring(&t, focus()) {
            card = card.border(border);
        } else if *hovered.read() {
            card = card.border(Border::new().fill(t.tile_ring).width(1.).alignment(BorderAlignment::Inner));
        }
        let mut picture = rect().width(Size::px(PICTURE)).height(Size::px(PICTURE)).child(InstanceIcon { id: l.id.clone(), name: l.name.clone(), picture: l.picture.clone(), side: PICTURE });
        if *hovered.read() || focus() != Focus::Not {
            let play = Mini { id: l.id.clone(), name: l.name.clone(), spot: "library.card".into(), plain: false };
            picture = picture.child(rect().position(Position::new_absolute().right(space::XS).bottom(space::XS)).child(play));
        }
        let mut name = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::XS).main_align(Alignment::Center);
        if let Some(mark) = state_mark(&t, l) {
            name = name.child(mark);
        }
        name = name.child(body(&t, l.name.clone()).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        let card = card.child(picture).child(name);
        match &l.meter {
            Some(meter) => card.child(Wait::new(format!("library.card.{}.progress", l.id), meter.clone()).compact()),
            None => card.child(caption(&t, l.what.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis)),
        }
    }
}

/// The dot or ring in front of a name: loading, running or needing the person.
pub fn state_mark(t: &crate::view::theme::Theme, l: &Line) -> Option<Element> {
    if let Some(p) = l.loading {
        return Some(install::ring(t, &Stage::Running(Some(p)), iconsize::XS));
    }
    if l.trouble {
        return Some(dot(t.attention));
    }
    l.running.is_some().then(|| dot(t.success))
}

/// Instances that are still arriving, one ghost card each, with a ring and their progress.
#[derive(PartialEq)]
pub struct Arriving {
    pub entries: Vec<Install>,
}

impl Component for Arriving {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        rect().width(Size::fill()).horizontal().spacing(t.grid_gap).content(Content::wrap_spacing(t.grid_gap)).children(self.entries.iter().map(|e| rect().key(e.id.to_string()).child(Ghost { entry: e.clone() }).into_element()))
    }
}

#[derive(PartialEq)]
struct Ghost {
    entry: Install,
}

impl Component for Ghost {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let e = &self.entry;
        let id = e.id;
        let failed = matches!(e.stage, Stage::Failed(_));
        let spot = use_spot(&format!("library.arriving.{id}"), "group", &e.name, true);
        let side = rect().width(Size::px(PICTURE)).height(Size::px(PICTURE)).center().corner_radius(t.shape.tile).background(t.accent.with_af32(frame::TILE_WASH)).child(install::ring(&t, &e.stage, iconsize::HERO));
        let card = rect()
            .a11y_role(AccessibilityRole::Group)
            .a11y_alt(format!("{}, {}", e.name, e.state()))
            .width(Size::px(CARD_W))
            .padding(space::M)
            .spacing(space::S)
            .cross_align(Alignment::Center)
            .corner_radius(t.shape.tile)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
            .on_sized(spot)
            .on_secondary_down(move |ev: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*ev {
                    menu::open(store.ui, Menu::at(vec![Item::new(format!("library.arriving.{id}.cancel"), tr("Installation abbrechen"), move |_| install::cancel(store, id))], (m.global_location.x as f32, m.global_location.y as f32)));
                }
            })
            .child(side)
            .child(body(&t, e.name.clone()).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        let card = if matches!(e.stage, Stage::Running(_)) && !e.stopping() {
            card.child(rect().width(Size::fill()).height(Size::px(frame::GHOST_WAIT)).child(Wait::new(format!("library.arriving.{id}.progress"), e.meter.clone())))
        } else {
            card.child(rect().width(Size::fill()).height(Size::px(frame::GHOST_WAIT)).child(caption(&t, e.state()).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::fill())))
        };
        if failed {
            card.child(
                rect()
                    .horizontal()
                    .spacing(space::XS)
                    .child(Button::new(format!("library.arriving.{id}.close"), tr("Schließen")).on_press(move |_| install::close(store, id)))
                    .child(Button::new(format!("library.arriving.{id}.retry"), tr("Erneut")).primary().on_press(move |_| install::retry(store, id))),
            )
        } else {
            card.child(Button::new(format!("library.arriving.{id}.cancel"), tr("Abbrechen")).disabled(e.stopping().then(|| tr("Bricht ab …"))).on_press(move |_| install::cancel(store, id)))
        }
    }
}