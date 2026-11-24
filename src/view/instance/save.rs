use std::path::PathBuf;

use freya::prelude::*;

use super::{Busy, actions};
use crate::app::event::Event;
use crate::core::i18n::{tr, trf};
use crate::core::batch::ModRef;
use crate::core::instance::{Instance, Loader};
use crate::core::options::{Gpu, LaunchOptions, Renderer, WindowSize};
use crate::ui::forms;
use crate::ui::remote::{self, InstanceEdit};
use crate::view::kit::{off_ui, toast};
use crate::view::store::Store;

/// One change of the start options, applied onto the instance as it is when the save runs.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionChange {
    Java(Option<PathBuf>),
    GameArgs(Vec<String>),
    Window(Option<WindowSize>),
    Gpu(Option<Gpu>),
    Renderer(Option<Renderer>),
    HideLogin(bool),
}

impl OptionChange {
    fn apply(&self, options: &mut LaunchOptions) {
        match self {
            Self::Java(java) => options.java.clone_from(java),
            Self::GameArgs(args) => options.game_args.clone_from(args),
            Self::Window(size) => options.window = *size,
            Self::Gpu(gpu) => options.gpu = *gpu,
            Self::Renderer(renderer) => options.renderer = *renderer,
            Self::HideLogin(on) => options.hide_login = *on,
        }
    }
}

/// One change of an instance's saved settings.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Name(String),
    Memory(Option<u32>),
    MemoryMin(Option<u32>),
    JvmArgs(Vec<String>),
    LoaderVersion(Option<String>),
    Option(OptionChange),
}

impl Change {
    fn apply(&self, edit: &mut InstanceEdit) {
        match self {
            Self::Name(name) => edit.name.clone_from(name),
            Self::Memory(mb) => edit.memory_mb = *mb,
            Self::MemoryMin(_) => {}
            Self::JvmArgs(args) => edit.jvm_args.clone_from(args),
            Self::LoaderVersion(version) => edit.loader = edit.loader.with_version(version.clone()),
            Self::Option(change) => change.apply(&mut edit.options),
        }
    }
}

/// A move to another game version or loader.
#[derive(Clone, Debug, PartialEq)]
pub enum Switch {
    Version(String),
    Loader(Loader),
}

/// What the save queue does next.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Save(Change),
    Switch(Switch),
    /// Moves to the newer loader version, named like "Fabric 0.18.1".
    Update(String, String),
}

/// What an undo of a saved change says it did.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Renamed,
    Switched(String),
    Updated(String),
}

/// Queues `job` for instance `id`; changes run one after the other, each onto the instance as the one before left it.
pub fn enqueue(store: Store, id: &str, job: Job) {
    let nav = store.ui.instance;
    let entry = (id.to_owned(), job);
    if nav.queue.peek().back().or(nav.saving.peek().as_ref()) == Some(&entry) {
        return;
    }
    let mut error = nav.error;
    if error.peek().contains_key(id) {
        error.write().remove(id);
    }
    let mut queue = nav.queue;
    queue.write().push_back(entry);
    if nav.saving.peek().is_none() {
        spawn_in_window(pump(store));
    }
}

/// The memory a queued or running save will set on instance `id`, `Some(None)` for the launcher default; a component that asks redraws when the queue moves.
pub fn pending_memory(store: Store, id: &str) -> Option<Option<u32>> {
    let nav = store.ui.instance;
    let queue = nav.queue.read();
    let saving = nav.saving.read();
    queue.iter().rev().chain(saving.iter()).filter(|(i, _)| i == id).find_map(|(_, job)| match job {
        Job::Save(Change::Memory(mb)) => Some(*mb),
        _ => None,
    })
}

/// The starting memory a queued or running save will set on instance `id`, `Some(None)` for the launcher default; a component that asks redraws when the queue moves.
pub fn pending_memory_min(store: Store, id: &str) -> Option<Option<u32>> {
    let nav = store.ui.instance;
    let queue = nav.queue.read();
    let saving = nav.saving.read();
    queue.iter().rev().chain(saving.iter()).filter(|(i, _)| i == id).find_map(|(_, job)| match job {
        Job::Save(Change::MemoryMin(mb)) => Some(*mb),
        _ => None,
    })
}

/// The JVM arguments of instance `id` once its queued and running changes are saved, when one of them sets them.
pub fn pending_jvm(store: Store, id: &str) -> Option<Vec<String>> {
    let nav = store.ui.instance;
    let queue = nav.queue.read();
    let saving = nav.saving.read();
    queue.iter().rev().chain(saving.iter()).filter(|(i, _)| i == id).find_map(|(_, job)| match job {
        Job::Save(Change::JvmArgs(args)) => Some(args.clone()),
        _ => None,
    })
}

/// The start options of instance `id` once its queued and running option changes are saved.
pub fn pending_options(store: Store, id: &str, saved: &LaunchOptions) -> LaunchOptions {
    let nav = store.ui.instance;
    let mut now = saved.clone();
    for (_, job) in nav.saving.read().iter().chain(nav.queue.read().iter()).filter(|(i, _)| i == id) {
        if let Job::Save(Change::Option(change)) = job {
            change.apply(&mut now);
        }
    }
    now
}

/// Whether instance `id` has a change waiting or running.
pub fn pending(store: Store, id: &str) -> bool {
    let nav = store.ui.instance;
    nav.queue.peek().iter().chain(nav.saving.peek().iter()).any(|(i, _)| i == id)
}

async fn pump(store: Store) {
    let nav = store.ui.instance;
    let (mut queue, mut saving, mut busy) = (nav.queue, nav.saving, nav.busy);
    loop {
        let Some((id, job)) = queue.write().pop_front() else { break };
        let Some(made) = store.model.peek().instance(&id).cloned() else { continue };
        saving.set(Some((id.clone(), job.clone())));
        let before = InstanceEdit::of(&made);
        let what = match &job {
            Job::Save(Change::Name(_)) => Some(Edit::Renamed),
            Job::Save(_) => None,
            Job::Switch(Switch::Version(v)) => Some(Edit::Switched(trf("Minecraft %@", &[v]))),
            Job::Switch(Switch::Loader(l)) => Some(Edit::Switched(l.name().to_owned())),
            Job::Update(_, to) => Some(Edit::Updated(to.clone())),
        };
        let text = match &job {
            Job::Switch(Switch::Version(v)) => Some(trf("Mods werden für %@ angepasst …", &[v])),
            Job::Switch(Switch::Loader(l)) => Some(trf("Auf %@ wird umgestellt …", &[&l.name()])),
            _ => None,
        };
        if let Some(text) = text.clone() {
            busy.write().insert(id.clone(), Busy { text, progress: None });
        }
        let work = job.clone();
        let result = off_ui(move || run(&made, &work)).await.unwrap_or_else(|| Err(anyhow::anyhow!("save thread ended")));
        if text.is_some() {
            busy.write().remove(&id);
        }
        saving.set(None);
        finished(store, &id, &job, before, what, result);
    }
}

fn run(made: &Instance, job: &Job) -> anyhow::Result<(Instance, Vec<String>)> {
    if let Job::Save(Change::MemoryMin(min)) = job {
        return Ok((remote::save_memory_min(&made.id, *min)?, Vec::new()));
    }
    let base = InstanceEdit::of(made);
    let mut edit = base.clone();
    let mut missing = Vec::new();
    match job {
        Job::Save(change) => change.apply(&mut edit),
        Job::Update(version, _) => edit.loader = edit.loader.with_version(Some(version.clone())),
        Job::Switch(Switch::Version(version)) => {
            edit.game_version.clone_from(version);
            edit.loader = edit.loader.with_version(None);
            if edit.loader.takes_mods() && !edit.mods.is_empty() {
                let (mods, gone) = remote::move_mods(&edit.mods, version, &edit.loader)?;
                edit.mods = mods;
                missing = gone.into_iter().map(|title| trf("„%@“ gibt es nicht für %@ und wurde entfernt.", &[&title, version])).collect();
            }
        }
        Job::Switch(Switch::Loader(loader)) => {
            edit.mods = mods_after_loader(&edit.loader, loader, std::mem::take(&mut edit.mods));
            edit.loader = loader.clone();
        }
    }
    Ok((remote::save_instance(&made.id, &base, edit)?, missing))
}

fn mods_after_loader(from: &Loader, to: &Loader, mods: Vec<ModRef>) -> Vec<ModRef> {
    let fabric_like = |l: &Loader| matches!(l, Loader::Fabric(_) | Loader::Quilt(_));
    if fabric_like(from) && fabric_like(to) { mods } else { Vec::new() }
}

fn finished(store: Store, id: &str, job: &Job, before: InstanceEdit, what: Option<Edit>, result: anyhow::Result<(Instance, Vec<String>)>) {
    let nav = store.ui.instance;
    let renamed = matches!(job, Job::Save(Change::Name(_)));
    match result {
        Ok((saved, notes)) => {
            let after = InstanceEdit::of(&saved);
            let name = saved.name.clone();
            store.dispatch(Event::InstanceChanged(saved));
            if matches!(job, Job::Switch(_)) {
                let mut all = nav.notes;
                all.write().insert(id.to_owned(), notes);
            }
            if renamed && nav.renaming.peek().as_deref() == Some(id) {
                actions::end_rename(store);
            }
            if let Some(what) = what.filter(|_| before != after) {
                offer(store, id.to_owned(), name, what, before, after);
            }
            if nav.queue.peek().is_empty() {
                store.reload(true);
            }
        }
        Err(e) => {
            let problem = forms::explain_as(tr("Nicht gespeichert."), &e);
            let mut queue = nav.queue;
            queue.write().retain(|(queued, _)| queued != id);
            if renamed && nav.renaming.peek().as_deref() == Some(id) {
                let mut rename_error = nav.rename_error;
                rename_error.set(Some(problem.text));
            } else {
                let mut error = nav.error;
                error.write().insert(id.to_owned(), problem.text);
            }
        }
    }
}

fn done_text(name: &str, what: &Edit) -> String {
    match what {
        Edit::Renamed => trf("Umbenannt in „%@“.", &[&name]),
        Edit::Switched(to) => trf("„%@“ auf %@ umgestellt.", &[&name, to]),
        Edit::Updated(to) => trf("„%@“ auf %@ aktualisiert.", &[&name, to]),
    }
}

fn offer(store: Store, id: String, name: String, what: Edit, before: InstanceEdit, after: InstanceEdit) {
    let text = done_text(&name, &what);
    toast::show(store.ui, text, Some(EventHandler::new(move |_| restore(store, id.clone(), name.clone(), what.clone(), before.clone(), after.clone()))));
}

fn restore(store: Store, id: String, name: String, what: Edit, before: InstanceEdit, after: InstanceEdit) {
    if store.model.peek().instance(&id).is_none() {
        toast::show(store.ui, tr("Diese Instanz gibt es nicht mehr."), None);
        return;
    }
    let moves = before.game_version != after.game_version || !before.loader.same_kind(&after.loader) || before.mods != after.mods;
    if moves && super::blocked(store, &id).is_some() {
        toast::show(store.ui, tr("Gerade laufen Spiele. Stoppe sie zuerst."), None);
        offer(store, id, name, what, before, after);
        return;
    }
    spawn_in_window(async move {
        let (instance, base, edit) = (id.clone(), after.clone(), before.clone());
        let result = off_ui(move || remote::save_instance(&instance, &base, edit)).await;
        match result {
            Some(Ok(saved)) => store.dispatch(Event::InstanceChanged(saved)),
            Some(Err(e)) => {
                let undone = if what == Edit::Renamed { trf("„%@“ wurde nicht zurückbenannt.", &[&name]) } else { trf("„%@“ wurde nicht zurückgestellt.", &[&name]) };
                toast::show(store.ui, format!("{undone} {}", forms::explain(&e).text), None);
                if !actions::permanent(&e) {
                    offer(store, id, name, what, before, after);
                }
            }
            None => offer(store, id, name, what, before, after),
        }
        store.reload(true);
    });
}