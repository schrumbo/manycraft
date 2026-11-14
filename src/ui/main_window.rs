use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{
    AddAccount, ClearSelection, MessageSelected, NewBatch, NextAttention, NextInstance, OpenChat, OpenPalette, PreviousInstance, ReconnectMany, ReconnectSelected, RestartMany, RestartSelected, SelectAll, ShowAccounts, ShowAll, ShowBatch, StartMany, StartSelected, StopMany, StopSelected, TileLarger, TileSmaller,
    ToggleSidebar,
};
use super::drive::Mark;
use super::grid::Grid;
use super::model;
use super::motion::Soft;
use super::native::{self, Rect};
use super::theme::{self, Theme, control, font, iconsize, shell, space, tint};
use super::widgets::{KeyNav, icon, primary_button};
use crate::core::i18n::{tr, trf};

actions!(main_window, [FocusSearch, ClearSearch, TileSmall, TileMedium, TileLarge, TileFit, FilterAll, FilterAttention, FilterRunning, FilterStopped]);

/// What the memory line under the toolbar was worked out for: the scope and its accounts as count and memory per batch.
type FitScope = (Option<String>, Vec<(u32, u32)>);

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
    pub(super) overlay: bool,
    pub(super) narrow_now: bool,
    pub(super) search: Entity<InputState>,
    pub(super) query: String,
    pub(super) grid: Grid,
    pub(super) palette: Option<super::palette::Palette>,
    fit: Option<(FitScope, Option<String>)>,
    title_count: Option<usize>,
    last_playing: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl MainWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = model::get(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Name, Batch, Server, Status")));
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
            cx.observe(&model, |this, model, cx| {
                let playing = model.read(cx).playing.clone();
                if let Some(left) = std::mem::replace(&mut this.last_playing, playing.clone()).filter(|_| playing.is_none()) {
                    this.grid.reveal(left);
                }
                cx.notify();
            }),
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
            narrow_now: false,
            search,
            query: String::new(),
            grid: Grid::new(cx),
            palette: None,
            fit: None,
            title_count: None,
            last_playing: None,
            _subscriptions: subscriptions,
        }
    }

    /// Shows `page`; switching between grids keeps the filter but drops the keyboard selection.
    pub(super) fn go(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = false;
        if self.page != page {
            self.page = page;
            self.grid.forget();
            self.grid.reset_scroll();
        }
        if matches!(self.page, Page::Grid(_)) {
            window.focus(&self.grid.focus, cx);
        }
        cx.notify();
    }

    /// Hands the keyboard to the page: the grid when it shows, the window otherwise.
    pub(super) fn focus_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.page, Page::Grid(_)) {
            window.focus(&self.grid.focus, cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    fn set_filter(&mut self, filter: Filter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = filter;
        self.grid.reset_scroll();
        self.go(Page::Grid(match &self.page {
            Page::Grid(batch) => batch.clone(),
            _ => None,
        }), window, cx);
    }

    /// Runs a selection action on the selected tiles, or on the cursor's tile; passes it on when there is none.
    fn on_chosen(&mut self, make: fn(Vec<String>) -> Box<dyn Action>, window: &mut Window, cx: &mut Context<Self>) {
        let keys = if matches!(self.page, Page::Grid(_)) { self.grid.chosen() } else { Vec::new() };
        if keys.is_empty() {
            cx.propagate();
            return;
        }
        window.dispatch_action(make(keys), cx);
    }

    /// The quiet memory line for `batch` while nothing in it runs: worked out off the UI thread once per scope, `None` when everything fits.
    fn fit_line(&mut self, batch: Option<&str>, cx: &mut Context<Self>) -> Option<String> {
        let m = model::get(cx).read(cx);
        let scope: Vec<_> = m.instances.values().filter(|i| batch.is_none_or(|b| i.batch_id == b)).collect();
        if scope.is_empty() || scope.iter().any(|i| i.phase.is_active()) {
            self.fit = None;
            return None;
        }
        let mut per: Vec<(u32, u32)> = m.batches.iter().filter_map(|b| {
            let n = scope.iter().filter(|i| i.batch_id == b.id).count();
            (n > 0).then(|| (u32::try_from(n).unwrap_or(u32::MAX), b.memory_mb))
        }).collect();
        per.sort_unstable();
        let key: FitScope = (batch.map(str::to_owned), per.clone());
        if let Some((done, line)) = &self.fit
            && *done == key
        {
            return line.clone();
        }
        self.fit = Some((key.clone(), None));
        cx.spawn(async move |this, cx| {
            let count: u32 = per.iter().map(|(n, _)| n).sum();
            let total: u64 = per.iter().map(|(n, mb)| u64::from(*n) * u64::from(*mb)).sum();
            let each = per.first().map(|(_, mb)| *mb).unwrap_or(0);
            let uniform = per.iter().all(|(_, mb)| *mb == each);
            let fit = cx.background_executor().spawn(async move { crate::core::fit::fit(1, u32::try_from(total).unwrap_or(u32::MAX)) }).await;
            let line = fit.filter(|f| !f.fits()).map(|f| {
                if uniform {
                    model::fit_line(count, each, &f)
                } else {
                    let label = |mb: u64| super::forms::memory_label(u32::try_from(mb).unwrap_or(u32::MAX));
                    trf("%lld Accounts brauchen %@, dein PC hat %@ frei", &[&count, &label(f.needed_mb), &label(f.free_mb)])
                }
            });
            let _ = this.update(cx, |this, cx| {
                if let Some((done, slot)) = this.fit.as_mut()
                    && *done == key
                {
                    *slot = line;
                    cx.notify();
                }
            });
        })
        .detach();
        None
    }

    fn set_size(&mut self, size: TileSize, window: &mut Window, cx: &mut Context<Self>) {
        self.size = size;
        self.grid.reset_scroll();
        window.focus(&self.grid.focus, cx);
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
                .child(div().mt(px(space::S)).max_w(px(control::READING)).text_size(px(font::CAPTION)).text_color(t.tertiary).text_center().child(tr("Prüfe die Regeln deines Servers, bevor du mehrere Accounts nutzt.")))
                .into_any_element(),
        )
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::perf::frame_start();
        super::theme::follow_focus(window, cx);
        self.narrow_now = Self::narrow(window);
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
            Page::Accounts => super::accounts::page(window, cx).into_any_element(),
            Page::Chat => super::chat::page(window, cx).into_any_element(),
            Page::Grid(batch) => match self.onboarding(cx) {
                Some(empty) => empty,
                None => {
                    let error = model::get(cx).read(cx).error.clone();
                    let fit = self.fit_line(batch.as_deref(), cx).filter(|_| error.is_none());
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(super::toolbar::render(self, batch.as_deref(), window, cx))
                        .children(error.map(|e| notice(e, &t, cx)))
                        .children(fit.map(|line| fit_note(line, &t)))
                        .child(div().relative().flex_1().min_h_0().child(super::grid::render(self, batch.as_deref(), window, cx)).children(super::grid::selection_bar(self, cx)))
                        .into_any_element()
                }
            },
        };
        let show_sidebar = self.sidebar && !narrow;
        let palette = super::palette::render(self, window, cx);
        let root = div()
            .id("main")
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .capture_any_mouse_down(|_, window, cx| window.defer(cx, |window, cx| super::theme::focus_moved(window, false, cx)))
            .capture_any_mouse_up(|_, window, cx| window.defer(cx, |window, cx| super::theme::focus_moved(window, false, cx)))
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if !this.overlay {
                    return;
                }
                match e.keystroke.key.as_str() {
                    "escape" => {
                        this.overlay = false;
                        cx.notify();
                        cx.stop_propagation();
                    }
                    "left" | "right" | "up" | "down" | "enter" | "home" | "end" | "pageup" | "pagedown" => cx.stop_propagation(),
                    _ => {}
                }
            }))
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
            .on_action(cx.listener(|this, _: &TileSmall, window, cx| this.set_size(TileSize::Small, window, cx)))
            .on_action(cx.listener(|this, _: &TileMedium, window, cx| this.set_size(TileSize::Medium, window, cx)))
            .on_action(cx.listener(|this, _: &TileLarge, window, cx| this.set_size(TileSize::Large, window, cx)))
            .on_action(cx.listener(|this, _: &TileFit, window, cx| this.set_size(TileSize::Fit, window, cx)))
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
                this.grid.reset_scroll();
                window.focus(&this.grid.focus, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| super::palette::toggle(this, window, cx)))
            .on_action(cx.listener(|this, _: &ShowAll, window, cx| this.go(Page::Grid(None), window, cx)))
            .on_action(cx.listener(|this, _: &ShowAccounts, window, cx| this.go(Page::Accounts, window, cx)))
            .on_action(cx.listener(|this, a: &ShowBatch, window, cx| this.go(Page::Grid(Some(a.0.clone())), window, cx)))
            .on_action(cx.listener(|this, _: &FilterAll, window, cx| this.set_filter(Filter::All, window, cx)))
            .on_action(cx.listener(|this, _: &FilterAttention, window, cx| this.set_filter(Filter::Attention, window, cx)))
            .on_action(cx.listener(|this, _: &FilterRunning, window, cx| this.set_filter(Filter::Running, window, cx)))
            .on_action(cx.listener(|this, _: &FilterStopped, window, cx| this.set_filter(Filter::Stopped, window, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, window, cx| {
                if !matches!(this.page, Page::Grid(_)) {
                    cx.propagate();
                    return;
                }
                this.grid.select_all();
                window.focus(&this.grid.focus, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                if this.grid.clear_selection() {
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &StartSelected, window, cx| this.on_chosen(|k| Box::new(StartMany(k)), window, cx)))
            .on_action(cx.listener(|this, _: &StopSelected, window, cx| this.on_chosen(|k| Box::new(StopMany(k)), window, cx)))
            .on_action(cx.listener(|this, _: &RestartSelected, window, cx| this.on_chosen(|k| Box::new(RestartMany(k)), window, cx)))
            .on_action(cx.listener(|this, _: &ReconnectSelected, window, cx| this.on_chosen(|k| Box::new(ReconnectMany(k)), window, cx)))
            .on_action(cx.listener(|this, _: &MessageSelected, window, cx| {
                if this.grid.picks.set.len() > 1 {
                    super::grid::open_message(this, window, cx);
                } else {
                    window.dispatch_action(Box::new(OpenChat), cx);
                }
            }))
            .relative()
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
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .shadow_lg()
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.overlay = false;
                                cx.notify();
                            }),
                        )
                        .child(super::sidebar::render(self, window, cx)),
                )
            })
            .children(palette);
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
        KeyBinding::new(k("1"), TileSmall, Some("MainWindow")),
        KeyBinding::new(k("2"), TileMedium, Some("MainWindow")),
        KeyBinding::new(k("3"), TileLarge, Some("MainWindow")),
        KeyBinding::new(k("0"), TileFit, Some("MainWindow")),
        KeyBinding::new(k("f"), FocusSearch, Some("MainWindow")),
        KeyBinding::new(k("shift-c"), OpenChat, Some("MainWindow")),
        KeyBinding::new(k("shift-g"), ShowAll, Some("MainWindow")),
        KeyBinding::new(k("shift-u"), ShowAccounts, Some("MainWindow")),
        KeyBinding::new(k("k"), OpenPalette, Some("MainWindow")),
        KeyBinding::new("ctrl-alt-right", NextInstance, Some("MainWindow")),
        KeyBinding::new("ctrl-alt-left", PreviousInstance, Some("MainWindow")),
        KeyBinding::new("ctrl-alt-down", NextAttention, Some("MainWindow")),
        KeyBinding::new(k("a"), SelectAll, Some("InstanceGrid")),
        KeyBinding::new("escape", ClearSelection, Some("InstanceGrid")),
        KeyBinding::new(k("r"), StartSelected, Some("InstanceGrid")),
        KeyBinding::new(k("."), StopSelected, Some("InstanceGrid")),
        KeyBinding::new(k("shift-r"), RestartSelected, Some("InstanceGrid")),
        KeyBinding::new(k("j"), ReconnectSelected, Some("InstanceGrid")),
        KeyBinding::new(k("enter"), MessageSelected, Some("InstanceGrid")),
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
        let Ok(root) = view.downcast::<gpui_kit::base::Root>() else { return };
        let content = root.read(cx).view().clone();
        if let Ok(view) = content.downcast::<MainWindow>() {
            view.update(cx, |this, cx| {
                this.filter = Filter::Attention;
                this.grid.reset_scroll();
                this.go(Page::Grid(Some(batch)), window, cx);
            });
        }
    });
}

/// A quiet line under the toolbar that says the accounts in view need more memory than is free; it goes once one runs.
fn fit_note(text: String, t: &Theme) -> AnyElement {
    div()
        .id("fit-note")
        .mark("notice", text.clone())
        .h(px(shell::NOTICE))
        .flex_none()
        .pl(px(theme::tile::INSET))
        .pr(px(space::S))
        .flex()
        .items_center()
        .gap(px(space::S))
        .border_b_1()
        .border_color(t.separator)
        .child(icon("info", iconsize::S, t.secondary))
        .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(text.clone()))
        .tooltip(super::widgets::tip(text))
        .into_any_element()
}

/// A quiet line under the toolbar for a problem the app could not handle on its own, with a way to dismiss it.
fn notice(text: String, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    div()
        .id("notice")
        .mark("notice", text.clone())
        .h(px(shell::NOTICE))
        .flex_none()
        .pl(px(theme::tile::INSET))
        .pr(px(space::S))
        .flex()
        .items_center()
        .gap(px(space::S))
        .bg(theme::with_alpha(t.attention, tint::ATTENTION))
        .border_b_1()
        .border_color(t.separator)
        .child(icon("triangle-alert", iconsize::S, t.attention_text))
        .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.primary).child(text.clone()))
        .tooltip(super::widgets::tip(text))
        .child(
            div()
                .id("notice-close")
                .mark("button", tr("Hinweis ausblenden"))
                .size(px(control::S))
                .rounded(px(theme::radius::INNER))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), theme::radius::INNER)
                .child(icon("x", iconsize::S, t.secondary))
                .tooltip(super::widgets::tip(tr("Hinweis ausblenden")))
                .on_click(cx.listener(|_, _, _, cx| {
                    model::get(cx).update(cx, |m, cx| {
                        m.error = None;
                        cx.notify();
                    });
                })),
        )
        .into_any_element()
}
