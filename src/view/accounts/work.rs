use std::cell::RefCell;
use std::rc::Rc;

use freya::prelude::*;
use uuid::Uuid;

use super::{View, view_of};
use crate::core::account::AccountKind;
use crate::core::i18n::{tr, trf};
use crate::ui::forms::{self, Problem};
use crate::ui::remote::{self, Removed};
use crate::view::kit::{sheet, toast};
use crate::view::store::Store;

/// Runs `job` on its own thread and hands its result to `done` on the UI thread.
pub fn off<T: Send + 'static>(name: &str, job: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    let (tx, rx) = async_channel::bounded(1);
    let _ = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
        let _ = tx.send_blocking(job());
    });
    spawn_in_window(async move {
        if let Ok(value) = rx.recv().await {
            done(value);
        }
    });
}

fn fail(store: Store, problem: Problem) {
    let mut p = store.ui.accounts.problem;
    p.set(Some(problem));
}

/// Looks for Prism Launcher's accounts once, so the page can offer to take them over.
pub fn look_for_prism(store: Store) {
    off("prism-accounts", || remote::prism_accounts().map_or(0, |found| found.len()), move |found| {
        let mut prism = store.ui.accounts.prism;
        if prism.peek().found != found {
            prism.write().found = found;
        }
    });
}

/// Saves Prism's offline accounts and lists the Microsoft ones that must sign in.
pub fn import_prism(store: Store) {
    let mut prism = store.ui.accounts.prism;
    if prism.peek().busy {
        return;
    }
    prism.write().busy = true;
    let mut problem = store.ui.accounts.problem;
    problem.set(None);
    off("prism-import", remote::import_prism_accounts, move |result| {
        let mut prism = store.ui.accounts.prism;
        prism.write().busy = false;
        match result {
            Ok(done) => {
                prism.write().done = Some(done);
                store.reload(true);
            }
            Err(e) => fail(store, forms::explain_as(tr("Aus Prism nicht übernommen."), &e)),
        }
    });
}

/// Edits the proxy of `ids`: one account in the list right in its row, else in a sheet.
pub fn edit_proxy(store: Store, ids: Vec<Uuid>) {
    let m = store.model.peek();
    let mut shared = store.ui.accounts;
    match ids.as_slice() {
        [] => {}
        [one] if view_of(&m, shared) == View::List => shared.editing.set(Some(*one)),
        _ => {
            let current: Vec<Option<String>> = m.accounts.iter().filter(|a| ids.contains(&a.id)).map(|a| a.proxy.clone()).collect();
            let start = match current.first() {
                Some(first) if current.iter().all(|p| p == first) => first.clone().unwrap_or_default(),
                _ => String::new(),
            };
            drop(m);
            sheet::open(store.ui, super::list::ProxySheet { ids, start });
        }
    }
}

/// Whether a proxy check is running, passed, or failed and why.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    Idle,
    Running,
    Passed,
    Failed(Problem),
}

/// Checks that `proxy` reaches the internet and reports into `check`, unless `field` holds another proxy by then.
pub fn test_proxy(proxy: String, mut check: State<Check>, field: State<String>) {
    if proxy.is_empty() || forms::proxy_problem(&proxy).is_some() || *check.peek() == Check::Running {
        return;
    }
    check.set(Check::Running);
    let asked = proxy.clone();
    off("proxy-test", move || remote::test_proxy(&asked).map_err(|e| Problem { text: tr("Über diesen Proxy kommt kein Internet. Prüfe Adresse, Port und Zugangsdaten."), detail: forms::explain(&e).detail }), move |result| {
        let same = field.try_peek().is_some_and(|v| v.trim() == proxy);
        let running = check.try_peek().is_some_and(|c| *c == Check::Running);
        if !running {
            return;
        }
        if let Some(mut c) = check.try_write() {
            *c = if same { result.map_or_else(Check::Failed, |()| Check::Passed) } else { Check::Idle };
        }
    });
}

/// Saves `proxy` (empty for none) on every account of `ids`, then offers to put the old ones back.
pub fn save_proxy(store: Store, ids: Vec<Uuid>, proxy: String, done: impl FnOnce(bool) + 'static) {
    let before: Vec<(Uuid, Option<String>)> = store.model.peek().accounts.iter().filter(|a| ids.contains(&a.id)).map(|a| (a.id, a.proxy.clone())).collect();
    let value = Some(proxy).filter(|p| !p.is_empty());
    let set: Vec<(Uuid, Option<String>)> = ids.iter().map(|id| (*id, value.clone())).collect();
    let count = before.len();
    off("proxy-save", move || remote::set_proxies(&set), move |result| {
        store.reload(true);
        match result {
            Ok(()) => {
                done(true);
                let text = match (count, value.is_some()) {
                    (1, true) => tr("Proxy gesichert"),
                    (1, false) => tr("Proxy entfernt"),
                    (_, true) => trf("Proxy für %lld Accounts gesichert", &[&count]),
                    (_, false) => trf("Proxy für %lld Accounts entfernt", &[&count]),
                };
                let back = Rc::new(RefCell::new(Some(before)));
                toast::show(store.ui, text, Some(EventHandler::new(move |_| {
                    let Some(before) = back.borrow_mut().take() else { return };
                    off("proxy-undo", move || remote::set_proxies(&before), move |result| {
                        if let Err(e) = result {
                            fail(store, forms::explain_as(tr("Der alte Proxy wurde nicht zurückgeholt."), &e));
                        }
                        store.reload(true);
                    });
                })));
            }
            Err(e) => {
                done(false);
                fail(store, forms::explain_as(tr("Der Proxy wurde nicht gespeichert."), &e));
            }
        }
    });
}

/// Asks before removing the accounts of `ids`, naming their batches and what happens to a Microsoft sign-in.
pub fn ask_remove(store: Store, ids: Vec<Uuid>) {
    let m = store.model.peek();
    let chosen: Vec<_> = m.accounts.iter().filter(|a| ids.contains(&a.id)).cloned().collect();
    if chosen.is_empty() {
        return;
    }
    let mut batches: Vec<String> = m.batches.iter().filter(|b| chosen.iter().any(|a| b.accounts.contains(&a.id))).map(|b| b.name.clone()).collect();
    batches.dedup();
    let microsoft = chosen.iter().any(|a| a.kind == AccountKind::Microsoft);
    let one = chosen.len() == 1;
    let title = if one { trf("„%@“ entfernen?", &[&chosen[0].name]) } else { trf("%lld Accounts entfernen?", &[&chosen.len()]) };
    let mut message = match (batches.is_empty(), one) {
        (true, _) => String::new(),
        (false, true) => trf("Der Account wird auch aus %@ entfernt.", &[&batches.join(", ")]),
        (false, false) => trf("Die Accounts werden auch aus %@ entfernt.", &[&batches.join(", ")]),
    };
    if microsoft {
        if !message.is_empty() {
            message.push(' ');
        }
        message.push_str(&if one { tr("Die gespeicherte Anmeldung wird gelöscht; zum Wiederherstellen meldest du dich neu an.") } else { tr("Gespeicherte Anmeldungen werden gelöscht; zum Wiederherstellen meldest du dich neu an.") });
    }
    if message.is_empty() {
        message = if one { tr("Die Spielordner dieses Accounts bleiben erhalten.") } else { tr("Die Spielordner dieser Accounts bleiben erhalten.") };
    }
    let action = if one { tr("Entfernen") } else { trf("%lld entfernen", &[&chosen.len()]) };
    let ids: Vec<Uuid> = chosen.iter().map(|a| a.id).collect();
    drop(m);
    sheet::confirm(store.ui, "accounts.remove", title, message, action, true, move |_| remove(store, ids.clone()));
}

fn remove(store: Store, ids: Vec<Uuid>) {
    let mut shared = store.ui.accounts;
    shared.removing.write().extend(ids.iter().copied());
    shared.problem.set(None);
    let asked = ids.clone();
    off("remove-accounts", move || {
        let mut removed: Vec<Removed> = Vec::new();
        for id in &asked {
            match remote::remove_account(*id) {
                Ok(Some(r)) => removed.push(r),
                Ok(None) => {}
                Err(e) => return (removed, Some(e)),
            }
        }
        (removed, None)
    }, move |(removed, error)| {
        let mut shared = store.ui.accounts;
        shared.removing.write().retain(|i| !ids.contains(i));
        if let Some(e) = error {
            fail(store, forms::explain_as(tr("Der Account wurde nicht vollständig entfernt."), &e));
        }
        store.reload(true);
        if !removed.is_empty() {
            offer_undo(store, removed);
        }
    });
}

fn offer_undo(store: Store, removed: Vec<Removed>) {
    let text = match removed.as_slice() {
        [one] => trf("„%@“ entfernt", &[&one.account.name]),
        many => trf("%lld Accounts entfernt", &[&many.len()]),
    };
    let kept = Rc::new(RefCell::new(Some(removed)));
    toast::show_kept(store.ui, text, EventHandler::new(move |_| {
        let Some(removed) = kept.borrow_mut().take() else { return };
        off("restore-accounts", move || {
            let mut left = Vec::new();
            let mut error = None;
            for r in removed.into_iter().rev() {
                if let Err(e) = remote::restore_account(&r) {
                    error.get_or_insert(e);
                    left.push(r);
                }
            }
            left.reverse();
            (left, error)
        }, move |(left, error)| {
            if let Some(e) = error {
                fail(store, forms::explain_as(tr("Der Account wurde nicht vollständig zurückgeholt."), &e));
                if !left.is_empty() {
                    offer_undo(store, left);
                }
            }
            store.reload(true);
        });
    }));
}

/// Takes the accounts of each batch in `groups` out of it, then offers to put them back.
pub fn take_out(store: Store, groups: Vec<(String, Vec<Uuid>)>) {
    let n: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
    let name = match groups.as_slice() {
        [(batch, _)] => store.model.peek().batch(batch).map(|b| b.name.clone()),
        _ => None,
    };
    let work = groups.clone();
    off("leave-batch", move || work.iter().try_for_each(|(batch, ids)| remote::leave_batch(batch, ids)), move |result| {
        store.reload(true);
        match result {
            Ok(()) => {
                let text = match (n, name) {
                    (1, Some(name)) => trf("1 Account aus „%@“ genommen", &[&name]),
                    (n, Some(name)) => trf("%1$lld Accounts aus „%2$@“ genommen", &[&n, &name]),
                    (1, None) => tr("1 Account aus seinem Batch genommen"),
                    (n, None) => trf("%lld Accounts aus ihren Batches genommen", &[&n]),
                };
                let back = Rc::new(RefCell::new(Some(groups)));
                let id = toast::show(store.ui, text, Some(EventHandler::new(move |_| {
                    let Some(groups) = back.borrow_mut().take() else { return };
                    off("join-batch", move || groups.iter().try_for_each(|(batch, ids)| remote::join_batch(batch, ids).map(|_| ())), move |result| {
                        if let Err(e) = result {
                            toast::show(store.ui, forms::explain_as(tr("Die Accounts wurden nicht wieder aufgenommen."), &e).text, None);
                        }
                        store.reload(true);
                    });
                })));
                crate::view::commands::name_undo(store, id, tr("Aus Batch nehmen"));
            }
            Err(e) => {
                toast::show(store.ui, forms::explain_as(tr("Die Accounts wurden nicht herausgenommen."), &e).text, None);
            }
        }
    });
}

/// Adds the accounts of `ids` to batch `batch`, then offers to take them out again.
pub fn add_to_batch(store: Store, batch: String, ids: Vec<Uuid>) {
    let name = store.model.peek().batch(&batch).map(|b| b.name.clone()).unwrap_or_default();
    let target = batch.clone();
    off("join-batch", move || remote::join_batch(&target, &ids), move |result| {
        store.reload(true);
        match result {
            Ok(added) if added.is_empty() => {
                toast::show(store.ui, trf("Schon alle in „%@“", &[&name]), None);
            }
            Ok(added) => {
                let text = if added.len() == 1 { trf("1 Account zu „%@“ hinzugefügt", &[&name]) } else { trf("%lld Accounts zu „%@“ hinzugefügt", &[&added.len(), &name]) };
                let back = Rc::new(RefCell::new(Some(added)));
                toast::show(store.ui, text, Some(EventHandler::new(move |_| {
                    let Some(added) = back.borrow_mut().take() else { return };
                    let batch = batch.clone();
                    off("leave-batch", move || remote::leave_batch(&batch, &added), move |result| {
                        if let Err(e) = result {
                            fail(store, forms::explain_as(tr("Die Accounts wurden nicht wieder herausgenommen."), &e));
                        }
                        store.reload(true);
                    });
                })));
            }
            Err(e) => fail(store, forms::explain_as(tr("Die Accounts wurden nicht hinzugefügt."), &e)),
        }
    });
}
