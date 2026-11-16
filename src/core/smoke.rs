//! `manycraft smoke`: starts one hidden offline game with the companion and checks that it connects, sends a preview
//! frame, keeps to the background frame cap and found every hook. CI runs it for each supported release.

use std::fs;
use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::account::Account;
use super::fleet::{self, Fleet, Target};
use super::instance::{self, Instance, Loader};
use super::ipc::{self, FromMod, Level, ToMod};

const USAGE: &str = "usage: manycraft smoke --version <version> [--loader vanilla|fabric|quilt|forge|neoforge] [--timeout <seconds>]";
const NAME: &str = "Smoke";
const FLAG: &str = "-Dmanycraft.smoke=true";
const TEMPLATE: [(&str, &str); 2] = [("soundCategory_master", "0.0"), ("onboardAccessibility", "false")];
const CAP: u32 = 3;
const SAMPLES: usize = 3;
const POLL: Duration = Duration::from_millis(250);
const DEFAULT_TIMEOUT: u64 = 900;
const FPS_LINE: &str = "manycraft smoke fps ";
const BACKEND_FAILED: &str = "Failed to create backend ";
const BACKEND_USED: &str = "Using graphics backend ";
const BACKENDS: [&str; 2] = ["OpenGL", "Vulkan"];
const TAIL: usize = 60;
const FAULTS: [&str; 7] = [
    "hooks not found",
    "failed, left to the game",
    "stopping the game",
    "the companion leaves it alone",
    "previews stay off",
    "stay unclamped",
    "options fields are not where",
];

/// Runs `manycraft smoke`, returning the exit code: 0 when every check passed.
pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("smoke failed: {e:#}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(flags) = fleet::flags(&args, &["--version", "--loader", "--timeout"]) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let Some(version) = flags.get("--version").copied() else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let loader = flags.get("--loader").map_or(Ok(Loader::Vanilla), |l| fleet::parse_loader(l))?;
    let timeout = flags.get("--timeout").map_or(Ok(DEFAULT_TIMEOUT), |t| t.parse().context("--timeout is a number of seconds"))?;
    let instance = smoke_instance(version, loader)?;
    quiet(&instance.options_template())?;
    let account = Account::offline(NAME)?;
    let game_dir = instance.game_dir(account.id);
    let target = Target::solo(&instance);
    let (fleet, events) = Fleet::new()?;
    let report = |p: super::install::Progress| {
        if p.total > 0 && p.done == p.total {
            println!("{}: {}/{}", p.stage.label(), p.done, p.total);
        }
    };
    let started = fleet.start_batch(&target, std::slice::from_ref(&account), None, &report)?;
    let key = match started.into_iter().next().map(|(_, outcome)| outcome) {
        Some(Ok(key)) => key,
        Some(Err(e)) => bail!("the game did not start: {e:#}"),
        None => bail!("the game did not start"),
    };
    let outcome = watch(&fleet, &events, &key, &game_dir.join("logs").join("manycraft-launch.log"), Duration::from_secs(timeout));
    let _ = fleet.send(&key, &ToMod::Quit);
    fleet.stop_all(std::slice::from_ref(&key));
    let log = read_log(&game_dir.join("logs").join("manycraft-launch.log"));
    for line in log.lines().filter(|l| l.contains("[manycraft-agent]")) {
        println!("{line}");
    }
    if let Some(backend) = log.lines().find_map(|l| message(l, BACKEND_USED)) {
        println!("{backend}");
    }
    let mut problems = match outcome {
        Ok(fps) => {
            println!("handshake ok, preview ok, hidden fps {fps:?} (cap {CAP})");
            Vec::new()
        }
        Err(e) => vec![format!("{e:#}")],
    };
    problems.extend(faults(&log));
    if problems.is_empty() {
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

fn watch(fleet: &Fleet, events: &std::sync::mpsc::Receiver<ipc::Event>, key: &str, log: &Path, timeout: Duration) -> Result<Vec<u32>> {
    let deadline = Instant::now() + timeout;
    let mut connected = false;
    let mut previewed = false;
    let mut checked_from = 0;
    while Instant::now() < deadline {
        match events.recv_timeout(POLL) {
            Ok(ipc::Event::Connected { instance, mod_version, .. }) if instance == key => {
                println!("connected, companion {mod_version}");
                connected = true;
                fleet.send(key, &ToMod::Level(Level::Saving))?;
            }
            Ok(ipc::Event::Message { instance, message }) if instance == key => match message {
                FromMod::Preview { w, h, .. } if !previewed => {
                    println!("preview {w}x{h}");
                    previewed = true;
                    checked_from = fps_samples(&read_log(log)).len();
                }
                FromMod::Window { visible: true, .. } => bail!("the game window became visible, it should start hidden"),
                _ => {}
            },
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("the manager stopped listening"),
        }
        if !fleet.is_running(key) {
            bail!("the game exited (code {:?})", fleet.exit_code(key).flatten());
        }
        if !connected && let Some(why) = no_backend(&read_log(log)) {
            bail!("no graphics backend started, the game waits on its error dialog: {why}");
        }
        if previewed {
            let samples: Vec<(u32, bool)> = fps_samples(&read_log(log)).into_iter().skip(checked_from).collect();
            if samples.len() >= SAMPLES {
                return judge(&samples[samples.len() - SAMPLES..]);
            }
        }
    }
    bail!("timed out: connected {connected}, preview {previewed}")
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
        let log = "[12:00:01] [Render thread/INFO]: manycraft smoke fps 3 hidden true\nnoise\n[x]: manycraft smoke fps 60 hidden false\n[x]: manycraft smoke fps x hidden true\n<log4j:Message><![CDATA[manycraft smoke fps 2 hidden true]]></log4j:Message>\n";
        assert_eq!(fps_samples(log), vec![(3, true), (60, false), (2, true)]);
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
        assert_eq!(fs::read_to_string(&path).unwrap(), "soundCategory_master:0.0\nonboardAccessibility:false\n");
        fs::write(&path, "fov:0.5\nsoundCategory_master:1.0\nonboardAccessibility:true\n").unwrap();
        quiet(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fov:0.5\nsoundCategory_master:0.0\nonboardAccessibility:false\n");
    }
}
