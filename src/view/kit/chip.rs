//! Chips: a removable or pickable token, such as a filter or a batch.

use freya::prelude::*;

use super::text::line;
use super::{ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{control, font, iconsize, space};

/// A capsule with a label that can be picked and, when it has `on_remove`, removed.
#[derive(Clone, PartialEq)]
pub struct Chip {
    id: String,
    label: String,
    on: bool,
    on_press: Option<EventHandler<()>>,
    on_remove: Option<EventHandler<()>>,
}

impl Chip {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), on: false, on_press: None, on_remove: None }
    }

    /// Shows the chip as picked.
    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    pub fn on_press(mut self, handler: impl FnMut(()) + 'static) -> Self {
        self.on_press = Some(EventHandler::new(handler));
        self
    }

    pub fn on_remove(mut self, handler: impl FnMut(()) + 'static) -> Self {
        self.on_remove = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Chip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let spot = use_spot(&self.id, "chip", &self.label, self.on_press.is_some() || self.on_remove.is_some());
        let ink = if self.on { t.accent_text } else { t.text1 };
        let fill = if self.on { t.selection } else { t.control };
        let clickable = self.on_press.is_some();
        let wash = if !clickable { None } else if *pressed.read() { Some(t.press) } else if *hovered.read() { Some(t.hover) } else { None };
        let press = self.on_press.clone();
        let remove = self.on_remove.clone();
        let keys = remove.clone();
        let focusable = clickable || remove.is_some();
        let mut c = rect()
            .a11y_id(a11y)
            .a11y_focusable(focusable)
            .a11y_role(if clickable { AccessibilityRole::Button } else { AccessibilityRole::GenericContainer })
            .a11y_alt(self.label.clone())
            .horizontal()
            .cross_align(Alignment::Center)
            .height(Size::px(control::S))
            .padding((0., if remove.is_some() { space::XS } else { space::S }, 0., space::S))
            .spacing(space::XS)
            .corner_radius(t.shape.chip)
            .cursor(if clickable { CursorIcon::Pointer } else { CursorIcon::Default })
            .on_sized(spot)
            .on_pointer_enter(move |_| {
                if clickable {
                    hovered.set(true);
                }
            })
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |e: Event<MouseEventData>| {
                if clickable && e.button == Some(MouseButton::Left) {
                    pressed.set(true);
                }
            })
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_all_press(move |e: Event<PressEventData>| {
                if !clickable {
                    return;
                }
                if let PressEventData::Mouse(m) = &*e
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
                a11y.request_focus();
                if let Some(h) = &press {
                    h.call(());
                }
            })
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if a11y.is_focused()
                    && matches!(e.key, Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::Delete))
                    && let Some(h) = &keys
                {
                    e.stop_propagation();
                    h.call(());
                }
            })
            .child(line(&t, self.label.clone(), font::CALLOUT, ink).font_weight(FontWeight::MEDIUM));
        if let Some(h) = remove {
            c = c.child(rect().padding(space::XXS).corner_radius(t.shape.small).cursor(CursorIcon::Pointer).on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                h.call(());
            }).child(icon(&t, Icon::Close, iconsize::XS, t.text2)));
        }
        if let Some(border) = ring(&t, focus()) {
            c = c.border(border);
        }
        match wash {
            Some(wash) => rect().corner_radius(t.shape.chip).background(fill).child(c.background(wash)).into_element(),
            None => c.background(fill).into_element(),
        }
    }
}
