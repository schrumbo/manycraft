use freya::prelude::*;

use super::layout::TileSize;
use crate::app::event::Event as AppEvent;
use crate::app::model::Model;
use crate::core::i18n::{tr, trf};
use crate::view::chat;
use crate::view::commands::{self, Command, Want};
use crate::view::icons::Icon;
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::Item;
use crate::view::kit::more::More;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::use_theme;
use crate::view::kit::select::Select;
use crate::view::store::use_store;
use crate::view::theme::{frame, space};
use crate::view::ui::{Filter, Show, View};

const FILTER_W: f32 = 150.;
const FILTER_INLINE: f32 = frame::TOOLBAR_SEARCH + FILTER_W;

fn shortcut_tip(c: Command) -> String {
    match c.shortcut() {
        Some(s) => format!("{} ({})", c.label(), s.label()),
        None => c.label(),
    }
}

fn with_keys(text: String, c: Command) -> String {
    match c.shortcut() {
        Some(s) => format!("{text} · {}", s.label()),
        None => text,
    }
}

fn has_games(m: &Model, filter: &Filter) -> bool {
    match filter {
        Filter::Batch(b) => m.games.values().any(|g| g.batch_id == *b),
        Filter::All => m.games.values().any(|g| !g.alone()),
        Filter::Attention => true,
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

/// The grid's controls in the toolbar: filter, tile size, search, view, and stopping and starting what the page shows,
/// with the batch and the count in each tooltip, then the batch's settings and the chat sidebar.
#[derive(PartialEq)]
pub struct Tools {
    pub filter: Filter,
    pub room: f32,
}

impl Component for Tools {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let games = has_games(&store.model.read(), &self.filter);
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        if games {
            row = row.child(Controls { filter: self.filter.clone(), room: self.room });
        }
        row.child(Aside { filter: self.filter.clone() })
    }
}

/// The batch's settings and the chat sidebar's switch, the last controls of the toolbar.
#[derive(PartialEq)]
struct Aside {
    filter: Filter,
}

impl Component for Aside {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let _ = store.chat.read();
        let open = *ui.chat.open.read();
        let scope = chat::scope_of(&self.filter);
        let unread = if open { 0 } else { chat::unread(&store.model.read(), *ui.chat.left_at.read(), scope.as_deref()) };
        let mut tip = if open { tr("Chat ausblenden") } else { with_keys(tr("Chat einblenden"), Command::OpenChat) };
        if unread > 0 {
            tip = format!("{tip} · {}", trf("%lld ungelesen", &[&unread]));
        }
        let button = Button::icon("toolbar.chat", Icon::Message, tr("Chat")).on(open).tip(tip).on_press(move |_| chat::set_open(store, !open));
        let mut chat_slot = rect().child(button);
        if unread > 0 {
            chat_slot = chat_slot.child(rect().position(Position::new_absolute().right(space::XS).top(space::XS)).child(dot(t.accent)));
        }
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
        if let Filter::Batch(id) = &self.filter {
            let wanted = Want::EditBatch(id.clone());
            let blocked = commands::want_blocked(&wanted);
            row = row.child(Button::icon("toolbar.batch.settings", Icon::Settings, tr("Batch-Einstellungen")).disabled(blocked).on_press(move |_| {
                commands::want(store, wanted.clone());
            }));
        }
        row.child(chat_slot)
    }
}

#[derive(PartialEq)]
struct Controls {
    pub filter: Filter,
    pub room: f32,
}

impl Component for Controls {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut ui = store.ui;
        let mut shared = ui.grid_view;
        let search_a11y = use_a11y();
        let focus_at = *ui.focus_search.read();
        use_side_effect_with_deps(&focus_at, move |at| {
            if at.is_some() {
                search_a11y.request_focus();
            }
        });
        let m = store.model.read();
        let filter = &self.filter;
        let room = self.room;
        let view = *ui.view.read();
        let batch = match filter {
            Filter::Batch(b) => Some(b.clone()),
            _ => None,
        };
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        let mut more = Vec::new();
        if *filter != Filter::Attention {
            let scope: Vec<_> = m.games.values().filter(|g| batch.as_ref().is_none_or(|b| g.batch_id == *b) && crate::core::fleet::solo_instance(&g.batch_id).is_none()).collect();
            let shows = [Show::All, Show::Running, Show::Stopped];
            let count = |s: Show| scope.iter().filter(|g| super::fits_show(g, s)).count();
            let names = [tr("Alle"), tr("Läuft"), tr("Gestoppt")];
            let options: Vec<String> = shows.iter().zip(names).map(|(s, n)| trf("%@ (%lld)", &[&n, &count(*s)])).collect();
            let chosen = shows.iter().position(|s| *s == super::show_of(&m, filter, *ui.show.read())).unwrap_or(0);
            if room >= FILTER_INLINE {
                row = row.child(Select::new("toolbar.filter", tr("Filter"), options, chosen).width(FILTER_W).on_choose(move |i| ui.show.set(shows[i])));
            } else {
                more.extend(options.into_iter().enumerate().map(|(i, label)| Item::new(format!("toolbar.more.show.{i}"), label, move |_| ui.show.set(shows[i])).checked(i == chosen)));
            }
        }
        if view == View::Tiles {
            let chosen = TileSize::ALL.iter().position(|s| *s == *ui.size.read()).unwrap_or(3);
            if room >= frame::TOOLBAR_FULL {
                let items = super::sizes().into_iter().map(|(size, label, tip)| Segment { id: format!("toolbar.size.{}", size_id(size)), label, tip: Some(tip) }).collect();
                row = row.child(Segmented::new(tr("Kachelgröße"), items, chosen).on_choose(move |i| ui.size.set(TileSize::ALL[i])));
            } else {
                if !more.is_empty() {
                    more.push(Item::Separator);
                }
                more.extend(super::sizes().into_iter().enumerate().map(|(i, (size, _, name))| {
                    let name = if size == TileSize::Fit { tr("Einpassen") } else { name };
                    Item::new(format!("toolbar.more.size.{}", size_id(size)), name, move |_| ui.size.set(size)).checked(i == chosen)
                }));
            }
            if *filter == Filter::All && m.batches.len() > 1 {
                if !more.is_empty() {
                    more.push(Item::Separator);
                }
                let grouped = *shared.grouped.read();
                let mut item = Item::new("toolbar.more.groups", tr("Nach Batch gruppieren"), move |_| shared.grouped.set(!grouped)).checked(grouped).kept();
                if let Some(s) = Command::GroupByBatch.shortcut() {
                    item = item.shortcut(s.label());
                }
                more.push(item);
            }
        }
        let search_w = if room >= frame::TOOLBAR_SEARCH { frame::SEARCH } else { frame::SEARCH_NARROW };
        let hint = if room >= frame::TOOLBAR_FULL { tr("Name, Batch, Server, Status") } else { tr("Suchen") };
        row = row.child(Field::new("toolbar.search", ui.search).placeholder(hint).glyph(Icon::Search).width(Size::px(search_w)).a11y(search_a11y));
        let (toggle, glyph, label) = if view == View::Tiles { (Command::ShowTable, Icon::Rows, tr("Tabelle")) } else { (Command::ShowTiles, Icon::Grid, tr("Kacheln")) };
        row = row.child(Button::icon("toolbar.view", glyph, label).plain().tip(shortcut_tip(toggle)).on_press(move |_| toggle.run(store)));
        if !more.is_empty() {
            row = row.child(More { id: "toolbar.more".into(), items: more });
        }
        let scope = || m.games.values().filter(|g| !g.alone() && batch.as_ref().is_none_or(|b| g.batch_id == *b));
        let running = scope().filter(|g| g.phase.stoppable() && !g.stopping).count();
        let startable = scope().filter(|g| !g.phase.is_active()).count();
        let name = batch.as_ref().and_then(|b| m.batch(b)).map(|b| b.name.clone());
        let (stop_label, start_label) = match &name {
            Some(n) => (trf("%@ stoppen", &[n]), trf("%@ starten", &[n])),
            None => (tr("Alle stoppen"), tr("Alle starten")),
        };
        let (stop_tip, start_tip) = {
            let stop = trf("%@ (%lld)", &[&stop_label, &running]);
            let start = trf("%@ (%lld)", &[&start_label, &startable]);
            if batch.is_some() { (stop, start) } else { (with_keys(stop, Command::StopAll), with_keys(start, Command::StartAll)) }
        };
        let (start_b, stop_b) = (batch.clone(), batch);
        row.child(
            Button::icon("toolbar.stop", Icon::Stop, stop_label.clone())
                .plain()
                .disabled((running == 0).then(|| format!("{stop_label} · {}", tr("Nichts läuft"))))
                .tip(stop_tip)
                .on_press(move |_| store.dispatch(match &stop_b {
                    Some(b) => AppEvent::StopBatch(b.clone()),
                    None => AppEvent::StopAll,
                })),
        )
        .child(rect().width(Size::px(space::S)))
        .child(
            Button::icon("toolbar.start", Icon::Play, start_label.clone())
                .plain()
                .disabled((startable == 0).then(|| format!("{start_label} · {}", tr("Alle sind schon wach"))))
                .tip(start_tip)
                .on_press(move |_| store.dispatch(match &start_b {
                    Some(b) => AppEvent::StartBatch(b.clone()),
                    None => AppEvent::StartAll,
                })),
        )
    }
}