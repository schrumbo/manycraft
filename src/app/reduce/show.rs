use uuid::Uuid;

use super::super::effect::{Effect, Redraw, Unshown};
use super::super::model::{Game, Model, Phase};
use super::super::session::Reveal;
use super::fleet;
use crate::core::account::AccountKind;
use crate::core::fleet as fleets;
use crate::core::i18n::trf;
use crate::core::instance::Instance;
use crate::core::ipc::{HotkeyAction, ToMod};


/// Puts the game window of `key` in the manager's place and hides the manager; a game played alone stays where it is.
pub(super) fn play(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    if m.games.get(key).is_some_and(Game::alone) {
        return;
    }
    if let Some(i) = m.games.get(key).filter(|i| matches!(i.phase, Phase::Plain)) {
        m.error = Some(trf("%@ läuft in einem eigenen Fenster, ohne Manycrafts Mod. Hier lässt es sich nicht zeigen.", &[&i.name]));
        out.push(Effect::Redraw(Redraw::All));
        return;
    }
    if !m.games.get(key).is_some_and(|i| i.phase.playable() && !i.hint.link_lost && !i.stopping) {
        return;
    }
    let s = &mut m.session;
    let whispers = m.games.get(key).map_or(0, |i| i.unread_whispers);
    let unshown = Unshown { playing: m.playing.clone(), reveal: s.reveal.clone(), whispers };
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
        let order: Vec<&Game> = m.sorted(None).into_iter().filter(|i| i.phase.playable() && !i.alone()).collect();
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

/// The account that plays instances alone that name none: the one the user chose, else the only Microsoft account,
/// else the one added last.
pub fn active_account(m: &Model) -> Option<Uuid> {
    m.active_account.filter(|id| m.account(*id).is_some()).or_else(|| {
        let mut microsoft = m.accounts.iter().filter(|a| a.kind == AccountKind::Microsoft);
        match (microsoft.next(), microsoft.next()) {
            (Some(only), None) => Some(only.id),
            _ => m.accounts.last().map(|a| a.id),
        }
    })
}

/// The account `made` is played with alone when none is named: its own default, else the [`active_account`].
pub fn solo_account(m: &Model, made: &Instance) -> Option<Uuid> {
    made.default_account.filter(|id| m.account(*id).is_some()).or_else(|| active_account(m))
}

/// Makes `id` the active account and keeps it.
pub(super) fn choose_account(m: &mut Model, id: Uuid, out: &mut Vec<Effect>) {
    if m.account(id).is_none() || m.active_account == Some(id) {
        return;
    }
    m.active_account = Some(id);
    out.push(Effect::Redraw(Redraw::All));
    out.push(Effect::SaveActiveAccount(id));
}

/// Makes `account` the default of instance `id`, or leaves it to the active account, and saves that.
pub(super) fn set_default_account(m: &mut Model, id: &str, account: Option<Uuid>, out: &mut Vec<Effect>) {
    if account.is_some_and(|a| m.account(a).is_none()) {
        return;
    }
    let Some(made) = m.instances.iter_mut().find(|made| made.id == id) else { return };
    if made.default_account == account {
        return;
    }
    made.default_account = account;
    out.push(Effect::Redraw(Redraw::All));
    out.push(Effect::SaveDefaultAccount { instance: id.to_owned(), account });
}

/// Plays instance `id` alone with `account`, else its [`solo_account`], as a normal game window; the game's key when there is one.
pub(super) fn play_instance(m: &mut Model, id: &str, account: Option<Uuid>, out: &mut Vec<Effect>) -> Option<String> {
    let picked = m.instance(id).and_then(|made| {
        let account = account.or_else(|| solo_account(m, made)).filter(|a| m.account(*a).is_some())?;
        let key = fleets::solo_key(made, account);
        let shown = m.games.get(&key).is_some_and(|g| g.phase.is_active());
        Some((key, account, shown))
    });
    let Some((key, account, running)) = picked else {
        out.push(if m.accounts.is_empty() { Effect::AddAccount } else { Effect::ShowInstance(id.to_owned()) });
        return None;
    };
    if running {
        out.push(Effect::ShowInstance(id.to_owned()));
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

/// Plays `instance` as another start of Manycraft or a shortcut asked, and brings the manager up when it cannot or the
/// game already runs; waits for the first load when the data is not there yet.
pub(super) fn request_play(m: &mut Model, instance: String, account: Option<String>, out: &mut Vec<Effect>) {
    if !m.loaded {
        m.session.waiting_plays.push((instance, account));
        return;
    }
    let account = account.as_deref().and_then(|id| id.parse().ok());
    let from = out.len();
    let started = play_instance(m, &instance, account, out);
    if started.is_none() || out[from..].iter().any(|e| matches!(e, Effect::ShowInstance(_))) {
        show_grid(m, out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::effect::Effect;
    use super::super::super::event::{Event, Report};
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
    fn a_stopping_game_is_not_shown() {
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.games.get_mut(&key).unwrap().stopping = true;
        let mut fx = Vec::new();
        super::play(&mut m, &key, &mut fx);
        assert!(m.playing.is_none() && fx.is_empty());
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
    fn an_instance_never_played_plays_with_the_only_microsoft_account_else_the_newest() {
        use crate::core::account::Account;
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let made = m.instances[0].clone();
        let add = |m: &mut super::super::super::model::Model, n: u128, kind: AccountKind| m.accounts.push(Account { id: uuid::Uuid::from_u128(n), name: format!("A{n}"), kind, proxy: None, color: None });
        assert_eq!(super::solo_account(&m, &made), Some(m.accounts[0].id), "the only account");
        add(&mut m, 20, AccountKind::Offline);
        assert_eq!(super::solo_account(&m, &made), Some(uuid::Uuid::from_u128(20)), "the account added last");
        add(&mut m, 30, AccountKind::Microsoft);
        add(&mut m, 40, AccountKind::Offline);
        assert_eq!(super::solo_account(&m, &made), Some(uuid::Uuid::from_u128(30)), "the only Microsoft account beats newer offline ones");
        add(&mut m, 50, AccountKind::Microsoft);
        assert_eq!(super::solo_account(&m, &made), Some(uuid::Uuid::from_u128(50)), "with two Microsoft accounts the newest");
        let mut played = made.clone();
        played.last_account = Some(uuid::Uuid::from_u128(20));
        assert_eq!(super::solo_account(&m, &played), Some(uuid::Uuid::from_u128(50)), "the last account no longer decides");
        m.active_account = Some(uuid::Uuid::from_u128(40));
        assert_eq!(super::solo_account(&m, &played), Some(uuid::Uuid::from_u128(40)), "the active account wins over the guess");
        played.default_account = Some(uuid::Uuid::from_u128(20));
        assert_eq!(super::solo_account(&m, &played), Some(uuid::Uuid::from_u128(20)), "the instance's default wins over the active account");
        played.default_account = Some(uuid::Uuid::from_u128(99));
        m.active_account = Some(uuid::Uuid::from_u128(98));
        assert_eq!(super::solo_account(&m, &played), Some(uuid::Uuid::from_u128(50)), "removed accounts fall back to the guess");
    }

    fn play_alone(m: &mut super::super::super::model::Model, s: &crate::core::settings::Settings) -> (String, Vec<Effect>) {
        let alone = uuid::Uuid::from_u128(8);
        m.accounts.push(crate::core::account::Account { id: alone, name: "Solo".into(), kind: AccountKind::Offline, proxy: None, color: None });
        m.active_account = Some(alone);
        let instance = m.instances[0].id.clone();
        let fx = run(m, s, std::time::Instant::now(), Event::PlayInstance { instance, account: None });
        let key = m.games.values().find(|g| g.alone()).expect("a game played alone").key.clone();
        (key, fx)
    }

    #[test]
    fn a_game_played_alone_starts_as_a_normal_window_and_never_swaps_in() {
        let mut s = settings();
        s.phone.enabled = true;
        let (mut m, batch_key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let (key, fx) = play_alone(&mut m, &s);
        assert!(fx.iter().any(|e| matches!(e, Effect::Start { target, .. } if target.batch.is_none())));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Show { .. } | Effect::HideManager | Effect::Place(_))));
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 7 });
        let fx = report(&mut m, &s, state(&key, crate::core::ipc::StateKind::Menu, None, None));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Show { .. } | Effect::HideManager | Effect::Place(_))), "{fx:?}");
        assert!(hides(&fx).is_empty());
        let fx = run(&mut m, &s, std::time::Instant::now(), Event::Play(key.clone()));
        assert!(fx.is_empty() && m.playing.is_none(), "Play never swaps a game played alone in");
        let instance = m.instances[0].id.clone();
        let fx = run(&mut m, &s, std::time::Instant::now(), Event::PlayInstance { instance, account: None });
        assert!(!fx.iter().any(|e| matches!(e, Effect::Start { .. } | Effect::Show { .. })), "playing it again leaves the running game be");
        assert!(fx.iter().any(|e| matches!(e, Effect::ShowInstance(_))), "and shows where it runs");
        let instance = m.instances[0].id.clone();
        m.loaded = true;
        let fx = report(&mut m, &s, Report::PlayRequest { instance, account: None });
        assert!(fx.iter().any(|e| matches!(e, Effect::ShowManager)) && !fx.iter().any(|e| matches!(e, Effect::Start { .. })), "a shortcut to it brings the manager up where it runs");
        let fx = report(&mut m, &s, Report::Mod { key: key.clone(), message: FromMod::Closing });
        assert!(m.games[&key].stopping && !fx.iter().any(|e| matches!(e, Effect::Closed(_) | Effect::ShowManager)), "quitting it is a normal quit, nothing to undo");
        m.games.get_mut(&key).unwrap().stopping = false;
        let fx = report(&mut m, &s, message(&key, FromMod::Hotkey { action: HotkeyAction::Grid }));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Show { .. } | Effect::Send { .. } | Effect::ShowManager)), "its hotkeys switch and hide nothing");
        let fx = report(&mut m, &s, message(&key, FromMod::Chat { kind: crate::core::ipc::ChatKind::Whisper, from: Some("Friend".into()), text: "hi".into() }));
        assert!(m.games[&key].unread_whispers == 0 && m.session.whispers.is_empty(), "a whisper in a window the player looks at is not news");
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        let fx = report(&mut m, &s, state(&key, crate::core::ipc::StateKind::Disconnected, Some("mc.example"), Some(crate::core::ipc::DisconnectCause::Banned)));
        assert!(m.session.alerts.is_empty() && m.session.phone.next_due().is_none(), "its trouble shows on its instance page, not as an alert or on the phone");
        m.games.get_mut(&key).unwrap().phase = Phase::Playing { server: "mc.example".into(), since: std::time::Instant::now() };
        report(&mut m, &s, state(&key, crate::core::ipc::StateKind::Disconnected, Some("mc.example"), Some(crate::core::ipc::DisconnectCause::ServerDown)));
        assert!(m.session.phone.next_due().is_none(), "nor does a drop it reconnects from");
        m.playing = Some(batch_key.clone());
        let fx = run(&mut m, &s, std::time::Instant::now(), Event::Step(HotkeyAction::Next));
        assert!(!fx.iter().any(|e| matches!(e, Effect::Show { key: k, .. } if *k == key)), "stepping skips it");
    }

    #[test]
    fn a_game_played_alone_that_was_picked_up_again_stays_visible() {
        let s = settings();
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let (key, _) = play_alone(&mut m, &s);
        m.session.adopted.insert(key.clone());
        let env = super::super::Env::new(std::time::Instant::now(), 1_700_000_000, 7, &s, true);
        let fx = super::super::reduce(&mut m, &env, Event::Reports(vec![Report::Connected { key: key.clone(), pid: 7 }]));
        assert!(hides(&fx).is_empty());
        assert!(!m.session.unplaced.contains(&key));
        let plan = super::super::previews::plan(&m, crate::core::settings::PreviewRate::Normal);
        assert!(!plan.contains_key(&key), "no preview is captured for it");
        m.games.get_mut(&key).unwrap().phase = Phase::Crashed { code: Some(1) };
        assert_eq!(m.attention_count(None), 0, "its crash shows on its instance page, not in the grid's count");
    }

    #[test]
    fn the_active_account_and_an_instance_default_are_chosen_and_kept() {
        let s = settings();
        let (mut m, _) = setup(Phase::Stopped, AccountKind::Offline, &[]);
        let id = m.accounts[0].id;
        let fx = run(&mut m, &s, std::time::Instant::now(), Event::ChooseAccount(id));
        assert_eq!(m.active_account, Some(id));
        assert!(fx.iter().any(|e| matches!(e, Effect::SaveActiveAccount(a) if *a == id)));
        assert!(run(&mut m, &s, std::time::Instant::now(), Event::ChooseAccount(id)).is_empty(), "choosing it again changes nothing");
        assert!(run(&mut m, &s, std::time::Instant::now(), Event::ChooseAccount(uuid::Uuid::from_u128(99))).is_empty(), "an unknown account is ignored");
        let instance = m.instances[0].id.clone();
        let fx = run(&mut m, &s, std::time::Instant::now(), Event::SetDefaultAccount { instance: instance.clone(), account: Some(id) });
        assert_eq!(m.instances[0].default_account, Some(id));
        assert!(fx.iter().any(|e| matches!(e, Effect::SaveDefaultAccount { instance: i, account: Some(a) } if *i == instance && *a == id)));
        assert!(run(&mut m, &s, std::time::Instant::now(), Event::SetDefaultAccount { instance: instance.clone(), account: Some(uuid::Uuid::from_u128(99)) }).is_empty());
        let fx = run(&mut m, &s, std::time::Instant::now(), Event::SetDefaultAccount { instance: instance.clone(), account: None });
        assert_eq!(m.instances[0].default_account, None);
        assert!(fx.iter().any(|e| matches!(e, Effect::SaveDefaultAccount { account: None, .. })));
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

