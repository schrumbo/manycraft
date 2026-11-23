//! One connection-only account: logs in, answers what the protocol requires and reports like a companion mod.

use std::collections::{HashSet, VecDeque};
use std::io::ErrorKind;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Spec;
use super::classify::classify;
use super::conn::{Reader, Writer};
use super::crypto;
use super::text::{self, Line};
use super::v777::{PROTOCOL, configuration as cfg, login, play};
use super::wire::{In, Out};
use crate::core::auth::Session;
use crate::core::batch::Packs;
use crate::core::fleet::Resolved;
use crate::core::ipc::{ChatKind, DisconnectCause, Event, FromMod, Offer, StateKind, ToMod};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const CLIENT_TICK: Duration = Duration::from_millis(50);
const BRAND: &str = "vanilla";
const INTENTION_LOGIN: i32 = 2;
const VIEW_DISTANCE: u8 = 2;
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
}

/// What the connection's thread and its handle share.
pub struct Shared {
    key: String,
    events: Sender<Event>,
    link: Mutex<Link>,
    wake: Condvar,
    running: AtomicBool,
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
        };
        Self { key: spec.key.clone(), events, link: Mutex::new(link), wake: Condvar::new(), running: AtomicBool::new(true) }
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
            _ => {}
        }
        Ok(())
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
    let actions: &[i32] = if accept { &[PACK_ACCEPTED, PACK_DOWNLOADED, PACK_LOADED] } else { &[PACK_DECLINED] };
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
        let mut link = shared.lock();
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
        _ => [p::CHUNK_BATCH_FINISHED, p::COOKIE_REQUEST, p::DISCONNECT, p::DISGUISED_CHAT, p::KEEP_ALIVE, p::PING, p::PLAYER_CHAT, p::PLAYER_COMBAT_KILL, p::PLAYER_POSITION, p::RESOURCE_PACK_POP, p::RESOURCE_PACK_PUSH, p::RESPAWN, p::SET_HEALTH, p::START_CONFIGURATION, p::SYSTEM_CHAT, p::TRANSFER].contains(&id),
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

fn chat_types(r: &mut In) -> Result<Option<Vec<ChatType>>> {
    if r.string()? != "minecraft:chat_type" {
        return Ok(None);
    }
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
    Ok(Some(types))
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
        c::REGISTRY_DATA => {
            if let Ok(Some(found)) = chat_types(r) {
                *types = found;
            }
        }
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
        c::RESPAWN => {
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
            let message = r.nbt().map(|m| text::line(&m).text).unwrap_or_default();
            link.dead = Some(if message.is_empty() { died() } else { message });
            shared.settle(link);
        }
        c::SYSTEM_CHAT => system_chat(shared, r)?,
        c::PLAYER_CHAT => player_chat(shared, link, r, types)?,
        c::DISGUISED_CHAT => disguised_chat(shared, r, types)?,
        c::CHUNK_BATCH_FINISHED => write(link, s::CHUNK_BATCH_RECEIVED, Out::default().f32(CHUNKS_PER_TICK))?,
        c::START_CONFIGURATION => {
            write(link, s::CONFIGURATION_ACKNOWLEDGED, &mut Out::default())?;
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

fn system_chat(shared: &Shared, r: &mut In) -> Result<()> {
    let line = text::line(&r.nbt()?);
    if !r.bool()? && !line.text.is_empty() {
        shared.emit(FromMod::Chat { kind: ChatKind::System, from: None, text: line.text, runs: line.runs });
    }
    Ok(())
}

fn disguised_chat(shared: &Shared, r: &mut In, types: &[ChatType]) -> Result<()> {
    let message = r.nbt()?;
    let text = text::line(&message).text;
    if let Some(message) = bound(r, types, &message, text)? {
        shared.emit(message);
    }
    Ok(())
}

fn player_chat(shared: &Shared, link: &mut Link, r: &mut In, types: &[ChatType]) -> Result<()> {
    let _global = r.varint()?;
    let _sender = r.uuid()?;
    let _index = r.varint()?;
    let signed = r.bool()?;
    if signed {
        link.unacked += 1;
        if link.unacked > ACK_AFTER {
            write(link, play::serverbound::CHAT_ACK, Out::default().varint(link.unacked))?;
            link.unacked = 0;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        let spec = Spec { key: "k".into(), address: "127.0.0.1:1".into(), name: "Dev".into(), uuid: Uuid::nil(), session: None, version: "26.3".into(), packs: Packs::Accept, proxy: None, conduct: Vec::new() };
        Shared::new(&spec, std::sync::mpsc::channel().0)
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
}
