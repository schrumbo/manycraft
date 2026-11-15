//! Instances in and out: importing packs, Prism instances and instances of other launchers, exporting packs, opening folders in the file manager.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gpui_kit::*;

use super::actions::{CancelImport, CreateShortcut, ExportBatch, ExportInstance, ImportBatch, OpenBatchFolder, OpenGameFolder, OpenInstanceFolder};
use super::{forms, model, remote};
use crate::core::instance::Instance;
use crate::core::drive;
use crate::core::i18n::{tr, trf};
use crate::core::launchers::{Found, Take};
use crate::core::mrpack::{self, Refused};

static NEXT: AtomicU64 = AtomicU64::new(1);
static LAST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Registers the import, export and folder actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &ImportBatch, cx| pick(cx, |_| {}));
    cx.on_action(|a: &ExportBatch, cx| {
        let made = model::get(cx).read(cx).instance_of(&a.0).map(|i| i.id.clone());
        if let Some(made) = made {
            export(&made, cx);
        }
    });
    cx.on_action(|a: &ExportInstance, cx| export(&a.0, cx));
    cx.on_action(|a: &CreateShortcut, cx| shortcut(a, cx));
    cx.on_action(|a: &CancelImport, cx| cancel(a.0, cx));
    cx.on_action(|a: &OpenBatchFolder, cx| {
        let dir = model::get(cx).read(cx).instance_of(&a.0).map(Instance::dir);
        if let Some(dir) = dir {
            open_folder(dir, cx);
        }
    });
    cx.on_action(|a: &OpenInstanceFolder, cx| {
        let dir = model::get(cx).read(cx).instance(&a.0).map(Instance::dir);
        if let Some(dir) = dir {
            open_folder(dir, cx);
        }
    });
    cx.on_action(|a: &OpenGameFolder, cx| {
        let m = model::get(cx).read(cx);
        let dir = m.games.get(&a.0).and_then(|i| m.instance(&i.instance_id).map(|made| made.game_dir(i.account_id)));
        if let Some(dir) = dir {
            open_folder(dir, cx);
        }
    });
}

/// Imports every dropped file or folder, each as its own instance.
pub fn drop_paths(paths: &ExternalPaths, cx: &mut App) {
    for path in paths.paths() {
        start(path.clone(), cx);
    }
}

/// Asks for packs to import and imports each; `chosen` runs first when at least one was picked.
pub fn pick(cx: &mut App, chosen: impl FnOnce(&mut App) + 'static) {
    if drive::on() {
        let paths = drive::answer("open").unwrap_or_default();
        if !paths.is_empty() {
            chosen(cx);
        }
        for path in paths {
            start(path, cx);
        }
        return;
    }
    let prompt = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(tr("Importieren").into()) });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = prompt.await
            && !paths.is_empty()
        {
            let _ = cx.update(|cx| {
                chosen(cx);
                for path in paths {
                    start(path, cx);
                }
            });
        }
    })
    .detach();
}

/// Imports the pack at `path` as a new instance, with a row in the sidebar.
pub(super) fn start(path: PathBuf, cx: &mut App) {
    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let shown = path.file_stem().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| label.clone());
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let cancel = Arc::new(AtomicBool::new(false));
    model::get(cx).update(cx, |m, cx| {
        if m.imports.is_empty() {
            m.error = None;
        }
        m.imports.push(model::Import { id, name: shown, percent: None, cancel: Arc::clone(&cancel) });
        cx.notify();
    });
    let (tx, rx) = async_channel::unbounded::<u8>();
    let stop = Arc::clone(&cancel);
    let source = path.clone();
    let job = cx.background_spawn(async move {
        let progress = |done: usize, total: usize| {
            let _ = tx.try_send((done * 100).checked_div(total).unwrap_or(0).min(100) as u8);
        };
        remote::import_instance(&path, &stop, &progress)
    });
    cx.spawn(async move |cx| follow(id, rx, job, &cancel, &label, Some(&source), true, cx).await).detach();
}

#[allow(clippy::too_many_arguments)]
async fn follow(id: u64, rx: async_channel::Receiver<u8>, job: Task<anyhow::Result<Instance>>, cancel: &AtomicBool, label: &str, source: Option<&Path>, show: bool, cx: &mut AsyncApp) {
    while let Ok(mut percent) = rx.recv().await {
        while let Ok(next) = rx.try_recv() {
            percent = next;
        }
        let _ = cx.update(|cx| {
            model::get(cx).update(cx, |m, cx| {
                if let Some(i) = m.imports.iter_mut().find(|i| i.id == id)
                    && i.percent != Some(percent)
                {
                    i.percent = Some(percent);
                    cx.notify();
                }
            });
        });
    }
    let result = job.await;
    let gone = result.is_err() && source.is_some_and(|s| !s.exists());
    let _ = cx.update(|cx| finish(id, label, gone, cancel, result, show, cx));
}

/// Imports `items` from other launchers one after another, each as its own instance with its own entry in the
/// import list; once the last one is done it opens.
pub fn import_found(items: Vec<Found>, take: Take, cx: &mut App) {
    if items.is_empty() {
        return;
    }
    let queue: Vec<(u64, Arc<AtomicBool>, Found)> = items.into_iter().map(|f| (NEXT.fetch_add(1, Ordering::SeqCst), Arc::new(AtomicBool::new(false)), f)).collect();
    model::get(cx).update(cx, |m, cx| {
        if m.imports.is_empty() {
            m.error = None;
        }
        for (id, cancel, found) in &queue {
            m.imports.push(model::Import { id: *id, name: found.name.clone(), percent: None, cancel: Arc::clone(cancel) });
        }
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let last = queue.len() - 1;
        for (at, (id, cancel, found)) in queue.into_iter().enumerate() {
            let (tx, rx) = async_channel::unbounded::<u8>();
            let stop = Arc::clone(&cancel);
            let item = found.clone();
            let job = cx.background_spawn(async move {
                let progress = |done: usize, total: usize| {
                    let _ = tx.try_send((done * 100).checked_div(total).unwrap_or(0).min(100) as u8);
                };
                remote::import_found(&item, take, &stop, &progress)
            });
            follow(id, rx, job, &cancel, &found.name, None, at == last, cx).await;
        }
    })
    .detach();
}

fn finish(id: u64, label: &str, gone: bool, cancel: &AtomicBool, result: anyhow::Result<Instance>, show: bool, cx: &mut App) {
    let cancelled = cancel.load(Ordering::SeqCst);
    model::get(cx).update(cx, |m, cx| {
        m.imports.retain(|i| i.id != id);
        if let Err(e) = &result
            && !cancelled
            && !is_cancelled(e)
        {
            m.error = Some(if gone { trf("„%@“ ist nicht mehr da. Wurde die Datei verschoben?", &[&label]) } else { explain(label, e) });
        }
        cx.notify();
    });
    match result {
        Ok(made) if cancelled => {
            let job = cx.background_spawn(async move { remote::drop_import(&made) });
            let label = label.to_owned();
            cx.spawn(async move |cx| {
                let result = job.await;
                let _ = cx.update(|cx| {
                    if let Err(e) = result {
                        model::get(cx).update(cx, |m, cx| {
                            m.error = Some(forms::explain_as(trf("„%@“ wurde abgebrochen, ließ sich aber nicht ganz entfernen.", &[&label]), &e).text);
                            cx.notify();
                        });
                    }
                    super::state::reload_keeping_error(cx);
                });
            })
            .detach();
        }
        Ok(made) => {
            let id = made.id.clone();
            model::get(cx).update(cx, |m, cx| {
                if !m.instances.iter().any(|i| i.id == made.id) {
                    m.instances.push(made);
                }
                cx.notify();
            });
            super::state::reload_keeping_error(cx);
            if show {
                super::main_window::show_instance(&id, cx);
            }
        }
        Err(_) => {}
    }
}

fn cancel(id: u64, cx: &mut App) {
    model::get(cx).update(cx, |m, cx| {
        if let Some(i) = m.imports.iter().find(|i| i.id == id) {
            i.cancel.store(true, Ordering::SeqCst);
            cx.notify();
        }
    });
}

fn is_cancelled(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<Refused>() == Some(&Refused::Cancelled))
}

/// What went wrong with importing `label`, in plain German.
fn explain(label: &str, e: &anyhow::Error) -> String {
    let Some(refused) = e.chain().find_map(|c| c.downcast_ref::<Refused>()) else {
        let what = trf("„%@“ nicht importiert.", &[&label]);
        return if local_io(e) { format!("{what} {}", tr("Die Datei ist beschädigt oder lässt sich nicht lesen.")) } else { forms::explain_as(what, e).text };
    };
    match refused {
        Refused::NotAPack => trf("„%@“ ist kein Modpack von Modrinth. Wähle eine .mrpack-Datei.", &[&label]),
        Refused::NoLoader => trf("„%@“ bringt Mods mit, nennt aber keinen Mod-Loader. Nichts wurde importiert.", &[&label]),
        Refused::NoGame => trf("„%@“ nennt keine Minecraft-Version.", &[&label]),
        Refused::Host(_) => trf("„%@“ lädt Dateien von einer Seite, die Manycraft nicht kennt. Nichts wurde importiert.", &[&label]),
        Refused::Hash(_) => trf("Eine Datei aus „%@“ kam verändert an. Nichts wurde importiert. Versuche es noch einmal.", &[&label]),
        Refused::Path(_) => trf("„%@“ enthält Dateien, die außerhalb des Instanzordners landen würden. Nichts wurde importiert.", &[&label]),
        Refused::Prism(_) => trf("„%@“ lässt sich nicht übernehmen.", &[&label]),
        Refused::Unknown => trf("„%@“ ist weder ein Modpack von Modrinth noch etwas aus Prism. Wähle eine .mrpack-Datei, einen Prism-Export oder einen Prism-Instanzordner.", &[&label]),
        Refused::Cancelled => String::new(),
    }
}

fn local_io(e: &anyhow::Error) -> bool {
    let io = e.chain().find_map(|c| c.downcast_ref::<std::io::Error>());
    let network = e.chain().any(|c| c.downcast_ref::<ureq::Error>().is_some()) || ["cannot download", "cannot look the mods up", "cannot find the Fabric loader"].iter().any(|m| format!("{e:#}").contains(m));
    io.is_some_and(|i| !matches!(i.kind(), std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::StorageFull)) && !network
}

fn file_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_end_matches('.');
    format!("{}.{}", if clean.is_empty() { "batch" } else { clean }, mrpack::EXTENSION)
}

fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() { path } else { path.with_extension(mrpack::EXTENSION) }
}

fn shortcut(a: &CreateShortcut, cx: &mut App) {
    let m = model::get(cx).read(cx);
    let Some(made) = m.instance(&a.instance) else { return };
    let account = a.account.as_deref().and_then(|id| id.parse().ok()).and_then(|id| m.account(id));
    let name = match account.filter(|_| m.accounts.len() > 1) {
        Some(account) => format!("{} ({})", made.name, account.name),
        None => made.name.clone(),
    };
    let play = crate::core::shortcut::Play { instance: made.id.clone(), account: account.map(|a| a.id.to_string()) };
    let job = cx.background_spawn({
        let name = name.clone();
        async move { remote::create_shortcut(&play, &name) }
    });
    cx.spawn(async move |cx| {
        let result = job.await;
        let _ = cx.update(|cx| match result {
            Ok(Some(path)) => {
                if !drive::fenced("reveal", path.display()) {
                    cx.reveal_path(&path);
                }
            }
            Ok(None) => {}
            Err(e) => model::get(cx).update(cx, |m, cx| {
                m.error = Some(forms::explain_as(trf("Verknüpfung für „%@“ nicht erstellt.", &[&name]), &e).text);
                cx.notify();
            }),
        });
    })
    .detach();
}

fn export(id: &str, cx: &mut App) {
    let m = model::get(cx).read(cx);
    let Some(made) = m.instance(id).cloned().filter(|b| !m.exporting.contains(&b.id)) else { return };
    let suggested = file_name(&made.name);
    if drive::on() {
        if let Some(path) = drive::answer("save").and_then(|p| p.into_iter().next()) {
            write(made, with_extension(path), cx);
        }
        return;
    }
    let last = LAST_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let dir = last.or_else(dirs::download_dir).or_else(dirs::home_dir).unwrap_or_default();
    let chosen = cx.prompt_for_new_path(&dir, Some(&suggested));
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(path))) = chosen.await {
            *LAST_DIR.lock().unwrap_or_else(|e| e.into_inner()) = path.parent().map(Path::to_path_buf);
            let _ = cx.update(|cx| write(made, with_extension(path), cx));
        }
    })
    .detach();
}

fn write(made: Instance, path: PathBuf, cx: &mut App) {
    let id = made.id.clone();
    let started = model::get(cx).update(cx, |m, cx| {
        cx.notify();
        m.exporting.insert(id.clone())
    });
    if !started {
        return;
    }
    let out = path.clone();
    let name = made.name.clone();
    let job = cx.background_spawn(async move { remote::export_instance(&made, &out) });
    cx.spawn(async move |cx| {
        let result = job.await;
        let _ = cx.update(|cx| {
            model::get(cx).update(cx, |m, cx| {
                m.exporting.remove(&id);
                if let Err(e) = &result {
                    let what = trf("„%@“ nicht exportiert.", &[&name]);
                    m.error = Some(if local_io(e) { format!("{what} {}", tr("Die Datei lässt sich nicht schreiben. Ist sie in einem anderen Programm offen?")) } else { forms::explain_as(what, e).text });
                }
                cx.notify();
            });
            if result.is_ok() && !drive::fenced("reveal", path.display()) {
                cx.reveal_path(&path);
            }
        });
    })
    .detach();
}

fn open_folder(dir: PathBuf, cx: &mut App) {
    let made = dir.clone();
    let job = cx.background_spawn(async move { std::fs::create_dir_all(&made) });
    cx.spawn(async move |cx| {
        let result = job.await;
        let _ = cx.update(|cx| match result {
            Ok(()) if !drive::fenced("open folder", dir.display()) => cx.open_with_system(Path::new(&dir)),
            Ok(()) => {}
            Err(e) => {
                let text = forms::explain_as(tr("Der Ordner lässt sich nicht öffnen."), &e.into()).text;
                model::get(cx).update(cx, |m, cx| {
                    m.error = Some(text);
                    cx.notify();
                });
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{PathBuf, file_name, with_extension};

    #[test]
    fn export_names_are_safe_file_names() {
        assert_eq!(file_name("Farm"), "Farm.mrpack");
        assert_eq!(file_name("a/b:c*?"), "a_b_c__.mrpack");
        assert_eq!(file_name("  ...  "), "batch.mrpack");
        assert_eq!(with_extension(PathBuf::from("x")), PathBuf::from("x.mrpack"));
        assert_eq!(with_extension(PathBuf::from("x.zip")), PathBuf::from("x.zip"));
    }
}
