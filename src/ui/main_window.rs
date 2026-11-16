use std::time::Duration;

use gpui_kit::component::WindowExt;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{
    AddAccount, ClearSelection, ImportBatch, ImportPrismAccounts, MessageSelected, ImportFromLauncher, NextAttention, NextInstance, OpenChat, OpenPalette, PickColor, PreviousInstance, ReconnectMany, ReconnectSelected, RestartMany, RestartSelected, SelectAll, ShowAccounts, Undo, ShowAll, ShowBatch, ShowInstance, NewInstance, NewInstanceToPlay, StartMany, StartSelected, StopMany, StopSelected,
    ToggleFullscreen, ToggleSidebar, Quit, EditShownBatch, EditBatch, StartBatch, StopBatch, ShowShortcuts,
};
use super::drive::Mark;
use super::grid::Grid;
use super::model;
use super::motion::Soft;
use super::native;
use super::theme::{self, Theme, chat, control, font, iconsize, selection, shell, space, tint};
use super::widgets::{KeyNav, TakeFocus, icon, primary_button};
use crate::core::i18n::{tr, trf};
use crate::core::settings;
use crate::core::place::{self, Mode, Place, Rect};
use crate::core::winstate::{self, Saved};

actions!(main_window, [FocusSearch, ClearSearch, FilterAll, FilterAttention, FilterRunning, FilterStopped]);

/// What the memory line under the toolbar was worked out for: the scope and its accounts as count and memory per batch.
type FitScope = (Option<String>, Vec<(u32, u32)>);

/// What the window shows next to the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// The instance grid of one batch, or of all when `None`.
    Grid(Option<String>),
    /// The page of one instance.
    Instance(String),
    Accounts,
    Chat,
}

/// Which instances the grid lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Attention,
    /// What needs attention, narrowed to one kind of work.
    Need(model::Need),
    Running,
    Stopped,
}

impl Filter {
    /// The filter the toolbar shows as picked: a kind of work counts as what needs attention.
    pub fn base(self) -> Self {
        if let Filter::Need(_) = self { Filter::Attention } else { self }
    }
}

pub struct MainWindow {
    pub(super) focus: FocusHandle,
    pub(super) page: Page,
    pub(super) filter: Filter,
    pub(super) sidebar: bool,
    pub(super) overlay: bool,
    pub(super) narrow_now: bool,
    pub(super) seen_offer: Option<String>,
    pub(super) search: Entity<InputState>,
    pub(super) query: String,
    pub(super) grid: Grid,
    pub(super) palette: Option<super::palette::Palette>,
    /// Whether the overview of every shortcut is open.
    pub(super) shortcuts: bool,
    fit: Option<(FitScope, Option<String>)>,
    saved: Saved,
    restoring: Option<Page>,
    dirty: bool,
    saving: Option<Task<()>>,
    writing: Option<Task<()>>,
    /// Prism Launcher instances that can become a batch, and accounts Prism lists, as found at startup.
    pub(super) prism: (usize, usize),
    pub(super) search_roomy: bool,
    title_count: Option<usize>,
    last_playing: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl MainWindow {
    fn new(saved: Saved, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            cx.observe_window_bounds(window, |this, window, cx| this.remember(window, cx)),
            cx.on_app_quit(|this, _| {
                this.write_now();
                async {}
            }),
            cx.on_release(|this, _| this.write_now()),
            cx.observe(&model, |this, model, cx| {
                if model.read(cx).loaded
                    && let Some(page) = this.restoring.take()
                    && this.page == Page::Grid(None)
                {
                    let m = model.read(cx);
                    let exists = match &page {
                        Page::Grid(Some(batch)) => m.batches.iter().any(|b| &b.id == batch),
                        Page::Instance(id) => m.instances.iter().any(|i| &i.id == id),
                        _ => false,
                    };
                    if exists {
                        this.page = page;
                    }
                }
                if let Page::Grid(Some(batch)) = &this.page {
                    let m = model.read(cx);
                    if m.loaded && m.batch(batch).is_none() && !m.games.values().any(|g| &g.batch_id == batch) && !m.session.waiting_starts.iter().any(|(b, _)| b == batch) {
                        this.page = Page::Grid(None);
                    }
                }
                let playing = model.read(cx).playing.clone();
                if let Some(left) = std::mem::replace(&mut this.last_playing, playing.clone()).filter(|_| playing.is_none()) {
                    this.grid.reveal(left);
                }
                this.grid.touch();
                cx.notify();
            }),
            cx.subscribe(&model, |this, _, _: &model::ChatArrived, cx| {
                if this.page == Page::Chat {
                    cx.notify();
                }
            }),
            cx.observe_window_visibility(window, |_, visibility, _, cx| {
                let away = !visibility.is_visible();
                cx.defer(move |cx| super::state::watch(|w| w.away = away, cx));
            }),
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() && model::get(cx).read(cx).playing.is_some() {
                    cx.defer(super::state::show_grid);
                }
            }),
            typed,
        ];
        Self::look_for_prism(cx);
        native::note_center(window);
        let page = match saved.page {
            winstate::Page::Accounts => Page::Accounts,
            winstate::Page::Chat if settings::active().merged_chat => Page::Chat,
            _ => Page::Grid(None),
        };
        Self {
            focus: cx.focus_handle(),
            restoring: saved.batch.clone().filter(|_| page == Page::Grid(None)).map(|id| match saved.page {
                winstate::Page::Instance => Page::Instance(id),
                _ => Page::Grid(Some(id)),
            }),
            page,
            sidebar: saved.sidebar,
            saved,
            dirty: false,
            saving: None,
            writing: None,
            filter: Filter::All,
            overlay: false,
            shortcuts: false,
            narrow_now: false,
            seen_offer: None,
            search,
            query: String::new(),
            grid: Grid::new(cx),
            palette: None,
            fit: None,
            prism: (0, 0),
            search_roomy: true,
            title_count: None,
            last_playing: None,
            _subscriptions: subscriptions,
        }
    }

    fn look_for_prism(cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async {
            let instances = super::remote::launcher_libraries().iter().flat_map(|l| &l.instances).filter(|i| i.importable()).count();
            (instances, super::remote::prism_accounts().map_or(0, |l| l.len()))
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                s.prism = found;
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows `page`; switching between grids keeps the filter but drops the keyboard selection.
    pub(super) fn go(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = false;
        self.restoring = None;
        if self.page != page {
            self.page = page;
            self.grid.forget();
            self.grid.reset_scroll();
        }
        self.focus_page(window, cx);
        cx.defer_in(window, |this, window, cx| this.focus_page(window, cx));
        self.remember(window, cx);
        cx.notify();
    }

    fn remember(&mut self, window: &Window, cx: &mut Context<Self>) {
        native::note_center(window);
        let mut saved = self.saved.clone();
        if self.restoring.is_none() {
            (saved.page, saved.batch) = match &self.page {
                Page::Grid(batch) => (winstate::Page::Grid, batch.clone()),
                Page::Accounts => (winstate::Page::Accounts, None),
                Page::Chat => (winstate::Page::Chat, None),
                Page::Instance(id) => (winstate::Page::Instance, Some(id.clone())),
            };
        }
        saved.sidebar = self.sidebar;
        if !crate::core::drive::away() && !window.is_fullscreen() && native::place_of(window).is_some() {
            let shown = match window.window_bounds() {
                WindowBounds::Windowed(b) => Some((b, false)),
                WindowBounds::Maximized(b) => Some((b, true)),
                WindowBounds::Fullscreen(_) => None,
            };
            if let Some((b, maximized)) = shown {
                saved.frame = Some(winstate::Frame { x: f32::from(b.origin.x), y: f32::from(b.origin.y), w: f32::from(b.size.width), h: f32::from(b.size.height) });
                saved.maximized = maximized;
                saved.display = window.display(cx).and_then(|d| d.uuid().ok()).map(|u| u.to_string());
            }
        }
        if saved == self.saved {
            return;
        }
        self.saved = saved;
        self.dirty = true;
        self.saving = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let _ = this.update(cx, |this, cx| this.flush(cx));
        }));
    }

    fn flush(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        self.saving = None;
        let saved = self.saved.clone();
        let before = self.writing.take();
        self.writing = Some(cx.background_spawn(async move {
            if let Some(before) = before {
                before.await;
            }
            let _ = winstate::save(&saved);
        }));
    }

    fn write_now(&mut self) {
        if std::mem::take(&mut self.dirty) {
            let _ = winstate::save(&self.saved);
        }
    }

    /// Hands the keyboard to the page: the grid when it shows, the window otherwise.
    pub(super) fn focus_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        if matches!(self.page, Page::Grid(_)) && !self.onboarding_shown(cx) {
            window.focus(&self.grid.focus, cx);
        } else if !matches!(self.page, Page::Instance(_)) || !super::instance_page::focus(window, cx) {
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

    /// Like [`Self::on_chosen`], but with nothing chosen on a batch's page acts on that batch.
    fn on_chosen_or_shown(&mut self, make: fn(Vec<String>) -> Box<dyn Action>, whole: fn(String) -> Box<dyn Action>, window: &mut Window, cx: &mut Context<Self>) {
        let keys = if matches!(self.page, Page::Grid(_)) { self.grid.chosen() } else { Vec::new() };
        match (keys.is_empty(), self.shown_batch(cx)) {
            (false, _) => window.dispatch_action(make(keys), cx),
            (true, Some(batch)) => window.dispatch_action(whole(batch), cx),
            (true, None) => cx.propagate(),
        }
    }

    fn shown_batch(&self, cx: &App) -> Option<String> {
        let Page::Grid(Some(batch)) = &self.page else { return None };
        model::get(cx).read(cx).batch(batch).map(|b| b.id.clone())
    }

    /// The quiet memory line for `batch` while nothing in it runs: worked out off the UI thread once per scope, `None` when everything fits.
    fn fit_line(&mut self, batch: Option<&str>, cx: &mut Context<Self>) -> Option<String> {
        let m = model::get(cx).read(cx);
        let scope: Vec<_> = m.games.values().filter(|i| batch.is_none_or(|b| i.batch_id == b)).collect();
        if scope.is_empty() || scope.iter().any(|i| i.phase.is_active()) {
            self.fit = None;
            return None;
        }
        let mut per: Vec<(u32, u32)> = m.batches.iter().filter_map(|b| {
            let n = scope.iter().filter(|i| i.batch_id == b.id).count();
            let each = m.measured.get(&b.id).copied().unwrap_or_else(|| b.memory(m.instance(&b.instance_id).map_or(crate::core::instance::DEFAULT_MEMORY_MB, |i| i.memory_mb)));
            (n > 0).then(|| (u32::try_from(n).unwrap_or(u32::MAX), each))
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

    /// Whether the current page has a search or filter for Ctrl+F to reach.
    pub fn can_search(&self, cx: &App) -> bool {
        match self.page {
            Page::Grid(_) => !self.onboarding_shown(cx),
            Page::Accounts => super::accounts::has_filter(cx),
            Page::Chat => super::chat::has_filter(cx),
            Page::Instance(_) => false,
        }
    }

    /// How far above the content's bottom edge the undo notices sit, so they clear what the page keeps there.
    fn toast_lift(&self, _cx: &App) -> f32 {
        match self.page {
            Page::Chat => chat::COMPOSER + space::S,
            Page::Grid(_) if self.grid.picks.set.len() > 1 => selection::BOTTOM + selection::BAR + space::S,
            _ => space::XL,
        }
    }

    fn onboarding_shown(&self, cx: &App) -> bool {
        let m = model::get(cx).read(cx);
        m.accounts.is_empty() || (m.batches.is_empty() && m.instances.is_empty())
    }

    fn onboarding(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = Theme::get(cx);
        let m = model::get(cx);
        let m = m.read(cx);
        let (title, text, id, label, action, prism): (String, String, &str, String, Box<dyn Action>, Option<(String, Box<dyn Action>)>) = if m.accounts.is_empty() {
            (
                tr("Hier ist es noch still"),
                tr("Füg einen Account hinzu und starte Minecraft in einem Fenster."),
                "add-account",
                tr("Account hinzufügen"),
                Box::new(AddAccount),
                (self.prism.1 > 0).then(|| (tr("Oder Accounts aus Prism übernehmen"), Box::new(ImportPrismAccounts) as Box<dyn Action>)),
            )
        } else if m.batches.is_empty() && m.instances.is_empty() {
            (
                tr("Fehlt noch: eine Instanz"),
                tr("Eine Instanz legt Version, Loader und Mods fest. Spiel sie allein oder schick mehrere Accounts als Batch los."),
                "new-instance",
                tr("Instanz erstellen"),
                Box::new(NewInstanceToPlay(None)),
                (self.prism.0 > 0).then(|| (tr("Oder aus anderem Launcher importieren"), Box::new(ImportFromLauncher) as Box<dyn Action>)),
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
                .children(prism.map(|(label, action)| {
                    super::sheet::button("empty-prism", label, Some("download"), super::sheet::Look::Plain, false, false, &t).on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
                }))
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
        if if self.narrow_now { self.overlay } else { self.sidebar } {
            if let Some(key) = super::updates::offer_key(&super::model::get(cx).read(cx).updates) {
                self.seen_offer = Some(key);
            }
        }
        if window.focused(cx).is_none() || (matches!(self.page, Page::Grid(_)) && !self.onboarding_shown(cx) && self.focus.is_focused(window)) {
            self.focus_page(window, cx);
        }
        self.update_title(window, cx);
        let t = Theme::get(cx);
        let narrow = Self::narrow(window);
        if !matches!(self.page, Page::Grid(_)) {
            self.grid.hide();
            super::state::set_visible(Default::default(), cx);
        }
        let content = match self.page.clone() {
            Page::Accounts => super::accounts::page(window, cx).into_any_element(),
            Page::Chat => super::chat::page(window, cx).into_any_element(),
            Page::Instance(id) => super::instance_page::page(&id, window, cx),
            Page::Grid(batch) => match self.onboarding(cx) {
                Some(empty) => match model::get(cx).read(cx).error.clone() {
                    Some(error) => div().size_full().flex().flex_col().child(div().h(px(shell::TOOLBAR)).flex_none()).child(notice(error, &t, cx)).child(div().flex_1().min_h_0().child(empty)).into_any_element(),
                    None => empty,
                },
                None => {
                    let error = model::get(cx).read(cx).error.clone();
                    if let Filter::Need(need) = self.filter
                        && !model::get(cx).read(cx).needs(batch.as_deref()).iter().any(|(n, _)| *n == need)
                    {
                        self.filter = Filter::Attention;
                    }
                    let fit = self.fit_line(batch.as_deref(), cx).filter(|_| error.is_none());
                    let outages = super::needs::outages(batch.as_deref(), &t, cx);
                    let chips = super::needs::chips(self, batch.as_deref(), cx);
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(super::toolbar::render(self, batch.as_deref(), window, cx))
                        .children(error.map(|e| notice(e, &t, cx)))
                        .children(fit.map(|line| fit_note(line, &t)))
                        .children(outages)
                        .children(chips)
                        .child(div().relative().flex_1().min_h_0().child(super::grid::render(self, batch.as_deref(), window, cx)).children(super::grid::selection_bar(self, window, cx)))
                        .into_any_element()
                }
            },
        };
        let show_sidebar = self.sidebar && !narrow;
        let palette = super::palette::render(self, window, cx);
        let shortcuts = super::shortcuts::render(self, window, cx);
        let dropping = cx.has_active_drag().then(|| drop_hint(&t));
        let root = div()
            .id("main")
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .capture_any_mouse_down(|_, window, cx| window.defer(cx, |window, cx| super::theme::focus_moved(window, false, cx)))
            .capture_any_mouse_up(|_, window, cx| window.defer(cx, |window, cx| super::theme::focus_moved(window, false, cx)))
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.shortcuts && e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    return super::shortcuts::close(this, window, cx);
                }
                if !this.overlay || this.palette.is_some() {
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
                    this.remember(window, cx);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                match this.page {
                    Page::Grid(_) if this.can_search(cx) => {
                        this.search.update(cx, |s, cx| s.take_focus(window, cx));
                        cx.notify();
                    }
                    Page::Accounts => super::accounts::focus_filter(window, cx),
                    Page::Chat => super::chat::focus_filter(window, cx),
                    Page::Grid(_) | Page::Instance(_) => {}
                }
            }))
            .on_action(cx.listener(|this, _: &ClearSearch, window, cx| {
                this.search.update(cx, |s, cx| s.set_value("", window, cx));
                this.query.clear();
                this.grid.reset_scroll();
                window.focus(&this.grid.focus, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| super::palette::toggle(this, window, cx)))
            .on_action(cx.listener(|this, _: &ShowShortcuts, window, cx| super::shortcuts::toggle(this, window, cx)))
            .on_action(cx.listener(|this, a: &PickColor, window, cx| super::palette::colors(this, &a.0, window, cx)))
            .on_drag_move::<ExternalPaths>(|_, window, cx| {
                if window.last_input_was_keyboard() {
                    let position = window.mouse_position();
                    window.defer(cx, move |window, cx| {
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() }), cx);
                    });
                }
            })
            .on_drop(|paths: &ExternalPaths, _, cx| super::transfer::drop_paths(paths, cx))
            .on_action(|_: &ToggleFullscreen, window, _| window.toggle_fullscreen())
            .on_action(cx.listener(|this, _: &ImportPrismAccounts, window, cx| {
                this.go(Page::Accounts, window, cx);
                super::accounts::import_prism(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowAll, window, cx| this.go(Page::Grid(None), window, cx)))
            .on_action(cx.listener(|this, _: &ShowAccounts, window, cx| this.go(Page::Accounts, window, cx)))
            .on_action(cx.listener(|this, a: &ShowBatch, window, cx| this.go(Page::Grid(Some(a.0.clone())), window, cx)))
            .on_action(cx.listener(|this, a: &ShowInstance, window, cx| this.go(Page::Instance(a.0.clone()), window, cx)))
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
            .on_action(cx.listener(|this, a: &super::actions::ShowHistory, window, cx| {
                this.grid.toggle_history(&a.0);
                window.focus(&this.grid.focus, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, window, cx| {
                if this.grid.clear_selection() {
                    cx.notify();
                } else if window.is_fullscreen() {
                    window.toggle_fullscreen();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &StartSelected, window, cx| this.on_chosen_or_shown(|k| Box::new(StartMany(k)), |b| Box::new(StartBatch(b)), window, cx)))
            .on_action(cx.listener(|this, _: &StopSelected, window, cx| this.on_chosen_or_shown(|k| Box::new(StopMany(k)), |b| Box::new(StopBatch(b)), window, cx)))
            .on_action(cx.listener(|this, _: &EditShownBatch, window, cx| match this.shown_batch(cx) {
                Some(batch) => window.dispatch_action(Box::new(EditBatch(batch)), cx),
                None => cx.propagate(),
            }))
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
            .child(div().relative().flex_1().min_w_0().h_full().bg(t.window).child(content).when(!(narrow && self.overlay), |d| d.child(super::toolbar::toggle(self, &t, cx))).child(div().absolute().left_0().right_0().bottom(px(self.toast_lift(cx))).child(super::undo::toasts(cx))))
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
            .when(narrow && self.overlay, |d| d.child(super::toolbar::toggle(self, &t, cx)))
            .children(dropping)
            .children(palette)
            .children(shortcuts);
        let root = super::perf::frame_end(root);
        super::chrome::frame("Manycraft", root, window.is_fullscreen(), &t)
    }
}

struct MainHandle(AnyWindowHandle);

impl Global for MainHandle {}

fn driven_size() -> Option<(f32, f32)> {
    let value = std::env::var("MANYCRAFT_DRIVE_SIZE").ok()?;
    let (w, h) = value.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

const FULLSCREEN_SETTLES: Duration = Duration::from_millis(150);
const SAVE_AFTER: Duration = Duration::from_millis(500);

fn bind_keys(cx: &mut App) {
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    let fullscreen = if cfg!(target_os = "macos") { "ctrl-cmd-f" } else { "f11" };
    let k = |key: &str| -> &'static str { Box::leak(format!("{m}-{key}").into_boxed_str()) };
    cx.bind_keys([
        KeyBinding::new(k("b"), ToggleSidebar, Some("MainWindow")),
        KeyBinding::new(k("z"), Undo, Some("MainWindow")),
        KeyBinding::new(k("f"), FocusSearch, Some("MainWindow")),
        KeyBinding::new(k("shift-c"), OpenChat, Some("MainWindow")),
        KeyBinding::new(k("shift-g"), ShowAll, Some("MainWindow")),
        KeyBinding::new(k("shift-u"), ShowAccounts, Some("MainWindow")),
        KeyBinding::new(k("k"), OpenPalette, Some("MainWindow")),
        KeyBinding::new(k("/"), ShowShortcuts, Some("MainWindow")),
        KeyBinding::new(k("o"), ImportBatch, Some("MainWindow")),
        KeyBinding::new(k("shift-n"), NewInstance, Some("MainWindow")),
        KeyBinding::new(k("e"), EditShownBatch, Some("MainWindow")),
        KeyBinding::new(fullscreen, ToggleFullscreen, Some("MainWindow")),
        KeyBinding::new("ctrl-alt-right", NextInstance, Some("MainWindow")),
        KeyBinding::new("ctrl-alt-left", PreviousInstance, Some("MainWindow")),
        KeyBinding::new("ctrl-alt-down", NextAttention, Some("MainWindow")),
        KeyBinding::new(k("a"), SelectAll, Some("InstanceGrid")),
        KeyBinding::new("escape", ClearSelection, Some("InstanceGrid")),
        KeyBinding::new("tab", super::palette::PaletteNext, Some("Palette")),
        KeyBinding::new("shift-tab", super::palette::PalettePrevious, Some("Palette")),
        KeyBinding::new(k("r"), StartSelected, Some("InstanceGrid")),
        KeyBinding::new(k("."), StopSelected, Some("InstanceGrid")),
        KeyBinding::new(k("shift-r"), RestartSelected, Some("InstanceGrid")),
        KeyBinding::new(k("j"), ReconnectSelected, Some("InstanceGrid")),
        KeyBinding::new(k("enter"), MessageSelected, Some("InstanceGrid")),
        KeyBinding::new("escape", ClearSearch, Some("Toolbar")),
        KeyBinding::new(k("q"), Quit, None),
    ]);
}

fn rect_of(b: Bounds<Pixels>) -> Rect {
    let at = |p: Pixels| f32::from(p).round();
    Rect { x: at(b.origin.x) as i32, y: at(b.origin.y) as i32, w: at(b.size.width).max(1.) as u32, h: at(b.size.height).max(1.) as u32 }
}

fn restored(saved: &Saved, cx: &App) -> Option<(WindowBounds, DisplayId)> {
    let frame = saved.frame?;
    let displays = cx.displays();
    let works: Vec<Rect> = displays.iter().map(|d| rect_of(d.visible_bounds())).collect();
    let prefer = saved.display.as_deref().and_then(|u| displays.iter().position(|d| d.uuid().is_ok_and(|id| id.to_string() == u)));
    let bounds = Bounds::new(point(px(frame.x), px(frame.y)), size(px(frame.w), px(frame.h)));
    let asked = rect_of(bounds);
    let (fitted, at) = place::restore(asked, &works, prefer)?;
    let bounds = if fitted == asked { bounds } else { Bounds::new(point(px(fitted.x as f32), px(fitted.y as f32)), size(px(fitted.w as f32), px(fitted.h as f32))) };
    Some((if saved.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) }, displays[at].id()))
}

/// Opens the main window where it was last, else centred.
pub fn open(cx: &mut App) {
    bind_keys(cx);
    let driven = crate::core::drive::on();
    let (w, h) = driven_size().filter(|_| driven).unwrap_or((1280., 800.));
    let saved = winstate::load();
    let (window_bounds, display_id) = match restored(&saved, cx) {
        Some((bounds, display)) => (bounds, Some(display)),
        None => (WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx)), None),
    };
    let maximized = matches!(window_bounds, WindowBounds::Maximized(_));
    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        display_id,
        show: !(maximized && !driven) && !crate::core::drive::away(),
        window_min_size: Some(size(px(640.), px(420.))),
        app_id: Some("io.github.schrumbo.manycraft".into()),
        ..Default::default()
    };
    let options = super::chrome::window_options(options, "Manycraft");
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::refresh(window, cx);
        if crate::core::drive::away() {
            super::drive::send_away(window);
        }
        if maximized && !driven {
            window.activate_window();
        }
        window.on_window_should_close(cx, close_requested);
        cx.new(|cx| MainWindow::new(saved, window, cx))
    }) {
        cx.set_global(MainHandle(handle));
    }
}

/// The monitor the main window is on.
pub fn display(cx: &mut App) -> Option<DisplayId> {
    handle(cx)?.update(cx, |_, window, cx| window.display(cx).map(|d| d.id())).ok().flatten()
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

/// Where the main window is and whether it is maximized or fullscreen; nothing while it is minimized.
pub fn place(cx: &mut App) -> Option<(Rect, Mode)> {
    handle(cx)?.update(cx, |_, window, _| native::place_of(window)).ok().flatten()
}

/// Gets the main window out of the way of the game that takes its place, ready to come back at `place`. A driven window stays, so agents keep seeing it.
pub fn hide(place: Option<Place>, cx: &mut App) {
    if crate::core::drive::fenced("hide", "main window") {
        return;
    }
    if let Some(h) = handle(cx) {
        let _ = h.update(cx, |_, window, _| native::hide(window, place));
    }
}

/// Closing the main window hides it on macOS, where the app stays in the Dock; elsewhere the app core decides.
fn close_requested(window: &mut Window, cx: &mut App) -> bool {
    if cfg!(target_os = "macos") {
        native::hide(window, None);
    } else {
        cx.defer(super::state::close_window);
    }
    false
}

/// Runs `f` in the main window once the current update is over.
pub fn in_main(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    cx.defer(move |cx| {
        if let Some(h) = handle(cx) {
            let _ = h.update(cx, |_, window, cx| f(window, cx));
        }
    });
}

/// Puts the main window away behind the notification-area icon, or minimizes it where the system has no notification area.
pub fn stow(cx: &mut App) {
    let tray = super::tray::show(cx);
    let Some(h) = handle(cx) else { return };
    let _ = h.update(cx, |_, window, _| if tray { native::stow(window) } else { window.minimize_window() });
}

/// Brings the main window back from the notification area and takes the icon down; a driven window comes back without taking the focus.
pub fn unstow(cx: &mut App) {
    if !super::tray::shown(cx) {
        return;
    }
    super::tray::hide(cx);
    let Some(h) = handle(cx) else { return };
    let _ = h.update(cx, |_, window, cx| if crate::core::drive::on() { native::unhide_quietly(window) } else { native::show(window, None, cx) });
}

/// Brings the main window back at `place`, where the game was.
pub fn show(place: Option<Place>, cx: &mut App) {
    if crate::core::drive::fenced("show", "main window") {
        return;
    }
    super::tray::hide(cx);
    let Some(h) = handle(cx) else { return };
    let _ = h.update(cx, |_, window, cx| {
        let fullscreen = window.is_fullscreen();
        native::show(window, place.filter(|_| !fullscreen), cx);
        let Some(place) = place else { return };
        let elsewhere = fullscreen
            && place.mode == Mode::Fullscreen
            && native::place_of(window).is_some_and(|(frame, _)| !crate::core::place::same_work_area(frame, place.frame, &crate::core::game_window::work_areas()));
        if elsewhere {
            window.toggle_fullscreen();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FULLSCREEN_SETTLES).await;
                let _ = h.update(cx, |_, window, cx| {
                    if model::get(cx).read(cx).playing.is_none() && !window.is_fullscreen() {
                        native::put(window, place);
                        window.toggle_fullscreen();
                    }
                });
            })
            .detach();
            return;
        }
        if (place.mode == Mode::Fullscreen) != fullscreen {
            window.toggle_fullscreen();
        }
        if fullscreen && place.mode != Mode::Fullscreen {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FULLSCREEN_SETTLES).await;
                let _ = h.update(cx, |_, window, cx| {
                    if model::get(cx).read(cx).playing.is_none() {
                        native::put(window, place);
                    }
                });
            })
            .detach();
        }
    });
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

/// Shows the page of instance `id`, as after creating or importing it; stays put while the palette or a sheet is open.
/// Returns whether the page is shown now.
pub fn show_instance(id: &str, cx: &mut App) -> bool {
    let Some(h) = handle(cx) else { return false };
    let id = id.to_owned();
    let mut shown = false;
    let _ = h.update(cx, |view, window, cx| {
        let Ok(root) = view.downcast::<gpui_kit::base::Root>() else { return };
        let content = root.read(cx).view().clone();
        if let Ok(view) = content.downcast::<MainWindow>() {
            view.update(cx, |this, cx| {
                if this.palette.is_some() || window.has_active_dialog(cx) {
                    return;
                }
                this.go(Page::Instance(id), window, cx);
                shown = true;
            });
        }
    });
    shown
}

/// Moves from the page of instance `gone` to the page of `to`, or to all games when `to` is gone too; any other page stays.
pub fn leave_instance(gone: &str, to: &str, cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let (gone, to) = (gone.to_owned(), to.to_owned());
    let _ = h.update(cx, |view, window, cx| {
        let Ok(root) = view.downcast::<gpui_kit::base::Root>() else { return };
        let content = root.read(cx).view().clone();
        if let Ok(view) = content.downcast::<MainWindow>() {
            view.update(cx, |this, cx| {
                if matches!(&this.page, Page::Instance(id) if *id == gone) {
                    let kept = model::get(cx).read(cx).instance(&to).is_some();
                    this.go(if kept { Page::Instance(to) } else { Page::Grid(None) }, window, cx);
                }
            });
        }
    });
}

/// What the window says while files are dragged onto it.
fn drop_hint(t: &Theme) -> AnyElement {
    let text = tr("Zum Importieren loslassen");
    div()
        .id("drop-hint")
        .mark("notice", text.clone())
        .absolute()
        .inset_0()
        .bg(t.accent.opacity(tint::DROP))
        .border_2()
        .border_color(t.accent)
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(space::XS))
        .child(icon("download", iconsize::L, t.accent_text))
        .child(div().text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(text))
        .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Modpack von Modrinth, Export oder Instanz aus Prism")))
        .into_any_element()
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
