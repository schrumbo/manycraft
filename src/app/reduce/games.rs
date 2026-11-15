use std::collections::HashSet;
use std::time::Duration;

use super::super::effect::Effect;
use super::super::event::{Event, Report};
use super::super::model::{ChatLine, Model, Phase, rank};
use super::super::phase::{self, Why};
use super::super::session::{Alert, Frame, Reveal, Whisper};
use super::{Env, fleet, go, notify, redraw, show};
use crate::core::account::AccountKind;
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
    pub check_proxy: Vec<Effect>,
    pub suspect_offline: bool,
    pub network_back: bool,
    pub later: Vec<Later>,
    pub alerts: Vec<(String, String, Alert)>,
    pub whispers: Vec<Whisper>,
    pub phone: Vec<(String, Incident)>,
    pub placed: Vec<String>,
    pub revealed: Option<Reveal>,
    pub game_focused: bool,
    pub show_grid: bool,
    pub plays: Vec<(String, Option<String>)>,
    pub hotkeys: Vec<HotkeyAction>,
}

/// Work a report leaves for later.
pub(super) enum Later {
    After(Duration, Event),
    Start(Vec<String>),
}

pub(super) fn reports(m: &mut Model, env: &Env, reports: Vec<Report>, out: &mut Vec<Effect>) {
    let frames_only = reports.iter().all(|r| matches!(r, Report::Frame { .. }));
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
        if after.1 && !seeded && before.is_some_and(|b| !b.1) {
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
    for whisper in fx.whispers {
        notify::queue_whisper(m, env, whisper, out);
    }
    notify::queue_phone(m, env, fx.phone, out);
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
        show::request_play(m, env, instance, account, out);
    }
    show::show_ready(m, env, out);
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

fn forget_show_when_ready(m: &mut Model, key: &str) {
    if m.session.show_when_ready.as_ref().is_some_and(|(k, _)| k == key) {
        m.session.show_when_ready = None;
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
            forget_show_when_ready(m, &key);
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
            forget_show_when_ready(m, &key);
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
            m.set_hint(&key, |h| h.link_lost = false);
            m.session.pids.insert(key.clone(), pid);
            if env.place_windows {
                m.session.unplaced.insert(key.clone());
            }
            for message in identity(m, env, &key) {
                fx.now.push(Effect::Send { key: key.clone(), message });
            }
            fx.now.push(Effect::Tint { key: key.clone(), pid });
            if m.session.adopted.contains(&key) && m.playing.as_deref() != Some(key.as_str()) {
                fx.now.push(Effect::Send { key: key.clone(), message: ToMod::Hide });
            }
            if m.games.get(&key).is_some_and(|i| matches!(i.phase, Phase::Starting { .. })) {
                go(m, &key, Phase::Menu, Why::Connected, &mut fx.now);
            }
        }
        Report::Reopen => fx.show_grid = true,
        Report::PlayRequest { instance, account } => fx.plays.push((instance, account)),
        Report::LinkLost { key } => {
            let alive = m.session.pids.contains_key(&key) && m.games.get(&key).is_some_and(|i| !i.stopping && i.phase != Phase::Stopped);
            if alive {
                m.set_hint(&key, |h| h.link_lost = true);
            }
        }
        Report::Mod { key, message } => {
            m.set_hint(&key, |h| h.link_lost = false);
            on_message(m, env, key, message, fx);
        }
    }
}

fn on_exit(m: &mut Model, env: &Env, fx: &mut Fx, key: String, code: Option<i32>) {
    m.set_hint(&key, |h| *h = Default::default());
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
        FromMod::Chat { kind, from, text } => {
            let whisper = kind == ChatKind::Whisper;
            let merged = env.settings.merged_chat;
            if whisper
                && !playing
                && merged
                && let Some(i) = m.games.get_mut(&key)
            {
                i.unread_whispers += 1;
            }
            let (account, batch) = m.games.get(&key).map(|i| (i.name.clone(), i.batch_id.clone())).unwrap_or_default();
            if whisper && !playing {
                fx.whispers.push(Whisper { account: account.clone(), from: from.clone(), text: text.clone() });
            }
            if merged {
                let server = m.games.get(&key).and_then(|i| i.phase.server().map(str::to_owned));
                let shared = !whisper && !(from.is_none() && text.contains(&account));
                m.receive_chat(ChatLine { instance: key, account, batch, also: Vec::new(), at: env.unix, whisper, from, text }, server.as_deref(), shared, env.now, KEPT_CHAT);
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
        FromMod::Window { .. } | FromMod::Hello { .. } | FromMod::Options { .. } | FromMod::Servers => {}
    }
}

fn preview_path(m: &Model, key: &str) -> Option<std::path::PathBuf> {
    let i = m.games.get(key)?;
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
        fleet::launch(m, &batch, Some(&[key.to_owned()]), true, out);
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
