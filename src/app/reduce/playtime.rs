use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::super::effect::Effect;
use super::super::event::Event;
use super::super::model::{Model, Phase};

/// How often the play time of the running games is written down: a crash costs at most this much.
pub const EVERY: Duration = Duration::from_secs(60);

/// Whether a game in `phase` counts as played: its process runs and got past the start.
pub(super) fn counts(phase: &Phase) -> bool {
    phase.is_running() && !matches!(phase, Phase::Starting { .. })
}

fn up(m: &Model) -> HashSet<String> {
    m.games.values().filter(|g| counts(&g.phase)).map(|g| g.instance_id.clone()).collect()
}

fn save(m: &Model, wait: bool) -> Effect {
    Effect::SavePlaytime { seen: m.playtime.written(), wait }
}

/// After an event: starts and stops the clocks of instances whose games came up or ended, arms the timer that writes the
/// time down while any runs, and writes it down before the manager quits.
pub(super) fn settle(m: &mut Model, now: Instant, out: &mut Vec<Effect>) {
    if std::mem::take(&mut m.session.playtime_dirty) {
        let up = up(m);
        if m.playtime.sync(&up, now) {
            out.push(save(m, false));
        }
    }
    if let Some(at) = out.iter().position(|e| matches!(e, Effect::Quit { .. })) {
        m.playtime.flush(now);
        out.insert(at, save(m, true));
    }
    if m.playtime.running() && !m.session.playtime_timer {
        m.session.playtime_timer = true;
        out.push(Effect::After { delay: EVERY, event: Box::new(Event::PlaytimeDue) });
    }
}

/// The write is due: counts what runs now, writes the clocks down and keeps the timer going while any runs.
pub(super) fn due(m: &mut Model, now: Instant, out: &mut Vec<Effect>) {
    m.session.playtime_timer = false;
    let up = up(m);
    m.playtime.sync(&up, now);
    m.playtime.flush(now);
    out.push(save(m, false));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::super::event::Report;
    use super::super::testing::{later, run, settings, setup};
    use super::*;
    use crate::core::account::AccountKind;

    fn saved(fx: &[Effect]) -> Option<(HashMap<String, u64>, bool)> {
        fx.iter().rev().find_map(|e| if let Effect::SavePlaytime { seen, wait } = e { Some((seen.clone(), *wait)) } else { None })
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_game_counts_once_it_is_past_the_start_and_is_written_down_when_it_ends() {
        let s = settings();
        let t0 = Instant::now();
        let (mut m, key) = setup(Phase::Starting { place: 0, total: 1 }, AccountKind::Offline, &[]);
        let id = m.games[&key].instance_id.clone();
        run(&mut m, &s, t0, Event::TellGames);
        assert!(!m.playtime.running(), "a game that is still starting does not count");

        let fx = run(&mut m, &s, t0 + secs(5), Event::Reports(vec![Report::Connected { key: key.clone(), pid: 1 }]));
        assert_eq!(m.games[&key].phase, Phase::Menu);
        assert!(m.playtime.running());
        assert!(later(&fx).iter().any(|(d, e)| *d == EVERY && matches!(e, Event::PlaytimeDue)));
        assert_eq!(m.playtime.of(&id, t0 + secs(65)), secs(60));

        let fx = run(&mut m, &s, t0 + secs(605), Event::Reports(vec![Report::Exited { key: key.clone(), code: Some(0) }]));
        assert!(!m.playtime.running());
        assert_eq!(saved(&fx), Some((HashMap::from([(id.clone(), 600)]), false)));
    }

    #[test]
    fn a_crash_ends_the_clock_too() {
        let s = settings();
        let t0 = Instant::now();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let id = m.games[&key].instance_id.clone();
        m.session.playtime_dirty = true;
        run(&mut m, &s, t0, Event::TellGames);
        assert!(m.playtime.running());
        let fx = run(&mut m, &s, t0 + secs(120), Event::Reports(vec![Report::Exited { key, code: Some(1) }]));
        assert_eq!(saved(&fx).map(|(seen, _)| seen.get(&id).copied()), Some(Some(120)));
    }

    #[test]
    fn the_timer_flushes_each_minute_and_stops_when_no_game_runs() {
        let s = settings();
        let t0 = Instant::now();
        let (mut m, key) = setup(Phase::Starting { place: 0, total: 1 }, AccountKind::Offline, &[]);
        let id = m.games[&key].instance_id.clone();
        run(&mut m, &s, t0, Event::Reports(vec![Report::Connected { key, pid: 1 }]));
        let fx = run(&mut m, &s, t0 + EVERY, Event::PlaytimeDue);
        assert_eq!(saved(&fx), Some((HashMap::from([(id.clone(), 60)]), false)), "a crash now would lose nothing of the first minute");
        assert!(later(&fx).iter().any(|(_, e)| matches!(e, Event::PlaytimeDue)), "the next write");
        let fx = run(&mut m, &s, t0 + EVERY * 2, Event::PlaytimeDue);
        assert_eq!(saved(&fx).map(|(seen, _)| seen[&id]), Some(120));
    }

    #[test]
    fn quitting_writes_the_time_down_first_and_waits_for_it() {
        let s = settings();
        let t0 = Instant::now();
        let (mut m, key) = setup(Phase::Starting { place: 0, total: 1 }, AccountKind::Offline, &[]);
        let id = m.games[&key].instance_id.clone();
        run(&mut m, &s, t0, Event::Reports(vec![Report::Connected { key, pid: 1 }]));
        let fx = run(&mut m, &s, t0 + secs(90), Event::QuitChosen { keep: true });
        let save = fx.iter().position(|e| matches!(e, Effect::SavePlaytime { wait: true, .. })).expect("saved");
        let quit = fx.iter().position(|e| matches!(e, Effect::Quit { .. })).expect("quits");
        assert!(save < quit);
        assert_eq!(saved(&fx).map(|(seen, _)| seen[&id]), Some(90));
    }

    #[test]
    fn a_restart_adds_to_what_the_last_run_wrote() {
        let s = settings();
        let t0 = Instant::now();
        let (mut m, key) = setup(Phase::Starting { place: 0, total: 1 }, AccountKind::Offline, &[]);
        let id = m.games[&key].instance_id.clone();
        m.playtime.restore(HashMap::from([(id.clone(), 3600)]));
        run(&mut m, &s, t0, Event::Reports(vec![Report::Connected { key, pid: 1 }]));
        assert_eq!(m.playtime.of(&id, t0 + secs(30)), secs(3630));
    }
}
