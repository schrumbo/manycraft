use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::view::commands::Command;
use crate::view::grid;
use crate::view::icons::Icon;
use crate::view::instance;
use crate::view::kit::button::Button;
use crate::view::kit::text::{caption, title};
use crate::view::kit::use_theme;
use crate::view::store::use_store;
use crate::view::theme::{frame, space};
use crate::view::ui::{Filter, Page};

fn shortcut_tip(c: Command) -> String {
    match c.shortcut() {
        Some(s) => format!("{} ({})", c.label(), s.label()),
        None => c.label(),
    }
}

/// The page title with what it shows, and on the grid the tile size and the search.
#[derive(PartialEq)]
pub struct Toolbar;

impl Component for Toolbar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut ui = store.ui;
        let page = ui.page.read().clone();
        let m = store.model.read();
        let (heading, sub) = match &page {
            Page::Grid(filter) => {
                let name = match filter {
                    Filter::All => tr("Alle"),
                    Filter::Attention => tr("Brauchen dich"),
                    Filter::Batch(id) => m.batch(id).map_or_else(|| tr("Batch"), |b| b.name.clone()),
                };
                let show = grid::show_of(&m, filter, *ui.show.read());
                let search = ui.search.read();
                let keys = grid::shown(&m, filter, show, &search);
                let sub = if *filter == Filter::All && m.batches.is_empty() { None } else { grid::count_line(&m, &keys) };
                (name, sub)
            }
            Page::Instance(id) => (m.instance(id).map_or_else(|| tr("Instanz"), |i| i.name.clone()), None),
            Page::Installing(id) => ui.installs.read().iter().find(|i| i.id == *id).map_or_else(|| (tr("Instanz"), None), |i| (i.name.clone(), matches!(i.stage, crate::view::install::Stage::Failed(_)).then(|| i.state()))),
            Page::Modpacks => (tr("Modpacks"), Some(tr("Von Modrinth, jedes wird eine eigene Instanz"))),
            Page::Browse(id, _) => match m.instance(id) {
                Some(i) => (trf("Mods für %@", &[&i.name]), Some(crate::view::sheets::browser::content_line(i, m.games.values().any(|g| g.instance_id == i.id && g.phase.is_active())))),
                None => (tr("Mods"), None),
            },
            Page::Accounts => (tr("Accounts"), (m.loaded && !m.accounts.is_empty()).then(|| crate::view::accounts::count_line(&m))),
            Page::Chat => (tr("Chat"), None),
            Page::Settings => (tr("Einstellungen"), None),
            Page::Trash => (tr("Papierkorb"), None),
        };

        let heading_col = match &page {
            Page::Instance(id) => rect().child(instance::Title { id: id.clone() }),
            _ => {
                let mut col = rect().spacing(1.).child(title(&t, heading).max_lines(1).text_overflow(TextOverflow::Ellipsis));
                if let Some(s) = sub {
                    col = col.child(caption(&t, s).max_lines(1).text_overflow(TextOverflow::Ellipsis));
                }
                col
            }
        };
        let mut bar = rect()
            .a11y_role(AccessibilityRole::Toolbar)
            .width(Size::fill())
            .height(Size::px(frame::TOOLBAR))
            .padding((0., space::L, 0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(Button::icon("toolbar.sidebar", Icon::Sidebar, tr("Seitenleiste")).plain().tip(shortcut_tip(Command::ToggleSidebar)).on_press(move |_| Command::ToggleSidebar.run(store)))
            .child(rect().width(Size::flex(1.)).child(heading_col))
            .content(Content::flex());
        let room = ui.window.read().0 - if *ui.sidebar.read() { frame::SIDEBAR + 1. } else { 0. };
        if let Page::Instance(_) = &page
            && room >= frame::TOOLBAR_FULL
        {
            bar = bar.child(instance::Tabs);
        }
        if let Page::Grid(filter) = &page
            && grid::tools::shown(&m, filter)
        {
            bar = bar.child(grid::tools::Tools { filter: filter.clone(), room });
        }
        if page == Page::Accounts {
            bar = bar.child(crate::view::accounts::Tools { room });
        }
        if page == Page::Chat {
            bar = bar.child(crate::view::chat::Tools { room });
        }
        if let Page::Installing(id) = &page {
            bar = bar.child(rect().width(Size::px(frame::HERO.min(room / 2.))).horizontal().cross_align(Alignment::Center).child(crate::view::install::PlaySlot { id: *id }));
        }
        if let Page::Instance(id) = &page {
            let hero = instance::Hero { id: id.clone(), room };
            bar = if room >= frame::TOOLBAR_FULL { bar.child(rect().width(Size::px(frame::HERO)).horizontal().main_align(Alignment::End).cross_align(Alignment::Center).child(hero)) } else { bar.child(hero) };
        }
        bar = bar.child(Button::icon("toolbar.palette", Icon::Keyboard, tr("Befehle")).plain().tip(shortcut_tip(Command::OpenPalette)).on_press(move |_| Command::OpenPalette.run(store)));
        bar
    }
}
