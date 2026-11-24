use freya::elements::image::ImageHandle;
use freya::prelude::*;

use crate::view::drive::use_spot;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::spinner;
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{ring, use_theme};
use crate::view::theme::{font, level, space};

const PICK: f32 = 2.;

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

/// The way to take a card away: what it does, and why it cannot now.
#[derive(Clone, PartialEq)]
pub struct Removal {
    pub label: String,
    pub on: EventHandler<()>,
    pub why_not: Option<String>,
}

/// A skin or cape to put on: a picture with a caption, ringed in the accent while worn. A change on its way shows on the
/// card alone, as a quiet line in place of the detail; a refused one as `failed`, with the reason as the tooltip.
#[derive(Clone, PartialEq)]
pub struct Card {
    pub id: String,
    pub label: String,
    pub caption: Option<String>,
    pub detail: Option<String>,
    pub picture: Element,
    pub worn: bool,
    pub enabled: bool,
    pub pending: Option<String>,
    pub failed: Option<(String, String)>,
    pub width: f32,
    pub on_press: EventHandler<()>,
    pub on_secondary: Option<EventHandler<(f32, f32)>>,
    pub remove: Option<Removal>,
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
        let enabled = self.enabled;
        let keyboard = focus() == Focus::Keyboard;
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
        if let Some(p) = &self.pending {
            inner = inner.child(rect().width(Size::fill()).horizontal().main_align(Alignment::Center).child(spinner::waiting(&t, p.clone())));
        } else if let Some((short, _)) = &self.failed {
            inner = inner.child(line(&t, short.clone(), font::CAPTION, t.danger_text).width(Size::fill()).text_align(TextAlign::Center));
        } else if let Some(d) = &self.detail {
            inner = inner.child(line(&t, d.clone(), font::CAPTION, t.text3).width(Size::fill()).text_align(TextAlign::Center));
        }
        let remove = self.remove.clone();
        let keys = remove.clone();
        let mut outer = rect()
            .a11y_id(a11y)
            .a11y_focusable(self.enabled)
            .a11y_role(AccessibilityRole::RadioButton)
            .a11y_alt(self.label.clone())
            .width(Size::px(self.width))
            .background(t.raised)
            .corner_radius(t.shape.control)
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::Default })
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set_if_modified(true))
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
        outer = outer.on_key_down(move |e: Event<KeyboardEventData>| {
            if a11y.is_focused()
                && matches!(e.key, Key::Named(NamedKey::Delete | NamedKey::Backspace))
                && let Some(r) = &keys
            {
                e.stop_propagation();
                r.on.call(());
            }
        });
        if let Some(r) = remove
            && (*hovered.read() || keyboard)
        {
            let button = Button::icon(format!("{}.remove", self.id), Icon::Trash, r.label).disabled(r.why_not).not_focusable().on_press(move |_| r.on.call(()));
            outer = outer.child(rect().position(Position::new_absolute().top(space::XS).right(space::XS)).layer(Layer::Relative(level::FLOAT)).child(button));
        }
        if let Some(border) = ring(&t, focus()) {
            outer = outer.border(border);
        }
        WithTip::new(Some(self.failed.as_ref().map_or_else(|| self.label.clone(), |(_, why)| format!("{}: {why}", self.label))), outer)
    }
}
