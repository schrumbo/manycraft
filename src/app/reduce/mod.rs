//! The one place the model changes: [`reduce`] takes an event, changes the model and returns the effects, without IO or clocks.

mod data;
mod fleet;
mod games;
mod notify;
mod quit;
mod show;

use std::cell::Cell;
use std::time::Instant;

use super::effect::{Effect, Redraw};
use super::event::{Event, Report};
use super::model::{Model, Phase};
use super::phase::{self, Why};
use crate::core::ipc::{ChatKind, FromMod};
use crate::core::settings::Settings;

pub use notify::{WHISPER_TAG, attention_tag};
pub use show::solo_account;

/// What an event needs from outside the model: the time, chance and the settings.
pub struct Env<'a> {
    pub now: Instant,
    /// Seconds since 1970.
    pub unix: i64,
    pub settings: &'a Settings,
    /// Whether the windows of new games are put in the manager's place once they exist.
    pub place_windows: bool,
    seed: Cell<u64>,
}

impl<'a> Env<'a> {
    /// The world at `now`; `seed` decides every jitter the event draws.
    pub fn new(now: Instant, unix: i64, seed: u64, settings: &'a Settings, place_windows: bool) -> Self {
        Self { now, unix, settings, place_windows, seed: Cell::new(seed) }
    }

    /// The next number in `[0, 1)`, the same for the same seed.
    pub fn unit(&self) -> f64 {
        let mut x = self.seed.get().wrapping_add(0x9e37_79b9_7f4a_7c15);
        self.seed.set(x);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Applies `event` to `model` and returns what the front end has to do, in order.
pub fn reduce(model: &mut Model, env: &Env, event: Event) -> Vec<Effect> {
    let mut out = Vec::new();
    match event {
        Event::Reports(reports) => {
            let freed = reports.iter().any(|r| matches!(r, Report::Exited { .. } | Report::Connected { .. }));
            games::reports(model, env, reports, &mut out);
            if freed && fleet::memory_waiting(model) {
                fleet::ask_memory(model, &mut out);
            }
        }
        Event::ReconnectDue { key, generation } => games::reconnect_due(model, &key, generation, &mut out),
        Event::RestartDue { key, generation } => games::restart_due(model, &key, generation, &mut out),
        Event::JoinDue { key, generation, lines, delay } => games::join_due(model, key, generation, lines, delay, &mut out),
        Event::WaitingStartDue { key } => {
            if model.games.get(&key).is_some_and(|i| i.phase == Phase::NoNetwork && !i.stopping) {
                fleet::start_keys(model, std::slice::from_ref(&key), &mut out);
            }
        }
        Event::AlertDue { batch } => notify::flush_alert(model, env, &batch, &mut out),
        Event::WhispersDue => notify::flush_whispers(model, env, &mut out),
        Event::PhoneDue { due } => {
            if model.session.phone_at == Some(due) {
                model.session.phone_at = None;
            }
            notify::flush_phone(model, env, &mut out);
        }
        Event::Loaded(loaded) => data::loaded(model, env, *loaded, &mut out),
        Event::Placed { key } => {
            model.session.unplaced.remove(&key);
        }
        Event::NotShown(unshown) => show::not_shown(model, *unshown, &mut out),
        Event::InstanceAdded(made) => {
            model.instances.push(made);
            out.push(Effect::Redraw(Redraw::All));
        }
        Event::InstanceRemoved(id) => {
            model.instances.retain(|made| made.id != id);
            out.push(Effect::Redraw(Redraw::All));
        }
        Event::Notice(text) => model.session.notice = Some(text),
        Event::Error(text) => {
            model.error = Some(text);
            out.push(Effect::Redraw(Redraw::All));
        }
        Event::PhoneOutcome(trouble) => {
            if model.phone_trouble != trouble {
                model.phone_trouble = trouble;
                out.push(Effect::Redraw(Redraw::All));
            }
        }
        Event::Start(keys) => fleet::start_keys(model, &keys, &mut out),
        Event::StartMany(keys) => {
            let keys = fleet::signed_in(model, &keys);
            fleet::start_keys(model, &keys, &mut out);
        }
        Event::StartBatch(batch) if !startable(model, &batch) => model.session.waiting_starts.push((batch, env.now)),
        Event::StartBatch(batch) => fleet::launch(model, &batch, None, fleet::Kick::Plain, &mut out),
        Event::StartAll => {
            for id in batch_ids(model) {
                fleet::launch(model, &id, None, fleet::Kick::Bulk, &mut out);
            }
        }
        Event::Stop(keys) => fleet::stop_keys(model, keys, &mut out),
        Event::StopBatch(batch) => {
            let stopped = fleet::stoppable(model, Some(&batch));
            fleet::stop_batch(model, &batch, &mut out);
            if !stopped.is_empty() {
                out.push(Effect::Stopped(stopped));
            }
        }
        Event::StopAll => {
            let stopped = fleet::stoppable(model, None);
            for id in batch_ids(model) {
                fleet::stop_batch(model, &id, &mut out);
            }
            if !stopped.is_empty() {
                out.push(Effect::Stopped(stopped));
            }
        }
        Event::StartAgain(keys) => fleet::start_again(model, &keys, &mut out),
        Event::StartAnyway(batch) => fleet::start_anyway(model, batch.as_deref(), &mut out),
        Event::CancelStart(batch) => fleet::cancel_start(model, batch.as_deref(), &mut out),
        Event::Memory(memory) => fleet::memory(model, memory, &mut out),
        Event::MemoryDue => {
            model.session.memory_timer = false;
            if fleet::memory_waiting(model) {
                fleet::ask_memory(model, &mut out);
            }
        }
        Event::Restart(keys) => {
            let keys = fleet::signed_in(model, &keys);
            fleet::restart_keys(model, &keys, &mut out);
        }
        Event::Reconnect(key) => fleet::reconnect(model, &key, &mut out),
        Event::ReconnectMany(keys) => fleet::reconnect_each(model, &keys, &mut out),
        Event::Respawn(key) => {
            if model.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Dead { .. })) {
                out.push(Effect::Send { key, message: crate::core::ipc::ToMod::Respawn });
            }
        }
        Event::Play(key) => show::play(model, &key, &mut out),
        Event::PlayInstance { instance, account } => {
            show::play_and_show(model, env, &instance, account, &mut out);
        }
        Event::RequestPlay { instance, account } => show::request_play(model, env, instance, account, &mut out),
        Event::ShowGrid => show::show_grid(model, &mut out),
        Event::Step(action) => show::step(model, action, &mut out),
        Event::SendChat { key, text } => {
            if model.games.get(&key).is_some_and(|i| i.phase.on_server()) {
                out.push(Effect::Send { key, message: crate::core::ipc::ToMod::ChatSend { text } });
            }
        }
        Event::Broadcast { batch, text } => {
            for i in model.games.values().filter(|i| i.batch_id == batch && i.phase.on_server()) {
                out.push(Effect::Send { key: i.key.clone(), message: crate::core::ipc::ToMod::ChatSend { text: text.clone() } });
            }
        }
        Event::ClearWhispers(batch) => {
            let batch = batch.as_deref();
            if model.games.values().any(|i| i.unread_whispers > 0 && batch.is_none_or(|b| i.batch_id == b)) {
                for i in model.games.values_mut().filter(|i| batch.is_none_or(|b| i.batch_id == b)) {
                    i.unread_whispers = 0;
                }
                out.push(Effect::Redraw(Redraw::All));
            }
        }
        Event::Recolor { ids, color } => {
            for account in model.accounts.iter_mut().filter(|account| ids.contains(&account.id)) {
                account.color = Some(color);
            }
            out.push(Effect::Redraw(Redraw::All));
            out.push(Effect::SaveColor { ids, color });
        }
        Event::Visible(keys) => games::visible(model, env, keys, &mut out),
        Event::TellGames => games::tell_games(model, env, &mut out),
        Event::CloseWindow => quit::close_window(model, env.settings.tray_explained, &mut out),
        Event::TrayExplained => {
            out.push(Effect::RememberTrayExplained);
            quit::close_window(model, true, &mut out);
        }
        Event::Quit => quit::ask(model, &mut out),
        Event::QuitChosen { keep } => out.push(Effect::Quit { keep }),
        Event::RestartForUpdate => quit::restart_for_update(model, &mut out),
    }
    if !model.session.runs.is_empty() {
        let games = &model.games;
        model.session.runs.retain(|_, keys| keys.iter().any(|k| games.get(k).is_some_and(|g| g.phase.waiting())));
    }
    out
}

fn batch_ids(m: &Model) -> Vec<String> {
    m.batches.iter().map(|b| b.id.clone()).collect()
}

/// Moves game `key` to `to` when the transition table allows it for `why`; otherwise logs and leaves it.
fn go(m: &mut Model, key: &str, to: Phase, why: Why, out: &mut Vec<Effect>) -> bool {
    let Some(i) = m.games.get_mut(key) else { return false };
    if !phase::allowed(&i.phase, &to, why) {
        out.push(Effect::Log(format!("phase: {key} stays {:?}, refused {to:?} by {why:?}", i.phase)));
        return false;
    }
    i.phase = to;
    true
}

/// How much of the views a batch of reports touches: preview notices and public chat of games that talk touch at most the chat.
fn redraw(m: &Model, env: &Env, reports: &[Report]) -> Redraw {
    let mut chat = false;
    for report in reports {
        match report {
            Report::Mod { key, message: FromMod::Preview { .. } } if !m.hint(key).link_lost => {}
            Report::Mod { key, message: FromMod::Chat { kind, .. } } if *kind != ChatKind::Whisper && !m.hint(key).link_lost => chat = true,
            _ => return Redraw::All,
        }
    }
    if chat && env.settings.merged_chat { Redraw::Chat } else { Redraw::Nothing }
}

/// Whether a start of `owner` can run now: it is known and, when it has accounts, its games are built.
pub(super) fn startable(m: &Model, owner: &str) -> bool {
    m.target(owner).is_some() && (m.batch(owner).is_none_or(|b| b.accounts.is_empty()) || m.games.values().any(|g| g.batch_id == owner))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::time::{Duration, Instant};

    use uuid::Uuid;

    use super::super::effect::Effect;
    use super::super::event::{Event, Loaded, Report};
    use super::super::model::{Game, Model, Phase};
    use super::{Env, reduce};
    use crate::core::account::{Account, AccountKind};
    use crate::core::batch::Batch;
    use crate::core::fleet::Adoption;
    use crate::core::instance::Instance;
    use crate::core::ipc::{DisconnectCause, FromMod, StateKind};
    use crate::core::settings::Settings;

    /// A model with one batch of one account whose game is in `phase`.
    pub fn setup(phase: Phase, kind: AccountKind, join: &[&str]) -> (Model, String) {
        let id = Uuid::from_u128(7);
        let made = Instance::new("b", "26.3", crate::core::instance::Loader::default());
        let mut batch = Batch::new("b", &made.id);
        batch.accounts = vec![id];
        batch.join_commands = join.iter().map(|s| (*s).to_owned()).collect();
        let key = crate::core::fleet::instance_key(&batch, id);
        let mut m = Model::default();
        m.accounts.push(Account { id, name: "AltOne".into(), kind, proxy: None, color: None });
        m.games.insert(key.clone(), Game { phase, ..Game::new(key.clone(), batch.id.clone(), made.id.clone(), id, "AltOne".into()) });
        m.batches.push(batch);
        m.instances.push(made);
        (m, key)
    }

    /// Settings as they come.
    pub fn settings() -> Settings {
        Settings::default()
    }

    /// Runs `event` at `now` with fixed chance.
    pub fn run(m: &mut Model, s: &Settings, now: Instant, event: Event) -> Vec<Effect> {
        let env = Env::new(now, 1_700_000_000, 7, s, false);
        reduce(m, &env, event)
    }

    /// Runs one report now.
    pub fn report(m: &mut Model, s: &Settings, report: Report) -> Vec<Effect> {
        run(m, s, Instant::now(), Event::Reports(vec![report]))
    }

    pub fn state(key: &str, kind: StateKind, server: Option<&str>, cause: Option<DisconnectCause>) -> Report {
        Report::Mod { key: key.into(), message: FromMod::State { kind, server: server.map(str::to_owned), reason: None, cause } }
    }

    /// The events delivered later, with their waits.
    pub fn later(fx: &[Effect]) -> Vec<(Duration, &Event)> {
        fx.iter().filter_map(|e| if let Effect::After { delay, event } = e { Some((*delay, &**event)) } else { None }).collect()
    }

    /// The account id of the only game.
    pub fn account(m: &Model) -> Uuid {
        m.accounts[0].id
    }

    /// Loads the model's own data again with `adopted` games still running.
    pub fn adopt(m: &mut Model, s: &Settings, adopted: Vec<Adoption>) -> Vec<Effect> {
        let data = Ok((m.accounts.clone(), m.batches.clone(), m.instances.clone()));
        run(m, s, Instant::now(), Event::Loaded(Box::new(Loaded { data, logs: Vec::new(), problems: Vec::new(), recovered: None, keep: false, adopted })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_draws_the_same_jitter() {
        let s = Settings::default();
        let now = Instant::now();
        let (a, b) = (Env::new(now, 0, 3, &s, false), Env::new(now, 0, 3, &s, false));
        let draws: Vec<f64> = (0..4).map(|_| a.unit()).collect();
        assert_eq!(draws, (0..4).map(|_| b.unit()).collect::<Vec<_>>());
        assert!(draws.iter().all(|u| (0.0..1.0).contains(u)));
        assert_ne!(draws[0], draws[1]);
    }

    #[test]
    fn preview_notices_and_public_chat_redraw_at_most_the_chat() {
        use super::testing::{setup, settings};
        use crate::core::account::AccountKind;

        let s = settings();
        let env = Env::new(Instant::now(), 0, 7, &s, false);
        let (mut m, lost) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.set_hint(&lost, |h| h.link_lost = true);
        let message = |instance: &str, message: FromMod| Report::Mod { key: instance.into(), message };
        let chat = |instance: &str, kind: ChatKind| message(instance, FromMod::Chat { kind, from: None, text: "hi".into() });
        let notice = |instance: &str| message(instance, FromMod::Preview { seq: 2, w: 1, h: 1 });
        let drawn = |reports: &[Report]| redraw(&m, &env, reports);
        assert_eq!(drawn(&[notice("redraw-a"), notice("redraw-b")]), Redraw::Nothing);
        assert_eq!(drawn(&[notice("redraw-a"), chat("redraw-b", ChatKind::Player), chat("redraw-a", ChatKind::System)]), Redraw::Chat);
        assert_eq!(drawn(&[chat("redraw-a", ChatKind::Player), chat("redraw-a", ChatKind::Whisper)]), Redraw::All);
        let playing = message("redraw-a", FromMod::State { kind: crate::core::ipc::StateKind::Playing, server: None, reason: None, cause: None });
        assert_eq!(drawn(&[notice("redraw-a"), playing]), Redraw::All);
        assert_eq!(drawn(&[notice(&lost)]), Redraw::All);
        assert_eq!(drawn(&[chat(&lost, ChatKind::Player)]), Redraw::All);
    }
}
