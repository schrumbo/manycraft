//! The grid page: games played alone as a "Zuletzt gespielt" shelf, every other game as a uniform 16:9 tile or as a row
//! of the table, only what is on screen drawn, with the lines about starts, outages and work above and the selection bar
//! below.

mod bar;
pub mod layout;
mod strips;
pub mod table;
mod tile;
mod words;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use freya::prelude::*;

use self::bar::SelectionBar;
use self::layout::{TileSize, layout};
use self::strips::Strips;
use self::table::{ROW, Table};
use self::tile::{History, Tile};
use super::commands::{self, Command};
use super::icons::{Icon, icon};
use super::kit::button::Button;
use super::kit::text::{body, heading, line, secondary};
use super::kit::{Bounds, use_theme};
use super::store::{Store, use_store};
use super::theme::{control, font, iconsize, space};
use super::ui::{Filter, Page, Show, View};
use crate::app::event::Event as AppEvent;
use crate::app::model::{Game, Model};
use crate::core::fleet;
use crate::core::i18n::{tr, trf};

const SHELF_CARD: f32 = 220.;
const SHELF_H: f32 = 64.;
const HISTORY_ROOM: f32 = 260.;

thread_local! {
    static NODES: RefCell<HashMap<String, AccessibilityId>> = RefCell::default();
    static PENDING: RefCell<Option<String>> = const { RefCell::new(None) };
    static ANCHOR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Notes the focus node of the tile or row of game `key`, and takes the keyboard when a key press moved there.
pub(super) fn use_node(key: &str, a11y: AccessibilityId) {
    let k = key.to_owned();
    use_hook(move || {
        NODES.with(|n| n.borrow_mut().insert(k.clone(), a11y));
        if PENDING.with(|p| p.borrow().as_deref() == Some(k.as_str())) {
            PENDING.with(|p| p.borrow_mut().take());
            a11y.request_focus();
        }
    });
    let k = key.to_owned();
    use_drop(move || {
        NODES.with(|n| {
            let mut n = n.borrow_mut();
            if n.get(&k) == Some(&a11y) {
                n.remove(&k);
            }
        });
    });
}

/// Forgets where Shift extends a range from, so the next one starts at the cursor.
pub(super) fn reset_anchor() {
    ANCHOR.with(|a| a.borrow_mut().take());
}

/// The games the grid shows for the page, the filter and the search, in the core's order.
pub fn shown(m: &Model, filter: &Filter, show: Show, search: &str) -> Vec<String> {
    let words = words::words(search);
    let batch = match filter {
        Filter::Batch(b) => Some(b.as_str()),
        _ => None,
    };
    m.sorted(batch)
        .into_iter()
        .filter(|g| fleet::solo_instance(&g.batch_id).is_none())
        .filter(|g| *filter != Filter::Attention || g.phase.needs_attention())
        .filter(|g| words::fits(g, show))
        .filter(|g| words::found(g, m, &words))
        .map(|g| g.key.clone())
        .collect()
}

/// Whether `g` shows under `show`.
pub fn fits_show(g: &Game, show: Show) -> bool {
    words::fits(g, show)
}

/// The filter in effect: a kind of work nobody needs any more shows everyone again.
pub fn show_of(m: &Model, filter: &Filter, show: Show) -> Show {
    match show {
        Show::Need(n) if !m.games.values().any(|g| g.phase.need() == Some(n) && filter_has(filter, g)) => Show::All,
        Show::Running | Show::Stopped if *filter == Filter::Attention => Show::All,
        other => other,
    }
}

fn filter_has(filter: &Filter, g: &Game) -> bool {
    match filter {
        Filter::Batch(b) => g.batch_id == *b,
        _ => true,
    }
}

/// The keys the grid now shows, in the order on screen.
pub fn shown_keys(store: Store) -> Vec<String> {
    let filter = match &*store.ui.page.peek() {
        Page::Grid(f) => f.clone(),
        _ => Filter::All,
    };
    let m = store.model.peek();
    let show = show_of(&m, &filter, *store.ui.show.peek());
    let mut keys = shown(&m, &filter, show, &store.ui.search.peek());
    if *store.ui.view.peek() == View::Table {
        table::sort(&m, &mut keys, *store.ui.sort.peek());
    }
    keys
}

/// Moves the cursor with the arrow keys, Home, End and the page keys, Shift extending the selection; Delete stops the
/// selected and Escape closes an open history. True when the key was the grid's.
pub fn key(store: Store, e: &KeyboardEventData) -> bool {
    let mut ui = store.ui;
    if *ui.message.peek() && e.key == Key::Named(NamedKey::Escape) && !ui.layered() && bar::messaging() {
        ui.message.set(false);
        return true;
    }
    if !matches!(*ui.page.peek(), Page::Grid(_)) || ui.layered() || super::kit::field::typing() {
        return false;
    }
    if super::kit::arrows() && matches!(e.key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Home | NamedKey::End)) {
        return false;
    }
    let mods = e.modifiers;
    if mods.contains(Modifiers::CONTROL) || mods.contains(Modifiers::ALT) || mods.contains(Modifiers::META) {
        return false;
    }
    if e.key == Key::Named(NamedKey::Escape) && ui.history.peek().is_some() {
        ui.history.set(None);
        return true;
    }
    if e.key == Key::Named(NamedKey::Delete) {
        if Command::StopSelected.available(store) {
            Command::StopSelected.run(store);
        }
        return true;
    }
    let (columns, rows) = *ui.grid.peek();
    let columns = columns.max(1);
    let page = columns * rows.max(1);
    let keys = shown_keys(store);
    let at = ui.cursor.peek().as_ref().and_then(|c| keys.iter().position(|k| k == c));
    let Some(last) = keys.len().checked_sub(1) else { return false };
    let next = match (e.key.clone(), at) {
        (Key::Named(NamedKey::Home), _) => 0,
        (Key::Named(NamedKey::End), _) => last,
        (Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp | NamedKey::PageUp), None) => last,
        (Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown | NamedKey::PageDown), None) => 0,
        (Key::Named(NamedKey::ArrowLeft), Some(i)) => i.saturating_sub(1),
        (Key::Named(NamedKey::ArrowRight), Some(i)) => (i + 1).min(last),
        (Key::Named(NamedKey::ArrowUp), Some(i)) => i.checked_sub(columns).unwrap_or(i),
        (Key::Named(NamedKey::ArrowDown), Some(i)) => if i + columns <= last { i + columns } else { i },
        (Key::Named(NamedKey::PageUp), Some(i)) => i.saturating_sub(page),
        (Key::Named(NamedKey::PageDown), Some(i)) => (i + page).min(last),
        _ => return false,
    };
    let to = keys[next].clone();
    if mods.contains(Modifiers::SHIFT) {
        let anchor = ANCHOR.with(|a| a.borrow().clone()).or_else(|| ui.cursor.peek().clone()).unwrap_or_else(|| to.clone());
        ANCHOR.with(|a| *a.borrow_mut() = Some(anchor.clone()));
        let from = keys.iter().position(|k| *k == anchor).unwrap_or(next);
        let (a, b) = if from <= next { (from, next) } else { (next, from) };
        ui.selection.set(keys[a..=b].iter().cloned().collect());
    } else {
        reset_anchor();
        ui.selection.set(BTreeSet::from([to.clone()]));
    }
    if ui.history.peek().is_some() {
        ui.history.set(Some(to.clone()));
    }
    ui.cursor.set(Some(to.clone()));
    match NODES.with(|n| n.borrow().get(&to).copied()) {
        Some(a11y) => a11y.request_focus(),
        None => PENDING.with(|p| *p.borrow_mut() = Some(to)),
    }
    true
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

/// Where the cursor sits in the scrolled content: its top and bottom, the view's height and how far it can scroll.
type Reveal = Rc<RefCell<Option<(f32, f32, f32, f32)>>>;

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
        let reveal: Reveal = use_hook(Reveal::default);
        use_side_effect_with_deps(&self.filter, move |_| {
            ui.show.set_if_modified(Show::All);
            ui.history.set_if_modified(None);
            top.set_if_modified(0.);
        });
        let cursor = ui.cursor.read().clone();
        let r = reveal.clone();
        use_side_effect_with_deps(&cursor, move |_| {
            let Some((y0, y1, h, max)) = *r.borrow() else { return };
            let now = *top.peek();
            let next = if y0 < now { y0 } else if y1 > now + h { y1 - h } else { now };
            top.set_if_modified(next.clamp(0., max));
        });
        let store_c = store;
        use_drop(move || store_c.set_visible(HashSet::new()));
        let m = store.model.read();
        let search = ui.search.read().clone();
        let view = *ui.view.read();
        let show = show_of(&m, &self.filter, *ui.show.read());
        let mut keys = shown(&m, &self.filter, show, &search);
        if view == View::Table {
            table::sort(&m, &mut keys, *ui.sort.read());
        }
        let shelf = if self.filter == Filter::All && search.is_empty() && show == Show::All { shelf(&m) } else { Vec::new() };
        let size = *ui.size.read();
        let Bounds { w, h, .. } = *area.read();
        let shelf_h = if shelf.is_empty() { 0. } else { SHELF_H + space::XL + font::CALLOUT + space::S };
        let grid = layout(keys.len(), size, w, (h - shelf_h).max(0.), t.grid_gap, t.grid_pad);
        if view == View::Tiles && *ui.tile_w.peek() != grid.tile_w {
            ui.tile_w.set(grid.tile_w);
        }
        let bar_room = if ui.selection.read().len() >= 2 { control::L + space::L * 2. } else { 0. };
        let (content, per_row, row_h) = match view {
            View::Tiles => (grid.content_h + shelf_h + bar_room, grid.columns.max(1), grid.tile_h + t.grid_gap),
            View::Table => (ROW * (keys.len() as f32 + 1.) + bar_room, 1, ROW),
        };
        let page_rows = ((h - if view == View::Table { ROW } else { 0. }) / row_h.max(1.)).floor().max(1.) as usize;
        if *ui.grid.peek() != (per_row, page_rows) {
            ui.grid.into_writable().write_if(|mut g| {
                *g = (per_row, page_rows);
                false
            });
        }
        let max_top = (content - h).max(0.);
        let at = top.read().clamp(0., max_top);
        *reveal.borrow_mut() = cursor.as_ref().and_then(|c| keys.iter().position(|k| k == c)).map(|i| match view {
            View::Tiles => {
                let (_, y) = grid.origin(i, t.grid_pad);
                (y + shelf_h - t.grid_pad, y + shelf_h + grid.tile_h + t.grid_pad, h, max_top)
            }
            View::Table => (i as f32 * ROW, (i as f32 + 2.) * ROW, h, max_top),
        });
        let (first, last) = match view {
            View::Tiles => {
                let rows = grid.visible_rows(t.grid_pad, (at - shelf_h).max(0.), h);
                (rows.start * grid.columns, (rows.end * grid.columns).min(keys.len()))
            }
            View::Table => (((at / ROW).floor() as usize).min(keys.len()), (((at + h) / ROW).ceil() as usize).min(keys.len())),
        };
        let on_screen: Option<HashSet<String>> = (view == View::Tiles).then(|| keys[first.min(last)..last].iter().cloned().collect());
        let set: HashSet<String> = keys.iter().cloned().collect();
        use_side_effect_with_deps(&(on_screen, set), move |(on_screen, set)| {
            if let Some(on_screen) = on_screen {
                store_c.set_visible(on_screen.clone());
            }
            commands::prune(store_c, set);
        });
        let focused: Vec<String> = {
            let picked = ui.selection.read();
            cursor.iter().chain(keys[first.min(last)..last].iter().filter(|k| picked.contains(*k) && cursor.as_ref() != Some(*k))).take(4).cloned().collect()
        };
        let width = ((if view == View::Tiles { grid.tile_w } else { table::PREVIEW_W }) * super::kit::scale()).round() as u32;
        use_side_effect_with_deps(&(focused, width), move |(focused, width)| {
            store_c.watch(|w| {
                w.focused = focused.clone();
                w.width = *width;
            });
        });

        let mut page = rect()
            .width(Size::fill())
            .height(Size::flex(1.))
            .overflow(Overflow::Clip)
            .on_sized(move |e: Event<SizedEventData>| area.set_if_modified(Bounds::of(&e)))
            .on_wheel(move |e: Event<WheelEventData>| {
                let next = (*top.peek() - e.delta_y as f32).clamp(0., max_top);
                top.set_if_modified(next);
            })
            .on_mouse_down(move |_| {
                ui.history.set_if_modified(None);
                if !ui.selection.peek().is_empty() {
                    ui.selection.set(BTreeSet::new());
                }
            });
        let column = rect().width(Size::fill()).height(Size::fill()).content(Content::flex()).child(Strips { filter: self.filter.clone() });

        if store.failed() {
            let reason = m.error.clone().unwrap_or_else(|| tr("Starte Manycraft neu."));
            let col = rect()
                .center()
                .width(Size::fill())
                .height(Size::fill())
                .spacing(space::M)
                .child(icon(&t, Icon::Alert, iconsize::L * 2., t.text3))
                .child(heading(&t, tr("Manycraft konnte nicht starten")))
                .child(secondary(&t, reason))
                .child(Button::new("grid.failed.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).tall().on_press(move |_| store.retry()));
            return column.child(page.child(col)).into_element();
        }
        if !m.loaded {
            return column.child(page.child(rect().center().width(Size::fill()).height(Size::fill()).child(secondary(&t, tr("Lädt …"))))).into_element();
        }
        if keys.is_empty() && shelf.is_empty() {
            return column.child(page.child(empty(&t, &self.filter, show, &search, store))).into_element();
        }

        if view == View::Table {
            let batches = !matches!(self.filter, Filter::Batch(_)) && m.batches.len() > 1;
            page = page.child(Table { keys: keys.clone(), top: at, h, batches });
        } else {
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
            let open = ui.history.read().clone();
            if let Some((key, i)) = open.and_then(|k| keys.iter().position(|x| *x == k).map(|i| (k, i))) {
                let (x, y) = grid.origin(i, t.grid_pad);
                let tile_top = y + shelf_h - at;
                let below = tile_top + grid.tile_h + space::XS;
                let left = x.min((w - HISTORY_ROOM * 1.25).max(0.));
                let at_y = if h - below >= HISTORY_ROOM || tile_top < HISTORY_ROOM { Position::new_absolute().left(left).top(below) } else { Position::new_absolute().left(left).bottom(h - tile_top + space::XS) };
                page = page.child(rect().position(at_y).child(History { key }));
            }
        }

        if content > h && h > 0. {
            let bar = (h * h / content).max(space::XL);
            let y = (h - bar) * (at / max_top.max(1.));
            page = page.child(rect().position(Position::new_absolute().right(space::XXS).top(y)).width(Size::px(space::XS)).height(Size::px(bar)).corner_radius(space::XXS).background(t.text3.with_a(90)));
        }
        column.child(page.child(SelectionBar {})).into_element()
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

fn empty(t: &super::theme::Theme, filter: &Filter, show: Show, search: &str, store: Store) -> impl IntoElement {
    let searching = !search.trim().is_empty();
    let (glyph, title, text) = if searching {
        (Icon::Search, tr("Nichts gefunden"), trf("Zu „%@“ passt keiner.", &[&search.trim()]))
    } else if show == Show::Running {
        (Icon::Moon, tr("Alle schlummern"), tr("Gerade läuft keiner."))
    } else if show == Show::Stopped {
        (Icon::Check, tr("Nichts gestoppt"), tr("Alle sind wach."))
    } else if *filter == Filter::Attention {
        (Icon::Check, tr("Alles läuft"), tr("Kein Account braucht dich gerade."))
    } else {
        (Icon::Users, tr("Noch keine Accounts"), tr("Füge einen Account hinzu, dann kannst du spielen."))
    };
    let mut col = rect().center().width(Size::fill()).height(Size::fill()).spacing(space::M).child(icon(t, glyph, iconsize::L * 2., t.text3)).child(heading(t, title)).child(secondary(t, text));
    let mut ui = store.ui;
    col = if searching {
        col.child(Button::new("grid.empty.clear", tr("Suche löschen")).tall().on_press(move |_| ui.search.set(String::new())))
    } else if show != Show::All {
        col.child(Button::new("grid.empty.every", tr("Alle anzeigen")).tall().on_press(move |_| ui.show.set(Show::All)))
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

