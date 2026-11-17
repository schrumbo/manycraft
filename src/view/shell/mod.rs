//! The window's frame: sidebar, toolbar and the page, with the palette, sheets, menus, toasts and tooltips above.

pub mod account;
mod palette;
mod sidebar;
mod toolbar;

use freya::prelude::*;

use self::palette::Palette;
use self::sidebar::Sidebar;
use self::toolbar::Toolbar;
use super::accounts::AccountsPage;
use super::chat::ChatPage;
use super::commands;
use super::grid::GridPage;
use super::instance::{InstancePage, Uptime};
use super::icons::{Icon, icon};
use super::kit::button::Button;
use super::kit::text::{heading, line, secondary};
use super::kit::{menu, note_held, sheet, toast, tooltip, use_theme};
use super::store::use_store;
use super::theme::{font, frame, iconsize, level, space};
use super::ui::{Filter, Page};
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
        let page = ui.page.read().clone();
        let missing = {
            let m = store.model.read();
            m.loaded
                && match &page {
                    Page::Grid(Filter::Batch(b)) => m.batch(b).is_none(),
                    Page::Instance(id) => m.instance(id).is_none(),
                    _ => false,
                }
        };
        use_side_effect_with_deps(&missing, move |missing| {
            if *missing {
                ui.page.set(Page::Grid(Filter::All));
            }
        });
        let wanted = *ui.sidebar_wanted.read();
        use_side_effect_with_deps(&(page.clone(), wanted), move |(page, wanted)| {
            remember(page, *wanted);
            ui.drawer.set_if_modified(false);
        });
        let shown_inline = store.failed() && matches!(page, Page::Grid(_) | Page::Accounts);
        let content = match page {
            Page::Grid(filter) => GridPage { filter }.into_element(),
            Page::Instance(id) => rect().key(&id).width(Size::fill()).height(Size::fill()).child(InstancePage { id }).into_element(),
            Page::Accounts => AccountsPage.into_element(),
            Page::Chat => ChatPage.into_element(),
            Page::Settings => super::settings::SettingsPage.into_element(),
        };
        let mut row = rect().width(Size::fill()).height(Size::fill()).horizontal();
        if *ui.sidebar.read() {
            row = row.child(Sidebar).child(rect().width(Size::px(1.)).height(Size::fill()).background(t.separator));
        }
        let error = store.model.read().error.clone().filter(|_| !shown_inline);
        let mut column = rect().width(Size::flex(1.)).height(Size::fill()).background(t.surface).child(Toolbar);
        if let Some(text) = error {
            column = column.child(notice(&t, store, text));
        }
        row = row.child(column.child(rect().width(Size::fill()).height(Size::flex(1.)).child(content)).content(Content::flex())).content(Content::flex());
        rect()
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
                if super::grid::key(store, &e) || commands::shortcut(store, &e) {
                    e.stop_propagation();
                }
            })
            .on_global_key_up(move |e: Event<KeyboardEventData>| note_held(e.modifiers))
            .child(row)
            .child(drawer(&t, ui))
            .child(Uptime)
            .child(Palette)
            .child(sheet::Host)
            .child(menu::Host)
            .child(toast::Host)
            .child(tooltip::host(ui))
    }
}

fn drawer(t: &super::theme::Theme, ui: super::ui::Ui) -> Element {
    if !*ui.drawer.read() || *ui.sidebar.read() {
        return rect().into_element();
    }
    let (w, h) = *ui.window.read();
    let mut open = ui.drawer;
    rect()
        .position(Position::new_global().left(0.).top(0.))
        .layer(Layer::OverlayLevel(level::DRAWER))
        .width(Size::px(w))
        .height(Size::px(h))
        .horizontal()
        .child(rect().height(Size::fill()).shadow(t.elevation.float.clone()).child(Sidebar))
        .child(rect().width(Size::flex(1.)).height(Size::fill()).background(t.scrim.with_a(t.scrim.a() / 2)).on_press(move |_| open.set(false)))
        .content(Content::flex())
        .into_element()
}

fn remember(page: &Page, sidebar: bool) {
    let mut saved = winstate::load();
    let (kind, batch) = super::ui::kept(page).unwrap_or_else(|| (saved.page, saved.batch.clone()));
    if (saved.page, &saved.batch, saved.sidebar) == (kind, &batch, sidebar) {
        return;
    }
    (saved.page, saved.batch, saved.sidebar) = (kind, batch, sidebar);
    if let Err(e) = winstate::save(&saved) {
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

fn pending(t: &super::theme::Theme, glyph: Icon, name: String) -> Element {
    rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .child(icon(t, glyph, iconsize::L * 2., t.text3))
        .child(heading(t, name))
        .child(secondary(t, tr("Diese Seite zieht als Nächstes in die neue Oberfläche um.")))
        .into_element()
}
