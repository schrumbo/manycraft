//! Closing the window and quitting without ending the games: they keep running in the notification area or after the manager quit.

use super::super::effect::Effect;
use super::super::model::{Model, Phase};

fn running(m: &Model) -> usize {
    m.games.values().filter(|i| i.phase.is_running()).count()
}

/// Closing the window quits when no game runs; otherwise the manager goes to the notification area, with the hint the first time.
/// Before the data loaded, games found from the last run are not counted yet, so the quit keeps them.
pub(super) fn close_window(m: &Model, explained: bool, out: &mut Vec<Effect>) {
    if !m.loaded {
        out.push(Effect::Quit { keep: true });
    } else if running(m) == 0 {
        out.push(Effect::Quit { keep: false });
    } else {
        out.push(Effect::ToTray { explain: !explained });
    }
}

/// Quitting asks what happens to the running games, and quits at once when none runs; before the data loaded it keeps them.
pub(super) fn ask(m: &Model, out: &mut Vec<Effect>) {
    if !m.loaded {
        out.push(Effect::Quit { keep: true });
        return;
    }
    match running(m) {
        0 => out.push(Effect::Quit { keep: false }),
        running => out.push(Effect::AskQuit { running }),
    }
}

/// The restart that installs an update keeps the games, for the new version to find again; it waits while a game starts.
pub(super) fn restart_for_update(m: &Model, out: &mut Vec<Effect>) {
    if !m.games.values().any(|i| matches!(i.phase, Phase::Starting { .. })) {
        out.push(Effect::Quit { keep: true });
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::effect::Effect;
    use super::super::super::event::Event;
    use super::super::super::model::{Model, Phase};
    use super::super::testing::{run, settings, setup};
    use crate::core::account::AccountKind;
    use std::time::Instant;

    fn quits(fx: &[Effect]) -> Vec<String> {
        fx.iter().filter(|e| matches!(e, Effect::Quit { .. } | Effect::ToTray { .. } | Effect::AskQuit { .. } | Effect::RememberTrayExplained)).map(|e| format!("{e:?}")).collect()
    }

    fn with(phase: Phase) -> Model {
        let mut m = setup(phase, AccountKind::Offline, &[]).0;
        m.loaded = true;
        m
    }

    #[test]
    fn before_the_data_loaded_closing_and_quitting_keep_the_games_found_from_the_last_run() {
        let s = settings();
        let mut m = with(Phase::Stopped);
        m.loaded = false;
        assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::CloseWindow)), ["Quit { keep: true }"]);
        assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::Quit)), ["Quit { keep: true }"]);
    }

    #[test]
    fn closing_without_running_games_quits() {
        let s = settings();
        for phase in [Phase::Stopped, Phase::Crashed { code: Some(1) }, Phase::LoginNeeded, Phase::NoNetwork] {
            let mut m = with(phase);
            assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::CloseWindow)), ["Quit { keep: false }"]);
        }
    }

    #[test]
    fn closing_with_running_games_goes_to_the_tray_and_explains_it_once() {
        let mut s = settings();
        for phase in [Phase::Menu, Phase::Starting { place: 1, total: 2 }, Phase::Playing { server: "a".into(), since: Instant::now() }] {
            let mut m = with(phase);
            s.tray_explained = false;
            assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::CloseWindow)), ["ToTray { explain: true }"]);
            assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::TrayExplained)), ["RememberTrayExplained", "ToTray { explain: false }"]);
            s.tray_explained = true;
            assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::CloseWindow)), ["ToTray { explain: false }"]);
        }
    }

    #[test]
    fn a_game_that_ended_during_the_hint_lets_the_window_quit() {
        let s = settings();
        let mut m = with(Phase::Stopped);
        assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::TrayExplained)), ["RememberTrayExplained", "Quit { keep: false }"]);
    }

    #[test]
    fn quitting_asks_only_while_games_run() {
        let s = settings();
        let mut idle = with(Phase::Stopped);
        assert_eq!(quits(&run(&mut idle, &s, Instant::now(), Event::Quit)), ["Quit { keep: false }"]);
        let mut busy = with(Phase::Menu);
        assert_eq!(quits(&run(&mut busy, &s, Instant::now(), Event::Quit)), ["AskQuit { running: 1 }"]);
    }

    #[test]
    fn the_quit_answer_decides_whether_games_keep_running() {
        let s = settings();
        let mut m = with(Phase::Menu);
        assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::QuitChosen { keep: true })), ["Quit { keep: true }"]);
        assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::QuitChosen { keep: false })), ["Quit { keep: false }"]);
        assert!(m.games.values().all(|i| i.phase == Phase::Menu));
    }

    #[test]
    fn restarting_for_an_update_keeps_the_games_and_waits_for_starts() {
        let s = settings();
        for phase in [Phase::Stopped, Phase::Menu, Phase::Playing { server: "a".into(), since: Instant::now() }] {
            let mut m = with(phase);
            assert_eq!(quits(&run(&mut m, &s, Instant::now(), Event::RestartForUpdate)), ["Quit { keep: true }"]);
        }
        let mut starting = with(Phase::Starting { place: 1, total: 1 });
        assert!(quits(&run(&mut starting, &s, Instant::now(), Event::RestartForUpdate)).is_empty());
    }
}
