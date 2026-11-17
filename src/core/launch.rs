//! Builds the Java command line for an installed version and starts the game.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use super::install::meta::{Env, Os, expand, substitute};
use super::account::{self, AccountKind};
use super::auth;
use super::install::{self, Installed};
use super::instance::Loader;
use super::paths;

const LAUNCHER_NAME: &str = "Manycraft";
const CLASSPATH_SEPARATOR: &str = if cfg!(windows) { ";" } else { ":" };
const REDACTED: &str = "<redacted>";
const USAGE: &str = "usage: manycraft launch [--version <version>] [--account <name or id> | --name <offline name>] [--server <host:port>]";

/// A signed-in account's session. Its token is never printed.
#[derive(Clone)]
pub struct Session {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub xuid: String,
}

impl From<auth::Session> for Session {
    fn from(s: auth::Session) -> Self {
        Session { name: s.name, uuid: s.uuid.to_string(), access_token: s.access_token, xuid: s.xuid }
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").field("name", &self.name).field("uuid", &self.uuid).field("access_token", &REDACTED).field("xuid", &self.xuid).finish()
    }
}

/// Everything needed to start one instance. Without a session the game starts offline as `offline_name`.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub installed: Installed,
    pub game_dir: PathBuf,
    pub session: Option<Session>,
    pub offline_name: String,
    pub memory_mb: u32,
    pub server: Option<String>,
    /// The folder of a world in `saves` the game opens right away.
    pub world: Option<String>,
    pub extra_jvm: Vec<String>,
    pub extra_game: Vec<String>,
    pub env: SecretEnv,
}

/// Environment variables for the game process. Debug output shows only the names.
#[derive(Clone, Default)]
pub struct SecretEnv(pub Vec<(String, String)>);

impl fmt::Debug for SecretEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter().map(|(k, _)| k)).finish()
    }
}

/// The finished argument list. Debug output hides the access token.
#[derive(Clone)]
pub struct Arguments {
    list: Vec<String>,
    secret: Option<String>,
}

impl Arguments {
    pub fn as_slice(&self) -> &[String] {
        &self.list
    }
}

impl fmt::Debug for Arguments {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shown = self.list.iter().map(|arg| match &self.secret {
            Some(secret) if arg.contains(secret.as_str()) => arg.replace(secret.as_str(), REDACTED),
            _ => arg.clone(),
        });
        f.debug_list().entries(shown).finish()
    }
}

impl LaunchSpec {
    fn features(&self) -> Vec<&'static str> {
        match (&self.server, &self.world) {
            (Some(_), _) => vec!["has_quick_plays_support", "is_quick_play_multiplayer"],
            (None, Some(_)) => vec!["has_quick_plays_support", "is_quick_play_singleplayer"],
            (None, None) => Vec::new(),
        }
    }

    fn variables(&self) -> HashMap<&'static str, String> {
        let installed = &self.installed;
        let (name, uuid, token, xuid, user_type) = match &self.session {
            Some(s) => (s.name.clone(), s.uuid.clone(), s.access_token.clone(), s.xuid.clone(), "msa"),
            None => (self.offline_name.clone(), crate::core::account::offline_uuid(&self.offline_name).to_string(), "0".to_owned(), "0".to_owned(), "legacy"),
        };
        let classpath = installed.classpath.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(CLASSPATH_SEPARATOR);
        let version_type = installed.profile.kind.clone().unwrap_or_else(|| "release".to_owned());
        let session = match &self.session {
            Some(s) => format!("token:{}:{}", s.access_token, s.uuid.replace('-', "")),
            None => "-".to_owned(),
        };
        let game_assets = install::assets::legacy_dir(&installed.assets_dir, &installed.asset_index, &self.game_dir).unwrap_or_else(|| installed.assets_dir.clone());
        HashMap::from([
            ("auth_session", session),
            ("game_assets", game_assets.to_string_lossy().into_owned()),
            ("auth_player_name", name),
            ("auth_uuid", uuid),
            ("auth_access_token", token),
            ("auth_xuid", xuid),
            ("user_type", user_type.to_owned()),
            ("user_properties", "{}".to_owned()),
            ("clientid", String::new()),
            ("version_name", installed.version_name.clone()),
            ("library_directory", installed.libraries_dir.to_string_lossy().into_owned()),
            ("version_type", version_type),
            ("game_directory", self.game_dir.to_string_lossy().into_owned()),
            ("assets_root", installed.assets_dir.to_string_lossy().into_owned()),
            ("assets_index_name", installed.asset_index.clone()),
            ("natives_directory", installed.natives_dir.to_string_lossy().into_owned()),
            ("classpath_separator", CLASSPATH_SEPARATOR.to_owned()),
            ("classpath", classpath),
            ("launcher_name", LAUNCHER_NAME.to_owned()),
            ("launcher_version", env!("CARGO_PKG_VERSION").to_owned()),
            ("quickPlayPath", self.game_dir.join("quickPlay").join("java").join("manycraft.json").to_string_lossy().into_owned()),
            ("quickPlayMultiplayer", self.server.clone().unwrap_or_default()),
            ("quickPlaySingleplayer", self.world.clone().unwrap_or_default()),
        ])
    }

    /// JVM arguments, main class and game arguments in launch order.
    pub fn arguments(&self) -> Result<Arguments> {
        let profile = &self.installed.profile;
        let env = Env::current(self.features());
        let vars = self.variables();
        let main_class = profile.main_class.as_deref().context("version has no main class")?;
        let declared = profile.arguments.clone().unwrap_or_default();
        let mut jvm = expand(&declared.jvm, &env);
        if declared.jvm.is_empty() {
            jvm.extend(legacy_jvm(env.os));
        }
        if !jvm.iter().any(|a| a.starts_with("-Djava.library.path")) {
            jvm.push("-Djava.library.path=${natives_directory}".to_owned());
        }
        if !jvm.iter().any(|a| a.contains("${classpath}")) {
            jvm.extend(["-cp", "${classpath}"].map(str::to_owned));
        }
        let mut game = expand(&declared.game, &env);
        if let Some(legacy) = &profile.minecraft_arguments {
            game.extend(legacy.split_whitespace().map(str::to_owned));
        }
        if let (Some(config), Some(log)) = (&self.installed.log_config, profile.logging.as_ref().and_then(|l| l.client.as_ref())) {
            jvm.push(log.argument.replace("${path}", &config.to_string_lossy()));
        }
        let mut list: Vec<String> = jvm.iter().chain(&game).map(|a| substitute(a, &vars)).collect();
        let game_start = jvm.len();
        list.insert(game_start, main_class.to_owned());
        let mut extra = vec![format!("-Xmx{}M", self.memory_mb), format!("-XX:ErrorFile={}", self.game_dir.join("logs").join("hs_err_pid%p.log").display())];
        extra.extend(self.extra_jvm.iter().cloned());
        list.splice(game_start..game_start, extra);
        let secret = self.session.as_ref().map(|s| s.access_token.clone()).filter(|t| !t.is_empty());
        Ok(Arguments { list, secret })
    }

    /// The Java process, run from the game folder. Its arguments contain the access token: never print it.
    fn command(&self) -> Result<Command> {
        let mut command = Command::new(&self.installed.java);
        command.args(self.arguments()?.as_slice()).args(&self.extra_game).envs(self.env.0.iter().map(|(k, v)| (k, v))).current_dir(&self.game_dir);
        Ok(command)
    }
}

/// JVM arguments that versions without an `arguments` block expect from the launcher, as Mojang's launcher passes them.
fn legacy_jvm(os: Os) -> Vec<String> {
    let mut out = vec!["-Dminecraft.launcher.brand=${launcher_name}".to_owned(), "-Dminecraft.launcher.version=${launcher_version}".to_owned()];
    match os {
        Os::Windows => out.push("-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump".to_owned()),
        Os::Osx | Os::Linux => {}
    }
    out
}

/// Starts the game with output going to `logs/manycraft-launch.log` in the game folder. Old versions get their assets
/// laid out by name first.
pub fn spawn(spec: &LaunchSpec) -> Result<Child> {
    let logs = spec.game_dir.join("logs");
    std::fs::create_dir_all(&logs).with_context(|| format!("cannot create {}", logs.display()))?;
    install::assets::place_legacy(&spec.installed.assets_dir, &spec.installed.asset_index, &spec.game_dir)?;
    let log_path = logs.join("manycraft-launch.log");
    let log = File::create(&log_path).with_context(|| format!("cannot write {}", log_path.display()))?;
    let log_err = log.try_clone().context("cannot write the launch log")?;
    let mut command = spec.command()?;
    command.stdin(Stdio::null()).stdout(log).stderr(log_err);
    command.spawn().with_context(|| format!("cannot start {}", spec.installed.java.display()))
}

/// `manycraft launch`: installs and starts a version from the terminal.
pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

struct Options {
    version: Option<String>,
    name: String,
    account: Option<String>,
    server: Option<String>,
}

fn parse(args: &[String]) -> Option<Options> {
    let mut options = Options { version: None, name: "Dev".to_owned(), account: None, server: None };
    let mut named = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?.clone();
        match flag.as_str() {
            "--version" => options.version = Some(value),
            "--name" if options.account.is_none() && !named => {
                options.name = value;
                named = true;
            }
            "--account" if !named => options.account = Some(value),
            "--server" => options.server = Some(value),
            _ => return None,
        }
    }
    Some(options)
}

fn run(args: &[String]) -> Result<i32> {
    let Some(options) = parse(args) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let (session, offline_name, id) = match &options.account {
        None => {
            account::validate_offline_name(&options.name)?;
            let id = account::offline_uuid(&options.name);
            (None, options.name, id)
        }
        Some(who) => {
            let accounts = account::load()?;
            let found = auth::find_account(&accounts, who)?;
            match found.kind {
                AccountKind::Offline => {
                    account::validate_offline_name(&found.name)?;
                    (None, found.name.clone(), found.id)
                }
                AccountKind::Microsoft => (Some(Session::from(auth::session_for(found)?)), found.name.clone(), found.id),
            }
        }
    };
    let version = match options.version {
        Some(v) => v,
        None => install::latest_release()?,
    };
    let report = |p: install::Progress| {
        let step = (p.total / 10).max(1);
        if p.total > 0 && (p.done == 0 || p.done == p.total || (p.total >= 100 && p.done.is_multiple_of(step))) {
            println!("{}: {}/{}", p.stage.label(), p.done, p.total);
        }
    };
    let installed = install::install(&version, &Loader::default(), &report)?;
    println!("Minecraft {} with Fabric {}: downloaded {} files", installed.game_version, installed.loader_version, installed.downloaded);
    let game_dir = paths::instances_dir().join(format!("dev-{}", id.simple()));
    let spec = LaunchSpec {
        installed,
        game_dir: game_dir.clone(),
        session,
        offline_name,
        memory_mb: 4096,
        server: options.server,
        world: None,
        extra_jvm: Vec::new(),
        extra_game: Vec::new(),
        env: SecretEnv::default(),
    };
    let mut child = spawn(&spec)?;
    println!("started {} in {} (log: logs/manycraft-launch.log)", spec.installed.id, game_dir.display());
    let status = child.wait().context("cannot wait for the game")?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::core::install::meta::{Argument, Arguments as DeclaredArguments, VersionJson, merge};

    const TOKEN: &str = "eyJ-secret-token-123";

    fn profile() -> VersionJson {
        let vanilla: VersionJson = serde_json::from_str(include_str!("testdata/install/vanilla.json")).unwrap();
        let fabric: VersionJson = serde_json::from_str(include_str!("testdata/install/fabric.json")).unwrap();
        merge(vanilla, fabric)
    }

    fn installed(log_config: Option<&str>) -> Installed {
        let profile = profile();
        Installed {
            game_version: "26.3".into(),
            loader_version: "0.20.1".into(),
            id: profile.id.clone(),
            version_name: profile.id.clone(),
            libraries_dir: PathBuf::from("libs"),
            profile,
            java: PathBuf::from("java"),
            classpath: vec![PathBuf::from("libs/a.jar"), PathBuf::from("libs/b.jar"), PathBuf::from("versions/26.3/26.3.jar")],
            natives_dir: PathBuf::from("versions/f/natives"),
            assets_dir: PathBuf::from("data/assets"),
            asset_index: "34".into(),
            log_config: log_config.map(PathBuf::from),
            downloaded: 0,
        }
    }

    fn spec(session: Option<Session>, server: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            installed: installed(Some("data/assets/log_configs/client.xml")),
            game_dir: PathBuf::from("inst"),
            session,
            offline_name: "Dev".into(),
            memory_mb: 3072,
            server: server.map(str::to_owned),
            world: None,
            extra_jvm: vec!["-Dextra=1".into()],
            extra_game: vec!["--extra".into()],
            env: SecretEnv(vec![("MANYCRAFT_TOKEN".into(), TOKEN.into())]),
        }
    }

    #[test]
    fn env_and_extra_game_reach_the_command_and_debug_hides_env_values() {
        let spec = spec(None, None);
        let command = spec.command().unwrap();
        assert!(command.get_args().any(|a| a == "--extra"));
        assert!(command.get_envs().any(|(k, v)| k == "MANYCRAFT_TOKEN" && v.is_some_and(|v| v == TOKEN)));
        let debug = format!("{spec:?}");
        assert!(debug.contains("MANYCRAFT_TOKEN") && !debug.contains(TOKEN));
    }

    fn session() -> Session {
        Session { name: "Alt".into(), uuid: "11111111-2222-3333-4444-555555555555".into(), access_token: TOKEN.into(), xuid: "999".into() }
    }

    fn after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let i = args.iter().position(|a| a == flag).unwrap();
        &args[i + 1]
    }

    #[test]
    fn jvm_arguments_precede_the_main_class_and_game_arguments_follow() {
        let args = spec(Some(session()), None).arguments().unwrap();
        let args = args.as_slice();
        let main = args.iter().position(|a| a == "net.fabricmc.loader.impl.launch.knot.KnotClient").unwrap();
        assert!(args.iter().position(|a| a == "-cp").unwrap() < main);
        assert!(args.iter().position(|a| a == "-Xmx3072M").unwrap() < main);
        assert!(args.iter().position(|a| a == "-Dextra=1").unwrap() < main);
        assert!(args.iter().position(|a| a == "-DFabricMarker=1").unwrap() < main);
        assert!(args.iter().position(|a| a == "--username").unwrap() > main);
        assert_eq!(args.last().map(String::as_str), Some("release"));
    }

    #[test]
    fn crash_files_land_in_the_instance_logs_folder() {
        let args = spec(None, None).arguments().unwrap();
        let file = args.as_slice().iter().find_map(|a| a.strip_prefix("-XX:ErrorFile=")).unwrap();
        assert_eq!(Path::new(file), Path::new("inst").join("logs").join("hs_err_pid%p.log"));
        let main = args.as_slice().iter().position(|a| a == "net.fabricmc.loader.impl.launch.knot.KnotClient").unwrap();
        assert!(args.as_slice().iter().position(|a| a.starts_with("-XX:ErrorFile=")).unwrap() < main);
    }

    #[test]
    fn extra_jvm_arguments_come_last_so_they_can_override() {
        let args = spec(None, None).arguments().unwrap();
        let args = args.as_slice();
        assert!(args.iter().position(|a| a == "-Dextra=1").unwrap() > args.iter().position(|a| a == "-Xmx3072M").unwrap());
        assert!(args.iter().position(|a| a == "-Xmx3072M").unwrap() > args.iter().position(|a| a == "-cp").unwrap());
    }

    #[test]
    fn placeholders_are_all_replaced() {
        let args = spec(Some(session()), Some("example.org:25565")).arguments().unwrap();
        let args = args.as_slice();
        assert!(args.iter().all(|a| !a.contains("${")), "{args:?}");
        assert_eq!(after(args, "--username"), "Alt");
        assert_eq!(after(args, "--uuid"), "11111111-2222-3333-4444-555555555555");
        assert_eq!(after(args, "--accessToken"), TOKEN);
        assert_eq!(after(args, "--xuid"), "999");
        assert_eq!(after(args, "--version"), "fabric-loader-0.20.1-26.3");
        assert_eq!(after(args, "--assetsDir"), "data/assets");
        assert_eq!(after(args, "--assetIndex"), "34");
        assert_eq!(after(args, "--gameDir"), "inst");
        assert!(args.contains(&"-Djava.library.path=versions/f/natives/java".to_owned()));
        assert!(args.contains(&"-Dminecraft.launcher.brand=Manycraft".to_owned()));
        assert!(args.contains(&"-Dlog4j.configurationFile=data/assets/log_configs/client.xml".to_owned()));
    }

    #[test]
    fn classpath_uses_the_os_separator() {
        let args = spec(None, None).arguments().unwrap();
        let classpath = after(args.as_slice(), "-cp");
        let sep = if cfg!(windows) { ';' } else { ':' };
        assert_eq!(classpath.split(sep).count(), 3);
        assert!(classpath.starts_with("libs/a.jar"));
        assert!(classpath.ends_with("26.3.jar"));
    }

    #[test]
    fn quick_play_only_with_a_server() {
        let without = spec(None, None).arguments().unwrap();
        assert!(!without.as_slice().contains(&"--quickPlayMultiplayer".to_owned()));
        let with = spec(None, Some("example.org:25565")).arguments().unwrap();
        assert_eq!(after(with.as_slice(), "--quickPlayMultiplayer"), "example.org:25565");
        assert!(after(with.as_slice(), "--quickPlayPath").ends_with("manycraft.json"));
        assert!(!without.as_slice().contains(&"--quickPlayPath".to_owned()));
        assert!(!with.as_slice().contains(&"--quickPlaySingleplayer".to_owned()));
    }

    #[test]
    fn a_world_opens_by_quick_play() {
        let mut world = spec(None, None);
        world.world = Some("New World".into());
        let args = world.arguments().unwrap();
        assert_eq!(after(args.as_slice(), "--quickPlaySingleplayer"), "New World");
        assert!(!args.as_slice().contains(&"--quickPlayMultiplayer".to_owned()));
    }

    #[test]
    fn rule_gated_flags_stay_out_by_default() {
        let args = spec(None, None).arguments().unwrap();
        assert!(!args.as_slice().contains(&"--demo".to_owned()));
        assert!(!args.as_slice().contains(&"--width".to_owned()));
        assert!(!args.as_slice().contains(&"-Xss1M".to_owned()));
    }

    #[test]
    fn offline_session_uses_the_name_and_a_stable_uuid() {
        let args = spec(None, None).arguments().unwrap();
        assert_eq!(after(args.as_slice(), "--username"), "Dev");
        assert_eq!(after(args.as_slice(), "--uuid"), crate::core::account::offline_uuid("Dev").to_string());
        assert_ne!(crate::core::account::offline_uuid("Dev"), crate::core::account::offline_uuid("Other"));
            }



    #[test]
    fn token_is_absent_from_debug_output() {
        let spec = spec(Some(session()), Some("example.org"));
        let args = spec.arguments().unwrap();
        assert!(args.as_slice().iter().any(|a| a == TOKEN));
        let shown = format!("{args:?} {spec:?} {:?}", spec.session);
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(shown.contains(REDACTED));
    }

    #[test]
    fn forge_arguments_find_the_libraries_and_skip_the_game_jar_by_name() {
        let vanilla: VersionJson = serde_json::from_str(include_str!("testdata/install/vanilla.json")).unwrap();
        let forge: VersionJson = serde_json::from_str(include_str!("testdata/install/forge-1.20.1-version.json")).unwrap();
        let mut spec = spec(None, None);
        spec.installed.profile = merge(vanilla, forge);
        spec.installed.version_name = "26.3".into();
        let args = spec.arguments().unwrap();
        let args = args.as_slice();
        let sep = if cfg!(windows) { ';' } else { ':' };
        assert!(after(args, "-p").split(sep).all(|p| p.starts_with("libs/")), "{}", after(args, "-p"));
        assert!(args.iter().any(|a| a.starts_with("-DignoreList=") && a.ends_with(",26.3.jar")));
        assert!(args.contains(&"-DlibraryDirectory=libs".to_owned()));
        assert_eq!(after(args, "--launchTarget"), "forgeclient");
        assert!(args.contains(&"cpw.mods.bootstraplauncher.BootstrapLauncher".to_owned()));
    }

    #[test]
    fn legacy_versions_get_default_jvm_arguments_and_split_game_arguments() {
        let mut spec = spec(None, None);
        spec.installed.profile.arguments = Some(DeclaredArguments { jvm: vec![Argument::Plain("-DFabricMarker=1".into())], game: Vec::new() });
        spec.installed.profile.minecraft_arguments = Some("--username ${auth_player_name} --version ${version_name}".into());
        let args = spec.arguments().unwrap();
        let args = args.as_slice();
        assert_eq!(after(args, "-cp").split(if cfg!(windows) { ';' } else { ':' }).count(), 3);
        assert_eq!(after(args, "--username"), "Dev");
        assert!(args.contains(&"-Djava.library.path=versions/f/natives".to_owned()));
    }

    fn legacy(json: &str) -> LaunchSpec {
        let mut spec = spec(None, None);
        spec.installed.profile = serde_json::from_str(json).unwrap();
        spec.installed.id = spec.installed.profile.id.clone();
        spec.installed.version_name = spec.installed.profile.id.clone();
        spec.installed.asset_index = spec.installed.profile.asset_index.as_ref().unwrap().id.clone();
        spec.installed.log_config = None;
        spec.extra_jvm.clear();
        spec.extra_game.clear();
        spec
    }

    #[test]
    fn a_1_8_9_game_gets_its_legacy_argument_string_and_launcher_defaults() {
        let spec = legacy(include_str!("testdata/install/legacy-1.8.9.json"));
        let args = spec.arguments().unwrap();
        let args = args.as_slice();
        assert!(args.iter().all(|a| !a.contains("${")), "{args:?}");
        let main = args.iter().position(|a| a == "net.minecraft.client.main.Main").unwrap();
        for jvm in ["-cp", "-Djava.library.path=versions/f/natives", "-Dminecraft.launcher.brand=Manycraft", "-Xmx3072M"] {
            assert!(args.iter().position(|a| a == jvm).unwrap() < main, "{jvm}");
        }
        assert_eq!(args.iter().any(|a| a.starts_with("-XX:HeapDumpPath=")), cfg!(windows));
        let game: Vec<&str> = args[main + 1..].iter().map(String::as_str).collect();
        let name_uuid = crate::core::account::offline_uuid("Dev").to_string();
        assert_eq!(
            game,
            ["--username", "Dev", "--version", "1.8.9", "--gameDir", "inst", "--assetsDir", "data/assets", "--assetIndex", "1.8", "--uuid", &name_uuid, "--accessToken", "0", "--userProperties", "{}", "--userType", "legacy"]
        );
    }

    #[test]
    fn a_pre_1_6_game_gets_the_session_and_its_resources_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = legacy(include_str!("testdata/install/legacy-1.5.2.json"));
        spec.installed.assets_dir = dir.path().join("assets");
        std::fs::create_dir_all(spec.installed.assets_dir.join("indexes")).unwrap();
        std::fs::write(spec.installed.assets_dir.join("indexes").join("pre-1.6.json"), r#"{"map_to_resources":true,"objects":{}}"#).unwrap();
        let args = spec.arguments().unwrap();
        let args = args.as_slice();
        let main = args.iter().position(|a| a == "net.minecraft.launchwrapper.Launch").unwrap();
        assert_eq!(&args[main + 1..main + 3], ["Dev", "-"]);
        assert_eq!(after(args, "--assetsDir"), Path::new("inst").join("resources").to_string_lossy());
        let mut online = spec.clone();
        online.session = Some(session());
        let args = online.arguments().unwrap();
        assert_eq!(args.as_slice()[main + 2], format!("token:{TOKEN}:11111111222233334444555555555555"));
        assert!(!format!("{args:?}").contains(TOKEN));
    }

    #[test]
    fn missing_main_class_is_an_error() {
        let mut spec = spec(None, None);
        spec.installed.profile.main_class = None;
        assert!(spec.arguments().is_err());
    }

    #[test]
    fn flags_are_parsed() {
        let o = parse(&["--version".into(), "26.3".into(), "--name".into(), "X".into(), "--server".into(), "h:1".into()]).unwrap();
        assert_eq!((o.version.as_deref(), o.name.as_str(), o.server.as_deref()), (Some("26.3"), "X", Some("h:1")));
        let defaults = parse(&[]).unwrap();
        assert_eq!((defaults.version, defaults.name.as_str(), defaults.server), (None, "Dev", None));
        assert!(parse(&["--bogus".into(), "x".into()]).is_none());
        assert!(parse(&["--version".into()]).is_none());
    }

    #[test]
    fn spawn_logs_to_the_game_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec(None, None);
        spec.game_dir = dir.path().join("inst");
        spec.installed.java = PathBuf::from("manycraft-no-such-java");
        let err = format!("{:#}", spawn(&spec).unwrap_err());
        assert!(err.contains("manycraft-no-such-java"));
        assert!(!err.contains("-cp"));
        assert!(Path::new(&spec.game_dir).join("logs").join("manycraft-launch.log").is_file());
    }
}
