//! The keyboard cursor of a list of rows of one height: where it stands, which keys move it, and keeping it in view.

use freya::prelude::*;

use super::entry::ROW;
use crate::view::kit::{Bounds, Live};
use crate::view::theme::space;

/// The file `delta` rows from `cursor`, clamped to the list; the first or last when there is no cursor.
pub(super) fn step(order: &[String], cursor: Option<&str>, delta: i64) -> Option<String> {
    if order.is_empty() {
        return None;
    }
    let last = order.len() as i64 - 1;
    let at = cursor.and_then(|c| order.iter().position(|f| f == c));
    let next = match at {
        Some(at) => (at as i64).saturating_add(delta).clamp(0, last),
        None if delta < 0 => last,
        None => 0,
    };
    order.get(next as usize).cloned()
}

/// How many rows a key moves the cursor; the ends of the list for Home and End.
pub(super) fn motion(key: &Key) -> Option<i64> {
    match key {
        Key::Named(NamedKey::ArrowUp) => Some(-1),
        Key::Named(NamedKey::ArrowDown) => Some(1),
        Key::Named(NamedKey::PageUp) => Some(-10),
        Key::Named(NamedKey::PageDown) => Some(10),
        Key::Named(NamedKey::Home) => Some(i64::MIN / 2),
        Key::Named(NamedKey::End) => Some(i64::MAX / 2),
        _ => None,
    }
}

/// The cursor, the focus that makes it visible, and the scrolling it keeps in view.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Cursor {
    pub at: State<Option<String>>,
    pub focus: AccessibilityId,
    pub scroll: ScrollController,
    pub viewport: State<f32>,
    pub bounds: State<Bounds>,
}

pub(super) fn use_cursor() -> Cursor {
    Cursor { at: use_state(|| None), focus: use_a11y(), scroll: use_hook(|| ScrollController::new(0, 0)), viewport: use_state(|| 0.), bounds: use_state(Bounds::default) }
}

impl Cursor {
    pub fn current(&self) -> Option<String> {
        self.at.got().flatten()
    }

    pub fn stand(&self, file: &str) {
        self.at.soft(|c| *c = Some(file.to_owned()));
    }

    /// Takes the size of the list viewport.
    pub fn sized(&self, e: &Event<SizedEventData>) {
        self.viewport.soft(|h| *h = e.area.height());
        self.bounds.soft(|b| *b = Bounds::of(e));
    }

    /// Scrolls row `index` into view.
    pub fn show(&self, index: usize) {
        let top = index as f32 * ROW;
        let bottom = top + ROW;
        let view = *self.viewport.peek();
        let mut scroll = self.scroll;
        let from = -scroll.position().y;
        if top < from {
            scroll.scroll_to_y(-(top as i32));
        } else if view > 0. && bottom > from + view {
            scroll.scroll_to_y(-((bottom - view).ceil() as i32));
        }
    }

    /// Moves `delta` rows through `order`, where every file is one row, and keeps it in view.
    pub fn go(&self, order: &[String], delta: i64) {
        let Some(next) = step(order, self.current().as_deref(), delta) else { return };
        self.stand(&next);
        if let Some(index) = order.iter().position(|f| *f == next) {
            self.show(index);
        }
    }

    /// Where a menu for row `index` opens when a key asked for it.
    pub fn menu_point(&self, index: usize) -> (f32, f32) {
        let bounds = *self.bounds.peek();
        let y = (bounds.y + index as f32 * ROW + self.scroll.position().y + ROW).clamp(bounds.y, bounds.y + bounds.h);
        (bounds.x + space::XL, y)
    }

    /// Keys in the filter above the list: Escape clears it, then hands the focus to the list; Down enters the list and calls `first` when the cursor was not in it yet.
    pub fn filter_keys(&self, e: &Event<KeyboardEventData>, query: State<String>, first: impl FnOnce()) {
        match e.key {
            Key::Named(NamedKey::Escape) => {
                e.stop_propagation();
                if query.peek().is_empty() {
                    self.focus.request_focus();
                } else {
                    query.soft(String::clear);
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                e.stop_propagation();
                self.focus.request_focus();
                if self.current().is_none() {
                    first();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{i}.jar")).collect()
    }

    #[test]
    fn stepping_clamps_and_starts_at_the_ends() {
        let order = list(5);
        assert_eq!(step(&order, Some("2.jar"), 1).as_deref(), Some("3.jar"));
        assert_eq!(step(&order, Some("4.jar"), 1).as_deref(), Some("4.jar"));
        assert_eq!(step(&order, Some("1.jar"), -10).as_deref(), Some("0.jar"));
        assert_eq!(step(&order, None, 1).as_deref(), Some("0.jar"));
        assert_eq!(step(&order, None, -1).as_deref(), Some("4.jar"));
        assert_eq!(step(&order, Some("2.jar"), i64::MAX / 2).as_deref(), Some("4.jar"));
        assert_eq!(step(&order, Some("2.jar"), i64::MIN / 2).as_deref(), Some("0.jar"));
        assert!(step(&[], None, 1).is_none());
    }

    #[test]
    fn the_arrow_page_and_end_keys_move_the_cursor() {
        assert_eq!(motion(&Key::Named(NamedKey::ArrowDown)), Some(1));
        assert_eq!(motion(&Key::Named(NamedKey::ArrowUp)), Some(-1));
        assert_eq!(motion(&Key::Named(NamedKey::PageDown)), Some(10));
        assert!(motion(&Key::Named(NamedKey::Home)).is_some_and(|d| d < -1000));
        assert!(motion(&Key::Named(NamedKey::End)).is_some_and(|d| d > 1000));
        assert_eq!(motion(&Key::Named(NamedKey::Enter)), None);
    }
}
