//! Where the shared files live: each kind's path as a caption that copies on a click, and a button that opens it.

use std::path::{Path, PathBuf};

use freya::prelude::*;

use super::Ctx;
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::text::para;
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::native;
use crate::view::theme::font;

/// Opens the folder `path`, or with `file` the folder that holds it with the file selected when it exists.
pub(super) fn open_button(id: String, path: PathBuf, file: bool) -> Button {
    Button::new(id, tr("Ordner öffnen")).glyph(Icon::Folder).on_press(move |_| open(path.clone(), file))
}

fn open(path: PathBuf, file: bool) {
    let folder = if file { path.parent().map_or_else(|| path.clone(), Path::to_path_buf) } else { path.clone() };
    spawn_in_window(async move {
        let made = folder.clone();
        if let Some(Ok(())) = off_ui(move || std::fs::create_dir_all(&made)).await {
            if file && path.is_file() { native::reveal(&path) } else { native::open(&folder) }
        }
    });
}

/// A path in caption size; a click copies it.
#[derive(PartialEq)]
pub(super) struct PathCaption {
    pub id: String,
    pub path: PathBuf,
    pub ctx: Ctx,
}

impl Component for PathCaption {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let shown = self.path.display().to_string();
        let spot = use_spot(&self.id, "caption", &shown, true);
        let (copy, ui) = (shown.clone(), self.ctx.store.ui);
        rect()
            .on_sized(spot)
            .cursor(CursorIcon::Pointer)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_pointer_press(move |e: Event<PointerEventData>| {
                e.stop_propagation();
                if let PointerEventData::Mouse(m) = &*e
                    && m.button == Some(MouseButton::Left)
                {
                    let text = if native::copy_text(copy.clone()) { tr("Pfad kopiert.") } else { tr("Der Pfad wurde nicht kopiert.") };
                    toast::show(ui, text, None);
                }
            })
            .child(para(&t, shown, font::CAPTION, if *hovered.read() { t.text2 } else { t.text3 }))
    }
}
