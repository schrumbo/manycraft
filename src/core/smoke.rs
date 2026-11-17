//! `manycraft smoke`: starts one hidden offline game with the companion and checks that it connects, sends a preview
//! frame, keeps to the background frame cap, keeps background values out of `options.txt` and found every hook. With
//! `--server` it also joins that server and gets its own chat line, a disguised `/me` line and a system line back; `--proxy` routes it through a local SOCKS5
//! relay. CI runs it for each supported version.

use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::account::Account;
use super::batch::Batch;
use super::fleet::{self, Fleet, Target};
use super::instance::{self, Instance, Loader};
use super::ipc::{self, ChatKind, FromMod, Level, StateKind, ToMod};

const USAGE: &str = "usage: manycraft smoke --version <version> [--loader vanilla|fabric|quilt|forge|neoforge] [--timeout <seconds>] [--server <host:port> [--proxy]] [--companion <jar>]";
const NAME: &str = "Smoke";
const FLAG: &str = "-Dmanycraft.smoke=true";
const PLAYER_SIGHT: &str = "10";
const TEMPLATE: [(&str, &str); 3] = [("soundCategory_master", "0.0"), ("onboardAccessibility", "false"), ("renderDistance", PLAYER_SIGHT)];
const CAP: u32 = 3;
const SAMPLES: usize = 3;
const POLL: Duration = Duration::from_millis(250);
const CHAT_RETRY: Duration = Duration::from_secs(10);
const DEFAULT_TIMEOUT: u64 = 900;

static STOPPED: AtomicBool = AtomicBool::new(false);
const FPS_LINE: &str = "manycraft smoke fps ";
const SAVED_LINE: &str = "manycraft smoke saved options while hidden";
const HOOKS_LINE: &str = " ms, hooks ";
const CHAT_LINE: &str = "manycraft smoke chat";
const SAY_LINE: &str = "manycraft smoke emote";
const LIST_COMMAND: &str = "/list";
const LIST_ANSWER: &str = "of a max of";
const BACKEND_FAILED: &str = "Failed to create backend ";
const BACKEND_USED: &str = "Using graphics backend ";
const BACKENDS: [&str; 2] = ["OpenGL", "Vulkan"];
const TAIL: usize = 60;
const FAULTS: [&str; 9] = [
    "hooks not found",
    "does not verify",
    "cannot patch",
    "failed, left to the game",
    "stopping the game",
    "the companion leaves it alone",
    "previews stay off",
    "stay unclamped",
    "options fields are not where",
];

/// Runs `manycraft smoke`, returning the exit code: 0 when every check passed.
pub fn cli(args: &[String]) -> i32 {
    tie_games();
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("smoke failed: {e:#}");
            1
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    Ok,
    Failed,
    Skipped,
}

impl Check {
    fn of(done: bool) -> Self {
        if done { Self::Ok } else { Self::Failed }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Default)]
struct Progress {
    connected: bool,
    previewed: bool,
    fps: Option<Vec<u32>>,
    saved: bool,
    playing: bool,
    chat: bool,
    say: bool,
    system: bool,
}

/// Ends the game with this process however it ends on Windows: every child shares a job that closes with it.
fn tie_games() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0);
        if SetInformationJobObject(job, JobObjectExtendedLimitInformation, std::ptr::from_ref(&info).cast(), size) != 0 {
            AssignProcessToJobObject(job, GetCurrentProcess());
        }
    }
}

/// Once the game runs, the first interrupt or termination stops the watch so the game is killed before the process
/// exits; a second one ends the process at once.
fn stop_on_signal() {
    #[cfg(unix)]
    unsafe {
        extern "C" fn stop(_: libc::c_int) {
            STOPPED.store(true, Ordering::SeqCst);
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESETHAND;
        libc::sigemptyset(&raw mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(signal, &raw const action, std::ptr::null_mut());
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let proxy = args.iter().any(|a| a == "--proxy");
    let args: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--proxy").collect();
    let Some(flags) = fleet::flags(&args, &["--version", "--loader", "--timeout", "--server", "--companion"]) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let Some(version) = flags.get("--version").copied() else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let server = flags.get("--server").copied();
    if proxy && server.is_none() {
        eprintln!("{USAGE}");
        return Ok(2);
    }
    let loader = flags.get("--loader").map_or(Ok(Loader::Vanilla), |l| fleet::parse_loader(l))?;
    let timeout = flags.get("--timeout").map_or(Ok(DEFAULT_TIMEOUT), |t| t.parse().context("--timeout is a number of seconds"))?;
    let companion = flags.get("--companion").map(Path::new);
    let instance = smoke_instance(version, loader)?;
    quiet(&instance.options_template())?;
    let relay = if proxy { Some(Relay::start()?) } else { None };
    let mut account = Account::offline(NAME)?;
    account.proxy = relay.as_ref().map(|r| format!("socks5://{}", r.address));
    let game_dir = instance.game_dir(account.id);
    let target = match server {
        Some(server) => {
            let mut batch = Batch::new(&format!("{NAME} {version}"), &instance.id);
            batch.server = Some(server.to_owned());
            batch.accounts = vec![account.id];
            Target::of_batch(&batch, &instance)
        }
        None => Target::solo(&instance),
    };
    let (fleet, events) = Fleet::new()?;
    let report = |p: super::install::Progress| {
        if p.total > 0 && p.done == p.total {
            println!("{}: {}/{}", p.stage.label(), p.done, p.total);
        }
    };
    let started = fleet.start_batch(&target, std::slice::from_ref(&account), companion, &report)?;
    stop_on_signal();
    let key = match started.into_iter().next().map(|(_, outcome)| outcome) {
        Some(Ok(key)) => key,
        Some(Err(e)) => bail!("the game did not start: {e:#}"),
        None => bail!("the game did not start"),
    };
    let launch_log = game_dir.join("logs").join("manycraft-launch.log");
    let mut progress = Progress::default();
    let outcome = watch(&fleet, &events, &key, &launch_log, server.is_some(), Duration::from_secs(timeout), &mut progress);
    if STOPPED.load(Ordering::SeqCst) {
        fleet.kill(std::slice::from_ref(&key));
    } else {
        let _ = fleet.send(&key, &ToMod::Quit);
        fleet.stop_all(std::slice::from_ref(&key));
    }
    let log = read_log(&launch_log);
    for line in log.lines().filter(|l| l.contains("[manycraft-agent]")) {
        println!("{line}");
    }
    if let Some(backend) = log.lines().find_map(|l| message(l, BACKEND_USED)) {
        println!("{backend}");
    }
    let mut problems = match outcome {
        Ok(()) => Vec::new(),
        Err(e) => vec![format!("{e:#}")],
    };
    let sight = sight(&fs::read_to_string(game_dir.join("options.txt")).unwrap_or_default());
    let options = Check::of(progress.saved && sight.as_deref() == Some(PLAYER_SIGHT));
    if progress.saved && options == Check::Failed {
        problems.push(format!("background values reached options.txt: renderDistance {sight:?}, the player set {PLAYER_SIGHT}"));
    }
    let joined = |done: bool| if server.is_some() { Check::of(done) } else { Check::Skipped };
    let proxied = match &relay {
        None => Check::Skipped,
        Some(relay) => {
            let seen = relay.seen();
            let carried = server.is_some_and(|s| seen.iter().any(|t| same_target(t, s)));
            if !carried {
                problems.push(format!("the proxy never carried the game connection, it saw {seen:?}"));
            }
            Check::of(carried && progress.playing)
        }
    };
    let checks = [
        ("handshake", Check::of(progress.connected)),
        ("preview", Check::of(progress.previewed)),
        ("fps", Check::of(progress.fps.is_some())),
        ("options", options),
        ("join", joined(progress.playing)),
        ("chat", joined(progress.chat)),
        ("say", joined(progress.say)),
        ("system", joined(progress.system)),
        ("proxy", proxied),
    ];
    if let Some(fps) = &progress.fps {
        println!("hidden fps {fps:?} (cap {CAP})");
    }
    if let Some(hooks) = log.lines().find_map(hook_map) {
        println!("hooks {hooks}");
        if hooks.contains("=false") {
            problems.push(format!("some hooks did not apply: {hooks}"));
        }
    }
    for (name, check) in checks {
        println!("check {name} {}", check.word());
    }
    problems.extend(faults(&log));
    if problems.is_empty() && checks.iter().all(|(_, c)| *c != Check::Failed) {
        println!("smoke passed: {version} {}", instance.loader.name());
        return Ok(0);
    }
    for problem in &problems {
        eprintln!("smoke failed: {problem}");
    }
    let logs = game_dir.join("logs");
    let modified = |name: &str| fs::metadata(logs.join(name)).and_then(|m| m.modified()).ok();
    let latest = match (modified("latest.log"), modified("manycraft-launch.log")) {
        (Some(latest), Some(launch)) if latest >= launch => read_log(&logs.join("latest.log")),
        _ => String::new(),
    };
    let (name, text) = if latest.is_empty() { ("manycraft-launch.log", &log) } else { ("latest.log", &latest) };
    eprintln!("last lines of {name}:");
    for line in tail(text, TAIL) {
        eprintln!("  {line}");
    }
    Ok(1)
}

fn smoke_instance(version: &str, loader: Loader) -> Result<Instance> {
    let name = format!("{NAME} {version} {}", loader.name());
    let mut made = match instance::load_all()?.into_iter().find(|i| i.name == name) {
        Some(found) => found,
        None => fleet::new_instance(&name, Some(version), loader, None)?,
    };
    if !made.jvm_args.iter().any(|a| a == FLAG) {
        made.jvm_args.push(FLAG.to_owned());
        instance::save(&made)?;
    }
    Ok(made)
}

fn quiet(template: &Path) -> Result<()> {
    let old = fs::read_to_string(template).unwrap_or_default();
    let ours = |line: &str| TEMPLATE.iter().any(|(key, _)| line.split_once(':').is_some_and(|(k, _)| k == *key));
    let mut text: String = old.lines().filter(|l| !ours(l)).map(|l| format!("{l}\n")).collect();
    for (key, value) in TEMPLATE {
        text.push_str(&format!("{key}:{value}\n"));
    }
    if let Some(parent) = template.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(template, text).with_context(|| format!("cannot write {}", template.display()))
}

fn watch(fleet: &Fleet, events: &std::sync::mpsc::Receiver<ipc::Event>, key: &str, log: &Path, server: bool, timeout: Duration, p: &mut Progress) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut checked_from = 0;
    let mut chat_sent: Option<Instant> = None;
    let nonce = format!("{CHAT_LINE} {}", std::process::id());
    let say = format!("{SAY_LINE} {}", std::process::id());
    let mut say_sent: Option<Instant> = None;
    let mut list_sent: Option<Instant> = None;
    while Instant::now() < deadline {
        if STOPPED.load(Ordering::SeqCst) {
            bail!("stopped before the checks finished");
        }
        match events.recv_timeout(POLL) {
            Ok(ipc::Event::Connected { instance, mod_version, .. }) if instance == key => {
                println!("connected, companion {mod_version}");
                p.connected = true;
                fleet.send(key, &ToMod::Level(Level::Saving))?;
            }
            Ok(ipc::Event::Message { instance, message }) if instance == key => match message {
                FromMod::Preview { w, h, .. } if !p.previewed => {
                    println!("preview {w}x{h}");
                    p.previewed = true;
                    checked_from = fps_samples(&read_log(log)).len();
                }
                FromMod::Window { visible: true, .. } => bail!("the game window became visible, it should start hidden"),
                FromMod::State { kind: StateKind::Playing, server: joined, .. } if !p.playing => {
                    println!("joined {}", joined.unwrap_or_default());
                    p.playing = true;
                }
                FromMod::State { kind: StateKind::Disconnected, reason, .. } if server => bail!("the game left the server: {}", reason.unwrap_or_default()),
                FromMod::Chat { text, .. } if text.contains(&nonce) => {
                    println!("chat came back: {text}");
                    p.chat = true;
                }
                FromMod::Chat { kind: ChatKind::Player, text, .. } if text.contains(&say) => {
                    println!("emote came back: {text}");
                    p.say = true;
                }
                FromMod::Chat { kind: ChatKind::System, text, .. } if text.contains(LIST_ANSWER) => {
                    println!("system chat came back: {text}");
                    p.system = true;
                }
                _ => {}
            },
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("the manager stopped listening"),
        }
        if !fleet.is_running(key) {
            bail!("the game exited (code {:?})", fleet.exit_code(key).flatten());
        }
        let text = read_log(log);
        if !p.connected && let Some(why) = no_backend(&text) {
            bail!("no graphics backend started, the game waits on its error dialog: {why}");
        }
        if p.previewed && p.fps.is_none() {
            let samples: Vec<(u32, bool)> = fps_samples(&text).into_iter().skip(checked_from).collect();
            if samples.len() >= SAMPLES {
                p.fps = Some(judge(&samples[samples.len() - SAMPLES..])?);
            }
        }
        p.saved = text.contains(SAVED_LINE);
        if p.playing && !p.chat && chat_sent.is_none_or(|at| at.elapsed() >= CHAT_RETRY) {
            fleet.send(key, &ToMod::ChatSend { text: nonce.clone() })?;
            chat_sent = Some(Instant::now());
        }
        if p.chat && !p.say && say_sent.is_none_or(|at| at.elapsed() >= CHAT_RETRY) {
            fleet.send(key, &ToMod::ChatSend { text: format!("/me {say}") })?;
            say_sent = Some(Instant::now());
        }
        if p.chat && !p.system && list_sent.is_none_or(|at| at.elapsed() >= CHAT_RETRY) {
            fleet.send(key, &ToMod::ChatSend { text: LIST_COMMAND.to_string() })?;
            list_sent = Some(Instant::now());
        }
        if p.fps.is_some() && p.saved && (!server || (p.chat && p.say && p.system)) {
            return Ok(());
        }
    }
    bail!("timed out: connected {}, preview {}, fps {}, options saved {}, joined {}, chat {}, say {}, system {}", p.connected, p.previewed, p.fps.is_some(), p.saved, p.playing, p.chat, p.say, p.system)
}

fn judge(samples: &[(u32, bool)]) -> Result<Vec<u32>> {
    let fps: Vec<u32> = samples.iter().map(|s| s.0).collect();
    if samples.iter().any(|s| !s.1) {
        bail!("the game was not in background mode while hidden: {samples:?}");
    }
    if fps.iter().any(|&f| f == 0 || f > CAP + 1) {
        bail!("hidden fps {fps:?} outside 1..={}", CAP + 1);
    }
    Ok(fps)
}

fn sight(options: &str) -> Option<String> {
    options.lines().find_map(|l| l.strip_prefix("renderDistance:")).map(|v| v.trim().to_owned())
}

fn hook_map(line: &str) -> Option<String> {
    let rest = &line[line.find(HOOKS_LINE)? + HOOKS_LINE.len()..];
    Some(rest.split("]]>").next().unwrap_or(rest).trim().to_owned())
}

fn same_target(seen: &str, server: &str) -> bool {
    let with_port = |s: &str| if s.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) { s.to_ascii_lowercase() } else { format!("{}:25565", s.to_ascii_lowercase()) };
    with_port(seen) == with_port(server)
}

/// A SOCKS5 relay on 127.0.0.1 without authentication that remembers where each connection went.
struct Relay {
    address: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Relay {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("cannot open the local proxy")?;
        let address = listener.local_addr()?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || {
                    let _ = relay(stream, &log);
                });
            }
        });
        Ok(Self { address, seen })
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

fn relay(mut client: TcpStream, seen: &Mutex<Vec<String>>) -> std::io::Result<()> {
    let mut head = [0u8; 2];
    client.read_exact(&mut head)?;
    let mut methods = vec![0u8; usize::from(head[1])];
    client.read_exact(&mut methods)?;
    if head[0] != 5 || !methods.contains(&0) {
        return client.write_all(&[5, 0xff]);
    }
    client.write_all(&[5, 0])?;
    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    let host = match request[3] {
        1 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip)?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len)?;
            let mut name = vec![0u8; usize::from(len[0])];
            client.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut ip = [0u8; 16];
            client.read_exact(&mut ip)?;
            format!("[{}]", std::net::Ipv6Addr::from(ip))
        }
        _ => return client.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port)?;
    let target = format!("{host}:{}", u16::from_be_bytes(port));
    seen.lock().unwrap_or_else(PoisonError::into_inner).push(target.clone());
    let upstream = match target.to_socket_addrs().ok().and_then(|mut a| a.next()).map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(10))) {
        Some(Ok(upstream)) => upstream,
        _ => return client.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]),
    };
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
    let (mut up_read, mut client_write) = (upstream.try_clone()?, client.try_clone()?);
    let back = std::thread::spawn(move || {
        let _ = std::io::copy(&mut up_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Both);
    });
    let mut up_write = upstream;
    let _ = std::io::copy(&mut client, &mut up_write);
    let _ = up_write.shutdown(Shutdown::Both);
    let _ = back.join();
    Ok(())
}

fn read_log(path: &Path) -> String {
    fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn fps_samples(log: &str) -> Vec<(u32, bool)> {
    log.lines()
        .filter_map(|line| {
            let rest = &line[line.find(FPS_LINE)? + FPS_LINE.len()..];
            let mut words = rest.split_whitespace();
            let fps = words.next()?.parse().ok()?;
            (words.next()? == "hidden").then_some(())?;
            Some((fps, words.next()?.starts_with("true")))
        })
        .collect()
}

fn message(line: &str, prefix: &str) -> Option<String> {
    let text = &line[line.find(prefix)?..];
    Some(text.split("]]>").next().unwrap_or(text).trim().to_owned())
}

fn no_backend(log: &str) -> Option<String> {
    if log.contains(BACKEND_USED) {
        return None;
    }
    let lines: Vec<&str> = log.lines().collect();
    let failed: Vec<(String, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let name: String = line.split_once(BACKEND_FAILED)?.1.chars().take_while(|c| c.is_alphanumeric()).collect();
            let reason = lines[i + 1..].iter().take(3).find_map(|l| l.split_once("BackendCreationException: ").map(|(_, r)| r.trim().to_owned()));
            Some((name, reason.unwrap_or_default()))
        })
        .collect();
    BACKENDS
        .iter()
        .all(|b| failed.iter().any(|(name, _)| name == b))
        .then(|| failed.iter().map(|(name, reason)| format!("{name}: {reason}")).collect::<Vec<_>>().join("; "))
}

fn tail(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

fn faults(log: &str) -> Vec<String> {
    log.lines().filter(|l| FAULTS.iter().any(|f| l.contains(f))).map(|l| l.trim().to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_lines_parse_and_others_are_skipped() {
        let log = "[12:00:01] [Render thread/INFO]: manycraft smoke fps 3 hidden true\nnoise\n[x]: manycraft smoke fps 60 hidden false\n[x]: manycraft smoke fps x hidden true\n<log4j:Message><![CDATA[manycraft smoke fps 2 hidden true]]></log4j:Message>\n[x]: manycraft smoke fps 58 hidden false worst 41 ms slow 0\n";
        assert_eq!(fps_samples(log), vec![(3, true), (60, false), (2, true), (58, false)]);
    }

    #[test]
    fn hidden_frames_must_keep_the_cap() {
        assert_eq!(judge(&[(3, true), (2, true), (4, true)]).unwrap(), vec![3, 2, 4]);
        assert!(judge(&[(3, true), (30, true), (3, true)]).is_err());
        assert!(judge(&[(3, true), (0, true), (3, true)]).is_err());
        assert!(judge(&[(3, false), (3, true), (3, true)]).is_err());
    }

    #[test]
    fn agent_faults_are_found() {
        let log = "[manycraft-agent] hooks not found in net/minecraft/client/Options: save (options)\nfine\n[x/WARN]: Hud has none of a, b, the companion leaves it alone\n";
        assert_eq!(faults(log).len(), 2);
        assert!(faults("[manycraft-agent] patched 17 features").is_empty());
        assert!(faults("[manycraft-agent] optional hooks absent in net/minecraft/client/gui/Gui: extractRenderState (hud)").is_empty());
    }

    #[test]
    fn every_backend_failing_is_found_in_plain_and_xml_logs() {
        let plain = "[10:48:46] [Render thread/ERROR]: Failed to create backend OpenGL
com.mojang.renderpearl.api.device.BackendCreationException: Failed to create window for OpenGL context: Couldn't find matching GLX visual
	at x
[10:48:46] [Render thread/ERROR]: Failed to create backend Vulkan
com.mojang.renderpearl.api.device.BackendCreationException: Vulkan is not supported
";
        assert_eq!(
            no_backend(plain).unwrap(),
            "OpenGL: Failed to create window for OpenGL context: Couldn't find matching GLX visual; Vulkan: Vulkan is not supported"
        );
        let xml = "<log4j:Message><![CDATA[Failed to create backend OpenGL]]></log4j:Message>
<log4j:Throwable><![CDATA[com.mojang.renderpearl.api.device.BackendCreationException: no visual
<log4j:Message><![CDATA[Failed to create backend Vulkan]]></log4j:Message>
";
        assert_eq!(no_backend(xml).unwrap(), "OpenGL: no visual; Vulkan: ");
        assert!(no_backend("[x]: Failed to create backend Vulkan
[x]: Using graphics backend OpenGL, using drivers: llvmpipe
").is_none());
        assert!(no_backend(&format!("{plain}[x]: Using graphics backend Vulkan, using drivers: lavapipe
")).is_none());
        assert!(no_backend("[x]: Failed to create backend 
<![CDATA[Failed to create backend ]]>
").is_none());
        assert!(no_backend("fine
").is_none());
    }

    #[test]
    fn the_used_backend_reads_without_xml() {
        let xml = "    <log4j:Message><![CDATA[Using graphics backend OpenGL, using drivers: 4.5 Mesa]]></log4j:Message>";
        assert_eq!(message(xml, BACKEND_USED).unwrap(), "Using graphics backend OpenGL, using drivers: 4.5 Mesa");
        assert_eq!(message("[x]: Using graphics backend Vulkan", BACKEND_USED).unwrap(), "Using graphics backend Vulkan");
        assert!(message("other", BACKEND_USED).is_none());
    }

    #[test]
    fn the_tail_keeps_the_last_lines() {
        assert_eq!(tail("a
b
c
", 2), vec!["b", "c"]);
        assert_eq!(tail("a
", 5), vec!["a"]);
        assert!(tail("", 5).is_empty());
    }

    #[test]
    fn the_template_is_muted_without_onboarding_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t").join("options.txt");
        quiet(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "soundCategory_master:0.0\nonboardAccessibility:false\nrenderDistance:10\n");
        fs::write(&path, "fov:0.5\nsoundCategory_master:1.0\nonboardAccessibility:true\n").unwrap();
        quiet(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fov:0.5\nsoundCategory_master:0.0\nonboardAccessibility:false\nrenderDistance:10\n");
    }

    #[test]
    fn the_saved_sight_and_the_hook_map_read_from_their_lines() {
        assert_eq!(sight("fov:0.5\nrenderDistance:10\n").as_deref(), Some("10"));
        assert!(sight("fov:0.5\n").is_none());
        let line = "[manycraft-agent] companion ready in 412 ms, hooks {chat=true, options=false}";
        assert_eq!(hook_map(line).as_deref(), Some("{chat=true, options=false}"));
        assert!(hook_map("[manycraft-agent] not attached").is_none());
    }

    #[test]
    fn proxy_targets_match_with_and_without_the_default_port() {
        assert!(same_target("127.0.0.1:25599", "127.0.0.1:25599"));
        assert!(same_target("Play.Example.org:25565", "play.example.org"));
        assert!(!same_target("127.0.0.1:25565", "127.0.0.1:25599"));
    }

    #[test]
    fn the_relay_carries_a_connection_and_remembers_its_target() {
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_addr = echo.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = echo.accept().unwrap();
            let mut b = [0u8; 4];
            s.read_exact(&mut b).unwrap();
            s.write_all(&b).unwrap();
        });
        let relay = Relay::start().unwrap();
        let mut c = TcpStream::connect(relay.address).unwrap();
        c.write_all(&[5, 1, 0]).unwrap();
        let mut reply = [0u8; 2];
        c.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [5, 0]);
        let host = b"127.0.0.1";
        let mut request = vec![5, 1, 0, 3, host.len() as u8];
        request.extend_from_slice(host);
        request.extend_from_slice(&echo_addr.port().to_be_bytes());
        c.write_all(&request).unwrap();
        let mut done = [0u8; 10];
        c.read_exact(&mut done).unwrap();
        assert_eq!(done[1], 0);
        c.write_all(b"ping").unwrap();
        let mut back = [0u8; 4];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"ping");
        assert_eq!(relay.seen(), vec![format!("127.0.0.1:{}", echo_addr.port())]);
    }
}
