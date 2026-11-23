//! Teilen: turning sharing on for instances from before it existed, and the earlier shared states to go back to.

use std::collections::BTreeSet;

use freya::prelude::*;

use crate::app::event::Event;
use crate::core::i18n::{self, tr, trf};
use crate::core::share::{self, ITEMS, Item, Sharing, Version};
use crate::ui::forms;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::select::Select;
use crate::view::kit::switch::Switch;
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{control, space};

/// The sharing section of the settings page.
#[derive(PartialEq)]
pub struct ShareGroup;

impl Component for ShareGroup {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut chosen = use_state(BTreeSet::<String>::new);
        let mut busy = use_state(|| false);
        let mut versions = use_state(|| None::<Vec<(Item, Vec<Version>)>>);
        let mut picked = use_state(|| [0usize; 3]);
        let mut asked = use_state(|| 0u32);
        use_side_effect(move || {
            let _ = asked.read();
            spawn(async move {
                let found = off_ui(|| ITEMS.into_iter().map(|i| (i, share::versions(i).unwrap_or_default())).collect::<Vec<_>>()).await;
                if versions.try_peek().is_some() {
                    versions.set(found);
                    picked.set([0; 3]);
                }
            });
        });

        let apart: Vec<(String, String)> = store.model.read().instances.iter().filter(|i| i.sync != Sharing::ALL).map(|i| (i.id.clone(), i.name.clone())).collect();
        let mut rows: Vec<Element> = Vec::new();
        if apart.is_empty() {
            rows.push(Line::new("settings.share.all", tr("Alle Instanzen teilen ihre Einstellungen.")).into_element());
        }
        for (id, name) in &apart {
            let on = chosen.read().contains(id);
            let pick = id.clone();
            let mut flip = move |want: bool| {
                let mut next = chosen.peek().clone();
                if want {
                    next.insert(pick.clone());
                } else {
                    next.remove(&pick);
                }
                chosen.set(next);
            };
            rows.push(Line::new(format!("settings.share.pick.{id}.row"), name.clone()).control(Switch::new(format!("settings.share.pick.{id}"), name.clone(), on).disabled(*busy.read()).on_toggle(flip.clone())).on_press(move |_| flip(!on)).into_element());
        }
        let count = chosen.read().iter().filter(|id| apart.iter().any(|(a, _)| a == *id)).count();
        if !apart.is_empty() {
            let why = if *busy.read() { Some(tr("Wird umgestellt …")) } else if count == 0 { Some(tr("Wähle zuerst Instanzen aus.")) } else { None };
            let all: Vec<String> = apart.iter().map(|(id, _)| id.clone()).collect();
            let label = if count > 1 { trf("Für %@ Instanzen einschalten", &[&count]) } else { tr("Teilen einschalten") };
            let start = Button::new("settings.share.turn_on", label).disabled(why).on_press(move |_| {
                let ids: Vec<String> = chosen.peek().iter().filter(|id| all.contains(id)).cloned().collect();
                if ids.is_empty() || *busy.peek() {
                    return;
                }
                busy.set(true);
                spawn(async move {
                    match off_ui(move || share::turn_on(&ids)).await {
                        Some(Ok(done)) => {
                            for made in done {
                                store.dispatch(Event::InstanceChanged(made));
                            }
                        }
                        Some(Err(e)) => {
                            let _ = toast::show(store.ui, forms::explain_as(tr("Nicht eingeschaltet."), &e).text, None);
                        }
                        None => {
                let _ = toast::show(store.ui, forms::unexplained(), None);
            }
                    }
                    if busy.try_peek().is_some() {
                        busy.set(false);
                        chosen.set(BTreeSet::new());
                        let next = asked.peek().wrapping_add(1);
                        asked.set(next);
                    }
                });
            });
            rows.push(rect().width(Size::fill()).padding((space::S, space::L)).horizontal().main_align(Alignment::End).child(start).into_element());
        }
        let turn_on = group(&t, Some(tr("Einstellungen teilen")), rows, Some(tr("Die zuletzt gespielte Instanz gibt vor, was noch fehlt. Was die anderen vorher hatten, bleibt unter „Frühere Stände“.")));

        let list = versions.read().clone();
        let history_rows: Vec<Element> = ITEMS
            .into_iter()
            .enumerate()
            .map(|(n, item)| {
                let (key, label) = match item {
                    Item::Options => ("options", tr("Spiel-Optionen")),
                    Item::Config => ("config", tr("Mod-Einstellungen")),
                    Item::Servers => ("servers", tr("Serverliste")),
                };
                let found: Vec<Version> = list.as_ref().and_then(|l| l.iter().find(|(i, _)| *i == item)).map(|(_, v)| v.clone()).unwrap_or_default();
                let mut line = Line::new(format!("settings.share.history.{key}.row"), label.clone());
                if found.is_empty() {
                    let hint = if list.is_none() { tr("Wird geladen …") } else { tr("Noch keine.") };
                    return line.hint(hint).dim(true).into_element();
                }
                let at = picked.read()[n].min(found.len() - 1);
                let labels: Vec<String> = found.iter().map(describe).collect();
                let restore_one = found[at].clone();
                let select = Select::new(format!("settings.share.history.{key}"), label.clone(), labels, at).width(control::VERSION_MENU).on_choose(move |i| {
                    let mut next = *picked.peek();
                    next[n] = i;
                    picked.set(next);
                });
                let back = Button::new(format!("settings.share.history.{key}.restore"), tr("Zurückholen")).on_press(move |_| restore(store, restore_one.clone(), asked));
                line = line.control(rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(select).child(back));
                line.into_element()
            })
            .collect();
        let history = group(&t, Some(tr("Frühere Stände")), history_rows, Some(tr("Zurückholen macht einen Stand wieder zum geteilten; der jetzige bleibt hier. Instanzen bekommen ihn beim nächsten Start.")));
        rect().width(Size::fill()).spacing(space::XL).child(turn_on).child(history)
    }
}

fn describe(v: &Version) -> String {
    let when = chrono::DateTime::from_timestamp_millis(v.written.unwrap_or(v.at)).map(|d| i18n::date_time(d.with_timezone(&chrono::Local).naive_local())).unwrap_or_default();
    let files = if v.item == Item::Config && v.files > 1 { trf(" · %@ Dateien", &[&v.files]) } else { String::new() };
    match (&v.source, v.own) {
        (Some(name), true) => format!("{when} · {}{files}", trf("eigener Stand von %@", &[name])),
        (Some(name), false) => format!("{when} · {}{files}", trf("von %@", &[name])),
        (None, _) => format!("{when}{files}"),
    }
}

fn restore(store: Store, version: Version, mut asked: State<u32>) {
    spawn(async move {
        match off_ui(move || share::restore(&version)).await {
            Some(Ok(())) => {
                let _ = toast::show(store.ui, tr("Zurückgeholt. Instanzen bekommen den Stand beim nächsten Start."), None);
            }
            Some(Err(e)) => {
                let _ = toast::show(store.ui, forms::explain_as(tr("Nicht zurückgeholt."), &e).text, None);
            }
            None => {
                let _ = toast::show(store.ui, forms::unexplained(), None);
            }
        }
        if asked.try_peek().is_some() {
            let next = asked.peek().wrapping_add(1);
            asked.set(next);
        }
    });
}