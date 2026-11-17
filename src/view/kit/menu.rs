//! Context menus and pop-up lists: open at the pointer or under a control, driven by arrows, Enter and Esc. Long
//! lists scroll and can carry a field that filters them as you type.

use std::sync::atomic::{AtomicU64, Ordering};

use freya::prelude::*;

use super::field::Field;
use super::text::{line, spacer};
use super::{Bounds, use_theme};
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::store::use_store;
use crate::view::theme::{control, font, frame, level, iconsize, space};
use crate::view::ui::Ui;

/// An account's face in front of a menu entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub id: uuid::Uuid,
    pub name: String,
    pub color: u8,
}

/// One entry of a menu.
#[derive(Clone, PartialEq)]
pub enum Item {
    Action {
        id: String,
        label: String,
        shortcut: Option<String>,
        checked: bool,
        danger: bool,
        disabled: bool,
        face: Option<Face>,
        warn: bool,
        keep: bool,
        run: EventHandler<()>,
    },
    Separator,
}

impl Item {
    /// An entry named `id` that runs `run`.
    pub fn new(id: impl Into<String>, label: impl Into<String>, run: impl FnMut(()) + 'static) -> Self {
        Self::Action { id: id.into(), label: label.into(), shortcut: None, checked: false, danger: false, disabled: false, face: None, warn: false, keep: false, run: EventHandler::new(run) }
    }

    pub fn shortcut(mut self, keys: impl Into<String>) -> Self {
        if let Self::Action { shortcut, .. } = &mut self {
            *shortcut = Some(keys.into());
        }
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        if let Self::Action { checked, .. } = &mut self {
            *checked = on;
        }
        self
    }

    /// Shows the account's face in front of the label.
    pub fn face(mut self, account: Face) -> Self {
        if let Self::Action { face, .. } = &mut self {
            *face = Some(account);
        }
        self
    }

    /// Marks the entry as needing attention: a warning glyph in front of its detail.
    pub fn warn(mut self) -> Self {
        if let Self::Action { warn, .. } = &mut self {
            *warn = true;
        }
        self
    }

    /// Stays in the list while it is filtered, below what matches.
    pub fn kept(mut self) -> Self {
        if let Self::Action { keep, .. } = &mut self {
            *keep = true;
        }
        self
    }

    pub fn danger(mut self) -> Self {
        if let Self::Action { danger, .. } = &mut self {
            *danger = true;
        }
        self
    }

    pub fn disabled(mut self, off: bool) -> Self {
        if let Self::Action { disabled, .. } = &mut self {
            *disabled = off;
        }
        self
    }

    fn live(&self) -> bool {
        matches!(self, Self::Action { disabled: false, .. })
    }
}

/// An open menu: its entries and where it opened.
#[derive(Clone, PartialEq)]
pub struct Menu {
    pub items: Vec<Item>,
    /// The pointer position, or the bottom left of the control it drops from.
    pub at: (f32, f32),
    /// At least this wide, for pop-ups that match their control.
    pub width: f32,
    /// The placeholder of the filter field, for lists long enough to need one.
    pub search: Option<String>,
    serial: u64,
}

impl Menu {
    /// A menu at the pointer.
    pub fn at(items: Vec<Item>, at: (f32, f32)) -> Self {
        Self { items, at, width: frame::MENU, search: None, serial: 0 }
    }

    /// A menu that drops from `anchor`, as wide as it.
    pub fn under(items: Vec<Item>, anchor: Bounds) -> Self {
        Self { items, at: (anchor.x, anchor.y + anchor.h + space::XS), width: anchor.w.max(frame::MENU * 0.6), search: None, serial: 0 }
    }

    /// Adds a field above the list that filters it by label.
    pub fn searchable(mut self, placeholder: impl Into<String>) -> Self {
        self.search = Some(placeholder.into());
        self
    }
}

const WIDE_GLYPH: f32 = 0.8;

static OPENED: AtomicU64 = AtomicU64::new(0);

/// Opens `menu`, closing any other.
pub fn open(mut ui: Ui, mut menu: Menu) {
    menu.serial = OPENED.fetch_add(1, Ordering::Relaxed) + 1;
    ui.tip.set_if_modified(None);
    ui.menu.set(Some(menu));
}

fn glyphs(text: &str, size: f32) -> f32 {
    text.chars().map(|c| if c.is_uppercase() || matches!(c, 'm' | 'w' | '…' | '%') { WIDE_GLYPH } else { control::RENAME_GLYPH }).sum::<f32>() * size
}

fn fits(item: &Item) -> f32 {
    let Item::Action { label, shortcut, face, warn, .. } = item else { return 0. };
    let mut w = space::XS + iconsize::S + space::XS + glyphs(label, font::BODY) + space::XS + space::S;
    if face.is_some() {
        w += frame::FACE + space::XS;
    }
    if let Some(keys) = shortcut {
        w += space::XL + glyphs(keys, font::CALLOUT);
    }
    if *warn {
        w += iconsize::S + space::XS;
    }
    w
}

fn height(item: &Item) -> f32 {
    if matches!(item, Item::Separator) { space::S + 1. } else { frame::MENU_ROW }
}

fn top(items: &[Item], i: usize) -> f32 {
    items.iter().take(i).map(height).sum()
}

fn filtered(items: &[Item], query: &str) -> Vec<Item> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return items.to_vec();
    }
    let mut found: Vec<Item> = items.iter().filter(|i| matches!(i, Item::Action { label, keep: false, .. } if label.to_lowercase().contains(&query))).cloned().collect();
    let kept: Vec<Item> = items.iter().filter(|i| kept(i)).cloned().collect();
    if !found.is_empty() && !kept.is_empty() {
        found.push(Item::Separator);
    }
    found.extend(kept);
    found
}

fn kept(item: &Item) -> bool {
    matches!(item, Item::Action { keep: true, .. })
}

fn step(items: &[Item], from: Option<usize>, down: bool) -> Option<usize> {
    let n = items.len();
    if n == 0 {
        return None;
    }
    let mut i = from.unwrap_or(if down { n - 1 } else { 0 });
    for _ in 0..n {
        i = if down { (i + 1) % n } else { (i + n - 1) % n };
        if items[i].live() {
            return Some(i);
        }
    }
    None
}

/// Draws the open menu of the window above everything else.
#[derive(PartialEq)]
pub struct Host;

impl Component for Host {
    fn render(&self) -> impl IntoElement {
        let mut ui = use_consume::<Ui>();
        let menu = ui.menu.read().clone();
        super::use_focus_return(menu.is_some());
        let Some(menu) = menu else { return rect().into_element() };
        let (w, h) = *ui.window.read();
        let serial = menu.serial;
        rect()
            .position(Position::new_global().left(0.).top(0.))
            .layer(Layer::OverlayLevel(level::MENU))
            .width(Size::px(w))
            .height(Size::px(h))
            .on_mouse_down(move |_| ui.menu.set(None))
            .child(rect().key(serial).width(Size::fill()).height(Size::fill()).child(Panel { menu }))
            .into_element()
    }
}

#[derive(PartialEq)]
struct Panel {
    menu: Menu,
}

impl Component for Panel {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ui = use_consume::<Ui>();
        let menu = &self.menu;
        let query = use_state(String::new);
        let mut hot = use_state(|| (String::new(), None::<usize>));
        let (w, h) = *ui.window.read();
        let list_max = (frame::MENU_ROW * frame::MENU_ROWS as f32).min(h * 0.6);
        let start = menu.items.iter().position(|i| matches!(i, Item::Action { checked: true, .. })).map_or(0., |i| (top(&menu.items, i) - list_max / 2. + frame::MENU_ROW / 2.).max(0.));
        let scroll = use_hook(|| ScrollController::new(0, -(start as i32)));
        let q = query.read().clone();
        use_side_effect_with_deps(&q, move |q| {
            if !q.is_empty() {
                let mut scroll = scroll;
                scroll.scroll_to_y(0);
            }
        });
        let items = filtered(&menu.items, &q);
        let none_found = !q.trim().is_empty() && items.iter().all(|i| kept(i) || matches!(i, Item::Separator));
        let chosen = match &*hot.read() {
            (at, i) if *at == q => *i,
            _ if q.trim().is_empty() => None,
            _ => step(&items, None, true).filter(|i| !kept(&items[*i])),
        };
        let searching = menu.search.is_some();
        let field_h = if searching { control::M + space::XS } else { 0. };
        let rows = items.iter().map(height).sum::<f32>() + if none_found { frame::MENU_ROW } else { 0. };
        let list_h = rows.min(list_max);
        let panel_h = list_h + field_h + space::XS * 2.;
        let tallest = menu.items.iter().map(height).sum::<f32>().min(list_max) + field_h + space::XS * 2.;
        let fit = menu.items.iter().map(fits).fold(0., f32::max) + space::XS * 2. + 2.;
        let width = menu.width.max(fit).min(w - space::S * 2.);
        let x = menu.at.0.min(w - width - space::S).max(space::S);
        let y = if menu.at.1 + tallest > h - space::S { (menu.at.1 - tallest).max(space::S) } else { menu.at.1 };
        let run = {
            let items = items.clone();
            move |i: usize, mut ui: Ui| {
                if let Some(Item::Action { run, disabled: false, .. }) = items.get(i) {
                    let run = run.clone();
                    ui.menu.set(None);
                    run.call(());
                }
            }
        };
        let keys_items = items.clone();
        let keys_run = run.clone();
        let keys_q = q.clone();
        let reveal = move |i: usize, mut scroll: ScrollController, items: &[Item]| {
            let (at, row, now) = (top(items, i), height(&items[i]), -scroll.position().y);
            if at < now {
                scroll.scroll_to_y(-(at as i32));
            } else if at + row > now + list_h {
                scroll.scroll_to_y(-((at + row - list_h).ceil() as i32));
            }
        };
        let mut entries: Vec<Element> = items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                Item::Separator => rect().width(Size::fill()).padding((space::XS, space::S)).child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator)).into_element(),
                Item::Action { id, label, shortcut, checked, danger, disabled, face, warn, .. } => {
                    let mut run = run.clone();
                    let at = q.clone();
                    Entry { id: id.clone(), label: label.clone(), shortcut: shortcut.clone(), checked: *checked, danger: *danger, disabled: *disabled, face: face.clone(), warn: *warn, hot: chosen == Some(i), on_hover: EventHandler::new(move |_| hot.set((at.clone(), Some(i)))), on_press: EventHandler::new(move |_| run(i, ui)) }.into_element()
                }
            })
            .collect();
        let mut panel = rect()
            .position(Position::new_absolute().left(x).top(y))
            .a11y_role(AccessibilityRole::Menu)
            .a11y_builder(|n| n.set_modal())
            .width(Size::px(width))
            .padding(space::XS)
            .background(t.raised)
            .corner_radius(t.shape.control + 2.)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Outer))
            .shadow(t.elevation.float.clone())
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                let mut ui = ui;
                let mut keys_run = keys_run.clone();
                match &e.key {
                    Key::Named(NamedKey::Escape) => ui.menu.set(None),
                    Key::Named(key @ (NamedKey::ArrowDown | NamedKey::ArrowUp)) => {
                        if let Some(next) = step(&keys_items, chosen, *key == NamedKey::ArrowDown) {
                            hot.set((keys_q.clone(), Some(next)));
                            reveal(next, scroll, &keys_items);
                        }
                    }
                    Key::Named(key @ (NamedKey::Home | NamedKey::End)) => {
                        if let Some(next) = step(&keys_items, None, *key == NamedKey::Home) {
                            hot.set((keys_q.clone(), Some(next)));
                            reveal(next, scroll, &keys_items);
                        }
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(i) = chosen {
                            keys_run(i, ui);
                        }
                    }
                    _ => return,
                }
                e.stop_propagation();
                e.prevent_default();
            });
        if let Some(placeholder) = &menu.search {
            panel = panel.child(rect().width(Size::fill()).padding((0., 0., space::XS, 0.)).child(Field::new("menu.search", query).placeholder(placeholder.clone()).glyph(Icon::Search).auto_focus().rows_through()));
        }
        if none_found {
            entries.insert(0, rect().width(Size::fill()).height(Size::px(frame::MENU_ROW)).padding((0., space::S, 0., space::XS + iconsize::S + space::XS)).cross_align(Alignment::Center).main_align(Alignment::Center).child(line(&t, tr("Nichts gefunden"), font::CALLOUT, t.text3)).into_element());
        }
        if rows <= list_h {
            return panel.children(entries).into_element();
        }
        panel.child(ScrollView::new_controlled(scroll).width(Size::fill()).height(Size::px(list_h)).children(entries)).into_element()
    }
}

#[derive(Clone, PartialEq)]
struct Entry {
    id: String,
    label: String,
    shortcut: Option<String>,
    checked: bool,
    danger: bool,
    disabled: bool,
    face: Option<Face>,
    warn: bool,
    hot: bool,
    on_hover: EventHandler<()>,
    on_press: EventHandler<()>,
}

impl Component for Entry {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let spot = use_spot(&self.id, "menuitem", &self.label, !self.disabled);
        let ink = if self.disabled {
            t.text3
        } else if self.hot {
            t.on_accent
        } else if self.danger {
            t.danger_text
        } else {
            t.text1
        };
        let hover = self.on_hover.clone();
        let press = self.on_press.clone();
        let mut e = rect()
            .a11y_role(AccessibilityRole::MenuItem)
            .a11y_alt(self.label.clone())
            .width(Size::fill())
            .height(Size::px(frame::MENU_ROW))
            .padding((0., space::S, 0., space::XS))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .corner_radius(t.shape.small)
            .on_sized(spot)
            .on_pointer_enter(move |_| hover.call(()))
            .child(rect().width(Size::px(iconsize::S)).child(if self.checked { icon(&t, Icon::Check, iconsize::S, ink) } else { rect().into_element() }));
        if let Some(f) = &self.face {
            e = e.child(rect().opacity(if self.disabled { 0.5 } else { 1. }).child(crate::view::accounts::head(&t, store, f.id, &f.name, f.color, frame::FACE)));
        }
        let mut e = e
            .child(line(&t, self.label.clone(), font::BODY, ink))
            .child(spacer())
            .content(Content::flex());
        if self.warn {
            e = e.child(icon(&t, Icon::Alert, iconsize::S, if self.hot && !self.disabled { t.on_accent } else { t.attention_text }));
        }
        if let Some(k) = &self.shortcut {
            e = e.child(line(&t, k.clone(), font::CALLOUT, if self.hot && !self.disabled { t.on_accent } else { t.text3 }));
        }
        if self.hot && !self.disabled {
            e = e.background(if self.danger { t.danger } else { t.accent });
        }
        if !self.disabled {
            e = e.on_press(move |_| press.call(()));
        }
        e
    }
}
