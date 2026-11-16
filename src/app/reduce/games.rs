use std::collections::HashSet;
use std::time::Duration;

use super::super::effect::{Effect, Redraw};
use super::super::event::{Event, Report};
use super::super::model::{ChatLine, Game, Model, Phase, rank};
use super::super::phase::{self, Why};
use super::super::session::{Alert, Frame, Reveal};
use super::{Env, fleet, go, notify, redraw, show};
use crate::core::account::AccountKind;
use crate::core::chat_text;
use crate::core::fleet as fleets;
use crate::core::history::{self, Entry, Kind};
use crate::core::i18n::tr;
use crate::core::ipc::{ChatKind, DisconnectCause, FromMod, HotkeyAction, StateKind, ToMod};
use crate::core::phone::{Incident, What};
use crate::core::reconnect::{self, Decision, Reaction};

const STABLE: Duration = Duration::from_secs(60);
const OFFSCREEN_PREVIEW: Duration = Duration::from_secs(30);
const KEPT_CHAT: usize = 5000;
const BACK_STAGGER: Duration = Duration::from_secs(1);
const BACK_WAIT_SECS: u64 = 2;
const WAITING_RETRY_SECS: u64 = 60;

/// What a batch of reports leaves to do once every report is in the model, in the order it is done.
#[derive(Default)]
pub(super) struct Fx {
    pub now: Vec<Effect>,
    pub frames: Vec<Effect>,
    pub histories: Vec<String>,
    pub stop: Vec<String>,
    pub closing: Vec<String>,
    pub check_proxy: Vec<Effect>,
    pub suspect_offline: bool,
    pub network_back: bool,
    pub later: Vec<Later>,
    pub alerts: Vec<(String, String, Alert)>,
    pub phone: Vec<(String, Incident)>,
    pub placed: Vec<String>,
    pub revealed: Option<Reveal>,
    pub game_focused: bool,
    pub show_grid: bool,
    pub plays: Vec<(String, Option<String>)>,
    pub hotkeys: Vec<HotkeyAction>,
}

/// How long a link to a game may be gone before its tile says so; a game that quits drops it a moment before it ends.
const LINK_PATIENCE: Duration = Duration::from_secs(2);

/// Whether `key` has a game whose loss of its link would be news.
fn linked(m: &Model, key: &str) -> bool {
    m.session.pids.contains_key(key) && m.games.get(key).is_some_and(|i| !i.stopping && i.phase != Phase::Stopped)
}

pub(super) fn link_lost_due(m: &mut Model, key: &str, out: &mut Vec<Effect>) {
    if m.session.lost.remove(key) && linked(m, key) {
        m.set_hint(key, |h| h.link_lost = true);
        out.push(Effect::Redraw(Redraw::All));
    }
}

/// Work a report leaves for later.
pub(super) enum Later {
    After(Duration, Event),
    Start(Vec<String>),
}

pub(super) fn reports(m: &mut Model, env: &Env, reports: Vec<Report>, out: &mut Vec<Effect>) {
    let frames_only = !reports.is_empty() && reports.iter().all(|r| matches!(r, Report::Frame { .. }));
    if !frames_only {
        out.push(Effect::Redraw(redraw(m, env, &reports)));
    }
    let mut fx = Fx::default();
    for report in reports {
        let key = report.key().map(str::to_owned);
        let before = key.as_ref().and_then(|k| m.games.get(k)).map(|i| (rank(&i.phase), i.phase.needs_attention()));
        let first_state = matches!(&report, Report::Mod { message: FromMod::State { .. }, .. }) && key.as_ref().is_some_and(|k| m.session.adopted.contains(k));
        let seeded = matches!(report, Report::Seed { .. }) || first_state;
        apply(m, env, report, &mut fx);
        let Some(key) = key else { continue };
        let Some(i) = m.games.get(&key) else { continue };
        let after = (rank(&i.phase), i.phase.needs_attention());
        if before.is_some_and(|b| b.0 != after.0) {
            m.order_seq += 1;
        }
        if after.1 && !seeded && !i.alone() && before.is_some_and(|b| !b.1) {
            fx.alerts.push((i.batch_id.clone(), i.name.clone(), Alert::of(&i.phase)));
            if let Some(what) = notify::phone_what(&i.phase) {
                fx.phone.push((i.batch_id.clone(), Incident::new(&key, &i.name, what, notify::phase_reason(&i.phase), i.phase.server())));
            }
        }
    }
    if frames_only {
        out.extend(fx.frames);
        return;
    }
    finish(m, env, fx, out);
}

fn finish(m: &mut Model, env: &Env, fx: Fx, out: &mut Vec<Effect>) {
    out.extend(fx.now);
    out.extend(fx.frames);
    for key in &fx.histories {
        let Some(i) = m.games.get(key) else { continue };
        let (Some(instance), Some(log)) = (m.instance(&i.instance_id), m.history.get(key)) else { continue };
        out.push(Effect::SaveHistory { path: history::path(instance, i.account_id), log: log.clone() });
    }
    if !fx.stop.is_empty() {
        out.push(Effect::Stop(fx.stop));
    }
    if !fx.closing.is_empty() {
        let closed: Vec<String> = fx.closing.iter().filter(|k| m.games.get(k.as_str()).is_some_and(|g| !g.stopping && !g.alone())).cloned().collect();
        fleet::stop_keys(m, fx.closing, out);
        out.extend(closed.into_iter().filter(|k| m.games.get(k).is_some_and(|g| g.stopping)).map(Effect::Closed));
    }
    out.extend(fx.check_proxy);
    if fx.suspect_offline {
        out.push(Effect::SuspectOffline);
    }
    if fx.network_back {
        out.push(Effect::NetworkBack);
    }
    for later in fx.later {
        match later {
            Later::After(delay, event) => out.push(Effect::After { delay, event: Box::new(event) }),
            Later::Start(keys) => fleet::start_keys(m, &keys, out),
        }
    }
    for (batch, name, kind) in fx.alerts {
        notify::queue_alert(m, batch, name, kind, out);
    }
    let phone: Vec<_> = fx.phone.into_iter().filter(|(batch, _)| fleets::solo_instance(batch).is_none()).collect();
    notify::queue_phone(m, env, phone, out);
    for key in fx.placed {
        out.push(Effect::Place(key));
    }
    if let Some(r) = fx.revealed {
        show::reveal(m, r, out);
    }
    if fx.game_focused {
        out.push(Effect::Follow(m.playing.clone()));
        out.push(Effect::HideManager);
    }
    if fx.show_grid {
        show::show_grid(m, out);
    }
    for (instance, account) in fx.plays {
        show::request_play(m, instance, account, out);
    }
    for action in fx.hotkeys {
        show::step(m, action, out);
    }
}

fn note(m: &mut Model, env: &Env, fx: &mut Fx, key: &str, kind: Kind, server: Option<String>, reason: &str) {
    let at = u64::try_from(env.unix).unwrap_or(0);
    history::push(m.history.entry(key.to_owned()).or_default(), Entry::at(at, kind, server, reason));
    if !fx.histories.iter().any(|k| k == key) {
        fx.histories.push(key.to_owned());
    }
}

fn apply(m: &mut Model, env: &Env, report: Report, fx: &mut Fx) {
    match report {
        Report::Preparing { batch, percent } => {
            m.preparing.insert(batch, percent);
        }
        Report::Prepared { batch } => {
            m.preparing.remove(&batch);
        }
        Report::Launched { key, plain } => {
            if m.games.get(&key).is_some_and(|i| !i.stopping) {
                let to = if plain { Phase::Plain } else { Phase::Starting { place: 0, total: 0 } };
                go(m, &key, to, Why::Launched, &mut fx.now);
            }
        }
        Report::Failed { key, reason, login, offline } => {
            if let Some(i) = m.games.get(&key) {
                m.preparing.remove(&i.batch_id);
            }
            let to = if login {
                Phase::LoginNeeded
            } else if offline {
                Phase::NoNetwork
            } else {
                Phase::Failed { reason }
            };
            if go(m, &key, to, Why::StartFailed, &mut fx.now) && offline && !login {
                fx.later.push(Later::After(waiting_retry(env), Event::WaitingStartDue { key }));
            }
            fx.suspect_offline |= offline;
        }
        Report::Reach { online } => on_reach(m, env, fx, online),
        Report::Seed { key, phase, attempts } => {
            if go(m, &key, phase, Why::Seed, &mut fx.now)
                && let Some(i) = m.games.get_mut(&key)
            {
                i.reconnect_attempts = attempts;
            }
        }
        Report::SignIn { account, valid } => {
            let keys: Vec<String> = m.games.values().filter(|i| i.account_id == account).map(|i| i.key.clone()).collect();
            for key in keys {
                let phase = &m.games[&key].phase;
                if !valid && matches!(phase, Phase::Stopped | Phase::Crashed { .. } | Phase::Failed { .. }) {
                    go(m, &key, Phase::LoginNeeded, Why::SignIn, &mut fx.now);
                } else if valid && *phase == Phase::LoginNeeded && !m.session.pids.contains_key(&key) {
                    go(m, &key, Phase::Stopped, Why::SignIn, &mut fx.now);
                }
            }
            m.order_seq += 1;
        }
        Report::Exited { key, code } => {
            on_exit(m, env, fx, key, code);
        }
        Report::Frame { key, seq, image } => {
            if m.games.get(&key).is_some_and(|i| i.phase.is_running()) {
                m.frames.insert(key.clone(), Frame { seq, live: true });
                fx.frames.push(Effect::ShowFrame { key, image, still: Some(false) });
            } else {
                fx.frames.push(Effect::DropImage(image));
            }
        }
        Report::Still { key, image } => {
            if m.games.contains_key(&key) && !m.frames.contains_key(&key) {
                m.frames.insert(key.clone(), Frame { seq: 0, live: false });
                fx.frames.push(Effect::ShowFrame { key, image, still: None });
            } else {
                fx.frames.push(Effect::DropImage(image));
            }
        }
        Report::ProxyChecked { key, fault } => {
            if m.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Connecting { .. } | Phase::Disconnected { .. })) {
                m.set_hint(&key, |h| h.proxy = fault);
            }
        }
        Report::Connected { key, pid } => {
            let alone = m.games.get(&key).is_some_and(Game::alone);
            m.session.lost.remove(&key);
            m.set_hint(&key, |h| h.link_lost = false);
            m.session.pids.insert(key.clone(), pid);
            m.session.shots.remove(&key);
            m.session.planned.remove(&key);
            if env.place_windows && !alone {
                m.session.unplaced.insert(key.clone());
            }
            for message in identity(m, env, &key) {
                fx.now.push(Effect::Send { key: key.clone(), message });
            }
            fx.now.push(Effect::Tint { key: key.clone(), pid });
            if let Some(path) = preview_path(m, &key).filter(|_| m.session.adopted.contains(&key)) {
                m.session.read_at.insert(key.clone(), env.now);
                fx.now.push(Effect::ReadPreview { key: key.clone(), path });
            }
            if !alone && m.session.adopted.contains(&key) && m.playing.as_deref() != Some(key.as_str()) {
                fx.now.push(Effect::Send { key: key.clone(), message: ToMod::Hide });
            }
            if m.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Starting { .. })) {
                go(m, &key, Phase::Menu, Why::Connected, &mut fx.now);
            }
        }
        Report::Reopen => fx.show_grid = true,
        Report::PlayRequest { instance, account } => fx.plays.push((instance, account)),
        Report::LinkLost { key } => {
            if linked(m, &key) {
                m.session.lost.insert(key.clone());
                fx.later.push(Later::After(LINK_PATIENCE, Event::LinkLostDue { key }));
            }
        }
        Report::Mod { key, message } => {
            m.session.lost.remove(&key);
            m.set_hint(&key, |h| h.link_lost = false);
            on_message(m, env, key, message, fx);
        }
    }
}

fn on_exit(m: &mut Model, env: &Env, fx: &mut Fx, key: String, code: Option<i32>) {
    m.set_hint(&key, |h| *h = Default::default());
    m.session.stale.remove(&key);
    fx.now.push(Effect::ReleasePreview(key.clone()));
    if m.games.contains_key(&key)
        && let Some(f) = m.frames.get_mut(&key)
    {
        if f.live {
            fx.frames.push(Effect::SaveStill(key.clone()));
        }
        f.live = false;
    }
    let s = &mut m.session;
    s.pids.remove(&key);
    s.usage.remove(&key);
    s.unplaced.remove(&key);
    s.joined.remove(&key);
    s.adopted.remove(&key);
    let unknown = s.foreign.remove(&key) && code.is_none();
    let ran = s.started.remove(&key).map(|t| env.now.saturating_duration_since(t));
    let Some(i) = m.games.get_mut(&key) else {
        let keys = fleet::drop_restart(m, &key);
        if !keys.is_empty() {
            fx.later.push(Later::Start(keys));
        }
        return;
    };
    *s.joins.entry(key.clone()).or_default() += 1;
    let crashed = !(i.stopping || code == Some(0) || unknown || matches!(i.phase, Phase::Stopped | Phase::LoginNeeded | Phase::Restarting { .. }));
    let relaunch = !i.stopping && matches!(i.phase, Phase::Restarting { .. });
    let restart = if i.stopping { s.restarts.remove(&key) } else { None };
    i.stopping = false;
    i.overdue = false;
    if m.playing.as_deref() == Some(key.as_str()) {
        fx.show_grid = true;
    }
    if relaunch {
        let generation = s.reconnects.get(&key).copied().unwrap_or_default();
        fx.later.push(Later::After(Duration::ZERO, Event::RestartDue { key, generation }));
        return;
    }
    if !crashed {
        if !matches!(i.phase, Phase::LoginNeeded | Phase::Restarting { .. }) {
            go(m, &key, Phase::Stopped, Why::Exit, &mut fx.now);
        }
        if let Some(group) = restart {
            m.session.restart_ready.entry(group).or_default().push(key);
            if let Some(keys) = fleet::restart_done(m, group) {
                fx.later.push(Later::Start(keys));
            }
        }
        return;
    }
    let server = server_of(&i.phase);
    let policy = &env.settings.crash_restart;
    if ran.is_some_and(|r| r >= STABLE) {
        s.crash_restarts.remove(&key);
    }
    let tried = s.crash_restarts.get(&key).copied().unwrap_or(0);
    let reason = tr("Unerwartet beendet");
    let banned = matches!(i.phase, Phase::Banned { .. });
    let to = match reconnect::restart(tried, policy, env.unit()) {
        Decision::Retry(_) if banned => Phase::Crashed { code },
        Decision::Retry(delay) => {
            s.crash_restarts.insert(key.clone(), tried + 1);
            let generation = s.reconnects.entry(key.clone()).or_default();
            *generation += 1;
            fx.later.push(Later::After(delay, Event::RestartDue { key: key.clone(), generation: *generation }));
            Phase::Restarting { attempt: tried + 1, of: policy.max_attempts }
        }
        Decision::Stop => Phase::Crashed { code },
    };
    go(m, &key, to, Why::Exit, &mut fx.now);
    note(m, env, fx, &key, Kind::Crashed, server, &reason);
}

fn join_lines_of(m: &Model, key: &str) -> Option<(Vec<String>, Duration)> {
    let batch = m.batch(&m.games.get(key)?.batch_id)?;
    let lines = batch.join_lines();
    (!lines.is_empty()).then(|| (lines, Duration::from_millis(u64::from(batch.join_delay_ms))))
}

fn on_state(m: &mut Model, env: &Env, fx: &mut Fx, key: String, kind: StateKind, server: Option<String>, reason: Option<String>, cause: Option<DisconnectCause>) {
    if m.session.adopted.remove(&key) {
        return seed_state(m, env, fx, &key, kind, server, reason, cause);
    }
    if kind != StateKind::Playing {
        *m.session.joins.entry(key.clone()).or_default() += 1;
    }
    let account = m.games.get(&key).and_then(|i| m.account(i.account_id));
    let offline = account.is_some_and(|a| a.kind == AccountKind::Offline);
    let direct = account.is_none_or(|a| a.proxy.is_none());
    let nearby = server.as_deref().is_some_and(local_server);
    let cut_off = m.offline && direct && !nearby;
    if matches!(kind, StateKind::Menu | StateKind::Playing) {
        m.set_hint(&key, |h| h.proxy = None);
    }
    if kind == StateKind::Disconnected
        && matches!(cause, None | Some(DisconnectCause::ServerDown | DisconnectCause::Auth | DisconnectCause::Unknown))
        && let Some(url) = m.games.get(&key).and_then(|i| m.account(i.account_id)).and_then(|a| a.proxy.clone())
    {
        fx.check_proxy.push(Effect::CheckProxy { key: key.clone(), url });
    }
    let join = if kind == StateKind::Playing { join_lines_of(m, &key) } else { None };
    let Some(i) = m.games.get_mut(&key) else { return };
    if i.stopping || i.phase == Phase::Stopped {
        return;
    }
    if !phase::mod_reports(&i.phase) {
        fx.now.push(Effect::Log(format!("phase: {key} stays {:?}, refused mod state {kind:?}", i.phase)));
        return;
    }
    let s = &mut m.session;
    let policy = &env.settings.reconnect;
    let server_text = server.clone().unwrap_or_default();
    let reason_text = reason.clone().unwrap_or_default();
    let mut event: Option<(Kind, Option<String>, String)> = None;
    let to = match kind {
        StateKind::Menu => {
            s.joined.remove(&key);
            Phase::Menu
        }
        StateKind::Connecting => Phase::Connecting { server: server_text },
        StateKind::Queue => Phase::Queue { server: server_text },
        StateKind::Playing => {
            if let Phase::Playing { since, .. } = i.phase {
                Phase::Playing { server: server_text, since }
            } else {
                s.started.entry(key.clone()).or_insert(env.now);
                if !matches!(i.phase, Phase::Dead { .. }) && !server_text.is_empty() && s.joined.insert(key.clone()) {
                    event = Some((Kind::Connected, server.clone(), String::new()));
                    fx.phone.push((i.batch_id.clone(), Incident::new(&key, &i.name, What::Back, "", server.as_deref())));
                    if let Some((lines, delay)) = join {
                        let generation = *s.joins.entry(key.clone()).or_default();
                        fx.later.push(Later::After(delay, Event::JoinDue { key: key.clone(), generation, lines, delay }));
                    }
                }
                Phase::Playing { server: server_text, since: env.now }
            }
        }
        StateKind::Dead => Phase::Dead { server, reason: reason_text },
        StateKind::Disconnected => {
            s.joined.remove(&key);
            if let Phase::Playing { since, .. } = i.phase
                && env.now.saturating_duration_since(since) >= STABLE
            {
                i.reconnect_attempts = 0;
            }
            let cause = cause.unwrap_or(DisconnectCause::Unknown);
            let generation = s.reconnects.entry(key.clone()).or_default();
            *generation += 1;
            fx.suspect_offline |= direct && !nearby && matches!(cause, DisconnectCause::ServerDown | DisconnectCause::Unknown);
            let reaction = match reconnect::react(cause, i.reconnect_attempts, policy, env.unit(), cut_off) {
                Reaction::Retry(_) | Reaction::WaitForNetwork if server.is_none() => Reaction::GiveUp,
                Reaction::SignInAgain if offline => Reaction::NotAllowed,
                other => other,
            };
            let kind = match cause {
                DisconnectCause::Banned => Kind::Banned,
                DisconnectCause::Kicked => Kind::Kicked,
                _ => Kind::Disconnected,
            };
            event = Some((kind, server.clone(), reason_text.clone()));
            match reaction {
                Reaction::Retry(delay) => {
                    let what = if cause == DisconnectCause::Kicked { What::Kicked } else { What::Dropped };
                    let said = if reason_text.trim().is_empty() { notify::cause_text(cause) } else { reason_text.clone() };
                    fx.phone.push((i.batch_id.clone(), Incident::new(&key, &i.name, what, &said, server.as_deref())));
                    i.reconnect_attempts += 1;
                    fx.later.push(Later::After(delay, Event::ReconnectDue { key: key.clone(), generation: *generation }));
                    Phase::Connecting { server: server_text }
                }
                Reaction::WaitForNetwork => {
                    fx.later.push(Later::After(waiting_retry(env), Event::ReconnectDue { key: key.clone(), generation: *generation }));
                    Phase::NetworkLost { server: server_text }
                }
                Reaction::GiveUp | Reaction::NotAllowed => Phase::Disconnected { server, reason: reason_text },
                Reaction::Banned => Phase::Banned { server, reason: reason_text },
                Reaction::SignInAgain => {
                    fx.stop.push(key.clone());
                    let crash = &env.settings.crash_restart;
                    let tried = s.crash_restarts.get(&key).copied().unwrap_or(0);
                    match reconnect::restart(tried, crash, 0.0) {
                        Decision::Retry(_) => {
                            s.crash_restarts.insert(key.clone(), tried + 1);
                            Phase::Restarting { attempt: tried + 1, of: crash.max_attempts }
                        }
                        Decision::Stop => Phase::LoginNeeded,
                    }
                }
            }
        }
    };
    go(m, &key, to, Why::Mod, &mut fx.now);
    if let Some((kind, server, reason)) = event {
        note(m, env, fx, &key, kind, server, &reason);
    }
}

fn on_reach(m: &mut Model, env: &Env, fx: &mut Fx, online: bool) {
    m.offline = !online;
    let (accounts, s, mut nth, mut start) = (&m.accounts, &mut m.session, 0, Vec::new());
    let mut moves = Vec::new();
    for i in m.games.values_mut().filter(|i| !i.stopping) {
        let direct = accounts.iter().find(|a| a.id == i.account_id).is_none_or(|a| a.proxy.is_none());
        match &i.phase {
            Phase::Connecting { server } if !online && direct && i.reconnect_attempts > 0 && !local_server(server) => {
                let generation = s.reconnects.entry(i.key.clone()).or_default();
                *generation += 1;
                fx.later.push(Later::After(waiting_retry(env), Event::ReconnectDue { key: i.key.clone(), generation: *generation }));
                i.reconnect_attempts -= 1;
                moves.push((i.key.clone(), Phase::NetworkLost { server: server.clone() }, Why::NetworkLost));
            }
            Phase::NetworkLost { server } if online => {
                let generation = s.reconnects.entry(i.key.clone()).or_default();
                *generation += 1;
                let delay = BACK_STAGGER * nth + reconnect::wait(&[BACK_WAIT_SECS], 0, 50, env.unit());
                nth += 1;
                fx.later.push(Later::After(delay, Event::ReconnectDue { key: i.key.clone(), generation: *generation }));
                moves.push((i.key.clone(), Phase::Connecting { server: server.clone() }, Why::NetworkBack));
            }
            Phase::NoNetwork if online => start.push(i.key.clone()),
            _ => {}
        }
    }
    for (key, to, why) in moves {
        go(m, &key, to, why, &mut fx.now);
    }
    if !start.is_empty() {
        fx.later.push(Later::Start(start));
    }
    fx.network_back |= online;
    m.order_seq += 1;
}

fn waiting_retry(env: &Env) -> Duration {
    reconnect::wait(&[WAITING_RETRY_SECS], 0, 20, env.unit())
}

/// Whether `server` is on this computer or in the home network, which works without the internet.
pub(super) fn local_server(server: &str) -> bool {
    let host = match server.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() && (!host.contains(':') || host.ends_with(']')) => host,
        _ => server,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || ip.segments()[0] & 0xfe00 == 0xfc00 || ip.segments()[0] & 0xffc0 == 0xfe80,
        Err(_) => host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".local"),
    }
}

/// The server a game in `phase` is on or was last on.
pub(super) fn server_of(phase: &Phase) -> Option<String> {
    phase.server().map(str::to_owned)
}

fn seed_state(m: &mut Model, env: &Env, fx: &mut Fx, key: &str, kind: StateKind, server: Option<String>, reason: Option<String>, cause: Option<DisconnectCause>) {
    let Some(i) = m.games.get(key) else { return };
    if i.stopping || !i.phase.is_running() {
        return;
    }
    if matches!(kind, StateKind::Menu | StateKind::Disconnected) {
        m.session.joined.remove(key);
    }
    let (server_text, reason) = (server.clone().unwrap_or_default(), reason.unwrap_or_default());
    let to = match kind {
        StateKind::Menu => Phase::Menu,
        StateKind::Connecting => Phase::Connecting { server: server_text },
        StateKind::Queue => Phase::Queue { server: server_text },
        StateKind::Playing => Phase::Playing { server: server_text, since: env.now },
        StateKind::Dead => Phase::Dead { server, reason },
        StateKind::Disconnected if cause == Some(DisconnectCause::Banned) => Phase::Banned { server, reason },
        StateKind::Disconnected => Phase::Disconnected { server, reason },
    };
    go(m, key, to, Why::Adopt, &mut fx.now);
}

fn on_message(m: &mut Model, env: &Env, key: String, message: FromMod, fx: &mut Fx) {
    let playing = m.playing.as_deref() == Some(key.as_str());
    if m.session.unplaced.contains(&key) {
        fx.placed.push(key.clone());
    }
    match message {
        FromMod::State { kind, server, reason, cause } => on_state(m, env, fx, key, kind, server, reason, cause),
        FromMod::Chat { from, text, runs, .. } => {
            let runs = if chat_text::needs_runs(&runs, &text) { chat_text::styled(&runs, &text) } else { Vec::new() };
            let text = if text.contains('§') { chat_text::plain(&chat_text::styled(&[], &text)) } else { text };
            let (account, batch) = m.games.get(&key).map(|i| (i.name.clone(), i.batch_id.clone())).unwrap_or_default();
            if env.settings.merged_chat {
                let server = m.games.get(&key).and_then(|i| i.phase.server().map(str::to_owned));
                m.receive_chat(ChatLine { instance: key, account, batch, also: Vec::new(), at: env.unix, from, text, runs }, server.as_deref(), true, env.now, KEPT_CHAT);
            }
        }
        FromMod::Preview { .. } => {
            let s = &mut m.session;
            let recent = s.read_at.get(&key).is_some_and(|t| env.now.saturating_duration_since(*t) < OFFSCREEN_PREVIEW);
            if s.visible.contains(&key) || !recent {
                if let Some(path) = preview_path(m, &key) {
                    let s = &mut m.session;
                    s.stale.remove(&key);
                    s.read_at.insert(key.clone(), env.now);
                    fx.now.push(Effect::ReadPreview { key, path });
                }
            } else {
                s.stale.insert(key, env.now);
            }
        }
        FromMod::Hotkey { .. } if m.games.get(&key).is_some_and(Game::alone) => {}
        FromMod::Hotkey { action } => {
            if playing || m.session.reveal.as_ref().is_some_and(|r| r.hide.contains(&key)) {
                fx.hotkeys.push(action);
            } else if action == HotkeyAction::Grid {
                fx.now.push(Effect::Send { key, message: ToMod::Hide });
            }
        }
        FromMod::Window { visible: true, focused } => {
            if let Some(pid) = m.session.pids.get(&key) {
                fx.now.push(Effect::Tint { key: key.clone(), pid: *pid });
            }
            fx.game_focused |= playing && focused;
            if m.session.reveal.as_ref().is_some_and(|r| r.key == key) {
                fx.revealed = m.session.reveal.take();
            }
        }
        FromMod::Closing => fx.closing.push(key),
        FromMod::Window { .. } | FromMod::Hello { .. } | FromMod::Options { .. } | FromMod::Servers => {}
    }
}

fn preview_path(m: &Model, key: &str) -> Option<std::path::PathBuf> {
    let i = m.games.get(key).filter(|i| !i.alone())?;
    Some(fleets::preview_path(m.instance(&i.instance_id)?, i.account_id))
}

fn identity(m: &Model, env: &Env, key: &str) -> Vec<ToMod> {
    let Some(i) = m.games.get(key) else { return Vec::new() };
    let (Some(account), Some(target)) = (m.account(i.account_id), m.target(&i.batch_id)) else { return Vec::new() };
    fleets::identity_messages(&target, account, env.settings.game_badge)
}

/// Every connected game hears its identity, badge and background level again.
pub(super) fn tell_games(m: &Model, env: &Env, out: &mut Vec<Effect>) {
    for key in m.session.pids.keys() {
        for message in identity(m, env, key) {
            out.push(Effect::Send { key: key.clone(), message });
        }
    }
}

pub(super) fn reconnect_due(m: &mut Model, key: &str, generation: u64, out: &mut Vec<Effect>) {
    if m.session.reconnects.get(key) != Some(&generation) {
        return;
    }
    if m.games.get(key).is_some_and(|i| matches!(i.phase, Phase::Connecting { .. } | Phase::NetworkLost { .. }) && !i.stopping) {
        out.push(Effect::Send { key: key.to_owned(), message: ToMod::Reconnect });
    }
}

pub(super) fn restart_due(m: &mut Model, key: &str, generation: u64, out: &mut Vec<Effect>) {
    if m.session.reconnects.get(key) != Some(&generation) {
        return;
    }
    let batch = m.games.get(key).filter(|i| matches!(i.phase, Phase::Restarting { .. }) && !i.stopping).map(|i| i.batch_id.clone());
    if let Some(batch) = batch {
        fleet::launch(m, &batch, Some(&[key.to_owned()]), fleet::Kick::Crash, out);
    }
}

pub(super) fn join_due(m: &mut Model, key: String, generation: u64, mut lines: Vec<String>, delay: Duration, out: &mut Vec<Effect>) {
    if lines.is_empty() || m.session.joins.get(&key) != Some(&generation) {
        return;
    }
    if !m.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Playing { .. }) && !i.stopping) {
        return;
    }
    let text = lines.remove(0);
    out.push(Effect::Send { key: key.clone(), message: ToMod::ChatSend { text } });
    if !lines.is_empty() {
        out.push(Effect::After { delay, event: Box::new(Event::JoinDue { key, generation, lines, delay }) });
    }
}

/// The tiles on screen changed: previews that went stale off screen are read now.
pub(super) fn visible(m: &mut Model, env: &Env, keys: HashSet<String>, out: &mut Vec<Effect>) {
    if m.session.visible == keys {
        return;
    }
    let appeared: Vec<String> = keys.iter().filter(|k| !m.session.visible.contains(*k) && m.session.stale.contains_key(*k)).cloned().collect();
    let paths: Vec<(String, std::path::PathBuf)> = appeared.iter().filter_map(|k| preview_path(m, k).map(|p| (k.clone(), p))).collect();
    let s = &mut m.session;
    s.visible = keys;
    for (key, path) in paths {
        s.stale.remove(&key);
        s.read_at.insert(key.clone(), env.now);
        out.push(Effect::ReadPreview { key, path });
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::super::effect::Effect;
    use super::super::super::event::{Event, Report};
    use super::super::super::model::{Hint, Model, Phase};
    use super::super::testing::*;
    use super::{local_server, server_of};
    use crate::core::account::AccountKind;
    use crate::core::fleet::{self, Adoption};
    use crate::core::history::{self, Entry, Kind};
    use crate::core::ipc::{DisconnectCause, FromMod, StateKind};
    use crate::core::phone::{Incident, What};
    use crate::core::proxy::Fault;
    use crate::core::settings::Settings;

    fn timers(fx: &[Effect]) -> Vec<(Duration, &Event)> {
        later(fx).into_iter().filter(|(_, e)| !matches!(e, Event::AlertDue { .. } | Event::PhoneDue { .. } | Event::ShotsDue { .. } | Event::UsageDue)).collect()
    }

    fn joins(fx: &[Effect]) -> usize {
        timers(fx).iter().filter(|(_, e)| matches!(e, Event::JoinDue { .. })).count()
    }

    fn stops(fx: &[Effect]) -> usize {
        fx.iter().filter(|e| matches!(e, Effect::Stop(_))).count()
    }

    fn checks(fx: &[Effect]) -> usize {
        fx.iter().filter(|e| matches!(e, Effect::CheckProxy { .. })).count()
    }

    fn suspects(fx: &[Effect]) -> bool {
        fx.iter().any(|e| matches!(e, Effect::SuspectOffline))
    }

    fn phoned() -> Settings {
        let mut s = settings();
        s.phone.enabled = true;
        s
    }

    fn incidents(m: &mut Model) -> Vec<Incident> {
        m.session.phone.take_due(Instant::now() + Duration::from_secs(3600), |_| true).into_iter().flat_map(|(_, list)| list).collect()
    }

    fn send_state(m: &mut Model, s: &Settings, key: &str, kind: StateKind, cause: Option<DisconnectCause>) -> Vec<Effect> {
        let message = FromMod::State { kind, server: Some("mc.example:25565".into()), reason: Some("why".into()), cause };
        report(m, s, Report::Mod { key: key.to_owned(), message })
    }

    fn exited(m: &mut Model, s: &Settings, key: &str, code: Option<i32>) -> Vec<Effect> {
        report(m, s, Report::Exited { key: key.to_owned(), code })
    }

    fn dropped(cause: DisconnectCause) -> (Phase, Vec<Effect>, Model) {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(cause));
        (m.games[&key].phase.clone(), fx, m)
    }

    fn adopted(phase: Phase) -> (Model, String) {
        let s = settings();
        let (mut m, key) = setup(phase, AccountKind::Offline, &["/login pw"]);
        let (account, batch, instance) = (m.games[&key].account_id, m.games[&key].batch_id.clone(), m.games[&key].instance_id.clone());
        adopt(&mut m, &s, vec![Adoption { key: key.clone(), batch: Some(batch), instance, account, pid: 41 }]);
        (m, key)
    }

    #[test]
    fn an_exit_releases_the_preview_file_and_forgets_a_stale_frame() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        m.session.stale.insert(key.clone(), Instant::now());
        let fx = exited(&mut m, &s, &key, Some(0));
        assert!(fx.iter().any(|e| matches!(e, Effect::ReleasePreview(k) if *k == key)));
        assert!(!m.session.stale.contains_key(&key));
        let mut out = Vec::new();
        super::visible(&mut m, &super::Env::new(Instant::now(), 0, 7, &s, false), std::collections::HashSet::from([key.clone()]), &mut out);
        assert!(!out.iter().any(|e| matches!(e, Effect::ReadPreview { .. })));
    }

    #[test]
    fn a_ban_stops_for_good_and_is_remembered() {
        let (phase, fx, m) = dropped(DisconnectCause::Banned);
        assert_eq!(phase, Phase::Banned { server: Some("mc.example:25565".into()), reason: "why".into() });
        assert!(timers(&fx).is_empty() && stops(&fx) == 0);
        assert!(phase.needs_attention());
        let log = m.history.values().next().unwrap();
        assert_eq!(log.back().unwrap().kind, Kind::Banned);
        assert_eq!(fx.iter().filter(|e| matches!(e, Effect::SaveHistory { .. })).count(), 1);
    }

    #[test]
    fn server_down_backs_off_five_times_then_needs_attention() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        for attempt in 1..=5 {
            let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
            assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }));
            assert_eq!(m.games[&key].reconnect_attempts, attempt);
            let Some((delay, Event::ReconnectDue { .. })) = timers(&fx).pop() else { panic!("no reconnect scheduled") };
            let base = [10.0, 30.0, 90.0, 300.0, 300.0][attempt as usize - 1];
            assert!(delay.as_secs_f64() >= base * 0.75 - 0.01 && delay.as_secs_f64() <= base * 1.25 + 0.01, "{delay:?} for try {attempt}");
        }
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(matches!(m.games[&key].phase, Phase::Disconnected { .. }) && timers(&fx).is_empty());
        assert_eq!(m.history[&key].len(), 6);
    }

    #[test]
    fn without_network_a_drop_waits_and_resumes_by_itself_when_it_is_back() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        report(&mut m, &s, Report::Reach { online: false });
        for _ in 0..8 {
            let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
            assert_eq!(m.games[&key].phase, Phase::NetworkLost { server: "mc.example:25565".into() });
            assert!(suspects(&fx));
            let waits = timers(&fx);
            let [(delay, Event::ReconnectDue { .. })] = waits.as_slice() else { panic!("no slow retry while waiting") };
            assert!(delay.as_secs() >= 48, "{delay:?}");
        }
        assert_eq!(m.games[&key].reconnect_attempts, 0);
        assert!(m.games[&key].phase.is_running() && !m.games[&key].phase.needs_attention());
        let fx = report(&mut m, &s, Report::Reach { online: true });
        assert_eq!(m.games[&key].phase, Phase::Connecting { server: "mc.example:25565".into() });
        assert!(fx.iter().any(|e| matches!(e, Effect::NetworkBack)));
        let Some((_, Event::ReconnectDue { generation, .. })) = timers(&fx).pop() else { panic!("no reconnect scheduled") };
        assert_eq!(m.session.reconnects.get(&key), Some(generation));
    }

    #[test]
    fn losing_the_network_gives_back_the_try_a_pending_reconnect_spent() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert_eq!(m.games[&key].reconnect_attempts, 1);
        let before = m.session.reconnects.get(&key).copied();
        report(&mut m, &s, Report::Reach { online: false });
        assert_eq!(m.games[&key].phase, Phase::NetworkLost { server: "mc.example:25565".into() });
        assert_eq!(m.games[&key].reconnect_attempts, 0);
        assert_ne!(m.session.reconnects.get(&key).copied(), before);
    }

    #[test]
    fn a_server_on_this_computer_or_in_the_home_network_does_not_wait_for_the_internet() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        report(&mut m, &s, Report::Reach { online: false });
        let message = FromMod::State { kind: StateKind::Disconnected, server: Some("192.168.1.20:25565".into()), reason: None, cause: Some(DisconnectCause::ServerDown) };
        let fx = report(&mut m, &s, Report::Mod { key: key.clone(), message });
        assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }) && !suspects(&fx));
        assert_eq!(m.games[&key].reconnect_attempts, 1);
        report(&mut m, &s, Report::Reach { online: false });
        assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }));
    }

    #[test]
    fn local_servers_are_told_apart_from_the_internet() {
        for near in ["localhost", "localhost:25565", "127.0.0.1:25565", "10.0.0.5", "192.168.1.20:25565", "172.16.3.4:1", "[::1]:25565", "fd00::1", "minecraft.local"] {
            assert!(local_server(near), "{near}");
        }
        for far in ["mc.example:25565", "8.8.8.8:25565", "[2001:db8::1]:25565", "localhost.example.com", ""] {
            assert!(!local_server(far), "{far}");
        }
    }

    #[test]
    fn a_waiting_game_can_be_played_and_a_waiting_start_can_be_stopped() {
        assert!(Phase::NetworkLost { server: "s".into() }.playable());
        assert_eq!(server_of(&Phase::NetworkLost { server: "s".into() }).as_deref(), Some("s"));
        assert!(Phase::NoNetwork.stoppable() && !Phase::NoNetwork.is_active());
    }

    #[test]
    fn a_ban_is_a_ban_even_without_network() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        report(&mut m, &s, Report::Reach { online: false });
        send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Banned));
        assert!(matches!(m.games[&key].phase, Phase::Banned { .. }));
    }

    #[test]
    fn a_proxied_account_does_not_wait_for_the_network() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        m.accounts[0].proxy = Some("socks5://127.0.0.1:1080".into());
        report(&mut m, &s, Report::Reach { online: false });
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(matches!(m.games[&key].phase, Phase::Connecting { .. }) && !suspects(&fx));
    }

    #[test]
    fn a_start_without_connection_waits_and_starts_when_it_is_back_unless_stopped() {
        let s = settings();
        let (mut m, key) = setup(Phase::Starting { place: 1, total: 1 }, AccountKind::Microsoft, &[]);
        let fx = report(&mut m, &s, Report::Failed { key: key.clone(), reason: "failed to lookup address".into(), login: false, offline: true });
        assert_eq!(m.games[&key].phase, Phase::NoNetwork);
        assert!(suspects(&fx) && !m.games[&key].phase.is_running() && !m.games[&key].phase.needs_attention());
        let waits = timers(&fx);
        let [(delay, Event::WaitingStartDue { key: waiting })] = waits.as_slice() else { panic!("no slow retry for a waiting start") };
        assert!(waiting == &key && delay.as_secs() >= 48);
        let fx = report(&mut m, &s, Report::Reach { online: true });
        assert!(fx.iter().any(|e| matches!(e, Effect::Start { accounts, .. } if accounts.len() == 1 && accounts[0].id == account(&m))));
        report(&mut m, &s, Report::Failed { key: key.clone(), reason: "bad jar".into(), login: false, offline: false });
        assert!(matches!(m.games[&key].phase, Phase::Failed { .. }));
    }

    #[test]
    fn a_waiting_start_retries_by_itself_when_its_timer_comes() {
        let s = settings();
        let (mut m, key) = setup(Phase::NoNetwork, AccountKind::Microsoft, &[]);
        let fx = run(&mut m, &s, Instant::now(), Event::WaitingStartDue { key: key.clone() });
        assert!(fx.iter().any(|e| matches!(e, Effect::Start { .. })), "a waiting start must not stay stuck");
        assert_eq!(m.games[&key].phase, Phase::Starting { place: 1, total: 1 });
    }

    #[test]
    fn stale_timers_do_not_touch_a_newer_state() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &["/home"]);
        m.session.pids.insert(key.clone(), 1);
        report(&mut m, &s, state(&key, StateKind::Disconnected, Some("mc.example"), Some(DisconnectCause::ServerDown)));
        let old = m.session.reconnects[&key];
        run(&mut m, &s, Instant::now(), Event::Reconnect(key.clone()));
        assert!(run(&mut m, &s, Instant::now(), Event::ReconnectDue { key: key.clone(), generation: old }).is_empty());
        m.games.get_mut(&key).unwrap().phase = Phase::Restarting { attempt: 1, of: 3 };
        let stale = m.session.reconnects[&key] - 1;
        assert!(run(&mut m, &s, Instant::now(), Event::RestartDue { key: key.clone(), generation: stale }).is_empty());
        assert_eq!(m.games[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
        m.games.get_mut(&key).unwrap().phase = Phase::Playing { server: "mc.example".into(), since: Instant::now() };
        let stale = m.session.joins.get(&key).copied().unwrap_or_default().wrapping_sub(1);
        assert!(run(&mut m, &s, Instant::now(), Event::JoinDue { key: key.clone(), generation: stale, lines: vec!["/home".into()], delay: Duration::ZERO }).is_empty());
    }

    #[test]
    fn a_late_launch_or_failure_after_a_stop_is_ignored() {
        let s = settings();
        let (mut m, key) = setup(Phase::Starting { place: 2, total: 3 }, AccountKind::Microsoft, &[]);
        run(&mut m, &s, Instant::now(), Event::Stop(vec![key.clone()]));
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        report(&mut m, &s, Report::Failed { key: key.clone(), reason: "cancelled".into(), login: false, offline: true });
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        report(&mut m, &s, Report::Launched { key: key.clone(), plain: false });
        assert_eq!(m.games[&key].phase, Phase::Stopped);
    }

    #[test]
    fn a_kick_waits_longer_than_a_server_that_is_down() {
        let (phase, fx, _) = dropped(DisconnectCause::Kicked);
        assert!(matches!(phase, Phase::Connecting { .. }));
        let Some((delay, Event::ReconnectDue { .. })) = timers(&fx).pop() else { panic!("no reconnect scheduled") };
        assert!(delay.as_secs() >= 45);
    }

    #[test]
    fn a_drop_that_reconnects_tells_the_phone_and_so_does_the_return() {
        let s = phoned();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Kicked));
        let list = incidents(&mut m);
        let [kicked] = list.as_slice() else { panic!("{} phone incidents", list.len()) };
        assert_eq!((kicked.what, kicked.name.as_str(), kicked.reason.as_str(), kicked.server.as_deref()), (What::Kicked, "AltOne", "why", Some("mc.example:25565")));
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        send_state(&mut m, &s, &key, StateKind::Playing, None);
        let list = incidents(&mut m);
        assert_eq!(list.first().map(|i| (i.what, i.healed)), Some((What::Dropped, true)));
    }

    #[test]
    fn a_whitelist_refusal_is_left_alone() {
        let (phase, fx, _) = dropped(DisconnectCause::Whitelist);
        assert!(matches!(phase, Phase::Disconnected { .. }) && timers(&fx).is_empty());
    }

    #[test]
    fn a_rejected_sign_in_restarts_to_refresh_then_asks_for_a_new_one() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        for attempt in 1..=3 {
            let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
            assert_eq!(m.games[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert_eq!(stops(&fx), 1);
            assert!(timers(&fx).is_empty());
            let exit = exited(&mut m, &s, &key, Some(1));
            assert_eq!(m.games[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert!(matches!(timers(&exit).pop(), Some((_, Event::RestartDue { .. }))));
            m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert_eq!(m.games[&key].phase, Phase::LoginNeeded);
        assert!(timers(&fx).is_empty());
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert!(matches!(m.games[&key].phase, Phase::Disconnected { .. }) && stops(&fx) == 0);
    }

    #[test]
    fn the_same_account_signed_in_elsewhere_is_left_alone() {
        let (phase, fx, _) = dropped(DisconnectCause::Duplicate);
        assert!(matches!(phase, Phase::Disconnected { .. }) && timers(&fx).is_empty());
    }

    #[test]
    fn a_banned_instance_that_dies_stays_crashed_and_does_not_rejoin() {
        let s = settings();
        let (mut m, key) = setup(Phase::Banned { server: None, reason: "x".into() }, AccountKind::Microsoft, &[]);
        let fx = exited(&mut m, &s, &key, Some(1));
        assert_eq!(m.games[&key].phase, Phase::Crashed { code: Some(1) });
        assert!(timers(&fx).is_empty());
    }

    #[test]
    fn a_server_switch_does_not_send_the_join_commands_again() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &["/login pw"]);
        let mut fx = send_state(&mut m, &s, &key, StateKind::Playing, None);
        assert_eq!(timers(&fx).len(), 1);
        fx.extend(send_state(&mut m, &s, &key, StateKind::Connecting, None));
        fx.extend(send_state(&mut m, &s, &key, StateKind::Playing, None));
        assert_eq!(timers(&fx).len(), 1);
        assert_eq!(m.history[&key].len(), 1);
        fx.extend(send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown)));
        fx.extend(send_state(&mut m, &s, &key, StateKind::Playing, None));
        assert_eq!(joins(&fx), 2);
    }

    #[test]
    fn leaving_to_the_menu_and_rejoining_sends_the_join_commands_again() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &["/login pw"]);
        let mut fx = send_state(&mut m, &s, &key, StateKind::Playing, None);
        for kind in [StateKind::Menu, StateKind::Connecting, StateKind::Playing] {
            fx.extend(send_state(&mut m, &s, &key, kind, None));
        }
        assert_eq!(joins(&fx), 2);
        assert_eq!(m.history[&key].iter().filter(|e| e.kind == Kind::Connected).count(), 2);
    }

    #[test]
    fn a_late_exit_relaunches_only_after_the_old_process_ended() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Auth));
        assert_eq!(m.games[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
        assert!(fx.iter().any(|e| matches!(e, Effect::Stop(k) if *k == [key.clone()])));
        assert!(timers(&fx).is_empty());
        assert!(m.session.pids.contains_key(&key));
        let exit = exited(&mut m, &s, &key, Some(1));
        assert!(!m.session.pids.contains_key(&key));
        assert_eq!(m.games[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
        let mut waits = timers(&exit);
        let Some((delay, Event::RestartDue { generation, .. })) = waits.pop() else { panic!("no restart scheduled") };
        assert_eq!(delay, Duration::ZERO);
        assert_eq!(*generation, m.session.reconnects[&key]);
        assert!(waits.is_empty());
    }

    #[test]
    fn an_adopted_game_shows_its_last_frame_without_waiting_for_a_new_one() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let fresh = report(&mut m, &s, Report::Connected { key: key.clone(), pid: 1 });
        assert!(!fresh.iter().any(|e| matches!(e, Effect::ReadPreview { .. })), "a fresh game's file may still hold its last run");
        m.session.adopted.insert(key.clone());
        let adopted = report(&mut m, &s, Report::Connected { key: key.clone(), pid: 1 });
        assert!(adopted.iter().any(|e| matches!(e, Effect::ReadPreview { key: k, .. } if *k == key)));
    }

    #[test]
    fn stability_counts_from_the_first_join_not_from_the_start() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 1 });
        assert!(!m.session.started.contains_key(&key));
        send_state(&mut m, &s, &key, StateKind::Playing, None);
        assert!(m.session.started.contains_key(&key));
    }

    #[test]
    fn a_manual_exit_after_a_rejected_sign_in_keeps_the_login_state() {
        let s = settings();
        let (mut m, key) = setup(Phase::LoginNeeded, AccountKind::Microsoft, &[]);
        exited(&mut m, &s, &key, Some(0));
        assert_eq!(m.games[&key].phase, Phase::LoginNeeded);
    }

    #[test]
    fn join_commands_go_out_once_per_join_and_not_after_a_respawn() {
        let s = settings();
        let (mut m, key) = setup(Phase::Connecting { server: "mc.example:25565".into() }, AccountKind::Offline, &["/login pw", "  ", "/home"]);
        let fx = send_state(&mut m, &s, &key, StateKind::Playing, None);
        let Some((delay, Event::JoinDue { lines, delay: wait, generation, .. })) = timers(&fx).pop() else { panic!("no join scheduled") };
        assert_eq!(lines, &["/login pw", "/home"]);
        assert_eq!((delay, *wait), (Duration::from_millis(2000), Duration::from_millis(2000)));
        assert_eq!(m.session.joins[&key], *generation);
        let generation = *generation;
        assert_eq!(m.history[&key].back().unwrap().kind, Kind::Connected);
        let mut fx = send_state(&mut m, &s, &key, StateKind::Playing, None);
        fx.extend(send_state(&mut m, &s, &key, StateKind::Dead, None));
        fx.extend(send_state(&mut m, &s, &key, StateKind::Playing, None));
        assert!(timers(&fx).is_empty());
        send_state(&mut m, &s, &key, StateKind::Menu, None);
        assert_ne!(m.session.joins[&key], generation);
    }

    #[test]
    fn an_adopted_game_runs_without_contact_until_its_mod_reports() {
        let s = settings();
        let (m, key) = adopted(Phase::Stopped);
        assert_eq!(m.games[&key].phase, Phase::Menu);
        assert!(m.games[&key].phase.is_running());
        assert_eq!(m.session.pids[&key], 41);
        assert!(m.hint(&key).link_lost);
        let (mut m, solo) = adopted(Phase::Stopped);
        let account = m.games[&solo].account_id;
        let made = m.instances[0].clone();
        let key = fleet::solo_key(&made, account);
        adopt(&mut m, &s, vec![Adoption { key: key.clone(), batch: None, instance: made.id.clone(), account, pid: 42 }]);
        assert_eq!(m.games[&key].batch_id, fleet::solo_owner(&made.id));
        assert_eq!(m.games[&key].phase, Phase::Menu);
    }

    #[test]
    fn an_adopted_game_that_plays_on_sends_no_join_commands_and_no_news() {
        let s = phoned();
        let (mut m, key) = adopted(Phase::Stopped);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 41 });
        assert!(!m.hint(&key).link_lost);
        let mut fx = send_state(&mut m, &s, &key, StateKind::Playing, None);
        assert!(matches!(m.games[&key].phase, Phase::Playing { .. }));
        assert!(timers(&fx).is_empty() && incidents(&mut m).is_empty());
        assert!(m.history.get(&key).is_none_or(|h| h.is_empty()));
        fx.extend(send_state(&mut m, &s, &key, StateKind::Playing, None));
        assert!(timers(&fx).is_empty());
        send_state(&mut m, &s, &key, StateKind::Menu, None);
        let fx = send_state(&mut m, &s, &key, StateKind::Playing, None);
        assert_eq!(joins(&fx), 1, "a real rejoin later sends them again");
    }

    #[test]
    fn an_adopted_game_on_a_disconnect_screen_is_not_reconnected_by_the_restart() {
        let s = phoned();
        let (mut m, key) = adopted(Phase::Stopped);
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert!(matches!(m.games[&key].phase, Phase::Disconnected { .. }));
        assert!(timers(&fx).is_empty() && incidents(&mut m).is_empty());
        let (mut m, key) = adopted(Phase::Stopped);
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Banned));
        assert!(matches!(m.games[&key].phase, Phase::Banned { .. }));
        assert!(timers(&fx).is_empty());
    }

    #[test]
    fn an_adopted_game_that_quits_without_an_exit_code_is_not_a_crash() {
        let s = settings();
        let (mut m, key) = adopted(Phase::Stopped);
        send_state(&mut m, &s, &key, StateKind::Playing, None);
        let fx = exited(&mut m, &s, &key, None);
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(timers(&fx).is_empty());
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        exited(&mut m, &s, &key, None);
        assert!(matches!(m.games[&key].phase, Phase::Crashed { .. } | Phase::Restarting { .. }), "a launched game still crashes");
    }

    #[test]
    fn no_join_commands_means_nothing_is_scheduled() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        let fx = send_state(&mut m, &s, &key, StateKind::Playing, None);
        assert!(timers(&fx).is_empty());
    }

    #[test]
    fn a_crash_restarts_three_times_then_stays_crashed() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        for attempt in 1..=3 {
            let fx = exited(&mut m, &s, &key, Some(1));
            assert_eq!(m.games[&key].phase, Phase::Restarting { attempt, of: 3 });
            assert!(!m.games[&key].phase.needs_attention());
            assert!(matches!(timers(&fx).pop(), Some((_, Event::RestartDue { .. }))));
            m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        let fx = exited(&mut m, &s, &key, Some(1));
        assert_eq!(m.games[&key].phase, Phase::Crashed { code: Some(1) });
        assert!(timers(&fx).is_empty());
        assert_eq!(m.history[&key].iter().filter(|e| e.kind == Kind::Crashed).count(), 4);
    }

    #[test]
    fn a_long_run_resets_the_crash_count() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        m.session.crash_restarts.insert(key.clone(), 3);
        m.session.started.insert(key.clone(), Instant::now() - Duration::from_secs(120));
        exited(&mut m, &s, &key, Some(1));
        assert_eq!(m.games[&key].phase, Phase::Restarting { attempt: 1, of: 3 });
    }

    #[test]
    fn clean_and_requested_exits_do_not_restart() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let mut fx = exited(&mut m, &s, &key, Some(0));
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        m.games.get_mut(&key).unwrap().stopping = true;
        fx.extend(exited(&mut m, &s, &key, Some(1)));
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(timers(&fx).is_empty() && m.history.is_empty());
    }

    #[test]
    fn a_stopped_banned_account_keeps_its_ban_in_the_status() {
        let s = settings();
        let (_, _, mut m) = dropped(DisconnectCause::Banned);
        let key = m.games.keys().next().unwrap().clone();
        m.games.get_mut(&key).unwrap().stopping = true;
        exited(&mut m, &s, &key, Some(0));
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        let last = m.history[&key].back().unwrap();
        assert_eq!((last.kind, last.reason.as_str()), (Kind::Banned, "why"));
        history::push(m.history.get_mut(&key).unwrap(), Entry::now(Kind::Connected, None, ""));
        assert_eq!(m.history[&key].back().unwrap().kind, Kind::Connected);
    }

    #[test]
    fn a_refresh_result_moves_idle_instances_in_and_out_of_login_needed() {
        let s = settings();
        let (mut m, key) = setup(Phase::Stopped, AccountKind::Microsoft, &[]);
        let id = account(&m);
        report(&mut m, &s, Report::SignIn { account: id, valid: false });
        assert_eq!(m.games[&key].phase, Phase::LoginNeeded);
        report(&mut m, &s, Report::SignIn { account: id, valid: true });
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        report(&mut m, &s, Report::SignIn { account: id, valid: false });
        assert_eq!(m.games[&key].phase, Phase::Menu);
    }

    /// Drops the link to `key` and lets the wait before the tile says so run out.
    fn lose(m: &mut Model, s: &Settings, key: &str) -> Vec<Effect> {
        let mut fx = report(m, s, Report::LinkLost { key: key.into() });
        assert!(!m.hint(key).link_lost, "not at once");
        let due = later(&fx).iter().any(|(d, e)| *d == super::LINK_PATIENCE && matches!(e, Event::LinkLostDue { key: k } if k == key));
        if due {
            fx.extend(run(m, s, Instant::now(), Event::LinkLostDue { key: key.into() }));
        }
        fx
    }

    #[test]
    fn a_dropped_link_on_a_living_game_shows_until_it_talks_again() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        lose(&mut m, &s, &key);
        assert!(!m.hint(&key).link_lost, "no process known yet");
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        lose(&mut m, &s, &key);
        assert!(m.hint(&key).link_lost);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        assert!(!m.hint(&key).link_lost);
        lose(&mut m, &s, &key);
        send_state(&mut m, &s, &key, StateKind::Playing, None);
        assert!(!m.hint(&key).link_lost);
    }

    #[test]
    fn a_link_that_drops_because_the_game_is_closing_is_not_news() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        m.games.get_mut(&key).unwrap().stopping = true;
        lose(&mut m, &s, &key);
        assert!(!m.hint(&key).link_lost);
        m.games.get_mut(&key).unwrap().stopping = false;
        lose(&mut m, &s, &key);
        assert!(m.hint(&key).link_lost);
        exited(&mut m, &s, &key, Some(0));
        assert_eq!(m.hint(&key), Hint::default());
    }

    #[test]
    fn a_game_that_quits_on_its_own_drops_its_link_without_a_flash_of_no_contact() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        let fx = report(&mut m, &s, Report::LinkLost { key: key.clone() });
        assert!(matches!(later(&fx).as_slice(), [(_, Event::LinkLostDue { .. })]));
        exited(&mut m, &s, &key, Some(0));
        run(&mut m, &s, Instant::now(), Event::LinkLostDue { key: key.clone() });
        assert_eq!(m.games[&key].phase, Phase::Stopped);
        assert!(!m.hint(&key).link_lost, "the game is down, not out of contact");
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        report(&mut m, &s, Report::LinkLost { key: key.clone() });
        send_state(&mut m, &s, &key, StateKind::Menu, None);
        run(&mut m, &s, Instant::now(), Event::LinkLostDue { key: key.clone() });
        assert!(!m.hint(&key).link_lost, "the link came back in time");
    }

    #[test]
    fn closing_the_game_window_stops_the_game_and_brings_the_grid_back() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Offline, &[]);
        report(&mut m, &s, Report::Connected { key: key.clone(), pid: 9 });
        m.playing = Some(key.clone());
        let fx = report(&mut m, &s, Report::Mod { key: key.clone(), message: FromMod::Closing });
        assert!(m.games[&key].stopping);
        assert_eq!(stops(&fx), 1, "the fleet still ends it if it hangs");
        assert!(m.playing.is_none() && fx.iter().any(|e| matches!(e, Effect::ShowManager)));
        assert!(later(&fx).iter().any(|(_, e)| matches!(e, Event::StopOverdue { .. })));
        assert!(fx.iter().any(|e| matches!(e, Effect::Closed(k) if *k == key)), "offers to start it again");
        let fx = report(&mut m, &s, Report::Mod { key: key.clone(), message: FromMod::Closing });
        assert_eq!(stops(&fx), 0, "once is enough");
        assert!(!fx.iter().any(|e| matches!(e, Effect::Closed(_))));
        exited(&mut m, &s, &key, Some(0));
        assert_eq!(m.games[&key].phase, Phase::Stopped);
    }

    #[test]
    fn a_connection_failure_on_a_proxied_account_asks_for_a_proxy_check() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let proxy = "socks5://u:p@10.0.0.1:1080";
        m.accounts[0].proxy = Some(proxy.into());
        let fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert_eq!(checks(&fx), 1);
        assert!(fx.iter().any(|e| matches!(e, Effect::CheckProxy { key: k, url } if *k == key && url == proxy)));
        for fault in [Some(Fault::Unreachable), Some(Fault::Rejected), None, Some(Fault::Unreachable)] {
            report(&mut m, &s, Report::ProxyChecked { key: key.clone(), fault });
            assert_eq!(m.hint(&key).proxy, fault);
        }
        send_state(&mut m, &s, &key, StateKind::Playing, None);
        assert_eq!(m.hint(&key).proxy, None);
    }

    #[test]
    fn proxy_checks_only_follow_connection_failures_of_proxied_accounts() {
        let s = settings();
        let (mut m, key) = setup(Phase::Menu, AccountKind::Microsoft, &[]);
        let mut fx = send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::ServerDown));
        assert_eq!(checks(&fx), 0, "no proxy, no check");
        m.accounts[0].proxy = Some("socks5://10.0.0.1".into());
        m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        for cause in [DisconnectCause::Banned, DisconnectCause::Kicked, DisconnectCause::Whitelist, DisconnectCause::ServerFull, DisconnectCause::Duplicate] {
            fx.extend(send_state(&mut m, &s, &key, StateKind::Disconnected, Some(cause)));
            m.games.get_mut(&key).unwrap().phase = Phase::Menu;
        }
        assert_eq!(checks(&fx), 0, "the server answered, the proxy works");
        fx.extend(send_state(&mut m, &s, &key, StateKind::Disconnected, Some(DisconnectCause::Auth)));
        assert_eq!(checks(&fx), 1);
    }

    #[test]
    fn a_late_proxy_result_for_a_running_game_is_ignored() {
        let s = settings();
        let (mut m, key) = setup(Phase::Playing { server: "s".into(), since: Instant::now() }, AccountKind::Microsoft, &[]);
        report(&mut m, &s, Report::ProxyChecked { key: key.clone(), fault: Some(Fault::Unreachable) });
        assert_eq!(m.hint(&key).proxy, None);
    }
}
