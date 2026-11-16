use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use freya::prelude::*;

use super::{Busy, actions};
use crate::app::event::Event;
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::core::snapshot::{self, Reason, Snapshot};
use crate::core::worlds::Progress;
use crate::ui::forms;
use crate::ui::instance_files::{bytes, when};
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::{after, off_ui, toast};
use crate::view::store::Store;
use crate::view::theme::frame;
use crate::view::ui::Ui;

const TICK: Duration = Duration::from_millis(250);

type Then = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

fn once(then: impl FnOnce() + 'static) -> Then {
    Rc::new(RefCell::new(Some(Box::new(then))))
}

fn run(then: &Then) {
    if let Some(then) = then.borrow_mut().take() {
        then();
    }
}

type Back = Option<Rc<dyn Fn()>>;

fn settle(ui: Ui, back: &Back) {
    match back {
        Some(back) => back(),
        None => sheet::close(ui),
    }
}

fn release(store: Store, id: &str) {
    let mut busy = store.ui.instance.busy;
    busy.write().remove(id);
}

/// Takes a snapshot of instance `id` for `reason`, then runs `then`. Asks once whether to keep taking them when its worlds
/// are large, and whether to go on without one when it fails; Abbrechen in either runs nothing.
pub fn before(store: Store, id: &str, reason: Reason, then: impl FnOnce() + 'static) {
    start(store, id, reason, None, then);
}

/// Same as [`before`], but whatever its questions answer, `back` runs so the sheet that asked comes back.
pub fn before_returning(store: Store, id: &str, reason: Reason, back: impl Fn() + 'static, then: impl FnOnce() + 'static) {
    start(store, id, reason, Some(Rc::new(back)), then);
}

fn start(store: Store, id: &str, reason: Reason, back: Back, then: impl FnOnce() + 'static) {
    let Some(dir) = store.model.peek().instance(id).map(Instance::dir) else {
        then();
        return;
    };
    if store.ui.instance.busy.peek().contains_key(id) {
        toast::show(store.ui, tr("Wird gerade gesichert …"), None);
        return;
    }
    let id = id.to_owned();
    let then = once(then);
    let mut busy = store.ui.instance.busy;
    busy.write().insert(id.clone(), Busy { text: tr("Wird gesichert …"), progress: None });
    spawn_in_window(async move {
        let check = dir.clone();
        let large = off_ui(move || snapshot::needs_choice(&check)).await.flatten();
        let Some(size) = large else {
            take(store, id, reason, back, then);
            return;
        };
        let (skip, keep) = (then.clone(), then);
        let (dir_skip, dir_keep) = (dir.clone(), dir);
        let (back_skip, back_keep, back_esc) = (back.clone(), back.clone(), back.clone());
        let ui = store.ui;
        release(store, &id);
        let card = Card::new("instance.snapshot.large", trf("Die Welten sind %@ groß", &[&bytes(size)]))
            .message(trf("Vor Updates und vor dem Zurücksetzen sichert Manycraft diese Instanz. Das braucht jedes Mal etwa %@ Platz.", &[&bytes(size)]))
            .secondary(Primary {
                label: tr("Nicht sichern"),
                disabled: None,
                danger: false,
                busy: false,
                run: EventHandler::new(move |_| {
                    settle(ui, &back_skip);
                    let dir = dir_skip.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = snapshot::set_large_choice(&dir, false) {
                            eprintln!("snapshot choice: {e:#}");
                        }
                    });
                    run(&skip);
                }),
            })
            .primary(Primary {
                label: tr("Sichern"),
                disabled: None,
                danger: false,
                busy: false,
                run: EventHandler::new(move |_| {
                    settle(ui, &back_keep);
                    let dir = dir_keep.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = snapshot::set_large_choice(&dir, true) {
                            eprintln!("snapshot choice: {e:#}");
                        }
                    });
                    take(store, id.clone(), reason.clone(), back_keep.clone(), keep.clone());
                }),
            })
            .on_escape(move |_| settle(ui, &back_esc));
        sheet::open(ui, card);
    });
}

fn take(store: Store, id: String, reason: Reason, back: Back, then: Then) {
    let Some(made) = store.model.peek().instance(&id).cloned() else {
        release(store, &id);
        run(&then);
        return;
    };
    let mut busy = store.ui.instance.busy;
    busy.write().insert(id.clone(), Busy { text: tr("Wird gesichert …"), progress: Some(0.) });
    let progress = Arc::new(Progress::default());
    let ticker = progress.clone();
    let tick_id = id.clone();
    spawn_in_window(async move {
        while Arc::strong_count(&ticker) > 1 {
            after(TICK).await;
            let done = ticker.fraction();
            if let Some(b) = busy.write().get_mut(&tick_id) {
                b.progress = Some(done);
            }
        }
    });
    spawn_in_window(async move {
        let result = off_ui(move || snapshot::take(&made.dir(), &made, reason, &progress)).await;
        busy.write().remove(&id);
        actions::changed(store);
        match result {
            Some(Ok(_)) => run(&then),
            failed => {
                let text = match failed {
                    Some(Err(e)) => forms::explain_as(tr("Nicht gesichert."), &e).text,
                    _ => tr("Nicht gesichert."),
                };
                let ui = store.ui;
                let (go, stop, esc) = (back.clone(), back.clone(), back);
                let card = Card::new("instance.snapshot.failed", tr("Sicherung fehlgeschlagen"))
                    .message(trf("%@ Ohne Sicherung lässt sich die Änderung an den Welten nicht zurücknehmen.", &[&text]))
                    .width(frame::SHEET_NARROW)
                    .secondary(Primary { label: tr("Abbrechen"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| settle(ui, &stop)) })
                    .secondary_id("instance.snapshot.failed.cancel")
                    .primary(Primary {
                        label: tr("Ohne Sicherung fortfahren"),
                        disabled: None,
                        danger: true,
                        busy: false,
                        run: EventHandler::new(move |_| {
                            settle(ui, &go);
                            run(&then);
                        }),
                    })
                    .on_escape(move |_| settle(ui, &esc));
                sheet::open(ui, card);
            }
        }
    });
}

/// Asks whether instance `id` should go back to `snap`, then puts it back.
pub fn ask_restore(store: Store, id: &str, snap: Snapshot) {
    if store.ui.instance.busy.peek().contains_key(id) {
        return;
    }
    let message = trf("Welten, Mods und Einstellungen gehen auf den Stand „%@“ zurück (%@). Was jetzt da ist, liegt danach 7 Tage im Papierkorb.", &[&snap.reason.label(), &snap.version_line]);
    let id = id.to_owned();
    sheet::confirm(store.ui, "instance.snapshot.restore", trf("Stand von %@ wiederherstellen?", &[&when(snap.at)]), message, tr("Wiederherstellen"), false, move |_| restore(store, &id, &snap));
}

fn restore(store: Store, id: &str, snap: &Snapshot) {
    if let Some(why) = super::blocked(store, id) {
        toast::show(store.ui, why, None);
        return;
    }
    let mut busy = store.ui.instance.busy;
    busy.write().insert(id.to_owned(), Busy { text: tr("Wird wiederhergestellt …"), progress: None });
    let (instance, snap_id, id) = (id.to_owned(), snap.id.clone(), id.to_owned());
    let at = when(snap.at);
    spawn_in_window(async move {
        let result = off_ui(move || snapshot::restore(&instance, &snap_id)).await;
        busy.write().remove(&id);
        match result {
            Some(Ok(done)) => {
                let name = done.instance.name.clone();
                store.dispatch(Event::InstanceChanged(done.instance));
                offer_unrestore(store, id, name, at, done.before, done.trashed);
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Nicht wiederhergestellt."), &e).text, None);
            }
            None => {
                toast::show(store.ui, tr("Nicht wiederhergestellt."), None);
            }
        }
        actions::changed(store);
        store.reload(true);
    });
}

fn offer_unrestore(store: Store, id: String, name: String, at: String, before: Instance, trashed: Vec<String>) {
    let text = trf("Stand von %@ wiederhergestellt.", &[&at]);
    toast::show(store.ui, text, Some(EventHandler::new(move |_| unrestore(store, id.clone(), name.clone(), at.clone(), before.clone(), trashed.clone()))));
}

fn unrestore(store: Store, id: String, name: String, at: String, before: Instance, trashed: Vec<String>) {
    if super::blocked(store, &id).is_some() {
        toast::show(store.ui, tr("Gerade laufen Spiele. Stoppe sie zuerst."), None);
        offer_unrestore(store, id, name, at, before, trashed);
        return;
    }
    let kept = (before.clone(), trashed.clone());
    spawn_in_window(async move {
        let result = off_ui(move || snapshot::unrestore(&kept.0, &kept.1)).await;
        match result {
            Some(Ok(made)) => store.dispatch(Event::InstanceChanged(made)),
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Nicht zurückgeholt."), &e).text, None);
                if !actions::permanent(&e) {
                    offer_unrestore(store, id, name, at, before, trashed);
                }
            }
            None => offer_unrestore(store, id, name, at, before, trashed),
        }
        actions::changed(store);
        store.reload(true);
    });
}
