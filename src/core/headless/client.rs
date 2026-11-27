//! One connection-only account: logs in, answers what the protocol requires and reports like a companion mod.

use std::collections::{HashSet, VecDeque};
use std::io::ErrorKind;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::commands::Tree;
use super::crowd::Crowd;
use super::maps::Maps;
use super::picture::{self, Request, View};
use super::render::hud::Hud;
use super::screen::Screen;
use super::signing::{Seen, Signature, Signer};
use super::status::Status;
use super::Spec;
use super::classify::classify;
use super::conn::{Reader, Writer};
use super::crypto;
use super::text::{self, Line};
use super::v777::{PROTOCOL, configuration as cfg, login, play};
use super::wire::{In, Out};
use super::world::World;
use crate::core::auth::{ChatCertificate, Session};
use crate::core::batch::Packs;
use crate::core::chat_pace::{Next, Queue};
use crate::core::chat_text::Run;
use crate::core::fleet::Resolved;
use crate::core::ipc::{ChatKind, ChatOutcome, ChatRefusal, DisconnectCause, Event, FromMod, Offer, StateKind, ToMod};
use crate::core::settings;
use crate::core::shots::{self, Moment};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const CLIENT_TICK: Duration = Duration::from_millis(50);
const BRAND: &str = "vanilla";
const INTENTION_LOGIN: i32 = 2;
const VIEW_DISTANCE: u8 = 2;
const PICTURE_RADIUS: i32 = VIEW_DISTANCE as i32 + 1;
const SEEN_RANGE: f64 = 48.0;
const MIN_PICTURE: u32 = 16;
const MAX_PICTURE: u32 = 1280;
const SHOT_WIDTH: u32 = 1280;
const TILE_WIDTH: u32 = 640;
const SHOT_RADIUS: i32 = 2;
const SHOT_WAIT: Duration = Duration::from_secs(10);
const QUEUED_MAX: usize = 4;
const CERTIFICATE_TIMEOUT: Duration = Duration::from_secs(5);
const RENEW_PAUSE: Duration = Duration::from_secs(300);

static PICTURES: AtomicU32 = AtomicU32::new(0);
static QUEUED: AtomicUsize = AtomicUsize::new(0);
const CHUNKS_PER_TICK: f32 = 10.0;
const ACK_AFTER: i32 = 64;
const MAX_CHAT: usize = 256;
const SESSION_JOIN: &str = "https://sessionserver.mojang.com/session/minecraft/join";
const OUTGOING: [&str; 2] = ["minecraft:msg_command_outgoing", "minecraft:team_msg_command_outgoing"];
/// The prefix of an offer id that stands for a server's Code of Conduct rather than a resource pack.
pub const CONDUCT: &str = "conduct-";

const PACK_LOADED: i32 = 0;
const PACK_DECLINED: i32 = 1;
const PACK_ACCEPTED: i32 = 3;
const PACK_DOWNLOADED: i32 = 4;
const PERFORM_RESPAWN: i32 = 0;
const MAIN_HAND: i32 = 0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Login,
    Configuration,
    Play,
}

enum Control {
    Quit,
    Reconnect,
    Leave(Line, DisconnectCause),
}

enum End {
    Quit,
    Reconnect,
    Dropped(Line, DisconnectCause),
}

enum Held {
    Pack { id: Uuid, required: bool, text: Option<String> },
    Conduct { hash: String, text: String },
}

impl Held {
    fn offer(&self) -> Offer {
        match self {
            Held::Pack { id, required, text } => Offer { id: id.to_string(), required: *required, text: text.clone() },
            Held::Conduct { hash, text } => Offer { id: format!("{CONDUCT}{hash}"), required: true, text: Some(text.clone()) },
        }
    }
}

type Shown = (StateKind, Option<String>, Option<String>, Option<DisconnectCause>, Option<Offer>);

struct Link {
    writer: Option<Writer>,
    phase: Phase,
    control: Option<Control>,
    packs: Packs,
    held: VecDeque<Held>,
    conduct: HashSet<String>,
    server: String,
    played: bool,
    dead: Option<String>,
    loaded: bool,
    position: [f64; 3],
    rotation: [f32; 2],
    chat_clock: i64,
    seen: Seen,
    certificate: Option<Arc<ChatCertificate>>,
    signer: Option<Signer>,
    commands: Option<Tree>,
    lines: Queue<(Option<u64>, String)>,
    closed: bool,
    shown: Option<Shown>,
    screen: Screen,
    maps: Maps,
    world: World,
    crowd: Crowd,
    status: Status,
    me: Uuid,
    dimension: String,
    due: Option<Instant>,
    vitals: Option<(u16, u8)>,
    nearby: Vec<String>,
    sequence: i32,
}

/// What the connection's thread and its handle share.
pub struct Shared {
    key: String,
    events: Sender<Event>,
    link: Mutex<Link>,
    certificate: Mutex<Option<Arc<ChatCertificate>>>,
    wake: Condvar,
    running: AtomicBool,
    drawing: Arc<AtomicBool>,
}

impl Shared {
    pub fn new(spec: &Spec, events: Sender<Event>) -> Self {
        let link = Link {
            writer: None,
            phase: Phase::Login,
            control: None,
            packs: spec.packs,
            held: VecDeque::new(),
            conduct: spec.conduct.iter().cloned().collect(),
            server: crate::core::fleet::server_name(&spec.address),
            played: false,
            dead: None,
            loaded: false,
            position: [0.0; 3],
            rotation: [0.0; 2],
            chat_clock: 0,
            seen: Seen::default(),
            certificate: None,
            signer: None,
            commands: None,
            lines: Queue::new(Instant::now()),
            closed: false,
            shown: None,
            screen: Screen::default(),
            maps: Maps::default(),
            world: World::default(),
            crowd: Crowd::default(),
            status: Status::new(spec.name.clone()),
            me: spec.uuid,
            dimension: String::new(),
            due: None,
            vitals: None,
            nearby: Vec::new(),
            sequence: 0,
        };
        Self { key: spec.key.clone(), events, link: Mutex::new(link), certificate: Mutex::new(None), wake: Condvar::new(), running: AtomicBool::new(true), drawing: Arc::default() }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn lock(&self) -> MutexGuard<'_, Link> {
        self.link.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, message: FromMod) {
        let _ = self.events.send(Event::Message { instance: self.key.clone(), message });
    }

    fn control(&self, link: &mut Link, control: Control) {
        if !matches!(link.control, Some(Control::Quit)) {
            link.control = Some(control);
        }
        if let Some(writer) = &link.writer {
            let _ = writer.output().shutdown(Shutdown::Both);
        }
        self.wake.notify_all();
    }

    fn outcome(&self, id: Option<u64>, outcome: ChatOutcome) {
        if let Some(id) = id {
            self.emit(FromMod::ChatOutcome { id, outcome });
        }
    }

    /// Takes a typed line: refused at once when it cannot go out, otherwise it waits for its turn under the server's
    /// spam limit.
    fn enqueue(&self, link: &mut Link, id: Option<u64>, text: &str) {
        let text = text.trim();
        if let Some(why) = refusal(link, text) {
            self.outcome(id, ChatOutcome::Refused(why));
            return;
        }
        link.lines.push((id, text.to_owned()));
        self.wake.notify_all();
    }

    /// Sends the lines whose turn has come; returns how long until the next one's.
    fn pump(&self, link: &mut Link) -> Option<Duration> {
        loop {
            match link.lines.next(Instant::now()) {
                Next::Ready((id, text)) => {
                    let outcome = match refusal(link, &text) {
                        Some(why) => ChatOutcome::Refused(why),
                        None => match chat(link, &text) {
                            Ok(()) => ChatOutcome::Sent,
                            Err(_) => ChatOutcome::Refused(ChatRefusal::Failed),
                        },
                    };
                    self.outcome(id, outcome);
                }
                Next::Wait(wait) => return Some(wait),
                Next::Empty => return None,
            }
        }
    }

    /// Keeps the chat key of a signed-in account current for as long as the connection lasts.
    fn renew(&self, spec: &Spec) {
        let Some(account) = &spec.account else { return };
        self.renew_with(|| {
            let session = crate::core::auth::session_for(account)?;
            crate::core::auth::chat_certificate(&crate::core::net::agent_within(CERTIFICATE_TIMEOUT), &session.access_token)
        });
    }

    /// Fetches a new key with `fetch` once the one in use is due for replacement, keeps it for reconnects and, while
    /// signing, announces it as a new chat session as the game does. A failure keeps the old key and tries again after
    /// [`RENEW_PAUSE`], which also bounds how often a key that is due at once is asked for.
    fn renew_with(&self, mut fetch: impl FnMut() -> Result<ChatCertificate>) {
        let mut not_before: Option<Instant> = None;
        let mut link = self.lock();
        while !link.closed {
            let due = match (&link.signer, &link.certificate) {
                (Some(_), Some(certificate)) => Some(certificate.renew_in(now_millis())),
                _ => None,
            };
            let Some(mut wait) = due else {
                link = self.wake.wait(link).unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            if let Some(at) = not_before {
                wait = wait.max(at.saturating_duration_since(Instant::now()));
            }
            if !wait.is_zero() {
                link = self.wake.wait_timeout(link, wait).unwrap_or_else(PoisonError::into_inner).0;
                continue;
            }
            drop(link);
            let fetched = fetch();
            link = self.lock();
            not_before = Some(Instant::now() + RENEW_PAUSE);
            match fetched {
                Ok(certificate) => {
                    let certificate = Arc::new(certificate);
                    *self.certificate.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&certificate));
                    if let Err(e) = renew_chat_session(&mut link, certificate) {
                        eprintln!("{}: chat key not announced: {e:#}", self.key);
                    }
                }
                Err(e) => eprintln!("{}: chat key not renewed, the old one stays: {e:#}", self.key),
            }
        }
    }

    /// Reports the lines that never got their turn; they are not sent after a reconnect.
    fn drop_lines(&self, link: &mut Link) {
        for (id, _) in link.lines.drain() {
            self.outcome(id, ChatOutcome::Refused(ChatRefusal::Dropped));
        }
    }

    fn pace(&self) {
        let mut link = self.lock();
        while !link.closed {
            link = match self.pump(&mut link) {
                Some(wait) => self.wake.wait_timeout(link, wait).unwrap_or_else(PoisonError::into_inner).0,
                None => self.wake.wait(link).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    /// Acts on `message` from the manager.
    pub fn send(&self, message: &ToMod) -> Result<()> {
        let mut link = self.lock();
        match message {
            ToMod::Quit => self.control(&mut link, Control::Quit),
            ToMod::Reconnect => self.control(&mut link, Control::Reconnect),
            ToMod::Respawn => {
                if link.phase == Phase::Play && link.dead.is_some() {
                    write(&mut link, play::serverbound::CLIENT_COMMAND, Out::default().varint(PERFORM_RESPAWN))?;
                }
            }
            ToMod::ChatSend { text } => self.enqueue(&mut link, None, text),
            ToMod::ChatLine { id, text } => self.enqueue(&mut link, Some(*id), text),
            ToMod::Packs { policy } => link.packs = *policy,
            ToMod::Answer { id, accept } => self.answer(&mut link, id, *accept)?,
            ToMod::Click { menu, slot, click } => {
                if link.phase == Phase::Play
                    && let Some(mut out) = link.screen.click(*menu, *slot, *click)
                {
                    write(&mut link, play::serverbound::CONTAINER_CLICK, &mut out)?;
                }
            }
            ToMod::CloseMenu { menu } => {
                if let Some(mut out) = link.screen.close(*menu) {
                    self.show_screen(&mut link);
                    if link.phase == Phase::Play {
                        write(&mut link, play::serverbound::CONTAINER_CLOSE, &mut out)?;
                    }
                }
            }
            ToMod::Hotbar { slot } => {
                if link.phase == Phase::Play
                    && link.dead.is_none()
                    && link.screen.select(usize::from(*slot))
                {
                    write(&mut link, play::serverbound::SET_CARRIED_ITEM, Out::default().i16(i16::from(*slot)))?;
                }
            }
            ToMod::UseItem => {
                if link.phase == Phase::Play && link.dead.is_none() {
                    link.sequence = link.sequence.wrapping_add(1);
                    let [yaw, pitch] = link.rotation;
                    let sequence = link.sequence;
                    write(&mut link, play::serverbound::USE_ITEM, Out::default().varint(MAIN_HAND).varint(sequence).f32(yaw).f32(pitch))?;
                }
            }
            ToMod::Capture { width } => {
                self.picture(&link, *width, false);
            }
            _ => {}
        }
        Ok(())
    }

    /// Draws what the account sees, from its eyes and from above, on the drawing thread, and reports both as a screenshot
    /// held in memory; the view from its eyes also goes to its tile. Whether it started: while one is being drawn, more
    /// asks are ignored, and one the account takes on its own waits while `QUEUED_MAX` pictures of all accounts are drawn.
    fn picture(&self, link: &Link, width: u32, own: bool) -> bool {
        if link.phase != Phase::Play || (own && QUEUED.load(Ordering::Acquire) >= QUEUED_MAX) || self.drawing.swap(true, Ordering::AcqRel) {
            return false;
        }
        let drawing = Drawing::taken(Arc::clone(&self.drawing));
        let width = width.clamp(MIN_PICTURE, MAX_PICTURE);
        let height = (width * 9 / 16).max(1);
        let [x, _, z] = link.position;
        let eye = [link.position[0], link.position[1] + picture::EYE_HEIGHT, link.position[2]];
        let seen = link.crowd.seen(eye, SEEN_RANGE, link.me, link.screen.held(), link.status.left_handed(), &link.maps, |member, name, hideable| link.status.tag(member, name, hideable));
        let hud = Hud { parts: settings::active().headless_hud, ..link.status.hud(&link.screen) };
        let request = Request { world: link.world.snapshot(x, z, PICTURE_RADIUS), seen, hud, position: link.position, rotation: link.rotation, width, height };
        let taken = SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_secs()).ok()).unwrap_or(0);
        let (key, events) = (self.key.clone(), self.events.clone());
        picture::queue(request, vec![View::Eyes, View::Above], move |mut pictures| {
            let above = pictures.pop().unwrap_or_default();
            let eyes = pictures.pop().unwrap_or_default();
            let moment = shots::encode(&eyes, width, height).and_then(|e| Ok(Moment { taken, eyes: e.into(), above: shots::encode(&above, width, height)?.into() }));
            drop(drawing);
            let moment = match moment {
                Ok(moment) => moment,
                Err(e) => {
                    eprintln!("picture not kept: {e:#}");
                    return;
                }
            };
            let (width, height, rgba) = for_tile(eyes, width, height);
            let seq = PICTURES.fetch_add(2, Ordering::Relaxed).wrapping_add(2);
            let _ = events.send(Event::Picture { instance: key, seq, width, height, rgba, moment });
        });
        true
    }

    /// Takes the screenshot a change of world asked for once the account stands in the new world and the chunks around it
    /// arrived, or once it waited `SHOT_WAIT` for them.
    fn world_shot(&self, link: &mut Link) {
        let Some(since) = link.due else { return };
        let [x, _, z] = link.position;
        if link.loaded && (link.world.around(x, z, SHOT_RADIUS) || since.elapsed() >= SHOT_WAIT) && self.picture(link, SHOT_WIDTH, true) {
            link.due = None;
        }
    }

    /// Reports health and food when the values the tile shows changed.
    fn show_vitals(&self, link: &mut Link, health: f32, food: i32) {
        let shown = (health.max(0.0).ceil().min(f32::from(u16::MAX)) as u16, food.clamp(0, i32::from(u8::MAX)) as u8);
        if link.vitals != Some(shown) {
            link.vitals = Some(shown);
            self.emit(FromMod::Vitals { health: shown.0, food: shown.1 });
        }
    }

    /// Reports the players near the account when the set changed.
    fn show_nearby(&self, link: &mut Link) {
        let players = link.crowd.players_near(link.me);
        if players != link.nearby {
            link.nearby.clone_from(&players);
            self.emit(FromMod::Nearby { players });
        }
    }

    fn show_screen(&self, link: &mut Link) {
        for message in link.screen.changes(&link.maps) {
            self.emit(message);
        }
    }

    fn answer(&self, link: &mut Link, id: &str, accept: bool) -> Result<()> {
        let Some(at) = link.held.iter().position(|h| h.offer().id == id) else { return Ok(()) };
        let Some(held) = link.held.remove(at) else { return Ok(()) };
        match held {
            Held::Pack { id, .. } => answer_pack(link, id, accept)?,
            Held::Conduct { hash, .. } if accept => {
                link.conduct.insert(hash);
                if link.phase == Phase::Configuration {
                    write(link, cfg::serverbound::ACCEPT_CODE_OF_CONDUCT, &mut Out::default())?;
                }
            }
            Held::Conduct { .. } => {
                let reason = Line { text: "Code of Conduct declined".into(), ..Line::default() };
                self.control(link, Control::Leave(reason, DisconnectCause::PackDeclined));
                return Ok(());
            }
        }
        self.settle(link);
        Ok(())
    }

    fn report(&self, link: &mut Link, kind: StateKind, reason: Option<String>, cause: Option<DisconnectCause>, offer: Option<Offer>) {
        let server = Some(link.server.clone());
        let shown = (kind, server.clone(), reason.clone(), cause, offer.clone());
        if link.shown.as_ref() == Some(&shown) {
            return;
        }
        link.shown = Some(shown);
        self.emit(FromMod::State { kind, server, reason, cause, offer });
    }

    /// Reports where the connection stands: a held offer first, then death, then in the game or still joining.
    fn settle(&self, link: &mut Link) {
        if let Some(offer) = link.held.front().map(Held::offer) {
            self.report(link, StateKind::Offer, None, None, Some(offer));
        } else if let Some(reason) = link.dead.clone() {
            self.report(link, StateKind::Dead, Some(reason), None, None);
        } else if link.played {
            self.report(link, StateKind::Playing, None, None, None);
        } else {
            self.report(link, StateKind::Connecting, None, None, None);
        }
    }
}

fn write(link: &mut Link, id: i32, body: &mut Out) -> Result<()> {
    let writer = link.writer.as_mut().context("not connected")?;
    let sent = writer.send(id, &body.0);
    if sent.is_err() {
        let _ = writer.output().shutdown(Shutdown::Both);
    }
    sent
}

/// The answers to a pushed pack in the order the game gives them: accepted, downloaded and loaded without fetching it, or declined.
fn pack_answers(phase: Phase, id: Uuid, accept: bool) -> Vec<(i32, Vec<u8>)> {
    let packet = if phase == Phase::Play { play::serverbound::RESOURCE_PACK } else { cfg::serverbound::RESOURCE_PACK };
    let actions: &[i32] = if accept { &[PACK_ACCEPTED, PACK_DOWNLOADED, PACK_LOADED] } else { &[PACK_DECLINED] };
    actions.iter().map(|action| (packet, Out::default().uuid(id).varint(*action).0.clone())).collect()
}

fn answer_pack(link: &mut Link, id: Uuid, accept: bool) -> Result<()> {
    for (packet, mut body) in pack_answers(link.phase, id, accept).into_iter().map(|(p, b)| (p, Out(b))) {
        write(link, packet, &mut body)?;
    }
    Ok(())
}

/// Why `text` cannot go out as a chat line right now, if it cannot.
fn refusal(link: &Link, text: &str) -> Option<ChatRefusal> {
    if link.phase != Phase::Play {
        return Some(ChatRefusal::NotInGame);
    }
    if text.encode_utf16().count() > MAX_CHAT {
        return Some(ChatRefusal::TooLong);
    }
    if text.is_empty() || text.chars().any(|c| c == '§' || c < ' ' || c == '\u{7f}') {
        return Some(ChatRefusal::BadText);
    }
    None
}

/// Tells the server which key this connection signs its chat with, if it has one.
fn start_chat_session(shared: &Shared, link: &mut Link) -> Result<()> {
    link.seen = Seen::default();
    link.signer = None;
    link.commands = None;
    let Some(certificate) = link.certificate.clone() else { return Ok(()) };
    match Signer::new(&certificate, link.me) {
        Ok(signer) => {
            write(link, play::serverbound::CHAT_SESSION_UPDATE, &mut signer.announcement())?;
            link.signer = Some(signer);
            shared.wake.notify_all();
        }
        Err(e) => eprintln!("{}: chat goes out unsigned, its key is unusable: {e:#}", shared.key),
    }
    Ok(())
}

/// Announces `certificate` as a new chat session of a connection that signs, which keeps what it has seen.
fn renew_chat_session(link: &mut Link, certificate: Arc<ChatCertificate>) -> Result<()> {
    if link.phase != Phase::Play || link.signer.is_none() {
        return Ok(());
    }
    let signer = Signer::new(&certificate, link.me)?;
    write(link, play::serverbound::CHAT_SESSION_UPDATE, &mut signer.announcement())?;
    link.signer = Some(signer);
    link.certificate = Some(certificate);
    Ok(())
}

fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Whether the server enforces secure chat, the last field of the play Login packet. Only then does the connection open a
/// chat session; a server that does not wants no signature, so it gets none.
fn enforces_secure_chat(login: &In) -> bool {
    let mut tail = login.clone();
    let before_flag = tail.left().saturating_sub(1);
    tail.take(before_flag).is_ok() && tail.bool().unwrap_or(false)
}

/// The key to sign with on this login: none unless the server enforces secure chat and the account has a session.
fn login_certificate(shared: &Shared, spec: &Spec, login: &In) -> Option<Arc<ChatCertificate>> {
    if !enforces_secure_chat(login) {
        return None;
    }
    chat_certificate(shared, spec, spec.session.as_ref()?)
}

/// The account's chat key: the one kept from an earlier connection while it is current, otherwise a fresh one from Mojang.
/// `None` when it cannot be had, and chat then goes out unsigned.
fn chat_certificate(shared: &Shared, spec: &Spec, session: &Session) -> Option<Arc<ChatCertificate>> {
    let now = now_millis();
    let mut kept = shared.certificate.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(certificate) = kept.as_ref().filter(|c| c.is_current(now)) {
        return Some(Arc::clone(certificate));
    }
    let fetched = crate::core::auth::chat_certificate(&crate::core::net::agent_within(CERTIFICATE_TIMEOUT), &session.access_token);
    match fetched {
        Ok(certificate) => Some(Arc::clone(kept.insert(Arc::new(certificate)))),
        Err(e) => {
            eprintln!("{}: chat goes out unsigned, its key was not fetched: {e:#}", spec.name);
            *kept = None;
            None
        }
    }
}

/// Sends what the user typed: a command when it starts with `/`, otherwise a chat message, signed when the connection has a key.
fn chat(link: &mut Link, text: &str) -> Result<()> {
    let text = text.trim();
    link.chat_clock = now_millis().max(link.chat_clock + 1);
    let mut salt = [0u8; 8];
    crypto::random(&mut salt)?;
    let mut out = Out::default();
    if let Some(command) = text.strip_prefix('/') {
        let arguments = match (&link.signer, &link.commands) {
            (Some(_), Some(tree)) => tree.signable(command),
            _ => Vec::new(),
        };
        if arguments.is_empty() {
            return write(link, play::serverbound::CHAT_COMMAND, out.string(command));
        }
        let update = link.seen.update();
        out.string(command).i64(link.chat_clock).raw(&salt).varint(i32::try_from(arguments.len())?);
        let signer = link.signer.as_mut().context("no chat key")?;
        for argument in &arguments {
            out.string(&argument.name).raw(&signer.sign(&argument.value, salt, link.chat_clock, &update.signatures)?);
        }
        out.varint(update.offset).raw(&update.acknowledged).u8(update.checksum);
        return write(link, play::serverbound::CHAT_COMMAND_SIGNED, &mut out);
    }
    let update = link.seen.update();
    out.string(text).i64(link.chat_clock).raw(&salt);
    match &mut link.signer {
        Some(signer) => out.bool(true).raw(&signer.sign(text, salt, link.chat_clock, &update.signatures)?),
        None => out.bool(false),
    };
    out.varint(update.offset).raw(&update.acknowledged).u8(update.checksum);
    write(link, play::serverbound::CHAT, &mut out)
}

/// Runs the connection until it is told to quit: joins, and after a drop waits to be told to reconnect.
pub fn run(shared: &Shared, spec: &Spec) {
    thread::scope(|scope| {
        scope.spawn(|| shared.pace());
        scope.spawn(|| shared.renew(spec));
        let _closing = Closing(shared);
        keep_connected(shared, spec);
    });
}

/// Ends the pacer and reports the lines that never got their turn, also when the connection's thread panics.
struct Closing<'a>(&'a Shared);

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        let mut link = self.0.lock();
        link.closed = true;
        self.0.drop_lines(&mut link);
        self.0.wake.notify_all();
    }
}

fn keep_connected(shared: &Shared, spec: &Spec) {
    let _ended = Ended(shared);
    let _ = shared.events.send(Event::Connected { instance: shared.key.clone(), pid: 0, mod_version: format!("connection-only protocol {PROTOCOL}") });
    loop {
        match session(shared, spec) {
            End::Quit => break,
            End::Reconnect => {
                let mut link = shared.lock();
                shared.drop_lines(&mut link);
            }
            End::Dropped(reason, cause) => {
                {
                    let mut link = shared.lock();
                    shared.drop_lines(&mut link);
                    link.held.clear();
                    link.dead = None;
                    link.world.clear();
                    link.crowd = Crowd::default();
                    shared.show_nearby(&mut link);
                    shared.report(&mut link, StateKind::Disconnected, Some(reason.text), Some(cause), None);
                }
                let mut link = shared.lock();
                let control = loop {
                    match link.control.take() {
                        Some(Control::Leave(..)) | None => link = shared.wake.wait(link).unwrap_or_else(PoisonError::into_inner),
                        Some(control) => break control,
                    }
                };
                if matches!(control, Control::Quit) {
                    break;
                }
            }
        }
    }
}

struct Ended<'a>(&'a Shared);

impl Drop for Ended<'_> {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
        let _ = self.0.events.send(Event::Disconnected { instance: self.0.key.clone() });
    }
}

fn session(shared: &Shared, spec: &Spec) -> End {
    {
        let mut link = shared.lock();
        link.phase = Phase::Login;
        link.played = false;
        link.dead = None;
        link.loaded = false;
        link.held.clear();
        link.seen = Seen::default();
        link.certificate = None;
        link.signer = None;
        link.commands = None;
        link.crowd = Crowd::default();
        link.vitals = None;
        link.sequence = 0;
        shared.show_nearby(&mut link);
        link.status.reset();
        shared.report(&mut link, StateKind::Connecting, None, None, None);
    }
    let outcome = join(shared, spec);
    let mut link = shared.lock();
    link.writer = None;
    link.phase = Phase::Login;
    link.held.clear();
    link.screen.reset();
    link.maps.reset();
    shared.show_screen(&mut link);
    match link.control.take() {
        Some(Control::Quit) => End::Quit,
        Some(Control::Reconnect) => End::Reconnect,
        Some(Control::Leave(reason, cause)) => End::Dropped(reason, cause),
        None => match outcome {
            Ok((reason, cause)) => End::Dropped(reason, cause),
            Err(e) => {
                let reason = failure(&e);
                let cause = classify(&reason.keys, &reason.text);
                End::Dropped(reason, cause)
            }
        },
    }
}

fn failure(e: &anyhow::Error) -> Line {
    let key = e.chain().find_map(|c| c.downcast_ref::<std::io::Error>()).map(|io| match io.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => "disconnect.timeout",
        ErrorKind::UnexpectedEof => "disconnect.endOfStream",
        ErrorKind::ConnectionRefused => "connect.failed",
        _ => "disconnect.lost",
    });
    match key {
        Some("connect.failed") => Line { text: format!("Connection refused: {e}"), keys: vec![], runs: vec![] },
        Some(key) => text::translate(key, &[]),
        None => Line { text: format!("{e:#}"), ..Line::default() },
    }
}

fn connect(shared: &Shared, address: &str) -> Result<(TcpStream, String, u16)> {
    let Resolved { host, port, addrs } = crate::core::fleet::resolve_server(address).map_err(|e| anyhow::anyhow!("Unknown host: {e}"))?;
    let mut last = None;
    for addr in addrs {
        if shared.lock().control.is_some() {
            bail!("stopped");
        }
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok((stream, host, port)),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| anyhow::anyhow!("Unknown host"), anyhow::Error::from))
}

/// The handshake that starts a login, as the game sends it: the protocol, the host and port it connected to, the login intention.
fn handshake(host: &str, port: u16) -> Out {
    let mut out = Out::default();
    out.varint(PROTOCOL).string(host).u16(port).varint(INTENTION_LOGIN);
    out
}

/// One TCP connection from handshake to its end. Returns the server's reason when it ends the connection.
fn join(shared: &Shared, spec: &Spec) -> Result<(Line, DisconnectCause)> {
    let (stream, host, port) = connect(shared, &spec.address)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut reader = Reader::new(stream.try_clone()?);
    {
        let mut link = shared.lock();
        if link.control.is_some() {
            bail!("stopped");
        }
        link.writer = Some(Writer::new(stream));
        write(&mut link, 0, &mut handshake(&host, port))?;
        write(&mut link, login::serverbound::HELLO, Out::default().string(&spec.name).uuid(spec.uuid))?;
    }
    if let Some(end) = log_in(shared, spec, &mut reader)? {
        return Ok(end);
    }
    let mut chat_types = Vec::new();
    let mut skipped: HashSet<(bool, i32)> = HashSet::new();
    loop {
        let phase = shared.lock().phase;
        let packet = reader.next(|id| wanted(phase, id))?;
        if phase == Phase::Play && packet.id == play::clientbound::PLAYER_POSITION {
            thread::sleep(CLIENT_TICK);
        }
        let id = packet.id;
        let mut r = In::new(packet.body);
        let certificate = (phase == Phase::Play && id == play::clientbound::LOGIN).then(|| login_certificate(shared, spec, &r));
        let mut link = shared.lock();
        if let Some(certificate) = certificate {
            link.certificate = certificate;
        }
        let handled = match phase {
            Phase::Configuration => configuration(shared, &mut link, id, &mut r, &mut chat_types),
            _ => playing(shared, &mut link, id, &mut r, &chat_types),
        };
        let end = match handled {
            Ok(end) => end,
            Err(e) if !essential(phase, id) => {
                if skipped.insert((phase == Phase::Configuration, id)) {
                    eprintln!("headless {}: skipped packet {id} it could not read: {e:#}", spec.name);
                }
                None
            }
            Err(e) => return Err(e),
        };
        if let Some(end) = end {
            return Ok(end);
        }
        shared.world_shot(&mut link);
    }
}

/// The packets whose loss the game itself does not survive: answers the server waits for, and its own goodbye. Anything
/// else the client reads for the manager's sake, so one it cannot read is skipped and the connection stays.
fn essential(phase: Phase, id: i32) -> bool {
    use cfg::clientbound as c;
    use play::clientbound as p;
    match phase {
        Phase::Configuration => [c::KEEP_ALIVE, c::PING, c::DISCONNECT, c::FINISH_CONFIGURATION, c::TRANSFER, c::COOKIE_REQUEST, c::CODE_OF_CONDUCT, c::RESOURCE_PACK_PUSH, c::SELECT_KNOWN_PACKS].contains(&id),
        _ => [p::KEEP_ALIVE, p::PING, p::DISCONNECT, p::PLAYER_POSITION, p::START_CONFIGURATION, p::TRANSFER, p::COOKIE_REQUEST].contains(&id),
    }
}

fn wanted(phase: Phase, id: i32) -> bool {
    use cfg::clientbound as c;
    use play::clientbound as p;
    match phase {
        Phase::Configuration => [c::COOKIE_REQUEST, c::DISCONNECT, c::FINISH_CONFIGURATION, c::KEEP_ALIVE, c::PING, c::REGISTRY_DATA, c::RESOURCE_PACK_POP, c::RESOURCE_PACK_PUSH, c::TRANSFER, c::SELECT_KNOWN_PACKS, c::CODE_OF_CONDUCT].contains(&id),
        _ => [p::BLOCK_UPDATE, p::CHUNK_BATCH_FINISHED, p::COMMANDS, p::FORGET_LEVEL_CHUNK, p::LEVEL_CHUNK_WITH_LIGHT, p::LIGHT_UPDATE, p::LOGIN, p::SECTION_BLOCKS_UPDATE, p::COOKIE_REQUEST, p::DISCONNECT, p::DISGUISED_CHAT, p::KEEP_ALIVE, p::PING, p::PLAYER_CHAT, p::PLAYER_COMBAT_KILL, p::PLAYER_POSITION, p::RESOURCE_PACK_POP, p::RESOURCE_PACK_PUSH, p::RESPAWN, p::SET_HEALTH, p::START_CONFIGURATION, p::SYSTEM_CHAT, p::TRANSFER].contains(&id) || Screen::reads(id) || Maps::reads(id) || Crowd::reads(id) || Status::reads(id) || [p::SET_TIME, p::GAME_EVENT].contains(&id),
    }
}

fn log_in(shared: &Shared, spec: &Spec, reader: &mut Reader) -> Result<Option<(Line, DisconnectCause)>> {
    use login::clientbound as c;
    use login::serverbound as s;
    let mut verified = false;
    loop {
        let packet = reader.next(|_| true)?;
        let mut r = In::new(packet.body);
        match packet.id {
            c::LOGIN_DISCONNECT => {
                let json = r.string()?;
                let value = serde_json::from_str(&json).unwrap_or(Value::String(json));
                let (reason, cause) = dropped(text::line(&value));
                let cause = if verified && cause == DisconnectCause::Auth { DisconnectCause::ServerDown } else { cause };
                return Ok(Some((reason, cause)));
            }
            c::HELLO => {
                let server_id = r.string()?;
                let key = r.bytes()?.to_vec();
                let token = r.bytes()?.to_vec();
                let authenticate = r.bool()?;
                let mut secret = [0u8; 16];
                crypto::random(&mut secret)?;
                if authenticate {
                    let Some(session) = &spec.session else {
                        let reason = Line { text: "Failed to log in: this server needs a Microsoft account".into(), keys: vec!["disconnect.loginFailed".into()], runs: vec![] };
                        return Ok(Some((reason, DisconnectCause::Auth)));
                    };
                    if let Err(reason) = join_session(session, &crypto::server_hash(&server_id, &secret, &key)) {
                        let cause = classify(&reason.keys, &reason.text);
                        return Ok(Some((reason, cause)));
                    }
                    verified = true;
                }
                let sealed_secret = crypto::rsa_encrypt(&key, &secret)?;
                let sealed_token = crypto::rsa_encrypt(&key, &token)?;
                {
                    let mut link = shared.lock();
                    write(&mut link, s::KEY, Out::default().bytes(&sealed_secret).bytes(&sealed_token))?;
                    link.writer.as_mut().context("not connected")?.encrypt(&secret);
                }
                reader.encrypt(&secret);
            }
            c::LOGIN_COMPRESSION => {
                let threshold = usize::try_from(r.varint()?).ok();
                shared.lock().writer.as_mut().context("not connected")?.compress(threshold);
                reader.compress(threshold);
            }
            c::CUSTOM_QUERY => {
                let transaction = r.varint()?;
                write(&mut shared.lock(), s::CUSTOM_QUERY_ANSWER, Out::default().varint(transaction).bool(false))?;
            }
            c::COOKIE_REQUEST => {
                let cookie = r.string()?;
                write(&mut shared.lock(), s::COOKIE_RESPONSE, Out::default().string(&cookie).bool(false))?;
            }
            c::LOGIN_FINISHED => {
                let uuid = r.uuid()?;
                let name = r.string()?;
                let mut link = shared.lock();
                link.me = uuid;
                link.status.profile(name);
                write(&mut link, s::LOGIN_ACKNOWLEDGED, &mut Out::default())?;
                link.phase = Phase::Configuration;
                write(&mut link, cfg::serverbound::CUSTOM_PAYLOAD, Out::default().string("minecraft:brand").string(BRAND))?;
                write(&mut link, cfg::serverbound::CLIENT_INFORMATION, &mut client_information())?;
                return Ok(None);
            }
            other => bail!("unexpected login packet {other}"),
        }
    }
}

fn client_information() -> Out {
    let mut out = Out::default();
    out.string("en_us").u8(VIEW_DISTANCE).varint(0).bool(true).u8(0x7f).varint(1).bool(false).bool(true).varint(2);
    out
}

/// Tells the session server this account joins the server with `hash`; online-mode servers check it.
fn join_session(session: &Session, hash: &str) -> std::result::Result<(), Line> {
    let body = serde_json::json!({ "accessToken": session.access_token, "selectedProfile": session.uuid.simple().to_string(), "serverId": hash });
    let failed = |key: &str| text::translate("disconnect.loginFailedInfo", &[serde_json::json!({ "translate": key })]);
    let agent = crate::core::net::agent(None).map_err(|_| failed("disconnect.loginFailedInfo.serversUnavailable"))?;
    match agent.post(SESSION_JOIN).send_json(&body) {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(401 | 403)) => Err(failed("disconnect.loginFailedInfo.invalidSession")),
        Err(_) => Err(failed("disconnect.loginFailedInfo.serversUnavailable")),
    }
}

fn died() -> String {
    text::english("deathScreen.title").unwrap_or("You Died!").to_owned()
}

fn dropped(reason: Line) -> (Line, DisconnectCause) {
    let cause = classify(&reason.keys, &reason.text);
    (reason, cause)
}

fn unsupported_transfer() -> (Line, DisconnectCause) {
    (Line { text: "The server moved this account to another server; connection-only accounts cannot follow yet".into(), ..Line::default() }, DisconnectCause::Kicked)
}

/// A chat type as the server's registry defines it.
struct ChatType {
    name: String,
    key: String,
    parameters: Vec<String>,
}

fn chat_types(r: &mut In) -> Result<Vec<ChatType>> {
    let count = r.varint()?;
    let mut types = Vec::new();
    for _ in 0..count {
        let name = r.string()?;
        let data = r.optional_nbt()?;
        let chat = data.as_ref().and_then(|d| d.get("chat"));
        let key = chat.and_then(|c| c.get("translation_key")).and_then(Value::as_str).unwrap_or("chat.type.text").to_owned();
        let parameters = chat.and_then(|c| c.get("parameters")).and_then(Value::as_array).map(|p| p.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_else(|| vec!["sender".into(), "content".into()]);
        types.push(ChatType { name, key, parameters });
    }
    Ok(types)
}

fn configuration(shared: &Shared, link: &mut Link, id: i32, r: &mut In, types: &mut Vec<ChatType>) -> Result<Option<(Line, DisconnectCause)>> {
    use cfg::clientbound as c;
    use cfg::serverbound as s;
    match id {
        c::KEEP_ALIVE => {
            let token = r.i64()?;
            write(link, s::KEEP_ALIVE, Out::default().i64(token))?;
        }
        c::PING => {
            let token = r.i32()?;
            write(link, s::PONG, Out::default().i32(token))?;
        }
        c::DISCONNECT => return Ok(Some(dropped(text::line(&r.nbt()?)))),
        c::FINISH_CONFIGURATION => {
            write(link, s::FINISH_CONFIGURATION, &mut Out::default())?;
            link.phase = Phase::Play;
            link.loaded = false;
        }
        c::SELECT_KNOWN_PACKS => write(link, s::SELECT_KNOWN_PACKS, Out::default().varint(0))?,
        c::REGISTRY_DATA => match r.string()?.as_str() {
            "minecraft:chat_type" => {
                if let Ok(found) = chat_types(r) {
                    *types = found;
                }
            }
            "minecraft:worldgen/biome" => {
                if let Err(e) = link.world.biomes(r) {
                    eprintln!("{}: biomes not read: {e:#}", shared.key);
                }
            }
            "minecraft:world_clock" => {
                if let Err(e) = link.world.clocks(r) {
                    eprintln!("{}: world clocks not read: {e:#}", shared.key);
                }
            }
            "minecraft:dimension_type" => {
                if let Err(e) = link.world.dimension_types(r) {
                    eprintln!("{}: dimension types not read: {e:#}", shared.key);
                }
            }
            other => {
                if let Err(e) = link.crowd.registry(other, r) {
                    eprintln!("{}: registry {other} not read: {e:#}", shared.key);
                }
            }
        },
        c::RESOURCE_PACK_PUSH => pushed(shared, link, r)?,
        c::RESOURCE_PACK_POP => popped(shared, link, r)?,
        c::CODE_OF_CONDUCT => {
            let text = r.string()?;
            let hash: String = Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
            if link.conduct.contains(&hash) {
                write(link, s::ACCEPT_CODE_OF_CONDUCT, &mut Out::default())?;
            } else {
                link.held.push_back(Held::Conduct { hash, text });
                shared.settle(link);
            }
        }
        c::COOKIE_REQUEST => {
            let cookie = r.string()?;
            write(link, s::COOKIE_RESPONSE, Out::default().string(&cookie).bool(false))?;
        }
        c::TRANSFER => return Ok(Some(unsupported_transfer())),
        _ => {}
    }
    Ok(None)
}

fn pushed(shared: &Shared, link: &mut Link, r: &mut In) -> Result<()> {
    let id = r.uuid()?;
    let _url = r.string().unwrap_or_default();
    let _hash = r.string().unwrap_or_default();
    let required = r.bool().unwrap_or(false);
    let text = r.optional_nbt().ok().flatten().map(|p| text::line(&p).text);
    match link.packs {
        Packs::Accept => answer_pack(link, id, true),
        Packs::Decline => answer_pack(link, id, false),
        Packs::Ask => {
            link.held.retain(|h| !matches!(h, Held::Pack { id: held, .. } if *held == id));
            link.held.push_back(Held::Pack { id, required, text });
            shared.settle(link);
            Ok(())
        }
    }
}

fn popped(shared: &Shared, link: &mut Link, r: &mut In) -> Result<()> {
    let id = if r.bool()? { Some(r.uuid()?) } else { None };
    link.held.retain(|h| !matches!(h, Held::Pack { id: held, .. } if id.is_none_or(|id| id == *held)));
    shared.settle(link);
    Ok(())
}

fn playing(shared: &Shared, link: &mut Link, id: i32, r: &mut In, types: &[ChatType]) -> Result<Option<(Line, DisconnectCause)>> {
    use play::clientbound as c;
    use play::serverbound as s;
    if id == c::SET_ENTITY_DATA
        && let Err(e) = link.status.data(&mut r.clone())
    {
        eprintln!("{}: own entity data not read: {e:#}", shared.key);
    }
    if Status::reads(id) {
        if let Err(e) = link.status.read(id, r) {
            eprintln!("{}: status packet {id} not read: {e:#}", shared.key);
        }
        return Ok(None);
    }
    if Crowd::reads(id) {
        let me = link.position;
        let read = link.crowd.read(id, r, me);
        if [c::ADD_ENTITY, c::REMOVE_ENTITIES, c::PLAYER_INFO_UPDATE, c::PLAYER_INFO_REMOVE].contains(&id) {
            shared.show_nearby(link);
        }
        read?;
        return Ok(None);
    }
    if Maps::reads(id) {
        let kept: std::collections::HashSet<i32> = link.screen.carried().chain(link.crowd.framed()).collect();
        match link.maps.read(r, |id| kept.contains(&id)) {
            Ok(Some(map)) => link.screen.map_changed(map),
            Ok(None) => {}
            Err(e) => eprintln!("{}: map packet not read: {e:#}", shared.key),
        }
        shared.show_screen(link);
        return Ok(None);
    }
    if Screen::reads(id) {
        if let Err(e) = link.screen.read(id, r) {
            eprintln!("{}: screen packet {id} not read: {e:#}", shared.key);
        }
        shared.show_screen(link);
        return Ok(None);
    }
    match id {
        c::KEEP_ALIVE => {
            let token = r.i64()?;
            write(link, s::KEEP_ALIVE, Out::default().i64(token))?;
        }
        c::PING => {
            let token = r.i32()?;
            write(link, s::PONG, Out::default().i32(token))?;
        }
        c::DISCONNECT => return Ok(Some(dropped(text::line(&r.nbt()?)))),
        c::PLAYER_POSITION => teleported(shared, link, r)?,
        c::LOGIN | c::LEVEL_CHUNK_WITH_LIGHT | c::LIGHT_UPDATE | c::FORGET_LEVEL_CHUNK | c::BLOCK_UPDATE | c::SECTION_BLOCKS_UPDATE => {
            if id == c::LOGIN {
                start_chat_session(shared, link)?;
            }
            if let Err(e) = seen(link, id, r) {
                eprintln!("{}: world packet {id} not read: {e:#}", shared.key);
            }
            if id == c::LOGIN {
                shared.show_nearby(link);
            }
        }
        c::RESPAWN => {
            link.world.clear();
            link.crowd.clear();
            shared.show_nearby(link);
            let mut keep = 0;
            if let Ok(kind) = r.varint() {
                link.world.enter(kind);
                if let Ok((dimension, mode)) = spawn_info(r) {
                    link.status.game_mode(mode);
                    keep = respawn_keep(r).unwrap_or(0);
                    if dimension != link.dimension {
                        link.dimension = dimension;
                        link.due = Some(Instant::now());
                    }
                }
            }
            link.status.respawned(keep);
            link.loaded = false;
            if link.dead.take().is_some() {
                shared.settle(link);
            }
        }
        c::SET_HEALTH => {
            let health = r.f32()?;
            let (food, saturation) = (r.varint()?, r.f32()?);
            link.status.health(health, food, saturation);
            shared.show_vitals(link, health, food);
            if health <= 0.0 && link.dead.is_none() {
                link.dead = Some(died());
                shared.settle(link);
            } else if health > 0.0 && link.dead.take().is_some() {
                shared.settle(link);
            }
        }
        c::PLAYER_COMBAT_KILL => {
            let _player = r.varint()?;
            let message = r.nbt().map(|m| text::line(&m).text).unwrap_or_default();
            link.dead = Some(if message.is_empty() { died() } else { message });
            shared.settle(link);
        }
        c::SYSTEM_CHAT => system_chat(shared, link, r)?,
        c::PLAYER_CHAT => player_chat(shared, link, r, types)?,
        c::DISGUISED_CHAT => disguised_chat(shared, link, r, types)?,
        c::SET_TIME => link.world.time(r)?,
        c::GAME_EVENT => {
            let (event, value) = (r.u8()?, r.f32()?);
            link.world.weather(event, value);
            link.status.game_event(event, value);
        }
        c::CHUNK_BATCH_FINISHED => write(link, s::CHUNK_BATCH_RECEIVED, Out::default().f32(CHUNKS_PER_TICK))?,
        c::START_CONFIGURATION => {
            write(link, s::CONFIGURATION_ACKNOWLEDGED, &mut Out::default())?;
            link.screen.reset();
            link.maps.reset();
            link.world.clear();
            link.crowd = Crowd::default();
            link.vitals = None;
            shared.show_nearby(link);
            link.status.reset();
            shared.show_screen(link);
            link.phase = Phase::Configuration;
            link.seen = Seen::default();
            link.signer = None;
            link.commands = None;
        }
        c::RESOURCE_PACK_PUSH => pushed(shared, link, r)?,
        c::RESOURCE_PACK_POP => popped(shared, link, r)?,
        c::COMMANDS if link.signer.is_some() => match Tree::read(r) {
            Ok(tree) => link.commands = Some(tree),
            Err(e) => {
                link.commands = None;
                eprintln!("{}: command tree not read, commands go out unsigned: {e:#}", shared.key);
            }
        },
        c::COOKIE_REQUEST => {
            let cookie = r.string()?;
            write(link, s::COOKIE_RESPONSE, Out::default().string(&cookie).bool(false))?;
        }
        c::TRANSFER => return Ok(Some(unsupported_transfer())),
        _ => {}
    }
    Ok(None)
}

/// Keeps what the world packet `id` shows, for pictures.
fn seen(link: &mut Link, id: i32, r: &mut In) -> Result<()> {
    use play::clientbound as c;
    match id {
        c::LOGIN => {
            link.world.clear();
            link.crowd.clear();
            let entity = r.i32()?;
            let hardcore = r.bool()?;
            link.status.login(entity, hardcore);
            for _ in 0..r.list()? {
                r.string()?;
            }
            for _ in 0..3 {
                r.varint()?;
            }
            for _ in 0..3 {
                r.bool()?;
            }
            link.world.enter(r.varint()?);
            let (dimension, mode) = spawn_info(r)?;
            link.status.game_mode(mode);
            link.dimension = dimension;
            link.due = Some(Instant::now());
        }
        c::LEVEL_CHUNK_WITH_LIGHT => {
            let [x, _, z] = link.position;
            link.world.chunk(r, ((x.floor() as i32) >> 4, (z.floor() as i32) >> 4))?;
        }
        c::FORGET_LEVEL_CHUNK => link.world.forget(r)?,
        c::BLOCK_UPDATE => link.world.block(r)?,
        c::SECTION_BLOCKS_UPDATE => link.world.section(r)?,
        c::LIGHT_UPDATE => link.world.light(r)?,
        _ => {}
    }
    Ok(())
}

/// The one picture an account has on the drawing thread, counted in `QUEUED` until it is drawn or given up.
struct Drawing(Arc<AtomicBool>);

impl Drawing {
    fn taken(flag: Arc<AtomicBool>) -> Self {
        QUEUED.fetch_add(1, Ordering::AcqRel);
        Self(flag)
    }
}

/// RGBA rows of `width` by `height` scaled down to at most `TILE_WIDTH` wide, as a tile shows them.
fn for_tile(rgba: Vec<u8>, width: u32, height: u32) -> (u32, u32, Vec<u8>) {
    if width <= TILE_WIDTH {
        return (width, height, rgba);
    }
    let Some(picture) = image::RgbaImage::from_raw(width, height, rgba) else { return (0, 0, Vec::new()) };
    let tall = (height * TILE_WIDTH / width).max(1);
    (TILE_WIDTH, tall, image::imageops::resize(&picture, TILE_WIDTH, tall, image::imageops::FilterType::Triangle).into_raw())
}

impl Drop for Drawing {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
        QUEUED.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Confirms a teleport with the position it leads to, and once per spawn tells the server the world is loaded.
fn teleported(shared: &Shared, link: &mut Link, r: &mut In) -> Result<()> {
    let id = r.varint()?;
    let target = [r.f64()?, r.f64()?, r.f64()?];
    let _motion = [r.f64()?, r.f64()?, r.f64()?];
    let turn = [r.f32()?, r.f32()?];
    let relative = r.i32()?;
    for (i, value) in target.into_iter().enumerate() {
        link.position[i] = if relative & (1 << i) != 0 { link.position[i] + value } else { value };
    }
    for (i, value) in turn.into_iter().enumerate() {
        link.rotation[i] = if relative & (1 << (3 + i)) != 0 { link.rotation[i] + value } else { value };
    }
    link.rotation[1] = link.rotation[1].clamp(-90.0, 90.0);
    let [x, y, z] = link.position;
    let [yaw, pitch] = link.rotation;
    write(link, play::serverbound::ACCEPT_TELEPORTATION, Out::default().varint(id).f64(x).f64(y).f64(z).f32(yaw).f32(pitch))?;
    if !link.loaded {
        write(link, play::serverbound::PLAYER_LOADED, &mut Out::default())?;
        link.loaded = true;
        link.played = true;
        shared.settle(link);
    }
    Ok(())
}

/// The dimension and the game mode from the rest of a spawn info, after its dimension type.
fn spawn_info(r: &mut In) -> Result<(String, i32)> {
    let dimension = r.string()?;
    r.i64()?;
    Ok((dimension, r.varint()?))
}

/// What a respawn keeps (1 attribute modifiers, 2 entity data), from the rest of its spawn info after the game mode.
fn respawn_keep(r: &mut In) -> Result<u8> {
    r.varint()?;
    r.bool()?;
    r.bool()?;
    if r.bool()? {
        r.string()?;
        r.i64()?;
    }
    r.varint()?;
    r.varint()?;
    r.u8()
}

fn system_chat(shared: &Shared, link: &mut Link, r: &mut In) -> Result<()> {
    let line = text::line(&r.nbt()?);
    if r.bool()? {
        link.screen.overlay(&line);
        shared.show_screen(link);
    } else if !line.text.is_empty() {
        said(shared, link, FromMod::Chat { kind: ChatKind::System, from: None, text: line.text, runs: line.runs });
    }
    Ok(())
}

fn disguised_chat(shared: &Shared, link: &mut Link, r: &mut In, types: &[ChatType]) -> Result<()> {
    let message = r.nbt()?;
    let text = text::line(&message).text;
    if let Some(message) = bound(r, types, &message, text)? {
        said(shared, link, message);
    }
    Ok(())
}

/// Hands a chat line to the manager and keeps it for the chat in pictures.
fn said(shared: &Shared, link: &mut Link, message: FromMod) {
    if let FromMod::Chat { text, runs, .. } = &message {
        link.status.chat(if runs.is_empty() { vec![Run { text: text.clone(), ..Run::default() }] } else { runs.clone() });
    }
    shared.emit(message);
}

fn player_chat(shared: &Shared, link: &mut Link, r: &mut In, types: &[ChatType]) -> Result<()> {
    let _global = r.varint()?;
    let _sender = r.uuid()?;
    let _index = r.varint()?;
    let signed = r.bool()?;
    if signed {
        let signature: Signature = r.take(256)?.try_into()?;
        link.seen.add(signature);
        if link.seen.offset() > ACK_AFTER {
            let seen = link.seen.take_offset();
            write(link, play::serverbound::CHAT_ACK, Out::default().varint(seen))?;
        }
    }
    let content = r.string()?;
    let _time = r.i64()?;
    let _salt = r.i64()?;
    for _ in 0..r.varint()? {
        if r.varint()? == 0 {
            r.take(256)?;
        }
    }
    let unsigned = r.optional_nbt()?;
    let mask = r.varint()?;
    if mask == 2 {
        let words = usize::try_from(r.varint()?)?;
        r.take(words.checked_mul(8).context("filter mask too long")?)?;
    }
    if mask == 1 {
        return Ok(());
    }
    let shown = unsigned.unwrap_or_else(|| Value::String(content.clone()));
    if let Some(message) = bound(r, types, &shown, content)? {
        said(shared, link, message);
    }
    Ok(())
}

/// Reads the bound chat type after a player's line and makes the chat message: `text` from the sender, with `content`
/// decorated as the game shows it. `None` for the echo of the account's own private messages, which the manager shows
/// already. A chat type defined inline is rare and not read; its line comes without sender and decoration.
fn bound(r: &mut In, types: &[ChatType], content: &Value, text: String) -> Result<Option<FromMod>> {
    let holder = r.varint()?;
    if holder == 0 {
        return Ok(Some(FromMod::Chat { kind: ChatKind::Player, from: None, text, runs: Vec::new() }));
    }
    let name = r.nbt()?;
    let target = r.optional_nbt()?;
    let kind = usize::try_from(holder).ok().and_then(|h| h.checked_sub(1)).and_then(|i| types.get(i));
    if kind.is_some_and(|k| OUTGOING.contains(&k.name.as_str())) {
        return Ok(None);
    }
    let (key, parameters): (&str, Vec<&str>) = match kind {
        Some(k) => (&k.key, k.parameters.iter().map(String::as_str).collect()),
        None => ("chat.type.text", vec!["sender", "content"]),
    };
    let args: Vec<Value> = parameters.into_iter().map(|p| match p {
        "sender" => name.clone(),
        "target" => target.clone().unwrap_or(Value::String(String::new())),
        _ => content.clone(),
    }).collect();
    let from = Some(text::line(&name).text).filter(|n| !n.is_empty());
    Ok(Some(FromMod::Chat { kind: ChatKind::Player, from, text, runs: text::translate(key, &args).runs }))
}

#[cfg(test)]
mod tests {
    use super::super::signing::test_certificate;
    use super::*;

    fn spec() -> Spec {
        Spec { key: "k".into(), address: "127.0.0.1:1".into(), name: "Dev".into(), uuid: Uuid::nil(), session: None, account: None, version: "26.3".into(), packs: Packs::Accept, proxy: None, conduct: Vec::new() }
    }

    fn shared() -> Shared {
        Shared::new(&spec(), std::sync::mpsc::channel().0)
    }

    fn noise(seed: &mut u64, len: usize) -> Vec<u8> {
        (0..len)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                if *seed % 5 == 0 { 0xff } else { (*seed >> 24) as u8 }
            })
            .collect()
    }

    #[test]
    fn no_packet_body_panics_or_overflows_the_stack_however_it_is_cut_or_made_up() {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let shared = shared();
                let mut seed = 0x9e37_79b9_7f4a_7c15u64;
                for phase in [Phase::Configuration, Phase::Play] {
                    for id in (0..0x100).filter(|id| wanted(phase, *id)) {
                        for round in 0..300 {
                            let body = noise(&mut seed, round % 90);
                            let mut link = shared.lock();
                            link.phase = phase;
                            let mut r = In::new(&body);
                            let _ = match phase {
                                Phase::Configuration => configuration(&shared, &mut link, id, &mut r, &mut Vec::new()),
                                _ => playing(&shared, &mut link, id, &mut r, &[]),
                            };
                        }
                    }
                }
            })
            .unwrap()
            .join()
            .expect("a packet body made the client panic");
    }

    #[test]
    fn only_what_the_game_itself_cannot_survive_ends_a_connection_on_a_bad_packet() {
        use cfg::clientbound as c;
        use play::clientbound as p;
        for id in [c::KEEP_ALIVE, c::PING, c::DISCONNECT, c::FINISH_CONFIGURATION, c::TRANSFER, c::COOKIE_REQUEST, c::CODE_OF_CONDUCT, c::RESOURCE_PACK_PUSH, c::SELECT_KNOWN_PACKS] {
            assert!(essential(Phase::Configuration, id), "the server waits for an answer to {id}");
        }
        for id in [c::REGISTRY_DATA, c::RESOURCE_PACK_POP] {
            assert!(!essential(Phase::Configuration, id));
        }
        for id in [p::KEEP_ALIVE, p::PING, p::DISCONNECT, p::PLAYER_POSITION, p::START_CONFIGURATION, p::TRANSFER, p::COOKIE_REQUEST] {
            assert!(essential(Phase::Play, id));
        }
        for id in [p::SYSTEM_CHAT, p::PLAYER_CHAT, p::DISGUISED_CHAT, p::SET_HEALTH, p::RESPAWN, p::PLAYER_COMBAT_KILL, p::RESOURCE_PACK_PUSH, p::RESOURCE_PACK_POP, p::CHUNK_BATCH_FINISHED] {
            assert!(!essential(Phase::Play, id));
        }
    }

    #[test]
    fn what_the_heads_up_display_shows_is_read_but_never_essential() {
        use play::clientbound as p;
        for id in [p::SET_EXPERIENCE, p::UPDATE_ATTRIBUTES, p::UPDATE_MOB_EFFECT, p::REMOVE_MOB_EFFECT, p::SET_OBJECTIVE, p::SET_DISPLAY_OBJECTIVE, p::SET_SCORE, p::RESET_SCORE, p::SET_PLAYER_TEAM, p::BOSS_EVENT, p::SET_TITLES_ANIMATION, p::SET_ENTITY_DATA, p::GAME_EVENT] {
            assert!(wanted(Phase::Play, id) && !essential(Phase::Play, id), "packet {id}");
        }
    }

    fn frame(stream: &mut std::net::TcpStream) -> Option<(i32, Vec<u8>)> {
        use std::io::Read;
        let mut byte = [0u8; 1];
        let mut len = 0usize;
        for shift in (0..).step_by(7).take(5) {
            stream.read_exact(&mut byte).ok()?;
            len |= usize::from(byte[0] & 0x7f) << shift;
            if byte[0] & 0x80 == 0 {
                break;
            }
        }
        let mut packet = vec![0u8; len];
        stream.read_exact(&mut packet).ok()?;
        let mut r = In::new(&packet);
        let id = r.varint().ok()?;
        let used = packet.len() - r.left();
        Some((id, packet[used..].to_vec()))
    }

    fn connected(shared: &Shared) -> std::net::TcpStream {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut link = shared.lock();
        link.writer = Some(Writer::new(client));
        link.phase = Phase::Play;
        server
    }

    fn playing_with_a_key(shared: &Shared, certificate: ChatCertificate) -> std::net::TcpStream {
        let server = connected(shared);
        let mut link = shared.lock();
        let certificate = Arc::new(certificate);
        link.signer = Some(Signer::new(&certificate, link.me).unwrap());
        link.certificate = Some(certificate);
        server
    }

    #[test]
    fn a_chat_key_that_runs_out_is_replaced_and_announced_as_a_new_session_while_the_connection_stays() {
        let shared = shared();
        let now = now_millis();
        let mut server = playing_with_a_key(&shared, test_certificate(1, now + 400, now + 5_000));
        server.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let old = shared.lock().signer.as_ref().unwrap().announcement().0.clone();
        shared.lock().seen.add([7; 256]);
        let started = Instant::now();
        let mut fetched = 0;
        let sent = thread::scope(|scope| {
            scope.spawn(|| {
                shared.renew_with(|| {
                    fetched += 1;
                    Ok(test_certificate(2, now_millis() + 3_600_000, now_millis() + 7_200_000))
                })
            });
            let sent = frame(&mut server);
            let mut link = shared.lock();
            link.closed = true;
            shared.wake.notify_all();
            sent
        });
        let (id, body) = sent.expect("a session update");
        assert_eq!(id, play::serverbound::CHAT_SESSION_UPDATE);
        assert!(started.elapsed() >= Duration::from_millis(350), "not before the key is due");
        assert_eq!(fetched, 1);
        assert_ne!(body[..16], old[..16], "a new session id");
        assert_eq!(body, shared.lock().signer.as_ref().unwrap().announcement().0);
        assert_eq!(shared.lock().seen.offset(), 1, "what was seen stays");
        assert_eq!(shared.certificate.lock().unwrap().as_ref().unwrap().public_key, vec![2; 8], "kept for a reconnect");
        assert_eq!(shared.lock().certificate.as_ref().unwrap().public_key, vec![2; 8]);
    }

    #[test]
    fn a_chat_key_that_cannot_be_renewed_leaves_the_old_one_signing() {
        let shared = shared();
        let now = now_millis();
        let mut server = playing_with_a_key(&shared, test_certificate(1, now - 1, now + 5_000));
        server.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let old = shared.lock().signer.as_ref().unwrap().announcement().0.clone();
        let (asked, was_asked) = std::sync::mpsc::channel();
        let sent = thread::scope(|scope| {
            scope.spawn(|| {
                shared.renew_with(|| {
                    asked.send(()).unwrap();
                    bail!("services are down")
                })
            });
            was_asked.recv_timeout(Duration::from_secs(20)).expect("it asks");
            let sent = frame(&mut server);
            let mut link = shared.lock();
            link.closed = true;
            shared.wake.notify_all();
            sent
        });
        assert!(sent.is_none());
        assert_eq!(old, shared.lock().signer.as_ref().unwrap().announcement().0);
    }

    fn login_ending_in(flag: bool) -> Vec<u8> {
        let mut body = vec![0u8, 1, 0, 3, 15, 0, 0, 1, 2, 3, 4];
        body.extend([0, u8::from(flag)]);
        body
    }

    fn account_with_a_kept_key() -> (Spec, Shared) {
        let mut spec = spec();
        spec.session = Some(Session { name: "Dev".into(), uuid: Uuid::nil(), access_token: "unused".into(), xuid: String::new(), expires_at: SystemTime::now() + Duration::from_secs(3600) });
        let shared = Shared::new(&spec, std::sync::mpsc::channel().0);
        *shared.certificate.lock().unwrap() = Some(Arc::new(test_certificate(9, now_millis() + 3_600_000, now_millis() + 7_200_000)));
        (spec, shared)
    }

    #[test]
    fn the_flag_is_the_last_byte_of_the_login_packet() {
        assert!(enforces_secure_chat(&In::new(&login_ending_in(true))));
        assert!(!enforces_secure_chat(&In::new(&login_ending_in(false))));
        assert!(!enforces_secure_chat(&In::new(&[])));
    }

    #[test]
    fn a_server_that_enforces_secure_chat_gets_a_chat_session_announced() {
        let (spec, shared) = account_with_a_kept_key();
        let mut server = connected(&shared);
        server.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let body = login_ending_in(true);
        let mut link = shared.lock();
        link.certificate = login_certificate(&shared, &spec, &In::new(&body));
        assert!(link.certificate.is_some());
        start_chat_session(&shared, &mut link).unwrap();
        let (id, sent) = frame(&mut server).expect("a session update");
        assert_eq!(id, play::serverbound::CHAT_SESSION_UPDATE);
        assert_eq!(sent, link.signer.as_ref().unwrap().announcement().0);
    }

    #[test]
    fn a_server_that_does_not_enforce_it_gets_no_session_no_key_and_no_signature() {
        let (spec, shared) = account_with_a_kept_key();
        let mut server = connected(&shared);
        server.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let body = login_ending_in(false);
        let mut link = shared.lock();
        link.certificate = login_certificate(&shared, &spec, &In::new(&body));
        assert!(link.certificate.is_none());
        start_chat_session(&shared, &mut link).unwrap();
        assert!(link.signer.is_none());
        assert!(frame(&mut server).is_none(), "nothing is sent");
        link.lines = Queue::new(Instant::now());
        chat(&mut link, "hello").unwrap();
        let (id, sent) = frame(&mut server).expect("the chat line");
        assert_eq!(id, play::serverbound::CHAT);
        assert_eq!(sent.last().copied(), Some(1), "an unsigned message acknowledges nothing and carries no signature");
    }

    #[test]
    fn a_connection_that_does_not_sign_has_no_key_to_renew() {
        let shared = shared();
        let mut link = shared.lock();
        link.phase = Phase::Play;
        assert!(renew_chat_session(&mut link, Arc::new(test_certificate(1, 0, 0))).is_ok());
        assert!(link.signer.is_none());
    }

    #[test]
    fn a_pack_is_answered_without_downloading_it_in_the_order_the_game_answers() {
        let id = Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
        let answer = |phase, accept| pack_answers(phase, id, accept);
        let tail = |action: u8| [id.as_bytes().as_slice(), &[action]].concat();
        assert_eq!(answer(Phase::Configuration, true), vec![(cfg::serverbound::RESOURCE_PACK, tail(3)), (cfg::serverbound::RESOURCE_PACK, tail(4)), (cfg::serverbound::RESOURCE_PACK, tail(0))]);
        assert_eq!(answer(Phase::Play, true), vec![(play::serverbound::RESOURCE_PACK, tail(3)), (play::serverbound::RESOURCE_PACK, tail(4)), (play::serverbound::RESOURCE_PACK, tail(0))]);
        assert_eq!(answer(Phase::Configuration, false), vec![(cfg::serverbound::RESOURCE_PACK, tail(1))]);
        assert_eq!(answer(Phase::Play, false), vec![(play::serverbound::RESOURCE_PACK, tail(1))]);
    }

    fn body(host: &str, port: u16) -> Vec<u8> {
        handshake(host, port).0
    }

    fn expected(host: &str, port: u16) -> Vec<u8> {
        let mut bytes = vec![0x89, 0x06, host.len() as u8];
        bytes.extend_from_slice(host.as_bytes());
        bytes.extend_from_slice(&port.to_be_bytes());
        bytes.push(2);
        bytes
    }

    #[test]
    fn the_handshake_carries_the_protocol_host_port_and_login_intention_and_nothing_else() {
        assert_eq!(PROTOCOL, 777, "the bytes below are for protocol 777");
        assert_eq!(body("mc.example.org", 25565), [&[0x89u8, 0x06, 14][..], b"mc.example.org", &[0x63, 0xdd], &[2]].concat());
        assert_eq!(body("localhost", 25570), expected("localhost", 25570));
        assert_eq!(body("mc.example.org.net.", 25570), expected("mc.example.org.net.", 25570), "an SRV target keeps its closing dot, as in the game");
        for host in ["mc.example.org", "10.0.0.1", "mc.example.org.net."] {
            assert!(!body(host, 25565).contains(&0), "no \\0FML or forwarding marker after {host}");
        }
    }

    fn respawn(dimension: &str) -> Vec<u8> {
        let mut out = Out::default();
        out.varint(0).string(dimension).i64(0).varint(0).varint(0).bool(false).bool(false).bool(false).varint(0).varint(0).u8(0);
        out.0
    }

    fn health(health: f32, food: i32) -> Vec<u8> {
        let mut out = Out::default();
        out.f32(health).varint(food).f32(0.0);
        out.0
    }

    #[test]
    fn health_and_food_are_reported_when_they_change_and_zero_health_is_a_death() {
        use play::clientbound as p;
        let (tx, rx) = std::sync::mpsc::channel();
        let spec = Spec { key: "k".into(), address: "127.0.0.1:1".into(), name: "Dev".into(), uuid: Uuid::nil(), session: None, account: None, version: "26.3".into(), packs: Packs::Accept, proxy: None, conduct: Vec::new() };
        let shared = Shared::new(&spec, tx);
        let mut link = shared.lock();
        link.phase = Phase::Play;
        let mut seen = |link: &mut Link, health: f32, food: i32| {
            playing(&shared, link, p::SET_HEALTH, &mut In::new(&self::health(health, food)), &[]).unwrap();
            rx.try_iter().filter_map(|e| if let Event::Message { message: FromMod::Vitals { health, food }, .. } = e { Some((health, food)) } else { None }).collect::<Vec<_>>()
        };
        assert_eq!(seen(&mut link, 14.0, 18), [(14, 18)]);
        assert!(seen(&mut link, 14.0, 18).is_empty(), "the same values again say nothing");
        assert_eq!(seen(&mut link, 6.5, 17), [(7, 17)], "half hearts round up so a living player never reads zero");
        assert!(link.dead.is_none());
        assert_eq!(seen(&mut link, 0.0, 17), [(0, 17)]);
        assert!(link.dead.is_some());
    }

    #[test]
    fn a_change_of_world_takes_one_screenshot_once_the_player_stands_in_it() {
        use play::clientbound as p;
        let (tx, rx) = std::sync::mpsc::channel();
        let spec = Spec { key: "k".into(), address: "127.0.0.1:1".into(), name: "Dev".into(), uuid: Uuid::nil(), session: None, account: None, version: "26.3".into(), packs: Packs::Accept, proxy: None, conduct: Vec::new() };
        let shared = Shared::new(&spec, tx);
        let mut link = shared.lock();
        link.phase = Phase::Play;
        link.dimension = "minecraft:overworld".into();
        playing(&shared, &mut link, p::RESPAWN, &mut In::new(&respawn("minecraft:overworld")), &[]).unwrap();
        assert!(link.due.is_none(), "a respawn in the same world is no change of world");
        playing(&shared, &mut link, p::RESPAWN, &mut In::new(&respawn("minecraft:the_nether")), &[]).unwrap();
        assert!(link.due.is_some());
        link.due = Some(Instant::now() - SHOT_WAIT);
        shared.world_shot(&mut link);
        assert!(link.due.is_some(), "not before the player stands in the new world");
        link.loaded = true;
        link.due = Some(Instant::now());
        shared.world_shot(&mut link);
        assert!(link.due.is_some(), "not while the chunks around it are missing and it waited less than SHOT_WAIT");
        link.due = Some(Instant::now() - SHOT_WAIT);
        shared.world_shot(&mut link);
        assert!(link.due.is_none());
        drop(link);
        let Ok(Event::Picture { width, height, moment, .. }) = rx.recv_timeout(Duration::from_secs(120)) else { panic!("no screenshot") };
        assert_eq!((width, height), (TILE_WIDTH, TILE_WIDTH * 9 / 16), "the tile gets a smaller picture");
        let full = shots::decode_png(&moment.eyes).unwrap();
        assert_eq!((full.width(), full.height()), (SHOT_WIDTH, SHOT_WIDTH * 9 / 16));
        assert!(moment.eyes.starts_with(b"\x89PNG") && moment.above.starts_with(b"\x89PNG"));
        assert!(moment.taken > 0);
    }
}
