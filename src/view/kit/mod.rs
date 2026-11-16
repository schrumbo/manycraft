//! The controls every screen is built from, drawn with the theme's tokens. Every control can be reached with the
//! keyboard, shows its focus, names itself for the drive tool and takes its text from `tr()`.

pub mod badge;
pub mod button;
pub mod chip;
pub mod field;
pub mod group;
pub mod menu;
pub mod row;
pub mod segmented;
pub mod select;
pub mod sheet;
pub mod stepper;
pub mod switch;
pub mod text;
pub mod toast;
pub mod tooltip;

use std::rc::Rc;
use std::time::Duration;

use freya::prelude::*;

use super::theme::{Theme, frame};

/// The theme of the window, shared by every component below the root.
pub fn use_theme() -> Rc<Theme> {
    use_consume::<State<Rc<Theme>>>().read().clone()
}

/// Waits `duration` without blocking the UI thread.
pub async fn after(duration: Duration) {
    timer(duration).await;
}

/// Runs `work` on a named thread; the receiver gets its result, or nothing when the thread could not start.
pub fn off_thread<T: Send + 'static>(name: &str, work: impl FnOnce() -> T + Send + 'static) -> Option<async_channel::Receiver<T>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.send_blocking(work());
        })
        .ok()
        .map(|_| rx)
}

/// Opens `target`, a web address or a folder, with the system; a driven run only notes it as `what`.
pub fn open_external(what: &str, target: String) {
    if !crate::core::drive::fenced(what, &target) {
        Platform::get().open_url(target);
    }
}

/// The keyboard focus ring drawn around a focused control.
pub fn ring(t: &Theme, focus: Focus) -> Option<Border> {
    (focus == Focus::Keyboard).then(|| Border::new().fill(t.focus).width(frame::RING).alignment(BorderAlignment::Outer))
}

thread_local! {
    static HELD: std::cell::Cell<Modifiers> = const { std::cell::Cell::new(Modifiers::empty()) };
}

/// The modifier keys held now, for clicks that select a range or add to the selection.
pub fn held() -> Modifiers {
    HELD.with(std::cell::Cell::get)
}

/// Notes the modifier keys of a key event.
pub fn note_held(modifiers: Modifiers) {
    HELD.with(|h| h.set(modifiers));
}

/// Device pixels per logical pixel in the current window.
pub fn scale() -> f32 {
    (*Platform::get().current_window().scale_factor.peek() as f32).max(0.1)
}

/// A rectangle's position and size in logical pixels, from the top left of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Bounds {
    /// The bounds of a sized element; layout measures in device pixels, the rest of the view in logical ones.
    pub fn of(e: &Event<SizedEventData>) -> Self {
        let a = e.area;
        Self { x: a.min_x(), y: a.min_y(), w: a.width(), h: a.height() }
    }

    /// The middle of the rectangle.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2., self.y + self.h / 2.)
    }

    /// Whether `(x, y)` lies inside.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}
