//! The box of a checkbox, for rows that select themselves when pressed.

use freya::prelude::*;

use super::{ring, use_theme};
use crate::view::icons::{Icon, icon};
use crate::view::theme::{Theme, frame, iconsize};

/// A checkbox that Tab reaches and Space or Enter toggles; its row handles the pointer.
#[derive(Clone, PartialEq)]
pub struct Check {
    label: String,
    on: bool,
    on_toggle: EventHandler<()>,
}

impl Check {
    pub fn new(label: impl Into<String>, on: bool, on_toggle: impl Into<EventHandler<()>>) -> Self {
        Self { label: label.into(), on, on_toggle: on_toggle.into() }
    }
}

impl Component for Check {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let toggle = self.on_toggle.clone();
        let mut c = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::CheckBox)
            .a11y_alt(self.label.clone())
            .corner_radius(t.shape.small)
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if a11y.is_focused() && (e.key == Key::Named(NamedKey::Enter) || e.key == Key::Character(" ".into())) {
                    e.stop_propagation();
                    toggle.call(());
                }
            })
            .child(mark(&t, self.on));
        if let Some(border) = ring(&t, focus()) {
            c = c.border(border);
        }
        c
    }
}

/// The box of a checkbox: filled with a check while `on`, an outline otherwise.
pub fn mark(t: &Theme, on: bool) -> Element {
    let b = rect().width(Size::px(iconsize::M)).height(Size::px(iconsize::M)).center().corner_radius(t.shape.small);
    if on {
        b.background(t.accent).child(icon(t, Icon::Check, iconsize::XS, t.on_accent)).into_element()
    } else {
        b.border(Border::new().fill(t.text3).width(frame::BOX_EDGE).alignment(BorderAlignment::Inner)).into_element()
    }
}
