//! The line games wait in before they start: only a few load at once, so the desktop stays usable and servers and
//! session services see a steady trickle instead of a burst.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The longest a loading game holds its place before the next one may start anyway.
pub const LOAD_CAP: Duration = Duration::from_secs(90);

/// The same for a game without the companion mod, which never reports its title screen.
pub const PLAIN_CAP: Duration = Duration::from_secs(30);

const MOST_AT_ONCE: usize = 8;
const CORES_PER_GAME: usize = 3;
const FREE_MB_PER_GAME: u64 = 2048;

/// How many games may load at once on a machine with `cores` threads and `free_mb` of free memory: one per three
/// threads and per 2 GB, at least one, at most eight.
pub fn auto_parallel(cores: usize, free_mb: Option<u64>) -> usize {
    let by_cores = (cores / CORES_PER_GAME).max(1);
    let by_memory = free_mb.map_or(MOST_AT_ONCE, |mb| usize::try_from(mb / FREE_MB_PER_GAME).unwrap_or(MOST_AT_ONCE).max(1));
    by_cores.min(by_memory).min(MOST_AT_ONCE)
}

struct Loading {
    since: Instant,
    plain: bool,
}

#[derive(Default)]
struct Inner {
    waiting: VecDeque<String>,
    loading: HashMap<String, Loading>,
    last: Option<Instant>,
}

/// Games in the order they asked to start, and the ones among them that are loading right now.
#[derive(Default)]
pub struct Line {
    inner: Mutex<Inner>,
}

impl Line {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts `keys` at the end of the line, in order. A key that already waits keeps its place.
    pub fn join(&self, keys: &[String]) {
        let mut inner = self.inner();
        for key in keys {
            if !inner.waiting.contains(key) {
                inner.waiting.push_back(key.clone());
            }
        }
    }

    /// Takes `key` out of the line, waiting or loading, so it blocks nobody: it was stopped, cancelled or failed.
    pub fn leave(&self, key: &str) {
        let mut inner = self.inner();
        inner.waiting.retain(|k| k != key);
        inner.loading.remove(key);
    }

    /// Takes every key out of the line.
    pub fn clear(&self) {
        let mut inner = self.inner();
        inner.waiting.clear();
        inner.loading.clear();
    }

    /// Reports that `key` reached its title screen, which frees its place.
    pub fn loaded(&self, key: &str) {
        self.inner().loading.remove(key);
    }

    /// Whether `key` may start now; when it may, it holds a place from here on. That is the case when it is first in
    /// line, `pause` passed since the last start and fewer than `limit()` games load. A loading game no longer
    /// holds its place once `LOAD_CAP` passed or `freed(key, plain)` says it is through its heavy part, which
    /// includes a game that ended. `limit` runs only when everything else allows the start.
    pub fn turn(&self, key: &str, plain: bool, now: Instant, pause: Duration, limit: impl FnOnce() -> usize, freed: impl Fn(&str, bool) -> bool) -> bool {
        let mut inner = self.inner();
        if inner.waiting.front().map(String::as_str) != Some(key) {
            return false;
        }
        if inner.last.is_some_and(|last| now.saturating_duration_since(last) < pause) {
            return false;
        }
        inner.loading.retain(|k, l| now.saturating_duration_since(l.since) < if l.plain { PLAIN_CAP } else { LOAD_CAP } && !freed(k, l.plain));
        if inner.loading.len() >= limit().max(1) {
            return false;
        }
        inner.waiting.pop_front();
        inner.loading.insert(key.to_owned(), Loading { since: now, plain });
        inner.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Duration = Duration::ZERO;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    impl Line {
        fn waiting(&self) -> usize {
            self.inner().waiting.len()
        }

        fn loading(&self) -> usize {
            self.inner().loading.len()
        }
    }

    fn turn(line: &Line, key: &str, now: Instant, limit: usize) -> bool {
        line.turn(key, false, now, NONE, || limit, |_, _| false)
    }

    #[test]
    fn auto_follows_cores_and_free_memory() {
        assert_eq!(auto_parallel(2, Some(64_000)), 1);
        assert_eq!(auto_parallel(12, Some(64_000)), 4);
        assert_eq!(auto_parallel(64, Some(256_000)), MOST_AT_ONCE);
        assert_eq!(auto_parallel(16, Some(4096)), 2);
        assert_eq!(auto_parallel(16, Some(500)), 1);
        assert_eq!(auto_parallel(12, None), 4);
        assert_eq!(auto_parallel(0, None), 1);
    }

    #[test]
    fn games_start_in_the_order_they_joined() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b", "c"]));
        assert!(!turn(&line, "b", now, 8));
        assert!(turn(&line, "a", now, 8));
        assert!(!turn(&line, "c", now, 8));
        assert!(turn(&line, "b", now, 8));
        assert!(turn(&line, "c", now, 8));
        assert_eq!((line.waiting(), line.loading()), (0, 3));
    }

    #[test]
    fn only_the_limit_loads_and_a_loaded_game_makes_room() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b", "c"]));
        assert!(turn(&line, "a", now, 2));
        assert!(turn(&line, "b", now, 2));
        assert!(!turn(&line, "c", now, 2));
        line.loaded("a");
        assert!(turn(&line, "c", now, 2));
        assert_eq!((line.waiting(), line.loading()), (0, 2));
    }

    #[test]
    fn the_pause_between_starts_holds_the_next_one_back() {
        let line = Line::default();
        let t0 = Instant::now();
        let pause = Duration::from_secs(3);
        line.join(&keys(&["a", "b"]));
        assert!(line.turn("a", false, t0, pause, || 8, |_, _| false));
        assert!(!line.turn("b", false, t0 + Duration::from_secs(2), pause, || 8, |_, _| false));
        assert!(line.turn("b", false, t0 + Duration::from_secs(3), pause, || 8, |_, _| false));
    }

    #[test]
    fn a_game_that_never_loads_frees_its_place_after_the_cap() {
        let line = Line::default();
        let t0 = Instant::now();
        line.join(&keys(&["a", "b"]));
        assert!(turn(&line, "a", t0, 1));
        assert!(!turn(&line, "b", t0 + LOAD_CAP - Duration::from_secs(1), 1));
        assert!(turn(&line, "b", t0 + LOAD_CAP, 1));
    }

    #[test]
    fn a_plain_game_frees_its_place_after_the_shorter_cap() {
        let line = Line::default();
        let t0 = Instant::now();
        line.join(&keys(&["a", "b"]));
        assert!(line.turn("a", true, t0, NONE, || 1, |_, _| false));
        assert!(!line.turn("b", true, t0 + PLAIN_CAP - Duration::from_secs(1), NONE, || 1, |_, _| false));
        assert!(line.turn("b", true, t0 + PLAIN_CAP, NONE, || 1, |_, _| false));
    }

    #[test]
    fn a_crashed_game_frees_its_place_at_once() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b"]));
        assert!(turn(&line, "a", now, 1));
        assert!(!line.turn("b", false, now, NONE, || 1, |k, _| k != "a"));
        assert!(line.turn("b", false, now, NONE, || 1, |k, _| k == "a"));
    }

    #[test]
    fn a_plain_game_is_judged_as_plain() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b"]));
        assert!(line.turn("a", true, now, NONE, || 1, |_, _| false));
        assert!(!line.turn("b", true, now, NONE, || 1, |_, plain| !plain));
        assert!(line.turn("b", true, now, NONE, || 1, |_, plain| plain));
    }

    #[test]
    fn leaving_while_waiting_unblocks_those_behind() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b"]));
        line.leave("a");
        assert!(turn(&line, "b", now, 1));
        assert_eq!(line.waiting(), 0);
    }

    #[test]
    fn leaving_while_loading_frees_the_place() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b"]));
        assert!(turn(&line, "a", now, 1));
        assert!(!turn(&line, "b", now, 1));
        line.leave("a");
        assert!(turn(&line, "b", now, 1));
    }

    #[test]
    fn clearing_stops_everything_at_once() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b", "c"]));
        assert!(turn(&line, "a", now, 1));
        line.clear();
        assert_eq!((line.waiting(), line.loading()), (0, 0));
        assert!(!turn(&line, "b", now, 1));
    }

    #[test]
    fn joining_twice_keeps_the_first_place() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b"]));
        line.join(&keys(&["b", "c"]));
        assert_eq!(line.waiting(), 3);
        assert!(turn(&line, "a", now, 8));
        assert!(turn(&line, "b", now, 8));
        assert!(turn(&line, "c", now, 8));
    }

    #[test]
    fn the_limit_is_asked_only_when_nothing_else_holds_the_game_back() {
        let line = Line::default();
        let now = Instant::now();
        line.join(&keys(&["a", "b"]));
        let asked = std::cell::Cell::new(0);
        let ask = || {
            asked.set(asked.get() + 1);
            8
        };
        assert!(!line.turn("b", false, now, NONE, ask, |_, _| false));
        assert_eq!(asked.get(), 0);
        assert!(line.turn("a", false, now, NONE, ask, |_, _| false));
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_limit_of_zero_still_lets_one_game_load() {
        let line = Line::default();
        line.join(&keys(&["a"]));
        assert!(turn(&line, "a", Instant::now(), 0));
    }
}
