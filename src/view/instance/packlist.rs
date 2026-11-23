use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use freya::prelude::*;

use super::entry::{EntryRow, RowAction, bar};
use super::shots_parts::centered;
use super::{Facts, actions};
use crate::core::i18n::{tr, trf};
use crate::core::batch::ModRef;
use crate::core::modrinth::{Filter, Kind};
use crate::core::packs;
use crate::core::trash;
use crate::ui::catalog;
use crate::ui::forms;
use crate::ui::instance_files::bytes;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::{caption, secondary};
use crate::view::kit::{Bounds, off_ui, open_external, toast, use_theme};
use crate::view::native;
use crate::view::sheets::browser;
use crate::view::store::{Store, use_store};
use crate::view::theme::{control, space};

const FRESH: Duration = Duration::from_secs(600);

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
    let updates = if current.is_empty() { Vec::new() } else { catalog::updates(filter, &current)? };
    Ok(installed
        .into_iter()
        .map(|i| {
            let update = updates.iter().any(|u| u.current.project_id == i.found.project_id);
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
            packs::remove(&from, &folder, &name)
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
        let mut known = use_state(|| Matches::default());
        let mut more_bounds = use_state(Bounds::default);
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
        let looking = dir.clone();
        use_side_effect_with_deps(&(signature, filter.clone()), move |(signature, filter)| {
            if signature.is_empty() {
                return;
            }
            if let Some(found) = looked(signature) {
                known.set(found);
                return;
            }
            let (signature, filter, dir) = (signature.clone(), filter.clone(), looking.clone());
            spawn(async move {
                if let Some(Ok(found)) = off_ui(move || lookup(&dir, &filter)).await {
                    let found = Matches::new(found);
                    remember(signature, found.clone());
                    if known.try_peek().is_some() {
                        known.set(found);
                    }
                }
            });
        });
        let entries = match &*listing.read() {
            Listing::Loading => return rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Lädt …"))).into_element(),
            Listing::Ready(entries) => entries.clone(),
        };
        let add_id = id.clone();
        let hint = (kind == Kind::Shader && shader_loader.is_none()).then(|| packs::shader_mod(&loader).map(|(_, name)| name)).flatten();
        if entries.is_empty() && hint.is_none() {
            let (text, body) = match kind {
                Kind::Shader => (tr("Noch keine Shader"), tr("Shader ändern das Licht und die Schatten im Spiel.")),
                _ => (tr("Noch keine Ressourcenpakete"), tr("Ressourcenpakete ändern, wie das Spiel aussieht und klingt.")),
            };
            let add = Button::new(format!("instance.{key}.empty.add"), tr("Hinzufügen…")).glyph(Icon::Plus).primary().on_press(move |_| browser::find(store, &add_id, kind));
            return centered(&t, if kind == Kind::Shader { Icon::Sparkles } else { Icon::Palette }, text, body, Some(add));
        }

        let text = query.read().clone();
        let needle = text.trim().to_lowercase();
        let shown: Vec<&Entry> = entries.iter().filter(|e| needle.is_empty() || shown_name(&e.file).to_lowercase().contains(&needle)).collect();
        let matches = known.read().clone();

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
        let mut top = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .content(Content::flex())
            .child(rect().width(Size::flex(1.)).child(Field::new(format!("instance.{key}.filter"), query).placeholder(placeholder).glyph(Icon::Search)))
            .child(more);
        if !entries.is_empty() {
            top = top.child(Button::new(format!("instance.{key}.add"), tr("Hinzufügen…")).glyph(Icon::Plus).primary().on_press(move |_| browser::find(store, &add_id, kind)));
        }

        let count = if needle.is_empty() { trf("%lld installiert", &[&entries.len()]) } else { trf("%lld von %lld", &[&shown.len(), &entries.len()]) };
        let counts = rect().width(Size::fill()).height(Size::px(control::S)).horizontal().cross_align(Alignment::Center).child(caption(&t, count).max_lines(1));

        let rows = shown.iter().map(|e| {
            let (file, on) = (e.file.clone(), e.on);
            let name = shown_name(&file);
            let lower = file.to_lowercase();
            let pack = matches.get(&file).cloned();
            let (toggle_dir, toggle_loader, toggle_file) = (instance_dir.clone(), loader.clone(), file.clone());
            let remover = {
                let (dir, pack_dir, loader, file) = (instance_dir.clone(), dir.clone(), loader.clone(), file.clone());
                move || remove(store, dir.clone(), pack_dir.clone(), kind, loader.clone(), file.clone(), on)
            };
            let mut actions = Vec::new();
            if let Some(p) = &pack {
                let (open_id, project, title) = (id.clone(), p.project.clone(), p.title.clone());
                actions.push(RowAction { id: "info", icon: Icon::Info, label: tr("Readme und Details"), danger: false, disabled: None, run: EventHandler::new(move |_| browser::open_project(store, &open_id, kind, &project, &title)) });
            }
            let gone = remover.clone();
            actions.push(RowAction { id: "remove", icon: Icon::Trash, label: tr("Entfernen"), danger: true, disabled: None, run: EventHandler::new(move |_| gone()) });
            let menu_pack = pack.clone();
            let menu_id = id.clone();
            let menu_remove = remover.clone();
            let on_menu = EventHandler::new(move |at: (f32, f32)| {
                let mut items = Vec::new();
                if let Some(p) = &menu_pack {
                    let (open_id, project, title) = (menu_id.clone(), p.project.clone(), p.title.clone());
                    let web = p.project.clone();
                    items.push(Item::new(format!("instance.{key}.menu.project"), tr("Readme und Versionen"), move |_| browser::open_project(store, &open_id, kind, &project, &title)));
                    items.push(Item::new(format!("instance.{key}.menu.web"), tr("Auf Modrinth öffnen"), move |_| open_external("modrinth", format!("https://modrinth.com/{}/{web}", kind.project_type()))));
                    items.push(Item::Separator);
                }
                let remove = menu_remove.clone();
                items.push(Item::new(format!("instance.{key}.menu.remove"), tr("Entfernen"), move |_| remove()).danger());
                menu::open(ui, Menu::at(items, at));
            });
            rect()
                .key(&file)
                .width(Size::fill())
                .child(EntryRow {
                    area: key,
                    key: lower,
                    tip: file.clone(),
                    name,
                    caption: if e.folder { tr("Ordner") } else { bytes(e.size) },
                    warn: false,
                    enabled: on,
                    icon: e.icon.clone(),
                    glyph: if e.folder { Icon::Folder } else if kind == Kind::Shader { Icon::Sparkles } else { Icon::Palette },
                    selected: false,
                    cursor: false,
                    locked: None,
                    update: pack.as_ref().is_some_and(|p| p.update).then(String::new),
                    actions,
                    trail: None,
                    on_toggle: EventHandler::new(move |next| switch(store, toggle_dir.clone(), kind, toggle_loader.clone(), toggle_file.clone(), next)),
                    on_press: None,
                    on_menu: Some(on_menu),
                })
                .into_element()
        });

        let body: Element = if shown.is_empty() {
            let mut clear = query;
            let button = Button::new(format!("instance.{key}.empty.clear"), tr("Filter löschen")).on_press(move |_| clear.set(String::new()));
            centered(&t, Icon::Search, trf("Nichts passt zu „%@“.", &[&text.trim()]), String::new(), Some(button))
        } else {
            ScrollView::new().width(Size::fill()).height(Size::flex(1.)).child(rect().width(Size::fill()).children(rows)).into_element()
        };

        let mut root = rect().width(Size::fill()).height(Size::fill()).padding(space::L).spacing(space::S).child(top);
        if let Some(name) = hint {
            let find_id = id.clone();
            root = root.child(bar(&t, Icon::Alert, t.attention_text, trf("Shader brauchen %@. Ohne laden sie nicht.", &[&name])).child(Button::new(format!("instance.{key}.loader"), trf("%@ hinzufügen…", &[&name])).on_press(move |_| browser::find(store, &find_id, Kind::Mod))));
        }
        root.child(counts).child(body).content(Content::flex()).into_element()
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