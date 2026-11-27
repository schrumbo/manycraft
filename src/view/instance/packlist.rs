use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use freya::prelude::*;

use super::cursor::{self, Cursor, use_cursor};
use super::detail::{self, Detail, Pane, Subject, use_pane};
use super::entry::{EntryRow, ROW, RowAction, bar};
use super::mods::drop_zone;
use super::shots_parts::centered;
use super::{Facts, actions};
use crate::core::batch::ModRef;
use crate::core::i18n::{tr, trf};
use crate::core::modrinth::{Filter, Kind};
use crate::core::packs;
use crate::core::trash;
use crate::ui::catalog;
use crate::ui::forms;
use crate::ui::instance_files::bytes;
use crate::view::commands::{Extra, Shortcut};
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::{caption, secondary};
use crate::view::kit::sheet;
use crate::view::kit::{Bounds, Live, after, off_ui, open_external, page, toast, use_theme};
use crate::view::native;
use crate::view::sheets::browser;
use crate::view::store::{Store, use_store};
use crate::view::theme::{control, frame, space};

const FRESH: Duration = Duration::from_secs(600);
const FLASH_FOR: Duration = Duration::from_millis(2500);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    file: String,
    folder: bool,
    size: u64,
    on: bool,
    icon: Option<PathBuf>,
}

/// What Modrinth knows about a pack of the folder.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Known {
    project: String,
    title: String,
    update: bool,
}

type Matches = Rc<HashMap<String, Known>>;

thread_local! {
    static LOOKED: RefCell<HashMap<String, (Instant, Matches)>> = RefCell::new(HashMap::new());
}

fn looked(signature: &str) -> Option<Matches> {
    LOOKED.with(|l| l.borrow().get(signature).filter(|(at, _)| at.elapsed() < FRESH).map(|(_, found)| found.clone()))
}

fn remember(signature: String, found: Matches) {
    LOOKED.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|_, (at, _)| at.elapsed() < FRESH);
        l.insert(signature, (Instant::now(), found));
    });
}

fn lookup(dir: &Path, filter: &Filter) -> Result<HashMap<String, Known>> {
    let installed = catalog::installed(dir)?;
    let current: Vec<ModRef> = installed.iter().map(|i| i.found.clone()).collect();
    let updates = if current.is_empty() { Vec::new() } else { catalog::updates(filter, &current).unwrap_or_default() };
    Ok(installed
        .into_iter()
        .map(|i| {
            let update = updates.iter().any(|u| u.current.version_id == i.found.version_id);
            (i.file, Known { project: i.found.project_id, title: i.found.title, update })
        })
        .collect())
}

#[derive(Clone, PartialEq)]
enum Listing {
    Loading,
    Ready(Vec<Entry>),
}

fn is_pack(name: &str, folder: bool) -> bool {
    !name.starts_with('.') && (folder || name.to_lowercase().ends_with(".zip"))
}

fn scan(dir: &Path, instance_dir: &Path, kind: Kind) -> Vec<Entry> {
    let on = packs::selected(instance_dir, kind);
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let file = e.file_name().to_str()?.to_owned();
            let meta = e.metadata().ok()?;
            let folder = meta.is_dir();
            is_pack(&file, folder).then(|| Entry { on: on.contains(&file), size: if folder { 0 } else { meta.len() }, icon: packs::icon_file(&e.path()), file, folder })
        })
        .collect();
    found.sort_by(|a, b| b.on.cmp(&a.on).then_with(|| a.file.to_lowercase().cmp(&b.file.to_lowercase())));
    found
}

fn spot(kind: Kind) -> &'static str {
    match kind {
        Kind::Shader => "shaders",
        _ => "packs",
    }
}

fn shown_name(file: &str) -> String {
    let stem = file.strip_suffix(".zip").or_else(|| file.strip_suffix(".ZIP")).unwrap_or(file);
    stem.to_owned()
}

fn remove(store: Store, instance_dir: PathBuf, dir: PathBuf, kind: Kind, loader: crate::core::instance::Loader, file: String, was_on: bool) {
    spawn_in_window(async move {
        let (from, folder, name, hold) = (instance_dir.clone(), dir.clone(), file.clone(), (kind, loader.clone()));
        let done = off_ui(move || {
            if was_on {
                packs::switch(&from, hold.0, &name, false, &hold.1)?;
            }
            let removed = packs::remove(&from, &folder, &name);
            if removed.is_err() && was_on {
                let _ = packs::switch(&from, hold.0, &name, true, &hold.1);
            }
            removed
        })
        .await;
        match done {
            Some(Ok(entry)) => offer_undo(store, instance_dir, kind, loader, file, was_on, entry),
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht entfernt.", &[&shown_name(&file)]), &e).text, None);
            }
            None => {
                toast::show(store.ui, trf("„%@“ wurde nicht entfernt.", &[&shown_name(&file)]), None);
            }
        }
        actions::changed(store);
    });
}

fn offer_undo(store: Store, instance_dir: PathBuf, kind: Kind, loader: crate::core::instance::Loader, file: String, was_on: bool, entry: String) {
    let text = trf("„%@“ entfernt.", &[&shown_name(&file)]);
    toast::show_kept(store.ui, text, EventHandler::new(move |_| undo(store, instance_dir.clone(), kind, loader.clone(), file.clone(), was_on, entry.clone())));
}

fn undo(store: Store, instance_dir: PathBuf, kind: Kind, loader: crate::core::instance::Loader, file: String, was_on: bool, entry: String) {
    spawn_in_window(async move {
        let (from, back, name, hold) = (instance_dir.clone(), entry.clone(), file.clone(), (kind, loader.clone()));
        let done = off_ui(move || {
            packs::put_back(&from, &back)?;
            if was_on {
                packs::switch(&from, hold.0, &name, true, &hold.1)?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;
        match done {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht zurückgeholt.", &[&shown_name(&file)]), &e).text, None);
                if !actions::permanent(&e) && trash::holds(&instance_dir, &entry) {
                    offer_undo(store, instance_dir, kind, loader, file, was_on, entry);
                }
            }
            None => offer_undo(store, instance_dir, kind, loader, file, was_on, entry),
        }
        actions::changed(store);
    });
}

fn switch(store: Store, instance_dir: PathBuf, kind: Kind, loader: crate::core::instance::Loader, file: String, on: bool) {
    spawn_in_window(async move {
        let (from, name) = (instance_dir, file.clone());
        match off_ui(move || packs::switch(&from, kind, &name, on, &loader)).await {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht umgeschaltet.", &[&shown_name(&file)]), &e).text, None);
            }
            None => {
                toast::show(store.ui, trf("„%@“ wurde nicht umgeschaltet.", &[&shown_name(&file)]), None);
            }
        }
        actions::changed(store);
    });
}

/// What a pack row does, from its icons, its menu and the keys of the list.
#[derive(Clone)]
struct Ops {
    store: Store,
    pane: Pane,
    settled: bool,
    id: String,
    instance_dir: PathBuf,
    dir: PathBuf,
    kind: Kind,
    loader: crate::core::instance::Loader,
    matches: Matches,
    flash: State<Rc<HashSet<String>>>,
}

impl Ops {
    /// What the tab does with files dropped on it.
    fn dropper(&self) -> impl Fn(Vec<PathBuf>) + 'static {
        let ops = self.clone();
        move |paths| ops.dropped(paths)
    }

    fn dropped(&self, paths: Vec<PathBuf>) {
        let (zips, other): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")));
        if !other.is_empty() {
            toast::show(self.store.ui, tr("Hier lassen sich nur .zip-Dateien ablegen."), None);
        }
        if !zips.is_empty() {
            self.add(zips, false);
        }
    }

    fn pick(&self) {
        let ops = self.clone();
        spawn_in_window(async move {
            let (title, filter) = if ops.kind == Kind::Shader { (tr("Shader hinzufügen"), tr("Shader")) } else { (tr("Ressourcenpakete hinzufügen"), tr("Ressourcenpakete")) };
            let Some(picked) = off_ui(move || native::pick_files(&title, &filter, &["zip"], true)).await else { return };
            ops.dropped(picked);
        });
    }

    /// Copies the packs `zips` into the folder; with `replace` packs of the same name are replaced, otherwise a sheet asks.
    fn add(&self, zips: Vec<PathBuf>, replace: bool) {
        let ops = self.clone();
        spawn_in_window(async move {
            let (home, dir, work) = (ops.instance_dir.clone(), ops.dir.clone(), zips.clone());
            let what = if replace { tr("Nicht ersetzt.") } else { tr("Nicht hinzugefügt.") };
            match off_ui(move || packs::add_files(&home, &dir, &work, replace)).await {
                Some(Ok(done)) => {
                    if !done.added.is_empty() {
                        ops.flash_rows(&done.added);
                        let label = if let [one] = done.added.as_slice() { format!("„{}“", shown_name(one)) } else { trf("%lld Dateien", &[&done.added.len()]) };
                        toast::show(ops.store.ui, if replace { trf("%@ ersetzt.", &[&label]) } else { trf("%@ hinzugefügt.", &[&label]) }, None);
                    }
                    let clashes: Vec<PathBuf> = zips.into_iter().filter(|p| p.file_name().is_some_and(|n| done.taken.contains(&n.to_string_lossy().into_owned()))).collect();
                    ops.ask_replace(clashes);
                }
                Some(Err(e)) => {
                    toast::show(ops.store.ui, forms::explain_as(what, &e).text, None);
                }
                None => {
                    toast::show(ops.store.ui, what, None);
                }
            }
            actions::changed(ops.store);
        });
    }

    fn ask_replace(&self, zips: Vec<PathBuf>) {
        let names: Vec<String> = zips.iter().filter_map(|p| p.file_name()).map(|n| shown_name(&n.to_string_lossy())).collect();
        let heading = match names.as_slice() {
            [] => return,
            [one] => trf("„%@“ ersetzen?", &[one]),
            many => trf("%lld Dateien ersetzen?", &[&many.len()]),
        };
        let message = match names.as_slice() {
            [_] => tr("Es gibt schon eine Datei mit diesem Namen."),
            _ => trf("Diese Namen gibt es schon: %@.", &[&names.join(", ")]),
        };
        let (ops, ui, key) = (self.clone(), self.store.ui, self.area());
        let primary = sheet::Primary {
            label: tr("Ersetzen"),
            disabled: None,
            danger: false,
            busy: false,
            run: EventHandler::new(move |_| {
                sheet::close(ui);
                ops.add(zips.clone(), true);
            }),
        };
        sheet::open(ui, sheet::Card::new(format!("instance.{key}.replace"), heading).message(message).primary(primary).primary_id(format!("instance.{key}.replace.ok")).on_enter(move |_| sheet::close(ui)).width(frame::SHEET_NARROW));
    }

    /// Washes the rows of `names` in the accent for a moment.
    fn flash_rows(&self, names: &[String]) {
        let mine: Rc<HashSet<String>> = Rc::new(names.iter().map(|n| n.to_lowercase()).collect());
        let flash = self.flash;
        flash.soft(|f| *f = mine.clone());
        spawn_in_window(async move {
            after(FLASH_FOR).await;
            flash.soft(|f| {
                if Rc::ptr_eq(f, &mine) {
                    *f = Rc::new(HashSet::new());
                }
            });
        });
    }

    fn area(&self) -> &'static str {
        spot(self.kind)
    }

    fn info(&self, file: &str) -> bool {
        if !self.matches.contains_key(file) {
            return false;
        }
        self.pane.show(file);
        true
    }

    fn subject(&self, file: Option<&str>) -> Subject {
        match file.and_then(|f| self.matches.get(f)) {
            Some(p) => Subject::Project { id: p.project.clone(), title: p.title.clone(), kind: self.kind },
            None if file.is_some() && !self.settled => Subject::Nothing(String::new()),
            None => Subject::Nothing(if file.is_some() { tr("Nicht auf Modrinth gefunden.") } else { tr("Wähle einen Eintrag, um Details zu sehen.") }),
        }
    }

    fn versions(&self, file: &str) {
        if let Some(p) = self.matches.get(file) {
            browser::open_project(self.store, &self.id, self.kind, &p.project, &p.title);
        }
    }

    fn remove(&self, e: &Entry) {
        remove(self.store, self.instance_dir.clone(), self.dir.clone(), self.kind, self.loader.clone(), e.file.clone(), e.on);
    }

    fn discard(&self, list: Cursor, among: &[Entry], e: &Entry) {
        if let Some(i) = among.iter().position(|x| x.file == e.file) {
            match among.get(i + 1).or_else(|| i.checked_sub(1).and_then(|p| among.get(p))) {
                Some(next) => list.stand(&next.file),
                None => list.at.soft(|c| *c = None),
            }
        }
        self.remove(e);
    }

    fn toggle(&self, e: &Entry, on: bool) {
        switch(self.store, self.instance_dir.clone(), self.kind, self.loader.clone(), e.file.clone(), on);
    }

    fn actions(&self, e: &Entry) -> Vec<RowAction> {
        let mut actions = Vec::new();
        if self.matches.contains_key(&e.file) {
            let (ops, file) = (self.clone(), e.file.clone());
            actions.push(RowAction { id: "info", icon: Icon::Info, label: tr("Readme und Details"), tip: Some(detail::with_key(tr("Readme und Details"))), danger: false, disabled: None, run: EventHandler::new(move |_| {
                ops.info(&file);
            }) });
        }
        let (ops, e) = (self.clone(), e.clone());
        actions.push(RowAction { id: "remove", icon: Icon::Trash, label: tr("Entfernen"), tip: None, danger: true, disabled: None, run: EventHandler::new(move |_| ops.remove(&e)) });
        actions
    }

    fn menu(&self, e: &Entry, at: (f32, f32)) {
        let key = self.area();
        let mut items = Vec::new();
        if let Some(p) = self.matches.get(&e.file) {
            let (ops, file, web) = (self.clone(), e.file.clone(), p.project.clone());
            let kind = self.kind;
            items.push(
                Item::new(format!("instance.{key}.menu.project"), tr("Readme und Versionen"), move |_| {
                    ops.info(&file);
                })
                .shortcut(detail::TOGGLE.label()),
            );
            items.push(Item::new(format!("instance.{key}.menu.web"), tr("Auf Modrinth öffnen"), move |_| open_external("modrinth", format!("https://modrinth.com/{}/{web}", kind.project_type()))));
            items.push(Item::Separator);
        }
        let (ops, e) = (self.clone(), e.clone());
        items.push(Item::new(format!("instance.{key}.menu.remove"), tr("Entfernen"), move |_| ops.remove(&e)).danger());
        menu::open(self.store.ui, Menu::at(items, at));
    }

    fn key_down(&self, list: Cursor, shown: &[Entry], e: &KeyboardEventData) -> bool {
        if self.store.ui.layered() || e.modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) {
            return false;
        }
        if matches!(e.key, Key::Named(NamedKey::Escape)) && self.pane.is_open() {
            self.pane.close();
            return true;
        }
        if let Some(delta) = cursor::motion(&e.key) {
            list.go(&shown.iter().map(|e| e.file.clone()).collect::<Vec<_>>(), delta);
            return true;
        }
        let own = list.focus.is_focused();
        let Some(i) = list.current().and_then(|f| shown.iter().position(|e| e.file == f)) else { return false };
        let entry = &shown[i];
        match &e.key {
            Key::Named(NamedKey::Enter) if own => {
                if !self.info(&entry.file) {
                    self.menu(entry, list.menu_point(i));
                }
            }
            Key::Named(NamedKey::ContextMenu) if own => self.menu(entry, list.menu_point(i)),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => self.discard(list, shown, entry),
            Key::Character(c) if own && c == " " => self.toggle(entry, !entry.on),
            _ => return false,
        }
        true
    }

    /// The palette entries of the tab; the pack they act on is the one under the cursor.
    fn extras(&self, list: Cursor, filter: AccessibilityId, entries: &[Entry]) -> Vec<Extra> {
        let key = self.area();
        let add = {
            let ops = self.clone();
            let label = if self.kind == Kind::Shader { tr("Shader aus Datei hinzufügen…") } else { tr("Ressourcenpakete aus Datei hinzufügen…") };
            Extra { id: format!("instance.{key}.addfile"), label, glyph: Icon::Upload, shortcut: Some(Shortcut::alt(Code::KeyO)), run: EventHandler::new(move |_| ops.pick()) }
        };
        if entries.is_empty() {
            return vec![add];
        }
        let shader = self.kind == Kind::Shader;
        let (find, flip, drop_it) = if shader {
            (tr("Shader filtern"), tr("Shader ein- oder ausschalten"), tr("Shader entfernen"))
        } else {
            (tr("Ressourcenpakete filtern"), tr("Ressourcenpaket ein- oder ausschalten"), tr("Ressourcenpaket entfernen"))
        };
        let among: Rc<Vec<Entry>> = Rc::new(entries.to_vec());
        let act = |id: &str, label: String, glyph: Icon, run: fn(&Ops, Cursor, &[Entry], &Entry)| {
            let (ops, among) = (self.clone(), among.clone());
            Extra {
                id: format!("instance.{key}.{id}"),
                label,
                glyph,
                shortcut: None,
                run: EventHandler::new(move |_| match list.current().and_then(|f| among.iter().find(|e| e.file == f)) {
                    Some(entry) => run(&ops, list, &among, entry),
                    None => {
                        toast::show(ops.store.ui, tr("Wähle zuerst einen Eintrag in der Liste."), None);
                    }
                }),
            }
        };
        let details = {
            let ops = self.clone();
            Extra {
                id: format!("instance.{key}.details"),
                label: self.pane.label(),
                glyph: Icon::Info,
                shortcut: Some(detail::TOGGLE),
                run: EventHandler::new(move |_| ops.pane.toggle()),
            }
        };
        vec![
            Extra { id: format!("instance.{key}.filter"), label: find, glyph: Icon::Search, shortcut: Some(Shortcut::primary(Code::KeyF)), run: EventHandler::new(move |_| filter.request_focus()) },
            details,
            act("toggle", flip, Icon::Check, |ops, _, _, e| ops.toggle(e, !e.on)),
            act("remove", drop_it, Icon::Trash, |ops, list, among, e| ops.discard(list, among, e)),
            add,
        ]
    }
}

/// The rows the list builds while they are on screen.
struct Rows {
    shown: Rc<Vec<Entry>>,
    ops: Ops,
    at: Option<String>,
    flash: Rc<HashSet<String>>,
    list: Cursor,
    area: &'static str,
}

impl PartialEq for Rows {
    fn eq(&self, other: &Self) -> bool {
        *self.shown == *other.shown && self.at == other.at && Rc::ptr_eq(&self.ops.matches, &other.ops.matches) && Rc::ptr_eq(&self.flash, &other.flash) && self.ops.id == other.ops.id && self.ops.kind == other.ops.kind
    }
}

impl Rows {
    fn item(&self, index: usize) -> Element {
        let Some(e) = self.shown.get(index) else { return rect().height(Size::px(ROW)).into_element() };
        let (ops, list) = (&self.ops, self.list);
        let kind = ops.kind;
        let on_toggle = {
            let (ops, e) = (ops.clone(), e.clone());
            EventHandler::new(move |on| ops.toggle(&e, on))
        };
        let on_menu = {
            let (ops, e) = (ops.clone(), e.clone());
            EventHandler::new(move |at| ops.menu(&e, at))
        };
        let on_press = {
            let file = e.file.clone();
            EventHandler::new(move |_| {
                list.stand(&file);
                list.focus.request_focus();
            })
        };
        let pack = ops.matches.get(&e.file);
        rect()
            .key(&e.file)
            .width(Size::fill())
            .height(Size::px(ROW))
            .child(EntryRow {
                area: self.area,
                key: e.file.to_lowercase(),
                tip: e.file.clone(),
                name: shown_name(&e.file),
                caption: if e.folder { tr("Ordner") } else { bytes(e.size) },
                warn: false,
                enabled: e.on,
                icon: e.icon.clone(),
                glyph: if e.folder { Icon::Folder } else if kind == Kind::Shader { Icon::Sparkles } else { Icon::Palette },
                selected: false,
                cursor: self.at.as_deref() == Some(e.file.as_str()),
                flash: self.flash.contains(&e.file.to_lowercase()),
                locked: None,
                update: pack.is_some_and(|p| p.update).then(String::new),
                actions: ops.actions(e),
                trail: None,
                on_toggle,
                on_press: Some(on_press),
                on_menu: Some(on_menu),
            })
            .into_element()
    }
}

/// The resource packs or shader packs of an instance with what switches them on and off.
#[derive(PartialEq)]
pub struct Packs {
    pub facts: Facts,
    pub kind: Kind,
}

fn matches_key(dir: &Path, entries: &[Entry]) -> String {
    format!("{}|{}", dir.display(), entries.iter().filter(|e| !e.folder).map(|e| e.file.as_str()).collect::<Vec<_>>().join("\n"))
}

impl Component for Packs {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let (kind, f) = (self.kind, &self.facts);
        let made = &f.made;
        let (id, instance_dir, loader) = (made.id.clone(), made.dir(), made.loader.clone());
        let dir = packs::pack_dir(&instance_dir, kind, None).unwrap_or_else(|| instance_dir.clone());
        let changed = *store.ui.instance.changed.read();
        let mut listing = use_state(|| Listing::Loading);
        let query = use_state(String::new);
        let list = use_cursor();
        let filter_focus = use_a11y();
        let mut known = use_state(|| (String::new(), Matches::default()));
        let mut more_bounds = use_state(Bounds::default);
        let flash = use_state(|| Rc::new(HashSet::<String>::new()));
        let dropping = use_state(|| false);
        let key = spot(kind);
        use_side_effect_with_deps(&(dir.clone(), instance_dir.clone(), kind, changed), move |(dir, instance_dir, kind, _)| {
            let (dir, instance_dir, kind) = (dir.clone(), instance_dir.clone(), *kind);
            spawn(async move {
                let found = off_ui(move || scan(&dir, &instance_dir, kind)).await.unwrap_or_default();
                if listing.try_peek().is_some() {
                    listing.set(Listing::Ready(found));
                }
            });
        });
        let shader_loader = packs::shader_loader(&made.mods, &made.local_mods);
        let filter = Filter::of(kind, &made.game_version, &loader, shader_loader);
        let signature = match &*listing.read() {
            Listing::Ready(entries) => matches_key(&dir, entries),
            Listing::Loading => String::new(),
        };
        let tag = format!("{signature}|{filter:?}");
        let looking = dir.clone();
        use_side_effect_with_deps(&(signature, filter.clone()), move |(signature, filter)| {
            if signature.is_empty() {
                return;
            }
            let tag = format!("{signature}|{filter:?}");
            if let Some(found) = looked(&tag) {
                known.set((tag, found));
                return;
            }
            let (filter, dir) = (filter.clone(), looking.clone());
            spawn(async move {
                if let Some(Ok(found)) = off_ui(move || lookup(&dir, &filter)).await {
                    let found = Matches::new(found);
                    remember(tag.clone(), found.clone());
                    if known.try_peek().is_some() {
                        known.set((tag, found));
                    }
                }
            });
        });
        let (ready, entries) = match &*listing.read() {
            Listing::Ready(entries) => (true, entries.clone()),
            Listing::Loading => (false, Vec::new()),
        };
        let text = query.read().clone();
        let needle = text.trim().to_lowercase();
        let shown: Rc<Vec<Entry>> = Rc::new(entries.iter().filter(|e| needle.is_empty() || shown_name(&e.file).to_lowercase().contains(&needle)).cloned().collect());
        let matches = {
            let known = known.read();
            if known.0 == tag { known.1.clone() } else { Matches::default() }
        };
        let pane = use_pane(store, list);
        let settled = known.read().0 == tag;
        let ops = Ops { store, pane, settled, id: id.clone(), instance_dir: instance_dir.clone(), dir: dir.clone(), kind, loader: loader.clone(), matches, flash };
        let at = list.at.read().clone().filter(|_| list.focus.is_focused() || pane.shown());
        super::use_extras(ops.extras(list, filter_focus, &entries));
        if !ready {
            return rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Lädt …"))).into_element();
        }
        let add_id = id.clone();
        let hint = (kind == Kind::Shader && shader_loader.is_none()).then(|| packs::shader_mod(&loader).map(|(_, name)| name)).flatten();
        let loader_bar = |name: &str| {
            let find_id = id.clone();
            bar(&t, Icon::Alert, t.attention_text, trf("Shader brauchen %@. Ohne laden sie nicht.", &[&name])).child(Button::new(format!("instance.{key}.loader"), trf("%@ hinzufügen…", &[&name])).on_press(move |_| browser::find(store, &find_id, Kind::Mod)))
        };
        if entries.is_empty() {
            let (text, body) = match kind {
                Kind::Shader => (tr("Noch keine Shader"), tr("Shader ändern das Licht und die Schatten im Spiel.")),
                _ => (tr("Noch keine Ressourcenpakete"), tr("Ressourcenpakete ändern, wie das Spiel aussieht und klingt.")),
            };
            let add = Button::new(format!("instance.{key}.empty.add"), tr("Hinzufügen…")).glyph(Icon::Plus).primary().on_press(move |_| browser::find(store, &add_id, kind));
            let from_file = {
                let ops = ops.clone();
                Button::new(format!("instance.{key}.empty.add_file"), tr("Aus Datei hinzufügen…")).glyph(Icon::Upload).on_press(move |_| ops.pick())
            };
            let empty = centered(&t, if kind == Kind::Shader { Icon::Sparkles } else { Icon::Palette }, text, body, [add, from_file]);
            let page = match hint {
                None => empty,
                Some(name) => page::column().height(Size::fill()).spacing(space::S).content(Content::flex()).child(loader_bar(&name)).child(rect().width(Size::fill()).height(Size::flex(1.)).child(empty)).into_element(),
            };
            return drop_zone(&t, dropping, ops.dropper(), page);
        }

        let placeholder = if kind == Kind::Shader { tr("Shader filtern") } else { tr("Ressourcenpakete filtern") };
        let more = {
            let reveal_dir = dir.clone();
            rect().on_sized(move |e: Event<SizedEventData>| more_bounds.set_if_modified(Bounds::of(&e))).child(Button::icon(format!("instance.{key}.more"), Icon::More, tr("Mehr")).plain().on_press(move |_| {
                let reveal_dir = reveal_dir.clone();
                let items = vec![Item::new(format!("instance.{key}.more.folder"), tr("Ordner öffnen"), move |_| {
                    let _ = std::fs::create_dir_all(&reveal_dir);
                    native::open(&reveal_dir);
                })];
                menu::open(ui, Menu::under(items, *more_bounds.peek()));
            }))
        };
        let filter = {
            let order: Vec<String> = shown.iter().map(|e| e.file.clone()).collect();
            rect()
                .width(Size::flex(1.))
                .on_key_down(move |e: Event<KeyboardEventData>| {
                    if matches!(e.key, Key::Named(NamedKey::Escape)) && pane.is_open() {
                        e.stop_propagation();
                        pane.close();
                    } else {
                        list.filter_keys(&e, query, || list.go(&order, 1));
                    }
                })
                .child(Field::new(format!("instance.{key}.filter"), query).placeholder(placeholder).glyph(Icon::Search).a11y(filter_focus).on_submit({
                    let order: Vec<String> = shown.iter().map(|e| e.file.clone()).collect();
                    move |_| {
                        list.focus.request_focus();
                        if list.current().is_none() {
                            list.go(&order, 1);
                        }
                    }
                }))
        };
        let from_file = {
            let ops = ops.clone();
            Button::new(format!("instance.{key}.add_file"), tr("Aus Datei hinzufügen…")).plain().glyph(Icon::Upload).on_press(move |_| ops.pick())
        };
        let mut top = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(filter).child(from_file).child(more);
        if !entries.is_empty() {
            top = top.child(Button::new(format!("instance.{key}.add"), tr("Hinzufügen…")).glyph(Icon::Plus).primary().on_press(move |_| browser::find(store, &add_id, kind)));
        }

        let count = if needle.is_empty() { trf("%lld installiert", &[&entries.len()]) } else { trf("%lld von %lld", &[&shown.len(), &entries.len()]) };
        let counts = rect().width(Size::fill()).height(Size::px(control::S)).horizontal().cross_align(Alignment::Center).child(caption(&t, count).max_lines(1));

        let rows = Rows { shown: shown.clone(), ops: ops.clone(), at, flash: ops.flash.read().clone(), list, area: key };

        let body: Element = if shown.is_empty() {
            let mut clear = query;
            let button = Button::new(format!("instance.{key}.empty.clear"), tr("Filter löschen")).on_press(move |_| clear.set(String::new()));
            centered(&t, Icon::Search, trf("Nichts passt zu „%@“.", &[&text.trim()]), String::new(), Some(button))
        } else {
            let keys = ops.clone();
            let among = shown.clone();
            rect()
                .a11y_id(list.focus)
                .a11y_focusable(true)
                .a11y_role(AccessibilityRole::List)
                .a11y_alt(if kind == Kind::Shader { tr("Shader") } else { tr("Ressourcenpakete") })
                .width(Size::fill())
                .height(Size::flex(1.))
                .on_key_down(move |e: Event<KeyboardEventData>| {
                    if keys.key_down(list, &among, &e) {
                        e.stop_propagation();
                    }
                })
                .child(VirtualScrollView::new_with_data_controlled(rows, |item, rows: &Rows| rows.item(item.index), list.scroll).length(shown.len()).item_size(ROW).scroll_with_arrows(false).on_sized(move |e: Event<SizedEventData>| list.sized(&e)))
                .into_element()
        };

        let mut root = page::column().height(Size::fill()).spacing(space::S).child(top);
        if let Some(name) = &hint {
            root = root.child(loader_bar(name));
        }
        let left = root.child(counts).child(body).content(Content::flex());
        if !pane.shown() {
            return drop_zone(&t, dropping, ops.dropper(), detail::split(&t, 0., left, None));
        }
        let window = ui.window.read().0;
        let current = list.current();
        let subject = ops.subject(current.as_deref());
        let (close, show) = (ops.clone(), ops.clone());
        let versions = current.filter(|f| ops.matches.contains_key(f)).map(|f| EventHandler::new(move |_| show.versions(&f)));
        let back = if kind == Kind::Shader { tr("Shader") } else { tr("Ressourcenpakete") };
        let detail = Detail { id: if kind == Kind::Shader { "instance.shaders.detail" } else { "instance.packs.detail" }, subject, narrow: window < page::SPLIT_AT, back, close: EventHandler::new(move |_| close.pane.close()), versions };
        drop_zone(&t, dropping, ops.dropper(), detail::split(&t, window, left, Some(detail)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_zips_and_folders_are_packs() {
        assert!(is_pack("Faithful.zip", false));
        assert!(is_pack("Loose Pack", true));
        assert!(is_pack("BSL.ZIP", false));
        assert!(!is_pack("readme.txt", false));
        assert!(!is_pack(".DS_Store", false));
        assert!(!is_pack(".hidden", true));
    }

    #[test]
    fn packs_that_are_on_come_first_then_by_name() {
        let inst = tempfile::tempdir().unwrap();
        let dir = inst.path().join("resourcepacks");
        std::fs::create_dir_all(dir.join("Folder pack")).unwrap();
        for name in ["b.zip", "A.zip", "c.zip", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        packs::switch(inst.path(), Kind::ResourcePack, "c.zip", true, &crate::core::instance::Loader::Vanilla).unwrap();
        let got: Vec<(String, bool)> = scan(&dir, inst.path(), Kind::ResourcePack).into_iter().map(|e| (e.file, e.on)).collect();
        assert_eq!(got, [("c.zip".to_owned(), true), ("A.zip".to_owned(), false), ("b.zip".to_owned(), false), ("Folder pack".to_owned(), false)]);
        assert!(scan(&inst.path().join("missing"), inst.path(), Kind::ResourcePack).is_empty());
    }

    #[test]
    fn a_pack_is_shown_without_its_extension() {
        assert_eq!(shown_name("Faithful 64x.zip"), "Faithful 64x");
        assert_eq!(shown_name("Folder"), "Folder");
    }
}