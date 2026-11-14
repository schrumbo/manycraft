//! Batches in and out: importing packs and Prism instances, exporting packs, opening folders in the file manager.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gpui_kit::*;

use super::actions::{CancelImport, ExportBatch, ImportBatch, OpenBatchFolder, OpenGameFolder};
use super::{forms, model, remote};
use crate::core::batch::Batch;
use crate::core::drive;
use crate::core::i18n::{tr, trf};
use crate::core::mrpack::{self, Refused};

static NEXT: AtomicU64 = AtomicU64::new(1);
static LAST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Registers the import, export and folder actions.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &ImportBatch, cx| pick(cx, |_| {}));
    cx.on_action(|a: &ExportBatch, cx| export(&a.0, cx));
    cx.on_action(|a: &CancelImport, cx| cancel(a.0, cx));
    cx.on_action(|a: &OpenBatchFolder, cx| {
        let dir = model::get(cx).read(cx).batch(&a.0).map(Batch::dir);
        if let Some(dir) = dir {
            open_folder(dir, cx);
        }
    });
    cx.on_action(|a: &OpenGameFolder, cx| {
        let m = model::get(cx).read(cx);
        let dir = m.instances.get(&a.0).and_then(|i| m.batch(&i.batch_id).map(|b| b.instance_dir(i.account_id)));
        if let Some(dir) = dir {
            open_folder(dir, cx);
        }
    });
}

/// Imports every dropped file or folder, each as its own batch.
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

fn start(path: PathBuf, cx: &mut App) {
    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let shown = path.file_stem().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| label.clone());
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let cancel = Arc::new(AtomicBool::new(false));
    model::get(cx).update(cx, |m, cx| {
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
        remote::import_batch(&path, &stop, &progress)
    });
    cx.spawn(async move |cx| {
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
        let gone = result.is_err() && !source.exists();
        let _ = cx.update(|cx| finish(id, &label, gone, &cancel, result, cx));
    })
    .detach();
}

fn finish(id: u64, label: &str, gone: bool, cancel: &AtomicBool, result: anyhow::Result<Batch>, cx: &mut App) {
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
        Ok(made) if cancelled => cx.background_spawn(async move { remote::drop_import(&made) }).detach(),
        Ok(made) => {
            super::state::reload(cx);
            super::main_window::show_batch(&made.id, cx);
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
        return forms::explain_as(trf("„%@“ nicht importiert.", &[&label]), e).text;
    };
    match refused {
        Refused::NotAPack => trf("„%@“ ist kein Modrinth-Modpack.", &[&label]),
        Refused::Loader(loader) => trf("„%@“ ist für %@ gebaut. Manycraft kann nur Fabric-Modpacks importieren.", &[&label, loader]),
        Refused::NoLoader => trf("„%@“ nennt keinen Mod-Loader. Manycraft kann nur Fabric-Modpacks importieren.", &[&label]),
        Refused::NoGame => trf("„%@“ nennt keine Minecraft-Version.", &[&label]),
        Refused::Host(_) => trf("„%@“ lädt Dateien von einer Seite, die Manycraft nicht kennt. Nichts wurde importiert.", &[&label]),
        Refused::Hash(_) => trf("Eine Datei aus „%@“ kam verändert an. Nichts wurde importiert. Versuche es noch einmal.", &[&label]),
        Refused::Path(_) => trf("„%@“ enthält Dateien, die außerhalb des Batch-Ordners landen würden. Nichts wurde importiert.", &[&label]),
        Refused::Prism(_) => trf("„%@“ lässt sich nicht übernehmen. Nur Fabric-Instanzen gehen.", &[&label]),
        Refused::Unknown => trf("„%@“ ist weder ein Modrinth-Modpack noch eine Prism-Instanz.", &[&label]),
        Refused::Cancelled => String::new(),
    }
}

fn file_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_end_matches('.');
    format!("{}.{}", if clean.is_empty() { "batch" } else { clean }, mrpack::EXTENSION)
}

fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() { path } else { path.with_extension(mrpack::EXTENSION) }
}

fn export(id: &str, cx: &mut App) {
    let m = model::get(cx).read(cx);
    let Some(made) = m.batch(id).cloned().filter(|b| !m.exporting.contains(&b.id)) else { return };
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

fn write(made: Batch, path: PathBuf, cx: &mut App) {
    let id = made.id.clone();
    model::get(cx).update(cx, |m, cx| {
        m.exporting.insert(id.clone());
        cx.notify();
    });
    let out = path.clone();
    let name = made.name.clone();
    let job = cx.background_spawn(async move { remote::export_batch(&made, &out) });
    cx.spawn(async move |cx| {
        let result = job.await;
        let _ = cx.update(|cx| {
            model::get(cx).update(cx, |m, cx| {
                m.exporting.remove(&id);
                if let Err(e) = &result {
                    m.error = Some(forms::explain_as(trf("„%@“ nicht exportiert.", &[&name]), e).text);
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
