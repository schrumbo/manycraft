//! Running games: preparing game folders from an instance, starting them staggered, tracking their processes.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::ResolverConfig;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;
use uuid::Uuid;

use super::account::{self, Account, AccountKind};
use super::batch::{self, Background, Batch};
use super::instance::{self, Instance, Loader};
use super::install::{self, Installed};
use super::launch::{self, LaunchSpec};
use super::proxy::{self, Kind};
use super::sync::{self, OPTIONS_FILE, PER_INSTANCE_KEYS};
use super::{auth, ipc, lock, net, paths, scrub};

/// Pause between two instance starts unless the caller sets another.
pub const DEFAULT_STAGGER: Duration = Duration::from_secs(3);
/// File name of the companion mod in a batch's shared mods folder.
pub const COMPANION_FILE: &str = "manycraft-companion.jar";

const DEFAULT_PORT: u16 = 25565;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
const SRV_TIMEOUT: Duration = Duration::from_secs(1);
const RESOLVE_CACHE: Duration = Duration::from_secs(60);
const REAP_INTERVAL: Duration = Duration::from_millis(500);
const QUIT_GRACE: Duration = Duration::from_secs(5);
const QUIT_POLL: Duration = Duration::from_millis(100);
const EVENT_POLL: Duration = Duration::from_millis(250);
const MIN_MEMORY_MB: u32 = 512;
const PREVIEW_FILE: &str = "manycraft-preview.bin";
const ICON_FILE: &str = "manycraft-icon.png";
const TOKEN_VAR: &str = "MANYCRAFT_TOKEN";
const PROXY_VAR: &str = "MANYCRAFT_PROXY";
const USAGE: &str = "usage: manycraft batch create <name> [--instance <instance>] [--version <version>] [--server <host:port>] [--memory <MB>]
       manycraft batch list
       manycraft batch add <batch> <account>
       manycraft batch remove <batch> <account>
       manycraft batch instance <batch> <instance>
       manycraft batch start <batch> [--companion <jar>] [--stagger <seconds>]
       manycraft batch delete <batch>";
const INSTANCE_USAGE: &str = "usage: manycraft instance create <name> [--version <version>] [--loader vanilla|fabric|quilt|forge|neoforge] [--memory <MB>]
       manycraft instance list
       manycraft instance play <instance> <account> [--companion <jar>]
       manycraft instance delete <instance>";
const SOLO: &str = "solo";

/// What a start plays: an instance, inside a batch or alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub instance: Instance,
    pub batch: Option<Batch>,
}

impl Target {
    /// `batch` playing `instance`.
    pub fn of_batch(batch: &Batch, instance: &Instance) -> Self {
        Self { instance: instance.clone(), batch: Some(batch.clone()) }
    }

    /// `instance` played alone.
    pub fn solo(instance: &Instance) -> Self {
        Self { instance: instance.clone(), batch: None }
    }

    /// Key of the game of `account`.
    pub fn key(&self, account: Uuid) -> String {
        match &self.batch {
            Some(batch) => instance_key(batch, account),
            None => solo_key(&self.instance, account),
        }
    }

    /// The batch id, or [`solo_owner`] when played alone, for progress and cancelling.
    pub fn owner(&self) -> String {
        self.batch.as_ref().map_or_else(|| solo_owner(&self.instance.id), |b| b.id.clone())
    }

    /// Memory each game gets.
    pub fn memory_mb(&self) -> u32 {
        self.batch.as_ref().map_or(self.instance.memory_mb, |b| b.memory(self.instance.memory_mb))
    }

    fn server(&self) -> Option<&str> {
        self.batch.as_ref().and_then(|b| b.server.as_deref())
    }

    fn label(&self) -> &str {
        self.batch.as_ref().map_or(&self.instance.name, |b| &b.name)
    }

    fn background(&self) -> Background {
        self.batch.as_ref().map_or_else(Background::default, |b| b.background)
    }
}

struct Shared {
    mods: PathBuf,
    config: PathBuf,
    resourcepacks: PathBuf,
    shaderpacks: PathBuf,
    options: PathBuf,
    servers: PathBuf,
}

impl Shared {
    fn of(instance: &Instance) -> Self {
        Self {
            mods: instance.mods_dir(),
            config: instance.config_dir(),
            resourcepacks: instance.resourcepacks_dir(),
            shaderpacks: instance.shaderpacks_dir(),
            options: instance.options_template(),
            servers: instance.servers_template(),
        }
    }
}

/// Readies the game folder of `account` in `instance` and returns it: shared `mods`, `config`, `resourcepacks` and
/// `shaderpacks` linked in, `options.txt` and `servers.dat` from the instance templates, the companion mod in the shared mods folder.
pub fn prepare(instance: &Instance, account: &Account, companion_jar: Option<&Path>) -> Result<PathBuf> {
    let dir = instance.game_dir(account.id);
    prepare_in(&dir, &Shared::of(instance), companion_jar, &instance.other_games(account.id))?;
    Ok(dir)
}

fn prepare_in(dir: &Path, shared: &Shared, companion_jar: Option<&Path>, siblings: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for (name, target, merge) in [("mods", &shared.mods, false), ("config", &shared.config, false), ("resourcepacks", &shared.resourcepacks, true), ("shaderpacks", &shared.shaderpacks, true)] {
        fs::create_dir_all(target).with_context(|| format!("cannot create {}", target.display()))?;
        if merge {
            merge_into(&dir.join(name), target)?;
        }
        link(&dir.join(name), target)?;
    }
    if let Some(jar) = companion_jar {
        install_companion(jar, &shared.mods)?;
    }
    sync::adopt_options(&shared.options, dir, siblings)?;
    copy_options(&shared.options, &dir.join(OPTIONS_FILE))?;
    sync::copy_servers(&shared.servers, dir, siblings)
}

fn merge_into(path: &Path, target: &Path) -> Result<()> {
    let real_dir = fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) && !is_junction(path);
    if !real_dir {
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("cannot read {}", path.display()))? {
        let entry = entry.with_context(|| format!("cannot read {}", path.display()))?;
        let dest = target.join(entry.file_name());
        if fs::symlink_metadata(&dest).is_err() {
            let _ = fs::rename(entry.path(), &dest);
        }
    }
    if fs::read_dir(path).is_ok_and(|mut d| d.next().is_none()) {
        let _ = fs::remove_dir(path);
    }
    Ok(())
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
    let existing = fs::read_to_string(dest).ok();
    net::write_atomic(dest, merge_options(&text, existing.as_deref(), PER_INSTANCE_KEYS).as_bytes())
}

fn merge_options(template: &str, existing: Option<&str>, keys: &[&str]) -> String {
    let key = |line: &str| line.split_once(':').map(|(k, _)| k.to_owned());
    let in_template: Vec<String> = template.lines().filter_map(key).collect();
    let kept: Vec<(String, &str)> = existing.unwrap_or_default().lines().filter_map(|l| key(l).filter(|k| keys.contains(&k.as_str()) || !in_template.contains(k)).map(|k| (k, l))).collect();
    let own: HashMap<&str, &str> = kept.iter().map(|(k, l)| (k.as_str(), *l)).collect();
    let mut out: Vec<&str> = template.lines().map(|l| key(l).and_then(|k| own.get(k.as_str()).copied()).unwrap_or(l)).collect();
    out.extend(kept.iter().filter(|(k, _)| !in_template.contains(k)).map(|(_, l)| *l));
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
    match net::write_atomic(&dest, &bytes) {
        Err(_) if dest.is_file() => Ok(()),
        result => result,
    }
}

/// The address of `server` ("host", "host:port", an IP or "[v6]:port"): a SRV record `_minecraft._tcp.<host>`
/// when no port is given, then A/AAAA. Answers are kept for 60 s; a lookup gives up after 3 s.
pub fn resolve(server: &str) -> Result<SocketAddr> {
    resolve_all(server)?.into_iter().next().with_context(|| format!("{server} has no address"))
}

fn resolve_all(server: &str) -> Result<Vec<SocketAddr>> {
    static RESOLVER: OnceLock<Resolver<SystemDns>> = OnceLock::new();
    RESOLVER.get_or_init(|| Resolver::new(SystemDns::default())).resolve(server, Instant::now())
}

trait Lookup {
    fn srv(&self, name: &str, deadline: Instant) -> Result<Option<(String, u16)>>;
    fn ips(&self, host: &str, deadline: Instant) -> Result<Vec<IpAddr>>;
}

struct Resolver<L> {
    lookup: L,
    cache: Mutex<HashMap<String, (Instant, Vec<SocketAddr>)>>,
}

impl<L: Lookup> Resolver<L> {
    fn new(lookup: L) -> Self {
        Self { lookup, cache: Mutex::default() }
    }

    fn resolve(&self, server: &str, now: Instant) -> Result<Vec<SocketAddr>> {
        let key = server.trim().to_ascii_lowercase();
        if let Some((at, addrs)) = lock(&self.cache).get(&key)
            && now.saturating_duration_since(*at) < RESOLVE_CACHE
        {
            return Ok(addrs.clone());
        }
        let addrs = resolve_with(&self.lookup, server, now)?;
        lock(&self.cache).insert(key, (now, addrs.clone()));
        Ok(addrs)
    }
}

pub(super) fn split_server(server: &str) -> Result<(String, Option<u16>)> {
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

fn resolve_with(lookup: &impl Lookup, server: &str, start: Instant) -> Result<Vec<SocketAddr>> {
    let (host, port) = split_server(server)?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port.unwrap_or(DEFAULT_PORT))]);
    }
    let (target, port) = match port {
        Some(port) => (host, port),
        None => lookup.srv(&format!("_minecraft._tcp.{host}"), start + SRV_TIMEOUT).ok().flatten().unwrap_or((host, DEFAULT_PORT)),
    };
    if let Ok(ip) = target.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<SocketAddr> = lookup.ips(&target, start + RESOLVE_TIMEOUT)?.into_iter().map(|ip| SocketAddr::new(ip, port)).collect();
    if addrs.is_empty() {
        bail!("{target} has no address");
    }
    Ok(addrs)
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
        let (config, mut options) = hickory_resolver::system_conf::read_system_conf()?;
        let all = config.name_servers().to_vec();
        let usable: Vec<_> = all.iter().filter(|n| usable_nameserver(n.ip)).cloned().collect();
        let config = ResolverConfig::from_parts(config.domain().cloned(), config.search().to_vec(), if usable.is_empty() { all } else { usable });
        options.timeout = SRV_TIMEOUT;
        let mut builder = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
        *builder.options_mut() = options;
        let resolver = {
            let _context = runtime.enter();
            builder.build()?
        };
        Ok(Self { runtime, resolver })
    }

    fn srv(&self, name: &str, deadline: Instant) -> Result<Option<(String, u16)>> {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(Ok(found)) = self.runtime.block_on(async { tokio::time::timeout(left, self.resolver.srv_lookup(name)).await }) else { return Ok(None) };
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
            .block_on(async { tokio::time::timeout(left, self.resolver.lookup_ip(host)).await })
            .map_err(|_| anyhow!("{host} did not answer within {} s", RESOLVE_TIMEOUT.as_secs()))?
            .map_err(|_| anyhow!("cannot find {host}"))?;
        Ok(found.iter().collect())
    }
}

fn usable_nameserver(ip: IpAddr) -> bool {
    !matches!(ip, IpAddr::V6(v6) if v6.is_unicast_link_local())
}

struct Extra {
    jvm: Vec<String>,
    game: Vec<String>,
    secrets: Vec<(String, String)>,
}

fn extra_arguments(instance: &Instance, account: &Account, game_dir: &Path, ipc_address: &str, key: &str, token: &str) -> Result<Extra> {
    let mut jvm = instance.jvm_args.clone();
    jvm.extend([
        format!("-Dmanycraft.ipc={ipc_address}"),
        format!("-Dmanycraft.instance={key}"),
        format!("-Dmanycraft.preview={}", game_dir.join(PREVIEW_FILE).display()),
        "-Dmanycraft.hidden=true".to_owned(),
    ]);
    let mut game = Vec::new();
    let mut secrets = vec![(TOKEN_VAR.to_owned(), token.to_owned())];
    if let Some(url) = &account.proxy {
        let proxy = proxy::parse(url).with_context(|| format!("the proxy of {} is invalid", account.name))?;
        secrets.push((PROXY_VAR.to_owned(), url.clone()));
        match proxy.kind {
            Kind::Socks => game.extend(["--proxyHost".to_owned(), proxy.host, "--proxyPort".to_owned(), proxy.port.to_string()]),
            Kind::Http => jvm.extend([format!("-Dhttps.proxyHost={}", proxy.host), format!("-Dhttps.proxyPort={}", proxy.port)]),
        }
    }
    Ok(Extra { jvm, game, secrets })
}

fn sync_mods(instance: &Instance) -> Result<()> {
    if instance.loader.takes_mods() {
        super::modrinth::sync_mods(instance)?;
    }
    Ok(())
}

/// Deletes `target`; its instance with mods, configs, packs, templates and game folders stays.
pub fn delete_batch(target: &Batch) -> Result<()> {
    batch::delete(target)
}

/// The window icon the game of `account` in `instance` shows, written at each launch.
pub fn icon_path(instance: &Instance, account: Uuid) -> PathBuf {
    instance.game_dir(account).join(ICON_FILE)
}

/// What the companion mod of `account` playing `target` needs to know: name, colour, icon, badge and background level.
pub fn identity_messages(target: &Target, account: &Account, badge: bool) -> Vec<ipc::ToMod> {
    vec![
        ipc::ToMod::Identity { name: account.name.clone(), batch: target.label().to_owned(), rgb: account.rgb(), icon: Some(icon_path(&target.instance, account.id).display().to_string()) },
        ipc::ToMod::Badge { visible: badge },
        ipc::ToMod::Level(target.background().into()),
    ]
}

/// Where the companion mod of `account` in `instance` writes its preview frames.
pub fn preview_path(instance: &Instance, account: Uuid) -> PathBuf {
    instance.game_dir(account).join(PREVIEW_FILE)
}

/// Key of the game of `account` in `batch`, used for IPC and the fleet.
pub fn instance_key(batch: &Batch, account: Uuid) -> String {
    format!("{}-{}", batch.id, account.simple())
}

/// What stands for a batch when `instance` is played alone; never a batch id.
pub fn solo_owner(instance: &str) -> String {
    format!("{instance}-{SOLO}")
}

/// The instance `owner` plays alone, when it stands for solo play.
pub fn solo_instance(owner: &str) -> Option<&str> {
    owner.strip_suffix(SOLO)?.strip_suffix('-')
}

/// Key of the game of `account` playing `instance` alone.
pub fn solo_key(instance: &Instance, account: Uuid) -> String {
    format!("{}-{}-{SOLO}", instance.id, account.simple())
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

struct Held {
    child: Child,
    in_use: Option<lock::Guard>,
}

impl Held {
    fn new(child: Child) -> Self {
        Self { child, in_use: install::clean::in_use().ok() }
    }
}

impl Process for Held {
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        let exit = Process::try_wait(&mut self.child)?;
        if exit.is_some() {
            self.in_use = None;
        }
        Ok(exit)
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(&mut self.child)
    }

    fn wait(&mut self) -> io::Result<Option<i32>> {
        let code = Process::wait(&mut self.child);
        self.in_use = None;
        code
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
    /// The batch the game runs in; `None` when played alone.
    pub batch: Option<String>,
    pub instance: String,
    pub account: Uuid,
    child: Box<dyn Process>,
    pub pid: u32,
    pub server_addr: Option<SocketAddr>,
    server_addrs: Vec<SocketAddr>,
    pub started: Instant,
    exit: Option<Option<i32>>,
    logs: PathBuf,
}

impl RunningInstance {
    fn poll(&mut self) -> bool {
        if self.exit.is_none() {
            match self.child.try_wait() {
                Ok(None) => return true,
                Ok(Some(code)) => self.ended(code),
                Err(_) => self.ended(None),
            }
        }
        false
    }

    fn ended(&mut self, code: Option<i32>) {
        self.exit = Some(code);
        scrub::scrub_logs(&self.logs);
    }
}

/// A target readied by `Fleet::prepare_launch`: mods synced, server found, game installed.
pub struct Prepared {
    target: Target,
    installed: Installed,
    server_addrs: Vec<SocketAddr>,
    companion_jar: Option<PathBuf>,
}

/// Outcome of starting one account: its instance key, or why it did not start.
pub type Started = (Uuid, Result<String>);

/// The running instances and the IPC server their companion mods connect to.
pub struct Fleet {
    ipc: ipc::Server,
    running: Arc<Mutex<HashMap<String, RunningInstance>>>,
    reserved: Mutex<HashMap<String, Reservation>>,
    games: sync::Games,
    reaping: Arc<AtomicBool>,
    closing: AtomicBool,
    /// Pause between two starts.
    pub stagger: Duration,
}

struct Reservation {
    account: Uuid,
    instance: String,
    server_addrs: Vec<SocketAddr>,
}

/// Redacts the crash files of all instance folders in the background.
pub fn scrub_crash_files() {
    let _ = std::thread::Builder::new().name("scrub-crash-files".into()).spawn(|| scrub::scrub_instances(&paths::instances_dir()));
}

impl Fleet {
    /// A fleet with its IPC server listening; IPC events arrive on the returned receiver.
    pub fn new() -> Result<(Fleet, Receiver<ipc::Event>)> {
        let (ipc, raw) = ipc::Server::start()?;
        let games = sync::Games::default();
        let events = sync::spawn(ipc.clone(), games.clone(), raw)?;
        Ok((Fleet { ipc, running: Arc::default(), reserved: Mutex::default(), games, reaping: Arc::default(), closing: AtomicBool::new(false), stagger: DEFAULT_STAGGER }, events))
    }

    /// Installs the target's game, then prepares and starts every account `stagger` apart. Blocks until the last one is started.
    /// Fails as a whole when the game cannot be installed or the server not found; otherwise reports each account.
    pub fn start_batch(&self, target: &Target, accounts: &[Account], companion_jar: Option<&Path>, progress: &(dyn Fn(install::Progress) + Sync)) -> Result<Vec<Started>> {
        let prepared = self.prepare_launch(target, accounts, companion_jar, progress)?;
        let ipc_address = self.ipc.address();
        let launch = |account: &Account, key: &str, token: &str| -> Result<Box<dyn Process>> {
            Ok(Box::new(Held::new(launch_one(&prepared.target, account, prepared.companion_jar.as_deref(), &prepared.installed, &ipc_address, key, token)?)))
        };
        Ok(self.start_all(target, accounts, &prepared.server_addrs, &SystemClock, launch))
    }

    /// Syncs the instance's mods, finds the server and installs the game, once before `start_one` launches the accounts.
    /// Leaves the mods alone while games of the instance run, since they hold the jars open.
    pub fn prepare_launch(&self, target: &Target, accounts: &[Account], companion_jar: Option<&Path>, progress: &(dyn Fn(install::Progress) + Sync)) -> Result<Prepared> {
        let instance = &target.instance;
        let Loader::Fabric(loader_version) = &instance.loader else {
            bail!(super::i18n::trf("Manycraft kann %@ noch nicht starten. Das kommt mit einem der nächsten Updates.", &[&instance.loader.name()]));
        };
        if !self.instance_runs(&instance.id) {
            sync_mods(instance)?;
        }
        let server_addrs = server_addresses(target.server(), accounts, resolve_all)?;
        let installed = install::install(&instance.game_version, loader_version.as_deref(), progress)?;
        Ok(Prepared { target: target.clone(), installed, server_addrs, companion_jar: companion_jar.map(Path::to_path_buf) })
    }

    /// Prepares and starts `account` in the prepared target right away and returns its key.
    /// Refuses a game that already runs, an account that already plays the instance elsewhere, and an account that is
    /// already on the same server in another batch.
    pub fn start_one(&self, prepared: &Prepared, account: &Account) -> Result<String> {
        let ipc_address = self.ipc.address();
        self.launch_checked(&prepared.target, account, &prepared.server_addrs, Instant::now(), |account, key, token| {
            Ok(Box::new(Held::new(launch_one(&prepared.target, account, prepared.companion_jar.as_deref(), &prepared.installed, &ipc_address, key, token)?)))
        })
    }

    fn instance_runs(&self, instance_id: &str) -> bool {
        lock(&self.running).values_mut().any(|r| r.instance == instance_id && r.poll())
    }

    /// Sends `message` to the companion mod of the instance `key`. Fails when it is not connected.
    pub fn send(&self, key: &str, message: &ipc::ToMod) -> Result<()> {
        self.ipc.send(key, message)
    }

    fn start_all(&self, target: &Target, accounts: &[Account], server_addrs: &[SocketAddr], clock: &dyn Clock, mut launch: impl FnMut(&Account, &str, &str) -> Result<Box<dyn Process>>) -> Vec<Started> {
        let mut last: Option<Instant> = None;
        let mut outcomes = Vec::with_capacity(accounts.len());
        for account in accounts {
            let key = target.key(account.id);
            let result = self.check_free(&key, &target.instance.id, account, server_addrs).and_then(|()| {
                if let Some(at) = last {
                    let wait = self.stagger.saturating_sub(clock.now().saturating_duration_since(at));
                    if !wait.is_zero() {
                        clock.sleep(wait);
                    }
                }
                let started = clock.now();
                last = Some(started);
                self.launch_checked(target, account, server_addrs, started, &mut launch)
            });
            outcomes.push((account.id, result));
        }
        outcomes
    }

    /// Refuses every start from now on, so quitting can stop what runs without new instances appearing.
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    fn launch_checked(&self, target: &Target, account: &Account, server_addrs: &[SocketAddr], started: Instant, launch: impl FnOnce(&Account, &str, &str) -> Result<Box<dyn Process>>) -> Result<String> {
        let key = target.key(account.id);
        self.reserve(&key, &target.instance.id, account, server_addrs)?;
        sync::register(&self.games, &key, &target.instance.id, account.id);
        let token = self.ipc.register(&key);
        let child = launch(account, &key, &token).inspect_err(|_| {
            self.ipc.unregister(&key);
            lock(&self.reserved).remove(&key);
        })?;
        if self.closing.load(Ordering::SeqCst) {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            self.ipc.unregister(&key);
            lock(&self.reserved).remove(&key);
            bail!("Manycraft is quitting");
        }
        let pid = child.id();
        let logs = target.instance.game_dir(account.id).join("logs");
        let instance = RunningInstance {
            batch: target.batch.as_ref().map(|b| b.id.clone()),
            instance: target.instance.id.clone(),
            account: account.id,
            child,
            pid,
            server_addr: server_addrs.first().copied(),
            server_addrs: server_addrs.to_vec(),
            started,
            exit: None,
            logs,
        };
        self.insert(key.clone(), instance);
        Ok(key)
    }

    fn check_free(&self, key: &str, instance: &str, account: &Account, server_addrs: &[SocketAddr]) -> Result<()> {
        let mut running = lock(&self.running);
        free(&mut running, &lock(&self.reserved), key, instance, account, server_addrs)
    }

    fn reserve(&self, key: &str, instance: &str, account: &Account, server_addrs: &[SocketAddr]) -> Result<()> {
        if self.closing.load(Ordering::SeqCst) {
            bail!("Manycraft is quitting");
        }
        let mut running = lock(&self.running);
        let mut reserved = lock(&self.reserved);
        free(&mut running, &reserved, key, instance, account, server_addrs)?;
        running.remove(key);
        reserved.insert(key.to_owned(), Reservation { account: account.id, instance: instance.to_owned(), server_addrs: server_addrs.to_vec() });
        Ok(())
    }

    fn insert(&self, key: String, instance: RunningInstance) {
        let mut running = lock(&self.running);
        lock(&self.reserved).remove(&key);
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
                for (key, instance) in running.iter_mut() {
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
    /// Whether a start is between its checks and its running instance.
    pub fn launching(&self) -> bool {
        !lock(&self.reserved).is_empty()
    }

    /// Whether the instance `key` runs.
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
        let keys: Vec<String> = lock(&self.running).iter().filter(|(_, r)| r.batch.as_deref() == Some(batch_id)).map(|(k, _)| k.clone()).collect();
        self.stop_all(&keys);
    }

    /// Asks the instances `keys` to quit together, and ends those that have not after a few seconds.
    pub fn stop_all(&self, keys: &[String]) {
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
                let code = instance.child.wait().ok().flatten();
                instance.ended(code);
            }
            self.ipc.unregister(key);
        }
    }
}

fn free(running: &mut HashMap<String, RunningInstance>, reserved: &HashMap<String, Reservation>, key: &str, instance: &str, account: &Account, server_addrs: &[SocketAddr]) -> Result<()> {
    if reserved.contains_key(key) || running.get_mut(key).is_some_and(RunningInstance::poll) {
        bail!("{} is already running in this batch", account.name);
    }
    let same_game = reserved.values().any(|r| r.account == account.id && r.instance == instance) || running.values_mut().any(|r| r.account == account.id && r.instance == instance && r.poll());
    if same_game {
        bail!("{} already plays this instance elsewhere", account.name);
    }
    if reserved.values().any(|r| r.account == account.id && r.server_addrs.iter().any(|a| server_addrs.contains(a))) {
        bail!("{} is already on this server in another batch", account.name);
    }
    let clash = running.values_mut().filter(|r| r.account == account.id && r.server_addrs.iter().any(|a| server_addrs.contains(a))).find_map(|r| r.poll().then_some(r.server_addr));
    if let Some(addr) = clash {
        let addr = addr.map_or_else(String::new, |a| a.to_string());
        bail!("{} is already on the server {addr} in another batch", account.name);
    }
    Ok(())
}

fn launch_one(target: &Target, account: &Account, companion_jar: Option<&Path>, installed: &Installed, ipc_address: &str, key: &str, token: &str) -> Result<Child> {
    let instance = &target.instance;
    let game_dir = prepare(instance, account, companion_jar)?;
    require_companion_for_proxy(account, &instance.mods_dir())?;
    let _ = super::game_icon::write(account, &game_dir.join(ICON_FILE));
    let extra = extra_arguments(instance, account, &game_dir, ipc_address, key, token)?;
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
        memory_mb: target.memory_mb(),
        server: target.server().map(str::to_owned),
        extra_jvm: extra.jvm,
        extra_game: extra.game,
        env: launch::SecretEnv(extra.secrets),
    };
    launch::spawn(&spec)
}

/// The addresses of `server` on this machine, plus one stand-in for its name so that accounts behind a proxy, which
/// resolve the name through it, still clash across batches. The lookup is skipped when all accounts have a proxy and
/// may fail when some do.
fn server_addresses(server: Option<&str>, accounts: &[Account], lookup: impl Fn(&str) -> Result<Vec<SocketAddr>>) -> Result<Vec<SocketAddr>> {
    let Some(server) = server else { return Ok(Vec::new()) };
    let name = name_stand_in(server)?;
    let proxied = accounts.iter().filter(|a| a.proxy.is_some()).count();
    if !accounts.is_empty() && proxied == accounts.len() {
        return Ok(vec![name]);
    }
    let mut found = match lookup(server) {
        Err(_) if proxied > 0 => Vec::new(),
        found => found.with_context(|| format!("cannot find the server {server}"))?,
    };
    found.push(name);
    Ok(found)
}

fn name_stand_in(server: &str) -> Result<SocketAddr> {
    use std::hash::{Hash, Hasher};
    let (host, port) = split_server(server)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    host.to_ascii_lowercase().hash(&mut hasher);
    let [_, a, b, c, ..] = hasher.finish().to_be_bytes();
    Ok(SocketAddr::from(([240, a, b, c], port.unwrap_or(25565))))
}

fn require_companion_for_proxy(account: &Account, mods: &Path) -> Result<()> {
    if account.proxy.is_some() && !mods.join(COMPANION_FILE).is_file() {
        bail!("{} has a proxy, but the companion mod is missing, so the game would connect without the proxy", account.name);
    }
    Ok(())
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
    report(run(args))
}

/// `manycraft instance`: creates, lists, plays and deletes instances from the terminal.
pub fn instance_cli(args: &[String]) -> i32 {
    report(run_instance(args))
}

fn report(outcome: Result<i32>) -> i32 {
    match outcome {
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
        ["instance", batch, instance] => set_instance(batch, instance),
        ["start", batch, flags @ ..] => start(batch, flags),
        ["delete", batch] => delete(batch),
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn run_instance(args: &[String]) -> Result<i32> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["create", name, flags @ ..] => create_instance(name, flags),
        ["list"] => list_instances(),
        ["play", instance, account, flags @ ..] => play(instance, account, flags),
        ["delete", instance] => delete_instance(instance),
        _ => {
            eprintln!("{INSTANCE_USAGE}");
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

fn find_instance<'a>(instances: &'a [Instance], who: &str) -> Result<&'a Instance> {
    let count = instances.iter().filter(|i| i.id == who || i.name.eq_ignore_ascii_case(who)).count();
    if count > 1 {
        bail!("several instances are named {who}, use the id");
    }
    instances.iter().find(|i| i.id == who || i.name.eq_ignore_ascii_case(who)).with_context(|| format!("no instance {who}"))
}

fn instance_of<'a>(instances: &'a [Instance], batch: &Batch) -> Result<&'a Instance> {
    instances.iter().find(|i| i.id == batch.instance_id).with_context(|| format!("the instance of {} is gone: manycraft batch instance {} <instance>", batch.name, batch.name))
}

fn parse_loader(text: &str) -> Result<Loader> {
    instance::LOADERS.into_iter().find(|l| l.name().eq_ignore_ascii_case(text)).with_context(|| format!("unknown loader {text}: vanilla, fabric, quilt, forge or neoforge"))
}

/// Saves a new instance named `name`, for the newest release unless `version` names one, with Manycraft's default mods for Fabric.
fn new_instance(name: &str, version: Option<&str>, loader: Loader, memory: Option<u32>) -> Result<Instance> {
    let version = match version {
        Some(v) => v.to_owned(),
        None => install::latest_release()?,
    };
    let mut made = Instance::new(name, &version, loader);
    if let Some(mb) = memory {
        made.memory_mb = mb;
    }
    if made.loader.has_companion() {
        let defaults = super::modrinth::default_mods(&made.game_version)?;
        for note in &defaults.notes {
            println!("{note}");
        }
        made.mods = defaults.mods;
    }
    instance::save(&made)?;
    Ok(made)
}

fn create(name: &str, args: &[&str]) -> Result<i32> {
    let Some(flags) = flags(args, &["--instance", "--version", "--server", "--memory"]) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let name = name.trim();
    if name.is_empty() {
        bail!("a batch needs a name");
    }
    if flags.contains_key("--instance") && flags.contains_key("--version") {
        bail!("--version belongs to the instance; leave it out with --instance");
    }
    let batches = batch::load_all()?;
    if batches.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        bail!("a batch named {name} exists already");
    }
    let server = flags.get("--server").map(|s| split_server(s).map(|_| (*s).to_owned())).transpose()?;
    let memory = flags.get("--memory").map(|m| parse_memory(m)).transpose()?;
    let (played, memory_override) = match flags.get("--instance") {
        Some(who) => (find_instance(&instance::load_all()?, who)?.clone(), memory),
        None => {
            let taken: Vec<String> = instance::load_all()?.into_iter().map(|i| i.name).collect();
            (new_instance(&instance::free_name(name, &taken), flags.get("--version").copied(), Loader::default(), memory)?, None)
        }
    };
    let mut new = Batch::new(name, &played.id);
    new.server = server;
    new.memory_override = memory_override;
    batch::save(&new)?;
    println!("Batch {} ({}) plays {}: {}", new.name, new.id, played.name, played.version_line());
    Ok(0)
}

fn parse_memory(text: &str) -> Result<u32> {
    text.parse().ok().filter(|m| *m >= MIN_MEMORY_MB).with_context(|| format!("memory is a number of MB, at least {MIN_MEMORY_MB}"))
}

fn list() -> Result<i32> {
    let accounts = account::load()?;
    let instances = instance::load_all()?;
    let name = |id: &Uuid| accounts.iter().find(|a| a.id == *id).map_or_else(|| id.to_string(), |a| a.name.clone());
    for b in batch::load_all()? {
        let members: Vec<String> = b.accounts.iter().map(name).collect();
        let (played, memory) = instance_of(&instances, &b).map_or_else(|_| ("-".to_owned(), b.memory_override.unwrap_or_default()), |i| (format!("{} ({})", i.name, i.version_line()), b.memory(i.memory_mb)));
        println!("{} ({})  instance {played}  server {}  {memory} MB  accounts: {}", b.name, b.id, b.server.as_deref().unwrap_or("-"), if members.is_empty() { "-".to_owned() } else { members.join(", ") });
    }
    Ok(0)
}

fn edit(who: &str, account_name: &str, add: bool) -> Result<i32> {
    let mut batches = batch::load_all()?;
    let target = find_batch(&mut batches, who)?;
    let accounts = account::load()?;
    let account = auth::find_account(&accounts, account_name)?;
    set_member(target, account, add)?;
    batch::save(target)?;
    println!("{} {} {}", if add { "Added" } else { "Removed" }, account.name, if add { format!("to {}", target.name) } else { format!("from {}", target.name) });
    Ok(0)
}

fn set_member(batch: &mut Batch, account: &Account, add: bool) -> Result<()> {
    match (add, batch.accounts.contains(&account.id)) {
        (true, true) => bail!("{} is in {} already", account.name, batch.name),
        (false, false) => bail!("{} is not in {}", account.name, batch.name),
        (true, false) => batch.accounts.push(account.id),
        (false, true) => batch.accounts.retain(|id| *id != account.id),
    }
    Ok(())
}

fn set_instance(who: &str, instance_name: &str) -> Result<i32> {
    let mut batches = batch::load_all()?;
    let target = find_batch(&mut batches, who)?;
    let instances = instance::load_all()?;
    let played = find_instance(&instances, instance_name)?;
    let old = instances.iter().find(|i| i.id == target.instance_id && i.id != played.id).map(|i| i.name.clone());
    target.instance_id.clone_from(&played.id);
    batch::save(target)?;
    println!("{} plays {} now", target.name, played.name);
    if let Some(old) = old {
        println!("Worlds and screenshots stay with {old}");
    }
    Ok(0)
}

fn delete(who: &str) -> Result<i32> {
    let mut batches = batch::load_all()?;
    let target = find_batch(&mut batches, who)?;
    delete_batch(target)?;
    println!("Deleted batch {}; its instance with mods, configs and worlds stays", target.name);
    Ok(0)
}

fn create_instance(name: &str, args: &[&str]) -> Result<i32> {
    let Some(flags) = flags(args, &["--version", "--loader", "--memory"]) else {
        eprintln!("{INSTANCE_USAGE}");
        return Ok(2);
    };
    let name = name.trim();
    if name.is_empty() {
        bail!("an instance needs a name");
    }
    if instance::load_all()?.iter().any(|i| i.name.eq_ignore_ascii_case(name)) {
        bail!("an instance named {name} exists already");
    }
    let loader = flags.get("--loader").map_or_else(|| Ok(Loader::default()), |l| parse_loader(l))?;
    let memory = flags.get("--memory").map(|m| parse_memory(m)).transpose()?;
    let made = new_instance(name, flags.get("--version").copied(), loader, memory)?;
    println!("Instance {} ({}): {}", made.name, made.id, made.version_line());
    Ok(0)
}

fn list_instances() -> Result<i32> {
    let batches = batch::load_all()?;
    for i in instance::load_all()? {
        let users: Vec<&str> = batches.iter().filter(|b| b.instance_id == i.id).map(|b| b.name.as_str()).collect();
        println!("{} ({})  {}  {} mods  {} MB  batches: {}", i.name, i.id, i.version_line(), i.mods.len() + i.local_mods.len(), i.memory_mb, if users.is_empty() { "-".to_owned() } else { users.join(", ") });
    }
    Ok(0)
}

fn delete_instance(who: &str) -> Result<i32> {
    let instances = instance::load_all()?;
    let doomed = find_instance(&instances, who)?;
    remove_instance(doomed)?;
    println!("Deleted instance {} with its mods, configs and worlds", doomed.name);
    Ok(0)
}

/// Deletes `doomed` with its mods, configs and worlds; refused while a batch plays it or any game runs.
pub fn remove_instance(doomed: &Instance) -> Result<()> {
    let users: Vec<String> = batch::load_all()?.into_iter().filter(|b| b.instance_id == doomed.id).map(|b| b.name).collect();
    if !users.is_empty() {
        bail!(super::i18n::trf("„%@“ wird noch von %@ gespielt. Gib ihnen zuerst eine andere Instanz.", &[&doomed.name, &users.join(", ")]));
    }
    let _quiet = lock::try_exclusive(&paths::data_dir().join(install::clean::LOCK_FILE))?.with_context(|| super::i18n::tr("Gerade laufen Spiele. Stoppe sie zuerst."))?;
    instance::delete(doomed)
}

fn start(who: &str, args: &[&str]) -> Result<i32> {
    let Some(flags) = flags(args, &["--companion", "--stagger"]) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let mut batches = batch::load_all()?;
    let chosen = find_batch(&mut batches, who)?.clone();
    if chosen.accounts.is_empty() {
        bail!("{} has no accounts yet: manycraft batch add {} <account>", chosen.name, chosen.name);
    }
    let instances = instance::load_all()?;
    let target = Target::of_batch(&chosen, instance_of(&instances, &chosen)?);
    let saved = account::load()?;
    let accounts = chosen
        .accounts
        .iter()
        .map(|id| saved.iter().find(|a| a.id == *id).cloned().with_context(|| format!("account {id} of {} is no longer saved", chosen.name)))
        .collect::<Result<Vec<_>>>()?;
    run_target(&target, &accounts, &flags)
}

fn play(who: &str, account_name: &str, args: &[&str]) -> Result<i32> {
    let Some(flags) = flags(args, &["--companion"]) else {
        eprintln!("{INSTANCE_USAGE}");
        return Ok(2);
    };
    let instances = instance::load_all()?;
    let target = Target::solo(find_instance(&instances, who)?);
    let saved = account::load()?;
    let account = auth::find_account(&saved, account_name)?.clone();
    run_target(&target, &[account], &flags)
}

fn run_target(target: &Target, accounts: &[Account], flags: &HashMap<&str, &str>) -> Result<i32> {
    scrub_crash_files();
    let (mut fleet, events) = Fleet::new()?;
    if let Some(seconds) = flags.get("--stagger") {
        fleet.stagger = seconds.parse::<f64>().ok().and_then(|s| Duration::try_from_secs_f64(s).ok()).context("--stagger is a number of seconds")?;
    }
    catch_ctrl_c();
    let report = |p: install::Progress| {
        if p.total > 0 && p.done == p.total {
            println!("{}: {}/{}", p.stage.label(), p.done, p.total);
        }
    };
    let names: HashMap<String, String> = accounts.iter().map(|a| (target.key(a.id), a.name.clone())).collect();
    let mut live = Vec::new();
    let companion = match flags.get("--companion") {
        Some(path) => PathBuf::from(path),
        None => super::companion::jar()?,
    };
    for (id, outcome) in fleet.start_batch(target, accounts, Some(&companion), &report)? {
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
    let mut pids = HashMap::new();
    let badge = super::settings::load().map_or(true, |s| s.game_badge);
    while !live.is_empty() && !STOP.load(Ordering::SeqCst) {
        match events.recv_timeout(EVENT_POLL) {
            Ok(event) => {
                print_event(&names, &event);
                greet(&fleet, target, accounts, &mut pids, badge, &event);
            }
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
    let keys: Vec<String> = names.into_keys().collect();
    fleet.stop_all(&keys);
    println!("Stopped {}", target.label());
    Ok(0)
}

fn greet(fleet: &Fleet, target: &Target, accounts: &[Account], pids: &mut HashMap<String, u32>, badge: bool, event: &ipc::Event) {
    let instance = match event {
        ipc::Event::Connected { instance, pid, .. } => {
            pids.insert(instance.clone(), *pid);
            instance
        }
        ipc::Event::Message { instance, message: ipc::FromMod::Window { visible: true, .. } } => instance,
        _ => return,
    };
    let Some(account) = accounts.iter().find(|a| target.key(a.id) == *instance) else { return };
    if matches!(event, ipc::Event::Connected { .. }) {
        for message in identity_messages(target, account, badge) {
            let _ = fleet.send(instance, &message);
        }
    }
    if let Some(pid) = pids.get(instance) {
        super::game_window::tint(*pid, account);
    }
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
    use std::process::{Command, Stdio};

    use super::*;

    #[test]
    fn a_solo_owner_names_its_instance_and_a_batch_id_names_none() {
        assert_eq!(solo_instance(&solo_owner("abc")), Some("abc"));
        assert_eq!(solo_instance("abc"), None);
    }

    fn target(name: &str) -> Target {
        let instance = Instance::new(name, "26.3", Loader::default());
        Target::of_batch(&Batch::new(name, &instance.id), &instance)
    }

    struct Dirs {
        _root: tempfile::TempDir,
        inst: PathBuf,
        mods: PathBuf,
        config: PathBuf,
        template: PathBuf,
        shared: Shared,
    }

    fn dirs() -> Dirs {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().to_path_buf();
        let batch = base.join("batch");
        let shared = Shared {
            mods: batch.join("mods"),
            config: batch.join("config"),
            resourcepacks: batch.join("resourcepacks"),
            shaderpacks: batch.join("shaderpacks"),
            options: batch.join(OPTIONS_FILE),
            servers: batch.join(sync::SERVERS_FILE),
        };
        Dirs { inst: base.join("inst"), mods: shared.mods.clone(), config: shared.config.clone(), template: shared.options.clone(), shared, _root: root }
    }

    fn prep(d: &Dirs, jar: Option<&Path>) -> Result<()> {
        prepare_in(&d.inst, &d.shared, jar, &[])
    }

    #[test]
    fn packs_are_merged_into_the_batch_and_clashes_kept() {
        let d = dirs();
        let own = d.inst.join("resourcepacks");
        fs::create_dir_all(&own).unwrap();
        fs::write(own.join("mine.zip"), b"mine").unwrap();
        fs::write(own.join("both.zip"), b"own copy").unwrap();
        fs::create_dir_all(&d.shared.resourcepacks).unwrap();
        fs::write(d.shared.resourcepacks.join("both.zip"), b"batch copy").unwrap();
        fs::create_dir_all(d.inst.join("shaderpacks")).unwrap();
        fs::write(d.inst.join("shaderpacks").join("s.zip"), b"s").unwrap();
        prep(&d, None).unwrap();
        assert!(same(&own, &d.shared.resourcepacks));
        assert!(same(&d.inst.join("shaderpacks"), &d.shared.shaderpacks));
        assert_eq!(fs::read(d.shared.resourcepacks.join("mine.zip")).unwrap(), b"mine");
        assert_eq!(fs::read(d.shared.resourcepacks.join("both.zip")).unwrap(), b"batch copy");
        assert_eq!(fs::read(d.inst.join("resourcepacks.old-1").join("both.zip")).unwrap(), b"own copy");
        assert_eq!(fs::read(d.shared.shaderpacks.join("s.zip")).unwrap(), b"s");
        assert!(!d.inst.join("shaderpacks.old-1").exists());
        prep(&d, None).unwrap();
        assert!(!d.inst.join("resourcepacks.old-2").exists());
        assert_eq!(fs::read(d.shared.resourcepacks.join("mine.zip")).unwrap(), b"mine");
    }

    #[test]
    fn the_server_list_comes_from_the_template_on_every_start() {
        let d = dirs();
        fs::create_dir_all(&d.inst).unwrap();
        fs::write(d.inst.join(sync::SERVERS_FILE), b"own").unwrap();
        prep(&d, None).unwrap();
        assert_eq!(fs::read(&d.shared.servers).unwrap(), b"own");
        fs::write(&d.shared.servers, b"batch").unwrap();
        prep(&d, None).unwrap();
        assert_eq!(fs::read(d.inst.join(sync::SERVERS_FILE)).unwrap(), b"batch");
        assert!(!d.inst.join("servers.dat.old-1").exists());
    }

    fn same(a: &Path, b: &Path) -> bool {
        fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
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
        assert!(same(&d.inst.join("mods"), &d.mods));
        assert!(same(&d.inst.join("config"), &d.config));
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
        assert!(!points_to(&d.inst.join("mods"), &d.mods));
        prep(&d, None).unwrap();
        assert!(same(&d.inst.join("mods"), &d.mods));
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
        assert!(same(&d.inst.join("mods"), &d.mods));
    }

    #[test]
    fn options_come_from_the_template_on_every_start() {
        let d = dirs();
        prep(&d, None).unwrap();
        assert!(!d.inst.join(OPTIONS_FILE).exists());
        fs::write(d.inst.join(OPTIONS_FILE), "fov:1.0\n").unwrap();
        prep(&d, None).unwrap();
        assert_eq!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap(), "fov:1.0\n");
        fs::write(&d.template, "soundCategory_master:0.0\nonboardAccessibility:false\nfullscreen:false\n").unwrap();
        fs::write(d.inst.join(OPTIONS_FILE), "fov:1.0\nfullscreen:true\nonboardAccessibility:true\n").unwrap();
        prep(&d, None).unwrap();
        assert_eq!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap(), "soundCategory_master:0.0\nonboardAccessibility:false\nfullscreen:true\nfov:1.0\n");
        fs::write(d.inst.join(OPTIONS_FILE), "soundCategory_master:1.0\n").unwrap();
        prep(&d, None).unwrap();
        assert!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap().contains("soundCategory_master:0.0"));
    }

    #[test]
    fn an_unreadable_template_is_an_error() {
        let d = dirs();
        fs::create_dir_all(&d.template).unwrap();
        assert!(prep(&d, None).is_err());
    }

    #[test]
    fn per_instance_keys_survive_the_template() {
        let merged = merge_options("a:1\nb:2\nd:5\n", Some("d:4\nb:9\nc:3\n"), &["b", "c"]);
        assert_eq!(merged, "a:1\nb:9\nd:5\nc:3\n");
        for key in ["fullscreen", "overrideWidth", "overrideHeight", "lastServer"] {
            assert!(PER_INSTANCE_KEYS.contains(&key));
        }
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
        let not_a_folder = jar.with_file_name("not-a-folder");
        fs::write(&not_a_folder, b"").unwrap();
        assert!(install_companion(&jar, &not_a_folder).is_err());
    }

    #[derive(Default)]
    struct FakeDns {
        srv: HashMap<String, (String, u16)>,
        ips: HashMap<String, Vec<IpAddr>>,
        calls: Cell<u32>,
        start: Option<Instant>,
        deadlines: RefCell<Vec<Duration>>,
    }

    impl FakeDns {
        fn record(&self, host: &str, deadline: Instant) {
            self.calls.set(self.calls.get() + 1);
            if host.contains("example.org")
                && !host.starts_with("mc.")
                && let Some(start) = self.start
            {
                self.deadlines.borrow_mut().push(deadline - start);
            }
        }
    }

    impl Lookup for FakeDns {
        fn srv(&self, name: &str, deadline: Instant) -> Result<Option<(String, u16)>> {
            self.record(name, deadline);
            Ok(self.srv.get(name).cloned())
        }

        fn ips(&self, host: &str, deadline: Instant) -> Result<Vec<IpAddr>> {
            self.record(host, deadline);
            self.ips.get(host).cloned().with_context(|| format!("cannot find {host}"))
        }
    }

    fn fake_dns() -> FakeDns {
        let mut dns = FakeDns::default();
        dns.srv.insert("_minecraft._tcp.example.org".into(), ("mc.example.org.net".into(), 25570));
        dns.ips.insert("mc.example.org.net".into(), vec!["10.0.0.7".parse().unwrap()]);
        dns.ips.insert("example.org".into(), vec!["10.0.0.1".parse().unwrap(), "::1".parse().unwrap()]);
        dns.ips.insert("plain.org".into(), vec!["10.0.0.2".parse().unwrap()]);
        dns.ips.insert("empty.org".into(), Vec::new());
        dns.srv.insert("_minecraft._tcp.srv-ip.org".into(), ("10.0.0.9".into(), 1));
        dns
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ip_literals_pass_through_without_lookups() {
        let dns = fake_dns();
        let at = Instant::now();
        assert_eq!(resolve_with(&dns, "127.0.0.1:25565", at).unwrap(), [addr("127.0.0.1:25565")]);
        assert_eq!(resolve_with(&dns, "127.0.0.1", at).unwrap(), [addr("127.0.0.1:25565")]);
        assert_eq!(resolve_with(&dns, "[::1]:25570", at).unwrap(), [addr("[::1]:25570")]);
        assert_eq!(resolve_with(&dns, "::1", at).unwrap(), [addr("[::1]:25565")]);
        assert_eq!(resolve_with(&dns, "[::1]", at).unwrap(), [addr("[::1]:25565")]);
        assert_eq!(dns.calls.get(), 0);
    }

    #[test]
    fn srv_only_without_a_port_and_a_records_otherwise() {
        let at = Instant::now();
        let dns = FakeDns { start: Some(at), ..fake_dns() };
        assert_eq!(resolve_with(&dns, "example.org", at).unwrap(), [addr("10.0.0.7:25570")]);
        assert_eq!(resolve_with(&dns, "example.org:25565", at).unwrap(), [addr("10.0.0.1:25565"), addr("[::1]:25565")]);
        assert_eq!(resolve_with(&dns, "plain.org", at).unwrap(), [addr("10.0.0.2:25565")]);
        assert!(resolve_with(&dns, "nowhere.org", at).is_err());
        assert!(resolve_with(&dns, "empty.org", at).is_err());
        assert_eq!(resolve_with(&dns, "srv-ip.org", at).unwrap(), [addr("10.0.0.9:1")]);
        assert_eq!(*dns.deadlines.borrow(), [Duration::from_secs(1), Duration::from_secs(3)]);
    }

    #[test]
    fn malformed_servers_are_refused() {
        for bad in ["", "host:", "host:0", "host:99999", "host:x", "[::1", "[::1]x", "a b:1"] {
            assert!(split_server(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn link_local_nameservers_without_a_scope_are_skipped() {
        assert!(!usable_nameserver("fe80::1".parse().unwrap()));
        assert!(usable_nameserver("192.168.2.1".parse().unwrap()));
        assert!(usable_nameserver("2001:4860:4860::8888".parse().unwrap()));
    }

    #[test]
    fn answers_are_cached_for_a_minute() {
        let resolver = Resolver::new(fake_dns());
        let t0 = Instant::now();
        assert_eq!(resolver.resolve("plain.org", t0).unwrap(), [addr("10.0.0.2:25565")]);
        let after_first = resolver.lookup.calls.get();
        resolver.resolve("PLAIN.org", t0 + Duration::from_secs(59)).unwrap();
        assert_eq!(resolver.lookup.calls.get(), after_first);
        resolver.resolve("plain.org", t0 + Duration::from_secs(60)).unwrap();
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
        let (fleet, _events) = Fleet::new().unwrap();
        let clock = FakeClock::new();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let alts = accounts(&["AltOne", "AltTwo"]);
        let a = target("A");
        let b = target("B");
        let c = target("C");
        let server = [addr("10.0.0.1:25565"), addr("10.0.0.3:25565")];
        let rotated = [addr("10.0.0.3:25565"), addr("10.0.0.1:25565")];
        let first = fleet.start_all(&a, &alts, &server, &clock, launcher(&exits, &tokens));
        assert!(first.iter().all(|(_, r)| r.is_ok()));
        let again = fleet.start_all(&a, &alts[..1], &server, &clock, launcher(&exits, &tokens));
        assert!(format!("{:#}", again[0].1.as_ref().unwrap_err()).contains("already running"));
        let dup = fleet.start_all(&b, &alts[..1], &rotated, &clock, launcher(&exits, &tokens));
        assert!(format!("{:#}", dup[0].1.as_ref().unwrap_err()).contains("AltOne is already on the server 10.0.0.1:25565"));
        let elsewhere = fleet.start_all(&c, &alts[..1], &[addr("10.0.0.2:25565")], &clock, launcher(&exits, &tokens));
        assert!(elsewhere[0].1.is_ok());
        let no_server = fleet.start_all(&target("D"), &alts[..1], &[], &clock, launcher(&exits, &tokens));
        assert!(no_server[0].1.is_ok());
        lock(&exits.borrow()[&alts[0].id]).replace(Some(0));
        let after_exit = fleet.start_all(&b, &alts[..1], &server, &clock, launcher(&exits, &tokens));
        assert!(after_exit[0].1.is_err(), "still on the server through batch C");
        let other_account = fleet.start_all(&b, &alts[1..], &[addr("10.0.0.2:25565")], &clock, launcher(&exits, &tokens));
        assert!(other_account[0].1.is_ok());
        assert_eq!(tokens.borrow().len(), 5);
    }

    #[test]
    fn starts_are_staggered_and_refusals_do_not_wait() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        let clock = FakeClock::new();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne", "AltTwo", "AltThree"]);
        fleet.start_all(&batch, &alts[..1], &[], &clock, launcher(&exits, &tokens));
        clock.advance(Duration::from_secs(10));
        let outcomes = fleet.start_all(&batch, &alts, &[], &clock, launcher(&exits, &tokens));
        assert!(outcomes[0].1.is_err());
        assert!(outcomes[1].1.is_ok() && outcomes[2].1.is_ok());
        assert_eq!(*clock.slept.borrow(), vec![DEFAULT_STAGGER]);
        fleet.stagger = Duration::from_secs(5);
        let more = accounts(&["AltFour", "AltFive"]);
        clock.slept.borrow_mut().clear();
        fleet.start_all(&batch, &more, &[], &clock, |a: &Account, k: &str, t: &str| {
            clock.advance(Duration::from_secs(2));
            launcher(&exits, &tokens)(a, k, t)
        });
        assert_eq!(*clock.slept.borrow(), vec![Duration::from_secs(3)]);
    }

    #[test]
    fn a_failed_launch_is_reported_and_unregistered() {
        let (fleet, _events) = Fleet::new().unwrap();
        let batch = target("A");
        let alts = accounts(&["AltOne"]);
        let outcomes = fleet.start_all(&batch, &alts, &[], &FakeClock::new(), |_: &Account, _: &str, _: &str| Err(anyhow!("no java")));
        assert!(outcomes[0].1.is_err());
        assert!(!fleet.is_running(&batch.key(alts[0].id)));
        assert!(fleet.exit_code(&batch.key(alts[0].id)).is_none());
    }

    #[test]
    fn exits_are_tracked_and_stop_ends_the_rest() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne", "AltTwo"]);
        fleet.stagger = Duration::ZERO;
        let keys: Vec<String> = fleet.start_all(&batch, &alts, &[], &SystemClock, launcher(&exits, &tokens)).into_iter().map(|(_, r)| r.unwrap()).collect();
        assert!(fleet.is_running(&keys[0]) && fleet.is_running(&keys[1]));
        lock(&exits.borrow()[&alts[0].id]).replace(Some(3));
        let deadline = Instant::now() + Duration::from_secs(3);
        while fleet.reaping.load(Ordering::SeqCst) && lock(&fleet.running)[&keys[0]].exit.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(lock(&fleet.running)[&keys[0]].exit, Some(Some(3)));
        assert!(!fleet.ipc.is_connected(&keys[0]));
        assert_eq!(fleet.exit_code(&keys[0]), Some(Some(3)));
        assert_eq!(lock(&fleet.running)[&keys[0]].pid, 1);
        assert_eq!(fleet.running_keys(), vec![keys[1].clone()]);
        fleet.stop_batch(&batch.owner());
        assert!(!fleet.is_running(&keys[1]));
        assert_eq!(fleet.exit_code(&keys[1]), Some(Some(1)));
        assert!(fleet.stop("nope").is_err());
    }

    #[test]
    fn proxy_credentials_go_through_the_environment_never_the_command_line() {
        let batch = target("A");
        let mut alt = Account::offline("AltOne").unwrap();
        alt.proxy = Some("socks5://us%40r:p%3Ass@10.1.1.1:1081".into());
        let extra = extra_arguments(&batch.instance, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert_eq!(extra.game, ["--proxyHost", "10.1.1.1", "--proxyPort", "1081"]);
        assert!(extra.secrets.contains(&("MANYCRAFT_PROXY".to_owned(), "socks5://us%40r:p%3Ass@10.1.1.1:1081".to_owned())));
        assert!(extra.secrets.contains(&("MANYCRAFT_TOKEN".to_owned(), "tok".to_owned())));
        let command_line = extra.jvm.iter().chain(&extra.game).cloned().collect::<Vec<_>>().join(" ");
        for secret in ["us%40r", "us@r", "p%3Ass", "p:ss", "tok"] {
            assert!(!command_line.contains(secret), "{secret} in {command_line}");
        }
        assert!(!extra.jvm.iter().any(|a| a.starts_with("-Dmanycraft.proxy")));
        assert!(extra.jvm.contains(&"-Dmanycraft.hidden=true".to_owned()));
        assert!(extra.jvm.contains(&"-Dmanycraft.instance=k".to_owned()));
        assert!(extra.jvm.contains(&"-Dmanycraft.ipc=127.0.0.1:9".to_owned()));
        alt.proxy = Some("http://proxy.example:3128".into());
        let extra = extra_arguments(&batch.instance, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert!(extra.game.is_empty());
        assert!(extra.jvm.contains(&"-Dhttps.proxyHost=proxy.example".to_owned()) && extra.jvm.contains(&"-Dhttps.proxyPort=3128".to_owned()));
        alt.proxy = None;
        let extra = extra_arguments(&batch.instance, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert!(extra.game.is_empty() && !extra.jvm.iter().any(|a| a.contains("proxy")));
        assert_eq!(extra.secrets.len(), 1);
        alt.proxy = Some("ftp://x:1".into());
        assert!(extra_arguments(&batch.instance, &alt, Path::new("inst"), "", "k", "tok").is_err());
    }

    #[test]
    fn a_proxy_needs_the_companion_mod() {
        let d = dirs();
        fs::create_dir_all(&d.mods).unwrap();
        let mut alt = Account::offline("AltOne").unwrap();
        assert!(require_companion_for_proxy(&alt, &d.mods).is_ok());
        alt.proxy = Some("socks5://h:1".into());
        assert!(format!("{:#}", require_companion_for_proxy(&alt, &d.mods).unwrap_err()).contains("companion mod is missing"));
        fs::write(d.mods.join(COMPANION_FILE), b"jar").unwrap();
        assert!(require_companion_for_proxy(&alt, &d.mods).is_ok());
    }

    #[test]
    fn child_processes_report_exit_and_can_be_killed() {
        let (quick, long): ((&str, &[&str]), (&str, &[&str])) =
            if cfg!(windows) { (("cmd", &["/C", "exit 7"]), ("ping", &["-n", "30", "127.0.0.1"])) } else { (("sh", &["-c", "exit 7"]), ("sleep", &["30"])) };
        let mut done = Command::new(quick.0).args(quick.1).stdout(Stdio::null()).spawn().unwrap();
        assert_eq!(Process::wait(&mut done).unwrap(), Some(7));
        assert_eq!(Process::try_wait(&mut done).unwrap(), Some(Some(7)));
        let mut running = Command::new(long.0).args(long.1).stdout(Stdio::null()).spawn().unwrap();
        assert_eq!(Process::id(&running), running.id());
        assert_eq!(Process::try_wait(&mut running).unwrap(), None);
        Process::kill(&mut running).unwrap();
        let code = Process::wait(&mut running).unwrap();
        assert_ne!(code, Some(0));
        assert!(Process::try_wait(&mut running).unwrap().is_some());
    }

    #[test]
    fn the_reaper_runs_only_while_instances_live() {
        let (fleet, _events) = Fleet::new().unwrap();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne"]);
        let key = fleet.start_all(&batch, &alts, &[], &SystemClock, launcher(&exits, &tokens)).remove(0).1.unwrap();
        std::thread::sleep(REAP_INTERVAL * 2);
        assert!(fleet.reaping.load(Ordering::SeqCst));
        lock(&exits.borrow()[&alts[0].id]).replace(Some(5));
        let deadline = Instant::now() + Duration::from_secs(3);
        while fleet.reaping.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!fleet.reaping.load(Ordering::SeqCst));
        assert_eq!(lock(&fleet.running)[&key].exit, Some(Some(5)));
    }

    #[test]
    fn stop_asks_a_connected_instance_to_quit_first() {
        use std::io::{BufRead, BufReader, Write};
        let (fleet, events) = Fleet::new().unwrap();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne"]);
        let key = fleet.start_all(&batch, &alts, &[], &SystemClock, launcher(&exits, &tokens)).remove(0).1.unwrap();
        let exit = Arc::clone(&exits.borrow()[&alts[0].id]);
        let mut stream = std::net::TcpStream::connect(fleet.ipc.address()).unwrap();
        let hello = serde_json::json!({"t": "hello", "instance": key, "token": tokens.borrow()[0], "pid": 1, "mod_version": "0"});
        stream.write_all(format!("{hello}\n").as_bytes()).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)).unwrap(), ipc::Event::Connected { .. }));
        let client = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            if line.contains("quit") {
                lock(&exit).replace(Some(0));
            }
        });
        let started = Instant::now();
        fleet.stop(&key).unwrap();
        client.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(fleet.exit_code(&key), Some(Some(0)));
    }

    #[test]
    fn send_reaches_a_connected_mod_and_fails_otherwise() {
        use std::io::{BufRead, BufReader, Write};
        let (fleet, events) = Fleet::new().unwrap();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne"]);
        let key = fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), launcher(&exits, &tokens)).unwrap();
        assert!(fleet.send(&key, &ipc::ToMod::Hide).is_err());
        let mut stream = std::net::TcpStream::connect(fleet.ipc.address()).unwrap();
        let hello = serde_json::json!({"t": "hello", "instance": key, "token": tokens.borrow()[0], "pid": 1, "mod_version": "0"});
        stream.write_all(format!("{hello}
").as_bytes()).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)).unwrap(), ipc::Event::Connected { .. }));
        fleet.send(&key, &ipc::ToMod::Reconnect).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert_eq!(line, "{\"t\":\"reconnect\"}
");
    }

    #[test]
    fn a_single_start_refuses_a_running_instance_and_a_taken_server() {
        let (fleet, _events) = Fleet::new().unwrap();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let alts = accounts(&["AltOne"]);
        let (a, b) = (target("A"), target("B"));
        let server = [addr("10.0.0.1:25565")];
        let key = fleet.launch_checked(&a, &alts[0], &server, Instant::now(), launcher(&exits, &tokens)).unwrap();
        assert_eq!(key, a.key(alts[0].id));
        assert!(format!("{:#}", fleet.launch_checked(&a, &alts[0], &server, Instant::now(), launcher(&exits, &tokens)).unwrap_err()).contains("already running"));
        assert!(format!("{:#}", fleet.launch_checked(&b, &alts[0], &server, Instant::now(), launcher(&exits, &tokens)).unwrap_err()).contains("already on the server"));
        assert_eq!(tokens.borrow().len(), 1);
        assert!(fleet.is_running(&key));
    }

    fn live(pid: u32) -> Box<dyn Process> {
        Box::new(FakeProcess { pid, exit: Arc::new(Mutex::new(None)) })
    }

    #[test]
    fn two_starts_at_once_launch_an_instance_only_once() {
        let (fleet, _events) = Fleet::new().unwrap();
        let alts = accounts(&["AltOne"]);
        let (a, b) = (target("A"), target("B"));
        let server = [addr("10.0.0.1:25565")];
        let launches = std::sync::atomic::AtomicUsize::new(0);
        let slow = |_: &Account, _: &str, _: &str| {
            launches.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            Ok(live(1))
        };
        std::thread::scope(|s| {
            let first = s.spawn(|| fleet.launch_checked(&a, &alts[0], &server, Instant::now(), slow));
            std::thread::sleep(Duration::from_millis(100));
            let same = fleet.launch_checked(&a, &alts[0], &server, Instant::now(), |_, _, _| Ok(live(2)));
            let other = fleet.launch_checked(&b, &alts[0], &server, Instant::now(), |_, _, _| Ok(live(3)));
            assert!(format!("{:#}", same.unwrap_err()).contains("already running"));
            assert!(format!("{:#}", other.unwrap_err()).contains("already on"));
            assert!(first.join().unwrap().is_ok());
        });
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert_eq!(fleet.running_keys().len(), 1);
    }

    #[test]
    fn a_restarted_instance_keeps_its_new_token_while_the_reaper_runs() {
        use std::io::Write;
        let (fleet, events) = Fleet::new().unwrap();
        let alts = accounts(&["AltOne", "AltTwo"]);
        let batch = target("A");
        fleet.launch_checked(&batch, &alts[1], &[], Instant::now(), |_, _, _| Ok(live(2))).unwrap();
        let exit = Arc::new(Mutex::new(None));
        let dying = Arc::clone(&exit);
        let key = fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), move |_, _, _| Ok(Box::new(FakeProcess { pid: 1, exit: dying }) as Box<dyn Process>)).unwrap();
        *lock(&exit) = Some(Some(1));
        let token = Mutex::new(String::new());
        fleet
            .launch_checked(&batch, &alts[0], &[], Instant::now(), |_, _, t| {
                *lock(&token) = t.to_owned();
                std::thread::sleep(REAP_INTERVAL * 3);
                Ok(live(3))
            })
            .unwrap();
        std::thread::sleep(REAP_INTERVAL * 2);
        let mut stream = std::net::TcpStream::connect(fleet.ipc.address()).unwrap();
        let hello = serde_json::json!({"t": "hello", "instance": key, "token": *lock(&token), "pid": 3, "mod_version": "0"});
        stream.write_all(format!("{hello}\n").as_bytes()).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)).unwrap(), ipc::Event::Connected { .. }));
    }

    #[test]
    fn a_closing_fleet_starts_nothing_and_a_failed_launch_frees_its_slot() {
        let (fleet, _events) = Fleet::new().unwrap();
        let alts = accounts(&["AltOne"]);
        let batch = target("A");
        assert!(fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), |_, _, _| bail!("no java")).is_err());
        assert!(fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), |_, _, _| Ok(live(1))).is_ok());
        fleet.stop_all(&fleet.running_keys());
        let exit = Arc::new(Mutex::new(None));
        let late = fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), |_, _, _| {
            fleet.close();
            Ok(Box::new(FakeProcess { pid: 3, exit: Arc::clone(&exit) }) as Box<dyn Process>)
        });
        assert!(format!("{:#}", late.unwrap_err()).contains("quitting"));
        assert!(exit.lock().unwrap().is_some());
        assert!(!fleet.launching());
        assert!(fleet.running_keys().is_empty());
        let refused = fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), |_, _, _| Ok(live(2)));
        assert!(format!("{:#}", refused.unwrap_err()).contains("quitting"));
    }

    #[test]
    fn a_reservation_blocks_only_its_account_on_its_server_and_mods_wait_for_the_batch() {
        let (fleet, _events) = Fleet::new().unwrap();
        let alts = accounts(&["AltOne", "AltTwo"]);
        let (a, b) = (target("A"), target("B"));
        let (x, y) = ([addr("10.0.0.1:25565")], [addr("10.0.0.2:25565")]);
        let reserved = HashMap::from([(a.key(alts[0].id), Reservation { account: alts[0].id, instance: a.instance.id.clone(), server_addrs: x.to_vec() })]);
        let mut running = HashMap::new();
        assert!(free(&mut running, &reserved, &b.key(alts[0].id), &b.instance.id, &alts[0], &y).is_ok());
        assert!(free(&mut running, &reserved, &b.key(alts[1].id), &b.instance.id, &alts[1], &x).is_ok());
        assert!(format!("{:#}", free(&mut running, &reserved, &b.key(alts[0].id), &b.instance.id, &alts[0], &x).unwrap_err()).contains("this server in another batch"));
        let shared = Target::of_batch(&Batch::new("C", &a.instance.id), &a.instance);
        assert!(format!("{:#}", free(&mut running, &reserved, &shared.key(alts[0].id), &a.instance.id, &alts[0], &y).unwrap_err()).contains("already plays this instance elsewhere"));
        assert!(free(&mut running, &reserved, &shared.key(alts[1].id), &a.instance.id, &alts[1], &y).is_ok());
        assert!(!fleet.instance_runs(&a.instance.id));
        let exit = Arc::new(Mutex::new(None));
        let key = fleet.launch_checked(&a, &alts[0], &x, Instant::now(), |_, _, _| Ok(Box::new(FakeProcess { pid: 1, exit: Arc::clone(&exit) }) as Box<dyn Process>)).unwrap();
        assert!(fleet.instance_runs(&a.instance.id));
        assert!(!fleet.instance_runs(&b.instance.id));
        *exit.lock().unwrap() = Some(Some(0));
        assert!(!fleet.instance_runs(&a.instance.id));
        assert!(fleet.running_keys().iter().all(|k| *k != key) || !fleet.instance_runs(&a.instance.id));
    }

    #[test]
    fn the_fleet_is_shared_across_threads_and_previews_land_where_the_mod_writes() {
        fn shared<T: Send + Sync>() {}
        shared::<Fleet>();
        shared::<Prepared>();
        let batch = target("A");
        let alt = Account::offline("AltOne").unwrap();
        let dir = batch.instance.game_dir(alt.id);
        let extra = extra_arguments(&batch.instance, &alt, &dir, "127.0.0.1:9", "k", "tok").unwrap();
        assert!(extra.jvm.contains(&format!("-Dmanycraft.preview={}", preview_path(&batch.instance, alt.id).display())));
    }

    #[test]
    fn a_server_name_is_only_looked_up_here_when_an_account_goes_out_directly() {
        let addr: SocketAddr = "10.0.0.1:25565".parse().unwrap();
        let found = |_: &str| Ok(vec![addr]);
        let missing = |s: &str| -> Result<Vec<SocketAddr>> { bail!("cannot find {s}") };
        let direct = Account::offline("AltOne").unwrap();
        let mut proxied = Account::offline("AltTwo").unwrap();
        proxied.proxy = Some("socks5://10.1.1.1:1080".into());
        let name = name_stand_in("s").unwrap();
        assert_eq!(server_addresses(None, &[direct.clone()], found).unwrap(), []);
        assert_eq!(server_addresses(Some("s"), &[direct.clone()], found).unwrap(), [addr, name]);
        assert!(server_addresses(Some("s"), &[direct.clone()], missing).is_err());
        assert_eq!(server_addresses(Some("s"), &[direct.clone(), proxied.clone()], found).unwrap(), [addr, name]);
        assert_eq!(server_addresses(Some("s"), &[direct, proxied.clone()], missing).unwrap(), [name]);
        let never = |_: &str| -> Result<Vec<SocketAddr>> { panic!("looked up on this machine") };
        assert_eq!(server_addresses(Some("s"), &[proxied.clone()], never).unwrap(), [name]);
        assert_eq!(name_stand_in("S").unwrap(), name);
        assert_eq!(name_stand_in("s:25565").unwrap(), name);
        assert_ne!(name_stand_in("s:25566").unwrap(), name);
        assert_ne!(name_stand_in("t").unwrap(), name);
        let other = server_addresses(Some("S"), &[proxied], never).unwrap();
        let reserved = HashMap::from([("k".to_owned(), Reservation { account: Uuid::nil(), instance: "i".into(), server_addrs: vec![name] })]);
        let mut account = Account::offline("AltThree").unwrap();
        account.id = Uuid::nil();
        assert!(free(&mut HashMap::new(), &reserved, "other", "j", &account, &other).is_err());
    }

    #[test]
    fn cli_flags_are_parsed() {
        let f = flags(&["--version", "26.3", "--memory", "4096"], &["--version", "--memory"]).unwrap();
        assert_eq!((f["--version"], f["--memory"]), ("26.3", "4096"));
        assert!(flags(&["--version"], &["--version"]).is_none());
        assert!(flags(&["--bogus", "1"], &["--version"]).is_none());
        assert!(flags(&["--version", "1", "--version", "2"], &["--version"]).is_none());
        let mut batches = vec![Batch::new("Test", "i1"), Batch::new("Other", "i1")];
        assert_eq!(find_batch(&mut batches, "test").unwrap().name, "Test");
        let id = batches[1].id.clone();
        assert_eq!(find_batch(&mut batches, &id).unwrap().name, "Other");
        assert!(find_batch(&mut batches, "none").is_err());
        batches.push(Batch::new("TEST", "i1"));
        assert!(format!("{:#}", find_batch(&mut batches, "test").unwrap_err()).contains("several"));
        assert_eq!(parse_memory("512").unwrap(), 512);
        assert!(parse_memory("511").is_err() && parse_memory("x").is_err());
        let alt = Account::offline("AltOne").unwrap();
        let b = &mut batches[0];
        set_member(b, &alt, true).unwrap();
        assert!(set_member(b, &alt, true).is_err());
        assert_eq!(b.accounts, [alt.id]);
        set_member(b, &alt, false).unwrap();
        assert!(set_member(b, &alt, false).is_err());
        assert!(b.accounts.is_empty());
    }
}
