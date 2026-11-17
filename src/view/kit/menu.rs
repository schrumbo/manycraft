//! Context menus and pop-up lists: open at the pointer or under a control, driven by arrows, Enter and Esc. Long
//! lists scroll and can carry a field that filters them as you type.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use freya::animation::*;
use freya::prelude::*;

use super::field::Field;
use super::text::{line, spacer};
use super::{Bounds, fades, use_theme};
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::store::use_store;
use crate::view::theme::{control, font, frame, iconsize, level, motion, space};
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
    /// Opens over its control with the checked entry on it, like a pop-up button's menu.
    pop: bool,
    /// Chooses the entry under the pointer when the button that opened it is let go, after a drag.
    drag: bool,
    /// The right edge of the control a menu drops from, which it lines up with when it would overflow the window.
    edge: Option<f32>,
    serial: u64,
}

impl Menu {
    /// A menu at the pointer.
    pub fn at(items: Vec<Item>, at: (f32, f32)) -> Self {
        Self { items, at, width: frame::MENU, search: None, pop: false, drag: false, edge: None, serial: 0 }
    }

    /// A menu that drops from `anchor`, as wide as it.
    pub fn under(items: Vec<Item>, anchor: Bounds) -> Self {
        Self { items, at: (anchor.x, anchor.y + anchor.h + space::XS), width: anchor.w.max(frame::MENU * 0.6), search: None, pop: false, drag: false, edge: Some(anchor.x + anchor.w), serial: 0 }
    }

    /// A menu that opens over `anchor`: its checked entry covers the control and its text starts where the control's
    /// text does, `inset` from the control's left edge.
    pub fn over(items: Vec<Item>, anchor: Bounds, inset: f32) -> Self {
        let lead = LEAD - inset;
        let row = items.iter().position(|i| matches!(i, Item::Action { checked: true, .. })).map_or(0., |i| top(&items, i));
        let y = anchor.y + (anchor.h - frame::MENU_ROW) / 2. - space::XS - row;
        Self { items, at: (anchor.x - lead, y), width: anchor.w + lead, search: None, pop: true, drag: false, edge: None, serial: 0 }
    }

    /// Lets the entry under the pointer be chosen by letting go of the mouse button that opened the menu, once the pointer
    /// moved to another entry or the button was held a moment.
    pub fn dragged(mut self) -> Self {
        self.drag = true;
        self
    }

    /// Adds a field above the list that filters it by label.
    pub fn searchable(mut self, placeholder: impl Into<String>) -> Self {
        self.search = Some(placeholder.into());
        self
    }
}

const WIDE_GLYPH: f32 = 0.8;
const LEAD: f32 = space::XS + space::S + iconsize::S + space::XS;
const TYPED: Duration = Duration::from_millis(900);
const HELD: Duration = Duration::from_millis(350);

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

fn fits(item: &Item, lead: bool) -> f32 {
    let Item::Action { label, shortcut, face, warn, .. } = item else { return 0. };
    let mut w = space::S * 2. + glyphs(label, font::BODY);
    if lead {
        w += iconsize::S + space::XS;
    }
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
            .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
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
        let checked = menu.items.iter().position(|i| matches!(i, Item::Action { checked: true, .. }));
        let mut hot = use_state(|| (String::new(), if menu.pop { checked } else { None }));
        let typed = use_state(|| (String::new(), Instant::now()));
        let born = use_hook(Instant::now);
        let mut down_in = use_state(|| false);
        let entered = use_state(|| (None::<usize>, false));
        let drag = menu.drag;
        let fade = use_animation(|conf| {
            conf.on_creation(OnCreation::Run);
            AnimNum::new(0., 1.).duration(motion::timed(motion::FAST)).ease(Ease::Out)
        });
        let (w, h) = *ui.window.read();
        let list_max = (frame::MENU_ROW * frame::MENU_ROWS as f32).min(h * 0.6);
        let lead = checked.is_some();
        let start = checked.map_or(0., |i| (top(&menu.items, i) - list_max / 2. + frame::MENU_ROW / 2.).max(0.));
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
        let fit = menu.items.iter().map(|i| fits(i, lead)).fold(0., f32::max) + space::XS * 2. + 2.;
        let width = menu.width.max(fit).min(w - space::S * 2.);
        let x = match menu.edge {
            Some(edge) if menu.at.0 + width > w - space::S => edge - width,
            _ => menu.at.0,
        }
        .min(w - width - space::S)
        .max(space::S);
        let y = if menu.pop {
            menu.at.1.min(h - space::S - panel_h).max(space::S)
        } else if menu.at.1 + tallest > h - space::S {
            (menu.at.1 - tallest).max(space::S)
        } else {
            menu.at.1
        };
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
                    let mut release_run = run.clone();
                    let release = EventHandler::new(move |_| {
                        if drag && !*down_in.peek() && (born.elapsed() > HELD || entered.peek().1) {
                            release_run(i, ui);
                        }
                    });
                    let hover = EventHandler::new(move |_| {
                        let mut entered = entered;
                        let now = *entered.peek();
                        match now {
                            (None, _) => entered.set((Some(i), false)),
                            (Some(first), false) if first != i => entered.set((Some(first), true)),
                            _ => {}
                        }
                        hot.set((at.clone(), Some(i)));
                    });
                    Entry { id: id.clone(), label: label.clone(), shortcut: shortcut.clone(), checked: *checked, lead, on_release: release, danger: *danger, disabled: *disabled, face: face.clone(), warn: *warn, hot: chosen == Some(i), on_hover: hover, on_press: EventHandler::new(move |_| run(i, ui)) }.into_element()
                }
            })
            .collect();
        let mut panel = rect()
            .position(Position::new_absolute().left(x).top(y))
            .a11y_role(AccessibilityRole::Menu)
            .a11y_builder(|n| n.set_modal())
            .width(Size::px(width))
            .opacity(fade.get().value())
            .padding(space::XS)
            .background(t.raised)
            .corner_radius(t.shape.control + 2.)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Outer))
            .shadow(t.elevation.float.clone())
            .on_mouse_down(move |e: Event<MouseEventData>| {
                down_in.set(true);
                e.stop_propagation();
            })
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
                    Key::Character(_) if searching || e.modifiers.intersects(Modifiers::CONTROL | Modifiers::META | Modifiers::ALT) => return,
                    Key::Character(c) => {
                        let mut typed = typed;
                        let (so_far, at) = typed.peek().clone();
                        let so_far = if at.elapsed() > TYPED { String::new() } else { so_far };
                        if c == " " && so_far.is_empty() {
                            if let Some(i) = chosen {
                                keys_run(i, ui);
                            }
                        } else {
                            let word = format!("{so_far}{}", c.to_lowercase());
                            let from = if so_far.is_empty() { chosen.map_or(0, |i| i + 1) } else { chosen.unwrap_or(0) };
                            let n = keys_items.len();
                            let found = (0..n).map(|k| (from + k) % n).find(|&i| matches!(&keys_items[i], Item::Action { label, disabled: false, .. } if label.to_lowercase().starts_with(&word)));
                            typed.set((word, Instant::now()));
                            if let Some(next) = found {
                                hot.set((keys_q.clone(), Some(next)));
                                reveal(next, scroll, &keys_items);
                            }
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
            entries.insert(0, rect().width(Size::fill()).height(Size::px(frame::MENU_ROW)).padding((0., space::S)).cross_align(Alignment::Center).main_align(Alignment::Center).child(line(&t, tr("Nichts gefunden"), font::CALLOUT, t.text3)).into_element());
        }
        if rows <= list_h {
            return panel.children(entries).into_element();
        }
        let edges = fades(t.raised, -scroll.position().y, rows, list_h);
        panel.child(rect().width(Size::fill()).height(Size::px(list_h)).child(ScrollView::new_controlled(scroll).width(Size::fill()).height(Size::px(list_h)).children(entries)).children(edges)).into_element()
    }
}

#[derive(Clone, PartialEq)]
struct Entry {
    id: String,
    label: String,
    shortcut: Option<String>,
    checked: bool,
    lead: bool,
    danger: bool,
    disabled: bool,
    face: Option<Face>,
    warn: bool,
    hot: bool,
    on_release: EventHandler<()>,
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
        let release = self.on_release.clone();
        let mut e = rect()
            .a11y_role(AccessibilityRole::MenuItem)
            .a11y_alt(self.label.clone())
            .width(Size::fill())
            .height(Size::px(frame::MENU_ROW))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .corner_radius(t.shape.small)
            .cursor(if self.disabled { CursorIcon::NotAllowed } else { CursorIcon::Pointer })
            .on_sized(spot)
            .on_pointer_enter(move |_| hover.call(()))
            .on_mouse_up(move |e: Event<MouseEventData>| {
                if e.button == Some(MouseButton::Left) {
                    release.call(());
                }
            });
        if self.lead {
            e = e.child(rect().width(Size::px(iconsize::S)).child(if self.checked { icon(&t, Icon::Check, iconsize::S, ink) } else { rect().into_element() }));
        }
        if let Some(f) = &self.face {
            e = e.child(rect().opacity(if self.disabled { 0.5 } else { 1. }).child(crate::view::accounts::head(&t, store, f.id, &f.name, f.color, frame::FACE)));
        }
        let mut e = e
            .child(line(&t, self.label.clone(), font::BODY, ink))
            .child(spacer())
            .content(Content::flex());
        if self.warn {
            e = e.child(icon(&t, Icon::Alert, iconsize::S, if self.disabled { t.text3 } else if self.hot { t.on_accent } else { t.attention_text }));
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
