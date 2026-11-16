//! Text styles: one line that ends in an ellipsis, wrapped paragraphs, titles, captions and numbers.

use freya::prelude::*;

use crate::view::theme::{Theme, faces, font, frame};

fn with_faces(mut l: Label, t: &Theme, chain: [&'static str; 4]) -> Label {
    if let Some(f) = &t.font {
        l = l.font_family(f.clone());
    }
    for family in chain {
        l = l.font_family(family);
    }
    l
}

/// One line of `s` that ends in an ellipsis when it does not fit.
pub fn line(t: &Theme, s: impl Into<String>, size: f32, color: Color) -> Label {
    with_faces(label().text(s.into()).font_size(size).color(color).max_lines(1).text_overflow(TextOverflow::Ellipsis), t, faces::BODY)
}

/// One line of body text in the main text colour.
pub fn body(t: &Theme, s: impl Into<String>) -> Label {
    line(t, s, font::BODY, t.text1)
}

/// One line of secondary text.
pub fn secondary(t: &Theme, s: impl Into<String>) -> Label {
    line(t, s, font::CALLOUT, t.text2)
}

/// One line of caption text, for hints and counts.
pub fn caption(t: &Theme, s: impl Into<String>) -> Label {
    line(t, s, font::CAPTION, t.text3)
}

/// One semi-bold line.
pub fn strong(t: &Theme, s: impl Into<String>, size: f32, color: Color) -> Label {
    line(t, s, size, color).font_weight(FontWeight::SEMI_BOLD)
}

/// The title of a page or sheet.
pub fn title(t: &Theme, s: impl Into<String>) -> Label {
    with_faces(label().text(s.into()).font_size(font::TITLE).color(t.text1).font_weight(FontWeight::SEMI_BOLD).max_lines(1).text_overflow(TextOverflow::Ellipsis), t, faces::DISPLAY)
}

/// A heading inside a page or sheet.
pub fn heading(t: &Theme, s: impl Into<String>) -> Label {
    with_faces(label().text(s.into()).font_size(font::TITLE3).color(t.text1).font_weight(FontWeight::SEMI_BOLD).max_lines(1), t, faces::DISPLAY)
}

/// A paragraph that wraps to its width.
pub fn para(t: &Theme, s: impl Into<String>, size: f32, color: Color) -> Label {
    with_faces(label().text(s.into()).font_size(size).color(color).width(Size::fill()).line_height(frame::PARA_LINE), t, faces::BODY)
}

/// Numbers that line up under each other.
pub fn figure(t: &Theme, s: impl Into<String>, size: f32, color: Color) -> Label {
    line(t, s, size, color).font_tabular()
}

/// Fixed-width text, for keys and paths.
pub fn mono(t: &Theme, s: impl Into<String>, size: f32, color: Color) -> Label {
    with_faces(label().text(s.into()).font_size(size).color(color).max_lines(1), t, faces::MONO)
}

/// A flexible gap that pushes what follows to the end of a row.
pub fn spacer() -> Element {
    rect().width(Size::flex(1.)).into_element()
}

/// A one-pixel line across its parent.
pub fn hairline(t: &Theme) -> Element {
    rect().width(Size::fill()).height(Size::px(1.)).background(t.separator).into_element()
}
