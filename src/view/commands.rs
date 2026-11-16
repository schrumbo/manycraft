//! Every command of the window: its name, its shortcut, when it is available and what it does. The palette, the
//! shortcuts and the drive tool all run commands from here, so a command works the same on every path.

use std::collections::{BTreeSet, HashSet};

use freya::prelude::*;

use super::grid::layout::TileSize;
use super::icons::Icon;
use super::kit::toast;
use super::store::Store;
use super::ui::{Filter, Page};
use crate::app::event::Event;
use crate::core::i18n::tr;
use crate::core::ipc::HotkeyAction;

/// What a command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenPalette,
    ToggleSidebar,
    Undo,
    FocusSearch,
    ShowAll,
    ShowAttention,
    ShowAccounts,
    OpenChat,
    OpenSettings,
    NextInstance,
    PreviousInstance,
    NextAttention,
    SelectAll,
    ClearSelection,
    StartSelected,
    StopSelected,
    RestartSelected,
    ReconnectSelected,
    StartAll,
    StopAll,
    Larger,
    Smaller,
    Fit,
    Quit,
}

/// A key combination; `primary` is Ctrl, or Cmd on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub code: Code,
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

const fn k(code: Code) -> Shortcut {
    Shortcut { code, primary: true, shift: false, alt: false, ctrl: false }
}

const fn ks(code: Code) -> Shortcut {
    Shortcut { code, primary: true, shift: true, alt: false, ctrl: false }
}

const fn ca(code: Code) -> Shortcut {
    Shortcut { code, primary: false, shift: false, alt: true, ctrl: true }
}

const fn bare(code: Code) -> Shortcut {
    Shortcut { code, primary: false, shift: false, alt: false, ctrl: false }
}

impl Shortcut {
    /// Whether the key event is this combination.
    pub fn matches(&self, e: &KeyboardEventData) -> bool {
        let m = e.modifiers;
        let same_code = e.code == self.code || (self.code == Code::Equal && e.code == Code::NumpadAdd) || (self.code == Code::Minus && e.code == Code::NumpadSubtract);
        let held = |flag| m.contains(flag);
        let mods = if cfg!(target_os = "macos") {
            held(Modifiers::META) == self.primary && held(Modifiers::CONTROL) == self.ctrl
        } else {
            held(Modifiers::CONTROL) == (self.primary || self.ctrl) && !held(Modifiers::META)
        };
        same_code && mods && held(Modifiers::SHIFT) == self.shift && held(Modifiers::ALT) == self.alt
    }

    /// The combination as the platform writes it: Strg+Umschalt+K, or ⇧⌘K on macOS.
    pub fn label(&self) -> String {
        let key = match self.code {
            Code::Period => tr("Punkt"),
            Code::Comma => tr("Komma"),
            Code::Equal => tr("Plus"),
            Code::Minus => tr("Minus"),
            Code::Enter => tr("Eingabe"),
            Code::Escape => "Esc".to_owned(),
            Code::ArrowRight => "→".to_owned(),
            Code::ArrowLeft => "←".to_owned(),
            Code::ArrowDown => "↓".to_owned(),
            other => format!("{other:?}").trim_start_matches("Key").trim_start_matches("Digit").to_owned(),
        };
        if cfg!(target_os = "macos") {
            let key = match self.code {
                Code::Period => ".".to_owned(),
                Code::Comma => ",".to_owned(),
                Code::Equal => "+".to_owned(),
                Code::Minus => "−".to_owned(),
                _ => key,
            };
            return format!("{}{}{}{}{key}", if self.ctrl { "⌃" } else { "" }, if self.alt { "⌥" } else { "" }, if self.shift { "⇧" } else { "" }, if self.primary { "⌘" } else { "" });
        }
        let mut parts = Vec::new();
        if self.primary || self.ctrl {
            parts.push(tr("Strg"));
        }
        if self.alt {
            parts.push("Alt".to_owned());
        }
        if self.shift {
            parts.push(tr("Umschalt"));
        }
        parts.push(key);
        parts.join("+")
    }
}

impl Command {
    /// Every command, in the order the palette lists them.
    pub const ALL: [Command; 24] = [
        Command::StartSelected,
        Command::StopSelected,
        Command::RestartSelected,
        Command::ReconnectSelected,
        Command::ClearSelection,
        Command::StartAll,
        Command::StopAll,
        Command::NextInstance,
        Command::PreviousInstance,
        Command::NextAttention,
        Command::SelectAll,
        Command::FocusSearch,
        Command::ShowAll,
        Command::ShowAttention,
        Command::ShowAccounts,
        Command::OpenChat,
        Command::Larger,
        Command::Smaller,
        Command::Fit,
        Command::ToggleSidebar,
        Command::Undo,
        Command::OpenSettings,
        Command::OpenPalette,
        Command::Quit,
    ];

    /// The name the drive tool and the GPUI shell use for it.
    pub fn name(self) -> &'static str {
        match self {
            Command::OpenPalette => "OpenPalette",
            Command::ToggleSidebar => "ToggleSidebar",
            Command::Undo => "Undo",
            Command::FocusSearch => "FocusSearch",
            Command::ShowAll => "ShowAll",
            Command::ShowAttention => "ShowAttention",
            Command::ShowAccounts => "ShowAccounts",
            Command::OpenChat => "OpenChat",
            Command::OpenSettings => "OpenSettings",
            Command::NextInstance => "NextInstance",
            Command::PreviousInstance => "PreviousInstance",
            Command::NextAttention => "NextAttention",
            Command::SelectAll => "SelectAll",
            Command::ClearSelection => "ClearSelection",
            Command::StartSelected => "StartSelected",
            Command::StopSelected => "StopSelected",
            Command::RestartSelected => "RestartSelected",
            Command::ReconnectSelected => "ReconnectSelected",
            Command::StartAll => "StartAll",
            Command::StopAll => "StopAll",
            Command::Larger => "Larger",
            Command::Smaller => "Smaller",
            Command::Fit => "FitTiles",
            Command::Quit => "Quit",
        }
    }

    /// The command named `name`.
    pub fn named(name: &str) -> Option<Self> {
        let name = name.rsplit("::").next().unwrap_or(name);
        Self::ALL.into_iter().find(|c| c.name().eq_ignore_ascii_case(name))
    }

    /// What the palette and menus call it.
    pub fn label(self) -> String {
        match self {
            Command::OpenPalette => tr("Befehle"),
            Command::ToggleSidebar => tr("Seitenleiste ein- oder ausblenden"),
            Command::Undo => tr("Rückgängig"),
            Command::FocusSearch => tr("Suchen"),
            Command::ShowAll => tr("Alle anzeigen"),
            Command::ShowAttention => tr("Brauchen dich anzeigen"),
            Command::ShowAccounts => tr("Accounts"),
            Command::OpenChat => tr("Chat"),
            Command::OpenSettings => tr("Einstellungen"),
            Command::NextInstance => tr("Nächsten Account spielen"),
            Command::PreviousInstance => tr("Vorherigen Account spielen"),
            Command::NextAttention => tr("Nächsten, der dich braucht"),
            Command::SelectAll => tr("Alles auswählen"),
            Command::ClearSelection => tr("Auswahl aufheben"),
            Command::StartSelected => tr("Ausgewählte starten"),
            Command::StopSelected => tr("Ausgewählte stoppen"),
            Command::RestartSelected => tr("Ausgewählte neu starten"),
            Command::ReconnectSelected => tr("Ausgewählte neu verbinden"),
            Command::StartAll => tr("Alle starten"),
            Command::StopAll => tr("Alle stoppen"),
            Command::Larger => tr("Größere Kacheln"),
            Command::Smaller => tr("Kleinere Kacheln"),
            Command::Fit => tr("Kacheln einpassen"),
            Command::Quit => tr("Manycraft beenden"),
        }
    }

    pub fn glyph(self) -> Icon {
        match self {
            Command::StartSelected | Command::StartAll | Command::NextInstance | Command::PreviousInstance => Icon::Play,
            Command::StopSelected | Command::StopAll => Icon::Stop,
            Command::RestartSelected | Command::ReconnectSelected => Icon::Refresh,
            Command::NextAttention | Command::ShowAttention => Icon::Alert,
            Command::ClearSelection | Command::Quit => Icon::Close,
            Command::SelectAll => Icon::Check,
            Command::FocusSearch => Icon::Search,
            Command::ShowAll => Icon::Grid,
            Command::ShowAccounts => Icon::Users,
            Command::OpenChat => Icon::Message,
            Command::OpenSettings => Icon::Settings,
            Command::Larger => Icon::Plus,
            Command::Smaller => Icon::Minus,
            Command::Fit => Icon::Fit,
            Command::ToggleSidebar => Icon::Sidebar,
            Command::Undo => Icon::Undo,
            Command::OpenPalette => Icon::Keyboard,
        }
    }

    pub fn shortcut(self) -> Option<Shortcut> {
        Some(match self {
            Command::OpenPalette => k(Code::KeyK),
            Command::ToggleSidebar => k(Code::KeyB),
            Command::Undo => k(Code::KeyZ),
            Command::FocusSearch => k(Code::KeyF),
            Command::ShowAll => ks(Code::KeyG),
            Command::ShowAccounts => ks(Code::KeyU),
            Command::OpenChat => ks(Code::KeyC),
            Command::OpenSettings => k(Code::Comma),
            Command::NextInstance => ca(Code::ArrowRight),
            Command::PreviousInstance => ca(Code::ArrowLeft),
            Command::NextAttention => ca(Code::ArrowDown),
            Command::SelectAll => k(Code::KeyA),
            Command::ClearSelection => bare(Code::Escape),
            Command::StartSelected => k(Code::KeyR),
            Command::StopSelected => k(Code::Period),
            Command::RestartSelected => ks(Code::KeyR),
            Command::ReconnectSelected => k(Code::KeyJ),
            Command::Larger => k(Code::Equal),
            Command::Smaller => k(Code::Minus),
            Command::Fit => k(Code::Digit0),
            Command::Quit => k(Code::KeyQ),
            Command::ShowAttention | Command::StartAll | Command::StopAll => return None,
        })
    }

    /// Whether the command can run now; the palette leaves out the others.
    pub fn available(self, store: Store) -> bool {
        let m = store.model.peek();
        let selected = store.ui.selection.peek();
        let picked = || selected.iter().filter_map(|k| m.games.get(k));
        let grid = matches!(*store.ui.page.peek(), Page::Grid(_));
        match self {
            Command::StartSelected => picked().any(|g| !g.phase.is_active()),
            Command::StopSelected => picked().any(|g| g.phase.is_active()),
            Command::RestartSelected => picked().any(|g| g.phase.has_process()),
            Command::ReconnectSelected => picked().any(|g| g.phase.is_running()),
            Command::ClearSelection => !selected.is_empty(),
            Command::StartAll => m.games.values().any(|g| !g.phase.is_active()),
            Command::StopAll => m.games.values().any(|g| g.phase.is_active()),
            Command::NextInstance | Command::PreviousInstance => m.games.values().filter(|g| g.phase.is_running()).count() > 0,
            Command::NextAttention | Command::ShowAttention => m.attention_count(None) > 0,
            Command::SelectAll | Command::Larger | Command::Smaller | Command::Fit => grid && !m.games.is_empty(),
            Command::FocusSearch => grid,
            Command::Undo => store.ui.toasts.peek().iter().any(|t| t.undo.is_some()),
            _ => true,
        }
    }

    /// Runs the command.
    pub fn run(self, store: Store) {
        let mut ui = store.ui;
        let keys = || store.ui.selection.peek().iter().cloned().collect::<Vec<String>>();
        match self {
            Command::OpenPalette => {
                let open = !*ui.palette.peek();
                ui.menu.set_if_modified(None);
                ui.palette.set(open);
            }
            Command::ToggleSidebar => {
                let shown = !*ui.sidebar.peek();
                ui.sidebar.set(shown);
            }
            Command::Undo => {
                toast::undo_last(ui);
            }
            Command::FocusSearch => {
                if !matches!(*ui.page.peek(), Page::Grid(_)) {
                    ui.page.set(Page::Grid(Filter::All));
                }
                ui.focus_search.set(Some(std::time::Instant::now()));
            }
            Command::ShowAll => ui.page.set(Page::Grid(Filter::All)),
            Command::ShowAttention => ui.page.set(Page::Grid(Filter::Attention)),
            Command::ShowAccounts => ui.page.set(Page::Accounts),
            Command::OpenChat => ui.page.set(Page::Chat),
            Command::OpenSettings => ui.page.set(Page::Settings),
            Command::NextInstance => store.dispatch(Event::Step(HotkeyAction::Next)),
            Command::PreviousInstance => store.dispatch(Event::Step(HotkeyAction::Prev)),
            Command::NextAttention => store.dispatch(Event::Step(HotkeyAction::NextAttention)),
            Command::SelectAll => {
                let all: BTreeSet<String> = super::grid::shown_keys(store).into_iter().collect();
                ui.selection.set(all);
            }
            Command::ClearSelection => ui.selection.set_if_modified(BTreeSet::new()),
            Command::StartSelected => store.dispatch(Event::StartMany(keys())),
            Command::StopSelected => store.dispatch(Event::Stop(keys())),
            Command::RestartSelected => store.dispatch(Event::Restart(keys())),
            Command::ReconnectSelected => store.dispatch(Event::ReconnectMany(keys())),
            Command::StartAll => store.dispatch(Event::StartAll),
            Command::StopAll => store.dispatch(Event::StopAll),
            Command::Larger => {
                let now = *ui.size.peek();
                ui.size.set(now.larger(*ui.tile_w.peek()));
            }
            Command::Smaller => {
                let now = *ui.size.peek();
                ui.size.set(now.smaller(*ui.tile_w.peek()));
            }
            Command::Fit => ui.size.set(TileSize::Fit),
            Command::Quit => store.dispatch(Event::Quit),
        }
    }
}

/// Runs the command whose shortcut the key event is; true when one ran. While a sheet, menu, the palette or a text field
/// has the keyboard only the palette toggle and Quit go through.
pub fn shortcut(store: Store, e: &KeyboardEventData) -> bool {
    let layered = store.ui.layered() || super::kit::field::typing();
    for command in Command::ALL {
        if command.shortcut().is_some_and(|s| s.matches(e)) {
            if layered && !matches!(command, Command::OpenPalette | Command::Quit) {
                return false;
            }
            if command == Command::ClearSelection && store.ui.selection.peek().is_empty() {
                return false;
            }
            if command.available(store) || matches!(command, Command::OpenPalette | Command::Quit | Command::ToggleSidebar) {
                command.run(store);
            }
            return true;
        }
    }
    false
}

/// The keys of the games picked, without those no longer there.
pub fn prune(store: Store, keys: &HashSet<String>) {
    let mut selection = store.ui.selection;
    if selection.peek().iter().any(|k| !keys.contains(k)) {
        selection.write().retain(|k| keys.contains(k));
    }
}
