use std::collections::HashMap;
use std::time::Duration;

use super::super::effect::{Effect, Redraw};
use super::super::event::Event;
use super::super::model::{Model, Phase};
use super::Env;

/// How often the memory of the running games is read.
pub const EVERY: Duration = Duration::from_secs(15);
const SETTLED: Duration = Duration::from_secs(60);

/// Keeps the reading timer going while any game runs.
pub(super) fn arm(m: &mut Model, out: &mut Vec<Effect>) {
    if m.session.pids.is_empty() || m.session.usage_timer {
        return;
    }
    m.session.usage_timer = true;
    out.push(Effect::After { delay: EVERY, event: Box::new(Event::UsageDue) });
}

/// The reading is due: asks for the memory of every running game.
pub(super) fn due(m: &mut Model, out: &mut Vec<Effect>) {
    m.session.usage_timer = false;
    if m.session.pids.is_empty() {
        if !m.session.usage.is_empty() {
            m.session.usage.clear();
            out.push(Effect::Redraw(Redraw::All));
        }
        return;
    }
    let mut pids: Vec<(String, u32)> = m.session.pids.iter().map(|(k, p)| (k.clone(), *p)).collect();
    pids.sort_unstable();
    out.push(Effect::ReadUsage(pids));
}

/// What each running game holds now. Games settled on a server teach their batch its figure, kept when it moved by more than 5 %.
pub(super) fn read(m: &mut Model, env: &Env, readings: Vec<(String, u64)>, out: &mut Vec<Effect>) {
    let before = tenths(m);
    m.session.usage = readings.into_iter().filter(|(k, mb)| *mb > 0 && m.session.pids.contains_key(k)).collect();
    let mut per: HashMap<&str, (u64, u64)> = HashMap::new();
    for (key, mb) in &m.session.usage {
        let Some(g) = m.games.get(key) else { continue };
        if matches!(g.phase, Phase::Playing { since, .. } if env.now.saturating_duration_since(since) >= SETTLED) {
            let e = per.entry(g.batch_id.as_str()).or_default();
            *e = (e.0 + mb, e.1 + 1);
        }
    }
    let mut learned = false;
    for (batch, (sum, n)) in per {
        let each = u32::try_from(sum / n).unwrap_or(u32::MAX);
        if m.measured.get(batch).is_none_or(|old| u64::from(each.abs_diff(*old)) * 20 > u64::from(*old)) {
            m.measured.insert(batch.to_owned(), each);
            learned = true;
        }
    }
    if learned {
        out.push(Effect::SaveMeasured(m.measured.clone()));
    }
    if tenths(m) != before {
        out.push(Effect::Redraw(Redraw::All));
    }
}

/// The memory the running games of `batch` (all when `None`) hold now, in MB; 0 when nothing was read.
pub fn held(m: &Model, batch: Option<&str>) -> u64 {
    m.session.usage.iter().filter(|(k, _)| batch.is_none_or(|b| m.games.get(*k).is_some_and(|g| g.batch_id == b))).map(|(_, mb)| mb).sum()
}

fn tenths(m: &Model) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = m.batches.iter().map(|b| (b.id.clone(), held(m, Some(&b.id)) * 10 / 1024)).collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::super::model::Game;
    use super::super::testing::{later, run, settings};
    use super::*;

    fn fleet(now: Instant) -> Model {
        let mut m = Model::default();
        let games = [("a", Phase::Playing { server: "s".into(), since: now - SETTLED }), ("b", Phase::Playing { server: "s".into(), since: now }), ("c", Phase::Menu)];
        for (i, (key, phase)) in games.into_iter().enumerate() {
            m.games.insert(key.into(), Game { phase, ..Game::new(key.into(), "batch".into(), "i".into(), uuid::Uuid::from_u128(i as u128), key.into()) });
            m.session.pids.insert(key.into(), i as u32 + 1);
        }
        m
    }

    #[test]
    fn running_games_are_read_every_few_seconds_until_none_runs() {
        let s = settings();
        let now = Instant::now();
        let mut m = fleet(now);
        let fx = run(&mut m, &s, now, Event::TellGames);
        assert!(later(&fx).iter().any(|(d, e)| *d == EVERY && matches!(e, Event::UsageDue)));
        assert!(!later(&run(&mut m, &s, now, Event::TellGames)).iter().any(|(_, e)| matches!(e, Event::UsageDue)), "one timer at a time");
        let fx = run(&mut m, &s, now, Event::UsageDue);
        assert!(fx.iter().any(|e| matches!(e, Effect::ReadUsage(p) if p.len() == 3)));
        assert!(later(&fx).iter().any(|(_, e)| matches!(e, Event::UsageDue)), "the next reading");
        m.session.pids.clear();
        m.session.usage.insert("a".into(), 1000);
        let fx = run(&mut m, &s, now, Event::UsageDue);
        assert!(m.session.usage.is_empty() && !later(&fx).iter().any(|(_, e)| matches!(e, Event::UsageDue)), "no game, no next reading");
    }

    #[test]
    fn only_games_settled_on_a_server_teach_the_batch_its_figure() {
        let s = settings();
        let now = Instant::now();
        let mut m = fleet(now);
        let fx = run(&mut m, &s, now, Event::Usage(vec![("a".into(), 1400), ("b".into(), 900), ("c".into(), 500), ("gone".into(), 9000)]));
        assert_eq!(m.measured.get("batch"), Some(&1400));
        assert!(fx.iter().any(|e| matches!(e, Effect::SaveMeasured(seen) if seen.get("batch") == Some(&1400))));
        assert_eq!(held(&m, Some("batch")), 2800, "what runs now counts, settled or not");
        let fx = run(&mut m, &s, now, Event::Usage(vec![("a".into(), 1450)]));
        assert_eq!(m.measured.get("batch"), Some(&1400), "a small move is not worth a write");
        assert!(!fx.iter().any(|e| matches!(e, Effect::SaveMeasured(_))));
        run(&mut m, &s, now, Event::Usage(vec![("a".into(), 1600)]));
        assert_eq!(m.measured.get("batch"), Some(&1600));
    }
}
