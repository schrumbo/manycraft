use gpui_kit::prelude::*;
use gpui_kit::*;

use super::drive::Mark;
use super::theme::{self, Theme, control, font, iconsize, space};
use super::widgets::{icon, primary_button};
use crate::core::i18n::tr;

struct MainWindow {
    focus: FocusHandle,
    focused_once: bool,
    _appearance: Subscription,
}

impl MainWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _appearance = cx.observe_window_appearance(window, |_, window, cx| {
            theme::refresh(window, cx);
            cx.notify();
        });
        Self { focus: cx.focus_handle(), focused_once: false, _appearance }
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::perf::frame_start();
        if !std::mem::replace(&mut self.focused_once, true) && window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        let t = Theme::get(cx);
        let title = tr("Noch keine Instanzen");
        let empty = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::M))
            .child(icon("layout-grid", iconsize::HERO, t.tertiary))
            .child(div().id("empty-title").mark("heading", title.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
            .child(
                div()
                    .max_w(px(control::READING))
                    .text_size(px(font::CALLOUT))
                    .text_color(t.secondary)
                    .text_center()
                    .child(tr("Füge einen Account hinzu, um Minecraft-Instanzen in einem Fenster zu starten.")),
            )
            .child(div().mt(px(space::S)).child(primary_button("add-account", tr("Account hinzufügen"), "plus", false, &t).on_click(|_, _, _| {})));
        let root = div()
            .id("main")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px(px(space::XL))
            .bg(t.window)
            .text_color(t.primary)
            .font_family(Theme::font_family())
            .text_size(px(font::BODY))
            .line_height(relative(font::LINE))
            .child(empty);
        let root = super::perf::frame_end(root);
        super::chrome::frame("Manycraft", root, &t)
    }
}

struct MainHandle(AnyWindowHandle);

impl Global for MainHandle {}

fn driven_size() -> Option<(f32, f32)> {
    let value = std::env::var("MANYCRAFT_DRIVE_SIZE").ok()?;
    let (w, h) = value.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Opens the main window.
pub fn open(cx: &mut App) {
    let (w, h) = driven_size().filter(|_| crate::core::drive::on()).unwrap_or((1280., 800.));
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(640.), px(420.))),
        app_id: Some("io.github.schrumbo.manycraft".into()),
        ..Default::default()
    };
    let options = super::chrome::window_options(options, "Manycraft");
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::refresh(window, cx);
        cx.new(|cx| MainWindow::new(window, cx))
    }) {
        cx.set_global(MainHandle(handle));
    }
}

/// Whether `id` is the main window, open or just closed.
pub fn is_main(id: WindowId, cx: &App) -> bool {
    cx.try_global::<MainHandle>().is_some_and(|m| m.0.window_id() == id)
}
