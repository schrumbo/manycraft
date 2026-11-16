//! The window's frame: sidebar, toolbar and the page, with the palette, sheets, menus, toasts and tooltips above.

mod palette;
mod sidebar;
mod toolbar;

use freya::prelude::*;

use self::palette::Palette;
use self::sidebar::Sidebar;
use self::toolbar::Toolbar;
use super::commands;
use super::grid::GridPage;
use super::icons::{Icon, icon};
use super::kit::text::{heading, secondary};
use super::kit::{menu, note_held, sheet, toast, tooltip, use_theme};
use super::store::use_store;
use super::theme::{iconsize, space};
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
        let content = match page {
            Page::Grid(filter) => GridPage { filter }.into_element(),
            Page::Instance(_) => pending(&t, Icon::Folder, tr("Instanz")),
            Page::Accounts => pending(&t, Icon::Users, tr("Accounts")),
            Page::Chat => pending(&t, Icon::Message, tr("Chat")),
            Page::Settings => pending(&t, Icon::Settings, tr("Einstellungen")),
        };
        let mut row = rect().width(Size::fill()).height(Size::fill()).horizontal();
        if *ui.sidebar.read() {
            row = row.child(Sidebar).child(rect().width(Size::px(1.)).height(Size::fill()).background(t.separator));
        }
        row = row.child(rect().width(Size::flex(1.)).height(Size::fill()).background(t.surface).child(Toolbar).child(rect().width(Size::fill()).height(Size::flex(1.)).child(content)).content(Content::flex())).content(Content::flex());
        rect()
            .width(Size::fill())
            .height(Size::fill())
            .background(t.surface)
            .color(t.text1)
            .font_size(super::theme::font::BODY)
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                note_held(e.modifiers);
                if commands::shortcut(store, &e) {
                    e.stop_propagation();
                }
            })
            .on_global_key_up(move |e: Event<KeyboardEventData>| note_held(e.modifiers))
            .child(row)
            .child(Palette)
            .child(sheet::Host)
            .child(menu::Host)
            .child(toast::Host)
            .child(tooltip::host(ui))
    }
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
