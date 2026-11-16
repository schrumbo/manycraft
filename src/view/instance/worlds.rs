use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;
use uuid::Uuid;

use super::{Facts, actions, use_extras};
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Final, Instance, games_dir};
use crate::core::snapshot::{self, Snapshot};
use crate::core::worlds::{self, Backup, Mode, Progress, World};
use crate::ui::forms::{self, Problem};
use crate::ui::instance_files::{FileUndo, bytes, stamp, when};
use crate::ui::theme::files;
use crate::view::commands::{Extra, Shortcut};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::row::Row;
use crate::view::kit::sheet;
use crate::view::kit::text::{figure, hairline, heading, line, mono, para, secondary, strong};
use crate::view::kit::{Bounds, after, off_ui, toast, use_theme};
use crate::view::native;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};

const TICK: Duration = Duration::from_millis(120);
const DOUBLE: Duration = Duration::from_millis(400);
const SNAPSHOTS_SHOWN: usize = 4;

thread_local! {
    static BACK: RefCell<Option<(String, PathBuf)>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Doing {
    BackUp,
    Restore,
    Copy,
    Delete,
}

#[derive(Clone)]
struct Work {
    doing: Doing,
    progress: Arc<Progress>,
}

#[derive(Clone)]
enum Outcome {
    Done(String),
    Failed(Problem),
}

#[derive(Clone)]
enum Slot {
    World(World),
    Lone(Backup),
}

impl Slot {
    fn key(&self) -> &Path {
        match self {
            Self::World(w) => &w.dir,
            Self::Lone(b) => &b.path,
        }
    }
}

type Pixels = (PathBuf, u32, u32, Vec<u8>);

struct Read {
    worlds: Vec<World>,
    backups: Vec<Backup>,
    snaps: Vec<Snapshot>,
    icons: Vec<Pixels>,
}

fn read(dir: &Path) -> Read {
    let worlds = worlds::list(dir);
    let mut backups = worlds::backups(dir);
    backups.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.path.cmp(&a.path)));
    let icons = worlds
        .iter()
        .filter_map(|w| {
            let path = w.icon.as_ref()?;
            let picture = crate::core::shots::thumbnail(path, files::WORLD_ICON_PX, files::WORLD_ICON_PX).ok()?;
            Some((w.dir.clone(), picture.width(), picture.height(), picture.into_raw()))
        })
        .collect();
    let snaps = snapshot::list(dir);
    Read { worlds, backups, snaps, icons }
}

#[derive(Clone)]
struct Scan {
    id: String,
    worlds: Vec<World>,
    backups: Vec<Backup>,
    snaps: Vec<Snapshot>,
    icons: HashMap<PathBuf, ImageHandle>,
    sizes: HashMap<PathBuf, u64>,
}

impl Scan {
    fn slots(&self) -> Vec<Slot> {
        let mut slots: Vec<Slot> = self.worlds.iter().cloned().map(Slot::World).collect();
        let mut seen: Vec<(Uuid, &str)> = self.worlds.iter().map(|w| (w.account, w.folder.as_str())).collect();
        for b in &self.backups {
            if !seen.contains(&(b.account, b.folder.as_str())) {
                seen.push((b.account, b.folder.as_str()));
                slots.push(Slot::Lone(b.clone()));
            }
        }
        slots
    }

    fn backups_of(&self, account: Uuid, folder: &str) -> Vec<Backup> {
        self.backups.iter().filter(|b| b.account == account && b.folder == folder).cloned().collect()
    }

    fn backups_for(&self, slot: &Slot) -> Vec<Backup> {
        match slot {
            Slot::World(w) => self.backups_of(w.account, &w.folder),
            Slot::Lone(b) => self.backups_of(b.account, &b.folder),
        }
    }
}

fn index_of(selected: &Option<PathBuf>, slots: &[Slot]) -> Option<usize> {
    let found = selected.as_ref().and_then(|k| slots.iter().position(|s| s.key() == k.as_path()));
    found.or(if slots.is_empty() { None } else { Some(0) })
}

fn put<T: 'static>(mut state: State<T>, change: impl FnOnce(&mut T)) {
    if let Some(mut w) = state.try_write() {
        change(&mut w);
    }
}

fn mode_label(mode: Mode) -> String {
    match mode {
        Mode::Survival => tr("Überleben"),
        Mode::Creative => tr("Kreativ"),
        Mode::Adventure => tr("Abenteuer"),
        Mode::Spectator => tr("Zuschauer"),
    }
}

fn button_focused() -> bool {
    Platform::get().current_window().focused_accessibility_node.peek().role() == AccessibilityRole::Button
}

fn reveal(mut scroll: ScrollController, at: usize, view: f32) {
    let top = at as f32 * files::WORLD_ROW;
    let now = -scroll.position().y;
    if top < now {
        scroll.scroll_to_y(-(top as i32));
    } else if top + files::WORLD_ROW > now + view {
        scroll.scroll_to_y(-((top + files::WORLD_ROW - view).ceil() as i32));
    }
}

#[derive(Clone)]
struct Page {
    store: Store,
    id: String,
    dir: PathBuf,
    accounts: Vec<(Uuid, String)>,
    scan: State<Option<Rc<Scan>>>,
    view: State<f32>,
    scroll: ScrollController,
    selected: State<Option<PathBuf>>,
    work: State<HashMap<PathBuf, Work>>,
    outcome: State<Option<Outcome>>,
    details: State<bool>,
    tick: State<u64>,
    last: State<Option<(PathBuf, Instant)>>,
}

impl Page {
    fn with_scan<R>(&self, f: impl FnOnce(&Scan) -> R) -> Option<R> {
        let guard = self.scan.try_peek()?;
        let scan: &Option<Rc<Scan>> = &guard;
        scan.as_ref().map(|s| f(s))
    }

    fn slots(&self) -> Vec<Slot> {
        self.with_scan(Scan::slots).unwrap_or_default()
    }

    fn index(&self, slots: &[Slot]) -> Option<usize> {
        let selected = self.selected.try_peek().and_then(|s| s.clone());
        index_of(&selected, slots)
    }

    fn slot(&self, key: &Path) -> Option<Slot> {
        self.slots().into_iter().find(|s| s.key() == key)
    }

    fn world(&self, dir: &Path) -> Option<World> {
        match self.slot(dir)? {
            Slot::World(w) => Some(w),
            Slot::Lone(_) => None,
        }
    }

    fn backup(&self, path: &Path) -> Option<Backup> {
        self.with_scan(|s| s.backups.iter().find(|b| b.path == path).cloned()).flatten()
    }

    fn backups_for(&self, slot: &Slot) -> Vec<Backup> {
        self.with_scan(|s| s.backups_for(slot)).unwrap_or_default()
    }

    fn target(&self, backup: &Backup) -> (PathBuf, PathBuf) {
        let game = games_dir(&self.dir).join(backup.account.simple().to_string());
        let world = game.join("saves").join(&backup.folder);
        (game, world)
    }

    fn account_name(&self, id: Uuid) -> String {
        self.accounts.iter().find(|(a, _)| *a == id).map_or_else(|| tr("Entfernter Account"), |(_, n)| n.clone())
    }

    fn playing(&self, account: Uuid) -> bool {
        self.store.model.peek().games.values().any(|g| g.instance_id == self.id && g.account_id == account && g.phase.is_active())
    }

    fn copy_targets(&self, account: Uuid) -> Vec<(Uuid, String)> {
        let mut targets: Vec<(Uuid, String)> = self.accounts.iter().filter(|(a, _)| *a != account).cloned().collect();
        targets.sort_by_key(|(_, n)| n.to_lowercase());
        targets
    }

    fn busy(&self, slot: &Slot) -> Option<Work> {
        let work = self.work.try_peek()?;
        match slot {
            Slot::World(w) => work.get(&w.dir).cloned(),
            Slot::Lone(b) => work.get(&self.target(b).1).cloned(),
        }
    }

    fn select(&self, key: PathBuf) {
        let moved = self.selected.try_peek().is_some_and(|s| s.as_deref() != Some(key.as_path()));
        if moved && self.outcome.try_peek().is_some_and(|o| matches!(*o, Some(Outcome::Done(_)))) {
            put(self.outcome, |o| *o = None);
        }
        put(self.selected, |s| *s = Some(key));
    }

    fn pick(&self, slots: &[Slot], at: usize, scroll: ScrollController, view: f32) {
        let Some(slot) = slots.get(at) else { return };
        self.select(slot.key().to_path_buf());
        reveal(scroll, at, view);
    }

    fn step(&self, by: isize, scroll: ScrollController, view: f32) -> bool {
        let slots = self.slots();
        if slots.is_empty() {
            return false;
        }
        let at = self.index(&slots).map_or(0, |at| at.saturating_add_signed(by).min(slots.len() - 1));
        self.pick(&slots, at, scroll, view);
        true
    }

    fn key(&self, e: &KeyboardEventData, scroll: ScrollController, view: f32) -> bool {
        if self.store.ui.layered() || field::typing() {
            return false;
        }
        if Shortcut::primary(Code::Backspace).matches(e) {
            return self.act_on_selected(|page, slot| {
                if let Slot::World(w) = slot {
                    page.ask_delete(&w.dir);
                }
            });
        }
        if !e.modifiers.is_empty() {
            return false;
        }
        let rows = ((view / files::WORLD_ROW).floor() as isize).max(1);
        match &e.key {
            Key::Named(NamedKey::ArrowUp) => self.step(-1, scroll, view),
            Key::Named(NamedKey::ArrowDown) => self.step(1, scroll, view),
            Key::Named(NamedKey::PageUp) => self.step(-rows, scroll, view),
            Key::Named(NamedKey::PageDown) => self.step(rows, scroll, view),
            Key::Named(NamedKey::Home) => self.step(isize::MIN / 2, scroll, view),
            Key::Named(NamedKey::End) => self.step(isize::MAX / 2, scroll, view),
            Key::Named(NamedKey::Enter) if !button_focused() => self.act_on_selected(|page, slot| page.open(slot.key())),
            _ => false,
        }
    }

    fn act_on_selected(&self, act: impl FnOnce(&Page, &Slot)) -> bool {
        let slots = self.slots();
        let Some(slot) = self.index(&slots).and_then(|at| slots.get(at)) else { return false };
        act(self, slot);
        true
    }

    fn press(&self, key: PathBuf) {
        self.select(key.clone());
        let now = Instant::now();
        let last: Option<(PathBuf, Instant)> = self.last.try_peek().and_then(|l| l.clone());
        let double = last.is_some_and(|(k, at)| k == key && now.duration_since(at) < DOUBLE);
        put(self.last, |l| *l = if double { None } else { Some((key.clone(), now)) });
        if double {
            self.open(&key);
        }
    }

    fn open(&self, key: &Path) {
        if key.extension().is_some_and(|x| x == "zip") {
            native::reveal(key);
        } else {
            native::open(key);
        }
    }

    fn open_backups(&self) {
        let dir = worlds::backups_dir(&self.dir);
        spawn_in_window(async move {
            let made = dir.clone();
            if let Some(Ok(())) = off_ui(move || std::fs::create_dir_all(&made)).await {
                native::open(&dir);
            }
        });
    }

    fn begin(&self, key: PathBuf, doing: Doing) -> Arc<Progress> {
        let progress = Arc::new(Progress::default());
        let first = self.work.try_peek().is_some_and(|w| w.is_empty());
        put(self.work, |w| {
            w.insert(key, Work { doing, progress: progress.clone() });
        });
        put(self.outcome, |o| *o = None);
        put(self.details, |d| *d = false);
        if first {
            let (work, tick) = (self.work, self.tick);
            spawn_in_window(async move {
                loop {
                    after(TICK).await;
                    if work.try_peek().is_none_or(|w| w.is_empty()) {
                        break;
                    }
                    put(tick, |t| *t += 1);
                }
            });
        }
        progress
    }

    fn end(&self, key: &Path, outcome: Option<Outcome>) {
        put(self.work, |w| {
            w.remove(key);
        });
        if let Some(Outcome::Failed(problem)) = &outcome
            && self.outcome.try_peek().is_none()
        {
            toast::show(self.store.ui, problem.text.clone(), None);
        }
        put(self.outcome, |o| *o = outcome);
        put(self.details, |d| *d = false);
        actions::changed(self.store);
    }

    fn refuse(&self, text: String) {
        put(self.outcome, |o| *o = Some(Outcome::Failed(Problem::plain(text))));
        put(self.details, |d| *d = false);
    }

    fn working(&self, key: &Path) -> bool {
        self.work.try_peek().is_none_or(|w| w.contains_key(key))
    }

    fn ask_back_up(&self, dir: &Path) {
        let Some(world) = self.world(dir) else { return };
        if self.working(&world.dir) {
            return;
        }
        if !self.playing(world.account) {
            self.back_up(world);
            return;
        }
        let page = self.clone();
        spawn_in_window(async move {
            let probe = world.dir.clone();
            let open = off_ui(move || worlds::is_open(&probe)).await.unwrap_or(true);
            if page.working(&world.dir) {
                return;
            }
            if open {
                page.refuse(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst."));
                return;
            }
            let title = trf("„%@“ trotzdem sichern?", &[&world.name()]);
            let message = tr("Das Spiel dieses Accounts läuft. Öffnet es die Welt während der Sicherung, fehlt in der Sicherung vielleicht etwas.");
            let again = page.clone();
            sheet::confirm(page.store.ui, "instance.worlds.backup.confirm", title, message, tr("Trotzdem sichern"), false, move |_| again.back_up(world.clone()));
        });
    }

    fn back_up(&self, world: World) {
        let key = world.dir.clone();
        let name = world.name().to_owned();
        let dir = self.dir.clone();
        let progress = self.begin(key.clone(), Doing::BackUp);
        let page = self.clone();
        spawn_in_window(async move {
            let result = off_ui(move || worlds::back_up(&dir, &world, &progress)).await;
            let what = trf("„%@“ wurde nicht gesichert.", &[&name]);
            let outcome = match result {
                Some(Ok(_)) => Outcome::Done(trf("„%@“ gesichert.", &[&name])),
                Some(Err(e)) => Outcome::Failed(forms::explain_as(what, &e)),
                None => Outcome::Failed(Problem::plain(what)),
            };
            page.end(&key, Some(outcome));
        });
    }

    fn ask_restore(&self, path: &Path) {
        let Some(backup) = self.backup(path) else { return };
        let (game, world) = self.target(&backup);
        if self.working(&world) {
            return;
        }
        let page = self.clone();
        spawn_in_window(async move {
            let probe = world.clone();
            let open = off_ui(move || worlds::is_open(&probe)).await.unwrap_or(true);
            if page.working(&world) {
                return;
            }
            if open {
                page.refuse(tr("Die Welt ist gerade im Spiel geöffnet. Verlasse sie zuerst."));
            } else {
                page.confirm_restore(backup, game, world);
            }
        });
    }

    fn confirm_restore(&self, backup: Backup, game: PathBuf, world: PathBuf) {
        let at = stamp(backup.at);
        let message = if self.world(&world).is_some() {
            trf("Die Welt kommt auf den Stand vom %@. Die jetzige kommt in den Papierkorb, das lässt sich gleich danach rückgängig machen.", &[&at])
        } else {
            trf("Die Welt kommt mit dem Stand vom %@ zurück.", &[&at])
        };
        let title = trf("„%@“ wiederherstellen?", &[&backup.name]);
        let page = self.clone();
        sheet::confirm(self.store.ui, "instance.worlds.restore.confirm", title, message, tr("Wiederherstellen"), false, move |_| page.restore(backup.clone(), game.clone(), world.clone()));
    }

    fn restore(&self, backup: Backup, game: PathBuf, world: PathBuf) {
        let dir = self.dir.clone();
        let (id, name) = (self.id.clone(), backup.name.clone());
        let progress = self.begin(world.clone(), Doing::Restore);
        let page = self.clone();
        spawn_in_window(async move {
            let result = off_ui(move || worlds::restore(&dir, &backup, &game, &progress)).await;
            let what = trf("„%@“ wurde nicht wiederhergestellt.", &[&name]);
            match result {
                Some(Ok(restored)) => {
                    let at = restored.world.clone();
                    put(page.selected, |s| *s = Some(at));
                    offer(page.store, name, FileUndo::Restored { instance: id, world: restored.world, replaced: restored.replaced });
                    page.end(&world, None);
                }
                Some(Err(e)) => page.end(&world, Some(Outcome::Failed(forms::explain_as(what, &e)))),
                None => page.end(&world, Some(Outcome::Failed(Problem::plain(what)))),
            }
        });
    }

    fn copy(&self, dir: &Path, account: Uuid) {
        let Some(world) = self.world(dir) else { return };
        if self.working(&world.dir) || world.account == account {
            return;
        }
        let game = games_dir(&self.dir).join(account.simple().to_string());
        let (key, name, id) = (world.dir.clone(), world.name().to_owned(), self.id.clone());
        let progress = self.begin(key.clone(), Doing::Copy);
        let page = self.clone();
        spawn_in_window(async move {
            let result = off_ui(move || worlds::copy_to(&world, &game, &progress)).await;
            let what = trf("„%@“ wurde nicht kopiert.", &[&name]);
            match result {
                Some(Ok(copy)) => {
                    offer(page.store, name, FileUndo::Copied { instance: id, world: copy });
                    page.end(&key, None);
                }
                Some(Err(e)) => page.end(&key, Some(Outcome::Failed(forms::explain_as(what, &e)))),
                None => page.end(&key, Some(Outcome::Failed(Problem::plain(what)))),
            }
        });
    }

    fn ask_delete(&self, dir: &Path) {
        let Some(world) = self.world(dir) else { return };
        if self.working(&world.dir) {
            return;
        }
        let title = trf("„%@“ löschen?", &[&world.name()]);
        let page = self.clone();
        sheet::confirm(self.store.ui, "instance.worlds.delete.confirm", title, tr("Die Welt kommt in den Papierkorb und bleibt dort 7 Tage."), tr("Löschen"), true, move |_| page.delete(world.clone()));
    }

    fn delete(&self, world: World) {
        let dir = self.dir.clone();
        let (id, key, name) = (self.id.clone(), world.dir.clone(), world.name().to_owned());
        self.begin(key.clone(), Doing::Delete);
        let page = self.clone();
        let path = key.clone();
        spawn_in_window(async move {
            let result = off_ui(move || worlds::delete(&dir, &path)).await;
            let what = trf("„%@“ wurde nicht gelöscht.", &[&name]);
            match result {
                Some(Ok(entry)) => {
                    let slots = page.slots();
                    if let Some(at) = slots.iter().position(|s| s.key() == key.as_path())
                        && page.index(&slots) == Some(at)
                    {
                        let next = slots.get(at + 1).or_else(|| at.checked_sub(1).and_then(|p| slots.get(p)));
                        let next = next.map(|s| s.key().to_path_buf());
                        put(page.selected, |s| *s = next);
                    }
                    offer(page.store, name, FileUndo::Deleted { instance: id, entry });
                    page.end(&key, None);
                }
                Some(Err(e)) => page.end(&key, Some(Outcome::Failed(forms::explain_as(what, &e)))),
                None => page.end(&key, Some(Outcome::Failed(Problem::plain(what)))),
            }
        });
    }

    fn restore_snapshot(&self, snap: Snapshot) {
        super::snapshots::ask_restore(self.store, &self.id, snap);
    }

    fn line(&self, slot: &Slot, scan: &Scan) -> String {
        match slot {
            Slot::World(w) => {
                let mut parts = vec![self.account_name(w.account)];
                if w.level.hardcore {
                    parts.push(tr("Hardcore"));
                } else if let Some(mode) = w.level.mode {
                    parts.push(mode_label(mode));
                }
                if let Some(at) = w.level.last_played {
                    parts.push(when(at.div_euclid(1000)));
                }
                if let Some(size) = scan.sizes.get(&w.dir) {
                    parts.push(bytes(*size));
                }
                parts.join(" · ")
            }
            Slot::Lone(b) => [tr("Nur als Sicherung"), self.account_name(b.account), when(b.at)].join(" · "),
        }
    }

    fn item(&self, id: String, label: String, run: impl Fn(&Page) + 'static) -> Item {
        let page = self.clone();
        Item::new(id, label, move |_| run(&page))
    }

    fn restore_items(&self, backups: &[Backup], flat: bool) -> Vec<Item> {
        backups
            .iter()
            .enumerate()
            .map(|(n, b)| {
                let what = format!("{} · {}", stamp(b.at), bytes(b.size));
                let label = if flat { trf("Wiederherstellen: %@", &[&what]) } else { what };
                let path = b.path.clone();
                self.item(format!("instance.worlds.restore.{n}"), label, move |page| page.ask_restore(&path))
            })
            .collect()
    }

    fn copy_items(&self, world: &World, flat: bool) -> Vec<Item> {
        self.copy_targets(world.account)
            .into_iter()
            .map(|(account, name)| {
                let label = if flat { trf("Kopieren zu: %@", &[&name]) } else { name };
                let path = world.dir.clone();
                self.item(format!("instance.worlds.copy.{}", account.simple()), label, move |page| page.copy(&path, account))
            })
            .collect()
    }

    fn menu_items(&self, slot: &Slot) -> Vec<Item> {
        let key = slot.key().to_path_buf();
        let mut items = vec![self.item("instance.worlds.menu.open".into(), tr("Ordner öffnen"), move |page| page.open(&key))];
        if let Slot::World(w) = slot {
            let path = w.dir.clone();
            items.push(self.item("instance.worlds.menu.backup".into(), tr("Sicherung erstellen"), move |page| page.ask_back_up(&path)).shortcut(Shortcut::primary(Code::KeyS).label()));
        }
        let backups = self.backups_for(slot);
        if !backups.is_empty() {
            items.push(Item::Separator);
            items.extend(self.restore_items(&backups, true));
        }
        if let Slot::World(w) = slot {
            let copies = self.copy_items(w, true);
            if !copies.is_empty() {
                items.push(Item::Separator);
                items.extend(copies);
            }
            let path = w.dir.clone();
            items.push(Item::Separator);
            items.push(self.item("instance.worlds.menu.delete".into(), tr("Löschen…"), move |page| page.ask_delete(&path)).shortcut(Shortcut::bare(Code::Delete).label()).danger());
        }
        items
    }

    fn open_menu(&self, slot: &Slot, at: (f32, f32)) {
        self.select(slot.key().to_path_buf());
        let items = self.menu_items(slot);
        let long = items.len() > frame::MENU_ROWS;
        let menu = Menu::at(items, at);
        menu::open(self.store.ui, if long { menu.searchable(tr("Suchen")) } else { menu });
    }

    fn extras(&self, slots: &[Slot], at: Option<usize>, snaps: &[Snapshot]) -> Vec<Extra> {
        let mut list = Vec::new();
        let mut push = |key: String, label: String, glyph: Icon, shortcut: Option<Shortcut>, run: EventHandler<()>| {
            list.push(Extra { id: format!("instance.worlds.{key}"), label, glyph, shortcut, run });
        };
        let page = self.clone();
        match at.and_then(|at| slots.get(at)) {
            Some(Slot::World(w)) => {
                let name = w.name().to_owned();
                let path = w.dir.clone();
                let backups = self.backups_for(&Slot::World(w.clone()));
                let (a, b, c) = (page.clone(), page.clone(), page.clone());
                let (pa, pb, pc) = (path.clone(), path.clone(), path.clone());
                push("backup".into(), trf("Sicherung von „%@“ erstellen", &[&name]), Icon::Download, Some(Shortcut::primary(Code::KeyS)), EventHandler::new(move |_| a.ask_back_up(&pa)));
                push("open".into(), trf("Ordner von „%@“ öffnen", &[&name]), Icon::Folder, None, EventHandler::new(move |_| b.open(&pb)));
                for (n, backup) in backups.iter().enumerate() {
                    let page = page.clone();
                    let from = backup.path.clone();
                    let label = format!("{} · {}", trf("„%@“ wiederherstellen", &[&name]), stamp(backup.at));
                    push(format!("restore.{n}"), label, Icon::Undo, None, EventHandler::new(move |_| page.ask_restore(&from)));
                }
                for (account, to) in self.copy_targets(w.account) {
                    let page = page.clone();
                    let from = path.clone();
                    push(format!("copy.{}", account.simple()), trf("„%1$@“ zu %2$@ kopieren", &[&name, &to]), Icon::Copy, None, EventHandler::new(move |_| page.copy(&from, account)));
                }
                push("delete".into(), trf("„%@“ löschen…", &[&name]), Icon::Close, Some(Shortcut::bare(Code::Delete)), EventHandler::new(move |_| c.ask_delete(&pc)));
            }
            Some(Slot::Lone(b)) => {
                let page = page.clone();
                let from = b.path.clone();
                let label = format!("{} · {}", trf("„%@“ wiederherstellen", &[&b.name]), stamp(b.at));
                push("restore.0".into(), label, Icon::Undo, None, EventHandler::new(move |_| page.ask_restore(&from)));
            }
            None => {}
        }
        for snap in snaps {
            let page = page.clone();
            let chosen = snap.clone();
            let label = format!("{} · {}", trf("Stand „%@“ wiederherstellen", &[&snap.reason.label()]), stamp(snap.at));
            push(format!("snapshot.{}", snap.id), label, Icon::Undo, None, EventHandler::new(move |_| page.restore_snapshot(chosen.clone())));
        }
        let page = page.clone();
        push("backups".into(), tr("Ordner mit den Sicherungen öffnen"), Icon::Folder, None, EventHandler::new(move |_| page.open_backups()));
        list
    }
}

fn copy_of(what: &FileUndo) -> FileUndo {
    match what {
        FileUndo::Deleted { instance, entry } => FileUndo::Deleted { instance: instance.clone(), entry: entry.clone() },
        FileUndo::Restored { instance, world, replaced } => FileUndo::Restored { instance: instance.clone(), world: world.clone(), replaced: replaced.clone() },
        FileUndo::Copied { instance, world } => FileUndo::Copied { instance: instance.clone(), world: world.clone() },
    }
}

fn offer(store: Store, name: String, what: FileUndo) {
    let text = match &what {
        FileUndo::Deleted { .. } => trf("„%@“ gelöscht.", &[&name]),
        FileUndo::Restored { .. } => trf("„%@“ wiederhergestellt.", &[&name]),
        FileUndo::Copied { .. } => trf("„%@“ kopiert.", &[&name]),
    };
    let kept = !matches!(what, FileUndo::Copied { .. });
    let back = EventHandler::new(move |_| undo(store, name.clone(), copy_of(&what)));
    if kept {
        toast::show_kept(store.ui, text, back);
    } else {
        toast::show(store.ui, text, Some(back));
    }
}

fn undo(store: Store, name: String, what: FileUndo) {
    let (instance, world, entry) = match &what {
        FileUndo::Deleted { instance, entry } => (instance.clone(), None, Some(entry.clone())),
        FileUndo::Restored { instance, world, replaced } => (instance.clone(), Some(world.clone()), replaced.clone()),
        FileUndo::Copied { instance, world } => (instance.clone(), Some(world.clone()), None),
    };
    let Some(dir) = store.model.peek().instance(&instance).map(Instance::dir) else {
        toast::show(store.ui, tr("Diese Instanz gibt es nicht mehr."), None);
        return;
    };
    if matches!(what, FileUndo::Restored { .. })
        && let Some(why) = super::blocked(store, &instance)
    {
        toast::show(store.ui, why, None);
        offer(store, name, what);
        return;
    }
    spawn_in_window(async move {
        let result = off_ui(move || -> anyhow::Result<Option<PathBuf>> {
            if let (Some(_), Some(entry)) = (&world, &entry)
                && !crate::core::trash::holds(&dir, entry)
            {
                anyhow::bail!(Final(tr("Das liegt nicht mehr im Papierkorb.")));
            }
            let mut parked = None;
            if let Some(world) = &world {
                if worlds::is_open(world) {
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
        })
        .await;
        match result {
            Some(Ok(back)) => {
                if let Some(back) = back.filter(|b| b.parent().and_then(Path::file_name).is_some_and(|n| n == "saves")) {
                    BACK.with(|b| *b.borrow_mut() = Some((instance, back)));
                }
                actions::changed(store);
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Nicht zurückgeholt."), &e).text, None);
                if !actions::permanent(&e) {
                    offer(store, name, what);
                }
            }
            None => offer(store, name, what),
        }
    });
}

fn kept_sizes(scan: State<Option<Rc<Scan>>>, id: &str) -> HashMap<PathBuf, u64> {
    let Some(guard) = scan.try_peek() else { return HashMap::new() };
    let old: &Option<Rc<Scan>> = &guard;
    old.as_ref().filter(|s| s.id == id).map(|s| s.sizes.clone()).unwrap_or_default()
}

fn use_page(store: Store, f: &Facts) -> Page {
    let id = f.made.id.clone();
    let dir = f.made.dir();
    let scan = use_state(|| None::<Rc<Scan>>);
    let selected = use_state(|| None::<PathBuf>);
    let work = use_state(HashMap::<PathBuf, Work>::new);
    let outcome = use_state(|| None::<Outcome>);
    let details = use_state(|| false);
    let tick = use_state(|| 0u64);
    let last = use_state(|| None::<(PathBuf, Instant)>);
    let mut again = use_state(|| 0u64);
    let latest = use_hook(|| Rc::new(Cell::new(0u64)));
    let view = use_state(|| 0.);
    let scroll = use_hook(|| ScrollController::new(0, 0));
    let playing: Vec<Uuid> = {
        let m = store.model.read();
        let mut now: Vec<Uuid> = m.games.values().filter(|g| g.instance_id == id && g.phase.is_active()).map(|g| g.account_id).collect();
        now.sort_unstable();
        now
    };
    let mut seen = use_state(|| playing.clone());
    use_side_effect_with_deps(&playing, move |now| {
        let left = seen.peek().iter().any(|a| !now.contains(a));
        seen.set(now.clone());
        if left {
            let next = *again.peek() + 1;
            again.set(next);
        }
    });
    let deps = (id.clone(), *store.ui.instance.changed.read(), *again.read());
    let scan_dir = dir.clone();
    use_side_effect_with_deps(&deps, move |(id, _, _)| {
        let (id, dir, latest) = (id.clone(), scan_dir.clone(), latest.clone());
        let stamp = latest.get() + 1;
        latest.set(stamp);
        let mut scan = scan;
        spawn(async move {
            let found = off_ui(move || read(&dir)).await;
            if latest.get() != stamp {
                return;
            }
            let found = found.unwrap_or(Read { worlds: Vec::new(), backups: Vec::new(), snaps: Vec::new(), icons: Vec::new() });
            let icons = found.icons.into_iter().filter_map(|(d, w, h, rgba)| Some((d, ImageHandle::from_rgba(w, h, Bytes::from(rgba), AlphaType::Unpremul)?))).collect();
            let dirs: Vec<PathBuf> = found.worlds.iter().map(|w| w.dir.clone()).collect();
            let sizes = kept_sizes(scan, &id);
            let next = Scan { id: id.clone(), worlds: found.worlds, backups: found.backups, snaps: found.snaps, icons, sizes };
            let slots = next.slots();
            scan.set(Some(Rc::new(next)));
            let back = BACK.with(|b| {
                let mut b = b.borrow_mut();
                if b.as_ref().is_some_and(|(i, _)| *i == id) { b.take().map(|(_, p)| p) } else { None }
            });
            if let Some(back) = back {
                let mut selected = selected;
                selected.set(Some(back));
            }
            let chosen = selected.peek().clone();
            if let Some(at) = index_of(&chosen, &slots) {
                reveal(scroll, at, *view.peek());
            }
            let measured = off_ui(move || dirs.into_iter().map(|d| (worlds::size(&d), d)).map(|(n, d)| (d, n)).collect::<HashMap<_, _>>()).await;
            if latest.get() == stamp
                && let Some(measured) = measured
            {
                put(scan, |s| {
                    if let Some(old) = s.as_ref().filter(|o| o.id == id) {
                        *s = Some(Rc::new(Scan { sizes: measured, ..(**old).clone() }));
                    }
                });
            }
        });
    });
    Page { store, id, dir, accounts: f.accounts.clone(), scan, view, scroll, selected, work, outcome, details, tick, last }
}

struct Listing {
    lines: Vec<Entry>,
}

impl PartialEq for Listing {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

struct Entry {
    key: PathBuf,
    title: String,
    detail: String,
    selected: bool,
    lone: bool,
    picture: Option<ImageHandle>,
    working: Option<String>,
    playing: bool,
}

fn thumb(t: &Theme, entry: &Entry) -> Element {
    let side = files::WORLD_ICON;
    let well = rect().width(Size::px(side)).height(Size::px(side)).corner_radius(t.shape.control).overflow(Overflow::Clip).background(t.well).center();
    let well = match &entry.picture {
        Some(handle) => well.child(image(handle.clone()).width(Size::px(side)).height(Size::px(side)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center)),
        None => well.child(icon(t, if entry.lone { Icon::Download } else { Icon::Grid }, iconsize::L, t.text3)),
    };
    if entry.lone { rect().opacity(crate::ui::theme::tile::DIMMED).child(well).into_element() } else { well.into_element() }
}

fn row(t: &Theme, page: &Page, at: usize, entry: &Entry) -> Element {
    let mut row = Row::new(format!("instance.worlds.row.{at}"), entry.title.clone()).detail(entry.detail.clone()).height(files::WORLD_ROW).leading(thumb(t, entry)).selected(entry.selected);
    if let Some(text) = &entry.working {
        row = row.trailing(figure(t, text.clone(), font::CALLOUT, if entry.selected { t.accent_text } else { t.text2 }));
    } else if entry.playing {
        row = row.trailing(line(t, tr("Spiel läuft"), font::CAPTION, if entry.selected { t.accent_text } else { t.success_text }));
    }
    let (press, menu) = (page.clone(), page.clone());
    let (press_key, menu_key) = (entry.key.clone(), entry.key.clone());
    row.on_press(move |()| press.press(press_key.clone()))
        .on_secondary(move |at| {
            if let Some(slot) = menu.slot(&menu_key) {
                menu.open_menu(&slot, at);
            }
        })
        .into_element()
}

fn work_text(work: &Work) -> String {
    let percent = (work.progress.fraction() * 100.).round() as u32;
    match work.doing {
        Doing::BackUp => trf("Wird gesichert … %lld %", &[&percent]),
        Doing::Restore => trf("Wird wiederhergestellt … %lld %", &[&percent]),
        Doing::Copy => trf("Wird kopiert … %lld %", &[&percent]),
        Doing::Delete => tr("Wird gelöscht …"),
    }
}

fn status(t: &Theme, glyph: Icon, color: Color, text: String) -> Element {
    rect().width(Size::fill()).horizontal().spacing(space::XS).child(icon(t, glyph, iconsize::S, color)).child(para(t, text, font::CALLOUT, color)).into_element()
}

fn outcome_view(t: &Theme, outcome: &Outcome, details: State<bool>) -> Element {
    let body = match outcome {
        Outcome::Done(text) => status(t, Icon::Check, t.success_text, text.clone()),
        Outcome::Failed(problem) => {
            let mut col = rect().width(Size::fill()).spacing(space::XXS).child(status(t, Icon::Alert, t.danger_text, problem.text.clone()));
            if let Some(detail) = &problem.detail {
                let open = *details.read();
                let mut toggle = details;
                let label = if open { tr("Details ausblenden") } else { tr("Details") };
                col = col.child(rect().horizontal().child(Button::new("instance.worlds.details", label).plain().glyph(if open { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| toggle.set(!open))));
                if open {
                    col = col.child(rect().width(Size::fill()).padding((space::XS, space::S)).corner_radius(t.shape.small).background(t.raised).child(mono(t, detail.clone(), font::CAPTION, t.text2).max_lines(6)));
                }
            }
            col.into_element()
        }
    };
    rect().width(Size::fill()).padding((0., space::L, space::S, space::L)).child(body).into_element()
}

fn snapshot_section(t: &Theme, page: &Page, snaps: &[Snapshot], taking: Option<f32>) -> Option<Element> {
    if snaps.is_empty() && taking.is_none() {
        return None;
    }
    let blocked = super::blocked(page.store, &page.id);
    let mut rows: Vec<Element> = Vec::new();
    if let Some(done) = taking {
        let text = trf("Wird gesichert … %lld %", &[&((done * 100.).round() as u32)]);
        rows.push(
            rect()
                .width(Size::fill())
                .height(Size::px(files::SNAPSHOT_ROW))
                .padding((0., space::S))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::M)
                .child(icon(t, Icon::Undo, iconsize::M, t.text3))
                .child(figure(t, text, font::CALLOUT, t.text2))
                .into_element(),
        );
    }
    for (at, snap) in snaps.iter().enumerate() {
        let label = snap.reason.label();
        let detail = [when(snap.at), snap.version_line.clone(), bytes(snap.size)].join(" · ");
        let (restore, chosen) = (page.clone(), snap.clone());
        let button = Button::new(format!("instance.worlds.snapshot.{at}"), tr("Wiederherstellen…")).plain().disabled(blocked.clone()).on_press(move |_| restore.restore_snapshot(chosen.clone()));
        rows.push(
            rect()
                .width(Size::fill())
                .height(Size::px(files::SNAPSHOT_ROW))
                .padding((0., space::S))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::M)
                .content(Content::flex())
                .child(icon(t, Icon::Undo, iconsize::M, t.text3))
                .child(rect().width(Size::flex(1.)).child(line(t, label, font::BODY, t.text1)).child(secondary(t, detail)))
                .child(button)
                .into_element(),
        );
    }
    let shown = rows.len().min(SNAPSHOTS_SHOWN) as f32;
    Some(
        rect()
            .width(Size::fill())
            .spacing(space::XXS)
            .child(hairline(t))
            .child(rect().width(Size::fill()).padding((space::S, space::L, 0., space::L)).child(strong(t, tr("Vor Änderungen gesichert"), font::CALLOUT, t.text2)))
            .child(ScrollView::new().width(Size::fill()).height(Size::px(files::SNAPSHOT_ROW * shown)).child(rect().width(Size::fill()).padding((0., space::S)).children(rows)))
            .into_element(),
    )
}

/// Die Welten der Instanz und ihre Sicherungen.
#[derive(PartialEq)]
pub struct Worlds {
    pub facts: Facts,
}

impl Component for Worlds {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let page = use_page(store, &self.facts);
        let (mut view, scroll) = (page.view, page.scroll);
        let restore_at = use_state(Bounds::default);
        let copy_at = use_state(Bounds::default);
        let _ = page.tick.read();
        let current = page.scan.read().clone().filter(|s| s.id == page.id);
        let selected = page.selected.read().clone();
        let slots = current.as_ref().map(|s| s.slots()).unwrap_or_default();
        let at = index_of(&selected, &slots);
        let snaps = current.as_ref().map(|s| s.snaps.clone()).unwrap_or_default();
        use_extras(page.extras(&slots, at, &snaps));
        let Some(scan) = current else {
            return rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Welten werden gelesen …"))).into_element();
        };
        let taking = store.ui.instance.busy.read().get(&page.id).and_then(|b| b.progress);
        let outcome = page.outcome.read().clone().map(|o| outcome_view(&t, &o, page.details));
        let saved = snapshot_section(&t, &page, &snaps, taking);
        let key_page = page.clone();
        let mut root = rect()
            .width(Size::fill())
            .height(Size::fill())
            .content(Content::flex())
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if key_page.key(&e, scroll, *view.peek()) {
                    e.stop_propagation();
                }
            });
        if slots.is_empty() {
            root = root.child(
                rect()
                    .width(Size::fill())
                    .height(Size::flex(1.))
                    .center()
                    .spacing(space::M)
                    .padding((0., space::XL))
                    .child(icon(&t, Icon::Grid, iconsize::HERO, t.text3))
                    .child(heading(&t, tr("Noch keine Welten")))
                    .child(rect().max_width(Size::px(control::READING)).child(para(&t, tr("Sobald ein Account in dieser Instanz eine Welt anlegt, erscheint sie hier."), font::CALLOUT, t.text2).text_align(TextAlign::Center))),
            );
            if let Some(o) = outcome {
                root = root.child(o);
            }
            if let Some(s) = saved {
                root = root.child(s);
            }
            return root.into_element();
        }
        root = root.child(toolbar(&t, &page, &scan, &slots, at, restore_at, copy_at));
        if let Some(o) = outcome {
            root = root.child(o);
        }
        let playing: Vec<Uuid> = {
            let m = store.model.read();
            m.games.values().filter(|g| g.instance_id == page.id && g.phase.is_active()).map(|g| g.account_id).collect()
        };
        let lines: Vec<Entry> = slots
            .iter()
            .enumerate()
            .map(|(n, slot)| {
                let busy = page.busy(slot);
                let (title, picture, lone, account) = match slot {
                    Slot::World(w) => (w.name().to_owned(), scan.icons.get(&w.dir).cloned(), false, Some(w.account)),
                    Slot::Lone(b) => (b.name.clone(), None, true, None),
                };
                Entry {
                    key: slot.key().to_path_buf(),
                    title,
                    detail: page.line(slot, &scan),
                    selected: at == Some(n),
                    lone,
                    picture,
                    playing: busy.is_none() && account.is_some_and(|a| playing.contains(&a)),
                    working: busy.as_ref().map(work_text),
                }
            })
            .collect();
        let count = lines.len();
        let (list_t, list_page) = (t.clone(), page.clone());
        let list = VirtualScrollView::new_with_data_controlled(
            Listing { lines },
            move |item, data: &Listing| match data.lines.get(item.index) {
                Some(entry) => rect().key(item.index).width(Size::fill()).height(Size::px(item.size)).child(row(&list_t, &list_page, item.index, entry)).into_element(),
                None => rect().into_element(),
            },
            scroll,
        )
        .length(count)
        .item_size(files::WORLD_ROW)
        .scroll_with_arrows(false)
        .on_sized(move |e: Event<SizedEventData>| view.set_if_modified(Bounds::of(&e).h));
        root = root.child(rect().width(Size::fill()).height(Size::flex(1.)).padding((0., space::S)).child(list));
        if let Some(s) = saved {
            root = root.child(s);
        }
        root.into_element()
    }
}

fn toolbar(t: &Theme, page: &Page, scan: &Scan, slots: &[Slot], at: Option<usize>, restore_at: State<Bounds>, copy_at: State<Bounds>) -> Element {
    let worlds = slots.iter().filter(|s| matches!(s, Slot::World(_))).count();
    let mut count = if worlds == 1 { tr("1 Welt") } else { trf("%lld Welten", &[&worlds]) };
    match scan.backups.len() {
        0 => {}
        1 => count = format!("{count} · {}", tr("1 Sicherung")),
        n => count = format!("{count} · {}", trf("%lld Sicherungen", &[&n])),
    }
    let mut bar = rect().width(Size::fill()).padding(space::L).horizontal().cross_align(Alignment::Center).spacing(space::XS).content(Content::flex()).child(rect().width(Size::flex(1.)).child(secondary(t, count)));
    if let Some(slot) = at.and_then(|at| slots.get(at)) {
        let busy = page.busy(slot).is_some();
        let wait = busy.then(|| tr("Warte, bis das fertig ist."));
        if let Slot::World(w) = slot {
            let (path, back_up) = (w.dir.clone(), page.clone());
            bar = bar.child(
                Button::new("instance.worlds.backup", tr("Sicherung erstellen"))
                    .glyph(Icon::Download)
                    .tip(Shortcut::primary(Code::KeyS).label())
                    .disabled(wait.clone())
                    .on_press(move |_| back_up.ask_back_up(&path)),
            );
        }
        let backups = page.backups_for(slot);
        if backups.is_empty() || busy {
            let reason = if busy { tr("Warte, bis das fertig ist.") } else { tr("Von dieser Welt gibt es noch keine Sicherung.") };
            bar = bar.child(Button::new("instance.worlds.restore", tr("Wiederherstellen")).disabled(Some(reason)));
        } else {
            let (pages, mut anchor) = (page.clone(), restore_at);
            bar = bar.child(rect().on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e))).child(Button::new("instance.worlds.restore", tr("Wiederherstellen")).glyph(Icon::ChevronDown).on_press(move |_| {
                let items = pages.restore_items(&backups, false);
                let long = items.len() > frame::MENU_ROWS;
                let menu = Menu::under(items, *restore_at.peek());
                menu::open(pages.store.ui, if long { menu.searchable(tr("Suchen")) } else { menu });
            })));
        }
        if let Slot::World(w) = slot {
            let targets = page.copy_targets(w.account);
            if targets.is_empty() || busy {
                let reason = if busy { tr("Warte, bis das fertig ist.") } else { tr("Es gibt keinen anderen Account.") };
                bar = bar.child(Button::new("instance.worlds.copy", tr("Kopieren zu")).disabled(Some(reason)));
            } else {
                let (pages, world, mut anchor) = (page.clone(), w.clone(), copy_at);
                bar = bar.child(rect().on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e))).child(Button::new("instance.worlds.copy", tr("Kopieren zu")).glyph(Icon::ChevronDown).on_press(move |_| {
                    let items = pages.copy_items(&world, false);
                    let long = items.len() > frame::MENU_ROWS;
                    let menu = Menu::under(items, *copy_at.peek());
                    menu::open(pages.store.ui, if long { menu.searchable(tr("Suchen")) } else { menu });
                })));
            }
        }
        let (key, opener) = (slot.key().to_path_buf(), page.clone());
        bar = bar.child(Button::icon("instance.worlds.open", Icon::Folder, tr("Ordner öffnen")).on_press(move |_| opener.open(&key)));
        if let Slot::World(w) = slot {
            let (path, remover) = (w.dir.clone(), page.clone());
            let label = tr("Löschen…");
            bar = bar.child(Button::icon("instance.worlds.delete", Icon::Close, label).disabled(wait).on_press(move |_| remover.ask_delete(&path)));
        }
    }
    bar.into_element()
}
