//! The Welten tab of the instance page: the worlds in every game folder of the instance, their backups, and what can be done with them.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_kit::component::{FocusableExt, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::drive::Mark;
use super::forms::{self, Problem};
use super::instance_files::{self as files, BackUpWorld, CopyWorld, DeleteWorld, FileUndo, OpenBackups, OpenWorldFolder, RestoreBackup, RestoreSnapshot};
use super::motion::Soft;
use super::palette::{Entry, Group};
use super::sheet::{self, Look};
use super::theme::{self, Theme, control, font, iconsize, radius, space, tint};
use super::widgets::{icon, menu_of, tip};
use super::{model, snapshots, undo};
use crate::core::i18n::{tr, trf};
use crate::core::instance::games_dir;
use crate::core::snapshot::{self, Snapshot};
use crate::core::worlds::{self, Backup, Mode, Progress, World};

/// Key context of the world list.
pub const CONTEXT: &str = "InstanceWorlds";
const TICK: Duration = Duration::from_millis(120);

struct Shown(Entity<Worlds>);

impl Global for Shown {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Doing {
    BackUp,
    Restore,
    Copy,
    Delete,
}

struct Work {
    doing: Doing,
    progress: Arc<Progress>,
}

enum Outcome {
    Done(String),
    Failed(Problem),
}

#[derive(Clone)]
enum Row {
    World(World),
    Lone(Backup),
}

impl Row {
    fn key(&self) -> &Path {
        match self {
            Self::World(w) => &w.dir,
            Self::Lone(b) => &b.path,
        }
    }
}

struct Worlds {
    id: String,
    dir: Option<PathBuf>,
    loaded: bool,
    rows: Vec<Row>,
    backups: Vec<Backup>,
    snapshots: Vec<Snapshot>,
    sizes: HashMap<PathBuf, u64>,
    icons: HashMap<PathBuf, Arc<RenderImage>>,
    selected: Option<PathBuf>,
    work: HashMap<PathBuf, Work>,
    outcome: Option<Outcome>,
    details: bool,
    scan: u64,
    stale: bool,
    playing: Vec<Uuid>,
    ticking: bool,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _model: Subscription,
}

/// Registers the world commands.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &OpenWorldFolder, cx| {
        let path = PathBuf::from(&a.0);
        if path.extension().is_some_and(|x| x == "zip") {
            files::reveal(path, cx);
        } else {
            files::open(path, cx);
        }
    });
    cx.on_action(|a: &OpenBackups, cx| {
        if let Some(dir) = files::instance_dir(&a.0, cx) {
            files::open_made(worlds::backups_dir(&dir), cx);
        }
    });
    cx.on_action(|a: &BackUpWorld, cx| {
        let path = PathBuf::from(&a.0);
        with(cx, move |s, window, cx| s.ask_back_up(&path, window, cx));
    });
    cx.on_action(|a: &RestoreBackup, cx| {
        let path = PathBuf::from(&a.0);
        with(cx, move |s, window, cx| s.ask_restore(&path, window, cx));
    });
    cx.on_action(|a: &RestoreSnapshot, cx| {
        let a = a.clone();
        with(cx, move |s, window, cx| {
            if s.id == a.instance
                && let Some(snap) = s.snapshots.iter().find(|n| n.id == a.id).cloned()
            {
                snapshots::ask_restore(&a.instance, snap, window, cx);
            }
        });
    });
    cx.on_action(|a: &CopyWorld, cx| {
        let (path, account) = (PathBuf::from(&a.world), Uuid::parse_str(&a.account).ok());
        with(cx, move |s, _, cx| {
            if let Some(account) = account {
                s.copy(&path, account, cx);
            }
        });
    });
    cx.on_action(|a: &DeleteWorld, cx| {
        let path = PathBuf::from(&a.0);
        with(cx, move |s, window, cx| s.ask_delete(&path, window, cx));
    });
}

fn with(cx: &mut App, f: impl FnOnce(&mut Worlds, &mut Window, &mut Context<Worlds>) + 'static) {
    let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) else { return };
    sheet::in_window(cx, move |window, cx| view.update(cx, |s, cx| f(s, window, cx)));
}

/// The Welten tab of instance `id`; `fresh` reads the worlds again.
pub fn view(id: &str, fresh: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let view = match cx.try_global::<Shown>() {
        Some(shown) => shown.0.clone(),
        None => {
            let view = cx.new(|cx| Worlds::new(id, cx));
            cx.set_global(Shown(view.clone()));
            view
        }
    };
    view.update(cx, |s, cx| {
        if s.id != id {
            s.bind(id, cx);
        } else if fresh {
            s.rescan(cx);
        }
    });
    if fresh && !window.has_active_dialog(cx) {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    }
    view.into_any_element()
}

/// Reads the worlds of instance `id` again where they are shown, and selects `back` when it came back.
pub fn changed(id: &str, back: Option<PathBuf>, cx: &mut App) {
    super::instance_page::worlds_changed(id, cx);
    let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) else { return };
    view.update(cx, |s, cx| {
        if s.id == id {
            if let Some(back) = back.filter(|b| b.parent().and_then(Path::file_name).is_some_and(|n| n == "saves")) {
                s.selected = Some(back);
            }
            s.rescan(cx);
        }
    });
}

/// Palette entries for the selected world of instance `id`.
pub fn palette(id: &str, cx: &App) -> Vec<Entry> {
    let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) else { return Vec::new() };
    let s = view.read(cx);
    if s.id != id {
        return Vec::new();
    }
    let mut out = Vec::new();
    match s.selected_row() {
        Some(Row::World(w)) => {
            let name = w.name().to_owned();
            let path = w.dir.display().to_string();
            out.push(files::entry(Group::Selection, "archive", trf("Sicherung von „%@“ erstellen", &[&name]), None, Box::new(BackUpWorld(path.clone())), Some(&files::BackUp), cx));
            out.push(files::entry(Group::Selection, "folder-open", trf("Ordner von „%@“ öffnen", &[&name]), None, Box::new(OpenWorldFolder(path.clone())), Some(&files::Open), cx));
            if let Some(newest) = s.backups_of(w.account, &w.folder).first() {
                out.push(files::entry(Group::Selection, "archive-restore", trf("„%@“ wiederherstellen", &[&name]), Some(files::stamp(newest.at)), Box::new(RestoreBackup(newest.path.display().to_string())), None, cx));
            }
            for b in s.backups_of(w.account, &w.folder).into_iter().skip(1) {
                out.push(files::entry(Group::Selection, "archive-restore", trf("„%@“ wiederherstellen", &[&name]), Some(files::stamp(b.at)), Box::new(RestoreBackup(b.path.display().to_string())), None, cx));
            }
            for (account, to) in s.copy_targets(&Row::World(w.clone()), cx) {
                out.push(files::entry(Group::Selection, "copy", trf("„%1$@“ zu %2$@ kopieren", &[&name, &to]), None, Box::new(CopyWorld { world: path.clone(), account: account.to_string() }), None, cx));
            }
            out.push(files::entry(Group::Selection, "trash", trf("„%@“ löschen…", &[&name]), None, Box::new(DeleteWorld(path)), Some(&files::Delete), cx));
        }
        Some(Row::Lone(b)) => out.push(files::entry(Group::Selection, "archive-restore", trf("„%@“ wiederherstellen", &[&b.name]), Some(files::stamp(b.at)), Box::new(RestoreBackup(b.path.display().to_string())), None, cx)),
        None => {}
    }
    for snap in &s.snapshots {
        out.push(files::entry(Group::Commands, "archive-restore", trf("Stand „%@“ wiederherstellen", &[&snap.reason.label()]), Some(files::stamp(snap.at)), Box::new(RestoreSnapshot { instance: id.to_owned(), id: snap.id.clone() }), None, cx));
    }
    out.push(files::entry(Group::Commands, "folder-open", tr("Ordner mit den Sicherungen öffnen"), None, Box::new(OpenBackups(id.to_owned())), None, cx));
    out
}

impl Worlds {
    fn new(id: &str, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&model::get(cx), |s: &mut Self, _, cx| {
            let now = s.active(cx);
            if now != s.playing {
                if s.playing.iter().any(|a| !now.contains(a)) {
                    s.stale = true;
                }
                s.playing = now;
                cx.notify();
            }
        });
        let mut page = Self {
            id: String::new(),
            dir: None,
            loaded: false,
            rows: Vec::new(),
            backups: Vec::new(),
            snapshots: Vec::new(),
            sizes: HashMap::new(),
            icons: HashMap::new(),
            selected: None,
            work: HashMap::new(),
            outcome: None,
            details: false,
            scan: 0,
            stale: false,
            playing: Vec::new(),
            ticking: false,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _model: subscription,
        };
        page.bind(id, cx);
        page
    }

    fn active(&self, cx: &App) -> Vec<Uuid> {
        let mut active: Vec<Uuid> = model::get(cx).read(cx).games.values().filter(|g| g.instance_id == self.id && g.phase.is_active()).map(|g| g.account_id).collect();
        active.sort_unstable();
        active
    }

    fn bind(&mut self, id: &str, cx: &mut Context<Self>) {
        self.id = id.to_owned();
        self.loaded = false;
        self.rows.clear();
        self.backups.clear();
        self.snapshots.clear();
        self.sizes.clear();
        self.selected = None;
        self.outcome = None;
        self.playing = self.active(cx);
        self.scroll = UniformListScrollHandle::new();
        self.rescan(cx);
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.dir = files::instance_dir(&self.id, cx);
        let Some(dir) = self.dir.clone() else {
            self.loaded = true;
            cx.notify();
            return;
        };
        self.scan += 1;
        let scan = self.scan;
        let task = cx.background_spawn(async move {
            let found = worlds::list(&dir);
            let mut backups = worlds::backups(&dir);
            backups.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.path.cmp(&a.path)));
            let icons: Vec<(PathBuf, u32, u32, Vec<u8>)> = found
                .iter()
                .filter_map(|w| {
                    let path = w.icon.as_ref()?;
                    let picture = crate::core::shots::thumbnail(path, theme::files::WORLD_ICON_PX, theme::files::WORLD_ICON_PX).ok()?;
                    Some((w.dir.clone(), picture.width(), picture.height(), picture.into_raw()))
                })
                .collect();
            let snaps = snapshot::list(&dir);
            (found, backups, icons, snaps)
        });
        cx.spawn(async move |this, cx| {
            let (found, backups, icons, snaps) = task.await;
            let dirs: Vec<PathBuf> = found.iter().map(|w| w.dir.clone()).collect();
            let fresh = this.update(cx, |s, cx| {
                if s.scan != scan {
                    return false;
                }
                s.snapshots = snaps;
                s.take(found, backups, icons, cx);
                true
            });
            if !matches!(fresh, Ok(true)) {
                return;
            }
            let sizes = cx.background_executor().spawn(async move { dirs.into_iter().map(|d| (worlds::size(&d), d)).map(|(n, d)| (d, n)).collect::<HashMap<_, _>>() }).await;
            let _ = this.update(cx, |s, cx| {
                if s.scan == scan {
                    s.sizes = sizes;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn take(&mut self, found: Vec<World>, backups: Vec<Backup>, icons: Vec<(PathBuf, u32, u32, Vec<u8>)>, cx: &mut Context<Self>) {
        for (_, image) in self.icons.drain() {
            cx.drop_image(image, None);
        }
        self.icons = icons.into_iter().filter_map(|(dir, w, h, rgba)| Some((dir, super::state::render_image(w, h, rgba)?))).collect();
        let mut rows: Vec<Row> = found.iter().cloned().map(Row::World).collect();
        let mut seen: Vec<(Uuid, &str)> = found.iter().map(|w| (w.account, w.folder.as_str())).collect();
        for b in &backups {
            if !seen.contains(&(b.account, b.folder.as_str())) {
                seen.push((b.account, b.folder.as_str()));
                rows.push(Row::Lone(b.clone()));
            }
        }
        self.rows = rows;
        self.backups = backups;
        self.loaded = true;
        if self.selected.as_ref().is_none_or(|k| !self.rows.iter().any(|r| r.key() == k)) {
            self.selected = self.rows.first().map(|r| r.key().to_path_buf());
        }
        if let Some(at) = self.at() {
            self.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn at(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|r| r.key() == selected)
    }

    fn selected_row(&self) -> Option<&Row> {
        self.at().and_then(|at| self.rows.get(at))
    }

    fn world(&self, dir: &Path) -> Option<&World> {
        self.rows.iter().find_map(|r| match r {
            Row::World(w) if w.dir == dir => Some(w),
            _ => None,
        })
    }

    fn backups_of(&self, account: Uuid, folder: &str) -> Vec<&Backup> {
        self.backups.iter().filter(|b| b.account == account && b.folder == folder).collect()
    }

    fn target(&self, backup: &Backup) -> Option<(PathBuf, PathBuf)> {
        let game = games_dir(self.dir.as_ref()?).join(backup.account.simple().to_string());
        let world = game.join("saves").join(&backup.folder);
        Some((game, world))
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let at = self.at().map_or(0, |at| at.saturating_add_signed(by).min(self.rows.len() - 1));
        self.select(at, cx);
    }

    fn page(&self) -> isize {
        let height = self.scroll.0.borrow().base_handle.bounds().size.height;
        ((height / px(theme::files::WORLD_ROW)).floor() as isize).max(1)
    }

    fn select(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(at) else { return };
        if self.selected.as_deref() != Some(row.key()) && matches!(self.outcome, Some(Outcome::Done(_))) {
            self.outcome = None;
        }
        self.selected = Some(row.key().to_path_buf());
        self.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn dispatch_selected(&self, what: fn(&Row) -> Option<Box<dyn Action>>, window: &mut Window, cx: &mut App) {
        if let Some(action) = self.selected_row().and_then(what) {
            window.dispatch_action(action, cx);
        }
    }

    fn begin(&mut self, key: PathBuf, doing: Doing, cx: &mut Context<Self>) -> Arc<Progress> {
        let progress = Arc::new(Progress::default());
        self.work.insert(key, Work { doing, progress: progress.clone() });
        self.outcome = None;
        self.details = false;
        if !self.ticking {
            self.ticking = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(TICK).await;
                    let go = this.update(cx, |s, cx| {
                        cx.notify();
                        s.ticking = !s.work.is_empty();
                        s.ticking
                    });
                    if !matches!(go, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
        progress
    }

    fn end(&mut self, key: &Path, outcome: Option<Outcome>, cx: &mut Context<Self>) {
        self.work.remove(key);
        self.outcome = outcome;
        self.details = false;
        self.rescan(cx);
    }

    fn refuse(&mut self, text: String, cx: &mut Context<Self>) {
        self.outcome = Some(Outcome::Failed(Problem::plain(text)));
        self.details = false;
        cx.notify();
    }

    fn ask_back_up(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(world) = self.world(dir).cloned() else { return };
        if self.work.contains_key(&world.dir) {
            return;
        }
        if !files::playing(&self.id, world.account, cx) {
            self.back_up(world, cx);
            return;
        }
        let dir = world.dir.clone();
        let open = cx.background_spawn(async move { worlds::is_open(&dir) });
        cx.spawn_in(window, async move |this, cx| {
            let open = open.await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.work.contains_key(&world.dir) {
                    return;
                }
                if open {
                    s.refuse(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst."), cx);
                    return;
                }
                let view = cx.entity();
                let title = trf("„%@“ trotzdem sichern?", &[&world.name()]);
                sheet::confirm(window, cx, title, tr("Das Spiel dieses Accounts läuft. Öffnet es die Welt während der Sicherung, fehlt in der Sicherung vielleicht etwas."), tr("Trotzdem sichern"), |_| None, move |_, cx| {
                    let world = world.clone();
                    view.update(cx, |s, cx| s.back_up(world, cx));
                });
            });
        })
        .detach();
    }

    fn back_up(&mut self, world: World, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else { return };
        let key = world.dir.clone();
        let name = world.name().to_owned();
        let progress = self.begin(key.clone(), Doing::BackUp, cx);
        let task = cx.background_spawn(async move { worlds::back_up(&dir, &world, &progress) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                let outcome = match result {
                    Ok(_) => Outcome::Done(trf("„%@“ gesichert.", &[&name])),
                    Err(e) => Outcome::Failed(forms::explain_as(trf("„%@“ wurde nicht gesichert.", &[&name]), &e)),
                };
                s.end(&key, Some(outcome), cx);
            });
        })
        .detach();
    }

    fn ask_restore(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backup) = self.backups.iter().find(|b| b.path == path).cloned() else { return };
        let Some((game, world)) = self.target(&backup) else { return };
        if self.work.contains_key(&world) {
            return;
        }
        let probe = world.clone();
        let open = cx.background_spawn(async move { worlds::is_open(&probe) });
        cx.spawn_in(window, async move |this, cx| {
            let open = open.await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.work.contains_key(&world) {
                    return;
                }
                if open {
                    s.refuse(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst."), cx);
                } else {
                    s.confirm_restore(backup, game, world, window, cx);
                }
            });
        })
        .detach();
    }

    fn confirm_restore(&mut self, backup: Backup, game: PathBuf, world: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let stamp = files::stamp(backup.at);
        let message = if self.world(&world).is_some() {
            trf("Die Welt kommt auf den Stand vom %@. Die jetzige kommt in den Papierkorb, das lässt sich gleich danach rückgängig machen.", &[&stamp])
        } else {
            trf("Die Welt kommt mit dem Stand vom %@ zurück.", &[&stamp])
        };
        let view = cx.entity();
        let title = trf("„%@“ wiederherstellen?", &[&backup.name]);
        sheet::confirm(window, cx, title, message, tr("Wiederherstellen"), |_| None, move |_, cx| {
            let (backup, game, world) = (backup.clone(), game.clone(), world.clone());
            view.update(cx, |s, cx| s.restore(backup, game, world, cx));
        });
    }

    fn restore(&mut self, backup: Backup, game: PathBuf, world: PathBuf, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else { return };
        let (id, name) = (self.id.clone(), backup.name.clone());
        let progress = self.begin(world.clone(), Doing::Restore, cx);
        let task = cx.background_spawn(async move { worlds::restore(&dir, &backup, &game, &progress) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| match result {
                Ok(restored) => {
                    s.selected = Some(restored.world.clone());
                    undo::offer(undo::Kind::Files { name: name.clone(), undo: FileUndo::Restored { instance: id.clone(), world: restored.world, replaced: restored.replaced } }, cx);
                    s.end(&world, None, cx);
                }
                Err(e) => s.end(&world, Some(Outcome::Failed(forms::explain_as(trf("„%@“ wurde nicht wiederhergestellt.", &[&name]), &e))), cx),
            });
        })
        .detach();
    }

    fn copy(&mut self, dir: &Path, account: Uuid, cx: &mut Context<Self>) {
        let Some(world) = self.world(dir).cloned() else { return };
        let Some(instance) = self.dir.clone() else { return };
        if self.work.contains_key(&world.dir) || world.account == account {
            return;
        }
        let game = games_dir(&instance).join(account.simple().to_string());
        let (key, name) = (world.dir.clone(), world.name().to_owned());
        let id = self.id.clone();
        let progress = self.begin(key.clone(), Doing::Copy, cx);
        let task = cx.background_spawn(async move { worlds::copy_to(&world, &game, &progress) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| match result {
                Ok(copy) => {
                    undo::offer(undo::Kind::Files { name: name.clone(), undo: FileUndo::Copied { instance: id.clone(), world: copy } }, cx);
                    s.end(&key, None, cx);
                    s.rescan(cx);
                }
                Err(e) => s.end(&key, Some(Outcome::Failed(forms::explain_as(trf("„%@“ wurde nicht kopiert.", &[&name]), &e))), cx),
            });
        })
        .detach();
    }

    fn ask_delete(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(world) = self.world(dir).cloned() else { return };
        if self.work.contains_key(&world.dir) {
            return;
        }
        let view = cx.entity();
        let title = trf("„%@“ löschen?", &[&world.name()]);
        sheet::confirm_danger(window, cx, title, tr("Die Welt kommt in den Papierkorb und bleibt dort 7 Tage."), tr("Löschen"), |_| None, move |_, cx| {
            let world = world.clone();
            view.update(cx, |s, cx| s.delete(world, cx));
        });
    }

    fn delete(&mut self, world: World, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else { return };
        let (id, key, name) = (self.id.clone(), world.dir.clone(), world.name().to_owned());
        self.begin(key.clone(), Doing::Delete, cx);
        let path = key.clone();
        let task = cx.background_spawn(async move { worlds::delete(&dir, &path) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| match result {
                Ok(entry) => {
                    if s.selected.as_ref() == Some(&key)
                        && let Some(at) = s.at()
                    {
                        let next = s.rows.get(at + 1).or_else(|| at.checked_sub(1).and_then(|p| s.rows.get(p)));
                        s.selected = next.map(|r| r.key().to_path_buf());
                    }
                    undo::offer(undo::Kind::Files { name: name.clone(), undo: FileUndo::Deleted { instance: id.clone(), entry } }, cx);
                    s.end(&key, None, cx);
                }
                Err(e) => s.end(&key, Some(Outcome::Failed(forms::explain_as(trf("„%@“ wurde nicht gelöscht.", &[&name]), &e))), cx),
            });
        })
        .detach();
    }

    fn line(&self, row: &Row, cx: &App) -> String {
        match row {
            Row::World(w) => {
                let mut parts = vec![files::account_name(w.account, cx)];
                if w.level.hardcore {
                    parts.push(tr("Hardcore"));
                } else if let Some(mode) = w.level.mode {
                    parts.push(mode_label(mode));
                }
                if let Some(at) = w.level.last_played {
                    parts.push(files::when(at.div_euclid(1000)));
                }
                if let Some(size) = self.sizes.get(&w.dir) {
                    parts.push(files::bytes(*size));
                }
                parts.join(" · ")
            }
            Row::Lone(b) => [tr("Nur als Sicherung"), files::account_name(b.account, cx), files::when(b.at)].join(" · "),
        }
    }

    fn doing(&self, row: &Row) -> Option<&Work> {
        match row {
            Row::World(w) => self.work.get(&w.dir),
            Row::Lone(b) => self.target(b).and_then(|(_, world)| self.work.get(&world)),
        }
    }

    fn row(&self, at: usize, row: &Row, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let selected = self.selected.as_deref() == Some(row.key());
        let key = format!("world-{at}");
        let (name, symbol) = match row {
            Row::World(w) => (w.name().to_owned(), "earth"),
            Row::Lone(b) => (b.name.clone(), "archive"),
        };
        let lone = matches!(row, Row::Lone(_));
        let picture = match row {
            Row::World(w) => self.icons.get(&w.dir).cloned(),
            Row::Lone(_) => None,
        };
        let thumb = div()
            .size(px(theme::files::WORLD_ICON))
            .flex_none()
            .rounded(px(radius::INNER))
            .overflow_hidden()
            .bg(t.well)
            .flex()
            .items_center()
            .justify_center()
            .map(|d| match picture {
                Some(image) => d.child(img(ImageSource::Render(image)).size_full()),
                None => d.child(icon(symbol, iconsize::L, t.tertiary)),
            });
        let trailing = self.doing(row).map(|work| {
            let percent = (work.progress.fraction() * 100.).round() as u32;
            let text = match work.doing {
                Doing::BackUp => trf("Wird gesichert … %lld %", &[&percent]),
                Doing::Restore => trf("Wird wiederhergestellt … %lld %", &[&percent]),
                Doing::Copy => trf("Wird kopiert … %lld %", &[&percent]),
                Doing::Delete => tr("Wird gelöscht …"),
            };
            div().flex_none().text_size(px(font::CALLOUT)).font_features(super::toolbar::tabular()).text_color(t.secondary).child(text)
        });
        let playing = match row {
            Row::World(w) => self.playing.contains(&w.account),
            Row::Lone(_) => false,
        };
        let badge = (playing && trailing.is_none()).then(|| div().flex_none().text_size(px(font::CAPTION)).text_color(t.success_text).child(tr("Spiel läuft")));
        let menu_row = row.clone();
        let accounts = self.copy_targets(row, cx);
        let backups: Vec<Backup> = match row {
            Row::World(w) => self.backups_of(w.account, &w.folder).into_iter().cloned().collect(),
            Row::Lone(b) => self.backups_of(b.account, &b.folder).into_iter().cloned().collect(),
        };
        let open_key = key.clone();
        div()
            .id(SharedString::from(key.clone()))
            .mark("row", name.clone())
            .w_full()
            .h(px(theme::files::WORLD_ROW))
            .px(px(space::S))
            .rounded(px(radius::CONTROL))
            .flex()
            .items_center()
            .gap(px(space::M))
            .cursor_pointer()
            .when(selected, |d| d.bg(t.accent.opacity(tint::SELECTED)))
            .when(!selected, |d| d.soft(t.ink.opacity(tint::HOVER), radius::CONTROL))
            .on_mouse_down(MouseButton::Left, cx.listener(move |s, _, window, cx| {
                window.focus(&s.focus, cx);
                s.select(at, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |s, _, _, cx| s.select(at, cx)))
            .on_click(cx.listener(move |s, e: &ClickEvent, window, cx| {
                if e.click_count() == 2 {
                    s.dispatch_selected(open_action, window, cx);
                }
            }))
            .child(div().when(lone, |d| d.opacity(theme::tile::DIMMED)).child(thumb))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .when(lone, |d| d.opacity(theme::tile::DIMMED))
                    .child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(name))
                    .child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(self.line(row, cx))),
            )
            .children(badge)
            .children(trailing)
            .context_menu(move |menu, window, cx| menu_of(&open_key, row_menu(menu, &menu_row, &backups, &accounts, window, cx), window, cx))
            .into_any_element()
    }

    fn snapshot_section(&self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        let taking = snapshots::progress(&self.id, cx);
        if self.snapshots.is_empty() && taking.is_none() {
            return None;
        }
        let blocked = (!self.playing.is_empty()).then(|| SharedString::from(tr("Stoppe zuerst die laufenden Spiele.")));
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(done) = taking {
            let text = trf("Wird gesichert … %lld %", &[&((done * 100.).round() as u32)]);
            rows.push(div().h(px(theme::files::SNAPSHOT_ROW)).px(px(space::S)).flex().items_center().gap(px(space::M)).child(icon("archive-restore", iconsize::M, t.tertiary)).child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).font_features(super::toolbar::tabular()).text_color(t.secondary).child(text)).into_any_element());
        }
        for (at, snap) in self.snapshots.iter().enumerate() {
            let label = snap.reason.label();
            let line = [files::when(snap.at), snap.version_line.clone(), files::bytes(snap.size)].join(" · ");
            let action = RestoreSnapshot { instance: self.id.clone(), id: snap.id.clone() };
            let restore = sheet::button_unless(SharedString::from(format!("snapshot-restore-{at}")), tr("Wiederherstellen…"), None, Look::Plain, blocked.clone(), super::snapshots::restoring(&self.id, cx), t)
                .when(blocked.is_none(), |b| b.on_click(move |_, window, cx| window.dispatch_action(Box::new(action.clone()), cx)));
            rows.push(
                div()
                    .id(SharedString::from(format!("snapshot-{at}")))
                    .mark("row", label.clone())
                    .h(px(theme::files::SNAPSHOT_ROW))
                    .px(px(space::S))
                    .flex()
                    .items_center()
                    .gap(px(space::M))
                    .child(icon("archive-restore", iconsize::M, t.tertiary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_size(px(font::BODY)).child(label))
                            .child(div().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(line)),
                    )
                    .child(restore)
                    .into_any_element(),
            );
        }
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(space::XXS))
                .pt(px(space::S))
                .border_t_1()
                .border_color(t.separator)
                .child(div().px(px(space::S)).text_size(px(font::CALLOUT)).font_weight(FontWeight::SEMIBOLD).text_color(t.secondary).child(tr("Vor Änderungen gesichert")))
                .children(rows),
        )
    }

    fn copy_targets(&self, row: &Row, cx: &App) -> Vec<(Uuid, String)> {
        let Row::World(w) = row else { return Vec::new() };
        let mut accounts: Vec<(Uuid, String)> = model::get(cx).read(cx).accounts.iter().filter(|a| a.id != w.account).map(|a| (a.id, a.name.clone())).collect();
        accounts.sort_by_key(|a| a.1.to_lowercase());
        accounts
    }

    fn toolbar(&self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let worlds = self.rows.iter().filter(|r| matches!(r, Row::World(_))).count();
        let mut count = if worlds == 1 { tr("1 Welt") } else { trf("%lld Welten", &[&worlds]) };
        match self.backups.len() {
            0 => {}
            1 => count = format!("{count} · {}", tr("1 Sicherung")),
            n => count = format!("{count} · {}", trf("%lld Sicherungen", &[&n])),
        }
        let rings = theme::rings(cx);
        let mut actions: Vec<AnyElement> = Vec::new();
        let row = self.selected_row().cloned();
        let busy = row.as_ref().and_then(|r| self.doing(r)).is_some();
        let wait = busy.then(|| SharedString::from(tr("Warte, bis das fertig ist.")));
        if let Some(Row::World(w)) = &row {
            let path = w.dir.display().to_string();
            let back_up = sheet::button_unless("worlds-back-up", tr("Sicherung erstellen"), Some("archive"), Look::Secondary, wait.clone(), false, t)
                .when(!busy, |b| b.tooltip(tip(super::palette::hint(tr("Sicherung erstellen"), &files::BackUp, cx))).on_click(move |_, window, cx| window.dispatch_action(Box::new(BackUpWorld(path.clone())), cx)));
            actions.push(back_up.into_any_element());
        }
        if let Some(r) = &row {
            let backups: Vec<Backup> = match r {
                Row::World(w) => self.backups_of(w.account, &w.folder).into_iter().cloned().collect(),
                Row::Lone(b) => self.backups_of(b.account, &b.folder).into_iter().cloned().collect(),
            };
            let label = tr("Wiederherstellen");
            let control = if backups.is_empty() || busy {
                let reason = if busy { tr("Warte, bis das fertig ist.") } else { tr("Von dieser Welt gibt es noch keine Sicherung.") };
                sheet::button_unless("worlds-restore", label.clone(), None, Look::Secondary, Some(reason.into()), false, t).into_any_element()
            } else {
                let button = Button::new("worlds-restore-menu").secondary().small().h(px(control::M)).px(px(space::M)).focus_ring(rings).dropdown_caret(true).child(div().text_size(px(font::BODY)).child(label.clone())).dropdown_menu(move |menu, _, _| restore_items(menu.scrollable(true), &backups));
                div().id("worlds-restore").mark("button", label).flex_none().child(button).into_any_element()
            };
            actions.push(control);
        }
        if let Some(r @ Row::World(w)) = &row {
            let label = tr("Kopieren zu");
            let accounts = self.copy_targets(r, cx);
            let control = if accounts.is_empty() || busy {
                let reason = if busy { tr("Warte, bis das fertig ist.") } else { tr("Es gibt keinen anderen Account.") };
                sheet::button_unless("worlds-copy", label.clone(), None, Look::Secondary, Some(reason.into()), false, t).into_any_element()
            } else {
                let path = w.dir.display().to_string();
                let button = Button::new("worlds-copy-menu").secondary().small().h(px(control::M)).px(px(space::M)).focus_ring(rings).dropdown_caret(true).child(div().text_size(px(font::BODY)).child(label.clone())).dropdown_menu(move |menu, _, _| copy_items(menu.scrollable(true), &path, &accounts));
                div().id("worlds-copy").mark("button", label).flex_none().child(button).into_any_element()
            };
            actions.push(control);
        }
        if let Some(r) = &row {
            let folder = r.key().display().to_string();
            let open = super::palette::hint(tr("Ordner öffnen"), &files::Open, cx);
            actions.push(sheet::icon_button("worlds-open", "folder-open", open, t.secondary, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(OpenWorldFolder(folder.clone())), cx)).into_any_element());
        }
        if let Some(Row::World(w)) = &row {
            let path = w.dir.display().to_string();
            let label = super::palette::hint(tr("Löschen…"), &files::Delete, cx);
            let delete = if busy {
                sheet::unavailable(div().id("worlds-delete").size(px(control::M)).flex_none().flex().items_center().justify_center().rounded(px(radius::INNER)), icon("trash", iconsize::M, t.tertiary), tr("Warte, bis das fertig ist."))
            } else {
                sheet::icon_button("worlds-delete", "trash", label, t.danger_text, t).on_click(move |_, window, cx| window.dispatch_action(Box::new(DeleteWorld(path.clone())), cx))
            };
            actions.push(delete.into_any_element());
        }
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space::S))
            .child(div().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(count))
            .child(div().flex().items_center().gap(px(space::XS)).children(actions))
    }
}

fn open_action(row: &Row) -> Option<Box<dyn Action>> {
    Some(Box::new(OpenWorldFolder(row.key().display().to_string())))
}

fn mode_label(mode: Mode) -> String {
    match mode {
        Mode::Survival => tr("Überleben"),
        Mode::Creative => tr("Kreativ"),
        Mode::Adventure => tr("Abenteuer"),
        Mode::Spectator => tr("Zuschauer"),
    }
}

fn restore_items(mut menu: PopupMenu, backups: &[Backup]) -> PopupMenu {
    for b in backups {
        menu = menu.menu(format!("{} · {}", files::stamp(b.at), files::bytes(b.size)), Box::new(RestoreBackup(b.path.display().to_string())));
    }
    menu
}

fn copy_items(mut menu: PopupMenu, path: &str, accounts: &[(Uuid, String)]) -> PopupMenu {
    for (id, name) in accounts {
        menu = menu.menu(name.clone(), Box::new(CopyWorld { world: path.to_owned(), account: id.to_string() }));
    }
    menu
}

fn row_menu(menu: PopupMenu, row: &Row, backups: &[Backup], accounts: &[(Uuid, String)], window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let path = row.key().display().to_string();
    let mut menu = menu.menu(tr("Ordner öffnen"), Box::new(OpenWorldFolder(path.clone())));
    if matches!(row, Row::World(_)) {
        menu = menu.menu(tr("Sicherung erstellen"), Box::new(BackUpWorld(path.clone())));
    }
    if !backups.is_empty() {
        let backups = backups.to_vec();
        menu = menu.submenu(tr("Wiederherstellen"), window, cx, move |m, _, _| restore_items(m.scrollable(true), &backups));
    }
    if !accounts.is_empty() {
        let (accounts, path) = (accounts.to_vec(), path.clone());
        menu = menu.submenu(tr("Kopieren zu"), window, cx, move |m, _, _| copy_items(m.scrollable(true), &path, &accounts));
    }
    if matches!(row, Row::World(_)) {
        menu = menu.separator().menu(tr("Löschen…"), Box::new(DeleteWorld(path)));
    }
    menu
}

impl Render for Worlds {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        if std::mem::take(&mut self.stale) {
            self.rescan(cx);
        }
        let root = div()
            .id("worlds")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .on_action(cx.listener(|s, _: &files::Up, _, cx| s.step(-1, cx)))
            .on_action(cx.listener(|s, _: &files::Down, _, cx| s.step(1, cx)))
            .on_action(cx.listener(|s, _: &files::First, _, cx| s.select(0, cx)))
            .on_action(cx.listener(|s, _: &files::Last, _, cx| s.select(s.rows.len().saturating_sub(1), cx)))
            .on_action(cx.listener(|s, _: &files::PageUp, _, cx| s.step(-s.page(), cx)))
            .on_action(cx.listener(|s, _: &files::PageDown, _, cx| s.step(s.page(), cx)))
            .on_action(cx.listener(|s, _: &files::Open, window, cx| s.dispatch_selected(open_action, window, cx)))
            .on_action(cx.listener(|s, _: &files::BackUp, window, cx| s.dispatch_selected(|r| if let Row::World(w) = r { Some(Box::new(BackUpWorld(w.dir.display().to_string()))) } else { None }, window, cx)))
            .on_action(cx.listener(|s, _: &files::Delete, window, cx| s.dispatch_selected(|r| if let Row::World(w) = r { Some(Box::new(DeleteWorld(w.dir.display().to_string()))) } else { None }, window, cx)));
        if !self.loaded {
            return root.child(div().size_full().flex().items_center().justify_center().child(sheet::waiting(tr("Welten werden gelesen …"), &t))).into_any_element();
        }
        let outcome = match &self.outcome {
            Some(Outcome::Done(text)) => Some(sheet::status("circle-check", t.success_text, text.clone()).flex_none()),
            Some(Outcome::Failed(problem)) => Some(sheet::problem("worlds-problem", "circle-alert", t.danger_text, problem, self.details, cx.listener(|s, _, _, cx| {
                s.details = !s.details;
                cx.notify();
            }), &t).flex_none()),
            None => None,
        };
        let saved = self.snapshot_section(cx, &t);
        if self.rows.is_empty() {
            return root.children(outcome).child(files::empty("worlds-empty", "earth", tr("Noch keine Welten"), tr("Sobald ein Account in dieser Instanz eine Welt anlegt, erscheint sie hier."), None, &t)).children(saved).into_any_element();
        }
        let count = self.rows.len();
        let list = uniform_list(
            "worlds-list",
            count,
            cx.processor(move |s: &mut Self, range: Range<usize>, _, cx| {
                let t = Theme::get(cx);
                let rows: Vec<(usize, Row)> = range.filter_map(|at| s.rows.get(at).cloned().map(|r| (at, r))).collect();
                rows.into_iter().map(|(at, row)| s.row(at, &row, cx, &t)).collect()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .min_h_0();
        root.child(self.toolbar(cx, &t)).children(outcome).child(list).children(saved).into_any_element()
    }
}
