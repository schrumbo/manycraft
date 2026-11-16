//! The window's frame: sidebar, toolbar and the page, with the palette, sheets, menus, toasts and tooltips above.

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
use super::theme::{font, frame, iconsize, space};
use super::ui::Page;
use crate::core::i18n::tr;

/// The whole window.
#[derive(PartialEq)]
pub struct Shell;

impl Component for Shell {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let page = ui.page.read().clone();
        let shown_inline = store.failed() && matches!(page, Page::Grid(_) | Page::Accounts);
        let content = match page {
            Page::Grid(filter) => GridPage { filter }.into_element(),
            Page::Instance(id) => rect().key(&id).width(Size::fill()).height(Size::fill()).child(InstancePage { id }).into_element(),
            Page::Accounts => AccountsPage.into_element(),
            Page::Chat => ChatPage.into_element(),
            Page::Settings => pending(&t, Icon::Settings, tr("Einstellungen")),
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
                if super::grid::key(store, &e) || commands::shortcut(store, &e) {
                    e.stop_propagation();
                }
            })
            .on_global_key_up(move |e: Event<KeyboardEventData>| note_held(e.modifiers))
            .child(row)
            .child(Uptime)
            .child(Palette)
            .child(sheet::Host)
            .child(menu::Host)
            .child(toast::Host)
            .child(tooltip::host(ui))
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
