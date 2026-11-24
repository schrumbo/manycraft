//! The block of instances: one switch each, what it does now, and a choice per item behind the row.

use freya::prelude::*;

use super::words::{instance_line, label};
use super::{Ctx, Who, switch};
use crate::core::i18n::tr;
use crate::core::share::{self, ITEMS, Sharing};
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::chip::Chip;
use crate::view::kit::group::{Line, group};
use crate::view::kit::switch::Switch;
use crate::view::kit::use_theme;
use crate::view::theme::space;

/// Every instance with its switch.
#[derive(PartialEq)]
pub(super) struct InstancesBlock {
    pub ctx: Ctx,
    pub who: Vec<Who>,
}

impl Component for InstancesBlock {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ctx = self.ctx;
        let mut opened = use_state(|| None::<String>);
        let mut working = use_state(Vec::<String>::new);
        let mut rows: Vec<Element> = Vec::new();
        for w in &self.who {
            let busy = working.read().contains(&w.id);
            let now = super::words::effective(w.sync, &w.version);
            let on = now.any();
            let hint = if busy { tr("Wird umgestellt …") } else { instance_line(w.sync, &w.version, w.running) };
            let mut flip = {
                let (id, before, running) = (w.id.clone(), w.sync, w.running);
                move |want: bool| {
                    if working.peek().contains(&id) {
                        return;
                    }
                    let mut next = working.peek().clone();
                    next.push(id.clone());
                    working.set(next);
                    let done = id.clone();
                    switch(ctx, id.clone(), before, if want { Sharing::ALL } else { Sharing::NONE }, running, move |_| {
                        if working.try_peek().is_some() {
                            let mut left = working.peek().clone();
                            left.retain(|i| *i != done);
                            working.set(left);
                        }
                    });
                }
            };
            let open = opened.read().as_deref() == Some(w.id.as_str());
            let id = w.id.clone();
            let more = Button::icon(format!("sync.instance.{}.more", w.id), if open { Icon::ChevronUp } else { Icon::ChevronDown }, if open { tr("Arten verbergen") } else { tr("Arten einzeln wählen") }).plain().on_press(move |_| opened.set(if open { None } else { Some(id.clone()) }));
            let control = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(more).child(Switch::new(format!("sync.instance.{}", w.id), w.name.clone(), on).on_toggle(flip.clone()));
            let mut line = Line::new(format!("sync.instance.{}.row", w.id), w.name.clone()).hint(hint).control(control).on_press(move |_| flip(!on));
            if open {
                let chips = ITEMS.into_iter().filter(|i| *i != share::Item::Options || share::options_travel(&w.version)).map(|item| {
                    let (id, before, running) = (w.id.clone(), w.sync, w.running);
                    let next = w.sync.with(item, !w.sync.get(item));
                    Chip::new(format!("sync.instance.{}.{}", w.id, label(item)), label(item)).on(w.sync.get(item)).on_press(move |_| switch(ctx, id.clone(), before, next, running, |_| {})).into_element()
                });
                line = line.below(rect().horizontal().spacing(space::S).children(chips));
            }
            rows.push(line.into_element());
        }
        group(&t, Some(tr("Instanzen")), rows, Some(tr("Aus: die Instanz behält den jetzigen Stand, nichts wird gelöscht. An: ihre eigenen Dateien wandern in den Verlauf und der gemeinsame Stand gilt ab dem nächsten Start.")))
    }
}
