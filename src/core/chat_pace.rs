//! Keeps one account under the server's chat spam limit: the server adds 20 to a counter per chat line or command, takes
//! 1 per tick off it and kicks the player above 200. Lines wait here in order, never dropped or reordered.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const COST: Duration = Duration::from_millis(1350);
const BURST: u32 = 6;

/// How much a player may send now: a burst of six lines, then one per 1.35 seconds. The server's own count falls by 20
/// per second and grows by 20 per line, so this keeps under its limit even when it ticks at 15 per second.
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    load: Duration,
    at: Instant,
}

impl Pace {
    pub fn new(now: Instant) -> Self {
        Self { load: Duration::ZERO, at: now }
    }

    fn settled(&self, now: Instant) -> Duration {
        self.load.saturating_sub(now.saturating_duration_since(self.at))
    }

    /// How long until one more line may go out; zero when it may go now.
    pub fn wait(&self, now: Instant) -> Duration {
        (self.settled(now) + COST).saturating_sub(COST * BURST)
    }

    /// Notes that a line went out at `now`.
    pub fn spend(&mut self, now: Instant) {
        self.load = self.settled(now) + COST;
        self.at = now;
    }
}

/// What a [`Queue`] has for the caller.
#[derive(Debug, PartialEq, Eq)]
pub enum Next<T> {
    Ready(T),
    Wait(Duration),
    Empty,
}

/// Lines waiting to go out in the order they were typed.
#[derive(Debug)]
pub struct Queue<T> {
    lines: VecDeque<T>,
    pace: Pace,
}

impl<T> Queue<T> {
    pub fn new(now: Instant) -> Self {
        Self { lines: VecDeque::new(), pace: Pace::new(now) }
    }

    pub fn push(&mut self, line: T) {
        self.lines.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The next line when its time has come, else how long to wait for it.
    pub fn next(&mut self, now: Instant) -> Next<T> {
        if self.lines.is_empty() {
            return Next::Empty;
        }
        let wait = self.pace.wait(now);
        if !wait.is_zero() {
            return Next::Wait(wait);
        }
        self.pace.spend(now);
        self.lines.pop_front().map_or(Next::Empty, Next::Ready)
    }

    /// Takes every line that has not gone out.
    pub fn drain(&mut self) -> Vec<T> {
        self.lines.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_then_a_steady_pace_in_order() {
        let t0 = Instant::now();
        let mut q = Queue::new(t0);
        for i in 0..10 {
            q.push(i);
        }
        let mut out = Vec::new();
        while let Next::Ready(i) = q.next(t0) {
            out.push(i);
        }
        assert_eq!(out, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(q.next(t0), Next::Wait(COST));
        assert_eq!(q.next(t0 + Duration::from_millis(400)), Next::Wait(COST - Duration::from_millis(400)));
        assert_eq!(q.next(t0 + COST), Next::Ready(6));
        assert_eq!(q.next(t0 + COST), Next::Wait(COST));
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn rests_refill_the_burst() {
        let t0 = Instant::now();
        let mut q = Queue::new(t0);
        for i in 0..12 {
            q.push(i);
        }
        let mut now = t0;
        let mut sent = 0;
        while let Next::Ready(_) = q.next(now) {
            sent += 1;
        }
        assert_eq!(sent, 6);
        now += Duration::from_secs(60);
        while let Next::Ready(_) = q.next(now) {
            sent += 1;
        }
        assert_eq!(sent, 12);
        assert!(q.is_empty());
    }

    #[test]
    fn never_exceeds_the_servers_limit() {
        let t0 = Instant::now();
        let mut q = Queue::new(t0);
        for i in 0..200 {
            q.push(i);
        }
        let mut load: i64 = 0;
        for tick in 0..8000u64 {
            let now = t0 + Duration::from_millis(tick * 50);
            load = (load - 1).max(0);
            while let Next::Ready(_) = q.next(now) {
                load += 20;
                assert!(load <= 200, "{load} at tick {tick}");
            }
        }
        assert!(q.is_empty());
    }
}