//! The line an agent drives a running instance through: a local port and token in `drive.json`, one JSON request and
//! one JSON answer per line, and a watchdog that quits an instance nobody drives any more.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::drive::{self, alive};

/// One request for the UI thread, with where its answer goes and until when anyone waits for it.
pub struct Call {
    pub body: Value,
    pub reply: mpsc::Sender<Value>,
    pub deadline: Instant,
}

/// Opens the port and writes `drive.json` when `MANYCRAFT_DRIVE` is set; the UI thread answers the calls that arrive.
pub fn listen() -> Option<async_channel::Receiver<Call>> {
    let dir = drive::dir()?;
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("drive: no local port: {e}");
            return None;
        }
    };
    let port = listener.local_addr().map_or(0, |a| a.port());
    let token = uuid::Uuid::new_v4().simple().to_string();
    let info = json!({ "port": port, "token": token, "pid": std::process::id() });
    let tmp = dir.join("drive.json.tmp");
    if std::fs::write(&tmp, info.to_string()).and_then(|()| std::fs::rename(&tmp, dir.join("drive.json"))).is_err() {
        eprintln!("drive: cannot write {}", dir.display());
        return None;
    }
    let (tx, rx) = async_channel::unbounded::<Call>();
    let last = Arc::new(Mutex::new(Instant::now()));
    {
        let (tx, last) = (tx.clone(), last.clone());
        let _ = std::thread::Builder::new().name("drive".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let (tx, last, token) = (tx.clone(), last.clone(), token.clone());
                let _ = std::thread::Builder::new().name("drive-client".into()).spawn(move || serve(stream, &token, &tx, &last));
            }
        });
    }
    let _ = std::thread::Builder::new().name("drive-watch".into()).spawn(move || watch(&tx, &last));
    Some(rx)
}

fn serve(stream: TcpStream, token: &str, tx: &async_channel::Sender<Call>, last: &Mutex<Instant>) {
    let Ok(mut writer) = stream.try_clone() else { return };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let out = match serde_json::from_str::<Value>(&line) {
            Err(e) => json!({ "ok": false, "error": format!("bad request: {e}") }),
            Ok(body) if body["token"].as_str() != Some(token) => json!({ "ok": false, "error": "wrong token" }),
            Ok(body) => {
                *last.lock().unwrap() = Instant::now();
                if body["cmd"] == "ping" {
                    let ui = ask(tx, json!({ "cmd": "ui" }), Duration::from_secs(2)).is_some();
                    json!({ "ok": true, "pid": std::process::id(), "ui": ui })
                } else {
                    let wait = Duration::from_secs_f64(body["timeout"].as_f64().unwrap_or(30.).clamp(0., 3600.) + 5.);
                    ask(tx, body, wait).unwrap_or_else(|| json!({ "ok": false, "hung": true, "error": format!("the UI thread did not answer within {} s", wait.as_secs()) }))
                }
            }
        };
        if writeln!(writer, "{out}").and_then(|()| writer.flush()).is_err() {
            break;
        }
    }
}

fn ask(tx: &async_channel::Sender<Call>, body: Value, wait: Duration) -> Option<Value> {
    let (reply, answer) = mpsc::channel();
    let until = Instant::now() + wait;
    tx.send_blocking(Call { body, reply, deadline: until }).ok()?;
    let mut closed = false;
    loop {
        match answer.recv_timeout(Duration::from_millis(100)) {
            Ok(mut out) => {
                if closed {
                    out["menu"] = json!("a native menu opened and was closed; native menus cannot be driven");
                }
                return Some(out);
            }
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < until => {
                if native_menu::close() {
                    closed = true;
                    drive::note("native menu closed".into());
                }
            }
            Err(_) => return None,
        }
    }
}

#[cfg(target_os = "windows")]
mod native_menu {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CANCELMODE};

    unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == std::process::id() && unsafe { IsWindowVisible(hwnd) } != 0 {
            let mut class = [0u16; 16];
            let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
            let menu = String::from_utf16_lossy(&class[..n.max(0) as usize]) == "#32768";
            unsafe { (*(list as *mut Vec<(HWND, bool)>)).push((hwnd, menu)) };
        }
        TRUE
    }

    pub fn close() -> bool {
        let mut list: Vec<(HWND, bool)> = Vec::new();
        unsafe { EnumWindows(Some(collect), &mut list as *mut _ as LPARAM) };
        if !list.iter().any(|(_, menu)| *menu) {
            return false;
        }
        for (hwnd, _) in list {
            let mut result = 0;
            unsafe { SendMessageTimeoutW(hwnd, WM_CANCELMODE, 0, 0, SMTO_ABORTIFHUNG, 500, &mut result) };
        }
        true
    }
}

#[cfg(not(target_os = "windows"))]
mod native_menu {
    pub fn close() -> bool {
        false
    }
}

fn watch(tx: &async_channel::Sender<Call>, last: &Mutex<Instant>) {
    let owner = std::env::var("MANYCRAFT_DRIVE_OWNER").ok().and_then(|p| p.parse::<u32>().ok()).filter(|&p| p != 0);
    let ttl = Duration::from_secs(std::env::var("MANYCRAFT_DRIVE_TTL").ok().and_then(|s| s.parse().ok()).unwrap_or(1800));
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let reason = if owner.is_some_and(|p| !alive(p)) {
            "the agent that started it is gone"
        } else if last.lock().unwrap().elapsed() > ttl {
            "nobody drove it for too long"
        } else {
            continue;
        };
        eprintln!("drive: quitting, {reason}");
        let _ = ask(tx, json!({ "cmd": "quit" }), Duration::from_secs(10));
        std::thread::sleep(Duration::from_secs(10));
        std::process::exit(3);
    }
}

