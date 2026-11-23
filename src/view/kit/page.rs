//! The frame every page is drawn in: the whole width of the content area with one gutter and one rhythm. Nothing centres
//! a page or caps its width; only prose keeps a readable measure inside its own pane.

use freya::prelude::*;

use crate::view::theme::frame;

/// The window width from which a page puts a detail pane beside its list instead of over it.
pub const SPLIT_AT: f32 = 1100.;

/// The column of a page: full width, the page gutter around it and the page rhythm between the blocks added to it.
pub fn column() -> Rect {
    rect().width(Size::fill()).padding(frame::GUTTER).spacing(frame::SECTION)
}

/// `body` in a scrolled page that fills the content area.
pub fn scrolling(scroll: ScrollController, body: impl IntoElement) -> Element {
    ScrollView::new_controlled(scroll).width(Size::fill()).height(Size::fill()).child(body).into_element()
}

/// `prose` at a readable measure, at the left edge of its pane.
pub fn reading(prose: impl IntoElement) -> Rect {
    rect().width(Size::fill()).max_width(Size::px(frame::READING)).child(prose)
}