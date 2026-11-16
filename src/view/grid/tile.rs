use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use freya::elements::image::ImageHandle;
use freya::prelude::*;

use super::layout::{TWO_LINES, strip};
use crate::app::event::Event as AppEvent;
use crate::app::model::{Phase, Primary, primary, status, stopped_ban};
use crate::core::account::Account;
use crate::core::i18n::tr;
use crate::view::commands::Command;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::line;
use crate::view::kit::{held, use_theme};
use crate::view::store::{Store, use_store};
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
        Primary::SignIn => {
            let mut page = store.ui.page;
            page.set(Page::Accounts);
        }
    }
}

fn glyph(p: Primary) -> Icon {
    match p {
        Primary::Play | Primary::Start => Icon::Play,
        Primary::Reconnect | Primary::Retry | Primary::Respawn => Icon::Refresh,
        Primary::SignIn => Icon::LogIn,
    }
}

fn pick(store: Store, key: &str, index: usize) {
    let mut ui = store.ui;
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

fn context(store: Store, key: &str) -> Vec<Item> {
    let mut ui = store.ui;
    if !ui.selection.peek().contains(key) {
        ui.selection.set(BTreeSet::from([key.to_owned()]));
        ui.cursor.set(Some(key.to_owned()));
    }
    let many = ui.selection.peek().len() > 1;
    let mut items = Vec::new();
    let m = store.model.peek();
    if !many
        && let Some(g) = m.games.get(key)
        && let Some(p) = primary(&g.phase, g.stopping)
    {
        let k = key.to_owned();
        items.push(Item::new(format!("menu.{key}.primary"), p.label(), move |_| run(store, &k, p)));
    }
    for c in [Command::StartSelected, Command::StopSelected, Command::RestartSelected, Command::ReconnectSelected] {
        if many || c != Command::StartSelected || m.games.get(key).is_some_and(|g| primary(&g.phase, g.stopping) != Some(Primary::Start)) {
            let label = if many { c.label() } else { single(c) };
            let mut item = Item::new(format!("menu.{}", c.name()), label, move |_| c.run(store)).disabled(!c.available(store));
            if let Some(s) = c.shortcut() {
                item = item.shortcut(s.label());
            }
            items.push(item);
        }
    }
    if let Some(g) = m.games.get(key) {
        let instance = g.instance_id.clone();
        items.push(Item::Separator);
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
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut last_press = use_state(|| None::<Instant>);
        let m = store.model.read();
        let Some(game) = m.games.get(&self.key) else {
            return rect().into_element();
        };
        let selected = store.ui.selection.read().contains(&self.key);
        let preview = store.previews.read().get(&self.key).cloned();
        let face = store.faces.read().get(&game.account_id).cloned();
        let attention = game.phase.needs_attention();
        let severe = matches!(game.phase, Phase::LoginNeeded | Phase::Banned { .. });
        let line2 = stopped_ban(game, &m).unwrap_or_else(|| status(game, m.preparing.get(&game.batch_id).copied()));
        let action = primary(&game.phase, game.stopping);
        let spot = use_spot(&format!("tile.{}", self.key), "tile", &format!("{} · {line2}", self.name), true);
        let preview_h = self.height - strip(self.width);
        let two = self.width >= TWO_LINES;
        let hover = *hovered.read();
        let (key, index) = (self.key.clone(), self.index);
        let (enter_key, menu_key) = (key.clone(), key.clone());
        let tone = if severe { t.danger_text } else if attention { t.attention_text } else { t.text2 };
        let ring_color = if selected { Some(t.accent) } else if severe { Some(t.danger) } else if attention { Some(t.attention) } else { None };

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
        let strip_row = rect().width(Size::fill()).height(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).padding((0., space::XXS)).child(face_view(&t, m.account(game.account_id), face, matches!(game.phase, Phase::Playing { .. }) && !game.stopping)).child(text).content(Content::flex());

        let mut tile = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(format!("{}, {line2}", self.name))
            .width(Size::px(self.width))
            .height(Size::px(self.height))
            .corner_radius(t.shape.tile + frame::RING_GAP)
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_mouse_down(move |e: Event<MouseEventData>| e.stop_propagation())
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                a11y.request_focus();
                if matches!(*e, PressEventData::Keyboard(_)) {
                    if let Some(p) = primary_of(store, &enter_key) {
                        run(store, &enter_key, p);
                    }
                    return;
                }
                let now = Instant::now();
                let double = last_press.peek().is_some_and(|at| now - at < DOUBLE);
                last_press.set(if double { None } else { Some(now) });
                if double {
                    if let Some(p) = primary_of(store, &enter_key) {
                        run(store, &enter_key, p);
                    }
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
        if let Some(c) = ring_color {
            tile = tile.border(Border::new().fill(c).width(frame::RING).alignment(BorderAlignment::Outer));
        } else if let Some(b) = crate::view::kit::ring(&t, focus()) {
            tile = tile.border(b);
        }
        tile.into_element()
    }
}

fn primary_of(store: Store, key: &str) -> Option<Primary> {
    let m = store.model.peek();
    let g = m.games.get(key)?;
    primary(&g.phase, g.stopping)
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

fn face_view(t: &Theme, account: Option<&Account>, face: Option<ImageHandle>, playing: bool) -> Element {
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
