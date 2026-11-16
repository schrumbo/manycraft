//! A stepper: a number with minus and plus, changed by clicks or the arrow keys.

use freya::prelude::*;

use super::text::figure;
use super::{ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{control, font, iconsize, space};

/// A whole number between `min` and `max`, shown with `unit`.
#[derive(Clone, PartialEq)]
pub struct Stepper {
    id: String,
    label: String,
    value: i64,
    min: i64,
    max: i64,
    step: i64,
    unit: String,
    on_change: Option<EventHandler<i64>>,
}

impl Stepper {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: i64, min: i64, max: i64) -> Self {
        Self { id: id.into(), label: label.into(), value, min, max, step: 1, unit: String::new(), on_change: None }
    }

    pub fn step(mut self, step: i64) -> Self {
        self.step = step.max(1);
        self
    }

    /// Text after the number, such as "GB".
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    pub fn on_change(mut self, handler: impl FnMut(i64) + 'static) -> Self {
        self.on_change = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Stepper {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let spot = use_spot(&self.id, "stepper", &self.label, true);
        let (value, min, max, step) = (self.value, self.min, self.max, self.step);
        let change = self.on_change.clone();
        let set = move |v: i64| {
            let v = v.clamp(min, max);
            if v != value
                && let Some(h) = &change
            {
                h.call(v);
            }
        };
        let side = |glyph: Icon, enabled: bool, to: i64| {
            let mut set = set.clone();
            let mut b = rect()
                .width(Size::px(control::S))
                .height(Size::px(control::S))
                .center()
                .corner_radius(t.shape.small)
                .child(icon(&t, glyph, iconsize::S, if enabled { t.text1 } else { t.text3 }));
            if enabled {
                b = b.cursor(CursorIcon::Pointer).on_press(move |_| {
                    a11y.request_focus();
                    set(to);
                });
            }
            b
        };
        let text = if self.unit.is_empty() { value.to_string() } else { format!("{value} {}", self.unit) };
        let mut keys = set.clone();
        let mut s = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::SpinButton)
            .a11y_alt(format!("{}: {text}", self.label))
            .horizontal()
            .cross_align(Alignment::Center)
            .height(Size::px(control::M))
            .padding((0., space::XXS))
            .spacing(space::XXS)
            .background(t.control)
            .corner_radius(t.shape.control)
            .on_sized(spot)
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !a11y.is_focused() {
                    return;
                }
                match e.key {
                    Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowRight) => keys(value + step),
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowLeft) => keys(value - step),
                    _ => return,
                }
                e.stop_propagation();
            })
            .child(side(Icon::Minus, value > min, value - step))
            .child(rect().width(Size::px(control::STEPPER_VALUE)).center().child(figure(&t, text, font::BODY, t.text1)))
            .child(side(Icon::Plus, value < max, value + step));
        if let Some(border) = ring(&t, focus()) {
            s = s.border(border);
        }
        s
    }
}
