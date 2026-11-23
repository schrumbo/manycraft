//! Bildschirm: what the server shows a connection-only account. The map in its hand, an open menu to click, the titles,
//! and a chat line to answer them, all in one sheet.

use std::cell::RefCell;
use std::rc::Rc;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::app::model::{Game, Model, Need};
use crate::core::headless::{MAP_SIDE, map_rgba};
use crate::core::i18n::{tr, trf};
use crate::core::ipc::{Click, Menu, Slot, Titles};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::{caption, figure, line, para, spacer, strong};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{held, ring, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, WAITING, font, frame, iconsize, space};

const CELL_W: f32 = 72.;
const CELL_MIN: f32 = 44.;
const TITLE_LINES: usize = 3;
const CELL_H: f32 = 52.;
const NAME_LINES: usize = 2;
const MAP_BESIDE: f32 = 2.;
const MAP_ALONE: f32 = 3.;
const ROOM: f32 = 200.;
const PLAYER_SLOTS: usize = 36;
const HOTBAR: usize = 27;
const RECENT_LINES: usize = 3;

/// Opens the Bildschirm of the game `key`.
pub fn open(store: Store, key: &str) {
    store.dispatch(AppEvent::LookAtScreen(Some(key.to_owned())));
    sheet::open(store.ui, Screen { key: key.to_owned() });
}

/// The game whose Bildschirm the palette opens: the one selected, or else the first that shows something unseen, or else
/// the first that shows anything.
pub fn target(store: Store) -> Option<String> {
    let m = store.model.peek();
    let selected = store.ui.selection.peek();
    selected.iter().find(|k| selected.len() == 1 && m.games.get(*k).is_some_and(shows)).cloned().or_else(|| first_showing(&m))
}

fn shows(g: &Game) -> bool {
    g.connection && g.phase.is_running() && g.shows.any()
}

fn first_showing(m: &Model) -> Option<String> {
    let mut keys: Vec<(bool, &String)> = m.games.iter().filter(|(_, g)| shows(g)).map(|(k, g)| (g.need() != Some(Need::Screen), k)).collect();
    keys.sort();
    keys.first().map(|(_, k)| (*k).clone())
}

fn connected(g: &Game) -> bool {
    g.connection && g.phase.can_chat()
}

#[derive(PartialEq)]
struct Screen {
    key: String,
}

type Picture = Rc<RefCell<Option<(u64, ImageHandle)>>>;

impl Component for Screen {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let draft = use_state(String::new);
        let mut inventory = use_state(|| false);
        let picture: Picture = use_hook(Picture::default);
        use_drop(move || store.dispatch(AppEvent::LookAtScreen(None)));
        let _ = store.screen.read();
        let _ = store.chat.read();
        let m = store.model.read();
        let (w, h) = *store.ui.window.read();
        let key = self.key.clone();
        let game = m.games.get(&key);
        let live = game.is_some_and(connected);
        let name = game.and_then(|g| m.account(g.account_id)).map(|a| a.name.clone()).unwrap_or_default();
        let screen = m.screens.get(&key).filter(|_| live);
        let map = screen.and_then(|s| s.map.as_ref().map(|p| (s.map_seq, p)));
        let menu = screen.and_then(|s| s.menu.as_ref());
        let room = (w - space::L * 2.).max(frame::SHEET_NARROW) - space::XL * 2.;
        let columns = menu.map_or(9, |m| usize::from(m.columns.max(1)).max(9));
        let cell = ((room - columns.saturating_sub(1) as f32 * space::XS) / columns as f32).clamp(CELL_MIN, CELL_W);
        let grid_w = menu.map_or(0., |_| grid_width(columns, cell));
        let beside = menu.is_some() && map.is_some() && grid_w + MAP_SIDE as f32 * MAP_BESIDE + space::XL + space::XL * 2. + space::L * 2. <= w;
        let scale = if menu.is_some() { MAP_BESIDE } else { MAP_ALONE };
        let side = MAP_SIDE as f32 * scale;
        let content = match (menu.is_some(), map.is_some()) {
            (true, true) if beside => grid_w + side + space::XL,
            (true, _) => grid_w.max(if map.is_some() { side } else { 0. }),
            (false, true) => side,
            (false, false) => 0.,
        };
        let width = (content + space::XL * 2.).max(frame::SHEET).min((w - space::L * 2.).max(frame::SHEET_NARROW));

        let mut body = rect().width(Size::fill()).spacing(space::L);
        if let Some(titles) = screen.map(|s| &s.titles).filter(|ti| ti.title.is_some() || ti.subtitle.is_some() || ti.actionbar.is_some()) {
            body = body.child(titles_block(&t, titles));
        }
        let shown_map = map.and_then(|(seq, p)| {
            let mut held_picture = picture.borrow_mut();
            if held_picture.as_ref().is_none_or(|(at, _)| *at != seq) {
                *held_picture = ImageHandle::from_rgba(MAP_SIDE as u32, MAP_SIDE as u32, Bytes::from(map_rgba(&p.colors)), AlphaType::Unpremul).map(|i| (seq, i));
            }
            held_picture.as_ref().map(|(_, i)| map_block(&t, i.clone(), side))
        });
        let menu_block = menu.map(|mn| menu_block(&t, store, &key, mn, live, inventory, cell));
        match (shown_map, menu_block) {
            (Some(p), Some(mb)) if beside => body = body.child(rect().key("beside").horizontal().spacing(space::XL).child(p).child(mb)),
            (p, mb) => {
                if let Some(p) = p {
                    body = body.child(rect().key("map").width(Size::fill()).cross_align(Alignment::Center).child(p));
                }
                if let Some(mb) = mb {
                    body = body.child(mb);
                }
            }
        }
        if menu.is_none() && map.is_none() {
            let text = if live { tr("Gerade zeigt der Server nichts. Eine Karte in der Hand oder ein geöffnetes Menü erscheint hier von selbst.") } else { tr("Das Konto ist nicht verbunden.") };
            body = body.child(para(&t, text, font::BODY, t.text2));
        }
        if menu.is_some_and(|mn| mn.readable && mn.slots.len() >= usize::from(mn.top) + PLAYER_SLOTS) {
            let toggle = Button::new("screen.inventory", if *inventory.read() { tr("Inventar ausblenden") } else { tr("Inventar zeigen") }).plain().glyph(if *inventory.read() { Icon::ChevronUp } else { Icon::ChevronDown }).on_press(move |_| {
                let open = *inventory.peek();
                inventory.set(!open);
            });
            body = body.child(rect().key("toggle").horizontal().child(toggle));
        }
        let recent: Vec<String> = m.chat.iter().rev().filter(|l| l.reached(&key)).take(RECENT_LINES).map(|l| l.from.as_ref().map_or_else(|| l.text.clone(), |f| format!("<{f}> {}", l.text))).collect();
        if !recent.is_empty() {
            body = body.child(rect().key("recent").width(Size::fill()).spacing(space::XXS).children(recent.into_iter().rev().map(|l| line(&t, l, font::CALLOUT, t.text2).width(Size::fill()).max_lines(1).text_overflow(TextOverflow::Ellipsis).into_element())));
        }
        body = body.child(chat(store, &key, draft, live, map.is_some() || menu.is_some()));
        let scroll = ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px((h - frame::SHEET_TOP - ROOM).max(ROOM))).child(body);
        let ui = store.ui;
        let done = Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(ui)) };
        let heading = if name.is_empty() { tr("Bildschirm") } else { trf("Bildschirm von %@", &[&name]) };
        Card::new("screen", heading).body(scroll).secondary(done).secondary_id("screen.done").width(width)
    }
}

fn grid_width(columns: usize, cell: f32) -> f32 {
    columns as f32 * cell + columns.saturating_sub(1) as f32 * space::XS
}

fn titles_block(t: &Theme, titles: &Titles) -> Element {
    let mut column = rect().key("titles").width(Size::fill()).spacing(space::XXS);
    if let Some(title) = &titles.title {
        column = column.child(strong(t, title.text.clone(), font::TITLE3, t.text1).width(Size::fill()).max_lines(TITLE_LINES));
    }
    if let Some(subtitle) = &titles.subtitle {
        column = column.child(line(t, subtitle.text.clone(), font::BODY, t.text2).width(Size::fill()).max_lines(TITLE_LINES));
    }
    if let Some(bar) = &titles.actionbar {
        column = column.child(line(t, bar.text.clone(), font::CALLOUT, t.text2).width(Size::fill()).max_lines(TITLE_LINES));
    }
    column.into_element()
}

fn map_block(t: &Theme, handle: ImageHandle, side: f32) -> Element {
    let picture = image(handle).sampling_mode(SamplingMode::Nearest).aspect_ratio(AspectRatio::None).width(Size::px(side)).height(Size::px(side));
    rect()
        .spacing(space::XS)
        .child(rect().corner_radius(t.shape.small).background(t.well).overflow(Overflow::Clip).a11y_role(AccessibilityRole::Image).a11y_alt(tr("Karte in der Hand")).child(picture))
        .child(caption(t, tr("Karte in der Hand")))
        .into_element()
}

fn menu_block(t: &Theme, store: Store, key: &str, menu: &Menu, live: bool, inventory: State<bool>, cell: f32) -> Element {
    let carried = menu.carried.as_ref();
    let k = key.to_owned();
    let id = menu.id;
    let close = Button::new("screen.close", tr("Menü schließen"))
        .disabled(if !live { Some(tr("Das Konto ist nicht verbunden.")) } else { carried.filter(|_| menu.readable).map(|_| tr("Lege zuerst ab, was du hältst.")) })
        .on_press(move |_| store.dispatch(AppEvent::CloseMenu { key: k.clone(), menu: id }));
    let title = if menu.title.text.is_empty() { tr("Menü") } else { menu.title.text.clone() };
    let mut head = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(strong(t, title, font::BODY, t.text1).max_lines(1).text_overflow(TextOverflow::Ellipsis));
    if let Some(c) = carried {
        head = head.child(line(t, trf("Hält: %@", &[&slot_label(c)]), font::CALLOUT, t.attention_text).max_lines(1).text_overflow(TextOverflow::Ellipsis));
    }
    head = head.child(spacer()).content(Content::flex()).child(close);
    let mut column = rect().key("menu").spacing(space::S).width(Size::px(grid_width(usize::from(menu.columns.max(1)).max(9), cell))).child(head);
    if !menu.readable {
        return column.child(para(t, tr("Dieses Menü kann Manycraft nicht lesen. Schließen geht trotzdem."), font::BODY, t.text2)).into_element();
    }
    let top = usize::from(menu.top).min(menu.slots.len());
    let columns = usize::from(menu.columns.max(1));
    column = column.child(cells(key, menu, 0..top, columns, live, cell));
    if *inventory.read() && menu.slots.len() >= top + PLAYER_SLOTS {
        column = column
            .child(caption(t, tr("Inventar")))
            .child(cells(key, menu, top..top + HOTBAR, 9, live, cell))
            .child(cells(key, menu, top + HOTBAR..top + PLAYER_SLOTS, 9, live, cell));
    }
    column.into_element()
}

fn cells(key: &str, menu: &Menu, range: std::ops::Range<usize>, columns: usize, live: bool, width: f32) -> Element {
    let indices: Vec<usize> = range.collect();
    let rows = indices.chunks(columns).map(|row| {
        rect()
            .horizontal()
            .spacing(space::XS)
            .children(row.iter().map(|i| Cell { key: key.to_owned(), menu: menu.id, index: *i, slot: menu.slots.get(*i).cloned().flatten(), live, width }.into_element()))
            .into_element()
    });
    rect().spacing(space::XS).children(rows).into_element()
}

fn slot_label(slot: &Slot) -> String {
    if slot.count > 1 { format!("{} ×{}", slot.name.text, slot.count) } else { slot.name.text.clone() }
}

#[derive(PartialEq)]
struct Cell {
    key: String,
    menu: i32,
    index: usize,
    slot: Option<Slot>,
    live: bool,
    width: f32,
}

impl Component for Cell {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let label = self.slot.as_ref().map_or_else(|| tr("Leerer Platz"), slot_label);
        let spot = use_spot(&format!("screen.slot.{}", self.index), "slot", &label, self.live);
        let live = self.live;
        let (key, menu) = (self.key.clone(), self.menu);
        let Ok(index) = i16::try_from(self.index) else {
            return rect().into_element();
        };
        let click = move |click: Click| store.dispatch(AppEvent::ClickSlot { key: key.clone(), menu, slot: index, click });
        let right = click.clone();
        let fill = if self.slot.is_some() { t.control } else { t.well };
        let wash = if !live {
            Color::TRANSPARENT
        } else if *pressed.read() {
            t.press
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let mut cell = rect()
            .a11y_id(a11y)
            .a11y_focusable(live)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(label.clone())
            .width(Size::px(self.width))
            .height(Size::px(CELL_H))
            .corner_radius(t.shape.small)
            .background(fill)
            .on_sized(spot)
            .cursor(if live { CursorIcon::Pointer } else { CursorIcon::Default })
            .on_pointer_enter(move |_| hovered.set(true))
            .opacity(if live { 1. } else { WAITING })
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |e: Event<MouseEventData>| {
                if live && e.button == Some(MouseButton::Left) {
                    pressed.set(true);
                }
            })
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_all_press(move |e: Event<PressEventData>| {
                if !live {
                    return;
                }
                let modifiers = match &*e {
                    PressEventData::Mouse(m) if m.button != Some(MouseButton::Left) => return,
                    PressEventData::Keyboard(k) if matches!(k.key, Key::Named(NamedKey::Enter)) => k.modifiers,
                    PressEventData::Keyboard(_) => Modifiers::empty(),
                    _ => held() & Modifiers::SHIFT,
                };
                e.stop_propagation();
                a11y.request_focus();
                click(keyed(modifiers));
            })
            .on_secondary_down(move |e: Event<PressEventData>| {
                if live {
                    e.stop_propagation();
                    right(Click::Right);
                }
            });
        if let Some(border) = ring(&t, focus()) {
            cell = cell.border(border);
        }
        let mut inner = rect().width(Size::fill()).height(Size::fill()).padding(space::XS).spacing(space::XXS).corner_radius(t.shape.small).background(wash);
        if let Some(slot) = &self.slot {
            let mut top = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::XS);
            if let Some(rgb) = slot.rgb {
                let [_, r, g, b] = rgb.to_be_bytes();
                top = top.child(rect().width(Size::px(frame::DOT)).height(Size::px(frame::DOT)).corner_radius(t.shape.small / 2.).background(Color::from_rgb(r, g, b)));
            }
            if slot.glint {
                top = top.child(icon(&t, Icon::Sparkles, iconsize::XS, t.accent_text));
            }
            top = top.child(spacer()).content(Content::flex());
            if slot.count > 1 {
                top = top.child(figure(&t, slot.count.to_string(), font::CAPTION, t.text1).font_weight(FontWeight::SEMI_BOLD));
            }
            inner = inner.child(top).child(line(&t, slot.name.text.clone(), font::CAPTION2, t.text1).width(Size::fill()).max_lines(NAME_LINES).text_overflow(TextOverflow::Ellipsis));
        }
        let cell = cell.child(inner);
        let tip = self.slot.as_ref().map(|s| std::iter::once(slot_label(s)).chain(s.lore.iter().map(|l| l.text.clone())).collect::<Vec<_>>().join("\n"));
        WithTip::new(tip, cell).into_element()
    }
}

/// What a key press on a focused slot does: Shift moves the stack, Ctrl, Alt or Cmd takes or puts half, plain is a left click.
fn keyed(modifiers: Modifiers) -> Click {
    if modifiers.contains(Modifiers::SHIFT) {
        Click::Shift
    } else if modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) {
        Click::Right
    } else {
        Click::Left
    }
}

fn send(store: Store, key: &str, mut draft: State<String>) {
    let text = draft.peek().trim().to_owned();
    if text.is_empty() {
        return;
    }
    store.dispatch(AppEvent::SendChat { key: key.to_owned(), text });
    draft.set(String::new());
}

fn chat(store: Store, key: &str, draft: State<String>, live: bool, focus: bool) -> Element {
    let offline = (!live).then(|| tr("Das Konto ist nicht verbunden."));
    let (typed, pressed) = (key.to_owned(), key.to_owned());
    let empty = draft.read().trim().is_empty();
    let mut field = Field::new("screen.chat", draft).placeholder(tr("Nachricht oder /Befehl")).disabled(offline.clone()).on_submit(move |_: String| send(store, &typed, draft));
    if focus && live {
        field = field.auto_focus();
    }
    let button = Button::icon("screen.send", Icon::Send, tr("Senden")).disabled(offline.or_else(|| empty.then(|| tr("Schreib zuerst etwas.")))).on_press(move |_| send(store, &pressed, draft));
    rect().key("chat").width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(rect().width(Size::flex(1.)).child(field)).child(button).content(Content::flex()).into_element()
}

#[cfg(test)]
mod tests {
    use freya::prelude::Modifiers;

    use super::keyed;
    use crate::core::ipc::Click;

    #[test]
    fn keys_on_a_slot_map_to_the_three_clicks() {
        assert_eq!(keyed(Modifiers::empty()), Click::Left);
        assert_eq!(keyed(Modifiers::SHIFT), Click::Shift);
        assert_eq!(keyed(Modifiers::CONTROL), Click::Right);
        assert_eq!(keyed(Modifiers::META), Click::Right);
        assert_eq!(keyed(Modifiers::SHIFT | Modifiers::CONTROL), Click::Shift);
    }
}