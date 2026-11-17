//! A stepper: a number with minus and plus, changed by clicks or the arrow keys.

use freya::prelude::*;

use super::text::figure;
use super::tooltip::WithTip;
use super::{ring, use_theme};
use crate::core::i18n::tr;
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
    shown: Option<fn(i64) -> String>,
    disabled: Option<String>,
    on_change: Option<EventHandler<i64>>,
}

impl Stepper {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: i64, min: i64, max: i64) -> Self {
        Self { id: id.into(), label: label.into(), value, min, max, step: 1, unit: String::new(), shown: None, disabled: None, on_change: None }
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

    /// Writes the value with `shown` instead of as a number with its unit, such as "4 GB" for 4096.
    pub fn shown(mut self, shown: fn(i64) -> String) -> Self {
        self.shown = Some(shown);
        self
    }

    /// Greys the stepper out and ignores clicks and keys when `reason` is set; the reason shows as its tooltip.
    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.disabled = reason;
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
        let spot = use_spot(&self.id, "stepper", &self.label, self.disabled.is_none());
        let (value, min, max, step) = (self.value, self.min, self.max, self.step);
        let live = self.disabled.is_none();
        let change = self.on_change.clone();
        let set = move |v: i64| {
            let v = v.clamp(min, max);
            if v != value
                && let Some(h) = &change
            {
                h.call(v);
            }
        };
        let side = |glyph: Icon, enabled: bool, to: i64, tip: String| {
            let mut set = set.clone();
            Side { glyph, enabled, tip, on_press: EventHandler::new(move |_| {
                a11y.request_focus();
                set(to);
            }) }
        };
        let text = match self.shown {
            Some(shown) => shown(value),
            None if self.unit.is_empty() => value.to_string(),
            None => format!("{value} {}", self.unit),
        };
        let mut keys = set.clone();
        let mut s = rect()
            .a11y_id(a11y)
            .a11y_focusable(live)
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
                if !live || !a11y.is_focused() {
                    return;
                }
                match e.key {
                    Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowRight) => keys(value + step),
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowLeft) => keys(value - step),
                    _ => return,
                }
                e.stop_propagation();
            })
            .child(side(Icon::Minus, live && value > min, value - step, tr("Weniger")))
            .child(rect().width(Size::px(control::STEPPER_VALUE)).center().child(figure(&t, text, font::BODY, if live { t.text1 } else { t.text3 })))
            .child(side(Icon::Plus, live && value < max, value + step, tr("Mehr")));
        if let Some(border) = ring(&t, focus()) {
            s = s.border(border);
        }
        WithTip::new(self.disabled.clone(), s)
    }
}

#[derive(PartialEq)]
struct Side {
    glyph: Icon,
    enabled: bool,
    tip: String,
    on_press: EventHandler<()>,
}

impl Component for Side {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let press = self.on_press.clone();
        let mut b = rect()
            .width(Size::px(control::S))
            .height(Size::px(control::S))
            .center()
            .corner_radius(t.shape.small)
            .child(icon(&t, self.glyph, iconsize::S, if self.enabled { t.text1 } else { t.text3 }));
        if self.enabled {
            b = b
                .background(if *pressed.read() { t.press } else if *hovered.read() { t.hover } else { Color::TRANSPARENT })
                .cursor(CursorIcon::Pointer)
                .on_pointer_enter(move |_| hovered.set(true))
                .on_pointer_leave(move |_| {
                    hovered.set_if_modified(false);
                    pressed.set_if_modified(false);
                })
                .on_mouse_down(move |_| pressed.set(true))
                .on_mouse_up(move |_| pressed.set_if_modified(false))
                .on_press(move |_| press.call(()));
        }
        WithTip::new(self.enabled.then(|| self.tip.clone()), b)
    }
}
