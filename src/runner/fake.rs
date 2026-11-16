//! A pretend fleet for driven runs: instances from the fixture accounts in a realistic mix of phases, with generated previews,
//! so agents can test the grid without starting Minecraft or touching the keychain.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::backend::{Backend, Pixels, Update};
use crate::app::model::Phase;
use crate::core::account::Account;
use crate::core::chat_text::Run;
use crate::core::fleet::Target;
use crate::core::ipc::{ChatKind, DisconnectCause, Event, FromMod, Preview, PreviewChanges, StateKind, ToMod};

const PREVIEW_W: u32 = 192;
const PREVIEW_H: u32 = 108;
const PREVIEW_EVERY: Duration = Duration::from_secs(3);
const TICK: Duration = Duration::from_millis(250);
const STAGGER: Duration = Duration::from_millis(400);
const SERVER: &str = "play.example.net";
const CHAT_EVERY: Duration = Duration::from_millis(1500);
const DIRECT_EVERY: u64 = 9;
const BACKLOG: usize = 60;
const BACKLOG_MAX: usize = 20_000;
const SPEAKERS: [&str; 6] = ["Notch", "Herobrine", "Steve_99", "Creeper_Fan", "xX_Miner_Xx", "Alina"];
const SAYINGS: [&str; 7] = [
    "Hat jemand Diamanten übrig?",
    "/home farm",
    "Der Spawn ist wieder voll, wer kommt zum Markt?",
    "Ich verkaufe 64 Eisenbarren für 200 Münzen, bei Interesse einfach melden.",
    "Achtung: In zehn Minuten startet der Server neu. Bitte sichert eure Farmen, leert die Kisten und loggt euch rechtzeitig aus, damit nichts verloren geht. Danke für euer Verständnis!",
    "gg",
    "Wer hat die Netherportale an der Küste gebaut?",
];
const DIRECTS: [&str; 4] = ["kommst du kurz zum Spawn?", "hast du meine Nachricht gesehen?", "wo bist du gerade?", "Kannst du mir 3 Stack Weizen leihen? Ich zahle morgen zurück, versprochen."];

enum Cmd {
    Known(Vec<(String, String)>),
    Start(Target, Vec<Account>, Arc<AtomicBool>),
    Stop(Vec<String>),
    Kill(Vec<String>),
    Send(String, ToMod),
}

/// The fake fleet: a thread that answers commands with plausible updates.
pub struct Fake {
    tx: mpsc::Sender<Cmd>,
}

impl Fake {
    pub fn new<P: Pixels>(out: async_channel::Sender<Update<P>>, visible: Arc<Mutex<HashSet<String>>>) -> Self {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("fake-fleet".into()).spawn(move || Sim::new(out, visible).run(&rx));
        Self { tx }
    }
}

impl Backend for Fake {
    fn start(&self, target: Target, accounts: Vec<Account>, cancel: Arc<AtomicBool>, _home: Option<crate::core::place::Rect>) {
        let _ = self.tx.send(Cmd::Start(target, accounts, cancel));
    }

    fn stop(&self, keys: Vec<String>) {
        let _ = self.tx.send(Cmd::Stop(keys));
    }

    fn kill(&self, keys: Vec<String>) {
        let _ = self.tx.send(Cmd::Kill(keys));
    }

    fn send(&self, key: &str, message: ToMod) {
        let _ = self.tx.send(Cmd::Send(key.to_owned(), message));
    }

    fn read_preview(&self, _key: String, _path: PathBuf) {}

    fn release_preview(&self, _key: String) {}

    fn quit(&self) {}

    fn known(&self, instances: Vec<(String, String)>) {
        let _ = self.tx.send(Cmd::Known(instances));
    }
}

struct Running {
    seq: u32,
    next_preview: Instant,
    every: Duration,
    width: u32,
}

struct Sim<P> {
    out: async_channel::Sender<Update<P>>,
    visible: Arc<Mutex<HashSet<String>>>,
    names: HashMap<String, String>,
    seeded: bool,
    running: HashMap<String, Running>,
    on_server: HashSet<String>,
    queue: Vec<(Instant, Option<Arc<AtomicBool>>, Update<P>)>,
    reconnects: u64,
    chat_at: Instant,
    chat_n: u64,
    still: bool,
    changes: PreviewChanges,
    outage: Option<(Instant, Instant)>,
    dropped: bool,
}

fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn state<P>(instance: &str, kind: StateKind, reason: Option<&str>) -> Update<P> {
    Update::Ipc(Event::Message { instance: instance.to_owned(), message: FromMod::State { kind, server: Some(SERVER.to_owned()), reason: reason.map(str::to_owned), cause: None } })
}

fn server_down<P>(instance: &str) -> Update<P> {
    Update::Ipc(Event::Message { instance: instance.to_owned(), message: FromMod::State { kind: StateKind::Disconnected, server: Some(SERVER.to_owned()), reason: Some("Server closed".to_owned()), cause: Some(DisconnectCause::ServerDown) } })
}

/// Seconds from an environment variable.
fn secs(name: &str) -> Option<Duration> {
    std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok()).map(Duration::from_secs)
}

fn connected<P>(instance: &str) -> Update<P> {
    Update::Ipc(Event::Connected { instance: instance.to_owned(), pid: 0, mod_version: "fake".into() })
}

impl<P: Pixels> Sim<P> {
    fn new(out: async_channel::Sender<Update<P>>, visible: Arc<Mutex<HashSet<String>>>) -> Self {
        Self { out, visible, names: HashMap::new(), seeded: false, running: HashMap::new(), on_server: HashSet::new(), queue: Vec::new(), reconnects: 0, chat_at: Instant::now() + CHAT_EVERY, chat_n: 0, still: std::env::var_os("MANYCRAFT_FAKE_STILL").is_some(), changes: PreviewChanges::default(), outage: None, dropped: false }
    }

    fn run(mut self, rx: &mpsc::Receiver<Cmd>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(cmd) => self.command(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(cmd) = rx.try_recv() {
                self.command(cmd);
            }
            if !self.tick() {
                return;
            }
        }
    }

    fn at(&mut self, delay: Duration, cancel: Option<&Arc<AtomicBool>>, update: Update<P>) {
        self.queue.push((Instant::now() + delay, cancel.cloned(), update));
    }

    fn launch(&mut self, key: &str, after: Duration, cancel: Option<&Arc<AtomicBool>>) {
        self.at(after, cancel, Update::Launched { key: key.to_owned(), plain: false });
        self.at(after + Duration::from_millis(1500), cancel, connected(key));
        self.at(after + Duration::from_millis(2500), cancel, state(key, StateKind::Connecting, None));
        self.at(after + Duration::from_millis(3500), cancel, state(key, StateKind::Playing, None));
    }

    fn launch_plain(&mut self, key: &str, after: Duration, cancel: Option<&Arc<AtomicBool>>) {
        self.at(after, cancel, Update::Launched { key: key.to_owned(), plain: true });
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Known(list) => {
                self.names = list.into_iter().collect();
                if !self.seeded {
                    self.seeded = true;
                    self.seed();
                }
            }
            Cmd::Start(target, accounts, cancel) => {
                let plain = !crate::core::companion::available(&target.instance.game_version);
                for (n, account) in accounts.iter().enumerate() {
                    let key = target.key(account.id);
                    let after = STAGGER * n as u32;
                    if plain {
                        self.launch_plain(&key, after, Some(&cancel));
                    } else {
                        self.launch(&key, after, Some(&cancel));
                    }
                }
            }
            Cmd::Stop(keys) => {
                let took = secs("MANYCRAFT_FAKE_SLOW_STOP").unwrap_or(Duration::from_millis(300));
                for key in keys {
                    self.queue.retain(|(_, _, u)| !touches(u, &key));
                    self.at(took, None, Update::Exited { key, code: Some(0) });
                }
            }
            Cmd::Kill(keys) => {
                for key in keys {
                    self.queue.retain(|(_, _, u)| !touches(u, &key));
                    self.at(Duration::ZERO, None, Update::Exited { key, code: Some(1) });
                }
            }
            Cmd::Send(key, message) => self.message(&key, message),
        }
    }

    fn message(&mut self, key: &str, message: ToMod) {
        match message {
            ToMod::Reconnect => {
                self.reconnects += 1;
                self.at(Duration::from_millis(500), None, state(key, StateKind::Connecting, None));
                if self.outage.is_some_and(|(from, to)| (from..to).contains(&(Instant::now() + Duration::from_secs(2)))) {
                    self.at(Duration::from_secs(2), None, server_down(key));
                } else if self.reconnects.is_multiple_of(4) {
                    self.at(Duration::from_secs(2), None, state(key, StateKind::Disconnected, Some("Zeitüberschreitung")));
                } else {
                    self.at(Duration::from_secs(2), None, state(key, StateKind::Playing, None));
                }
            }
            ToMod::Respawn => self.at(Duration::from_millis(300), None, state(key, StateKind::Playing, None)),
            ToMod::Show { .. } | ToMod::Hide => {
                let visible = matches!(message, ToMod::Show { .. });
                self.at(Duration::ZERO, None, Update::Ipc(Event::Message { instance: key.to_owned(), message: FromMod::Window { visible, focused: visible } }));
            }
            ToMod::ChatSend { text } => {
                let from = self.names.get(key).cloned();
                let runs = from.iter().map(|f| Run { text: format!("<{f}> "), ..Run::default() }).chain([Run { text: text.clone(), ..Run::default() }]).collect();
                self.at(Duration::from_millis(100), None, Update::Ipc(Event::Message { instance: key.to_owned(), message: FromMod::Chat { kind: ChatKind::Player, from, text, runs } }));
            }
            ToMod::Quit => self.at(Duration::from_millis(300), None, Update::Exited { key: key.to_owned(), code: Some(0) }),
            ToMod::Profile { preview_interval_ms, preview_width } => {
                if let Some(run) = self.running.get_mut(key) {
                    let every = Duration::from_millis(u64::from(preview_interval_ms.max(250)));
                    let width = preview_width.clamp(16, 1280);
                    if every < run.every || width != run.width {
                        run.next_preview = Instant::now();
                    }
                    (run.every, run.width) = (every, width);
                }
            }
            ToMod::Hotkeys { .. } | ToMod::Identity { .. } | ToMod::Badge { .. } | ToMod::Hint { .. } | ToMod::Level(_) | ToMod::Options { .. } | ToMod::Servers => {}
        }
    }

    fn chat_line(&mut self, key: &str, direct: bool, unique: bool) -> Update<P> {
        self.chat_n += 1;
        let h = hash(&format!("{key}{}", self.chat_n));
        let name = self.names.get(key).cloned().unwrap_or_default();
        let (kind, from, text, runs) = match if direct { h % 10 } else { 1 + h % 9 } {
            0 => (ChatKind::System, None, format!("§d[{} -> du]§r {}", SPEAKERS[(h >> 8) as usize % 2], DIRECTS[(h >> 16) as usize % DIRECTS.len()]), Vec::new()),
            1 => (ChatKind::System, None, format!("§e{name} joined the game"), Vec::new()),
            2 => (ChatKind::System, None, format!("{name} was slain by Zombie"), Vec::new()),
            _ => {
                let said = SAYINGS[(h >> 16) as usize % SAYINGS.len()];
                let speaker = (h >> 8) as usize % SPEAKERS.len();
                let from = SPEAKERS[speaker];
                let text = if unique { format!("{said} ({})", self.chat_n) } else { said.to_owned() };
                let rank = if speaker % 3 == 0 { "§6[VIP]§r " } else { "" };
                let runs = vec![Run { text: format!("{rank}<{from}> "), ..Run::default() }, Run { text: text.clone(), ..Run::default() }];
                (ChatKind::Player, Some(from.to_owned()), text, runs)
            }
        };
        Update::Ipc(Event::Message { instance: key.to_owned(), message: FromMod::Chat { kind, from, text, runs } })
    }

    fn backlog(&mut self) {
        let wanted = std::env::var("MANYCRAFT_FAKE_CHAT").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(BACKLOG).min(BACKLOG_MAX);
        let mut keys: Vec<String> = self.names.keys().cloned().collect();
        keys.sort();
        if keys.is_empty() {
            return;
        }
        for n in 0..wanted {
            let key = keys[(hash(&n.to_string()) as usize) % keys.len()].clone();
            let line = self.chat_line(&key, true, true);
            self.at(Duration::ZERO, None, line);
        }
    }

    fn seed(&mut self) {
        self.backlog();
        if let Some(after) = secs("MANYCRAFT_FAKE_OUTAGE") {
            let from = Instant::now() + after;
            self.outage = Some((from, from + secs("MANYCRAFT_FAKE_OUTAGE_FOR").unwrap_or(Duration::from_secs(90))));
        }
        let mut keys: Vec<String> = self.names.keys().cloned().collect();
        keys.sort();
        for key in keys {
            let kind = hash(&key) % 40;
            let seed = |phase: Phase, attempts: u32| Update::Seed { key: key.clone(), phase, attempts };
            match kind {
                0..=27 => {
                    self.at(Duration::ZERO, None, seed(Phase::Menu, 0));
                    self.at(Duration::ZERO, None, connected(&key));
                    self.at(Duration::ZERO, None, state(&key, StateKind::Playing, None));
                }
                28 | 29 => {
                    self.at(Duration::ZERO, None, seed(Phase::Menu, 0));
                    self.at(Duration::ZERO, None, connected(&key));
                    self.at(Duration::ZERO, None, state(&key, StateKind::Menu, None));
                }
                30..=32 => {
                    self.at(Duration::ZERO, None, seed(Phase::Starting { place: 12, total: 50 }, 0));
                    let after = Duration::from_millis(4000 + hash(&key) % 12_000);
                    self.launch(&key, after, None);
                }
                33 | 34 => {
                    self.at(Duration::ZERO, None, connected(&key));
                    self.at(Duration::ZERO, None, seed(Phase::Disconnected { server: Some(SERVER.into()), reason: "Zeitüberschreitung".into() }, 3));
                }
                35 => self.at(Duration::ZERO, None, seed(Phase::LoginNeeded, 0)),
                36 => self.at(Duration::ZERO, None, seed(Phase::Crashed { code: Some(1) }, 0)),
                37 => self.at(Duration::ZERO, None, seed(Phase::Banned { server: Some(SERVER.into()), reason: "Cheating".into() }, 0)),
                38 => self.at(Duration::ZERO, None, seed(Phase::Restarting { attempt: 1, of: 3 }, 0)),
                _ => {}
            }
        }
    }

    fn tick(&mut self) -> bool {
        let now = Instant::now();
        if !self.dropped && self.outage.is_some_and(|(from, _)| from <= now) {
            self.dropped = true;
            let mut keys: Vec<String> = self.running.keys().cloned().collect();
            keys.sort();
            for key in keys {
                self.at(Duration::ZERO, None, server_down(&key));
            }
        }
        let mut due = Vec::new();
        let mut i = 0;
        while i < self.queue.len() {
            if self.queue[i].0 <= now {
                due.push(self.queue.swap_remove(i));
            } else {
                i += 1;
            }
        }
        due.sort_by_key(|(at, _, _)| *at);
        for (_, cancel, update) in due {
            if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                continue;
            }
            match &update {
                Update::Ipc(Event::Connected { instance, .. }) => {
                    self.running.insert(instance.clone(), Running { seq: 0, next_preview: now, every: PREVIEW_EVERY, width: PREVIEW_W });
                }
                Update::Exited { key, .. } => {
                    self.running.remove(key);
                    self.on_server.remove(key);
                }
                Update::Ipc(Event::Message { instance, message: FromMod::State { kind, .. } }) => {
                    if matches!(kind, StateKind::Playing | StateKind::Queue) {
                        self.on_server.insert(instance.clone());
                    } else {
                        self.on_server.remove(instance);
                    }
                }
                _ => {}
            }
            if self.out.send_blocking(update).is_err() {
                return false;
            }
        }
        if now >= self.chat_at {
            self.chat_at = now + CHAT_EVERY;
            let mut keys: Vec<String> = self.running.keys().filter(|k| self.on_server.contains(*k)).cloned().collect();
            keys.sort();
            if !keys.is_empty() {
                let key = keys[(hash(&self.chat_n.to_string()) as usize) % keys.len()].clone();
                let line = self.chat_line(&key, !self.still && (self.chat_n + 1).is_multiple_of(DIRECT_EVERY), false);
                let lines = match &line {
                    Update::Ipc(Event::Message { message: FromMod::Chat { kind: ChatKind::Player, from, text, runs }, .. }) => keys
                        .iter()
                        .map(|k| Update::Ipc(Event::Message { instance: k.clone(), message: FromMod::Chat { kind: ChatKind::Player, from: from.clone(), text: text.clone(), runs: runs.clone() } }))
                        .collect(),
                    _ => vec![line],
                };
                for line in lines {
                    if self.out.send_blocking(line).is_err() {
                        return false;
                    }
                }
            }
        }
        let visible = self.visible.lock().map(|v| v.clone()).unwrap_or_default();
        for (key, run) in &mut self.running {
            if !visible.contains(key) || run.next_preview > now {
                continue;
            }
            run.seq += 2;
            run.next_preview = now + run.every + run.every / 10 * (hash(key) % 10) as u32 / 10;
            let (w, h) = (run.width, (run.width * PREVIEW_H / PREVIEW_W).max(1));
            let preview = Preview { seq: run.seq, width: w, height: h, rgba: frame(hash(key), if self.still { 2 } else { run.seq }, w, h) };
            if !self.changes.changed(key, &preview) {
                continue;
            }
            let Some(image) = P::from_rgba(preview.width, preview.height, preview.rgba) else { continue };
            if self.out.send_blocking(Update::Preview { key: key.clone(), seq: run.seq, image }).is_err() {
                return false;
            }
        }
        true
    }
}

fn touches<P>(update: &Update<P>, key: &str) -> bool {
    match update {
        Update::Launched { key: k, .. } | Update::Failed { key: k, .. } | Update::Exited { key: k, .. } | Update::Seed { key: k, .. } | Update::Preview { key: k, .. } | Update::ProxyChecked { key: k, .. } => k == key,
        Update::Ipc(Event::Connected { instance, .. } | Event::Message { instance, .. } | Event::Disconnected { instance }) => instance == key,
        Update::Preparing { .. } | Update::Prepared { .. } | Update::SignIn { .. } | Update::Still { .. } | Update::Face { .. } | Update::Reach { .. } | Update::Ipc(Event::Reopen | Event::Play { .. }) => false,
    }
}

fn frame(seed: u64, seq: u32, w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let sky = [(80 + seed % 60) as u8, (140 + (seed >> 8) % 60) as u8, (200 + (seed >> 16) % 50) as u8];
    let ground = [(70 + (seed >> 24) % 40) as u8, (110 + (seed >> 32) % 50) as u8, (40 + (seed >> 40) % 30) as u8];
    let horizon = h * 3 / 5 + (seed % 8) as usize;
    let sun = (seed as usize / 7 + seq as usize * 9) % (w + 24);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let px = if (x + 24).abs_diff(sun + 12) < 8 && y.abs_diff(18) < 8 {
                [250, 230, 140]
            } else if y < horizon {
                let shade = (y * 40 / horizon) as u8;
                [sky[0].saturating_sub(shade), sky[1].saturating_sub(shade), sky[2].saturating_sub(shade / 2)]
            } else {
                let block = ((x / 12 + y / 12) % 2) as u8 * 12;
                [ground[0].saturating_add(block), ground[1].saturating_add(block), ground[2]]
            };
            let at = (y * w + x) * 4;
            out[at..at + 4].copy_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Raw;

    impl Pixels for Raw {
        fn from_rgba(_: u32, _: u32, _: Vec<u8>) -> Option<Self> {
            Some(Raw)
        }
    }

    #[test]
    fn frames_have_the_preview_size_and_move() {
        let a = frame(42, 2, PREVIEW_W, PREVIEW_H);
        assert_eq!(a.len(), (PREVIEW_W * PREVIEW_H * 4) as usize);
        assert_ne!(a, frame(42, 4, PREVIEW_W, PREVIEW_H));
    }

    #[test]
    fn stop_drops_pending_updates_of_that_instance() {
        let (out, rx) = async_channel::unbounded::<Update<Raw>>();
        let mut sim = Sim::new(out, Arc::default());
        sim.launch("a", Duration::from_secs(60), None);
        sim.launch("b", Duration::from_secs(60), None);
        sim.command(Cmd::Stop(vec!["a".into()]));
        assert!(sim.queue.iter().all(|(_, _, u)| !touches(u, "a") || matches!(u, Update::Exited { .. })));
        assert!(sim.queue.iter().any(|(_, _, u)| touches(u, "b")));
        drop(rx);
    }
}
