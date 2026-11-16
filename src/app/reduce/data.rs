use std::collections::HashSet;
use std::time::Duration;

use super::super::effect::{Effect, Redraw};
use super::super::event::Loaded;
use super::super::model::{Game, Model, Phase};
use super::super::phase::Why;
use super::{Env, fleet as launching, games, go, show};
use crate::core::fleet::{self, Adoption};

const START_WAITS_FOR: Duration = Duration::from_secs(10);

pub(super) fn loaded(m: &mut Model, env: &Env, loaded: Loaded, out: &mut Vec<Effect>) {
    let Loaded { data, logs, problems, recovered, keep, adopted, measured } = loaded;
    for (batch, mb) in measured {
        m.measured.entry(batch).or_insert(mb);
    }
    if let Some(text) = recovered {
        let s = &mut m.session;
        s.notice = Some(s.notice.take().map_or(text.clone(), |before| format!("{before} {text}")));
    }
    let notice = if data.is_ok() { m.session.notice.take() } else { None };
    let notice = if problems.is_empty() { notice } else { Some(notice.into_iter().fold(problems.join("\n"), |p, n| format!("{p}\n{n}"))) };
    for (key, log) in logs {
        m.history.entry(key).or_insert(log);
    }
    match data {
        Ok((accounts, batches, instances)) => {
            m.accounts = accounts;
            m.batches = batches;
            m.instances = instances;
            m.loaded = true;
            m.error = if keep { notice.or(m.error.take()) } else { notice };
        }
        Err(e) => m.error = Some(e),
    }
    rebuild(m);
    out.push(Effect::Redraw(Redraw::All));
    out.push(Effect::Retain);
    if m.loaded && !adopted.is_empty() {
        adopt(m, env, adopted, out);
    }
    let known: Vec<(String, String)> = m.games.values().map(|i| (i.key.clone(), i.name.clone())).collect();
    let first = !m.session.refreshed;
    let to_check = m.accounts.iter().filter(|a| first || m.games.values().any(|i| i.account_id == a.id && i.phase == Phase::LoginNeeded)).cloned().collect();
    let bare = m.games.values().filter(|i| !m.frames.contains_key(&i.key)).map(|i| i.key.clone()).collect();
    out.push(Effect::LoadStills(bare));
    out.push(Effect::Known(known));
    out.push(Effect::Refresh(to_check));
    m.session.refreshed = true;
    games::tell_games(m, env, out);
    if m.loaded {
        for (instance, account) in std::mem::take(&mut m.session.waiting_plays) {
            show::request_play(m, env, instance, account, out);
        }
    }
    for (batch, since) in std::mem::take(&mut m.session.waiting_starts) {
        if env.now.saturating_duration_since(since) > START_WAITS_FOR {
            continue;
        }
        if super::startable(m, &batch) {
            launching::launch(m, &batch, None, launching::Kick::Plain, out);
        } else {
            m.session.waiting_starts.push((batch, since));
        }
    }
}

/// Makes the games match the batches and accounts, keeping what runs and what was played alone.
fn rebuild(m: &mut Model) {
    let mut keep: HashSet<String> = HashSet::new();
    for batch in &m.batches {
        for id in &batch.accounts {
            let Some(account) = m.accounts.iter().find(|a| a.id == *id) else { continue };
            let key = fleet::instance_key(batch, *id);
            keep.insert(key.clone());
            let game = m.games.entry(key.clone()).or_insert_with(|| Game::new(key, batch.id.clone(), batch.instance_id.clone(), *id, account.name.clone()));
            game.name = account.name.clone();
            if !game.phase.is_running() {
                game.instance_id.clone_from(&batch.instance_id);
            }
        }
    }
    let instances = &m.instances;
    let accounts = &m.accounts;
    m.games.retain(|k, i| {
        let solo = i.batch_id == fleet::solo_owner(&i.instance_id) && instances.iter().any(|made| made.id == i.instance_id) && accounts.iter().any(|a| a.id == i.account_id);
        keep.contains(k) || i.phase.is_running() || (solo && !matches!(i.phase, Phase::Stopped))
    });
    let games = &m.games;
    m.history.retain(|k, _| games.contains_key(k));
    m.frames.retain(|k, _| games.contains_key(k));
    m.order_seq += 1;
}

/// Takes over the games an earlier manager launched that still run; those without a tile are let go.
fn adopt(m: &mut Model, env: &Env, adopted: Vec<Adoption>, out: &mut Vec<Effect>) {
    let (mut placed, mut dropped) = (Vec::new(), Vec::new());
    for a in adopted {
        if a.batch.is_none() && !m.games.contains_key(&a.key) {
            let Some(name) = m.account(a.account).map(|account| account.name.clone()) else {
                dropped.push(a.key);
                continue;
            };
            m.games.insert(a.key.clone(), Game::new(a.key.clone(), fleet::solo_owner(&a.instance), a.instance.clone(), a.account, name));
        }
        let Some(i) = m.games.get_mut(&a.key) else {
            dropped.push(a.key);
            continue;
        };
        i.hint.link_lost = true;
        let s = &mut m.session;
        s.pids.insert(a.key.clone(), a.pid);
        s.joined.insert(a.key.clone());
        s.started.insert(a.key.clone(), env.now);
        s.adopted.insert(a.key.clone());
        s.foreign.insert(a.key.clone());
        if env.place_windows {
            s.unplaced.insert(a.key.clone());
        }
        go(m, &a.key, Phase::Menu, Why::Adopt, out);
        placed.push(a.key);
    }
    out.push(Effect::Settle { placed, dropped });
    m.order_seq += 1;
    out.push(Effect::Redraw(Redraw::All));
}

#[cfg(test)]
mod tests {
    use super::super::super::effect::Effect;
    use super::super::super::model::Phase;
    use super::super::testing::*;
    use crate::core::account::AccountKind;
    use crate::core::fleet::Adoption;

    #[test]
    fn a_batch_started_before_a_load_knows_it_starts_with_the_load() {
        let s = settings();
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let batch = m.batches.remove(0);
        m.games.clear();
        let now = std::time::Instant::now();
        let fx = run(&mut m, &s, now, super::super::super::event::Event::StartBatch(batch.id.clone()));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Start { .. })));
        m.batches.push(batch.clone());
        let fx = adopt(&mut m, &s, Vec::new());
        assert!(fx.iter().any(|e| matches!(e, Effect::Start { owner, .. } if *owner == batch.id)), "starts once the load knows the batch");
        assert!(m.session.waiting_starts.is_empty());
        let fx = adopt(&mut m, &s, Vec::new());
        assert!(!fx.iter().any(|e| matches!(e, Effect::Start { .. })), "only once");
    }

    #[test]
    fn a_batch_known_before_its_games_waits_for_them_and_a_solo_start_runs_at_once() {
        let s = settings();
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let batch = m.batches[0].id.clone();
        let games = std::mem::take(&mut m.games);
        let now = std::time::Instant::now();
        let fx = run(&mut m, &s, now, super::super::super::event::Event::StartBatch(batch.clone()));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Start { .. })));
        assert_eq!(m.session.waiting_starts.len(), 1, "waits for the games the next load builds");
        m.games = games;
        m.session.waiting_starts.clear();
        let solo = crate::core::fleet::solo_owner(&m.batches[0].instance_id);
        let key = m.games.keys().next().cloned().unwrap();
        m.batches.clear();
        let mut game = m.games.remove(&key).unwrap();
        m.games.clear();
        game.batch_id = solo.clone();
        m.games.insert(key, game.clone());
        game.key = m.target(&solo).unwrap().key(game.account_id);
        m.games.clear();
        m.games.insert(game.key.clone(), game);
        let fx = run(&mut m, &s, now, super::super::super::event::Event::StartBatch(solo.clone()));
        assert!(m.session.waiting_starts.is_empty(), "a solo owner is never queued");
        assert!(fx.iter().any(|e| matches!(e, Effect::Start { owner, .. } if *owner == solo)));
    }

    #[test]
    fn a_start_for_a_batch_that_never_shows_up_is_dropped_after_a_while() {
        let s = settings();
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let long_ago = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(11)).unwrap();
        m.session.waiting_starts.push(("missing".into(), std::time::Instant::now()));
        adopt(&mut m, &s, Vec::new());
        assert_eq!(m.session.waiting_starts.len(), 1, "kept while it may still come");
        m.session.waiting_starts = vec![("missing".into(), long_ago)];
        adopt(&mut m, &s, Vec::new());
        assert!(m.session.waiting_starts.is_empty());
    }

    #[test]
    fn adopted_games_without_a_tile_are_let_go_and_the_rest_may_talk() {
        let s = settings();
        let (mut m, key) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let game = &m.games[&key];
        let (batch, instance, account) = (game.batch_id.clone(), game.instance_id.clone(), game.account_id);
        let gone = Adoption { key: "removed-member".into(), batch: Some(batch.clone()), instance: instance.clone(), account, pid: 2 };
        let stranger = Adoption { key: "solo-of-deleted-account".into(), batch: None, instance: instance.clone(), account: uuid::Uuid::from_u128(99), pid: 3 };
        let fx = adopt(&mut m, &s, vec![Adoption { key: key.clone(), batch: Some(batch), instance, account, pid: 1 }, gone, stranger]);
        let Some(Effect::Settle { placed, dropped }) = fx.iter().find(|e| matches!(e, Effect::Settle { .. })) else { panic!("no settle") };
        assert_eq!(placed, &[key.clone()]);
        assert_eq!(dropped, &["removed-member", "solo-of-deleted-account"]);
        assert!(!m.games.contains_key("removed-member") && !m.session.pids.contains_key("removed-member"));
    }
}
