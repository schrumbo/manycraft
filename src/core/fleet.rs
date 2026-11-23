//! Running games: preparing game folders from an instance, starting them staggered, tracking their processes.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::ResolverConfig;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;
use uuid::Uuid;

use super::account::{self, Account, AccountKind};
use super::batch::{self, Background, Batch, Packs, RunMode};
use super::instance::{self, Instance, Loader};
use super::install::{self, Installed};
use super::launch::{self, LaunchSpec};
use super::options::LaunchOptions;
use super::proxy::{self, Kind};
use super::share;
use super::sync::{self, OPTIONS_FILE, PER_INSTANCE_KEYS};
use super::{adopt, auth, gpu, headless, ipc, javas, lock, net, paths, scrub, stagger};

/// Pause between two instance starts unless the caller sets another.
pub const DEFAULT_STAGGER: Duration = Duration::from_secs(3);
/// File name of the companion mod older builds put in a batch's shared mods folder; skipped in content lists and removed
/// before each start, since the agent replaced it.
pub const COMPANION_FILE: &str = "manycraft-companion.jar";

pub(super) const DEFAULT_PORT: u16 = 25565;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
const SRV_TIMEOUT: Duration = Duration::from_secs(1);
const RESOLVE_CACHE: Duration = Duration::from_secs(60);
const REAP_INTERVAL: Duration = Duration::from_millis(500);
const EXITS_WAIT: Duration = Duration::from_secs(40);
/// How long a stopped game may take to save and quit before it is ended.
const STOP_GRACE: Duration = Duration::from_secs(30);
const LINKED: [&str; 5] = ["mods", "config", "resourcepacks", "shaderpacks", "datapacks"];
const QUIT_POLL: Duration = Duration::from_millis(100);
const TURN_POLL: Duration = Duration::from_millis(100);
const EVENT_POLL: Duration = Duration::from_millis(250);
const MIN_MEMORY_MB: u32 = 512;
const PREVIEW_FILE: &str = "manycraft-preview.bin";
const FML_CONFIG: &str = "fml.toml";
const HOME_WAIT: Duration = Duration::from_secs(600);
const HOME_SETTLE: Duration = Duration::from_secs(20);
const EARLY_WINDOW: &str = "earlyWindowControl";
const ICON_FILE: &str = "manycraft-icon.png";
const TOKEN_VAR: &str = "MANYCRAFT_TOKEN";
const PROXY_VAR: &str = "MANYCRAFT_PROXY";
const HEADLESS_FLAG: &str = "-Dmanycraft.headless=true";
/// SDL reads its hints from the environment; this one keeps a macOS game out of the Dock and the app switcher.
const MAC_BACKGROUND_VAR: &str = "SDL_MAC_BACKGROUND_APP";
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
    /// Where a game played alone goes right after it starts.
    pub quick: Option<Quick>,
}

/// A world or server a game opens right away, as Minecraft's quick play does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Quick {
    /// The folder of a world in the game's `saves`.
    World(String),
    /// A server address, as the server list keeps it.
    Server(String),
}

impl Target {
    /// `batch` playing `instance`.
    pub fn of_batch(batch: &Batch, instance: &Instance) -> Self {
        Self { instance: instance.clone(), batch: Some(batch.clone()), quick: None }
    }

    /// `instance` played alone.
    pub fn solo(instance: &Instance) -> Self {
        Self { instance: instance.clone(), batch: None, quick: None }
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
        self.batch.as_ref().map_or_else(|| self.instance.memory(), |b| b.memory(self.instance.memory()))
    }

    fn server(&self) -> Option<&str> {
        self.batch.as_ref().and_then(|b| b.server.as_deref())
    }

    fn join(&self) -> Option<&str> {
        self.server().or(match &self.quick {
            Some(Quick::Server(address)) => Some(address.as_str()),
            _ => None,
        })
    }

    fn label(&self) -> &str {
        self.batch.as_ref().map_or(&self.instance.name, |b| &b.name)
    }

    fn background(&self) -> Background {
        self.batch.as_ref().map_or(Background::Full, |b| b.background)
    }

    /// Whether its games are the real game without any window.
    pub fn headless(&self) -> bool {
        self.batch.as_ref().is_some_and(|b| b.mode == RunMode::Hidden)
    }

    /// Whether its accounts only connect to the server, without the game.
    pub fn connection_only(&self) -> bool {
        self.batch.as_ref().is_some_and(|b| b.mode == RunMode::Connection)
    }

    fn packs(&self) -> Packs {
        self.batch.as_ref().map_or(Packs::Ask, |b| b.packs)
    }
}

struct Shared {
    mods: PathBuf,
    config: PathBuf,
    resourcepacks: PathBuf,
    shaderpacks: PathBuf,
    datapacks: PathBuf,
    options: PathBuf,
    servers: PathBuf,
    saves: PathBuf,
}

impl Shared {
    fn of(instance: &Instance) -> Self {
        Self {
            mods: instance.mods_dir(),
            config: instance.config_dir(),
            resourcepacks: instance.resourcepacks_dir(),
            shaderpacks: instance.shaderpacks_dir(),
            datapacks: instance.datapacks_dir(),
            options: instance.options_template(),
            servers: instance.servers_template(),
            saves: instance.dir().join(super::launchers::SAVES_SEED),
        }
    }
}

/// Readies the game folder of `account` in `instance` and returns it: shared `mods`, `config`, `resourcepacks`,
/// `shaderpacks` and `datapacks` linked in, `options.txt` and `servers.dat` from the instance templates, an old companion gone from the shared mods folder.
pub fn prepare(instance: &Instance, account: &Account) -> Result<PathBuf> {
    instance::existing(&instance.id)?;
    let dir = instance.game_dir(account.id);
    prepare_in(&dir, &Shared::of(instance), &instance.other_games(account.id))?;
    Ok(dir)
}

/// Points every link in the game folders of the instance folder `instance_dir` that leads to a shared folder at that
/// folder in `instance_dir` again, after the instance folder moved. Only links change; real folders stay as they are.
pub fn relink(instance_dir: &Path) -> Result<()> {
    let games = super::instance::games_dir(instance_dir);
    let entries = match fs::read_dir(&games) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", games.display())),
    };
    let mut first = Ok(());
    for game in entries.filter_map(Result::ok) {
        for name in LINKED {
            let path = game.path().join(name);
            let target = instance_dir.join(name);
            let linked = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) || is_junction(&path);
            if !linked || points_to(&path, &target) {
                continue;
            }
            let renewed = fs::create_dir_all(&target).with_context(|| format!("cannot create {}", target.display())).and_then(|()| remove_link(&path)).and_then(|()| create_link(&target, &path).with_context(|| format!("cannot link {} to {}", path.display(), target.display())));
            if let Err(e) = renewed
                && first.is_ok()
            {
                first = Err(e);
            }
        }
    }
    first
}

fn prepare_in(dir: &Path, shared: &Shared, siblings: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for (name, target, merge) in [("mods", &shared.mods, false), ("config", &shared.config, false), ("resourcepacks", &shared.resourcepacks, true), ("shaderpacks", &shared.shaderpacks, true), ("datapacks", &shared.datapacks, true)] {
        fs::create_dir_all(target).with_context(|| format!("cannot create {}", target.display()))?;
        if merge {
            merge_into(&dir.join(name), target)?;
        }
        link(&dir.join(name), target)?;
    }
    drop_companion(&shared.mods);
    sync::adopt_options(&shared.options, dir, siblings)?;
    copy_options(&shared.options, &dir.join(OPTIONS_FILE))?;
    sync::copy_servers(&shared.servers, dir, siblings)?;
    adopt_saves(&shared.saves, dir)
}

fn adopt_saves(seed: &Path, dir: &Path) -> Result<()> {
    let target = dir.join("saves");
    if !seed.is_dir() || fs::symlink_metadata(&target).is_ok() {
        return Ok(());
    }
    fs::rename(seed, &target).with_context(|| format!("cannot move the imported worlds to {}", target.display()))
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
    share::writable(dest);
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

fn drop_companion(mods: &Path) {
    let _ = fs::remove_file(mods.join(COMPANION_FILE));
}

/// Keeps Forge and NeoForge from opening their own loading window, which would show the game before Manycraft places it.
fn quiet_early_window(config: &Path) -> Result<()> {
    let path = config.join(FML_CONFIG);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let quiet = early_window_off(&text);
    if quiet != text {
        net::write_atomic(&path, quiet.as_bytes())?;
    }
    Ok(())
}

fn early_window_off(text: &str) -> String {
    let wanted = format!("{EARLY_WINDOW} = false");
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| match l.split_once('=') {
            Some((key, _)) if key.trim() == EARLY_WINDOW => {
                found = true;
                wanted.clone()
            }
            _ => l.to_owned(),
        })
        .collect();
    if !found {
        out.push(wanted);
    }
    out.join("
") + "
"
}

/// The addresses of `server`, with its SRV record when it names no port; cached for a while.
pub(crate) fn resolve_all(server: &str) -> Result<Vec<SocketAddr>> {
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

/// The one name of a server address: the host in lower case with the port, 25565 when none is given, an IPv6 host in
/// brackets. Accepted codes of conduct and the connection-only client's reports both name and compare servers by it.
pub fn server_name(address: &str) -> String {
    match split_server(address) {
        Ok((host, port)) => {
            let (host, port) = (host.to_ascii_lowercase(), port.unwrap_or(DEFAULT_PORT));
            if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
        }
        Err(_) => address.trim().to_ascii_lowercase(),
    }
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

fn extra_arguments(target: &Target, account: &Account, game_dir: &Path, ipc_address: &str, key: &str, token: &str) -> Result<Extra> {
    let instance = &target.instance;
    let mut jvm = instance.jvm_args.clone();
    jvm.extend([format!("-Dmanycraft.ipc={ipc_address}"), format!("-Dmanycraft.instance={key}")]);
    if target.batch.is_some() {
        jvm.extend([format!("-Dmanycraft.preview={}", game_dir.join(PREVIEW_FILE).display()), "-Dmanycraft.hidden=true".to_owned()]);
    }
    if target.headless() {
        jvm.push(HEADLESS_FLAG.to_owned());
    }
    let mut game = instance.options.game_arguments(&instance.game_version);
    if target.batch.is_none() {
        game.extend(default_window(&instance.options, &super::settings::active().game));
    }
    let mut secrets = vec![(TOKEN_VAR.to_owned(), token.to_owned())];
    if cfg!(target_os = "macos") && target.headless() {
        secrets.push((MAC_BACKGROUND_VAR.to_owned(), "1".to_owned()));
    }
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

/// The size and fullscreen a game played alone gets from the launcher defaults where its instance sets neither.
fn default_window(options: &LaunchOptions, game: &super::settings::Game) -> Vec<String> {
    let mut out = Vec::new();
    if options.window.is_none()
        && let Some(size) = game.window
    {
        out.extend(["--width".to_owned(), size.width.to_string(), "--height".to_owned(), size.height.to_string()]);
    }
    if game.fullscreen && options.window.is_none() && !options.game_args.iter().any(|a| a == "--fullscreen") {
        out.push("--fullscreen".to_owned());
    }
    out
}

/// Starts with the Java the user picked for `instance` when it fits the game, else with the launcher's default Java
/// when that fits, and gives that Java the graphics card the instance or the launcher default names.
fn choose_java(installed: &mut Installed, instance: &Instance) -> Result<()> {
    let declared = installed.profile.java_version.as_ref();
    let required = javas::required_major(declared.and_then(|j| j.major_version), declared.map(|j| j.component.as_str()));
    let defaults = &super::settings::active().game;
    if let Some(picked) = &instance.options.java {
        let home = javas::home_of(picked).with_context(|| super::i18n::trf("Das gewählte Java fehlt: %@", &[&picked.display()]))?;
        let java = javas::probe(&home)?;
        if let Some(reason) = javas::mismatch(required, java.major, &instance.game_version) {
            bail!(reason);
        }
        installed.java = java.path;
    } else if let Some(standard) = &defaults.java {
        match javas::home_of(standard).ok_or_else(|| anyhow!("{} is missing", standard.display())).and_then(|home| javas::probe(&home)) {
            Ok(java) if javas::mismatch(required, java.major, &instance.game_version).is_none() => installed.java = java.path,
            Ok(java) => eprintln!("default Java {} does not fit {}, using the version's own", java.major, instance.game_version),
            Err(e) => eprintln!("default Java: {e:#}"),
        }
    }
    if let Err(e) = gpu::apply(&installed.java, &instance.id, instance.options.gpu.or(defaults.gpu)) {
        eprintln!("graphics card preference: {e:#}");
    }
    Ok(())
}

fn sync_mods(instance: &Instance) -> Result<()> {
    if instance.loader.takes_mods() {
        super::modrinth::sync_mods(instance)?;
    }
    Ok(())
}

/// Deletes `target`; its instance with mods, configs, packs, templates and game folders stays.
pub fn delete_batch(target: &Batch) -> Result<String> {
    batch::trash(target)
}

/// The window icon the game of `account` in `instance` shows, written at each launch.
pub fn icon_path(instance: &Instance, account: Uuid) -> PathBuf {
    instance.game_dir(account).join(ICON_FILE)
}

/// What the companion of `account` playing `target` needs to know: name, colour, icon, badge and background level.
pub fn identity_messages(target: &Target, account: &Account, badge: bool) -> Vec<ipc::ToMod> {
    vec![
        ipc::ToMod::Identity { name: account.name.clone(), batch: target.label().to_owned(), rgb: account.rgb(), icon: Some(icon_path(&target.instance, account.id).display().to_string()) },
        ipc::ToMod::Badge { visible: badge },
        ipc::ToMod::Level(target.background().into()),
        ipc::ToMod::Packs { policy: target.packs() },
    ]
}

/// Where the companion of `account` in `instance` writes its preview frames.
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
    /// Hands `message` over when the process takes messages by itself instead of through the IPC server.
    fn deliver(&self, _message: &ipc::ToMod) -> Option<Result<()>> {
        None
    }
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>>;
    fn kill(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<Option<i32>>;
    fn close(&self) -> bool {
        super::game_window::close(self.id())
    }
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

/// What a connection-only account's writer thread is told. Writes can block for seconds while a server stalls, so none
/// of them runs under the fleet's locks.
enum Job {
    Send(ipc::ToMod),
    Retire(Arc<headless::Connection>),
}

/// A connection-only account as a process: it has no id and ends when the connection does.
struct Linked {
    connection: Option<Arc<headless::Connection>>,
    jobs: Sender<Job>,
}

impl Linked {
    fn new(connection: headless::Connection) -> Self {
        let connection = Arc::new(connection);
        let held = Arc::clone(&connection);
        let (jobs, queue) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("link-send".into())
            .spawn(move || {
                for job in queue {
                    match job {
                        Job::Send(message) => {
                            let _ = held.send(&message);
                        }
                        Job::Retire(last) => {
                            drop(last);
                            break;
                        }
                    }
                }
            })
            .ok();
        Self { connection: Some(connection), jobs }
    }

    fn running(&self) -> bool {
        self.connection.as_ref().is_some_and(|c| c.is_running())
    }

    fn enqueue(&self, message: ipc::ToMod) -> Result<()> {
        if !self.running() {
            bail!("the connection is closed");
        }
        self.jobs.send(Job::Send(message)).map_err(|_| anyhow!("the connection is closed"))
    }
}

impl Drop for Linked {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let _ = self.jobs.send(Job::Retire(connection));
        }
    }
}

impl Process for Linked {
    fn id(&self) -> u32 {
        0
    }

    fn deliver(&self, message: &ipc::ToMod) -> Option<Result<()>> {
        Some(self.enqueue(message.clone()))
    }

    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        Ok((!self.running()).then_some(None))
    }

    fn kill(&mut self) -> io::Result<()> {
        let _ = self.enqueue(ipc::ToMod::Quit);
        Ok(())
    }

    fn wait(&mut self) -> io::Result<Option<i32>> {
        Ok(None)
    }

    fn close(&self) -> bool {
        self.enqueue(ipc::ToMod::Quit).is_ok()
    }
}

struct Held {
    child: Child,
    in_use: Option<lock::Guard>,
    instance_in_use: Option<lock::Guard>,
}

impl Held {
    fn launch(instance: &str, launch: impl FnOnce() -> Result<Child>) -> Result<Box<dyn Process>> {
        let hold = instance::hold(instance).with_context(|| super::i18n::tr("Die Instanz wird gerade umbenannt oder zurückgesetzt. Versuch es gleich noch einmal."))?;
        Ok(Box::new(Self { child: launch()?, in_use: install::clean::in_use().ok(), instance_in_use: Some(hold) }))
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
            self.instance_in_use = None;
        }
        Ok(exit)
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(&mut self.child)
    }

    fn wait(&mut self) -> io::Result<Option<i32>> {
        let code = Process::wait(&mut self.child);
        self.in_use = None;
        self.instance_in_use = None;
        code
    }
}

struct Adopted {
    process: adopt::Process,
    in_use: Option<lock::Guard>,
    instance_in_use: Option<lock::Guard>,
}

impl Process for Adopted {
    fn id(&self) -> u32 {
        self.process.id()
    }

    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        let exit = self.process.try_wait()?;
        if exit.is_some() {
            self.in_use = None;
            self.instance_in_use = None;
        }
        Ok(exit)
    }

    fn kill(&mut self) -> io::Result<()> {
        self.process.kill()
    }

    fn wait(&mut self) -> io::Result<Option<i32>> {
        let code = self.process.wait();
        self.in_use = None;
        self.instance_in_use = None;
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
        scrub::scrub_game_logs(&self.logs);
    }
}

/// A target readied by `Fleet::prepare_launch`: mods synced, server found, game installed.
pub struct Prepared {
    target: Target,
    server_addrs: Vec<SocketAddr>,
    way: Way,
}

enum Way {
    Game { installed: Installed, companion_jar: Option<PathBuf> },
    Connection,
}

impl Prepared {
    /// Whether the games run without the companion: no preview, no background mode, no swapping.
    pub fn plain(&self) -> bool {
        matches!(&self.way, Way::Game { companion_jar: None, .. })
    }
}

/// A game an earlier manager launched that still runs and is tracked again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adoption {
    pub key: String,
    /// The batch the game runs in; `None` when played alone.
    pub batch: Option<String>,
    pub instance: String,
    pub account: Uuid,
    pub pid: u32,
    /// Whether it runs without the companion, so no mod will ever report.
    pub plain: bool,
    /// Whether it runs without any window.
    pub headless: bool,
}

/// Outcome of starting one account: its instance key, or why it did not start.
pub type Started = (Uuid, Result<String>);

/// The running instances and the IPC server their companions connect to.
pub struct Fleet {
    ipc: ipc::Server,
    running: Arc<Mutex<HashMap<String, RunningInstance>>>,
    reserved: Mutex<HashMap<String, Reservation>>,
    games: sync::Games,
    reaping: Arc<AtomicBool>,
    closing: AtomicBool,
    ledger: Option<adopt::Ledger>,
    adopted: Mutex<HashMap<String, String>>,
    line: stagger::Line,
    events: Sender<ipc::Event>,
    notes: Sender<sync::Note>,
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
        Self::with(ipc, raw, None)
    }

    /// The fleet of the manager: it listens where the last manager did, records every launch, and tracks again the
    /// games the last manager launched that still run.
    pub fn manager() -> Result<(Fleet, Receiver<ipc::Event>, Vec<Adoption>)> {
        let ledger = adopt::Ledger::open();
        let (ipc, raw) = ipc::Server::start_on(ledger.port())?;
        ledger.set_port(ipc.port());
        let (fleet, events) = Self::with(ipc, raw, Some(ledger.clone()))?;
        let adopted = fleet.adopt(&ledger.games());
        ledger.keep_only(&adopted.iter().map(|a| a.key.clone()).collect::<Vec<_>>());
        Ok((fleet, events, adopted))
    }

    fn with(ipc: ipc::Server, raw: Receiver<ipc::Event>, ledger: Option<adopt::Ledger>) -> Result<(Fleet, Receiver<ipc::Event>)> {
        let games = sync::Games::default();
        let (events, sender, notes) = sync::spawn(ipc.clone(), games.clone(), raw)?;
        Ok((Fleet { ipc, running: Arc::default(), reserved: Mutex::default(), games, reaping: Arc::default(), closing: AtomicBool::new(false), ledger, adopted: Mutex::default(), line: stagger::Line::default(), events: sender, notes, stagger: DEFAULT_STAGGER }, events))
    }

    fn adopt(&self, runs: &BTreeMap<String, adopt::Run>) -> Vec<Adoption> {
        let mut adopted = Vec::new();
        for (key, run) in runs {
            let Some(process) = adopt::Process::adopt(run) else { continue };
            let pid = process.id();
            sync::register(&self.games, key, &run.instance, run.account);
            lock(&self.adopted).insert(key.clone(), run.token.clone());
            let instance = RunningInstance {
                instance: run.instance.clone(),
                account: run.account,
                child: Box::new(Adopted { process, in_use: install::clean::in_use().ok(), instance_in_use: instance::hold(&run.instance) }),
                pid,
                server_addr: run.server_addrs.first().copied(),
                server_addrs: run.server_addrs.clone(),
                started: Instant::now(),
                exit: None,
                logs: run.game_dir.join("logs"),
            };
            self.insert(key.clone(), instance);
            if !run.headless
                && let Ok(dir) = instance::existing(&run.instance)
            {
                let start = share::Start { key: key.clone(), instance: run.instance.clone(), name: share::name_in(&dir), dir, game: run.game_dir.clone(), pid, companion: !run.plain, at: SystemTime::now(), adopted: true };
                let _ = self.notes.send(sync::Note::Started(start));
            }
            adopted.push(Adoption { key: key.clone(), batch: run.batch.clone(), instance: run.instance.clone(), account: run.account, pid, plain: run.plain, headless: run.headless });
        }
        adopted
    }

    /// Lets the companions of adopted games connect again, once their tiles exist.
    pub fn admit(&self, keys: &[String]) {
        let mut adopted = lock(&self.adopted);
        for key in keys {
            if let Some(token) = adopted.remove(key) {
                self.ipc.register_with(key, &token);
            }
        }
    }

    /// Stops tracking adopted games that have no place any more; they keep running untouched.
    pub fn release(&self, keys: &[String]) {
        let mut gone = Vec::new();
        {
            let mut running = lock(&self.running);
            let mut adopted = lock(&self.adopted);
            for key in keys {
                if adopted.remove(key).is_some()
                    && let Some(instance) = running.remove(key)
                {
                    gone.push((key.clone(), instance.pid));
                }
            }
        }
        for (key, pid) in &gone {
            let _ = self.notes.send(sync::Note::Ended { key: key.clone(), pid: *pid });
        }
        if let Some(ledger) = &self.ledger {
            ledger.remove(&gone);
        }
    }

    fn ipc_file(&self) -> Option<PathBuf> {
        self.ledger.is_some().then(adopt::address_file)
    }

    /// Installs the target's game, then prepares and starts every account `stagger` apart. `companion_jar` replaces the
    /// built-in companion agent and must be named `manycraft-agent.jar`; without either the games run plain. Blocks until the last one is started.
    /// Fails as a whole when the game cannot be installed or the server not found; otherwise reports each account.
    pub fn start_batch(&self, target: &Target, accounts: &[Account], companion_jar: Option<&Path>, progress: &(dyn Fn(install::Progress) + Sync)) -> Result<Vec<Started>> {
        let prepared = self.prepare_launch(target, accounts, companion_jar, progress)?;
        let launch = |account: &Account, key: &str, token: &str| self.launch_prepared(&prepared, account, key, token);
        Ok(self.start_all(target, accounts, &prepared.server_addrs, &SystemClock, launch))
    }

    /// Syncs the instance's mods, finds the server and installs the game, once before `start_one` launches the accounts.
    /// Leaves the mods alone while games of the instance run, since they hold the jars open. `companion_jar` replaces the
    /// built-in companion agent for the instance's game.
    pub fn prepare_launch(&self, target: &Target, accounts: &[Account], companion_jar: Option<&Path>, progress: &(dyn Fn(install::Progress) + Sync)) -> Result<Prepared> {
        let instance = &target.instance;
        if target.connection_only() {
            return self.prepare_connection(target, accounts);
        }
        let _hold = instance::hold(&instance.id).with_context(|| super::i18n::tr("Die Instanz wird gerade umbenannt oder zurückgesetzt. Versuch es gleich noch einmal."))?;
        self.settle_exits();
        let joining = self.instance_runs(&instance.id);
        share::inbound(instance, joining)?;
        if !joining {
            sync_mods(instance)?;
        }
        let server_addrs = server_addresses(target.join(), accounts, resolve_all, || !super::online::reachable())?;
        let mut installed = install::install(&instance.game_version, &instance.loader, progress)?;
        choose_java(&mut installed, instance)?;
        let companion_jar = match companion_jar {
            Some(jar) => Some(jar.to_path_buf()),
            None if wants_companion(target, accounts) => super::companion::jar(&instance.game_version)?,
            None => None,
        };
        Ok(Prepared { target: target.clone(), server_addrs, way: Way::Game { installed, companion_jar } })
    }

    /// Waits until the settings of every game that ended were shared, so a start never writes over what they saved.
    fn settle_exits(&self) {
        let ended: Vec<(String, u32)> = lock(&self.running).iter_mut().filter_map(|(k, r)| (!r.poll()).then(|| (k.clone(), r.pid))).collect();
        for (key, pid) in ended {
            let _ = self.notes.send(sync::Note::Ended { key, pid });
        }
        let (done, answer) = std::sync::mpsc::channel();
        if self.notes.send(sync::Note::Flush(done)).is_ok() && answer.recv_timeout(EXITS_WAIT).is_err() {
            eprintln!("the settings of games that ended are still being shared");
        }
    }

    fn prepare_connection(&self, target: &Target, accounts: &[Account]) -> Result<Prepared> {
        let version = &target.instance.game_version;
        if !headless::supports(version) {
            bail!(super::i18n::trf("„Nur Verbindung“ geht für Minecraft %@ noch nicht. Schalte es aus oder wähle eine andere Version.", &[version]));
        }
        let Some(server) = target.join() else { bail!(super::i18n::tr("„Nur Verbindung“ braucht einen Server. Trag im Batch einen ein.")) };
        let server_addrs = server_addresses(Some(server), accounts, resolve_all, || !super::online::reachable())?;
        Ok(Prepared { target: target.clone(), server_addrs, way: Way::Connection })
    }

    fn launch_prepared(&self, prepared: &Prepared, account: &Account, key: &str, token: &str) -> Result<Box<dyn Process>> {
        match &prepared.way {
            Way::Game { installed, companion_jar } => {
                let (ipc_address, ipc_file) = (self.ipc.address(), self.ipc_file());
                Held::launch(&prepared.target.instance.id, || launch_one(&prepared.target, account, companion_jar.as_deref(), installed, &ipc_address, ipc_file.as_deref(), key, token))
            }
            Way::Connection => connect_one(&prepared.target, account, key, &self.events),
        }
    }

    /// Prepares and starts `account` in the prepared target right away and returns its key.
    /// Refuses a game that already runs, an account that already plays the instance elsewhere, and an account that is
    /// already on the same server in another batch.
    pub fn start_one(&self, prepared: &Prepared, account: &Account) -> Result<String> {
        self.launch_checked(&prepared.target, account, &prepared.server_addrs, Instant::now(), |account, key, token| self.launch_prepared(prepared, account, key, token))
    }

    fn instance_runs(&self, instance_id: &str) -> bool {
        lock(&self.running).values_mut().any(|r| r.instance == instance_id && r.poll())
    }

    /// Sends `message` to the companion of the instance `key`. Fails when it is not connected.
    pub fn send(&self, key: &str, message: &ipc::ToMod) -> Result<()> {
        if let Some(sent) = lock(&self.running).get(key).and_then(|r| r.child.deliver(message)) {
            return sent;
        }
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
        self.line.clear();
    }

    /// Puts `keys` in the line for a start, in order. Each of them later calls `wait_turn` and, when it did not start,
    /// `stop_all` or `give_up`.
    pub fn queue(&self, keys: &[String]) {
        self.line.join(keys);
    }

    /// Takes `key` out of the line: its start failed or it was not wanted any more.
    pub fn give_up(&self, key: &str) {
        self.line.leave(key);
    }

    /// Frees the place of `key`, whose game reached its title screen, for the next one in line.
    pub fn loaded(&self, key: &str) {
        self.line.loaded(key);
    }

    /// Waits until `key` is first in line, `stagger` passed since the last start and fewer than `parallel` games load
    /// (0 lets the machine's cores and free memory decide). A game without the companion counts as loaded when its
    /// window shows. Returns false when `abandon` or quitting ends the wait first; quitting empties the line, whereas
    /// after `abandon` the caller takes `key` out with `give_up` if the abandoning run still owns it, since a newer
    /// run may have queued the same key.
    pub fn wait_turn(&self, key: &str, parallel: u32, plain: bool, abandon: &dyn Fn() -> bool) -> bool {
        loop {
            if self.closing.load(Ordering::SeqCst) {
                self.line.leave(key);
                return false;
            }
            if abandon() {
                return false;
            }
            if self.line.turn(key, plain, Instant::now(), self.stagger, || parallel_limit(parallel), |loading, plain| self.through_loading(loading, plain)) {
                return true;
            }
            std::thread::sleep(TURN_POLL);
        }
    }

    fn through_loading(&self, key: &str, plain: bool) -> bool {
        let mut running = lock(&self.running);
        match running.get_mut(key) {
            Some(r) => !r.poll() || (plain && super::game_window::shown(r.pid)),
            None => !lock(&self.reserved).contains_key(key),
        }
    }

    fn launch_checked(&self, target: &Target, account: &Account, server_addrs: &[SocketAddr], started: Instant, launch: impl FnOnce(&Account, &str, &str) -> Result<Box<dyn Process>>) -> Result<String> {
        let key = target.key(account.id);
        self.reserve(&key, &target.instance.id, account, server_addrs)?;
        sync::register(&self.games, &key, &target.instance.id, account.id);
        let token = self.ipc.register(&key);
        let at = SystemTime::now();
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
        let game_dir = target.instance.game_dir(account.id);
        let plain = !super::companion::available(&target.instance.game_version) || !wants_companion(target, std::slice::from_ref(account));
        if let Some(ledger) = self.ledger.as_ref().filter(|_| !target.connection_only())
            && let Some(seen) = adopt::identify(pid)
        {
            let run = adopt::Run {
                batch: target.batch.as_ref().map(|b| b.id.clone()),
                instance: target.instance.id.clone(),
                account: account.id,
                pid,
                started: seen.started,
                exe: seen.exe,
                game_dir: game_dir.clone(),
                token,
                server_addrs: server_addrs.to_vec(),
                plain,
                headless: target.headless(),
            };
            ledger.add(&key, run);
        }
        let logs = game_dir.join("logs");
        let instance = RunningInstance {
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
        if !target.connection_only() {
            let start = share::Start { key: key.clone(), instance: target.instance.id.clone(), name: target.instance.name.clone(), dir: target.instance.dir(), game: game_dir, pid, companion: !plain, at, adopted: false };
            let _ = self.notes.send(sync::Note::Started(start));
        }
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
        let ledger = self.ledger.clone();
        let notes = self.notes.clone();
        std::thread::Builder::new().name("fleet-reaper".into()).spawn(move || {
            loop {
                std::thread::sleep(REAP_INTERVAL);
                let Some(running) = running.upgrade() else { return };
                let mut alive = false;
                let mut ended = Vec::new();
                for (key, instance) in lock(&running).iter_mut() {
                    if instance.poll() {
                        alive = true;
                    } else {
                        ipc.unregister(key);
                        ended.push((key.clone(), instance.pid));
                    }
                }
                for (key, pid) in &ended {
                    let _ = notes.send(sync::Note::Ended { key: key.clone(), pid: *pid });
                }
                if let Some(ledger) = &ledger
                    && !ended.is_empty()
                {
                    ledger.remove(&ended);
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
    /// Keeps the window of the game `key`, which runs without the companion, on the monitor of `home` while it
    /// opens, in the background.
    pub fn bring_home(&self, key: &str, home: super::place::Rect) {
        let Some(pid) = lock(&self.running).get(key).map(|r| r.pid) else { return };
        let _ = std::thread::Builder::new().name("bring-home".into()).spawn(move || super::game_window::bring_home(pid, home, HOME_WAIT, HOME_SETTLE));
    }

    /// Brings the window of the game `key` to the front, restored when it was minimized.
    pub fn raise(&self, key: &str) {
        if let Some(pid) = lock(&self.running).get(key).map(|r| r.pid) {
            super::game_window::raise(pid);
        }
    }

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

    /// Asks the instances `keys` to quit together, through Manycraft's mod or else their window, and ends those that
    /// have not after [`STOP_GRACE`], so each can save its world.
    pub fn stop_all(&self, keys: &[String]) {
        for key in keys {
            self.line.leave(key);
        }
        let runs = self.runs(keys);
        let asked: Vec<&(String, Option<Instant>)> = runs
            .iter()
            .filter(|(_, run)| run.is_some())
            .filter(|(k, _)| self.ipc.send(k, &ipc::ToMod::Quit).is_ok() || lock(&self.running).get(k.as_str()).is_some_and(|r| r.child.close()))
            .collect();
        let deadline = Instant::now() + STOP_GRACE;
        while asked.iter().any(|(k, run)| self.run(k) == *run) && Instant::now() < deadline {
            std::thread::sleep(QUIT_POLL);
        }
        self.end(&runs);
    }

    /// Ends the instances `keys` at once, without waiting for them to save.
    pub fn kill(&self, keys: &[String]) {
        self.end(&self.runs(keys));
    }

    fn run(&self, key: &str) -> Option<Instant> {
        lock(&self.running).get_mut(key).and_then(|r| r.poll().then_some(r.started))
    }

    fn runs(&self, keys: &[String]) -> Vec<(String, Option<Instant>)> {
        keys.iter().map(|k| (k.clone(), self.run(k))).collect()
    }

    fn end(&self, runs: &[(String, Option<Instant>)]) {
        let mut running = lock(&self.running);
        for (key, run) in runs {
            let now = running.get_mut(key).and_then(|r| r.poll().then_some(r.started));
            if now != *run {
                continue;
            }
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

fn parallel_limit(parallel: u32) -> usize {
    if parallel > 0 {
        return parallel as usize;
    }
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    stagger::auto_parallel(cores, super::fit::system_memory().map(|m| m.free_mb))
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

#[allow(clippy::too_many_arguments)]
fn launch_one(target: &Target, account: &Account, companion_jar: Option<&Path>, installed: &Installed, ipc_address: &str, ipc_file: Option<&Path>, key: &str, token: &str) -> Result<Child> {
    let instance = &target.instance;
    let game_dir = prepare(instance, account)?;
    require_companion_for_proxy(account, companion_jar)?;
    require_companion_for_headless(target, companion_jar)?;
    if matches!(instance.loader, Loader::Forge(_) | Loader::NeoForge(_)) {
        quiet_early_window(&instance.config_dir())?;
    }
    share::verify_game(&instance.options_template(), &game_dir.join(OPTIONS_FILE), share::data_version(&installed.classpath, &[&installed.game_version, &installed.version_name]))?;
    let _ = super::game_icon::write(account, &game_dir.join(ICON_FILE));
    let mut extra = extra_arguments(target, account, &game_dir, ipc_address, key, token)?;
    extra.jvm.splice(0..0, batch_defaults(target, installed, companion_jar.is_some()));
    if let Some(jar) = companion_jar {
        extra.jvm.push(format!("-javaagent:{}", jar.display()));
    }
    if let Some(file) = ipc_file {
        extra.jvm.push(format!("-Dmanycraft.ipcfile={}", file.display()));
    }
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
        server: target.join().map(str::to_owned),
        world: match &target.quick {
            Some(Quick::World(folder)) if target.server().is_none() => Some(folder.clone()),
            _ => None,
        },
        extra_jvm: extra.jvm,
        extra_game: extra.game,
        env: launch::SecretEnv(extra.secrets),
    };
    launch::spawn(&spec)
}

/// The JVM arguments a game of `target` starts with before the instance's own: the batch defaults for batch games, which
/// start hidden; none for a game played alone, which runs like in any launcher. Batch games compile with the quick
/// compiler only while the `companion` keeps them hidden, unless their batch asks for full speed.
fn batch_defaults(target: &Target, installed: &Installed, companion: bool) -> Vec<String> {
    let Some(batch) = &target.batch else { return Vec::new() };
    let major = if target.instance.options.java.is_none() { java_major(installed) } else { None };
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
    launch::batch_jvm(major, cores, companion && !batch.full_speed, &target.instance.jvm_args)
}

fn java_major(installed: &Installed) -> Option<u32> {
    installed.profile.java_version.as_ref().and_then(|j| j.major_version)
}

/// The addresses of `server` on this machine, plus one stand-in for its name so that accounts behind a proxy, which
/// resolve the name through it, still clash across batches. The lookup is skipped when all accounts have a proxy and
/// may fail when some do.
fn server_addresses(server: Option<&str>, accounts: &[Account], lookup: impl Fn(&str) -> Result<Vec<SocketAddr>>, offline: impl Fn() -> bool) -> Result<Vec<SocketAddr>> {
    let Some(server) = server else { return Ok(Vec::new()) };
    let name = name_stand_in(server)?;
    let proxied = accounts.iter().filter(|a| a.proxy.is_some()).count();
    if !accounts.is_empty() && proxied == accounts.len() {
        return Ok(vec![name]);
    }
    let mut found = match lookup(server) {
        Err(_) if proxied > 0 || offline() => Vec::new(),
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

/// Whether the games of `target` start with the companion: always in a batch, and alone when the instance asks for it
/// or an account has a proxy, which must not connect without it.
pub fn wants_companion(target: &Target, accounts: &[Account]) -> bool {
    target.batch.is_some() || target.instance.companion || accounts.iter().any(|a| a.proxy.is_some())
}

fn require_companion_for_proxy(account: &Account, companion_jar: Option<&Path>) -> Result<()> {
    if account.proxy.is_some() && companion_jar.is_none_or(|jar| !jar.is_file()) {
        bail!(super::i18n::trf("Der Proxy braucht Minecraft %@ oder neuer. Entferne ihn oder wähle eine neuere Version.", &[&super::companion::first()]));
    }
    Ok(())
}

/// Starts the connection of `account` to the batch's server: no game folder, no Java, only the signed-in session.
fn connect_one(target: &Target, account: &Account, key: &str, events: &Sender<ipc::Event>) -> Result<Box<dyn Process>> {
    let proxy = match account.proxy.as_deref() {
        Some(url) => Some(proxy::parse(url).map_err(|_| refusal(headless::Refused::Proxy))?),
        None => None,
    };
    let session = match account.kind {
        AccountKind::Offline => {
            account::validate_offline_name(&account.name)?;
            None
        }
        AccountKind::Microsoft => Some(auth::session_for(account)?),
    };
    let address = target.join().context("no server")?.to_owned();
    let conduct = target.batch.as_ref().map(|b| b.conduct_hashes(&address)).unwrap_or_default();
    let spec = headless::Spec { key: key.to_owned(), address, name: account.name.clone(), uuid: account.id, session, version: target.instance.game_version.clone(), packs: target.packs(), proxy, conduct };
    match headless::start(spec, events.clone()) {
        Ok(connection) => Ok(Box::new(Linked::new(connection))),
        Err(e) => Err(match e.downcast_ref::<headless::Refused>() {
            Some(why) => refusal(*why),
            None => e,
        }),
    }
}

fn refusal(why: headless::Refused) -> anyhow::Error {
    anyhow!(match why {
        headless::Refused::Proxy => super::i18n::tr("Mit einem Proxy geht „Nur Verbindung“ noch nicht. Entferne den Proxy oder schalte den Modus aus."),
        headless::Refused::Version => super::i18n::tr("Diese Minecraft-Version geht mit „Nur Verbindung“ noch nicht."),
    })
}

/// A game without the companion would open its window, so a headless batch does not start one.
fn require_companion_for_headless(target: &Target, companion_jar: Option<&Path>) -> Result<()> {
    if target.headless() && companion_jar.is_none_or(|jar| !jar.is_file()) {
        bail!(super::i18n::trf("Ohne Spielfenster geht erst ab Minecraft %@. Schalte es aus oder wähle eine neuere Version.", &[&super::companion::first()]));
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

pub(super) fn flags<'a>(args: &[&'a str], allowed: &[&str]) -> Option<HashMap<&'a str, &'a str>> {
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

pub(super) fn parse_loader(text: &str) -> Result<Loader> {
    instance::LOADERS.into_iter().find(|l| l.name().eq_ignore_ascii_case(text)).with_context(|| format!("unknown loader {text}: vanilla, fabric, quilt, forge or neoforge"))
}

/// Saves a new instance named `name`, for the newest release unless `version` names one, with Manycraft's default mods for Fabric.
pub(super) fn new_instance(name: &str, version: Option<&str>, loader: Loader, memory: Option<u32>) -> Result<Instance> {
    let version = match version {
        Some(v) => v.to_owned(),
        None => install::latest_release()?,
    };
    let mut made = Instance::new(name, &version, loader);
    if memory.is_some() {
        made.set_memory(memory);
    }
    if super::companion::available(&made.game_version) {
        let defaults = super::modrinth::default_mods(&made.game_version, &made.loader)?;
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
        let (played, memory) = instance_of(&instances, &b).map_or_else(|_| ("-".to_owned(), b.memory_override.unwrap_or_default()), |i| (format!("{} ({})", i.name, i.version_line()), b.memory(i.memory())));
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
        println!("{} ({})  {}  {} mods  {} MB  batches: {}", i.name, i.id, i.version_line(), i.mods.len() + i.local_mods.len(), i.memory(), if users.is_empty() { "-".to_owned() } else { users.join(", ") });
    }
    Ok(0)
}

fn delete_instance(who: &str) -> Result<i32> {
    let instances = instance::load_all()?;
    let doomed = find_instance(&instances, who)?;
    let entry = remove_instance(doomed)?;
    instance::purge(&entry)?;
    println!("Deleted instance {} with its mods, configs and worlds", doomed.name);
    Ok(0)
}

/// Why batches keep an instance from going: "Wird vom Batch „A“ benutzt" or "Wird von den Batches „A“, „B“ benutzt".
pub fn used_by(batches: &[String]) -> String {
    let quoted: Vec<String> = batches.iter().map(|n| super::i18n::trf("„%@“", &[n])).collect();
    match batches {
        [one] => super::i18n::trf("Wird vom Batch „%@“ benutzt", &[one]),
        _ => super::i18n::trf("Wird von den Batches %@ benutzt", &[&quoted.join(", ")]),
    }
}

fn check_unused(doomed: &Instance) -> Result<()> {
    let users: Vec<String> = batch::load_all()?.into_iter().filter(|b| b.instance_id == doomed.id).map(|b| b.name).collect();
    if users.is_empty() { Ok(()) } else { bail!(used_by(&users)) }
}

/// Moves `doomed` with its mods, configs and worlds to the trash and returns its trash entry; refused while a batch
/// plays it or any game runs. [`restore_instance`] brings it back until it is purged.
pub fn remove_instance(doomed: &Instance) -> Result<String> {
    check_unused(doomed)?;
    let _quiet = lock::try_exclusive(&paths::data_dir().join(install::clean::LOCK_FILE))?.with_context(|| super::i18n::tr("Gerade laufen Spiele. Stoppe sie zuerst."))?;
    let entry = instance::trash(doomed)?;
    release_gpu(doomed);
    Ok(entry)
}

fn release_gpu(gone: &Instance) {
    if gone.options.gpu.is_some()
        && let Err(e) = super::gpu::release(&gone.id)
    {
        eprintln!("graphics card preference of {} not released: {e:#}", gone.id);
    }
}

/// Moves `doomed` to the trash and returns its trash entry like [`remove_instance`], but without waiting for every game
/// to stop; refused only while a batch plays it. Callers check first that no game of it runs.
pub fn discard_instance(doomed: &Instance) -> Result<String> {
    check_unused(doomed)?;
    let entry = instance::trash(doomed)?;
    release_gpu(doomed);
    Ok(entry)
}

/// Brings the trash entry `entry` back out of the trash.
pub fn restore_instance(entry: &str) -> Result<Instance> {
    instance::untrash(entry)
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
    let companion = flags.get("--companion").map(PathBuf::from);
    for (id, outcome) in fleet.start_batch(target, accounts, companion.as_deref(), &report)? {
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
        ipc::Event::Reopen | ipc::Event::Play { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_reason_names_one_batch_or_all_of_them() {
        crate::core::i18n::init("de");
        assert_eq!(super::used_by(&["Farm".to_owned()]), "Wird vom Batch „Farm“ benutzt");
        assert_eq!(super::used_by(&["Farm".to_owned(), "Lobby".to_owned()]), "Wird von den Batches „Farm“, „Lobby“ benutzt");
    }

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
            datapacks: batch.join("datapacks"),
            options: batch.join(OPTIONS_FILE),
            servers: batch.join(sync::SERVERS_FILE),
            saves: batch.join(super::super::launchers::SAVES_SEED),
        };
        Dirs { inst: base.join("inst"), mods: shared.mods.clone(), config: shared.config.clone(), template: shared.options.clone(), shared, _root: root }
    }

    fn prep(d: &Dirs) -> Result<()> {
        prepare_in(&d.inst, &d.shared, &[])
    }

    #[test]
    fn imported_worlds_move_into_the_first_game_folder_once() {
        let d = dirs();
        fs::create_dir_all(d.shared.saves.join("World One")).unwrap();
        fs::write(d.shared.saves.join("World One").join("level.dat"), b"w").unwrap();
        prep(&d).unwrap();
        assert_eq!(fs::read(d.inst.join("saves").join("World One").join("level.dat")).unwrap(), b"w");
        assert!(!d.shared.saves.exists());
        let second = d.inst.with_file_name("inst2");
        prepare_in(&second, &d.shared, &[]).unwrap();
        assert!(!second.join("saves").exists());
    }

    #[test]
    fn imported_worlds_never_replace_saves_a_game_folder_has() {
        let d = dirs();
        fs::create_dir_all(d.shared.saves.join("Seed")).unwrap();
        fs::create_dir_all(d.inst.join("saves").join("Mine")).unwrap();
        prep(&d).unwrap();
        assert!(d.inst.join("saves").join("Mine").is_dir() && !d.inst.join("saves").join("Seed").exists());
        assert!(d.shared.saves.join("Seed").is_dir());
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
        prep(&d).unwrap();
        assert!(same(&own, &d.shared.resourcepacks));
        assert!(same(&d.inst.join("shaderpacks"), &d.shared.shaderpacks));
        assert_eq!(fs::read(d.shared.resourcepacks.join("mine.zip")).unwrap(), b"mine");
        assert_eq!(fs::read(d.shared.resourcepacks.join("both.zip")).unwrap(), b"batch copy");
        assert_eq!(fs::read(d.inst.join("resourcepacks.old-1").join("both.zip")).unwrap(), b"own copy");
        assert_eq!(fs::read(d.shared.shaderpacks.join("s.zip")).unwrap(), b"s");
        assert!(!d.inst.join("shaderpacks.old-1").exists());
        prep(&d).unwrap();
        assert!(!d.inst.join("resourcepacks.old-2").exists());
        assert_eq!(fs::read(d.shared.resourcepacks.join("mine.zip")).unwrap(), b"mine");
    }

    #[test]
    fn the_server_list_comes_from_the_template_on_every_start() {
        let d = dirs();
        fs::create_dir_all(&d.inst).unwrap();
        fs::write(d.inst.join(sync::SERVERS_FILE), b"own").unwrap();
        prep(&d).unwrap();
        assert_eq!(fs::read(&d.shared.servers).unwrap(), b"own");
        fs::write(&d.shared.servers, b"batch").unwrap();
        prep(&d).unwrap();
        assert_eq!(fs::read(d.inst.join(sync::SERVERS_FILE)).unwrap(), b"batch");
        assert!(!d.inst.join("servers.dat.old-1").exists());
    }

    fn same(a: &Path, b: &Path) -> bool {
        fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
    }

    #[test]
    fn links_point_into_the_batch_and_are_reused() {
        let d = dirs();
        prep(&d).unwrap();
        fs::write(d.inst.join("mods").join("a.jar"), b"x").unwrap();
        fs::write(d.inst.join("config").join("c.txt"), b"y").unwrap();
        assert_eq!(fs::read(d.mods.join("a.jar")).unwrap(), b"x");
        assert_eq!(fs::read(d.config.join("c.txt")).unwrap(), b"y");
        prep(&d).unwrap();
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
        prep(&d).unwrap();
        assert_eq!(fs::read(d.inst.join("mods.old-2").join("mine.jar")).unwrap(), b"keep");
        assert_eq!(fs::read(d.inst.join("config.old-1")).unwrap(), b"a file");
        assert!(same(&d.inst.join("mods"), &d.mods));
        assert!(same(&d.inst.join("config"), &d.config));
        prep(&d).unwrap();
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
        prep(&d).unwrap();
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
        prep(&d).unwrap();
        assert!(same(&d.inst.join("mods"), &d.mods));
    }

    #[test]
    fn options_come_from_the_template_on_every_start() {
        let d = dirs();
        prep(&d).unwrap();
        assert!(!d.inst.join(OPTIONS_FILE).exists());
        fs::write(d.inst.join(OPTIONS_FILE), "fov:1.0\n").unwrap();
        prep(&d).unwrap();
        assert_eq!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap(), "fov:1.0\n");
        fs::write(&d.template, "soundCategory_master:0.0\nonboardAccessibility:false\nfullscreen:false\n").unwrap();
        fs::write(d.inst.join(OPTIONS_FILE), "fov:1.0\nfullscreen:true\nonboardAccessibility:true\n").unwrap();
        prep(&d).unwrap();
        assert_eq!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap(), "soundCategory_master:0.0\nonboardAccessibility:false\nfullscreen:true\nfov:1.0\n");
        fs::write(d.inst.join(OPTIONS_FILE), "soundCategory_master:1.0\n").unwrap();
        prep(&d).unwrap();
        assert!(fs::read_to_string(d.inst.join(OPTIONS_FILE)).unwrap().contains("soundCategory_master:0.0"));
    }

    #[test]
    fn an_unreadable_template_is_an_error() {
        let d = dirs();
        fs::create_dir_all(&d.template).unwrap();
        assert!(prep(&d).is_err());
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
    fn a_companion_mod_from_an_older_build_is_dropped() {
        let d = dirs();
        fs::create_dir_all(&d.mods).unwrap();
        fs::write(d.mods.join(COMPANION_FILE), b"v1").unwrap();
        fs::write(d.mods.join("other.jar"), b"mod").unwrap();
        prep(&d).unwrap();
        assert!(!d.mods.join(COMPANION_FILE).exists() && d.mods.join("other.jar").is_file());
    }

    #[test]
    fn the_fml_loading_window_is_turned_off_and_the_rest_stays() {
        assert_eq!(early_window_off(""), "earlyWindowControl = false\n");
        assert_eq!(early_window_off("#c\nearlyWindowControl = true\nmaxThreads = -1\n"), "#c\nearlyWindowControl = false\nmaxThreads = -1\n");
        assert_eq!(early_window_off("versionCheck = true"), "versionCheck = true\nearlyWindowControl = false\n");
        let dir = tempfile::tempdir().unwrap();
        quiet_early_window(dir.path()).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join(FML_CONFIG)).unwrap(), "earlyWindowControl = false\n");
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
    fn a_server_has_one_name_in_lower_case_with_its_port_that_names_itself_again() {
        for (address, name) in [("Play.Example.COM", "play.example.com:25565"), ("play.example.com:25570", "play.example.com:25570"), (" MC.example:25565 ", "mc.example:25565"), ("::1", "[::1]:25565"), ("[FD00::1]:1", "[fd00::1]:1"), ("not a server", "not a server")] {
            assert_eq!(server_name(address), name, "{address}");
            assert_eq!(server_name(&server_name(address)), name, "{address} is stable");
        }
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
        fn close(&self) -> bool {
            false
        }

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

    fn keys_of(batch: &Target, alts: &[Account]) -> Vec<String> {
        alts.iter().map(|a| batch.key(a.id)).collect()
    }

    #[test]
    fn a_game_waits_for_a_free_place_and_a_crash_frees_it() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        fleet.stagger = Duration::ZERO;
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne", "AltTwo"]);
        let keys = keys_of(&batch, &alts);
        fleet.queue(&keys);
        let never = || false;
        assert!(fleet.wait_turn(&keys[0], 1, false, &never));
        fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), launcher(&exits, &tokens)).unwrap();
        let exit = Arc::clone(&exits.borrow()[&alts[0].id]);
        let through = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                assert!(fleet.wait_turn(&keys[1], 1, false, &never));
                through.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(350));
            assert!(!through.load(Ordering::SeqCst), "the first game still loads");
            lock(&exit).replace(Some(1));
        });
        assert!(through.load(Ordering::SeqCst));
    }

    #[test]
    fn a_loaded_game_makes_room_and_more_places_let_more_load() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        fleet.stagger = Duration::ZERO;
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne", "AltTwo", "AltThree"]);
        let keys = keys_of(&batch, &alts);
        fleet.queue(&keys);
        let never = || false;
        for (key, alt) in keys.iter().zip(&alts).take(2) {
            assert!(fleet.wait_turn(key, 2, false, &never));
            fleet.launch_checked(&batch, alt, &[], Instant::now(), launcher(&exits, &tokens)).unwrap();
        }
        let through = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                assert!(fleet.wait_turn(&keys[2], 2, false, &never));
                through.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(350));
            assert!(!through.load(Ordering::SeqCst));
            fleet.loaded(&keys[0]);
        });
        assert!(through.load(Ordering::SeqCst));
    }

    #[test]
    fn cancelling_a_waiting_game_lets_the_next_one_go_and_stopping_clears_the_line() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        fleet.stagger = Duration::ZERO;
        let batch = target("A");
        let alts = accounts(&["AltOne", "AltTwo", "AltThree", "AltFour"]);
        let keys = keys_of(&batch, &alts);
        fleet.queue(&keys);
        assert!(!fleet.wait_turn(&keys[0], 1, false, &|| true));
        fleet.give_up(&keys[0]);
        assert!(fleet.wait_turn(&keys[1], 1, false, &|| false));
        fleet.stop_all(&keys[2..3]);
        fleet.give_up(&keys[1]);
        assert!(fleet.wait_turn(&keys[3], 1, false, &|| false));
        fleet.queue(&keys[..1]);
        fleet.close();
        assert!(!fleet.wait_turn(&keys[0], 1, false, &|| false));
    }

    #[test]
    fn a_game_abandoned_by_an_old_run_leaves_the_place_of_the_new_run_alone() {
        let (mut fleet, _events) = Fleet::new().unwrap();
        fleet.stagger = Duration::ZERO;
        let batch = target("A");
        let alts = accounts(&["AltOne", "AltTwo"]);
        let keys = keys_of(&batch, &alts);
        fleet.queue(&keys);
        fleet.stop_all(&keys[1..]);
        fleet.queue(&keys[1..]);
        assert!(!fleet.wait_turn(&keys[1], 1, false, &|| true));
        fleet.give_up(&keys[0]);
        assert!(fleet.wait_turn(&keys[1], 1, false, &|| false));
    }

    #[test]
    fn adopted_games_wait_for_their_tile_and_released_ones_keep_running() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = adopt::Ledger::at(dir.path().join("launched.json"));
        let (ipc, raw) = ipc::Server::start_on(None).unwrap();
        let (fleet, _events) = Fleet::with(ipc, raw, Some(ledger.clone())).unwrap();
        let mut kept = adopt::tests::sleeper();
        let mut freed = adopt::tests::sleeper();
        let mut runs = BTreeMap::new();
        for (key, child) in [("kept", &kept), ("freed", &freed)] {
            let run = adopt::tests::run(dir.path(), child.id(), &adopt::identify(child.id()).unwrap());
            ledger.add(key, run.clone());
            runs.insert(key.to_owned(), run);
        }
        let adopted = fleet.adopt(&runs);
        assert_eq!(adopted.len(), 2);
        assert_eq!(lock(&fleet.adopted).len(), 2, "no mod may connect before its tile exists");
        fleet.admit(&["kept".to_owned()]);
        fleet.release(&["freed".to_owned()]);
        assert!(lock(&fleet.adopted).is_empty());
        assert!(lock(&fleet.running).contains_key("kept"));
        assert!(!lock(&fleet.running).contains_key("freed"));
        assert_eq!(ledger.games().keys().collect::<Vec<_>>(), ["kept"]);
        assert!(freed.try_wait().unwrap().is_none(), "a released game is left running");
        let _ = freed.kill();
        let _ = kept.kill();
        let _ = freed.wait();
        let _ = kept.wait();
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
        fleet.stop_all(&keys);
        assert!(!fleet.is_running(&keys[1]));
        assert_eq!(fleet.exit_code(&keys[1]), Some(Some(1)));
    }

    #[test]
    fn start_options_reach_the_game_before_the_proxy() {
        let mut batch = target("A");
        batch.instance.game_version = "26.3".into();
        batch.instance.options = super::super::options::LaunchOptions {
            game_args: vec!["--fullscreen".into()],
            window: Some(super::super::options::WindowSize { width: 1280, height: 720 }),
            renderer: Some(super::super::options::Renderer::OpenGl),
            ..Default::default()
        };
        let mut alt = Account::offline("AltOne").unwrap();
        alt.proxy = Some("socks5://10.1.1.1:1081".into());
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert_eq!(extra.game, ["--fullscreen", "--width", "1280", "--height", "720", "--graphicsBackend", "opengl", "--proxyHost", "10.1.1.1", "--proxyPort", "1081"]);
    }

    #[test]
    fn launcher_defaults_fill_only_what_a_solo_instance_leaves_open() {
        use super::super::options::WindowSize;
        use super::super::settings::Game;
        let wide = Game { window: Some(WindowSize { width: 1920, height: 1080 }), fullscreen: true, ..Game::default() };
        assert_eq!(default_window(&LaunchOptions::default(), &wide), ["--width", "1920", "--height", "1080", "--fullscreen"]);
        let own = LaunchOptions { window: Some(WindowSize { width: 800, height: 600 }), game_args: vec!["--fullscreen".into()], ..Default::default() };
        assert!(default_window(&own, &wide).is_empty());
        let sized = LaunchOptions { window: Some(WindowSize { width: 800, height: 600 }), ..Default::default() };
        assert!(default_window(&sized, &wide).is_empty());
        assert!(default_window(&LaunchOptions::default(), &Game::default()).is_empty());
    }

    fn installed_needing(component: &str, major: u32) -> Installed {
        let profile: install::meta::VersionJson = serde_json::from_value(serde_json::json!({"id": "1.21.5", "javaVersion": {"component": component, "majorVersion": major}})).unwrap();
        Installed {
            game_version: "1.21.5".into(),
            loader_version: "0.20.1".into(),
            id: profile.id.clone(),
            version_name: profile.id.clone(),
            libraries_dir: PathBuf::new(),
            profile,
            java: PathBuf::from("mojang-java"),
            classpath: Vec::new(),
            natives_dir: PathBuf::new(),
            assets_dir: PathBuf::new(),
            asset_index: String::new(),
            log_config: None,
            downloaded: 0,
        }
    }

    #[test]
    fn batch_games_get_the_lean_defaults_and_solo_games_none() {
        let batch = target("A");
        let solo = Target::solo(&batch.instance);
        let java25 = installed_needing("java-runtime-epsilon", 25);
        let java21 = installed_needing("java-runtime-delta", 21);
        let quick = "-XX:TieredStopAtLevel=1".to_owned();
        let headers = "-XX:+UseCompactObjectHeaders".to_owned();
        assert!(batch_defaults(&batch, &java25, true).contains(&headers) && batch_defaults(&batch, &java25, true).contains(&quick));
        assert!(!batch_defaults(&batch, &java21, true).contains(&headers));
        assert!(batch_defaults(&solo, &java25, true).is_empty());
        assert!(!batch_defaults(&batch, &java25, false).contains(&quick), "without the companion a game may be played at any time");
        let mut full = batch.clone();
        full.batch.as_mut().unwrap().full_speed = true;
        assert!(!batch_defaults(&full, &java25, true).contains(&quick) && batch_defaults(&full, &java25, true).contains(&headers));
        let mut own_java = batch.clone();
        own_java.instance.options.java = Some(PathBuf::from("jdk-25"));
        assert!(!batch_defaults(&own_java, &java25, true).contains(&headers));
    }

    fn java_home(dir: &Path, version: &str) -> PathBuf {
        let bin = dir.join(format!("jdk-{version}")).join("bin");
        fs::create_dir_all(&bin).unwrap();
        let launcher = bin.join(if cfg!(windows) { "javaw.exe" } else { "java" });
        fs::write(&launcher, b"").unwrap();
        fs::write(bin.parent().unwrap().join("release"), format!("JAVA_VERSION=\"{version}\"
")).unwrap();
        launcher
    }

    #[test]
    fn an_own_java_replaces_mojangs_only_when_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        let mut instance = target("A").instance;
        instance.game_version = "1.21.5".into();
        let mut installed = installed_needing("java-runtime-delta", 21);
        choose_java(&mut installed, &instance).unwrap();
        assert_eq!(installed.java, PathBuf::from("mojang-java"));
        instance.options.java = Some(java_home(dir.path(), "25.0.1"));
        choose_java(&mut installed, &instance).unwrap();
        assert_eq!(Some(installed.java.clone()), instance.options.java);
        let mut installed = installed_needing("java-runtime-delta", 21);
        instance.options.java = Some(java_home(dir.path(), "17.0.2"));
        let reason = choose_java(&mut installed, &instance).unwrap_err().to_string();
        assert!(reason.contains("Java 21") && reason.contains("Java 17"), "{reason}");
        assert_eq!(installed.java, PathBuf::from("mojang-java"));
        instance.options.java = Some(dir.path().join("gone").join("bin").join("javaw.exe"));
        assert!(choose_java(&mut installed, &instance).is_err());
    }

    #[test]
    fn proxy_credentials_go_through_the_environment_never_the_command_line() {
        let batch = target("A");
        let mut alt = Account::offline("AltOne").unwrap();
        alt.proxy = Some("socks5://us%40r:p%3Ass@10.1.1.1:1081".into());
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
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
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert!(extra.game.is_empty());
        assert!(extra.jvm.contains(&"-Dhttps.proxyHost=proxy.example".to_owned()) && extra.jvm.contains(&"-Dhttps.proxyPort=3128".to_owned()));
        alt.proxy = None;
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert!(extra.game.is_empty() && !extra.jvm.iter().any(|a| a.contains("proxy")));
        assert_eq!(extra.secrets.len(), 1);
        alt.proxy = Some("ftp://x:1".into());
        assert!(extra_arguments(&batch, &alt, Path::new("inst"), "", "k", "tok").is_err());
    }

    #[test]
    fn the_companion_starts_in_batches_with_a_proxy_or_when_the_instance_asks() {
        let plain = Account::offline("AltOne").unwrap();
        let mut proxied = Account::offline("AltTwo").unwrap();
        proxied.proxy = Some("socks5://h:1".into());
        let mut instance = Instance::new("Solo", "26.3", Loader::default());
        let solo = Target::solo(&instance);
        assert!(!wants_companion(&solo, std::slice::from_ref(&plain)));
        assert!(wants_companion(&solo, &[plain.clone(), proxied.clone()]));
        assert!(wants_companion(&target("A"), std::slice::from_ref(&plain)));
        instance.companion = true;
        assert!(wants_companion(&Target::solo(&instance), &[plain]));
    }

    #[test]
    fn a_proxy_needs_the_companion() {
        let d = dirs();
        fs::create_dir_all(&d.mods).unwrap();
        let jar = d.mods.join("manycraft-agent.jar");
        let mut alt = Account::offline("AltOne").unwrap();
        assert!(require_companion_for_proxy(&alt, None).is_ok());
        alt.proxy = Some("socks5://h:1".into());
        assert!(format!("{:#}", require_companion_for_proxy(&alt, None).unwrap_err()).contains("Minecraft 26.1"));
        assert!(require_companion_for_proxy(&alt, Some(&jar)).is_err());
        fs::write(&jar, b"jar").unwrap();
        assert!(require_companion_for_proxy(&alt, Some(&jar)).is_ok());
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
        fleet.stop_all(std::slice::from_ref(&key));
        client.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(fleet.exit_code(&key), Some(Some(0)));
    }

    #[test]
    fn kill_ends_a_game_at_once_without_asking_it_to_quit() {
        let (fleet, _events) = Fleet::new().unwrap();
        let exits = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne"]);
        let key = fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), launcher(&exits, &tokens)).unwrap();
        assert!(fleet.is_running(&key));
        let started = Instant::now();
        fleet.kill(std::slice::from_ref(&key));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!fleet.is_running(&key));
        assert_eq!(fleet.exit_code(&key), Some(Some(1)));
    }

    #[test]
    fn a_late_stop_leaves_a_game_started_again_under_the_same_key_alone() {
        let (fleet, _events) = Fleet::new().unwrap();
        let exits: RefCell<Exits> = RefCell::default();
        let tokens = RefCell::default();
        let batch = target("A");
        let alts = accounts(&["AltOne"]);
        let key = fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), launcher(&exits, &tokens)).unwrap();
        let first = fleet.runs(std::slice::from_ref(&key));
        *lock(&exits.borrow()[&alts[0].id]) = Some(Some(0));
        std::thread::sleep(Duration::from_millis(5));
        let again = fleet.launch_checked(&batch, &alts[0], &[], Instant::now(), launcher(&exits, &tokens)).unwrap();
        assert_eq!(again, key);
        fleet.end(&first);
        assert!(fleet.is_running(&key));
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
        let extra = extra_arguments(&batch, &alt, &dir, "127.0.0.1:9", "k", "tok").unwrap();
        assert!(extra.jvm.contains(&format!("-Dmanycraft.preview={}", preview_path(&batch.instance, alt.id).display())));
    }

    #[test]
    fn a_connection_only_start_refuses_a_proxy_account() {
        let mut t = target("A");
        t.batch.as_mut().unwrap().mode = RunMode::Connection;
        t.batch.as_mut().unwrap().server = Some("127.0.0.1:9".into());
        assert!(t.connection_only() && !t.headless());
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut proxied = Account::offline("AltOne").unwrap();
        proxied.proxy = Some("socks5://127.0.0.1:1080".into());
        let err = connect_one(&t, &proxied, "k", &tx).err().expect("refused").to_string();
        assert!(err.contains("Proxy"), "{err}");
    }

    #[test]
    fn a_headless_batch_starts_its_games_windowless_answers_packs_its_way_and_needs_the_companion() {
        let mut batch = target("A");
        let alt = Account::offline("AltOne").unwrap();
        let plain = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert!(!plain.jvm.contains(&HEADLESS_FLAG.to_owned()));
        assert!(identity_messages(&batch, &alt, true).contains(&ipc::ToMod::Packs { policy: Packs::Ask }));
        let b = batch.batch.as_mut().unwrap();
        b.mode = RunMode::Hidden;
        b.packs = Packs::Decline;
        let extra = extra_arguments(&batch, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert!(extra.jvm.contains(&HEADLESS_FLAG.to_owned()) && extra.jvm.contains(&"-Dmanycraft.hidden=true".to_owned()));
        assert_eq!(extra.secrets.iter().any(|(k, _)| k == MAC_BACKGROUND_VAR), cfg!(target_os = "macos"));
        assert!(identity_messages(&batch, &alt, true).contains(&ipc::ToMod::Packs { policy: Packs::Decline }));
        assert!(require_companion_for_headless(&batch, None).is_err(), "without the companion the game would open a window");
        assert!(require_companion_for_headless(&target("B"), None).is_ok());
        assert!(!Target::solo(&batch.instance).headless());
    }

    #[test]
    fn a_game_played_alone_opens_as_a_normal_window_without_previews_or_background_mode() {
        let alone = Target::solo(&target("A").instance);
        let alt = Account::offline("AltOne").unwrap();
        let extra = extra_arguments(&alone, &alt, Path::new("inst"), "127.0.0.1:9", "k", "tok").unwrap();
        assert!(!extra.jvm.iter().any(|a| a.starts_with("-Dmanycraft.hidden") || a.starts_with("-Dmanycraft.preview")), "{:?}", extra.jvm);
        assert!(extra.jvm.contains(&"-Dmanycraft.ipc=127.0.0.1:9".to_owned()));
        assert!(identity_messages(&alone, &alt, true).contains(&ipc::ToMod::Level(ipc::Level::Full)));
        let batch = target("A");
        assert!(identity_messages(&batch, &alt, true).contains(&ipc::ToMod::Level(batch.background().into())));
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
        let online = || false;
        assert_eq!(server_addresses(None, &[direct.clone()], found, online).unwrap(), []);
        assert_eq!(server_addresses(Some("s"), &[direct.clone()], found, online).unwrap(), [addr, name]);
        assert!(server_addresses(Some("s"), &[direct.clone()], missing, online).is_err());
        assert_eq!(server_addresses(Some("s"), &[direct.clone()], missing, || true).unwrap(), [name]);
        assert_eq!(server_addresses(Some("s"), &[direct.clone(), proxied.clone()], found, online).unwrap(), [addr, name]);
        assert_eq!(server_addresses(Some("s"), &[direct, proxied.clone()], missing, online).unwrap(), [name]);
        let never = |_: &str| -> Result<Vec<SocketAddr>> { panic!("looked up on this machine") };
        assert_eq!(server_addresses(Some("s"), &[proxied.clone()], never, online).unwrap(), [name]);
        assert_eq!(name_stand_in("S").unwrap(), name);
        assert_eq!(name_stand_in("s:25565").unwrap(), name);
        assert_ne!(name_stand_in("s:25566").unwrap(), name);
        assert_ne!(name_stand_in("t").unwrap(), name);
        let other = server_addresses(Some("S"), &[proxied], never, online).unwrap();
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
