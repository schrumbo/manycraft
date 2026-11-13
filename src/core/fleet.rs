//! Running instances: preparing game folders from a batch, starting them staggered, tracking their processes.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use uuid::Uuid;

use super::account::{self, Account, AccountKind};
use super::batch::{self, Batch};
use super::install::{self, Installed};
use super::launch::{self, LaunchSpec};
use super::{auth, ipc, net};

/// Pause between two instance starts unless the caller sets another.
pub const DEFAULT_STAGGER: Duration = Duration::from_secs(3);
/// File name of the companion mod in a batch's shared mods folder.
pub const COMPANION_FILE: &str = "manycraft-companion.jar";

const DEFAULT_PORT: u16 = 25565;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
const RESOLVE_CACHE: Duration = Duration::from_secs(60);
const REAP_INTERVAL: Duration = Duration::from_millis(500);
const QUIT_GRACE: Duration = Duration::from_secs(5);
const QUIT_POLL: Duration = Duration::from_millis(100);
const EVENT_POLL: Duration = Duration::from_millis(250);
const MIN_MEMORY_MB: u32 = 512;
const PREVIEW_FILE: &str = "manycraft-preview.bin";
const OPTIONS_FILE: &str = "options.txt";
const TOKEN_VAR: &str = "MANYCRAFT_TOKEN";
const PER_INSTANCE_KEYS: &[&str] = &[];
const USAGE: &str = "usage: manycraft batch create <name> [--version <version>] [--server <host:port>] [--memory <MB>]
       manycraft batch list
       manycraft batch add <batch> <account>
       manycraft batch remove <batch> <account>
       manycraft batch start <batch> [--companion <jar>] [--stagger <seconds>]
       manycraft batch delete <batch>";

/// Readies the game folder of `account` in `batch` and returns it: shared `mods` and `config` linked in,
/// `options.txt` from the batch template, the companion mod in the shared mods folder.
pub fn prepare(batch: &Batch, account: &Account, companion_jar: Option<&Path>) -> Result<PathBuf> {
    let dir = batch.instance_dir(account.id);
    prepare_in(&dir, &batch.mods_dir(), &batch.config_dir(), &batch.options_template(), companion_jar)?;
    Ok(dir)
}

fn prepare_in(dir: &Path, mods: &Path, config: &Path, template: &Path, companion_jar: Option<&Path>) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for (name, target) in [("mods", mods), ("config", config)] {
        fs::create_dir_all(target).with_context(|| format!("cannot create {}", target.display()))?;
        link(&dir.join(name), target)?;
    }
    if let Some(jar) = companion_jar {
        install_companion(jar, mods)?;
    }
    copy_options(template, &dir.join(OPTIONS_FILE))
}

fn link(path: &Path, target: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        Ok(meta) if meta.file_type().is_symlink() || is_junction(path) => {
            if points_to(path, target) {
                return Ok(());
            }
            remove_link(path)?;
        }
        Ok(_) => move_aside(path)?,
    }
    create_link(target, path).with_context(|| format!("cannot link {} to {}", path.display(), target.display()))
}

fn points_to(link: &Path, target: &Path) -> bool {
    matches!((fs::canonicalize(link), fs::canonicalize(target)), (Ok(a), Ok(b)) if a == b)
}

fn move_aside(path: &Path) -> Result<()> {
    let name = path.file_name().context("path has no name")?.to_string_lossy().into_owned();
    let free = (1..).map(|n| path.with_file_name(format!("{name}.old-{n}"))).find(|p| fs::symlink_metadata(p).is_err()).context("no free name")?;
    fs::rename(path, &free).with_context(|| format!("cannot move {} to {}", path.display(), free.display()))
}

#[cfg(windows)]
fn is_junction(path: &Path) -> bool {
    junction::exists(path).unwrap_or(false)
}

#[cfg(not(windows))]
fn is_junction(_path: &Path) -> bool {
    false
}

#[cfg(windows)]
fn create_link(target: &Path, link: &Path) -> io::Result<()> {
    junction::create(target, link)
}

#[cfg(not(windows))]
fn create_link(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn remove_link(path: &Path) -> Result<()> {
    fs::remove_dir(path).or_else(|_| fs::remove_file(path)).with_context(|| format!("cannot remove the link {}", path.display()))
}

#[cfg(not(windows))]
fn remove_link(path: &Path) -> Result<()> {
    fs::remove_file(path).with_context(|| format!("cannot remove the link {}", path.display()))
}

fn copy_options(template: &Path, dest: &Path) -> Result<()> {
    let text = match fs::read_to_string(template) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", template.display())),
    };
    let existing = if PER_INSTANCE_KEYS.is_empty() { None } else { fs::read_to_string(dest).ok() };
    net::write_atomic(dest, merge_options(&text, existing.as_deref(), PER_INSTANCE_KEYS).as_bytes())
}

fn merge_options(template: &str, existing: Option<&str>, keys: &[&str]) -> String {
    let key = |line: &str| line.split_once(':').map(|(k, _)| k.to_owned());
    let kept: HashMap<String, &str> = existing.unwrap_or_default().lines().filter_map(|l| key(l).filter(|k| keys.contains(&k.as_str())).map(|k| (k, l))).collect();
    let mut out: Vec<&str> = template.lines().map(|l| key(l).and_then(|k| kept.get(&k).copied()).unwrap_or(l)).collect();
    let in_template: Vec<String> = template.lines().filter_map(key).collect();
    let mut extra: Vec<(&String, &&str)> = kept.iter().filter(|(k, _)| !in_template.contains(k)).collect();
    extra.sort();
    out.extend(extra.into_iter().map(|(_, l)| *l));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn install_companion(jar: &Path, mods: &Path) -> Result<()> {
    let dest = mods.join(COMPANION_FILE);
    let want = net::file_sha1(jar).with_context(|| format!("cannot read {}", jar.display()))?;
    if net::file_sha1(&dest).is_ok_and(|have| have == want) {
        return Ok(());
    }
    let bytes = fs::read(jar).with_context(|| format!("cannot read {}", jar.display()))?;
    net::write_atomic(&dest, &bytes)
}

/// The address of `server` ("host", "host:port", an IP or "[v6]:port"): a SRV record `_minecraft._tcp.<host>`
/// when no port is given, then A/AAAA. Answers are kept for 60 s; a lookup gives up after 3 s.
pub fn resolve(server: &str) -> Result<SocketAddr> {
    static RESOLVER: OnceLock<Resolver<SystemDns>> = OnceLock::new();
    RESOLVER.get_or_init(|| Resolver::new(SystemDns::default())).resolve(server, Instant::now())
}

trait Lookup {
    fn srv(&self, name: &str, deadline: Instant) -> Result<Option<(String, u16)>>;
    fn ips(&self, host: &str, deadline: Instant) -> Result<Vec<IpAddr>>;
}

struct Resolver<L> {
    lookup: L,
    cache: Mutex<HashMap<String, (Instant, SocketAddr)>>,
}

impl<L: Lookup> Resolver<L> {
    fn new(lookup: L) -> Self {
        Self { lookup, cache: Mutex::default() }
    }

    fn resolve(&self, server: &str, now: Instant) -> Result<SocketAddr> {
        let key = server.trim().to_ascii_lowercase();
        if let Some((at, addr)) = lock(&self.cache).get(&key).copied()
            && now.saturating_duration_since(at) < RESOLVE_CACHE
        {
            return Ok(addr);
        }
        let addr = resolve_with(&self.lookup, server, now + RESOLVE_TIMEOUT)?;
        lock(&self.cache).insert(key, (now, addr));
        Ok(addr)
    }
}

fn split_server(server: &str) -> Result<(String, Option<u16>)> {
    let s = server.trim();
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p != 0).with_context(|| format!("{s}: {p} is not a port"));
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']').with_context(|| format!("{s}: missing ]"))?;
        match after.strip_prefix(':') {
            Some(p) => (host, Some(port(p)?)),
            None if after.is_empty() => (host, None),
            None => bail!("{s} is not a server address"),
        }
    } else {
        match s.rsplit_once(':') {
            Some((host, p)) if !host.contains(':') => (host, Some(port(p)?)),
            _ => (s, None),
        }
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        bail!("{s:?} is not a server address");
    }
    Ok((host.to_owned(), port))
}

fn resolve_with(lookup: &impl Lookup, server: &str, deadline: Instant) -> Result<SocketAddr> {
    let (host, port) = split_server(server)?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port.unwrap_or(DEFAULT_PORT)));
    }
    let (target, port) = match port {
        Some(port) => (host, port),
        None => lookup.srv(&format!("_minecraft._tcp.{host}"), deadline).ok().flatten().unwrap_or((host, DEFAULT_PORT)),
    };
    if let Ok(ip) = target.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let ip = lookup.ips(&target, deadline)?.into_iter().next().with_context(|| format!("{target} has no address"))?;
    Ok(SocketAddr::new(ip, port))
}

#[derive(Default)]
struct SystemDns(OnceLock<Result<Dns, String>>);

impl SystemDns {
    fn dns(&self) -> Result<&Dns> {
        self.0.get_or_init(|| Dns::new().map_err(|e| format!("{e:#}"))).as_ref().map_err(|e| anyhow!("name lookup is not available: {e}"))
    }
}

impl Lookup for SystemDns {
    fn srv(&self, name: &str, deadline: Instant) -> Result<Option<(String, u16)>> {
        self.dns()?.srv(name, deadline)
    }

    fn ips(&self, host: &str, deadline: Instant) -> Result<Vec<IpAddr>> {
        self.dns()?.ips(host, deadline)
    }
}

struct Dns {
    runtime: tokio::runtime::Runtime,
    resolver: TokioResolver,
}

impl Dns {
    fn new() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let mut builder = TokioResolver::builder_tokio()?;
        builder.options_mut().timeout = RESOLVE_TIMEOUT;
        let resolver = {
            let _context = runtime.enter();
            builder.build()?
        };
        Ok(Self { runtime, resolver })
    }

    fn srv(&self, name: &str, deadline: Instant) -> Result<Option<(String, u16)>> {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(Ok(found)) = self.runtime.block_on(tokio::time::timeout(left, self.resolver.srv_lookup(name))) else { return Ok(None) };
        let best = found.answers().iter().filter_map(|r| match &r.data {
            RData::SRV(srv) => Some(srv),
            _ => None,
        });
        Ok(best.min_by_key(|s| (s.priority, Reverse(s.weight))).map(|s| (s.target.to_string().trim_end_matches('.').to_owned(), s.port)))
    }

    fn ips(&self, host: &str, deadline: Instant) -> Result<Vec<IpAddr>> {
        let left = deadline.saturating_duration_since(Instant::now());
        let found = self
            .runtime
            .block_on(tokio::time::timeout(left, self.resolver.lookup_ip(host)))
            .map_err(|_| anyhow!("{host} did not answer within {} s", RESOLVE_TIMEOUT.as_secs()))?
            .with_context(|| format!("cannot find {host}"))?;
        Ok(found.iter().collect())
    }
}

#[derive(PartialEq, Eq)]
enum ProxyKind {
    Socks,
    Http,
}

struct Proxy {
    kind: ProxyKind,
    host: String,
    port: u16,
    user: Option<String>,
    pass: Option<String>,
}

fn parse_proxy(url: &str) -> Result<Proxy> {
    let (scheme, rest) = url.trim().split_once("://").context("a proxy looks like socks5://host:1080 or http://host:8080")?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "socks" | "socks5" | "socks5h" => ProxyKind::Socks,
        "http" | "https" => ProxyKind::Http,
        other => bail!("proxy type {other} is not supported"),
    };
    let rest = rest.trim_end_matches('/');
    let (auth, address) = match rest.rsplit_once('@') {
        Some((auth, address)) => (Some(auth), address),
        None => (None, rest),
    };
    let (user, pass) = match auth.map(|a| a.split_once(':').unwrap_or((a, ""))) {
        Some((user, pass)) => (Some(percent_decode(user)), Some(percent_decode(pass)).filter(|p| !p.is_empty())),
        None => (None, None),
    };
    let (host, port) = split_server(address).context("the proxy address is invalid")?;
    let port = port.unwrap_or(if kind == ProxyKind::Socks { 1080 } else { 80 });
    Ok(Proxy { kind, host, port, user, pass })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Extra {
    jvm: Vec<String>,
    game: Vec<String>,
}

fn extra_arguments(batch: &Batch, account: &Account, game_dir: &Path, ipc_address: &str, key: &str) -> Result<Extra> {
    let mut jvm = batch.jvm_args.clone();
    jvm.extend([
        format!("-Dmanycraft.ipc={ipc_address}"),
        format!("-Dmanycraft.instance={key}"),
        format!("-Dmanycraft.preview={}", game_dir.join(PREVIEW_FILE).display()),
        "-Dmanycraft.hidden=true".to_owned(),
    ]);
    let mut game = Vec::new();
    if let Some(url) = &account.proxy {
        let proxy = parse_proxy(url).with_context(|| format!("the proxy of {} is invalid", account.name))?;
        jvm.push(format!("-Dmanycraft.proxy={url}"));
        match proxy.kind {
            ProxyKind::Socks => {
                game.extend(["--proxyHost".to_owned(), proxy.host, "--proxyPort".to_owned(), proxy.port.to_string()]);
                if let Some(user) = proxy.user {
                    game.extend(["--proxyUser".to_owned(), user]);
                }
                if let Some(pass) = proxy.pass {
                    game.extend(["--proxyPass".to_owned(), pass]);
                }
            }
            ProxyKind::Http => jvm.extend([format!("-Dhttps.proxyHost={}", proxy.host), format!("-Dhttps.proxyPort={}", proxy.port)]),
        }
    }
    Ok(Extra { jvm, game })
}

fn spawn(spec: &LaunchSpec, extra_game: &[String], token: &str) -> Result<Child> {
    let logs = spec.game_dir.join("logs");
    fs::create_dir_all(&logs).with_context(|| format!("cannot create {}", logs.display()))?;
    let log_path = logs.join("manycraft-launch.log");
    let log = File::create(&log_path).with_context(|| format!("cannot write {}", log_path.display()))?;
    let log_err = log.try_clone().context("cannot write the launch log")?;
    let mut command = Command::new(&spec.installed.java);
    command.args(spec.arguments()?.as_slice()).args(extra_game).current_dir(&spec.game_dir).env(TOKEN_VAR, token);
    command.stdin(Stdio::null()).stdout(log).stderr(log_err);
    command.spawn().with_context(|| format!("cannot start {}", spec.installed.java.display()))
}

fn sync_mods(_batch: &Batch) -> Result<()> {
    Ok(())
}

/// Key of the instance of `account` in `batch`, used for IPC and the fleet.
pub fn instance_key(batch: &Batch, account: Uuid) -> String {
    format!("{}-{}", batch.id, account.simple())
}

trait Process: Send {
    fn id(&self) -> u32;
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>>;
    fn kill(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<Option<i32>>;
}

impl Process for Child {
    fn id(&self) -> u32 {
        Child::id(self)
    }

    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        Ok(Child::try_wait(self)?.map(|s| s.code()))
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }

    fn wait(&mut self) -> io::Result<Option<i32>> {
        Ok(Child::wait(self)?.code())
    }
}

trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A started game process.
pub struct RunningInstance {
    pub batch: String,
    pub account: Uuid,
    child: Box<dyn Process>,
    pub pid: u32,
    pub server_addr: Option<SocketAddr>,
    pub started: Instant,
    exit: Option<Option<i32>>,
}

impl RunningInstance {
    fn poll(&mut self) -> bool {
        if self.exit.is_none() {
            match self.child.try_wait() {
                Ok(None) => return true,
                Ok(Some(code)) => self.exit = Some(code),
                Err(_) => self.exit = Some(None),
            }
        }
        false
    }
}

/// Outcome of starting one account: its instance key, or why it did not start.
pub type Started = (Uuid, Result<String>);

/// The running instances and the IPC server their companion mods connect to.
pub struct Fleet {
    ipc: ipc::Server,
    running: Arc<Mutex<HashMap<String, RunningInstance>>>,
    reaping: Arc<AtomicBool>,
    /// Pause between two starts.
    pub stagger: Duration,
}

impl Fleet {
    /// A fleet with its IPC server listening; IPC events arrive on the returned receiver.
    pub fn new() -> Result<(Fleet, Receiver<ipc::Event>)> {
        let (ipc, events) = ipc::Server::start()?;
        Ok((Fleet { ipc, running: Arc::default(), reaping: Arc::default(), stagger: DEFAULT_STAGGER }, events))
    }

    /// Installs the batch's game, then prepares and starts every account `stagger` apart. Blocks until the last one is started.
    /// Fails as a whole when the game cannot be installed or the server not found; otherwise reports each account.
    pub fn start_batch(&mut self, batch: &Batch, accounts: &[Account], companion_jar: Option<&Path>, progress: &(dyn Fn(install::Progress) + Sync)) -> Result<Vec<Started>> {
        sync_mods(batch)?;
        let server_addr = batch.server.as_deref().map(|s| resolve(s).with_context(|| format!("cannot find the server {s}"))).transpose()?;
        let installed = install::install(&batch.game_version, batch.loader_version.as_deref(), progress)?;
        let ipc_address = self.ipc.address();
        let launch = |account: &Account, key: &str, token: &str| -> Result<Box<dyn Process>> {
            Ok(Box::new(launch_one(batch, account, companion_jar, &installed, &ipc_address, key, token)?))
        };
        Ok(self.start_all(batch, accounts, server_addr, &SystemClock, launch))
    }

    fn start_all(&mut self, batch: &Batch, accounts: &[Account], server_addr: Option<SocketAddr>, clock: &dyn Clock, mut launch: impl FnMut(&Account, &str, &str) -> Result<Box<dyn Process>>) -> Vec<Started> {
        let mut last: Option<Instant> = None;
        let mut outcomes = Vec::with_capacity(accounts.len());
        for account in accounts {
            let key = instance_key(batch, account.id);
            let result = self.check_free(&key, account, server_addr).and_then(|()| {
                if let Some(at) = last {
                    let wait = self.stagger.saturating_sub(clock.now().saturating_duration_since(at));
                    if !wait.is_zero() {
                        clock.sleep(wait);
                    }
                }
                let started = clock.now();
                last = Some(started);
                let token = self.ipc.register(&key);
                let child = launch(account, &key, &token).inspect_err(|_| self.ipc.unregister(&key))?;
                let pid = child.id();
                self.insert(key.clone(), RunningInstance { batch: batch.id.clone(), account: account.id, child, pid, server_addr, started, exit: None });
                Ok(key.clone())
            });
            outcomes.push((account.id, result));
        }
        outcomes
    }

    fn check_free(&self, key: &str, account: &Account, server_addr: Option<SocketAddr>) -> Result<()> {
        let mut running = lock(&self.running);
        if running.get_mut(key).is_some_and(RunningInstance::poll) {
            bail!("{} is already running in this batch", account.name);
        }
        if let Some(addr) = server_addr
            && running.values_mut().any(|r| r.account == account.id && r.server_addr == Some(addr) && r.poll())
        {
            bail!("{} is already on the server {addr} in another batch", account.name);
        }
        Ok(())
    }

    fn insert(&self, key: String, instance: RunningInstance) {
        let mut running = lock(&self.running);
        running.insert(key, instance);
        if !self.reaping.swap(true, Ordering::SeqCst) && self.spawn_reaper().is_err() {
            self.reaping.store(false, Ordering::SeqCst);
        }
    }

    fn spawn_reaper(&self) -> io::Result<()> {
        let running = Arc::downgrade(&self.running);
        let reaping = Arc::clone(&self.reaping);
        let ipc = self.ipc.clone();
        std::thread::Builder::new().name("fleet-reaper".into()).spawn(move || {
            loop {
                std::thread::sleep(REAP_INTERVAL);
                let Some(running) = running.upgrade() else { return };
                let mut running = lock(&running);
                let mut alive = false;
                for (key, instance) in running.iter_mut().filter(|(_, r)| r.exit.is_none()) {
                    if instance.poll() {
                        alive = true;
                    } else {
                        ipc.unregister(key);
                    }
                }
                if !alive {
                    reaping.store(false, Ordering::SeqCst);
                    return;
                }
            }
        })?;
        Ok(())
    }

    /// Whether the instance `key` is running right now.
    pub fn is_running(&self, key: &str) -> bool {
        lock(&self.running).get_mut(key).is_some_and(RunningInstance::poll)
    }

    /// The exit code of the instance `key` once it ended; `Some(None)` when it ended without one.
    pub fn exit_code(&self, key: &str) -> Option<Option<i32>> {
        let mut running = lock(&self.running);
        let instance = running.get_mut(key)?;
        instance.poll();
        instance.exit
    }

    /// Keys of all instances that are running.
    pub fn running_keys(&self) -> Vec<String> {
        lock(&self.running).iter_mut().filter_map(|(k, r)| r.poll().then(|| k.clone())).collect()
    }

    /// Asks the instance `key` to quit, and ends it when it has not after a few seconds.
    pub fn stop(&self, key: &str) -> Result<()> {
        if !lock(&self.running).contains_key(key) {
            bail!("instance {key} was not started");
        }
        self.stop_all(&[key.to_owned()]);
        Ok(())
    }

    /// Stops every instance of the batch `batch_id`.
    pub fn stop_batch(&self, batch_id: &str) {
        let keys: Vec<String> = lock(&self.running).iter().filter(|(_, r)| r.batch == batch_id).map(|(k, _)| k.clone()).collect();
        self.stop_all(&keys);
    }

    fn stop_all(&self, keys: &[String]) {
        let asked: Vec<&String> = keys.iter().filter(|k| self.is_running(k) && self.ipc.send(k, &ipc::ToMod::Quit).is_ok()).collect();
        let deadline = Instant::now() + QUIT_GRACE;
        while asked.iter().any(|k| self.is_running(k)) && Instant::now() < deadline {
            std::thread::sleep(QUIT_POLL);
        }
        let mut running = lock(&self.running);
        for key in keys {
            if let Some(instance) = running.get_mut(key)
                && instance.poll()
            {
                let _ = instance.child.kill();
                instance.exit = Some(instance.child.wait().ok().flatten());
            }
            self.ipc.unregister(key);
        }
    }
}

fn launch_one(batch: &Batch, account: &Account, companion_jar: Option<&Path>, installed: &Installed, ipc_address: &str, key: &str, token: &str) -> Result<Child> {
    let game_dir = prepare(batch, account, companion_jar)?;
    if account.proxy.is_some() && !batch.mods_dir().join(COMPANION_FILE).is_file() {
        bail!("{} has a proxy, but the companion mod is missing, so the game would connect without the proxy", account.name);
    }
    let extra = extra_arguments(batch, account, &game_dir, ipc_address, key)?;
    let session = match account.kind {
        AccountKind::Offline => {
            account::validate_offline_name(&account.name)?;
            None
        }
        AccountKind::Microsoft => Some(launch::Session::from(auth::session_for(account)?)),
    };
    let spec = LaunchSpec {
        installed: installed.clone(),
        game_dir,
        session,
        offline_name: account.name.clone(),
        memory_mb: batch.memory_mb,
        server: batch.server.clone(),
        extra_jvm: extra.jvm,
    };
    spawn(&spec, &extra.game, token)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn catch_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn on_ctrl(_kind: u32) -> i32 {
        STOP.store(true, Ordering::SeqCst);
        1
    }
    unsafe {
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(windows))]
fn catch_ctrl_c() {}

/// `manycraft batch`: creates, edits and starts batches from the terminal.
pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["create", name, flags @ ..] => create(name, flags),
        ["list"] => list(),
        ["add", batch, account] => edit(batch, account, true),
        ["remove", batch, account] => edit(batch, account, false),
        ["start", batch, flags @ ..] => start(batch, flags),
        ["delete", batch] => delete(batch),
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn flags<'a>(args: &[&'a str], allowed: &[&str]) -> Option<HashMap<&'a str, &'a str>> {
    let mut found = HashMap::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?;
        if !allowed.contains(flag) || found.insert(*flag, *value).is_some() {
            return None;
        }
    }
    Some(found)
}

fn find_batch<'a>(batches: &'a mut [Batch], who: &str) -> Result<&'a mut Batch> {
    let count = batches.iter().filter(|b| b.id == who || b.name.eq_ignore_ascii_case(who)).count();
    if count > 1 {
        bail!("several batches are named {who}, use the id");
    }
    batches.iter_mut().find(|b| b.id == who || b.name.eq_ignore_ascii_case(who)).with_context(|| format!("no batch {who}"))
}

fn create(name: &str, args: &[&str]) -> Result<i32> {
    let Some(flags) = flags(args, &["--version", "--server", "--memory"]) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let name = name.trim();
    if name.is_empty() {
        bail!("a batch needs a name");
    }
    let batches = batch::load_all()?;
    if batches.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        bail!("a batch named {name} exists already");
    }
    let version = match flags.get("--version") {
        Some(v) => (*v).to_owned(),
        None => install::latest_release()?,
    };
    let mut new = Batch::new(name, &version);
    if let Some(server) = flags.get("--server") {
        split_server(server)?;
        new.server = Some((*server).to_owned());
    }
    if let Some(memory) = flags.get("--memory") {
        new.memory_mb = memory.parse().ok().filter(|m| *m >= MIN_MEMORY_MB).with_context(|| format!("memory is a number of MB, at least {MIN_MEMORY_MB}"))?;
    }
    batch::save(&new)?;
    println!("Batch {} ({}) for Minecraft {}", new.name, new.id, new.game_version);
    Ok(0)
}

fn list() -> Result<i32> {
    let accounts = account::load()?;
    let name = |id: &Uuid| accounts.iter().find(|a| a.id == *id).map_or_else(|| id.to_string(), |a| a.name.clone());
    for b in batch::load_all()? {
        let members: Vec<String> = b.accounts.iter().map(name).collect();
        println!("{} ({})  Minecraft {}  server {}  {} MB  accounts: {}", b.name, b.id, b.game_version, b.server.as_deref().unwrap_or("-"), b.memory_mb, if members.is_empty() { "-".to_owned() } else { members.join(", ") });
    }
    Ok(0)
}

fn edit(who: &str, account_name: &str, add: bool) -> Result<i32> {
    let mut batches = batch::load_all()?;
    let target = find_batch(&mut batches, who)?;
    let accounts = account::load()?;
    let account = auth::find_account(&accounts, account_name)?;
    let present = target.accounts.contains(&account.id);
    match (add, present) {
        (true, true) => bail!("{} is in {} already", account.name, target.name),
        (false, false) => bail!("{} is not in {}", account.name, target.name),
        (true, false) => target.accounts.push(account.id),
        (false, true) => target.accounts.retain(|id| *id != account.id),
    }
    batch::save(target)?;
    println!("{} {} {}", if add { "Added" } else { "Removed" }, account.name, if add { format!("to {}", target.name) } else { format!("from {}", target.name) });
    Ok(0)
}

fn delete(who: &str) -> Result<i32> {
    let mut batches = batch::load_all()?;
    let target = find_batch(&mut batches, who)?;
    batch::delete(target)?;
    println!("Deleted batch {}; instance folders are kept", target.name);
    Ok(0)
}

fn start(who: &str, args: &[&str]) -> Result<i32> {
    let Some(flags) = flags(args, &["--companion", "--stagger"]) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let mut batches = batch::load_all()?;
    let target = find_batch(&mut batches, who)?.clone();
    if target.accounts.is_empty() {
        bail!("{} has no accounts yet: manycraft batch add {} <account>", target.name, target.name);
    }
    let saved = account::load()?;
    let accounts = target
        .accounts
        .iter()
        .map(|id| saved.iter().find(|a| a.id == *id).cloned().with_context(|| format!("account {id} of {} is no longer saved", target.name)))
        .collect::<Result<Vec<_>>>()?;
    let (mut fleet, events) = Fleet::new()?;
    if let Some(seconds) = flags.get("--stagger") {
        fleet.stagger = Duration::from_secs_f64(seconds.parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0).context("--stagger is a number of seconds")?);
    }
    catch_ctrl_c();
    let report = |p: install::Progress| {
        if p.total > 0 && p.done == p.total {
            println!("{}: {}/{}", p.stage.label(), p.done, p.total);
        }
    };
    let names: HashMap<String, String> = accounts.iter().map(|a| (instance_key(&target, a.id), a.name.clone())).collect();
    let mut live = Vec::new();
    for (id, outcome) in fleet.start_batch(&target, &accounts, flags.get("--companion").map(Path::new), &report)? {
        let name = accounts.iter().find(|a| a.id == id).map_or("?", |a| a.name.as_str());
        match outcome {
            Ok(key) => {
                println!("{name} started");
                live.push(key);
            }
            Err(e) => eprintln!("{name} did not start: {e:#}"),
        }
    }
    println!("Press Ctrl+C to stop all");
    while !live.is_empty() && !STOP.load(Ordering::SeqCst) {
        match events.recv_timeout(EVENT_POLL) {
            Ok(event) => print_event(&names, &event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        live.retain(|key| {
            let running = fleet.is_running(key);
            if !running {
                let code = fleet.exit_code(key).flatten().map_or_else(|| "none".to_owned(), |c| c.to_string());
                println!("{} exited (code {code})", names.get(key).map_or(key.as_str(), String::as_str));
            }
            running
        });
    }
    fleet.stop_batch(&target.id);
    println!("Stopped {}", target.name);
    Ok(0)
}

fn print_event(names: &HashMap<String, String>, event: &ipc::Event) {
    let name = |key: &str| names.get(key).map_or(key, String::as_str).to_owned();
    match event {
        ipc::Event::Connected { instance, pid, mod_version } => println!("{} connected (pid {pid}, mod {mod_version})", name(instance)),
        ipc::Event::Message { instance, message } => println!("{}: {message:?}", name(instance)),
        ipc::Event::Disconnected { instance } => println!("{} disconnected", name(instance)),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    struct Dirs {
        _root: tempfile::TempDir,
        inst: PathBuf,
        mods: PathBuf,
        config: PathBuf,
        template: PathBuf,
    }

    fn dirs() -> Dirs {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().to_path_buf();
        Dirs { inst: base.join("inst"), mods: base.join("batch").join("mods"), config: base.join("batch").join("config"), template: base.join("batch").join(OPTIONS_FILE), _root: root }
    }

    fn prep(d: &Dirs, jar: Option<&Path>) -> Result<()> {
        prepare_in(&d.inst, &d.mods, &d.config, &d.template, jar)
    }

    #[test]
    fn links_point_into_the_batch_and_are_reused() {
        let d = dirs();
        prep(&d, None).unwrap();
        fs::write(d.inst.join("mods").join("a.jar"), b"x").unwrap();
        fs::write(d.inst.join("config").join("c.txt"), b"y").unwrap();
        assert_eq!(fs::read(d.mods.join("a.jar")).unwrap(), b"x");
        assert_eq!(fs::read(d.config.join("c.txt")).unwrap(), b"y");
        prep(&d, None).unwrap();
        assert!(!d.inst.join("mods.old-1").exists());
        assert_eq!(fs::read(d.inst.join("mods").join("a.jar")).unwrap(), b"x");
    }

    #[test]
    fn a_real_folder_is_moved_aside_once_and_never_deleted() {
        let d = dirs();
        fs::create_dir_all(d.inst.join("mods")).unwrap();
        fs::write(d.inst.join("mods").join("mine.jar"), b"keep").unwrap();
        fs::create_dir_all(d.inst.join("mods.old-1")).unwrap();
        fs::write(d.inst.join("config"), b"a file").unwrap();
        prep(&d, None).unwrap();
        assert_eq!(fs::read(d.inst.join("mods.old-2").join("mine.jar")).unwrap(), b"keep");
        assert_eq!(fs::read(d.inst.join("config.old-1")).unwrap(), b"a file");
        assert!(points_to(&d.inst.join("mods"), &d.mods));
        assert!(points_to(&d.inst.join("config"), &d.config));
        prep(&d, None).unwrap();
        assert!(!d.inst.join("mods.old-3").exists());
        assert!(!d.inst.join("config.old-2").exists());
    }

    #[test]
    fn a_link_to_another_folder_is_replaced_and_the_old_folder_kept() {
        let d = dirs();
        let other = d.inst.parent().unwrap().join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("old.jar"), b"old").unwrap();
        fs::create_dir_all(&d.inst).unwrap();
        create_link(&other, &d.inst.join("mods")).unwrap();
        prep(&d, None).unwrap();
        assert!(points_to(&d.inst.join("mods"), &d.mods));
        assert_eq!(fs::read(other.join("old.jar")).unwrap(), b"old");
        assert!(!d.inst.join("mods.old-1").exists());
    }

    #[test]
    fn a_dangling_link_is_replaced() {
        let d = dirs();
        let gone = d.inst.parent().unwrap().join("gone");
        fs::create_dir_all(&gone).unwrap();
        fs::create_dir_all(&d.inst).unwrap();
        create_link(&gone, &d.inst.join("mods")).unwrap();
        fs::remove_dir(&gone).unwrap();
        prep(&d, None).unwrap();
        assert!(points_to(&d.inst.join("mods"), &d.mods));
    }

    #[test]
    fn options_come_from_the_template_on_every_start() {
        let d = dirs();
        prep(&d, None).unwrap();
        assert!(!d.inst.join(OPTIONS_FILE).exists());
        fs::write(d.inst.join(OPTIONS_FILE), "fov:1.0\n").unwrap();
        prep(&d, None).unwrap();
        assert_eq!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap(), "fov:1.0\n");
        fs::write(&d.template, "soundCategory_master:0.0\nonboardAccessibility:false\n").unwrap();
        prep(&d, None).unwrap();
        assert_eq!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap(), "soundCategory_master:0.0\nonboardAccessibility:false\n");
        fs::write(d.inst.join(OPTIONS_FILE), "soundCategory_master:1.0\n").unwrap();
        prep(&d, None).unwrap();
        assert!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap().contains("soundCategory_master:0.0"));
    }

    #[test]
    fn per_instance_keys_survive_the_template() {
        let merged = merge_options("a:1\nb:2\n", Some("b:9\nc:3\nd:4\n"), &["b", "c"]);
        assert_eq!(merged, "a:1\nb:9\nc:3\n");
        assert_eq!(merge_options("a:1\n", None, &["a"]), "a:1\n");
        assert_eq!(merge_options("a:1\n", Some("a:5\n"), &[]), "a:1\n");
    }

    #[test]
    fn companion_goes_into_the_shared_mods_only_when_missing_or_different() {
        let d = dirs();
        let jar = d.inst.parent().unwrap().join("companion.jar");
        fs::write(&jar, b"v1").unwrap();
        prep(&d, Some(&jar)).unwrap();
        let dest = d.mods.join(COMPANION_FILE);
        assert_eq!(fs::read(&dest).unwrap(), b"v1");
        let stamp = fs::metadata(&dest).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        prep(&d, Some(&jar)).unwrap();
        assert_eq!(fs::metadata(&dest).unwrap().modified().unwrap(), stamp);
        fs::write(&jar, b"v2").unwrap();
        prep(&d, Some(&jar)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"v2");
        assert!(prep(&d, Some(&jar.with_file_name("missing.jar"))).is_err());
    }

    #[derive(Default)]
    struct FakeDns {
        srv: HashMap<String, (String, u16)>,
        ips: HashMap<String, Vec<IpAddr>>,
        calls: Cell<u32>,
    }

    impl Lookup for FakeDns {
        fn srv(&self, name: &str, _deadline: Instant) -> Result<Option<(String, u16)>> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.srv.get(name).cloned())
        }

        fn ips(&self, host: &str, _deadline: Instant) -> Result<Vec<IpAddr>> {
            self.calls.set(self.calls.get() + 1);
            self.ips.get(host).cloned().with_context(|| format!("cannot find {host}"))
        }
    }

    fn fake_dns() -> FakeDns {
        let mut dns = FakeDns::default();
        dns.srv.insert("_minecraft._tcp.example.org".into(), ("mc.example.org.net".into(), 25570));
        dns.ips.insert("mc.example.org.net".into(), vec!["10.0.0.7".parse().unwrap()]);
        dns.ips.insert("example.org".into(), vec!["10.0.0.1".parse().unwrap(), "::1".parse().unwrap()]);
        dns.ips.insert("plain.org".into(), vec!["10.0.0.2".parse().unwrap()]);
        dns
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ip_literals_pass_through_without_lookups() {
        let dns = fake_dns();
        let at = Instant::now();
        assert_eq!(resolve_with(&dns, "127.0.0.1:25565", at).unwrap(), addr("127.0.0.1:25565"));
        assert_eq!(resolve_with(&dns, "127.0.0.1", at).unwrap(), addr("127.0.0.1:25565"));
        assert_eq!(resolve_with(&dns, "[::1]:25570", at).unwrap(), addr("[::1]:25570"));
        assert_eq!(resolve_with(&dns, "::1", at).unwrap(), addr("[::1]:25565"));
        assert_eq!(resolve_with(&dns, "[::1]", at).unwrap(), addr("[::1]:25565"));
        assert_eq!(dns.calls.get(), 0);
    }

    #[test]
    fn srv_only_without_a_port_and_a_records_otherwise() {
        let dns = fake_dns();
        let at = Instant::now();
        assert_eq!(resolve_with(&dns, "example.org", at).unwrap(), addr("10.0.0.7:25570"));
        assert_eq!(resolve_with(&dns, "example.org:25565", at).unwrap(), addr("10.0.0.1:25565"));
        assert_eq!(resolve_with(&dns, "plain.org", at).unwrap(), addr("10.0.0.2:25565"));
        assert!(resolve_with(&dns, "nowhere.org", at).is_err());
    }

    #[test]
    fn malformed_servers_are_refused() {
        for bad in ["", "host:", "host:0", "host:99999", "host:x", "[::1", "[::1]x", "a b:1"] {
            assert!(split_server(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn answers_are_cached_for_a_minute() {
        let resolver = Resolver::new(fake_dns());
        let t0 = Instant::now();
        assert_eq!(resolver.resolve("plain.org", t0).unwrap(), addr("10.0.0.2:25565"));
        let after_first = resolver.lookup.calls.get();
        resolver.resolve("PLAIN.org", t0 + Duration::from_secs(59)).unwrap();
        assert_eq!(resolver.lookup.calls.get(), after_first);
        resolver.resolve("plain.org", t0 + Duration::from_secs(61)).unwrap();
        assert!(resolver.lookup.calls.get() > after_first);
    }

    struct FakeProcess {
        pid: u32,
        exit: Arc<Mutex<Option<Option<i32>>>>,
    }

    impl Process for FakeProcess {
        fn id(&self) -> u32 {
            self.pid
        }

        fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
            Ok(*lock(&self.exit))
        }

        fn kill(&mut self) -> io::Result<()> {
            lock(&self.exit).get_or_insert(Some(1));
            Ok(())
        }

        fn wait(&mut self) -> io::Result<Option<i32>> {
            Ok(lock(&self.exit).flatten())
        }
    }

    struct FakeClock {
        start: Instant,
        offset: Cell<Duration>,
        slept: RefCell<Vec<Duration>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self { start: Instant::now(), offset: Cell::new(Duration::ZERO), slept: RefCell::default() }
        }

        fn advance(&self, d: Duration) {
            self.offset.set(self.offset.get() + d);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.start + self.offset.get()
        }

        fn sleep(&self, d: Duration) {
            self.slept.borrow_mut().push(d);
            self.advance(d);
        }
    }

    type Exits = HashMap<Uuid, Arc<Mutex<Option<Option<i32>>>>>;

    fn launcher<'a>(exits: &'a RefCell<Exits>, tokens: &'a RefCell<Vec<String>>) -> impl FnMut(&Account, &str, &str) -> Result<Box<dyn Process>> + 'a {
        move |account, _key, token| {
            tokens.borrow_mut().push(token.to_owned());
            let exit = Arc::new(Mutex::new(None));
            exits.borrow_mut().insert(account.id, Arc::clone(&exit));
            Ok(Box::new(FakeProcess { pid: 1, exit }) as Box<dyn Process>)
        }
    }

    fn accounts(names: &[&str]) -> Vec<Account> {
        names.iter().map(|n| Account::offline(n).unwrap()).collect()
    }

    #[test]
    fn the_same_account_cannot_join_the_same_server_twice() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        let clock = FakeClock::new();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let alts = accounts(&["AltOne", "AltTwo"]);
        let a = Batch::new("A", "26.3");
        let b = Batch::new("B", "26.3");
        let c = Batch::new("C", "26.3");
        let server = Some(addr("10.0.0.1:25565"));
        let first = fleet.start_all(&a, &alts, server, &clock, launcher(&exits, &tokens));
        assert!(first.iter().all(|(_, r)| r.is_ok()));
        let again = fleet.start_all(&a, &alts[..1], server, &clock, launcher(&exits, &tokens));
        assert!(format!("{:#}", again[0].1.as_ref().unwrap_err()).contains("already running"));
        let dup = fleet.start_all(&b, &alts[..1], server, &clock, launcher(&exits, &tokens));
        assert!(format!("{:#}", dup[0].1.as_ref().unwrap_err()).contains("AltOne is already on the server 10.0.0.1:25565"));
        let elsewhere = fleet.start_all(&c, &alts[..1], Some(addr("10.0.0.2:25565")), &clock, launcher(&exits, &tokens));
        assert!(elsewhere[0].1.is_ok());
        lock(&exits.borrow()[&alts[0].id]).replace(Some(0));
        let after_exit = fleet.start_all(&b, &alts[..1], server, &clock, launcher(&exits, &tokens));
        assert!(after_exit[0].1.is_err(), "still on the server through batch C");
        assert_eq!(tokens.borrow().len(), 3);
    }

    #[test]
    fn starts_are_staggered_and_refusals_do_not_wait() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        let clock = FakeClock::new();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = Batch::new("A", "26.3");
        let alts = accounts(&["AltOne", "AltTwo", "AltThree"]);
        fleet.start_all(&batch, &alts[..1], None, &clock, launcher(&exits, &tokens));
        clock.advance(Duration::from_secs(10));
        let outcomes = fleet.start_all(&batch, &alts, None, &clock, launcher(&exits, &tokens));
        assert!(outcomes[0].1.is_err());
        assert!(outcomes[1].1.is_ok() && outcomes[2].1.is_ok());
        assert_eq!(*clock.slept.borrow(), vec![DEFAULT_STAGGER]);
        fleet.stagger = Duration::from_secs(5);
        let more = accounts(&["AltFour", "AltFive"]);
        clock.slept.borrow_mut().clear();
        fleet.start_all(&batch, &more, None, &clock, |a: &Account, k: &str, t: &str| {
            clock.advance(Duration::from_secs(2));
            launcher(&exits, &tokens)(a, k, t)
        });
        assert_eq!(*clock.slept.borrow(), vec![Duration::from_secs(3)]);
    }

    #[test]
    fn a_failed_launch_is_reported_and_unregistered() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        let batch = Batch::new("A", "26.3");
        let alts = accounts(&["AltOne"]);
        let outcomes = fleet.start_all(&batch, &alts, None, &FakeClock::new(), |_: &Account, _: &str, _: &str| Err(anyhow!("no java")));
        assert!(outcomes[0].1.is_err());
        assert!(!fleet.is_running(&instance_key(&batch, alts[0].id)));
        assert!(fleet.exit_code(&instance_key(&batch, alts[0].id)).is_none());
    }

    #[test]
    fn exits_are_tracked_and_stop_ends_the_rest() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = Batch::new("A", "26.3");
        let alts = accounts(&["AltOne", "AltTwo"]);
        fleet.stagger = Duration::ZERO;
        let keys: Vec<String> = fleet.start_all(&batch, &alts, None, &SystemClock, launcher(&exits, &tokens)).into_iter().map(|(_, r)| r.unwrap()).collect();
        assert!(fleet.is_running(&keys[0]) && fleet.is_running(&keys[1]));
        lock(&exits.borrow()[&alts[0].id]).replace(Some(3));
        let deadline = Instant::now() + Duration::from_secs(3);
        while fleet.reaping.load(Ordering::SeqCst) && lock(&fleet.running)[&keys[0]].exit.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(lock(&fleet.running)[&keys[0]].exit, Some(Some(3)));
        assert_eq!(fleet.running_keys(), vec![keys[1].clone()]);
        fleet.stop_batch(&batch.id);
        assert!(!fleet.is_running(&keys[1]));
        assert_eq!(fleet.exit_code(&keys[1]), Some(Some(1)));
        assert!(fleet.stop("nope").is_err());
    }

    #[test]
    fn proxies_become_game_and_jvm_arguments() {
        let batch = Batch::new("A", "26.3");
        let mut alt = Account::offline("AltOne").unwrap();
        alt.proxy = Some("socks5://us%40r:p%3Ass@10.1.1.1:1081".into());
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k").unwrap();
        assert_eq!(extra.game, ["--proxyHost", "10.1.1.1", "--proxyPort", "1081", "--proxyUser", "us@r", "--proxyPass", "p:ss"]);
        assert!(extra.jvm.contains(&"-Dmanycraft.proxy=socks5://us%40r:p%3Ass@10.1.1.1:1081".to_owned()));
        assert!(extra.jvm.contains(&"-Dmanycraft.hidden=true".to_owned()));
        assert!(extra.jvm.contains(&"-Dmanycraft.instance=k".to_owned()));
        assert!(extra.jvm.contains(&"-Dmanycraft.ipc=127.0.0.1:9".to_owned()));
        alt.proxy = Some("http://proxy.example:3128".into());
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k").unwrap();
        assert!(extra.game.is_empty());
        assert!(extra.jvm.contains(&"-Dhttps.proxyHost=proxy.example".to_owned()) && extra.jvm.contains(&"-Dhttps.proxyPort=3128".to_owned()));
        alt.proxy = None;
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k").unwrap();
        assert!(extra.game.is_empty() && !extra.jvm.iter().any(|a| a.contains("proxy")));
        alt.proxy = Some("ftp://x:1".into());
        assert!(extra_arguments(&batch, &alt, Path::new("inst"), "", "k").is_err());
    }

    #[test]
    fn proxy_defaults_and_errors_never_show_the_password() {
        let p = parse_proxy("socks5://host").unwrap();
        assert_eq!((p.host.as_str(), p.port, p.user, p.pass), ("host", 1080, None, None));
        let p = parse_proxy("SOCKS5://u@[::1]:9/").unwrap();
        assert_eq!((p.host.as_str(), p.port, p.user.as_deref(), p.pass), ("::1", 9, Some("u"), None));
        let err = format!("{:#}", parse_proxy("socks5://u:secret@host:x").err().unwrap());
        assert!(!err.contains("secret"), "{err}");
        assert!(parse_proxy("host:1080").is_err());
    }

    #[test]
    fn cli_flags_are_parsed() {
        let f = flags(&["--version", "26.3", "--memory", "4096"], &["--version", "--memory"]).unwrap();
        assert_eq!((f["--version"], f["--memory"]), ("26.3", "4096"));
        assert!(flags(&["--version"], &["--version"]).is_none());
        assert!(flags(&["--bogus", "1"], &["--version"]).is_none());
        assert!(flags(&["--version", "1", "--version", "2"], &["--version"]).is_none());
        let mut batches = vec![Batch::new("Test", "26.3"), Batch::new("Other", "26.3")];
        assert_eq!(find_batch(&mut batches, "test").unwrap().name, "Test");
        assert!(find_batch(&mut batches, "none").is_err());
    }
}
