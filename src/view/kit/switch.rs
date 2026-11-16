//! A switch for settings that take effect at once.

use freya::animation::*;
use freya::prelude::*;

use super::{ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::theme::{frame, motion, space};

/// An on/off switch named `id`, described by `label` for screen readers and the drive tool.
#[derive(Clone, PartialEq)]
pub struct Switch {
    id: String,
    label: String,
    on: bool,
    disabled: bool,
    on_toggle: Option<EventHandler<bool>>,
}

impl Switch {
    pub fn new(id: impl Into<String>, label: impl Into<String>, on: bool) -> Self {
        Self { id: id.into(), label: label.into(), on, disabled: false, on_toggle: None }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Called with the new value when the person flips the switch.
    pub fn on_toggle(mut self, handler: impl FnMut(bool) + 'static) -> Self {
        self.on_toggle = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Switch {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let spot = use_spot(&self.id, "switch", &self.label, !self.disabled);
        let on = self.on;
        let knob = frame::SWITCH_H - space::XXS * 2.;
        let travel = frame::SWITCH_W - frame::SWITCH_H;
        let slide = use_animation_with_dependencies(&on, |conf, on| {
            conf.on_creation(OnCreation::Finish);
            conf.on_change(OnChange::Rerun);
            let (from, to) = if *on { (0., 1.) } else { (1., 0.) };
            AnimNum::new(from, to).duration(motion::timed(motion::FAST)).ease(Ease::Out)
        });
        let at = slide.get().value();
        let track = if on { t.accent } else { t.separator };
        let toggle = self.on_toggle.clone();
        let mut s = rect()
            .a11y_id(a11y)
            .a11y_focusable(!self.disabled)
            .a11y_role(AccessibilityRole::Switch)
            .a11y_alt(self.label.clone())
            .width(Size::px(frame::SWITCH_W))
            .height(Size::px(frame::SWITCH_H))
            .padding(space::XXS)
            .corner_radius(frame::SWITCH_H / 2.)
            .background(track)
            .opacity(if self.disabled { 0.45 } else { 1. })
            .on_sized(spot)
            .child(
                rect()
                    .position(Position::new_absolute().left(space::XXS + travel * at).top(space::XXS))
                    .width(Size::px(knob))
                    .height(Size::px(knob))
                    .corner_radius(knob / 2.)
                    .background(Color::WHITE)
                    .shadow(Shadow::from((0., 1., 2., 0., Color::BLACK.with_af32(0.25)))),
            );
        if !self.disabled {
            s = s.cursor(CursorIcon::Pointer).on_all_press(move |_| {
                a11y.request_focus();
                if let Some(h) = &toggle {
                    h.call(!on);
                }
            });
        }
        if let Some(border) = ring(&t, focus()) {
            s = s.border(border);
        }
        s
    }
}
