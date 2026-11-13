//! The instance grid: tiles with previews, sorted attention first, only the rows on screen drawn.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{EditBatch, Play, StartAll, StartBatch};
use super::drive::Mark;
use super::main_window::{Filter, MainWindow, TileSize};
use super::model::{self, Instance, Model};
use super::motion::SLOW;
use super::theme::{Theme, control, font, iconsize, shell, space, tile};
use super::tile::{Geometry, TileData};
use super::widgets::{icon, primary_button};
use crate::core::i18n::{tr, trf};

/// Keyboard selection, sort order and scroll position of the grid.
pub struct Grid {
    pub focus: FocusHandle,
    pub selected: Option<String>,
    order: Vec<String>,
    seq: u64,
    hovered: bool,
    scroll: UniformListScrollHandle,
    area: Rc<Cell<Size<Pixels>>>,
    shown: Vec<String>,
    moves: HashMap<String, usize>,
    moved_at: Option<Instant>,
    columns: usize,
    scope: Option<(Option<String>, Filter, String)>,
}

impl Grid {
    pub fn new(cx: &mut App) -> Self {
        Self {
            scope: None,
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            selected: None,
            order: Vec::new(),
            seq: u64::MAX,
            hovered: false,
            scroll: UniformListScrollHandle::new(),
            area: Rc::default(),
            shown: Vec::new(),
            moves: HashMap::new(),
            moved_at: None,
            columns: 1,
        }
    }

    /// Scrolls back to the first row, for a new filter, search or page.
    pub fn reset_scroll(&mut self) {
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn sort(&mut self, m: &Model) -> bool {
        if self.seq == m.order_seq || self.hovered {
            if self.order.len() != m.instances.len() || self.order.iter().any(|k| !m.instances.contains_key(k)) {
                let known: HashSet<&str> = self.order.iter().map(String::as_str).collect();
                let mut fresh: Vec<&Instance> = m.instances.values().filter(|i| !known.contains(i.key.as_str())).collect();
                fresh.sort_by_cached_key(|i| i.name.to_lowercase());
                let fresh: Vec<String> = fresh.into_iter().map(|i| i.key.clone()).collect();
                self.order.retain(|k| m.instances.contains_key(k));
                self.order.extend(fresh);
            }
            return false;
        }
        self.seq = m.order_seq;
        let sorted: Vec<String> = m.sorted(None).into_iter().map(|i| i.key.clone()).collect();
        let changed = sorted != self.order;
        self.order = sorted;
        changed
    }
}

/// Whether `i` shows under `filter` and matches the search `query`.
pub fn matches(i: &Instance, batch: Option<&str>, filter: Filter, query: &str) -> bool {
    batch.is_none_or(|b| i.batch_id == b)
        && match filter {
            Filter::All => true,
            Filter::Attention => i.phase.needs_attention(),
            Filter::Running => i.phase.is_running() && !i.phase.needs_attention(),
            Filter::Stopped => !i.phase.is_running() && !i.phase.needs_attention(),
        }
        && (query.is_empty() || i.name.to_lowercase().contains(&query.to_lowercase()))
}

/// Columns and tile width for `count` tiles in a `width` × `height` area.
pub fn layout(size: TileSize, count: usize, width: f32, height: f32) -> (usize, f32) {
    let gap = space::M;
    let fit = |min: f32| {
        let columns = (((width + gap) / (min + gap)).floor() as usize).max(1);
        (columns, ((width - gap * (columns - 1) as f32) / columns as f32).max(1.))
    };
    match size {
        TileSize::Small => fit(tile::SMALL),
        TileSize::Medium => fit(tile::MEDIUM),
        TileSize::Large => fit(tile::LARGE),
        TileSize::Fit => {
            let count = count.max(1);
            let best = (1..=count)
                .map(|columns| {
                    let rows = count.div_ceil(columns) as f32;
                    let by_width = (width - gap * (columns - 1) as f32) / columns as f32;
                    let row_h = (height - gap * (rows - 1.)) / rows;
                    let by_height = |strip: f32| (row_h - strip) * 16. / 9.;
                    let w = by_width.min(by_height(tile::STRIP));
                    let w = if w < tile::MEDIUM { by_width.min(by_height(tile::STRIP_SMALL)).min(tile::MEDIUM - 1.) } else { w };
                    (columns, w.floor())
                })
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((1, width));
            if best.1 >= tile::FIT_MIN { best } else { fit(tile::FIT_MIN) }
        }
    }
}

fn data(i: &Instance, m: &Model, selected: &Option<String>) -> TileData {
    TileData {
        key: i.key.clone(),
        name: i.name.clone(),
        account_id: i.account_id,
        batch_id: i.batch_id.clone(),
        phase: i.phase.clone(),
        status: super::state::status(i, m.preparing.get(&i.batch_id).copied()),
        stopping: i.stopping,
        whispers: i.unread_whispers,
        preview: i.preview.clone(),
        selected: selected.as_deref() == Some(i.key.as_str()),
        playing: m.playing.as_deref() == Some(i.key.as_str()),
    }
}

fn move_selection(this: &mut MainWindow, keys: &[String], delta: isize, cx: &mut Context<MainWindow>) {
    if keys.is_empty() {
        return;
    }
    let at = this.grid.selected.as_ref().and_then(|s| keys.iter().position(|k| k == s));
    let next = match at {
        None if delta >= isize::MAX / 4 => keys.len() - 1,
        None => 0,
        Some(at) => (at as isize + delta).clamp(0, keys.len() as isize - 1) as usize,
    };
    this.grid.selected = Some(keys[next].clone());
    this.grid.scroll.scroll_to_item(next / this.grid.columns.max(1), ScrollStrategy::Nearest);
    cx.notify();
}

fn action(id: &'static str, label: String, icon_name: &str, action: Box<dyn Action>, t: &Theme) -> AnyElement {
    primary_button(id, label, icon_name, false, t).on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)).into_any_element()
}

fn empty(id: &'static str, symbol: &str, title: String, text: String, button: Option<AnyElement>, t: &Theme) -> AnyElement {
    div()
        .id(id)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(space::S))
        .px(px(space::XL))
        .child(icon(symbol, iconsize::HERO, t.tertiary))
        .child(div().id(SharedString::from(format!("{id}-title"))).mark("heading", title.clone()).mt(px(space::S)).text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
        .child(div().max_w(px(control::READING)).text_size(px(font::CALLOUT)).text_color(t.secondary).text_center().child(text))
        .when_some(button, |d, button| d.child(div().mt(px(space::S)).child(button)))
        .into_any_element()
}

/// Draws the grid of `batch` (all batches when `None`) with the window's filter, size and search.
pub fn render(this: &mut MainWindow, batch: Option<&str>, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = Theme::get(cx);
    let model = model::get(cx);
    let m = model.read(cx);
    let resorted = this.grid.sort(m);
    let scope = (batch.map(str::to_owned), this.filter, this.query.clone());
    let same_scope = this.grid.scope.as_ref() == Some(&scope);
    this.grid.scope = Some(scope);
    let held: HashSet<&str> = if this.grid.hovered && same_scope { this.grid.shown.iter().map(String::as_str).collect() } else { HashSet::new() };
    let keys: Vec<String> = this.grid.order.iter().filter(|k| held.contains(k.as_str()) || m.instances.get(*k).is_some_and(|i| matches(i, batch, this.filter, &this.query))).cloned().collect();
    drop(held);
    if resorted && !cx.reduce_motion() {
        let before: HashMap<&str, usize> = this.grid.shown.iter().enumerate().map(|(n, k)| (k.as_str(), n)).collect();
        this.grid.moves = keys.iter().enumerate().filter_map(|(n, k)| before.get(k.as_str()).filter(|old| **old != n).map(|old| (k.clone(), *old))).collect();
        this.grid.moved_at = (!this.grid.moves.is_empty()).then(Instant::now);
    }
    this.grid.shown = keys.clone();
    if keys.is_empty() {
        let in_scope = m.instances.values().any(|i| batch.is_none_or(|b| i.batch_id == b));
        super::state::set_visible(HashSet::new(), cx);
        if !in_scope {
            let edit = batch.map(|b| action("edit-batch", tr("Batch bearbeiten"), "pencil", Box::new(EditBatch(b.to_owned())), &t));
            return empty("no-accounts", "users", tr("Keine Accounts in diesem Batch"), tr("Füge dem Batch Accounts hinzu, um sie hier zu starten."), edit, &t);
        }
        if !this.query.is_empty() {
            let clear = primary_button("clear-search", tr("Suche löschen"), "x", false, &t)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.search.update(cx, |s, cx| s.set_value("", window, cx));
                    this.query.clear();
                    this.grid.reset_scroll();
                    cx.notify();
                }))
                .into_any_element();
            return empty("no-match", "search", tr("Keine Treffer"), trf("Kein Account passt zu „%@“.", &[&this.query]), Some(clear), &t);
        }
        return match this.filter {
            Filter::Attention => empty("all-good", "circle-check", tr("Alles in Ordnung"), tr("Kein Account braucht dich gerade."), None, &t),
            Filter::Stopped => empty("none-stopped", "circle-check", tr("Alles läuft"), tr("Kein Account ist gestoppt."), None, &t),
            Filter::Running | Filter::All => {
                let start: Box<dyn Action> = match batch {
                    Some(b) => Box::new(StartBatch(b.to_owned())),
                    None => Box::new(StartAll),
                };
                let label = if batch.is_some() { tr("Starten") } else { tr("Alle starten") };
                empty("none-running", "play", tr("Nichts läuft"), tr("Starte die Accounts, um sie hier zu sehen."), Some(action("start-empty", label, "play", start, &t)), &t)
            }
        };
    }
    let area = this.grid.area.get();
    let pad = space::L;
    let (width, height) = if area.width > px(0.) { (f32::from(area.width) - pad * 2., f32::from(area.height) - pad * 2.) } else { (f32::from(window.viewport_size().width) - pad * 2., f32::from(window.viewport_size().height) - pad * 2. - shell::TOOLBAR) };
    let (columns, tile_w) = layout(this.size, keys.len(), width.max(tile::FIT_MIN), height.max(1.));
    this.grid.columns = columns;
    let geometry = Geometry::new(tile_w);
    let row_h = geometry.height() + space::M;
    let rows = keys.len().div_ceil(columns);
    let shown: Rc<Vec<String>> = Rc::new(keys.clone());
    let selected = this.grid.selected.clone();
    let progress = this.grid.moved_at.map(|at| (at.elapsed().as_secs_f32() / SLOW.as_secs_f32()).min(1.));
    if progress.is_some_and(|p| p >= 1.) {
        this.grid.moved_at = None;
        this.grid.moves.clear();
    } else if progress.is_some() {
        window.request_animation_frame();
    }
    let ease = progress.filter(|p| *p < 1.).map(|p| 1. - (1. - p).powi(3));
    let moves = Rc::new(this.grid.moves.clone());
    let (scroll, viewport) = (this.grid.scroll.clone(), Rc::clone(&this.grid.area));
    let focused = this.grid.focus.is_focused(window);
    let view = cx.entity().downgrade();
    let list = uniform_list("instance-rows", rows, move |range, _, cx| {
        let top = -f32::from(scroll.0.borrow().base_handle.offset().y);
        let height = f32::from(viewport.get().height).max(row_h);
        let first = (top / row_h).floor().max(0.) as usize;
        let last = ((top + height) / row_h).ceil() as usize;
        let visible: HashSet<String> = shown.iter().skip(first * columns).take(last.saturating_sub(first) * columns).cloned().collect();
        let model = model::get(cx);
        let rows_data: Vec<Vec<(usize, TileData)>> = {
            let m = model.read(cx);
            range
                .map(|row| {
                    let from = row * columns;
                    shown[from..(from + columns).min(shown.len())].iter().enumerate().filter_map(|(c, k)| m.instances.get(k).map(|i| (from + c, data(i, m, &selected)))).collect()
                })
                .collect()
        };
        let rows: Vec<AnyElement> = rows_data
            .into_iter()
            .map(|cells| {
                let cells: Vec<AnyElement> = cells
                    .into_iter()
                    .map(|(index, d)| {
                        let offset = match (moves.get(&d.key), ease) {
                            (Some(old), Some(e)) => {
                                let dx = (old % columns) as f32 - (index % columns) as f32;
                                let dy = (old / columns) as f32 - (index / columns) as f32;
                                (dx * (tile_w + space::M) * (1. - e), dy * row_h * (1. - e))
                            }
                            _ => (0., 0.),
                        };
                        super::tile::view(d, geometry, offset, focused, view.clone(), cx)
                    })
                    .collect();
                div().h(px(row_h)).flex().gap(px(space::M)).children(cells).into_any_element()
            })
            .collect();
        super::state::set_visible(visible, cx);
        rows
    })
    .track_scroll(&this.grid.scroll)
    .px(px(pad))
    .py(px(pad))
    .size_full();
    let measure = {
        let area = Rc::clone(&this.grid.area);
        canvas(
            move |bounds, window, _| {
                if area.get() != bounds.size {
                    area.set(bounds.size);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    };
    let nav_keys = keys.clone();
    let can_play: HashMap<String, bool> = keys.iter().filter_map(|k| m.instances.get(k)).map(|i| (i.key.clone(), super::tile::playable(&i.phase))).collect();
    div()
        .id("grid")
        .key_context("InstanceGrid")
        .track_focus(&this.grid.focus)
        .relative()
        .size_full()
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            this.grid.hovered = *hovered;
            if !*hovered {
                cx.notify();
            }
        }))
        .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
            let columns = this.grid.columns.max(1) as isize;
            let page = columns * (f32::from(this.grid.area.get().height) / row_h).floor().max(1.) as isize;
            let delta = match e.keystroke.key.as_str() {
                "left" => -1,
                "right" => 1,
                "up" => -columns,
                "down" => columns,
                "pageup" => -page,
                "pagedown" => page,
                "home" => isize::MIN / 2,
                "end" => isize::MAX / 2,
                "enter" => {
                    if let Some(key) = this.grid.selected.clone().filter(|k| nav_keys.contains(k)) {
                        if can_play.get(&key).copied().unwrap_or(false) {
                            window.dispatch_action(Box::new(Play(key)), cx);
                        } else {
                            super::tile::primary_action(&key, window, cx);
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => return,
            };
            cx.stop_propagation();
            move_selection(this, &nav_keys, delta, cx);
        }))
        .child(measure)
        .child(list)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Filter, Instance, TileSize, layout, matches, space, tile};
    use crate::ui::model::Phase;

    #[test]
    fn sizes_fill_the_width() {
        let (columns, w) = layout(TileSize::Medium, 50, 1000., 600.);
        assert_eq!(columns, 4);
        assert!((w * 4. + space::M * 3. - 1000.).abs() < 0.01);
        assert_eq!(layout(TileSize::Large, 50, 200., 600.).0, 1);
        assert!(layout(TileSize::Small, 50, 1000., 600.).0 > columns);
    }

    #[test]
    fn fit_shows_every_tile_without_scrolling_until_tiles_get_too_small() {
        for (count, width, height) in [(1, 1000., 600.), (6, 1000., 600.), (50, 1400., 800.), (7, 500., 900.)] {
            let (columns, w) = layout(TileSize::Fit, count, width, height);
            let rows = count.div_ceil(columns) as f32;
            let strip = if w < tile::MEDIUM { tile::STRIP_SMALL } else { tile::STRIP };
            assert!(columns as f32 * w + (columns - 1) as f32 * space::M <= width + 0.5, "{count} tiles overflow the width");
            assert!(rows * (w * 9. / 16. + strip) + (rows - 1.) * space::M <= height + 1., "{count} tiles overflow the height");
        }
        assert_eq!(layout(TileSize::Fit, 1, 1000., 600.).0, 1);
        let (_, tiny) = layout(TileSize::Fit, 500, 1000., 600.);
        assert!(tiny >= tile::FIT_MIN);
    }

    #[test]
    fn filters_split_attention_running_and_stopped() {
        let mk = |phase| Instance { key: "k".into(), batch_id: "b".into(), account_id: uuid::Uuid::nil(), name: "AltOne".into(), phase, reconnect_attempts: 0, unread_whispers: 0, preview: None, preview_seq: 0, stopping: false };
        let lost = mk(Phase::Disconnected { server: None, reason: String::new() });
        let menu = mk(Phase::Menu);
        let off = mk(Phase::Stopped);
        assert!(matches(&lost, None, Filter::Attention, "") && !matches(&lost, None, Filter::Running, ""));
        assert!(matches(&menu, None, Filter::Running, "") && !matches(&menu, None, Filter::Stopped, ""));
        assert!(matches(&off, Some("b"), Filter::Stopped, "alt") && !matches(&off, Some("c"), Filter::All, ""));
        assert!(!matches(&off, None, Filter::All, "two"));
    }
}
