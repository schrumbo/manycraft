//! What only the view holds: the page shown, the tile size, the selection, and the layers above the content.

use std::collections::BTreeSet;
use std::time::Instant;

use freya::prelude::*;

use super::accounts;
use super::chat;
use super::grid::layout::TileSize;
use super::grid::table::Column;
use super::instance::Nav;
use super::kit::menu::Menu;
use super::kit::sheet::Sheet;
use super::kit::toast::Toast;
use super::kit::tooltip::Tip;
use crate::app::model::Need;

/// The pages the sidebar leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// The grid of every game, or of one batch, or of those that need the person.
    Grid(Filter),
    Instance(String),
    Accounts,
    Chat,
    Settings,
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

/// The view state every component reaches through the context.
#[derive(Clone, Copy, PartialEq)]
pub struct Ui {
    /// The window's content size in logical pixels.
    pub window: State<(f32, f32)>,
    pub page: State<Page>,
    pub sidebar: State<bool>,
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
    pub toasts: State<Vec<Toast>>,
    pub tip: State<Option<Tip>>,
    /// When the last tooltip went away, so the next one shows at once.
    pub tip_seen: State<Option<Instant>>,
    /// What the Accounts page shows and has picked.
    pub accounts: accounts::Shared,
    /// What the Chat page shows and is about to send.
    pub chat: chat::Shared,
    /// What the instance page keeps while it is not shown.
    pub instance: Nav,
}

impl Ui {
    /// Fresh view state for a window of `size`.
    pub fn create(size: (f32, f32)) -> Self {
        Self {
            window: State::create(size),
            page: State::create(Page::Grid(Filter::All)),
            sidebar: State::create(true),
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
            toasts: State::create(Vec::new()),
            tip: State::create(None),
            tip_seen: State::create(None),
            accounts: accounts::Shared::create(),
            chat: chat::Shared::create(),
            instance: Nav::create(),
        }
    }

    /// Whether a layer above the content takes the keyboard: a sheet, the palette or a menu.
    pub fn layered(&self) -> bool {
        self.sheet.peek().is_some() || *self.palette.peek() || self.menu.peek().is_some()
    }
}
