//! How long each instance was played: time with a running game, kept between runs. Time since the last write is the most a crash can cost.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::i18n::trf;
use super::{net, paths};

const FILE: &str = "playtime.json";

/// Seconds played by instance id; empty when nothing was played yet or the file cannot be read.
pub fn load() -> HashMap<String, u64> {
    load_from(&paths::data_dir())
}

/// Keeps `seen` for the next run.
pub fn save(seen: &HashMap<String, u64>) -> Result<()> {
    save_to(&paths::data_dir(), seen)
}

fn load_from(dir: &Path) -> HashMap<String, u64> {
    std::fs::read(dir.join(FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn save_to(dir: &Path, seen: &HashMap<String, u64>) -> Result<()> {
    net::write_atomic(&dir.join(FILE), &serde_json::to_vec_pretty(seen)?)
}

/// The play time of every instance: what was written down, plus the stretch of each instance with a game up right now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playtime {
    done: HashMap<String, Duration>,
    open: HashMap<String, Instant>,
}

impl Playtime {
    /// Takes what an earlier run wrote down, unless this run already counted more.
    pub fn restore(&mut self, seen: HashMap<String, u64>) {
        for (id, secs) in seen {
            let known = self.done.entry(id).or_default();
            *known = (*known).max(Duration::from_secs(secs));
        }
    }

    /// Whether any instance has a game up, so the clock has to be written down now and then.
    pub fn running(&self) -> bool {
        !self.open.is_empty()
    }

    /// Makes exactly the instances in `up` count as played from `now`; those that stopped are written into the total.
    /// Returns whether one stopped, which is worth saving.
    pub fn sync(&mut self, up: &HashSet<String>, now: Instant) -> bool {
        let ended: Vec<String> = self.open.keys().filter(|id| !up.contains(*id)).cloned().collect();
        for id in &ended {
            if let Some(since) = self.open.remove(id) {
                *self.done.entry(id.clone()).or_default() += now.saturating_duration_since(since);
            }
        }
        for id in up {
            self.open.entry(id.clone()).or_insert(now);
        }
        !ended.is_empty()
    }

    /// Writes the running stretches into the total, so saving now loses nothing a crash would take.
    pub fn flush(&mut self, now: Instant) {
        for (id, since) in &mut self.open {
            *self.done.entry(id.clone()).or_default() += now.saturating_duration_since(*since);
            *since = now;
        }
    }

    /// What to write to disk: whole seconds by instance id, without the running stretches since the last flush.
    pub fn written(&self) -> HashMap<String, u64> {
        self.done.iter().map(|(id, d)| (id.clone(), d.as_secs())).filter(|(_, s)| *s > 0).collect()
    }

    /// The time `id` was played at `now`.
    pub fn of(&self, id: &str, now: Instant) -> Duration {
        self.done.get(id).copied().unwrap_or_default() + self.open.get(id).map_or(Duration::ZERO, |s| now.saturating_duration_since(*s))
    }

    /// The time every instance was played at `now`.
    pub fn total(&self, now: Instant) -> Duration {
        self.done.values().sum::<Duration>() + self.open.values().map(|s| now.saturating_duration_since(*s)).sum::<Duration>()
    }
}

/// "35 Min." under an hour, "2 Std. 5 Min." under ten hours, else "12 Std."; nothing in the first minute.
pub fn short(played: Duration) -> Option<String> {
    let minutes = played.as_secs() / 60;
    Some(match (minutes / 60, minutes % 60) {
        (0, 0) => return None,
        (0, m) => trf("%lld Min.", &[&m]),
        (h, m) if h < 10 && m > 0 => trf("%1$lld Std. %2$lld Min.", &[&h, &m]),
        (h, _) => trf("%lld Std.", &[&h]),
    })
}

/// "12 Std. gespielt"; nothing in the first minute.
pub fn played_text(played: Duration) -> Option<String> {
    short(played).map(|t| trf("%@ gespielt", &[&t]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_game_counts_from_its_start_until_it_stops() {
        let t0 = Instant::now();
        let mut p = Playtime::default();
        assert!(!p.sync(&up(&["a"]), t0));
        assert_eq!(p.of("a", t0 + secs(90)), secs(90));
        assert!(p.sync(&up(&[]), t0 + secs(600)));
        assert_eq!(p.of("a", t0 + secs(9000)), secs(600));
        assert!(!p.running());
        assert_eq!(p.written().get("a"), Some(&600));
    }

    #[test]
    fn several_games_of_one_instance_count_once_and_instances_count_apart() {
        let t0 = Instant::now();
        let mut p = Playtime::default();
        p.sync(&up(&["a"]), t0);
        p.sync(&up(&["a", "b"]), t0 + secs(60));
        p.sync(&up(&["a", "b"]), t0 + secs(120));
        p.sync(&up(&["b"]), t0 + secs(300));
        let at = t0 + secs(400);
        assert_eq!(p.of("a", at), secs(300));
        assert_eq!(p.of("b", at), secs(340));
        assert_eq!(p.total(at), secs(640));
    }

    #[test]
    fn a_restart_keeps_what_was_written_and_adds_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        let mut p = Playtime::default();
        p.sync(&up(&["a"]), t0);
        p.sync(&up(&[]), t0 + secs(3700));
        save_to(dir.path(), &p.written()).unwrap();

        let mut next = Playtime::default();
        next.restore(load_from(dir.path()));
        next.sync(&up(&["a"]), t0);
        assert_eq!(next.of("a", t0 + secs(100)), secs(3800));
    }

    #[test]
    fn a_crash_loses_only_what_ran_since_the_last_flush() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        let mut p = Playtime::default();
        p.sync(&up(&["a"]), t0);
        p.flush(t0 + secs(60));
        save_to(dir.path(), &p.written()).unwrap();
        p.flush(t0 + secs(120));
        assert_eq!(p.of("a", t0 + secs(150)), secs(150), "the clock keeps running across a flush");

        let mut after_crash = Playtime::default();
        after_crash.restore(load_from(dir.path()));
        assert_eq!(after_crash.of("a", t0 + secs(500)), secs(60));
    }

    #[test]
    fn the_written_file_leaves_out_unplayed_instances_and_a_damaged_one_counts_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_from(dir.path()).is_empty());
        let t0 = Instant::now();
        let mut p = Playtime::default();
        p.sync(&up(&["a"]), t0);
        p.sync(&up(&[]), t0 + Duration::from_millis(400));
        assert!(p.written().is_empty());
        std::fs::write(dir.path().join(FILE), b"{oops").unwrap();
        assert!(load_from(dir.path()).is_empty());
    }

    #[test]
    fn the_text_gives_minutes_under_an_hour_and_hours_above() {
        assert_eq!(short(secs(59)), None);
        assert_eq!(short(secs(35 * 60 + 20)), Some("35 Min.".to_owned()));
        assert_eq!(short(secs(2 * 3600 + 5 * 60)), Some("2 Std. 5 Min.".to_owned()));
        assert_eq!(short(secs(12 * 3600 + 40 * 60)), Some("12 Std.".to_owned()));
        assert_eq!(short(secs(3600)), Some("1 Std.".to_owned()));
        assert_eq!(played_text(secs(12 * 3600)), Some("12 Std. gespielt".to_owned()));
        assert_eq!(played_text(secs(10)), None);
    }
}
