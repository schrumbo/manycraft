//! What only the view holds: the page shown, the tile size, the selection, and the layers above the content.

use std::collections::BTreeSet;
use std::time::Instant;

use freya::prelude::*;

use super::accounts;
use super::chat;
use super::grid::layout::TileSize;
use super::grid::table::Column;
use super::instance::Nav;
use super::kit::Bounds;
use super::kit::menu::Menu;
use super::kit::sheet::Sheet;
use super::kit::toast::Toast;
use super::kit::tooltip::Tip;
use super::theme::frame;
use crate::app::model::Need;
use crate::core::winstate;

/// The pages the sidebar leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// The grid of every game, or of one batch, or of those that need the person.
    Grid(Filter),
    Instance(String),
    /// An instance still on its way in, by install id.
    Installing(u64),
    /// Modpacks from Modrinth, each installable as a new instance.
    Modpacks,
    Accounts,
    Chat,
    Settings,
    /// What was deleted in the last seven days.
    Trash,
}

/// Which games the grid shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Attention,
    Batch(String),
}

/// Which games of the page the grid shows: all, those that run, those that are stopped, or one kind that needs the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    All,
    Running,
    Stopped,
    Need(Need),
}

/// How the grid page lays out its games.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Tiles,
    Table,
}

/// What the open browser would undo next: how the palette names it, and what it does.
#[derive(Clone, PartialEq)]
pub struct BrowserUndo {
    pub label: String,
    pub run: EventHandler<()>,
}

/// A deletion that can be taken back for as long as the window is open: what it undoes and how.
#[derive(Clone, PartialEq)]
pub struct SessionUndo {
    pub key: String,
    pub label: String,
    pub run: EventHandler<()>,
}

/// Puts `entry` first in `list`, in place of an older one with its key.
pub fn remember(list: &mut Vec<SessionUndo>, entry: SessionUndo) {
    list.retain(|u| u.key != entry.key);
    list.insert(0, entry);
}

/// The view state every component reaches through the context.
#[derive(Clone, Copy, PartialEq)]
pub struct Ui {
    /// The window's content size in logical pixels.
    pub window: State<(f32, f32)>,
    pub page: State<Page>,
    /// Whether the sidebar takes its column: what the user chose, unless the window is too narrow for it.
    pub sidebar: State<bool>,
    /// Whether the user wants the sidebar, remembered while a narrow window hides it.
    pub sidebar_wanted: State<bool>,
    /// Whether a narrow window shows the sidebar above the page.
    pub drawer: State<bool>,
    pub size: State<TileSize>,
    pub show: State<Show>,
    pub view: State<View>,
    /// The table's sort column, and whether it runs downward.
    pub sort: State<(Column, bool)>,
    /// The game whose history is open.
    pub history: State<Option<String>>,
    /// Whether the selection bar's message field is open.
    pub message: State<bool>,
    /// How many games a row of the grid holds, and how many rows one page shows.
    pub grid: State<(usize, usize)>,
    /// The tile width the grid came to, so a size step goes from what is on screen.
    pub tile_w: State<f32>,
    pub search: State<String>,
    /// Set when the search field should take the keyboard.
    pub focus_search: State<Option<Instant>>,
    /// The games picked in the grid, by key.
    pub selection: State<BTreeSet<String>>,
    /// The game the keyboard is on, and where Shift extends a range from.
    pub cursor: State<Option<String>>,
    pub palette: State<bool>,
    pub sheet: State<Option<Sheet>>,
    pub menu: State<Option<Menu>>,
    /// Where the sidebar shows the active account, for the switcher opened from the keyboard.
    pub account_at: State<Option<Bounds>>,
    pub toasts: State<Vec<Toast>>,
    /// How far toasts keep above the window's bottom edge, for a bar pinned there such as the chat composer.
    pub toast_lift: State<f32>,
    pub tip: State<Option<Tip>>,
    /// The removal the open content browser would undo next; the palette and Ctrl or Cmd+Z reach it from here.
    pub browser_undo: State<Option<BrowserUndo>>,
    /// Whether version pickers also offer snapshots, pre-releases and old versions; remembered in the settings.
    pub all_versions: State<bool>,
    /// The deletions still to undo, newest first; the palette and Ctrl or Cmd+Z reach them after their toasts are gone.
    pub session_undo: State<Vec<SessionUndo>>,
    /// When the last tooltip went away, so the next one shows at once.
    pub tip_seen: State<Option<Instant>>,
    /// Instances on their way in, oldest first.
    pub installs: State<Vec<crate::view::install::Install>>,
    /// Whether the install runner is working through the queue.
    pub installing: State<bool>,
    /// What the Accounts page shows and has picked.
    pub accounts: accounts::Shared,
    /// What the Chat page shows and is about to send.
    pub chat: chat::Shared,
    /// How the grid groups its games, and the last mass stop to undo.
    pub grid_view: crate::view::grid::mass::Shared,
    /// What the instance page keeps while it is not shown.
    pub instance: Nav,
}

impl Ui {
    /// View state for a window of `size`, on the page and with the sidebar it had when it last closed.
    pub fn create(size: (f32, f32)) -> Self {
        let saved = winstate::load();
        let page = restored(saved.page, saved.batch);
        Self {
            window: State::create(size),
            page: State::create(page),
            sidebar: State::create(saved.sidebar && size.0 >= frame::SIDEBAR_FOLD),
            sidebar_wanted: State::create(saved.sidebar),
            drawer: State::create(false),
            size: State::create(TileSize::Fit),
            show: State::create(Show::All),
            view: State::create(View::Tiles),
            sort: State::create((Column::Order, false)),
            history: State::create(None),
            message: State::create(false),
            grid: State::create((1, 1)),
            tile_w: State::create(0.),
            search: State::create(String::new()),
            focus_search: State::create(None),
            selection: State::create(BTreeSet::new()),
            cursor: State::create(None),
            palette: State::create(false),
            sheet: State::create(None),
            menu: State::create(None),
            account_at: State::create(None),
            toasts: State::create(Vec::new()),
            toast_lift: State::create(0.),
            tip: State::create(None),
            browser_undo: State::create(None),
            all_versions: State::create(crate::core::settings::loaded().all_versions),
            session_undo: State::create(Vec::new()),
            tip_seen: State::create(None),
            installs: State::create(Vec::new()),
            installing: State::create(false),
            accounts: accounts::Shared::create(),
            chat: chat::Shared::create(),
            grid_view: crate::view::grid::mass::Shared::create(),
            instance: Nav::create(),
        }
    }

    /// Whether the window is too narrow for the sidebar's column.
    pub fn narrow(&self) -> bool {
        self.window.peek().0 < frame::SIDEBAR_FOLD
    }

    /// Sets the window's content size and gives the sidebar its column only where it fits.
    pub fn resize(mut self, size: (f32, f32)) {
        self.window.set_if_modified(size);
        let narrow = self.narrow();
        self.sidebar.set_if_modified(!narrow && *self.sidebar_wanted.peek());
        if !narrow {
            self.drawer.set_if_modified(false);
        }
    }

    /// Shows or hides the sidebar: its column in a wide window, above the page in a narrow one.
    pub fn toggle_sidebar(mut self) {
        if self.narrow() {
            let open = !*self.drawer.peek();
            self.drawer.set(open);
        } else {
            let shown = !*self.sidebar.peek();
            self.sidebar_wanted.set(shown);
            self.sidebar.set(shown);
        }
    }

    /// Whether a layer above the content takes the keyboard: a sheet, the palette or a menu.
    pub fn layered(&self) -> bool {
        self.sheet.peek().is_some() || *self.palette.peek() || self.menu.peek().is_some()
    }
}

/// The page `window.json` keeps for `page`, with its batch or instance; None for a page it does not keep.
pub fn kept(page: &Page) -> Option<(winstate::Page, Option<String>)> {
    match page {
        Page::Grid(Filter::Batch(b)) => Some((winstate::Page::Grid, Some(b.clone()))),
        Page::Grid(_) => Some((winstate::Page::Grid, None)),
        Page::Instance(id) => Some((winstate::Page::Instance, Some(id.clone()))),
        Page::Accounts => Some((winstate::Page::Accounts, None)),
        Page::Chat => Some((winstate::Page::Chat, None)),
        Page::Settings | Page::Installing(_) | Page::Modpacks | Page::Trash => None,
    }
}

fn restored(page: winstate::Page, batch: Option<String>) -> Page {
    match (page, batch) {
        (winstate::Page::Grid, Some(b)) => Page::Grid(Filter::Batch(b)),
        (winstate::Page::Instance, Some(id)) => Page::Instance(id),
        (winstate::Page::Accounts, _) => Page::Accounts,
        (winstate::Page::Chat, _) => Page::Chat,
        _ => Page::Grid(Filter::All),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kept_page_comes_back_after_a_restart() {
        for page in [Page::Grid(Filter::All), Page::Grid(Filter::Batch("b".into())), Page::Instance("i".into()), Page::Accounts, Page::Chat] {
            let (kind, batch) = kept(&page).unwrap();
            assert_eq!(restored(kind, batch), page);
        }
        assert_eq!(kept(&Page::Settings), None);
    }

    fn entry(key: &str, label: &str) -> SessionUndo {
        SessionUndo { key: key.into(), label: label.into(), run: EventHandler::new(|_| {}) }
    }

    #[test]
    fn the_newest_undo_comes_first_and_a_repeat_replaces_the_old_one() {
        let mut list = Vec::new();
        remember(&mut list, entry("a", "A"));
        remember(&mut list, entry("b", "B"));
        remember(&mut list, entry("a", "A again"));
        let labels: Vec<&str> = list.iter().map(|u| u.label.as_str()).collect();
        assert_eq!(labels, ["A again", "B"]);
    }
}
