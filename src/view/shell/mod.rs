//! The window's frame: title bar, sidebar and the page under it, with the palette, sheets, menus, toasts and tooltips above.

pub mod account;
pub mod history;
mod palette;
pub mod shortcuts;
mod sidebar;
mod controls;
mod titlebar;
mod toolbar;

use freya::prelude::*;

use self::palette::Palette;
use self::sidebar::{Grip, Sidebar};
pub use self::sidebar::{instance_items, open_folder, swatch};
use self::titlebar::TitleBar;
use self::toolbar::Toolbar;
use super::accounts::AccountsPage;
use super::commands;
use super::grid::GridPage;
use super::icons::{Icon, icon};
use super::instance::{InstancePage, Uptime};
use super::kit::button::Button;
use super::kit::text::line;
use super::kit::{menu, note_held, sheet, toast, tooltip, use_theme};
use super::store::use_store;
use super::theme::{font, frame, iconsize, level, space};
use super::ui::{Filter, Page, Pane};
use crate::core::i18n::tr;
use crate::core::winstate;

/// The whole window.
#[derive(PartialEq)]
pub struct Shell;

impl Component for Shell {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut ui = store.ui;
        crate::view::kit::use_focus_return(*ui.drawer.read() && !*ui.sidebar.read());
        let page = ui.page.read().clone();
        let mut seen = use_state(|| None::<Page>);
        let mut booted = use_state(|| false);
        let dropping = use_state(|| false);
        let found = {
            let m = store.model.read();
            m.loaded.then(|| match &page {
                Page::Grid(Filter::Batch(b)) => m.batch(b).is_some(),
                Page::Instance(id) | Page::Browse(id, _) => m.instance(id).is_some(),
                _ => true,
            })
        };
        use_side_effect_with_deps(&(page.clone(), found), move |(page, found)| {
            let Some(found) = *found else { return };
            let first = !*booted.peek();
            booted.set_if_modified(true);
            if found {
                seen.set_if_modified(Some(page.clone()));
            } else if first || seen.peek().as_ref() == Some(page) {
                ui.page.set(Page::Start);
            }
        });
        use_side_effect_with_deps(&page, move |page| {
            let mut hist = ui.hist;
            hist.write().visit(page);
        });
        let wanted = *ui.sidebar_wanted.read();
        let rail = *ui.rail.read();
        let dragging = *ui.sidebar_drag.read();
        let width = *ui.sidebar_w.read();
        let mut kept_width = use_state(|| width);
        if !dragging {
            kept_width.set_if_modified(width);
        }
        use_side_effect_with_deps(&(page.clone(), wanted, rail, *kept_width.read()), move |(page, wanted, rail, width)| {
            remember(page, *wanted, *rail, *width);
            ui.drawer.set_if_modified(false);
        });
        let shown_inline = store.failed() && matches!(page, Page::Grid(_) | Page::Settings(Pane::Accounts));
        let content = match page {
            Page::Start => super::start::StartPage.into_element(),
            Page::Library => super::library::LibraryPage.into_element(),
            Page::Discover => crate::view::discover::DiscoverPage.into_element(),
            Page::Grid(filter) => GridPage { filter }.into_element(),
            Page::Instance(id) => rect().key(&id).width(Size::fill()).height(Size::fill()).child(InstancePage { id }).into_element(),
            Page::Browse(id, kind) => rect().key(&id).width(Size::fill()).height(Size::fill()).child(crate::view::sheets::browser::ContentPage { id, kind, outer: false }).into_element(),
            Page::Wardrobe => crate::view::wardrobe::WardrobePage.into_element(),
            Page::Settings(Pane::General) => super::settings::SettingsPage.into_element(),
            Page::Settings(Pane::Accounts) => AccountsPage.into_element(),
        };
        let mut row = rect().width(Size::fill()).height(Size::flex(1.)).horizontal();
        if *ui.sidebar.read() {
            row = row.child(Sidebar { overlay: false }).child(Grip);
        }
        let error = store.model.read().error.clone().filter(|_| !shown_inline);
        let mut column = rect().width(Size::flex(1.)).height(Size::fill()).background(t.surface).child(Toolbar);
        if let Some(text) = error {
            column = column.child(notice(&t, store, text));
        }
        row = row.child(column.child(rect().width(Size::fill()).height(Size::flex(1.)).child(content)).content(Content::flex())).content(Content::flex());
        super::kit::drop::window(rect(), dropping, move |paths| super::sheets::dropped::ask(store, paths))
            .width(Size::fill())
            .height(Size::fill())
            .background(t.surface)
            .color(t.text1)
            .font_size(super::theme::font::BODY)
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                note_held(e.modifiers);
                if e.key == Key::Named(NamedKey::Escape) && *ui.drawer.peek() && !ui.layered() {
                    ui.drawer.set(false);
                    e.stop_propagation();
                    return;
                }
                if super::grid::key(store, &e) || commands::shortcut(store, &e) || history_key(store, &e) {
                    e.stop_propagation();
                }
            })
            .on_global_key_up(move |e: Event<KeyboardEventData>| note_held(e.modifiers))
            .on_global_pointer_down(move |e: Event<PointerEventData>| match e.button() {
                Some(MouseButton::Back) if !ui.layered() => commands::Command::GoBack.run(store),
                Some(MouseButton::Forward) if !ui.layered() => commands::Command::GoForward.run(store),
                _ => {}
            })
            .child(TitleBar)
            .child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator))
            .child(row)
            .child(drawer(&t, ui))
            .child(Uptime)
            .child(Palette)
            .child(sheet::Host)
            .child(menu::Host)
            .child(toast::Host)
            .child(tooltip::host(ui))
            .maybe_child(super::kit::drop::outline(&t, dropping, ui.layered()))
    }
}

/// Alt with the left or right arrow walks the history, while no text field or layer has the keys.
fn history_key(store: super::store::Store, e: &KeyboardEventData) -> bool {
    if !e.modifiers.contains(Modifiers::ALT) || e.modifiers.contains(Modifiers::CONTROL) || e.modifiers.contains(Modifiers::SHIFT) || store.ui.layered() || super::kit::field::typing() {
        return false;
    }
    match e.key {
        Key::Named(NamedKey::ArrowLeft) => commands::Command::GoBack.run(store),
        Key::Named(NamedKey::ArrowRight) => commands::Command::GoForward.run(store),
        _ => return false,
    }
    true
}

fn drawer(t: &super::theme::Theme, ui: super::ui::Ui) -> Element {
    if !*ui.drawer.read() || *ui.sidebar.read() {
        return rect().into_element();
    }
    let (w, h) = *ui.window.read();
    let mut open = ui.drawer;
    rect()
        .position(Position::new_global().left(0.).top(frame::CHROME))
        .layer(Layer::OverlayLevel(level::DRAWER))
        .a11y_role(AccessibilityRole::Dialog)
        .a11y_alt(tr("Seitenleiste"))
        .a11y_builder(|n| n.set_modal())
        .width(Size::px(w))
        .height(Size::px((h - frame::CHROME).max(0.)))
        .horizontal()
        .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
        .child(rect().height(Size::fill()).shadow(t.elevation.float.clone()).child(Sidebar { overlay: true }))
        .child(rect().width(Size::flex(1.)).height(Size::fill()).background(t.scrim.with_a(t.scrim.a() / 2)).on_press(move |_| open.set(false)))
        .content(Content::flex())
        .into_element()
}

struct Kept {
    page: Option<(winstate::Page, Option<String>)>,
    sidebar: bool,
    rail: bool,
    width: f32,
}

fn remember(page: &Page, sidebar: bool, rail: bool, width: f32) {
    static WRITER: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<Kept>>> = std::sync::OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Kept>();
        std::thread::spawn(move || {
            while let Ok(mut next) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    next = newer;
                }
                write_kept(next);
            }
        });
        std::sync::Mutex::new(tx)
    });
    if let Ok(tx) = writer.lock() {
        let _ = tx.send(Kept { page: super::ui::kept(page), sidebar, rail, width });
    }
}

fn write_kept(kept: Kept) {
    let result = winstate::update(|saved| {
        let (kind, batch) = kept.page.unwrap_or_else(|| (saved.page, saved.batch.clone()));
        (saved.page, saved.batch, saved.sidebar, saved.rail, saved.sidebar_width) = (kind, batch, kept.sidebar, kept.rail, kept.width);
    });
    if let Err(e) = result {
        eprintln!("window state: {e:#}");
    }
}

fn notice(t: &super::theme::Theme, store: super::store::Store, text: String) -> Element {
    let mut model = store.model;
    rect()
        .width(Size::fill())
        .height(Size::px(frame::NOTICE))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .padding((0., space::S, 0., space::L))
        .background(t.attention.with_af32(frame::NOTICE_WASH))
        .border(Border::new().fill(t.separator).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner))
        .child(icon(t, Icon::Alert, iconsize::S, t.attention_text))
        .child(line(t, text, font::CALLOUT, t.text1).width(Size::flex(1.)).max_lines(1).text_overflow(TextOverflow::Ellipsis))
        .content(Content::flex())
        .child(Button::icon("notice.close", Icon::Close, tr("Hinweis ausblenden")).on_press(move |_| model.write().error = None))
        .into_element()
}