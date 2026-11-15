//! Waiting out a service that answered "too many requests": every request waits, doubling per answer in a row.

use std::time::{Duration, Instant};

use super::reconnect;

const SCHEDULE: [u64; 6] = [2, 4, 8, 16, 32, 60];
const JITTER_PERCENT: u32 = 25;
const LONGEST: Duration = Duration::from_secs(120);

/// When requests to a service may go out again after it said "too many".
#[derive(Default)]
pub struct Cooldown {
    until: Option<Instant>,
    strikes: u32,
}

impl Cooldown {
    pub const fn new() -> Self {
        Self { until: None, strikes: 0 }
    }

    /// How long a request must still wait at `now`.
    pub fn wait(&self, now: Instant) -> Duration {
        self.until.map_or(Duration::ZERO, |until| until.saturating_duration_since(now))
    }

    /// Records a "too many" answer at `now` and returns how long everything waits: what the service asked for, at most two
    /// minutes, else the next step of the schedule spread by `unit` in `[0, 1)`.
    pub fn limited(&mut self, now: Instant, asked: Option<Duration>, unit: f64) -> Duration {
        let delay = asked.map_or_else(|| reconnect::wait(&SCHEDULE, self.strikes, JITTER_PERCENT, unit), |d| d.min(LONGEST));
        self.strikes = self.strikes.saturating_add(1);
        let until = now + delay;
        self.until = Some(self.until.map_or(until, |old| old.max(until)));
        delay
    }

    /// Records a normal answer: the next "too many" starts from the shortest wait again.
    pub fn answered(&mut self) {
        self.strikes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_double_per_answer_and_start_over_after_a_normal_one() {
        let now = Instant::now();
        let mut cooldown = Cooldown::new();
        assert_eq!(cooldown.wait(now), Duration::ZERO);
        let secs = |d: Duration| d.as_secs_f64().round() as u64;
        assert_eq!(secs(cooldown.limited(now, None, 0.5)), 2);
        assert_eq!(secs(cooldown.limited(now, None, 0.5)), 4);
        assert_eq!(secs(cooldown.limited(now, None, 0.5)), 8);
        cooldown.answered();
        assert_eq!(secs(cooldown.limited(now, None, 0.5)), 2);
    }

    #[test]
    fn the_schedule_ends_at_a_minute() {
        let now = Instant::now();
        let mut cooldown = Cooldown::new();
        let last = (0..20).map(|_| cooldown.limited(now, None, 0.5)).last().unwrap();
        assert_eq!(last, Duration::from_secs(60));
    }

    #[test]
    fn what_the_service_asks_for_wins_up_to_two_minutes() {
        let now = Instant::now();
        let mut cooldown = Cooldown::new();
        assert_eq!(cooldown.limited(now, Some(Duration::from_secs(7)), 0.5), Duration::from_secs(7));
        assert_eq!(cooldown.limited(now, Some(Duration::from_secs(900)), 0.5), LONGEST);
    }

    #[test]
    fn every_request_waits_until_the_latest_end() {
        let now = Instant::now();
        let mut cooldown = Cooldown::new();
        cooldown.limited(now, Some(Duration::from_secs(30)), 0.5);
        cooldown.limited(now, Some(Duration::from_secs(5)), 0.5);
        assert_eq!(cooldown.wait(now), Duration::from_secs(30));
        assert_eq!(cooldown.wait(now + Duration::from_secs(12)), Duration::from_secs(18));
        assert_eq!(cooldown.wait(now + Duration::from_secs(31)), Duration::ZERO);
    }

    #[test]
    fn jitter_stays_inside_its_band() {
        let now = Instant::now();
        for unit in [0.0, 0.5, 0.999] {
            let wait = Cooldown::new().limited(now, None, unit).as_secs_f64();
            assert!((1.5..=2.5).contains(&wait), "{wait}");
        }
    }
}
