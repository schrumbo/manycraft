//! The notification-area icon that holds Manycraft while its window is closed and games run, on Windows and Linux. It
//! exists only while the window is away and waits for clicks without a timer.

use gpui_kit::*;

#[cfg(not(target_os = "macos"))]
use super::actions::{OpenFromTray, Quit, QuitAll};
#[cfg(not(target_os = "macos"))]
use crate::core::i18n::tr;

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy)]
enum Pick {
    Open,
    QuitAll,
    Quit,
}

struct Tray {
    #[cfg(not(target_os = "macos"))]
    icon: Option<tray_icon::TrayIcon>,
    shown: bool,
    #[cfg(not(target_os = "macos"))]
    picks: async_channel::Sender<Pick>,
}

impl Global for Tray {}

/// Gets clicks on the icon and its menu to the actions that handle them.
pub fn init(cx: &mut App) {
    #[cfg(not(target_os = "macos"))]
    {
        let (picks, rx) = async_channel::unbounded();
        cx.set_global(Tray { icon: None, shown: false, picks });
        cx.spawn(async move |cx| {
            while let Ok(pick) = rx.recv().await {
                cx.update(|cx| match pick {
                    Pick::Open => cx.dispatch_action(&OpenFromTray),
                    Pick::QuitAll => cx.dispatch_action(&QuitAll),
                    Pick::Quit => cx.dispatch_action(&Quit),
                });
            }
        })
        .detach();
    }
    #[cfg(target_os = "macos")]
    cx.set_global(Tray { shown: false });
}

/// Whether the icon is up.
pub fn shown(cx: &App) -> bool {
    cx.try_global::<Tray>().is_some_and(|t| t.shown)
}

/// Puts the icon up; false when the system has no notification area to put it in.
pub fn show(cx: &mut App) -> bool {
    if shown(cx) {
        return true;
    }
    let Some(tray) = cx.try_global::<Tray>() else { return false };
    if crate::core::drive::fenced("tray", "shown") {
        cx.global_mut::<Tray>().shown = true;
        return true;
    }
    #[cfg(not(target_os = "macos"))]
    {
        match build(tray.picks.clone()) {
            Ok(icon) => {
                let tray = cx.global_mut::<Tray>();
                tray.icon = Some(icon);
                tray.shown = true;
                true
            }
            Err(e) => {
                eprintln!("tray: {e:#}");
                false
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = tray;
        false
    }
}

/// Takes the icon down.
pub fn hide(cx: &mut App) {
    if !shown(cx) {
        return;
    }
    crate::core::drive::note("tray hidden".into());
    let tray = cx.global_mut::<Tray>();
    tray.shown = false;
    #[cfg(not(target_os = "macos"))]
    tray.icon.take();
}

#[cfg(not(target_os = "macos"))]
fn build(picks: async_channel::Sender<Pick>) -> anyhow::Result<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

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
