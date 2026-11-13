//! The toolbar over the grid: sidebar toggle, filter, tile size, search, and starting or stopping what is shown.

use gpui_kit::component::Sizable;
use gpui_kit::component::input::Input;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{StartAll, StartBatch, StopAll, StopBatch, ToggleSidebar};
use super::drive::Mark;
use super::main_window::{Filter, MainWindow, TileSize};
use super::model;
use super::motion::Soft;
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tint};
use super::widgets::{KeyNav, icon, tip};
use crate::core::i18n::{tr, trf};

fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") { format!("⌘{key}") } else { format!("{}+{key}", tr("Strg")) }
}

/// A toolbar with only the sidebar toggle and `title`, for pages without grid controls.
pub fn bar(this: &MainWindow, title: String, t: &Theme) -> AnyElement {
    div()
        .h(px(shell::TOOLBAR))
        .flex_none()
        .px(px(space::M))
        .flex()
        .items_center()
        .gap(px(space::S))
        .border_b_1()
        .border_color(t.separator)
        .child(toggle(this, t))
        .child(div().min_w_0().truncate().text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
        .into_any_element()
}

fn toggle(this: &MainWindow, t: &Theme) -> Stateful<Div> {
    let hint = format!("{} · {}", if this.sidebar { tr("Seitenleiste ausblenden") } else { tr("Seitenleiste einblenden") }, shortcut("B"));
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
    label: Option<String>,
    icon: Option<&'static str>,
    count: Option<usize>,
    hint: Option<String>,
    selected: bool,
}

fn segmented(id: &'static str, segments: Vec<Segment>, on_pick: impl Fn(usize, &mut MainWindow, &mut Window, &mut Context<MainWindow>) + 'static, t: &Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let on_pick = std::rc::Rc::new(on_pick);
    let items = segments.into_iter().enumerate().map(|(n, s)| {
        let pick = std::rc::Rc::clone(&on_pick);
        let label = s.label.clone().or(s.hint.clone()).unwrap_or_default();
        div()
            .id(SharedString::from(s.id))
            .mark("segment", label)
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
            .when_some(s.icon, |d, name| d.child(icon(name, iconsize::S, if s.selected { t.primary } else { t.secondary })))
            .when_some(s.label, |d, label| d.child(label))
            .when_some(s.count.filter(|n| *n > 0), |d, n| d.child(div().text_size(px(font::CAPTION)).font_weight(FontWeight::SEMIBOLD).text_color(t.attention_text).child(n.to_string())))
            .when_some(s.hint, |d, hint| d.tooltip(tip(hint)))
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
    let running = scope.iter().filter(|i| i.phase.is_running() && !i.stopping).count();
    let startable = scope.iter().filter(|i| !i.phase.is_running()).count();
    let attention = m.attention_count(batch);
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
            label: Some(label.clone()),
            icon: None,
            count: (*f == Filter::Attention).then_some(attention),
            hint: None,
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
    let sizes = [
        (TileSize::Small, "S", tr("Kleine Kacheln")),
        (TileSize::Medium, "M", tr("Mittlere Kacheln")),
        (TileSize::Large, "L", tr("Große Kacheln")),
    ];
    let mut size_segments: Vec<Segment> = sizes
        .iter()
        .map(|(s, label, hint)| Segment { id: format!("size-{label}").to_lowercase(), label: Some((*label).into()), icon: None, count: None, hint: Some(format!("{hint} · {} / {}", shortcut("−"), shortcut("+"))), selected: this.size == *s })
        .collect();
    size_segments.push(Segment { id: "size-fit".into(), label: None, icon: Some("scan"), count: None, hint: Some(tr("Einpassen: so groß, dass möglichst alle Kacheln sichtbar sind")), selected: this.size == TileSize::Fit });
    let size_picks = [TileSize::Small, TileSize::Medium, TileSize::Large, TileSize::Fit];
    let size = segmented(
        "tile-size",
        size_segments,
        move |n, this, _, _| {
            this.size = size_picks[n];
            this.grid.reset_scroll();
        },
        &t,
        cx,
    );
    let search = div()
        .key_context("Toolbar")
        .w(px(shell::SEARCH))
        .min_w(px(shell::SEARCH / 2.))
        .child(Input::new(&this.search).small().cleanable(true).prefix(icon("search", iconsize::S, t.tertiary)));
    let start_label = if batch.is_some() { tr("Starten") } else { tr("Alle starten") };
    let stop_label = if batch.is_some() { tr("Stoppen") } else { tr("Alle stoppen") };
    div()
        .h(px(shell::TOOLBAR))
        .flex_none()
        .px(px(space::M))
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
        .when(!compact, |d| d.child(size))
        .child(search)
        .child(action_button("stop-scope", stop_label, "square", false, running == 0, compact, stop, &t).tooltip(tip(if running == 0 { tr("Nichts läuft") } else { trf("%lld laufende stoppen", &[&running]) })))
        .child(action_button("start-scope", start_label, "play", true, startable == 0, compact, start, &t).tooltip(tip(if startable == 0 { tr("Alle laufen schon") } else { trf("%lld gestoppte starten", &[&startable]) })))
        .into_any_element()
}
