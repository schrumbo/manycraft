//! Lines the user sends to accounts: each account gets its own numbered copy and reports what became of it. A line for an
//! account that is not on a server yet but is on its way waits in the outbox and goes out, in order, once it is.

use std::time::Duration;

use super::super::effect::{Effect, Redraw};
use super::super::event::Event;
use super::super::model::{Delivery, Game, Model, Said, SaidRow};
use super::super::session::Line;
use crate::core::ipc::{ChatOutcome, ChatRefusal, ToMod};

/// How often a game that says it is on a server may refuse one line before it is given up.
const RETRIES: u8 = 3;

/// How long a refused line waits before its game gets it again.
const RETRY: Duration = Duration::from_secs(2);

/// How long a line waits for its account to get on a server before it is given up.
const HOLD: Duration = Duration::from_secs(120);

enum Route {
    Now,
    Hold,
    Never,
}

fn route(g: &Game) -> Route {
    if g.stopping {
        Route::Never
    } else if g.phase.can_chat() {
        Route::Now
    } else if g.phase.chat_follows() {
        Route::Hold
    } else {
        Route::Never
    }
}

fn hand(m: &mut Model, key: &str, line: Line, out: &mut Vec<Effect>) {
    out.push(Effect::Send { key: key.to_owned(), message: ToMod::ChatLine { id: line.id, text: line.text.clone() } });
    m.session.outbox.handed.entry(key.to_owned()).or_default().push(line);
}

fn hold(m: &mut Model, key: &str, mut line: Line, out: &mut Vec<Effect>) {
    let outbox = &mut m.session.outbox;
    outbox.tickets += 1;
    line.ticket = outbox.tickets;
    let (id, ticket) = (line.id, line.ticket);
    let queue = outbox.held.entry(key.to_owned()).or_default();
    let at = queue.iter().position(|l| l.id > id).unwrap_or(queue.len());
    queue.insert(at, line);
    out.push(Effect::After { delay: HOLD, event: Box::new(Event::HoldExpired { key: key.to_owned(), id, ticket }) });
}

fn mark(m: &mut Model, key: &str, id: u64, delivery: Delivery) {
    if let Some(said) = m.said.as_mut() {
        said.set(key, id, delivery);
    }
}

/// Sends `text` to the games `keys`: those on a server get it now, those on their way get it once they are on one, the
/// others are marked as not running.
pub(super) fn say(m: &mut Model, keys: &[String], text: String, out: &mut Vec<Effect>) {
    let mut games: Vec<_> = keys.iter().filter_map(|k| m.games.get(k)).map(|g| (g.name.to_lowercase(), g.key.clone(), g.name.clone(), route(g))).collect();
    games.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    games.dedup_by(|a, b| a.1 == b.1);
    let mut rows = Vec::with_capacity(games.len());
    for (_, key, name, route) in games {
        m.said_seq += 1;
        let id = m.said_seq;
        let line = Line { id, text: text.clone(), ticket: 0, tries: 0 };
        let route = if matches!(route, Route::Now) && joining(m, &key) { Route::Hold } else { route };
        let delivery = match route {
            Route::Now => {
                hand(m, &key, line, out);
                Delivery::Waiting
            }
            Route::Hold => {
                hold(m, &key, line, out);
                Delivery::Held
            }
            Route::Never => Delivery::Refused(ChatRefusal::NotRunning),
        };
        rows.push(SaidRow { key, name, id, delivery });
    }
    m.said = Some(Said { text, rows });
    out.push(Effect::Redraw(Redraw::Chat));
}

/// The account `key` reports what became of line `id`. A line it could not write because it was not on a server goes
/// back to waiting.
pub(super) fn outcome(m: &mut Model, key: &str, id: u64, outcome: ChatOutcome, out: &mut Vec<Effect>) {
    if let Some(queue) = m.session.outbox.held.get_mut(key)
        && let Some(at) = queue.iter().position(|l| l.id == id)
    {
        if matches!(outcome, ChatOutcome::Refused(ChatRefusal::NotInGame | ChatRefusal::Dropped)) {
            return;
        }
        queue.remove(at);
        if queue.is_empty() {
            m.session.outbox.held.remove(key);
        }
    }
    let handed = m.session.outbox.handed.get_mut(key).and_then(|v| v.iter().position(|l| l.id == id).map(|at| v.remove(at)));
    if m.session.outbox.handed.get(key).is_some_and(Vec::is_empty) {
        m.session.outbox.handed.remove(key);
    }
    let delivery = match outcome {
        ChatOutcome::Sent => Delivery::Sent,
        ChatOutcome::Unconfirmed => Delivery::Unconfirmed,
        ChatOutcome::Refused(why @ (ChatRefusal::NotInGame | ChatRefusal::Dropped)) => match (handed, m.games.get(key).map(route)) {
            (Some(line), Some(Route::Hold)) => {
                hold(m, key, line, out);
                Delivery::Held
            }
            (Some(mut line), Some(Route::Now)) if line.tries < RETRIES => {
                line.tries += 1;
                hold(m, key, line, out);
                out.push(Effect::After { delay: RETRY, event: Box::new(Event::ReleaseDue { key: key.to_owned() }) });
                Delivery::Held
            }
            _ => Delivery::Refused(why),
        },
        ChatOutcome::Refused(why) => Delivery::Refused(why),
    };
    mark(m, key, id, delivery);
}

/// Game `key` moved to a new phase: on a server it sends what waited for it, off one what was handed to it and not written
/// waits again, and when nothing brings it back what waits is given up.
pub(super) fn moved(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    if !m.session.outbox.has(key) {
        return;
    }
    let Some(g) = m.games.get(key) else { return };
    if g.phase.can_chat() {
        if !joining(m, key) {
            release(m, key, out);
        }
        return;
    }
    if matches!(route(g), Route::Never) {
        clear(m, key);
        return;
    }
    if g.phase.is_running() {
        return;
    }
    for line in m.session.outbox.handed.remove(key).unwrap_or_default() {
        mark(m, key, line.id, Delivery::Held);
        hold(m, key, line, out);
    }
}

/// A line a game refused while it was on a server is due again: it goes out if the game still is on one.
pub(super) fn retry(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    if m.games.get(key).is_some_and(|g| g.phase.can_chat()) && !joining(m, key) {
        release(m, key, out);
    }
}

/// Whether the join commands of game `key` are still going out, which the user's lines wait for.
fn joining(m: &Model, key: &str) -> bool {
    let s = &m.session;
    s.held_joins.contains_key(key) || s.join_pending.get(key).is_some_and(|g| s.joins.get(key) == Some(g))
}

/// Hands game `key` the lines that waited for it, in the order they were sent.
pub(super) fn release(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    if m.games.get(key).is_some_and(|g| g.stopping) {
        return;
    }
    for line in m.session.outbox.held.remove(key).unwrap_or_default() {
        mark(m, key, line.id, Delivery::Waiting);
        hand(m, key, Line { ticket: 0, ..line }, out);
    }
}

/// Game `key` stops or has nothing to come back to: what waits for it or sits at it is given up.
pub(super) fn clear(m: &mut Model, key: &str) {
    let outbox = &mut m.session.outbox;
    let ids: Vec<u64> = outbox.held.remove(key).into_iter().flatten().chain(outbox.handed.remove(key).into_iter().flatten()).map(|l| l.id).collect();
    for id in ids {
        mark(m, key, id, Delivery::Refused(ChatRefusal::NotRunning));
    }
}

/// The wait of line `id` at `key` ran out, unless it was sent or held again since.
pub(super) fn expire(m: &mut Model, key: &str, id: u64, ticket: u64, out: &mut Vec<Effect>) {
    let Some(queue) = m.session.outbox.held.get_mut(key) else { return };
    let Some(at) = queue.iter().position(|l| l.id == id && l.ticket == ticket) else { return };
    queue.remove(at);
    if queue.is_empty() {
        m.session.outbox.held.remove(key);
    }
    mark(m, key, id, Delivery::Refused(ChatRefusal::Expired));
    out.push(Effect::Redraw(Redraw::Chat));
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::testing::{report, run, settings, state};
    use super::super::testing::setup;
    use super::*;
    use crate::app::model::Phase;
    use crate::core::account::AccountKind;
    use crate::core::ipc::StateKind;

    fn playing() -> Phase {
        Phase::Playing { server: "mc.example".into(), since: Instant::now() }
    }

    fn game(m: &mut Model, name: &str, phase: Phase) -> String {
        let first = m.games.values().next().cloned().unwrap();
        let key = format!("k-{name}");
        m.games.insert(key.clone(), Game { phase, ..Game::new(key.clone(), first.batch_id, first.instance_id, first.account_id, name.into()) });
        key
    }

    fn chat_lines(fx: &[Effect], key: &str) -> Vec<(u64, String)> {
        fx.iter().filter_map(|e| if let Effect::Send { key: k, message: ToMod::ChatLine { id, text } } = e { (k == key).then(|| (*id, text.clone())) } else { None }).collect()
    }

    fn id_of(m: &Model, key: &str) -> u64 {
        m.said.as_ref().unwrap().rows.iter().find(|r| r.key == key).unwrap().id
    }

    fn join(key: &str) -> Report {
        state(key, StateKind::Playing, Some("mc.example"), None)
    }

    use super::super::super::event::Report;

    #[test]
    fn lines_go_now_to_games_on_a_server_and_wait_for_those_on_their_way() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (a, b, c) = (game(&mut m, "a", playing()), game(&mut m, "b", Phase::Connecting { server: "mc.example".into() }), game(&mut m, "c", Phase::Starting { place: 2, total: 3 }));
        let mut out = Vec::new();
        say(&mut m, &[a.clone(), b.clone(), c.clone()], "/home".into(), &mut out);
        let said = m.said.as_ref().unwrap();
        assert_eq!((said.counts(), said.held()), ((0, 1, 0, 0), 2));
        assert_eq!(chat_lines(&out, &a).len(), 1);
        assert!(chat_lines(&out, &b).is_empty() && chat_lines(&out, &c).is_empty());
        assert_eq!(out.iter().filter(|e| matches!(e, Effect::After { event, .. } if matches!(**event, Event::HoldExpired { .. }))).count(), 2);
    }

    #[test]
    fn a_held_line_goes_out_once_with_its_number_when_the_account_gets_on_a_server() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (b, c) = (game(&mut m, "b", Phase::Connecting { server: "mc.example".into() }), game(&mut m, "c", Phase::Starting { place: 1, total: 2 }));
        say(&mut m, &[b.clone(), c.clone()], "/me hi".into(), &mut Vec::new());
        let (id_b, s) = (id_of(&m, &b), settings());
        let fx = report(&mut m, &s, join(&b));
        assert_eq!(chat_lines(&fx, &b), vec![(id_b, "/me hi".to_owned())]);
        assert!(chat_lines(&fx, &c).is_empty());
        assert!(chat_lines(&report(&mut m, &s, join(&b)), &b).is_empty());
        assert_eq!(m.said.as_ref().unwrap().counts(), (0, 1, 0, 0));
        assert_eq!(m.said.as_ref().unwrap().held(), 1);
        outcome(&mut m, &b, id_b, ChatOutcome::Sent, &mut Vec::new());
        report(&mut m, &s, state(&b, StateKind::Menu, None, None));
        assert!(chat_lines(&report(&mut m, &s, join(&b)), &b).is_empty(), "a written line is not sent again");
    }

    #[test]
    fn a_line_the_game_could_not_write_waits_again_and_goes_out_with_the_same_number() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let a = game(&mut m, "a", playing());
        say(&mut m, &[a.clone()], "hi".into(), &mut Vec::new());
        let id = id_of(&m, &a);
        let mut out = Vec::new();
        outcome(&mut m, &a, id, ChatOutcome::Refused(ChatRefusal::Dropped), &mut out);
        assert_eq!(m.said.as_ref().unwrap().held(), 1);
        outcome(&mut m, &a, id, ChatOutcome::Refused(ChatRefusal::Dropped), &mut out);
        assert_eq!(m.session.outbox.held[&a].len(), 1);
        let mut out = Vec::new();
        release(&mut m, &a, &mut out);
        assert_eq!(chat_lines(&out, &a), vec![(id, "hi".to_owned())]);
        outcome(&mut m, &a, id, ChatOutcome::Sent, &mut Vec::new());
        assert_eq!(m.said.as_ref().unwrap().counts(), (1, 0, 0, 0));
        assert!(!m.session.outbox.has(&a));
    }

    #[test]
    fn a_line_handed_to_a_game_with_a_live_process_stays_with_it_when_an_offer_comes() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let a = game(&mut m, "a", playing());
        say(&mut m, &[a.clone()], "hi".into(), &mut Vec::new());
        m.games.get_mut(&a).unwrap().phase = Phase::Connecting { server: "mc.example".into() };
        moved(&mut m, &a, &mut Vec::new());
        assert_eq!((m.said.as_ref().unwrap().held(), m.said.as_ref().unwrap().counts().1), (0, 1));
        assert!(m.session.outbox.held.is_empty());
    }

    #[test]
    fn a_line_handed_to_a_game_whose_process_ended_waits_for_its_restart() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let a = game(&mut m, "a", playing());
        say(&mut m, &[a.clone()], "hi".into(), &mut Vec::new());
        m.games.get_mut(&a).unwrap().phase = Phase::Restarting { attempt: 1, of: 3 };
        moved(&mut m, &a, &mut Vec::new());
        assert_eq!(m.said.as_ref().unwrap().held(), 1);
    }

    #[test]
    fn a_line_typed_while_the_join_commands_go_out_waits_for_them() {
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &["/login pw"]);
        let s = settings();
        report(&mut m, &s, join(&key));
        let mut out = Vec::new();
        say(&mut m, &[key.clone()], "/home".into(), &mut out);
        assert!(chat_lines(&out, &key).is_empty());
        assert_eq!(m.said.as_ref().unwrap().held(), 1);
    }

    #[test]
    fn a_game_that_keeps_refusing_while_on_a_server_gets_the_line_a_few_times_then_it_is_given_up() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let a = game(&mut m, "a", playing());
        say(&mut m, &[a.clone()], "hi".into(), &mut Vec::new());
        let id = id_of(&m, &a);
        for round in 0..=RETRIES {
            let mut out = Vec::new();
            outcome(&mut m, &a, id, ChatOutcome::Refused(ChatRefusal::NotInGame), &mut out);
            if round < RETRIES {
                assert!(out.iter().any(|e| matches!(e, Effect::After { event, .. } if matches!(**event, Event::ReleaseDue { .. }))));
                let mut out = Vec::new();
                retry(&mut m, &a, &mut out);
                assert_eq!(chat_lines(&out, &a).len(), 1);
            }
        }
        assert_eq!(m.said.as_ref().unwrap().missed().collect::<Vec<_>>(), vec![("a", ChatRefusal::NotInGame)]);
        assert!(!m.session.outbox.has(&a));
    }

    #[test]
    fn a_line_handed_to_a_game_that_drops_waits_for_its_return() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (a, s) = (game(&mut m, "a", playing()), settings());
        let (batch, instance, account) = { let g = &m.games[&a]; (g.batch_id.clone(), g.instance_id.clone(), g.account_id) };
        m.games.insert(a.clone(), Game { phase: playing(), ..Game::new(a.clone(), batch, instance, account, "a".into()) });
        say(&mut m, &[a.clone()], "hi".into(), &mut Vec::new());
        let id = id_of(&m, &a);
        report(&mut m, &s, state(&a, StateKind::Disconnected, Some("mc.example"), Some(crate::core::ipc::DisconnectCause::ServerDown)));
        outcome(&mut m, &a, id, ChatOutcome::Refused(ChatRefusal::Dropped), &mut Vec::new());
        assert_eq!(m.said.as_ref().unwrap().held(), 1);
        let fx = report(&mut m, &s, join(&a));
        assert_eq!(chat_lines(&fx, &a), vec![(id, "hi".to_owned())]);
    }

    fn dropped_game() -> (Model, String, u64) {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let a = game(&mut m, "a", playing());
        say(&mut m, &[a.clone()], "hi".into(), &mut Vec::new());
        let id = id_of(&m, &a);
        report(&mut m, &settings(), state(&a, StateKind::Disconnected, Some("mc.example"), Some(crate::core::ipc::DisconnectCause::ServerDown)));
        outcome(&mut m, &a, id, ChatOutcome::Refused(ChatRefusal::Dropped), &mut Vec::new());
        (m, a, id)
    }

    #[test]
    fn a_late_refusal_of_a_line_that_already_waits_changes_nothing() {
        let (mut m, a, id) = dropped_game();
        outcome(&mut m, &a, id, ChatOutcome::Refused(ChatRefusal::Dropped), &mut Vec::new());
        assert_eq!((m.said.as_ref().unwrap().held(), m.said.as_ref().unwrap().missed().count()), (1, 0));
        assert_eq!(m.session.outbox.held[&a].len(), 1);
    }

    #[test]
    fn a_late_sent_report_takes_the_line_out_of_the_queue() {
        let (mut m, a, id) = dropped_game();
        outcome(&mut m, &a, id, ChatOutcome::Sent, &mut Vec::new());
        assert!(!m.session.outbox.has(&a));
        assert_eq!(m.said.as_ref().unwrap().counts(), (1, 0, 0, 0));
        assert!(chat_lines(&report(&mut m, &settings(), join(&a)), &a).is_empty(), "a written line is not sent again");
    }

    #[test]
    fn a_line_that_waited_too_long_is_refused_as_expired() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (b, s) = (game(&mut m, "b", Phase::Menu), settings());
        let mut out = Vec::new();
        say(&mut m, &[b.clone()], "hi".into(), &mut out);
        let (id, ticket) = out.iter().find_map(|e| if let Effect::After { event, .. } = e { if let Event::HoldExpired { id, ticket, .. } = **event { Some((id, ticket)) } else { None } } else { None }).unwrap();
        run(&mut m, &s, Instant::now(), Event::HoldExpired { key: b.clone(), id, ticket: ticket + 1 });
        assert_eq!(m.said.as_ref().unwrap().held(), 1);
        run(&mut m, &s, Instant::now(), Event::HoldExpired { key: b.clone(), id, ticket });
        assert_eq!(m.said.as_ref().unwrap().missed().collect::<Vec<_>>(), vec![("b", ChatRefusal::Expired)]);
        assert!(!m.session.outbox.has(&b));
        let fx = report(&mut m, &s, join(&b));
        assert!(chat_lines(&fx, &b).is_empty());
    }

    #[test]
    fn join_commands_go_before_the_lines_that_waited() {
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &["/login pw"]);
        let s = settings();
        say(&mut m, &[key.clone()], "/me hi".into(), &mut Vec::new());
        let fx = report(&mut m, &s, join(&key));
        assert!(chat_lines(&fx, &key).is_empty(), "the join command goes first");
        let (generation, delay, lines) = fx.iter().find_map(|e| if let Effect::After { event, .. } = e { if let Event::JoinDue { generation, delay, lines, .. } = &**event { Some((*generation, *delay, lines.clone())) } else { None } } else { None }).expect("join scheduled");
        let fx = run(&mut m, &s, Instant::now(), Event::JoinDue { key: key.clone(), generation, lines, delay });
        let sends: Vec<&str> = fx.iter().filter_map(|e| match e { Effect::Send { message: ToMod::ChatSend { .. }, .. } => Some("join"), Effect::Send { message: ToMod::ChatLine { .. }, .. } => Some("line"), _ => None }).collect();
        assert_eq!(sends, ["join", "line"]);
    }

    #[test]
    fn games_that_do_not_run_get_a_refusal_and_nothing_waits() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let failed = game(&mut m, "f", Phase::Failed { reason: "x".into() });
        let login = game(&mut m, "l", Phase::LoginNeeded);
        let away = game(&mut m, "d", Phase::Disconnected { server: Some("s".into()), reason: String::new() });
        say(&mut m, &[failed, login, away], "hi".into(), &mut Vec::new());
        let said = m.said.as_ref().unwrap();
        assert_eq!(said.missed().count(), 3);
        assert!(said.missed().all(|(_, why)| why == ChatRefusal::NotRunning));
        assert_eq!(said.held(), 0);
    }

    #[test]
    fn stopping_an_account_drops_what_waits_for_it() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (b, s) = (game(&mut m, "b", Phase::Connecting { server: "mc.example".into() }), settings());
        say(&mut m, &[b.clone()], "hi".into(), &mut Vec::new());
        run(&mut m, &s, Instant::now(), Event::Stop(vec![b.clone()]));
        assert!(!m.session.outbox.has(&b));
        assert_eq!(m.said.as_ref().unwrap().missed().collect::<Vec<_>>(), vec![("b", ChatRefusal::NotRunning)]);
    }

    #[test]
    fn outcomes_settle_the_row_with_their_number_only() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (a, b) = (game(&mut m, "a", playing()), game(&mut m, "b", playing()));
        say(&mut m, &[a, b], "hi".into(), &mut Vec::new());
        let ids: Vec<u64> = m.said.as_ref().unwrap().rows.iter().map(|r| r.id).collect();
        let mut out = Vec::new();
        outcome(&mut m, "k-a", ids[0], ChatOutcome::Sent, &mut out);
        outcome(&mut m, "k-b", ids[1], ChatOutcome::Refused(ChatRefusal::TooLong), &mut out);
        outcome(&mut m, "k-a", 999, ChatOutcome::Sent, &mut out);
        assert_eq!(m.said.as_ref().unwrap().counts(), (1, 0, 0, 1));
        say(&mut m, &[], "next".into(), &mut Vec::new());
        outcome(&mut m, "k-a", ids[0], ChatOutcome::Sent, &mut out);
        assert_eq!(m.said.as_ref().unwrap().counts(), (0, 0, 0, 0));
    }

    #[test]
    fn an_outcome_of_another_account_does_not_settle_the_row() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (a, b) = (game(&mut m, "a", playing()), game(&mut m, "b", playing()));
        say(&mut m, &[a, b], "hi".into(), &mut Vec::new());
        let id = m.said.as_ref().unwrap().rows[0].id;
        outcome(&mut m, "k-b", id, ChatOutcome::Sent, &mut Vec::new());
        assert_eq!(m.said.as_ref().unwrap().counts(), (0, 2, 0, 0));
    }
}
