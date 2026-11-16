//! Snapshots before a change that may hurt an instance: asking once for large worlds, taking them, and putting one back.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::*;

use super::{forms, instance_files as files, instance_worlds, model, sheet, state, undo};
use crate::core::i18n::{tr, trf};
use crate::core::snapshot::{self, Reason, Snapshot};
use crate::core::worlds::Progress;

#[derive(Default)]
struct Taking(HashMap<String, Arc<Progress>>);

impl Global for Taking {}

/// How far the snapshot of instance `id` is, from 0 to 1, while one is taken.
pub fn progress(id: &str, cx: &App) -> Option<f32> {
    cx.try_global::<Taking>()?.0.get(id).map(|p| p.fraction())
}

fn running(id: &str, cx: &App) -> bool {
    model::get(cx).read(cx).games.values().any(|g| g.instance_id == id && g.phase.is_active())
}

/// Takes a snapshot of instance `id` for `reason`, then runs `then`. Asks once whether to keep taking them when its worlds are
/// large, and whether to go on without one when it fails; Abbrechen in either runs nothing.
pub fn before(id: &str, reason: Reason, cx: &mut App, then: impl FnOnce(&mut App) + 'static) {
    let Some(dir) = files::instance_dir(id, cx) else {
        then(cx);
        return;
    };
    if progress(id, cx).is_some() {
        undo::note(tr("Wird gerade gesichert …"), cx);
        return;
    }
    let id = id.to_owned();
    let check = cx.background_spawn({
        let dir = dir.clone();
        async move { snapshot::needs_choice(&dir) }
    });
    cx.spawn(async move |cx| {
        let large = check.await;
        cx.update(move |cx| match large {
            None => take(id, reason, cx, then),
            Some(size) => {
                let then = std::rc::Rc::new(std::cell::RefCell::new(Some(then)));
                sheet::in_window(cx, move |window, cx| {
                    let (skip, keep) = (then.clone(), then);
                    let (dir_skip, dir_keep, reason_keep) = (dir.clone(), dir, reason);
                    sheet::choose(
                        window,
                        cx,
                        trf("Die Welten sind %@ groß", &[&files::bytes(size)]),
                        trf("Vor Updates und vor dem Zurücksetzen sichert Manycraft diese Instanz. Das braucht jedes Mal etwa %@ Platz.", &[&files::bytes(size)]),
                        tr("Nicht sichern"),
                        move |_, cx| {
                            if let Err(e) = snapshot::set_large_choice(&dir_skip, false) {
                                eprintln!("snapshot choice: {e:#}");
                            }
                            if let Some(then) = skip.borrow_mut().take() {
                                then(cx);
                            }
                        },
                        tr("Sichern"),
                        move |_, cx| {
                            if let Err(e) = snapshot::set_large_choice(&dir_keep, true) {
                                eprintln!("snapshot choice: {e:#}");
                            }
                            if let Some(then) = keep.borrow_mut().take() {
                                take(id.clone(), reason_keep.clone(), cx, then);
                            }
                        },
                    );
                });
            }
        });
    })
    .detach();
}

fn take(id: String, reason: Reason, cx: &mut App, then: impl FnOnce(&mut App) + 'static) {
    let Some(made) = model::get(cx).read(cx).instance(&id).cloned() else {
        then(cx);
        return;
    };
    let progress = Arc::new(Progress::default());
    cx.default_global::<Taking>().0.insert(id.clone(), progress.clone());
    cx.refresh_windows();
    let ticker = cx.spawn({
        let progress = progress.clone();
        async move |cx| {
            while Arc::strong_count(&progress) > 2 {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                cx.update(|cx| cx.refresh_windows());
            }
        }
    });
    let task = cx.background_spawn(async move { snapshot::take(&made.dir(), &made, reason, &progress) });
    cx.spawn(async move |cx| {
        let result = task.await;
        drop(ticker);
        cx.update(move |cx| {
            cx.default_global::<Taking>().0.remove(&id);
            cx.refresh_windows();
            instance_worlds::changed(&id, None, cx);
            match result {
                Ok(_) => then(cx),
                Err(e) => {
                    let problem = forms::explain_as(tr("Nicht gesichert."), &e);
                    let then = std::rc::Rc::new(std::cell::RefCell::new(Some(then)));
                    sheet::in_window(cx, move |window, cx| {
                        sheet::confirm_danger(window, cx, tr("Sicherung fehlgeschlagen"), trf("%@ Ohne Sicherung lässt sich die Änderung an den Welten nicht zurücknehmen.", &[&problem.text]), tr("Ohne Sicherung fortfahren"), |_| None, move |_, cx| {
                            if let Some(then) = then.borrow_mut().take() {
                                then(cx);
                            }
                        });
                    });
                }
            }
        });
    })
    .detach();
}

/// Asks whether instance `id` should go back to `snap`, then puts it back.
pub fn ask_restore(id: &str, snap: Snapshot, window: &mut Window, cx: &mut App) {
    if sheet::busy(window, cx) {
        return;
    }
    let blocked_id = id.to_owned();
    let id = id.to_owned();
    let message = trf("Welten, Mods und Einstellungen gehen auf den Stand „%@“ zurück (%@). Was jetzt da ist, liegt danach 7 Tage im Papierkorb.", &[&snap.reason.label(), &snap.version_line]);
    sheet::confirm(window, cx, trf("Stand von %@ wiederherstellen?", &[&files::when(snap.at)]), message, tr("Wiederherstellen"), move |cx| running(&blocked_id, cx).then(|| tr("Stoppe zuerst die laufenden Spiele.")), move |_, cx| restore(&id, &snap, cx));
}

fn restore(id: &str, snap: &Snapshot, cx: &mut App) {
    if running(id, cx) {
        undo::note(tr("Stoppe zuerst die laufenden Spiele."), cx);
        return;
    }
    let (instance, snap_id) = (id.to_owned(), snap.id.clone());
    let task = cx.background_spawn(async move { snapshot::restore(&instance, &snap_id) });
    let id = id.to_owned();
    let when = files::when(snap.at);
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(done) => {
                    model::get(cx).update(cx, |m, cx| {
                        if let Some(slot) = m.instances.iter_mut().find(|made| made.id == done.instance.id) {
                            *slot = done.instance;
                        }
                        cx.notify();
                    });
                    undo::note(trf("Stand von %@ wiederhergestellt.", &[&when]), cx);
                }
                Err(e) => undo::note(forms::explain_as(tr("Nicht wiederhergestellt."), &e).text, cx),
            }
            instance_worlds::changed(&id, None, cx);
            state::reload(cx);
        });
    })
    .detach();
}
