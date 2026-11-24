//! Papierkorb: the page of what was deleted or replaced in the last seven days, batches and everything of every instance,
//! put back or deleted for good.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use freya::prelude::*;

use crate::core::bin::{self, Item, What};
use crate::core::i18n::{tr, trf};
use crate::ui::forms::explain_as;
use crate::ui::instance_files::{bytes, when};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::{Button, Look};
use crate::view::kit::group::{group, status};
use crate::view::kit::text::{caption, heading, line, para, spacer};
use crate::view::kit::{after, at_least, off_thread, toast, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, frame, iconsize, space};

const PAGE: f32 = 640.;
const ROW: f32 = 52.;
const RECHECK: Duration = Duration::from_millis(600);
const TEXT_GAP: f32 = 1.;

/// Shows the Papierkorb: the library's group of what was deleted lately, opened.
pub fn open(store: Store) {
    crate::view::library::show_recent(store);
}

#[derive(Clone, PartialEq)]
enum Load {
    Loading,
    Failed(String),
    Ready(Vec<Entry>),
}

#[derive(Clone)]
struct Entry(Item);

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        key(&self.0) == key(&other.0) && self.0.size == other.0.size
    }
}

fn key(item: &Item) -> String {
    match item.what {
        What::Batch => format!("batch/{}", item.entry),
        _ => format!("{}/{}", item.instance, item.entry),
    }
}

fn look(item: &Item) -> (Icon, String) {
    match &item.what {
        What::Instance => (Icon::Package, if item.name.is_empty() { tr("Instanz") } else { item.name.clone() }),
        What::Batch => (Icon::Layers, if item.name.is_empty() { tr("Batch") } else { item.name.clone() }),
        What::Reset(_) => (Icon::RotateCcw, tr("Stand vor dem Zurücksetzen")),
        What::Mods(Some(t)) if t.files.len() == 1 => (Icon::Puzzle, t.files[0].trim_end_matches(".disabled").to_owned()),
        What::Mods(t) => {
            let n = t.as_ref().map_or(0, |t| t.files.len());
            (Icon::Puzzle, if n == 0 { tr("Mods") } else { trf("%lld Mods", &[&n]) })
        }
        What::File { origin, .. } => {
            let parts: Vec<&str> = origin.split('/').collect();
            let last = parts.last().copied().unwrap_or_default().to_owned();
            match parts.as_slice() {
                ["games", _, "saves", _] => (Icon::Earth, last),
                ["games", _, "saves"] => (Icon::Earth, tr("Welten")),
                ["games", _, "screenshots", _] => (Icon::Image, last),
                ["mods"] => (Icon::Puzzle, tr("Mods")),
                ["config"] => (Icon::Folder, tr("Einstellungen der Mods")),
                ["resourcepacks"] => (Icon::Folder, tr("Ressourcenpakete")),
                ["shaderpacks"] => (Icon::Folder, tr("Shader")),
                _ => (Icon::File, last),
            }
        }
    }
}

fn running(store: Store, instance: &str) -> bool {
    store.model.peek().games.values().any(|g| g.instance_id == instance && g.phase.is_active())
}

fn instance_dir(store: Store, instance: &str) -> Option<PathBuf> {
    store.model.peek().instance(instance).map(crate::core::instance::Instance::dir)
}

/// Whether `item` can be put back now, and why not when it cannot.
fn blocked(store: Store, item: &Item) -> Option<String> {
    match &item.what {
        What::Reset(None) | What::Mods(None) => Some(tr("Dafür fehlen die Angaben, was zurückgelegt werden soll.")),
        What::Instance => None,
        What::Batch if !item.instance.is_empty() && instance_dir(store, &item.instance).is_none() => Some(tr("Die Instanz dieses Batches gibt es nicht mehr.")),
        What::Batch => None,
        _ if instance_dir(store, &item.instance).is_none() => Some(tr("Diese Instanz gibt es nicht mehr.")),
        What::Reset(Some(_)) | What::File { taken: true, .. } if running(store, &item.instance) => Some(tr("Stoppe zuerst die laufenden Spiele.")),
        _ => None,
    }
}

fn put_back(store: Store, item: Item) -> Result<(), String> {
    let title = look(&item).1;
    let dir = instance_dir(store, &item.instance);
    let current = store.model.peek().instance(&item.instance).cloned();
    let work: Box<dyn FnOnce() -> anyhow::Result<()> + Send + 'static> = match item.what.clone() {
        What::Instance => {
            let entry = item.entry.clone();
            Box::new(move || crate::ui::remote::restore_instance(&entry).map(|_| ()))
        }
        What::Batch => {
            let (entry, instance) = (item.entry.clone(), item.instance.clone());
            Box::new(move || crate::ui::remote::restore_trashed_batch(&entry, &instance))
        }
        What::Reset(Some(meta)) => {
            let current = current.ok_or_else(|| tr("Diese Instanz gibt es nicht mehr."))?;
            if !crate::ui::remote::same_files(&current, &meta.after) {
                return Err(trf("„%@“ lässt sich nicht zurücklegen: Mods oder Version wurden inzwischen geändert.", &[&title]));
            }
            let (id, entry) = (item.instance.clone(), item.entry.clone());
            Box::new(move || crate::ui::remote::restore_reset(&id, &meta.before, &meta.after, &entry).map(|_| ()))
        }
        What::Mods(Some(trashed)) => {
            let id = item.instance.clone();
            Box::new(move || crate::core::content::untrash(&id, &trashed).map(|_| ()))
        }
        What::File { origin, taken } => {
            let dir = dir.ok_or_else(|| tr("Diese Instanz gibt es nicht mehr."))?;
            let entry = item.entry.clone();
            Box::new(move || {
                let world = taken.then(|| dir.join(&origin));
                let mut parked = None;
                if let Some(world) = &world {
                    if !crate::core::trash::holds(&dir, &entry) {
                        anyhow::bail!(crate::core::instance::Final(tr("Das liegt nicht mehr im Papierkorb.")));
                    }
                    if crate::core::worlds::is_open(world) {
                        anyhow::bail!(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst."));
                    }
                    if std::fs::symlink_metadata(world).is_ok() {
                        parked = Some(crate::core::trash::put(&dir, world)?);
                    }
                }
                crate::core::trash::take_back(&dir, &entry).map(|_| ()).inspect_err(|_| {
                    if let Some(parked) = &parked {
                        let _ = crate::core::trash::take_back(&dir, parked);
                    }
                })
            })
        }
        What::Reset(None) | What::Mods(None) => return Err(tr("Dafür fehlen die Angaben, was zurückgelegt werden soll.")),
    };
    let rx = off_thread("trash-back", work).ok_or_else(|| tr("Nicht wiederhergestellt."))?;
    spawn_in_window(async move {
        match rx.recv().await {
            Ok(Ok(())) => {
                toast::show(store.ui, trf("„%@“ ist wieder da.", &[&title]), None);
                store.reload(true);
            }
            Ok(Err(e)) => {
                toast::show(store.ui, explain_as(tr("Nicht wiederhergestellt."), &e).text, None);
            }
            Err(_) => {}
        }
    });
    Ok(())
}

/// What was deleted lately; `embedded` lists it inline under its group header in the library instead of filling a page.
#[derive(PartialEq)]
pub struct TrashPage {
    pub embedded: bool,
}

impl Component for TrashPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut load = use_state(|| Load::Loading);
        let mut busy = use_state(HashSet::<String>::new);
        let mut confirm = use_state(|| None::<String>);
        let mut emptying = use_state(|| false);
        let mut asking = use_state(|| false);
        let mut ticket = use_state(|| 0u64);
        let mut reload = move || {
            if ticket.try_peek().is_none() {
                return;
            }
            let n = *ticket.peek() + 1;
            ticket.set(n);
            let Some(rx) = off_thread("trash-list", bin::list) else { return };
            spawn_in_window(async move {
                let Ok(items) = rx.recv().await else { return };
                if ticket.try_peek().is_none_or(|t| *t != n) {
                    return;
                }
                load.set(match items {
                    Ok(items) => Load::Ready(items.into_iter().map(Entry).collect()),
                    Err(e) => Load::Failed(explain_as(tr("Der Papierkorb lässt sich nicht lesen."), &e).text),
                });
            });
        };
        let m = store.model.read();
        let shape = (m.batches.len(), m.instances.len());
        use_side_effect_with_deps(&shape, move |_| reload());
        let state = load.read().clone();
        let embedded = self.embedded;
        let body = match &state {
            Load::Loading => rect().width(Size::fill()).padding((space::XL, 0.)).center().child(caption(&t, tr("Wird geladen …"))).into_element(),
            Load::Failed(problem) => rect()
                .width(Size::fill())
                .padding((space::XL, 0.))
                .center()
                .spacing(space::M)
                .child(status(&t, problem.clone(), t.danger_text))
                .child(Button::new("trash.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| {
                    load.set(Load::Loading);
                    reload();
                }))
                .into_element(),
            Load::Ready(items) if items.is_empty() && embedded => rect().width(Size::fill()).padding((space::M, 0.)).child(caption(&t, tr("Der Papierkorb ist leer. Was du löschst oder ersetzt, bleibt hier 7 Tage."))).into_element(),
            Load::Ready(items) if items.is_empty() => rect()
                .width(Size::fill())
                .height(Size::fill())
                .center()
                .spacing(space::M)
                .child(icon(&t, Icon::Trash, iconsize::HERO, t.text3))
                .child(heading(&t, tr("Der Papierkorb ist leer.")))
                .child(caption(&t, tr("Was du löschst oder ersetzt, bleibt hier 7 Tage.")))
                .into_element(),
            Load::Ready(items) => {
                let size: u64 = items.iter().map(|e| e.0.size).sum();
                let locked = *emptying.read();
                let open = confirm.read().clone();
                let rows = items
                    .iter()
                    .enumerate()
                    .map(|(at, Entry(item))| {
                        let k = key(item);
                        let working = busy.read().contains(&k);
                        let context = match item.what {
                            What::Batch => m.instance(&item.instance).map(|i| i.name.clone()),
                            What::Instance => None,
                            _ => Some(item.name.clone()).filter(|n| !n.is_empty()),
                        };
                        let actions = if open.as_deref() == Some(k.as_str()) {
                            let doomed = item.clone();
                            rect()
                                .horizontal()
                                .cross_align(Alignment::Center)
                                .spacing(space::S)
                                .child(caption(&t, tr("Kann nicht rückgängig gemacht werden.")))
                                .child(Button::new(format!("trash.{at}.keep"), tr("Abbrechen")).on_press(move |_| confirm.set(None)))
                                .child(Button::new(format!("trash.{at}.purge"), tr("Löschen")).danger().disabled((working || locked).then(|| tr("Einen Moment …"))).on_press(move |_| {
                                    confirm.set(None);
                                    let k = key(&doomed);
                                    busy.write().insert(k.clone());
                                    let item = doomed.clone();
                                    let Some(rx) = off_thread("trash-delete", move || bin::delete(&item)) else {
                                        busy.write().remove(&k);
                                        return;
                                    };
                                    spawn_in_window(async move {
                                        if let Ok(Err(e)) = rx.recv().await {
                                            toast::show(store.ui, explain_as(tr("Nicht gelöscht."), &e).text, None);
                                        }
                                        if let Some(mut b) = busy.try_write() {
                                            b.remove(&k);
                                        }
                                        reload();
                                    });
                                }))
                                .into_element()
                        } else {
                            let reason = if working { Some(tr("Einen Moment …")) } else if locked { Some(tr("Wird geleert …")) } else { blocked(store, item) };
                            let picked = item.clone();
                            let back = Button::new(format!("trash.{at}.restore"), tr("Wiederherstellen")).disabled(reason).on_press(move |_| match put_back(store, picked.clone()) {
                                Ok(()) => {
                                    let k = key(&picked);
                                    busy.write().insert(k.clone());
                                    spawn_in_window(async move {
                                        after(RECHECK).await;
                                        if let Some(mut b) = busy.try_write() {
                                            b.remove(&k);
                                        }
                                        reload();
                                    });
                                }
                                Err(text) => {
                                    toast::show(store.ui, text, None);
                                }
                            });
                            let ask = k.clone();
                            let purge = Button::new(format!("trash.{at}.delete"), tr("Endgültig löschen"))
                                .look(Look::PlainDanger)
                                .disabled((working || locked).then(|| tr("Einen Moment …")))
                                .on_press(move |_| confirm.set(Some(ask.clone())));
                            rect().horizontal().cross_align(Alignment::Center).spacing(space::XS).child(purge).child(back).into_element()
                        };
                        row(&t, item, context, actions)
                    })
                    .collect();
                let summary = if items.len() == 1 { trf("1 Eintrag · %@", &[&bytes(size)]) } else { trf("%1$lld Einträge · %2$@", &[&items.len(), &bytes(size)]) };
                let mut head = rect()
                    .width(Size::fill())
                    .padding((0., space::XS))
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .child(line(&t, summary, font::CALLOUT, t.text2).font_tabular())
                    .child(spacer())
                    .content(Content::flex());
                if !*asking.read() {
                    head = head.child(Button::new("trash.empty", tr("Papierkorb leeren…")).look(Look::PlainDanger).disabled(locked.then(|| tr("Wird geleert …"))).on_press(move |_| asking.set(true)));
                }
                let mut column = rect().width(Size::fill()).max_width(Size::px(PAGE)).spacing(space::M).child(head);
                if *asking.read() {
                    let message = if items.len() == 1 { trf("Ein Eintrag (%@) wird endgültig gelöscht.", &[&bytes(size)]) } else { trf("%1$lld Einträge (%2$@) werden endgültig gelöscht.", &[&items.len(), &bytes(size)]) };
                    column = column.child(
                        rect()
                            .width(Size::fill())
                            .padding(space::M)
                            .corner_radius(t.shape.control)
                            .background(t.danger.with_af32(frame::NOTICE_WASH))
                            .horizontal()
                            .cross_align(Alignment::Center)
                            .spacing(space::S)
                            .child(para(&t, message, font::CALLOUT, t.text1).width(Size::flex(1.)))
                            .child(Button::new("trash.empty.no", tr("Abbrechen")).on_press(move |_| asking.set(false)))
                            .child(Button::new("trash.empty.yes", tr("Leeren")).danger().on_press(move |_| {
                                asking.set(false);
                                emptying.set(true);
                                let Some(rx) = off_thread("trash-empty", bin::empty) else {
                                    emptying.set(false);
                                    return;
                                };
                                spawn_in_window(async move {
                                    if let Ok((_, Err(e))) = rx.recv().await {
                                        toast::show(store.ui, explain_as(tr("Nicht alles wurde gelöscht."), &e).text, None);
                                    }
                                    if emptying.try_peek().is_none() {
                                        return;
                                    }
                                    emptying.set(false);
                                    reload();
                                });
                            }))
                            .content(Content::flex()),
                    );
                }
                column = column.child(group(&t, None, rows, Some(tr("Wird nach 7 Tagen endgültig gelöscht"))));
                if embedded {
                    return column.into_element();
                }
                ScrollView::new().width(Size::fill()).height(Size::fill()).child(rect().width(Size::fill()).padding(space::XL).cross_align(Alignment::Center).child(column)).into_element()
            }
        };
        if embedded {
            return rect().width(Size::fill()).child(body).into_element();
        }
        rect().width(Size::fill()).height(Size::fill()).child(body).into_element()
    }
}

fn row(t: &crate::view::theme::Theme, item: &Item, context: Option<String>, actions: Element) -> Element {
    let (glyph, title) = look(item);
    let mut parts: Vec<String> = context.into_iter().collect();
    parts.push(when(i64::try_from(item.at / 1000).unwrap_or(i64::MAX)));
    parts.push(bytes(item.size));
    rect()
        .width(Size::fill())
        .padding((space::XS, space::M, space::XS, space::L))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .child(at_least(ROW - space::XS * 2., icon(t, glyph, iconsize::M, t.text3)))
        .child(rect().width(Size::flex(1.)).spacing(TEXT_GAP).child(line(t, title, font::BODY, t.text1).max_lines(1).text_overflow(TextOverflow::Ellipsis)).child(line(t, parts.join(" · "), font::CAPTION, t.text2).font_tabular().max_lines(1).text_overflow(TextOverflow::Ellipsis)))
        .child(actions)
        .content(Content::flex())
        .into_element()
}
