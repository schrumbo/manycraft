use freya::prelude::*;

use crate::core::i18n::tr;
use crate::view::commands::Command;
use crate::view::grid::{self, layout::TileSize};
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::segmented::{Segment, Segmented};
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
        let search_a11y = use_a11y();
        let focus_at = *ui.focus_search.read();
        use_side_effect_with_deps(&focus_at, move |at| {
            if at.is_some() {
                search_a11y.request_focus();
            }
        });
        let page = ui.page.read().clone();
        let m = store.model.read();
        let (heading, sub) = match &page {
            Page::Grid(filter) => {
                let name = match filter {
                    Filter::All => tr("Alle"),
                    Filter::Attention => tr("Brauchen dich"),
                    Filter::Batch(id) => m.batch(id).map_or_else(|| tr("Batch"), |b| b.name.clone()),
                };
                let keys = grid::shown(&m, filter, &ui.search.read());
                (name, Some(grid::count_line(&m, &keys)))
            }
            Page::Instance(id) => (m.instance(id).map_or_else(|| tr("Instanz"), |i| i.name.clone()), None),
            Page::Accounts => (tr("Accounts"), (m.loaded && !m.accounts.is_empty()).then(|| crate::view::accounts::count_line(&m))),
            Page::Chat => (tr("Chat"), None),
            Page::Settings => (tr("Einstellungen"), None),
        };

        let mut heading_col = rect().spacing(1.).child(title(&t, heading).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        if let Some(s) = sub {
            heading_col = heading_col.child(caption(&t, s));
        }
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
        if let Page::Grid(_) = page {
            let chosen = TileSize::ALL.iter().position(|s| *s == *ui.size.read()).unwrap_or(3);
            let items = grid::sizes().into_iter().map(|(size, label, tip)| Segment { id: format!("toolbar.size.{}", size_id(size)), label, tip: Some(tip) }).collect();
            if room >= frame::TOOLBAR_FULL {
                bar = bar.child(Segmented::new(tr("Kachelgröße"), items, chosen).on_choose(move |i| ui.size.set(TileSize::ALL[i])));
            }
            let search_w = if room >= frame::TOOLBAR_SEARCH { frame::SEARCH } else { frame::SEARCH_NARROW };
            bar = bar.child(Field::new("toolbar.search", ui.search).placeholder(tr("Suchen")).glyph(Icon::Search).width(Size::px(search_w)).a11y(search_a11y));
        }
        if page == Page::Accounts {
            bar = bar.child(crate::view::accounts::Tools { room });
        }
        if page == Page::Chat {
            bar = bar.child(crate::view::chat::Tools { room });
        }
        bar.child(Button::icon("toolbar.palette", Icon::Keyboard, tr("Befehle")).plain().tip(shortcut_tip(Command::OpenPalette)).on_press(move |_| Command::OpenPalette.run(store)))
    }
}

fn size_id(size: TileSize) -> &'static str {
    match size {
        TileSize::Small => "s",
        TileSize::Medium => "m",
        TileSize::Large => "l",
        TileSize::Fit => "fit",
    }
}
