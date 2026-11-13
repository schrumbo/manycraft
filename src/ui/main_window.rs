use gpui_kit::prelude::*;
use gpui_kit::*;

struct MainWindow;

impl Render for MainWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().items_center().justify_center().child(format!("Keine Instanzen in {}", crate::core::paths::instances_dir().display()))
    }
}

/// Opens the main window with the instance grid.
pub fn open(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(640.), px(480.))),
        titlebar: Some(TitlebarOptions { title: Some("Manycraft".into()), ..Default::default() }),
        app_id: Some("io.github.schrumbo.manycraft".into()),
        ..Default::default()
    };
    let _ = cx.open_window(options, |_, cx| cx.new(|_| MainWindow));
}
