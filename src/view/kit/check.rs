//! The box of a checkbox, for rows that select themselves when pressed.

use freya::prelude::*;

use crate::view::icons::{Icon, icon};
use crate::view::theme::{Theme, frame, iconsize};

/// The box of a checkbox: filled with a check while `on`, an outline otherwise.
pub fn mark(t: &Theme, on: bool) -> Element {
    let b = rect().width(Size::px(iconsize::M)).height(Size::px(iconsize::M)).center().corner_radius(t.shape.small);
    if on {
        b.background(t.accent).child(icon(t, Icon::Check, iconsize::XS, t.on_accent)).into_element()
    } else {
        b.border(Border::new().fill(t.text3).width(frame::BOX_EDGE).alignment(BorderAlignment::Inner)).into_element()
    }
}
