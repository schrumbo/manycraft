//! The title bar over both columns: the sidebar toggle, back and forward, where the window is, the palette, what runs in
//! the background and what needs the person. Every pixel that is not a control drags the window.

use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::view::commands::Command;
use crate::view::icons::{Icon, icon};
use crate::view::install::{self, Stage};
use crate::view::instance::Tab;
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::text::{caption, line};
use crate::view::kit::{Bounds, ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, frame, iconsize, space};
use crate::view::ui::{Filter, Page, Pane};

const MAC_LIGHTS: f32 = 78.;
const PALETTE_W: f32 = 340.;
const CRUMB_CHAR: f32 = 7.;

/// One step of the breadcrumb; the ones before the last lead back.
#[derive(Clone, Debug, PartialEq)]
pub struct Crumb {
    pub label: String,
    pub to: Option<Page>,
}

/// The names the breadcrumb needs from the model.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pub instance: Option<String>,
    pub batch: Option<String>,
    pub tab: String,
}

fn crumb(label: impl Into<String>, to: Option<Page>) -> Crumb {
    Crumb { label: label.into(), to }
}

/// Where `page` is, from the top: its section, then what is open in it.
pub fn trail(page: &Page, names: &Names) -> Vec<Crumb> {
    let library = || crumb(tr("Instanzen"), Some(Page::Library));
    let named = |fallback: String| names.instance.clone().unwrap_or(fallback);
    match page {
        Page::Start => vec![crumb(tr("Start"), None)],
        Page::Library => vec![crumb(tr("Instanzen"), None)],
        Page::Discover => vec![crumb(tr("Entdecken"), None)],
        Page::Grid(Filter::All) => vec![crumb(tr("Alle Batches"), None)],
        Page::Grid(Filter::Attention) => vec![crumb(tr("Alle Batches"), Some(Page::Grid(Filter::All))), crumb(tr("Brauchen dich"), None)],
        Page::Grid(Filter::Batch(_)) => vec![crumb(names.batch.clone().unwrap_or_else(|| tr("Batch")), None)],
        Page::Instance(id) => vec![library(), crumb(named(tr("Instanz")), Some(Page::Instance(id.clone()))), crumb(names.tab.clone(), None)],
        Page::Browse(id, _) => vec![library(), crumb(named(tr("Instanz")), Some(Page::Instance(id.clone()))), crumb(tr("Hinzufügen"), None)],
        Page::Chat => vec![crumb(tr("Chat"), None)],
        Page::Settings(Pane::General) => vec![crumb(tr("Einstellungen"), None)],
        Page::Settings(Pane::Accounts) => vec![crumb(tr("Einstellungen"), Some(Page::Settings(Pane::General))), crumb(tr("Accounts"), None)],
    }
}

fn width_of(c: &Crumb) -> f32 {
    c.label.chars().count() as f32 * CRUMB_CHAR + space::L
}

/// `crumbs` cut to what fits in `room`: the middle ones fold into one "…" so the first and the last remain.
pub fn fit(mut crumbs: Vec<Crumb>, room: f32) -> Vec<Crumb> {
    if crumbs.len() > 2 && crumbs.iter().map(width_of).sum::<f32>() > room {
        let last = crumbs.pop();
        let first = crumbs.remove(0);
        return [Some(first), Some(crumb("…", None)), last].into_iter().flatten().collect();
    }
    crumbs
}

/// The title bar.
#[derive(PartialEq)]
pub struct TitleBar;

impl Component for TitleBar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let page = ui.page.read().clone();
        let (back, forward) = {
            let h = ui.hist.read();
            (h.can_back(), h.can_forward())
        };
        let window = ui.window.read().0;
        let names = {
            let m = store.model.read();
            let instance = match &page {
                Page::Instance(id) | Page::Browse(id, _) => m.instance(id).map(|i| i.name.clone()),
                _ => None,
            };
            let batch = match &page {
                Page::Grid(Filter::Batch(b)) => m.batch(b).map(|b| b.name.clone()),
                _ => None,
            };
            Names { instance, batch, tab: ui.instance.tab.read().label() }
        };
        let lights = if cfg!(target_os = "macos") { MAC_LIGHTS } else { 0. };
        let room = (window - lights - PALETTE_W - 2. * frame::TITLEBAR * 3.) / 2.;
        let crumbs = fit(trail(&page, &names), room.max(120.));

        let mut left = rect().horizontal().cross_align(Alignment::Center).spacing(space::XXS);
        left = left
            .child(Button::icon("titlebar.sidebar", Icon::Sidebar, tr("Seitenleiste")).plain().tip(Command::ToggleSidebar.tip()).on_press(move |_| Command::ToggleSidebar.run(store)))
            .child(Button::icon("titlebar.back", Icon::ChevronLeft, tr("Zurück")).plain().tip(Command::GoBack.tip()).disabled((!back).then(|| tr("Nichts, wohin es zurückgeht."))).on_press(move |_| Command::GoBack.run(store)))
            .child(Button::icon("titlebar.forward", Icon::ChevronRight, tr("Vor")).plain().tip(Command::GoForward.tip()).disabled((!forward).then(|| tr("Nichts, wohin es vorgeht."))).on_press(move |_| Command::GoForward.run(store)));
        let mut trail_row = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS).padding((0., space::S));
        let count = crumbs.len();
        for (i, c) in crumbs.into_iter().enumerate() {
            let last = i + 1 == count;
            if i > 0 {
                trail_row = trail_row.child(icon(&t, Icon::ChevronRight, iconsize::XS, t.text3));
            }
            trail_row = trail_row.child(Crumbs { crumb: c, last });
        }

        let mut right = rect().horizontal().cross_align(Alignment::Center).main_align(Alignment::End).spacing(space::S).padding((0., space::S));
        right = right.child(Activity);
        right = right.child(Attention);

        rect()
            .a11y_role(AccessibilityRole::Toolbar)
            .width(Size::fill())
            .height(Size::px(frame::TITLEBAR))
            .background(t.sidebar)
            .horizontal()
            .cross_align(Alignment::Center)
            .padding((0., 0., 0., space::S + lights))
            .window_drag()
            .child(left)
            .child(rect().width(Size::flex(1.)).child(trail_row))
            .child(Palette)
            .child(rect().width(Size::flex(1.)).child(right))
            .content(Content::flex())
    }
}

#[derive(PartialEq)]
struct Crumbs {
    crumb: Crumb,
    last: bool,
}

impl Component for Crumbs {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut hovered = use_state(|| false);
        let c = &self.crumb;
        let ink = if self.last { t.text1 } else if *hovered.read() && c.to.is_some() { t.text1 } else { t.text2 };
        let weight = if self.last { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL };
        let text = line(&t, c.label.clone(), font::BODY, ink).font_weight(weight).max_lines(1).text_overflow(TextOverflow::Ellipsis);
        let Some(to) = c.to.clone().filter(|_| !self.last) else {
            return rect().max_width(Size::px(220.)).child(text).into_element();
        };
        let mut page = store.ui.page;
        rect()
            .max_width(Size::px(220.))
            .cursor(CursorIcon::Pointer)
            .a11y_role(AccessibilityRole::Link)
            .a11y_alt(c.label.clone())
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_press(move |_| page.set(to.clone()))
            .child(text)
            .into_element()
    }
}

#[derive(PartialEq)]
struct Palette;

impl Component for Palette {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let spot = use_spot("titlebar.palette", "button", &tr("Suchen oder Befehl …"), true);
        let keys = Command::OpenPalette.keys();
        let mut field = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(tr("Suchen oder Befehl …"))
            .width(Size::px(PALETTE_W))
            .height(Size::px(frame::TITLEBAR - space::S * 1.5))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .background(if *hovered.read() { t.hover } else { t.raised })
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |_| Command::OpenPalette.run(store))
            .child(icon(&t, Icon::Search, iconsize::S, t.text3))
            .child(rect().width(Size::flex(1.)).child(line(&t, tr("Suchen oder Befehl …"), font::CALLOUT, t.text3).max_lines(1)))
            .content(Content::flex());
        if !keys.is_empty() {
            field = field.child(caption(&t, keys));
        }
        if let Some(border) = ring(&t, focus()) {
            field = field.border(border);
        }
        field
    }
}

/// The background work now: installs that run and bulk starts that wait to launch.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub state: String,
    pub cancel: TaskKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskKind {
    Install(u64),
    Start(Option<String>),
}

fn tasks(store: Store) -> Vec<Task> {
    let mut out = Vec::new();
    for e in store.ui.installs.read().iter().filter(|e| !matches!(e.stage, Stage::Failed(_))) {
        out.push(Task { id: format!("install.{}", e.id), name: e.name.clone(), state: e.state(), cancel: TaskKind::Install(e.id) });
    }
    let m = store.model.read();
    for b in &m.batches {
        if let Some(run) = m.run(Some(&b.id)) {
            out.push(Task { id: format!("start.{}", b.id), name: b.name.clone(), state: trf("Startet %lld/%lld", &[&run.launched, &run.total]), cancel: TaskKind::Start(Some(b.id.clone())) });
        }
    }
    out
}

#[derive(PartialEq)]
struct Activity;

impl Component for Activity {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut at = use_state(Bounds::default);
        let list = tasks(store);
        if list.is_empty() {
            return rect().into_element();
        }
        let n = list.len();
        let progress = list.iter().find_map(|task| match task.cancel {
            TaskKind::Install(id) => store.ui.installs.read().iter().find(|e| e.id == id).and_then(|e| match e.stage {
                Stage::Running(Some(p)) => Some(p),
                _ => None,
            }),
            TaskKind::Start(_) => None,
        });
        let stage = Stage::Running(progress.filter(|_| n == 1));
        let label = if n == 1 { tr("1 Aufgabe läuft") } else { trf("%lld Aufgaben laufen", &[&n]) };
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut spot = use_spot("titlebar.activity", "button", &label, true);
        let mut chip = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(label.clone())
            .height(Size::px(frame::TITLEBAR - space::M))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .corner_radius(t.shape.control)
            .background(if *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .cursor(CursorIcon::Pointer)
            .on_sized(move |e: Event<SizedEventData>| {
                at.set_if_modified(Bounds::of(&e));
                spot(e);
            })
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |_| open_tasks(store, *at.peek()))
            .child(install::ring(&t, &stage, iconsize::S))
            .child(caption(&t, n.to_string()).font_tabular());
        if let Some(border) = ring(&t, focus()) {
            chip = chip.border(border);
        }
        WithTip::new(Some(label), chip).into_element()
    }
}

fn open_tasks(store: Store, anchor: Bounds) {
    let items = tasks(store)
        .into_iter()
        .map(|task| {
            let id = task.id.clone();
            let kind = task.cancel.clone();
            Item::new(format!("activity.{id}"), trf("„%@“ abbrechen", &[&task.name]), move |_| match &kind {
                TaskKind::Install(i) => install::cancel(store, *i),
                TaskKind::Start(b) => store.dispatch(crate::app::event::Event::CancelStart(b.clone())),
            })
            .shortcut(task.state)
        })
        .collect::<Vec<_>>();
    if !items.is_empty() {
        menu::open(store.ui, Menu::under(items, anchor));
    }
}

#[derive(PartialEq)]
struct Attention;

impl Component for Attention {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let n = store.model.read().attention_count(None);
        if n == 0 {
            return rect().into_element();
        }
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut page = store.ui.page;
        let label = if n == 1 { tr("1 braucht dich") } else { trf("%lld brauchen dich", &[&n]) };
        let spot = use_spot("titlebar.attention", "button", &label, true);
        let mut chip = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(label.clone())
            .height(Size::px(frame::TITLEBAR - space::M))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .corner_radius(t.shape.control)
            .background(t.attention.with_af32(frame::NOTICE_WASH))
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |_| page.set(Page::Grid(Filter::Attention)))
            .child(dot(t.attention))
            .child(line(&t, label, font::CALLOUT, t.attention_text).font_weight(FontWeight::MEDIUM).max_lines(1));
        if let Some(border) = ring(&t, focus()) {
            chip = chip.border(border);
        }
        chip.into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(c: &[Crumb]) -> Vec<&str> {
        c.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn an_instance_page_leads_back_through_the_library() {
        let names = Names { instance: Some("Technik".into()), batch: None, tab: "Mods".into() };
        let t = trail(&Page::Instance("a".into()), &names);
        assert_eq!(labels(&t), ["Instanzen", "Technik", "Mods"]);
        assert_eq!(t[0].to, Some(Page::Library));
        assert_eq!(t[1].to, Some(Page::Instance("a".into())));
        assert_eq!(t[2].to, None);
    }

    #[test]
    fn a_long_trail_folds_its_middle_and_keeps_the_ends() {
        let names = Names { instance: Some("Ein sehr langer Name einer Instanz".into()), batch: None, tab: "Mods".into() };
        let t = fit(trail(&Page::Instance("a".into()), &names), 100.);
        assert_eq!(labels(&t), ["Instanzen", "…", "Mods"]);
        let t = fit(trail(&Page::Instance("a".into()), &names), 1000.);
        assert_eq!(t.len(), 3);
        assert_eq!(fit(trail(&Page::Start, &names), 10.).len(), 1);
    }

    #[test]
    fn the_accounts_pane_sits_under_the_settings() {
        let t = trail(&Page::ACCOUNTS, &Names::default());
        assert_eq!(t[0].to, Some(Page::Settings(Pane::General)));
        assert_eq!(t.len(), 2);
    }
}
