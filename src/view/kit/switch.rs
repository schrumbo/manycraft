//! A switch for settings that take effect at once.

use freya::animation::*;
use freya::prelude::*;

use super::text::para;
use super::tooltip::WithTip;
use super::{blend, ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::theme::{font, frame, motion, space};

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
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let on = self.on;
        let down = *pressed.read();
        let knob = frame::SWITCH_H - frame::SWITCH_INSET * 2.;
        let travel = frame::SWITCH_W - frame::SWITCH_INSET * 2. - knob;
        let slide = use_animation_with_dependencies(&on, |conf, on| {
            conf.on_creation(OnCreation::Finish);
            conf.on_change(OnChange::Rerun);
            let (from, to) = if *on { (0., 1.) } else { (1., 0.) };
            AnimNum::new(from, to).duration(motion::timed(motion::FAST)).ease(Ease::Out)
        });
        let grow = use_animation_with_dependencies(&down, |conf, down| {
            conf.on_creation(OnCreation::Finish);
            conf.on_change(OnChange::Rerun);
            let (from, to) = if *down { (0., 1.) } else { (1., 0.) };
            AnimNum::new(from, to).duration(motion::timed(motion::FAST)).ease(Ease::Out)
        });
        let at = slide.get().value();
        let stretch = frame::SWITCH_STRETCH * grow.get().value();
        let track = blend(t.text3.with_af32(frame::SWITCH_OFF), t.accent, at);
        let toggle = self.on_toggle.clone();
        let mut s = rect()
            .a11y_id(a11y)
            .a11y_focusable(!self.disabled)
            .a11y_role(AccessibilityRole::Switch)
            .a11y_alt(self.label.clone())
            .width(Size::px(frame::SWITCH_W))
            .height(Size::px(frame::SWITCH_H))
            .corner_radius(frame::SWITCH_H / 2.)
            .background(track)
            .opacity(if self.disabled { 0.45 } else { 1. })
            .on_sized(spot);
        let wash = if self.disabled { None } else if *pressed.read() { Some(t.press) } else if *hovered.read() { Some(t.hover) } else { None };
        if let Some(wash) = wash {
            s = s.child(rect().position(Position::new_absolute().left(0.).top(0.)).width(Size::px(frame::SWITCH_W)).height(Size::px(frame::SWITCH_H)).corner_radius(frame::SWITCH_H / 2.).background(wash));
        }
        s = s.child(
            rect()
                .position(Position::new_absolute().left(frame::SWITCH_INSET + (travel - stretch) * at).top(frame::SWITCH_INSET))
                .width(Size::px(knob + stretch))
                .height(Size::px(knob))
                .corner_radius(knob / 2.)
                .background(blend(t.pale, t.on_accent, at))
                .shadow(Shadow::from((0., 1., 2., 0., t.shade.with_af32(0.3)))),
        );
        if !self.disabled {
            s = s
                .cursor(CursorIcon::Pointer)
                .on_pointer_enter(move |_| hovered.set(true))
                .on_pointer_leave(move |_| {
                    hovered.set_if_modified(false);
                    pressed.set_if_modified(false);
                })
                .on_mouse_down(move |e: Event<MouseEventData>| {
                    if e.button == Some(MouseButton::Left) {
                        pressed.set(true);
                    }
                })
                .on_mouse_up(move |_| pressed.set_if_modified(false))
                .on_all_press(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
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

/// A switch with its text beside it; a click on the text flips it too, and a disabled one shows its reason as a tip.
#[derive(PartialEq)]
pub struct SwitchLabel {
    /// The drive id of the switch.
    pub id: &'static str,
    /// The text beside the switch, also its name for screen readers.
    pub text: String,
    /// Whether the switch is on.
    pub on: bool,
    /// Why the switch cannot be flipped now, if it cannot.
    pub disabled: Option<String>,
    /// Called with the new value when the person flips the switch or clicks the text.
    pub on_toggle: EventHandler<bool>,
}

impl Component for SwitchLabel {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let enabled = self.disabled.is_none();
        let (on, toggle) = (self.on, self.on_toggle.clone());
        let flip = toggle.clone();
        let ink = if !enabled { t.text3 } else if *hovered.read() { t.text1 } else { t.text2 };
        let mut text = para(&t, self.text.clone(), font::CALLOUT, ink).width(Size::fill()).cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed });
        if enabled {
            text = text.on_press(move |_| flip.call(!on));
        }
        let row = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .child(Switch::new(self.id, self.text.clone(), on).disabled(!enabled).on_toggle(move |on: bool| toggle.call(on)))
            .child(text);
        WithTip::new(self.disabled.clone(), row).fill()
    }
}
