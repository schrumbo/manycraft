//! Sends the incidents that raise attention to the phone through Discord, Telegram or ntfy.
//! Webhook address, bot token and ntfy topic live only in the OS keychain and never print.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::i18n::{tr, trf};
use super::{account, drive, settings};

const TELEGRAM_API: &str = "https://api.telegram.org";
/// The ntfy server a new setup uses.
pub const NTFY_DEFAULT: &str = "https://ntfy.sh";
const TIMEOUT: Duration = Duration::from_secs(15);
const QUIET: Duration = Duration::from_secs(5);
const DROP_HOLD: Duration = Duration::from_secs(60);
const BACK_HOLD: Duration = Duration::from_secs(30);
const MAX_HOLD: Duration = Duration::from_secs(120);
const RETRIES: [u64; 3] = [5, 30, 120];
const MAX_NAMES: usize = 10;
const MAX_REASON: usize = 100;
const MAX_WAIT: Duration = Duration::from_secs(600);

/// Where messages for the phone go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    #[default]
    Discord,
    Telegram,
    Ntfy,
}

impl Service {
    /// Every service, in the order the picker shows them.
    pub const ALL: [Service; 3] = [Service::Discord, Service::Telegram, Service::Ntfy];

    /// The service's own name.
    pub fn name(self) -> &'static str {
        match self {
            Service::Discord => "Discord",
            Service::Telegram => "Telegram",
            Service::Ntfy => "ntfy",
        }
    }

    fn per_minute(self) -> usize {
        match self {
            Service::Discord => 30,
            Service::Telegram | Service::Ntfy => 20,
        }
    }

    fn max_chars(self) -> usize {
        match self {
            Service::Discord => 2000,
            Service::Telegram => 4096,
            Service::Ntfy => 4000,
        }
    }

    /// The keychain slot of this service's secret.
    pub fn slot(self) -> Slot {
        match self {
            Service::Discord => Slot::DiscordUrl,
            Service::Telegram => Slot::TelegramToken,
            Service::Ntfy => Slot::NtfyTopic,
        }
    }
}

/// The phone settings in `settings.json`; the secrets are not part of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Phone {
    pub enabled: bool,
    pub service: Service,
    pub telegram_chat: String,
    pub ntfy_server: String,
    /// Whether accounts that come back after a reported drop are reported too.
    pub reconnected: bool,
}

impl Default for Phone {
    fn default() -> Self {
        Self { enabled: false, service: Service::Discord, telegram_chat: String::new(), ntfy_server: NTFY_DEFAULT.to_owned(), reconnected: false }
    }
}

/// A secret that never shows in Debug output, logs or errors, and has no serde form.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into().trim().to_owned())
    }

    /// The plain value, for the request that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// A secret's entry in the keychain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    DiscordUrl,
    TelegramToken,
    NtfyTopic,
}

impl Slot {
    fn account(self) -> &'static str {
        match self {
            Slot::DiscordUrl => "phone-discord-webhook",
            Slot::TelegramToken => "phone-telegram-token",
            Slot::NtfyTopic => "phone-ntfy-topic",
        }
    }
}

static KEPT: LazyLock<Mutex<HashMap<Slot, String>>> = LazyLock::new(Mutex::default);

fn fenced() -> bool {
    cfg!(test) || drive::on()
}

fn entry(slot: Slot) -> Result<keyring::Entry, String> {
    keyring::Entry::new(account::KEYCHAIN_SERVICE, slot.account()).map_err(|_| tr("Der Passwortspeicher des Systems ist nicht erreichbar."))
}

/// Reads a secret; empty when none is stored. A driven or tested app keeps secrets in memory instead.
pub fn read_secret(slot: Slot) -> Result<Secret, String> {
    if fenced() {
        return Ok(Secret::new(lock(&KEPT).get(&slot).cloned().unwrap_or_default()));
    }
    match entry(slot)?.get_password() {
        Ok(value) => Ok(Secret::new(value)),
        Err(keyring::Error::NoEntry) => Ok(Secret::default()),
        Err(_) => Err(tr("Der Passwortspeicher des Systems ist nicht erreichbar.")),
    }
}

/// Stores a secret; an empty one removes the entry.
pub fn store_secret(slot: Slot, secret: &Secret) -> Result<(), String> {
    if fenced() {
        let mut kept = lock(&KEPT);
        if secret.is_empty() {
            kept.remove(&slot);
        } else {
            kept.insert(slot, secret.expose().to_owned());
        }
        return Ok(());
    }
    let entry = entry(slot)?;
    let result = if secret.is_empty() {
        match entry.delete_credential() {
            Err(keyring::Error::NoEntry) => Ok(()),
            other => other,
        }
    } else {
        entry.set_password(secret.expose())
    };
    result.map_err(|_| tr("Der Passwortspeicher des Systems hat die Angabe nicht gespeichert."))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Everything one send needs, secret included.
#[derive(Clone, PartialEq)]
pub struct Target {
    service: Service,
    secret: Secret,
    chat: String,
    server: String,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target").field("service", &self.service).finish_non_exhaustive()
    }
}

impl Target {
    /// The target for `phone` with `secret`, or what is still missing.
    pub fn new(phone: &Phone, secret: Secret) -> Result<Self, String> {
        let missing = match phone.service {
            Service::Discord if secret.is_empty() => Some(tr("Trag zuerst die Webhook-URL ein.")),
            Service::Telegram if secret.is_empty() => Some(tr("Trag zuerst den Bot-Token ein.")),
            Service::Telegram if phone.telegram_chat.trim().is_empty() => Some(tr("Trag zuerst die Chat-ID ein.")),
            Service::Ntfy if secret.is_empty() => Some(tr("Trag zuerst ein Thema ein.")),
            _ => None,
        };
        if let Some(missing) = missing {
            return Err(missing);
        }
        let server = phone.ntfy_server.trim().trim_end_matches('/');
        Ok(Self { service: phone.service, secret, chat: phone.telegram_chat.trim().to_owned(), server: if server.is_empty() { NTFY_DEFAULT.to_owned() } else { server.to_owned() } })
    }

    /// The target the settings describe, with its secret from the keychain.
    pub fn load(phone: &Phone) -> Result<Self, String> {
        Self::new(phone, read_secret(phone.service.slot())?)
    }
}

/// A fresh ntfy topic nobody can guess: `manycraft-` and 20 random letters and digits from `random`.
pub fn new_topic(random: u128) -> Secret {
    const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let tail: String = (0..20).map(|i| ALPHABET[((random >> (i * 5)) & 31) as usize] as char).collect();
    Secret::new(format!("manycraft-{tail}"))
}

/// The address that opens `topic` on `server` in the ntfy app or a browser, for the QR code.
pub fn ntfy_link(server: &str, topic: &str) -> Secret {
    let server = server.trim().trim_end_matches('/');
    let server = if server.is_empty() { NTFY_DEFAULT } else { server };
    let server = if server.contains("://") { server.to_owned() } else { format!("https://{server}") };
    Secret::new(format!("{server}/{}", topic.trim()))
}

/// One HTTP request to a service. The address can hold the secret, so it never prints.
pub struct Request {
    url: Secret,
    body: serde_json::Value,
}

/// The request that posts `text` to `target`; Telegram's API lives at `telegram`.
pub fn request(target: &Target, text: &str, telegram: &str) -> Request {
    let text = cut(text, target.service.max_chars());
    match target.service {
        Service::Discord => Request { url: target.secret.clone(), body: serde_json::json!({ "content": text, "username": "Manycraft", "allowed_mentions": { "parse": [] } }) },
        Service::Telegram => Request {
            url: Secret::new(format!("{}/bot{}/sendMessage", telegram.trim_end_matches('/'), target.secret.expose())),
            body: serde_json::json!({ "chat_id": target.chat, "text": text, "disable_web_page_preview": true }),
        },
        Service::Ntfy => Request { url: Secret::new(target.server.clone()), body: serde_json::json!({ "topic": target.secret.expose(), "message": text, "title": "Manycraft" }) },
    }
}

/// Why a send did not arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The service refused this address, token, chat or topic; trying again does not help.
    Refused(u16),
    /// Too many messages; the service asks to wait this long.
    Busy(Duration),
    /// The service has trouble of its own.
    Down(u16),
    Offline,
    BadAddress,
}

impl Failure {
    fn retry(&self) -> bool {
        matches!(self, Failure::Busy(_) | Failure::Down(_) | Failure::Offline)
    }

    /// What happened, in plain words with a next step, for `service`.
    pub fn text(&self, service: Service) -> String {
        let name = service.name();
        match (self, service) {
            (Failure::Refused(401 | 403 | 404), Service::Discord) => tr("Discord kennt diese Webhook-URL nicht. Kopiere sie in Discord neu."),
            (Failure::Refused(401 | 404), Service::Telegram) => tr("Telegram kennt diesen Bot-Token nicht. Kopiere ihn bei @BotFather neu."),
            (Failure::Refused(400 | 403), Service::Telegram) => tr("Der Bot darf in diesen Chat nicht schreiben. Schreib dem Bot eine Nachricht und such die Chat-ID neu."),
            (Failure::Refused(401 | 403), Service::Ntfy) => tr("Dieses ntfy-Thema ist geschützt. Nimm ein anderes Thema."),
            (Failure::Refused(404), Service::Ntfy) => tr("Unter dieser Adresse antwortet kein ntfy-Server. Prüfe die Adresse."),
            (Failure::Refused(code), _) => trf("%@ hat die Meldung abgelehnt (Fehler %lld).", &[&name, code]),
            (Failure::Busy(_), _) => trf("%@ nimmt gerade keine Meldungen an. Versuche es später noch einmal.", &[&name]),
            (Failure::Down(_), _) => trf("%@ antwortet gerade nicht. Versuche es später noch einmal.", &[&name]),
            (Failure::Offline, _) => trf("Keine Verbindung zu %@. Prüfe das Internet.", &[&name]),
            (Failure::BadAddress, Service::Discord) => tr("Diese Webhook-URL ist ungültig. Kopiere sie in Discord neu."),
            (Failure::BadAddress, _) => tr("Diese Server-Adresse ist ungültig. Sie beginnt mit https://."),
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(concat!("Manycraft/", env!("CARGO_PKG_VERSION")))
        .proxy(ureq::Proxy::try_from_env())
        .build()
        .into()
}

fn valid_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "https" | "http") && u.host_str().is_some() && (!fenced() || local(&u)))
}

fn local(url: &url::Url) -> bool {
    matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback()) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback()) || url.host_str() == Some("localhost")
}

fn classify(e: &ureq::Error) -> Failure {
    match e {
        ureq::Error::BadUri(_) | ureq::Error::Http(_) | ureq::Error::HostNotFound => Failure::BadAddress,
        _ => Failure::Offline,
    }
}

/// Posts the request; the error never carries the address.
pub fn post(request: &Request) -> Result<(), Failure> {
    if !valid_url(request.url.expose()) {
        return Err(Failure::BadAddress);
    }
    let mut response = agent().post(request.url.expose()).send_json(&request.body).map_err(|e| classify(&e))?;
    let code = response.status().as_u16();
    if response.status().is_success() {
        return Ok(());
    }
    let header = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok());
    let body: serde_json::Value = response.body_mut().with_config().limit(64 << 10).read_json().unwrap_or_default();
    Err(failure(code, header, &body))
}

fn failure(code: u16, header: Option<f64>, body: &serde_json::Value) -> Failure {
    match code {
        429 => {
            let secs = header.or_else(|| body.get("retry_after").and_then(serde_json::Value::as_f64)).or_else(|| body.pointer("/parameters/retry_after").and_then(serde_json::Value::as_f64)).filter(|s| s.is_finite()).unwrap_or(5.0);
            Failure::Busy(Duration::from_secs_f64(secs.clamp(0.5, MAX_WAIT.as_secs_f64())))
        }
        500.. => Failure::Down(code),
        _ => Failure::Refused(code),
    }
}

/// Keeps sends to one service under its limit per minute.
pub struct Limiter {
    per_minute: usize,
    sent: VecDeque<Instant>,
    until: Option<Instant>,
}

impl Limiter {
    pub fn new(per_minute: usize) -> Self {
        Self { per_minute: per_minute.max(1), sent: VecDeque::new(), until: None }
    }

    /// How long to wait at `now` before the next send.
    pub fn wait(&mut self, now: Instant) -> Duration {
        let minute = Duration::from_secs(60);
        while self.sent.front().is_some_and(|t| now.saturating_duration_since(*t) >= minute) {
            self.sent.pop_front();
        }
        let full = if self.sent.len() >= self.per_minute { self.sent.front().map_or(Duration::ZERO, |t| (*t + minute).saturating_duration_since(now)) } else { Duration::ZERO };
        let held = self.until.map_or(Duration::ZERO, |u| u.saturating_duration_since(now));
        full.max(held)
    }

    /// Counts a send at `now`.
    pub fn record(&mut self, now: Instant) {
        self.sent.push_back(now);
    }

    /// Waits at least `wait` from `now`, as the service asked.
    pub fn hold(&mut self, now: Instant, wait: Duration) {
        let until = now + wait.min(MAX_WAIT);
        self.until = Some(self.until.map_or(until, |u| u.max(until)));
    }
}

static LIMITS: LazyLock<Mutex<HashMap<Service, Limiter>>> = LazyLock::new(Mutex::default);

fn wait_for(service: Service) -> Duration {
    lock(&LIMITS).entry(service).or_insert_with(|| Limiter::new(service.per_minute())).wait(Instant::now())
}

fn sent_to(service: Service, failure: Option<&Failure>) {
    let now = Instant::now();
    let mut limits = lock(&LIMITS);
    let limiter = limits.entry(service).or_insert_with(|| Limiter::new(service.per_minute()));
    limiter.record(now);
    if let Some(Failure::Busy(wait)) = failure {
        limiter.hold(now, *wait);
    }
}

fn telegram_api() -> String {
    std::env::var("MANYCRAFT_TELEGRAM_API").ok().filter(|u| drive::on() && !u.trim().is_empty()).unwrap_or_else(|| TELEGRAM_API.to_owned())
}

/// Sends `text` once to `target` right away, for the test button. Respects the limit instead of waiting for it.
pub fn send_now(target: &Target, text: &str) -> Result<(), String> {
    let wait = wait_for(target.service);
    if !wait.is_zero() {
        return Err(trf("Gerade wurden zu viele Meldungen geschickt. Versuche es in %lld Sekunden noch einmal.", &[&wait.as_secs().max(1)]));
    }
    let result = post(&request(target, text, &telegram_api()));
    sent_to(target.service, result.as_ref().err());
    result.map_err(|f| f.text(target.service))
}

/// The chat a Telegram bot last got a message from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    pub id: String,
    pub name: String,
}

fn last_chat(body: &serde_json::Value) -> Option<Chat> {
    let updates = body.get("result")?.as_array()?;
    updates.iter().rev().find_map(|u| {
        let chat = ["message", "edited_message", "channel_post", "my_chat_member"].iter().find_map(|k| u.get(*k)?.get("chat"))?;
        let id = chat.get("id")?;
        let id = id.as_i64().map(|n| n.to_string()).or_else(|| id.as_str().map(str::to_owned))?;
        let name = ["title", "first_name", "username"].iter().find_map(|k| chat.get(*k)?.as_str()).unwrap_or_default().to_owned();
        Some(Chat { id, name })
    })
}

/// Asks Telegram which chat last wrote to the bot with `token`.
pub fn find_chat(token: &Secret) -> Result<Chat, String> {
    if token.is_empty() {
        return Err(tr("Trag zuerst den Bot-Token ein."));
    }
    let url = Secret::new(format!("{}/bot{}/getUpdates", telegram_api().trim_end_matches('/'), token.expose()));
    if !valid_url(url.expose()) {
        return Err(Failure::BadAddress.text(Service::Telegram));
    }
    let mut response = agent().get(url.expose()).call().map_err(|e| classify(&e).text(Service::Telegram))?;
    let code = response.status().as_u16();
    let body: serde_json::Value = response.body_mut().with_config().limit(1 << 20).read_json().unwrap_or_default();
    if !response.status().is_success() {
        return Err(failure(code, None, &body).text(Service::Telegram));
    }
    last_chat(&body).ok_or_else(|| tr("Noch keine Nachricht an den Bot gefunden. Schreib ihm in Telegram etwas und versuche es noch einmal."))
}

/// What happened to a message the app sent on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Arrived,
    Lost(String),
}

/// Sends messages one after another on its own thread, within each service's limit, merging what piles up.
pub struct Outbox {
    tx: mpsc::Sender<String>,
}

impl Outbox {
    /// Starts the sender; `report` hears each outcome.
    pub fn start(report: impl Fn(Outcome) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<String>();
        let _ = std::thread::Builder::new().name("phone".into()).spawn(move || run(&rx, &report));
        Self { tx }
    }

    /// Queues `text` for the phone; gives it back when the sender is gone.
    pub fn send(&self, text: String) -> Result<(), String> {
        self.tx.send(text).map_err(|e| e.0)
    }
}

fn pack(queue: &VecDeque<String>, max: usize) -> (String, usize) {
    let mut text = String::new();
    let mut used = 0;
    for next in queue {
        if used > 0 && text.chars().count() + 2 + next.chars().count() > max {
            break;
        }
        if used > 0 {
            text.push_str("\n\n");
        }
        text.push_str(next);
        used += 1;
    }
    (text, used)
}

fn run(rx: &mpsc::Receiver<String>, report: &dyn Fn(Outcome)) {
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut tries = 0usize;
    loop {
        if queue.is_empty() {
            let Ok(first) = rx.recv() else { return };
            queue.push_back(first);
            tries = 0;
        }
        let service = settings::active().phone.service;
        std::thread::sleep(wait_for(service));
        queue.extend(rx.try_iter());
        let phone = settings::active().phone.clone();
        if !phone.enabled {
            queue.clear();
            continue;
        }
        let target = match Target::load(&phone) {
            Ok(target) => target,
            Err(e) => {
                report(Outcome::Lost(e));
                queue.clear();
                continue;
            }
        };
        let (text, used) = pack(&queue, target.service.max_chars());
        let result = post(&request(&target, &text, &telegram_api()));
        sent_to(target.service, result.as_ref().err());
        match result {
            Ok(()) => {
                queue.drain(..used);
                tries = 0;
                report(Outcome::Arrived);
            }
            Err(f) if f.retry() && tries < RETRIES.len() => {
                if !matches!(f, Failure::Busy(_)) {
                    std::thread::sleep(Duration::from_secs(RETRIES[tries]));
                }
                tries += 1;
            }
            Err(f) => {
                report(Outcome::Lost(f.text(target.service)));
                queue.clear();
            }
        }
    }
}

/// What happened to an account, as the phone hears it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum What {
    Banned,
    Login,
    GaveUp,
    Crashed,
    Failed,
    Died,
    Kicked,
    Dropped,
    Back,
}

impl What {
    fn reconnecting(self) -> bool {
        matches!(self, What::Kicked | What::Dropped)
    }

    fn hold(self) -> Duration {
        match self {
            What::Kicked | What::Dropped => DROP_HOLD,
            What::Back => BACK_HOLD,
            _ => Duration::ZERO,
        }
    }
}

/// One account's incident.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incident {
    pub key: String,
    pub name: String,
    pub what: What,
    pub reason: String,
    pub server: Option<String>,
    /// A drop that came back before it was reported.
    pub healed: bool,
}

impl Incident {
    pub fn new(key: &str, name: &str, what: What, reason: &str, server: Option<&str>) -> Self {
        Self { key: key.to_owned(), name: name.to_owned(), what, reason: reason.to_owned(), server: server.map(str::to_owned), healed: false }
    }
}

struct Bucket {
    opened: Instant,
    last: Instant,
    entries: Vec<(Instant, Incident)>,
}

fn ready((at, e): &(Instant, Incident)) -> Instant {
    *at + if e.healed { Duration::ZERO } else { e.what.hold() }
}

impl Bucket {
    fn due(&self) -> Instant {
        let first = self.entries.iter().map(ready).min().unwrap_or(self.opened);
        first.max((self.last + QUIET).min(self.opened + MAX_HOLD))
    }
}

/// Collects incidents per batch into one message: drops wait a minute for their reconnect, so a server restart that heals is no news.
#[derive(Default)]
pub struct Coalescer {
    buckets: HashMap<String, Bucket>,
    reported: HashSet<String>,
}

impl Coalescer {
    /// Adds an incident of `batch` at `now`; gives the time its message is due, or `None` when it is no news.
    pub fn push(&mut self, batch: &str, incident: Incident, now: Instant) -> Option<Instant> {
        if incident.what == What::Back {
            let Some(bucket) = self.buckets.get_mut(batch).filter(|b| b.entries.iter().any(|(_, e)| e.key == incident.key)) else {
                if !self.reported.remove(&incident.key) {
                    return None;
                }
                return self.add(batch, incident, now);
            };
            bucket.last = now;
            if let Some((_, entry)) = bucket.entries.iter_mut().find(|(_, e)| e.key == incident.key && e.what != What::Back) {
                if entry.what.reconnecting() {
                    entry.healed = true;
                } else {
                    bucket.entries.retain(|(_, e)| e.key != incident.key);
                }
            }
            let due = (!bucket.entries.is_empty()).then(|| bucket.due());
            if due.is_none() {
                self.buckets.remove(batch);
            }
            return due;
        }
        self.add(batch, incident, now)
    }

    fn add(&mut self, batch: &str, incident: Incident, now: Instant) -> Option<Instant> {
        let bucket = self.buckets.entry(batch.to_owned()).or_insert_with(|| Bucket { opened: now, last: now, entries: Vec::new() });
        bucket.last = now;
        bucket.entries.retain(|(_, e)| e.key != incident.key);
        bucket.entries.push((now, incident));
        Some(bucket.due())
    }

    /// When the next message is due, if any is pending.
    pub fn next_due(&self) -> Option<Instant> {
        self.buckets.values().map(Bucket::due).min()
    }

    /// Takes the batches whose message is due at `now`, keeping only incidents `current` still confirms.
    pub fn take_due(&mut self, now: Instant, current: impl Fn(&Incident) -> bool) -> Vec<(String, Vec<Incident>)> {
        let due: Vec<String> = self.buckets.iter().filter(|(_, b)| b.due() <= now).map(|(id, _)| id.clone()).collect();
        let mut out = Vec::new();
        for id in due {
            let Some(bucket) = self.buckets.remove(&id) else { continue };
            let (now_due, later): (Vec<_>, Vec<_>) = bucket.entries.into_iter().partition(|e| ready(e) <= now + QUIET);
            if let (Some(opened), Some(last)) = (later.iter().map(|(at, _)| *at).min(), later.iter().map(|(at, _)| *at).max()) {
                self.buckets.insert(id.clone(), Bucket { opened, last, entries: later });
            }
            let kept: Vec<Incident> = now_due.into_iter().map(|(_, e)| e).filter(|e| current(e)).collect();
            for e in &kept {
                if e.what == What::Back || e.healed {
                    self.reported.remove(&e.key);
                } else {
                    self.reported.insert(e.key.clone());
                }
            }
            if !kept.is_empty() {
                out.push((id, kept));
            }
        }
        out
    }

    /// Forgets `key`, for an instance the user stopped.
    pub fn forget(&mut self, key: &str) {
        self.reported.remove(key);
        for bucket in self.buckets.values_mut() {
            bucket.entries.retain(|(_, e)| e.key != key);
        }
        self.buckets.retain(|_, b| !b.entries.is_empty());
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Is(What),
    Healed,
}

fn phrase(group: Group, n: usize) -> String {
    let one = n == 1;
    let key = match group {
        Group::Is(What::Banned) => if one { "%lld Account gesperrt" } else { "%lld Accounts gesperrt" },
        Group::Is(What::Login) => if one { "%lld Account muss sich anmelden" } else { "%lld Accounts müssen sich anmelden" },
        Group::Is(What::GaveUp) => if one { "%lld Account getrennt" } else { "%lld Accounts getrennt" },
        Group::Is(What::Crashed) => if one { "%lld Account abgestürzt" } else { "%lld Accounts abgestürzt" },
        Group::Is(What::Failed) => if one { "%lld Account startet nicht" } else { "%lld Accounts starten nicht" },
        Group::Is(What::Died) => if one { "%lld Account gestorben" } else { "%lld Accounts gestorben" },
        Group::Is(What::Kicked) => if one { "%lld Account gekickt" } else { "%lld Accounts gekickt" },
        Group::Is(What::Dropped) => if one { "%lld Account getrennt" } else { "%lld Accounts getrennt" },
        Group::Is(What::Back) => if one { "%lld Account wieder verbunden" } else { "%lld Accounts wieder verbunden" },
        Group::Healed => if one { "%lld Account kurz getrennt" } else { "%lld Accounts kurz getrennt" },
    };
    trf(key, &[&n])
}

fn tail(group: Group, n: usize) -> Option<String> {
    match group {
        Group::Is(What::Kicked | What::Dropped) => Some(tr(if n == 1 { "verbindet sich neu" } else { "verbinden sich neu" })),
        Group::Is(What::GaveUp) => Some(tr("neu verbinden aufgegeben")),
        Group::Healed => Some(tr("wieder verbunden")),
        _ => None,
    }
}

/// `reason` as one short line without colour codes, line breaks or IP addresses.
pub fn clean_reason(reason: &str) -> String {
    let mut plain = String::new();
    let mut chars = reason.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            plain.push(if c.is_control() { ' ' } else { c });
        }
    }
    let words: Vec<String> = plain.split_whitespace().map(|w| if is_ip(w) { "…".to_owned() } else { w.to_owned() }).collect();
    cut(&words.join(" "), MAX_REASON)
}

fn is_ip(word: &str) -> bool {
    let word = word.trim_matches(|c: char| !c.is_ascii_hexdigit() && c != ':' && c != '.');
    let host = word.rsplit_once(':').filter(|(h, p)| h.contains('.') && p.chars().all(|c| c.is_ascii_digit())).map_or(word, |(h, _)| h);
    host.parse::<std::net::IpAddr>().is_ok() || word.parse::<std::net::IpAddr>().is_ok()
}

/// The host of a server address, without the default port; `None` for a bare IP address.
pub fn host(server: &str) -> Option<String> {
    let server = server.trim();
    let host = server.strip_suffix(":25565").unwrap_or(server);
    if host.is_empty() || is_ip(host) {
        return None;
    }
    Some(host.to_owned())
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn same<'a>(values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut values = values.peekable();
    let first = *values.peek()?;
    values.all(|v| v == first).then_some(first).filter(|v| !v.is_empty())
}

/// The message for one batch's incidents, or `None` when nothing in it is news; healed drops and comebacks count only with `reconnected`.
pub fn format(batch: &str, incidents: &[Incident], reconnected: bool) -> Option<String> {
    let mut groups: Vec<(Group, Vec<&Incident>)> = Vec::new();
    for incident in incidents {
        let group = if incident.healed { Group::Healed } else { Group::Is(incident.what) };
        if !reconnected && matches!(group, Group::Healed | Group::Is(What::Back)) {
            continue;
        }
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, list)) => list.push(incident),
            None => groups.push((group, vec![incident])),
        }
    }
    if groups.is_empty() {
        return None;
    }
    groups.sort_by_key(|(g, _)| *g);
    let lines: Vec<String> = groups
        .into_iter()
        .map(|(group, list)| {
            let n = list.len();
            let mut head = format!("{batch}: {}", phrase(group, n));
            let reasons: Vec<String> = list.iter().map(|i| clean_reason(&i.reason)).collect();
            if group != Group::Is(What::Back)
                && let Some(reason) = same(reasons.iter().map(String::as_str))
            {
                head.push_str(&format!(" ({reason})"));
            }
            if let Some(tail) = tail(group, n) {
                head.push_str(&format!(" – {tail}"));
            }
            head.push('.');
            let mut names = list.iter().take(MAX_NAMES).map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ");
            if n > MAX_NAMES {
                names = trf("%@ und %lld weitere", &[&names, &(n - MAX_NAMES)]);
            }
            let hosts: Vec<Option<String>> = list.iter().map(|i| i.server.as_deref().and_then(host)).collect();
            if let Some(host) = same(hosts.iter().map(|h| h.as_deref().unwrap_or_default())) {
                names.push_str(&format!(" · {host}"));
            }
            format!("{head}\n{names}")
        })
        .collect();
    Some(lines.join("\n"))
}

/// The test message.
pub fn test_text() -> String {
    tr("Manycraft: Testnachricht. Hier kommen Meldungen an, wenn ein Account getrennt wird.")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::Receiver;

    use super::*;

    fn at(t0: Instant, secs: u64) -> Instant {
        t0 + Duration::from_secs(secs)
    }

    fn dropped(key: &str) -> Incident {
        Incident::new(key, key, What::Dropped, "Server closed", Some("mc.example.net:25565"))
    }

    #[test]
    fn a_new_topic_is_long_unguessable_and_opens_on_the_right_server() {
        let a = new_topic(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let b = new_topic(0xfedc_ba98_7654_3210_fedc_ba98_7654_3210);
        assert_ne!(a.expose(), b.expose());
        assert!(a.expose().starts_with("manycraft-") && a.expose().len() == 30);
        assert!(a.expose()[10..].chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_eq!(ntfy_link("", "t").expose(), "https://ntfy.sh/t");
        assert_eq!(ntfy_link("https://push.example.org/", " t ").expose(), "https://push.example.org/t");
        assert_eq!(ntfy_link("push.example.org", "t").expose(), "https://push.example.org/t");
    }

    #[test]
    fn a_restart_that_heals_within_a_minute_is_one_message_or_none() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        for n in 0..50 {
            c.push("farm", dropped(&format!("a{n}")), at(t0, n % 3));
        }
        for n in 0..50 {
            c.push("farm", Incident::new(&format!("a{n}"), "", What::Back, "", None), at(t0, 30));
        }
        assert!(c.take_due(at(t0, 34), |_| true).is_empty());
        let due = c.take_due(at(t0, 35), |_| true);
        assert_eq!(due.len(), 1);
        assert_eq!(format("Farm", &due[0].1, false), None);
        let text = format("Farm", &due[0].1, true).unwrap();
        assert!(text.starts_with("Farm: 50 Accounts kurz getrennt (Server closed) – wieder verbunden."), "{text}");
        assert_eq!(text.lines().count(), 2);
        assert!(c.push("farm", Incident::new("a1", "", What::Back, "", None), at(t0, 90)).is_none());
    }

    #[test]
    fn drops_still_out_after_a_minute_are_reported_once_and_their_return_follows() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        for key in ["Alex", "Bert", "Carl"] {
            c.push("farm", dropped(key), t0);
        }
        let due = c.take_due(at(t0, 60), |_| true);
        let text = format("Farm", &due[0].1, false).unwrap();
        assert_eq!(text, "Farm: 3 Accounts getrennt (Server closed) – verbinden sich neu.\nAlex, Bert, Carl · mc.example.net");
        let due_back = c.push("farm", Incident::new("Alex", "Alex", What::Back, "", None), at(t0, 100)).unwrap();
        assert_eq!(due_back, at(t0, 130));
        let back = c.take_due(due_back, |_| true);
        assert_eq!(format("Farm", &back[0].1, true).unwrap(), "Farm: 1 Account wieder verbunden.\nAlex");
        assert_eq!(format("Farm", &back[0].1, false), None);
    }

    #[test]
    fn attention_flushes_after_a_quiet_spell_and_groups_by_kind() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        assert_eq!(c.push("farm", Incident::new("k1", "Alex", What::Banned, "Hacking", Some("mc.example.net")), t0), Some(at(t0, 5)));
        assert_eq!(c.push("farm", Incident::new("k2", "Bert", What::Crashed, "", None), at(t0, 3)), Some(at(t0, 8)));
        c.push("pvp", Incident::new("k3", "Carl", What::Login, "", None), at(t0, 3));
        let due = c.take_due(at(t0, 8), |_| true);
        assert_eq!(due.len(), 2);
        let farm = due.iter().find(|(b, _)| b == "farm").unwrap();
        assert_eq!(format("Farm", &farm.1, false).unwrap(), "Farm: 1 Account gesperrt (Hacking).\nAlex · mc.example.net\nFarm: 1 Account abgestürzt.\nBert");
    }

    #[test]
    fn a_steady_trickle_still_sends_within_two_minutes() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        let mut due = t0;
        for n in 0..60 {
            due = c.push("farm", Incident::new(&format!("k{n}"), "x", What::Crashed, "", None), at(t0, n * 4)).unwrap();
        }
        assert_eq!(due, at(t0, 120));
    }

    #[test]
    fn a_late_drop_in_an_open_batch_still_waits_its_full_minute() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.push("farm", dropped("Alex"), t0);
        c.push("farm", dropped("Bert"), at(t0, 30));
        let first = c.take_due(at(t0, 60), |_| true);
        assert_eq!(first[0].1.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Alex"]);
        assert_eq!(c.next_due(), Some(at(t0, 90)));
        c.push("farm", Incident::new("Bert", "", What::Back, "", None), at(t0, 75));
        let healed = c.take_due(at(t0, 80), |_| true);
        assert_eq!(format("Farm", &healed[0].1, false), None);
        assert_eq!(c.next_due(), None);
    }

    #[test]
    fn a_ban_is_not_held_back_by_a_pending_drop() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.push("farm", dropped("Alex"), t0);
        assert_eq!(c.push("farm", Incident::new("Bert", "Bert", What::Banned, "", None), at(t0, 10)), Some(at(t0, 15)));
        let due = c.take_due(at(t0, 15), |_| true);
        assert_eq!(due[0].1.iter().map(|e| e.what).collect::<Vec<_>>(), [What::Banned]);
        assert_eq!(c.next_due(), Some(at(t0, 60)));
    }

    #[test]
    fn piled_up_messages_are_sent_in_parts_that_fit() {
        let queue: VecDeque<String> = ["a".repeat(1500), "b".repeat(400), "c".repeat(200)].into_iter().collect();
        assert_eq!(pack(&queue, 2000).1, 2);
        assert_eq!(pack(&queue, 1000).1, 1);
        assert_eq!(pack(&queue, 1000).0.len(), 1500);
    }

    #[test]
    fn a_retry_after_that_is_not_a_number_falls_back_to_a_few_seconds() {
        assert_eq!(failure(429, Some(f64::NAN), &serde_json::Value::Null), Failure::Busy(Duration::from_secs(5)));
        assert_eq!(failure(429, Some(f64::INFINITY), &serde_json::Value::Null), Failure::Busy(Duration::from_secs(5)));
    }

    #[test]
    fn incidents_the_user_already_fixed_are_dropped_and_not_remembered() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.push("farm", dropped("Alex"), t0);
        assert!(c.take_due(at(t0, 60), |_| false).is_empty());
        assert!(c.push("farm", Incident::new("Alex", "", What::Back, "", None), at(t0, 70)).is_none());
    }

    #[test]
    fn an_attention_incident_replaces_the_reconnecting_drop() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.push("farm", dropped("Alex"), t0);
        c.push("farm", Incident::new("Alex", "Alex", What::GaveUp, "Server closed", None), at(t0, 20));
        let due = c.take_due(at(t0, 60), |_| true);
        assert_eq!(format("Farm", &due[0].1, false).unwrap(), "Farm: 1 Account getrennt (Server closed) – neu verbinden aufgegeben.\nAlex");
    }

    #[test]
    fn names_are_capped_and_mixed_reasons_left_out() {
        let list: Vec<Incident> = (0..12).map(|n| Incident::new(&n.to_string(), &format!("n{n}"), What::Banned, if n == 0 { "a" } else { "b" }, None)).collect();
        let text = format("Farm", &list, false).unwrap();
        assert_eq!(text, "Farm: 12 Accounts gesperrt.\nn0, n1, n2, n3, n4, n5, n6, n7, n8, n9 und 2 weitere");
    }

    #[test]
    fn reasons_lose_colour_codes_line_breaks_and_ip_addresses() {
        assert_eq!(clean_reason("§cBanned\nfrom §lserver"), "Banned from server");
        assert_eq!(clean_reason("Your IP 203.0.113.7 is banned"), "Your IP … is banned");
        assert_eq!(clean_reason("connect to 203.0.113.7:25565 failed"), "connect to … failed");
        assert_eq!(clean_reason("from [2001:db8::1]"), "from …");
        assert_eq!(clean_reason("Took 12:30 minutes"), "Took 12:30 minutes");
        assert_eq!(clean_reason(&"x".repeat(300)).chars().count(), MAX_REASON);
        assert_eq!(host("mc.example.net:25565").as_deref(), Some("mc.example.net"));
        assert_eq!(host("mc.example.net:25570").as_deref(), Some("mc.example.net:25570"));
        assert_eq!(host("192.168.1.20:25565"), None);
        assert_eq!(host("192.168.1.20"), None);
    }

    #[test]
    fn the_limiter_keeps_each_minute_under_its_cap_and_honours_holds() {
        let t0 = Instant::now();
        let mut l = Limiter::new(30);
        for n in 0..30 {
            assert_eq!(l.wait(t0 + Duration::from_millis(n * 100)), Duration::ZERO);
            l.record(t0 + Duration::from_millis(n * 100));
        }
        assert_eq!(l.wait(at(t0, 3)), Duration::from_secs(57));
        assert_eq!(l.wait(at(t0, 60)), Duration::ZERO);
        l.hold(at(t0, 60), Duration::from_secs(7));
        assert_eq!(l.wait(at(t0, 61)), Duration::from_secs(6));
        l.hold(at(t0, 61), Duration::from_secs(1));
        assert_eq!(l.wait(at(t0, 61)), Duration::from_secs(6));
        assert_eq!(Limiter::new(Service::Telegram.per_minute()).per_minute, 20);
    }

    #[test]
    fn rate_limit_answers_turn_into_waits() {
        assert_eq!(failure(429, Some(3.0), &serde_json::Value::Null), Failure::Busy(Duration::from_secs(3)));
        assert_eq!(failure(429, None, &serde_json::json!({"retry_after": 1.5})), Failure::Busy(Duration::from_millis(1500)));
        assert_eq!(failure(429, None, &serde_json::json!({"ok": false, "parameters": {"retry_after": 9}})), Failure::Busy(Duration::from_secs(9)));
        assert_eq!(failure(503, None, &serde_json::Value::Null), Failure::Down(503));
        assert_eq!(failure(404, None, &serde_json::Value::Null), Failure::Refused(404));
        assert!(Failure::Busy(Duration::ZERO).retry() && !Failure::Refused(401).retry());
    }

    fn phone(service: Service) -> Phone {
        Phone { enabled: true, service, telegram_chat: "42".into(), ntfy_server: "https://ntfy.example/".into(), ..Phone::default() }
    }

    #[test]
    fn payloads_fit_each_service() {
        let discord = Target::new(&phone(Service::Discord), Secret::new("https://discord.com/api/webhooks/1/abc")).unwrap();
        let r = request(&discord, "hi @everyone", TELEGRAM_API);
        assert_eq!(r.url.expose(), "https://discord.com/api/webhooks/1/abc");
        assert_eq!(&r.body, &serde_json::json!({"content": "hi @everyone", "username": "Manycraft", "allowed_mentions": {"parse": []}}));
        let telegram = Target::new(&phone(Service::Telegram), Secret::new("123:XYZ")).unwrap();
        let r = request(&telegram, "hi", "https://tg.example/");
        assert_eq!(r.url.expose(), "https://tg.example/bot123:XYZ/sendMessage");
        assert_eq!(&r.body, &serde_json::json!({"chat_id": "42", "text": "hi", "disable_web_page_preview": true}));
        let ntfy = Target::new(&phone(Service::Ntfy), Secret::new("farm-alerts")).unwrap();
        let r = request(&ntfy, "hi", TELEGRAM_API);
        assert_eq!(r.url.expose(), "https://ntfy.example");
        assert_eq!(&r.body, &serde_json::json!({"topic": "farm-alerts", "message": "hi", "title": "Manycraft"}));
        let long = request(&discord, &"x".repeat(5000), TELEGRAM_API);
        assert_eq!(long.body["content"].as_str().unwrap().chars().count(), 2000);
    }

    #[test]
    fn missing_fields_are_named() {
        assert!(Target::new(&phone(Service::Discord), Secret::default()).unwrap_err().contains("Webhook-URL"));
        let mut no_chat = phone(Service::Telegram);
        no_chat.telegram_chat = " ".into();
        assert!(Target::new(&no_chat, Secret::new("t")).unwrap_err().contains("Chat-ID"));
        let mut blank_server = phone(Service::Ntfy);
        blank_server.ntfy_server = String::new();
        assert_eq!(request(&Target::new(&blank_server, Secret::new("t")).unwrap(), "x", TELEGRAM_API).url.expose(), NTFY_DEFAULT);
    }

    #[test]
    fn secrets_never_show_in_debug_or_serde() {
        let secret = Secret::new("https://discord.com/api/webhooks/1/topsecret");
        assert_eq!(format!("{secret:?}"), "Secret(…)");
        let target = Target::new(&phone(Service::Telegram), Secret::new("123:topsecret")).unwrap();
        assert!(!format!("{target:?}").contains("topsecret"));
        let json = serde_json::to_string(&phone(Service::Discord)).unwrap();
        assert!(!json.contains("secret") && !json.contains("webhook"), "{json}");
        for f in [Failure::Refused(401), Failure::Refused(418), Failure::Offline, Failure::BadAddress, Failure::Down(500)] {
            for s in Service::ALL {
                assert!(!f.text(s).contains("topsecret"));
            }
        }
    }

    #[test]
    fn secrets_round_trip_in_the_fenced_store() {
        store_secret(Slot::NtfyTopic, &Secret::new(" topic ")).unwrap();
        assert_eq!(read_secret(Slot::NtfyTopic).unwrap().expose(), "topic");
        store_secret(Slot::NtfyTopic, &Secret::default()).unwrap();
        assert!(read_secret(Slot::NtfyTopic).unwrap().is_empty());
    }

    #[test]
    fn chat_ids_come_from_the_latest_update() {
        let body = serde_json::json!({"ok": true, "result": [
            {"update_id": 1, "message": {"chat": {"id": 11, "first_name": "Old"}}},
            {"update_id": 2, "message": {"chat": {"id": -100200, "title": "Farm group"}}}
        ]});
        assert_eq!(last_chat(&body), Some(Chat { id: "-100200".into(), name: "Farm group".into() }));
        assert_eq!(last_chat(&serde_json::json!({"ok": true, "result": []})), None);
    }

    struct Seen {
        line: String,
        body: String,
    }

    fn json(seen: &Seen) -> serde_json::Value {
        serde_json::from_str(&seen.body).unwrap()
    }

    fn fake(status: &'static str, extra: &'static str, reply: &'static str) -> (String, Receiver<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (mut length, mut chunked) = (0, false);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    let header = header.to_ascii_lowercase();
                    if let Some(v) = header.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    chunked |= header.starts_with("transfer-encoding:") && header.contains("chunked");
                }
                let mut body = Vec::new();
                if chunked {
                    loop {
                        let mut size = String::new();
                        reader.read_line(&mut size).unwrap();
                        let size = usize::from_str_radix(size.trim(), 16).unwrap();
                        let mut chunk = vec![0; size + 2];
                        reader.read_exact(&mut chunk).unwrap();
                        if size == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..size]);
                    }
                } else {
                    body.resize(length, 0);
                    reader.read_exact(&mut body).unwrap();
                }
                write!(stream, "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
                tx.send(Seen { line: line.trim().to_owned(), body: String::from_utf8(body).unwrap() }).unwrap();
            }
        });
        (base, rx)
    }

    #[test]
    fn posts_reach_a_local_server_for_every_service() {
        let (base, rx) = fake("204 No Content", "", "");
        let discord = Target::new(&phone(Service::Discord), Secret::new(format!("{base}/api/webhooks/1/abc"))).unwrap();
        post(&request(&discord, "Farm: 1 Account gesperrt.", TELEGRAM_API)).unwrap();
        let seen = rx.recv().unwrap();
        assert_eq!(seen.line, "POST /api/webhooks/1/abc HTTP/1.1");
        assert_eq!(json(&seen)["content"], "Farm: 1 Account gesperrt.");

        let (base, rx) = fake("200 OK", "", r#"{"ok":true}"#);
        let telegram = Target::new(&phone(Service::Telegram), Secret::new("123:XYZ")).unwrap();
        post(&request(&telegram, "hi", &base)).unwrap();
        let seen = rx.recv().unwrap();
        assert_eq!(seen.line, "POST /bot123:XYZ/sendMessage HTTP/1.1");
        assert_eq!(json(&seen)["chat_id"], "42");

        let (base, rx) = fake("200 OK", "", "{}");
        let mut p = phone(Service::Ntfy);
        p.ntfy_server = base;
        post(&request(&Target::new(&p, Secret::new("farm")).unwrap(), "hi", TELEGRAM_API)).unwrap();
        let seen = rx.recv().unwrap();
        assert_eq!(seen.line, "POST / HTTP/1.1");
        assert_eq!(json(&seen)["topic"], "farm");
    }

    #[test]
    fn refusals_and_rate_limits_from_a_local_server_are_read() {
        let (base, _rx) = fake("429 Too Many Requests", "Retry-After: 2\r\n", r#"{"retry_after": 2}"#);
        let discord = Target::new(&phone(Service::Discord), Secret::new(format!("{base}/api/webhooks/1/abc"))).unwrap();
        assert_eq!(post(&request(&discord, "x", TELEGRAM_API)), Err(Failure::Busy(Duration::from_secs(2))));
        let (base, _rx) = fake("404 Not Found", "", r#"{"message": "Unknown Webhook"}"#);
        let discord = Target::new(&phone(Service::Discord), Secret::new(format!("{base}/api/webhooks/1/abc"))).unwrap();
        let failure = post(&request(&discord, "x", TELEGRAM_API)).unwrap_err();
        assert_eq!(failure, Failure::Refused(404));
        assert!(failure.text(Service::Discord).contains("Webhook-URL"));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let gone = Target::new(&phone(Service::Discord), Secret::new(format!("http://{closed}/api/webhooks/1/abc"))).unwrap();
        assert_eq!(post(&request(&gone, "x", TELEGRAM_API)), Err(Failure::Offline));
        let junk = Target::new(&phone(Service::Discord), Secret::new("not a url")).unwrap();
        assert_eq!(post(&request(&junk, "x", TELEGRAM_API)), Err(Failure::BadAddress));
    }
}
