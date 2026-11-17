//! The "Zuletzt gespielt" shelf over the grid: one card per instance played alone, running first, then the most recently
//! played. A card opens its instance, offers play or stop while hovered or focused, and its instance menu on right-click.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use freya::prelude::*;
use uuid::Uuid;

use super::words;
use crate::app::event::Event as AppEvent;
use crate::app::model::Model;
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::view::drive::use_spot;
use crate::view::icons::Icon;
use crate::view::instance::InstanceIcon;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Menu};
use crate::view::kit::text::{body, secondary};
use crate::view::kit::{after, ring, use_theme};
use crate::view::shell::instance_items;
use crate::view::store::use_store;
use crate::view::theme::{font, space};
use crate::view::ui::Page;

const BIG_UP_TO: usize = 3;
const BIG: (f32, f32, f32) = (300., 88., 48.);
const SMALL: (f32, f32, f32) = (220., 64., 32.);

/// An instance on the shelf: its game in play, if any, and who played it last and when.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    name: String,
    picture: Option<std::path::PathBuf>,
    running: Option<String>,
    starting: bool,
    account: Option<String>,
    at: Option<u64>,
}

thread_local! {
    static PLAYED: RefCell<HashMap<(String, Uuid), (Instant, Option<u64>)>> = RefCell::new(HashMap::new());
}

/// When `account` last played `instance`, from its game log, read at most once a minute.
fn played(instance: &Instance, account: Uuid) -> Option<u64> {
    let key = (instance.id.clone(), account);
    PLAYED.with(|p| {
        if let Some((read, at)) = p.borrow().get(&key)
            && read.elapsed() < Duration::from_secs(60)
        {
            return *at;
        }
        let at = std::fs::metadata(instance.game_dir(account).join("logs").join("latest.log")).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
        p.borrow_mut().insert(key, (Instant::now(), at));
        at
    })
}

fn entries(m: &Model) -> Vec<Entry> {
    let mut list: Vec<Entry> = m
        .instances
        .iter()
        .filter_map(|i| {
            let last = m.solo_games(&i.id).max_by_key(|g| (g.phase.is_active(), words::last_change(&g.key, m).map_or(0, |e| e.at)));
            let account = last.map(|g| g.account_id).or(i.last_account)?;
            let name = m.account(account)?.name.clone();
            let active = last.is_some_and(|g| g.phase.is_active());
            Some(Entry {
                id: i.id.clone(),
                name: i.name.clone(),
                picture: i.icon_path(),
                running: last.filter(|_| active).map(|g| g.key.clone()),
                starting: last.is_some_and(|g| active && g.phase.waiting()),
                account: Some(name),
                at: last.and_then(|g| words::last_change(&g.key, m)).map(|e| e.at).or_else(|| played(i, account)),
            })
        })
        .collect();
    list.sort_by(|a, b| b.running.is_some().cmp(&a.running.is_some()).then(b.at.cmp(&a.at)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    list
}

/// How many instances the shelf shows: those played alone, now or before.
pub fn count(m: &Model) -> usize {
    m.instances.iter().filter(|i| m.solo_games(&i.id).next().map(|g| g.account_id).or(i.last_account).is_some_and(|a| m.account(a).is_some())).count()
}

/// The height the shelf takes over the grid with `count` cards, its title and the gap under it included.
pub fn height(count: usize) -> f32 {
    if count == 0 {
        return 0.;
    }
    let (_, h, _) = if count <= BIG_UP_TO { BIG } else { SMALL };
    h + space::XL + font::CALLOUT + space::S
}

/// "vor 5 Min." for `at`, seconds since 1970; the day beyond yesterday.
fn ago(at: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(at);
    match secs {
        0..60 => tr("gerade eben"),
        60..3600 => trf("vor %lld Min.", &[&(secs / 60)]),
        3600..86400 => trf("vor %lld Std.", &[&(secs / 3600)]),
        _ => words::when(at),
    }
}

/// The shelf's cards, as many as fit into `width`.
#[derive(PartialEq)]
pub struct Shelf {
    pub width: f32,
    pub gap: f32,
}

impl Component for Shelf {
    fn render(&self) -> impl IntoElement {
        let mut tick = use_state(|| 0u32);
        let _ = tick.read();
        use_hook(move || {
            spawn(async move {
                loop {
                    after(Duration::from_secs(60)).await;
                    *tick.write() += 1;
                }
            });
        });
        let store = use_store();
        let list = entries(&store.model.read());
        let big = list.len() <= BIG_UP_TO;
        let (w, _, _) = if big { BIG } else { SMALL };
        let fit = ((self.width + self.gap) / (w + self.gap)).floor().max(1.) as usize;
        rect().horizontal().spacing(self.gap).children(list.into_iter().take(fit).map(|entry| Card { entry, big }.into_element()))
    }
}

#[derive(PartialEq)]
struct Card {
    entry: Entry,
    big: bool,
}

impl Component for Card {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let e = &self.entry;
        let spot = use_spot(&format!("shelf.{}", e.id), "button", &e.name, true);
        let (w, h, side) = if self.big { BIG } else { SMALL };
        let when = if e.running.is_some() {
            if e.starting { tr("Startet…") } else { tr("Im Spiel") }
        } else {
            e.at.map(ago).unwrap_or_default()
        };
        let status = match (&e.account, when.is_empty()) {
            (Some(who), false) => format!("{when} · {}", trf("als %@", &[who])),
            (Some(who), true) => trf("als %@", &[who]),
            (None, _) => when,
        };
        let open_id = e.id.clone();
        let menu_id = e.id.clone();
        let mut c = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(format!("{}, {status}", e.name))
            .width(Size::px(w))
            .height(Size::px(h))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(if self.big { space::M } else { space::S })
            .background(t.raised)
            .corner_radius(t.shape.tile)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_all_press(move |ev: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*ev
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
                let mut ui = store.ui;
                ui.page.set(Page::Instance(open_id.clone()));
            })
            .on_secondary_down(move |ev: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*ev {
                    menu::open(store.ui, Menu::at(instance_items(store, &menu_id), (m.global_location.x as f32, m.global_location.y as f32)));
                }
            });
        if let Some(shadow) = t.elevation.card.clone() {
            c = c.shadow(shadow);
        }
        if let Some(border) = ring(&t, focus()) {
            c = c.border(border);
        } else if *hovered.read() {
            c = c.border(Border::new().fill(t.tile_ring).width(1.).alignment(BorderAlignment::Inner));
        }
        let text = rect()
            .width(Size::flex(1.))
            .spacing(space::XXS)
            .child(body(&t, e.name.clone()).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis))
            .child(secondary(&t, status).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        c = c.child(InstanceIcon { id: e.id.clone(), name: e.name.clone(), picture: e.picture.clone(), side }).child(text).content(Content::flex());
        if *hovered.read() {
            let id = e.id.clone();
            let action = match e.running.clone() {
                Some(key) => Button::icon(format!("shelf.{id}.stop"), Icon::Stop, tr("Stoppen")).not_focusable().tip(trf("„%@“ stoppen", &[&e.name])).on_press(move |_| store.dispatch(AppEvent::Stop(vec![key.clone()]))),
                None => Button::icon(format!("shelf.{id}.play"), Icon::Play, tr("Spielen")).primary().not_focusable().tip(trf("„%@“ spielen", &[&e.name])).on_press(move |_| store.dispatch(AppEvent::PlayInstance { instance: id.clone(), account: None })),
            };
            c = c.child(action);
        }
        c
    }
}
