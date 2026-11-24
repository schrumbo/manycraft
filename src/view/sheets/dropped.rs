//! A file dropped on the window: asks whether to import it as an instance.

use std::path::{Path, PathBuf};

use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::core::mrpack;
use crate::view::icons::Icon;
use crate::view::install::{self, Source};
use crate::view::kit::group::status;
use crate::view::kit::row::Row;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::{off_ui, use_theme};
use crate::view::readme;
use crate::view::store::{Store, use_store};
use crate::view::theme::space;

/// One dropped file as the question shows it: the instance it would become, or why it cannot.
#[derive(Clone, PartialEq)]
struct Found {
    path: PathBuf,
    verdict: Result<String, String>,
}

fn candidate(path: &Path) -> bool {
    path.is_dir() || path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mrpack") || e.eq_ignore_ascii_case("zip"))
}

fn label(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

fn read(path: PathBuf) -> Found {
    let verdict = if candidate(&path) {
        mrpack::peek(&path).map_err(|e| install::explain(&label(&path), &e))
    } else {
        Err(trf("„%@“ ist keine .mrpack- oder .zip-Datei.", &[&label(&path)]))
    };
    Found { path, verdict }
}

/// Asks whether to import `paths`, unless a sheet, the palette or a menu has the window's attention.
pub fn ask(store: Store, paths: Vec<PathBuf>) {
    if paths.is_empty() || store.ui.layered() {
        return;
    }
    sheet::open(store.ui, Ask { paths });
}

#[derive(PartialEq)]
struct Ask {
    paths: Vec<PathBuf>,
}

impl Component for Ask {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut found = use_state(|| None::<Vec<Found>>);
        let paths = self.paths.clone();
        use_hook(move || {
            spawn_in_window(async move {
                let read = off_ui(move || paths.into_iter().map(read).collect::<Vec<_>>()).await.unwrap_or_default();
                if let Some(mut slot) = found.try_write() {
                    *slot = Some(read);
                }
            });
        });
        let close = move |_| sheet::close(store.ui);
        let Some(list) = found.read().clone() else {
            let reading = Primary { label: tr("Importieren"), disabled: Some(tr("Wird gelesen …")), danger: false, busy: false, run: EventHandler::new(|_| {}) };
            return Card::new("dropped", tr("Als Instanz importieren?")).body(rect().width(Size::fill()).child(readme::loading(&t))).primary(reading).primary_id("dropped.import").into_element();
        };
        let good: Vec<&Found> = list.iter().filter(|f| f.verdict.is_ok()).collect();
        let rows = list.iter().enumerate().map(|(i, f)| match &f.verdict {
            Ok(name) => Row::new(format!("dropped.row.{i}"), name.clone()).glyph(Icon::Package).detail(label(&f.path)).into_element(),
            Err(why) => rect().width(Size::fill()).padding((space::XS, space::S)).child(status(&t, why.clone(), t.danger_text)).into_element(),
        });
        let body = rect().width(Size::fill()).spacing(space::XXS).children(rows);
        if good.is_empty() {
            return Card::new("dropped", tr("Das lässt sich nicht importieren"))
                .message(tr("Ziehe eine .mrpack- oder .zip-Datei oder einen Instanz-Ordner auf das Fenster."))
                .body(body)
                .primary(Primary { label: tr("OK"), disabled: None, danger: false, busy: false, run: EventHandler::new(close) })
                .primary_id("dropped.ok")
                .inform()
                .into_element();
        }
        let sources: Vec<PathBuf> = good.iter().map(|f| f.path.clone()).collect();
        let label = if sources.len() == 1 { tr("Importieren") } else { trf("%lld importieren", &[&sources.len()]) };
        let run = EventHandler::new(move |_| {
            sheet::close(store.ui);
            install::start(store, sources.iter().cloned().map(Source::File).collect());
        });
        Card::new("dropped", tr("Als Instanz importieren?"))
            .message(tr("Die Datei wird als neue Instanz kopiert, die Datei selbst bleibt unverändert."))
            .body(body)
            .primary(Primary { label, disabled: None, danger: false, busy: false, run })
            .primary_id("dropped.import")
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_packs_zips_and_folders_are_candidates() {
        assert!(candidate(Path::new("Farm.mrpack")) && candidate(Path::new("x.ZIP")) && candidate(&std::env::temp_dir()));
        assert!(!candidate(Path::new("shot.png")) && !candidate(Path::new("mod.jar")) && !candidate(Path::new("noext")));
    }

    #[test]
    fn a_file_that_is_no_pack_says_why() {
        let found = read(PathBuf::from("shot.png"));
        assert!(found.verdict.as_ref().is_err_and(|why| why.contains("shot.png")));
    }
}
