//! Shows what runs in Discord's "Playing" status. Discord is reached over its local IPC pipe or socket, on a thread of its own, so the UI never waits for it. Nothing is sent while no game runs, and a Discord that starts later is picked up silently.

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The id of the Discord application named Manycraft; while it is empty the status stays off.
const APP_ID: &str = "";

const RETRY: Duration = Duration::from_secs(15);
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const MAX_FRAME: usize = 1 << 16;
const MAX_TEXT: usize = 100;

/// One running game, reduced to what the status needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    /// The batch it plays in; empty when it plays an instance alone.
    pub batch_id: String,
    pub batch_name: String,
    pub instance_id: String,
    pub instance_name: String,
    pub version: String,
    pub loader: String,
}

/// The two lines Discord shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    pub details: String,
    pub state: Option<String>,
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text.to_owned();
    }
    text.chars().take(MAX_TEXT - 1).chain(Some('…')).collect()
}

fn version_line(game: &Running) -> Option<String> {
    let parts: Vec<&str> = [game.version.as_str(), game.loader.as_str()].into_iter().filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn accounts(n: usize) -> String {
    format!("{n} {}", if n == 1 { "account" } else { "accounts" })
}

/// What to show for the running games: nothing for none, the batch for one batch, a count for several batches, and the instance for games played alone. Batches win over single instances.
pub fn choose(games: &[Running]) -> Option<Presence> {
    let mut batches: Vec<(&str, &Running, usize)> = Vec::new();
    for game in games.iter().filter(|g| !g.batch_id.is_empty()) {
        match batches.iter_mut().find(|(id, _, _)| *id == game.batch_id) {
            Some(entry) => entry.2 += 1,
            None => batches.push((&game.batch_id, game, 1)),
        }
    }
    match batches.as_slice() {
        [] => {}
        [(_, game, n)] => return Some(Presence { details: clip(&format!("{} gooning in {}", accounts(*n), game.batch_name)), state: version_line(game) }),
        many => {
            let total = many.iter().map(|(_, _, n)| n).sum();
            return Some(Presence { details: format!("{} gooning in {} batches", accounts(total), many.len()), state: None });
        }
    }
    let mut solo: Vec<&Running> = Vec::new();
    for game in games {
        if !solo.iter().any(|s| s.instance_id == game.instance_id) {
            solo.push(game);
        }
    }
    match solo.as_slice() {
        [] => None,
        [game] => Some(Presence { details: clip(&format!("Playing {}", game.instance_name)), state: version_line(game) }),
        many => Some(Presence { details: format!("Playing {} instances", many.len()), state: None }),
    }
}

fn frame(op: u32, body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

fn read_frame(stream: &mut dyn Read) -> io::Result<(u32, serde_json::Value)> {
    let mut head = [0u8; 8];
    stream.read_exact(&mut head)?;
    let op = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len > MAX_FRAME {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok((op, serde_json::from_slice(&body).unwrap_or_default()))
}

trait Wire: Read + Write + Send {}
impl<T: Read + Write + Send> Wire for T {}
type Stream = Box<dyn Wire>;

fn handshake(stream: &mut dyn Wire, app_id: &str) -> io::Result<()> {
    stream.write_all(&frame(OP_HANDSHAKE, &serde_json::json!({ "v": 1, "client_id": app_id }).to_string()))?;
    let (op, body) = read_frame(stream)?;
    if op == OP_FRAME && body["evt"] == "READY" { Ok(()) } else { Err(io::ErrorKind::ConnectionRefused.into()) }
}

fn activity(presence: Option<&Presence>, start: i64, pid: u32) -> String {
    let activity = presence.map(|p| {
        let mut a = serde_json::json!({ "details": p.details, "timestamps": { "start": start } });
        if let Some(state) = &p.state {
            a["state"] = state.clone().into();
        }
        a
    });
    serde_json::json!({ "cmd": "SET_ACTIVITY", "args": { "pid": pid, "activity": activity }, "nonce": uuid::Uuid::new_v4().to_string() }).to_string()
}

fn send(stream: &mut dyn Wire, presence: Option<&Presence>, start: i64, pid: u32) -> io::Result<()> {
    stream.write_all(&frame(OP_FRAME, &activity(presence, start, pid)))?;
    let (op, _) = read_frame(stream)?;
    if op == OP_FRAME { Ok(()) } else { Err(io::ErrorKind::ConnectionAborted.into()) }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn work(rx: Receiver<Option<Presence>>, mut connect: impl FnMut() -> Option<Stream>, retry: Duration, app_id: &str) {
    let pid = std::process::id();
    let mut wanted: Option<Presence> = None;
    let mut start = 0;
    let mut link: Option<Stream> = None;
    loop {
        let message = if wanted.is_some() { rx.recv_timeout(retry) } else { rx.recv().map_err(|_| RecvTimeoutError::Disconnected) };
        match message {
            Ok(mut next) => {
                let mut paused = next.is_none();
                while let Ok(newer) = rx.try_recv() {
                    paused |= newer.is_none();
                    next = newer;
                }
                if next.is_some() && (wanted.is_none() || paused) {
                    start = now();
                }
                wanted = next;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        match &wanted {
            None => {
                if let Some(mut stream) = link.take() {
                    let _ = send(&mut *stream, None, 0, pid);
                }
            }
            Some(presence) => {
                if link.is_none() {
                    link = connect().and_then(|mut s| handshake(&mut *s, app_id).ok().map(|()| s));
                }
                if let Some(stream) = link.as_mut()
                    && send(&mut **stream, Some(presence), start, pid).is_err()
                {
                    link = None;
                }
            }
        }
    }
}

#[cfg(windows)]
fn open(n: u32) -> Option<Stream> {
    let file = std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{n}")).ok()?;
    Some(Box::new(file))
}

#[cfg(unix)]
fn open(n: u32) -> Option<Stream> {
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    let mut dirs: Vec<PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"].iter().filter_map(|k| std::env::var_os(k)).map(PathBuf::from).collect();
    dirs.push(PathBuf::from("/tmp"));
    for dir in dirs {
        for sub in ["", "snap.discord", "app/com.discordapp.Discord", "app/com.discordapp.DiscordCanary"] {
            if let Ok(stream) = UnixStream::connect(dir.join(sub).join(format!("discord-ipc-{n}"))) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                return Some(Box::new(stream));
            }
        }
    }
    None
}

fn connect() -> Option<Stream> {
    (0..10).find_map(open)
}

/// Whether this build has a Discord application to show a status for.
pub const fn available() -> bool {
    !APP_ID.is_empty()
}

static LAST: Mutex<Option<Presence>> = Mutex::new(None);
static WORKER: OnceLock<Sender<Option<Presence>>> = OnceLock::new();

/// Shows `presence` in Discord, or clears the status for none. Cheap to call after every change: it only acts when the presence changed, and the work runs on its own thread.
pub fn set(presence: Option<Presence>) {
    if !available() {
        return;
    }
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *last == presence {
        return;
    }
    let worker = match WORKER.get() {
        Some(worker) => worker,
        None if presence.is_none() => return,
        None => {
            let (send, receive) = mpsc::channel();
            if std::thread::Builder::new().name("discord".into()).spawn(move || work(receive, connect, RETRY, APP_ID)).is_err() {
                return;
            }
            WORKER.get_or_init(|| send)
        }
    };
    let _ = worker.send(presence.clone());
    *last = presence;
}

#[cfg(test)]
mod tests {
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::channel;

    use super::*;

    fn game(batch: &str, instance: &str) -> Running {
        let name = |id: &str| if id.is_empty() { String::new() } else { id.to_uppercase() };
        Running { batch_id: batch.into(), batch_name: name(batch), instance_id: instance.into(), instance_name: name(instance), version: "1.21.4".into(), loader: "Fabric".into() }
    }

    fn shown(games: &[Running]) -> Option<(String, Option<String>)> {
        choose(games).map(|p| (p.details, p.state))
    }

    #[test]
    fn nothing_runs_nothing_shows() {
        assert_eq!(choose(&[]), None);
    }

    #[test]
    fn a_batch_counts_its_accounts() {
        let five: Vec<Running> = (0..5).map(|_| game("farm", "i")).collect();
        assert_eq!(shown(&five), Some(("5 accounts gooning in FARM".into(), Some("1.21.4 · Fabric".into()))));
        assert_eq!(shown(&five[..1]).map(|p| p.0), Some("1 account gooning in FARM".into()));
    }

    #[test]
    fn several_batches_combine() {
        let games = [game("a", "i"), game("a", "i"), game("b", "j")];
        assert_eq!(shown(&games), Some(("3 accounts gooning in 2 batches".into(), None)));
        assert_eq!(shown(&[game("a", "i"), game("b", "j")]).map(|p| p.0), Some("2 accounts gooning in 2 batches".into()));
    }

    #[test]
    fn one_instance_alone_shows_name_version_and_loader() {
        assert_eq!(shown(&[game("", "survival")]), Some(("Playing SURVIVAL".into(), Some("1.21.4 · Fabric".into()))));
        let mut vanilla = game("", "x");
        vanilla.loader.clear();
        assert_eq!(shown(&[vanilla]).and_then(|p| p.1), Some("1.21.4".into()));
        let mut bare = game("", "x");
        bare.loader.clear();
        bare.version.clear();
        assert_eq!(shown(&[bare]).and_then(|p| p.1), None);
    }

    #[test]
    fn several_instances_alone_count() {
        assert_eq!(shown(&[game("", "a"), game("", "b")]), Some(("Playing 2 instances".into(), None)));
    }

    #[test]
    fn a_batch_wins_over_an_instance_played_alone() {
        assert_eq!(shown(&[game("", "solo"), game("farm", "i")]).map(|p| p.0), Some("1 account gooning in FARM".into()));
    }

    #[test]
    fn long_names_are_cut_and_every_line_fits() {
        let mut g = game("a", "i");
        g.batch_name = "x".repeat(300);
        let details = choose(&[g]).map(|p| p.details).unwrap_or_default();
        assert!(details.chars().count() <= 128, "{details}");
        assert_eq!(clip(&"ä".repeat(MAX_TEXT)).chars().count(), MAX_TEXT);
        assert!(clip(&"ä".repeat(MAX_TEXT + 1)).ends_with('…'));
    }

    fn serve(mut stream: TcpStream, seen: Sender<serde_json::Value>) {
        let Ok((_, hello)) = read_frame(&mut stream) else { return };
        let _ = seen.send(hello);
        let _ = stream.write_all(&frame(OP_FRAME, r#"{"cmd":"DISPATCH","evt":"READY"}"#));
        while let Ok((_, body)) = read_frame(&mut stream) {
            let _ = seen.send(body);
            if stream.write_all(&frame(OP_FRAME, r#"{"cmd":"SET_ACTIVITY","evt":null}"#)).is_err() {
                return;
            }
        }
        let _ = seen.send(serde_json::json!("closed"));
    }

    #[test]
    fn the_worker_connects_late_shows_updates_and_clears() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen, heard) = channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = seen.clone();
                std::thread::spawn(move || serve(stream, seen));
            }
        });
        let (tx, rx) = channel();
        let mut refused = 1;
        let worker = std::thread::spawn(move || {
            work(
                rx,
                move || {
                    if refused > 0 {
                        refused -= 1;
                        return None;
                    }
                    TcpStream::connect(addr).ok().map(|s| Box::new(s) as Stream)
                },
                Duration::from_millis(20),
                "1234",
            )
        });
        let wait = || heard.recv_timeout(Duration::from_secs(5)).expect("the fake Discord heard nothing");
        let first = Presence { details: "Playing A".into(), state: Some("1.21.4 · Fabric".into()) };
        tx.send(Some(first)).unwrap();
        let hello = wait();
        assert_eq!(hello["client_id"], "1234");
        assert_eq!(hello["v"], 1);
        let set = wait();
        assert_eq!(set["cmd"], "SET_ACTIVITY");
        assert_eq!(set["args"]["pid"], std::process::id());
        assert_eq!(set["args"]["activity"]["details"], "Playing A");
        assert_eq!(set["args"]["activity"]["state"], "1.21.4 · Fabric");
        let started = set["args"]["activity"]["timestamps"]["start"].as_i64().unwrap();
        assert!((now() - started).abs() < 60);
        tx.send(Some(Presence { details: "2 accounts gooning in B".into(), state: None })).unwrap();
        let next = wait();
        assert_eq!(next["args"]["activity"]["details"], "2 accounts gooning in B");
        assert!(next["args"]["activity"].get("state").is_none());
        assert_eq!(next["args"]["activity"]["timestamps"]["start"].as_i64(), Some(started), "the timer runs since the first game");
        tx.send(None).unwrap();
        let cleared = wait();
        assert!(cleared["args"]["activity"].is_null());
        assert_eq!(wait(), "closed");
        drop(tx);
        worker.join().unwrap();
    }

    #[test]
    fn a_refusing_discord_is_dropped_and_asked_again() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = read_frame(&mut stream);
                let _ = stream.write_all(&frame(2, r#"{"code":4000,"message":"Invalid Client ID"}"#));
            }
        });
        let (tx, rx) = channel();
        let (tried, counted) = channel();
        let worker = std::thread::spawn(move || {
            work(
                rx,
                move || {
                    let _ = tried.send(());
                    TcpStream::connect(addr).ok().map(|s| Box::new(s) as Stream)
                },
                Duration::from_millis(10),
                "bad",
            )
        });
        tx.send(Some(Presence { details: "Playing A".into(), state: None })).unwrap();
        for _ in 0..3 {
            counted.recv_timeout(Duration::from_secs(5)).expect("no retry");
        }
        drop(tx);
        worker.join().unwrap();
    }

    #[test]
    fn a_discord_that_restarted_gets_the_status_again() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = read_frame(&mut stream);
                let _ = stream.write_all(&frame(OP_FRAME, r#"{"cmd":"DISPATCH","evt":"READY"}"#));
                if read_frame(&mut stream).is_ok() {
                    let _ = stream.write_all(&frame(OP_FRAME, r#"{"evt":null}"#));
                }
            }
        });
        let (tx, rx) = channel();
        let (tried, counted) = channel();
        let worker = std::thread::spawn(move || {
            work(
                rx,
                move || {
                    let _ = tried.send(());
                    TcpStream::connect(addr).ok().map(|s| Box::new(s) as Stream)
                },
                Duration::from_millis(10),
                "1",
            )
        });
        tx.send(Some(Presence { details: "Playing A".into(), state: None })).unwrap();
        for _ in 0..3 {
            counted.recv_timeout(Duration::from_secs(5)).expect("the dead link was not noticed");
        }
        drop(tx);
        worker.join().unwrap();
    }
}
