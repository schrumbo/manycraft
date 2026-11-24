//! The screenshots of a game without a window, shown large in a sheet: the eyes and the view from above, newest first.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::core::shots::{self, Shot};
use crate::core::trash;
use crate::ui::forms;
use crate::view::instance::actions;
use crate::view::instance::shots_parts::Same;
use crate::view::instance::shots_view::Viewer;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::native;
use crate::view::store::{Store, use_preview, use_store};
use crate::view::theme::{frame, space};

const ABOVE: &str = "_above";
const WIDTH_MAX: f32 = 1760.;
const HEIGHT_MIN: f32 = 280.;
const CARD_CHROME: f32 = 156.;

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
}

#[derive(PartialEq)]
struct Gallery {
    key: String,
}

impl Component for Gallery {
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
        let mut list = use_state(|| List { loaded: false, shots: Same(Arc::new(Vec::new())) });
        let mut current = use_state(|| None::<PathBuf>);
        let copying = use_state(|| false);
        let mut scan = use_state(|| 0u64);
        let _frame = use_preview(&key);
        let drawn = (store.model.peek().frames.get(&key).map(|f| f.seq), *store.ui.instance.changed.read());
        let found = place.clone();
        use_side_effect_with_deps(&drawn, move |_| {
            let Some(place) = found.clone() else {
                list.set(List { loaded: true, shots: Same(Arc::new(Vec::new())) });
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
                list.set(List { loaded: true, shots: Same(Arc::new(shots)) });
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
                    shots: shown.shots.clone(),
                    at,
                    who,
                    copying: *copying.read(),
                    on_step: EventHandler::new(move |by: isize| {
                        let next = (at as isize).saturating_add(by).clamp(0, step.shots.0.len() as isize - 1) as usize;
                        current.set(step.shots.0.get(next).map(|s| s.path.clone()));
                    }),
                    on_close: EventHandler::new(move |_| sheet::close(ui)),
                    on_copy: EventHandler::new(move |_| copy_image(store, copying, copy.shots.0.get(at).map(|s| s.path.clone()))),
                    on_reveal: EventHandler::new(move |_| {
                        if let Some(s) = reveal.shots.0.get(at) {
                            native::reveal(&s.path);
                        }
                    }),
                    on_external: EventHandler::new(move |_| {
                        if let Some(s) = external.shots.0.get(at) {
                            native::open(&s.path);
                        }
                    }),
                    on_delete: EventHandler::new(move |_| {
                        let Some(shot) = delete.shots.0.get(at).cloned() else { return };
                        let rest: Vec<Shot> = delete.shots.0.iter().filter(|s| s.path != shot.path).cloned().collect();
                        let next = rest.get(at).or_else(|| rest.last()).map(|s| s.path.clone());
                        let ticket = scan.peek().wrapping_add(1);
                        scan.set(ticket);
                        current.set(next);
                        list.set(List { loaded: true, shots: Same(Arc::new(rest)) });
                        discard(store, dir.clone(), shot.path);
                    }),
                })
                .into_element()
        };
        Card::new("shots", heading).body(body).secondary(done).secondary_id("shots.done").width(width)
    }
}

fn copy_image(store: Store, mut copying: State<bool>, path: Option<PathBuf>) {
    let Some(path) = path else { return };
    if *copying.peek() {
        return;
    }
    copying.set(true);
    let ui = store.ui;
    spawn_in_window(async move {
        let done = off_ui(move || native::copy_image(&path)).await;
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
}