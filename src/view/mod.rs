//! The Freya front end, behind the `freya` feature: the same core and runner as the GPUI shell, drawn with Freya.

mod accounts;
mod alert;
mod chat;
mod commands;
pub mod drive;
mod grid;
mod icons;
mod install;
mod instance;
mod kit;
mod library;
mod start;
mod menubar;
mod native;
mod settings;
mod sheets;
mod shell;
mod signals;
mod stills;
mod store;
mod theme;
mod tray;
mod ui;
mod updates;

use std::rc::Rc;
use std::sync::Mutex;

use freya::prelude::*;

use self::store::Store;
use self::theme::Theme;
use self::ui::Ui;
use crate::app::event::Event;
use crate::core::settings::{self as prefs, Appearance};
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

/// The theme choice the window draws with: the theme library and the saved appearance and theme, so Settings can change
/// them live.
#[derive(Clone, Copy, PartialEq)]
pub struct Look {
    pub library: State<Rc<Library>>,
    pub choice: State<(Appearance, Option<String>)>,
}

impl Look {
    /// Reads the user's theme files again, off the UI thread.
    pub fn reload(self) {
        self.reload_then(|| {});
    }

    /// Reads the user's theme files again and runs `then` once the new library is in place.
    pub fn reload_then(mut self, then: impl FnOnce() + 'static) {
        let Some(rx) = kit::off_thread("themes-load", || Library::load(&themes::dir())) else { return };
        spawn_in_window(async move {
            let Ok(library) = rx.recv().await else { return };
            if self.library.try_peek().is_none() {
                return;
            }
            self.library.set(Rc::new(library));
            then();
        });
    }
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
    let (told, hears) = async_channel::unbounded();
    let close = told.clone();
    let window = WindowConfig::new_app(Root { play, hears })
        .with_size(size.0, size.1)
        .with_min_size(MIN.0, MIN.1)
        .with_title("Manycraft")
        .with_shown(!away)
        .with_on_close(move |_, _| {
            let _ = close.try_send(native::Told::Close);
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
    launch(LaunchConfig::new().with_plugin(drive::Watch).with_plugin(native::Follow::new(told)).with_plugin(signals::Watch::default()).with_window(window));
}

struct Root {
    play: Option<Play>,
    hears: async_channel::Receiver<native::Told>,
}

impl App for Root {
    fn render(&self) -> impl IntoElement {
        let platform = Platform::get().current_window();
        let os_dark = *platform.preferred_theme.read() == PreferredTheme::Dark;
        let look = use_provide_context(|| {
            let s = prefs::loaded();
            Look { library: State::create(Rc::new(Library::load(&themes::dir()))), choice: State::create((s.appearance, s.theme.clone())) }
        });
        let library = look.library.read().clone();
        let (chosen, id) = look.choice.read().clone();
        let dark = appearance().unwrap_or(match chosen {
            Appearance::System => os_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        });
        let id = std::env::var("MANYCRAFT_FREYA_THEME").ok().or(id);
        let mut theme = use_provide_context(|| State::create(Rc::new(load(&library, id.as_deref(), dark))));
        drive::share_theme(theme);
        let key = (dark, id.clone(), Rc::as_ptr(&library) as usize);
        use_side_effect_with_deps(&key, move |(dark, id, _)| {
            let library = look.library.peek().clone();
            let next = Rc::new(load(&library, id.as_deref(), *dark));
            if *theme.peek() != next {
                theme.set(next);
            }
        });
        let root = platform.root_size;
        let ui = use_provide_context(|| {
            let s = *root.peek();
            Ui::create((s.width, s.height))
        });
        let s = *root.read();
        ui.resize((s.width, s.height));
        let play = self.play.clone();
        let hears = self.hears.clone();
        let store = use_provide_context(move || {
            let store = Store::create(ui);
            store.start();
            if let Some(play) = play {
                store.dispatch(Event::RequestPlay { instance: play.instance, account: play.account });
            }
            if let Some(device) = drive_scale() {
                let os = *OS_SCALE.lock().unwrap_or_else(|e| e.into_inner());
                Platform::get().current_window().set_custom_scale_factor(device / os);
            }
            store.listen(hears, signals::listen());
            drive::start(store);
            updates::start(store);
            sheets::answer();
            store
        });
        #[cfg(not(target_os = "windows"))]
        use_side_effect(move || native::attention(store.model.read().attention_count(None)));
        #[cfg(target_os = "macos")]
        use_side_effect(move || menubar::refresh(store));
        use_drop(move || {
            updates::on_quit(store);
            store.close();
        });
        shell::Shell
    }
}

fn load(library: &Library, id: Option<&str>, dark: bool) -> Theme {
    Theme::load(library, id, dark).expect("the built-in themes always load")
}
