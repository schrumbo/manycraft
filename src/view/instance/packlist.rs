use std::path::{Path, PathBuf};

use freya::prelude::*;

use super::shots_parts::centered;
use super::{Facts, actions};
use crate::core::i18n::{tr, trf};
use crate::core::modrinth::Kind;
use crate::core::packs;
use crate::core::trash;
use crate::ui::forms;
use crate::ui::instance_files::bytes;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::row::Row;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::{caption, hairline, heading, secondary};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::native;
use crate::view::sheets::browser;
use crate::view::store::{Store, use_store};
use crate::view::theme::{frame, space};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    file: String,
    folder: bool,
    size: u64,
    on: bool,
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
            is_pack(&file, folder).then(|| Entry { on: on.contains(&file), size: if folder { 0 } else { meta.len() }, file, folder })
        })
        .collect();
    found.sort_by(|a, b| b.on.cmp(&a.on).then_with(|| a.file.to_lowercase().cmp(&b.file.to_lowercase())));
    found
}

fn title(kind: Kind) -> String {
    match kind {
        Kind::Shader => tr("Shader"),
        _ => tr("Ressourcenpakete"),
    }
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

impl Component for Packs {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let (kind, f) = (self.kind, &self.facts);
        let made = &f.made;
        let (id, instance_dir, loader) = (made.id.clone(), made.dir(), made.loader.clone());
        let dir = packs::pack_dir(&instance_dir, kind, None).unwrap_or_else(|| instance_dir.clone());
        let changed = *store.ui.instance.changed.read();
        let mut listing = use_state(|| Listing::Loading);
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
        let entries = match &*listing.read() {
            Listing::Loading => return rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Lädt …"))).into_element(),
            Listing::Ready(entries) => entries.clone(),
        };
        let add_id = id.clone();
        let hint = (kind == Kind::Shader && packs::shader_loader(&made.mods, &made.local_mods).is_none()).then(|| packs::shader_mod(&loader).map(|(_, name)| name)).flatten();
        if entries.is_empty() && hint.is_none() {
            let (text, body) = match kind {
                Kind::Shader => (tr("Noch keine Shader"), tr("Shader ändern das Licht und die Schatten im Spiel.")),
                _ => (tr("Noch keine Ressourcenpakete"), tr("Ressourcenpakete ändern, wie das Spiel aussieht und klingt.")),
            };
            let add = Button::new(format!("instance.{key}.empty.add"), tr("Hinzufügen…")).glyph(Icon::Plus).primary().on_press(move |_| browser::find(store, &add_id, kind));
            return centered(&t, if kind == Kind::Shader { Icon::Sparkles } else { Icon::Palette }, text, body, Some(add));
        }
        let mut head = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .padding((space::M, space::L))
            .child(rect().width(Size::flex(1.)).child(heading(&t, title(kind))).child(caption(&t, trf("%lld installiert", &[&entries.len()]))))
            .content(Content::flex());
        let reveal_dir = dir.clone();
        head = head.child(Button::new(format!("instance.{key}.folder"), tr("Ordner öffnen")).glyph(Icon::Folder).on_press(move |_| {
            let _ = std::fs::create_dir_all(&reveal_dir);
            native::open(&reveal_dir);
        }));
        head = head.child(Button::new(format!("instance.{key}.add"), tr("Hinzufügen…")).glyph(Icon::Plus).primary().on_press(move |_| browser::find(store, &add_id, kind)));
        let mut column = rect().width(Size::fill()).child(head);
        if let Some(name) = hint {
            let find_id = id.clone();
            column = column.child(
                rect()
                    .width(Size::fill())
                    .padding((space::S, space::L))
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .child(rect().width(Size::flex(1.)).child(secondary(&t, trf("Shader brauchen %@. Ohne laden sie nicht.", &[&name]))))
                    .child(Button::new(format!("instance.{key}.loader"), trf("%@ hinzufügen…", &[&name])).on_press(move |_| browser::find(store, &find_id, Kind::Mod)))
                    .content(Content::flex()),
            );
        }
        column = column.child(hairline(&t));
        let rows = entries.iter().map(|e| {
            let (file, on) = (e.file.clone(), e.on);
            let (toggle_dir, toggle_loader, toggle_file) = (instance_dir.clone(), loader.clone(), file.clone());
            let (gone_dir, gone_pack, gone_loader, gone_file) = (instance_dir.clone(), dir.clone(), loader.clone(), file.clone());
            let name = shown_name(&file);
            let detail = if e.folder { tr("Ordner") } else { bytes(e.size) };
            let trailing = rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::S)
                .child(Switch::new(format!("instance.{key}.on.{file}"), trf("%@ einschalten", &[&name]), on).on_toggle(move |next| switch(store, toggle_dir.clone(), kind, toggle_loader.clone(), toggle_file.clone(), next)))
                .child(Button::new(format!("instance.{key}.remove.{file}"), tr("Entfernen")).plain().on_press(move |_| remove(store, gone_dir.clone(), gone_pack.clone(), kind, gone_loader.clone(), gone_file.clone(), on)));
            rect().key(&file).width(Size::fill()).child(Row::new(format!("instance.{key}.row.{file}"), name).detail(detail).glyph(if e.folder { Icon::Folder } else { Icon::Package }).trailing(trailing)).into_element()
        });
        let list = ScrollView::new().width(Size::fill()).height(Size::fill()).child(rect().width(Size::fill()).max_width(Size::px(frame::CONTENT_MAX)).padding((space::S, space::M)).spacing(space::XXS).children(rows));
        column.child(list).into_element()
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
