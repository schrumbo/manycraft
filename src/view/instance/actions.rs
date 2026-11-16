use freya::prelude::*;

use super::save::{self, Change, Job};
use super::{Facts, links, snapshots};
use crate::app::event::Event;
use crate::core::i18n::{tr, trf};
use crate::core::install;
use crate::core::instance::Instance;
use crate::core::origin;
use crate::core::snapshot::Reason;
use crate::ui::forms;
use crate::ui::remote;
use crate::view::icons::Icon;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::{off_ui, toast};
use crate::view::native;
use crate::view::store::Store;
use crate::view::theme::frame;
use crate::view::ui::{Filter, Page};

/// An action on an instance, with why it is unavailable.
pub struct Action {
    pub key: &'static str,
    pub label: String,
    /// The label with the instance's name, for the palette.
    pub long: String,
    pub glyph: Icon,
    pub disabled: Option<String>,
    pub danger: bool,
    pub run: EventHandler<()>,
}

impl Action {
    /// An action named `key` that runs `run`.
    pub fn new(key: &'static str, label: String, long: String, glyph: Icon, run: impl FnMut(()) + 'static) -> Self {
        Self { key, label, long, glyph, disabled: None, danger: false, run: EventHandler::new(run) }
    }

    pub fn disabled(mut self, why: Option<String>) -> Self {
        self.disabled = why;
        self
    }
}

/// Whether `e` will fail again however often it is tried.
pub fn permanent(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<forms::Plain>() || c.is::<crate::core::instance::Final>())
}

/// Tells the tabs that files of an instance changed on disk, so they read them again.
pub fn changed(store: Store) {
    let mut changed = store.ui.instance.changed;
    let next = *changed.peek() + 1;
    changed.set(next);
}

/// The actions on the instance of `f`, for the inspector, the menu of a narrow page and the palette, in that order.
pub fn all(store: Store, f: &Facts) -> Vec<Action> {
    let id = f.made.id.clone();
    let name = f.made.name.clone();
    let running = f.running.then(|| tr("Stoppe zuerst die laufenden Spiele."));
    let used = (!f.batches.is_empty()).then(|| crate::core::fleet::used_by(&f.batches.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>()));
    let mut list = Vec::new();
    let (a, b, c, d, e, g) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
    list.push(Action::new("folder", tr("Ordner öffnen"), trf("Ordner von %@ öffnen", &[&name]), Icon::Folder, move |_| open_folder(store, &a)));
    list.push(Action::new("clone", tr("Klonen"), trf("%@ klonen", &[&name]), Icon::Copy, move |_| clone(store, &b)));
    list.push(Action::new("shortcut", tr("Verknüpfung erstellen"), trf("Verknüpfung für %@ erstellen", &[&name]), Icon::Play, move |_| shortcut(store, &c)));
    list.push(links::export(store, &id, &name));
    if f.made.loader.version().is_some() {
        list.push(Action::new("loader", tr("Loader aktualisieren…"), trf("Loader von %@ aktualisieren…", &[&name]), Icon::Download, move |_| look_for_loader(store, &g)));
    }
    list.push(Action::new("reset", tr("Zurücksetzen…"), trf("%@ zurücksetzen…", &[&name]), Icon::Undo, move |_| ask_reset(store, &d)).disabled(running));
    let mut delete = Action::new("delete", tr("Löschen…"), trf("%@ löschen…", &[&name]), Icon::Close, move |_| ask_delete(store, &e)).disabled(used);
    delete.danger = true;
    list.push(delete);
    list
}

/// Starts editing the name of instance `id` in the toolbar.
pub fn begin_rename(store: Store, id: &str) {
    let Some(made) = store.model.peek().instance(id).cloned() else { return };
    let nav = store.ui.instance;
    let (mut renaming, mut text, mut error) = (nav.renaming, nav.rename, nav.rename_error);
    let mut page = store.ui.page;
    if *page.peek() != Page::Instance(id.to_owned()) {
        page.set(Page::Instance(id.to_owned()));
    }
    text.set(made.name);
    error.set_if_modified(None);
    renaming.set(Some(id.to_owned()));
}

/// Saves the typed name, or ends editing when it did not change.
pub fn commit_rename(store: Store) {
    let nav = store.ui.instance;
    let Some(id) = nav.renaming.peek().clone() else { return };
    let typed = nav.rename.peek().trim().to_owned();
    let mut error = nav.rename_error;
    if typed.is_empty() {
        error.set(Some(tr("Der Name darf nicht leer sein.")));
        return;
    }
    match store.model.peek().instance(&id) {
        Some(made) if made.name != typed => {}
        _ => {
            end_rename(store);
            return;
        }
    }
    save::enqueue(store, &id, Job::Save(Change::Name(typed)));
}

/// Leaves the name as it was.
pub fn end_rename(store: Store) {
    let nav = store.ui.instance;
    let (mut renaming, mut error) = (nav.renaming, nav.rename_error);
    error.set_if_modified(None);
    renaming.set_if_modified(None);
}

fn open_folder(store: Store, id: &str) {
    let Some(dir) = store.model.peek().instance(id).map(Instance::dir) else { return };
    spawn_in_window(async move {
        let made = dir.clone();
        if let Some(Ok(())) = off_ui(move || std::fs::create_dir_all(&made)).await {
            native::open(&dir);
        }
    });
}

fn shortcut(store: Store, id: &str) {
    let m = store.model.peek();
    let Some(made) = m.instance(id) else { return };
    let solo = super::facts(&m, id).and_then(|f| f.solo);
    let account = solo.and_then(|a| m.account(a));
    let name = match account.filter(|_| m.accounts.len() > 1) {
        Some(account) => format!("{} ({})", made.name, account.name),
        None => made.name.clone(),
    };
    let play = crate::core::shortcut::Play { instance: made.id.clone(), account: account.map(|a| a.id.to_string()) };
    drop(m);
    spawn_in_window(async move {
        let label = name.clone();
        match off_ui(move || remote::create_shortcut(&play, &label)).await {
            Some(Ok(Some(path))) => native::reveal(&path),
            Some(Ok(None)) => {}
            Some(Err(e)) => store.dispatch(Event::Error(forms::explain_as(trf("Verknüpfung für „%@“ nicht erstellt.", &[&name]), &e).text)),
            None => store.dispatch(Event::Error(trf("Verknüpfung für „%@“ nicht erstellt.", &[&name]))),
        }
    });
}

fn clone(store: Store, id: &str) {
    let source = id.to_owned();
    spawn_in_window(async move {
        let from = source.clone();
        match off_ui(move || remote::clone_instance(&from)).await {
            Some(Ok(made)) => {
                let (new_id, name) = (made.id.clone(), made.name.clone());
                store.dispatch(Event::InstanceAdded(made));
                begin_rename(store, &new_id);
                offer_discard(store, new_id, name, source);
                store.reload(true);
            }
            Some(Err(e)) => store.dispatch(Event::Error(forms::explain_as(tr("Nicht geklont."), &e).text)),
            None => store.dispatch(Event::Error(tr("Nicht geklont."))),
        }
    });
}

fn offer_discard(store: Store, id: String, name: String, source: String) {
    toast::show(store.ui, trf("„%@“ geklont.", &[&name]), Some(EventHandler::new(move |_| discard(store, id.clone(), name.clone(), source.clone()))));
}

fn discard(store: Store, id: String, name: String, source: String) {
    if store.model.peek().instance(&id).is_none() {
        toast::show(store.ui, tr("Diese Instanz gibt es nicht mehr."), None);
        return;
    }
    if store.model.peek().games.values().any(|g| g.instance_id == id && g.phase.is_active()) {
        toast::show(store.ui, tr("Gerade laufen Spiele. Stoppe sie zuerst."), None);
        offer_discard(store, id, name, source);
        return;
    }
    spawn_in_window(async move {
        let gone = id.clone();
        match off_ui(move || remote::discard_instance(&gone)).await {
            Some(Ok(_)) => {
                store.dispatch(Event::InstanceRemoved(id.clone()));
                leave(store, &id, Some(&source));
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht gelöscht.", &[&name]), &e).text, None);
                if !permanent(&e) {
                    offer_discard(store, id, name, source);
                }
            }
            None => offer_discard(store, id, name, source),
        }
        store.reload(true);
    });
}

fn leave(store: Store, id: &str, to: Option<&str>) {
    let mut page = store.ui.page;
    if *page.peek() != Page::Instance(id.to_owned()) {
        return;
    }
    let m = store.model.peek();
    let next = to.filter(|t| m.instance(t).is_some()).map(str::to_owned).or_else(|| m.instances.first().map(|i| i.id.clone()));
    drop(m);
    page.set(next.map_or(Page::Grid(Filter::All), Page::Instance));
}

fn ask_delete(store: Store, id: &str) {
    let m = store.model.peek();
    let Some(made) = m.instance(id) else { return };
    let name = made.name.clone();
    let users: Vec<String> = m.batches.iter().filter(|b| b.instance_id == made.id).map(|b| b.name.clone()).collect();
    drop(m);
    let ui = store.ui;
    if !users.is_empty() {
        let done = Primary { label: tr("Fertig"), disabled: None, danger: false, run: EventHandler::new(move |_| sheet::close(ui)) };
        sheet::open(ui, Card::new("instance.delete.used", trf("„%@“ lässt sich nicht löschen.", &[&name])).message(crate::core::fleet::used_by(&users)).primary(done).width(frame::SHEET * 0.8));
        return;
    }
    let id = id.to_owned();
    sheet::confirm(ui, "instance.delete", trf("„%@“ löschen?", &[&name]), tr("Die Instanz mit Mods, Welten und Screenshots kommt in den Papierkorb und bleibt dort 7 Tage."), tr("Löschen"), true, move |_| delete(store, id.clone(), name.clone()));
}

fn delete(store: Store, id: String, name: String) {
    let next = {
        let m = store.model.peek();
        let mut names: Vec<(String, String)> = m.instances.iter().map(|i| (i.name.to_lowercase(), i.id.clone())).collect();
        names.sort();
        let at = names.iter().position(|(_, i)| *i == id);
        at.and_then(|at| names.get(at + 1).or_else(|| at.checked_sub(1).and_then(|p| names.get(p)))).map(|(_, i)| i.clone())
    };
    spawn_in_window(async move {
        let doomed = id.clone();
        match off_ui(move || remote::delete_instance(&doomed)).await {
            Some(Ok(entry)) => {
                store.dispatch(Event::InstanceRemoved(id.clone()));
                leave(store, &id, next.as_deref());
                offer_restore(store, name, entry);
            }
            Some(Err(e)) => store.dispatch(Event::Error(forms::explain_as(tr("Nicht gelöscht."), &e).text)),
            None => store.dispatch(Event::Error(tr("Nicht gelöscht."))),
        }
        store.reload(true);
    });
}

fn offer_restore(store: Store, name: String, entry: String) {
    toast::show(store.ui, trf("„%@“ gelöscht.", &[&name]), Some(EventHandler::new(move |_| restore(store, name.clone(), entry.clone()))));
}

fn restore(store: Store, name: String, entry: String) {
    spawn_in_window(async move {
        let from = entry.clone();
        match off_ui(move || remote::restore_instance(&from)).await {
            Some(Ok(made)) => {
                let id = made.id.clone();
                if store.model.peek().instance(&id).is_none() {
                    store.dispatch(Event::InstanceAdded(made));
                }
                let mut page = store.ui.page;
                page.set(Page::Instance(id));
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht zurückgeholt.", &[&name]), &e).text, None);
                if !permanent(&e) {
                    offer_restore(store, name, entry);
                }
            }
            None => offer_restore(store, name, entry),
        }
        store.reload(true);
    });
}

fn ask_reset(store: Store, id: &str) {
    let Some(made) = store.model.peek().instance(id).cloned() else { return };
    if let Some(why) = super::blocked(store, id) {
        toast::show(store.ui, why, None);
        return;
    }
    let message = if origin::is_kept(&made) {
        tr("Mods, Konfiguration, Ressourcen- und Shader-Pakete gehen auf den Stand von der Erstellung zurück. Welten, Screenshots, Serverliste und Optionen bleiben. Der jetzige Stand bleibt 7 Tage im Papierkorb.")
    } else {
        tr("Der Anfangszustand dieser Instanz ist nicht gespeichert: Mods, Konfiguration, Ressourcen- und Shader-Pakete werden geleert. Welten, Screenshots, Serverliste und Optionen bleiben. Der jetzige Stand bleibt 7 Tage im Papierkorb.")
    };
    let id = id.to_owned();
    sheet::confirm(store.ui, "instance.reset", trf("„%@“ zurücksetzen?", &[&made.name]), message, tr("Zurücksetzen"), true, move |_| {
        let target = id.clone();
        snapshots::before(store, &id, Reason::Reset, move || reset(store, target));
    });
}

fn reset(store: Store, id: String) {
    if let Some(why) = super::blocked(store, &id) {
        toast::show(store.ui, why, None);
        return;
    }
    let mut busy = store.ui.instance.busy;
    busy.write().insert(id.clone(), super::Busy { text: tr("Wird zurückgesetzt …"), progress: None });
    spawn_in_window(async move {
        let target = id.clone();
        let result = off_ui(move || remote::reset_instance(&target)).await;
        busy.write().remove(&id);
        match result {
            Some(Ok(done)) => {
                let (name, version) = (done.after.name.clone(), done.after.game_version.clone());
                store.dispatch(Event::InstanceChanged(done.after.clone()));
                match done.missing.as_slice() {
                    [] => {}
                    [one] => {
                        toast::show(store.ui, trf("„%@“ gibt es nicht für %@ und blieb draußen.", &[one, &version]), None);
                    }
                    many => {
                        toast::show(store.ui, trf("%lld Mods gibt es nicht für %@ und blieben draußen.", &[&many.len(), &version]), None);
                    }
                }
                offer_unreset(store, id.clone(), name, done.entry, done.before, done.after);
            }
            Some(Err(e)) => store.dispatch(Event::Error(forms::explain_as(tr("Nicht zurückgesetzt."), &e).text)),
            None => store.dispatch(Event::Error(tr("Nicht zurückgesetzt."))),
        }
        changed(store);
        store.reload(true);
    });
}

fn offer_unreset(store: Store, id: String, name: String, entry: String, before: Instance, after: Instance) {
    toast::show(store.ui, trf("„%@“ zurückgesetzt.", &[&name]), Some(EventHandler::new(move |_| unreset(store, id.clone(), name.clone(), entry.clone(), before.clone(), after.clone()))));
}

fn unreset(store: Store, id: String, name: String, entry: String, before: Instance, after: Instance) {
    let Some(current) = store.model.peek().instance(&id).cloned() else {
        toast::show(store.ui, tr("Diese Instanz gibt es nicht mehr."), None);
        return;
    };
    if !remote::same_files(&current, &after) {
        toast::show(store.ui, trf("„%@“ lässt sich nicht zurückholen: Mods oder Version wurden inzwischen geändert.", &[&name]), None);
        return;
    }
    if super::blocked(store, &id).is_some() {
        toast::show(store.ui, tr("Gerade laufen Spiele. Stoppe sie zuerst."), None);
        offer_unreset(store, id, name, entry, before, after);
        return;
    }
    spawn_in_window(async move {
        let kept = (id.clone(), before.clone(), after.clone(), entry.clone());
        match off_ui(move || remote::restore_reset(&kept.0, &kept.1, &kept.2, &kept.3)).await {
            Some(Ok(saved)) => store.dispatch(Event::InstanceChanged(saved)),
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Nicht zurückgeholt."), &e).text, None);
                if !permanent(&e) {
                    offer_unreset(store, id, name, entry, before, after);
                }
            }
            None => offer_unreset(store, id, name, entry, before, after),
        }
        changed(store);
        store.reload(true);
    });
}

/// Looks for a newer loader for instance `id` and offers it, or says it is current.
pub fn look_for_loader(store: Store, id: &str) {
    let Some(made) = store.model.peek().instance(id).cloned() else { return };
    let Some(current) = made.loader.version().map(str::to_owned) else {
        let text = if made.loader.takes_mods() { tr("Der Loader folgt schon der neuesten Version.") } else { tr("Vanilla hat keinen Loader.") };
        toast::show(store.ui, text, None);
        return;
    };
    let id = id.to_owned();
    spawn_in_window(async move {
        let (loader, version) = (made.loader.clone(), made.game_version.clone());
        match off_ui(move || remote::newest_loader(&loader, &version)).await {
            Some(Ok(Some(newest))) if install::is_newer(&newest, &current) => ask_loader(store, &id, &newest),
            Some(Ok(_)) => {
                toast::show(store.ui, trf("%@ %@ ist aktuell.", &[&made.loader.name(), &current]), None);
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Loader-Liste nicht geladen."), &e).text, None);
            }
            None => {
                toast::show(store.ui, tr("Loader-Liste nicht geladen."), None);
            }
        }
    });
}

/// Asks whether instance `id` should move to loader version `to`, then switches; running games keep their loader until
/// their next start.
pub fn ask_loader(store: Store, id: &str, to: &str) {
    let Some(made) = store.model.peek().instance(id).cloned() else { return };
    let running = store.model.peek().games.values().any(|g| g.instance_id == id && g.phase.is_active());
    let message = if running { tr("Laufende Spiele bleiben unverändert. Der neue Loader gilt ab dem nächsten Start.") } else { tr("Mods und Welten bleiben. Der neue Loader wird beim nächsten Start installiert.") };
    let target = format!("{} {to}", made.loader.name());
    let (id, to) = (id.to_owned(), to.to_owned());
    sheet::confirm(store.ui, "instance.loader", trf("Auf %@ %@ aktualisieren?", &[&made.loader.name(), &to]), message, tr("Aktualisieren"), false, move |_| {
        let (job_id, to, target) = (id.clone(), to.clone(), target.clone());
        snapshots::before(store, &id, Reason::LoaderUpdate(target.clone()), move || save::enqueue(store, &job_id, Job::Update(to, target)));
    });
}
