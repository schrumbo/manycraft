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
use crate::view::native::frame::{self, Cover, Heard, Zone};
use crate::view::store::{Store, use_store};
use crate::view::theme::{WAITING, font, frame as size, iconsize, level, space};
use crate::view::ui::{Filter, Page, Pane};

use super::controls::{Controls, Edges};

const MAC_LIGHTS: f32 = 78.;
const PALETTE_W: f32 = 340.;
const PALETTE_ICON: f32 = size::TITLEBAR - space::S * 1.5;
const PALETTE_KEYS_FROM: f32 = 260.;
const PALETTE_LABEL_FROM: f32 = 150.;
const CRUMB_ROOM: f32 = 96.;
const ACTIVITY_W: f32 = 56.;
const CHIP_PAD: f32 = 36.;
const CRUMB_CHAR: f32 = 7.;
const CAPTIONS_LEFT: f32 = 3. * size::CAPTION_ROUND + 2. * size::CAPTION_GAP + space::S + space::M;

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
        Page::Wardrobe => vec![crumb(tr("Garderobe"), None)],
        Page::Grid(Filter::All) => vec![crumb(tr("Batches"), None)],
        Page::Grid(Filter::Attention) => vec![crumb(tr("Batches"), Some(Page::Grid(Filter::All))), crumb(tr("Brauchen dich"), None)],
        Page::Grid(Filter::Batch(_)) => vec![crumb(names.batch.clone().unwrap_or_else(|| tr("Batch")), None)],
        Page::Instance(id) => vec![library(), crumb(named(tr("Instanz")), Some(Page::Instance(id.clone()))), crumb(names.tab.clone(), None)],
        Page::Browse(id, _) => vec![library(), crumb(named(tr("Instanz")), Some(Page::Instance(id.clone()))), crumb(tr("Hinzufügen"), None)],
        Page::Settings(Pane::General) => vec![crumb(tr("Einstellungen"), None)],
        Page::Settings(Pane::Accounts) => vec![crumb(tr("Einstellungen"), Some(Page::Settings(Pane::General))), crumb(tr("Accounts"), None)],
    }
}

/// How wide the palette field is in a window `window` wide when the left side needs `left` and the right side `right`
/// beside it: its full width when both fit, else what remains, down to a lone glyph.
pub fn palette_width(window: f32, left: f32, right: f32) -> f32 {
    (window - 2. * left.max(right)).clamp(PALETTE_ICON, PALETTE_W)
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
        let (_, buttons_left) = frame::buttons();
        let captions = if buttons_left && frame::own_buttons(frame::chrome()) { CAPTIONS_LEFT } else { 0. };
        let mut chrome = use_state(frame::chrome);
        let own = frame::own_buttons(*chrome.read());
        let (attention, busy) = (store.model.read().attention_count(None), !tasks(store).is_empty());
        let chips = if attention > 0 { CHIP_PAD + CRUMB_CHAR * if attention == 1 { 14. } else { 16. } } else { 0. } + if busy { ACTIVITY_W } else { 0. };
        let own_w = if !own { 0. } else if cfg!(target_os = "windows") { 3. * size::CAPTION_W } else { CAPTIONS_LEFT };
        let left_need = lights + if buttons_left { own_w } else { 0. } + size::TITLEBAR * 3. + CRUMB_ROOM;
        let right_need = chips + if buttons_left { 0. } else { own_w } + space::S;
        let field = palette_width(window, left_need, right_need);
        let room = (window - field) / 2. - lights - captions - size::TITLEBAR * 3. - space::L;
        let crumbs = fit(trail(&page, &names), room.max(CRUMB_ROOM));
        let mut palette = ui.palette;
        let mut menu_open = ui.menu;
        use_hook(move || {
            let heard = frame::heard();
            spawn(async move {
                while let Ok(h) = heard.recv().await {
                    match h {
                        Heard::Chrome(c) => chrome.set_if_modified(c),
                        Heard::Bar => {
                            palette.set_if_modified(false);
                            menu_open.set_if_modified(None);
                        }
                    }
                }
            });
        });
        use_side_effect(move || frame::set_scale(*Platform::get().current_window().scale_factor.read() as f32));
        let sheet = ui.sheet.read().is_some();
        let menu = ui.menu.read().is_some();
        frame::set_cover(if menu { Cover::Menu } else if sheet { Cover::Sheet } else { Cover::None });
        let shut = menu.then_some(ui.menu);
        let mut bar_zone = frame::use_zone(Zone::Bar);
        let mut left_zone = frame::use_zone(Zone::Control);

        let mut left = rect().horizontal().cross_align(Alignment::Center).spacing(space::XXS).on_sized(move |e: Event<SizedEventData>| left_zone(&e));
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

        let mut right = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).main_align(Alignment::End).spacing(space::S).padding((0., space::S));
        right = right.child(Activity);
        right = right.child(Attention);

        let mut start = rect().width(Size::flex(1.)).height(Size::fill()).horizontal().cross_align(Alignment::Center).padding((0., 0., 0., if own && buttons_left { 0. } else { space::S + lights }));
        if own && buttons_left {
            start = start.child(Controls { chrome });
        }
        start = start.child(waiting(left, sheet, shut)).child(rect().width(Size::flex(1.)).child(waiting(trail_row, sheet, shut))).content(Content::flex());
        let mut end = rect().width(Size::flex(1.)).height(Size::fill()).horizontal().cross_align(Alignment::Center).child(rect().width(Size::flex(1.)).child(waiting(right, sheet, shut))).content(Content::flex());
        if own && !buttons_left {
            end = end.child(Controls { chrome });
        }
        let mut bar = rect()
            .a11y_role(AccessibilityRole::Toolbar)
            .width(Size::fill())
            .height(Size::px(size::TITLEBAR))
            .background(t.sidebar)
            .horizontal()
            .cross_align(Alignment::Center)
            .on_sized(move |e: Event<SizedEventData>| bar_zone(&e))
            .on_mouse_down(move |e: Event<MouseEventData>| {
                palette.set_if_modified(false);
                menu_open.set_if_modified(None);
                if cfg!(target_os = "linux") && e.button == Some(MouseButton::Right) {
                    frame::window_menu(e.global_location.x, e.global_location.y);
                }
            })
            .child(start.content(Content::flex()))
            .child(waiting(Palette { width: field }, sheet, shut))
            .child(end.content(Content::flex()))
            .child(Edges { chrome })
            .content(Content::flex());
        if frame::draws_drag() {
            bar = bar.window_drag();
        }
        bar
    }
}

/// `child` while a sheet waits for an answer: dimmed, and its clicks only move the window; while a menu is open its
/// first click only closes the menu.
fn waiting(child: impl IntoElement, sheet: bool, menu: Option<State<Option<Menu>>>) -> Element {
    let cover = |at: u8| {
        rect()
            .position(Position::new_absolute().left(0.).top(0.))
            .layer(Layer::OverlayLevel(at))
            .width(Size::fill())
            .height(Size::fill())
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
    };
    let cover = if sheet {
        let c = cover(level::SHEET).on_pointer_press(|e: Event<PointerEventData>| e.stop_propagation());
        Some(if frame::draws_drag() { c.window_drag() } else { c })
    } else {
        menu.map(|mut menu| {
            cover(level::MENU).on_pointer_press(move |e: Event<PointerEventData>| {
                e.stop_propagation();
                menu.set(None);
            })
        })
    };
    rect().opacity(if sheet { WAITING } else { 1. }).child(child).maybe_child(cover).into_element()
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
        let mut zone = frame::use_zone(if self.crumb.to.is_some() && !self.last { Zone::Control } else { Zone::Bar });
        let c = &self.crumb;
        let ink = if self.last { t.text1 } else if *hovered.read() && c.to.is_some() { t.text1 } else { t.text2 };
        let weight = if self.last { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL };
        let text = line(&t, c.label.clone(), font::BODY, ink).font_weight(weight).max_lines(1).text_overflow(TextOverflow::Ellipsis);
        let Some(to) = c.to.clone().filter(|_| !self.last) else {
            return rect().max_width(Size::px(220.)).on_sized(move |e: Event<SizedEventData>| zone(&e)).child(text).into_element();
        };
        let mut page = store.ui.page;
        rect()
            .max_width(Size::px(220.))
            .on_sized(move |e: Event<SizedEventData>| zone(&e))
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
struct Palette {
    width: f32,
}

impl Component for Palette {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut spot = use_spot("titlebar.palette", "button", &tr("Suchen oder Befehl …"), true);
        let mut zone = frame::use_zone(Zone::Control);
        let width = self.width;
        let icon_only = width < PALETTE_LABEL_FROM;
        let keys = if width < PALETTE_KEYS_FROM { String::new() } else { Command::OpenPalette.keys() };
        let mut field = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(tr("Suchen oder Befehl …"))
            .width(Size::px(width))
            .height(Size::px(size::TITLEBAR - space::S * 1.5))
            .padding((0., if icon_only { 0. } else { space::S }))
            .horizontal()
            .cross_align(Alignment::Center)
            .main_align(if icon_only { Alignment::Center } else { Alignment::Start })
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .background(if *hovered.read() { t.hover } else { t.raised })
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
            .cursor(CursorIcon::Pointer)
            .on_sized(move |e: Event<SizedEventData>| {
                zone(&e);
                spot(e);
            })
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |_| {
                let mut menu = store.ui.menu;
                if store.ui.sheet.peek().is_some() {
                    return;
                }
                if menu.peek().is_some() {
                    menu.set(None);
                    return;
                }
                Command::OpenPalette.run(store);
            })
            .child(icon(&t, Icon::Search, iconsize::S, t.text3))
            .content(Content::flex());
        if !icon_only {
            field = field.child(rect().width(Size::flex(1.)).child(line(&t, tr("Suchen oder Befehl …"), font::CALLOUT, t.text3).max_lines(1)));
        }
        let bare = keys.is_empty();
        if !bare {
            field = field.child(caption(&t, keys));
        }
        if let Some(border) = ring(&t, focus()) {
            field = field.border(border);
        }
        if icon_only || bare {
            let tip = format!("{} ({})", tr("Suchen oder Befehl …"), Command::OpenPalette.keys());
            return WithTip::new(Some(tip), field).into_element();
        }
        field.into_element()
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
        if tasks(use_store()).is_empty() {
            return rect().into_element();
        }
        ActivityChip.into_element()
    }
}

#[derive(PartialEq)]
struct ActivityChip;

impl Component for ActivityChip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut at = use_state(Bounds::default);
        let list = tasks(store);
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
        let mut zone = frame::use_zone(Zone::Control);
        let mut chip = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(label.clone())
            .height(Size::px(size::TITLEBAR - space::M))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .corner_radius(t.shape.control)
            .background(if *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .cursor(CursorIcon::Pointer)
            .on_sized(move |e: Event<SizedEventData>| {
                at.set_if_modified(Bounds::of(&e));
                zone(&e);
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
        if use_store().model.read().attention_count(None) == 0 {
            return rect().into_element();
        }
        AttentionChip.into_element()
    }
}

#[derive(PartialEq)]
struct AttentionChip;

impl Component for AttentionChip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let n = store.model.read().attention_count(None);
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut page = store.ui.page;
        let label = if n == 1 { tr("1 braucht dich") } else { trf("%lld brauchen dich", &[&n]) };
        let mut spot = use_spot("titlebar.attention", "button", &label, true);
        let mut zone = frame::use_zone(Zone::Control);
        let mut chip = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(label.clone())
            .height(Size::px(size::TITLEBAR - space::M))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .corner_radius(t.shape.control)
            .background(t.attention.with_af32(size::NOTICE_WASH))
            .cursor(CursorIcon::Pointer)
            .on_sized(move |e: Event<SizedEventData>| {
                zone(&e);
                spot(e);
            })
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