use std::collections::HashSet;

use super::super::effect::{Effect, Redraw};
use super::super::event::Loaded;
use super::super::model::{Game, Model, Phase};
use super::super::phase::Why;
use super::{Env, games, go, show};
use crate::core::fleet::{self, Adoption};

pub(super) fn loaded(m: &mut Model, env: &Env, loaded: Loaded, out: &mut Vec<Effect>) {
    let Loaded { data, logs, problems, recovered, keep, adopted } = loaded;
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
