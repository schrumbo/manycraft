//! Every command of the window: its name, its shortcut, when it is available and what it does. The palette, the
//! shortcuts and the drive tool all run commands from here, so a command works the same on every path.

use std::collections::{BTreeSet, HashSet};

use freya::prelude::*;

use super::grid::layout::TileSize;
use super::icons::Icon;
use super::kit::toast;
use super::store::Store;
use super::ui::{Filter, Page, Show, View};
use crate::app::event::Event;
use crate::app::model::Phase;
use crate::core::i18n::{tr, trf};
use crate::core::ipc::HotkeyAction;

/// What a command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenPalette,
    ToggleSidebar,
    Undo,
    FocusSearch,
    ShowStart,
    ShowLibrary,
    ShowDiscover,
    ShowWardrobe,
    GoBack,
    GoForward,
    LibraryAsGrid,
    LibraryAsList,
    ShowAll,
    ShowAttention,
    ShowAccounts,
    SwitchAccount,
    OpenChat,
    ShowScreen,
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
    ScreenshotSelected,
    AcceptPacks,
    DeclinePacks,
    AcceptRules,
    DeclineRules,
    StartAll,
    StopAll,
    MessageSelected,
    MoveSelected,
    LeaveSelected,
    ShowHistory,
    GroupByBatch,
    ShowUngrouped,
    ShowTiles,
    ShowTable,
    ShowRunning,
    ShowStopped,
    CancelStart,
    StartAnyway,
    ReconnectDropped,
    Larger,
    Smaller,
    Fit,
    Quit,
    AddAccount,
    SignInWaiting,
    RemoveAccounts,
    AccountsAsList,
    AccountsAsTable,
    ImportPrism,
    NewBatch,
    NewInstance,
    OpenTrash,
    Import,
    FindModpacks,
    AddMods,
    ShowShortcuts,
    Minimize,
    ToggleMaximize,
    QuickPlay,
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

const fn ka(code: Code) -> Shortcut {
    Shortcut { code, primary: true, shift: false, alt: true, ctrl: false }
}

const fn ca(code: Code) -> Shortcut {
    Shortcut { code, primary: false, shift: false, alt: true, ctrl: true }
}

const fn bare(code: Code) -> Shortcut {
    Shortcut { code, primary: false, shift: false, alt: false, ctrl: false }
}

impl Shortcut {
    /// Ctrl, or Cmd on macOS, with `code`.
    pub const fn primary(code: Code) -> Self {
        k(code)
    }

    /// Alt, or Option on macOS, with `code`.
    pub const fn alt(code: Code) -> Self {
        Shortcut { code, primary: false, shift: false, alt: true, ctrl: false }
    }

    /// `code` alone.
    pub const fn bare(code: Code) -> Self {
        bare(code)
    }

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
            Code::Delete => tr("Entf"),
            Code::Backspace => "⌫".to_owned(),
            Code::ArrowRight => "→".to_owned(),
            Code::ArrowLeft => "←".to_owned(),
            Code::ArrowDown => "↓".to_owned(),
            Code::F1 => "F1".to_owned(),
            Code::F2 => "F2".to_owned(),
            Code::Backslash => "\\".to_owned(),
            Code::BracketLeft => "[".to_owned(),
            Code::BracketRight => "]".to_owned(),
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
    pub const ALL: [Command; 68] = [
        Command::StartSelected,
        Command::StopSelected,
        Command::RestartSelected,
        Command::ReconnectSelected,
        Command::ScreenshotSelected,
        Command::AcceptPacks,
        Command::DeclinePacks,
        Command::AcceptRules,
        Command::DeclineRules,
        Command::MessageSelected,
        Command::MoveSelected,
        Command::LeaveSelected,
        Command::ShowHistory,
        Command::ClearSelection,
        Command::StartAll,
        Command::StopAll,
        Command::CancelStart,
        Command::StartAnyway,
        Command::ReconnectDropped,
        Command::ShowTiles,
        Command::ShowTable,
        Command::GroupByBatch,
        Command::ShowUngrouped,
        Command::ShowRunning,
        Command::ShowStopped,
        Command::NextInstance,
        Command::PreviousInstance,
        Command::NextAttention,
        Command::SelectAll,
        Command::FocusSearch,
        Command::ShowStart,
        Command::ShowLibrary,
        Command::ShowDiscover,
        Command::ShowWardrobe,
        Command::GoBack,
        Command::GoForward,
        Command::LibraryAsGrid,
        Command::LibraryAsList,
        Command::ShowAll,
        Command::ShowAttention,
        Command::ShowAccounts,
        Command::SwitchAccount,
        Command::OpenChat,
        Command::ShowScreen,
        Command::Larger,
        Command::Smaller,
        Command::Fit,
        Command::ToggleSidebar,
        Command::Undo,
        Command::OpenSettings,
        Command::OpenPalette,
        Command::Quit,
        Command::AddAccount,
        Command::SignInWaiting,
        Command::RemoveAccounts,
        Command::AccountsAsList,
        Command::AccountsAsTable,
        Command::ImportPrism,
        Command::NewBatch,
        Command::NewInstance,
        Command::OpenTrash,
        Command::Import,
        Command::FindModpacks,
        Command::AddMods,
        Command::ShowShortcuts,
        Command::QuickPlay,
        Command::Minimize,
        Command::ToggleMaximize,
    ];

    /// The name the drive tool and the GPUI shell use for it.
    pub fn name(self) -> &'static str {
        match self {
            Command::OpenPalette => "OpenPalette",
            Command::ToggleSidebar => "ToggleSidebar",
            Command::Undo => "Undo",
            Command::FocusSearch => "FocusSearch",
            Command::ShowStart => "ShowStart",
            Command::ShowLibrary => "ShowLibrary",
            Command::ShowDiscover => "ShowDiscover",
            Command::ShowWardrobe => "ShowWardrobe",
            Command::GoBack => "GoBack",
            Command::GoForward => "GoForward",
            Command::LibraryAsGrid => "LibraryAsGrid",
            Command::LibraryAsList => "LibraryAsList",
            Command::ShowAll => "ShowAll",
            Command::ShowAttention => "ShowAttention",
            Command::ShowAccounts => "ShowAccounts",
            Command::SwitchAccount => "SwitchAccount",
            Command::OpenChat => "OpenChat",
            Command::ShowScreen => "ShowScreen",
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
            Command::ScreenshotSelected => "ScreenshotSelected",
            Command::AcceptPacks => "AcceptPacks",
            Command::DeclinePacks => "DeclinePacks",
            Command::AcceptRules => "AcceptRules",
            Command::DeclineRules => "DeclineRules",
            Command::StartAll => "StartAll",
            Command::StopAll => "StopAll",
            Command::MessageSelected => "MessageSelected",
            Command::MoveSelected => "MoveSelected",
            Command::LeaveSelected => "LeaveSelected",
            Command::GroupByBatch => "GroupByBatch",
            Command::ShowUngrouped => "ShowUngrouped",
            Command::ShowHistory => "ShowHistory",
            Command::ShowTiles => "ShowTiles",
            Command::ShowTable => "ShowTable",
            Command::ShowRunning => "FilterRunning",
            Command::ShowStopped => "FilterStopped",
            Command::CancelStart => "CancelStart",
            Command::StartAnyway => "StartAnyway",
            Command::ReconnectDropped => "ReconnectDropped",
            Command::Larger => "Larger",
            Command::Smaller => "Smaller",
            Command::Fit => "FitTiles",
            Command::Quit => "Quit",
            Command::Import => "Import",
            Command::ShowShortcuts => "ShowShortcuts",
            Command::Minimize => "Minimize",
            Command::ToggleMaximize => "ToggleMaximize",
            Command::QuickPlay => "QuickPlay",
            c => c.page_name(),
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
            Command::ShowStart => tr("Start"),
            Command::ShowLibrary => tr("Instanzen"),
            Command::ShowDiscover => tr("Entdecken"),
            Command::ShowWardrobe => tr("Garderobe"),
            Command::GoBack => tr("Zurück"),
            Command::GoForward => tr("Vor"),
            Command::LibraryAsGrid => tr("Instanzen als Raster"),
            Command::LibraryAsList => tr("Instanzen als Liste"),
            Command::ShowAll => tr("Batches"),
            Command::ShowAttention => tr("Brauchen dich anzeigen"),
            Command::ShowAccounts => tr("Accounts"),
            Command::SwitchAccount => tr("Account wechseln…"),
            Command::OpenChat => tr("Chat"),
            Command::ShowScreen => tr("Bildschirm zeigen"),
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
            Command::ScreenshotSelected => tr("Screenshot der Ausgewählten"),
            Command::AcceptPacks => tr("Ressourcenpakete annehmen"),
            Command::DeclinePacks => tr("Ressourcenpakete ablehnen"),
            Command::AcceptRules => tr("Serverregeln annehmen"),
            Command::DeclineRules => tr("Serverregeln ablehnen"),
            Command::StartAll => tr("Alle starten"),
            Command::StopAll => tr("Alle stoppen"),
            Command::MessageSelected => tr("Nachricht an Ausgewählte…"),
            Command::MoveSelected => tr("In Batch verschieben…"),
            Command::LeaveSelected => tr("Aus Batch nehmen"),
            Command::GroupByBatch => tr("Nach Batch gruppieren"),
            Command::ShowUngrouped => tr("Ohne Gruppen zeigen"),
            Command::ShowHistory => tr("Verlauf zeigen"),
            Command::ShowTiles => tr("Als Kacheln zeigen"),
            Command::ShowTable => tr("Als Tabelle zeigen"),
            Command::ShowRunning => tr("Nur laufende zeigen"),
            Command::ShowStopped => tr("Nur gestoppte zeigen"),
            Command::CancelStart => tr("Starten abbrechen"),
            Command::StartAnyway => tr("Trotzdem alle starten"),
            Command::ReconnectDropped => tr("Getrennte jetzt verbinden"),
            Command::Larger => tr("Größere Kacheln"),
            Command::Smaller => tr("Kleinere Kacheln"),
            Command::Fit => tr("Kacheln einpassen"),
            Command::Quit => tr("Manycraft beenden"),
            Command::Import => tr("Instanz importieren…"),
            Command::ShowShortcuts => tr("Tastenkürzel anzeigen"),
            Command::Minimize => tr("Minimieren"),
            Command::ToggleMaximize if super::native::frame::chrome().maximized => tr("Wiederherstellen"),
            Command::ToggleMaximize => tr("Maximieren"),
            Command::QuickPlay => tr("Schnell spielen"),
            c => c.page_label(),
        }
    }

    pub fn glyph(self) -> Icon {
        match self {
            Command::StartSelected | Command::StartAll | Command::StartAnyway | Command::NextInstance | Command::PreviousInstance | Command::ShowRunning => Icon::Play,
            Command::StopSelected | Command::StopAll | Command::CancelStart | Command::ShowStopped => Icon::Stop,
            Command::RestartSelected | Command::ReconnectSelected | Command::ReconnectDropped => Icon::Refresh,
            Command::MessageSelected => Icon::Message,
            Command::ScreenshotSelected => Icon::Image,
            Command::AcceptPacks | Command::DeclinePacks | Command::AcceptRules | Command::DeclineRules => Icon::Package,
            Command::MoveSelected | Command::LeaveSelected | Command::GroupByBatch => Icon::Layers,
            Command::ShowUngrouped => Icon::Grid,
            Command::ShowHistory => Icon::Clock,
            Command::ShowTiles => Icon::Grid,
            Command::ShowTable => Icon::Rows,
            Command::NextAttention | Command::ShowAttention => Icon::Alert,
            Command::ClearSelection | Command::Quit => Icon::Close,
            Command::SelectAll => Icon::Check,
            Command::FocusSearch => Icon::Search,
            Command::ShowStart => Icon::Home,
            Command::ShowLibrary => Icon::Package,
            Command::ShowDiscover => Icon::Compass,
            Command::ShowWardrobe => Icon::Shirt,
            Command::GoBack => Icon::ChevronLeft,
            Command::GoForward => Icon::ChevronRight,
            Command::LibraryAsGrid => Icon::Grid,
            Command::LibraryAsList => Icon::Rows,
            Command::ShowAll => Icon::Layers,
            Command::ShowAccounts => Icon::Users,
            Command::SwitchAccount => Icon::ChevronsUpDown,
            Command::OpenChat => Icon::Message,
            Command::ShowScreen => Icon::Image,
            Command::OpenSettings => Icon::Settings,
            Command::Larger => Icon::Plus,
            Command::Smaller => Icon::Minus,
            Command::Fit => Icon::Fit,
            Command::ToggleSidebar => Icon::Sidebar,
            Command::Undo => Icon::Undo,
            Command::OpenPalette => Icon::Keyboard,
            Command::Import => Icon::Download,
            Command::ShowShortcuts => Icon::Keyboard,
            Command::Minimize => Icon::Minus,
            Command::ToggleMaximize if super::native::frame::chrome().maximized => Icon::Copy,
            Command::ToggleMaximize => Icon::Square,
            Command::QuickPlay => Icon::Play,
            c => c.page_glyph(),
        }
    }

    pub fn shortcut(self) -> Option<Shortcut> {
        Some(match self {
            Command::OpenPalette => k(Code::KeyK),
            Command::ToggleSidebar => k(Code::Backslash),
            Command::Undo => k(Code::KeyZ),
            Command::FocusSearch => k(Code::KeyF),
            Command::ShowStart => k(Code::Digit1),
            Command::ShowLibrary => k(Code::Digit2),
            Command::ShowDiscover => k(Code::Digit3),
            Command::ShowWardrobe => k(Code::Digit4),
            Command::GoBack => k(Code::BracketLeft),
            Command::GoForward => k(Code::BracketRight),
            Command::LibraryAsGrid => ka(Code::Digit1),
            Command::LibraryAsList => ka(Code::Digit2),
            Command::ShowAll => ks(Code::KeyG),
            Command::ShowAccounts => ks(Code::KeyU),
            Command::SwitchAccount => ks(Code::KeyA),
            Command::OpenChat => ks(Code::KeyC),
            Command::ShowScreen => k(Code::KeyB),
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
            Command::ScreenshotSelected => ks(Code::KeyS),
            Command::AcceptPacks => ks(Code::KeyY),
            Command::DeclinePacks => ka(Code::KeyY),
            Command::AcceptRules => ks(Code::KeyL),
            Command::DeclineRules => ka(Code::KeyL),
            Command::Larger => k(Code::Equal),
            Command::Smaller => k(Code::Minus),
            Command::Fit => k(Code::Digit0),
            Command::ShowTiles => ka(Code::Digit1),
            Command::ShowTable => ka(Code::Digit2),
            Command::GroupByBatch | Command::ShowUngrouped => ka(Code::Digit3),
            Command::MoveSelected => ks(Code::KeyB),
            Command::LeaveSelected => ka(Code::KeyB),
            Command::Quit => k(Code::KeyQ),
            Command::Import => k(Code::KeyO),
            Command::StartAll => ks(Code::Enter),
            Command::StopAll => ks(Code::Period),
            Command::ShowShortcuts => bare(Code::F1),
            Command::Minimize => k(Code::KeyM),
            Command::ToggleMaximize => ka(Code::KeyM),
            Command::QuickPlay => ks(Code::KeyP),
            Command::ShowAttention
            | Command::MessageSelected
            | Command::ShowHistory
            | Command::ShowRunning
            | Command::ShowStopped
            | Command::CancelStart
            | Command::StartAnyway
            | Command::ReconnectDropped => return None,
            c => return c.page_shortcut(),
        })
    }

    /// Whether the command can run now; the palette leaves out the others.
    pub fn available(self, store: Store) -> bool {
        let m = store.model.peek();
        let selected = store.ui.selection.peek();
        let picked = || selected.iter().filter_map(|k| m.games.get(k));
        let grid = matches!(*store.ui.page.peek(), Page::Grid(_));
        let accounts = *store.ui.page.peek() == Page::ACCOUNTS;
        match self {
            Command::SelectAll if accounts => !m.accounts.is_empty(),
            Command::ClearSelection if accounts => !store.ui.accounts.picked.peek().is_empty(),
            Command::FocusSearch if accounts => m.accounts.len() >= super::accounts::FILTER_FROM,
            Command::StartSelected => grid && picked().any(|g| !g.phase.is_active()),
            Command::StopSelected => grid && picked().any(|g| g.phase.is_active()),
            Command::RestartSelected => grid && picked().any(|g| g.phase.has_process()),
            Command::ReconnectSelected => grid && picked().any(|g| g.phase.reconnectable() && !g.stopping),
            Command::ScreenshotSelected => grid && picked().any(|g| g.headless && !g.connection && g.phase.server().is_some() && !g.stopping),
            Command::AcceptPacks if selected.is_empty() => m.games.values().any(|g| g.phase.needs_pack_answer()),
            Command::AcceptPacks | Command::DeclinePacks => grid && picked().any(|g| g.phase.needs_pack_answer()),
            Command::AcceptRules if selected.is_empty() => m.games.values().any(|g| g.phase.needs_rules_answer()),
            Command::AcceptRules | Command::DeclineRules => grid && picked().any(|g| g.phase.needs_rules_answer()),
            Command::ClearSelection => grid && !selected.is_empty(),
            Command::MessageSelected => grid && selected.len() >= 2 && m.able(&selected.iter().cloned().collect::<Vec<_>>()).message > 0,
            Command::MoveSelected => grid && m.batches.len() > 1 && picked().any(|g| m.batch(&g.batch_id).is_some()),
            Command::LeaveSelected => grid && picked().any(|g| m.batch(&g.batch_id).is_some()),
            Command::GroupByBatch | Command::ShowUngrouped => {
                *store.ui.page.peek() == Page::Grid(Filter::All) && *store.ui.view.peek() == View::Tiles && m.batches.len() > 1 && *store.ui.grid_view.grouped.peek() == (self == Command::ShowUngrouped)
            }
            Command::ShowHistory => grid && store.ui.cursor.peek().as_ref().or(selected.iter().next()).is_some_and(|k| m.games.contains_key(k)),
            Command::ShowTiles => grid && *store.ui.view.peek() != View::Tiles,
            Command::ShowTable => grid && *store.ui.view.peek() != View::Table,
            Command::ShowRunning => grid && !matches!(*store.ui.page.peek(), Page::Grid(Filter::Attention)) && *store.ui.show.peek() != Show::Running,
            Command::ShowStopped => grid && !matches!(*store.ui.page.peek(), Page::Grid(Filter::Attention)) && *store.ui.show.peek() != Show::Stopped,
            Command::CancelStart | Command::StartAnyway => m.run(None).is_some_and(|r| self == Command::CancelStart || r.memory > 0),
            Command::ReconnectDropped => m.batches.iter().any(|b| m.outage(&b.id).is_some()),
            Command::StartAll => m.games.values().any(|g| !g.phase.is_active() && !g.alone()),
            Command::StopAll => m.games.values().any(|g| g.phase.is_active() && !g.alone()),
            Command::NextInstance | Command::PreviousInstance => m.games.values().filter(|g| g.phase.is_running()).count() > 0,
            Command::NextAttention | Command::ShowAttention => m.attention_count(None) > 0,
            Command::SelectAll | Command::Larger | Command::Smaller | Command::Fit => grid && !m.games.is_empty(),
            Command::FocusSearch => grid || *store.ui.page.peek() == Page::Library,
            Command::GoBack => store.ui.hist.peek().can_back(),
            Command::GoForward => store.ui.hist.peek().can_forward(),
            Command::LibraryAsGrid => *store.ui.page.peek() == Page::Library && *store.ui.library.mode.peek() != super::library::Mode::Grid,
            Command::LibraryAsList => *store.ui.page.peek() == Page::Library && *store.ui.library.mode.peek() != super::library::Mode::List,
            Command::Undo => store.ui.browser_undo.peek().is_some() || store.ui.toasts.peek().iter().any(|t| t.undo.is_some()) || !store.ui.session_undo.peek().is_empty(),
            Command::SwitchAccount => store.model.peek().loaded,
            Command::Import => can_want(&Want::Import),
            Command::QuickPlay => !m.instances.is_empty(),
            c @ (Command::AddAccount | Command::SignInWaiting | Command::RemoveAccounts | Command::AccountsAsList | Command::AccountsAsTable | Command::ImportPrism | Command::NewBatch | Command::NewInstance | Command::OpenTrash | Command::FindModpacks | Command::AddMods) => c.page_available(store),
            Command::ShowScreen => super::sheets::screen::target(store).is_some(),
            _ => true,
        }
    }

    /// Runs the command.
    pub fn run(self, store: Store) {
        let mut ui = store.ui;
        let keys = || store.ui.selection.peek().iter().cloned().collect::<Vec<String>>();
        let accounts = *ui.page.peek() == Page::ACCOUNTS;
        match self {
            Command::SelectAll if accounts => super::accounts::pick_all(store),
            Command::ClearSelection if accounts => {
                super::accounts::clear(store);
            }
            Command::FocusSearch if accounts => super::accounts::focus_filter(store),
            Command::OpenPalette => {
                let open = !*ui.palette.peek();
                ui.menu.set_if_modified(None);
                ui.palette.set(open);
            }
            Command::ToggleSidebar => ui.toggle_sidebar(),
            Command::ShowShortcuts => super::shell::shortcuts::open(ui),
            Command::Minimize => super::native::frame::minimize(),
            Command::ToggleMaximize => super::native::frame::toggle_maximize(),
            Command::QuickPlay => {
                ui.page.set(Page::Start);
                ui.focus_quick.set(Some(std::time::Instant::now()));
            }
            Command::Undo => match latest_undo(store) {
                Some(Latest::Browser) => {
                    let browser = ui.browser_undo.peek().clone();
                    if let Some(undo) = browser {
                        undo.run.call(());
                    }
                }
                Some(Latest::Toast) => {
                    toast::undo_last(ui);
                }
                Some(Latest::Session) => {
                    let newest = ui.session_undo.peek().first().cloned();
                    if let Some(undo) = newest {
                        undo.run.call(());
                    }
                }
                None => {}
            },
            Command::FocusSearch => {
                if !matches!(*ui.page.peek(), Page::Grid(_) | Page::Library) {
                    ui.page.set(Page::Library);
                }
                ui.focus_search.set(Some(std::time::Instant::now()));
            }
            Command::ShowStart => ui.page.set(Page::Start),
            Command::ShowLibrary => ui.page.set(Page::Library),
            Command::ShowDiscover => ui.page.set(Page::Discover),
            Command::ShowWardrobe => ui.page.set(Page::Wardrobe),
            Command::GoBack => super::shell::history::back(store),
            Command::GoForward => super::shell::history::forward(store),
            Command::LibraryAsGrid => ui.library.mode.set(super::library::Mode::Grid),
            Command::LibraryAsList => ui.library.mode.set(super::library::Mode::List),
            Command::ShowAll => ui.page.set(Page::Grid(Filter::All)),
            Command::ShowAttention => ui.page.set(Page::Grid(Filter::Attention)),
            Command::ShowAccounts => ui.page.set(Page::ACCOUNTS),
            Command::SwitchAccount => super::shell::account::open_from_keys(store),
            Command::OpenChat => super::chat::focus(store),
            Command::ShowScreen => {
                if let Some(key) = super::sheets::screen::target(store) {
                    super::sheets::screen::open(store, &key);
                }
            }
            Command::OpenSettings => ui.page.set(Page::Settings(Default::default())),
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
            Command::ScreenshotSelected => store.dispatch(Event::Capture(keys())),
            Command::AcceptPacks | Command::DeclinePacks => {
                let accept = self == Command::AcceptPacks;
                store.dispatch(Event::AnswerPack { keys: offers(store, accept), accept });
            }
            Command::AcceptRules | Command::DeclineRules => {
                let accept = self == Command::AcceptRules;
                store.dispatch(Event::AnswerRules { keys: rules(store), accept });
            }
            Command::StartAll => store.dispatch(Event::StartAll),
            Command::StopAll => store.dispatch(Event::StopAll),
            Command::MessageSelected => ui.message.set(true),
            Command::MoveSelected => super::grid::ask_move(store, keys(), None),
            Command::LeaveSelected => super::grid::leave(store, keys()),
            Command::GroupByBatch | Command::ShowUngrouped => {
                let mut shared = ui.grid_view;
                shared.grouped.set(self == Command::GroupByBatch);
            }
            Command::ShowHistory => {
                let key = ui.cursor.peek().clone().or_else(|| ui.selection.peek().iter().next().cloned());
                ui.history.set(key);
            }
            Command::ShowTiles => ui.view.set(View::Tiles),
            Command::ShowTable => ui.view.set(View::Table),
            Command::ShowRunning => ui.show.set(Show::Running),
            Command::ShowStopped => ui.show.set(Show::Stopped),
            Command::CancelStart => store.dispatch(Event::CancelStart(None)),
            Command::StartAnyway => store.dispatch(Event::StartAnyway(None)),
            Command::ReconnectDropped => {
                let keys: Vec<String> = {
                    let m = store.model.peek();
                    m.batches.iter().filter_map(|b| m.outage(&b.id)).flat_map(|o| o.keys).collect()
                };
                store.dispatch(Event::ReconnectMany(keys));
            }
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
            Command::Import => {
                want(store, Want::Import);
            }
            c => c.page_run(store),
        }
    }
}

fn offers(store: Store, accept: bool) -> Vec<String> {
    let m = store.model.peek();
    let held = |k: &String| m.games.get(k).is_some_and(|g| g.phase.needs_pack_answer());
    let selection = store.ui.selection.peek();
    if selection.is_empty() && accept { m.games.keys().filter(|k| held(k)).cloned().collect() } else { selection.iter().filter(|k| held(k)).cloned().collect() }
}

fn rules(store: Store) -> Vec<String> {
    let m = store.model.peek();
    let held = |k: &String| m.games.get(k).is_some_and(|g| g.phase.needs_rules_answer());
    store.ui.selection.peek().iter().filter(|k| held(k)).cloned().collect()
}

/// Whether `command` still runs while a sheet, menu, the palette or a text field has the keyboard: Quit, and the palette
/// toggle unless a sheet is open, since the palette reaches pages the sheet covers.
fn passes_layers(store: Store, command: Command) -> bool {
    match command {
        Command::Quit => true,
        Command::OpenPalette => store.ui.sheet.peek().is_none(),
        _ => false,
    }
}

/// Runs the command whose shortcut the key event is; true when one ran. While a sheet, menu, the palette or a text field
/// has the keyboard only the palette toggle and Quit go through, and the palette toggle not over a sheet. A command that
/// cannot run leaves its keys to the page.
pub fn shortcut(store: Store, e: &KeyboardEventData) -> bool {
    let layered = store.ui.layered() || super::kit::field::typing();
    for command in Command::ALL {
        if command.shortcut().is_some_and(|s| s.matches(e)) {
            if layered && !passes_layers(store, command) {
                return false;
            }
            if command.available(store) || matches!(command, Command::OpenPalette | Command::Quit | Command::ToggleSidebar) {
                command.run(store);
                return true;
            }
        }
    }
    if layered {
        return false;
    }
    if let Some(extra) = extras(store).into_iter().find(|x| x.shortcut.is_some_and(|s| s.matches(e))) {
        extra.run.call(());
        return true;
    }
    false
}

/// The command a menu item for `command` runs now: the one its shortcut runs on the shown page, else `command` itself
/// while it can run; while a sheet, menu or the palette is open only the palette toggle and Quit.
#[cfg(target_os = "macos")]
pub fn for_menu(store: Store, command: Command) -> Option<Command> {
    let free = |c: Command| passes_layers(store, c);
    if store.ui.layered() && !free(command) {
        return None;
    }
    let ready = |c: Command| c.available(store) || free(c) || c == Command::ToggleSidebar;
    let keys = command.shortcut();
    keys.and_then(|keys| Command::ALL.into_iter().find(|c| c.shortcut() == Some(keys) && ready(*c))).or_else(|| ready(command).then_some(command))
}

thread_local! {
    static UNDO_NAMES: std::cell::RefCell<Vec<(u64, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Names what the Rückgängig of toast `id` takes back, such as „184 stoppen“, for the palette and the menu bar.
pub fn name_undo(store: Store, id: u64, what: String) {
    let live: HashSet<u64> = store.ui.toasts.peek().iter().map(|t| t.id).collect();
    UNDO_NAMES.with_borrow_mut(|names| {
        names.retain(|(n, _)| live.contains(n));
        names.push((id, what));
    });
}

/// Where Rückgängig takes the newest change back from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Latest {
    Browser,
    Toast,
    Session,
}

/// The source holding the newest change, given the stamps of the open browser's top removal, the newest undoable toast and
/// the newest kept deletion; every undo, however it is offered, goes in the order it happened.
fn newest_of(browser: Option<u64>, toast: Option<u64>, session: Option<u64>) -> Option<Latest> {
    [(browser, Latest::Browser), (toast, Latest::Toast), (session, Latest::Session)].into_iter().filter_map(|(at, from)| at.map(|at| (at, from))).max_by_key(|(at, _)| *at).map(|(_, from)| from)
}

fn latest_undo(store: Store) -> Option<Latest> {
    newest_of(store.ui.browser_undo.peek().as_ref().map(|u| u.at), toast::newest_undo(&store.ui.toasts.peek()), store.ui.session_undo.peek().first().map(|u| u.at))
}

/// What the palette calls Rückgängig: what the newest change takes back, the same one Strg+Z runs; a toast without a
/// name leaves the plain word.
pub fn undo_label(store: Store) -> Option<String> {
    match latest_undo(store)? {
        Latest::Browser => store.ui.browser_undo.peek().as_ref().map(|u| u.label.clone()),
        Latest::Toast => {
            let id = toast::newest_undo(&store.ui.toasts.peek())?;
            UNDO_NAMES.with_borrow(|names| names.iter().find(|(n, _)| *n == id).map(|(_, what)| trf("Rückgängig: %@", &[what])))
        }
        Latest::Session => store.ui.session_undo.peek().first().map(|u| u.label.clone()),
    }
}

/// A command the shown page offers beyond the window's: in the palette, and through its shortcut while the page is shown.
#[derive(Clone, PartialEq)]
pub struct Extra {
    pub id: String,
    pub label: String,
    pub glyph: Icon,
    pub shortcut: Option<Shortcut>,
    pub run: EventHandler<()>,
}

/// What the shown page offers beyond the window's commands.
pub fn extras(store: Store) -> Vec<Extra> {
    match &*store.ui.page.peek() {
        Page::Instance(_) => super::instance::extras(store),
        Page::Grid(_) => super::chat::extras(store),
        Page::Discover => super::discover::extras(store),
        _ => Vec::new(),
    }
}

/// The keys of the games picked, without those no longer there.
pub fn prune(store: Store, keys: &HashSet<String>) {
    let mut selection = store.ui.selection;
    if selection.peek().iter().any(|k| !keys.contains(k)) {
        selection.write().retain(|k| keys.contains(k));
    }
}

/// A sheet or flow another part of the window asks for; the sheets answer it once they are registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    AddAccount,
    AddMicrosoft,
    /// Signs these Microsoft accounts in again, one after another.
    SignInAgain(Vec<uuid::Uuid>),
    /// Signs in the Microsoft accounts of these names, one after another, as Prism listed them.
    SignInNames(Vec<String>),
    ChangeSkin(uuid::Uuid),
    OpenTrash,
    NewInstance,
    NewBatch,
    /// A new batch playing this instance.
    NewBatchFor(String),
    EditBatch(String),
    DeleteBatch(String),
    RenameInstance(String),
    DeleteInstance(String),
    ExportBatch(String),
    ExportInstance(String),
    CloneInstance(String),
    AddMods(String),
    EditMods(String),
    UpdateLoader(String),
    ResetInstance(String),
    CreateShortcut(String),
    FetchUpdate,
    CancelUpdate,
    RestartToUpdate,
    DismissNews,
    Import,
    FindModpacks,
    /// Imports the pack or Prism export at this path at once.
    ImportFile(std::path::PathBuf),
}

/// Who answers wants: whether it takes one, and what it does with it.
#[derive(Clone, Copy)]
pub struct Answer {
    pub takes: fn(&Want) -> bool,
    pub run: fn(Store, Want),
}

thread_local! {
    static ANSWER: std::cell::Cell<Option<Answer>> = const { std::cell::Cell::new(None) };
}

/// Registers who answers wants; the last one registered wins.
pub fn answer_wants(answer: Answer) {
    ANSWER.with(|a| a.set(Some(answer)));
}

/// Whether `want` would be answered now.
pub fn can_want(want: &Want) -> bool {
    ANSWER.with(std::cell::Cell::get).is_some_and(|a| (a.takes)(want))
}

/// Asks for `want`; false when nobody takes it.
pub fn want(store: Store, want: Want) -> bool {
    match ANSWER.with(std::cell::Cell::get) {
        Some(a) if (a.takes)(&want) => {
            (a.run)(store, want);
            true
        }
        _ => false,
    }
}

/// Why signing in cannot start now: nobody takes it, or another sheet is open.
pub fn sign_in_blocked(store: Store) -> Option<String> {
    (!can_want(&Want::SignInAgain(Vec::new())) || store.ui.sheet.peek().is_some()).then(|| tr("Anmeldung gerade nicht möglich"))
}

/// The reason a control that asks for `want` is off, while nobody takes it.
pub fn want_blocked(want: &Want) -> Option<String> {
    (!can_want(want)).then(|| tr("Zieht noch in die neue Oberfläche um."))
}

impl Command {
    fn page_name(self) -> &'static str {
        match self {
            Command::AddAccount => "AddAccount",
            Command::SignInWaiting => "SignInWaiting",
            Command::RemoveAccounts => "RemoveAccounts",
            Command::AccountsAsList => "AccountsAsList",
            Command::AccountsAsTable => "AccountsAsTable",
            Command::ImportPrism => "ImportPrismAccounts",
            Command::NewBatch => "NewBatch",
            Command::NewInstance => "NewInstance",
            Command::OpenTrash => "OpenTrash",
            Command::FindModpacks => "FindModpacks",
            Command::AddMods => "AddMods",
            _ => "",
        }
    }

    fn page_label(self) -> String {
        match self {
            Command::AddAccount => tr("Account hinzufügen"),
            Command::SignInWaiting => tr("Alle nacheinander anmelden"),
            Command::RemoveAccounts => tr("Ausgewählte Accounts entfernen…"),
            Command::AccountsAsList => tr("Accounts als Liste"),
            Command::AccountsAsTable => tr("Accounts als Tabelle"),
            Command::ImportPrism => tr("Accounts aus Prism übernehmen"),
            Command::NewBatch => tr("Neuer Batch"),
            Command::NewInstance => tr("Neue Instanz"),
            Command::OpenTrash => tr("Zuletzt gelöschte Instanzen"),
            Command::FindModpacks => tr("Modpacks suchen…"),
            Command::AddMods => tr("Inhalte hinzufügen…"),
            _ => String::new(),
        }
    }

    fn page_glyph(self) -> Icon {
        match self {
            Command::AddAccount => Icon::UserPlus,
            Command::SignInWaiting => Icon::LogIn,
            Command::RemoveAccounts | Command::OpenTrash => Icon::Trash,
            Command::AccountsAsList => Icon::Rows,
            Command::AccountsAsTable => Icon::Grid,
            Command::ImportPrism => Icon::Download,
            Command::NewBatch => Icon::Layers,
            Command::NewInstance => Icon::Plus,
            Command::FindModpacks => Icon::Package,
            Command::AddMods => Icon::Plus,
            _ => Icon::Help,
        }
    }

    fn page_shortcut(self) -> Option<Shortcut> {
        Some(match self {
            Command::AddAccount => k(Code::KeyN),
            Command::NewInstance => ks(Code::KeyN),
            Command::RemoveAccounts => bare(if cfg!(target_os = "macos") { Code::Backspace } else { Code::Delete }),
            Command::AccountsAsList => ka(Code::Digit1),
            Command::AccountsAsTable => ka(Code::Digit2),
            Command::AddMods => ks(Code::KeyM),
            Command::NewBatch => ka(Code::KeyN),
            Command::FindModpacks => ks(Code::KeyO),
            Command::OpenTrash => ks(Code::KeyT),
            _ => return None,
        })
    }

    fn page_available(self, store: Store) -> bool {
        let accounts = *store.ui.page.peek() == Page::ACCOUNTS;
        let view = || super::accounts::view_of(&store.model.peek(), store.ui.accounts);
        let any = || !store.model.peek().accounts.is_empty();
        match self {
            Command::AddAccount => can_want(&Want::AddAccount),
            Command::SignInWaiting => !super::accounts::waiting(&store.model.peek()).is_empty() && can_want(&Want::SignInAgain(Vec::new())),
            Command::RemoveAccounts => accounts && !super::accounts::targets(store).is_empty(),
            Command::AccountsAsList => accounts && any() && view() != super::accounts::View::List,
            Command::AccountsAsTable => accounts && any() && view() != super::accounts::View::Table,
            Command::ImportPrism => {
                let prism = store.ui.accounts.prism.peek();
                prism.found > 0 && !prism.busy
            }
            Command::NewBatch => can_want(&Want::NewBatch),
            Command::NewInstance => can_want(&Want::NewInstance),
            Command::OpenTrash => can_want(&Want::OpenTrash),
            Command::FindModpacks => can_want(&Want::FindModpacks),
            Command::AddMods => can_want(&Want::AddMods(String::new())) && super::instance::current(store).is_some_and(|id| super::instance::held(store, &id).is_none()),
            _ => true,
        }
    }

    fn page_run(self, store: Store) {
        let mut shared = store.ui.accounts;
        match self {
            Command::AddAccount => {
                want(store, Want::AddAccount);
            }
            Command::SignInWaiting => {
                let ids = super::accounts::waiting(&store.model.peek());
                super::accounts::sign_in(store, ids);
            }
            Command::RemoveAccounts => super::accounts::ask_remove(store, super::accounts::targets(store)),
            Command::AccountsAsList => shared.view.set(Some(super::accounts::View::List)),
            Command::AccountsAsTable => shared.view.set(Some(super::accounts::View::Table)),
            Command::ImportPrism => {
                let mut ui = store.ui;
                ui.page.set(Page::ACCOUNTS);
                super::accounts::import_prism(store);
            }
            Command::NewBatch => {
                want(store, Want::NewBatch);
            }
            Command::NewInstance => {
                want(store, Want::NewInstance);
            }
            Command::OpenTrash => {
                want(store, Want::OpenTrash);
            }
            Command::FindModpacks => {
                want(store, Want::FindModpacks);
            }
            Command::AddMods => {
                if let Some(id) = super::instance::current(store) {
                    want(store, Want::AddMods(id));
                }
            }
            _ => {}
        }
    }

    /// The shortcut as the platform writes it, or nothing.
    pub fn keys(self) -> String {
        self.shortcut().map(|s| s.label()).unwrap_or_default()
    }

    /// The label with the shortcut in brackets, for tooltips.
    pub fn tip(self) -> String {
        match self.shortcut() {
            Some(s) => format!("{} ({})", self.label(), s.label()),
            None => self.label(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_commands_share_a_chord_unless_their_pages_differ() {
        let paged = [
            (Command::ShowTiles, Command::AccountsAsList),
            (Command::ShowTable, Command::AccountsAsTable),
            (Command::ShowTiles, Command::LibraryAsGrid),
            (Command::ShowTable, Command::LibraryAsList),
            (Command::AccountsAsList, Command::LibraryAsGrid),
            (Command::AccountsAsTable, Command::LibraryAsList),
            (Command::GroupByBatch, Command::ShowUngrouped),
        ];
        for (i, a) in Command::ALL.into_iter().enumerate() {
            for b in Command::ALL.into_iter().skip(i + 1) {
                if a.shortcut().is_some() && a.shortcut() == b.shortcut() {
                    assert!(paged.contains(&(a, b)) || paged.contains(&(b, a)), "{} and {} share a chord", a.name(), b.name());
                }
            }
        }
    }

    #[test]
    fn undo_goes_to_the_newest_change_whichever_way_it_is_offered() {
        assert_eq!(newest_of(None, None, None), None);
        assert_eq!(newest_of(None, Some(4), Some(2)), Some(Latest::Toast));
        assert_eq!(newest_of(None, Some(2), Some(4)), Some(Latest::Session));
        assert_eq!(newest_of(Some(9), Some(4), Some(2)), Some(Latest::Browser));
        assert_eq!(newest_of(Some(1), Some(4), None), Some(Latest::Toast));
        assert_eq!(newest_of(Some(1), None, Some(3)), Some(Latest::Session));
    }

    #[test]
    fn the_window_wide_commands_of_the_shell_have_keys() {
        for c in [Command::StartAll, Command::StopAll, Command::NewBatch, Command::OpenTrash, Command::FindModpacks] {
            assert!(c.shortcut().is_some(), "{} has no key", c.name());
        }
    }
}
