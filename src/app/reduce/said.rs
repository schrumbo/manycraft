//! Lines the user sends to accounts: each account gets its own numbered copy and reports what became of it.

use super::super::effect::{Effect, Redraw};
use super::super::model::{Delivery, Model, Said, SaidRow};
use crate::core::ipc::{ChatOutcome, ChatRefusal, ToMod};

/// Sends `text` to the games `keys`: those on a server get it, the others are marked as not in the game.
pub(super) fn say(m: &mut Model, keys: &[String], text: String, out: &mut Vec<Effect>) {
    let mut games: Vec<_> = keys.iter().filter_map(|k| m.games.get(k)).map(|g| (g.name.to_lowercase(), g.key.clone(), g.name.clone(), g.phase.can_chat())).collect();
    games.sort();
    games.dedup();
    let mut rows = Vec::with_capacity(games.len());
    for (_, key, name, chat) in games {
        m.said_seq += 1;
        let id = m.said_seq;
        let delivery = if chat {
            out.push(Effect::Send { key: key.clone(), message: ToMod::ChatLine { id, text: text.clone() } });
            Delivery::Waiting
        } else {
            Delivery::Refused(ChatRefusal::NotInGame)
        };
        rows.push(SaidRow { key, name, id, delivery });
    }
    m.said = Some(Said { text, rows });
    out.push(Effect::Redraw(Redraw::Chat));
}

/// The account `key` reports what became of line `id`.
pub(super) fn outcome(m: &mut Model, key: &str, id: u64, outcome: ChatOutcome) {
    if let Some(said) = m.said.as_mut() {
        said.settle(key, id, outcome);
    }
}

/// The game `key` left its server: what still waited for its turn will not be sent.
pub(super) fn drop_waiting(m: &mut Model, key: &str) {
    if let Some(said) = m.said.as_mut() {
        for row in said.rows.iter_mut().filter(|r| r.key == key && r.delivery == Delivery::Waiting) {
            row.delivery = Delivery::Refused(ChatRefusal::Dropped);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::testing::setup;
    use super::*;
    use crate::app::model::{Game, Phase};
    use crate::core::account::AccountKind;

    fn playing() -> Phase {
        Phase::Playing { server: "mc.example".into(), since: Instant::now() }
    }

    fn game(m: &mut Model, name: &str, phase: Phase) -> String {
        let first = m.games.values().next().cloned().unwrap();
        let key = format!("k-{name}");
        m.games.insert(key.clone(), Game { phase, ..Game::new(key.clone(), first.batch_id, first.instance_id, first.account_id, name.into()) });
        key
    }

    #[test]
    fn lines_go_to_games_on_a_server_and_the_rest_are_marked() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (on, off) = (game(&mut m, "on", playing()), game(&mut m, "off", Phase::Menu));
        let mut out = Vec::new();
        say(&mut m, &[on.clone(), off.clone()], "/home".into(), &mut out);
        let said = m.said.as_ref().unwrap();
        assert_eq!(said.counts(), (0, 1, 0, 1));
        assert!(out.iter().any(|e| matches!(e, Effect::Send { key, message: ToMod::ChatLine { text, .. } } if *key == on && text == "/home")));
        assert!(!out.iter().any(|e| matches!(e, Effect::Send { key, .. } if *key == off)));
        assert_eq!(said.missed().collect::<Vec<_>>(), vec![("off", ChatRefusal::NotInGame)]);
        assert_eq!(m.games.len(), 3);
    }

    #[test]
    fn outcomes_settle_the_row_with_their_number_only() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (a, b) = (game(&mut m, "a", playing()), game(&mut m, "b", playing()));
        say(&mut m, &[a, b], "hi".into(), &mut Vec::new());
        let ids: Vec<u64> = m.said.as_ref().unwrap().rows.iter().map(|r| r.id).collect();
        outcome(&mut m, "k-a", ids[0], ChatOutcome::Sent);
        outcome(&mut m, "k-b", ids[1], ChatOutcome::Refused(ChatRefusal::Dropped));
        outcome(&mut m, "k-a", 999, ChatOutcome::Sent);
        assert_eq!(m.said.as_ref().unwrap().counts(), (1, 0, 0, 1));
        say(&mut m, &[], "next".into(), &mut Vec::new());
        outcome(&mut m, "k-a", ids[0], ChatOutcome::Sent);
        assert_eq!(m.said.as_ref().unwrap().counts(), (0, 0, 0, 0));
    }

    #[test]
    fn an_outcome_of_another_account_does_not_settle_the_row() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (a, b) = (game(&mut m, "a", playing()), game(&mut m, "b", playing()));
        say(&mut m, &[a, b], "hi".into(), &mut Vec::new());
        let id = m.said.as_ref().unwrap().rows[0].id;
        outcome(&mut m, "k-b", id, ChatOutcome::Sent);
        assert_eq!(m.said.as_ref().unwrap().counts(), (0, 2, 0, 0));
    }

    #[test]
    fn leaving_the_server_drops_what_waits() {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (a, b) = (game(&mut m, "a", playing()), game(&mut m, "b", playing()));
        say(&mut m, &[a.clone(), b], "hi".into(), &mut Vec::new());
        drop_waiting(&mut m, &a);
        assert_eq!(m.said.as_ref().unwrap().counts(), (0, 1, 0, 1));
        assert_eq!(m.said.as_ref().unwrap().missed().collect::<Vec<_>>(), vec![("a", ChatRefusal::Dropped)]);
    }
}