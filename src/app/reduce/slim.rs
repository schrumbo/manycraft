//! The manager steps aside while a game plays alone and comes back when it ends.

use super::Env;
use crate::app::effect::Effect;
use crate::app::model::{Game, Model};
use crate::core::settings::WhilePlaying;

fn plays_alone(g: &Game) -> bool {
    g.alone() && g.phase.is_running() && !g.phase.waiting()
}

fn wanted(m: &Model, env: &Env) -> bool {
    env.settings.while_playing != WhilePlaying::Keep && m.games.values().any(plays_alone) && !m.games.values().any(|g| !g.alone() && g.phase.is_running())
}

/// Puts the manager away when a game played alone has just launched, and brings it back when the last one ended, a batch started, or one of them needs the user.
pub(super) fn settle(m: &mut Model, env: &Env, launched: bool, out: &mut Vec<Effect>) {
    if wanted(m, env) {
        if launched || !m.session.slimmed.is_empty() {
            let first = m.session.slimmed.is_empty();
            let playing: Vec<String> = m.games.values().filter(|g| plays_alone(g)).map(|g| g.key.clone()).collect();
            m.session.slimmed.extend(playing);
            if first {
                out.push(Effect::Slim { tray: env.settings.while_playing == WhilePlaying::Tray });
            }
        }
    } else if !m.session.slimmed.is_empty() {
        let kept = std::mem::take(&mut m.session.slimmed);
        let trouble = kept.iter().any(|k| m.games.get(k).is_some_and(|g| g.phase.needs_attention()));
        if env.settings.reopen_after || trouble {
            out.push(Effect::ShowManager);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::testing::{report, run, settings};
    use super::*;
    use crate::app::event::{Event, Report};
    use crate::app::model::Phase;
    use crate::core::account::{Account, AccountKind};
    use crate::core::batch::Batch;
    use crate::core::instance::{Instance, Loader};
    use uuid::Uuid;

    fn solo_model() -> (Model, String) {
        let id = Uuid::from_u128(7);
        let made = Instance::new("Solo", "26.3", Loader::default());
        let key = crate::core::fleet::solo_key(&made, id);
        let mut m = Model::default();
        m.accounts.push(Account { id, name: "AltOne".into(), kind: AccountKind::Offline, proxy: None, color: None });
        let owner = crate::core::fleet::solo_owner(&made.id);
        m.games.insert(key.clone(), Game::new(key.clone(), owner, made.id.clone(), id, "AltOne".into()));
        m.instances.push(made);
        (m, key)
    }

    fn slims(fx: &[Effect]) -> Vec<bool> {
        fx.iter().filter_map(|e| if let Effect::Slim { tray } = e { Some(*tray) } else { None }).collect()
    }

    fn comes_back(fx: &[Effect]) -> bool {
        fx.iter().any(|e| matches!(e, Effect::ShowManager))
    }

    fn launch(m: &mut Model, key: &str) -> Vec<Effect> {
        m.games.get_mut(key).unwrap().phase = Phase::Starting { place: 0, total: 0 };
        report(m, &settings(), Report::Launched { key: key.into(), plain: true })
    }

    #[test]
    fn the_manager_steps_aside_once_a_game_played_alone_launched_and_returns_when_it_ends() {
        let s = settings();
        let (mut m, key) = solo_model();
        let fx = launch(&mut m, &key);
        assert_eq!(slims(&fx), [false]);
        assert!(slims(&report(&mut m, &s, Report::Connected { key: key.clone(), pid: 1 })).is_empty(), "only the launch puts it away");
        let fx = report(&mut m, &s, Report::Exited { key: key.clone(), code: Some(0) });
        assert!(comes_back(&fx) && m.session.slimmed.is_empty());
    }

    #[test]
    fn the_notification_area_is_chosen_in_the_settings() {
        let mut s = settings();
        s.while_playing = WhilePlaying::Tray;
        let (mut m, key) = solo_model();
        m.games.get_mut(&key).unwrap().phase = Phase::Starting { place: 0, total: 0 };
        let fx = run(&mut m, &s, Instant::now(), Event::Reports(vec![Report::Launched { key, plain: true }]));
        assert_eq!(slims(&fx), [true]);
    }

    #[test]
    fn keeping_it_open_never_moves_the_window() {
        let mut s = settings();
        s.while_playing = WhilePlaying::Keep;
        let (mut m, key) = solo_model();
        m.games.get_mut(&key).unwrap().phase = Phase::Starting { place: 0, total: 0 };
        let fx = run(&mut m, &s, Instant::now(), Event::Reports(vec![Report::Launched { key: key.clone(), plain: true }]));
        assert!(slims(&fx).is_empty());
        assert!(!comes_back(&report(&mut m, &s, Report::Exited { key, code: Some(0) })));
    }

    #[test]
    fn a_game_waiting_for_its_turn_has_not_launched() {
        let s = settings();
        let (mut m, key) = solo_model();
        m.games.get_mut(&key).unwrap().phase = Phase::Starting { place: 1, total: 2 };
        assert!(slims(&run(&mut m, &s, Instant::now(), Event::Reports(vec![Report::Preparing { batch: "x".into(), percent: 5 }]))).is_empty());
    }

    #[test]
    fn staying_closed_after_the_game_is_a_choice_but_a_crash_is_always_shown() {
        let mut s = settings();
        s.reopen_after = false;
        let (mut m, key) = solo_model();
        launch(&mut m, &key);
        let fx = report(&mut m, &s, Report::Exited { key: key.clone(), code: Some(0) });
        assert!(!comes_back(&fx) && m.session.slimmed.is_empty());
        launch(&mut m, &key);
        let fx = report(&mut m, &s, Report::Exited { key, code: Some(1) });
        assert!(m.games.values().all(|g| g.phase.needs_attention()) && comes_back(&fx));
    }

    #[test]
    fn an_old_crash_does_not_bring_the_window_back_against_the_choice() {
        let mut s = settings();
        s.reopen_after = false;
        let (mut m, key) = solo_model();
        let other = {
            let mut g = m.games[&key].clone();
            g.key = "other-solo".into();
            g.phase = Phase::Crashed { code: Some(1) };
            g
        };
        m.games.insert(other.key.clone(), other);
        launch(&mut m, &key);
        assert!(!comes_back(&report(&mut m, &s, Report::Exited { key, code: Some(0) })));
    }

    #[test]
    fn a_batch_running_keeps_the_manager_where_it_is_and_one_starting_calls_it_back() {
        let s = settings();
        let (mut m, key) = solo_model();
        let batch_game = {
            let mut g = Game::new("b-1".into(), "batch".into(), "i".into(), Uuid::from_u128(9), "B".into());
            g.phase = Phase::Menu;
            g
        };
        m.games.insert(batch_game.key.clone(), batch_game);
        m.batches.push(Batch::new("batch", "i"));
        assert!(slims(&launch(&mut m, &key)).is_empty());
        let (mut m, key) = solo_model();
        launch(&mut m, &key);
        assert!(!m.session.slimmed.is_empty());
        let mut g = Game::new("b-1".into(), "batch".into(), "i".into(), Uuid::from_u128(9), "B".into());
        g.phase = Phase::Menu;
        m.games.insert(g.key.clone(), g);
        let fx = run(&mut m, &s, Instant::now(), Event::Reports(vec![Report::Preparing { batch: "batch".into(), percent: 1 }]));
        assert!(comes_back(&fx));
    }
}