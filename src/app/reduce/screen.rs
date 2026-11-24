//! What servers show connection-only accounts besides the world: the map in hand, an open menu, titles. Kept only while
//! shown; a new map or menu asks for the user until they look at its screen.

use super::super::effect::{Effect, Redraw};
use super::super::model::{Model, Shows};
use crate::core::ipc::{Click, FromMod, ToMod};

/// Takes in what the server of `key` shows it now.
pub(super) fn shown(m: &mut Model, key: &str, message: FromMod, out: &mut Vec<Effect>) {
    let Some(before) = m.games.get(key).filter(|g| g.connection).map(|g| g.shows) else { return };
    m.session.map_seq += 1;
    let seq = m.session.map_seq;
    let screen = m.screens.entry(key.to_owned()).or_default();
    let fresh = match message {
        FromMod::Map { map } => {
            let fresh = map.as_ref().is_some_and(|new| Some(new.id) != before.seen_map && screen.map.as_ref().is_none_or(|old| old.id != new.id));
            screen.map = map;
            screen.map_seq = seq;
            fresh
        }
        FromMod::Menu { menu } => {
            let fresh = menu.as_ref().is_some_and(|new| screen.menu.as_ref().is_none_or(|old| old.id != new.id));
            screen.menu = menu;
            fresh
        }
        FromMod::Titles { titles } => {
            screen.titles = titles;
            false
        }
        _ => return,
    };
    let (map, menu) = (screen.map.is_some(), screen.menu.is_some());
    let shown_map = screen.map.as_ref().map(|p| p.id);
    if screen.is_empty() {
        m.screens.remove(key);
    }
    let looking = m.session.screen_open.as_deref() == Some(key);
    let seen_map = if looking { shown_map.or(before.seen_map) } else { before.seen_map };
    let shows = Shows { map, menu, unseen: (map || menu) && !looking && (before.unseen || fresh), seen_map };
    if let Some(g) = m.games.get_mut(key) {
        g.shows = shows;
    }
    if shows != before {
        out.push(Effect::Redraw(Redraw::All));
    }
    out.push(Effect::Redraw(Redraw::Screen));
}

/// Forgets what the server of `key` showed it, once it is gone or starts again.
pub(super) fn forget(m: &mut Model, key: &str) {
    m.screens.remove(key);
    if let Some(g) = m.games.get_mut(key) {
        g.shows = Shows::default();
    }
}

/// The user looks at the screen of `key`, or at none: what it shows stops asking for them.
pub(super) fn look(m: &mut Model, key: Option<String>, out: &mut Vec<Effect>) {
    let held = key.as_deref().and_then(|k| m.screens.get(k)).and_then(|s| s.map.as_ref()).map(|p| p.id);
    if let Some(g) = key.as_deref().and_then(|k| m.games.get_mut(k)) {
        g.shows.seen_map = held.or(g.shows.seen_map);
        if g.shows.unseen {
            g.shows.unseen = false;
            m.order_seq += 1;
            out.push(Effect::Redraw(Redraw::All));
        }
    }
    m.session.screen_open = key;
}

/// Sends the user's `click` on `slot` of menu `menu` that `key` has open.
pub(super) fn click(m: &Model, key: String, menu: i32, slot: i16, click: Click, out: &mut Vec<Effect>) {
    if open(m, &key, menu) {
        out.push(Effect::Send { key, message: ToMod::Click { menu, slot, click } });
    }
}

/// Closes menu `menu` that `key` has open.
pub(super) fn close(m: &Model, key: String, menu: i32, out: &mut Vec<Effect>) {
    if open(m, &key, menu) {
        out.push(Effect::Send { key, message: ToMod::CloseMenu { menu } });
    }
}

fn open(m: &Model, key: &str, menu: i32) -> bool {
    m.games.get(key).is_some_and(|g| g.connection && g.phase.is_running()) && m.screens.get(key).and_then(|s| s.menu.as_ref()).is_some_and(|open| open.id == menu)
}
#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::super::effect::{Effect, Redraw};
    use super::super::super::event::{Event, Report};
    use super::super::super::model::{Model, Need, Phase, Primary};
    use super::super::testing::*;
    use crate::core::account::AccountKind;
    use crate::core::ipc::{Click, FromMod, MapPicture, Menu, StateKind, ToMod, Words};
    use crate::core::settings::Settings;

    fn playing() -> (Model, String, Settings) {
        let s = settings();
        let (mut m, key) = setup(Phase::Starting { place: 0, total: 1 }, AccountKind::Offline, &[]);
        m.batches[0].mode = crate::core::batch::RunMode::Connection;
        let g = m.games.get_mut(&key).unwrap();
        g.headless = true;
        g.connection = true;
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 0 });
        report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        (m, key, s)
    }

    fn menu(id: i32, first: &str) -> Menu {
        let slot = crate::core::ipc::Slot { item: "stone".into(), name: Words { text: first.into(), runs: Vec::new() }, count: 1, lore: Vec::new(), glint: false, rgb: None, named: false };
        Menu { id, kind: "generic_9x1".into(), title: Words::default(), columns: 9, top: 9, slots: vec![Some(slot)], carried: None, readable: true }
    }

    fn shows(m: &mut Model, s: &Settings, key: &str, message: FromMod) -> Vec<Effect> {
        report(m, s, Report::Mod { key: key.to_owned(), message })
    }

    #[test]
    fn a_new_menu_asks_for_the_user_until_they_look() {
        let (mut m, key, s) = playing();
        let fx = shows(&mut m, &s, &key, FromMod::Menu { menu: Some(menu(1, "a")) });
        assert!(fx.iter().any(|e| matches!(e, Effect::Redraw(Redraw::All))));
        assert_eq!(m.needs(None), [(Need::Screen, 1)]);
        assert_eq!(m.attention_count(None), 1);
        assert_eq!(m.games[&key].action(false), Some(Primary::Screen));
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(Some(key.clone())));
        assert!(m.needs(None).is_empty());
        let fx = shows(&mut m, &s, &key, FromMod::Menu { menu: Some(menu(1, "b")) });
        assert!(m.needs(None).is_empty(), "new content of the menu in view asks for nothing");
        assert!(fx.iter().any(|e| matches!(e, Effect::Redraw(Redraw::Screen))) && !fx.iter().any(|e| matches!(e, Effect::Redraw(Redraw::All))), "{fx:?}");
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(None));
        shows(&mut m, &s, &key, FromMod::Menu { menu: Some(menu(1, "c")) });
        assert!(m.needs(None).is_empty(), "the same menu changing asks for nothing");
        shows(&mut m, &s, &key, FromMod::Menu { menu: Some(menu(2, "d")) });
        assert_eq!(m.needs(None), [(Need::Screen, 1)]);
    }

    #[test]
    fn a_seen_map_falls_back_to_chat_and_an_empty_screen_is_forgotten() {
        let (mut m, key, s) = playing();
        shows(&mut m, &s, &key, FromMod::Map { map: Some(MapPicture { id: 3, colors: vec![0; 16384] }) });
        assert_eq!(m.screens[&key].map_seq, 1);
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(Some(key.clone())));
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(None));
        assert_eq!(m.games[&key].action(false), Some(Primary::Chat));
        assert!(m.games[&key].shows.map);
        shows(&mut m, &s, &key, FromMod::Map { map: None });
        assert!(!m.screens.contains_key(&key) && !m.games[&key].shows.any());
    }

    #[test]
    fn a_seen_map_changing_asks_for_nothing_and_its_number_never_repeats() {
        let (mut m, key, s) = playing();
        let map = |id, c| FromMod::Map { map: Some(MapPicture { id, colors: vec![c; 16384] }) };
        shows(&mut m, &s, &key, map(3, 0));
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(Some(key.clone())));
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(None));
        let first = m.screens[&key].map_seq;
        shows(&mut m, &s, &key, map(3, 1));
        assert!(m.needs(None).is_empty(), "new pixels of a seen map ask for nothing");
        shows(&mut m, &s, &key, FromMod::Map { map: None });
        shows(&mut m, &s, &key, map(4, 2));
        assert!(m.screens[&key].map_seq > first + 1, "a map after an emptied screen gets a new number");
        assert_eq!(m.needs(None), [(Need::Screen, 1)]);
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(Some(key.clone())));
        run(&mut m, &s, Instant::now(), Event::LookAtScreen(None));
        shows(&mut m, &s, &key, FromMod::Map { map: None });
        shows(&mut m, &s, &key, map(4, 2));
        assert!(m.needs(None).is_empty(), "a seen map taken back in hand asks for nothing");
    }

    #[test]
    fn clicks_and_closing_go_only_to_the_open_menu() {
        let (mut m, key, s) = playing();
        let click = |m: &mut Model, menu: i32| run(m, &s, Instant::now(), Event::ClickSlot { key: key.clone(), menu, slot: 0, click: Click::Shift });
        assert!(click(&mut m, 1).is_empty());
        shows(&mut m, &s, &key, FromMod::Menu { menu: Some(menu(1, "a")) });
        assert!(click(&mut m, 2).is_empty());
        let fx = click(&mut m, 1);
        assert!(matches!(fx.as_slice(), [Effect::Send { message: ToMod::Click { menu: 1, slot: 0, click: Click::Shift }, .. }]), "{fx:?}");
        let fx = run(&mut m, &s, Instant::now(), Event::CloseMenu { key: key.clone(), menu: 1 });
        assert!(matches!(fx.as_slice(), [Effect::Send { message: ToMod::CloseMenu { menu: 1 }, .. }]), "{fx:?}");
    }

    #[test]
    fn the_screen_is_freed_on_exit_and_ignored_for_games_with_a_window() {
        let (mut m, key, s) = playing();
        shows(&mut m, &s, &key, FromMod::Menu { menu: Some(menu(1, "a")) });
        report(&mut m, &s, Report::Exited { key: key.clone(), code: Some(0) });
        assert!(m.screens.is_empty() && !m.games[&key].shows.any());
        m.games.get_mut(&key).unwrap().connection = false;
        shows(&mut m, &s, &key, FromMod::Menu { menu: Some(menu(1, "a")) });
        assert!(m.screens.is_empty());
    }
}