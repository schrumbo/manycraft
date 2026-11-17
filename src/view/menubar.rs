//! The macOS menu bar, built with muda since Freya drives none: the app, Ablage, Bearbeiten, Darstellung, Spiele and
//! Fenster menus.
//! Every item runs a window command, so a menu item, its shortcut and the palette do the same; items that cannot run
//! now are dimmed, never hidden. Other systems have no menu bar and these calls do nothing.

use super::native::Told;
use super::store::Store;

#[cfg(target_os = "macos")]
use super::commands::{self, Command};
#[cfg(target_os = "macos")]
use crate::core::i18n::tr;

#[cfg(target_os = "macos")]
struct Item {
    command: Command,
    item: muda::MenuItem,
    text: String,
    enabled: bool,
}

#[cfg(target_os = "macos")]
thread_local! {
    static ITEMS: std::cell::RefCell<Vec<Item>> = const { std::cell::RefCell::new(Vec::new()) };
    static BAR: std::cell::RefCell<Option<muda::Menu>> = const { std::cell::RefCell::new(None) };
}

/// Puts the menu bar up in place of winit's, whose Quit would end Manycraft without asking about running games;
/// chosen items go to `told`.
pub fn install(told: async_channel::Sender<Told>) {
    #[cfg(target_os = "macos")]
    if let Err(e) = build(told) {
        eprintln!("menu bar: {e:#}");
    }
    #[cfg(not(target_os = "macos"))]
    let _ = told;
}

#[cfg(target_os = "macos")]
fn build(told: async_channel::Sender<Told>) -> anyhow::Result<()> {
    use muda::{AboutMetadata, Menu, PredefinedMenuItem as Pre, Submenu};
    let separator = Pre::separator;
    let mut items = Vec::new();
    let mut item = |command: Command| {
        let text = label(command);
        let made = muda::MenuItem::with_id(command.name(), &text, true, command.shortcut().map(accelerator));
        items.push(Item { command, item: made.clone(), text, enabled: true });
        made
    };
    let about = AboutMetadata { name: Some("Manycraft".into()), version: Some(env!("CARGO_PKG_VERSION").into()), ..AboutMetadata::default() };
    let app = Submenu::with_items(
        "Manycraft",
        true,
        &[
            &Pre::about(Some(&tr("Über Manycraft")), Some(about)),
            &separator(),
            &item(Command::OpenSettings),
            &separator(),
            &Pre::services(Some(&tr("Dienste"))),
            &separator(),
            &Pre::hide(Some(&tr("Manycraft ausblenden"))),
            &Pre::hide_others(Some(&tr("Andere ausblenden"))),
            &Pre::show_all(Some(&tr("Alle einblenden"))),
            &separator(),
            &item(Command::Quit),
        ],
    )?;
    let file = Submenu::with_items(
        tr("Ablage"),
        true,
        &[
            &item(Command::AddAccount),
            &item(Command::NewInstance),
            &item(Command::NewBatch),
            &item(Command::OpenTrash),
            &separator(),
            &item(Command::Import),
            &item(Command::FindModpacks),
            &item(Command::AddMods),
            &separator(),
            &item(Command::ImportPrism),
            &item(Command::SignInWaiting),
            &separator(),
            &Pre::close_window(Some(&tr("Fenster schließen"))),
        ],
    )?;
    let edit = Submenu::with_items(tr("Bearbeiten"), true, &[&item(Command::Undo)])?;
    let view = Submenu::with_items(
        tr("Darstellung"),
        true,
        &[
            &item(Command::ShowAll),
            &item(Command::ShowAttention),
            &item(Command::ShowAccounts),
            &item(Command::SwitchAccount),
            &item(Command::OpenChat),
            &separator(),
            &item(Command::ShowTiles),
            &item(Command::ShowTable),
            &separator(),
            &item(Command::Larger),
            &item(Command::Smaller),
            &item(Command::Fit),
            &separator(),
            &item(Command::ToggleSidebar),
            &item(Command::OpenPalette),
            &separator(),
            &Pre::fullscreen(Some(&tr("Vollbild"))),
        ],
    )?;
    let games = Submenu::with_items(
        tr("Spiele"),
        true,
        &[
            &item(Command::StartSelected),
            &item(Command::StopSelected),
            &item(Command::RestartSelected),
            &item(Command::ReconnectSelected),
            &item(Command::MessageSelected),
            &separator(),
            &item(Command::StartAll),
            &item(Command::StopAll),
            &item(Command::ReconnectDropped),
            &separator(),
            &item(Command::NextInstance),
            &item(Command::PreviousInstance),
            &item(Command::NextAttention),
        ],
    )?;
    let window = Submenu::with_items(
        tr("Fenster"),
        true,
        &[&Pre::minimize(Some(&tr("Im Dock ablegen"))), &Pre::maximize(Some(&tr("Zoomen"))), &separator(), &Pre::bring_all_to_front(Some(&tr("Alle nach vorne bringen")))],
    )?;
    let bar = Menu::with_items(&[&app, &file, &edit, &view, &games, &window])?;
    bar.init_for_nsapp();
    window.set_as_windows_menu_for_nsapp();
    muda::MenuEvent::set_event_handler(Some(move |event: muda::MenuEvent| {
        let _ = told.try_send(Told::Menu(event.id.as_ref().to_owned()));
    }));
    ITEMS.with_borrow_mut(|all| *all = items);
    BAR.with_borrow_mut(|b| *b = Some(bar));
    Ok(())
}

#[cfg(target_os = "macos")]
fn accelerator(keys: commands::Shortcut) -> muda::accelerator::Accelerator {
    use muda::accelerator::{Accelerator, Modifiers};
    let mut mods = Modifiers::empty();
    for (held, flag) in [(keys.primary, Modifiers::META), (keys.shift, Modifiers::SHIFT), (keys.alt, Modifiers::ALT), (keys.ctrl, Modifiers::CONTROL)] {
        if held {
            mods |= flag;
        }
    }
    Accelerator::new(mods, keys.code)
}

#[cfg(target_os = "macos")]
fn label(command: Command) -> String {
    match command {
        Command::OpenSettings => tr("Einstellungen …"),
        Command::Import => tr("Importieren…"),
        other => other.label(),
    }
}

/// Runs the menu item `id` as its shortcut would on the shown page; while a text field has focus only the palette toggle
/// and Quit, since the menu sees its keys before the field does.
pub fn chosen(store: Store, id: &str) {
    #[cfg(target_os = "macos")]
    if let Some(run) = Command::named(id).and_then(|c| commands::for_menu(store, c)).filter(|c| matches!(c, Command::OpenPalette | Command::Quit) || !super::kit::field::typing()) {
        run.run(store);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (store, id);
}

/// Dims the items that cannot run now and names each for what it runs on the shown page; reading the state it depends
/// on, so an effect calls it again when that changes.
#[cfg(target_os = "macos")]
pub fn refresh(store: Store) {
    let ui = store.ui;
    let _ = (store.model.read(), ui.page.read(), ui.selection.read(), ui.cursor.read(), ui.toasts.read(), ui.view.read(), ui.show.read(), ui.size.read());
    let _ = (ui.sheet.read(), ui.palette.read(), ui.menu.read(), ui.accounts.picked.read(), ui.accounts.prism.read(), ui.accounts.view.read());
    let _ = (ui.browser_undo.read(), ui.session_undo.read());
    let sidebar = *ui.sidebar.read() && !*ui.rail.read();
    ITEMS.with_borrow_mut(|all| {
        for it in all.iter_mut() {
            let runs = commands::for_menu(store, it.command);
            let text = match it.command {
                Command::ToggleSidebar if sidebar => tr("Seitenleiste einklappen"),
                Command::ToggleSidebar => tr("Seitenleiste ausklappen"),
                Command::Undo if runs == Some(Command::Undo) => commands::undo_label(store).unwrap_or_else(|| label(Command::Undo)),
                c => label(runs.unwrap_or(c)),
            };
            if text != it.text {
                it.item.set_text(&text);
                it.text = text;
            }
            if runs.is_some() != it.enabled {
                it.enabled = runs.is_some();
                it.item.set_enabled(it.enabled);
            }
        }
    });
}
