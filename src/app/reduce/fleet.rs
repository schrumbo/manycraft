use std::collections::{HashMap, HashSet};
use std::time::Duration;

use uuid::Uuid;

use super::super::effect::{Effect, Redraw};
use super::super::event::Event;
use super::super::model::{Model, Phase};
use super::super::phase::Why;
use super::go;
use crate::core::account::Account;
use crate::core::fleet::Target;
use crate::core::fit::{self, Memory};
use crate::core::i18n::trf;
use crate::core::ipc::ToMod;

/// Why games start: on their own, as part of one action on many, or again after a crash.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kick {
    /// Waits for memory when more than one game of the owner starts.
    Plain,
    /// Part of one action on many games: always waits for memory.
    Bulk,
    /// A restart after a crash: keeps the crash count and never waits.
    Crash,
}

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
    let kick = if keys.len() > 1 { Kick::Bulk } else { Kick::Plain };
    for (batch, keys) in by_batch {
        launch(m, &batch, Some(&keys), kick, out);
    }
}

/// How long games that wait for free memory wait before it is read again.
const MEMORY_EVERY: Duration = Duration::from_secs(10);

/// Starts the games of `owner` that are not on their way yet, `only` these when given.
/// Games started together, unless after a crash, wait in the memory line and start as far as they fit.
pub(super) fn launch(m: &mut Model, owner: &str, only: Option<&[String]>, kick: Kick, out: &mut Vec<Effect>) {
    let restart = kick == Kick::Crash;
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
            i.memory_wait = None;
        }
    }
    m.order_seq += 1;
    out.push(Effect::Redraw(Redraw::All));
    if kick == Kick::Bulk || (kick == Kick::Plain && total > 1) {
        let keys: Vec<String> = accounts.iter().map(|a| target.key(a.id)).filter(|k| m.games.get(k).is_some_and(|g| g.phase.waiting())).collect();
        for key in &keys {
            m.session.memory_seq += 1;
            let seq = m.session.memory_seq;
            if let Some(g) = m.games.get_mut(key) {
                g.memory_wait = Some(seq);
            }
        }
        let run = m.session.runs.entry(owner.to_owned()).or_default();
        run.retain(|k| !keys.contains(k));
        run.extend(keys);
        ask_memory(m, out);
        return;
    }
    if !restart {
        out.push(Effect::CheckFit { count: u32::try_from(accounts.len()).unwrap_or(u32::MAX), memory_mb: target.memory_mb() });
    }
    out.push(Effect::Start { owner: owner.to_owned(), target, accounts });
}

/// Asks for a reading of the free memory, unless one is on its way.
pub(super) fn ask_memory(m: &mut Model, out: &mut Vec<Effect>) {
    if !m.session.memory_asked {
        m.session.memory_asked = true;
        out.push(Effect::ReadMemory);
    }
}

/// The games of `owner` (of all when `None`) that wait for free memory, first in line first.
fn memory_line(m: &Model, owner: Option<&str>) -> Vec<String> {
    let mut line: Vec<(u64, &String)> = m
        .games
        .values()
        .filter(|g| owner.is_none_or(|o| g.batch_id == o) && !g.stopping && g.phase.waiting())
        .filter_map(|g| g.memory_wait.map(|seq| (seq, &g.key)))
        .collect();
    line.sort_unstable();
    line.into_iter().map(|(_, k)| k.clone()).collect()
}

/// Whether any game waits for free memory.
pub(super) fn memory_waiting(m: &Model) -> bool {
    m.games.values().any(|g| g.memory_wait.is_some() && !g.stopping && g.phase.waiting())
}

fn cap_of(m: &Model, caps: &mut HashMap<String, u64>, owner: &str) -> u64 {
    *caps.entry(owner.to_owned()).or_insert_with(|| m.target(owner).map_or(0, |t| u64::from(t.memory_mb())))
}

/// Starts as many waiting games, first in line first, as fit into `memory` beside a reserve and the games still on their way; all when the machine does not tell.
pub(super) fn memory(m: &mut Model, memory: Option<Memory>, out: &mut Vec<Effect>) {
    m.session.memory_asked = false;
    let line = memory_line(m, None);
    if line.is_empty() {
        return;
    }
    let fits = match memory {
        None => line.len(),
        Some(memory) => {
            let mut caps = HashMap::new();
            let pending: u64 = m.games.values().filter(|g| matches!(g.phase, Phase::Starting { .. }) && g.memory_wait.is_none()).map(|g| cap_of(m, &mut caps, &g.batch_id)).sum();
            let mut budget = memory.free_mb.saturating_sub(fit::reserve_mb(memory.total_mb)).saturating_sub(pending);
            line.iter()
                .take_while(|key| {
                    let cap = m.games.get(*key).map_or(0, |g| cap_of(m, &mut caps, &g.batch_id));
                    let fits = cap <= budget;
                    budget = budget.saturating_sub(cap);
                    fits
                })
                .count()
        }
    };
    let fits = if fits == 0 && m.run(None).is_none_or(|r| r.launched == 0) && !m.games.values().any(|g| matches!(g.phase, Phase::Starting { .. }) && g.memory_wait.is_none()) { 1 } else { fits };
    release(m, &line[..fits], out);
    if fits < line.len() && !m.session.memory_timer {
        m.session.memory_timer = true;
        out.push(Effect::After { delay: MEMORY_EVERY, event: Box::new(Event::MemoryDue) });
    }
}

/// Starts the waiting games of `owner` (all when `None`) now, whether they fit or not.
pub(super) fn start_anyway(m: &mut Model, owner: Option<&str>, out: &mut Vec<Effect>) {
    let line = memory_line(m, owner);
    release(m, &line, out);
}

fn release(m: &mut Model, keys: &[String], out: &mut Vec<Effect>) {
    if keys.is_empty() {
        return;
    }
    let mut by_owner: Vec<(String, Target, Vec<Account>)> = Vec::new();
    for key in keys {
        let Some(g) = m.games.get_mut(key) else { continue };
        g.memory_wait = None;
        let (owner, id) = (g.batch_id.clone(), g.account_id);
        let found = match by_owner.iter().position(|(o, ..)| *o == owner) {
            Some(at) => Some(at),
            None => m.target(&owner).map(|target| {
                by_owner.push((owner, target, Vec::new()));
                by_owner.len() - 1
            }),
        };
        match (found, m.account(id).cloned()) {
            (Some(at), Some(account)) => by_owner[at].2.push(account),
            _ => {
                go(m, key, Phase::Stopped, Why::Stop, out);
            }
        }
    }
    out.push(Effect::Redraw(Redraw::All));
    for (owner, target, accounts) in by_owner {
        if !accounts.is_empty() {
            out.push(Effect::Start { owner, target, accounts });
        }
    }
}

/// Cancels the bulk start of `owner` (of every batch when `None`): what has not launched yet stops, what launched keeps running.
pub(super) fn cancel_start(m: &mut Model, owner: Option<&str>, out: &mut Vec<Effect>) {
    let owners: Vec<String> = match owner {
        Some(o) => vec![o.to_owned()],
        None => m.session.runs.keys().cloned().collect(),
    };
    for owner in &owners {
        m.session.runs.remove(owner);
        out.push(Effect::CancelStarts(owner.clone()));
    }
    let keys: Vec<String> = m.games.values().filter(|g| owners.contains(&g.batch_id) && g.phase.waiting() && !m.session.pids.contains_key(&g.key)).map(|g| g.key.clone()).collect();
    stop_keys(m, keys, out);
}

/// The games of `owner` (all when `None`) a stop would end now.
pub(super) fn stoppable(m: &Model, owner: Option<&str>) -> Vec<String> {
    let mut keys: Vec<String> = m.games.values().filter(|g| owner.is_none_or(|o| g.batch_id == o) && g.phase.stoppable() && !g.stopping).map(|g| g.key.clone()).collect();
    keys.sort_unstable();
    keys
}

/// Starts `keys` again after a stop: those still on their way down once they are down, the rest now; games started since stay as they are.
pub(super) fn start_again(m: &mut Model, keys: &[String], out: &mut Vec<Effect>) {
    let mut start = Vec::new();
    let mut down = Vec::new();
    for key in signed_in(m, keys) {
        match m.games.get(&key) {
            Some(g) if g.stopping => down.push(key),
            Some(g) if !g.phase.is_active() => start.push(key),
            _ => {}
        }
    }
    if !down.is_empty() {
        m.session.restart_groups += 1;
        let group = m.session.restart_groups;
        for key in down {
            m.session.restarts.insert(key, group);
        }
    }
    start_keys(m, &start, out);
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
        i.memory_wait = None;
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

    use uuid::Uuid;

    use super::super::super::effect::Effect;
    use super::super::super::event::{Event, Report};
    use super::super::super::model::{Game, Model, Phase, Run};
    use super::super::testing::*;
    use crate::core::account::{Account, AccountKind};
    use crate::core::fit::Memory;

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

    const PC: Memory = Memory { total_mb: 16384, free_mb: 6700 };

    /// A batch of `n` stopped offline accounts whose games get `memory_mb` each, and their keys in the batch's order.
    fn farm(n: u128, memory_mb: u32) -> (Model, Vec<String>) {
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        m.games.clear();
        m.accounts.clear();
        let ids: Vec<Uuid> = (1..=n).map(|n| Uuid::from_u128(100 + n)).collect();
        m.batches[0].accounts = ids.clone();
        m.batches[0].memory_override = Some(memory_mb);
        let (batch, made) = (m.batches[0].clone(), m.instances[0].id.clone());
        let mut keys = Vec::new();
        for (n, id) in ids.into_iter().enumerate() {
            let name = format!("Alt{n}");
            let key = crate::core::fleet::instance_key(&batch, id);
            m.accounts.push(Account { id, name: name.clone(), kind: AccountKind::Offline, proxy: None, color: None });
            m.games.insert(key.clone(), Game::new(key.clone(), batch.id.clone(), made.clone(), id, name));
            keys.push(key);
        }
        (m, keys)
    }

    /// The games the effects hand to the fleet to start, in order.
    fn started(fx: &[Effect]) -> Vec<String> {
        fx.iter().filter_map(|e| if let Effect::Start { target, accounts, .. } = e { Some(accounts.iter().map(|a| target.key(a.id)).collect::<Vec<_>>()) } else { None }).flatten().collect()
    }

    fn reads(fx: &[Effect]) -> bool {
        fx.iter().any(|e| matches!(e, Effect::ReadMemory))
    }

    #[test]
    fn a_bulk_start_starts_what_fits_and_the_rest_waits_for_memory() {
        let s = settings();
        let (mut m, keys) = farm(5, 2048);
        let batch = m.batches[0].id.clone();
        let fx = run(&mut m, &s, Instant::now(), Event::StartBatch(batch.clone()));
        assert!(reads(&fx) && started(&fx).is_empty(), "nothing starts before the memory is known");
        assert!(keys.iter().all(|k| m.waits_for_memory(k)));
        assert_eq!(m.run(None), Some(Run { launched: 0, total: 5, memory: 5 }));
        let fx = run(&mut m, &s, Instant::now(), Event::Memory(Some(PC)));
        assert_eq!(started(&fx), keys[..2], "6700 MB free, 2048 kept back: two games of 2048 MB fit");
        assert!(later(&fx).iter().any(|(_, e)| matches!(e, Event::MemoryDue)), "the rest waits for the next reading");
        assert_eq!(m.run(Some(&batch)), Some(Run { launched: 0, total: 5, memory: 3 }));
        assert!(started(&run(&mut m, &s, Instant::now(), Event::Memory(Some(PC)))).is_empty(), "games still on their way count with their whole memory");
        assert_eq!(started(&run(&mut m, &s, Instant::now(), Event::StartAnyway(None))), keys[2..], "anyway starts every waiting game, in line");
        assert!(keys.iter().all(|k| !m.waits_for_memory(k)));
        let (mut m, keys) = farm(3, 2048);
        run(&mut m, &s, Instant::now(), Event::StartAll);
        assert_eq!(started(&run(&mut m, &s, Instant::now(), Event::Memory(None))), keys, "a machine that does not tell starts everything");
        let (mut m, keys) = farm(3, 2048);
        assert_eq!(started(&run(&mut m, &s, Instant::now(), Event::Start(vec![keys[1].clone()]))), keys[1..2], "one game starts at once");
    }

    #[test]
    fn a_game_that_ends_frees_memory_for_the_next_in_line() {
        let s = settings();
        let (mut m, keys) = farm(5, 2048);
        let batch = m.batches[0].id.clone();
        run(&mut m, &s, Instant::now(), Event::StartBatch(batch));
        run(&mut m, &s, Instant::now(), Event::Memory(Some(PC)));
        for (pid, key) in keys[..2].iter().enumerate() {
            report(&mut m, &s, Report::Launched { key: key.clone(), plain: false });
            let fx = report(&mut m, &s, Report::Connected { key: key.clone(), pid: pid as u32 + 1 });
            assert!(pid == 1 || reads(&fx), "a game that came up has its memory in use: read again");
        }
        assert!(started(&run(&mut m, &s, Instant::now(), Event::Memory(Some(Memory { free_mb: 2604, ..PC })))).is_empty());
        run(&mut m, &s, Instant::now(), Event::Stop(vec![keys[2].clone()]));
        assert_eq!(m.games[&keys[2]].phase, Phase::Stopped);
        assert!(!m.waits_for_memory(&keys[2]), "a stopped game leaves the line");
        let fx = report(&mut m, &s, Report::Exited { key: keys[0].clone(), code: Some(0) });
        assert!(reads(&fx), "an exit frees memory: read again");
        assert_eq!(started(&run(&mut m, &s, Instant::now(), Event::Memory(Some(Memory { free_mb: 4652, ..PC })))), keys[3..4]);
        let fx = run(&mut m, &s, Instant::now(), Event::MemoryDue);
        assert!(reads(&fx), "the timer reads again while games wait");
    }

    #[test]
    fn a_bulk_start_starts_one_game_even_when_none_fits() {
        let s = settings();
        let (mut m, keys) = farm(3, 4096);
        let batch = m.batches[0].id.clone();
        run(&mut m, &s, Instant::now(), Event::StartBatch(batch));
        let full = Memory { total_mb: 16384, free_mb: 5120 };
        assert_eq!(started(&run(&mut m, &s, Instant::now(), Event::Memory(Some(full)))), keys[..1], "like a single start, the first game goes");
        assert!(started(&run(&mut m, &s, Instant::now(), Event::Memory(Some(full)))).is_empty(), "the rest waits while it is on its way");
        report(&mut m, &s, Report::Launched { key: keys[0].clone(), plain: false });
        report(&mut m, &s, Report::Connected { key: keys[0].clone(), pid: 1 });
        assert!(started(&run(&mut m, &s, Instant::now(), Event::Memory(Some(full)))).is_empty(), "and keeps waiting once it runs");
    }

    #[test]
    fn a_waiting_game_whose_account_is_gone_stops_instead_of_waiting_forever() {
        let s = settings();
        let (mut m, keys) = farm(3, 2048);
        let batch = m.batches[0].id.clone();
        run(&mut m, &s, Instant::now(), Event::StartBatch(batch));
        let gone = m.games[&keys[1]].account_id;
        m.accounts.retain(|a| a.id != gone);
        assert_eq!(started(&run(&mut m, &s, Instant::now(), Event::Memory(None))), [keys[0].clone(), keys[2].clone()]);
        assert_eq!(m.games[&keys[1]].phase, Phase::Stopped);
        assert_eq!(m.run(None), Some(Run { launched: 0, total: 2, memory: 0 }), "the stopped game leaves the run");
    }

    #[test]
    fn cancelling_a_start_stops_what_has_not_launched() {
        let s = settings();
        let (mut m, keys) = farm(4, 2048);
        let batch = m.batches[0].id.clone();
        run(&mut m, &s, Instant::now(), Event::StartBatch(batch.clone()));
        run(&mut m, &s, Instant::now(), Event::Memory(Some(PC)));
        report(&mut m, &s, Report::Launched { key: keys[0].clone(), plain: false });
        let fx = run(&mut m, &s, Instant::now(), Event::CancelStart(Some(batch.clone())));
        assert!(fx.iter().any(|e| matches!(e, Effect::CancelStarts(b) if *b == batch)));
        assert_eq!(m.games[&keys[0]].phase, Phase::Starting { place: 0, total: 0 }, "what launched keeps running");
        assert!(keys[1..].iter().all(|k| m.games[k].phase == Phase::Stopped), "in the fleet's line or waiting for memory: stopped");
        assert_eq!(m.run(None), None);
    }

    #[test]
    fn undoing_a_stop_of_all_starts_the_same_games_in_their_order() {
        let s = settings();
        let (mut m, keys) = farm(4, 2048);
        for (pid, key) in keys.iter().enumerate() {
            m.games.get_mut(key).unwrap().phase = Phase::Menu;
            m.session.pids.insert(key.clone(), pid as u32 + 1);
        }
        m.games.get_mut(&keys[3]).unwrap().phase = Phase::Stopped;
        m.session.pids.remove(&keys[3]);
        let fx = run(&mut m, &s, Instant::now(), Event::StopAll);
        let Some(Effect::Stopped(stopped)) = fx.iter().find(|e| matches!(e, Effect::Stopped(_))) else { panic!("no undo offer") };
        assert_eq!(*stopped, keys[..3], "only what ran is offered back");
        let stopped = stopped.clone();
        assert!(started(&run(&mut m, &s, Instant::now(), Event::StartAgain(stopped))).is_empty(), "not before the old games are down");
        let mut fx = Vec::new();
        for key in &keys[..3] {
            fx = report(&mut m, &s, Report::Exited { key: key.clone(), code: Some(0) });
        }
        assert!(reads(&fx) && started(&fx).is_empty(), "back in the line that waits for memory");
        assert_eq!(started(&run(&mut m, &s, Instant::now(), Event::Memory(Some(Memory { free_mb: 32000, ..PC })))), keys[..3]);
        assert_eq!(m.games[&keys[3]].phase, Phase::Stopped, "a game that was not running stays stopped");
    }
}
