//! The Wardrobe page: every skin and cape, to switch between and to add to.

mod card;
mod ctl;
mod library;
mod rename;
mod stage;
mod text;

use freya::prelude::*;
use uuid::Uuid;

use crate::view::kit::{page, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::frame;
use crate::view::ui::Page;
use ctl::{Ctl, Phase};

const STAGE: f32 = 340.;

/// The page's state that outlives a visit: the account the Wardrobe dresses.
#[derive(Clone, Copy, PartialEq)]
pub struct Shared {
    pub chosen: State<Option<Uuid>>,
}

impl Shared {
    pub fn create() -> Self {
        Self { chosen: State::create(None) }
    }
}

/// Opens the Wardrobe for `account`.
pub fn show(store: Store, account: Uuid) {
    let mut chosen = store.ui.wardrobe.chosen;
    chosen.set(Some(account));
    let mut page = store.ui.page;
    page.set(Page::Wardrobe);
}

/// The Wardrobe page.
#[derive(PartialEq)]
pub struct WardrobePage;

impl Component for WardrobePage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let scroll = use_scroll_controller(ScrollConfig::default);
        let ctl = Ctl {
            store,
            chosen: store.ui.wardrobe.chosen,
            shelf: use_state(Vec::new),
            look: use_state(|| None),
            phase: use_state(|| Phase::Idle),
            views: use_state(|| None),
            busy: use_state(|| None),
            text: use_state(String::new),
            adding: use_state(|| false),
            add_problem: use_state(|| None),
            add_details: use_state(|| false),
            error: use_state(|| None),
            anchor: use_state(|| None),
            details: use_state(|| false),
            grid: use_state(|| 0.),
        };
        use_hook(move || spawn_in_window(async move { ctl.start() }));
        let mut room = use_state(|| store.ui.window.peek().0);
        let mut dropping = use_state(|| false);
        let m = store.model.read();
        let split = *room.read() >= frame::SPLIT;
        let stage = stage::stage(&t, ctl, &m, if split { Size::px(STAGE) } else { Size::fill() }, split);
        let library = library::library(&t, ctl, if split { Size::flex(1.) } else { Size::fill() }, *dropping.read());
        let mut body = page::column();
        body = if split {
            body.child(rect().width(Size::fill()).horizontal().content(Content::flex()).spacing(frame::SECTION).child(stage).child(library))
        } else {
            body.child(stage).child(library)
        };
        rect()
            .width(Size::fill())
            .height(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| room.set_if_modified(e.area.width()))
            .on_global_file_hover(move |_| dropping.set_if_modified(true))
            .on_global_file_hover_cancelled(move |_| dropping.set_if_modified(false))
            .on_file_drop(move |e: Event<FileEventData>| {
                dropping.set_if_modified(false);
                ctl.add_paths(e.file_paths.clone());
            })
            .child(page::scrolling(scroll, body))
    }
}
