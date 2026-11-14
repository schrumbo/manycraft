//! The toolbar over the grid: sidebar toggle, filter, tile size, search, and starting or stopping what is shown.

use gpui_kit::component::{FocusableExt, Sizable};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{StartAll, StartBatch, StopAll, StopBatch, ToggleSidebar};
use super::drive::Mark;
use super::main_window::{Filter, MainWindow, TileFit, TileLarge, TileMedium, TileSize, TileSmall};
use super::model;
use super::motion::Soft;
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tile, tint};
use super::widgets::{KeyNav, icon, tip};
use crate::core::i18n::{tr, trf};

fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") { format!("⌘{key}") } else { format!("{}+{key}", tr("Strg")) }
}

/// The toolbar's leading padding that puts the sidebar symbol on the grid's edge.
fn lead() -> f32 {
    tile::INSET - (control::M - iconsize::M) / 2.
}

fn tabular() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

fn toggle(this: &MainWindow, t: &Theme) -> Stateful<Div> {
    let shown = if this.narrow_now { this.overlay } else { this.sidebar };
    let hint = format!("{} · {}", if shown { tr("Seitenleiste ausblenden") } else { tr("Seitenleiste einblenden") }, shortcut("B"));
    icon_button("toggle-sidebar", "panel-left", hint, t).on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx))
}

fn icon_button(id: &'static str, name: &'static str, hint: String, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .mark("button", hint.clone())
        .size(px(control::M))
        .rounded(px(radius::INNER))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(tint::HOVER), radius::INNER)
        .child(icon(name, iconsize::M, t.secondary))
        .tooltip(tip(hint))
}

struct Segment {
    id: String,
    label: String,
    count: usize,
    loud: bool,
    selected: bool,
}

fn segmented(id: &'static str, segments: Vec<Segment>, on_pick: impl Fn(usize, &mut MainWindow, &mut Window, &mut Context<MainWindow>) + 'static, t: &Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let on_pick = std::rc::Rc::new(on_pick);
    let items = segments.into_iter().enumerate().map(|(n, s)| {
        let pick = std::rc::Rc::clone(&on_pick);
        let count = if s.loud { t.attention_text } else if s.selected { t.secondary } else { t.tertiary };
        let counted = s.count > 0 || s.id != "filter-attention";
        div()
            .id(SharedString::from(s.id))
            .mark("segment", s.label.clone())
            .h(px(control::S))
            .px(px(space::S))
            .rounded(px(radius::INNER))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(space::XS))
            .text_size(px(font::CALLOUT))
            .when(s.selected, |d| d.bg(t.content).shadow_xs().font_weight(FontWeight::MEDIUM).text_color(t.primary))
            .when(!s.selected, |d| d.bg(t.raised).text_color(t.secondary))
            .cursor_pointer()
            .key_nav()
            .soft(t.ink.opacity(if s.selected { 0. } else { tint::HOVER }), radius::INNER)
            .child(s.label)
            .when(counted, |d| d.child(div().text_size(px(font::CAPTION)).font_weight(FontWeight::SEMIBOLD).font_features(tabular()).text_color(count).child(s.count.to_string())))
            .on_click(cx.listener(move |this, _, window, cx| {
                pick(n, this, window, cx);
                cx.notify();
            }))
    });
    div().id(id).p(px(space::XXS)).rounded(px(radius::CONTROL)).bg(t.raised).flex().flex_none().items_center().gap(px(space::XXS)).children(items)
}

fn action_button(id: &'static str, label: String, name: &'static str, prominent: bool, disabled: bool, compact: bool, action: Box<dyn Action>, t: &Theme) -> Stateful<Div> {
    let (fill, text) = if prominent { (t.accent, t.on_accent) } else { (t.raised, t.primary) };
    let mark = label.clone();
    let label = (!compact).then_some(label);
    let button = div()
        .id(id)
        .mark("button", mark)
        .h(px(control::M))
        .when(compact, |d| d.w(px(control::M)).justify_center())
        .when(!compact, |d| d.px(px(space::M)))
        .rounded(px(radius::INNER))
        .bg(fill)
        .text_color(text)
        .text_size(px(font::BODY))
        .font_weight(FontWeight::MEDIUM)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::XS));
    if disabled {
        return button.opacity(tint::DISABLED).cursor_not_allowed().child(icon(name, iconsize::S, text)).children(label);
    }
    button
        .cursor_pointer()
        .key_nav()
        .soft(if prominent { t.shade } else { t.ink.opacity(tint::HOVER) }, radius::INNER)
        .child(icon(name, iconsize::S, text))
        .children(label)
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
}

/// Draws the toolbar for the grid of `batch`, or of all batches.
pub fn render(this: &mut MainWindow, batch: Option<&str>, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = Theme::get(cx);
    let model = model::get(cx);
    let m = model.read(cx);
    let title = batch.and_then(|b| m.batch(b)).map(|b| b.name.clone()).unwrap_or_else(|| tr("Alle"));
    let scope: Vec<_> = m.instances.values().filter(|i| batch.is_none_or(|b| i.batch_id == b)).collect();
    let running = scope.iter().filter(|i| i.phase.is_active() && !i.stopping).count();
    let startable = scope.iter().filter(|i| !i.phase.is_active()).count();
    let attention = m.attention_count(batch);
    let count = |f: Filter| scope.iter().filter(|i| super::grid::matches(i, None, f, "")).count();
    let (start, stop): (Box<dyn Action>, Box<dyn Action>) = match batch {
        Some(b) => (Box::new(StartBatch(b.to_owned())), Box::new(StopBatch(b.to_owned()))),
        None => (Box::new(StartAll), Box::new(StopAll)),
    };
    let compact = this.content_width(window) < shell::COMPACT;
    let filters = [(Filter::All, tr("Alle")), (Filter::Attention, tr("Brauchen dich")), (Filter::Running, tr("Läuft")), (Filter::Stopped, tr("Gestoppt"))];
    let filter_segments = filters
        .iter()
        .map(|(f, label)| Segment {
            id: format!("filter-{f:?}").to_lowercase(),
            label: label.clone(),
            count: count(*f),
            loud: *f == Filter::Attention && attention > 0,
            selected: this.filter == *f,
        })
        .collect();
    let picks = [Filter::All, Filter::Attention, Filter::Running, Filter::Stopped];
    let filter = segmented(
        "filter",
        filter_segments,
        move |n, this, _, _| {
            this.filter = picks[n];
            this.grid.reset_scroll();
        },
        &t,
        cx,
    );
    let current = this.size;
    let grid_focus = this.focus.clone();
    let size = Button::new("tile-size")
        .ghost()
        .focus_ring(super::theme::rings(cx))
        .small()
        .h(px(control::M))
        .px(px(space::XS))
        .gap(px(space::XXS))
        .child(icon("layout-grid", iconsize::M, t.secondary))
        .child(icon("chevron-down", iconsize::XS, t.tertiary))
        .tooltip(tr("Kachelgröße"))
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            menu.action_context(grid_focus.clone())
                .label(tr("Kachelgröße"))
                .menu_with_check(tr("Klein"), current == TileSize::Small, Box::new(TileSmall))
                .menu_with_check(tr("Mittel"), current == TileSize::Medium, Box::new(TileMedium))
                .menu_with_check(tr("Groß"), current == TileSize::Large, Box::new(TileLarge))
                .separator()
                .menu_with_check(tr("Einpassen"), current == TileSize::Fit, Box::new(TileFit))
        });
    let search = div()
        .key_context("Toolbar")
        .w(px(if compact { shell::SEARCH * 0.75 } else { shell::SEARCH }))
        .min_w(px(shell::SEARCH / 2.))
        .child(Input::new(&this.search).small().cleanable(true).prefix(icon("search", iconsize::S, t.tertiary)));
    let start_label = if batch.is_some() { tr("Starten") } else { tr("Alle starten") };
    let stop_label = if batch.is_some() { tr("Stoppen") } else { tr("Alle stoppen") };
    div()
        .h(px(shell::TOOLBAR))
        .flex_none()
        .pl(px(lead()))
        .pr(px(tile::INSET))
        .flex()
        .items_center()
        .gap(px(space::S))
        .border_b_1()
        .border_color(t.separator)
        .child(toggle(this, &t))
        .child(
            div()
                .id("toolbar-title")
                .mark("heading", title.clone())
                .min_w_0()
                .truncate()
                .text_size(px(font::TITLE3))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.primary)
                .child(title),
        )
        .child(div().w(px(space::S)).flex_none())
        .child(filter)
        .child(div().flex_1().min_w(px(space::S)))
        .child(size)
        .child(search)
        .child(div().w(px(space::XS)).flex_none())
        .child(action_button("stop-scope", stop_label.clone(), "square", false, running == 0, compact, stop, &t).tooltip(tip(format!("{stop_label} · {}", if running == 0 { tr("Nichts läuft") } else { trf("%lld laufende stoppen", &[&running]) }))))
        .child(action_button("start-scope", start_label.clone(), "play", true, startable == 0, compact, start, &t).tooltip(tip(format!("{start_label} · {}", if startable == 0 { tr("Alle laufen schon") } else { trf("%lld gestoppte starten", &[&startable]) }))))
        .into_any_element()
}
