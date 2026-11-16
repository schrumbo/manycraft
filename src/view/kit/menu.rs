//! Context menus and pop-up lists: open at the pointer or under a control, driven by arrows, Enter and Esc.

use freya::prelude::*;

use super::text::{line, spacer};
use super::{Bounds, use_theme};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{font, frame, iconsize, space};
use crate::view::ui::Ui;

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
        run: EventHandler<()>,
    },
    Separator,
}

impl Item {
    /// An entry named `id` that runs `run`.
    pub fn new(id: impl Into<String>, label: impl Into<String>, run: impl FnMut(()) + 'static) -> Self {
        Self::Action { id: id.into(), label: label.into(), shortcut: None, checked: false, danger: false, disabled: false, run: EventHandler::new(run) }
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
}

impl Menu {
    /// A menu at the pointer.
    pub fn at(items: Vec<Item>, at: (f32, f32)) -> Self {
        Self { items, at, width: frame::MENU }
    }

    /// A menu that drops from `anchor`, as wide as it.
    pub fn under(items: Vec<Item>, anchor: Bounds) -> Self {
        Self { items, at: (anchor.x, anchor.y + anchor.h + space::XS), width: anchor.w.max(frame::MENU * 0.6) }
    }
}

/// Opens `menu`, closing any other.
pub fn open(mut ui: Ui, menu: Menu) {
    ui.tip.set_if_modified(None);
    ui.menu.set(Some(menu));
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
        let t = use_theme();
        let mut ui = use_consume::<Ui>();
        let mut hot = use_state(|| None::<usize>);
        let Some(menu) = ui.menu.read().clone() else {
            if hot.peek().is_some() {
                hot.set(None);
            }
            return rect().into_element();
        };
        let (w, h) = *ui.window.read();
        let rows = menu.items.iter().map(|i| if matches!(i, Item::Separator) { space::S + 1. } else { frame::MENU_ROW }).sum::<f32>() + space::XS * 2.;
        let width = menu.width.min(w - space::S * 2.);
        let x = menu.at.0.min(w - width - space::S).max(space::S);
        let y = if menu.at.1 + rows > h - space::S { (menu.at.1 - rows).max(space::S) } else { menu.at.1 };
        let items = menu.items.clone();
        let chosen = *hot.read();
        let run = move |i: usize, mut ui: Ui| {
            if let Some(Item::Action { run, disabled: false, .. }) = items.get(i) {
                let run = run.clone();
                ui.menu.set(None);
                run.call(());
            }
        };
        let keys_items = menu.items.clone();
        let keys_run = run.clone();
        let entries = menu.items.iter().enumerate().map(|(i, item)| match item {
            Item::Separator => rect().width(Size::fill()).padding((space::XS, space::S)).child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator)).into_element(),
            Item::Action { id, label, shortcut, checked, danger, disabled, .. } => {
                let mut run = run.clone();
                Entry { id: id.clone(), label: label.clone(), shortcut: shortcut.clone(), checked: *checked, danger: *danger, disabled: *disabled, hot: chosen == Some(i), on_hover: EventHandler::new(move |_| hot.set(Some(i))), on_press: EventHandler::new(move |_| run(i, ui)) }.into_element()
            }
        });
        rect()
            .position(Position::new_global().left(0.).top(0.))
            .layer(Layer::Overlay)
            .width(Size::px(w))
            .height(Size::px(h))
            .on_mouse_down(move |_| ui.menu.set(None))
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                let mut keys_run = keys_run.clone();
                match &e.key {
                    Key::Named(NamedKey::Escape) => ui.menu.set(None),
                    Key::Named(NamedKey::ArrowDown) => {
                        let next = step(&keys_items, *hot.peek(), true);
                        hot.set(next);
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        let next = step(&keys_items, *hot.peek(), false);
                        hot.set(next);
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(i) = *hot.peek() {
                            keys_run(i, ui);
                        }
                    }
                    _ => return,
                }
                e.stop_propagation();
            })
            .child(
                rect()
                    .position(Position::new_absolute().left(x).top(y))
                    .width(Size::px(width))
                    .padding(space::XS)
                    .background(t.raised)
                    .corner_radius(t.shape.control + 2.)
                    .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Outer))
                    .shadow(t.elevation.float.clone())
                    .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
                    .children(entries),
            )
            .into_element()
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
    hot: bool,
    on_hover: EventHandler<()>,
    on_press: EventHandler<()>,
}

impl Component for Entry {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
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
            .child(rect().width(Size::px(iconsize::S)).child(if self.checked { icon(&t, Icon::Check, iconsize::S, ink) } else { rect().into_element() }))
            .child(line(&t, self.label.clone(), font::BODY, ink))
            .child(spacer())
            .content(Content::flex());
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
