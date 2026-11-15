use std::collections::HashSet;

use uuid::Uuid;

use super::super::effect::{Effect, Redraw};
use super::super::model::{Model, Phase};
use super::super::phase::Why;
use super::go;
use crate::core::account::Account;
use crate::core::i18n::trf;
use crate::core::ipc::ToMod;

/// Starts these games, grouped by what plays them.
pub(super) fn start_keys(m: &mut Model, keys: &[String], out: &mut Vec<Effect>) {
    let mut by_batch: Vec<(String, Vec<String>)> = Vec::new();
    for key in keys {
        let Some(i) = m.games.get(key) else { continue };
        match by_batch.iter_mut().find(|(b, _)| *b == i.batch_id) {
            Some((_, keys)) => keys.push(key.clone()),
            None => by_batch.push((i.batch_id.clone(), vec![key.clone()])),
        }
    }
    for (batch, keys) in by_batch {
        launch(m, &batch, Some(&keys), false, out);
    }
}

/// Starts the games of `owner` that are not on their way yet, `only` these when given; `restart` keeps the crash count of a restart after a crash.
pub(super) fn launch(m: &mut Model, owner: &str, only: Option<&[String]>, restart: bool, out: &mut Vec<Effect>) {
    let alive: HashSet<&String> = m.session.pids.keys().collect();
    let Some(target) = m.target(owner) else { return };
    let ids: Vec<Uuid> = match &target.batch {
        Some(batch) => batch.accounts.clone(),
        None => m.games.values().filter(|g| g.batch_id == owner).map(|g| g.account_id).collect(),
    };
    let wanted: Vec<Account> = ids
        .iter()
        .filter_map(|id| m.account(*id).cloned())
        .filter(|a| {
            let key = target.key(a.id);
            only.is_none_or(|o| o.contains(&key)) && !alive.contains(&key) && m.games.get(&key).is_some_and(|i| if only.is_some() { !i.phase.is_running() } else { !i.phase.is_active() })
        })
        .collect();
    let (busy, accounts): (Vec<Account>, Vec<Account>) = wanted.into_iter().partition(|a| m.playing_elsewhere(&target.instance.id, a.id, &target.key(a.id)).is_some());
    let busy: Vec<(String, String)> = busy.iter().filter_map(|a| m.playing_elsewhere(&target.instance.id, a.id, &target.key(a.id)).map(|place| (target.key(a.id), trf("Spielt schon in %@", &[&place])))).collect();
    if !busy.is_empty() {
        for (key, reason) in busy {
            go(m, &key, Phase::Failed { reason }, Why::Busy, out);
        }
        m.order_seq += 1;
        out.push(Effect::Redraw(Redraw::All));
    }
    if accounts.is_empty() {
        return;
    }
    let total = accounts.len();
    for (n, account) in accounts.iter().enumerate() {
        let key = target.key(account.id);
        if !restart {
            m.session.crash_restarts.remove(&key);
            *m.session.reconnects.entry(key.clone()).or_default() += 1;
        }
        if go(m, &key, Phase::Starting { place: n + 1, total }, Why::Launch, out)
            && let Some(i) = m.games.get_mut(&key)
        {
            i.reconnect_attempts = 0;
            i.stopping = false;
        }
    }
    m.order_seq += 1;
    out.push(Effect::Redraw(Redraw::All));
    if !restart {
        out.push(Effect::CheckFit { count: u32::try_from(accounts.len()).unwrap_or(u32::MAX), memory_mb: target.memory_mb() });
    }
    out.push(Effect::Start { owner: owner.to_owned(), target, accounts });
}

/// `keys` without the accounts that need to sign in again, which a bulk start would only fail.
pub(super) fn signed_in(m: &Model, keys: &[String]) -> Vec<String> {
    keys.iter().filter(|k| m.games.get(*k).is_some_and(|i| i.phase != Phase::LoginNeeded)).cloned().collect()
}

/// The keys of restart `group` once none of them is still on its way down, so they start again together.
pub(super) fn restart_done(m: &mut Model, group: u64) -> Option<Vec<String>> {
    let s = &mut m.session;
    if s.restarts.values().any(|g| *g == group) { None } else { s.restart_ready.remove(&group).filter(|keys| !keys.is_empty()) }
}

/// Takes `key` out of any restart, so it is not started again; returns the others of its group once none of them is still on its way down.
pub(super) fn drop_restart(m: &mut Model, key: &str) -> Vec<String> {
    for waiting in m.session.restart_ready.values_mut() {
        waiting.retain(|k| k != key);
    }
    m.session.restarts.remove(key).and_then(|group| restart_done(m, group)).unwrap_or_default()
}

pub(super) fn stop_keys(m: &mut Model, keys: Vec<String>, out: &mut Vec<Effect>) {
    let mut running = Vec::new();
    let mut ready = Vec::new();
    for key in &keys {
        ready.extend(drop_restart(m, key));
        let s = &mut m.session;
        s.reconnects.entry(key.clone()).and_modify(|g| *g += 1).or_insert(1);
        s.joins.entry(key.clone()).and_modify(|g| *g += 1).or_insert(1);
        s.crash_restarts.remove(key);
        s.joined.remove(key);
        s.phone.forget(key);
        if s.show_when_ready.as_ref().is_some_and(|(k, _)| k == key) {
            s.show_when_ready = None;
        }
        let has_pid = s.pids.contains_key(key);
        let Some(i) = m.games.get_mut(key) else { continue };
        match i.phase {
            Phase::Starting { .. } if !has_pid => {
                go(m, key, Phase::Stopped, Why::Stop, out);
                running.push(key.clone());
            }
            ref p if p.is_running() || has_pid => {
                i.stopping = true;
                running.push(key.clone());
            }
            Phase::Crashed { .. } | Phase::Restarting { .. } | Phase::Failed { .. } | Phase::NoNetwork => {
                go(m, key, Phase::Stopped, Why::Stop, out);
            }
            _ => {}
        }
    }
    m.order_seq += 1;
    out.push(Effect::Redraw(Redraw::All));
    ready.retain(|k| !keys.contains(k));
    if !running.is_empty() {
        out.push(Effect::Stop(running));
    }
    if !ready.is_empty() {
        start_keys(m, &ready, out);
    }
}

pub(super) fn restart_keys(m: &mut Model, keys: &[String], out: &mut Vec<Effect>) {
    let running: Vec<String> = keys.iter().filter(|k| m.session.pids.contains_key(*k) || m.games.get(*k).is_some_and(|i| i.phase.is_running())).cloned().collect();
    stop_keys(m, running.clone(), out);
    let mut start: Vec<String> = keys.iter().filter(|k| !running.contains(k)).cloned().collect();
    m.session.restart_groups += 1;
    let group = m.session.restart_groups;
    for key in running {
        if m.games.get(&key).is_some_and(|i| i.stopping) {
            m.session.restarts.insert(key, group);
        } else {
            start.push(key);
        }
    }
    out.push(Effect::Redraw(Redraw::All));
    start_keys(m, &start, out);
}

pub(super) fn stop_batch(m: &mut Model, batch: &str, out: &mut Vec<Effect>) {
    out.push(Effect::CancelStarts(batch.to_owned()));
    let keys: Vec<String> = m.games.values().filter(|i| i.batch_id == batch).map(|i| i.key.clone()).collect();
    stop_keys(m, keys, out);
}

pub(super) fn reconnect(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    out.push(Effect::Redraw(Redraw::All));
    let Some(i) = m.games.get(key) else { return };
    if !i.phase.is_running() || matches!(i.phase, Phase::Plain) {
        return;
    }
    let server = super::games::server_of(&i.phase).unwrap_or_default();
    *m.session.reconnects.entry(key.to_owned()).or_default() += 1;
    *m.session.joins.entry(key.to_owned()).or_default() += 1;
    if let Some(i) = m.games.get_mut(key) {
        i.reconnect_attempts = 0;
    }
    go(m, key, Phase::Connecting { server }, Why::Reconnect, out);
    m.order_seq += 1;
    out.push(Effect::Send { key: key.to_owned(), message: ToMod::Reconnect });
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::super::effect::Effect;
    use super::super::super::event::Event;
    use super::super::super::model::Phase;
    use super::super::testing::*;
    use crate::core::account::AccountKind;

    #[test]
    fn a_requested_restart_starts_again_only_after_a_stop_exit() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.session.pids.insert(key.clone(), 1);
        let fx = run(&mut m, &s, Instant::now(), Event::Restart(vec![key.clone()]));
        assert!(fx.iter().any(|e| matches!(e, Effect::Stop(k) if *k == [key.clone()])));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Start { .. })), "not before the old game is down");
        assert!(m.games[&key].stopping);
        let fx = report(&mut m, &s, super::super::super::event::Report::Exited { key: key.clone(), code: Some(0) });
        assert!(fx.iter().any(|e| matches!(e, Effect::Start { accounts, .. } if accounts.len() == 1)));
        assert_eq!(m.games[&key].phase, Phase::Starting { place: 1, total: 1 });
    }

    #[test]
    fn a_stopped_waiting_start_ends_at_once_and_cancels_its_timer() {
        let s = settings();
        let (mut m, key) = setup(Phase::Starting { place: 2, total: 3 }, AccountKind::Offline, &[]);
        let fx = run(&mut m, &s, Instant::now(), Event::Stop(vec![key.clone()]));
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(!m.games[&key].stopping, "nothing runs, so nothing is on its way down");
        assert!(fx.iter().any(|e| matches!(e, Effect::Stop(k) if *k == [key.clone()])), "the fleet drops it from its line");
        let (mut m, key) = setup(Phase::NoNetwork, AccountKind::Offline, &[]);
        run(&mut m, &s, Instant::now(), Event::Stop(vec![key.clone()]));
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(run(&mut m, &s, Instant::now(), Event::WaitingStartDue { key: key.clone() }).is_empty(), "a stopped waiting start does not start later");
    }

    #[test]
    fn a_stop_drops_a_pending_show() {
        let s = settings();
        let (mut m, key) = setup(Phase::Starting { place: 0, total: 0 }, AccountKind::Offline, &[]);
        m.session.show_when_ready = Some((key.clone(), Instant::now()));
        run(&mut m, &s, Instant::now(), Event::Stop(vec![key.clone()]));
        assert!(m.session.show_when_ready.is_none());
        run(&mut m, &s, Instant::now(), Event::Start(vec![key.clone()]));
        let fx = report(&mut m, &s, state(&key, crate::core::ipc::StateKind::Menu, None, None));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Show { .. })), "the new start is not shown by the old request");
    }

    #[test]
    fn a_stopped_account_leaves_its_restart_group_and_the_rest_still_starts() {
        let s = settings();
        let (mut m, key) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        m.session.restarts.insert("a".into(), 1);
        m.session.restarts.insert("b".into(), 1);
        m.session.restart_ready.insert(1, vec!["c".into()]);
        m.session.restarts.remove("a");
        m.session.restart_ready.get_mut(&1).unwrap().push("a".into());
        assert!(super::drop_restart(&mut m, "a").is_empty(), "b is still going down");
        assert_eq!(super::drop_restart(&mut m, "b"), vec!["c".to_string()], "a user stop never starts a, and c goes on once b is down");
        m.session.restarts.insert("gone".into(), 2);
        m.session.restart_ready.insert(2, vec![key.clone()]);
        let fx = report(&mut m, &s, super::super::super::event::Report::Exited { key: "gone".into(), code: Some(0) });
        assert!(fx.iter().any(|e| matches!(e, Effect::Start { accounts, .. } if accounts.len() == 1 && accounts[0].id == account(&m))), "a removed instance does not hold up its group");
    }

    #[test]
    fn a_running_game_is_not_started_twice() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        assert!(run(&mut m, &s, Instant::now(), Event::Start(vec![key.clone()])).iter().all(|e| !matches!(e, Effect::Start { .. })));
        assert_eq!(m.games[&key].phase, Phase::Menu);
    }
}
