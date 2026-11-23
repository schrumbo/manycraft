//! The strip at the left edge of a side panel that drags its width: a pointer shows where it is, a double click
//! restores the default width, the arrow keys move it while it has the focus.

use std::time::{Duration, Instant};

use freya::prelude::*;

use super::{ring, use_arrows, use_theme};
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::theme::{frame, level};

const DOUBLE: Duration = Duration::from_millis(400);
const KEY_STEP: f32 = 16.;
const KEY_JUMP: f32 = 64.;

/// The new width of a panel `start` wide after its left edge was dragged from `from` to `to`, kept between `min` and `max`.
pub fn dragged(start: f32, from: f32, to: f32, min: f32, max: f32) -> f32 {
    (start + (from - to)).clamp(min, max.max(min))
}

/// The grip on the left edge of a panel that is `shown` wide: dragging it left widens the panel. `width` holds the
/// chosen width, 0 for the default, which a double click goes back to; `done` hears the width once a change ends.
#[derive(PartialEq)]
pub struct PanelGrip {
    pub id: &'static str,
    pub label: String,
    pub width: State<f32>,
    pub shown: f32,
    pub min: f32,
    pub max: f32,
    pub done: EventHandler<f32>,
}

impl Component for PanelGrip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        use_arrows(a11y);
        let mut start = use_state(|| None::<(f32, f32)>);
        let mut last = use_state(|| None::<Instant>);
        let mut hovered = use_state(|| false);
        let spot = use_spot(self.id, "separator", &self.label, true);
        let (mut width, shown, min, max) = (self.width, self.shown, self.min, self.max);
        let (down_done, up_done, key_done) = (self.done.clone(), self.done.clone(), self.done.clone());
        let live = start.read().is_some() || *hovered.read() || ring(&t, focus()).is_some();
        let line = rect().width(Size::px(if live { 2. } else { 0. })).height(Size::fill()).background(t.accent);
        rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Slider)
            .a11y_alt(self.label.clone())
            .position(Position::new_absolute().left(-frame::SIDEBAR_GRIP / 2.).top(0.))
            .width(Size::px(frame::SIDEBAR_GRIP))
            .height(Size::fill())
            .layer(level::FLOAT)
            .main_align(Alignment::Center)
            .cross_align(Alignment::Center)
            .cursor(CursorIcon::ColResize)
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !a11y.is_focused() {
                    return;
                }
                let jump = if e.modifiers.contains(Modifiers::SHIFT) { KEY_JUMP } else { KEY_STEP };
                let next = match e.key {
                    Key::Named(NamedKey::ArrowLeft) => dragged(shown, 0., -jump, min, max),
                    Key::Named(NamedKey::ArrowRight) => dragged(shown, 0., jump, min, max),
                    Key::Named(NamedKey::Home) => 0.,
                    _ => return,
                };
                width.set(next);
                key_done.call(next);
                e.stop_propagation();
            })
            .on_pointer_down(move |e: Event<PointerEventData>| {
                if !e.data().is_primary() {
                    return;
                }
                a11y.request_focus();
                let now = Instant::now();
                if last.peek().is_some_and(|at| now - at < DOUBLE) {
                    last.set(None);
                    start.set(None);
                    width.set(0.);
                    down_done.call(0.);
                } else {
                    last.set(Some(now));
                    start.set(Some((e.global_location().x as f32, shown)));
                }
                e.stop_propagation();
            })
            .on_global_pointer_move(move |e: Event<PointerEventData>| {
                let Some((from, at)) = *start.peek() else { return };
                width.set_if_modified(dragged(at, from, e.global_location().x as f32, min, max));
                e.stop_propagation();
            })
            .on_capture_global_pointer_up(move |e: Event<PointerEventData>| {
                if start.peek().is_some() {
                    start.set(None);
                    up_done.call(*width.peek());
                    e.stop_propagation();
                }
            })
            .child(line)
    }
}

/// The accessible name of the chat sidebar's grip.
pub fn chat_label() -> String {
    tr("Breite des Chats")
}

#[cfg(test)]
mod tests {
    use super::dragged;

    #[test]
    fn dragging_left_widens_and_right_narrows() {
        assert_eq!(dragged(340., 500., 450., 280., 720.), 390.);
        assert_eq!(dragged(340., 500., 540., 280., 720.), 300.);
    }

    #[test]
    fn the_width_stays_between_the_limits() {
        assert_eq!(dragged(340., 500., 0., 280., 720.), 720.);
        assert_eq!(dragged(340., 500., 900., 280., 720.), 280.);
        assert_eq!(dragged(340., 500., 0., 280., 200.), 280.);
    }
}
