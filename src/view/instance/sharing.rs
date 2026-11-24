//! "Sync": whether this instance keeps its settings the same as the others, said in words, with a way to the Sync page.

use freya::prelude::*;

use super::Facts;
use crate::core::i18n::tr;
use crate::core::share::Sharing as Shared;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::switch::Switch as Toggle;
use crate::view::kit::use_theme;
use crate::view::store::use_store;
use crate::view::sync::words::{effective, instance_line};
use crate::view::sync::{Ctx, switch};
use crate::view::theme::space;
use crate::view::ui::Page;

/// The sync group of the Einstellungen tab.
#[derive(PartialEq)]
pub struct Sharing {
    pub facts: Facts,
}

impl Component for Sharing {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ctx = Ctx { store, asked: use_state(|| 0u32) };
        let mut working = use_state(|| false);
        let made = &self.facts.made;
        let on = effective(made.sync, &made.game_version).any();
        let hint = if *working.read() { tr("Wird umgestellt …") } else { instance_line(made.sync, &made.game_version, self.facts.running) };
        let (id, before, running) = (made.id.clone(), made.sync, self.facts.running);
        let mut flip = move |want: bool| {
            if *working.peek() {
                return;
            }
            working.set(true);
            switch(ctx, id.clone(), before, if want { Shared::ALL } else { Shared::NONE }, running, move |_| {
                if working.try_peek().is_some() {
                    working.set(false);
                }
            });
        };
        let mut page = store.ui.page;
        let open = Button::new("instance.settings.sync.open", tr("In Sync anzeigen")).plain().on_press(move |_| page.set(Page::Sync));
        let control = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(open).child(Toggle::new("instance.settings.sync", tr("Mit den anderen Instanzen synchronisieren"), on).on_toggle(flip.clone()));
        let line = Line::new("instance.settings.sync.row", tr("Mit den anderen Instanzen synchronisieren")).hint(hint).control(control).on_press(move |_| flip(!on));
        group(&t, Some(tr("Sync")), vec![line.into_element()], Some(tr("Welche Dateien und von welcher Instanz sie kommen, zeigt die Seite Sync.")))
    }
}
