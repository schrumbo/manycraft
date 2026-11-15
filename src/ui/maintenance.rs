//! Upkeep of an instance from outside its page: reset to its start, a newer loader, and the undo of those and of renames and switches.

use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{ResetInstance, UpdateLoader};
use super::remote::{self, InstanceEdit};
use super::undo::{self, Edit, Kind};
use super::{forms, model, sheet, state};
use crate::core::i18n::{tr, trf};
use crate::core::install;
use crate::core::instance::Instance;

/// Registers the reset and the loader update, which the page, the sidebar menu and the palette start.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &ResetInstance, cx| {
        let id = a.0.clone();
        sheet::in_window(cx, move |window, cx| ask_reset(&id, window, cx));
    });
    cx.on_action(|a: &UpdateLoader, cx| look_for_loader(&a.0, cx));
}

fn running(id: &str, cx: &App) -> bool {
    model::get(cx).read(cx).games.values().any(|g| g.instance_id == id && g.phase.is_active())
}

fn put(saved: Instance, cx: &mut App) {
    model::get(cx).update(cx, |m, cx| {
        if let Some(slot) = m.instances.iter_mut().find(|made| made.id == saved.id) {
            *slot = saved;
        }
        cx.notify();
    });
}

fn ask_reset(id: &str, window: &mut Window, cx: &mut App) {
    if sheet::busy(window, cx) {
        return;
    }
    let Some(name) = model::get(cx).read(cx).instance(id).map(|made| made.name.clone()) else { return };
    if running(id, cx) {
        undo::note(tr("Stoppe zuerst die laufenden Spiele."), cx);
        return;
    }
    let id = id.to_owned();
    sheet::ask(
        window,
        cx,
        trf("„%@“ zurücksetzen?", &[&name]),
        tr("Mods, Konfiguration, Ressourcen- und Shader-Pakete gehen auf den Stand von der Erstellung zurück. Welten, Screenshots, Serverliste und Optionen bleiben. Solange Manycraft offen ist, kannst du das rückgängig machen."),
        tr("Zurücksetzen"),
        move |_, cx| reset(&id, cx),
    );
}

fn reset(id: &str, cx: &mut App) {
    if running(id, cx) {
        undo::note(tr("Stoppe zuerst die laufenden Spiele."), cx);
        return;
    }
    let id = id.to_owned();
    let task = cx.background_spawn(async move { remote::reset_instance(&id) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(done) => {
                    let (name, version) = (done.after.name.clone(), done.after.game_version.clone());
                    put(done.after.clone(), cx);
                    match done.missing.as_slice() {
                        [] => {}
                        [one] => undo::note(trf("„%@“ gibt es nicht für %@ und blieb draußen.", &[one, &version]), cx),
                        many => undo::note(trf("%lld Mods gibt es nicht für %@ und blieben draußen.", &[&many.len(), &version]), cx),
                    }
                    undo::offer(Kind::Reset { instance_id: done.after.id.clone(), name, entry: done.entry, before: done.before, after: done.after }, cx);
                }
                Err(e) => undo::note(forms::explain_as(tr("Nicht zurückgesetzt."), &e).text, cx),
            }
            state::reload(cx);
        });
    })
    .detach();
}

/// Puts back what a reset took away, unless the instance's mods, version or loader changed since; says why when it cannot,
/// and offers a retry in the old place only when that may help.
pub fn restore_reset(id: &str, name: String, entry: String, before: Instance, after: Instance, at: Instant, cx: &mut App) {
    let again = |name: String, entry: String, before: Instance, after: Instance| Kind::Reset { instance_id: id.to_owned(), name, entry, before, after };
    let current = model::get(cx).read(cx).instance(id).cloned();
    let Some(current) = current else {
        undo::note(tr("Diese Instanz gibt es nicht mehr."), cx);
        state::reload(cx);
        return;
    };
    if !remote::same_files(&current, &after) {
        undo::note(trf("„%@“ lässt sich nicht zurückholen: Mods oder Version wurden inzwischen geändert.", &[&name]), cx);
        state::reload(cx);
        return;
    }
    if running(id, cx) {
        undo::note(tr("Gerade laufen Spiele. Stoppe sie zuerst."), cx);
        undo::retry(again(name, entry, before, after), at, cx);
        state::reload(cx);
        return;
    }
    let (instance, kept) = (id.to_owned(), (before.clone(), after.clone(), entry.clone()));
    let task = cx.background_spawn(async move { remote::restore_reset(&instance, &kept.0, &kept.1, &kept.2) });
    let id = id.to_owned();
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(saved) => put(saved, cx),
                Err(e) => {
                    undo::note(forms::explain_as(tr("Nicht zurückgeholt."), &e).text, cx);
                    if !undo::permanent(&e) {
                        undo::retry(Kind::Reset { instance_id: id, name, entry, before, after }, at, cx);
                    }
                }
            }
            state::reload(cx);
        });
    })
    .detach();
}

/// Takes back a rename, a version or loader switch or a loader update by writing the old fields again, unless one of them changed
/// since. While a game runs, only a change that touches no mods and no game files is taken back.
pub fn restore_edit(id: &str, name: String, what: Edit, before: InstanceEdit, after: InstanceEdit, at: Instant, cx: &mut App) {
    let again = |name: String, what: Edit, before: InstanceEdit, after: InstanceEdit| Kind::Edited { instance_id: id.to_owned(), name, what, before, after };
    if model::get(cx).read(cx).instance(id).is_none() {
        undo::note(tr("Diese Instanz gibt es nicht mehr."), cx);
        state::reload(cx);
        return;
    }
    let moves = before.game_version != after.game_version || !before.loader.same_kind(&after.loader) || before.mods != after.mods;
    if moves && running(id, cx) {
        undo::note(tr("Gerade laufen Spiele. Stoppe sie zuerst."), cx);
        undo::retry(again(name, what, before, after), at, cx);
        state::reload(cx);
        return;
    }
    let task = cx.background_spawn({
        let (instance, base, edit) = (id.to_owned(), after.clone(), before.clone());
        async move { remote::save_instance(&instance, &base, edit) }
    });
    let id = id.to_owned();
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(saved) => put(saved, cx),
                Err(e) => {
                    undo::note(forms::explain_as(tr("Nicht gespeichert."), &e).text, cx);
                    if !undo::permanent(&e) {
                        undo::retry(Kind::Edited { instance_id: id, name, what, before, after }, at, cx);
                    }
                }
            }
            state::reload(cx);
        });
    })
    .detach();
}

fn look_for_loader(id: &str, cx: &mut App) {
    let Some(made) = model::get(cx).read(cx).instance(id).cloned() else {
        undo::note(tr("Diese Instanz gibt es nicht mehr."), cx);
        return;
    };
    let Some(current) = made.loader.version().map(str::to_owned) else {
        let text = if made.loader.takes_mods() { tr("Der Loader folgt schon der neuesten Version.") } else { tr("Vanilla hat keinen Loader.") };
        undo::note(text, cx);
        return;
    };
    let id = id.to_owned();
    let task = cx.background_spawn({
        let (loader, version) = (made.loader.clone(), made.game_version.clone());
        async move { remote::newest_loader(&loader, &version) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(Some(newest)) if install::is_newer(&newest, &current) => sheet::in_window(cx, move |window, cx| ask_loader(&id, &newest, window, cx)),
            Ok(_) => undo::note(trf("%@ %@ ist aktuell.", &[&made.loader.name(), &current]), cx),
            Err(e) => undo::note(forms::explain_as(tr("Loader-Liste nicht geladen."), &e).text, cx),
        });
    })
    .detach();
}

/// Asks whether instance `id` should move to loader version `to`, then switches. Running games keep running; the change applies on the next start.
pub fn ask_loader(id: &str, to: &str, window: &mut Window, cx: &mut App) {
    if sheet::busy(window, cx) {
        return;
    }
    let Some(made) = model::get(cx).read(cx).instance(id).cloned() else { return };
    let message = if running(id, cx) { tr("Laufende Spiele bleiben unverändert. Der neue Loader gilt ab dem nächsten Start.") } else { tr("Mods und Welten bleiben. Der neue Loader wird beim nächsten Start installiert.") };
    let (id, to) = (id.to_owned(), to.to_owned());
    sheet::confirm(window, cx, trf("Auf %@ %@ aktualisieren?", &[&made.loader.name(), &to]), message, tr("Aktualisieren"), |_| None, move |_, cx| update_loader(&id, &to, cx));
}

fn update_loader(id: &str, to: &str, cx: &mut App) {
    let Some(made) = model::get(cx).read(cx).instance(id).cloned() else {
        undo::note(tr("Diese Instanz gibt es nicht mehr."), cx);
        return;
    };
    let base = InstanceEdit::of(&made);
    let edit = InstanceEdit { loader: made.loader.with_version(Some(to.to_owned())), ..base.clone() };
    let (id, to) = (id.to_owned(), to.to_owned());
    let task = cx.background_spawn({
        let base = base.clone();
        async move { remote::save_instance(&id, &base, edit) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(saved) => {
                    let (name, target, after) = (saved.name.clone(), format!("{} {to}", saved.loader.name()), InstanceEdit::of(&saved));
                    let instance_id = saved.id.clone();
                    put(saved, cx);
                    undo::offer(Kind::Edited { instance_id, name, what: Edit::Updated(target), before: base, after }, cx);
                }
                Err(e) => undo::note(forms::explain_as(tr("Nicht gespeichert."), &e).text, cx),
            }
            state::reload(cx);
        });
    })
    .detach();
}
