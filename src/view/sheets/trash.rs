//! Papierkorb: what was deleted or replaced in the last seven days, across every instance, put back or deleted for good.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use freya::prelude::*;

use crate::core::bin::{self, Item, What};
use crate::core::i18n::{tr, trf};
use crate::ui::forms::explain_as;
use crate::ui::instance_files::{bytes, when};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::group::status;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::{caption, line, para, spacer};
use crate::view::kit::{after, at_least, off_thread, toast, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, frame, iconsize, space};

const LIST: f32 = 360.;
const ROW: f32 = 48.;
const RECHECK: Duration = Duration::from_millis(600);
const EMPTY_ICON: f32 = iconsize::L * 1.5;
const TEXT_GAP: f32 = 1.;

/// Opens the Papierkorb.
pub fn open(store: Store) {
    sheet::open(store.ui, Trash);
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
    format!("{}/{}", item.instance, item.entry)
}

fn look(item: &Item) -> (Icon, String) {
    match &item.what {
        What::Instance => (Icon::Package, if item.name.is_empty() { tr("Instanz") } else { item.name.clone() }),
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
    let rx = off_thread("trash-back", work).ok_or_else(|| tr("Nicht zurückgelegt."))?;
    spawn_in_window(async move {
        match rx.recv().await {
            Ok(Ok(())) => {
                toast::show(store.ui, trf("„%@“ liegt wieder an seinem Platz", &[&title]), None);
                store.reload(true);
            }
            Ok(Err(e)) => {
                toast::show(store.ui, explain_as(tr("Nicht zurückgelegt."), &e).text, None);
            }
            Err(_) => {}
        }
    });
    Ok(())
}

#[derive(PartialEq)]
struct Trash;

impl Component for Trash {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut load = use_state(|| Load::Loading);
        let mut taken = use_state(HashSet::<String>::new);
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
                taken.write().clear();
                load.set(match items {
                    Ok(items) => Load::Ready(items.into_iter().map(Entry).collect()),
                    Err(e) => Load::Failed(explain_as(tr("Der Papierkorb lässt sich nicht lesen."), &e).text),
                });
            });
        };
        use_hook(move || reload());
        let _ = store.model.read();
        let state = load.read().clone();
        let (body, subtitle, items) = match &state {
            Load::Loading => (rect().padding((space::L, 0.)).child(caption(&t, tr("Wird geladen …"))).into_element(), None, Vec::new()),
            Load::Failed(problem) => (
                rect()
                    .padding((space::L, 0.))
                    .spacing(space::M)
                    .child(status(&t, problem.clone(), t.danger_text))
                    .child(Button::new("trash.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| {
                        load.set(Load::Loading);
                        reload();
                    }))
                    .into_element(),
                None,
                Vec::new(),
            ),
            Load::Ready(items) if items.is_empty() => (
                rect().width(Size::fill()).padding((space::XL, 0.)).center().spacing(space::S).child(icon(&t, Icon::Trash, EMPTY_ICON, t.text3)).child(line(&t, tr("Der Papierkorb ist leer."), font::BODY, t.text2)).into_element(),
                Some(tr("Was du löschst oder ersetzt, bleibt hier 7 Tage.")),
                Vec::new(),
            ),
            Load::Ready(items) => {
                let size: u64 = items.iter().map(|e| e.0.size).sum();
                let mut list = rect().width(Size::fill());
                for (at, Entry(item)) in items.iter().enumerate() {
                    if at > 0 {
                        list = list.child(rect().width(Size::fill()).padding((0., space::M)).child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator)));
                    }
                    let k = key(item);
                    let gone = taken.read().contains(&k);
                    let reason = if gone { Some(tr("Wird zurückgelegt …")) } else if *emptying.read() { Some(tr("Wird geleert …")) } else { blocked(store, item) };
                    let picked = item.clone();
                    let back = Button::new(format!("trash.back.{at}"), tr("Zurücklegen")).plain().disabled(reason).on_press(move |_| {
                        match put_back(store, picked.clone()) {
                            Ok(()) => {
                                taken.write().insert(key(&picked));
                                spawn_in_window(async move {
                                    after(RECHECK).await;
                                    reload();
                                });
                            }
                            Err(text) => {
                                toast::show(store.ui, text, None);
                            }
                        }
                    });
                    list = list.child(row(&t, item, back.into_element()));
                }
                let boxed = rect().width(Size::fill()).corner_radius(t.shape.control).border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner)).overflow(Overflow::Clip).child(ScrollView::new().width(Size::fill()).height(Size::px((items.len() as f32 * (ROW + 1.)).min(LIST))).child(list));
                (boxed.into_element(), Some(trf("Bleibt 7 Tage, dann wird es gelöscht · %@", &[&bytes(size)])), items.clone())
            }
        };
        let mut content = rect().width(Size::fill()).spacing(space::M).child(body);
        if *asking.read() && !items.is_empty() {
            let size: u64 = items.iter().map(|e| e.0.size).sum();
            let message = if items.len() == 1 { trf("Ein Eintrag (%@) wird endgültig gelöscht.", &[&bytes(size)]) } else { trf("%1$lld Einträge (%2$@) werden endgültig gelöscht.", &[&items.len(), &bytes(size)]) };
            content = content.child(
                rect()
                    .width(Size::fill())
                    .padding(space::M)
                    .corner_radius(t.shape.control)
                    .background(t.danger.with_af32(frame::NOTICE_WASH))
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .child(para(&t, message, font::CALLOUT, t.text1).width(Size::flex(1.)))
                    .child(Button::new("trash.empty.no", tr("Behalten")).on_press(move |_| asking.set(false)))
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
        let done = Primary { label: tr("Fertig"), disabled: None, danger: false, run: EventHandler::new(move |_| sheet::close(store.ui)) };
        let busy = *emptying.read();
        let mut card = Card::new("trash", tr("Papierkorb")).body(content).primary(done).primary_id("trash.done").inform().locked(busy.then(|| tr("Wird geleert …"))).width(frame::SHEET);
        if let Some(s) = subtitle {
            card = card.message(s);
        }
        if !items.is_empty() {
            card = card.secondary_id("trash.empty").secondary(Primary {
                label: tr("Papierkorb leeren…"),
                disabled: busy.then(|| tr("Wird geleert …")),
                danger: true,
                run: EventHandler::new(move |_| asking.set(true)),
            });
        }
        card
    }
}

fn row(t: &crate::view::theme::Theme, item: &Item, button: Element) -> Element {
    let (glyph, title) = look(item);
    let mut parts = Vec::new();
    if !item.name.is_empty() && !matches!(item.what, What::Instance) {
        parts.push(item.name.clone());
    }
    parts.push(when(i64::try_from(item.at / 1000).unwrap_or(i64::MAX)));
    parts.push(bytes(item.size));
    rect()
        .width(Size::fill())
        .padding((space::XS, space::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .child(at_least(ROW - space::XS * 2., icon(t, glyph, iconsize::M, t.text3)))
        .child(rect().width(Size::flex(1.)).spacing(TEXT_GAP).child(line(t, title, font::BODY, t.text1)).child(line(t, parts.join(" · "), font::CAPTION, t.text2).font_tabular()))
        .child(spacer())
        .child(button)
        .content(Content::flex())
        .into_element()
}
