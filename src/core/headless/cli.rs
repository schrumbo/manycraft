//! `manycraft headless`: drives connection-only accounts from the console for live tests.

use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::{Connection, Spec, conduct, start};
use crate::core::account::offline_uuid;
use crate::core::batch::Packs;
use crate::core::ipc::{Event, FromMod, ToMod};

const USAGE: &str = "manycraft headless [--server host[:port]] [--name Bot] [--count 1] [--version 26.3] [--stagger-ms 250] [--packs ask|accept|decline]

Offline accounts only. Names are <name>1..<name>N when --count is above 1.
stdin: say <text> (all), to <name> <text>, reconnect, respawn, accept, decline, quit";

/// Runs the subcommand.
pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("headless: {e:#}\n\n{USAGE}");
            2
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let mut opts: BTreeMap<&str, &str> = BTreeMap::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if matches!(flag.as_str(), "help" | "--help" | "-h") {
            println!("{USAGE}");
            return Ok(());
        }
        let name = flag.strip_prefix("--").with_context(|| format!("unknown argument {flag}"))?;
        opts.insert(name, it.next().with_context(|| format!("{flag} needs a value"))?);
    }
    let get = |k: &str, default: &'static str| opts.get(k).copied().unwrap_or(default);
    let server = get("server", "127.0.0.1:25565").to_owned();
    let prefix = get("name", "Bot");
    let count: usize = get("count", "1").parse().context("--count")?;
    let stagger = Duration::from_millis(get("stagger-ms", "250").parse().context("--stagger-ms")?);
    let version = get("version", "26.3").to_owned();
    let packs = match get("packs", "ask") {
        "ask" => Packs::Ask,
        "accept" => Packs::Accept,
        "decline" => Packs::Decline,
        other => bail!("--packs {other}"),
    };
    let names: Vec<String> = if count == 1 { vec![prefix.to_owned()] } else { (1..=count).map(|i| format!("{prefix}{i}")).collect() };
    if let Some(long) = names.iter().find(|n| n.len() > 16) {
        bail!("{long} is longer than 16 characters");
    }
    let (tx, rx) = channel();
    let offers: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let seen = Arc::clone(&offers);
    std::thread::spawn(move || {
        for event in rx {
            print_event(&event, &seen);
        }
    });
    let mut connections: Vec<(String, Connection)> = Vec::new();
    for name in names {
        let spec = Spec { key: name.clone(), address: server.clone(), name: name.clone(), uuid: offline_uuid(&name), session: None, version: version.clone(), packs, proxy: None, conduct: Vec::new() };
        connections.push((name, start(spec, tx.clone())?));
        std::thread::sleep(stagger);
    }
    drop(tx);
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let (word, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        let send = |who: Option<&str>, message: &ToMod| {
            for (name, connection) in connections.iter().filter(|(n, _)| who.is_none_or(|w| w == n)) {
                if let Err(e) = connection.send(message) {
                    println!("[{name}] not sent: {e:#}");
                }
            }
        };
        match word {
            "say" => send(None, &ToMod::ChatSend { text: rest.to_owned() }),
            "to" => {
                let (who, text) = rest.split_once(' ').unwrap_or((rest, ""));
                send(Some(who), &ToMod::ChatSend { text: text.to_owned() });
            }
            "reconnect" => send(None, &ToMod::Reconnect),
            "respawn" => send(None, &ToMod::Respawn),
            "accept" | "decline" => {
                let held: Vec<(String, String)> = offers.lock().unwrap_or_else(PoisonError::into_inner).drain().collect();
                for (key, id) in held {
                    send(Some(&key), &ToMod::Answer { id, accept: word == "accept" });
                }
            }
            "quit" => break,
            "" => {}
            other => println!("unknown command {other}"),
        }
    }
    for (_, connection) in &connections {
        let _ = connection.send(&ToMod::Quit);
    }
    for _ in 0..50 {
        if connections.iter().all(|(_, c)| !c.is_running()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn print_event(event: &Event, offers: &Mutex<HashMap<String, String>>) {
    match event {
        Event::Connected { instance, mod_version, .. } => println!("[{instance}] started ({mod_version})"),
        Event::Disconnected { instance } => println!("[{instance}] ended"),
        Event::Message { instance, message: FromMod::State { kind, server, reason, cause, offer } } => {
            if let Some(offer) = offer {
                offers.lock().unwrap_or_else(PoisonError::into_inner).insert(instance.clone(), offer.id.clone());
            }
            let offered = offer.as_ref().map(|o| match conduct(o) {
                Some(hash) => format!("code of conduct {hash}: {}", o.text.clone().unwrap_or_default()),
                None => format!("pack {} required={} {}", o.id, o.required, o.text.clone().unwrap_or_default()),
            });
            let detail = [server.clone(), reason.clone(), cause.map(|c| format!("{c:?}")), offered];
            println!("[{instance}] {kind:?} {}", detail.into_iter().flatten().collect::<Vec<_>>().join(" | "));
        }
        Event::Message { instance, message: FromMod::Chat { kind, from, text, .. } } => println!("[{instance}] chat {kind:?} {}{text}", from.as_ref().map(|f| format!("<{f}> ")).unwrap_or_default()),
        Event::Message { instance, message } => println!("[{instance}] {message:?}"),
        other => println!("{other:?}"),
    }
}
