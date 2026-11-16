use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use freya::elements::image::ImageHandle;
use freya::prelude::*;

use super::layout::{TWO_LINES, strip};
use super::words;
use crate::app::event::Event as AppEvent;
use crate::app::model::{Game, Model, Phase, Primary, main_action, status, stopped_ban};
use crate::core::account::Account;
use crate::core::i18n::tr;
use crate::view::commands::Command;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{held, use_theme};
use crate::view::store::{Store, use_preview, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};
use crate::view::ui::Page;

const DOUBLE: Duration = Duration::from_millis(450);

/// One game in the grid: its newest preview, and under it its face, name and status.
#[derive(PartialEq)]
pub struct Tile {
    pub key: String,
    pub name: String,
    pub width: f32,
    pub height: f32,
    pub index: usize,
}

fn run(store: Store, key: &str, p: Primary) {
    let key = key.to_owned();
    match p {
        Primary::Play => store.dispatch(AppEvent::Play(key)),
        Primary::Reconnect => store.dispatch(AppEvent::Reconnect(key)),
        Primary::Respawn => store.dispatch(AppEvent::Respawn(key)),
        Primary::Start | Primary::Retry => store.dispatch(AppEvent::Start(vec![key])),
        Primary::TakeOver => store.dispatch(AppEvent::TakeOver(key)),
        Primary::SignIn => {
            let mut page = store.ui.page;
            page.set(Page::Accounts);
        }
    }
}

fn glyph(p: Primary) -> Icon {
    match p {
        Primary::Play | Primary::Start => Icon::Play,
        Primary::Reconnect | Primary::Retry | Primary::Respawn | Primary::TakeOver => Icon::Refresh,
        Primary::SignIn => Icon::LogIn,
    }
}

fn action_of(g: &Game, m: &Model) -> Option<Primary> {
    let busy = !g.phase.is_active() && m.playing_elsewhere(&g.instance_id, g.account_id, &g.key).is_some();
    main_action(&g.phase, g.stopping, busy)
}

/// Forgets the pointer over `key` once it leaves the tile or row.
pub(super) fn unhover(store: Store, key: &str) {
    store.watch(|w| {
        if w.hovered.as_deref() == Some(key) {
            w.hovered = None;
        }
    });
}

pub(super) fn pick(store: Store, key: &str, index: usize) {
    let mut ui = store.ui;
    ui.history.set_if_modified(None);
    let mods = held();
    let toggle = if cfg!(target_os = "macos") { mods.contains(Modifiers::META) } else { mods.contains(Modifiers::CONTROL) };
    let mut selection = ui.selection.peek().clone();
    if mods.contains(Modifiers::SHIFT)
        && let Some(anchor) = ui.cursor.peek().clone()
    {
        let keys = super::shown_keys(store);
        if let Some(from) = keys.iter().position(|k| *k == anchor) {
            let (a, b) = if from <= index { (from, index) } else { (index, from) };
            if !toggle {
                selection.clear();
            }
            selection.extend(keys[a..=b.min(keys.len().saturating_sub(1))].iter().cloned());
            ui.selection.set(selection);
            return;
        }
    }
    super::reset_anchor();
    if toggle {
        if !selection.remove(key) {
            selection.insert(key.to_owned());
        }
    } else {
        selection = BTreeSet::from([key.to_owned()]);
    }
    ui.selection.set(selection);
    ui.cursor.set(Some(key.to_owned()));
}

pub(super) fn context(store: Store, key: &str) -> Vec<Item> {
    let mut ui = store.ui;
    if !ui.selection.peek().contains(key) {
        ui.selection.set(BTreeSet::from([key.to_owned()]));
        ui.cursor.set(Some(key.to_owned()));
    }
    let many = ui.selection.peek().len() > 1;
    let mut items = Vec::new();
    let m = store.model.peek();
    let primary = if many { None } else { m.games.get(key).and_then(|g| action_of(g, &m)) };
    if let Some(p) = primary {
        let k = key.to_owned();
        let mut item = Item::new(format!("menu.{key}.primary"), p.label(), move |_| run(store, &k, p));
        let same = match p {
            Primary::Start => Some(Command::StartSelected),
            Primary::Reconnect => Some(Command::ReconnectSelected),
            _ => None,
        };
        if let Some(s) = same.filter(|c| c.available(store)).and_then(Command::shortcut) {
            item = item.shortcut(s.label());
        }
        items.push(item);
    }
    for c in [Command::StartSelected, Command::StopSelected, Command::RestartSelected, Command::ReconnectSelected] {
        let doubled = match c {
            Command::StartSelected => matches!(primary, Some(Primary::Start | Primary::TakeOver)),
            Command::ReconnectSelected => matches!(primary, Some(Primary::Reconnect)),
            _ => false,
        };
        if !doubled {
            let label = if many { c.label() } else { single(c) };
            if !c.available(store) {
                continue;
            }
            let mut item = Item::new(format!("menu.{}", c.name()), label, move |_| c.run(store));
            if let Some(s) = c.shortcut() {
                item = item.shortcut(s.label());
            }
            items.push(item);
        }
    }
    if let Some(g) = m.games.get(key) {
        let instance = g.instance_id.clone();
        if !items.is_empty() {
            items.push(Item::Separator);
        }
        if !many {
            let k = key.to_owned();
            items.push(Item::new(format!("menu.{key}.history"), tr("Verlauf"), move |_| toggle_history(store, &k)).shortcut(tr("Leertaste")));
        }
        items.push(Item::new(format!("menu.{key}.instance"), tr("Instanz öffnen"), move |_| {
            let mut page = store.ui.page;
            page.set(Page::Instance(instance.clone()));
        }));
    }
    items
}

fn single(c: Command) -> String {
    match c {
        Command::StartSelected => tr("Starten"),
        Command::StopSelected => tr("Stoppen"),
        Command::RestartSelected => tr("Neu starten"),
        Command::ReconnectSelected => tr("Neu verbinden"),
        other => other.label(),
    }
}

impl Component for Tile {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let gone = self.key.clone();
        use_drop(move || unhover(store, &gone));
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut track = use_state(Track::default);
        super::use_node(&self.key, a11y);
        let preview = use_preview(&self.key);
        let m = store.model.read();
        let Some(game) = m.games.get(&self.key) else {
            return rect().into_element();
        };
        let selected = store.ui.selection.read().contains(&self.key);
        let face = store.faces.read().get(&game.account_id).cloned();
        let attention = game.phase.needs_attention();
        let severe = matches!(game.phase, Phase::LoginNeeded | Phase::Banned { .. });
        let detail = stopped_ban(game, &m).unwrap_or_else(|| status(game, m.preparing.get(&game.batch_id).copied()));
        let reason = words::reason(game, &m);
        let line2 = words::line(game, &m);
        let action = action_of(game, &m);
        let spot = use_spot(&format!("tile.{}", self.key), "tile", &format!("{} · {line2}", self.name), true);
        let preview_h = self.height - strip(self.width);
        let two = self.width >= TWO_LINES;
        let hover = *hovered.read();
        let (key, index) = (self.key.clone(), self.index);
        let (enter_key, menu_key) = (key.clone(), key.clone());
        let tone = if severe { t.danger_text } else if attention { t.attention_text } else { t.text2 };
        let state_ring = if severe { Some(t.danger) } else if attention { Some(t.attention) } else { None };

        let mut pic = rect().width(Size::fill()).height(Size::px(preview_h)).background(t.well).corner_radius(t.shape.tile).overflow(Overflow::Clip);
        pic = match &preview {
            Some(handle) => pic.child(frame_image(handle.clone(), self.width, preview_h, if attention { 0.55 } else { 1. })),
            None => pic.center().child(placeholder(&t, &game.phase, face.clone())),
        };
        if game.phase == Phase::Stopped && preview.is_some() {
            pic = pic.child(rect().position(Position::new_absolute().left(0.).top(0.)).width(Size::fill()).height(Size::fill()).background(t.surface.with_a((t.hidden_dim * 255.) as u8)));
        }
        if let Some(p) = action.filter(|_| hover || focus() == Focus::Keyboard) {
            let compact = self.width < TWO_LINES;
            let k = key.clone();
            pic = pic.child(rect().position(Position::new_absolute().right(space::S).bottom(space::S)).child(pill(&t, p, compact).on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                run(store, &k, p);
            })));
        }

        let mut text = rect().width(Size::flex(1.)).spacing(1.);
        let name = line(&t, self.name.clone(), font::CALLOUT, t.text1).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis);
        if two {
            text = text.child(name).child(line(&t, line2.clone(), font::CAPTION, tone).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        } else {
            let short = if attention || game.phase.is_running() { line2.clone() } else { String::new() };
            text = text.horizontal().spacing(space::XS).child(name);
            if !short.is_empty() && attention {
                text = text.child(crate::view::kit::badge::dot(tone));
            }
        }
        let text = if two {
            text.into_element()
        } else {
            let tip = match &reason {
                Some(r) if detail.contains(r.as_str()) => detail.clone(),
                Some(r) if *r != line2 => format!("{detail}
{r}"),
                _ => line2.clone(),
            };
            WithTip::new(Some(tip), text.width(Size::fill())).into_element()
        };
        let text = rect().width(Size::flex(1.)).child(text);
        let strip_row = rect().width(Size::fill()).height(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).padding((0., space::XXS)).child(face_view(&t, m.account(game.account_id), face, matches!(game.phase, Phase::Playing { .. }) && !game.stopping)).child(text).content(Content::flex());

        let (over_key, leave_key) = (self.key.clone(), self.key.clone());
        let mut tile = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(format!("{}, {line2}", self.name))
            .width(Size::px(self.width))
            .height(Size::px(self.height))
            .corner_radius(t.shape.tile + frame::RING_GAP)
            .on_sized(spot)
            .on_pointer_enter(move |_| {
                hovered.set(true);
                store.watch(|w| w.hovered = Some(over_key.clone()));
            })
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                unhover(store, &leave_key);
            })
            .cursor(CursorIcon::Pointer)
            .on_mouse_down(move |e: Event<MouseEventData>| e.stop_propagation())
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                a11y.request_focus();
                if let PressEventData::Keyboard(k) = &*e {
                    keyed(store, &enter_key, k);
                } else if track.write().double(Instant::now()) {
                    primary(store, &enter_key);
                } else {
                    pick(store, &enter_key, index);
                }
            })
            .on_secondary_down(move |e: Event<PressEventData>| {
                e.stop_propagation();
                if let PressEventData::Mouse(m) = &*e {
                    let at = (m.global_location.x as f32, m.global_location.y as f32);
                    menu::open(store.ui, Menu::at(context(store, &menu_key), at));
                }
            })
            .child(pic)
            .child(strip_row);
        let keyboard = crate::view::kit::ring(&t, focus()).map(|b| b.fill);
        let rings = keyboard.into_iter().chain(selected.then_some(t.accent)).chain(state_ring);
        for (i, c) in rings.take(2).enumerate() {
            tile = tile.border(Border::new().fill(c).width(frame::RING).alignment(if i == 0 { BorderAlignment::Outer } else { BorderAlignment::Inner }));
        }
        tile.into_element()
    }
}

/// Runs the main action of game `key`, if it has one.
pub(super) fn primary(store: Store, key: &str) {
    if let Some(p) = primary_of(store, key) {
        run(store, key, p);
    }
}

/// What a key press on a focused tile or row does: Enter runs its main action, Space opens its history, Shift+Space stops
/// what is selected.
pub(super) fn keyed(store: Store, key: &str, e: &KeyboardEventData) {
    if e.key == Key::Named(NamedKey::Enter) {
        primary(store, key);
    } else if e.modifiers.contains(Modifiers::SHIFT) {
        if Command::StopSelected.available(store) {
            Command::StopSelected.run(store);
        }
    } else {
        toggle_history(store, key);
    }
}

/// Opens the history of game `key`, or closes it when it is open.
pub(super) fn toggle_history(store: Store, key: &str) {
    let mut history = store.ui.history;
    let open = history.peek().as_deref() == Some(key);
    history.set(if open { None } else { Some(key.to_owned()) });
}

/// Tells a double press from two single ones.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Track {
    last: Option<Instant>,
}

impl Track {
    /// Notes a press at `now`; true when it completes a double press.
    pub(super) fn double(&mut self, now: Instant) -> bool {
        let double = self.last.is_some_and(|at| now - at < DOUBLE);
        self.last = if double { None } else { Some(now) };
        double
    }
}

/// The newest events of a game: when, what, and why.
#[derive(PartialEq)]
pub(super) struct History {
    pub key: String,
}

impl Component for History {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let spot = use_spot(&format!("history.{}", self.key), "popover", &tr("Verlauf"), true);
        let moments = words::moments(&self.key, &store.model.read());
        let mut list = rect().width(Size::fill()).spacing(space::XS);
        if moments.is_empty() {
            list = list.child(line(&t, tr("Noch nichts passiert."), font::CALLOUT, t.text2));
        }
        for m in moments {
            let mut row = rect().width(Size::fill()).horizontal().spacing(space::S).child(line(&t, m.when, font::CALLOUT, t.text3).width(Size::px(HISTORY_WHEN)).max_lines(1)).child(line(&t, m.what, font::CALLOUT, t.text1).font_weight(FontWeight::MEDIUM).max_lines(1));
            if !m.reason.is_empty() {
                row = row.child(line(&t, m.reason, font::CALLOUT, t.text2).width(Size::flex(1.)).max_lines(1).text_overflow(TextOverflow::Ellipsis)).content(Content::flex());
            }
            list = list.child(row);
        }
        let mut card = rect()
            .a11y_role(AccessibilityRole::Dialog)
            .a11y_alt(tr("Verlauf"))
            .width(Size::px(HISTORY_W))
            .padding(space::M)
            .spacing(space::S)
            .corner_radius(t.shape.tile)
            .background(t.raised)
            .layer(Layer::Overlay)
            .on_sized(spot)
            .on_mouse_down(move |e: Event<MouseEventData>| e.stop_propagation())
            .child(line(&t, tr("Verlauf"), font::CALLOUT, t.text2).font_weight(FontWeight::SEMI_BOLD))
            .child(list);
        card = card.shadow(t.elevation.float.clone());
        card
    }
}

const HISTORY_W: f32 = 320.;
const HISTORY_WHEN: f32 = 72.;

fn primary_of(store: Store, key: &str) -> Option<Primary> {
    let m = store.model.peek();
    let g = m.games.get(key)?;
    action_of(g, &m)
}

fn frame_image(handle: ImageHandle, w: f32, h: f32, opacity: f32) -> impl IntoElement {
    image(handle).width(Size::px(w)).height(Size::px(h)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center).opacity(opacity)
}

fn placeholder(t: &Theme, phase: &Phase, face: Option<ImageHandle>) -> Element {
    let glyph = match phase {
        Phase::Stopped => None,
        Phase::LoginNeeded => Some(Icon::LogIn),
        Phase::Crashed { .. } | Phase::Failed { .. } | Phase::Dead { .. } => Some(Icon::Alert),
        Phase::Disconnected { .. } | Phase::NetworkLost { .. } | Phase::NoNetwork => Some(Icon::Wifi),
        _ => None,
    };
    match (glyph, face) {
        (Some(g), _) => icon(t, g, iconsize::L, t.text3),
        (None, Some(face)) => image(face).width(Size::px(frame::FACE * 2.)).height(Size::px(frame::FACE * 2.)).opacity(0.6).into_element(),
        (None, None) => rect().into_element(),
    }
}

pub(super) fn face_view(t: &Theme, account: Option<&Account>, face: Option<ImageHandle>, playing: bool) -> Element {
    let fill = account.map_or(t.control, |a| t.identity(usize::from(a.color())));
    let mut chip = rect().width(Size::px(frame::FACE)).height(Size::px(frame::FACE)).corner_radius(t.shape.small).background(fill).center().overflow(Overflow::Clip);
    chip = match (face, account) {
        (Some(face), _) => chip.padding(1.5).child(image(face).width(Size::fill()).height(Size::fill()).corner_radius(t.shape.small / 2.)),
        (None, Some(a)) => chip.child(label().text(a.name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()).font_size(font::CAPTION).font_weight(FontWeight::BOLD).color(t.ink_on(fill))),
        (None, None) => chip,
    };
    let mut r = rect().width(Size::px(frame::FACE)).height(Size::px(frame::FACE)).child(chip);
    if playing {
        r = r.child(rect().position(Position::new_absolute().right(0.).bottom(0.)).width(Size::px(frame::DOT)).height(Size::px(frame::DOT)).corner_radius(frame::DOT).background(t.success).border(Border::new().fill(t.surface).width(1.5).alignment(BorderAlignment::Inner)));
    }
    r.into_element()
}

fn pill(t: &Theme, p: Primary, compact: bool) -> Rect {
    let (fill, ink) = if p == Primary::SignIn { (t.danger, t.on_danger) } else { (t.accent, t.on_accent) };
    let label = p.label();
    let mut r = rect()
        .a11y_role(AccessibilityRole::Button)
        .a11y_alt(label.clone())
        .height(Size::px(control::S))
        .horizontal()
        .center()
        .spacing(space::XS)
        .corner_radius(control::S)
        .background(fill)
        .cursor(CursorIcon::Pointer)
        .shadow(t.elevation.float.clone())
        .child(icon(t, glyph(p), iconsize::XS, ink));
    if compact {
        r = r.width(Size::px(control::S));
    } else {
        r = r.padding((0., space::M, 0., space::S)).child(line(t, label, font::CALLOUT, ink).font_weight(FontWeight::SEMI_BOLD));
    }
    r
}
