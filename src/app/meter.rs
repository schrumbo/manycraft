//! How far a piece of work is: the step it is on, a count or byte figure and, once the pace is steady, how long is left.
//! Pure state with the words that go with it; the views draw it.

use std::time::{Duration, Instant};

use crate::core::i18n::{decimal, trf};
use crate::core::install::Stage;

/// Seconds of work before a bar without a figure names how long it has run.
const SHOW_ELAPSED: Duration = Duration::from_secs(2);
/// Seconds a step must run before an estimate is worth showing.
const SETTLE: Duration = Duration::from_secs(3);
/// Estimates under this share done or over [SURE] are too rough to show.
const EARLY: f32 = 0.02;
const SURE: f32 = 0.98;
const LONGEST: Duration = Duration::from_secs(24 * 60 * 60);
/// What a figure counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Count,
    Bytes,
}

/// How far a piece of work is: the step it is on, and what that step has done of what it has to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Meter {
    step: String,
    done: u64,
    total: u64,
    unit: Unit,
    born: Instant,
    step_born: Instant,
    step_first: u64,
}

impl Meter {
    /// Work that began at `now` on `step`, with nothing measurable yet.
    pub fn begin(step: impl Into<String>, now: Instant) -> Self {
        Meter { step: step.into(), done: 0, total: 0, unit: Unit::Count, born: now, step_born: now, step_first: 0 }
    }

    /// Notes that `step` has done `done` of `total`; a new step starts its estimate over.
    pub fn note(&mut self, step: &str, done: u64, total: u64, unit: Unit, now: Instant) {
        if self.step != step {
            self.step = step.to_owned();
            self.step_born = now;
            self.step_first = done;
        }
        self.done = if total > 0 { done.min(total) } else { done };
        self.total = total;
        self.unit = unit;
    }

    /// The step it is on.
    pub fn step(&self) -> &str {
        &self.step
    }

    /// How far the step is, 0 to 1; none while nothing is measurable.
    pub fn share(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).clamp(0., 1.))
    }

    /// The whole percent the step is at.
    pub fn percent(&self) -> Option<u8> {
        self.share().map(|s| (s * 100.).floor() as u8)
    }

    /// How long the work has run.
    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.born)
    }

    /// How long the step still takes at the pace it has kept so far, once that pace is steady enough to say.
    pub fn eta(&self, now: Instant) -> Option<Duration> {
        let share = self.share()?;
        let ran = now.saturating_duration_since(self.step_born);
        let moved = self.done.checked_sub(self.step_first).filter(|m| *m > 0)?;
        if ran < SETTLE || !(EARLY..SURE).contains(&share) {
            return None;
        }
        let left = (self.total - self.done) as f64;
        let wait = Duration::try_from_secs_f64(ran.as_secs_f64() * left / moved as f64).ok()?;
        (wait < LONGEST).then_some(wait)
    }

    /// The figures beside the bar: what is done of what, then the time left; without a figure, the seconds it has run once
    /// they are worth naming.
    pub fn detail(&self, now: Instant) -> String {
        if self.total == 0 {
            let ran = self.elapsed(now);
            return if ran >= SHOW_ELAPSED { seconds(ran) } else { String::new() };
        }
        let figures = figures(self.done, self.total, self.unit);
        match self.eta(now) {
            Some(left) => format!("{figures} · {}", remaining(left)),
            None => figures,
        }
    }

    /// The step and its figures as one line, for places too small for a bar.
    pub fn line(&self, now: Instant) -> String {
        let detail = self.detail(now);
        if detail.is_empty() { self.step.clone() } else { format!("{} · {detail}", self.step) }
    }
}

fn figures(done: u64, total: u64, unit: Unit) -> String {
    match unit {
        Unit::Count => trf("%lld von %lld", &[&done, &total]),
        Unit::Bytes => {
            let (scale, name) = bytes_unit(total);
            trf("%@ von %@ %@", &[&amount(done, scale), &amount(total, scale), &name])
        }
    }
}

fn bytes_unit(of: u64) -> (f64, &'static str) {
    match of {
        n if n < 1 << 20 => (1024., "KB"),
        n if n < 1 << 30 => (f64::from(1u32 << 20), "MB"),
        _ => (f64::from(1u32 << 30), "GB"),
    }
}

fn amount(bytes: u64, scale: f64) -> String {
    let value = bytes as f64 / scale;
    decimal(if value >= 100. || scale == 1024. { format!("{value:.0}") } else { format!("{value:.1}") })
}

/// `12 s`, `3 Min. 05 s`: how long something has run.
pub fn seconds(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 { trf("%lld s", &[&s]) } else { trf("%lld Min. %lld s", &[&(s / 60), &format!("{:02}", s % 60)]) }
}

/// `noch 40 s`, `noch 3 Min.`: how long something still takes, rounded so it does not flicker.
pub fn remaining(d: Duration) -> String {
    let s = d.as_secs().max(1);
    match s {
        s if s < 60 => trf("noch %lld s", &[&((s + 4) / 5 * 5).min(55).max(5)]),
        s if s < 3600 => trf("noch %lld Min.", &[&s.div_ceil(60)]),
        s => {
            let minutes = s.div_ceil(60);
            trf("noch %lld Std. %lld Min.", &[&(minutes / 60), &(minutes % 60)])
        }
    }
}


/// The words for the stage of a game install.
pub fn install_step(stage: Stage) -> String {
    use crate::core::i18n::tr;
    match stage {
        Stage::Metadata => tr("Versionsdaten werden geladen"),
        Stage::Client => tr("Minecraft wird geladen"),
        Stage::Libraries => tr("Bibliotheken werden geladen"),
        Stage::Natives => tr("Systemdateien werden vorbereitet"),
        Stage::Assets => tr("Sounds und Texturen werden geladen"),
        Stage::Runtime => tr("Java wird geladen"),
        Stage::Processors => tr("Mod-Loader wird eingerichtet"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: u64) -> Instant {
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *ORIGIN.get_or_init(Instant::now) + Duration::from_secs(s)
    }

    #[test]
    fn work_without_a_figure_has_no_share_and_names_its_seconds_only_after_two() {
        let m = Meter::begin("Wird vorbereitet", at(0));
        assert_eq!(m.share(), None);
        assert_eq!(m.detail(at(1)), "");
        assert_eq!(m.detail(at(2)), seconds(Duration::from_secs(2)));
        assert_eq!(m.line(at(1)), "Wird vorbereitet");
        assert!(m.line(at(7)).starts_with("Wird vorbereitet · 7"));
    }

    #[test]
    fn a_figure_gives_a_share_and_a_percent() {
        let mut m = Meter::begin("Bibliotheken", at(0));
        m.note("Bibliotheken", 50, 200, Unit::Count, at(1));
        assert_eq!(m.share(), Some(0.25));
        assert_eq!(m.percent(), Some(25));
        assert_eq!(m.detail(at(1)), "50 von 200");
    }

    #[test]
    fn done_never_passes_total() {
        let mut m = Meter::begin("x", at(0));
        m.note("x", 5, 3, Unit::Count, at(0));
        assert_eq!(m.share(), Some(1.));
        assert_eq!(m.detail(at(0)), "3 von 3");
    }

    #[test]
    fn the_estimate_waits_for_a_steady_pace() {
        let mut m = Meter::begin("Dateien", at(0));
        m.note("Dateien", 10, 100, Unit::Count, at(1));
        assert_eq!(m.eta(at(2)), None, "too early");
        m.note("Dateien", 10, 100, Unit::Count, at(10));
        assert_eq!(m.eta(at(10)), Some(Duration::from_secs(90)));
        m.note("Dateien", 99, 100, Unit::Count, at(10));
        assert_eq!(m.eta(at(10)), None, "nearly done is not worth an estimate");
        m.note("Dateien", 0, 100, Unit::Count, at(10));
        assert_eq!(m.eta(at(10)), None, "nothing moved");
    }

    #[test]
    fn a_new_step_starts_the_estimate_over() {
        let mut m = Meter::begin("Bibliotheken", at(0));
        m.note("Bibliotheken", 90, 100, Unit::Count, at(10));
        m.note("Assets", 10, 1000, Unit::Count, at(10));
        assert_eq!(m.eta(at(11)), None);
        m.note("Assets", 20, 1000, Unit::Count, at(14));
        let left = m.eta(at(14)).unwrap();
        assert!(left > Duration::from_secs(300) && left < Duration::from_secs(500), "{left:?}");
    }

    #[test]
    fn bytes_are_shown_in_the_unit_of_the_total() {
        assert_eq!(figures(512 << 10, 2 << 20, Unit::Bytes), "0,5 von 2,0 MB");
        assert_eq!(figures(300 << 20, 2 << 30, Unit::Bytes), "300 von 2048 MB".replace("2048 MB", "2,0 GB").replace("300", "0,3"));
    }

    #[test]
    fn the_time_left_is_rounded_to_what_can_be_read_at_a_glance() {
        assert_eq!(remaining(Duration::from_secs(38)), trf("noch %lld s", &[&40]));
        assert_eq!(remaining(Duration::from_secs(61)), trf("noch %lld Min.", &[&2]));
        assert_eq!(remaining(Duration::from_secs(3700)), trf("noch %lld Std. %lld Min.", &[&1, &2]));
        assert_eq!(remaining(Duration::from_secs(7199)), trf("noch %lld Std. %lld Min.", &[&2, &0]));
    }
}
