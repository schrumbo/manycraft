//! Builds the Java command line for an installed version and starts the game.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use super::install::meta::{Env, expand, substitute};
use super::install::{self, Installed};
use super::paths;

const LAUNCHER_NAME: &str = "Manycraft";
const CLASSPATH_SEPARATOR: &str = if cfg!(windows) { ";" } else { ":" };
const REDACTED: &str = "<redacted>";
const USAGE: &str = "usage: manycraft launch [--version <version>] [--name <name>] [--server <host:port>]";

/// A signed-in account's session. Its token is never printed.
#[derive(Clone)]
pub struct Session {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub xuid: String,
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
    pub extra_jvm: Vec<String>,
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
        if self.server.is_some() { vec!["has_quick_plays_support", "is_quick_play_multiplayer"] } else { Vec::new() }
    }

    fn variables(&self) -> HashMap<&'static str, String> {
        let installed = &self.installed;
        let (name, uuid, token, xuid, user_type) = match &self.session {
            Some(s) => (s.name.clone(), s.uuid.clone(), s.access_token.clone(), s.xuid.clone(), "msa"),
            None => (self.offline_name.clone(), crate::core::account::offline_uuid(&self.offline_name).to_string(), "0".to_owned(), "0".to_owned(), "legacy"),
        };
        let classpath = installed.classpath.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(CLASSPATH_SEPARATOR);
        let version_type = installed.profile.kind.clone().unwrap_or_else(|| "release".to_owned());
        HashMap::from([
            ("auth_player_name", name),
            ("auth_uuid", uuid),
            ("auth_access_token", token),
            ("auth_xuid", xuid),
            ("user_type", user_type.to_owned()),
            ("user_properties", "{}".to_owned()),
            ("clientid", String::new()),
            ("version_name", installed.id.clone()),
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
        let mut extra = vec![format!("-Xmx{}M", self.memory_mb)];
        extra.extend(self.extra_jvm.iter().cloned());
        list.splice(game_start..game_start, extra);
        let secret = self.session.as_ref().map(|s| s.access_token.clone()).filter(|t| !t.is_empty());
        Ok(Arguments { list, secret })
    }

    /// The Java process, run from the game folder. Its arguments contain the access token: never print it.
    fn command(&self) -> Result<Command> {
        let mut command = Command::new(&self.installed.java);
        command.args(self.arguments()?.as_slice()).current_dir(&self.game_dir);
        Ok(command)
    }
}

/// Starts the game with output going to `logs/manycraft-launch.log` in the game folder.
pub fn spawn(spec: &LaunchSpec) -> Result<Child> {
    let logs = spec.game_dir.join("logs");
    std::fs::create_dir_all(&logs).with_context(|| format!("cannot create {}", logs.display()))?;
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
    server: Option<String>,
}

fn parse(args: &[String]) -> Option<Options> {
    let mut options = Options { version: None, name: "Dev".to_owned(), server: None };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?.clone();
        match flag.as_str() {
            "--version" => options.version = Some(value),
            "--name" => options.name = value,
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
    let installed = install::install(&version, None, &report)?;
    println!("Minecraft {} with Fabric {}: downloaded {} files", installed.game_version, installed.loader_version, installed.downloaded);
    let game_dir = paths::instances_dir().join("dev");
    let spec = LaunchSpec {
        installed,
        game_dir: game_dir.clone(),
        session: None,
        offline_name: options.name,
        memory_mb: 4096,
        server: options.server,
        extra_jvm: Vec::new(),
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
            extra_jvm: vec!["-Dextra=1".into()],
        }
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
