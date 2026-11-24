use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use super::layout::{self, TWO_LINES};
use super::words;
use crate::app::event::Event as AppEvent;
use crate::app::model::{Game, Model, Phase, Primary, status, stopped_ban};
use crate::core::account::Account;
use crate::core::i18n::{tr, trf};
use crate::view::commands::Command;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::{Tone, badge};
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{held, use_theme};
use crate::view::store::{Store, use_preview, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, radius, space};
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
        Primary::ForceQuit => store.dispatch(AppEvent::Stop(vec![key])),
        Primary::Screenshot => store.dispatch(AppEvent::Capture(vec![key])),
        Primary::AcceptPack => store.dispatch(AppEvent::AnswerPack { keys: vec![key], accept: true }),
        Primary::AcceptRules => store.dispatch(AppEvent::AnswerRules { keys: vec![key], accept: true }),
        Primary::Chat => open_chat(store, &key),
        Primary::Screen => crate::view::sheets::screen::open(store, &key),
        Primary::SignIn => {
            let mut page = store.ui.page;
            page.set(Page::ACCOUNTS);
        }
    }
}

fn glyph(p: Primary) -> Icon {
    match p {
        Primary::Play | Primary::Start => Icon::Play,
        Primary::Reconnect | Primary::Retry | Primary::Respawn | Primary::TakeOver => Icon::Refresh,
        Primary::SignIn => Icon::LogIn,
        Primary::ForceQuit => Icon::Stop,
        Primary::Screenshot => Icon::Image,
        Primary::AcceptPack => Icon::Package,
        Primary::AcceptRules => Icon::CircleCheck,
        Primary::Chat => Icon::Message,
        Primary::Screen => Icon::Image,
    }
}

fn open_chat(store: Store, key: &str) {
    let mut shared = store.ui.chat;
    shared.target.set(Some(crate::view::chat::Target::Instance(key.to_owned())));
    crate::view::chat::focus(store);
}

fn action_of(g: &Game, m: &Model) -> Option<Primary> {
    let busy = !g.phase.is_active() && m.playing_elsewhere(&g.instance_id, g.account_id, &g.key).is_some();
    g.action(busy)
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
            Primary::Screenshot => Some(Command::ScreenshotSelected),
            Primary::AcceptPack => Some(Command::AcceptPacks),
            Primary::AcceptRules => Some(Command::AcceptRules),
            _ => None,
        };
        if let Some(s) = same.filter(|c| c.available(store)).and_then(Command::shortcut) {
            item = item.shortcut(s.label());
        }
        items.push(item);
    }
    if !many && primary != Some(Primary::Screen) && m.games.get(key).is_some_and(|g| g.connection && g.phase.is_running() && g.shows.any()) {
        let k = key.to_owned();
        let mut item = Item::new(format!("menu.{key}.screen"), tr("Bildschirm"), move |_| crate::view::sheets::screen::open(store, &k));
        if let Some(s) = Command::ShowScreen.shortcut() {
            item = item.shortcut(s.label());
        }
        items.push(item);
    }
    if !many && m.games.get(key).is_some_and(|g| g.connection) {
        items.push(Item::new(format!("menu.{key}.play"), tr("Spielen"), |_| {}).disabled(true).detail(tr("Läuft ohne Spiel – kein Bild")));
    }
    let offered = ui.selection.peek().iter().any(|k| m.games.get(k).is_some_and(|g| g.phase.needs_pack_answer()));
    let ruled = ui.selection.peek().iter().any(|k| m.games.get(k).is_some_and(|g| g.phase.needs_rules_answer()));
    for c in [Command::StartSelected, Command::StopSelected, Command::RestartSelected, Command::ReconnectSelected, Command::ScreenshotSelected, Command::AcceptPacks, Command::DeclinePacks, Command::AcceptRules, Command::DeclineRules] {
        let doubled = match c {
            Command::StartSelected => matches!(primary, Some(Primary::Start | Primary::TakeOver)),
            Command::ReconnectSelected => matches!(primary, Some(Primary::Reconnect)),
            Command::ScreenshotSelected => matches!(primary, Some(Primary::Screenshot)),
            Command::AcceptPacks => matches!(primary, Some(Primary::AcceptPack)) || !offered,
            Command::DeclinePacks => !offered,
            Command::AcceptRules => matches!(primary, Some(Primary::AcceptRules)) || !ruled,
            Command::DeclineRules => !ruled,
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
        Command::ScreenshotSelected => tr("Screenshot"),
        Command::AcceptPacks => tr("Paket annehmen"),
        Command::DeclinePacks => tr("Paket ablehnen"),
        Command::AcceptRules => tr("Regeln annehmen"),
        Command::DeclineRules => tr("Regeln ablehnen"),
        other => other.label(),
    }
}

/// The space between tiles: every tile keeps a gutter for its selection ring, so the rings of neighbours never touch.
pub(super) fn gap(t: &Theme) -> f32 {
    t.grid_gap.max(HALO * 2. + space::XS)
}

const HALO: f32 = frame::RING + frame::RING_GAP;
const HEAD: f32 = 16.;
const STATUS_DOT: f32 = 6.;
const NAME_ROW: f32 = 16.;
const STATUS_ROW: f32 = 14.;
const HOVER_LIFT: f32 = 0.04;
const PRESS_SHADE: f32 = 0.08;
const DOWN_DIM: f32 = 0.3;
const LEAD: f32 = space::XS + space::XXS;
const TRACK: f32 = 0.16;

fn mix(a: Color, b: Color, share: f32) -> Color {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * share).round() as u8;
    Color::from_argb(255, m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn cover(w: f32, h: f32, fill: Color) -> Rect {
    rect().position(Position::new_absolute().left(0.).top(0.)).width(Size::px(w)).height(Size::px(h)).background(fill)
}

fn dot(color: Color) -> Rect {
    rect().width(Size::px(STATUS_DOT)).height(Size::px(STATUS_DOT)).corner_radius(STATUS_DOT / 2.).background(color)
}

fn progress(g: &Game, m: &Model) -> Option<Option<f32>> {
    if !matches!(g.phase, Phase::Starting { .. }) || g.stopping {
        return None;
    }
    Some(m.preparing.get(&g.batch_id).filter(|_| g.memory_wait.is_none()).map(|p| f32::from(*p) / 100.))
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
        let mut pressed = use_state(|| false);
        let mut track = use_state(Track::default);
        super::use_node(&self.key, a11y);
        let preview = use_preview(&self.key);
        let grayed = use_hook(|| Rc::new(RefCell::new(None::<(u32, ImageHandle)>)));
        let m = store.model.read();
        let Some(game) = m.games.get(&self.key) else {
            return rect().into_element();
        };
        let preview = preview.filter(|_| !game.connection);
        let selected = store.ui.selection.read().contains(&self.key);
        let face = store.faces.read().get(&game.account_id).cloned();
        let attention = game.needs_attention();
        let severe = matches!(game.phase, Phase::LoginNeeded | Phase::Banned { .. });
        let detail = stopped_ban(game, &m).unwrap_or_else(|| status(game, m.preparing.get(&game.batch_id).copied()));
        let reason = words::reason(game, &m);
        let line2 = words::line(game, &m);
        let said = words::caption(game, &m);
        let action = action_of(game, &m);
        let spot = use_spot(&format!("tile.{}", self.key), "tile", &format!("{} · {line2}", self.name), true);
        let two = self.width >= TWO_LINES;
        let hover = *hovered.read();
        let keyboard = focus() == Focus::Keyboard;
        let (key, index) = (self.key.clone(), self.index);
        let (enter_key, menu_key) = (key.clone(), key.clone());
        let stopped = game.phase == Phase::Stopped && !game.stopping;
        let tinted = attention || severe;
        let tone = if severe { t.danger_text } else if attention { t.attention_text } else if stopped { t.text3 } else { t.text2 };
        let signal = if severe {
            Some(t.danger)
        } else if attention {
            Some(t.attention)
        } else if matches!(game.phase, Phase::Playing { .. }) && !game.stopping && !game.hint.link_lost {
            Some(t.success)
        } else if stopped {
            None
        } else {
            Some(t.text3)
        };
        let r = t.shape.tile;
        let (w, h) = (self.width, layout::picture_h(self.width));

        let well = mix(t.surface, t.text1, if t.dark { 0.05 } else { 0.1 });
        let mut pic = rect().width(Size::px(w)).height(Size::px(h)).background(well).corner_radius(r).overflow(Overflow::Clip);
        if let Some(handle) = &preview {
            let shown = if stopped { gray_of(&grayed, handle) } else { handle.clone() };
            pic = pic.child(frame_image(shown, w, h, if attention { DOWN_DIM } else { 1. }));
            if stopped {
                pic = pic.child(cover(w, h, t.surface.with_af32(t.hidden_dim)));
            }
        }
        if game.connection {
            pic = pic.child(bare(&t, game, &m, m.account(game.account_id), face.clone(), w, h));
            let (mark, tip, tone) = match (game.shows.menu, game.shows.map) {
                (true, _) => (tr("Menü offen"), tr("Der Server hat ein Menü geöffnet. Klick zeigt es."), Tone::Attention),
                (false, true) => (tr("Karte"), tr("Eine Karte in der Hand. Klick zeigt sie."), Tone::Attention),
                _ => (tr("ohne Bild"), tr("Läuft ohne Spiel – kein Bild"), Tone::Quiet),
            };
            let tone = if tone == Tone::Attention && !game.shows.unseen { Tone::Accent } else { tone };
            pic = pic.child(rect().position(Position::new_absolute().left(space::S).top(space::S)).child(WithTip::new(Some(tip), badge(&t, mark, tone))));
        } else if let Some(mark) = placeholder(&t, &game.phase, if preview.is_none() { face.clone() } else { None }) {
            pic = pic.child(rect().position(Position::new_absolute().left(0.).top(0.)).width(Size::px(w)).height(Size::px(h)).center().child(mark));
        }
        if keyboard && selected {
            pic = pic.child(cover(w, h, Color::TRANSPARENT).corner_radius(r).border(Border::new().fill(t.accent).width(frame::RING).alignment(BorderAlignment::Inner)));
        }
        if *pressed.read() {
            pic = pic.child(cover(w, h, t.shade.with_af32(PRESS_SHADE)));
        } else if hover {
            pic = pic.child(cover(w, h, t.pale.with_af32(HOVER_LIFT)));
        }
        if let Some(done) = progress(game, &m) {
            let mut bar = rect().position(Position::new_absolute().left(0.).bottom(0.)).width(Size::px(w)).height(Size::px(frame::PROGRESS_EDGE)).background(t.text1.with_af32(TRACK));
            if let Some(done) = done {
                bar = bar.child(rect().width(Size::px((w * done.clamp(0., 1.)).round())).height(Size::fill()).background(t.accent));
            }
            pic = pic.child(bar);
        }
        if preview.is_none() {
            pic = pic.child(cover(w, h, Color::TRANSPARENT).corner_radius(r).border(Border::new().fill(t.elevation.rim.unwrap_or_else(|| t.separator.with_af32(0.6))).width(1.).alignment(BorderAlignment::Inner)));
        }
        if let Some(p) = action.filter(|_| hover || keyboard) {
            let k = key.clone();
            let button = pill(&t, p, !two).on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation()).on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                run(store, &k, p);
            });
            let tip = (!two).then(|| p.label());
            pic = pic.child(rect().position(Position::new_absolute().right(space::S).bottom(space::S)).child(WithTip::new(tip, button)));
        }

        let head = face_chip(&t, m.account(game.account_id), face, HEAD);
        let caption = if two {
            let name = line(&t, self.name.clone(), font::BODY, t.text1).font_weight(FontWeight::SEMI_BOLD).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.));
            let mut lead = rect().width(Size::px(HEAD)).height(Size::px(STATUS_ROW)).center();
            if let Some(c) = signal.map(|c| if tinted { t.text3 } else { c }) {
                lead = lead.child(dot(c));
            }
            let tip = (said != line2).then(|| line2.clone());
            let words = line(&t, said, font::CAPTION, tone).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.));
            rect()
                .width(Size::fill())
                .padding((space::S, 0., 0., 0.))
                .spacing(space::XXS)
                .child(rect().width(Size::fill()).height(Size::px(NAME_ROW)).horizontal().cross_align(Alignment::Center).spacing(LEAD).child(head).child(name).content(Content::flex()))
                .child(WithTip::new(tip, rect().width(Size::fill()).height(Size::px(STATUS_ROW)).horizontal().cross_align(Alignment::Center).spacing(LEAD).child(lead).child(words).content(Content::flex())))
                .into_element()
        } else {
            let name = line(&t, self.name.clone(), font::CALLOUT, t.text1).font_weight(FontWeight::SEMI_BOLD).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.));
            let mut row = rect().width(Size::fill()).height(Size::px(NAME_ROW)).horizontal().cross_align(Alignment::Center).spacing(LEAD).child(head).child(name).content(Content::flex());
            if let Some(word) = words::short(game, &m) {
                let ink = if severe || stopped_ban(game, &m).is_some() { t.danger_text } else { t.attention_text };
                row = row.child(line(&t, word, font::CAPTION, ink).font_weight(FontWeight::SEMI_BOLD).max_lines(1));
            } else if let Some(g) = mark(&game.phase) {
                row = row.child(icon(&t, g, iconsize::XS, tone));
            } else if let Some(c) = signal.filter(|_| !stopped) {
                row = row.child(dot(c));
            }
            let tip = match &reason {
                Some(r) if detail.contains(r.as_str()) => detail.clone(),
                Some(r) if *r != line2 => format!("{detail}\n{r}"),
                _ => line2.clone(),
            };
            rect().width(Size::fill()).padding((LEAD, 0., 0., 0.)).child(WithTip::new(Some(tip), row)).into_element()
        };

        let (over_key, leave_key) = (self.key.clone(), self.key.clone());
        let mut tile = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(format!("{}, {line2}", self.name))
            .width(Size::px(self.width))
            .height(Size::px(self.height))
            .on_sized(spot)
            .on_pointer_over(move |_| {
                hovered.set(true);
                store.watch(|w| w.hovered = Some(over_key.clone()));
            })
            .on_pointer_out(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
                unhover(store, &leave_key);
            })
            .cursor(CursorIcon::Pointer)
            .on_mouse_down(move |e: Event<MouseEventData>| {
                e.stop_propagation();
                if e.button == Some(MouseButton::Left) {
                    pressed.set(true);
                }
            })
            .on_mouse_up(move |_| pressed.set_if_modified(false))
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
            .child(caption);
        let ring = if keyboard { Some(t.focus) } else if selected { Some(t.accent) } else { None };
        if let Some(c) = ring {
            tile = tile.child(
                rect()
                    .position(Position::new_absolute().left(-HALO).top(-HALO))
                    .width(Size::px(w + HALO * 2.))
                    .height(Size::px(h + HALO * 2.))
                    .corner_radius(r + HALO)
                    .border(Border::new().fill(c).width(frame::RING).alignment(BorderAlignment::Inner)),
            );
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
        let mass = store.ui.grid_view.mass.read().clone().filter(|m| m.keys.contains(&self.key));
        let mut list = rect().width(Size::fill()).spacing(space::XS);
        if let Some(mass) = &mass {
            let others = mass.keys.len().saturating_sub(1);
            let with = if others == 0 { tr("von dir") } else { trf("mit %lld anderen", &[&others]) };
            list = list.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .child(line(&t, words::when(mass.at), font::CALLOUT, t.text3).width(Size::px(HISTORY_WHEN)).max_lines(1))
                    .child(line(&t, tr("Gestoppt"), font::CALLOUT, t.text1).font_weight(FontWeight::MEDIUM).max_lines(1))
                    .child(line(&t, with, font::CALLOUT, t.text2).width(Size::flex(1.)).max_lines(1).text_overflow(TextOverflow::Ellipsis))
                    .child(Button::new(format!("history.{}.undo", self.key), tr("Rückgängig")).plain().tip(trf("Rückgängig: %@", &[&mass.what])).on_press(move |_| super::mass::undo(store)))
                    .content(Content::flex()),
            );
        }
        if moments.is_empty() && mass.is_none() {
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

pub(super) const HISTORY_W: f32 = 360.;
const HISTORY_WHEN: f32 = 72.;

fn primary_of(store: Store, key: &str) -> Option<Primary> {
    let m = store.model.peek();
    let g = m.games.get(key)?;
    action_of(g, &m)
}

fn frame_image(handle: ImageHandle, w: f32, h: f32, opacity: f32) -> impl IntoElement {
    image(handle).width(Size::px(w)).height(Size::px(h)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center).opacity(opacity)
}

fn mark(phase: &Phase) -> Option<Icon> {
    match phase {
        Phase::LoginNeeded => Some(Icon::LogIn),
        Phase::Banned { .. } => Some(Icon::Ban),
        Phase::Crashed { .. } => Some(Icon::Crash),
        Phase::Disconnected { .. } | Phase::NetworkLost { .. } | Phase::NoNetwork => Some(Icon::Wifi),
        Phase::Dead { .. } | Phase::Failed { .. } => Some(Icon::Alert),
        Phase::ServerAway { .. } => Some(Icon::Clock),
        _ => None,
    }
}

fn gray_of(cache: &RefCell<Option<(u32, ImageHandle)>>, handle: &ImageHandle) -> ImageHandle {
    let id = handle.image.unique_id();
    if let Some((have, gray)) = cache.borrow().as_ref()
        && *have == id
    {
        return gray.clone();
    }
    let gray = gray(handle).unwrap_or_else(|| handle.clone());
    *cache.borrow_mut() = Some((id, gray.clone()));
    gray
}

fn gray(handle: &ImageHandle) -> Option<ImageHandle> {
    let (w, h) = (u32::try_from(handle.image.width()).ok()?, u32::try_from(handle.image.height()).ok()?);
    if handle.bytes.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let mut px = handle.bytes.to_vec();
    for p in px.chunks_exact_mut(4) {
        let y = ((u32::from(p[0]) * 54 + u32::from(p[1]) * 183 + u32::from(p[2]) * 19) >> 8) as u8;
        p[..3].fill(y);
    }
    ImageHandle::from_rgba(w, h, Bytes::from(px), AlphaType::Unpremul)
}

fn placeholder(t: &Theme, phase: &Phase, face: Option<ImageHandle>) -> Option<Element> {
    let quiet = matches!(phase, Phase::NetworkLost { .. } | Phase::NoNetwork | Phase::ServerAway { .. });
    match (mark(phase), face) {
        (Some(g), _) => Some(icon(t, g, iconsize::L, if quiet { t.text3 } else { t.text2 })),
        (None, Some(face)) if *phase != Phase::Stopped => Some(image(face).width(Size::px(space::XXL)).height(Size::px(space::XXL)).opacity(0.5).into_element()),
        _ => None,
    }
}

const BARE_HEAD: f32 = 40.;
const BARE_ROOM: f32 = 84.;
const BARE_CHAT_ROOM: f32 = 112.;

fn bare(t: &Theme, game: &Game, m: &Model, account: Option<&Account>, face: Option<ImageHandle>, w: f32, h: f32) -> Element {
    let stopped = game.phase == Phase::Stopped && !game.stopping;
    let roomy = h >= BARE_ROOM;
    let mut column = rect().width(Size::px(w)).height(Size::px(h)).center().spacing(space::XS).padding((0., space::M, control::S, space::M));
    let head = face_chip(t, account, face, if roomy { BARE_HEAD } else { BARE_HEAD * 0.7 });
    column = column.child(rect().opacity(if stopped { 0.5 } else { 1. }).child(head));
    if roomy && !stopped {
        let place = match (&game.phase, words::since(&game.key, m)) {
            (Phase::Playing { server, .. }, Some(since)) if !game.stopping => format!("{} · {since}", server.strip_suffix(":25565").unwrap_or(server)),
            _ => words::caption(game, m),
        };
        column = column.child(line(t, place, font::CAPTION, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::fill()).text_align(TextAlign::Center));
        let rules = match &game.phase {
            Phase::Offer { offer, .. } if offer.is_conduct() => offer.text.clone(),
            _ => None,
        };
        if let Some(text) = rules.filter(|_| h >= BARE_CHAT_ROOM) {
            column = column.child(line(t, text, font::CAPTION2, t.text2).max_lines(3).text_overflow(TextOverflow::Ellipsis).width(Size::fill()).text_align(TextAlign::Center));
        } else if let Some(said) = game.last_chat.as_ref().filter(|_| h >= BARE_CHAT_ROOM) {
            column = column.child(line(t, said.clone(), font::CAPTION2, t.text3).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::fill()).text_align(TextAlign::Center));
        }
    }
    column.into_element()
}

fn face_chip(t: &Theme, account: Option<&Account>, face: Option<ImageHandle>, size: f32) -> Rect {
    let fill = account.map_or(t.control, |a| t.identity(usize::from(a.color())));
    let mut chip = rect().width(Size::px(size)).height(Size::px(size)).corner_radius(t.shape.small).background(fill).center().overflow(Overflow::Clip);
    chip = match (face, account) {
        (Some(face), _) => chip.padding(space::XXS).child(image(face).width(Size::fill()).height(Size::fill()).corner_radius(t.shape.small / 2.)),
        (None, Some(a)) => chip.child(label().text(a.name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()).font_size(if size > HEAD { font::CAPTION } else { font::CAPTION2 }).font_weight(FontWeight::BOLD).color(t.ink_on(fill))),
        (None, None) => chip,
    };
    chip
}

pub(super) fn face_view(t: &Theme, account: Option<&Account>, face: Option<ImageHandle>, playing: bool) -> Element {
    let mut r = rect().width(Size::px(frame::FACE)).height(Size::px(frame::FACE)).child(face_chip(t, account, face, frame::FACE));
    if playing {
        let out = -frame::DOT / 2.;
        r = r.child(rect().position(Position::new_absolute().right(out).bottom(out)).width(Size::px(frame::DOT)).height(Size::px(frame::DOT)).corner_radius(frame::DOT).background(t.success).border(Border::new().fill(t.surface).width(1.5).alignment(BorderAlignment::Outer)));
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
