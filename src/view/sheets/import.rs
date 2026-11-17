//! Importieren: instances of other launchers found on this machine, chosen and copied, and `.mrpack` or Prism files picked by hand.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use freya::prelude::*;

use crate::core::drive;
use crate::core::i18n::{tr, trf};
use crate::core::launchers::{Found, Launcher, Library, Take, Unsupported};
use crate::ui::instance_files::bytes;
use crate::ui::remote;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::{self, Field};
use crate::view::kit::group::{Line, group, status};
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::spinner;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::{line, para};
use crate::view::install::{self, Source};
use crate::view::kit::{fades, off_thread, off_ui, ring, use_arrows, use_theme};
use crate::view::native;
use crate::view::sheets::instance::Origin;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};

const FILTER_FROM: usize = 8;
const LIST_MIN: f32 = 140.;
const LIST_MAX: f32 = 320.;
const CHROME: f32 = 580.;
const ROW_H: f32 = control::L + space::S;
const HEAD_H: f32 = control::M;
const FOOT_H: f32 = font::CAPTION + space::S;
const BLOCK_GAP: f32 = space::M;
const BLOCK_INNER: f32 = space::XS;
const BOX: f32 = iconsize::M;
const LOOKED_FOR: [Launcher; 7] = [Launcher::Prism, Launcher::MultiMc, Launcher::Modrinth, Launcher::CurseForge, Launcher::AtLauncher, Launcher::GdLauncher, Launcher::Vanilla];

/// Opens the new-instance sheet on Importieren.
pub fn open(store: Store) {
    super::instance::import(store);
}

/// Installs the pack or Prism export at `path` as a new instance, with its progress on the instance's page.
pub fn open_file(store: Store, path: PathBuf) {
    install::start(store, vec![Source::File(path)]);
}

type Profile = (PathBuf, String);

fn profile_key(found: &Found) -> String {
    format!("{}|{}", found.key(), found.game_dir.display())
}

fn profiles(libs: &[Library]) -> HashMap<String, Profile> {
    libs.iter()
        .flat_map(|l| &l.instances)
        .map(|found| {
            let dir = std::fs::canonicalize(&found.game_dir).unwrap_or_else(|_| found.game_dir.clone());
            (profile_key(found), (dir, format!("{:?} {:?}", found.game_version, found.loader)))
        })
        .collect()
}

fn profile_of(known: &HashMap<String, Profile>, found: &Found) -> Profile {
    known.get(&profile_key(found)).cloned().unwrap_or_else(|| (found.game_dir.clone(), String::new()))
}

fn picked(libs: &[Library], chosen: &HashSet<String>, known: &HashMap<String, Profile>) -> Vec<Found> {
    let mut seen = HashSet::new();
    libs.iter().flat_map(|l| &l.instances).filter(|i| i.importable() && chosen.contains(&i.key())).filter(|i| seen.insert(profile_of(known, i))).cloned().collect()
}

fn merge(libs: &mut Vec<Library>, known: &mut HashMap<String, Profile>, found: Vec<Library>, scanned: HashMap<String, Profile>) -> (bool, Vec<String>) {
    let listed: HashSet<Profile> = libs.iter().flat_map(|l| &l.instances).map(|i| profile_of(known, i)).collect();
    let found: Vec<Library> = found
        .into_iter()
        .map(|mut lib| {
            lib.instances.retain(|i| !listed.contains(&profile_of(&scanned, i)));
            lib
        })
        .filter(|lib| !lib.instances.is_empty())
        .collect();
    let fresh: Vec<String> = found.iter().flat_map(|l| &l.instances).filter(|i| i.importable()).map(Found::key).collect();
    let added = !found.is_empty();
    known.extend(scanned);
    for lib in found {
        match libs.iter_mut().find(|l| l.launcher == lib.launcher && l.root == lib.root) {
            Some(have) => have.instances.extend(lib.instances),
            None => libs.push(lib),
        }
    }
    (added, fresh)
}

struct Shown {
    lib: usize,
    launcher: Launcher,
    root: Option<String>,
    items: Vec<(usize, Found)>,
}

fn shown(libs: &[Library], query: &str) -> Vec<Shown> {
    libs.iter()
        .enumerate()
        .map(|(lib, l)| Shown { lib, launcher: l.launcher, root: l.root.is_absolute().then(|| l.root.display().to_string()), items: l.instances.iter().cloned().enumerate().filter(|(_, i)| query.is_empty() || i.name.to_lowercase().contains(query)).collect() })
        .filter(|s| !s.items.is_empty())
        .collect()
}

fn keys(list: &[Shown]) -> Vec<String> {
    list.iter().flat_map(|s| &s.items).filter(|(_, i)| i.importable()).map(|(_, i)| i.key()).collect()
}

fn block_height(rows: usize, foot: bool) -> f32 {
    HEAD_H + BLOCK_INNER + rows as f32 * ROW_H + rows.saturating_sub(1) as f32 * frame::HAIRLINE + if foot { BLOCK_INNER + FOOT_H } else { 0. }
}

fn top_of(list: &[Shown], key: &str) -> Option<(f32, f32)> {
    let mut top = 0.;
    for s in list {
        let first = top + HEAD_H + BLOCK_INNER;
        for (at, (_, i)) in s.items.iter().enumerate() {
            if i.key() == key {
                let row = first + at as f32 * (ROW_H + frame::HAIRLINE);
                return Some(if at == 0 { (top, row - top + ROW_H) } else { (row, ROW_H) });
            }
        }
        top += block_height(s.items.len(), s.root.is_some()) + BLOCK_GAP;
    }
    None
}

fn reason(why: &Unsupported) -> String {
    match why {
        Unsupported::NoVersion => tr("Ohne Minecraft-Version."),
        Unsupported::Loader(loader) => trf("Der Loader %@ wird nicht unterstützt.", &[&loader]),
        Unsupported::Unreadable => tr("Die Instanz ist unvollständig oder beschädigt."),
    }
}

fn subtitle(found: &Found) -> String {
    let line = found.version_line();
    match found.mods {
        0 => line,
        1 => format!("{line} · {}", tr("1 Mod")),
        n => format!("{line} · {}", trf("%lld Mods", &[&n])),
    }
}

type Ids = Rc<RefCell<HashMap<String, AccessibilityId>>>;

fn id_of(ids: &Ids, key: &str) -> AccessibilityId {
    *ids.borrow_mut().entry(key.to_owned()).or_insert_with(AccessibilityId::new_unique)
}

#[derive(Clone, Copy)]
struct Flow {
    store: Store,
    libs: State<Option<Vec<Library>>>,
    known: State<HashMap<String, Profile>>,
    chosen: State<HashSet<String>>,
    reveal: State<Option<String>>,
    ids: State<Ids>,
    filter: State<String>,
    settings: State<bool>,
    worlds: State<bool>,
    note: State<Option<String>>,
    failed: State<Option<String>>,
    scanning: State<bool>,
    picking: State<bool>,
    saves: State<HashMap<PathBuf, Option<u64>>>,
}

impl Flow {
    fn alive(self) -> bool {
        self.libs.try_peek().is_some()
    }

    fn total(self) -> usize {
        self.libs.peek().as_ref().map_or(0, |l| l.iter().map(|l| l.instances.len()).sum())
    }

    fn query(self) -> String {
        if self.total() > FILTER_FROM { self.filter.peek().trim().to_lowercase() } else { String::new() }
    }

    fn visible(self) -> Vec<Shown> {
        let libs = self.libs.peek();
        shown(libs.as_deref().unwrap_or_default(), &self.query())
    }

    fn picked(self) -> Vec<Found> {
        let seen: HashSet<String> = keys(&self.visible()).into_iter().collect();
        let chosen: HashSet<String> = self.chosen.peek().iter().filter(|k| seen.contains(*k)).cloned().collect();
        let libs = self.libs.peek();
        picked(libs.as_deref().unwrap_or_default(), &chosen, &self.known.peek())
    }

    fn scan(mut self) {
        self.failed.set(None);
        let Some(rx) = off_thread("import-scan", || {
            let libs = remote::launcher_libraries();
            let known = profiles(&libs);
            (libs, known)
        }) else {
            self.libs.set(Some(Vec::new()));
            self.failed.set(Some(tr("Die Suche ließ sich nicht starten.")));
            return;
        };
        spawn_in_window(async move {
            let found = rx.recv().await;
            if !self.alive() {
                return;
            }
            match found {
                Ok((libs, known)) => {
                    let all: Vec<String> = libs.iter().flat_map(|l| &l.instances).filter(|i| i.importable()).map(Found::key).collect();
                    self.chosen.write().extend(all);
                    self.known.set(known);
                    self.libs.set(Some(libs));
                }
                Err(_) => {
                    self.libs.set(Some(Vec::new()));
                    self.failed.set(Some(tr("Die Suche ließ sich nicht abschließen.")));
                }
            }
        });
    }

    fn rescan(mut self) {
        if self.libs.peek().is_none() {
            return;
        }
        self.libs.set(None);
        self.note.set(None);
        self.scan();
    }

    fn choose_folder(mut self) {
        if *self.scanning.peek() || *self.picking.peek() || self.libs.peek().is_none() {
            return;
        }
        self.picking.set(true);
        spawn_in_window(async move {
            let folder = if drive::on() { drive::answer("open folder").and_then(|p| p.into_iter().next()) } else { rfd::AsyncFileDialog::new().set_title(tr("Ordner wählen")).pick_folder().await.map(|f| f.path().to_path_buf()) };
            if !self.alive() {
                return;
            }
            self.picking.set(false);
            if let Some(folder) = folder {
                self.scan_folder(folder);
            }
        });
    }

    fn scan_folder(mut self, folder: PathBuf) {
        self.scanning.set(true);
        let Some(rx) = off_thread("import-folder", move || {
            let found = remote::launcher_folder(&folder);
            let known = profiles(&found);
            (found, known)
        }) else {
            self.scanning.set(false);
            self.note.set(Some(tr("Die Suche ließ sich nicht starten.")));
            return;
        };
        spawn_in_window(async move {
            let result = rx.recv().await;
            if !self.alive() {
                return;
            }
            self.scanning.set(false);
            let Ok((found, scanned)) = result else {
                self.note.set(Some(tr("Die Suche ließ sich nicht abschließen.")));
                return;
            };
            let mut libs = self.libs.peek().clone().unwrap_or_default();
            let mut known = self.known.peek().clone();
            let (added, fresh) = merge(&mut libs, &mut known, found, scanned);
            self.note.set(if added { None } else { Some(tr("In diesem Ordner liegt keine neue Instanz, die sich übernehmen lässt.")) });
            self.known.set(known);
            self.libs.set(Some(libs));
            self.chosen.write().extend(fresh);
        });
    }

    fn pick_files(mut self) {
        if *self.picking.peek() || *self.scanning.peek() {
            return;
        }
        self.picking.set(true);
        spawn_in_window(async move {
            let title = tr("Modpack wählen");
            let paths = off_ui(move || native::pick_files(&title, "", &[], true)).await.unwrap_or_default();
            if !self.alive() {
                return;
            }
            self.picking.set(false);
            self.hand_over(paths.into_iter().map(Source::File).collect());
        });
    }

    fn toggle(mut self, key: &str) {
        let mut chosen = self.chosen.write();
        if !chosen.remove(key) {
            chosen.insert(key.to_owned());
        }
    }

    fn flip(mut self, keys: Vec<String>) {
        let mut chosen = self.chosen.write();
        if keys.iter().all(|k| chosen.contains(k)) {
            for key in &keys {
                chosen.remove(key);
            }
        } else {
            chosen.extend(keys);
        }
    }

    fn flip_library(self, lib: usize) {
        let keys = self.visible().into_iter().filter(|s| s.lib == lib).flat_map(|s| s.items).filter(|(_, i)| i.importable()).map(|(_, i)| i.key()).collect();
        self.flip(keys);
    }

    fn select_all(mut self) {
        let all = keys(&self.visible());
        self.chosen.write().extend(all);
    }

    fn start(self) {
        if *self.picking.peek() {
            return;
        }
        let take = Take { settings: *self.settings.peek(), worlds: *self.worlds.peek() };
        self.hand_over(self.picked().into_iter().map(|f| Source::Found(f, take)).collect());
    }

    fn measure(mut self, dirs: Vec<PathBuf>) {
        let missing: Vec<PathBuf> = dirs.into_iter().filter(|d| !self.saves.peek().contains_key(d)).collect();
        if missing.is_empty() {
            return;
        }
        {
            let mut saves = self.saves.write();
            for dir in &missing {
                saves.insert(dir.clone(), None);
            }
        }
        spawn_in_window(async move {
            let measured = off_ui(move || missing.into_iter().map(|d| (crate::core::worlds::size(&d.join("saves")), d)).collect::<Vec<_>>()).await.unwrap_or_default();
            if !self.alive() {
                return;
            }
            let mut saves = self.saves.write();
            for (size, dir) in measured {
                saves.insert(dir, Some(size));
            }
        });
    }

    fn worlds_size(self) -> Option<u64> {
        let saves = self.saves.read();
        self.picked().iter().map(|f| saves.get(&f.game_dir).copied().flatten()).sum::<Option<u64>>().filter(|n| *n > 0)
    }

    fn hand_over(self, sources: Vec<Source>) {
        if sources.is_empty() {
            return;
        }
        sheet::close(self.store.ui);
        install::start(self.store, sources);
    }
}

fn primary_held(m: Modifiers) -> bool {
    m.contains(if cfg!(target_os = "macos") { Modifiers::META } else { Modifiers::CONTROL }) && !m.contains(Modifiers::SHIFT) && !m.contains(Modifiers::ALT)
}

/// The Importieren view of the new-instance sheet.
#[derive(PartialEq)]
pub(super) struct Import {
    pub origin: State<Origin>,
}

impl Component for Import {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let flow = Flow {
            store,
            libs: use_state(|| None),
            known: use_state(HashMap::new),
            chosen: use_state(HashSet::new),
            reveal: use_state(|| None),
            ids: use_state(Ids::default),
            filter: use_state(String::new),
            settings: use_state(|| true),
            worlds: use_state(|| true),
            note: use_state(|| None),
            failed: use_state(|| None),
            scanning: use_state(|| false),
            picking: use_state(|| false),
            saves: use_state(HashMap::new),
        };
        let scroll = use_scroll_controller(ScrollConfig::default);
        let viewport = use_state(|| 0f32);
        let content = use_state(|| 0f32);
        use_hook(move || flow.scan());
        let worlds_on = *flow.worlds.read();
        let dirs: Vec<PathBuf> = if worlds_on { flow.picked().into_iter().map(|f| f.game_dir).collect() } else { Vec::new() };
        use_side_effect_with_deps(&dirs, move |dirs| flow.measure(dirs.clone()));
        let typed = flow.filter.read().clone();
        use_side_effect_with_deps(&typed, move |_| {
            let mut reveal = flow.reveal;
            reveal.set_if_modified(None);
            let mut scroll = scroll;
            scroll.scroll_to_y(0);
        });
        let _ = flow.libs.read();
        let _ = flow.chosen.read();
        let list = flow.visible();
        let revealed = flow.reveal.read().clone();
        let at = revealed.as_deref().and_then(|k| top_of(&list, k));
        use_side_effect_with_deps(&at, move |at| {
            let Some((top, height)) = *at else { return };
            let mut scroll = scroll;
            let now = -scroll.position().y;
            let h = *viewport.peek();
            if top < now {
                scroll.scroll_to_y(-(top as i32));
            } else if h > 0. && top + height > now + h {
                scroll.scroll_to_y(-((top + height - h).ceil() as i32));
            }
        });
        let (_, window_h) = *store.ui.window.read();
        let list_max = (window_h - CHROME).clamp(LIST_MIN, LIST_MAX);
        let loading = flow.libs.read().is_none();
        let count = flow.picked().len();
        let scanning = *flow.scanning.read();
        let picking = *flow.picking.read();
        let blocked = if loading {
            Some(tr("Suche Launcher …"))
        } else if picking {
            Some(tr("Datei wird gewählt …"))
        } else if count == 0 {
            Some(tr("Wähle mindestens eine Instanz."))
        } else {
            None
        };
        let label = match count {
            0 => tr("Importieren"),
            1 => tr("Instanz importieren"),
            n => trf("%lld Instanzen importieren", &[&n]),
        };
        let waiting = if loading { Some(tr("Suche Launcher …")) } else { scanning.then(|| tr("Der Ordner wird gerade durchsucht.")) };
        let folder = Button::new("import.folder", if scanning { tr("Suche …") } else { tr("Ordner wählen…") }).glyph(Icon::Folder).disabled(waiting).on_press(move |_| flow.choose_folder());
        let body = rect()
            .width(Size::fill())
            .spacing(space::M)
            .child(super::instance::origins(self.origin, None))
            .child(para(&t, tr("Kopiert Instanzen. Der andere Launcher bleibt unverändert, Accounts werden nicht übernommen."), font::CALLOUT, t.text2))
            .child(pick_body(flow, &t, &list, (scroll, viewport, content), list_max, loading));
        Card::new("import", tr("Instanz erstellen"))
            .width(frame::SHEET)
            .body(body)
            .leading(folder)
            .primary(Primary { label, disabled: blocked, danger: false, busy: false, run: EventHandler::new(move |_| flow.start()) })
            .primary_id("import.start")
    }
}

fn pick_body(flow: Flow, t: &Theme, list: &[Shown], (scroll, mut viewport, mut content): (ScrollController, State<f32>, State<f32>), list_max: f32, loading: bool) -> Element {
    let mut body = rect().width(Size::fill()).spacing(space::M).on_global_key_down(move |e: Event<KeyboardEventData>| {
        if let Key::Character(c) = &e.key
            && c.eq_ignore_ascii_case("a")
            && primary_held(e.modifiers)
            && !field::typing()
        {
            e.stop_propagation();
            flow.select_all();
        }
    });
    let busy = *flow.picking.read() || *flow.scanning.read();
    body = body.child(PickFile { busy, press: EventHandler::new(move |_| flow.pick_files()) });
    if loading {
        return body.child(rect().padding((space::L, 0.)).child(spinner::waiting(t, tr("Suche Launcher …")))).into_element();
    }
    let total = flow.total();
    if total == 0 {
        if let Some(why) = flow.failed.read().clone() {
            return body.child(failure(flow, t, why)).into_element();
        }
        if let Some(note) = flow.note.read().clone() {
            body = body.child(status(t, note, t.attention_text));
        }
        return body.child(nothing(t)).into_element();
    }
    if total > FILTER_FROM {
        let mut head = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(Field::new("import.filter", flow.filter).placeholder(tr("Instanzen filtern")).glyph(Icon::Search).rows_through().width(Size::flex(1.)));
        if !flow.filter.read().is_empty() {
            let mut filter = flow.filter;
            head = head.child(Button::icon("import.filter.clear", Icon::Close, tr("Filter löschen")).plain().on_press(move |_| filter.set(String::new())));
        }
        body = body.child(head);
    }
    if let Some(note) = flow.note.read().clone() {
        body = body.child(status(t, note, t.attention_text));
    }
    if list.is_empty() {
        let query = flow.filter.read().trim().to_owned();
        body = body.child(rect().width(Size::fill()).padding((space::L, 0.)).child(para(t, trf("Keine Instanz passt zu „%@“. Lösche den Filter oder suche anders.", &[&query]), font::CALLOUT, t.text2)));
    } else {
        let chosen = flow.chosen.read().clone();
        let ids = flow.ids.read().clone();
        let order = keys(list);
        let mut column = rect().width(Size::fill()).spacing(BLOCK_GAP).on_sized(move |e: Event<SizedEventData>| content.set_if_modified(e.area.height()));
        for s in list {
            column = column.child(library(flow, t, s, &chosen, &ids, &order));
        }
        let edges = fades(t.raised, -scroll.position().y, *content.read(), *viewport.read());
        body = body.child(
            rect()
                .width(Size::fill())
                .on_sized(move |e: Event<SizedEventData>| viewport.set_if_modified(e.area.height()))
                .child(ScrollView::new_controlled(scroll).width(Size::fill()).height(Size::auto()).max_height(Size::px(list_max)).child(column))
                .children(edges),
        );
    }
    body.child(options(flow, t)).into_element()
}

fn failure(flow: Flow, t: &Theme, why: String) -> Element {
    rect()
        .width(Size::fill())
        .padding((space::L, 0.))
        .center()
        .spacing(space::M)
        .child(para(t, why, font::CALLOUT, t.danger_text).text_align(TextAlign::Center))
        .child(Button::new("import.scan.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| flow.rescan()))
        .into_element()
}

fn library(flow: Flow, t: &Theme, s: &Shown, chosen: &HashSet<String>, ids: &Ids, order: &[String]) -> Element {
    let any = s.items.iter().any(|(_, i)| i.importable());
    let all = s.items.iter().filter(|(_, i)| i.importable()).all(|(_, i)| chosen.contains(&i.key()));
    let lib = s.lib;
    let mut head = rect().width(Size::fill()).height(Size::px(HEAD_H)).padding((0., space::XS)).horizontal().cross_align(Alignment::Center).content(Content::flex()).child(rect().width(Size::flex(1.)).child(line(t, s.launcher.name(), font::CALLOUT, t.text2).font_weight(FontWeight::SEMI_BOLD)));
    if any {
        head = head.child(Button::new(format!("import.all.{}", s.launcher.key()), if all { tr("Keine abwählen") } else { tr("Alle wählen") }).plain().on_press(move |_| flow.flip_library(lib)));
    }
    let rows: Vec<Element> = s
        .items
        .iter()
        .map(|(_, found)| {
            let key = found.key();
            let why = found.unsupported.as_ref().map(reason);
            let detail = why.clone().unwrap_or_else(|| subtitle(found));
            let id = format!("import.row.{key}");
            let place = order.iter().position(|k| *k == key);
            let before = place.and_then(|p| p.checked_sub(1)).and_then(|p| order.get(p)).map(|k| id_of(ids, k));
            let after = place.and_then(|p| order.get(p + 1)).map(|k| id_of(ids, k));
            let (toggled, shown) = (key.clone(), key.clone());
            let mut reveal = flow.reveal;
            rect()
                .key(id.clone())
                .width(Size::fill())
                .child(Entry {
                    id,
                    own: id_of(ids, &key),
                    before,
                    after,
                    title: found.name.clone(),
                    detail,
                    on: chosen.contains(&key),
                    reason: why,
                    press: EventHandler::new(move |_| flow.toggle(&toggled)),
                    reveal: EventHandler::new(move |_| reveal.set_if_modified(Some(shown.clone()))),
                })
                .into_element()
        })
        .collect();
    rect()
        .width(Size::fill())
        .spacing(BLOCK_INNER)
        .child(head)
        .child(group(t, None, rows, None))
        .maybe_child(s.root.clone().map(|root| rect().width(Size::fill()).height(Size::px(FOOT_H)).padding((0., space::XS)).child(line(t, root, font::CAPTION, t.text3))))
        .into_element()
}

fn nothing(t: &Theme) -> Element {
    let names = LOOKED_FOR.iter().map(|l| l.name()).collect::<Vec<_>>().join(", ");
    rect()
        .width(Size::fill())
        .padding((space::L, 0.))
        .center()
        .spacing(space::S)
        .child(icon(t, Icon::Folder, iconsize::HERO, t.text3))
        .child(para(t, tr("Kein anderer Launcher gefunden. Wähle den Ordner eines Launchers oder einer Instanz."), font::BODY, t.text2).text_align(TextAlign::Center))
        .child(para(t, trf("Gesucht wird nach %@.", &[&names]), font::CAPTION, t.text3).text_align(TextAlign::Center))
        .into_element()
}

#[derive(PartialEq)]
struct PickFile {
    busy: bool,
    press: EventHandler<()>,
}

impl Component for PickFile {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let label = tr("Aus Datei importieren…");
        let busy = self.busy;
        let keyed = self.press.clone();
        let clicked = self.press.clone();
        let mut r = rect()
            .a11y_id(a11y)
            .a11y_focusable(!busy)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(label.clone())
            .width(Size::fill())
            .corner_radius(t.shape.tile)
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if busy || !a11y.is_focused() {
                    return;
                }
                if matches!(&e.key, Key::Named(NamedKey::Enter)) || matches!(&e.key, Key::Character(c) if c == " ") {
                    e.stop_propagation();
                    e.prevent_default();
                    keyed.call(());
                }
            });
        if let Some(border) = ring(&t, focus()) {
            r = r.border(border);
        }
        let pick = Line::new("import.file", label)
            .hint(tr("Modpack von Modrinth, Export oder Instanz aus Prism"))
            .control(icon(&t, Icon::ChevronRight, iconsize::S, t.text3))
            .dim(busy)
            .on_press(move |_| {
                a11y.request_focus();
                clicked.call(());
            });
        r.child(group(&t, None, vec![pick.into_element()], None))
    }
}

fn options(flow: Flow, t: &Theme) -> Element {
    let settings = *flow.settings.read();
    let worlds = *flow.worlds.read();
    let (mut s, mut w) = (flow.settings, flow.worlds);
    let (mut s_switch, mut w_switch) = (flow.settings, flow.worlds);
    let label = tr("Einstellungen und Ressourcenpakete");
    let title = match flow.worlds_size().filter(|_| worlds) {
        Some(n) => trf("Welten · %@", &[&bytes(n)]),
        None => tr("Welten"),
    };
    let mut worlds_line = Line::new("import.take.worlds", title).control(Switch::new("import.take.worlds.switch", tr("Welten"), worlds).on_toggle(move |on| w_switch.set(on))).on_press(move |_| {
        let on = !*w.peek();
        w.set(on);
    });
    if worlds {
        worlds_line = worlds_line.hint(tr("Welten kommen mit und erscheinen nach dem ersten Start unter Welten."));
    }
    group(
        t,
        Some(tr("Mitnehmen")),
        vec![
            Line::new("import.take.settings", label.clone())
                .control(Switch::new("import.take.settings.switch", label, settings).on_toggle(move |on| s_switch.set(on)))
                .on_press(move |_| {
                    let on = !*s.peek();
                    s.set(on);
                })
                .into_element(),
            worlds_line.into_element(),
        ],
        None,
    )
}

#[derive(PartialEq)]
struct Entry {
    id: String,
    own: AccessibilityId,
    before: Option<AccessibilityId>,
    after: Option<AccessibilityId>,
    title: String,
    detail: String,
    on: bool,
    reason: Option<String>,
    press: EventHandler<()>,
    reveal: EventHandler<()>,
}

impl Component for Entry {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let own = self.own;
        use_arrows(own);
        let focus = use_focus(own);
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let enabled = self.reason.is_none();
        let spot = use_spot(&self.id, "checkbox", &self.title, enabled);
        let keyed = focus() == Focus::Keyboard;
        let reveal = self.reveal.clone();
        use_side_effect_with_deps(&keyed, move |keyed| {
            if *keyed {
                reveal.call(());
            }
        });
        let (press, space_press) = (self.press.clone(), self.press.clone());
        let (before, after) = (self.before, self.after);
        let fill = if !enabled {
            Color::TRANSPARENT
        } else if *pressed.read() {
            t.press
        } else if self.on {
            t.selection
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let (ink, quiet) = if enabled { (t.text1, t.text2) } else { (t.text3, t.text2) };
        let mut r = rect()
            .a11y_id(own)
            .a11y_focusable(enabled)
            .a11y_role(AccessibilityRole::CheckBox)
            .a11y_alt(self.title.clone())
            .width(Size::fill())
            .height(Size::px(ROW_H))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .background(fill)
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed })
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |_| pressed.set(true))
            .on_mouse_up(move |_| pressed.set_if_modified(false));
        if enabled {
            r = r
                .on_all_press(move |e: Event<PressEventData>| {
                    match &*e {
                        PressEventData::Mouse(m) if m.button != Some(MouseButton::Left) => return,
                        PressEventData::Keyboard(_) => return,
                        _ => {}
                    }
                    own.request_focus();
                    press.call(());
                })
                .on_key_down(move |e: Event<KeyboardEventData>| {
                    if !own.is_focused() {
                        return;
                    }
                    let target = match &e.key {
                        Key::Named(NamedKey::ArrowDown) => Some(after),
                        Key::Named(NamedKey::ArrowUp) => Some(before),
                        Key::Character(c) if c == " " && e.modifiers.is_empty() => {
                            e.stop_propagation();
                            space_press.call(());
                            None
                        }
                        _ => None,
                    };
                    if let Some(target) = target {
                        e.stop_propagation();
                        if let Some(id) = target {
                            id.request_focus();
                        }
                    }
                });
        }
        if keyed {
            r = r.border(Border::new().fill(t.focus).width(frame::RING).alignment(BorderAlignment::Inner));
        }
        let lead = if enabled { checkbox(&t, self.on) } else { icon(&t, Icon::CircleAlert, iconsize::M, t.attention_text) };
        r.child(lead)
            .child(rect().width(Size::flex(1.)).spacing(space::XXS).child(line(&t, self.title.clone(), font::BODY, ink).font_weight(FontWeight::SEMI_BOLD)).child(line(&t, self.detail.clone(), font::CALLOUT, quiet)))
            .content(Content::flex())
    }
}

fn checkbox(t: &Theme, on: bool) -> Element {
    let mut b = rect()
        .width(Size::px(BOX))
        .height(Size::px(BOX))
        .center()
        .corner_radius(t.shape.small)
        .background(if on { t.accent } else { Color::TRANSPARENT })
        .border(Border::new().fill(if on { t.accent } else { t.text3 }).width(t.icon_stroke).alignment(BorderAlignment::Inner));
    if on {
        b = b.child(icon(t, Icon::Check, iconsize::XS, t.on_accent));
    }
    b.into_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instance::Loader;

    fn found(id: &str, name: &str, loader: Loader, mods: usize) -> Found {
        Found { launcher: Launcher::Prism, id: id.into(), name: name.into(), game_dir: format!("dir-{id}").into(), game_version: Some("1.21.1".into()), loader, mods, unsupported: None }
    }

    fn library(items: Vec<Found>) -> Library {
        Library { launcher: Launcher::Prism, root: "root".into(), instances: items }
    }

    #[test]
    fn the_subtitle_names_version_loader_and_mod_count() {
        assert_eq!(subtitle(&found("a", "A", Loader::Fabric(Some("0.16.5".into())), 12)), "Minecraft 1.21.1 · Fabric 0.16.5 · 12 Mods");
        assert_eq!(subtitle(&found("a", "A", Loader::Vanilla, 0)), "Minecraft 1.21.1");
        assert_eq!(subtitle(&found("a", "A", Loader::NeoForge(None), 1)), "Minecraft 1.21.1 · NeoForge · 1 Mod");
    }

    #[test]
    fn every_unsupported_reason_has_a_text() {
        for why in [Unsupported::NoVersion, Unsupported::Loader("liteloader".into()), Unsupported::Unreadable] {
            assert!(!reason(&why).is_empty());
        }
    }

    #[test]
    fn the_filter_keeps_matching_names_and_drops_empty_groups() {
        let libs = vec![library(vec![found("a", "Farm", Loader::Vanilla, 0), found("b", "Base", Loader::Vanilla, 0)])];
        assert_eq!(shown(&libs, "").iter().map(|s| s.items.len()).sum::<usize>(), 2);
        let some = shown(&libs, "far");
        assert_eq!(some.len(), 1);
        assert_eq!(some[0].items[0].1.name, "Farm");
        assert!(shown(&libs, "zzz").is_empty());
    }

    #[test]
    fn keys_skip_instances_that_cannot_be_imported() {
        let mut bad = found("b", "Bad", Loader::Vanilla, 0);
        bad.unsupported = Some(Unsupported::Unreadable);
        let libs = vec![library(vec![found("a", "A", Loader::Vanilla, 0), bad])];
        assert_eq!(keys(&shown(&libs, "")), vec!["prism:a".to_owned()]);
    }

    #[test]
    fn rows_know_where_they_sit_in_the_scrolled_list() {
        let libs = vec![library(vec![found("a", "A", Loader::Vanilla, 0), found("b", "B", Loader::Vanilla, 0)]), library(vec![found("c", "C", Loader::Vanilla, 0)])];
        let list = shown(&libs, "");
        assert_eq!(top_of(&list, "prism:a"), Some((0., HEAD_H + BLOCK_INNER + ROW_H)));
        assert_eq!(top_of(&list, "prism:b"), Some((HEAD_H + BLOCK_INNER + ROW_H + frame::HAIRLINE, ROW_H)));
        assert_eq!(top_of(&list, "prism:c").map(|(top, _)| top), Some(block_height(2, false) + BLOCK_GAP));
        assert_eq!(top_of(&list, "prism:none"), None);
    }

    #[test]
    fn one_profile_listed_twice_is_imported_once() {
        let libs = vec![library(vec![found("a", "A", Loader::Vanilla, 0), found("b", "B", Loader::Vanilla, 0)])];
        let mut known = HashMap::new();
        for i in &libs[0].instances {
            known.insert(profile_key(i), (PathBuf::from("same"), "x".to_owned()));
        }
        let chosen: HashSet<String> = ["prism:a", "prism:b"].map(String::from).into_iter().collect();
        assert_eq!(picked(&libs, &chosen, &known).len(), 1);
        assert_eq!(picked(&libs, &chosen, &HashMap::new()).len(), 2);
    }

    #[test]
    fn a_folder_scan_adds_only_profiles_not_listed_yet() {
        let mut libs = vec![library(vec![found("a", "A", Loader::Vanilla, 0)])];
        let mut known = HashMap::new();
        known.insert(profile_key(&libs[0].instances[0]), (PathBuf::from("one"), "x".to_owned()));
        let again = library(vec![found("a", "A", Loader::Vanilla, 0)]);
        let mut scanned = HashMap::new();
        scanned.insert(profile_key(&again.instances[0]), (PathBuf::from("one"), "x".to_owned()));
        let (added, fresh) = merge(&mut libs, &mut known, vec![again], scanned);
        assert!(!added && fresh.is_empty());
        let other = library(vec![found("n", "N", Loader::Vanilla, 0)]);
        let mut scanned = HashMap::new();
        scanned.insert(profile_key(&other.instances[0]), (PathBuf::from("two"), "x".to_owned()));
        let (added, fresh) = merge(&mut libs, &mut known, vec![other], scanned);
        assert!(added);
        assert_eq!(fresh, vec!["prism:n".to_owned()]);
        assert_eq!(libs[0].instances.len(), 2);
    }
}
