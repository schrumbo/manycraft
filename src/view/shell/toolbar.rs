use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::view::grid;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::text::{caption, title};
use crate::view::kit::use_theme;
use crate::view::store::use_store;
use crate::view::theme::{frame, space};
use crate::view::ui::{Filter, Page, Pane};

/// The heading of the shown page with what it holds, and the tools that belong to it.
#[derive(PartialEq)]
pub struct Toolbar;

impl Component for Toolbar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let page = ui.page.read().clone();
        if matches!(page, Page::Start | Page::Instance(_)) {
            return rect().into_element();
        }
        let m = store.model.read();
        let (heading, sub) = match &page {
            Page::Start => (String::new(), None),
            Page::Library => (tr("Instanzen"), (!m.instances.is_empty()).then(|| if m.instances.len() == 1 { tr("1 Instanz") } else { trf("%lld Instanzen", &[&m.instances.len()]) })),
            Page::Discover => (tr("Entdecken"), Some(if ui.discover.read().kind == crate::core::modrinth::Kind::Modpack { tr("Von Modrinth, jedes Modpack wird eine eigene Instanz") } else { tr("Von Modrinth") })),
            Page::Wardrobe => (tr("Garderobe"), Some(tr("Skins und Umhänge"))),
            Page::Sync => (tr("Sync"), None),
            Page::Grid(filter) => {
                let name = match filter {
                    Filter::All => tr("Batches"),
                    Filter::Attention => tr("Brauchen dich"),
                    Filter::Batch(id) => m.batch(id).map_or_else(|| tr("Batch"), |b| b.name.clone()),
                };
                let show = grid::show_of(&m, filter, *ui.show.read());
                let search = ui.search.read();
                let keys = grid::shown(&m, filter, show, &search);
                let sub = if *filter == Filter::All && m.batches.is_empty() { None } else { grid::count_line(&m, &keys) };
                (name, sub)
            }
            Page::Instance(_) => (String::new(), None),
            Page::Browse(id, _) => match m.instance(id) {
                Some(i) => (trf("Mods für %@", &[&i.name]), Some(crate::view::sheets::browser::content_line(i, m.games.values().any(|g| g.instance_id == i.id && g.phase.is_active())))),
                None => (tr("Mods"), None),
            },
            Page::Settings(Pane::General) => (tr("Einstellungen"), None),
            Page::Settings(Pane::Accounts) => (tr("Accounts"), (m.loaded && !m.accounts.is_empty()).then(|| crate::view::accounts::count_line(&m))),
        };

        let mut heading_col = rect().spacing(1.).child(title(&t, heading).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        if let Some(s) = sub {
            heading_col = heading_col.child(caption(&t, s).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        }
        let mut bar = rect()
            .a11y_role(AccessibilityRole::Toolbar)
            .width(Size::fill())
            .height(Size::px(frame::TOOLBAR))
            .padding((0., space::L))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(rect().width(Size::flex(1.)).child(heading_col))
            .content(Content::flex());
        let room = ui.window.read().0 - ui.sidebar_room();
        if let Page::Grid(filter) = &page {
            bar = bar.child(grid::tools::Tools { filter: filter.clone(), room });
        }
        match &page {
            Page::Library => bar = bar.child(crate::view::library::Tools { room }),
            Page::Settings(pane) => {
                let mut open = ui.page;
                let items = vec![Segment { id: "settings.pane.general".into(), label: tr("Allgemein"), tip: None }, Segment { id: "settings.pane.accounts".into(), label: tr("Accounts"), tip: None }];
                bar = bar.child(Segmented::new(tr("Bereich"), items, usize::from(*pane == Pane::Accounts)).on_choose(move |i| open.set(Page::Settings(if i == 1 { Pane::Accounts } else { Pane::General }))));
                if *pane == Pane::Accounts {
                    bar = bar.child(crate::view::accounts::Tools { room });
                }
            }
            _ => {}
        }
        bar.into_element()
    }
}