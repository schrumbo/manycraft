//! Buttons in four looks: the one primary action, secondary actions, plain text actions and destructive ones.

use freya::prelude::*;

use super::spinner::Spinner;
use super::text::line;
use super::tooltip::WithTip;
use super::{ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{Theme, control, font, iconsize, space};

/// How a button looks, by the weight of its action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Primary,
    Secondary,
    Plain,
    Danger,
    /// Plain, in the danger color: a destructive action in a list of plain ones.
    PlainDanger,
}

/// A button with a label, an optional icon in front and an optional tooltip; disabled with the reason as its tooltip.
#[derive(Clone, PartialEq)]
pub struct Button {
    id: String,
    label: String,
    glyph: Option<Icon>,
    busy: bool,
    look: Look,
    tall: bool,
    icon_only: bool,
    tip: Option<String>,
    disabled: Option<String>,
    on_press: Option<EventHandler<()>>,
}

impl Button {
    /// A button named `id` for the drive tool, showing `label`.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), glyph: None, busy: false, look: Look::Secondary, tall: false, icon_only: false, tip: None, disabled: None, on_press: None }
    }

    /// A square button that shows only `glyph`; `label` becomes its tooltip and accessible name.
    pub fn icon(id: impl Into<String>, glyph: Icon, label: impl Into<String>) -> Self {
        let label = label.into();
        Self { tip: Some(label.clone()), glyph: Some(glyph), icon_only: true, look: Look::Plain, ..Self::new(id, label) }
    }

    pub fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    pub fn primary(self) -> Self {
        self.look(Look::Primary)
    }

    pub fn plain(self) -> Self {
        self.look(Look::Plain)
    }

    /// The danger look; a plain button stays plain and only takes the danger color.
    pub fn danger(self) -> Self {
        let look = if self.look == Look::Plain { Look::PlainDanger } else { Look::Danger };
        self.look(look)
    }

    pub fn glyph(mut self, glyph: Icon) -> Self {
        self.glyph = Some(glyph);
        self
    }

    /// Shows the turning arc in place of the glyph while a short wait runs.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// The prominent height, for the main action of an empty state or a sheet.
    pub fn tall(mut self) -> Self {
        self.tall = true;
        self
    }

    pub fn tip(mut self, tip: impl Into<String>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    /// Disables the button when `reason` is set; the reason shows as its tooltip.
    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.disabled = reason;
        self
    }

    pub fn on_press(mut self, handler: impl FnMut(()) + 'static) -> Self {
        self.on_press = Some(EventHandler::new(handler));
        self
    }
}

fn colors(t: &Theme, look: Look, enabled: bool) -> (Color, Color) {
    if !enabled {
        return (if matches!(look, Look::Plain | Look::PlainDanger) { Color::TRANSPARENT } else { t.control }, t.text3);
    }
    match look {
        Look::Primary => (t.accent, t.on_accent),
        Look::Secondary => (t.control, t.text1),
        Look::Plain => (Color::TRANSPARENT, t.accent_text),
        Look::Danger => (t.danger, t.on_danger),
        Look::PlainDanger => (Color::TRANSPARENT, t.danger_text),
    }
}

impl Component for Button {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let enabled = self.disabled.is_none();
        let spot = use_spot(&self.id, "button", &self.label, enabled);
        let (fill, ink) = colors(&t, self.look, enabled);
        let ink = if self.icon_only && enabled && self.look == Look::Plain { t.text2 } else { ink };
        let h = if self.tall { control::L } else { control::M };
        let wash = if !enabled {
            None
        } else if *pressed.read() {
            Some(t.press)
        } else if *hovered.read() {
            Some(t.hover)
        } else {
            None
        };
        let on_press = self.on_press.clone();
        let mut b = rect()
            .a11y_id(a11y)
            .a11y_focusable(enabled)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(match &self.disabled {
                Some(reason) => format!("{}: {reason}", self.label),
                None => self.label.clone(),
            })
            .height(Size::px(h))
            .horizontal()
            .center()
            .spacing(space::XS + space::XXS)
            .background(fill)
            .corner_radius(t.shape.control)
            .on_sized(spot)
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            });
        if self.icon_only {
            b = b.width(Size::px(h));
        } else {
            b = b.padding((0., if matches!(self.look, Look::Plain | Look::PlainDanger) { space::S } else { space::M }));
        }
        if enabled {
            b = b.on_mouse_down(move |_| pressed.set(true)).on_mouse_up(move |_| pressed.set_if_modified(false)).on_all_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                a11y.request_focus();
                if let Some(h) = &on_press {
                    h.call(());
                }
            });
        } else {
            b = b.on_pointer_press(|e: Event<PointerEventData>| e.stop_propagation());
        }
        if let Some(border) = ring(&t, focus()) {
            b = b.border(border);
        }
        if self.busy {
            b = b.child(Spinner);
        } else if let Some(g) = self.glyph {
            b = b.child(icon(&t, g, if self.icon_only { iconsize::M } else { iconsize::S }, ink));
        }
        if !self.icon_only {
            let weight = if self.look == Look::Primary || self.look == Look::Danger { FontWeight::SEMI_BOLD } else { FontWeight::MEDIUM };
            b = b.child(line(&t, self.label.clone(), font::BODY, ink).font_weight(weight));
        }
        let b = match wash {
            Some(wash) => rect().corner_radius(t.shape.control).background(fill).child(b.background(wash)).into_element(),
            None => b.into_element(),
        };
        WithTip::new(self.disabled.clone().or_else(|| self.tip.clone()), b)
    }
}
