//! The drive tool's side in the Freya window: the same line protocol as the GPUI shell, answered on the UI thread.
//! Input goes in as platform events of this window only, so nothing reaches the owner's mouse, keyboard or clipboard.

mod spots;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use freya::engine::prelude::{AlphaType as SkAlpha, ColorType, IPoint, ISize, ImageInfo};
use freya::prelude::*;
use freya_core::integration::{KeyboardEventName, MouseEventName, PlatformEvent, WheelEventName};
use freya_core::prelude::NavigationMode;
use freya_winit::plugins::{FreyaPlugin, PluginEvent, PluginHandle};
use serde_json::{Value, json};

pub use self::spots::use_spot;
use self::spots::Spot;
use super::commands::Command;
use super::kit::{after, scale};
use super::native;
use super::store::Store;
use crate::app::event::Event;
use crate::core::drive::{self, on};
use crate::core::drive_link::{self, Call};

const KEPT_FRAMES: usize = 200_000;

thread_local! {
    static HANDLE: RefCell<Option<PluginHandle>> = const { RefCell::new(None) };
    static LAST_FRAME: Cell<Option<Instant>> = const { Cell::new(None) };
    static FRAME_START: Cell<Option<Instant>> = const { Cell::new(None) };
    static FRAMES: RefCell<(u64, Vec<f32>)> = const { RefCell::new((0, Vec::new())) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
    static POINTER: Cell<(f32, f32)> = const { Cell::new((0., 0.)) };
}

/// Notes every frame the window presents, and keeps the handle that feeds it input.
pub struct Watch;

impl FreyaPlugin for Watch {
    fn plugin_id(&self) -> &'static str {
        "manycraft-drive"
    }

    fn on_event(&mut self, event: &mut PluginEvent, handle: PluginHandle) {
        if !on() {
            return;
        }
        match event {
            PluginEvent::WindowCreated { .. } => {
                HANDLE.with(|h| *h.borrow_mut() = Some(handle));
                if cfg!(target_os = "windows") && drive::away() {
                    native::send_away();
                }
            }
            PluginEvent::StartedUpdatingTree { .. } => FRAME_START.with(|s| s.set(Some(Instant::now()))),
            PluginEvent::AfterPresenting { .. } => {
                let now = Instant::now();
                LAST_FRAME.with(|l| l.set(Some(now)));
                COUNT.with(|c| c.set(c.get() + 1));
                if let Some(start) = FRAME_START.with(Cell::take) {
                    FRAMES.with_borrow_mut(|(first, times)| {
                        if times.len() == KEPT_FRAMES {
                            times.drain(..KEPT_FRAMES / 2);
                            *first += (KEPT_FRAMES / 2) as u64;
                        }
                        times.push((now - start).as_secs_f32() * 1000.);
                    });
                }
            }
            _ => {}
        }
    }
}

/// Opens the drive channel when an agent drives this instance, and answers its calls on the UI thread.
pub fn start(store: Store) {
    if !on() {
        return;
    }
    spots::enable();
    let Some(calls) = drive_link::listen() else { return };
    spawn(async move {
        while let Ok(call) = calls.recv().await {
            if Instant::now() > call.deadline {
                continue;
            }
            spawn_in_window(answer(store, call));
        }
    });
}

async fn answer(store: Store, call: Call) {
    let out = match run(store, &call.body).await {
        Ok(Value::Object(mut map)) => {
            map.insert("ok".into(), json!(true));
            Value::Object(map)
        }
        Ok(other) => json!({ "ok": true, "value": other }),
        Err(e) => json!({ "ok": false, "error": e }),
    };
    let _ = call.reply.send(out);
}

async fn run(store: Store, b: &Value) -> Result<Value, String> {
    let cmd = b["cmd"].as_str().unwrap_or_default();
    match cmd {
        "ui" => Ok(json!({})),
        "quit" => {
            Platform::get().exit();
            Ok(json!({}))
        }
        "close" => {
            store.dispatch(Event::CloseWindow);
            Ok(json!({}))
        }
        "windows" => Ok(json!({ "windows": [window()] })),
        "events" => Ok(json!({ "events": drive::take_events() })),
        "dialog" => {
            let paths: Option<Vec<PathBuf>> = b["paths"].as_array().map(|p| p.iter().filter_map(|p| p.as_str()).map(PathBuf::from).collect());
            drive::queue_answer(paths.filter(|p| !p.is_empty()));
            Ok(json!({}))
        }
        "state" => Ok(state(store)),
        "actions" => Ok(json!({ "actions": Command::ALL.iter().map(|c| c.name()).collect::<Vec<_>>() })),
        "frames" => {
            let since = b["since"].as_u64().unwrap_or(u64::MAX);
            Ok(FRAMES.with_borrow(|(first, times)| {
                let next = first + times.len() as u64;
                let from = since.saturating_sub(*first).min(times.len() as u64) as usize;
                json!({ "next": next, "frames": times[from..].iter().map(|ms| json!([1, ms])).collect::<Vec<_>>() })
            }))
        }
        "tree" => {
            settle().await;
            let target = b["target"].as_str();
            let spots: Vec<Value> = spots::all().iter().filter(|s| target.is_none_or(|t| matches(t, s))).map(spot_json).collect();
            Ok(json!({ "spots": spots }))
        }
        "find" => {
            settle().await;
            Ok(spot_json(&find(b, "")?))
        }
        "wait" => wait(b).await,
        "click" | "move" | "down" | "up" | "wheel" => pointer(cmd, b).await,
        "key" => {
            let names: Vec<String> = b["keys"].as_array().map(|k| k.iter().filter_map(|k| k.as_str()).map(str::to_owned).collect()).unwrap_or_default();
            let mut keys = Vec::new();
            for name in &names {
                let parsed = parse_key(name).ok_or_else(|| format!("{name}: unknown key"))?;
                if !b["clipboard"].as_bool().unwrap_or(false) && parsed.1.intersects(Modifiers::CONTROL | Modifiers::META) && matches!(parsed.2, Code::KeyC | Code::KeyV | Code::KeyX) {
                    return Err(format!("{name} would use the owner's clipboard; pass --clipboard if that is intended"));
                }
                keys.push(parsed);
            }
            for (key, modifiers, code) in keys {
                press(key, code, modifiers);
                settle().await;
            }
            Ok(json!({}))
        }
        "type" => {
            for c in b["text"].as_str().unwrap_or_default().chars() {
                let (key, code) = match c {
                    '\n' => (Key::Named(NamedKey::Enter), Code::Enter),
                    '\t' => (Key::Named(NamedKey::Tab), Code::Tab),
                    c => (Key::Character(c.to_string()), Code::Unidentified),
                };
                let modifiers = if c.is_uppercase() { Modifiers::SHIFT } else { Modifiers::empty() };
                press(key, code, modifiers);
            }
            settle().await;
            Ok(json!({}))
        }
        "action" => {
            let name = b["name"].as_str().unwrap_or_default();
            let command = Command::named(name).ok_or_else(|| format!("no action {name}"))?;
            command.run(store);
            settle().await;
            Ok(json!({}))
        }
        "resize" => {
            let (w, h) = (b["w"].as_f64().ok_or("w missing")?, b["h"].as_f64().ok_or("h missing")?);
            let s = f64::from(scale());
            Platform::get().with_window(Platform::window_id(), move |window| {
                crate::view::native::set_inner(window, (w * s).round() as u32, (h * s).round() as u32);
            });
            settle().await;
            Ok(json!({}))
        }
        "capture" => {
            settle().await;
            capture(PathBuf::from(b["path"].as_str().ok_or("path missing")?)).await
        }
        "theme" => {
            let mut theme = use_try_consume_theme().ok_or("no theme")?;
            let dark = match b["name"].as_str().unwrap_or("system") {
                "dark" => true,
                "light" => false,
                _ => *Platform::get().current_window().preferred_theme.peek() == PreferredTheme::Dark,
            };
            let library = crate::core::themes::Library::load(&crate::core::themes::dir());
            let id = std::env::var("MANYCRAFT_FREYA_THEME").ok();
            if let Some(next) = super::theme::Theme::load(&library, id.as_deref(), dark) {
                theme.set(std::rc::Rc::new(next));
            }
            settle().await;
            Ok(json!({}))
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

thread_local! {
    static THEME: Cell<Option<State<std::rc::Rc<super::theme::Theme>>>> = const { Cell::new(None) };
}

/// Lets the drive tool switch the theme it was given.
pub fn share_theme(theme: State<std::rc::Rc<super::theme::Theme>>) {
    THEME.with(|t| t.set(Some(theme)));
}

fn use_try_consume_theme() -> Option<State<std::rc::Rc<super::theme::Theme>>> {
    THEME.with(Cell::get)
}

fn window() -> Value {
    let platform = Platform::get().current_window();
    let size = *platform.root_size.peek();
    let (x, y) = native::place().map_or((0., 0.), |(r, _)| (r.x as f32, r.y as f32));
    let dark = THEME.with(Cell::get).is_some_and(|t| t.peek().dark);
    json!({
        "id": 1,
        "title": "Manycraft",
        "main": true,
        "active": *platform.is_app_focused.peek(),
        "x": x, "y": y,
        "w": size.width, "h": size.height,
        "scale": *platform.scale_factor.peek(),
        "dark": dark,
        "native": if native::handle() == 0 { Value::Null } else { json!(native::handle()) },
        "freya": true,
    })
}

fn state(store: Store) -> Value {
    let ui = store.ui;
    json!({
        "home": crate::core::paths::data_dir().display().to_string(),
        "page": format!("{:?}", *ui.page.peek()),
        "selection": ui.selection.peek().iter().cloned().collect::<Vec<_>>(),
        "size": format!("{:?}", *ui.size.peek()),
        "tile_w": *ui.tile_w.peek(),
        "palette": *ui.palette.peek(),
        "sheet": ui.sheet.peek().is_some(),
        "menu": ui.menu.peek().is_some(),
        "toasts": ui.toasts.peek().iter().map(|t| t.text.clone()).collect::<Vec<_>>(),
        "games": store.model.peek().games.len(),
        "playing": store.model.peek().playing.clone(),
    })
}

fn visible(s: &Spot) -> bool {
    let size = *Platform::get().current_window().root_size.peek();
    let b = s.bounds;
    b.w > 0. && b.h > 0. && b.x < size.width && b.y < size.height && b.x + b.w > 0. && b.y + b.h > 0.
}

fn spot_json(s: &Spot) -> Value {
    let b = s.bounds;
    let shown = visible(s);
    json!({ "window": 1, "id": s.id, "role": s.role, "label": s.label, "bounds": [b.x, b.y, b.w, b.h], "visible": shown.then(|| json!([b.x, b.y, b.w, b.h])), "enabled": s.enabled })
}

fn matches(target: &str, s: &Spot) -> bool {
    let (kind, query) = target.split_once([':', '~']).unwrap_or(("any", target));
    let fuzzy = target.as_bytes().get(kind.len()) == Some(&b'~') || kind == "any";
    let query = query.to_lowercase();
    let test = |value: &str| {
        let value = value.to_lowercase();
        if fuzzy { value.contains(&query) } else { value == query }
    };
    match kind {
        "id" => test(&s.id),
        "text" | "label" => test(&s.label),
        "role" => test(s.role),
        _ => test(&s.id) || test(&s.label),
    }
}

fn find(b: &Value, prefix: &str) -> Result<Spot, String> {
    let target = b[format!("{prefix}target")].as_str().ok_or("target missing")?;
    spots::find(target, b["nth"].as_u64().unwrap_or(0) as usize)
}

async fn wait(b: &Value) -> Result<Value, String> {
    let started = Instant::now();
    let limit = Duration::from_secs_f64(b["timeout"].as_f64().unwrap_or(10.).clamp(0., 3600.));
    let quiet = Duration::from_millis(b["quiet"].as_u64().unwrap_or(300));
    let what = b["for"].as_str().unwrap_or("idle");
    loop {
        let done = match what {
            "idle" => started.elapsed() >= quiet && LAST_FRAME.with(Cell::get).is_none_or(|l| l.elapsed() >= quiet),
            "window" => native::handle() != 0,
            "gone" => find(b, "").is_err(),
            _ => find(b, "").is_ok(),
        };
        if done {
            return Ok(json!({ "waited": started.elapsed().as_millis() as u64 }));
        }
        if started.elapsed() > limit {
            return Err(format!("still not {what} after {:.1} s", limit.as_secs_f64()));
        }
        after(Duration::from_millis(if what == "idle" { 30 } else { 100 })).await;
    }
}

fn send(event: PlatformEvent) -> Result<(), String> {
    let handle = HANDLE.with(|h| h.borrow().clone()).ok_or("the window is not up yet")?;
    handle.send_platform_event(event, Platform::window_id());
    Ok(())
}

fn press(key: Key, code: Code, modifiers: Modifiers) {
    for name in [KeyboardEventName::KeyDown, KeyboardEventName::KeyUp] {
        let _ = send(PlatformEvent::Keyboard { name, key: key.clone(), code, modifiers });
    }
}

fn hold(modifiers: Modifiers, down: bool) {
    let name = if down { KeyboardEventName::KeyDown } else { KeyboardEventName::KeyUp };
    let now = if down { modifiers } else { Modifiers::empty() };
    for (flag, key, code) in [(Modifiers::SHIFT, NamedKey::Shift, Code::ShiftLeft), (Modifiers::CONTROL, NamedKey::Control, Code::ControlLeft), (Modifiers::ALT, NamedKey::Alt, Code::AltLeft), (Modifiers::META, NamedKey::Meta, Code::MetaLeft)] {
        if modifiers.contains(flag) {
            let _ = send(PlatformEvent::Keyboard { name, key: Key::Named(key), code, modifiers: now });
        }
    }
}

fn mods_of(b: &Value) -> Modifiers {
    let mut m = Modifiers::empty();
    for word in b["mods"].as_str().unwrap_or_default().split(['+', '-']) {
        m |= match word {
            "shift" => Modifiers::SHIFT,
            "alt" | "option" => Modifiers::ALT,
            "ctrl" | "control" => Modifiers::CONTROL,
            "platform" | "super" | "win" => Modifiers::META,
            "cmd" | "mod" | "secondary" if cfg!(target_os = "macos") => Modifiers::META,
            "cmd" | "mod" | "secondary" => Modifiers::CONTROL,
            _ => Modifiers::empty(),
        };
    }
    m
}

async fn pointer(cmd: &str, b: &Value) -> Result<Value, String> {
    settle().await;
    let (at, spot) = match (b["x"].as_f64(), b["y"].as_f64()) {
        (Some(x), Some(y)) => ((x as f32, y as f32), None),
        _ => {
            let spot = find(b, "")?;
            if !visible(&spot) {
                return Err(format!("{} is not on screen", spot.id));
            }
            (spot.bounds.center(), Some(spot))
        }
    };
    let s = f64::from(scale());
    let cursor = CursorPoint::new(f64::from(at.0) * s, f64::from(at.1) * s);
    let button = match b["button"].as_str() {
        Some("right") => MouseButton::Right,
        Some("middle") => MouseButton::Middle,
        _ => MouseButton::Left,
    };
    let modifiers = mods_of(b);
    if matches!(cmd, "click" | "down" | "up") {
        let mut mode = Platform::get().current_window().navigation_mode;
        mode.set_if_modified(NavigationMode::NotKeyboard);
    }
    hold(modifiers, true);
    if !modifiers.is_empty() {
        settle().await;
    }
    send(PlatformEvent::Mouse { name: MouseEventName::MouseMove, cursor, button: None })?;
    POINTER.with(|p| p.set(at));
    match cmd {
        "click" => {
            settle().await;
            for _ in 0..b["count"].as_u64().unwrap_or(1).max(1) {
                send(PlatformEvent::Mouse { name: MouseEventName::MouseDown, cursor, button: Some(button) })?;
                send(PlatformEvent::Mouse { name: MouseEventName::MouseUp, cursor, button: Some(button) })?;
            }
        }
        "down" => send(PlatformEvent::Mouse { name: MouseEventName::MouseDown, cursor, button: Some(button) })?,
        "up" => send(PlatformEvent::Mouse { name: MouseEventName::MouseUp, cursor, button: Some(button) })?,
        "wheel" => {
            let (dx, dy) = (b["dx"].as_f64().unwrap_or(0.), b["dy"].as_f64().unwrap_or(0.));
            let per = if b["pixels"].as_bool().unwrap_or(false) { 1. } else { 53. };
            for _ in 0..b["steps"].as_u64().unwrap_or(1).max(1) {
                send(PlatformEvent::Wheel { name: WheelEventName::Wheel, scroll: CursorPoint::new(dx * per, dy * per), cursor, source: WheelSource::Line, phase: TouchPhase::Moved })?;
                settle().await;
            }
        }
        _ => {}
    }
    if !modifiers.is_empty() {
        settle().await;
    }
    hold(modifiers, false);
    settle().await;
    Ok(json!({ "window": 1, "at": [at.0, at.1], "spot": spot.as_ref().map(spot_json), "held": Value::Null }))
}

/// Waits until the window drew what the last input changed, and then for a short calm.
async fn settle() {
    let before = COUNT.with(Cell::get);
    Platform::get().current_window().request_redraw();
    let started = Instant::now();
    while COUNT.with(Cell::get) == before && started.elapsed() < Duration::from_millis(400) {
        after(Duration::from_millis(8)).await;
    }
    while LAST_FRAME.with(Cell::get).is_some_and(|l| l.elapsed() < Duration::from_millis(60)) && started.elapsed() < Duration::from_millis(1500) {
        after(Duration::from_millis(16)).await;
    }
}

async fn capture(path: PathBuf) -> Result<Value, String> {
    let window = Platform::get().current_window();
    let receiver = window.post_render_callback(move |surface| {
        let (w, h) = (surface.width(), surface.height());
        let info = ImageInfo::new(ISize::new(w, h), ColorType::RGBA8888, SkAlpha::Unpremul, None);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let ok = surface.read_pixels(&info, &mut pixels, (w * 4) as usize, IPoint::new(0, 0));
        (ok, w as u32, h as u32, pixels)
    });
    window.request_redraw();
    let (ok, w, h, pixels) = receiver.await.map_err(|_| "the window did not draw".to_owned())?;
    if !ok {
        return Err("the window's pixels could not be read".into());
    }
    let img = image::RgbaImage::from_raw(w, h, pixels).ok_or("bad pixel buffer")?;
    img.save_with_format(&path, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(json!({ "path": path.display().to_string(), "w": w, "h": h }))
}

/// Parses `ctrl+shift+k`, `cmd-enter`, `esc` or `down` into the key, its modifiers and its physical key.
fn parse_key(name: &str) -> Option<(Key, Modifiers, Code)> {
    let lower = name.to_lowercase();
    let parts: Vec<&str> = if lower.len() > 1 { lower.split(['+', '-']).filter(|p| !p.is_empty()).collect() } else { vec![lower.as_str()] };
    let (last, mods) = parts.split_last()?;
    let mut modifiers = Modifiers::empty();
    for m in mods {
        modifiers |= match *m {
            "shift" => Modifiers::SHIFT,
            "alt" | "option" => Modifiers::ALT,
            "ctrl" | "control" => Modifiers::CONTROL,
            "platform" | "super" | "win" => Modifiers::META,
            "cmd" | "mod" | "secondary" | "primary" if cfg!(target_os = "macos") => Modifiers::META,
            "cmd" | "mod" | "secondary" | "primary" => Modifiers::CONTROL,
            _ => return None,
        };
    }
    let named = |k: NamedKey, c: Code| Some((Key::Named(k), c));
    let (key, code) = match *last {
        "enter" | "return" => named(NamedKey::Enter, Code::Enter),
        "esc" | "escape" => named(NamedKey::Escape, Code::Escape),
        "tab" => named(NamedKey::Tab, Code::Tab),
        "space" => Some((Key::Character(" ".into()), Code::Space)),
        "backspace" => named(NamedKey::Backspace, Code::Backspace),
        "delete" => named(NamedKey::Delete, Code::Delete),
        "up" => named(NamedKey::ArrowUp, Code::ArrowUp),
        "down" => named(NamedKey::ArrowDown, Code::ArrowDown),
        "left" => named(NamedKey::ArrowLeft, Code::ArrowLeft),
        "right" => named(NamedKey::ArrowRight, Code::ArrowRight),
        "home" => named(NamedKey::Home, Code::Home),
        "end" => named(NamedKey::End, Code::End),
        "pageup" => named(NamedKey::PageUp, Code::PageUp),
        "pagedown" => named(NamedKey::PageDown, Code::PageDown),
        "f1" => named(NamedKey::F1, Code::F1),
        "f2" => named(NamedKey::F2, Code::F2),
        "." | "period" => Some((Key::Character(".".into()), Code::Period)),
        "," | "comma" => Some((Key::Character(",".into()), Code::Comma)),
        "=" | "plus" => Some((Key::Character("=".into()), Code::Equal)),
        "minus" => Some((Key::Character("-".into()), Code::Minus)),
        c if c.chars().count() == 1 => {
            let ch = c.chars().next()?;
            let code = match ch {
                'a'..='z' => format!("Key{}", ch.to_ascii_uppercase()).parse().ok()?,
                '0'..='9' => format!("Digit{ch}").parse().ok()?,
                _ => Code::Unidentified,
            };
            Some((Key::Character(c.to_owned()), code))
        }
        _ => None,
    }?;
    Some((key, modifiers, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_with_modifiers() {
        let (key, mods, code) = parse_key("ctrl+shift+k").unwrap();
        assert_eq!((key, code), (Key::Character("k".into()), Code::KeyK));
        assert!(mods.contains(Modifiers::CONTROL | Modifiers::SHIFT));
        assert_eq!(parse_key("esc").unwrap().2, Code::Escape);
        assert_eq!(parse_key("ctrl+.").unwrap().2, Code::Period);
        assert_eq!(parse_key("-").unwrap().0, Key::Character("-".into()));
        assert!(parse_key("hyper+k").is_none());
    }
}
