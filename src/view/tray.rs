//! The notification-area icon that holds Manycraft while its window is closed and games run, on Windows and Linux. It
//! exists only while the window is away and waits for clicks without a timer.

use crate::core::drive;
#[cfg(not(target_os = "macos"))]
use crate::core::i18n::tr;

/// What was clicked on the icon or its menu.
#[derive(Clone, Copy, Debug)]
pub enum Pick {
    Open,
    QuitAll,
    Quit,
}

/// The icon while it is up, and where its clicks go.
pub struct Tray {
    #[cfg(not(target_os = "macos"))]
    icon: Option<tray_icon::TrayIcon>,
    shown: bool,
    picks: async_channel::Sender<Pick>,
}

impl Tray {
    /// A tray with no icon up yet; clicks arrive on the returned receiver.
    pub fn new() -> (Self, async_channel::Receiver<Pick>) {
        let (picks, rx) = async_channel::unbounded();
        (
            Self {
                #[cfg(not(target_os = "macos"))]
                icon: None,
                shown: false,
                picks,
            },
            rx,
        )
    }

    /// Whether the icon is up.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Puts the icon up; false when the system has no notification area to put it in.
    pub fn show(&mut self) -> bool {
        if self.shown {
            return true;
        }
        if drive::fenced("tray", "shown") {
            self.shown = true;
            return true;
        }
        #[cfg(not(target_os = "macos"))]
        {
            match build(self.picks.clone()) {
                Ok(icon) => {
                    self.icon = Some(icon);
                    self.shown = true;
                    true
                }
                Err(e) => {
                    eprintln!("tray: {e:#}");
                    false
                }
            }
        }
        #[cfg(target_os = "macos")]
        false
    }

    /// Takes the icon down.
    pub fn hide(&mut self) {
        if !self.shown {
            return;
        }
        drive::note("tray hidden".into());
        self.shown = false;
        #[cfg(not(target_os = "macos"))]
        self.icon.take();
    }
}

#[cfg(not(target_os = "macos"))]
fn build(picks: async_channel::Sender<Pick>) -> anyhow::Result<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    #[cfg(target_os = "linux")]
    if !host() {
        anyhow::bail!("no notification area");
    }
    let open = MenuItem::with_id("open", tr("Öffnen"), true, None);
    let quit_all = MenuItem::with_id("quit-all", tr("Alle beenden"), true, None);
    let quit = MenuItem::with_id("quit", tr("Beenden"), true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit_all, &quit])?;
    let clicks = picks.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        let open = matches!(event, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. });
        if open {
            let _ = clicks.try_send(Pick::Open);
        }
    }));
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let pick = match event.id.as_ref() {
            "open" => Pick::Open,
            "quit-all" => Pick::QuitAll,
            "quit" => Pick::Quit,
            _ => return,
        };
        let _ = picks.try_send(pick);
    }));
    Ok(TrayIconBuilder::new().with_menu(Box::new(menu)).with_menu_on_left_click(false).with_tooltip("Manycraft").with_icon(icon()?).build()?)
}

/// Whether a desktop shell shows status icons, asked once; GNOME without its extension has none, and an icon nobody
/// shows would leave no way back to the window.
#[cfg(target_os = "linux")]
fn host() -> bool {
    static HOST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HOST.get_or_init(ask_host)
}

#[cfg(target_os = "linux")]
fn ask_host() -> bool {
    let session = zbus::blocking::connection::Builder::session().map(|b| b.method_timeout(std::time::Duration::from_millis(500))).and_then(|b| b.build());
    let Ok(session) = session else { return false };
    zbus::blocking::Proxy::new(&session, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher")
        .and_then(|watcher| watcher.get_property::<bool>("IsStatusNotifierHostRegistered"))
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn icon() -> anyhow::Result<tray_icon::Icon> {
    Ok(tray_icon::Icon::from_resource(1, None)?)
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn icon() -> anyhow::Result<tray_icon::Icon> {
    let image = image::load_from_memory(include_bytes!("../../packaging/icons/manycraft.png"))?.resize(64, 64, image::imageops::FilterType::Triangle).into_rgba8();
    let (w, h) = image.dimensions();
    Ok(tray_icon::Icon::from_rgba(image.into_raw(), w, h)?)
}
