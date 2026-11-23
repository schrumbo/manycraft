//! Badges: a count or a short state in a capsule.

use freya::prelude::*;

use super::text::figure;
use crate::view::theme::{Theme, font, frame, space};

/// What a badge says by its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Quiet,
    Accent,
    Attention,
    Danger,
}

/// A capsule with `value`, coloured by `tone`.
pub fn badge(t: &Theme, value: impl Into<String>, tone: Tone) -> Element {
    let (fill, ink) = match tone {
        Tone::Quiet => (t.hover, t.text2),
        Tone::Accent => (t.accent, t.on_accent),
        Tone::Attention => (t.attention, t.on_attention),
        Tone::Danger => (t.danger, t.on_danger),
    };
    rect()
        .min_width(Size::px(frame::BADGE))
        .height(Size::px(frame::BADGE))
        .padding((0., space::XS + 1.))
        .center()
        .corner_radius(frame::BADGE / 2.)
        .background(fill)
        .child(figure(t, value, font::CAPTION, ink).font_weight(FontWeight::SEMI_BOLD))
        .into_element()
}

/// `host`, a `w` by `h` box, with `mark` over its top right corner, `inset` in from the edges (negative: outside). The size has to be given: `right` of an absolute child resolves against the space the host was offered, not against a host that sizes itself.
pub fn corner(host: impl IntoElement, w: f32, h: f32, mark: Element, inset: f32) -> Element {
    rect()
        .width(Size::px(w))
        .height(Size::px(h))
        .child(host)
        .child(rect().position(Position::new_absolute().top(inset).right(inset)).interactive(false).child(mark))
        .into_element()
}

/// A small dot that marks a state next to a name.
pub fn dot(color: Color) -> Element {
    rect().width(Size::px(frame::DOT)).height(Size::px(frame::DOT)).corner_radius(frame::DOT / 2.).background(color).into_element()
}
