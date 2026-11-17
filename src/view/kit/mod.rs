//! The controls every screen is built from, drawn with the theme's tokens. Every control can be reached with the
//! keyboard, shows its focus, names itself for the drive tool and takes its text from `tr()`.

pub mod badge;
pub mod button;
pub mod check;
pub mod chip;
pub mod field;
pub mod group;
pub mod markdown;
pub mod menu;
pub mod more;
pub mod row;
pub mod segmented;
pub mod select;
pub mod sheet;
pub mod spinner;
pub mod stepper;
pub mod switch;
pub mod text;
pub mod toast;
pub mod tooltip;

use std::rc::Rc;
use std::time::Duration;

use freya::prelude::*;

use super::theme::{Theme, frame};

/// Keeps `child` at least `height` tall, centred; torin adds `min_height` to the content instead of clamping, so it cannot do this.
pub fn at_least(height: f32, child: impl IntoElement) -> Rect {
    rect().horizontal().cross_align(Alignment::Center).child(rect().width(Size::px(0.)).height(Size::px(height))).child(child)
}

/// `a` moved `share` of the way to `b`, alpha included.
pub fn blend(a: Color, b: Color, share: f32) -> Color {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * share.clamp(0., 1.)).round() as u8;
    Color::from_argb(m(a.a(), b.a()), m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Soft edges over a scrolled list: a fade into `base` at the top once it is scrolled, at the bottom while more follows.
/// `scrolled` is how far the list moved, `content` and `viewport` the heights of the list and of its window.
pub fn fades(base: Color, scrolled: f32, content: f32, viewport: f32) -> Vec<Element> {
    let edge = |top: bool| {
        let (from, to) = if top { (base, base.with_a(0)) } else { (base.with_a(0), base) };
        let at = Position::new_absolute().left(0.);
        rect()
            .position(if top { at.top(0.) } else { at.top(viewport - frame::FADE) })
            .layer(Layer::Relative(frame::FADE_LAYER))
            .width(Size::fill())
            .height(Size::px(frame::FADE))
            .background(LinearGradient::new().angle(0.).stop((from, 0.)).stop((to, 100.)))
            .into_element()
    };
    let mut out = Vec::new();
    if scrolled > 0.5 {
        out.push(edge(true));
    }
    if viewport > 0. && content - viewport - scrolled > 0.5 {
        out.push(edge(false));
    }
    out
}

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

/// Runs `work` on a thread of its own and hands its result back to the UI thread; `None` when the thread died.
pub async fn off_ui<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("work".into())
        .spawn(move || {
            let _ = tx.send_blocking(work());
        })
        .ok()?;
    rx.recv().await.ok()
}

/// The keyboard focus ring drawn around a focused control.
pub fn ring(t: &Theme, focus: Focus) -> Option<Border> {
    (focus == Focus::Keyboard).then(|| Border::new().fill(t.focus).width(frame::RING).alignment(BorderAlignment::Outer))
}

thread_local! {
    static HELD: std::cell::Cell<Modifiers> = const { std::cell::Cell::new(Modifiers::empty()) };
    static ARROWS: std::cell::RefCell<std::collections::HashMap<AccessibilityId, bool>> = std::cell::RefCell::default();
}

/// Notes that the control `a11y` moves with the arrow keys itself while it has the focus.
pub fn use_arrows(a11y: AccessibilityId) {
    arrows_hook(a11y, true);
}

/// Like `use_arrows`, for a control that moves with Left and Right only.
pub fn use_side_arrows(a11y: AccessibilityId) {
    arrows_hook(a11y, false);
}

fn arrows_hook(a11y: AccessibilityId, vertical: bool) {
    use_hook(|| ARROWS.with(|a| a.borrow_mut().insert(a11y, vertical)));
    use_drop(move || {
        ARROWS.with(|a| a.borrow_mut().remove(&a11y));
    });
}

/// True while a control that moves with the arrow keys has the focus.
pub fn arrows() -> bool {
    let focused = *Platform::get().current_window().focused_accessibility_id.peek();
    ARROWS.with(|a| a.borrow().contains_key(&focused))
}

/// True while a button has the keyboard focus, so Enter belongs to it.
pub fn button_focused() -> bool {
    Platform::get().current_window().focused_accessibility_node.peek().role() == AccessibilityRole::Button
}

/// True while a control that moves with Up and Down itself has the focus.
pub fn vertical_arrows() -> bool {
    let focused = *Platform::get().current_window().focused_accessibility_id.peek();
    ARROWS.with(|a| a.borrow().get(&focused).copied().unwrap_or(false))
}

/// Gives the keyboard focus back to where it was when a layer opened, once the layer closes.
pub fn use_focus_return(open: bool) {
    let platform = use_hook(|| Platform::get().current_window());
    let mut before = use_state(|| None::<AccessibilityId>);
    use_side_effect_with_deps(&open, move |open| {
        if *open {
            before.set(Some(*platform.focused_accessibility_id.peek()));
        } else if let Some(id) = before.write().take() {
            platform.send(UserEvent::FocusAccessibilityNode(AccessibilityFocusStrategy::Node(id)));
        }
    });
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
