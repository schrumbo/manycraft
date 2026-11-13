use gpui_kit::component::TitleBar;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::theme::{Theme, font};

/// Prepares a window for the title bar; a driven window never takes the focus.
pub fn window_options(options: WindowOptions, title: impl Into<SharedString>) -> WindowOptions {
    let driven = crate::core::drive::on();
    let options = WindowOptions {
        focus: options.focus && !driven,
        inactive_frame_interval: if driven { None } else { options.inactive_frame_interval },
        ..options
    };
    let bar = TitleBar::window_options();
    WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some(title.into()), ..TitleBar::title_bar_options() }),
        app_owns_titlebar_drag: bar.app_owns_titlebar_drag,
        window_decorations: cfg!(target_os = "linux").then_some(WindowDecorations::Client),
        ..options
    }
}

/// Puts a window's content below the title bar, which draws the window buttons off macOS and carries the drag.
pub fn frame(title: impl Into<SharedString>, content: impl IntoElement, t: &Theme) -> AnyElement {
    let bar = TitleBar::new().child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(title.into()));
    div().size_full().flex().flex_col().child(bar).child(div().flex_1().min_h_0().child(content)).into_any_element()
}
