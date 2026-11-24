//! A drop target: washes its content in the accent while files are dragged over the window and hands over what is dropped.

use std::path::PathBuf;

use freya::prelude::*;

use super::text::strong;
use super::use_theme;
use crate::view::theme::{Theme, font, frame, level};

/// Marks a drop as taken by the target it landed on, so the window does not ask about it again.
pub fn claim(e: &Event<FileEventData>) {
    e.stop_propagation();
}

/// Lets `root`, the whole window, take files dropped where nothing else took them. `dropping` follows whether files are
/// dragged over the window; draw [`outline`] while it is set. A sheet, the palette or a menu has the window's attention,
/// so nothing is asked there.
pub fn window(root: Rect, mut dropping: State<bool>, on_drop: impl Fn(Vec<PathBuf>) + 'static) -> Rect {
    root.on_global_file_hover(move |_| dropping.set_if_modified(true))
        .on_global_file_hover_cancelled(move |_| dropping.set_if_modified(false))
        .on_global_pointer_move(move |_| dropping.set_if_modified(false))
        .on_file_drop(move |e: Event<FileEventData>| {
            dropping.set_if_modified(false);
            on_drop(e.file_paths.clone());
        })
}

/// The accent frame around the window while files are dragged over it.
pub fn outline(t: &Theme, dropping: State<bool>, layered: bool) -> Option<Element> {
    (*dropping.read() && !layered).then(|| {
        rect()
            .position(Position::new_absolute().left(0.).top(0.))
            .layer(Layer::Relative(level::FLOAT))
            .width(Size::fill())
            .height(Size::fill())
            .border(Border::new().fill(t.accent).width(frame::DROP_FRAME).alignment(BorderAlignment::Inner))
            .into_element()
    })
}

/// `content` that takes dropped files; while files are dragged over the window it says what dropping does with `label`.
#[derive(PartialEq)]
pub struct DropTarget {
    content: Element,
    label: String,
    on_drop: EventHandler<Vec<PathBuf>>,
}

impl DropTarget {
    pub fn new(content: impl IntoElement, label: impl Into<String>, on_drop: impl FnMut(Vec<PathBuf>) + 'static) -> Self {
        Self { content: content.into_element(), label: label.into(), on_drop: EventHandler::new(on_drop) }
    }
}

impl Component for DropTarget {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut dropping = use_state(|| false);
        let on_drop = self.on_drop.clone();
        let mut root = rect()
            .width(Size::fill())
            .on_global_file_hover(move |_| dropping.set_if_modified(true))
            .on_global_file_hover_cancelled(move |_| dropping.set_if_modified(false))
            .on_global_pointer_move(move |_| dropping.set_if_modified(false))
            .on_file_drop(move |e: Event<FileEventData>| {
                dropping.set_if_modified(false);
                claim(&e);
                on_drop.call(e.file_paths.clone());
            })
            .child(self.content.clone());
        if *dropping.read() {
            root = root.child(
                rect()
                    .position(Position::new_absolute().left(0.).top(0.))
                    .width(Size::fill())
                    .height(Size::fill())
                    .corner_radius(t.shape.tile)
                    .background(t.accent.with_a((frame::NOTICE_WASH * 255.) as u8))
                    .center()
                    .child(strong(&t, self.label.clone(), font::BODY, t.accent_text)),
            );
        }
        root
    }
}