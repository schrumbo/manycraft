//! The one place the model changes: [`reduce`] takes an event, changes the model and returns the effects, without IO or clocks.

mod data;
mod fleet;
mod games;
mod headless;
mod notify;
mod playtime;
mod previews;
pub mod usage;
mod quit;
mod said;
mod screen;
mod show;
mod slim;

use std::cell::Cell;
use std::time::Instant;

use super::effect::{Effect, Redraw};
use super::event::{Event, Report};
use super::model::{Game, Model, Phase};
use super::phase::{self, Why};
use crate::core::ipc::{ChatKind, FromMod};
use crate::core::settings::Settings;

pub use notify::attention_batch;
#[cfg(test)]
pub use notify::attention_tag;
pub use quit::{Protected, protected};
pub use show::{active_account, solo_account, solo_pick};

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
    let mut launched = false;
    match event {
        Event::Reports(reports) => {
            launched = reports.iter().any(|r| matches!(r, Report::Launched { key, .. } if model.games.get(key).is_some_and(Game::alone)));
            let freed = reports.iter().any(|r| matches!(r, Report::Exited { .. } | Report::Connected { .. }));
            games::reports(model, env, reports, &mut out);
            if freed && fleet::memory_waiting(model) {
                fleet::ask_memory(model, &mut out);
            }
        }
        Event::ReconnectDue { key, generation } => games::reconnect_due(model, &key, generation, &mut out),
        Event::ProbeDue { server } => games::probe_due(model, &server, &mut out),
        Event::RestartDue { key, generation } => games::restart_due(model, &key, generation, &mut out),
        Event::LinkLostDue { key } => games::link_lost_due(model, &key, &mut out),
        Event::JoinDue { key, generation, lines, delay } => games::join_due(model, key, generation, lines, delay, &mut out),
        Event::CaptureDue { key, generation } => headless::capture_due(model, &key, generation, &mut out),
        Event::WaitingStartDue { key } => {
            if model.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::NoNetwork | Phase::SignInWait { .. }) && !i.stopping) {
                fleet::start_keys(model, std::slice::from_ref(&key), &mut out);
            }
        }
        Event::AlertDue { batch } => notify::flush_alert(model, env, &batch, &mut out),
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
        Event::InstanceChanged(made) => {
            if let Some(slot) = model.instances.iter_mut().find(|i| i.id == made.id) {
                *slot = crate::core::instance::Instance { default_account: slot.default_account, last_account: slot.last_account, group: slot.group.clone(), ..made };
                out.push(Effect::Redraw(Redraw::All));
            }
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
        Event::Stop(keys) => fleet::stop_or_kill(model, keys, &mut out),
        Event::StopOverdue { key, generation } => fleet::stop_overdue(model, &key, generation, &mut out),
        Event::TakeOver(key) => fleet::take_over(model, &key, env.now, &mut out),
        Event::StopBatch(batch) => {
            let stopped = fleet::stoppable(model, Some(&batch));
            fleet::stop_batch(model, &batch, &mut out);
            if !stopped.is_empty() {
                out.push(Effect::Stopped(stopped));
            }
        }
        Event::StopAll => {
            let stopped: Vec<String> = fleet::stoppable(model, None).into_iter().filter(|k| !model.games.get(k).is_some_and(Game::alone)).collect();
            for id in owners(model).into_iter().filter(|o| crate::core::fleet::solo_instance(o).is_none()) {
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
        Event::ClickSlot { key, menu, slot, click } => screen::click(model, key, menu, slot, click, &mut out),
        Event::CloseMenu { key, menu } => screen::close(model, key, menu, &mut out),
        Event::LookAtScreen(key) => screen::look(model, key, &mut out),
        Event::Respawn(key) => {
            if model.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Dead { .. })) {
                out.push(Effect::Send { key, message: crate::core::ipc::ToMod::Respawn });
            }
        }
        Event::Capture(keys) => headless::capture(model, &keys, &mut out),
        Event::AnswerPack { keys, accept } => headless::answer(model, &keys, accept, false, &mut out),
        Event::AnswerRules { keys, accept } => headless::answer(model, &keys, accept, true, &mut out),
        Event::Play(key) => show::play(model, &key, &mut out),
        Event::PlayInstance { instance, account } => {
            show::play_instance(model, &instance, account, &mut out);
        }
        Event::QuickPlay { instance, account, quick } => show::quick_play(model, &instance, account, quick, &mut out),
        Event::Raise(key) => {
            if model.games.get(&key).is_some_and(|g| g.alone() && g.phase.is_active()) {
                out.push(Effect::Raise(key));
            }
        }
        Event::RequestPlay { instance, account } => show::request_play(model, instance, account, &mut out),
        Event::ChooseAccount(id) => show::choose_account(model, id, &mut out),
        Event::SetDefaultAccount { instance, account } => show::set_default_account(model, &instance, account, &mut out),
        Event::SetGroup { instance, group } => show::set_group(model, &instance, group, &mut out),
        Event::ShowGrid => show::show_grid(model, &mut out),
        Event::Step(action) => show::step(model, action, &mut out),
        Event::SendChat { key, text } => {
            if model.games.get(&key).is_some_and(|i| i.phase.can_chat()) {
                out.push(Effect::Send { key, message: crate::core::ipc::ToMod::ChatSend { text } });
            }
        }
        Event::Say { keys, text } => said::say(model, &keys, text, &mut out),
        Event::ReleaseDue { key } => said::retry(model, &key, &mut out),
        Event::HoldExpired { key, id, ticket } => said::expire(model, &key, id, ticket, &mut out),
        Event::Recolor { ids, color } => {
            for account in model.accounts.iter_mut().filter(|account| ids.contains(&account.id)) {
                account.color = Some(color);
            }
            out.push(Effect::Redraw(Redraw::All));
            out.push(Effect::SaveColor { ids, color });
        }
        Event::Visible(keys) => games::visible(model, env, keys, &mut out),
        Event::Watch(watch) => model.session.watch = watch,
        Event::ShotsDue { generation } => previews::due(model, generation, &mut out),
        Event::UsageDue => usage::due(model, &mut out),
        Event::PlaytimeDue => playtime::due(model, env.now, &mut out),
        Event::Usage(readings) => usage::read(model, env, readings, &mut out),
        Event::TellGames => games::tell_games(model, env, &mut out),
        Event::CloseWindow => quit::close_window(model, env.settings.tray_explained, &mut out),
        Event::TrayExplained => {
            out.push(Effect::RememberTrayExplained);
            quit::close_window(model, true, &mut out);
        }
        Event::Quit => quit::ask(model, &mut out),
        Event::QuitChosen { keep } => out.push(Effect::Quit { keep }),
        Event::RestartForUpdate => quit::restart_for_update(model, &mut out),
        Event::Updates(updates) => {
            if model.updates != *updates {
                model.updates = *updates;
                out.push(Effect::Redraw(Redraw::All));
            }
        }
    }
    fleet::finish_take_overs(model, env.now, &mut out);
    slim::settle(model, env, launched, &mut out);
    if !model.session.runs.is_empty() {
        let games = &model.games;
        model.session.runs.retain(|_, keys| keys.iter().any(|k| games.get(k).is_some_and(|g| g.phase.waiting())));
    }
    previews::replan(model, env.settings.preview_rate, &mut out);
    usage::arm(model, &mut out);
    playtime::settle(model, env.now, &mut out);
    out
}

fn batch_ids(m: &Model) -> Vec<String> {
    m.batches.iter().map(|b| b.id.clone()).collect()
}

fn owners(m: &Model) -> Vec<String> {
    let mut ids = batch_ids(m);
    for g in m.games.values() {
        if !ids.contains(&g.batch_id) {
            ids.push(g.batch_id.clone());
        }
    }
    ids
}

/// Moves game `key` to `to` when the transition table allows it for `why`; otherwise logs and leaves it.
fn go(m: &mut Model, key: &str, to: Phase, why: Why, out: &mut Vec<Effect>) -> bool {
    let Some(i) = m.games.get_mut(key) else { return false };
    if !phase::allowed(&i.phase, &to, why) {
        out.push(Effect::Log(format!("phase: {key} stays {:?}, refused {to:?} by {why:?}", i.phase)));
        return false;
    }
    if playtime::counts(&i.phase) != playtime::counts(&to) {
        m.session.playtime_dirty = true;
    }
    i.phase = to;
    said::moved(m, key, out);
    true
}

/// How much of the views a batch of reports touches: preview notices and public chat of games that talk touch at most the chat.
fn redraw(m: &Model, env: &Env, reports: &[Report]) -> Redraw {
    let mut chat = false;
    let mut outcome = false;
    for report in reports {
        match report {
            Report::Mod { key, message: FromMod::Preview { .. } } if !m.hint(key).link_lost => {}
            Report::Mod { message: FromMod::Map { .. } | FromMod::Menu { .. } | FromMod::Titles { .. }, .. } => {}
            Report::Mod { message: FromMod::ChatOutcome { .. }, .. } => outcome = true,
            Report::Mod { key, message: FromMod::Chat { .. } } if !m.hint(key).link_lost => {
                if m.games.get(key).is_some_and(|g| g.connection) && m.session.visible.contains(key) {
                    return Redraw::All;
                }
                chat = true;
            }
            _ => return Redraw::All,
        }
    }
    if outcome || (chat && env.settings.merged_chat) { Redraw::Chat } else { Redraw::Nothing }
}

/// Whether a start of `owner` can run now: it is known and, when it has accounts, its games are built.
pub(super) fn startable(m: &Model, owner: &str) -> bool {
    m.target(owner).is_some() && (m.batch(owner).is_none_or(|b| b.accounts.is_empty()) || m.games.values().any(|g| g.batch_id == owner))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
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
        Report::Mod { key: key.into(), message: FromMod::State { kind, server: server.map(str::to_owned), reason: None, cause, offer: None } }
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
        run(m, s, Instant::now(), Event::Loaded(Box::new(Loaded { data, logs: Vec::new(), problems: Vec::new(), recovered: None, keep: false, adopted, resume: Vec::new(), measured: HashMap::new(), playtime: HashMap::new() })))
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
    fn a_saved_instance_replaces_only_its_own_entry() {
        use super::testing::{run, settings, setup};
        use crate::core::account::AccountKind;
        use crate::core::instance::Instance;

        let s = settings();
        let (mut m, _) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let mut saved = m.instances[0].clone();
        saved.name = "Renamed".into();
        let fx = run(&mut m, &s, Instant::now(), Event::InstanceChanged(saved.clone()));
        assert_eq!(m.instances, vec![saved]);
        assert!(fx.iter().any(|e| matches!(e, Effect::Redraw(Redraw::All))));
        let stranger = Instance::new("x", "26.3", crate::core::instance::Loader::default());
        assert!(run(&mut m, &s, Instant::now(), Event::InstanceChanged(stranger)).is_empty());
        assert_eq!(m.instances.len(), 1);
    }

    #[test]
    fn dead_accounts_still_chat_and_a_line_naming_one_merges() {
        use super::testing::{report, run, settings, setup};
        use crate::core::account::AccountKind;

        let s = settings();
        let (mut m, dead) = setup(Phase::Dead { server: Some("srv".into()), reason: String::new() }, AccountKind::Offline, &[]);
        let mut other = m.games[&dead].clone();
        other.key = "other".into();
        other.name = "AltTwo".into();
        other.phase = Phase::Playing { server: "srv".into(), since: Instant::now() };
        m.games.insert(other.key.clone(), other);
        let line = |key: &str| Report::Mod { key: key.into(), message: FromMod::Chat { kind: ChatKind::System, from: None, text: "AltOne was slain".into(), runs: Vec::new() } };
        report(&mut m, &s, line("other"));
        report(&mut m, &s, line(&dead));
        assert_eq!(m.chat.len(), 1);
        assert_eq!(m.chat[0].also.len(), 1);
        report(&mut m, &s, Report::Mod { key: "other".into(), message: FromMod::Chat { kind: ChatKind::Player, from: Some("Notch".into()), text: "§chi".into(), runs: Vec::new() } });
        let styled = &m.chat.last().unwrap();
        assert_eq!((styled.text.as_str(), styled.runs[0].text.as_str(), styled.runs[1].color), ("hi", "Notch: ", Some(0xFF5555)));
        let fx = run(&mut m, &s, Instant::now(), Event::SendChat { key: dead.clone(), text: "hi".into() });
        assert!(fx.iter().any(|e| matches!(e, Effect::Send { key, .. } if *key == dead)));
        m.games.get_mut(&dead).unwrap().phase = Phase::Dead { server: None, reason: String::new() };
        assert!(run(&mut m, &s, Instant::now(), Event::SendChat { key: dead, text: "hi".into() }).is_empty());
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
        let chat = |instance: &str, kind: ChatKind| message(instance, FromMod::Chat { kind, from: None, text: "hi".into(), runs: Vec::new() });
        let notice = |instance: &str| message(instance, FromMod::Preview { seq: 2, w: 1, h: 1 });
        let drawn = |reports: &[Report]| redraw(&m, &env, reports);
        assert_eq!(drawn(&[notice("redraw-a"), notice("redraw-b")]), Redraw::Nothing);
        assert_eq!(drawn(&[notice("redraw-a"), chat("redraw-b", ChatKind::Player), chat("redraw-a", ChatKind::System)]), Redraw::Chat);
        let playing = message("redraw-a", FromMod::State { kind: crate::core::ipc::StateKind::Playing, server: None, reason: None, cause: None, offer: None });
        assert_eq!(drawn(&[notice("redraw-a"), playing]), Redraw::All);
        assert_eq!(drawn(&[notice(&lost)]), Redraw::All);
        assert_eq!(drawn(&[chat(&lost, ChatKind::Player)]), Redraw::All);
    }
}