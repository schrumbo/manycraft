//! A segmented control: a few mutually exclusive views or modes, changed with a click or the arrow keys.

use freya::prelude::*;

use super::text::line;
use super::tooltip::WithTip;
use super::{ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::theme::{control, font, space};

/// One segment: its drive id, its label and an optional tooltip, such as the shortcut.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub id: String,
    pub label: String,
    pub tip: Option<String>,
}

/// A row of segments with one chosen.
#[derive(Clone, PartialEq)]
pub struct Segmented {
    label: String,
    items: Vec<Segment>,
    chosen: usize,
    disabled: Option<String>,
    on_choose: Option<EventHandler<usize>>,
}

impl Segmented {
    /// Segments named for screen readers by `label`.
    pub fn new(label: impl Into<String>, items: Vec<Segment>, chosen: usize) -> Self {
        Self { label: label.into(), items, chosen, disabled: None, on_choose: None }
    }

    /// Greys the segments out and shows `reason` as the tooltip of the whole control.
    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.disabled = reason;
        self
    }

    pub fn on_choose(mut self, handler: impl FnMut(usize) + 'static) -> Self {
        self.on_choose = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Segmented {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        super::use_side_arrows(a11y);
        let focus = use_focus(a11y);
        let inset = space::XXS;
        let count = self.items.len();
        let chosen = self.chosen;
        let enabled = self.disabled.is_none();
        let choose = self.on_choose.clone().filter(|_| enabled);
        let keys = choose.clone();
        let segments = self.items.iter().enumerate().map(|(i, item)| {
            let pick = choose.clone();
            Piece { item: item.clone(), on: i == chosen, enabled, on_press: EventHandler::new(move |_| {
                a11y.request_focus();
                if let Some(h) = &pick {
                    h.call(i);
                }
            }) }
            .into_element()
        });
        let mut s = rect()
            .a11y_id(a11y)
            .a11y_focusable(enabled)
            .a11y_role(AccessibilityRole::RadioGroup)
            .a11y_alt(self.label.clone())
            .horizontal()
            .padding(inset)
            .spacing(space::XXS)
            .background(t.control)
            .corner_radius(t.shape.control)
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !a11y.is_focused() || count == 0 {
                    return;
                }
                let next = match e.key {
                    Key::Named(NamedKey::ArrowRight) => (chosen + 1).min(count - 1),
                    Key::Named(NamedKey::ArrowLeft) => chosen.saturating_sub(1),
                    _ => return,
                };
                e.stop_propagation();
                if next != chosen
                    && let Some(h) = &keys
                {
                    h.call(next);
                }
            })
            .children(segments);
        if let Some(border) = ring(&t, focus()) {
            s = s.border(border);
        }
        WithTip::new(self.disabled.clone(), s)
    }
}

#[derive(Clone, PartialEq)]
struct Piece {
    item: Segment,
    on: bool,
    enabled: bool,
    on_press: EventHandler<()>,
}

impl Component for Piece {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let spot = use_spot(&self.item.id, "segment", &self.item.label, self.enabled);
        let press = self.on_press.clone();
        let inset = space::XXS;
        let mut p = rect()
            .height(Size::px(control::M - inset * 2.))
            .padding((0., space::M))
            .center()
            .corner_radius((t.shape.control - inset).max(0.))
            .cursor(if self.enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed })
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |_| pressed.set(true))
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_press(move |_| press.call(()))
            .child(line(&t, self.item.label.clone(), font::CALLOUT, if !self.enabled { t.text3 } else if self.on { t.text1 } else { t.text2 }).font_weight(if self.on { FontWeight::SEMI_BOLD } else { FontWeight::MEDIUM }));
        if self.on {
            p = p.background(if t.dark { t.separator } else { t.surface }).shadow(Shadow::from((0., 1., 2., 0., Color::BLACK.with_af32(if t.dark { 0.35 } else { 0.12 }))));
        } else if *pressed.read() && self.enabled {
            p = p.background(t.press);
        } else if *hovered.read() && self.enabled {
            p = p.background(t.hover);
        }
        WithTip::new(self.item.tip.clone(), p)
    }
}
