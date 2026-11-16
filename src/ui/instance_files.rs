//! What the Welten, Screenshots and Protokolle tabs of the instance page share: commands, palette entries, undo, labels.

use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use super::palette::{Entry, Group, shortcut};
use super::theme::{Theme, control, font, iconsize, space};
use super::widgets::icon;
use super::{forms, model, undo};
use super::drive::Mark;
use crate::core::i18n::{Lang, lang, tr, trf};
use crate::core::instance::Instance;

actions!(files, [Up, Down, Left, Right, First, Last, PageUp, PageDown, Open, Delete, Copy, BackUp, Find, FindNext, FindPrevious, CopyAll]);

/// Shows tab `tab` ("worlds", "screenshots" or "logs") of instance `instance`.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct ShowInstanceTab {
    pub instance: String,
    pub tab: String,
}

/// Opens the folder of the world at this path.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenWorldFolder(pub String);

/// Zips the world at this path into the instance's backups.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct BackUpWorld(pub String);

/// Puts snapshot `id` of instance `instance` back, after asking.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct RestoreSnapshot {
    pub instance: String,
    pub id: String,
}

/// Restores the backup zip at this path, after asking.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct RestoreBackup(pub String);

/// Copies the world at `world` into the game folder of `account` in the same instance.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct CopyWorld {
    pub world: String,
    pub account: String,
}

/// Moves the world at this path to the trash, after asking.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct DeleteWorld(pub String);

/// Opens the backup folder of this instance.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenBackups(pub String);

/// Opens the screenshot at this path in the system viewer.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenShot(pub String);

/// Shows the screenshot at this path in its folder.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct RevealShot(pub String);

/// Puts the screenshot at this path on the clipboard as an image.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct CopyShot(pub String);

/// Moves the screenshot at this path to the trash.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct DeleteShot(pub String);

/// Shows the logs of this account (id) in the logs tab.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct PickLogAccount(pub String);

/// Shows the log file at this path in the logs tab.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct PickLog(pub String);

/// Uploads the log shown in the logs tab to mclo.gs, after asking.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct UploadLog;

/// Opens the folder of the log shown in the logs tab.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = manycraft)]
pub struct OpenLogsFolder;

/// What undoing a change to the files of an instance brings back.
pub enum FileUndo {
    /// A world or screenshot went to the instance's trash as `entry`.
    Deleted { instance: String, entry: String },
    /// A backup replaced the world at `world`; the world before went to the trash as `replaced`, if there was one.
    Restored { instance: String, world: PathBuf, replaced: Option<String> },
    /// A world was copied to `world`.
    Copied { instance: String, world: PathBuf },
}

/// Registers the commands of the three tabs and their keys.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &ShowInstanceTab, cx| {
        let a = a.clone();
        cx.defer(move |cx| super::instance_page::show_tab(&a.instance, &a.tab, cx));
    });
    super::instance_worlds::init(cx);
    super::instance_shots::init(cx);
    super::instance_logs::init(cx);
    let primary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    let key = |k: &str| -> &'static str { Box::leak(k.replace("primary", primary).into_boxed_str()) };
    let mut bindings = Vec::new();
    for context in [super::instance_worlds::CONTEXT, super::instance_shots::CONTEXT, super::instance_logs::CONTEXT] {
        bindings.extend([
            KeyBinding::new("up", Up, Some(context)),
            KeyBinding::new("down", Down, Some(context)),
            KeyBinding::new("home", First, Some(context)),
            KeyBinding::new("end", Last, Some(context)),
            KeyBinding::new("pageup", PageUp, Some(context)),
            KeyBinding::new("pagedown", PageDown, Some(context)),
        ]);
    }
    for context in [super::instance_worlds::CONTEXT, super::instance_shots::CONTEXT] {
        bindings.extend([
            KeyBinding::new("enter", Open, Some(context)),
            KeyBinding::new(key("primary-backspace"), Delete, Some(context)),
            KeyBinding::new("delete", Delete, Some(context)),
        ]);
    }
    bindings.extend([
        KeyBinding::new("left", Left, Some(super::instance_shots::CONTEXT)),
        KeyBinding::new("right", Right, Some(super::instance_shots::CONTEXT)),
        KeyBinding::new(key("primary-c"), Copy, Some(super::instance_shots::CONTEXT)),
        KeyBinding::new(key("primary-s"), BackUp, Some(super::instance_worlds::CONTEXT)),
        KeyBinding::new(key("primary-f"), Find, Some(super::instance_logs::CONTEXT)),
        KeyBinding::new("f3", FindNext, Some(super::instance_logs::CONTEXT)),
        KeyBinding::new("shift-f3", FindPrevious, Some(super::instance_logs::CONTEXT)),
        KeyBinding::new(key("primary-g"), FindNext, Some(super::instance_logs::CONTEXT)),
        KeyBinding::new(key("primary-shift-g"), FindPrevious, Some(super::instance_logs::CONTEXT)),
        KeyBinding::new(key("primary-shift-c"), CopyAll, Some(super::instance_logs::CONTEXT)),
    ]);
    cx.bind_keys(bindings);
}

/// Palette entries for the page of instance `id`: its tabs, and what the selection in the shown tab can do.
pub fn palette(id: &str, cx: &App) -> Vec<Entry> {
    let mut out = Vec::new();
    let tab = super::instance_page::tab_key(cx);
    match tab {
        Some("worlds") => out.extend(super::instance_worlds::palette(id, cx)),
        Some("screenshots") => out.extend(super::instance_shots::palette(id, cx)),
        Some("logs") => out.extend(super::instance_logs::palette(id, cx)),
        _ => {}
    }
    out
}

/// The palette entries that show the three tabs of `made`.
pub fn tab_entries(made: &Instance, cx: &App) -> Vec<Entry> {
    let name = &made.name;
    [("earth", trf("Welten von %@", &[name]), "worlds"), ("image", trf("Screenshots von %@", &[name]), "screenshots"), ("scroll-text", trf("Protokolle von %@", &[name]), "logs")]
        .into_iter()
        .map(|(symbol, label, tab)| entry(Group::Instances, symbol, label, Some(made.version_line()), Box::new(ShowInstanceTab { instance: made.id.clone(), tab: tab.to_owned() }), None, cx))
        .collect()
}

/// A palette entry that shows the shortcut of `keys` beside it, if that has one.
pub fn entry(group: Group, symbol: &'static str, label: String, detail: Option<String>, action: Box<dyn Action>, keys: Option<&dyn Action>, cx: &App) -> Entry {
    let keys = keys.and_then(|k| shortcut(k, cx));
    Entry { group, icon: symbol, label, detail, keys, action, enabled: true, mass: false }
}

/// The name of account `id`, or that it is gone.
pub fn account_name(id: Uuid, cx: &App) -> String {
    model::get(cx).read(cx).accounts.iter().find(|a| a.id == id).map_or_else(|| tr("Entfernter Account"), |a| a.name.clone())
}

/// Whether the game of `account` in instance `instance` runs.
pub fn playing(instance: &str, account: Uuid, cx: &App) -> bool {
    model::get(cx).read(cx).games.values().any(|g| g.instance_id == instance && g.account_id == account && g.phase.is_active())
}

/// The folder of instance `id`, if it exists.
pub fn instance_dir(id: &str, cx: &App) -> Option<PathBuf> {
    model::get(cx).read(cx).instance(id).map(Instance::dir)
}

fn local(secs: i64) -> Option<chrono::DateTime<chrono::Local>> {
    chrono::DateTime::from_timestamp(secs, 0).map(|d| d.with_timezone(&chrono::Local))
}

/// "Heute, 14:03", "Gestern, 14:03" or "15.11.2026, 14:03" for unix seconds `secs`.
pub fn when(secs: i64) -> String {
    let Some(at) = local(secs) else { return String::new() };
    let today = chrono::Local::now().date_naive();
    let clock = at.format("%H:%M").to_string();
    if at.date_naive() == today {
        trf("Heute, %@", &[&clock])
    } else if today.pred_opt() == Some(at.date_naive()) {
        trf("Gestern, %@", &[&clock])
    } else {
        format!("{}, {clock}", at.format("%d.%m.%Y"))
    }
}

/// "15.11.2026, 14:03" for unix seconds `secs`.
pub fn stamp(secs: i64) -> String {
    local(secs).map(|at| at.format("%d.%m.%Y, %H:%M").to_string()).unwrap_or_default()
}

/// Opens `path` with the system's default app.
pub fn open(path: PathBuf, cx: &mut App) {
    if !crate::core::drive::fenced("open", path.display()) {
        cx.open_with_system(&path);
    }
}

/// Creates the folder `dir` if it is missing and opens it.
pub fn open_made(dir: PathBuf, cx: &mut App) {
    let made = dir.clone();
    let task = cx.background_spawn(async move { std::fs::create_dir_all(&made) });
    cx.spawn(async move |cx| {
        if task.await.is_ok() {
            let _ = cx.update(|cx| open(dir, cx));
        }
    })
    .detach();
}

/// Shows `path` selected in its folder.
pub fn reveal(path: PathBuf, cx: &mut App) {
    if !crate::core::drive::fenced("reveal", path.display()) {
        cx.reveal_path(&path);
    }
}

/// "840 KB", "12,4 MB", "1,2 GB", with the decimal mark of the UI language.
pub fn bytes(n: u64) -> String {
    let (value, unit) = match n {
        n if n < 1 << 20 => return format!("{} KB", n.div_ceil(1 << 10)),
        n if n < 1 << 30 => (n as f64 / f64::from(1u32 << 20), "MB"),
        n => (n as f64 / f64::from(1u32 << 30), "GB"),
    };
    let text = if value >= 100. { format!("{value:.0}") } else { format!("{value:.1}") };
    let text = if lang() == Lang::De { text.replace('.', ",") } else { text };
    format!("{text} {unit}")
}

/// A centred empty, loading or failed state: a symbol, a title, a line and an optional button.
pub fn empty(id: &'static str, symbol: &str, title: String, text: String, button: Option<AnyElement>, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(space::M))
        .px(px(space::XL))
        .child(icon(symbol, iconsize::HERO, t.tertiary))
        .child(div().id(SharedString::from(format!("{id}-title"))).mark("heading", title.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
        .child(div().max_w(px(control::READING)).text_size(px(font::CALLOUT)).text_color(t.secondary).text_center().child(text))
        .children(button.map(|b| div().mt(px(space::S)).child(b)))
}

/// Undoes `what`, which removed or replaced `name`; says why when it cannot, and offers a retry when that may help.
pub fn undo(name: String, what: FileUndo, at: Instant, cx: &mut App) {
    let (instance, world, entry) = match &what {
        FileUndo::Deleted { instance, entry } => (instance.clone(), None, Some(entry.clone())),
        FileUndo::Restored { instance, world, replaced } => (instance.clone(), Some(world.clone()), replaced.clone()),
        FileUndo::Copied { instance, world } => (instance.clone(), Some(world.clone()), None),
    };
    let Some(dir) = instance_dir(&instance, cx) else {
        undo::note(tr("Diese Instanz gibt es nicht mehr."), cx);
        return;
    };
    let task = cx.background_spawn(async move {
        if let (Some(_), Some(entry)) = (&world, &entry)
            && !crate::core::trash::holds(&dir, entry)
        {
            anyhow::bail!(crate::core::instance::Final(tr("Das liegt nicht mehr im Papierkorb.")));
        }
        let mut parked = None;
        if let Some(world) = &world {
            if crate::core::worlds::is_open(world) {
                anyhow::bail!(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst."));
            }
            if std::fs::symlink_metadata(world).is_ok() {
                parked = Some(crate::core::trash::put(&dir, world)?);
            }
        }
        match &entry {
            Some(entry) => crate::core::trash::take_back(&dir, entry).map(Some).inspect_err(|_| {
                if let Some(parked) = &parked {
                    let _ = crate::core::trash::take_back(&dir, parked);
                }
            }),
            None => Ok(None),
        }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        let _ = cx.update(|cx| {
            match result {
                Ok(back) => changed(&instance, back, cx),
                Err(e) => {
                    undo::note(forms::explain_as(tr("Nicht zurückgeholt."), &e).text, cx);
                    if !undo::permanent(&e) {
                        undo::retry(undo::Kind::Files { name, undo: what }, at, cx);
                    }
                }
            }
        });
    })
    .detach();
}

/// Tells the tabs that files of instance `id` changed, and which path came back, if one did.
pub fn changed(id: &str, back: Option<PathBuf>, cx: &mut App) {
    super::instance_worlds::changed(id, back.clone(), cx);
    super::instance_shots::changed(id, back, cx);
}

#[cfg(test)]
mod tests {
    use super::bytes;

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(bytes(0), "0 KB");
        assert_eq!(bytes(1), "1 KB");
        assert_eq!(bytes(5 << 20).replace(',', "."), "5.0 MB");
        assert_eq!(bytes(150 << 20), "150 MB");
        assert_eq!(bytes(3 << 30).replace(',', "."), "3.0 GB");
    }
}
