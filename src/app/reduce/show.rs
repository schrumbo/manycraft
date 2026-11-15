use std::time::Duration;

use uuid::Uuid;

use super::super::effect::{Effect, Redraw, Unshown};
use super::super::model::{Game, Model, Phase};
use super::super::session::Reveal;
use super::{Env, fleet};
use crate::core::fleet as fleets;
use crate::core::i18n::trf;
use crate::core::instance::Instance;
use crate::core::ipc::{HotkeyAction, ToMod};

const SHOW_READY_FOR: Duration = Duration::from_secs(600);

/// Puts the game window of `key` in the manager's place and hides the manager.
pub(super) fn play(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    if let Some(i) = m.games.get(key).filter(|i| matches!(i.phase, Phase::Plain)) {
        m.error = Some(trf("%@ läuft in einem eigenen Fenster, ohne Manycrafts Mod. Hier lässt es sich nicht zeigen.", &[&i.name]));
        out.push(Effect::Redraw(Redraw::All));
        return;
    }
    if !m.games.get(key).is_some_and(|i| i.phase.playable() && !i.hint.link_lost) {
        return;
    }
    let s = &mut m.session;
    let whispers = m.games.get(key).map_or(0, |i| i.unread_whispers);
    let unshown = Unshown { playing: m.playing.clone(), reveal: s.reveal.clone(), show_when_ready: s.show_when_ready.take(), whispers };
    let before = m.playing.replace(key.to_owned());
    let mut skipped = None;
    let mut reveal = match s.reveal.take() {
        Some(r) if r.key != key => {
            skipped = Some(r.key);
            Reveal { key: key.to_owned(), hide: r.hide, manager: r.manager }
        }
        Some(r) => r,
        None => Reveal { key: key.to_owned(), manager: before.is_none(), hide: before.clone().into_iter().collect() },
    };
    reveal.hide.retain(|k| k != key);
    s.reveal = Some(reveal);
    if let Some(i) = m.games.get_mut(key) {
        i.unread_whispers = 0;
    }
    out.push(Effect::Redraw(Redraw::All));
    out.push(Effect::Show { key: key.to_owned(), before, skipped, unshown: Box::new(unshown) });
}

/// The game could not be shown because the manager has no place yet: what changed for it goes back.
pub(super) fn not_shown(m: &mut Model, unshown: Unshown, out: &mut Vec<Effect>) {
    if let Some(key) = m.playing.take()
        && let Some(i) = m.games.get_mut(&key)
    {
        i.unread_whispers += unshown.whispers;
    }
    m.playing = unshown.playing;
    m.session.reveal = unshown.reveal;
    m.session.show_when_ready = unshown.show_when_ready;
    out.push(Effect::Redraw(Redraw::All));
}

/// Hides what the game that is now shown covers: the manager and the games shown before it.
pub(super) fn reveal(m: &Model, reveal: Reveal, out: &mut Vec<Effect>) {
    for key in reveal.hide.iter().filter(|k| m.playing.as_ref() != Some(*k)) {
        out.push(Effect::Send { key: key.clone(), message: ToMod::Hide });
    }
    if reveal.manager && m.playing.is_some() {
        out.push(Effect::HideManager);
    }
}

/// Hides the game that is played and brings the manager back in its place.
pub(super) fn show_grid(m: &mut Model, out: &mut Vec<Effect>) {
    out.push(Effect::Follow(m.playing.clone()));
    let previous = m.playing.take();
    m.session.show_when_ready = None;
    let pending = m.session.reveal.take().map(|r| r.hide).unwrap_or_default();
    out.push(Effect::Redraw(Redraw::All));
    for key in pending.iter().filter(|k| previous.as_ref() != Some(*k)) {
        out.push(Effect::Send { key: key.clone(), message: ToMod::Hide });
    }
    if let Some(previous) = previous {
        out.push(Effect::Trace(format!("grid {previous}")));
        out.push(Effect::Send { key: previous, message: ToMod::Hide });
    }
    out.push(Effect::ShowManager);
}

pub(super) fn step(m: &mut Model, action: HotkeyAction, out: &mut Vec<Effect>) {
    if action == HotkeyAction::Grid {
        if m.playing.is_some() {
            show_grid(m, out);
        }
        return;
    }
    let target = {
        let order: Vec<&Game> = m.sorted(None).into_iter().filter(|i| i.phase.playable()).collect();
        let at = m.playing.as_ref().and_then(|p| order.iter().position(|i| i.key == *p));
        let n = order.len();
        let pick = |from: usize, forward: bool, want: &dyn Fn(&Game) -> bool| -> Option<String> {
            (1..=n).map(|d| if forward { (from + d) % n } else { (from + n - d % n) % n }).map(|ix| order[ix]).find(|i| want(i)).map(|i| i.key.clone())
        };
        match (action, at) {
            (_, _) if n == 0 => None,
            (HotkeyAction::Next, Some(at)) => pick(at, true, &|_| true),
            (HotkeyAction::Prev, Some(at)) => pick(at, false, &|_| true),
            (HotkeyAction::NextAttention, Some(at)) => pick(at, true, &|i| i.phase.needs_attention()),
            (HotkeyAction::NextAttention, None) => order.iter().find(|i| i.phase.needs_attention()).map(|i| i.key.clone()),
            (_, None) => Some(order[0].key.clone()),
            _ => None,
        }
    };
    if let Some(target) = target {
        play(m, &target, out);
    }
}

/// Whether a game in `phase` may still become ready to show, so a pending show keeps waiting for it.
fn on_the_way(phase: &Phase) -> bool {
    (phase.is_active() && !matches!(phase, Phase::Banned { .. })) || matches!(phase, Phase::NoNetwork)
}

/// The account `made` is played with alone when none is named: the one that played it last, else the only one.
pub fn solo_account(m: &Model, made: &Instance) -> Option<Uuid> {
    made.last_account.filter(|id| m.account(*id).is_some()).or_else(|| match m.accounts.as_slice() {
        [only] => Some(only.id),
        _ => None,
    })
}

/// Plays instance `id` alone with `account`, else the account that played it last; the game's key when there is one to play.
pub(super) fn play_instance(m: &mut Model, id: &str, account: Option<Uuid>, out: &mut Vec<Effect>) -> Option<String> {
    let picked = m.instance(id).and_then(|made| {
        let account = account.or_else(|| solo_account(m, made)).filter(|a| m.account(*a).is_some())?;
        let key = fleets::solo_key(made, account);
        let shown = m.games.get(&key).is_some_and(|g| g.phase.is_active());
        Some((key, account, shown))
    });
    let Some((key, account, shown)) = picked else {
        out.push(if m.accounts.is_empty() { Effect::AddAccount } else { Effect::ShowInstance(id.to_owned()) });
        return None;
    };
    if shown {
        play(m, &key, out);
        return Some(key);
    }
    let owner = fleets::solo_owner(id);
    let name = m.account(account).map(|a| a.name.clone()).unwrap_or_default();
    m.games.entry(key.clone()).or_insert_with(|| Game::new(key.clone(), owner.clone(), id.to_owned(), account, name));
    if let Some(made) = m.instances.iter_mut().find(|made| made.id == id) {
        made.last_account = Some(account);
    }
    m.order_seq += 1;
    out.push(Effect::Redraw(Redraw::All));
    fleet::launch(m, &owner, Some(std::slice::from_ref(&key)), fleet::Kick::Plain, out);
    out.push(Effect::RememberAccount { instance: id.to_owned(), account });
    Some(key)
}

/// Plays `instance` as another start of Manycraft or a shortcut asked, and shows the game as soon as it can be shown; waits for the first load when the data is not there yet.
pub(super) fn request_play(m: &mut Model, env: &Env, instance: String, account: Option<String>, out: &mut Vec<Effect>) {
    if !m.loaded {
        m.session.waiting_plays.push((instance, account));
        return;
    }
    let account = account.as_deref().and_then(|id| id.parse().ok());
    if play_and_show(m, env, &instance, account, out).is_none() {
        show_grid(m, out);
    }
}

/// Plays instance `id` alone like [`play_instance`] and shows the game as soon as it can be shown.
pub(super) fn play_and_show(m: &mut Model, env: &Env, id: &str, account: Option<Uuid>, out: &mut Vec<Effect>) -> Option<String> {
    let key = play_instance(m, id, account, out)?;
    m.session.show_when_ready = Some((key.clone(), env.now));
    Some(key)
}

/// Shows the game a play request asked for once it can be shown, or gives up on it after a while.
pub(super) fn show_ready(m: &mut Model, env: &Env, out: &mut Vec<Effect>) {
    let Some((key, since)) = m.session.show_when_ready.clone() else { return };
    let shown = m.playing.as_deref() == Some(key.as_str());
    let phase = m.games.get(&key).map(|g| (&g.phase, g.hint.link_lost));
    let ready = phase.is_some_and(|(p, lost)| p.playable() && !lost);
    let waiting = phase.is_some_and(|(p, _)| on_the_way(p));
    if !shown && !ready && waiting && env.now.saturating_duration_since(since) < SHOW_READY_FOR {
        return;
    }
    m.session.show_when_ready = None;
    if ready && !shown {
        play(m, &key, out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::effect::Effect;
    use super::super::super::event::Report;
    use super::super::super::model::Phase;
    use super::super::super::session::Reveal;
    use super::super::testing::*;
    use crate::core::account::AccountKind;
    use crate::core::ipc::{FromMod, HotkeyAction, ToMod};

    fn hides(fx: &[Effect]) -> Vec<&str> {
        fx.iter().filter_map(|e| if let Effect::Send { key, message: ToMod::Hide } = e { Some(key.as_str()) } else { None }).collect()
    }

    fn message(key: &str, message: FromMod) -> Report {
        Report::Mod { key: key.to_owned(), message }
    }

    #[test]
    fn closing_a_game_that_is_not_played_hides_only_it() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.playing = Some("other".into());
        let fx = report(&mut m, &s, message(&key, FromMod::Hotkey { action: HotkeyAction::Grid }));
        assert!(!fx.iter().any(|e| matches!(e, Effect::ShowManager | Effect::Follow(_))));
        assert_eq!(hides(&fx), [key.as_str()]);
        assert_eq!(fx.iter().filter(|e| matches!(e, Effect::Send { .. })).count(), 1);
        assert_eq!(m.playing.as_deref(), Some("other"));
    }

    #[test]
    fn the_shown_game_reveals_once() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.playing = Some(key.clone());
        m.session.reveal = Some(Reveal { key: key.clone(), hide: vec!["before".into()], manager: true });
        let fx = report(&mut m, &s, message("before", FromMod::Window { visible: true, focused: false }));
        assert!(hides(&fx).is_empty() && !fx.iter().any(|e| matches!(e, Effect::HideManager)) && m.session.reveal.is_some());
        let fx = report(&mut m, &s, message(&key, FromMod::Window { visible: true, focused: false }));
        assert_eq!(hides(&fx), ["before"]);
        assert!(fx.iter().any(|e| matches!(e, Effect::HideManager)));
        assert!(m.session.reveal.is_none());
    }

    #[test]
    fn hotkeys_from_the_game_still_covering_count() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.playing = Some("next".into());
        m.session.reveal = Some(Reveal { key: "next".into(), hide: vec![key.clone()], manager: false });
        let fx = report(&mut m, &s, message(&key, FromMod::Hotkey { action: HotkeyAction::Grid }));
        assert!(fx.iter().any(|e| matches!(e, Effect::ShowManager)));
        assert!(m.playing.is_none());
    }

    #[test]
    fn a_play_request_from_another_start_is_kept_for_the_manager() {
        let s = settings();
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let fx = report(&mut m, &s, Report::PlayRequest { instance: "abc".into(), account: Some("acc".into()) });
        assert_eq!(m.session.waiting_plays, vec![("abc".to_owned(), Some("acc".to_owned()))]);
        assert!(!fx.iter().any(|e| matches!(e, Effect::ShowManager)));
    }

    #[test]
    fn a_second_start_brings_the_manager_back() {
        let s = settings();
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let fx = report(&mut m, &s, Report::Reopen);
        assert!(fx.iter().any(|e| matches!(e, Effect::ShowManager)));
    }

    #[test]
    fn a_pending_show_waits_only_while_the_game_is_on_its_way() {
        for phase in [Phase::Starting { place: 0, total: 0 }, Phase::Restarting { attempt: 1, of: 3 }, Phase::NoNetwork, Phase::Menu] {
            assert!(super::on_the_way(&phase), "{phase:?}");
        }
        for phase in [Phase::Stopped, Phase::Failed { reason: "x".into() }, Phase::Crashed { code: None }, Phase::LoginNeeded, Phase::Banned { server: None, reason: "x".into() }] {
            assert!(!super::on_the_way(&phase), "{phase:?}");
        }
    }

    #[test]
    fn a_show_that_finds_no_place_leaves_everything_as_it_was() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.games.get_mut(&key).unwrap().unread_whispers = 2;
        let fx = run(&mut m, &s, std::time::Instant::now(), super::super::super::event::Event::Play(key.clone()));
        let Some(Effect::Show { unshown, .. }) = fx.into_iter().find(|e| matches!(e, Effect::Show { .. })) else { panic!("no show") };
        assert_eq!(m.games[&key].unread_whispers, 0);
        run(&mut m, &s, std::time::Instant::now(), super::super::super::event::Event::NotShown(unshown));
        assert_eq!(m.games[&key].unread_whispers, 2);
        assert!(m.playing.is_none() && m.session.reveal.is_none());
    }
}

