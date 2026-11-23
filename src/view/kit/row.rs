//! List rows: leading glyph, title, optional detail and trailing content, with hover, selection and focus.

use freya::prelude::*;

use super::badge::corner;
use super::text::{caption, line};
use super::{ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{font, frame, iconsize, space};

/// One row of a list or sidebar.
#[derive(PartialEq)]
pub struct Row {
    id: String,
    title: String,
    detail: Option<String>,
    glyph: Option<Icon>,
    leading: Option<Element>,
    trailing: Option<Element>,
    reveal: Option<Element>,
    selected: bool,
    disabled: bool,
    compact: Option<Option<Element>>,
    height: f32,
    on_press: Option<EventHandler<()>>,
    on_secondary: Option<EventHandler<(f32, f32)>>,
}

impl Row {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self { id: id.into(), title: title.into(), detail: None, glyph: None, leading: None, trailing: None, reveal: None, selected: false, disabled: false, compact: None, height: frame::ROW, on_press: None, on_secondary: None }
    }

    /// A second, quieter line under the title.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self.height = self.height.max(frame::ROW + font::CAPTION + space::XS);
        self
    }

    pub fn glyph(mut self, glyph: Icon) -> Self {
        self.glyph = Some(glyph);
        self
    }

    /// Content in front of the title in place of a glyph, such as a face.
    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_element());
        self
    }

    /// Content at the end of the row, such as a count.
    pub fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing = Some(trailing.into_element());
        self
    }

    /// Content after the trailing part that shows only while the pointer rests on the row, it is selected or has the keyboard focus, such as its actions.
    pub fn reveal(mut self, reveal: impl IntoElement) -> Self {
        self.reveal = Some(reveal.into_element());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Shows only the glyph or leading content, centred, with `mark` over its corner: the row of a folded sidebar.
    pub fn compact(mut self, on: bool, mark: Option<Element>) -> Self {
        self.compact = on.then_some(mark);
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn on_press(mut self, handler: impl FnMut(()) + 'static) -> Self {
        self.on_press = Some(EventHandler::new(handler));
        self
    }

    /// Called with the pointer position on a right click, to open a context menu there.
    pub fn on_secondary(mut self, handler: impl FnMut((f32, f32)) + 'static) -> Self {
        self.on_secondary = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Row {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let spot = use_spot(&self.id, "row", &self.title, !self.disabled);
        let disabled = self.disabled;
        let press = (!disabled).then(|| self.on_press.clone()).flatten();
        let secondary = (!disabled).then(|| self.on_secondary.clone()).flatten();
        let fill = if self.selected { t.selection } else if !disabled && *hovered.read() { t.hover } else { Color::TRANSPARENT };
        let ink = if self.selected { t.accent_text } else if disabled { t.text3 } else { t.text1 };
        let mut r = rect()
            .a11y_id(a11y)
            .a11y_focusable(!disabled)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(self.title.clone())
            .width(Size::fill())
            .height(Size::px(self.height))
            .padding((0., if self.compact.is_some() { 0. } else { space::S }))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.small + 2.)
            .background(fill)
            .opacity(if disabled { 0.55 } else { 1. })
            .on_sized(spot)
            .cursor(if press.is_some() { CursorIcon::Pointer } else { CursorIcon::Default })
            .on_pointer_over(move |_| {
                if !disabled {
                    hovered.set(true);
                }
            })
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_all_press(move |e: Event<PressEventData>| {
                if disabled {
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
            });
        if let Some(h) = secondary {
            r = r.on_secondary_down(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e {
                    h.call((m.global_location.x as f32, m.global_location.y as f32));
                }
            });
        }
        if let Some(border) = ring(&t, focus()) {
            r = r.border(border);
        }
        let mut lead = None;
        if let Some(l) = &self.leading {
            lead = Some(l.clone());
        } else if let Some(g) = self.glyph {
            lead = Some(icon(&t, g, iconsize::S, if self.selected { t.accent_text } else { t.text2 }));
        }
        if let Some(mark) = &self.compact {
            let face = rect().width(Size::px(frame::FACE)).height(Size::px(frame::FACE)).center().children(lead);
            let face = match mark {
                Some(m) => corner(face, frame::FACE, frame::FACE, m.clone(), -space::XXS),
                None => face.into_element(),
            };
            return r.main_align(Alignment::Center).child(face);
        }
        r = r.children(lead);
        let mut text = rect().width(Size::flex(1.)).spacing(1.).child(line(&t, self.title.clone(), font::BODY, ink).font_weight(if self.selected { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL }));
        if let Some(d) = &self.detail {
            text = text.child(caption(&t, d.clone()));
        }
        r = r.child(text).content(Content::flex());
        if let Some(tr) = &self.trailing {
            r = r.child(tr.clone());
        }
        if let Some(reveal) = self.reveal.as_ref().filter(|_| self.selected || *hovered.read() || focus() == Focus::Keyboard) {
            r = r.child(reveal.clone());
        }
        r
    }
}
