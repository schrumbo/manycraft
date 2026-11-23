use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::super::effect::Effect;
use super::super::event::Event;
use super::super::model::{Model, Phase};
use super::super::session::Alert;
use super::Env;
use crate::core::i18n::{tr, trf};
use crate::core::ipc::DisconnectCause;
use crate::core::phone::{self, Incident, What};

const ALERT_AFTER: Duration = Duration::from_secs(3);
const ALERT_NAMES: usize = 4;

const ATTENTION_PREFIX: &str = "attention-";

/// The tag of attention notifications for `batch`.
pub fn attention_tag(batch: &str) -> String {
    format!("{ATTENTION_PREFIX}{batch}")
}

/// The batch an attention notification tag belongs to.
pub fn attention_batch(tag: &str) -> Option<&str> {
    tag.strip_prefix(ATTENTION_PREFIX)
}

pub(super) fn phone_what(phase: &Phase) -> Option<What> {
    Some(match phase {
        Phase::Disconnected { .. } => What::GaveUp,
        Phase::Banned { .. } => What::Banned,
        Phase::LoginNeeded => What::Login,
        Phase::Crashed { .. } => What::Crashed,
        Phase::Failed { .. } => What::Failed,
        Phase::Dead { .. } => What::Died,
        _ => return None,
    })
}

pub(super) fn phase_reason(phase: &Phase) -> &str {
    match phase {
        Phase::Disconnected { reason, .. } | Phase::Banned { reason, .. } => reason,
        _ => "",
    }
}

pub(super) fn cause_text(cause: DisconnectCause) -> String {
    match cause {
        DisconnectCause::ServerDown => tr("Server nicht erreichbar"),
        DisconnectCause::ServerFull => tr("Server voll"),
        DisconnectCause::PackDeclined => tr("Der Server verlangt ein Ressourcenpaket, es wurde abgelehnt"),
        _ => String::new(),
    }
}

pub(super) fn queue_alert(m: &mut Model, batch: String, name: String, kind: Alert, out: &mut Vec<Effect>) {
    let list = m.session.alerts.entry(batch.clone()).or_default();
    let first = list.is_empty();
    list.push((name, kind));
    if first {
        out.push(Effect::After { delay: ALERT_AFTER, event: Box::new(Event::AlertDue { batch }) });
    }
}

pub(super) fn flush_alert(m: &mut Model, env: &Env, batch_id: &str, out: &mut Vec<Effect>) {
    let Some(list) = m.session.alerts.remove(batch_id) else { return };
    if !env.settings.notifications.attention {
        return;
    }
    let open: Vec<&(String, Alert)> = list.iter().filter(|(name, _)| m.games.values().any(|i| i.batch_id == batch_id && i.name == *name && i.phase.needs_attention())).collect();
    let mut seen = HashSet::new();
    let names: Vec<String> = open.iter().filter(|(n, _)| seen.insert(n.clone())).map(|(n, _)| n.clone()).collect();
    let kinds: HashSet<Alert> = open.iter().map(|(_, k)| *k).collect();
    let batch_name = m.batch(batch_id).map(|b| b.name.clone()).unwrap_or_default();
    if names.is_empty() {
        return;
    }
    let n = names.len();
    let what = match (kinds.len(), kinds.iter().next()) {
        (1, Some(Alert::Disconnected)) => trf(if n == 1 { "%lld Account getrennt" } else { "%lld Accounts getrennt" }, &[&n]),
        (1, Some(Alert::Banned)) => trf(if n == 1 { "%lld Account gesperrt" } else { "%lld Accounts gesperrt" }, &[&n]),
        (1, Some(Alert::Crashed)) => trf(if n == 1 { "%lld Account abgestürzt" } else { "%lld Accounts abgestürzt" }, &[&n]),
        (1, Some(Alert::Login)) => trf(if n == 1 { "%lld Account muss sich anmelden" } else { "%lld Accounts müssen sich anmelden" }, &[&n]),
        _ => trf(if n == 1 { "%lld Account braucht dich" } else { "%lld Accounts brauchen dich" }, &[&n]),
    };
    let title = format!("{what} · {batch_name}");
    let mut body = names.iter().take(ALERT_NAMES).cloned().collect::<Vec<_>>().join(", ");
    if n > ALERT_NAMES {
        body = trf("%@ und %lld weitere", &[&body, &(n - ALERT_NAMES)]);
    }
    let mut keys: Vec<String> = m.games.iter().filter(|(_, i)| i.batch_id == batch_id && i.phase.needs_attention() && names.contains(&i.name)).map(|(k, _)| k.clone()).collect();
    keys.sort();
    out.push(Effect::Notify { tag: attention_tag(batch_id), title, body, keys });
}

pub(super) fn queue_phone(m: &mut Model, env: &Env, incidents: Vec<(String, Incident)>, out: &mut Vec<Effect>) {
    if incidents.is_empty() || !env.settings.phone.enabled {
        return;
    }
    for (batch, incident) in incidents {
        m.session.phone.push(&batch, incident, env.now);
    }
    if let Some(due) = m.session.phone.next_due() {
        flush_phone_at(m, env, due, out);
    }
}

fn flush_phone_at(m: &mut Model, env: &Env, due: Instant, out: &mut Vec<Effect>) {
    if m.session.phone_at.is_some_and(|at| at <= due) {
        return;
    }
    m.session.phone_at = Some(due);
    out.push(Effect::After { delay: due.saturating_duration_since(env.now), event: Box::new(Event::PhoneDue { due }) });
}

pub(super) fn flush_phone(m: &mut Model, env: &Env, out: &mut Vec<Effect>) {
    let mut coalescer = std::mem::take(&mut m.session.phone);
    let current = |e: &Incident| {
        m.games.get(&e.key).is_some_and(|i| {
            !i.stopping
                && match e.what {
                    _ if e.healed => i.phase.on_server(),
                    What::Back => i.phase.on_server(),
                    What::Dropped | What::Kicked => !i.phase.on_server() && i.phase.is_active(),
                    _ => i.phase.needs_attention(),
                }
        })
    };
    let batches = coalescer.take_due(env.now, current);
    let due: Vec<(String, Vec<Incident>)> = batches.into_iter().map(|(id, list)| (m.batch(&id).map(|b| b.name.clone()).unwrap_or_default(), list)).collect();
    let next = coalescer.next_due();
    m.session.phone = coalescer;
    if let Some(next) = next {
        flush_phone_at(m, env, next, out);
    }
    let phone = &env.settings.phone;
    if !phone.enabled {
        return;
    }
    for (batch, list) in due {
        if let Some(text) = phone::format(&batch, &list, phone.reconnected) {
            out.push(Effect::Phone(text));
        }
    }
}