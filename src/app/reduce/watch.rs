//! What the server of a connection-only account shows about its player: health and food, the players near it, and its death.

use std::collections::VecDeque;
use std::time::Duration;

use super::super::effect::{Effect, Redraw};
use super::super::event::Event;
use super::super::model::{Model, Phase, Vitals};
use super::super::session::Alert;
use super::games::{Fx, Later, finish, note};
use super::Env;
use crate::core::batch::AfterDeath;
use crate::core::history::Kind;
use crate::core::i18n::tr;
use crate::core::ipc::ToMod;

const RESPAWN_AFTER: Duration = Duration::from_secs(2);
const DEATH_WINDOW: Duration = Duration::from_secs(60);
const DEATHS_MAX: usize = 3;
const STRANGER_COOLDOWN: Duration = Duration::from_secs(600);

/// Takes in the health and food of connection-only account `key`; only tiles on screen redraw.
pub(super) fn vitals(m: &mut Model, key: &str, health: u16, food: u8, out: &mut Vec<Effect>) {
    let Some(g) = m.games.get_mut(key).filter(|g| g.connection) else { return };
    let vitals = Some(Vitals { health, food });
    if g.vitals == vitals {
        return;
    }
    g.vitals = vitals;
    if m.session.visible.contains(key) {
        out.push(Effect::Redraw(Redraw::All));
    }
}

/// Takes in the players near connection-only account `key`. Those whose name is one of the user's accounts are left out.
/// An alert goes out when one arrives that did not raise one near this account in the last [`STRANGER_COOLDOWN`].
pub(super) fn nearby(m: &mut Model, env: &Env, key: &str, players: Vec<String>, fx: &mut Fx) {
    let own: Vec<String> = m.accounts.iter().map(|a| a.name.to_lowercase()).collect();
    let strangers: Vec<String> = players.into_iter().filter(|p| !own.contains(&p.to_lowercase())).collect();
    let Some(g) = m.games.get_mut(key).filter(|g| g.connection) else { return };
    if g.nearby == strangers {
        return;
    }
    let arrived: Vec<String> = strangers.iter().filter(|p| !g.nearby.contains(p)).cloned().collect();
    g.nearby = strangers;
    let alone = g.alone();
    let (batch, name) = (g.batch_id.clone(), g.name.clone());
    let seen = &mut m.session.strangers;
    seen.retain(|_, at| env.now.saturating_duration_since(*at) < STRANGER_COOLDOWN);
    let mut raised = false;
    for player in arrived {
        if let std::collections::hash_map::Entry::Vacant(slot) = seen.entry((key.to_owned(), player.to_lowercase())) {
            slot.insert(env.now);
            raised = true;
        }
    }
    if raised && !alone {
        fx.alerts.push((batch, name, Alert::Nearby));
    }
    fx.now.push(Effect::Redraw(Redraw::All));
}

/// Forgets what the server of `key` showed about its player, once it is gone or starts again.
pub(super) fn forget(m: &mut Model, key: &str) {
    if let Some(g) = m.games.get_mut(key) {
        g.vitals = None;
        g.nearby.clear();
    }
    m.session.respawns.remove(key);
}

/// Connection-only account `key` just died: the death goes into its history, and when its batch lets it, it respawns on its
/// own after a short delay unless it keeps dying. Whether it will, which spares the user a notification.
pub(super) fn died(m: &mut Model, env: &Env, key: &str, fx: &mut Fx) -> bool {
    let Some(g) = m.games.get(key) else { return false };
    let Phase::Dead { server, reason } = &g.phase else { return false };
    let (server, reason) = (server.clone(), reason.clone());
    let wants = m.batch(&g.batch_id).is_some_and(|b| b.unattended().after_death == AfterDeath::Respawn);
    note(m, env, fx, key, Kind::Died, server, &reason);
    let deaths = m.session.deaths.entry(key.to_owned()).or_insert_with(VecDeque::new);
    deaths.push_back(env.now);
    while deaths.front().is_some_and(|at| env.now.saturating_duration_since(*at) > DEATH_WINDOW) {
        deaths.pop_front();
    }
    if !wants || deaths.len() > DEATHS_MAX {
        return false;
    }
    m.session.respawn_seq += 1;
    let generation = m.session.respawn_seq;
    m.session.respawns.insert(key.to_owned(), generation);
    fx.later.push(Later::After(RESPAWN_AFTER, Event::AutoRespawnDue { key: key.to_owned(), generation }));
    true
}

/// The delay of the automatic respawn of `key` ran out: respawns it once when it still is dead and the timer is the latest.
pub(super) fn respawn_due(m: &mut Model, env: &Env, key: &str, generation: u64, out: &mut Vec<Effect>) {
    if m.session.respawns.get(key) != Some(&generation) {
        return;
    }
    m.session.respawns.remove(key);
    let Some((Phase::Dead { server, .. }, batch)) = m.games.get(key).filter(|g| g.connection && !g.stopping).map(|g| (g.phase.clone(), g.batch_id.clone())) else { return };
    if !m.batch(&batch).is_some_and(|b| b.unattended().after_death == AfterDeath::Respawn) {
        return;
    }
    out.push(Effect::Send { key: key.to_owned(), message: ToMod::Respawn });
    let mut fx = Fx::default();
    note(m, env, &mut fx, key, Kind::Respawned, server, &tr("Automatisch nach dem Tod"));
    finish(m, env, fx, out);
}

/// The user chose hotbar slot `slot` for connection-only account `key`.
pub(super) fn hotbar(m: &Model, key: String, slot: u8, out: &mut Vec<Effect>) {
    if playing(m, &key) && slot < 9 {
        out.push(Effect::Send { key, message: ToMod::Hotbar { slot } });
    }
}

/// The user asked connection-only account `key` to use the item in its hand.
pub(super) fn use_item(m: &Model, key: String, out: &mut Vec<Effect>) {
    if playing(m, &key) {
        out.push(Effect::Send { key, message: ToMod::UseItem });
    }
}

fn playing(m: &Model, key: &str) -> bool {
    m.games.get(key).is_some_and(|g| g.connection && !g.stopping && matches!(g.phase, Phase::Playing { .. }))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::super::effect::{Effect, Redraw};
    use super::super::super::event::{Event, Report};
    use super::super::super::model::{Model, Need, Phase};
    use super::super::testing::{later, report, run, settings, setup, state};
    use crate::core::account::{Account, AccountKind};
    use crate::core::batch::{AfterDeath, RunMode};
    use crate::core::history::Kind;
    use crate::core::ipc::{FromMod, StateKind, ToMod};
    use crate::core::settings::Settings;

    const SERVER: &str = "mc.example:25565";

    fn fleet() -> (Model, Vec<String>, Settings) {
        let s = settings();
        let (mut m, first) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.batches[0].mode = RunMode::Connection;
        let mut second = m.games[&first].clone();
        second.key = "second".into();
        second.name = "AltTwo".into();
        second.account_id = uuid::Uuid::from_u128(8);
        m.accounts.push(Account { id: second.account_id, name: "AltTwo".into(), kind: AccountKind::Offline, proxy: None, color: None });
        m.games.insert(second.key.clone(), second);
        let keys = vec![first, "second".to_owned()];
        for key in &keys {
            let g = m.games.get_mut(key).unwrap();
            g.headless = true;
            g.connection = true;
            report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
            report(&mut m, &s, state(key, StateKind::Playing, Some(SERVER), None));
        }
        (m, keys, s)
    }

    fn near(key: &str, players: &[&str]) -> Report {
        Report::Mod { key: key.into(), message: FromMod::Nearby { players: players.iter().map(|p| (*p).to_owned()).collect() } }
    }

    fn die(m: &mut Model, s: &Settings, key: &str, at: Instant) -> Vec<Effect> {
        run(m, s, at, Event::Reports(vec![state(key, StateKind::Dead, Some(SERVER), None)]))
    }

    fn alerts_due(fx: &[Effect]) -> usize {
        later(fx).iter().filter(|(_, e)| matches!(e, Event::AlertDue { .. })).count()
    }

    fn sends(fx: &[Effect], wanted: fn(&ToMod) -> bool) -> usize {
        fx.iter().filter(|e| matches!(e, Effect::Send { message, .. } if wanted(message))).count()
    }

    fn notified(fx: &[Effect]) -> Vec<(&str, &[String])> {
        fx.iter().filter_map(|e| if let Effect::Notify { title, keys, .. } = e { Some((title.as_str(), keys.as_slice())) } else { None }).collect()
    }

    fn history(m: &Model, key: &str) -> Vec<Kind> {
        m.history.get(key).map(|log| log.iter().map(|e| e.kind).filter(|k| *k != Kind::Connected).collect()).unwrap_or_default()
    }

    #[test]
    fn vitals_show_on_the_game_and_a_repeat_changes_nothing() {
        let (mut m, keys, s) = fleet();
        let vitals = |health| Report::Mod { key: keys[0].clone(), message: FromMod::Vitals { health, food: 18 } };
        report(&mut m, &s, vitals(14));
        assert_eq!(m.games[&keys[0]].vitals.map(|v| (v.health, v.food)), Some((14, 18)));
        let fx = report(&mut m, &s, vitals(14));
        assert!(fx.iter().all(|e| matches!(e, Effect::Redraw(Redraw::Nothing))), "{fx:?}");
        assert_eq!(m.games[&keys[1]].vitals, None);
    }

    #[test]
    fn strangers_in_range_are_one_notification_for_the_batch_and_own_accounts_are_not_strangers() {
        let (mut m, keys, s) = fleet();
        let now = Instant::now();
        let fx = run(&mut m, &s, now, Event::Reports(vec![near(&keys[0], &["altTWO"])]));
        assert_eq!(alerts_due(&fx), 0);
        assert!(m.games[&keys[0]].nearby.is_empty(), "an own account is no stranger");
        assert!(!m.games[&keys[0]].needs_attention());
        let fx = run(&mut m, &s, now, Event::Reports(vec![near(&keys[0], &["Bob"]), near(&keys[1], &["Bob", "Eve"])]));
        assert_eq!(alerts_due(&fx), 1, "both accounts, one timer: {fx:?}");
        assert_eq!(m.needs(None), [(Need::Nearby, 2)]);
        let batch = m.batches[0].id.clone();
        let fx = run(&mut m, &s, now + Duration::from_secs(3), Event::AlertDue { batch });
        let sent = notified(&fx);
        assert_eq!(sent.len(), 1, "{fx:?}");
        assert_eq!(sent[0].1.len(), 2);
    }

    #[test]
    fn a_stranger_who_leaves_and_returns_inside_the_cooldown_raises_nothing_new_and_a_gone_one_clears_the_need() {
        let (mut m, keys, s) = fleet();
        let now = Instant::now();
        let batch = m.batches[0].id.clone();
        run(&mut m, &s, now, Event::Reports(vec![near(&keys[0], &["Bob"])]));
        run(&mut m, &s, now + Duration::from_secs(3), Event::AlertDue { batch });
        run(&mut m, &s, now + Duration::from_secs(20), Event::Reports(vec![near(&keys[0], &[])]));
        assert_eq!(m.needs(None), []);
        let fx = run(&mut m, &s, now + Duration::from_secs(60), Event::Reports(vec![near(&keys[0], &["Bob"])]));
        assert_eq!(alerts_due(&fx), 0, "{fx:?}");
        assert_eq!(m.needs(None), [(Need::Nearby, 1)], "the tile still shows him");
        run(&mut m, &s, now + Duration::from_secs(70), Event::Reports(vec![near(&keys[0], &[])]));
        let fx = run(&mut m, &s, now + Duration::from_secs(700), Event::Reports(vec![near(&keys[0], &["Bob"])]));
        assert_eq!(alerts_due(&fx), 1, "after the cooldown he counts as new");
    }

    #[test]
    fn the_cooldown_counts_from_the_alert_not_from_every_return() {
        let (mut m, keys, s) = fleet();
        let now = Instant::now();
        let at = |secs| now + Duration::from_secs(secs);
        let batch = m.batches[0].id.clone();
        run(&mut m, &s, at(0), Event::Reports(vec![near(&keys[0], &["Bob"])]));
        run(&mut m, &s, at(3), Event::AlertDue { batch });
        run(&mut m, &s, at(10), Event::Reports(vec![near(&keys[0], &[])]));
        let fx = run(&mut m, &s, at(300), Event::Reports(vec![near(&keys[0], &["Bob"])]));
        assert_eq!(alerts_due(&fx), 0);
        run(&mut m, &s, at(310), Event::Reports(vec![near(&keys[0], &[])]));
        let fx = run(&mut m, &s, at(650), Event::Reports(vec![near(&keys[0], &["Bob"])]));
        assert_eq!(alerts_due(&fx), 1, "{fx:?}");
    }

    #[test]
    fn a_batch_switched_to_wait_during_the_delay_does_not_respawn() {
        let (mut m, keys, s) = fleet();
        let key = &keys[0];
        let now = Instant::now();
        let fx = die(&mut m, &s, key, now);
        let generation = later(&fx).into_iter().find_map(|(_, e)| if let Event::AutoRespawnDue { generation, .. } = e { Some(*generation) } else { None }).unwrap();
        let mut u = m.batches[0].unattended();
        u.after_death = AfterDeath::Wait;
        m.batches[0].set_unattended(u);
        let fx = run(&mut m, &s, now + Duration::from_secs(2), Event::AutoRespawnDue { key: key.clone(), generation });
        assert_eq!(sends(&fx, |m| matches!(m, ToMod::Respawn)), 0);
        assert_eq!(history(&m, key), [Kind::Died]);
    }

    #[test]
    fn a_death_respawns_once_after_the_delay_without_an_alert_and_is_logged() {
        let (mut m, keys, s) = fleet();
        let key = &keys[0];
        let now = Instant::now();
        let fx = die(&mut m, &s, key, now);
        let due: Vec<(Duration, u64)> = later(&fx).into_iter().filter_map(|(d, e)| if let Event::AutoRespawnDue { generation, .. } = e { Some((d, *generation)) } else { None }).collect();
        assert_eq!(due.len(), 1, "{fx:?}");
        assert!(due[0].0 < Duration::from_secs(3), "before the alert window ends");
        assert_eq!(alerts_due(&fx), 0, "an automatic respawn is no incident");
        let fx = run(&mut m, &s, now + due[0].0, Event::AutoRespawnDue { key: key.clone(), generation: due[0].1 });
        assert_eq!(sends(&fx, |m| matches!(m, ToMod::Respawn)), 1);
        let fx = run(&mut m, &s, now + due[0].0, Event::AutoRespawnDue { key: key.clone(), generation: due[0].1 });
        assert_eq!(sends(&fx, |m| matches!(m, ToMod::Respawn)), 0, "one timer, one respawn");
        assert_eq!(history(&m, key), [Kind::Died, Kind::Respawned]);
    }

    #[test]
    fn a_batch_that_waits_never_respawns_on_its_own_and_asks_for_the_user() {
        let (mut m, keys, s) = fleet();
        let mut u = m.batches[0].unattended();
        u.after_death = AfterDeath::Wait;
        m.batches[0].set_unattended(u);
        let fx = die(&mut m, &s, &keys[0], Instant::now());
        assert!(!later(&fx).iter().any(|(_, e)| matches!(e, Event::AutoRespawnDue { .. })), "{fx:?}");
        assert_eq!(alerts_due(&fx), 1);
        assert_eq!(m.needs(None), [(Need::Dead, 1)]);
        assert_eq!(history(&m, &keys[0]), [Kind::Died]);
    }

    #[test]
    fn a_stale_timer_or_a_changed_game_respawns_nothing() {
        let (mut m, keys, s) = fleet();
        let key = &keys[0];
        let now = Instant::now();
        let fx = die(&mut m, &s, key, now);
        let generation = later(&fx).into_iter().find_map(|(_, e)| if let Event::AutoRespawnDue { generation, .. } = e { Some(*generation) } else { None }).unwrap();
        let fx = run(&mut m, &s, now, Event::AutoRespawnDue { key: key.clone(), generation: generation + 1 });
        assert_eq!(sends(&fx, |m| matches!(m, ToMod::Respawn)), 0);
        run(&mut m, &s, now, Event::Respawn(key.clone()));
        report(&mut m, &s, state(key, StateKind::Playing, Some(SERVER), None));
        let fx = run(&mut m, &s, now, Event::AutoRespawnDue { key: key.clone(), generation });
        assert_eq!(sends(&fx, |m| matches!(m, ToMod::Respawn)), 0, "alive again");
    }

    #[test]
    fn a_game_that_keeps_dying_is_left_on_the_death_screen_for_the_user() {
        let (mut m, keys, s) = fleet();
        let key = &keys[0];
        let now = Instant::now();
        let mut automatic = 0;
        for n in 0..5u64 {
            let at = now + Duration::from_secs(n * 5);
            let fx = die(&mut m, &s, key, at);
            let armed = later(&fx).into_iter().find_map(|(_, e)| if let Event::AutoRespawnDue { generation, .. } = e { Some(*generation) } else { None });
            if let Some(generation) = armed {
                automatic += 1;
                run(&mut m, &s, at + Duration::from_secs(2), Event::AutoRespawnDue { key: key.clone(), generation });
            }
            report(&mut m, &s, state(key, StateKind::Playing, Some(SERVER), None));
        }
        assert_eq!(automatic, super::DEATHS_MAX);
        let fx = die(&mut m, &s, key, now + Duration::from_secs(30));
        assert!(!later(&fx).iter().any(|(_, e)| matches!(e, Event::AutoRespawnDue { .. })));
        assert_eq!(m.needs(None), [(Need::Dead, 1)]);
    }

    #[test]
    fn hotbar_and_item_go_out_once_per_click_and_only_to_a_playing_connection_only_game() {
        let (mut m, keys, s) = fleet();
        let key = &keys[0];
        let fx = run(&mut m, &s, Instant::now(), Event::Hotbar { key: key.clone(), slot: 4 });
        assert_eq!(sends(&fx, |m| matches!(m, ToMod::Hotbar { slot: 4 })), 1);
        let fx = run(&mut m, &s, Instant::now(), Event::Hotbar { key: key.clone(), slot: 9 });
        assert!(fx.is_empty(), "{fx:?}");
        let fx = run(&mut m, &s, Instant::now(), Event::UseItem(key.clone()));
        assert_eq!(sends(&fx, |m| matches!(m, ToMod::UseItem)), 1);
        m.games.get_mut(key).unwrap().connection = false;
        assert!(run(&mut m, &s, Instant::now(), Event::UseItem(key.clone())).is_empty());
    }
}
