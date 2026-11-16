//! The Freya front end, behind the `freya` feature: the same core and runner as the GPUI shell, drawn with Freya.

mod commands;
pub mod drive;
mod grid;
mod icons;
mod kit;
mod native;
mod shell;
mod stills;
mod store;
mod theme;
mod tray;
mod ui;

use std::rc::Rc;
use std::sync::Mutex;

use freya::prelude::*;

use self::store::Store;
use self::theme::Theme;
use self::ui::Ui;
use crate::app::event::Event;
use crate::core::shortcut::Play;
use crate::core::themes::{self, Library};

const SIZE: (f64, f64) = (1280., 820.);
const MIN: (f64, f64) = (720., 480.);

static OS_SCALE: Mutex<f64> = Mutex::new(1.);

/// The window size an agent asked for, from `MANYCRAFT_DRIVE_SIZE` as `WxH`.
fn drive_size() -> Option<(f64, f64)> {
    let value = std::env::var("MANYCRAFT_DRIVE_SIZE").ok()?;
    let (w, h) = value.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The device scale a driven window renders at, from `MANYCRAFT_DRIVE_SCALE`; 2 by default, so screenshots are sharp.
fn drive_scale() -> Option<f64> {
    if !crate::core::drive::on() {
        return None;
    }
    Some(std::env::var("MANYCRAFT_DRIVE_SCALE").ok().and_then(|s| s.parse().ok()).filter(|s: &f64| *s >= 1. && *s <= 4.).unwrap_or(2.))
}

fn appearance() -> Option<bool> {
    match std::env::var("MANYCRAFT_THEME").unwrap_or_default().as_str() {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    }
}

/// Opens the manager window and runs until it closes; `play` starts an instance as a desktop shortcut asked.
pub fn run(play: Option<Play>) {
    let size = drive_size().unwrap_or(SIZE);
    let scale = drive_scale();
    let away = cfg!(target_os = "windows") && crate::core::drive::away();
    let (close, closes) = async_channel::unbounded();
    let window = WindowConfig::new_app(Root { play, closes })
        .with_size(size.0, size.1)
        .with_min_size(MIN.0, MIN.1)
        .with_title("Manycraft")
        .with_shown(!away)
        .with_on_close(move |_, _| {
            let _ = close.try_send(());
            CloseDecision::KeepOpen
        })
        .with_window_handle(move |window| {
            let os = window.scale_factor();
            *OS_SCALE.lock().unwrap_or_else(|e| e.into_inner()) = os;
            if let Some(device) = scale {
                let _ = window.request_inner_size(freya::winit::dpi::PhysicalSize::new((size.0 * device).round() as u32, (size.1 * device).round() as u32));
            }
            native::attach(window);
        });
    launch(LaunchConfig::new().with_plugin(drive::Watch).with_window(window));
}

struct Root {
    play: Option<Play>,
    closes: async_channel::Receiver<()>,
}

impl App for Root {
    fn render(&self) -> impl IntoElement {
        let platform = Platform::get().current_window();
        let os_dark = *platform.preferred_theme.read() == PreferredTheme::Dark;
        let dark = appearance().unwrap_or(os_dark);
        let library = use_hook(|| Rc::new(Library::load(&themes::dir())));
        let mut theme = use_provide_context(|| State::create(Rc::new(load(&library, dark))));
        drive::share_theme(theme);
        let lib = library.clone();
        use_side_effect_with_deps(&dark, move |dark| {
            if theme.peek().dark != *dark {
                theme.set(Rc::new(load(&lib, *dark)));
            }
        });
        let root = platform.root_size;
        let ui = use_provide_context(|| {
            let s = *root.peek();
            Ui::create((s.width, s.height))
        });
        let mut window = ui.window;
        let s = *root.read();
        window.set_if_modified((s.width, s.height));
        let play = self.play.clone();
        let closes = self.closes.clone();
        let store = use_provide_context(move || {
            let (store, updates) = Store::create(ui);
            if let Some(updates) = updates {
                spawn(store.pump(updates));
            }
            store.reload(false);
            if let Some(play) = play {
                store.dispatch(Event::RequestPlay { instance: play.instance, account: play.account });
            }
            if let Some(device) = drive_scale() {
                let os = *OS_SCALE.lock().unwrap_or_else(|e| e.into_inner());
                Platform::get().current_window().set_custom_scale_factor(device / os);
            }
            store.listen(closes);
            drive::start(store);
            store
        });
        use_drop(move || store.close());
        shell::Shell
    }
}

fn load(library: &Library, dark: bool) -> Theme {
    let id = std::env::var("MANYCRAFT_FREYA_THEME").ok();
    Theme::load(library, id.as_deref(), dark).expect("the built-in themes always load")
}
