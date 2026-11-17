//! The "…" button that holds what a narrow toolbar has no room for, so nothing vanishes when the window shrinks.

use freya::prelude::*;

use super::Bounds;
use super::button::Button;
use super::menu::{self, Item, Menu};
use crate::core::i18n::tr;
use crate::view::icons::Icon;
use crate::view::store::use_store;

/// A plain "…" button named `id` that drops `items` under itself.
#[derive(PartialEq)]
pub struct More {
    pub id: String,
    pub items: Vec<Item>,
}

impl Component for More {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut at = use_state(Bounds::default);
        let items = self.items.clone();
        rect().on_sized(move |e: Event<SizedEventData>| at.set_if_modified(Bounds::of(&e))).child(Button::icon(self.id.clone(), Icon::More, tr("Mehr")).plain().tip(tr("Mehr")).on_press(move |_| {
            menu::open(store.ui, Menu::under(items.clone(), *at.peek()));
        }))
    }
}
