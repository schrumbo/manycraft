//! Which phase changes are allowed, by what caused them. A change the table does not allow is logged and ignored, never applied.

use super::model::Phase;

/// What asks for a phase change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// A start puts the game in line.
    Launch,
    /// A start found the account already playing elsewhere.
    Busy,
    /// The fleet launched the game process.
    Launched,
    /// The fleet could not start the game.
    StartFailed,
    /// The game's mod connected for the first time.
    Connected,
    /// The game's mod reported where it is.
    Mod,
    /// The game process ended.
    Exit,
    /// The user stopped a game that has no process.
    Stop,
    /// A token refresh found the sign-in working or broken.
    SignIn,
    /// The user asked a running game to reconnect.
    Reconnect,
    /// The network went away while a reconnect waited.
    NetworkLost,
    /// The network came back for a game that waited for it.
    NetworkBack,
    /// The game lost its server along with most of its batch and waits for it instead of spending tries.
    ServerAway,
    /// The server a game waited for answered again.
    ServerBack,
    /// A test backend sets the phase directly.
    Seed,
    /// A game an earlier manager launched was taken over.
    Adopt,
}

/// Whether `why` may move a game from `from` to `to`.
pub fn allowed(from: &Phase, to: &Phase, why: Why) -> bool {
    use Phase::*;
    match why {
        Why::Seed | Why::Adopt => true,
        Why::Launch => !from.is_running() && matches!(to, Starting { .. }),
        Why::Busy => !from.is_running() && matches!(to, Failed { .. }),
        Why::Launched => matches!(from, Starting { .. } | Failed { .. }) && matches!(to, Plain | Starting { place: 0, .. }),
        Why::StartFailed => matches!(from, Starting { .. } | NoNetwork | Failed { .. }) && matches!(to, LoginNeeded | NoNetwork | Failed { .. }),
        Why::Connected => matches!(from, Starting { .. }) && *to == Menu,
        Why::Mod => mod_reports(from) && matches!(to, Menu | Connecting { .. } | Queue { .. } | Playing { .. } | Dead { .. } | Disconnected { .. } | NetworkLost { .. } | ServerAway { .. } | Banned { .. } | Restarting { .. } | LoginNeeded | Offer { .. }),
        Why::Exit => matches!(to, Stopped | Crashed { .. } | Restarting { .. }),
        Why::Stop => matches!(from, Starting { .. } | Crashed { .. } | Restarting { .. } | Failed { .. } | NoNetwork) && *to == Stopped,
        Why::SignIn => matches!((from, to), (Stopped | Crashed { .. } | Failed { .. }, LoginNeeded) | (LoginNeeded, Stopped)),
        Why::Reconnect => mod_reports(from) && matches!(to, Connecting { .. }),
        Why::NetworkLost => matches!((from, to), (Connecting { .. }, NetworkLost { .. })),
        Why::NetworkBack => matches!((from, to), (NetworkLost { .. }, Connecting { .. })),
        Why::ServerAway => matches!((from, to), (Connecting { .. } | Disconnected { .. }, ServerAway { .. })),
        Why::ServerBack => matches!((from, to), (ServerAway { .. }, Connecting { .. })),
    }
}

/// Whether a game in `phase` has a process whose mod may report: one that runs and is not a plain game.
pub fn mod_reports(phase: &Phase) -> bool {
    phase.is_running() && *phase != Phase::Plain
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn all() -> Vec<Phase> {
        let s = || "s".to_owned();
        vec![
            Phase::Stopped,
            Phase::Starting { place: 2, total: 3 },
            Phase::Starting { place: 0, total: 0 },
            Phase::Plain,
            Phase::Menu,
            Phase::Connecting { server: s() },
            Phase::Queue { server: s() },
            Phase::Playing { server: s(), since: Instant::now() },
            Phase::Disconnected { server: Some(s()), reason: s() },
            Phase::Dead { server: None, reason: s() },
            Phase::Crashed { code: Some(1) },
            Phase::Restarting { attempt: 1, of: 3 },
            Phase::Banned { server: None, reason: s() },
            Phase::LoginNeeded,
            Phase::Failed { reason: s() },
            Phase::NetworkLost { server: s() },
            Phase::ServerAway { server: s() },
            Phase::NoNetwork,
            Phase::Offer { server: s(), offer: crate::core::ipc::Offer { id: s(), required: true, text: None, conduct: false } },
        ]
    }

    #[test]
    fn a_held_resource_pack_needs_the_user_and_only_the_mod_raises_it() {
        let offer = Phase::Offer { server: "s".into(), offer: crate::core::ipc::Offer { id: "1".into(), required: false, text: None, conduct: false } };
        assert!(offer.needs_attention() && offer.is_running() && offer.reconnectable() && !offer.can_chat());
        assert_eq!(offer.need(), Some(super::super::model::Need::Offer));
        for from in all() {
            assert_eq!(allowed(&from, &offer, Why::Mod), mod_reports(&from), "{from:?}");
            assert!(!allowed(&from, &offer, Why::Connected) && !allowed(&from, &offer, Why::Reconnect));
        }
        assert!(allowed(&offer, &Phase::Playing { server: "s".into(), since: Instant::now() }, Why::Mod));
        assert!(allowed(&offer, &Phase::Disconnected { server: None, reason: "x".into() }, Why::Mod));
        assert!(allowed(&offer, &Phase::Connecting { server: "s".into() }, Why::Reconnect));
    }

    #[test]
    fn a_start_waiting_for_the_network_is_neither_running_nor_stuck() {
        assert!(!Phase::NoNetwork.is_running() && !Phase::NoNetwork.is_active());
        assert!(Phase::NoNetwork.stoppable());
        assert!(allowed(&Phase::NoNetwork, &Phase::Stopped, Why::Stop));
        assert!(allowed(&Phase::NoNetwork, &Phase::Starting { place: 1, total: 1 }, Why::Launch));
        assert!(!allowed(&Phase::NoNetwork, &Phase::Menu, Why::Mod), "no process, no mod");
    }

    #[test]
    fn only_a_start_can_fail_to_start() {
        for from in all() {
            let ok = allowed(&from, &Phase::Failed { reason: "x".into() }, Why::StartFailed);
            assert_eq!(ok, matches!(from, Phase::Starting { .. } | Phase::NoNetwork | Phase::Failed { .. }), "{from:?}");
        }
    }

    #[test]
    fn the_mod_moves_only_games_with_a_process() {
        for from in all() {
            let ok = allowed(&from, &Phase::Menu, Why::Mod);
            assert_eq!(ok, from.is_running() && from != Phase::Plain, "{from:?}");
        }
        assert!(!allowed(&Phase::LoginNeeded, &Phase::Menu, Why::Mod), "a dying game does not undo the sign-in request");
        assert!(!allowed(&Phase::Menu, &Phase::Stopped, Why::Mod), "only an exit stops");
    }

    #[test]
    fn a_launch_never_lands_on_a_running_game() {
        for from in all() {
            assert_eq!(allowed(&from, &Phase::Starting { place: 1, total: 1 }, Why::Launch), !from.is_running(), "{from:?}");
        }
        assert!(!allowed(&Phase::Stopped, &Phase::Plain, Why::Launched), "a late launch report does not revive a stopped game");
        assert!(!allowed(&Phase::Stopped, &Phase::Menu, Why::Connected));
    }

    #[test]
    fn an_exit_always_ends_the_game_and_nothing_else_does() {
        for from in all() {
            assert!(allowed(&from, &Phase::Stopped, Why::Exit));
            assert!(!allowed(&from, &Phase::Menu, Why::Exit));
        }
        assert!(!allowed(&Phase::Menu, &Phase::Stopped, Why::Stop), "a running game stops by its exit");
    }

    #[test]
    fn the_network_moves_only_games_that_wait_for_it() {
        let connecting = Phase::Connecting { server: "s".into() };
        let lost = Phase::NetworkLost { server: "s".into() };
        assert!(allowed(&connecting, &lost, Why::NetworkLost) && allowed(&lost, &connecting, Why::NetworkBack));
        assert!(!allowed(&Phase::Menu, &lost, Why::NetworkLost) && !allowed(&Phase::Stopped, &connecting, Why::NetworkBack));
    }

    #[test]
    fn only_games_that_lost_the_server_wait_for_it_and_only_its_answer_wakes_them() {
        let away = Phase::ServerAway { server: "s".into() };
        let connecting = Phase::Connecting { server: "s".into() };
        assert!(allowed(&connecting, &away, Why::ServerAway) && allowed(&away, &connecting, Why::ServerBack));
        assert!(allowed(&Phase::Disconnected { server: Some("s".into()), reason: String::new() }, &away, Why::ServerAway));
        for from in all().into_iter().filter(|p| !matches!(p, Phase::Connecting { .. } | Phase::Disconnected { .. })) {
            assert!(!allowed(&from, &away, Why::ServerAway), "{from:?}");
        }
        assert!(!allowed(&lost(), &connecting, Why::ServerBack));
        assert!(away.is_running() && away.playable() && !away.needs_attention());
    }

    fn lost() -> Phase {
        Phase::NetworkLost { server: "s".into() }
    }
}