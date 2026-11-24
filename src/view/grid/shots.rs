//! The screenshots of a game without a window, shown large in a sheet: the eyes and the view from above, newest first.
//! Those of a connection-only account live only in memory; a hidden game's are the files in its game folder.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::core::shots::{self, Moment, Shot};
use crate::core::trash;
use crate::ui::forms;
use crate::view::instance::actions;
use crate::view::instance::shots_parts::Same;
use crate::view::instance::shots_thumbs::{Held, Source};
use crate::view::instance::shots_view::{Picture, Viewer, files};
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::{heading, secondary};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::native;
use crate::view::store::{Store, use_preview, use_store};
use crate::view::theme::{frame, space};
use crate::view::ui::Page;

const ABOVE: &str = "_above";
const WIDTH_MAX: f32 = 1760.;
const HEIGHT_MIN: f32 = 280.;
const CARD_CHROME: f32 = 156.;

thread_local! {
    static SHOWN: RefCell<Option<EventHandler<()>>> = const { RefCell::new(None) };
}

/// Opens the screenshots of game `key` in a sheet.
pub fn open(store: Store, key: &str) {
    sheet::open(store.ui, Gallery { key: key.to_owned() });
}

/// The game whose screenshots the palette opens: the one selected that runs without a window.
pub fn target(store: Store) -> Option<String> {
    let m = store.model.peek();
    let selected = store.ui.selection.peek();
    let [key] = selected.iter().collect::<Vec<_>>()[..] else { return None };
    m.games.get(key).filter(|g| g.headless).map(|_| key.clone())
}

/// Whether Speichern unter … has a screenshot: the one a sheet shows, or else on the grid the newest of the selected
/// account, while no layer or text field has the keyboard.
pub fn can_save(store: Store) -> bool {
    shown_saves() || (matches!(*store.ui.page.peek(), Page::Grid(_)) && !store.ui.layered() && !crate::view::kit::field::typing() && newest(store).is_some())
}

/// Whether a sheet shows a screenshot Speichern unter … saves.
pub fn shown_saves() -> bool {
    SHOWN.with_borrow(Option::is_some)
}

/// Saves a copy of the screenshot the sheet shows, or else the newest view from the eyes of the selected account, where the
/// user picks.
pub fn save(store: Store) {
    if let Some(run) = SHOWN.with_borrow(Clone::clone) {
        run.call(());
    } else if let Some((name, moment)) = newest(store) {
        save_png(store, suggested(&name, moment.taken, false), moment.eyes);
    }
}

fn newest(store: Store) -> Option<(String, Moment)> {
    let key = target(store)?;
    let m = store.model.peek();
    let game = m.games.get(&key).filter(|g| g.connection)?;
    let moment = m.moments.get(&key)?.first()?.clone();
    Some((m.account(game.account_id).map(|a| a.name.clone()).unwrap_or_default(), moment))
}

fn local(taken: i64) -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::from_timestamp(taken, 0).unwrap_or_default().with_timezone(&chrono::Local)
}

fn suggested(name: &str, taken: i64, above: bool) -> String {
    let when = local(taken).format("%Y-%m-%d %H.%M.%S");
    if above { format!("{name} {when} {}.png", tr("von oben")) } else { format!("{name} {when}.png") }
}

/// "Stand 14:02": when the newest screenshot of game `key` was taken, while it is not on a server; a clock time, which
/// never ticks. None while it is on a server or has none.
pub fn as_of(key: &str, m: &crate::app::model::Model) -> Option<String> {
    let game = m.games.get(key).filter(|g| g.connection && !g.phase.on_server())?;
    let taken = m.moments.get(&game.key)?.first()?.taken;
    let at = local(taken);
    let when = if at.date_naive() == chrono::Local::now().date_naive() { crate::core::i18n::clock(at.time()) } else { crate::ui::instance_files::when(taken) };
    Some(trf("Stand %@", &[&when]))
}

/// The screenshots held in memory as the viewer shows them, each moment's view from the eyes before its view from above,
/// titled by when they were taken.
fn pictures(moments: &[Moment]) -> Vec<Picture> {
    moments
        .iter()
        .flat_map(|m| {
            let title = crate::ui::instance_files::when(m.taken);
            [&m.eyes, &m.above].map(|png| Picture { source: Source::Held(Held(Arc::clone(png))), stamp: m.taken, title: title.clone() })
        })
        .collect()
}

fn save_png(store: Store, suggested: String, png: Arc<[u8]>) {
    let ui = store.ui;
    spawn_in_window(async move {
        let title = tr("Screenshot speichern");
        let saved = off_ui(move || {
            let path = native::save_file(&title, &suggested, &tr("PNG-Bild"), "png", dirs::picture_dir())?;
            let path = if path.extension().is_some() { path } else { path.with_extension("png") };
            Some(crate::core::net::write_atomic(&path, &png).map(|()| name_of(&path)))
        })
        .await
        .flatten();
        match saved {
            None => {}
            Some(Ok(name)) => {
                toast::show(ui, trf("„%@“ gespeichert.", &[&name]), None);
            }
            Some(Err(e)) => {
                toast::show(ui, forms::explain_as(tr("Der Screenshot wurde nicht gespeichert."), &e).text, None);
            }
        }
    });
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn is_above(path: &Path) -> bool {
    stem(path).ends_with(ABOVE)
}

/// The shots of `account`, newest first, each moment's view from the eyes before its view from above.
fn order(mut found: Vec<Shot>, account: uuid::Uuid) -> Vec<Shot> {
    found.retain(|s| s.account == account);
    let moment = |s: &Shot| {
        let name = stem(&s.path);
        name.strip_suffix(ABOVE).map_or(name.clone(), str::to_owned)
    };
    found.sort_by(|a, b| moment(b).cmp(&moment(a)).then_with(|| is_above(&a.path).cmp(&is_above(&b.path))));
    found
}

#[derive(Clone, PartialEq)]
struct Place {
    dir: PathBuf,
    account: uuid::Uuid,
    name: String,
}

#[derive(Clone)]
struct List {
    loaded: bool,
    shots: Same<Vec<Shot>>,
    pictures: Same<Vec<Picture>>,
}

impl List {
    fn of(loaded: bool, shots: Vec<Shot>) -> Self {
        Self { loaded, pictures: Same(Arc::new(files(&shots))), shots: Same(Arc::new(shots)) }
    }
}

#[derive(PartialEq)]
struct Gallery {
    key: String,
}

impl Component for Gallery {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let key = self.key.clone();
        let held = use_hook(move || store.model.peek().games.get(&key).is_some_and(|g| g.connection));
        if held { Album { key: self.key.clone() }.into_element() } else { Files { key: self.key.clone() }.into_element() }
    }
}

#[derive(PartialEq)]
struct Album {
    key: String,
}

impl Component for Album {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut current = use_state(|| None::<Source>);
        let copying = use_state(|| false);
        use_drop(|| SHOWN.with_borrow_mut(|s| *s = None));
        let m = store.model.read();
        let game = m.games.get(&self.key);
        let name = game.and_then(|g| m.account(g.account_id)).map(|a| a.name.clone()).unwrap_or_default();
        let shown = Same(Arc::new(pictures(m.moments.get(&self.key).map_or(&[][..], Vec::as_slice))));
        drop(m);
        let (w, h) = *store.ui.window.read();
        let count = shown.0.len();
        let at = current.read().as_ref().and_then(|s| shown.0.iter().position(|p| &p.source == s)).unwrap_or(0);
        let heading_text = if name.is_empty() { tr("Screenshots") } else { trf("Screenshots von %@", &[&name]) };
        let width = (w - space::L * 2.).clamp(frame::SHEET_NARROW, WIDTH_MAX);
        let height = (h - frame::SHEET_TOP - space::L - CARD_CHROME).max(HEIGHT_MIN);
        let ui = store.ui;
        let done = Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(ui)) };
        let body = if count == 0 {
            SHOWN.with_borrow_mut(|s| *s = None);
            rect()
                .width(Size::fill())
                .height(Size::px(HEIGHT_MIN))
                .center()
                .spacing(space::S)
                .child(heading(&t, tr("Noch kein Screenshot")))
                .child(secondary(&t, tr("Der erste entsteht von selbst, sobald der Account in einer Welt ist. Ein Doppelklick auf die Kachel nimmt sofort einen auf.")).text_align(TextAlign::Center))
                .into_element()
        } else {
            let above = at % 2 == 1;
            let who = if above { trf("%@ · von oben", &[&name]) } else { trf("%@ · Blick", &[&name]) };
            let picked = shown.0.get(at).map(|p| (p.source.clone(), p.stamp));
            let save_png_of = {
                let (picked, name) = (picked.clone(), name.clone());
                EventHandler::new(move |_| {
                    if let Some((Source::Held(png), stamp)) = picked.clone() {
                        save_png(store, suggested(&name, stamp, above), png.0);
                    }
                })
            };
            SHOWN.with_borrow_mut(|s| *s = Some(save_png_of.clone()));
            let step = shown.clone();
            rect()
                .width(Size::fill())
                .height(Size::px(height))
                .child(Viewer {
                    shots: shown.clone(),
                    at,
                    who,
                    copying: *copying.read(),
                    on_step: EventHandler::new(move |by: isize| {
                        let next = (at as isize).saturating_add(by).clamp(0, step.0.len() as isize - 1) as usize;
                        current.set(step.0.get(next).map(|p| p.source.clone()));
                    }),
                    on_close: EventHandler::new(move |_| sheet::close(ui)),
                    on_copy: EventHandler::new(move |_| copy_image(store, copying, picked.clone().map(|(s, _)| s))),
                    on_save: Some(save_png_of),
                    on_reveal: None,
                    on_external: None,
                    on_delete: None,
                })
                .into_element()
        };
        Card::new("shots", heading_text).message(tr("Bleiben nur, solange Manycraft läuft.")).body(body).secondary(done).secondary_id("shots.done").width(width)
    }
}

#[derive(PartialEq)]
struct Files {
    key: String,
}

impl Component for Files {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let key = self.key.clone();
        let place = use_hook({
            let key = key.clone();
            move || {
                let m = store.model.peek();
                let game = m.games.get(&key)?;
                let made = m.instance(&game.instance_id)?;
                Some(Place { dir: made.dir(), account: game.account_id, name: m.account(game.account_id).map(|a| a.name.clone()).unwrap_or_default() })
            }
        });
        let mut list = use_state(|| List::of(false, Vec::new()));
        let mut current = use_state(|| None::<PathBuf>);
        let copying = use_state(|| false);
        let mut scan = use_state(|| 0u64);
        let _frame = use_preview(&key);
        let drawn = (store.model.peek().frames.get(&key).map(|f| f.seq), *store.ui.instance.changed.read());
        let found = place.clone();
        use_side_effect_with_deps(&drawn, move |_| {
            let Some(place) = found.clone() else {
                list.set(List::of(true, Vec::new()));
                return;
            };
            let ticket = scan.peek().wrapping_add(1);
            scan.set(ticket);
            spawn(async move {
                let shots = off_ui(move || order(shots::list(&place.dir), place.account)).await.unwrap_or_default();
                if *scan.peek() != ticket {
                    return;
                }
                let keep = current.peek().clone().filter(|p| shots.iter().any(|s| &s.path == p));
                current.set(keep.or_else(|| shots.first().map(|s| s.path.clone())));
                list.set(List::of(true, shots));
            });
        });

        let (w, h) = *store.ui.window.read();
        let shown = list.read().clone();
        let count = shown.shots.0.len();
        let at = current.read().as_ref().and_then(|p| shown.shots.0.iter().position(|s| &s.path == p)).unwrap_or(0);
        let name = place.as_ref().map(|p| p.name.clone()).unwrap_or_default();
        let heading = if name.is_empty() { tr("Screenshots") } else { trf("Screenshots von %@", &[&name]) };
        let width = (w - space::L * 2.).clamp(frame::SHEET_NARROW, WIDTH_MAX);
        let height = (h - frame::SHEET_TOP - space::L - CARD_CHROME).max(HEIGHT_MIN);
        let ui = store.ui;
        let done = Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(ui)) };
        let body = if !shown.loaded || count == 0 {
            let text = if shown.loaded { tr("Noch keine Screenshots. Doppelklick auf die Kachel nimmt einen auf.") } else { tr("Screenshots werden gesucht …") };
            rect().width(Size::fill()).height(Size::px(HEIGHT_MIN)).center().child(crate::view::kit::text::secondary(&t, text)).into_element()
        } else {
            let who = match shown.shots.0.get(at) {
                Some(s) if is_above(&s.path) => trf("%@ · von oben", &[&name]),
                Some(_) => trf("%@ · Blick", &[&name]),
                None => name.clone(),
            };
            let (step, copy, reveal, external, delete) = (shown.clone(), shown.clone(), shown.clone(), shown.clone(), shown.clone());
            let dir = place.as_ref().map(|p| p.dir.clone()).unwrap_or_default();
            rect()
                .width(Size::fill())
                .height(Size::px(height))
                .child(Viewer {
                    shots: shown.pictures.clone(),
                    at,
                    who,
                    copying: *copying.read(),
                    on_step: EventHandler::new(move |by: isize| {
                        let next = (at as isize).saturating_add(by).clamp(0, step.shots.0.len() as isize - 1) as usize;
                        current.set(step.shots.0.get(next).map(|s| s.path.clone()));
                    }),
                    on_close: EventHandler::new(move |_| sheet::close(ui)),
                    on_copy: EventHandler::new(move |_| copy_image(store, copying, copy.shots.0.get(at).map(|s| Source::File(s.path.clone())))),
                    on_save: None,
                    on_reveal: Some(EventHandler::new(move |_| {
                        if let Some(s) = reveal.shots.0.get(at) {
                            native::reveal(&s.path);
                        }
                    })),
                    on_external: Some(EventHandler::new(move |_| {
                        if let Some(s) = external.shots.0.get(at) {
                            native::open(&s.path);
                        }
                    })),
                    on_delete: Some(EventHandler::new(move |_| {
                        let Some(shot) = delete.shots.0.get(at).cloned() else { return };
                        let rest: Vec<Shot> = delete.shots.0.iter().filter(|s| s.path != shot.path).cloned().collect();
                        let next = rest.get(at).or_else(|| rest.last()).map(|s| s.path.clone());
                        let ticket = scan.peek().wrapping_add(1);
                        scan.set(ticket);
                        current.set(next);
                        list.set(List::of(true, rest));
                        discard(store, dir.clone(), shot.path);
                    })),
                })
                .into_element()
        };
        Card::new("shots", heading).body(body).secondary(done).secondary_id("shots.done").width(width)
    }
}

fn copy_image(store: Store, mut copying: State<bool>, source: Option<Source>) {
    let Some(source) = source else { return };
    if *copying.peek() {
        return;
    }
    copying.set(true);
    let ui = store.ui;
    spawn_in_window(async move {
        let done = off_ui(move || match &source {
            Source::File(path) => native::copy_image(path),
            Source::Held(png) => native::copy_png(&png.0),
        })
        .await;
        if let Some(mut c) = copying.try_write() {
            *c = false;
        }
        let text = match done {
            Some(Ok(())) => tr("Bild kopiert."),
            Some(Err(e)) => forms::explain_as(tr("Das Bild wurde nicht kopiert."), &e).text,
            None => tr("Das Bild wurde nicht kopiert."),
        };
        toast::show(ui, text, None);
    });
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn discard(store: Store, dir: PathBuf, path: PathBuf) {
    let name = name_of(&path);
    spawn_in_window(async move {
        let (from, file) = (dir.clone(), path.clone());
        match off_ui(move || trash::put(&from, &file)).await {
            Some(Ok(entry)) => offer_undo(store, dir, name, entry),
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht gelöscht.", &[&name]), &e).text, None);
            }
            None => {
                toast::show(store.ui, trf("„%@“ wurde nicht gelöscht.", &[&name]), None);
            }
        }
        actions::changed(store);
    });
}

fn offer_undo(store: Store, dir: PathBuf, name: String, entry: String) {
    let text = trf("„%@“ gelöscht.", &[&name]);
    toast::show_kept(store.ui, text, EventHandler::new(move |_| restore(store, dir.clone(), name.clone(), entry.clone())));
}

fn restore(store: Store, dir: PathBuf, name: String, entry: String) {
    spawn_in_window(async move {
        let (from, back) = (dir.clone(), entry.clone());
        match off_ui(move || trash::take_back(&from, &back)).await {
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht zurückgeholt.", &[&name]), &e).text, None);
                if !actions::permanent(&e) {
                    offer_undo(store, dir, name, entry);
                }
            }
            None => offer_undo(store, dir, name, entry),
        }
        actions::changed(store);
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use uuid::Uuid;

    use super::*;

    fn shot(account: u128, name: &str) -> Shot {
        Shot { account: Uuid::from_u128(account), path: PathBuf::from(name), taken: 0 }
    }

    #[test]
    fn newest_first_the_eyes_before_the_view_from_above_of_one_account() {
        let found = vec![
            shot(1, "2026-11-24_08.00.00_above.png"),
            shot(1, "2026-11-24_08.00.00.png"),
            shot(2, "2026-11-24_09.00.00.png"),
            shot(1, "2026-11-24_08.30.00_above.png"),
            shot(1, "2026-11-24_08.30.00.png"),
        ];
        let names: Vec<String> = order(found, Uuid::from_u128(1)).iter().map(|s| s.path.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["2026-11-24_08.30.00.png", "2026-11-24_08.30.00_above.png", "2026-11-24_08.00.00.png", "2026-11-24_08.00.00_above.png"]);
    }

    #[test]
    fn held_screenshots_show_newest_first_the_eyes_before_the_view_from_above() {
        let moment = |taken: i64| Moment { taken, eyes: Arc::from(vec![taken as u8]), above: Arc::from(vec![taken as u8 + 100]) };
        let shown = pictures(&[moment(2), moment(1)]);
        let bytes: Vec<u8> = shown.iter().map(|p| match &p.source { Source::Held(h) => h.0[0], Source::File(_) => 0 }).collect();
        assert_eq!(bytes, [2, 102, 1, 101]);
        assert_eq!(shown[0], shown[0].clone());
        let again = pictures(&[moment(2)]);
        assert_ne!(again[0], shown[0], "another picture with the same bytes is another picture");
    }
}