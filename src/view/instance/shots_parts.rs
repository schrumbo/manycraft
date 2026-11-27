use std::sync::Arc;

use freya::prelude::*;

use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::text::{heading, para};
use crate::view::theme::{Theme, control, font, iconsize, space};
use crate::ui::theme::{files, tile};

/// Shared data of a virtual list that is equal while it is the same allocation.
pub struct Same<T>(pub Arc<T>);

impl<T> Clone for Same<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> PartialEq for Same<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What a key does in the screenshot grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    First,
    Last,
    PageUp,
    PageDown,
    Open,
    Delete,
}

/// The grid command of a key press; `None` for keys that belong to someone else.
pub fn key_move(key: &Key, mods: Modifiers) -> Option<Move> {
    let primary = if cfg!(target_os = "macos") { mods.contains(Modifiers::META) } else { mods.contains(Modifiers::CONTROL) };
    if let Key::Named(NamedKey::Backspace) = key {
        return primary.then_some(Move::Delete);
    }
    if mods.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) {
        return None;
    }
    let Key::Named(named) = key else { return None };
    Some(match named {
        NamedKey::ArrowLeft => Move::Left,
        NamedKey::ArrowRight => Move::Right,
        NamedKey::ArrowUp => Move::Up,
        NamedKey::ArrowDown => Move::Down,
        NamedKey::Home => Move::First,
        NamedKey::End => Move::Last,
        NamedKey::PageUp => Move::PageUp,
        NamedKey::PageDown => Move::PageDown,
        NamedKey::Enter => Move::Open,
        NamedKey::Delete => Move::Delete,
        _ => return None,
    })
}

/// How many cells fit side by side in `width`.
pub fn columns(width: f32) -> usize {
    (((width + space::S) / (files::SHOT_MIN + space::S)).floor() as usize).max(1)
}

/// The width of one cell when `columns` share `width`.
pub fn cell_width(width: f32, columns: usize) -> f32 {
    ((width - space::S * columns.saturating_sub(1) as f32) / columns.max(1) as f32).floor().max(1.)
}

/// The height of the picture of a cell `cell` wide.
pub fn picture_height(cell: f32) -> f32 {
    (cell / tile::ASPECT).floor().max(1.)
}

/// The height of a row of cells `cell` wide: picture, caption and the gap to the next row.
pub fn row_height(cell: f32) -> f32 {
    picture_height(cell) + files::SHOT_CAPTION + space::S
}

/// The index `by` steps from `at` in a list of `len`, held to the ends; from nothing selected it starts at the first.
pub fn moved(at: Option<usize>, by: isize, len: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(at.map_or(0, |at| at.saturating_add_signed(by).min(last)))
}

/// How many cells a page key moves: the rows that fit, at least one, of `columns` cells.
pub fn page(height: f32, row: f32, columns: usize) -> isize {
    let rows = if row > 0. { ((height / row).floor() as isize).max(1) } else { 1 };
    rows * columns as isize
}

/// The scroll offset that brings row `row` of height `height` into a viewport of `view` that is scrolled to `y` (zero or
/// negative), moving as little as possible; `None` when it is in view already.
pub fn bring_into_view(row: usize, height: f32, view: f32, y: i32) -> Option<i32> {
    let top = row as f32 * height;
    let seen = -(y as f32);
    if top < seen {
        Some(-(top as i32))
    } else if top + height > seen + view {
        Some(-((top + height - view).ceil() as i32))
    } else {
        None
    }
}

/// An empty, loading or failed state in the middle of the tab: a glyph, a heading, a line and the actions that help.
pub fn centered(t: &Theme, glyph: Icon, title: String, text: String, actions: impl IntoIterator<Item = Button>) -> Element {
    let mut column = rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .padding((0., space::XL))
        .child(icon(t, glyph, iconsize::L * 2., t.text3))
        .child(heading(t, title));
    if !text.is_empty() {
        column = column.child(rect().width(Size::fill()).max_width(Size::px(control::READING)).child(para(t, text, font::CALLOUT, t.text2).text_align(TextAlign::Center)));
    }
    let actions: Vec<Element> = actions.into_iter().map(IntoElement::into_element).collect();
    if !actions.is_empty() {
        column = column.child(rect().horizontal().spacing(space::S).children(actions));
    }
    column.into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_follow_the_width_and_never_drop_below_one() {
        assert_eq!(columns(0.), 1);
        assert_eq!(columns(files::SHOT_MIN), 1);
        assert_eq!(columns(files::SHOT_MIN * 2. + space::S), 2);
        assert_eq!(columns(files::SHOT_MIN * 2. + space::S - 1.), 1);
        assert_eq!(columns(f32::NAN), 1);
    }

    #[test]
    fn cells_share_the_width_without_overflowing() {
        let width = 700.;
        let n = columns(width);
        assert!(cell_width(width, n) * n as f32 + space::S * (n - 1) as f32 <= width);
        assert_eq!(cell_width(0., 1), 1.);
    }

    #[test]
    fn rows_keep_the_aspect_of_the_picture() {
        assert_eq!(picture_height(160.), (160. / tile::ASPECT).floor());
        assert_eq!(row_height(160.), picture_height(160.) + files::SHOT_CAPTION + space::S);
        assert_eq!(picture_height(0.), 1.);
    }

    #[test]
    fn keys_map_to_grid_commands() {
        let none = Modifiers::empty();
        assert_eq!(key_move(&Key::Named(NamedKey::ArrowDown), none), Some(Move::Down));
        assert_eq!(key_move(&Key::Named(NamedKey::Enter), none), Some(Move::Open));
        assert_eq!(key_move(&Key::Named(NamedKey::Delete), none), Some(Move::Delete));
        assert_eq!(key_move(&Key::Named(NamedKey::Backspace), none), None);
        assert_eq!(key_move(&Key::Named(NamedKey::ArrowDown), Modifiers::ALT), None);
        assert_eq!(key_move(&Key::Character("a".into()), none), None);
        let primary = if cfg!(target_os = "macos") { Modifiers::META } else { Modifiers::CONTROL };
        assert_eq!(key_move(&Key::Named(NamedKey::Backspace), primary), Some(Move::Delete));
    }

    #[test]
    fn steps_stop_at_the_ends() {
        assert_eq!(moved(Some(2), 1, 5), Some(3));
        assert_eq!(moved(Some(4), 3, 5), Some(4));
        assert_eq!(moved(Some(1), -3, 5), Some(0));
        assert_eq!(moved(None, 1, 5), Some(0));
        assert_eq!(moved(None, 1, 0), None);
    }

    #[test]
    fn a_page_is_the_rows_in_view() {
        assert_eq!(page(500., 100., 3), 15);
        assert_eq!(page(50., 100., 3), 3);
        assert_eq!(page(500., 0., 3), 3);
    }

    #[test]
    fn scrolling_moves_only_as_far_as_needed() {
        assert_eq!(bring_into_view(1, 100., 300., 0), None);
        assert_eq!(bring_into_view(3, 100., 300., 0), Some(-100));
        assert_eq!(bring_into_view(0, 100., 300., -250), Some(0));
        assert_eq!(bring_into_view(2, 100., 300., -250), Some(-200));
        assert_eq!(bring_into_view(5, 100., 300., 0), Some(-300));
    }
}
