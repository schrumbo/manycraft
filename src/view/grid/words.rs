//! The words the grid shows for its games: a status in one word, why a game dropped, its history, what the search finds,
//! and the lines about starts, outages and kinds of work above the games.

use uuid::Uuid;

use crate::app::event::Event;
use crate::app::model::{Able, Game, Model, Need, Outage, Phase, Run, proxy_status, status, stopped_ban};
use crate::core::account::AccountKind;
use crate::core::history::{Entry, Kind};
use crate::core::i18n::{tr, trf};
use crate::view::ui::Show;

/// How many events the history of a game lists.
pub const HISTORY: usize = 12;

/// The status of `g` in a word or two, for small tiles and the table.
pub fn word(g: &Game, m: &Model) -> String {
    if g.stopping {
        return tr("Stoppt…");
    }
    let hint = &g.hint;
    if hint.proxy.is_some() && matches!(g.phase, Phase::Connecting { .. } | Phase::Disconnected { .. }) {
        return tr("Proxy-Fehler");
    }
    if hint.link_lost && g.phase.is_running() {
        return tr("Kein Kontakt");
    }
    let tries = crate::core::settings::active().reconnect.max_attempts;
    let preparing = m.preparing.get(&g.batch_id).copied();
    match &g.phase {
        _ if stopped_ban(g, m).is_some() => tr("Gesperrt"),
        Phase::Stopped => tr("Gestoppt"),
        Phase::Starting { .. } if g.memory_wait.is_some() => tr("Wartet auf Arbeitsspeicher"),
        Phase::Starting { .. } if preparing.is_some() => trf("Lädt %lld %%", &[&preparing.unwrap_or(0)]),
        Phase::Starting { place, total } if *total > 1 && *place > 0 => trf("Wartet %lld/%lld", &[place, total]),
        Phase::Starting { .. } => tr("Startet…"),
        Phase::Restarting { .. } => tr("Startet neu…"),
        Phase::Plain => tr("Läuft · ohne Vorschau"),
        Phase::Menu => tr("Hauptmenü"),
        Phase::Connecting { .. } if g.reconnect_attempts > 0 => trf("Versuch %lld/%lld", &[&g.reconnect_attempts, &tries]),
        Phase::Connecting { .. } => tr("Verbindet…"),
        Phase::Queue { .. } => tr("Warteschlange"),
        Phase::Playing { .. } => tr("Spielt"),
        Phase::Disconnected { .. } => tr("Getrennt"),
        Phase::Dead { .. } => tr("Gestorben"),
        Phase::Crashed { .. } => tr("Abgestürzt"),
        Phase::Banned { .. } => tr("Gesperrt"),
        Phase::LoginNeeded => tr("Anmeldung nötig"),
        Phase::Failed { .. } => tr("Fehlgeschlagen"),
        Phase::NetworkLost { .. } | Phase::NoNetwork => tr("Keine Verbindung"),
    }
}

/// The full status line of `g`.
pub fn detail(g: &Game, m: &Model) -> String {
    stopped_ban(g, m).unwrap_or_else(|| status(g, m.preparing.get(&g.batch_id).copied()))
}

/// Why `g` dropped, in a few words, while it is down.
pub fn reason(g: &Game, m: &Model) -> Option<String> {
    if g.stopping {
        return None;
    }
    if let Some(fault) = g.hint.proxy.filter(|_| matches!(g.phase, Phase::Connecting { .. } | Phase::Disconnected { .. })) {
        return Some(proxy_status(fault));
    }
    let said = |r: &str| Some(r.trim().to_owned()).filter(|r| !r.is_empty());
    let last = m.history.get(&g.key).and_then(|log| log.back()).filter(|e| e.kind != Kind::Connected);
    match &g.phase {
        Phase::Disconnected { reason, .. } | Phase::Dead { reason, .. } | Phase::Banned { reason, .. } | Phase::Failed { reason } => said(reason),
        Phase::Connecting { .. } if g.reconnect_attempts > 0 => Some(match last.and_then(|e| said(&e.reason)) {
            Some(why) => trf("Getrennt · %@", &[&why]),
            None => tr("Getrennt"),
        }),
        Phase::Crashed { code: Some(code) } => Some(trf("Beendet mit Code %lld", &[code])),
        Phase::Restarting { .. } => Some(tr("Nach einem Absturz")),
        Phase::LoginNeeded => Some(tr("Anmeldung abgelaufen")),
        Phase::Stopped => last.filter(|e| e.kind == Kind::Banned).and_then(|e| said(&e.reason)),
        _ => None,
    }
}

/// One event of a game's history as the popover lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Moment {
    pub when: String,
    pub what: String,
    pub reason: String,
}

/// The newest events of game `key`, newest first.
pub fn moments(key: &str, m: &Model) -> Vec<Moment> {
    m.events(key)
        .into_iter()
        .take(HISTORY)
        .map(|e| {
            let what = match e.kind {
                Kind::Connected => tr("Verbunden"),
                Kind::Disconnected => tr("Getrennt"),
                Kind::Kicked => tr("Gekickt"),
                Kind::Banned => tr("Gesperrt"),
                Kind::Crashed => tr("Abgestürzt"),
            };
            let reason = if e.reason.trim().is_empty() { e.server.clone().unwrap_or_default() } else { e.reason.trim().to_owned() };
            Moment { when: when(e.at), what, reason }
        })
        .collect()
}

/// When `at`, seconds since 1970, was: the time today, otherwise the day.
pub fn when(at: u64) -> String {
    crate::ui::instance_files::when(i64::try_from(at).unwrap_or(i64::MAX))
}

/// The newest event of game `key` that changed how it stands, for the table's "Seit".
pub fn last_change<'a>(key: &str, m: &'a Model) -> Option<&'a Entry> {
    m.history.get(key).and_then(|log| log.back())
}

/// The search words of `query`, lowercase.
pub fn words(query: &str) -> Vec<String> {
    query.to_lowercase().split_whitespace().map(str::to_owned).collect()
}

/// Whether every one of `words` occurs in the name, batch, server or status of `g`.
pub fn found(g: &Game, m: &Model, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let batch = m.batch(&g.batch_id);
    let server = g.phase.server().or_else(|| batch.and_then(|b| b.server.as_deref())).unwrap_or_default();
    let state = if g.phase.needs_attention() {
        tr("braucht dich")
    } else if g.phase.is_active() {
        tr("läuft")
    } else {
        tr("gestoppt")
    };
    let text = [g.name.as_str(), batch.map(|b| b.name.as_str()).unwrap_or_default(), server, &detail(g, m), &state].join(" ").to_lowercase();
    words.iter().all(|w| text.contains(w.as_str()))
}

/// Whether `g` shows under `show`. "Läuft" counts only games with a process that is not waiting.
pub fn fits(g: &Game, show: Show) -> bool {
    match show {
        Show::All => true,
        Show::Need(need) => g.phase.need() == Some(need),
        Show::Running => g.phase.is_active() && !g.phase.waiting() && !g.phase.needs_attention(),
        Show::Stopped => !g.phase.is_active() && !g.phase.needs_attention(),
    }
}

/// "Startet 12/200 · 3 warten auf Arbeitsspeicher".
pub fn run_line(run: &Run) -> String {
    crate::ui::model::run_line(run)
}

/// "Server weg seit 14:10 · 48 verbinden sich neu · 2 haben aufgegeben", led by the batch name when `named`.
pub fn outage_line(m: &Model, batch_id: &str, outage: &Outage, named: bool) -> String {
    let mut parts = Vec::new();
    if named && let Some(b) = m.batch(batch_id) {
        parts.push(b.name.clone());
    }
    parts.push(match outage.since {
        Some(at) => trf("Server weg seit %@", &[&clock(at)]),
        None => tr("Server weg"),
    });
    let games = || outage.keys.iter().filter_map(|k| m.games.get(k));
    let back = games().filter(|g| matches!(g.phase, Phase::Connecting { .. })).count();
    let gave_up = outage.keys.len().saturating_sub(back);
    if back > 0 {
        parts.push(if back == 1 { tr("1 verbindet sich neu") } else { trf("%lld verbinden sich neu", &[&back]) });
    }
    if gave_up > 0 {
        parts.push(if gave_up == 1 { tr("1 hat aufgegeben") } else { trf("%lld haben aufgegeben", &[&gave_up]) });
    }
    parts.join(" · ")
}

fn clock(at: u64) -> String {
    let Some(time) = chrono::DateTime::from_timestamp(i64::try_from(at).unwrap_or(i64::MAX), 0) else { return String::new() };
    let local = time.with_timezone(&chrono::Local);
    if local.date_naive() == chrono::Local::now().date_naive() { local.format("%H:%M").to_string() } else { local.format("%d.%m. %H:%M").to_string() }
}

/// "7 Anmeldung": how many need this kind of work.
pub fn need_label(need: Need, count: usize) -> String {
    match need {
        Need::SignIn => trf("%lld Anmeldung", &[&count]),
        Need::Disconnected => trf("%lld Getrennt", &[&count]),
        Need::Dead => trf("%lld Gestorben", &[&count]),
        Need::Crashed => trf("%lld Abgestürzt", &[&count]),
        Need::Failed => trf("%lld Fehlgeschlagen", &[&count]),
        Need::Banned => trf("%lld Gesperrt", &[&count]),
    }
}

/// What fixes a kind of work for all of its games.
#[derive(Debug)]
pub enum Fix {
    /// Hands these events to the core.
    Events(Vec<Event>),
    /// The accounts need signing in again on the accounts page.
    SignIn(Vec<Uuid>),
}

/// The one action that fixes `need` for all of `keys`, with its label; none for banned accounts or offline sign-ins.
pub fn fix(need: Need, keys: &[String], m: &Model) -> Option<(String, Fix)> {
    let many = keys.len() > 1;
    let pick = |all: &str, one: &str| if many { tr(all) } else { tr(one) };
    Some(match need {
        Need::SignIn => {
            let mut accounts: Vec<Uuid> = keys.iter().filter_map(|k| m.games.get(k)).map(|g| g.account_id).collect();
            accounts.sort_unstable();
            accounts.dedup();
            let accounts: Vec<&crate::core::account::Account> = accounts.into_iter().filter_map(|id| m.account(id)).filter(|a| a.kind == AccountKind::Microsoft).collect();
            match accounts.as_slice() {
                [] => return None,
                [one] => (trf("%@ anmelden", &[&one.name]), Fix::SignIn(vec![one.id])),
                more => (tr("Alle anmelden"), Fix::SignIn(more.iter().map(|a| a.id).collect())),
            }
        }
        Need::Disconnected => (pick("Alle verbinden", "Verbinden"), Fix::Events(vec![Event::ReconnectMany(keys.to_vec())])),
        Need::Dead => (pick("Alle wiederbeleben", "Wiederbeleben"), Fix::Events(keys.iter().map(|k| Event::Respawn(k.clone())).collect())),
        Need::Crashed | Need::Failed => (pick("Alle starten", "Starten"), Fix::Events(vec![Event::StartMany(keys.to_vec())])),
        Need::Banned => return None,
    })
}

/// The games of `batch` (all when `None`) that need `need`, in name order.
pub fn keys_of(need: Need, batch: Option<&str>, m: &Model) -> Vec<String> {
    let mut games: Vec<&Game> = m.games.values().filter(|g| batch.is_none_or(|b| g.batch_id == b) && g.phase.need() == Some(need)).collect();
    games.sort_by_key(|g| (g.name.to_lowercase(), g.key.clone()));
    games.into_iter().map(|g| g.key.clone()).collect()
}

/// One action of the selection bar: its label with how many it acts on when that is not all, and why the rest are skipped.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub id: &'static str,
    pub label: String,
    pub name: String,
    pub able: usize,
    pub note: Option<String>,
}

/// The selection bar's actions for `keys`.
pub fn offers(keys: &[String], m: &Model) -> [Offer; 5] {
    let Able { start, stop, restart, reconnect, message } = m.able(keys);
    let total = keys.iter().filter(|k| m.games.contains_key(*k)).count();
    let offer = |id: &'static str, name: String, able: usize, why: String| {
        let skipped = total.saturating_sub(able);
        let partial = able > 0 && skipped > 0;
        let label = if partial { trf("%@ (%lld)", &[&name, &able]) } else { name.clone() };
        Offer { id, label, name, able, note: partial.then(|| trf("%lld übersprungen (%@)", &[&skipped, &why])) }
    };
    [
        offer("start", tr("Starten"), start, tr("schon gestartet oder Anmeldung nötig")),
        offer("stop", tr("Stoppen"), stop, tr("nicht gestartet")),
        offer("restart", tr("Neu starten"), restart, tr("Anmeldung nötig oder wird gestoppt")),
        offer("reconnect", tr("Neu verbinden"), reconnect, tr("nicht im Spiel oder gesperrt")),
        offer("message", tr("Nachricht senden…"), message, tr("auf keinem Server")),
    ]
}
