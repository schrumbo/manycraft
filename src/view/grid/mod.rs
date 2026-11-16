//! The grid page: games played alone as a "Zuletzt gespielt" shelf, every other game as a uniform 16:9 tile, only the
//! rows on screen drawn.

pub mod layout;
mod tile;

use std::collections::{BTreeSet, HashSet};

use freya::prelude::*;

use self::layout::{TileSize, layout};
use self::tile::Tile;
use super::commands;
use super::icons::{Icon, icon};
use super::kit::button::Button;
use super::kit::text::{body, heading, line, secondary};
use super::kit::{Bounds, use_theme};
use super::store::{Store, use_store};
use super::theme::{font, iconsize, space};
use super::ui::{Filter, Page};
use crate::app::event::Event as AppEvent;
use crate::app::model::{Game, Model};
use crate::core::fleet;
use crate::core::i18n::{tr, trf};

const SHELF_CARD: f32 = 220.;
const SHELF_H: f32 = 64.;

/// The games the grid shows for the page and the search, in the core's order.
pub fn shown(m: &Model, filter: &Filter, search: &str) -> Vec<String> {
    let query = search.trim().to_lowercase();
    let batch = match filter {
        Filter::Batch(b) => Some(b.as_str()),
        _ => None,
    };
    m.sorted(batch)
        .into_iter()
        .filter(|g| fleet::solo_instance(&g.batch_id).is_none())
        .filter(|g| *filter != Filter::Attention || g.phase.needs_attention())
        .filter(|g| query.is_empty() || g.name.to_lowercase().contains(&query) || m.batch(&g.batch_id).is_some_and(|b| b.name.to_lowercase().contains(&query)))
        .map(|g| g.key.clone())
        .collect()
}

/// The keys the grid now shows, for Select All.
pub fn shown_keys(store: Store) -> Vec<String> {
    let filter = match &*store.ui.page.peek() {
        Page::Grid(f) => f.clone(),
        _ => Filter::All,
    };
    shown(&store.model.peek(), &filter, &store.ui.search.peek())
}

/// Instances played alone, most recently played first.
fn shelf(m: &Model) -> Vec<(String, String, Option<String>)> {
    let mut list: Vec<(String, String, Option<String>)> = m
        .instances
        .iter()
        .filter(|i| m.solo_games(&i.id).next().is_some())
        .map(|i| {
            let running = m.solo_games(&i.id).find(|g| g.phase.is_active()).map(|g| g.key.clone());
            (i.id.clone(), i.name.clone(), running)
        })
        .collect();
    list.sort_by_key(|(_, name, running)| (running.is_none(), name.to_lowercase()));
    list
}

/// The grid page.
#[derive(PartialEq)]
pub struct GridPage {
    pub filter: Filter,
}

impl Component for GridPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut ui = store.ui;
        let mut area = use_state(Bounds::default);
        let mut top = use_state(|| 0f32);
        let m = store.model.read();
        let search = ui.search.read().clone();
        let keys = shown(&m, &self.filter, &search);
        let shelf = if self.filter == Filter::All && search.is_empty() { shelf(&m) } else { Vec::new() };
        let size = *ui.size.read();
        let Bounds { w, h, .. } = *area.read();
        let shelf_h = if shelf.is_empty() { 0. } else { SHELF_H + space::XL + font::CALLOUT + space::S };
        let grid = layout(keys.len(), size, w, (h - shelf_h).max(0.), t.grid_gap, t.grid_pad);
        if *ui.tile_w.peek() != grid.tile_w {
            ui.tile_w.set(grid.tile_w);
        }
        let content = grid.content_h + shelf_h;
        let max_top = (content - h).max(0.);
        let at = top.read().clamp(0., max_top);
        let rows = grid.visible_rows(t.grid_pad, (at - shelf_h).max(0.), h);
        let first = rows.start * grid.columns;
        let last = (rows.end * grid.columns).min(keys.len());
        let on_screen: HashSet<String> = keys[first.min(last)..last].iter().cloned().collect();
        let set: HashSet<String> = keys.iter().cloned().collect();
        let store_c = store;
        use_drop(move || store_c.set_visible(HashSet::new()));
        use_side_effect_with_deps(&(on_screen.clone(), set.clone()), move |(on_screen, set)| {
            store_c.set_visible(on_screen.clone());
            commands::prune(store_c, set);
        });

        let mut page = rect()
            .width(Size::fill())
            .height(Size::fill())
            .overflow(Overflow::Clip)
            .on_sized(move |e: Event<SizedEventData>| area.set_if_modified(Bounds::of(&e)))
            .on_wheel(move |e: Event<WheelEventData>| {
                let next = (*top.peek() - e.delta_y as f32).clamp(0., max_top);
                top.set_if_modified(next);
            })
            .on_mouse_down(move |_| {
                if !ui.selection.peek().is_empty() {
                    ui.selection.set(BTreeSet::new());
                }
            });

        if !m.loaded {
            return page.child(rect().center().width(Size::fill()).height(Size::fill()).child(secondary(&t, tr("Lädt …")))).into_element();
        }
        if keys.is_empty() && shelf.is_empty() {
            return page.child(empty(&t, &self.filter, !search.is_empty(), store)).into_element();
        }

        if !shelf.is_empty() && at < shelf_h {
            page = page.child(
                rect()
                    .position(Position::new_absolute().left(grid.left.min(t.grid_pad)).top(t.grid_pad - at))
                    .spacing(space::S)
                    .child(line(&t, tr("Zuletzt gespielt"), font::CALLOUT, t.text2).font_weight(FontWeight::SEMI_BOLD))
                    .child(rect().horizontal().spacing(t.grid_gap).children(shelf.into_iter().map(|(id, name, running)| card(&t, store, id, name, running).into_element()))),
            );
        }

        for (i, key) in keys.iter().enumerate().take(last).skip(first) {
            let Some(game) = m.games.get(key) else { continue };
            let (x, y) = grid.origin(i, t.grid_pad);
            page = page.child(
                rect()
                    .key(key)
                    .position(Position::new_absolute().left(x).top(y + shelf_h - at))
                    .child(Tile { key: key.clone(), name: game.name.clone(), width: grid.tile_w, height: grid.tile_h, index: i }),
            );
        }

        if content > h && h > 0. {
            let bar = (h * h / content).max(space::XL);
            let y = (h - bar) * (at / max_top.max(1.));
            page = page.child(rect().position(Position::new_absolute().right(space::XXS).top(y)).width(Size::px(space::XS)).height(Size::px(bar)).corner_radius(space::XXS).background(t.text3.with_a(90)));
        }
        page.into_element()
    }
}

fn card(t: &super::theme::Theme, store: Store, id: String, name: String, running: Option<String>) -> impl IntoElement {
    let status = if running.is_some() { tr("Läuft") } else { tr("Gestoppt") };
    let play_id = id.clone();
    let mut action = Button::icon(format!("shelf.{id}.play"), Icon::Play, tr("Spielen")).primary();
    action = action.on_press(move |_| match &running {
        Some(key) => store.dispatch(AppEvent::Play(key.clone())),
        None => store.dispatch(AppEvent::PlayInstance { instance: play_id.clone(), account: None }),
    });
    let mut c = rect()
        .width(Size::px(SHELF_CARD))
        .height(Size::px(SHELF_H))
        .padding((0., space::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .background(t.raised)
        .corner_radius(t.shape.tile);
    if let Some(shadow) = t.elevation.card.clone() {
        c = c.shadow(shadow);
    }
    c.child(rect().width(Size::flex(1.)).spacing(2.).child(body(t, name).max_lines(1).text_overflow(TextOverflow::Ellipsis)).child(secondary(t, status)))
        .content(Content::flex())
        .child(action)
}

fn empty(t: &super::theme::Theme, filter: &Filter, searching: bool, store: Store) -> impl IntoElement {
    let (glyph, title, text) = if searching {
        (Icon::Search, tr("Nichts gefunden"), tr("Kein Account und kein Batch passt zur Suche."))
    } else if *filter == Filter::Attention {
        (Icon::Check, tr("Alles läuft"), tr("Kein Account braucht dich gerade."))
    } else {
        (Icon::Users, tr("Noch keine Accounts"), tr("Füge einen Account hinzu, dann kannst du spielen."))
    };
    let mut col = rect().center().width(Size::fill()).height(Size::fill()).spacing(space::M).child(icon(t, glyph, iconsize::L * 2., t.text3)).child(heading(t, title)).child(secondary(t, text));
    let mut ui = store.ui;
    col = if searching {
        col.child(Button::new("grid.empty.clear", tr("Suche löschen")).tall().on_press(move |_| ui.search.set(String::new())))
    } else if *filter == Filter::Attention {
        col.child(Button::new("grid.empty.all", tr("Alle anzeigen")).tall().on_press(move |_| ui.page.set(Page::Grid(Filter::All))))
    } else {
        col.child(Button::new("grid.empty.add", tr("Account hinzufügen")).glyph(Icon::Plus).primary().tall().on_press(move |_| ui.page.set(Page::Accounts)))
    };
    col
}

/// The word for how many games the page shows, for the toolbar.
pub fn count_line(m: &Model, shown: &[String]) -> String {
    let games = || shown.iter().filter_map(|k| m.games.get(k));
    let keys = shown.len();
    let running = games().filter(|g: &&Game| g.phase.has_process()).count();
    let attention = games().filter(|g| g.phase.needs_attention()).count();
    let mut parts = vec![if keys == 1 { tr("1 Account") } else { trf("%lld Accounts", &[&keys]) }, if running == 1 { tr("1 läuft") } else { trf("%lld laufen", &[&running]) }];
    if attention == 1 {
        parts.push(tr("1 braucht dich"));
    } else if attention > 1 {
        parts.push(trf("%lld brauchen dich", &[&attention]));
    }
    parts.join(" · ")
}

/// The tile sizes as the toolbar offers them.
pub fn sizes() -> [(TileSize, String, String); 4] {
    [(TileSize::Small, "S".into(), tr("Klein")), (TileSize::Medium, "M".into(), tr("Mittel")), (TileSize::Large, "L".into(), tr("Groß")), (TileSize::Fit, tr("Einpassen"), tr("Alle Kacheln ohne Scrollen zeigen"))]
}

