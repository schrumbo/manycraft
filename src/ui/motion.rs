use std::time::Duration;

use gpui_kit::base::{Transition, transition};
use gpui_kit::prelude::*;
use gpui_kit::*;

pub use crate::core::scale::motion::{FAST, MEDIUM, SLOW};
/// How long a part a link opened stays outlined.
pub const POINT_OUT: Duration = Duration::from_millis(1600);

/// A duration for `with_animation`, which does not look at the reduced-motion setting by itself.
pub fn timed(duration: Duration, cx: &App) -> Duration {
    if cx.reduce_motion() { Duration::from_millis(1) } else { duration }
}

/// An interruptible, ease-out value that follows `target`; it adopts its first value without animating.
pub fn follow(id: impl Into<ElementId>, target: f32, duration: Duration, window: &mut Window, cx: &mut App) -> f32 {
    transition(id.into(), target, Transition::new(timed(duration, cx)), window, cx)
}

/// Hover and press feedback that fades instead of switching.
pub trait Soft {
    /// Paints `color` behind the children on hover and twice as strong on press. Call it before adding children.
    fn soft(self, color: Hsla, radius: f32) -> Self;
    /// Like `soft`, with its own radius per corner, for rows at the top or bottom of a rounded group.
    fn soft_corners(self, color: Hsla, corners: Corners<f32>) -> Self;
}

impl Soft for Stateful<Div> {
    fn soft(self, color: Hsla, radius: f32) -> Self {
        self.soft_corners(color, Corners::all(radius))
    }

    fn soft_corners(mut self, color: Hsla, corners: Corners<f32>) -> Self {
        if self.style().position.is_none() {
            self = self.relative();
        }
        super::drive::marked(self, "clickable").child(HoverLayer { color, corners })
    }
}

#[derive(IntoElement)]
struct HoverLayer {
    color: Hsla,
    corners: Corners<f32>,
}

impl RenderOnce for HoverLayer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state("soft-state", cx, |_, _| (false, false));
        let (hovered, pressed) = *state.read(cx);
        let target = if pressed {
            2.
        } else if hovered {
            1.
        } else {
            0.
        };
        let level = follow("soft-level", target, FAST, window, cx);
        let (on_hover, on_down, on_up, on_up_out) = (state.clone(), state.clone(), state.clone(), state);
        div()
            .id("soft")
            .absolute()
            .inset_0()
            .rounded_tl(px(self.corners.top_left))
            .rounded_tr(px(self.corners.top_right))
            .rounded_br(px(self.corners.bottom_right))
            .rounded_bl(px(self.corners.bottom_left))
            .when(level > 0., |d| d.bg(Hsla { a: (self.color.a * level).min(1.), ..self.color }))
            .on_hover(move |hovered, _, cx| {
                on_hover.update(cx, |s, cx| {
                    s.0 = *hovered;
                    s.1 &= *hovered;
                    cx.notify();
                })
            })
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                on_down.update(cx, |s, cx| {
                    s.1 = true;
                    cx.notify();
                })
            })
            .capture_any_mouse_up(move |_, _, cx| {
                on_up.update(cx, |s, cx| {
                    s.1 = false;
                    cx.notify();
                })
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                on_up_out.update(cx, |s, cx| {
                    s.1 = false;
                    cx.notify();
                })
            })
    }
}
