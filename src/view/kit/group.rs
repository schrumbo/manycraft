//! Grouped rows as in system settings: a caption, rows on a raised card divided by hairlines, and a quiet footer.

use freya::prelude::*;

use super::text::{line, para};
use super::use_theme;
use crate::view::drive::use_spot;
use crate::view::theme::{Theme, control, font, space};

/// A group of `rows` under `title`, with `footer` below the card.
pub fn group(t: &Theme, title: Option<String>, rows: Vec<Element>, footer: Option<String>) -> Element {
    let count = rows.len();
    let mut card = rect().width(Size::fill()).background(t.raised).corner_radius(t.shape.tile).border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner)).overflow(Overflow::Clip);
    for (i, r) in rows.into_iter().enumerate() {
        card = card.child(r);
        if i + 1 < count {
            card = card.child(rect().width(Size::fill()).padding((0., 0., 0., space::L)).child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator)));
        }
    }
    let mut column = rect().width(Size::fill()).spacing(space::S);
    if let Some(title) = title {
        column = column.child(rect().padding((0., space::XS)).child(line(t, title, font::CALLOUT, t.text2).font_weight(FontWeight::SEMI_BOLD)));
    }
    column = column.child(card);
    if let Some(f) = footer {
        column = column.child(rect().width(Size::fill()).padding((0., space::XS)).child(para(t, f, font::CAPTION, t.text3)));
    }
    column.into_element()
}

/// One row of a group: a label with an optional hint on the left, a control on the right and content below.
#[derive(PartialEq)]
pub struct Line {
    id: String,
    label: String,
    hint: Option<String>,
    hint_tone: Option<Color>,
    control: Option<Element>,
    below: Option<Element>,
    dim: bool,
    on_press: Option<EventHandler<()>>,
}

impl Line {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), hint: None, hint_tone: None, control: None, below: None, dim: false, on_press: None }
    }

    /// A quieter line under the label that says what the setting does.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Draws the hint in `color`, for a status such as a warning.
    pub fn hint_tone(mut self, color: Color) -> Self {
        self.hint_tone = Some(color);
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_element());
        self
    }

    /// Content across the row below the label, such as a field or a status line.
    pub fn below(mut self, below: impl IntoElement) -> Self {
        self.below = Some(below.into_element());
        self
    }

    /// Greys the label out, for a row that depends on a switch that is off.
    pub fn dim(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }

    /// Makes the whole row a target, such as flipping its switch; the control keeps its own clicks.
    pub fn on_press(mut self, handler: impl FnMut(()) + 'static) -> Self {
        self.on_press = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Line {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let spot = use_spot(&self.id, "line", &self.label, !self.dim);
        let press = self.on_press.clone();
        let mut text = rect().width(Size::flex(1.)).spacing(space::XXS).child(line(&t, self.label.clone(), font::BODY, if self.dim { t.text3 } else { t.text1 }).max_lines(2));
        if let Some(h) = &self.hint {
            text = text.child(para(&t, h.clone(), font::CALLOUT, self.hint_tone.unwrap_or(if self.dim { t.text3 } else { t.text2 })));
        }
        let mut top = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::L).child(text).content(Content::flex());
        if let Some(c) = &self.control {
            let mut held = rect().on_pointer_press(|e: Event<PointerEventData>| e.stop_propagation());
            if press.is_some() {
                held = held.on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation());
            }
            top = top.child(held.child(c.clone()));
        }
        let mut r = rect().width(Size::fill()).min_height(Size::px(control::L + space::M)).padding((space::M, space::L)).main_align(Alignment::Center).spacing(space::S).on_sized(spot).child(top);
        if let Some(b) = &self.below {
            r = r.child(b.clone());
        }
        if let Some(p) = press {
            r = r
                .background(if *pressed.read() { t.press } else if *hovered.read() { t.hover } else { Color::TRANSPARENT })
                .cursor(CursorIcon::Pointer)
                .on_pointer_enter(move |_| hovered.set(true))
                .on_pointer_leave(move |_| {
                    hovered.set_if_modified(false);
                    pressed.set_if_modified(false);
                })
                .on_mouse_down(move |_| pressed.set(true))
                .on_mouse_up(move |_| pressed.set_if_modified(false))
                .on_pointer_press(move |e: Event<PointerEventData>| {
                    if let PointerEventData::Mouse(m) = &*e
                        && m.button == Some(MouseButton::Left)
                    {
                        p.call(());
                    }
                });
        }
        r
    }
}

/// A caption-sized line in `color`, for outcomes such as "Gesendet" or an error.
pub fn status(t: &Theme, text: impl Into<String>, color: Color) -> Element {
    para(t, text.into(), font::CAPTION, color).into_element()
}

/// A caption-sized hint that wraps.
pub fn note(t: &Theme, text: impl Into<String>) -> Element {
    status(t, text, t.text3)
}
