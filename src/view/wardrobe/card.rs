use freya::elements::image::ImageHandle;
use freya::prelude::*;

use crate::view::drive::use_spot;
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{ring, use_theme};
use crate::view::theme::{font, space};

const PICK: f32 = 2.;
const DIM: f32 = 0.45;

/// A picture drawn pixel for pixel, never smoothed.
pub fn pixels(handle: ImageHandle, w: f32, h: f32) -> Element {
    image(handle).sampling_mode(SamplingMode::Nearest).aspect_ratio(AspectRatio::None).width(Size::px(w)).height(Size::px(h)).into_element()
}

/// `items` in rows of `per_row`.
pub fn rows(items: Vec<Element>, per_row: usize, gap: f32) -> Element {
    let mut column = rect().spacing(gap);
    for chunk in items.chunks(per_row.max(1)) {
        column = column.child(rect().horizontal().spacing(gap).children(chunk.iter().cloned()));
    }
    column.into_element()
}

/// A skin or cape to put on: a picture with a caption, ringed in the accent while worn.
#[derive(Clone, PartialEq)]
pub struct Card {
    pub id: String,
    pub label: String,
    pub caption: Option<String>,
    pub detail: Option<String>,
    pub picture: Element,
    pub worn: bool,
    pub enabled: bool,
    pub busy: bool,
    pub width: f32,
    pub on_press: EventHandler<()>,
    pub on_secondary: Option<EventHandler<(f32, f32)>>,
}

impl Component for Card {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let spot = use_spot(&self.id, "option", &self.label, self.enabled);
        let press = self.on_press.clone();
        let secondary = self.on_secondary.clone();
        let enabled = self.enabled && !self.busy;
        let edge = Border::new().fill(if self.worn { t.accent } else { Color::TRANSPARENT }).width(PICK).alignment(BorderAlignment::Inner);
        let mut inner = rect()
            .width(Size::fill())
            .padding(space::S)
            .corner_radius(t.shape.control)
            .border(edge)
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .background(if enabled && *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .child(self.picture.clone());
        if let Some(c) = &self.caption {
            inner = inner.child(line(&t, c.clone(), font::CALLOUT, if self.worn { t.text1 } else { t.text2 }).width(Size::fill()).text_align(TextAlign::Center));
        }
        if let Some(d) = &self.detail {
            inner = inner.child(line(&t, d.clone(), font::CAPTION, t.text3).width(Size::fill()).text_align(TextAlign::Center));
        }
        let mut outer = rect()
            .a11y_id(a11y)
            .a11y_focusable(self.enabled)
            .a11y_role(AccessibilityRole::RadioButton)
            .a11y_alt(self.label.clone())
            .width(Size::px(self.width))
            .background(t.raised)
            .corner_radius(t.shape.control)
            .opacity(if self.enabled { if self.busy { 0.7 } else { 1. } } else { DIM })
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed })
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .child(inner);
        if enabled {
            outer = outer.on_all_press(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
                a11y.request_focus();
                press.call(());
            });
        }
        if let Some(h) = secondary {
            outer = outer.on_secondary_down(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e {
                    h.call((m.global_location.x as f32, m.global_location.y as f32));
                }
            });
        }
        if let Some(border) = ring(&t, focus()) {
            outer = outer.border(border);
        }
        WithTip::new(Some(self.label.clone()), outer)
    }
}
