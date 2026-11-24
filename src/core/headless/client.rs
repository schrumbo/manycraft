//! One connection-only account: logs in, answers what the protocol requires and reports like a companion mod.

use std::collections::{HashSet, VecDeque};
use std::io::ErrorKind;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::picture::{self, Request, View};
use super::screen::Screen;
use super::Spec;
use super::classify::classify;
use super::conn::{Reader, Writer};
use super::crypto;
use super::text::{self, Line};
use super::v777::{PROTOCOL, configuration as cfg, login, play};
use super::wire::{In, Out};
use super::world::World;
use crate::core::auth::Session;
use crate::core::batch::Packs;
use crate::core::ipc::{ChatKind, DisconnectCause, Event, FromMod, Offer, StateKind, ToMod};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const CLIENT_TICK: Duration = Duration::from_millis(50);
const BRAND: &str = "vanilla";
const VIEW_DISTANCE: u8 = 2;
const PICTURE_RADIUS: i32 = VIEW_DISTANCE as i32 + 1;
const MIN_PICTURE: u32 = 16;
const MAX_PICTURE: u32 = 1280;

static PICTURES: AtomicU32 = AtomicU32::new(0);
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
const PERFORM_RESPAWN: i32 = 0;

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
    unacked: i32,
    shown: Option<Shown>,
    screen: Screen,
    world: World,
}

/// What the connection's thread and its handle share.
pub struct Shared {
    key: String,
    events: Sender<Event>,
    link: Mutex<Link>,
    wake: Condvar,
    running: AtomicBool,
    shots: Option<PathBuf>,
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
            unacked: 0,
            shown: None,
            screen: Screen::default(),
            world: World::default(),
        };
        Self { key: spec.key.clone(), events, link: Mutex::new(link), wake: Condvar::new(), running: AtomicBool::new(true), shots: spec.shots.clone(), drawing: Arc::default() }
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
            ToMod::ChatSend { text } => chat(&mut link, text)?,
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
            ToMod::Capture { width } => self.picture(&link, *width),
            _ => {}
        }
        Ok(())
    }

    /// Draws what the account sees, from its eyes and from above, on the drawing thread. Both are saved to its screenshots
    /// folder; the view from its eyes goes to its tile. While one is being drawn, more asks are ignored.
    fn picture(&self, link: &Link, width: u32) {
        if link.phase != Phase::Play || self.drawing.swap(true, Ordering::AcqRel) {
            return;
        }
        let width = width.clamp(MIN_PICTURE, MAX_PICTURE);
        let height = (width * 9 / 16).max(1);
        let [x, _, z] = link.position;
        let request = Request { world: link.world.snapshot(x, z, PICTURE_RADIUS), position: link.position, rotation: link.rotation, width, height };
        let stamp = chrono::Local::now().format("%Y-%m-%d_%H.%M.%S").to_string();
        let (key, events, shots, drawing) = (self.key.clone(), self.events.clone(), self.shots.clone(), Drawing(Arc::clone(&self.drawing)));
        picture::queue(request, vec![View::Eyes, View::Above], move |mut pictures| {
            let above = pictures.pop().unwrap_or_default();
            let eyes = pictures.pop().unwrap_or_default();
            if let Some(dir) = shots {
                save(&dir, &stamp, &[("", &eyes), ("_above", &above)], width, height);
            }
            drop(drawing);
            let seq = PICTURES.fetch_add(2, Ordering::Relaxed).wrapping_add(2);
            let _ = events.send(Event::Picture { instance: key, seq, width, height, rgba: eyes });
        });
    }

    fn show_screen(&self, link: &mut Link) {
        for message in link.screen.changes() {
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

fn answer_pack(link: &mut Link, id: Uuid, accept: bool) -> Result<()> {
    let packet = if link.phase == Phase::Play { play::serverbound::RESOURCE_PACK } else { cfg::serverbound::RESOURCE_PACK };
    let actions: &[i32] = if accept { &[PACK_ACCEPTED, PACK_LOADED] } else { &[PACK_DECLINED] };
    for action in actions {
        write(link, packet, Out::default().uuid(id).varint(*action))?;
    }
    Ok(())
}

/// Sends what the user typed: a command when it starts with `/`, otherwise a chat message, both unsigned.
fn chat(link: &mut Link, text: &str) -> Result<()> {
    ensure!(link.phase == Phase::Play, "not on a server");
    let text = text.trim();
    ensure!(!text.is_empty(), "nothing to send");
    ensure!(text.encode_utf16().count() <= MAX_CHAT, "longer than {MAX_CHAT} characters");
    ensure!(!text.chars().any(|c| c == '§' || c < ' ' || c == '\u{7f}'), "contains characters the server refuses");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    link.chat_clock = now.max(link.chat_clock + 1);
    let mut salt = [0u8; 8];
    crypto::random(&mut salt)?;
    let mut out = Out::default();
    let id = if let Some(command) = text.strip_prefix('/') {
        out.string(command).i64(link.chat_clock).raw(&salt).varint(0);
        play::serverbound::CHAT_COMMAND_SIGNED
    } else {
        out.string(text).i64(link.chat_clock).raw(&salt).bool(false);
        play::serverbound::CHAT
    };
    out.varint(link.unacked).raw(&[0, 0, 0]).u8(0);
    write(link, id, &mut out)?;
    link.unacked = 0;
    Ok(())
}

/// Runs the connection until it is told to quit: joins, and after a drop waits to be told to reconnect.
pub fn run(shared: &Shared, spec: &Spec) {
    let _ended = Ended(shared);
    let _ = shared.events.send(Event::Connected { instance: shared.key.clone(), pid: 0, mod_version: format!("connection-only protocol {PROTOCOL}") });
    loop {
        match session(shared, spec) {
            End::Quit => break,
            End::Reconnect => continue,
            End::Dropped(reason, cause) => {
                {
                    let mut link = shared.lock();
                    link.held.clear();
                    link.dead = None;
                    link.world.clear();
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
        link.unacked = 0;
        shared.report(&mut link, StateKind::Connecting, None, None, None);
    }
    let outcome = join(shared, spec);
    let mut link = shared.lock();
    link.writer = None;
    link.phase = Phase::Login;
    link.held.clear();
    link.screen.reset();
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
    let (host, _) = crate::core::fleet::split_server(address)?;
    let addrs = crate::core::fleet::resolve_all(address).map_err(|e| anyhow::anyhow!("Unknown host: {e}"))?;
    let mut last = None;
    for addr in addrs {
        if shared.lock().control.is_some() {
            bail!("stopped");
        }
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok((stream, host, port(addr))),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| anyhow::anyhow!("Unknown host"), anyhow::Error::from))
}

fn port(addr: SocketAddr) -> u16 {
    addr.port()
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
        write(&mut link, 0, Out::default().varint(PROTOCOL).string(&host).u16(port).varint(2))?;
        write(&mut link, login::serverbound::HELLO, Out::default().string(&spec.name).uuid(spec.uuid))?;
    }
    if let Some(end) = log_in(shared, spec, &mut reader)? {
        return Ok(end);
    }
    let mut chat_types = Vec::new();
    loop {
        let phase = shared.lock().phase;
        let packet = reader.next(|id| wanted(phase, id))?;
        if phase == Phase::Play && packet.id == play::clientbound::PLAYER_POSITION {
            thread::sleep(CLIENT_TICK);
        }
        let mut r = In::new(packet.body);
        let mut link = shared.lock();
        let end = match phase {
            Phase::Configuration => configuration(shared, &mut link, packet.id, &mut r, &mut chat_types)?,
            _ => playing(shared, &mut link, packet.id, &mut r, &chat_types)?,
        };
        if let Some(end) = end {
            return Ok(end);
        }
    }
}

fn wanted(phase: Phase, id: i32) -> bool {
    use cfg::clientbound as c;
    use play::clientbound as p;
    match phase {
        Phase::Configuration => [c::COOKIE_REQUEST, c::DISCONNECT, c::FINISH_CONFIGURATION, c::KEEP_ALIVE, c::PING, c::REGISTRY_DATA, c::RESOURCE_PACK_POP, c::RESOURCE_PACK_PUSH, c::TRANSFER, c::SELECT_KNOWN_PACKS, c::CODE_OF_CONDUCT].contains(&id),
        _ => [p::BLOCK_UPDATE, p::CHUNK_BATCH_FINISHED, p::FORGET_LEVEL_CHUNK, p::LEVEL_CHUNK_WITH_LIGHT, p::LOGIN, p::SECTION_BLOCKS_UPDATE, p::COOKIE_REQUEST, p::DISCONNECT, p::DISGUISED_CHAT, p::KEEP_ALIVE, p::PING, p::PLAYER_CHAT, p::PLAYER_COMBAT_KILL, p::PLAYER_POSITION, p::RESOURCE_PACK_POP, p::RESOURCE_PACK_PUSH, p::RESPAWN, p::SET_HEALTH, p::START_CONFIGURATION, p::SYSTEM_CHAT, p::TRANSFER].contains(&id) || Screen::reads(id),
    }
}

fn log_in(shared: &Shared, spec: &Spec, reader: &mut Reader) -> Result<Option<(Line, DisconnectCause)>> {
    use login::clientbound as c;
    use login::serverbound as s;
    loop {
        let packet = reader.next(|_| true)?;
        let mut r = In::new(packet.body);
        match packet.id {
            c::LOGIN_DISCONNECT => {
                let json = r.string()?;
                let value = serde_json::from_str(&json).unwrap_or(Value::String(json));
                return Ok(Some(dropped(text::line(&value))));
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
                }
                let sealed_secret = crypto::rsa_encrypt(&key, &secret)?;
                let sealed_token = crypto::rsa_encrypt(&key, &token)?;
                let mut link = shared.lock();
                write(&mut link, s::KEY, Out::default().bytes(&sealed_secret).bytes(&sealed_token))?;
                link.writer.as_mut().context("not connected")?.encrypt(&secret);
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
                let mut link = shared.lock();
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
            "minecraft:chat_type" => *types = chat_types(r)?,
            "minecraft:dimension_type" => {
                if let Err(e) = link.world.dimension_types(r) {
                    eprintln!("{}: dimension types not read: {e:#}", shared.key);
                }
            }
            _ => {}
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
    let _url = r.string()?;
    let _hash = r.string()?;
    let required = r.bool()?;
    let text = r.optional_nbt()?.map(|p| text::line(&p).text);
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
        c::LOGIN | c::LEVEL_CHUNK_WITH_LIGHT | c::FORGET_LEVEL_CHUNK | c::BLOCK_UPDATE | c::SECTION_BLOCKS_UPDATE => {
            if let Err(e) = seen(link, id, r) {
                eprintln!("{}: world packet {id} not read: {e:#}", shared.key);
            }
        }
        c::RESPAWN => {
            link.world.clear();
            if let Ok(kind) = r.varint() {
                link.world.enter(kind);
            }
            link.loaded = false;
            if link.dead.take().is_some() {
                shared.settle(link);
            }
        }
        c::SET_HEALTH => {
            let health = r.f32()?;
            if health <= 0.0 && link.dead.is_none() {
                link.dead = Some(died());
                shared.settle(link);
            } else if health > 0.0 && link.dead.take().is_some() {
                shared.settle(link);
            }
        }
        c::PLAYER_COMBAT_KILL => {
            let _player = r.varint()?;
            let message = text::line(&r.nbt()?).text;
            link.dead = Some(if message.is_empty() { died() } else { message });
            shared.settle(link);
        }
        c::SYSTEM_CHAT => {
            let line = text::line(&r.nbt()?);
            if !r.bool()? && !line.text.is_empty() {
                shared.emit(FromMod::Chat { kind: ChatKind::System, from: None, text: line.text, runs: line.runs });
            }
        }
        c::PLAYER_CHAT => player_chat(shared, link, r, types)?,
        c::DISGUISED_CHAT => {
            let message = r.nbt()?;
            let text = text::line(&message).text;
            if let Some(message) = bound(r, types, &message, text)? {
                shared.emit(message);
            }
        }
        c::CHUNK_BATCH_FINISHED => write(link, s::CHUNK_BATCH_RECEIVED, Out::default().f32(CHUNKS_PER_TICK))?,
        c::START_CONFIGURATION => {
            write(link, s::CONFIGURATION_ACKNOWLEDGED, &mut Out::default())?;
            link.screen.reset();
            link.world.clear();
            shared.show_screen(link);
            link.phase = Phase::Configuration;
            link.unacked = 0;
        }
        c::RESOURCE_PACK_PUSH => pushed(shared, link, r)?,
        c::RESOURCE_PACK_POP => popped(shared, link, r)?,
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
            let _entity = r.i32()?;
            let _hardcore = r.bool()?;
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
        }
        c::LEVEL_CHUNK_WITH_LIGHT => {
            let [x, _, z] = link.position;
            link.world.chunk(r, ((x.floor() as i32) >> 4, (z.floor() as i32) >> 4))?;
        }
        c::FORGET_LEVEL_CHUNK => link.world.forget(r)?,
        c::BLOCK_UPDATE => link.world.block(r)?,
        c::SECTION_BLOCKS_UPDATE => link.world.section(r)?,
        _ => {}
    }
    Ok(())
}

struct Drawing(Arc<AtomicBool>);

impl Drop for Drawing {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn save(dir: &std::path::Path, stamp: &str, pictures: &[(&str, &[u8])], width: u32, height: u32) {
    let name = |n: u32, suffix: &str| dir.join(if n == 0 { format!("{stamp}{suffix}.png") } else { format!("{stamp}_{n}{suffix}.png") });
    let free = (0..1000).find(|n| pictures.iter().all(|(suffix, _)| !name(*n, suffix).exists())).unwrap_or(1000);
    for (suffix, rgba) in pictures {
        let path = name(free, suffix);
        let part = path.with_extension("png.part");
        let saved = std::fs::create_dir_all(dir)
            .map_err(anyhow::Error::from)
            .and_then(|()| image::save_buffer_with_format(&part, rgba, width, height, image::ExtendedColorType::Rgba8, image::ImageFormat::Png).map_err(anyhow::Error::from))
            .and_then(|()| std::fs::rename(&part, &path).map_err(anyhow::Error::from));
        if let Err(e) = saved {
            let _ = std::fs::remove_file(&part);
            eprintln!("picture not saved to {}: {e:#}", path.display());
        }
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

fn player_chat(shared: &Shared, link: &mut Link, r: &mut In, types: &[ChatType]) -> Result<()> {
    let _global = r.varint()?;
    let _sender = r.uuid()?;
    let _index = r.varint()?;
    let signed = r.bool()?;
    if signed {
        r.take(256)?;
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
    if signed {
        link.unacked += 1;
        if link.unacked > ACK_AFTER {
            write(link, play::serverbound::CHAT_ACK, Out::default().varint(link.unacked))?;
            link.unacked = 0;
        }
    }
    if mask == 1 {
        return Ok(());
    }
    let shown = unsigned.unwrap_or_else(|| Value::String(content.clone()));
    if let Some(message) = bound(r, types, &shown, content)? {
        shared.emit(message);
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
