//! The sign-in window process: a small native window with a fresh, non-persistent web view on the Microsoft page.
//! It runs its own event loop, so the app's loop is untouched and the web view loads only while it is open.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use wry::{NewWindowResponse, PageLoadEvent, WebContext, WebViewBuilder};

use super::webauth::{self, Reply, Request};

const WIDTH: f64 = 480.0;
const HEIGHT: f64 = 680.0;
const MIN_WIDTH: f64 = 380.0;
const MIN_HEIGHT: f64 = 480.0;

enum Message {
    Done(Reply),
    Quit,
}

/// Runs the window for the request on stdin and reports on stdout; see [`webauth`].
pub fn cli(_args: &[String]) -> i32 {
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return 2;
    }
    let Ok(request) = serde_json::from_str::<Request>(&line) else { return 2 };
    match show(&request) {
        Ok(()) => 0,
        Err(e) => {
            say(&Reply::Failed(format!("{e:#}")));
            1
        }
    }
}

fn on_monitor(at: (i32, i32), origin: (i32, i32), size: (u32, u32)) -> bool {
    let inside = |p: i32, from: i32, len: u32| p >= from && i64::from(p) < i64::from(from) + i64::from(len);
    inside(at.0, origin.0, size.0) && inside(at.1, origin.1, size.1)
}

fn say(reply: &Reply) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", reply.line());
    let _ = out.flush();
}

fn show(request: &Request) -> Result<()> {
    let mut event_loop = EventLoopBuilder::<Message>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(&request.title)
        .with_inner_size(LogicalSize::new(WIDTH, HEIGHT))
        .with_min_inner_size(LogicalSize::new(MIN_WIDTH, MIN_HEIGHT))
        .with_visible(false)
        .build(&event_loop)
        .context("cannot open the sign-in window")?;
    set_icon(&window);
    edit_menu();
    let near = request.near.and_then(|at| window.available_monitors().find(|m| on_monitor(at, (m.position().x, m.position().y), (m.size().width, m.size().height))));
    if let Some(monitor) = near.or_else(|| window.current_monitor()).or_else(|| event_loop.primary_monitor()) {
        let (area, at, size) = (monitor.size(), monitor.position(), window.outer_size());
        let x = at.x + (i32::try_from(area.width).unwrap_or(0) - i32::try_from(size.width).unwrap_or(0)) / 2;
        let y = at.y + (i32::try_from(area.height).unwrap_or(0) - i32::try_from(size.height).unwrap_or(0)) / 2;
        window.set_outer_position(PhysicalPosition::new(x, y.max(at.y)));
    }
    let store = Store::new()?;
    let mut context = WebContext::new(store.path());
    let found = event_loop.create_proxy();
    let loaded = event_loop.create_proxy();
    let asked = event_loop.create_proxy();
    let quiet = request.quiet;
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_incognito(true)
        .with_url(&request.url)
        .with_initialization_script_for_main_only(passkeys(request.quiet), false)
        .with_initialization_script_for_main_only(browser_link(&request.browser_label), true)
        .with_navigation_handler(move |address| match webauth::redirect(&address) {
            Some(redirect) => {
                let _ = found.send_event(Message::Done(Reply::from_redirect(redirect)));
                false
            }
            None => true,
        })
        .with_on_page_load_handler(move |event, address| {
            if matches!(event, PageLoadEvent::Started)
                && let Some(redirect) = webauth::redirect(&address)
            {
                let _ = loaded.send_event(Message::Done(Reply::from_redirect(redirect)));
            }
        })
        .with_ipc_handler(move |message| match message.body().as_str() {
            "browser" => {
                let _ = asked.send_event(Message::Done(Reply::Browser));
            }
            held if quiet && held.starts_with("passkey ") => eprintln!("{held}"),
            _ => {}
        })
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny);
    let webview = builder.build(&window).context("cannot show the Microsoft page; the system web view may be missing")?;
    window.set_visible(true);
    window.set_focus();
    say(&Reply::Ready);
    let quit = event_loop.create_proxy();
    std::thread::spawn(move || {
        let mut sink = String::new();
        while std::io::stdin().lock().read_line(&mut sink).is_ok_and(|n| n > 0) {
            sink.clear();
        }
        let _ = quit.send_event(Message::Quit);
    });
    let mut sent = false;
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } | Event::UserEvent(Message::Quit) => *flow = ControlFlow::Exit,
            Event::UserEvent(Message::Done(reply)) => {
                if !sent {
                    say(&reply);
                    sent = true;
                }
                *flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
    drop(webview);
    drop(window);
    drop(context);
    drop(store);
    Ok(())
}

fn passkeys(quiet: bool) -> String {
    format!(
        r##"(function () {{
  var quiet = {quiet};
  var store = navigator.credentials;
  if (!store || !store.get) return;
  var get = store.get.bind(store), create = store.create.bind(store);
  function hold(options, kind) {{
    if (quiet && window.ipc) window.ipc.postMessage("passkey " + kind);
    return new Promise(function (resolve, reject) {{
      var signal = options && options.signal;
      if (signal) signal.addEventListener("abort", function () {{ reject(new DOMException("aborted", "AbortError")); }});
    }});
  }}
  store.get = function (options) {{
    if (options && options.publicKey && (quiet || options.mediation === "conditional")) return hold(options, options.mediation || "get");
    return get(options);
  }};
  store.create = function (options) {{
    if (quiet && options && options.publicKey) return hold(options, "create");
    return create(options);
  }};
}})();"##
    )
}

fn browser_link(label: &str) -> String {
    let label = serde_json::to_string(label).unwrap_or_else(|_| "\"\"".into());
    format!(
        r##"(function () {{
  var label = {label};
  function add() {{
    if (!document.body || document.getElementById("manycraft-browser")) return;
    var style = document.createElement("style");
    style.textContent = "#manycraft-browser{{position:fixed;right:12px;top:12px;z-index:2147483647;font:13px -apple-system,'Segoe UI',system-ui,sans-serif;color:#0067b8;background:rgba(255,255,255,.94);padding:5px 10px;border-radius:6px;text-decoration:none;box-shadow:0 1px 4px rgba(0,0,0,.18)}}#manycraft-browser:hover{{text-decoration:underline}}@media (prefers-color-scheme: dark){{#manycraft-browser{{color:#6cb8f6;background:rgba(32,32,32,.94)}}}}";
    document.head ? document.head.appendChild(style) : document.body.appendChild(style);
    var link = document.createElement("a");
    link.id = "manycraft-browser";
    link.href = "#";
    link.textContent = label;
    link.addEventListener("click", function (e) {{ e.preventDefault(); window.ipc.postMessage("browser"); }});
    document.body.appendChild(link);
  }}
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", add); else add();
}})();"##
    )
}

#[cfg(windows)]
fn set_icon(window: &tao::window::Window) {
    use tao::platform::windows::IconExtWindows;
    window.set_window_icon(tao::window::Icon::from_resource(1, None).ok());
}

#[cfg(not(windows))]
fn set_icon(_window: &tao::window::Window) {}

#[cfg(target_os = "macos")]
fn edit_menu() {
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let item = |title: &str, action: Option<objc2::runtime::Sel>, key: &str| unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), &NSString::from_str(title), action, &NSString::from_str(key)) };
    let bar = NSMenu::new(mtm);
    let app_menu = NSMenu::new(mtm);
    app_menu.addItem(&item("Quit", Some(sel!(performClose:)), "q"));
    let app_item = item("", None, "");
    app_item.setSubmenu(Some(&app_menu));
    bar.addItem(&app_item);
    let edit = NSMenu::new(mtm);
    for (title, action, key) in [("Undo", sel!(undo:), "z"), ("Redo", sel!(redo:), "Z"), ("Cut", sel!(cut:), "x"), ("Copy", sel!(copy:), "c"), ("Paste", sel!(paste:), "v"), ("Select All", sel!(selectAll:), "a")] {
        edit.addItem(&item(title, Some(action), key));
    }
    let edit_item = item("Edit", None, "");
    edit_item.setSubmenu(Some(&edit));
    bar.addItem(&edit_item);
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&bar));
}

#[cfg(not(target_os = "macos"))]
fn edit_menu() {}

/// The web view's data folder: a new temporary one on Windows, deleted afterwards; elsewhere the web view keeps nothing on disk.
struct Store(Option<tempfile::TempDir>);

impl Store {
    #[cfg(windows)]
    fn new() -> Result<Self> {
        Ok(Self(Some(tempfile::Builder::new().prefix(webauth::STORE_PREFIX).tempdir().context("cannot create a folder for the sign-in window")?)))
    }

    #[cfg(not(windows))]
    fn new() -> Result<Self> {
        Ok(Self(None))
    }

    fn path(&self) -> Option<std::path::PathBuf> {
        self.0.as_ref().map(|d| d.path().to_path_buf())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let Some(dir) = self.0.take() else { return };
        let path = dir.keep();
        for _ in 0..30 {
            if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_belongs_to_the_monitor_it_lies_on() {
        let (primary, left) = (((0, 0), (2560, 1440)), ((-1920, 139), (1920, 1080)));
        let on = |at, m: ((i32, i32), (u32, u32))| on_monitor(at, m.0, m.1);
        assert!(on((-960, 600), left) && !on((-960, 600), primary));
        assert!(on((0, 0), primary) && !on((0, 0), left));
        assert!(on((-1, 139), left) && !on((-1, 138), left) && !on((-1921, 500), left));
        assert!(on((2559, 1439), primary) && !on((2560, 1439), primary));
    }

    #[test]
    fn passkey_script_holds_only_what_it_should() {
        assert!(passkeys(false).contains("var quiet = false;"));
        assert!(passkeys(true).contains("var quiet = true;"));
        assert!(passkeys(false).contains("\"conditional\""));
        assert!(browser_link("Im \"Browser\"").contains(r#"var label = "Im \"Browser\"";"#));
    }

    #[cfg(windows)]
    #[test]
    fn the_store_is_gone_after_the_window() {
        let store = Store::new().unwrap();
        let path = store.path().unwrap();
        assert!(path.exists() && path.file_name().unwrap().to_string_lossy().starts_with(webauth::STORE_PREFIX));
        std::fs::write(path.join("Local State"), "x").unwrap();
        drop(store);
        assert!(!path.exists());
    }
}
