//! Teilen: turning sharing on for instances from before it existed, and the earlier shared states to go back to.

use std::collections::BTreeSet;

use freya::prelude::*;

use crate::app::event::Event;
use crate::core::i18n::{self, tr, trf};
use crate::core::share::{self, ITEMS, Item, Note, Sharing, Version};
use crate::ui::forms;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::select::Select;
use crate::view::kit::switch::Switch;
use crate::view::instance::sharing::{folder_line, label, note_line, sharers};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::native;
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
        let mut versions = use_state(|| None::<Vec<(Item, Option<Vec<Version>>)>>);
        let mut picked = use_state(|| [0usize; 3]);
        let mut asked = use_state(|| 0u32);
        let mut notes = use_state(Vec::<Note>::new);
        let mut present = use_state(|| [false; 3]);
        use_side_effect(move || {
            let _ = asked.read();
            spawn_in_window(async move {
                let found = off_ui(|| (ITEMS.into_iter().map(|i| (i, share::versions(i).ok())).collect::<Vec<_>>(), share::notes(), ITEMS.map(|i| share::folder().join(file_of(i)).exists()))).await;
                if versions.try_peek().is_some()
                    && let Some((found, noted, there)) = found
                {
                    versions.set(Some(found));
                    picked.set([0; 3]);
                    notes.set(noted);
                    present.set(there);
                }
            });
        });
        let who = sharers(store, None);
        let mut file_rows: Vec<Element> = ITEMS.into_iter().enumerate().map(|(n, item)| file_line(item, &who[n], present.read()[n])).collect();
        if let Some(note) = notes.read().first() {
            file_rows.push(note_line(&t, "settings.share.note", note, notes));
        }
        file_rows.push(folder_line("settings.share.folder"));
        let files = group(&t, Some(tr("Gemeinsame Dateien")), file_rows, Some(tr("Es gibt jede Datei nur einmal. Alle teilenden Instanzen lesen und ändern dieselbe. Bevor ein Spiel startet, hebt Manycraft den Stand auf und holt ihn zurück, wenn der Start ihn zurücksetzt.")));

        let (any, apart, running): (bool, Vec<(String, String, Sharing)>, Vec<String>) = {
            let model = store.model.read();
            (!model.instances.is_empty(), model.instances.iter().filter(|i| i.sync != Sharing::ALL).map(|i| (i.id.clone(), i.name.clone(), i.sync)).collect(), model.instances.iter().filter(|i| model.games.values().any(|g| g.instance_id == i.id && g.phase.is_active())).map(|i| i.id.clone()).collect())
        };
        let mut rows: Vec<Element> = Vec::new();
        if !any {
            rows.push(Line::new("settings.share.none", tr("Noch keine Instanzen")).dim(true).into_element());
        } else if apart.is_empty() {
            rows.push(Line::new("settings.share.all", tr("Alle Instanzen teilen ihre Einstellungen.")).into_element());
        }
        for (id, name, sync) in &apart {
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
            rows.push(Line::new(format!("settings.share.pick.{id}.row"), name.clone()).hint(missing(*sync)).control(Switch::new(format!("settings.share.pick.{id}"), name.clone(), on).disabled(*busy.read()).on_toggle(flip.clone())).on_press(move |_| {
                if !*busy.peek() {
                    flip(!on);
                }
            }).into_element());
        }
        let count = chosen.read().iter().filter(|id| apart.iter().any(|(a, _, _)| a == *id)).count();
        if !apart.is_empty() {
            let why = if *busy.read() { Some(tr("Wird umgestellt …")) } else if count == 0 { Some(tr("Wähle zuerst Instanzen aus.")) } else { None };
            let all: Vec<String> = apart.iter().map(|(id, _, _)| id.clone()).collect();
            let every = all.clone();
            let before: Vec<(String, Sharing)> = apart.iter().map(|(id, _, sync)| (id.clone(), *sync)).collect();
            let pick_all = Button::new("settings.share.pick_all", tr("Alle auswählen")).disabled(if *busy.read() { Some(tr("Wird umgestellt …")) } else if count == all.len() { Some(tr("Alle sind ausgewählt.")) } else { None }).on_press(move |_| {
                chosen.set(every.iter().cloned().collect());
            });
            let label = if count > 1 { trf("Für %@ Instanzen einschalten", &[&count]) } else { tr("Teilen einschalten") };
            let start = Button::new("settings.share.turn_on", label).disabled(why).on_press(move |_| {
                let ids: Vec<String> = chosen.peek().iter().filter(|id| all.contains(id)).cloned().collect();
                if ids.is_empty() || *busy.peek() {
                    return;
                }
                let earlier: Vec<(String, Sharing)> = before.iter().filter(|(id, _)| ids.contains(id)).cloned().collect();
                busy.set(true);
                let running = running.clone();
                let back = running.clone();
                spawn_in_window(async move {
                    match off_ui(move || share::turn_on(&ids, &running)).await {
                        Some(Ok(done)) => {
                            let note = if done.len() > 1 { trf("Teilen ist an für %@ Instanzen.", &[&done.len()]) } else { tr("Teilen ist an.") };
                            for made in done {
                                store.dispatch(Event::InstanceChanged(made));
                            }
                            let undo = EventHandler::new(move |_| take_back(store, earlier.clone(), back.clone()));
                            let _ = toast::show(store.ui, note, Some(undo));
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
            if count == 0 && !*busy.read() {
                rows.push(Line::new("settings.share.turn_on.why", tr("Wähle zuerst Instanzen aus.")).hint(tr("Tippe auf die Instanzen, die ihre Einstellungen teilen sollen.")).dim(true).into_element());
            }
            rows.push(rect().width(Size::fill()).padding((space::S, space::L)).horizontal().main_align(Alignment::End).spacing(space::S).child(pick_all).child(start).into_element());
        }
        let turn_on = group(&t, Some(tr("Einstellungen teilen")), rows, Some(tr("Die zuletzt gespielte Instanz gibt vor, was noch fehlt. Was die anderen vorher hatten, bleibt unter „Frühere Stände“.")));

        let list = versions.read().clone();
        let nothing_yet = list.as_ref().is_some_and(|l| l.iter().all(|(_, v)| v.as_ref().is_some_and(Vec::is_empty)));
        let history_rows: Vec<Element> = if nothing_yet {
            vec![Line::new("settings.share.history.none", tr("Noch keine früheren Stände.")).dim(true).into_element()]
        } else {
            ITEMS
            .into_iter()
            .enumerate()
            .map(|(n, item)| {
                let (key, label) = match item {
                    Item::Options => ("options", tr("Spiel-Optionen")),
                    Item::Config => ("config", tr("Mod-Einstellungen")),
                    Item::Servers => ("servers", tr("Serverliste")),
                };
                let entry = list.as_ref().and_then(|l| l.iter().find(|(i, _)| *i == item)).map(|(_, v)| v.clone());
                let mut line = Line::new(format!("settings.share.history.{key}.row"), label.clone());
                let found = match entry {
                    None => return line.hint(tr("Wird geladen …")).dim(true).into_element(),
                    Some(None) => return line.hint(tr("Die früheren Stände lassen sich nicht lesen.")).dim(true).into_element(),
                    Some(Some(found)) if found.is_empty() => return line.hint(tr("Noch keine.")).dim(true).into_element(),
                    Some(Some(found)) => found,
                };
                let at = picked.read()[n].min(found.len() - 1);
                let labels: Vec<String> = found.iter().map(describe).collect();
                let restore_one = found[at].clone();
                let select = Select::new(format!("settings.share.history.{key}"), label.clone(), labels, at).width(control::VERSION_MENU).on_choose(move |i| {
                    let mut next = *picked.peek();
                    next[n] = i;
                    picked.set(next);
                });
                let why = busy.read().then(|| tr("Wird zurückgeholt …"));
                let back = Button::new(format!("settings.share.history.{key}.restore"), tr("Zurückholen")).disabled(why).on_press(move |_| {
                    if !*busy.peek() {
                        restore(store, restore_one.clone(), asked, busy);
                    }
                });
                line = line.control(rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(select).child(back));
                line.into_element()
            })
            .collect()
        };
        let history = group(&t, Some(tr("Frühere Stände")), history_rows, Some(tr("Zurückholen macht einen Stand wieder zum gemeinsamen; der jetzige bleibt hier.")));
        rect().width(Size::fill()).spacing(space::XL).child(files).child(turn_on).child(history)
    }
}

fn take_back(store: Store, earlier: Vec<(String, Sharing)>, running: Vec<String>) {
    spawn_in_window(async move {
        match off_ui(move || earlier.iter().map(|(id, sync)| share::set(id, *sync, running.contains(id))).collect::<anyhow::Result<Vec<_>>>()).await {
            Some(Ok(made)) => {
                for one in made {
                    store.dispatch(Event::InstanceChanged(one));
                }
            }
            Some(Err(e)) => {
                let _ = toast::show(store.ui, forms::explain_as(tr("Nicht zurückgenommen."), &e).text, None);
            }
            None => {
                let _ = toast::show(store.ui, forms::unexplained(), None);
            }
        }
    });
}

fn file_of(item: Item) -> &'static str {
    match item {
        Item::Options => "options.txt",
        Item::Config => "config",
        Item::Servers => "servers.dat",
    }
}

/// One shared file: who shares it and a way to open it.
fn file_line(item: Item, who: &[String], present: bool) -> Element {
    let key = file_of(item).replace('.', "_");
    let names = match who.len() {
        0 => tr("Noch teilt keine Instanz."),
        1..=3 => trf("Geteilt von %@.", &[&who.join(", ")]),
        n => trf("Geteilt von %@ und %@ weiteren.", &[&who[..2].join(", "), &(n - 2)]),
    };
    let mut line = Line::new(format!("settings.share.file.{key}"), label(item));
    if item == Item::Servers {
        return line.hint(format!("{names} {}", tr("Bearbeiten im Spiel unter Mehrspieler."))).into_element();
    }
    let path = share::folder().join(file_of(item));
    let why = (!present).then(|| tr("Entsteht, sobald ein teilendes Spiel etwas speichert."));
    let hint = why.as_ref().map_or_else(|| names.clone(), |why| format!("{names} {why}"));
    let open = Button::new(format!("settings.share.file.{key}.open"), if item == Item::Config { tr("Ordner öffnen") } else { tr("Öffnen") }).plain().disabled(why).on_press(move |_| native::open(&path));
    line = line.hint(hint).control(open);
    line.into_element()
}

fn missing(sync: Sharing) -> String {
    if sync == Sharing::NONE {
        return tr("Teilt noch nichts");
    }
    let off: Vec<String> = ITEMS
        .into_iter()
        .filter(|i| !sync.get(*i))
        .map(|i| match i {
            Item::Options => tr("Spiel-Optionen"),
            Item::Config => tr("Mod-Einstellungen"),
            Item::Servers => tr("Serverliste"),
        })
        .collect();
    trf("Ohne %@", &[&off.join(", ")])
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

fn restore(store: Store, version: Version, mut asked: State<u32>, mut busy: State<bool>) {
    busy.set(true);
    spawn_in_window(async move {
        match off_ui(move || share::restore(&version)).await {
            Some(Ok(())) => {
                let _ = toast::show(store.ui, tr("Zurückgeholt. Mod-Einstellungen gelten sofort, Spiel-Optionen und Serverliste ab dem nächsten Start."), None);
            }
            Some(Err(e)) => {
                let _ = toast::show(store.ui, forms::explain_as(tr("Nicht zurückgeholt."), &e).text, None);
            }
            None => {
                let _ = toast::show(store.ui, forms::unexplained(), None);
            }
        }
        if asked.try_peek().is_some() {
            busy.set(false);
            let next = asked.peek().wrapping_add(1);
            asked.set(next);
        }
    });
}