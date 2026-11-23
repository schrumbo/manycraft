//! Games without a window: screenshots on request, on join and on their batch's interval, and the user's answers to the
//! resource packs they hold.

use std::time::Duration;

use super::super::effect::Effect;
use super::super::event::Event;
use super::super::model::{Model, Phase};
use super::super::phase::{self, Why};
use super::super::session::Timer;
use super::go;
use crate::core::ipc::{Offer, ToMod};

/// How wide a screenshot of a game without a window is: sharp on a large tile, small enough for fifty of them.
pub const SHOT_WIDTH: u32 = 640;

/// Takes a screenshot of each of `keys` that runs without a window and has a mod to take it.
pub(super) fn capture(m: &Model, keys: &[String], out: &mut Vec<Effect>) {
    for key in keys.iter().filter(|k| capturable(m, k)) {
        out.push(shot(key));
    }
}

fn shot(key: &str) -> Effect {
    Effect::Send { key: key.to_owned(), message: ToMod::Capture { width: SHOT_WIDTH } }
}

fn capturable(m: &Model, key: &str) -> bool {
    m.session.pids.contains_key(key) && m.games.get(key).is_some_and(|i| i.headless && !i.stopping && !i.hint.link_lost && phase::mod_reports(&i.phase) && !matches!(i.phase, Phase::Starting { .. }))
}

/// The game `key` got onto its server: a game without a window shows where it is with one screenshot.
pub(super) fn joined(m: &Model, key: &str, out: &mut Vec<Effect>) {
    if capturable(m, key) {
        out.push(shot(key));
    }
}

/// Matches the screenshot timer of every connected game to its batch's interval: arms new ones, stops those turned off.
pub(super) fn arm(m: &mut Model, out: &mut Vec<Effect>) {
    let mut keys: Vec<String> = m.session.pids.keys().cloned().collect();
    keys.sort();
    for key in keys {
        arm_one(m, &key, out);
    }
}

/// Matches the screenshot timer of `key` to its batch's interval, staggered by the account's place in the batch.
pub(super) fn arm_one(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    let wanted = m.games.get(key).filter(|i| i.headless && m.session.pids.contains_key(key)).and_then(|i| {
        let batch = m.batches.iter().find(|b| b.id == i.batch_id)?;
        let minutes = batch.unattended().screenshots?;
        let place = batch.accounts.iter().position(|a| *a == i.account_id).unwrap_or(0) as u32;
        Some((minutes, place, batch.accounts.len().max(1) as u32))
    });
    let s = &mut m.session;
    match wanted {
        None => {
            s.captures.remove(key);
        }
        Some((minutes, _, _)) if s.captures.get(key).is_some_and(|t| t.minutes == minutes) => {}
        Some((minutes, place, of)) => {
            s.capture_seq += 1;
            let generation = s.capture_seq;
            s.captures.insert(key.to_owned(), Timer { generation, minutes });
            let first = period(minutes) * (place % of + 1) / of;
            out.push(Effect::After { delay: first, event: Box::new(Event::CaptureDue { key: key.to_owned(), generation }) });
        }
    }
}

fn period(minutes: u32) -> Duration {
    Duration::from_secs(u64::from(minutes) * 60)
}

/// The screenshot timer of `key` ran out: takes one when it is still the latest timer, and waits for the next.
pub(super) fn capture_due(m: &Model, key: &str, generation: u64, out: &mut Vec<Effect>) {
    let Some(timer) = m.session.captures.get(key).filter(|t| t.generation == generation) else { return };
    if capturable(m, key) {
        out.push(shot(key));
    }
    out.push(Effect::After { delay: period(timer.minutes), event: Box::new(Event::CaptureDue { key: key.to_owned(), generation }) });
}

/// The game `key` ended: its screenshot timer stops.
pub(super) fn ended(m: &mut Model, key: &str) {
    m.session.captures.remove(key);
}

/// The game `key` holds a resource pack `offer` from `server` until the user answers.
pub(super) fn offered(m: &mut Model, key: &str, server: Option<String>, offer: Option<Offer>, out: &mut Vec<Effect>) {
    let why = if m.session.adopted.remove(key) { Why::Adopt } else { Why::Mod };
    let Some(i) = m.games.get(key) else { return };
    if i.stopping || !phase::mod_reports(&i.phase) {
        return;
    }
    let Some(offer) = offer else {
        out.push(Effect::Log(format!("phase: {key} reported an offer without one")));
        return;
    };
    go(m, key, Phase::Offer { server: server.unwrap_or_default(), offer }, why, out);
}

/// Answers the resource pack each of `keys` holds; the game reports where the answer took it.
pub(super) fn answer(m: &Model, keys: &[String], accept: bool, out: &mut Vec<Effect>) {
    for key in keys {
        if let Some(i) = m.games.get(key).filter(|i| !i.stopping)
            && let Phase::Offer { offer, .. } = &i.phase
        {
            out.push(Effect::Send { key: key.clone(), message: ToMod::Answer { id: offer.id.clone(), accept } });
        }
    }
}
#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::super::effect::Effect;
    use super::super::super::event::{Event, Report};
    use super::super::super::model::{Model, Need, Phase, Primary};
    use super::super::testing::*;
    use super::SHOT_WIDTH;
    use crate::core::account::AccountKind;
    use crate::core::ipc::{DisconnectCause, FromMod, HotkeyAction, Offer, StateKind, ToMod};
    use crate::core::settings::Settings;

    fn headless(minutes: Option<u32>) -> (Model, String, Settings) {
        let s = settings();
        let (mut m, key) = setup(Phase::Starting { place: 0, total: 1 }, AccountKind::Offline, &[]);
        m.batches[0].headless = true;
        m.batches[0].screenshots = minutes;
        m.games.get_mut(&key).unwrap().headless = true;
        (m, key, s)
    }

    fn connected(m: &mut Model, s: &Settings, key: &str) -> Vec<Effect> {
        report(m, s, Report::Connected { key: key.to_owned(), pid: 9 })
    }

    fn shots(fx: &[Effect]) -> usize {
        fx.iter().filter(|e| matches!(e, Effect::Send { message: ToMod::Capture { width: SHOT_WIDTH }, .. })).count()
    }

    fn due(fx: &[Effect]) -> Option<(Duration, u64)> {
        later(fx).into_iter().find_map(|(d, e)| if let Event::CaptureDue { generation, .. } = e { Some((d, *generation)) } else { None })
    }

    fn offer(key: &str, id: &str, required: bool) -> Report {
        Report::Mod { key: key.into(), message: FromMod::State { kind: StateKind::Offer, server: Some("mc.example:25565".into()), reason: None, cause: None, offer: Some(Offer { id: id.into(), required, text: None }) } }
    }

    #[test]
    fn a_headless_game_is_never_placed_tinted_shown_or_stepped_to() {
        let (mut m, key, s) = headless(None);
        let fx = run(&mut m, &s, Instant::now(), Event::Reports(vec![Report::Connected { key: key.clone(), pid: 9 }]));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Tint { .. } | Effect::Place(_))), "{fx:?}");
        assert!(!m.session.unplaced.contains(&key));
        assert_eq!(m.games[&key].phase, Phase::Menu);
        assert!(!m.games[&key].showable());
        for event in [Event::Play(key.clone()), Event::Step(HotkeyAction::Next), Event::Step(HotkeyAction::NextAttention)] {
            let fx = run(&mut m, &s, Instant::now(), event);
            assert!(!fx.iter().any(|e| matches!(e, Effect::Show { .. } | Effect::HideManager)), "{fx:?}");
        }
        assert_eq!(m.playing, None);
        assert_eq!(m.games[&key].action(false), Some(Primary::Screenshot), "where a windowed game is played, a headless one takes a screenshot");
    }

    #[test]
    fn screenshots_come_on_request_and_on_join_only_from_headless_games_with_a_mod() {
        let (mut m, key, s) = headless(None);
        assert_eq!(shots(&run(&mut m, &s, Instant::now(), Event::Capture(vec![key.clone()]))), 0, "no process, no shot");
        let fx = connected(&mut m, &s, &key);
        assert_eq!(due(&fx), None, "the interval is off by default");
        assert_eq!(shots(&run(&mut m, &s, Instant::now(), Event::Capture(vec![key.clone()]))), 1);
        assert_eq!(shots(&report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None))), 1, "one shot shows where it joined");
        assert_eq!(shots(&report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None))), 0, "staying on is no new join");
        m.games.get_mut(&key).unwrap().headless = false;
        assert_eq!(shots(&run(&mut m, &s, Instant::now(), Event::Capture(vec![key.clone()]))), 0, "a game with a window has its preview");
    }

    #[test]
    fn the_interval_timer_follows_the_batch_and_stops_with_the_game() {
        let (mut m, key, s) = headless(Some(5));
        let (delay, generation) = due(&connected(&mut m, &s, &key)).expect("armed");
        assert_eq!(delay, Duration::from_secs(300));
        let fx = run(&mut m, &s, Instant::now(), Event::CaptureDue { key: key.clone(), generation });
        assert_eq!(shots(&fx), 1);
        assert_eq!(due(&fx), Some((Duration::from_secs(300), generation)), "and waits for the next");
        assert!(due(&run(&mut m, &s, Instant::now(), Event::TellGames)).is_none(), "the same interval keeps its timer");
        m.batches[0].screenshots = Some(15);
        let (_, newer) = due(&run(&mut m, &s, Instant::now(), Event::TellGames)).expect("rearmed");
        assert!(run(&mut m, &s, Instant::now(), Event::CaptureDue { key: key.clone(), generation }).is_empty(), "the older timer does nothing");
        m.batches[0].screenshots = None;
        run(&mut m, &s, Instant::now(), Event::TellGames);
        assert!(run(&mut m, &s, Instant::now(), Event::CaptureDue { key: key.clone(), generation: newer }).is_empty(), "turned off, it stops");
        m.batches[0].screenshots = Some(1);
        let (_, last) = due(&run(&mut m, &s, Instant::now(), Event::TellGames)).expect("armed again");
        report(&mut m, &s, Report::Exited { key: key.clone(), code: Some(0) });
        assert!(run(&mut m, &s, Instant::now(), Event::CaptureDue { key, generation: last }).is_empty(), "an ended game takes none");
    }

    #[test]
    fn a_held_resource_pack_asks_the_user_and_the_answer_names_the_offer() {
        let (mut m, key, s) = headless(None);
        connected(&mut m, &s, &key);
        report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        report(&mut m, &s, offer(&key, "pack-1", true));
        assert!(matches!(&m.games[&key].phase, Phase::Offer { offer, .. } if offer.id == "pack-1" && offer.required));
        assert_eq!(m.needs(None), [(Need::Offer, 1)]);
        assert_eq!(m.games[&key].action(false), Some(Primary::AcceptPack));
        let fx = run(&mut m, &s, Instant::now(), Event::AnswerPack { keys: vec![key.clone()], accept: false });
        assert!(fx.iter().any(|e| matches!(e, Effect::Send { message: ToMod::Answer { id, accept: false }, .. } if id == "pack-1")), "{fx:?}");
        let fx = report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        assert!(matches!(m.games[&key].phase, Phase::Playing { .. }));
        assert_eq!(shots(&fx), 0, "back from the offer is no new join");
        assert!(run(&mut m, &s, Instant::now(), Event::AnswerPack { keys: vec![key], accept: true }).is_empty(), "nothing held, nothing to answer");
        let fx = report(&mut m, &s, Report::Mod { key: "nobody".into(), message: FromMod::State { kind: StateKind::Offer, server: None, reason: None, cause: None, offer: None } });
        assert!(!fx.iter().any(|e| matches!(e, Effect::Send { .. })));
    }

    #[test]
    fn join_commands_wait_out_a_pack_offer_and_go_once_it_is_answered() {
        let (mut m, key, s) = headless(None);
        m.batches[0].join_commands = vec!["/login pw".into()];
        connected(&mut m, &s, &key);
        let fx = report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        let join = later(&fx).into_iter().find_map(|(_, e)| if let Event::JoinDue { key, generation, lines, delay } = e { Some(Event::JoinDue { key: key.clone(), generation: *generation, lines: lines.clone(), delay: *delay }) } else { None }).expect("join commands scheduled");
        report(&mut m, &s, offer(&key, "pack-1", false));
        let fx = run(&mut m, &s, Instant::now(), join);
        assert!(!fx.iter().any(|e| matches!(e, Effect::Send { message: ToMod::ChatSend { .. }, .. })), "nothing typed while the offer waits");
        assert!(later(&fx).is_empty(), "no polling while the offer waits");
        let fx = report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        let again = later(&fx).into_iter().find_map(|(_, e)| if let Event::JoinDue { key, generation, lines, delay } = e { Some(Event::JoinDue { key: key.clone(), generation: *generation, lines: lines.clone(), delay: *delay }) } else { None }).expect("released once answered");
        let fx = run(&mut m, &s, Instant::now(), again);
        assert!(fx.iter().any(|e| matches!(e, Effect::Send { message: ToMod::ChatSend { text }, .. } if text == "/login pw")), "{fx:?}");
    }

    fn join_due(fx: &[Effect]) -> Vec<Event> {
        later(fx).into_iter().filter_map(|(_, e)| if let Event::JoinDue { key, generation, lines, delay } = e { Some(Event::JoinDue { key: key.clone(), generation: *generation, lines: lines.clone(), delay: *delay }) } else { None }).collect()
    }

    #[test]
    fn join_commands_held_by_an_offer_end_with_the_connection_and_run_once_after_a_reconnect() {
        let (mut m, key, s) = headless(None);
        m.batches[0].join_commands = vec!["/login pw".into()];
        connected(&mut m, &s, &key);
        let fx = report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        let [join] = join_due(&fx).try_into().expect("one join scheduled");
        report(&mut m, &s, offer(&key, "pack-1", false));
        run(&mut m, &s, Instant::now(), join);
        report(&mut m, &s, state(&key, StateKind::Disconnected, Some("mc.example:25565"), None));
        assert!(m.session.held_joins.is_empty(), "the held lines belonged to the old connection");
        let fx = report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        assert_eq!(join_due(&fx).len(), 1, "one run of the join commands per join");
    }

    #[test]
    fn join_commands_held_by_an_offer_survive_a_queue_on_the_way_back() {
        let (mut m, key, s) = headless(None);
        m.batches[0].join_commands = vec!["/login pw".into()];
        connected(&mut m, &s, &key);
        let fx = report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        let [join] = join_due(&fx).try_into().expect("one join scheduled");
        report(&mut m, &s, offer(&key, "pack-1", false));
        run(&mut m, &s, Instant::now(), join);
        let fx = report(&mut m, &s, state(&key, StateKind::Queue, Some("mc.example:25565"), None));
        assert!(join_due(&fx).is_empty(), "nothing typed in the queue");
        let fx = report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        let [again] = join_due(&fx).try_into().expect("released once back in the game");
        let fx = run(&mut m, &s, Instant::now(), again);
        assert!(fx.iter().any(|e| matches!(e, Effect::Send { message: ToMod::ChatSend { text }, .. } if text == "/login pw")), "{fx:?}");
    }

    #[test]
    fn a_declined_required_pack_drops_with_its_reason_and_no_reconnect() {
        let (mut m, key, s) = headless(None);
        connected(&mut m, &s, &key);
        report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        let fx = report(&mut m, &s, state(&key, StateKind::Disconnected, Some("mc.example:25565"), Some(DisconnectCause::PackDeclined)));
        assert!(matches!(&m.games[&key].phase, Phase::Disconnected { reason, .. } if reason.contains("Ressourcenpaket")), "{:?}", m.games[&key].phase);
        assert!(!later(&fx).iter().any(|(_, e)| matches!(e, Event::ReconnectDue { .. })), "declining again would only loop");
    }

    #[test]
    fn chat_reaches_a_headless_game_like_any_other() {
        let (mut m, key, s) = headless(None);
        connected(&mut m, &s, &key);
        report(&mut m, &s, state(&key, StateKind::Playing, Some("mc.example:25565"), None));
        let fx = run(&mut m, &s, Instant::now(), Event::SendChat { key: key.clone(), text: "/home".into() });
        assert!(fx.iter().any(|e| matches!(e, Effect::Send { message: ToMod::ChatSend { text }, .. } if text == "/home")));
        let batch = m.batches[0].id.clone();
        let fx = run(&mut m, &s, Instant::now(), Event::Broadcast { batch, text: "hi".into() });
        assert!(fx.iter().any(|e| matches!(e, Effect::Send { key: k, message: ToMod::ChatSend { .. } } if *k == key)));
    }
}