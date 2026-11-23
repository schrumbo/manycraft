//! When a dropped or crashed instance tries again: exponential backoff with jitter and an attempt cap.

use std::time::Duration;

use super::ipc::DisconnectCause;
use super::settings::{CrashRestart, Reconnect};

/// What to do about a crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Retry(Duration),
    Stop,
}

/// What a disconnect asks of the manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    Retry(Duration),
    /// The network is gone: wait for it, without spending a try.
    WaitForNetwork,
    GiveUp,
    Banned,
    SignInAgain,
    NotAllowed,
}

/// A random number in `[0, 1)` for jitter.
pub fn unit() -> f64 {
    (uuid::Uuid::new_v4().as_u128() >> 75) as f64 / (1u64 << 53) as f64
}

/// The wait before try number `attempt` (counting from 0): the schedule entry for it, spread by `jitter_percent` with `unit` in `[0, 1)`.
pub fn wait(schedule: &[u64], attempt: u32, jitter_percent: u32, unit: f64) -> Duration {
    let base = schedule.get(attempt as usize).or(schedule.last()).copied().unwrap_or(1) as f64;
    let spread = f64::from(jitter_percent.min(100)) / 100.0;
    Duration::from_secs_f64((base * (1.0 + spread * (2.0 * unit.clamp(0.0, 1.0) - 1.0))).max(0.5))
}

/// How to answer a disconnect for `cause` after `attempt` earlier tries, `offline` when this computer has no connection.
/// Banned stops with no retry ever; a missing whitelist entry or the same account signed in elsewhere stops; a rejected sign-in asks for a new one; a full server or a kick waits longer.
/// A lost server while the network is gone waits for the network and spends no try.
pub fn react(cause: DisconnectCause, attempt: u32, policy: &Reconnect, unit: f64, offline: bool) -> Reaction {
    match cause {
        DisconnectCause::Banned => return Reaction::Banned,
        DisconnectCause::Auth => return Reaction::SignInAgain,
        DisconnectCause::Whitelist | DisconnectCause::Duplicate | DisconnectCause::PackDeclined => return Reaction::NotAllowed,
        DisconnectCause::ServerDown | DisconnectCause::Unknown if offline && policy.enabled => return Reaction::WaitForNetwork,
        DisconnectCause::ServerFull | DisconnectCause::Kicked | DisconnectCause::ServerDown | DisconnectCause::Unknown => {}
    }
    if !policy.enabled || attempt >= policy.max_attempts {
        return Reaction::GiveUp;
    }
    let schedule = match cause {
        DisconnectCause::ServerFull | DisconnectCause::Kicked => &policy.patient_secs,
        _ => &policy.backoff_secs,
    };
    Reaction::Retry(wait(schedule, attempt, policy.jitter_percent, unit))
}

/// Whether a game that exited with an error starts again after `attempt` earlier restarts.
pub fn restart(attempt: u32, policy: &CrashRestart, unit: f64) -> Decision {
    if !policy.enabled || attempt >= policy.max_attempts {
        return Decision::Stop;
    }
    Decision::Retry(wait(&policy.backoff_secs, attempt, policy.jitter_percent, unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECS: [u64; 4] = [10, 30, 90, 300];

    #[test]
    fn waits_follow_the_schedule_and_repeat_its_last_entry() {
        let at = |n| wait(&SECS, n, 0, 0.5).as_secs();
        assert_eq!((at(0), at(1), at(2), at(3), at(4), at(40)), (10, 30, 90, 300, 300, 300));
    }

    #[test]
    fn jitter_stays_inside_its_band() {
        assert_eq!(wait(&SECS, 0, 25, 0.0), Duration::from_secs_f64(7.5));
        assert_eq!(wait(&SECS, 0, 25, 1.0), Duration::from_secs_f64(12.5));
        assert_eq!(wait(&SECS, 2, 25, 0.5), Duration::from_secs(90));
        assert_eq!(wait(&SECS, 0, 500, 0.0), Duration::from_secs_f64(0.5));
        assert_eq!(wait(&[], 0, 0, 0.5), Duration::from_secs(1));
        for _ in 0..1000 {
            let u = unit();
            assert!((0.0..1.0).contains(&u));
        }
    }

    #[test]
    fn banned_auth_and_whitelist_never_retry() {
        let p = Reconnect::default();
        assert_eq!(react(DisconnectCause::Banned, 0, &p, 0.5, false), Reaction::Banned);
        assert_eq!(react(DisconnectCause::Auth, 0, &p, 0.5, false), Reaction::SignInAgain);
        assert_eq!(react(DisconnectCause::Whitelist, 0, &p, 0.5, false), Reaction::NotAllowed);
        assert_eq!(react(DisconnectCause::Duplicate, 0, &p, 0.5, false), Reaction::NotAllowed);
    }

    #[test]
    fn down_and_unknown_back_off_and_give_up_at_the_cap() {
        let p = Reconnect::default();
        assert_eq!(react(DisconnectCause::ServerDown, 0, &p, 0.5, false), Reaction::Retry(Duration::from_secs(10)));
        assert_eq!(react(DisconnectCause::Unknown, 1, &p, 0.5, false), Reaction::Retry(Duration::from_secs(30)));
        assert_eq!(react(DisconnectCause::ServerDown, 4, &p, 0.5, false), Reaction::Retry(Duration::from_secs(300)));
        assert_eq!(react(DisconnectCause::ServerDown, 5, &p, 0.5, false), Reaction::GiveUp);
    }

    #[test]
    fn kicks_and_full_servers_wait_longer() {
        let p = Reconnect::default();
        assert_eq!(react(DisconnectCause::Kicked, 0, &p, 0.5, false), Reaction::Retry(Duration::from_secs(60)));
        assert_eq!(react(DisconnectCause::ServerFull, 2, &p, 0.5, false), Reaction::Retry(Duration::from_secs(600)));
        assert_eq!(react(DisconnectCause::Kicked, 5, &p, 0.5, false), Reaction::GiveUp);
    }

    #[test]
    fn disabled_reconnect_gives_up_at_once_but_still_classifies_bans() {
        let p = Reconnect { enabled: false, ..Reconnect::default() };
        assert_eq!(react(DisconnectCause::ServerDown, 0, &p, 0.5, false), Reaction::GiveUp);
        assert_eq!(react(DisconnectCause::Banned, 0, &p, 0.5, false), Reaction::Banned);
    }

    #[test]
    fn no_network_waits_without_spending_tries_and_never_hides_a_ban() {
        let p = Reconnect::default();
        for attempt in [0, 4, 5, 40] {
            assert_eq!(react(DisconnectCause::ServerDown, attempt, &p, 0.5, true), Reaction::WaitForNetwork);
            assert_eq!(react(DisconnectCause::Unknown, attempt, &p, 0.5, true), Reaction::WaitForNetwork);
        }
        assert_eq!(react(DisconnectCause::ServerDown, 0, &Reconnect { enabled: false, ..Reconnect::default() }, 0.5, true), Reaction::GiveUp);
        assert_eq!(react(DisconnectCause::Banned, 0, &p, 0.5, true), Reaction::Banned);
        assert_eq!(react(DisconnectCause::Auth, 0, &p, 0.5, true), Reaction::SignInAgain);
        assert_eq!(react(DisconnectCause::Kicked, 0, &p, 0.5, true), Reaction::Retry(Duration::from_secs(60)));
    }

    #[test]
    fn crash_restart_follows_its_own_cap() {
        let p = CrashRestart::default();
        assert_eq!(restart(0, &p, 0.5), Decision::Retry(Duration::from_secs(10)));
        assert_eq!(restart(2, &p, 0.5), Decision::Retry(Duration::from_secs(90)));
        assert_eq!(restart(3, &p, 0.5), Decision::Stop);
        assert_eq!(restart(0, &CrashRestart { enabled: false, ..p }, 0.5), Decision::Stop);
    }
}