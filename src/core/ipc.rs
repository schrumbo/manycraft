//! Link between the manager and the companion mod in each instance: newline-delimited JSON over local TCP,
//! plus a preview file the mod writes and the manager reads.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::batch::Background;
use super::place::Mode;

const PREVIEW_MAGIC: u32 = 0x4D43_5056;
const PREVIEW_HEADER: usize = 16;
const MAX_PREVIEW_SIDE: u32 = 4096;
const MAX_LINE: u64 = 64 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// What an instance is doing, as reported by its companion mod.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    Menu,
    Connecting,
    Queue,
    Playing,
    Disconnected,
    Dead,
}

/// Why a connection dropped, as the mod's classifier read the disconnect screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectCause {
    Banned,
    Whitelist,
    ServerFull,
    Kicked,
    ServerDown,
    Auth,
    Duplicate,
    #[serde(other)]
    Unknown,
}

/// Kind of a chat line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    System,
    Player,
    Whisper,
}

/// Hotkey actions the mod forwards from inside the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    Next,
    Prev,
    NextAttention,
    Grid,
}

/// Messages from the mod to the manager.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromMod {
    Hello { instance: String, token: String, pid: u32, mod_version: String },
    State { kind: StateKind, server: Option<String>, reason: Option<String>, #[serde(default)] cause: Option<DisconnectCause> },
    Chat { kind: ChatKind, from: Option<String>, text: String },
    Preview { seq: u32, w: u32, h: u32 },
    Hotkey { action: HotkeyAction },
    Window { visible: bool, focused: bool },
    /// The `options.txt` keys this save changed, with the player's values, never the background ones.
    Options { values: BTreeMap<String, String> },
    /// The game saved its `servers.dat`.
    Servers,
}

/// Messages from the manager to the mod.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToMod {
    /// Shows the game with its client area at `x`, `y`, `w`, `h`, then maximized or fullscreen as `mode` says.
    Show { x: i32, y: i32, w: u32, h: u32, #[serde(default, skip_serializing_if = "Mode::is_normal")] mode: Mode },
    Hide,
    Profile { fps: u32, render_distance: u32, preview_interval_ms: u32, preview_width: u32 },
    ChatSend { text: String },
    Reconnect,
    Respawn,
    Quit,
    Hotkeys { next: String, prev: String, next_attention: String, grid: String },
    /// Who the game is: window title, window icon (a PNG path) and badge colour as `0xRRGGBB`.
    Identity { name: String, batch: String, rgb: u32, icon: Option<String> },
    /// Whether the name badge shows in the game's corner.
    Badge { visible: bool },
    /// What the game may cost while hidden.
    Level(Level),
    /// `options.txt` keys another game of the batch changed; the game applies and saves them without reporting back.
    Options { values: BTreeMap<String, String> },
    /// Another game of the batch changed the server list; an open server list reloads it.
    Servers,
}

/// A background level as the mod reads it: the level name, plus frames per second and sight for `custom`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum Level {
    Saving,
    Balanced,
    Full,
    Custom { fps: u8, render_distance: u8 },
}

impl From<Background> for Level {
    fn from(b: Background) -> Self {
        match b.clamped() {
            Background::Saving => Self::Saving,
            Background::Balanced => Self::Balanced,
            Background::Full => Self::Full,
            Background::Custom { fps, sight } => Self::Custom { fps, render_distance: sight },
        }
    }
}

/// What the manager learns about an instance.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Connected { instance: String, pid: u32, mod_version: String },
    Message { instance: String, message: FromMod },
    Disconnected { instance: String },
    /// Another start of Manycraft asks this one to come to the front.
    Reopen,
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Opening {
    Hello { instance: String, token: String, pid: u32, mod_version: String },
    Reopen,
}

struct Link {
    id: u64,
    stream: Arc<TcpStream>,
    writing: Mutex<()>,
}

#[derive(Default)]
struct Shared {
    tokens: HashMap<String, String>,
    links: HashMap<String, Arc<Link>>,
}

/// Listens on a local port for companion mods; one connection per instance.
#[derive(Clone)]
pub struct Server {
    port: u16,
    shared: Arc<Mutex<Shared>>,
    next_id: Arc<AtomicU64>,
}

impl Server {
    /// Binds to an ephemeral port on 127.0.0.1 and starts accepting. Events arrive on the returned receiver.
    pub fn start() -> Result<(Server, Receiver<Event>)> {
        Self::start_on(None)
    }

    /// Like [`Server::start`], but takes `port` when it is free, so games launched by an earlier manager find this one.
    pub fn start_on(port: Option<u16>) -> Result<(Server, Receiver<Event>)> {
        let listener = match port.and_then(|p| TcpListener::bind(("127.0.0.1", p)).ok()) {
            Some(listener) => listener,
            None => TcpListener::bind("127.0.0.1:0").context("cannot open the local instance port")?,
        };
        let port = listener.local_addr()?.port();
        let server = Server { port, shared: Arc::default(), next_id: Arc::default() };
        let (tx, rx) = channel();
        let accepting = server.clone();
        std::thread::Builder::new().name("ipc-accept".into()).spawn(move || {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let conn = accepting.clone();
                        let tx = tx.clone();
                        let _ = std::thread::Builder::new().name("ipc-conn".into()).spawn(move || conn.serve(stream, &tx));
                    }
                    Err(_) => std::thread::sleep(ACCEPT_BACKOFF),
                }
            }
        })?;
        Ok((server, rx))
    }

    /// Address the mod connects to, for `-Dmanycraft.ipc`.
    pub fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Allows `instance` to connect and returns the secret it must present, passed in the `MANYCRAFT_TOKEN` environment variable.
    pub fn register(&self, instance: &str) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.lock().tokens.insert(instance.to_owned(), token.clone());
        token
    }

    /// Allows `instance` to connect again with the `token` it was launched with.
    pub fn register_with(&self, instance: &str, token: &str) {
        self.lock().tokens.insert(instance.to_owned(), token.to_owned());
    }

    /// The port this server listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Forgets `instance` and closes its connection.
    pub fn unregister(&self, instance: &str) {
        let mut shared = self.lock();
        shared.tokens.remove(instance);
        if let Some(link) = shared.links.remove(instance) {
            let _ = link.stream.shutdown(Shutdown::Both);
        }
    }

    /// Whether `instance` is connected right now.
    pub fn is_connected(&self, instance: &str) -> bool {
        self.lock().links.contains_key(instance)
    }

    /// The instances connected right now.
    pub fn connected(&self) -> Vec<String> {
        self.lock().links.keys().cloned().collect()
    }

    /// Sends `message` to `instance`. Fails when it is not connected; a link that fails to take a message is closed.
    pub fn send(&self, instance: &str, message: &ToMod) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let link = self.lock().links.get(instance).cloned().with_context(|| format!("instance {instance} is not connected"))?;
        let written = {
            let _turn = link.writing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (&*link.stream).write_all(&line)
        };
        if written.is_err() {
            let _ = link.stream.shutdown(Shutdown::Both);
        }
        written.with_context(|| format!("instance {instance} is not reachable"))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn serve(&self, stream: TcpStream, tx: &Sender<Event>) {
        if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() || stream.set_read_timeout(Some(HELLO_TIMEOUT)).is_err() {
            return;
        }
        let stream = Arc::new(stream);
        let mut lines = BufReader::new(StreamReader(Arc::clone(&stream)).take(MAX_LINE));
        let mut line = String::new();
        let (instance, token, pid, mod_version) = match read_message(&mut lines, &mut line) {
            Some(Opening::Hello { instance, token, pid, mod_version }) => (instance, token, pid, mod_version),
            Some(Opening::Reopen) => {
                let _ = tx.send(Event::Reopen);
                return;
            }
            None => return,
        };
        if stream.set_read_timeout(None).is_err() {
            return;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut shared = self.lock();
            if shared.tokens.get(&instance) != Some(&token) {
                return;
            }
            let link = Arc::new(Link { id, stream: Arc::clone(&stream), writing: Mutex::new(()) });
            if let Some(old) = shared.links.insert(instance.clone(), link) {
                let _ = old.stream.shutdown(Shutdown::Both);
            }
            let _ = tx.send(Event::Connected { instance: instance.clone(), pid, mod_version });
        }
        while let Some(message) = read_message::<FromMod>(&mut lines, &mut line) {
            if matches!(message, FromMod::Hello { .. }) {
                continue;
            }
            let shared = self.lock();
            if shared.links.get(&instance).is_none_or(|l| l.id != id) || tx.send(Event::Message { instance: instance.clone(), message }).is_err() {
                break;
            }
        }
        let mut shared = self.lock();
        if shared.links.get(&instance).is_some_and(|l| l.id == id) {
            shared.links.remove(&instance);
            let _ = tx.send(Event::Disconnected { instance });
        }
        let _ = stream.shutdown(Shutdown::Both);
    }
}

struct StreamReader(Arc<TcpStream>);

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&*self.0).read(buf)
    }
}

fn read_message<T: serde::de::DeserializeOwned>(lines: &mut BufReader<std::io::Take<StreamReader>>, line: &mut String) -> Option<T> {
    loop {
        line.clear();
        lines.get_mut().set_limit(MAX_LINE);
        match lines.read_line(line) {
            Ok(0) | Err(_) => return None,
            Ok(_) if !line.ends_with('\n') => return None,
            Ok(_) => {}
        }
        if let Ok(message) = serde_json::from_str(line.trim_end()) {
            return Some(message);
        }
    }
}

/// One preview frame: RGBA8 rows, top-down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub seq: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Reads the preview file the mod writes. Returns `None` while the file is missing, incomplete or being rewritten.
pub fn read_preview(path: &Path) -> Option<Preview> {
    let bytes = std::fs::read(path).ok()?;
    let preview = parse_preview(&bytes)?;
    let mut header = [0; 8];
    std::fs::File::open(path).ok()?.read_exact(&mut header).ok()?;
    (u32::from_le_bytes(header[4..8].try_into().ok()?) == preview.seq).then_some(preview)
}

/// Remembers the last frame shown per instance, so a frame that repeats the last one is not drawn again.
#[derive(Default)]
pub struct PreviewChanges(HashMap<String, (u32, u64)>);

impl PreviewChanges {
    /// Whether `preview` differs from the last frame passed for `key`. A `seq` that did not rise means a new game process or a read again and counts as a change.
    pub fn changed(&mut self, key: &str, preview: &Preview) -> bool {
        let mut hasher = std::hash::DefaultHasher::new();
        (preview.width, preview.height, &preview.rgba).hash(&mut hasher);
        let print = hasher.finish();
        let fresh = self.0.get(key).is_none_or(|&(seq, last)| last != print || preview.seq <= seq);
        self.0.insert(key.to_owned(), (preview.seq, print));
        fresh
    }
}

fn parse_preview(bytes: &[u8]) -> Option<Preview> {
    let word = |i: usize| bytes.get(i..i + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    if word(0)? != PREVIEW_MAGIC {
        return None;
    }
    let (seq, width, height) = (word(4)?, word(8)?, word(12)?);
    if seq % 2 == 1 || width == 0 || height == 0 || width > MAX_PREVIEW_SIDE || height > MAX_PREVIEW_SIDE {
        return None;
    }
    let len = width as usize * height as usize * 4;
    let rgba = bytes.get(PREVIEW_HEADER..PREVIEW_HEADER + len)?.to_vec();
    Some(Preview { seq, width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect(server: &Server, hello: &FromMod) -> (TcpStream, BufReader<TcpStream>) {
        let mut stream = TcpStream::connect(server.address()).unwrap();
        let mut line = serde_json::to_string(hello).unwrap();
        line.push('\n');
        stream.write_all(line.as_bytes()).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        (stream, reader)
    }

    fn hello(instance: &str, token: &str) -> FromMod {
        FromMod::Hello { instance: instance.into(), token: token.into(), pid: 7, mod_version: "0.1.0".into() }
    }

    #[test]
    fn wire_format_matches_the_mod_protocol() {
        let line = r#"{"t":"state","kind":"disconnected","server":"127.0.0.1:25565","reason":"Kicked"}"#;
        assert_eq!(serde_json::from_str::<FromMod>(line).unwrap(), FromMod::State { kind: StateKind::Disconnected, server: Some("127.0.0.1:25565".into()), reason: Some("Kicked".into()), cause: None });
        let classified = r#"{"t":"state","kind":"disconnected","server":null,"reason":"x","cause":"server_full"}"#;
        assert!(matches!(serde_json::from_str::<FromMod>(classified).unwrap(), FromMod::State { cause: Some(DisconnectCause::ServerFull), .. }));
        let newer = r#"{"t":"state","kind":"disconnected","server":null,"reason":"x","cause":"something_new"}"#;
        assert!(matches!(serde_json::from_str::<FromMod>(newer).unwrap(), FromMod::State { cause: Some(DisconnectCause::Unknown), .. }));
        let none = r#"{"t":"state","kind":"menu","server":null,"reason":null,"cause":null}"#;
        assert!(matches!(serde_json::from_str::<FromMod>(none).unwrap(), FromMod::State { cause: None, .. }));
        let chat = r#"{"t":"chat","kind":"whisper","from":"Alex","text":"hi"}"#;
        assert!(matches!(serde_json::from_str::<FromMod>(chat).unwrap(), FromMod::Chat { kind: ChatKind::Whisper, .. }));
        let hotkey = r#"{"t":"hotkey","action":"next_attention"}"#;
        assert_eq!(serde_json::from_str::<FromMod>(hotkey).unwrap(), FromMod::Hotkey { action: HotkeyAction::NextAttention });
        assert_eq!(serde_json::to_string(&ToMod::Hide).unwrap(), r#"{"t":"hide"}"#);
        assert_eq!(serde_json::to_string(&ToMod::Respawn).unwrap(), r#"{"t":"respawn"}"#);
        assert_eq!(serde_json::to_string(&ToMod::ChatSend { text: "/home".into() }).unwrap(), r#"{"t":"chat_send","text":"/home"}"#);
        assert_eq!(serde_json::to_string(&ToMod::Show { x: -5, y: 10, w: 800, h: 600, mode: Mode::Normal }).unwrap(), r#"{"t":"show","x":-5,"y":10,"w":800,"h":600}"#);
        assert_eq!(serde_json::to_string(&ToMod::Show { x: 0, y: 0, w: 800, h: 600, mode: Mode::Fullscreen }).unwrap(), r#"{"t":"show","x":0,"y":0,"w":800,"h":600,"mode":"fullscreen"}"#);
        let identity = ToMod::Identity { name: "Alt".into(), batch: "Farm".into(), rgb: 0x12_34_56, icon: Some("icon.png".into()) };
        assert_eq!(serde_json::to_string(&identity).unwrap(), r#"{"t":"identity","name":"Alt","batch":"Farm","rgb":1193046,"icon":"icon.png"}"#);
        let bare = ToMod::Identity { name: "Alt".into(), batch: "Farm".into(), rgb: 0, icon: None };
        assert_eq!(serde_json::to_string(&bare).unwrap(), r#"{"t":"identity","name":"Alt","batch":"Farm","rgb":0,"icon":null}"#);
        assert_eq!(serde_json::to_string(&ToMod::Badge { visible: false }).unwrap(), r#"{"t":"badge","visible":false}"#);
        let level = |b: Background| serde_json::to_string(&ToMod::Level(b.into())).unwrap();
        assert_eq!(level(Background::Saving), r#"{"t":"level","level":"saving"}"#);
        assert_eq!(level(Background::Balanced), r#"{"t":"level","level":"balanced"}"#);
        assert_eq!(level(Background::Full), r#"{"t":"level","level":"full"}"#);
        assert_eq!(level(Background::Custom { fps: 10, sight: 6 }), r#"{"t":"level","level":"custom","fps":10,"render_distance":6}"#);
        assert_eq!(level(Background::Custom { fps: 0, sight: 99 }), r#"{"t":"level","level":"custom","fps":1,"render_distance":32}"#);
        let options = r#"{"t":"options","values":{"guiScale":"3","key_key.jump":"key.keyboard.j"}}"#;
        let values = BTreeMap::from([("guiScale".to_owned(), "3".to_owned()), ("key_key.jump".to_owned(), "key.keyboard.j".to_owned())]);
        assert_eq!(serde_json::from_str::<FromMod>(options).unwrap(), FromMod::Options { values: values.clone() });
        assert_eq!(serde_json::to_string(&ToMod::Options { values }).unwrap(), options);
        assert_eq!(serde_json::from_str::<FromMod>(r#"{"t":"servers"}"#).unwrap(), FromMod::Servers);
        assert_eq!(serde_json::to_string(&ToMod::Servers).unwrap(), r#"{"t":"servers"}"#);
        let custom = r#"{"t":"level","level":"custom","fps":10,"render_distance":6}"#;
        assert_eq!(serde_json::from_str::<ToMod>(custom).unwrap(), ToMod::Level(Level::Custom { fps: 10, render_distance: 6 }));
    }

    #[test]
    fn registered_mod_connects_reports_and_receives() {
        let (server, events) = Server::start().unwrap();
        let token = server.register("a");
        let (mut stream, mut reader) = connect(&server, &hello("a", &token));
        assert!(matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Connected { instance, pid: 7, .. } if instance == "a"));
        stream.write_all(b"{\"t\":\"window\",\"visible\":false,\"focused\":false}\n").unwrap();
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(event, Event::Message { instance: "a".into(), message: FromMod::Window { visible: false, focused: false } });
        server.send("a", &ToMod::Reconnect).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"t\":\"reconnect\"}\n");
        drop(stream);
        drop(reader);
        assert_eq!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Disconnected { instance: "a".into() });
        assert!(!server.is_connected("a"));
    }

    #[test]
    fn silent_connection_is_dropped_after_the_hello_timeout() {
        let (server, _events) = Server::start().unwrap();
        let stream = TcpStream::connect(server.address()).unwrap();
        stream.set_read_timeout(Some(HELLO_TIMEOUT * 2)).unwrap();
        let mut buf = [0; 1];
        assert_eq!((&stream).read(&mut buf).unwrap_or(0), 0);
    }

    #[test]
    fn failed_send_closes_the_link() {
        let (server, events) = Server::start().unwrap();
        let token = server.register("a");
        let (stream, reader) = connect(&server, &hello("a", &token));
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(stream);
        drop(reader);
        assert_eq!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Disconnected { instance: "a".into() });
        assert!(server.send("a", &ToMod::Hide).is_err());
    }

    #[test]
    fn wrong_token_is_refused() {
        let (server, events) = Server::start().unwrap();
        server.register("a");
        let (_stream, mut reader) = connect(&server, &hello("a", "guess"));
        let mut line = String::new();
        assert_eq!(reader.read_line(&mut line).unwrap(), 0);
        assert!(events.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(server.send("a", &ToMod::Hide).is_err());
    }

    #[test]
    fn garbage_lines_are_skipped() {
        let (server, events) = Server::start().unwrap();
        let token = server.register("a");
        let (mut stream, _reader) = connect(&server, &hello("a", &token));
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        stream.write_all(b"not json\n{\"t\":\"unknown\"}\n{\"t\":\"preview\",\"seq\":3,\"w\":2,\"h\":1}\n").unwrap();
        assert_eq!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Message { instance: "a".into(), message: FromMod::Preview { seq: 3, w: 2, h: 1 } });
    }

    #[test]
    fn reconnect_replaces_the_old_link_without_a_disconnect_event() {
        let (server, events) = Server::start().unwrap();
        let token = server.register("a");
        let first = connect(&server, &hello("a", &token));
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        let _second = connect(&server, &hello("a", &token));
        assert!(matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Connected { .. }));
        drop(first);
        assert!(events.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(server.is_connected("a"));
    }

    #[test]
    fn a_reopen_request_is_passed_on_without_a_token() {
        let (server, events) = Server::start().unwrap();
        let mut stream = TcpStream::connect(server.address()).unwrap();
        stream.write_all(b"{\"t\":\"reopen\"}
").unwrap();
        assert_eq!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Reopen);
        assert!(server.connected().is_empty());
    }

    #[test]
    fn a_restarted_server_takes_the_old_port_and_token() {
        let (first, _first_events) = Server::start().unwrap();
        let token = first.register("a");
        let (busy, _busy_events) = Server::start_on(Some(first.port())).unwrap();
        assert_ne!(busy.port(), first.port(), "a taken port falls back to a free one");
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (server, events) = Server::start_on(Some(free)).unwrap();
        assert_eq!(server.port(), free);
        server.register_with("a", &token);
        let _link = connect(&server, &hello("a", &token));
        assert!(matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Connected { instance, .. } if instance == "a"));
    }

    #[test]
    fn preview_file_is_parsed_and_validated() {
        let mut bytes = Vec::new();
        for word in [PREVIEW_MAGIC, 8, 2, 1] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.bin");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_preview(&path).unwrap(), Preview { seq: 8, width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] });
        assert!(parse_preview(&bytes[..bytes.len() - 1]).is_none());
        let mut huge = bytes.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_preview(&huge).is_none());
        assert!(parse_preview(&[0; 16]).is_none());
        let mut writing = bytes.clone();
        writing[4..8].copy_from_slice(&9u32.to_le_bytes());
        assert!(parse_preview(&writing).is_none());
        assert!(read_preview(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn a_repeated_frame_is_no_change_until_the_game_restarts() {
        let frame = |seq, rgba: &[u8]| Preview { seq, width: 1, height: 1, rgba: rgba.to_vec() };
        let mut changes = PreviewChanges::default();
        assert!(changes.changed("a", &frame(2, &[1, 2, 3, 4])));
        assert!(!changes.changed("a", &frame(4, &[1, 2, 3, 4])));
        assert!(changes.changed("a", &frame(4, &[1, 2, 3, 4])));
        assert!(changes.changed("b", &frame(4, &[1, 2, 3, 4])));
        assert!(changes.changed("a", &frame(6, &[1, 2, 3, 5])));
        assert!(!changes.changed("a", &frame(8, &[1, 2, 3, 5])));
        assert!(changes.changed("a", &frame(2, &[1, 2, 3, 5])));
        assert!(!changes.changed("a", &frame(4, &[1, 2, 3, 5])));
        let wide = Preview { seq: 6, width: 2, height: 1, rgba: vec![1, 2, 3, 5] };
        assert!(changes.changed("a", &wide));
    }
}
