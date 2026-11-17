use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::view::commands::Command;
use crate::view::grid::{self, layout::TileSize};
use crate::view::icons::Icon;
use crate::view::instance;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::text::{caption, title};
use crate::view::kit::use_theme;
use crate::view::store::use_store;
use crate::view::theme::{frame, space};
use crate::view::ui::{Filter, Page, Show, View};
use crate::view::kit::select::Select;

const FILTER_W: f32 = 150.;

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
                let show = grid::show_of(&m, filter, *ui.show.read());
                let search = ui.search.read();
                let keys = grid::shown(&m, filter, show, &search);
                let shelf = if grid::shelf_shown(filter, &search, show, *ui.view.read()) { grid::shelf_running(&m) } else { None };
                (name, Some(grid::count_line(&m, &keys, shelf)))
            }
            Page::Instance(id) => (m.instance(id).map_or_else(|| tr("Instanz"), |i| i.name.clone()), None),
            Page::Accounts => (tr("Accounts"), (m.loaded && !m.accounts.is_empty()).then(|| crate::view::accounts::count_line(&m))),
            Page::Chat => (tr("Chat"), None),
            Page::Settings => (tr("Einstellungen"), None),
        };

        let heading_col = match &page {
            Page::Instance(id) => rect().child(instance::Title { id: id.clone() }),
            _ => {
                let mut col = rect().spacing(1.).child(title(&t, heading).max_lines(1).text_overflow(TextOverflow::Ellipsis));
                if let Some(s) = sub {
                    col = col.child(caption(&t, s));
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
        if let Page::Grid(filter) = &page {
            let view = *ui.view.read();
            let batch = match filter {
                Filter::Batch(b) => Some(b.clone()),
                _ => None,
            };
            if *filter != Filter::Attention {
                let scope: Vec<_> = m.games.values().filter(|g| batch.as_ref().is_none_or(|b| g.batch_id == *b) && crate::core::fleet::solo_instance(&g.batch_id).is_none()).collect();
                let shows = [Show::All, Show::Running, Show::Stopped];
                let count = |s: Show| scope.iter().filter(|g| grid::fits_show(g, s)).count();
                let names = [tr("Alle"), tr("Läuft"), tr("Gestoppt")];
                let options = shows.iter().zip(names).map(|(s, n)| trf("%@ (%lld)", &[&n, &count(*s)])).collect();
                let chosen = shows.iter().position(|s| *s == grid::show_of(&m, filter, *ui.show.read())).unwrap_or(0);
                bar = bar.child(Select::new("toolbar.filter", tr("Filter"), options, chosen).width(FILTER_W).on_choose(move |i| ui.show.set(shows[i])));
            }
            if view == View::Tiles && room >= frame::TOOLBAR_FULL {
                let chosen = TileSize::ALL.iter().position(|s| *s == *ui.size.read()).unwrap_or(3);
                let items = grid::sizes().into_iter().map(|(size, label, tip)| Segment { id: format!("toolbar.size.{}", size_id(size)), label, tip: Some(tip) }).collect();
                bar = bar.child(Segmented::new(tr("Kachelgröße"), items, chosen).on_choose(move |i| ui.size.set(TileSize::ALL[i])));
            }
            let search_w = if room >= frame::TOOLBAR_SEARCH { frame::SEARCH } else { frame::SEARCH_NARROW };
            let hint = if room >= frame::TOOLBAR_FULL { tr("Name, Batch, Server, Status") } else { tr("Suchen") };
            bar = bar.child(Field::new("toolbar.search", ui.search).placeholder(hint).glyph(Icon::Search).width(Size::px(search_w)).a11y(search_a11y));
            let (toggle, glyph, label) = if view == View::Tiles { (Command::ShowTable, Icon::Rows, tr("Tabelle")) } else { (Command::ShowTiles, Icon::Grid, tr("Kacheln")) };
            bar = bar.child(Button::icon("toolbar.view", glyph, label).plain().tip(shortcut_tip(toggle)).on_press(move |_| toggle.run(store)));
            let scope = || m.games.values().filter(|g| !g.alone() && batch.as_ref().is_none_or(|b| g.batch_id == *b));
            let running = scope().filter(|g| g.phase.stoppable() && !g.stopping).count();
            let startable = scope().filter(|g| !g.phase.is_active()).count();
            let (start_label, stop_label) = if batch.is_some() { (tr("Starten"), tr("Stoppen")) } else { (tr("Alle starten"), tr("Alle stoppen")) };
            let (start_b, stop_b) = (batch.clone(), batch.clone());
            bar = bar
                .child(
                    Button::icon("toolbar.stop", Icon::Stop, stop_label.clone())
                        .plain()
                        .disabled((running == 0).then(|| format!("{stop_label} · {}", tr("Nichts läuft"))))
                        .tip(format!("{stop_label} · {}", trf("%lld laufende stoppen", &[&running])))
                        .on_press(move |_| store.dispatch(match &stop_b {
                            Some(b) => AppEvent::StopBatch(b.clone()),
                            None => AppEvent::StopAll,
                        })),
                )
                .child(
                    Button::icon("toolbar.start", Icon::Play, start_label.clone())
                        .plain()
                        .disabled((startable == 0).then(|| format!("{start_label} · {}", tr("Alle sind schon wach"))))
                        .tip(format!("{start_label} · {}", trf("%lld gestoppte starten", &[&startable])))
                        .on_press(move |_| store.dispatch(match &start_b {
                            Some(b) => AppEvent::StartBatch(b.clone()),
                            None => AppEvent::StartAll,
                        })),
                );
        }
        if page == Page::Accounts {
            bar = bar.child(crate::view::accounts::Tools { room });
        }
        if page == Page::Chat {
            bar = bar.child(crate::view::chat::Tools { room });
        }
        bar = bar.child(Button::icon("toolbar.palette", Icon::Keyboard, tr("Befehle")).plain().tip(shortcut_tip(Command::OpenPalette)).on_press(move |_| Command::OpenPalette.run(store)));
        if let Page::Instance(id) = &page {
            bar = bar.child(instance::Hero { id: id.clone(), room });
        }
        bar
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
