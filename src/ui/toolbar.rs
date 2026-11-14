//! The toolbar over the grid: sidebar toggle, filter, tile size, search, and starting or stopping what is shown.

use gpui_kit::component::{FocusableExt, Sizable};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{StartAll, StartBatch, StopAll, StopBatch, ToggleSidebar};
use super::drive::Mark;
use super::main_window::{Filter, FilterAll, FilterAttention, FilterRunning, FilterStopped, MainWindow, TileFit, TileLarge, TileMedium, TileSize, TileSmall};
use super::palette::{hint, menu_row, shortcut};
use super::model;
use super::motion::Soft;
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tile, tint};
use super::widgets::{KeyNav, icon, tip};
use crate::core::i18n::{tr, trf};

/// The leading padding of a page's top bar that puts the sidebar symbol on the grid's edge.
pub fn lead() -> f32 {
    tile::INSET - (control::M - iconsize::M) / 2.
}

fn tabular() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

/// The room a page's top bar keeps free for the sidebar toggle.
pub fn slot() -> Div {
    div().size(px(control::M)).flex_none()
}

/// The sidebar toggle, drawn once by the main window at the same place on every page.
pub fn toggle(this: &MainWindow, t: &Theme, cx: &App) -> Div {
    let shown = if this.narrow_now { this.overlay } else { this.sidebar };
    let hint = hint(if shown { tr("Seitenleiste ausblenden") } else { tr("Seitenleiste einblenden") }, &ToggleSidebar, cx);
    div().absolute().occlude().top(px((shell::TOOLBAR - control::M) / 2.)).left(px(lead() + if this.narrow_now && this.overlay { shell::SIDEBAR } else { 0. })).child(icon_button("toggle-sidebar", "panel-left", hint, t).on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)))
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

fn text_w(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * shell::GLYPH
}

struct Widths {
    fixed: f32,
    segments: f32,
    menu: f32,
    labelled: f32,
    icons: f32,
}

#[derive(Debug, PartialEq)]
struct Plan {
    search: f32,
    menu: bool,
    icons: bool,
}

fn plan(room: f32, w: &Widths) -> Plan {
    for (menu, icons) in [(false, false), (true, false), (true, true)] {
        let rest = w.fixed + if menu { w.menu } else { w.segments } + if icons { w.icons } else { w.labelled };
        if let Some(search) = [shell::SEARCH, shell::SEARCH_MID, shell::SEARCH_MIN].into_iter().find(|s| rest + s <= room) {
            return Plan { search, menu, icons };
        }
    }
    Plan { search: shell::SEARCH_MIN, menu: true, icons: true }
}

/// Draws the toolbar for the grid of `batch`, or of all batches.
pub fn render(this: &mut MainWindow, batch: Option<&str>, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = Theme::get(cx);
    let model = model::get(cx);
    let m = model.read(cx);
    let title = batch.and_then(|b| m.batch(b)).map(|b| b.name.clone()).unwrap_or_else(|| tr("Alle"));
    let title_w = title.chars().count() as f32 * font::TITLE3;
    let scope: Vec<_> = m.instances.values().filter(|i| batch.is_none_or(|b| i.batch_id == b)).collect();
    let running = scope.iter().filter(|i| i.phase.is_active() && !i.stopping).count();
    let startable = scope.iter().filter(|i| !i.phase.is_active()).count();
    let attention = m.attention_count(batch);
    let count = |f: Filter| scope.iter().filter(|i| super::grid::matches(i, None, f)).count();
    let (start, stop): (Box<dyn Action>, Box<dyn Action>) = match batch {
        Some(b) => (Box::new(StartBatch(b.to_owned())), Box::new(StopBatch(b.to_owned()))),
        None => (Box::new(StartAll), Box::new(StopAll)),
    };
    let filters = [(Filter::All, tr("Alle")), (Filter::Attention, tr("Brauchen dich")), (Filter::Running, tr("Läuft")), (Filter::Stopped, tr("Gestoppt"))];
    let picked_label = filters.iter().find(|(f, _)| *f == this.filter).map(|(_, l)| l.clone()).unwrap_or_default();
    let picked_count = count(this.filter);
    let counts: Vec<(Filter, String, usize)> = filters.iter().map(|(f, label)| (*f, label.clone(), count(*f))).collect();
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
    let start_label = if batch.is_some() { tr("Starten") } else { tr("Alle starten") };
    let stop_label = if batch.is_some() { tr("Stoppen") } else { tr("Alle stoppen") };
    let segment_w = |label: &str, n: usize, counted: bool| space::S * 2. + text_w(label, font::CALLOUT) + if counted { space::XS + text_w(&n.to_string(), font::CAPTION) } else { 0. };
    let segments = space::XXS * 2.
        + space::XXS * (counts.len() - 1) as f32
        + counts.iter().map(|(f, label, n)| segment_w(label, *n, *n > 0 || *f != Filter::Attention)).sum::<f32>();
    let suffix = if attention > 0 && this.filter != Filter::Attention { space::XS + text_w(&trf("%lld brauchen dich", &[&attention]), font::CAPTION) } else { 0. };
    let menu = space::S * 2. + text_w(&picked_label, font::CALLOUT) + space::XS + text_w(&picked_count.to_string(), font::CAPTION) + suffix + space::XS + iconsize::XS;
    let labelled = |label: &str| space::M * 2. + iconsize::S + space::XS + text_w(label, font::BODY);
    let widths = Widths {
        fixed: lead() + tile::INSET + control::M + shell::TITLE.min(title_w) + space::S + space::XS + (space::XS * 2. + iconsize::M + space::XXS + iconsize::XS) + space::S + space::S * 9.,
        segments,
        menu,
        labelled: labelled(&start_label) + labelled(&stop_label) + space::S,
        icons: control::M * 2. + space::S,
    };
    let Plan { search: search_w, menu: compact_filter, icons: compact } = plan(this.content_width(window), &widths);
    let filter = if compact_filter {
        let current = this.filter;
        let grid_focus = this.grid.focus.clone();
        Button::new("filter")
            .ghost()
            .focus_ring(super::theme::rings(cx))
            .small()
            .h(px(control::M))
            .px(px(space::S))
            .gap(px(space::XS))
            .child(div().text_size(px(font::CALLOUT)).font_weight(FontWeight::MEDIUM).text_color(t.primary).child(picked_label))
            .child(div().text_size(px(font::CAPTION)).font_weight(FontWeight::SEMIBOLD).font_features(tabular()).text_color(if this.filter == Filter::Attention && attention > 0 { t.attention_text } else { t.secondary }).child(picked_count.to_string()))
            .when(attention > 0 && this.filter != Filter::Attention, |b| {
                b.child(div().text_size(px(font::CAPTION)).font_weight(FontWeight::SEMIBOLD).font_features(tabular()).text_color(t.attention_text).child(trf("%lld brauchen dich", &[&attention])))
            })
            .child(icon("chevron-down", iconsize::XS, t.tertiary))
            .tooltip(tr("Filter"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.action_context(grid_focus.clone());
                for (f, label, n) in counts.clone() {
                    let action: Box<dyn Action> = match f {
                        Filter::All => Box::new(FilterAll),
                        Filter::Attention => Box::new(FilterAttention),
                        Filter::Running => Box::new(FilterRunning),
                        Filter::Stopped => Box::new(FilterStopped),
                    };
                    let loud = f == Filter::Attention && n > 0;
                    menu = menu.menu_element_with_check(current == f, action, move |_, cx| {
                        let t = Theme::get(cx);
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap(px(space::L))
                            .child(div().flex_1().child(label.clone()))
                            .child(div().flex_none().font_features(tabular()).when(loud, |d| d.font_weight(FontWeight::SEMIBOLD)).text_color(if loud { t.attention_text } else { t.tertiary }).child(n.to_string()))
                            .into_any_element()
                    });
                }
                menu
            })
            .into_any_element()
    } else {
        segmented(
            "filter",
            filter_segments,
            move |n, this, _, _| {
                this.filter = picks[n];
                this.grid.reset_scroll();
            },
            &t,
            cx,
        )
        .into_any_element()
    };
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
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
            let row = |label: &str, action: &dyn Action| menu_row(tr(label), shortcut(action, cx), false);
            let (small, medium, large, fit) = (row("Klein", &TileSmall), row("Mittel", &TileMedium), row("Groß", &TileLarge), row("Einpassen", &TileFit));
            menu.action_context(grid_focus.clone())
                .label(tr("Kachelgröße"))
                .menu_element_with_check(current == TileSize::Small, Box::new(TileSmall), small)
                .menu_element_with_check(current == TileSize::Medium, Box::new(TileMedium), medium)
                .menu_element_with_check(current == TileSize::Large, Box::new(TileLarge), large)
                .separator()
                .menu_element_with_check(current == TileSize::Fit, Box::new(TileFit), fit)
        });
    let roomy = search_w >= shell::SEARCH;
    if this.search_roomy != roomy {
        this.search_roomy = roomy;
        let hint = if roomy { tr("Name, Batch, Server, Status") } else { tr("Suchen") };
        this.search.update(cx, |s, cx| s.set_placeholder(hint, window, cx));
    }
    let search = div()
        .key_context("Toolbar")
        .w(px(search_w))
        .min_w(px(shell::SEARCH_FLOOR))
        .child(Input::new(&this.search).small().cleanable(true).prefix(icon("search", iconsize::S, t.tertiary)));
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
        .child(slot())
        .child(
            div()
                .id("toolbar-title")
                .mark("heading", title.clone())
                .min_w(px(shell::TITLE.min(title_w)))
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
        .child(action_button("start-scope", start_label.clone(), "play", true, startable == 0, compact, start, &t).tooltip(tip(format!("{start_label} · {}", if startable == 0 { tr("Alle sind schon wach") } else { trf("%lld gestoppte starten", &[&startable]) }))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Plan, Widths, plan};
    use crate::ui::theme::{font, shell, space};

    fn widths(filter_digits: usize, wide_labels: bool) -> Widths {
        let digits = filter_digits as f32 * font::CAPTION * shell::GLYPH;
        let label = if wide_labels { 14. } else { 8. };
        Widths {
            fixed: 200.,
            segments: 4. * (space::S * 2. + label * font::CALLOUT * shell::GLYPH + space::XS + digits) + 10.,
            menu: 120. + digits * 2.,
            labelled: 2. * (space::M * 2. + 18. + label * font::BODY * shell::GLYPH),
            icons: 64.,
        }
    }

    fn need(w: &Widths, p: &Plan) -> f32 {
        w.fixed + if p.menu { w.menu } else { w.segments } + if p.icons { w.icons } else { w.labelled } + p.search
    }

    #[test]
    fn what_the_plan_picks_always_fits() {
        for digits in 1..=5 {
            for wide in [false, true] {
                let w = widths(digits, wide);
                let smallest = Plan { search: shell::SEARCH_MIN, menu: true, icons: true };
                for room in (400..2000).step_by(7) {
                    let p = plan(room as f32, &w);
                    assert!(need(&w, &p) <= room as f32 || p == smallest, "room {room} digits {digits}");
                }
            }
        }
    }

    #[test]
    fn it_degrades_in_order() {
        let w = widths(3, true);
        let mut last = 0;
        for room in (400..2000).rev() {
            let p = plan(room as f32, &w);
            let stage = p.menu as u8 + p.icons as u8;
            assert!(stage >= last, "a stage never comes back while shrinking, room {room}");
            assert!(!p.icons || p.menu, "the filter collapses before the buttons become icons");
            last = stage;
        }
        assert_eq!(plan(2000., &w), Plan { search: shell::SEARCH, menu: false, icons: false });
    }

    #[test]
    fn the_search_field_shrinks_before_the_filter_collapses() {
        let w = widths(3, false);
        let full = w.fixed + w.segments + w.labelled;
        assert_eq!(plan(full + shell::SEARCH, &w).search, shell::SEARCH);
        assert_eq!(plan(full + shell::SEARCH_MID, &w), Plan { search: shell::SEARCH_MID, menu: false, icons: false });
        assert_eq!(plan(full + shell::SEARCH_MIN, &w), Plan { search: shell::SEARCH_MIN, menu: false, icons: false });
        assert!(plan(full + shell::SEARCH_MIN - 1., &w).menu);
    }
}
