use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{AddAccount, NewBatch, TileLarger, TileSmaller, ToggleSidebar};
use super::drive::Mark;
use super::grid::Grid;
use super::model;
use super::native::{self, Rect};
use super::theme::{self, Theme, control, font, iconsize, shell, space, tint};
use super::widgets::{icon, primary_button};
use crate::core::i18n::tr;

actions!(main_window, [FocusSearch, ClearSearch]);

/// What the window shows next to the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// The instance grid of one batch, or of all when `None`.
    Grid(Option<String>),
    Accounts,
    Chat,
}

/// Which instances the grid lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Attention,
    Running,
    Stopped,
}

/// How big the tiles are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileSize {
    Small,
    Medium,
    Large,
    Fit,
}

impl TileSize {
    fn larger(self) -> Self {
        match self {
            Self::Small => Self::Medium,
            Self::Medium | Self::Fit => Self::Large,
            Self::Large => Self::Large,
        }
    }

    fn smaller(self) -> Self {
        match self {
            Self::Large => Self::Medium,
            Self::Medium | Self::Fit => Self::Small,
            Self::Small => Self::Small,
        }
    }
}

pub struct MainWindow {
    pub(super) focus: FocusHandle,
    focused_once: bool,
    pub(super) page: Page,
    pub(super) filter: Filter,
    pub(super) size: TileSize,
    pub(super) sidebar: bool,
    overlay: bool,
    pub(super) search: Entity<InputState>,
    pub(super) query: String,
    pub(super) grid: Grid,
    title_count: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl MainWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = model::get(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Suchen")));
        let me = cx.entity().downgrade();
        let app: &mut App = cx;
        let typed = app.subscribe(&search, move |s, e: &InputEvent, cx| {
            if let InputEvent::Change = e {
                let value = s.read(cx).value().to_string();
                let _ = me.update(cx, |this, cx| {
                    this.query = value;
                    this.grid.reset_scroll();
                    cx.notify();
                });
            }
        });
        let subscriptions = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::refresh(window, cx);
                cx.notify();
            }),
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() && model::get(cx).read(cx).playing.is_some() {
                    cx.defer(super::state::show_grid);
                }
            }),
            typed,
        ];
        Self {
            focus: cx.focus_handle(),
            focused_once: false,
            page: Page::Grid(None),
            filter: Filter::All,
            size: TileSize::Medium,
            sidebar: true,
            overlay: false,
            search,
            query: String::new(),
            grid: Grid::new(cx),
            title_count: None,
            _subscriptions: subscriptions,
        }
    }

    /// Shows `page`; switching between grids keeps the filter but drops the keyboard selection.
    pub(super) fn go(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = false;
        if self.page != page {
            self.page = page;
            self.grid.selected = None;
            self.grid.reset_scroll();
        }
        if matches!(self.page, Page::Grid(_)) {
            window.focus(&self.grid.focus, cx);
        }
        cx.notify();
    }

    /// Whether the window is too narrow to keep the sidebar beside the grid.
    pub(super) fn narrow(window: &Window) -> bool {
        window.viewport_size().width < px(shell::NARROW)
    }

    /// Width left for the page next to the sidebar.
    pub(super) fn content_width(&self, window: &Window) -> f32 {
        let width = f32::from(window.viewport_size().width);
        if self.sidebar && !Self::narrow(window) { width - shell::SIDEBAR } else { width }
    }

    fn update_title(&mut self, window: &mut Window, cx: &App) {
        let count = model::get(cx).read(cx).attention_count(None);
        if self.title_count == Some(count) {
            return;
        }
        self.title_count = Some(count);
        let title = if count == 0 || cfg!(target_os = "macos") { "Manycraft".to_string() } else { format!("Manycraft ({count})") };
        window.set_window_title(&title);
    }

    fn onboarding(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = Theme::get(cx);
        let m = model::get(cx);
        let m = m.read(cx);
        let (title, text, id, label, action): (String, String, &str, String, Box<dyn Action>) = if m.accounts.is_empty() {
            (
                tr("Noch keine Accounts"),
                tr("Füge einen Account hinzu, um Minecraft-Instanzen in einem Fenster zu starten."),
                "add-account",
                tr("Account hinzufügen"),
                Box::new(AddAccount),
            )
        } else if m.batches.is_empty() {
            (
                tr("Noch keine Batches"),
                tr("Ein Batch legt Version, Server und Mods fest und startet deine Accounts gemeinsam."),
                "new-batch",
                tr("Batch erstellen"),
                Box::new(NewBatch),
            )
        } else {
            return None;
        };
        let action = action.boxed_clone();
        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::M))
                .px(px(space::XL))
                .child(icon("layout-grid", iconsize::HERO, t.tertiary))
                .child(div().id("empty-title").mark("heading", title.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
                .child(div().max_w(px(control::READING)).text_size(px(font::CALLOUT)).text_color(t.secondary).text_center().child(text))
                .child(div().mt(px(space::S)).child(primary_button(id, label, "plus", false, &t).on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))))
                .into_any_element(),
        )
    }

    fn page_placeholder(&self, name: &str, title: String, text: String, t: &Theme) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(super::toolbar::bar(self, title.clone(), t))
            .child(
                div()
                    .id(SharedString::from(format!("page-{name}")))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(space::M))
                    .px(px(space::XL))
                    .child(icon(if name == "chat" { "messages-square" } else { "users" }, iconsize::HERO, t.tertiary))
                    .child(div().text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
                    .child(div().max_w(px(control::READING)).text_size(px(font::CALLOUT)).text_color(t.secondary).text_center().child(text)),
            )
            .into_any_element()
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::perf::frame_start();
        if !std::mem::replace(&mut self.focused_once, true) && window.focused(cx).is_none() {
            window.focus(&self.grid.focus, cx);
        }
        self.update_title(window, cx);
        let t = Theme::get(cx);
        let narrow = Self::narrow(window);
        if !matches!(self.page, Page::Grid(_)) {
            super::state::set_visible(Default::default(), cx);
        }
        let content = match self.page.clone() {
            Page::Accounts => self.page_placeholder("accounts", tr("Accounts"), tr("Die Account-Verwaltung erscheint hier."), &t),
            Page::Chat => self.page_placeholder("chat", tr("Chat"), tr("Der gemeinsame Chat aller Instanzen erscheint hier."), &t),
            Page::Grid(batch) => match self.onboarding(cx) {
                Some(empty) => empty,
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(super::toolbar::render(self, batch.as_deref(), window, cx))
                    .child(div().flex_1().min_h_0().child(super::grid::render(self, batch.as_deref(), window, cx)))
                    .into_any_element(),
            },
        };
        let show_sidebar = self.sidebar && !narrow;
        let root = div()
            .id("main")
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                if Self::narrow(window) {
                    this.overlay = !this.overlay;
                } else {
                    this.sidebar = !this.sidebar;
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &TileLarger, _, cx| {
                this.size = this.size.larger();
                this.grid.reset_scroll();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &TileSmaller, _, cx| {
                this.size = this.size.smaller();
                this.grid.reset_scroll();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if !matches!(this.page, Page::Grid(_)) {
                    this.page = Page::Grid(None);
                }
                this.search.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearSearch, window, cx| {
                this.search.update(cx, |s, cx| s.set_value("", window, cx));
                this.query.clear();
                window.focus(&this.grid.focus, cx);
                cx.notify();
            }))
            .size_full()
            .flex()
            .bg(t.window)
            .text_color(t.primary)
            .font_family(Theme::font_family())
            .text_size(px(font::BODY))
            .line_height(relative(font::LINE))
            .when(show_sidebar, |d| d.child(super::sidebar::render(self, window, cx)))
            .child(div().flex_1().min_w_0().h_full().bg(t.window).child(content))
            .when(narrow && self.overlay, |d| {
                d.child(
                    div()
                        .id("sidebar-scrim")
                        .absolute()
                        .inset_0()
                        .bg(black().opacity(tint::SCRIM))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.overlay = false;
                            cx.notify();
                        })),
                )
                .child(div().absolute().top_0().left_0().h_full().shadow_lg().child(super::sidebar::render(self, window, cx)))
            });
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

fn bind_keys(cx: &mut App) {
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    let k = |key: &str| -> &'static str { Box::leak(format!("{m}-{key}").into_boxed_str()) };
    cx.bind_keys([
        KeyBinding::new(k("b"), ToggleSidebar, Some("MainWindow")),
        KeyBinding::new(k("="), TileLarger, Some("MainWindow")),
        KeyBinding::new(k("+"), TileLarger, Some("MainWindow")),
        KeyBinding::new(k("-"), TileSmaller, Some("MainWindow")),
        KeyBinding::new(k("f"), FocusSearch, Some("MainWindow")),
        KeyBinding::new("escape", ClearSearch, Some("Toolbar")),
    ]);
}

/// Opens the main window.
pub fn open(cx: &mut App) {
    bind_keys(cx);
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

fn handle(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<MainHandle>().map(|m| m.0).filter(|h| cx.windows().contains(h))
}

/// Whether `id` is the main window, open or just closed.
pub fn is_main(id: WindowId, cx: &App) -> bool {
    cx.try_global::<MainHandle>().is_some_and(|m| m.0.window_id() == id)
}

/// Whether the main window is the frontmost window of the app.
pub fn is_active(cx: &App) -> bool {
    cx.active_window().is_some_and(|w| handle(cx) == Some(w))
}

/// Where the main window is, in the coordinates a game window uses.
pub fn game_rect(cx: &mut App) -> Option<Rect> {
    handle(cx)?.update(cx, |_, window, _| native::rect_of(window)).ok()
}

/// Gets the main window out of the way of the game that takes its place. A driven window stays, so agents keep seeing it.
pub fn hide(cx: &mut App) {
    if crate::core::drive::fenced("hide", "main window") {
        return;
    }
    if let Some(h) = handle(cx) {
        let _ = h.update(cx, |_, window, _| native::hide(window));
    }
}

/// Brings the main window back where the game was.
pub fn show(_rect: Option<Rect>, cx: &mut App) {
    if crate::core::drive::fenced("show", "main window") {
        return;
    }
    if let Some(h) = handle(cx) {
        let _ = h.update(cx, |_, window, cx| native::show(window, cx));
    }
}

/// Shows the grid of `batch`, filtered to what needs attention, as a notification click asks.
pub fn select_batch(batch: &str, cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let batch = batch.to_owned();
    let _ = h.update(cx, |view, window, cx| {
        if let Ok(view) = view.downcast::<MainWindow>() {
            view.update(cx, |this, cx| {
                this.filter = Filter::Attention;
                this.go(Page::Grid(Some(batch)), window, cx);
            });
        }
    });
}
