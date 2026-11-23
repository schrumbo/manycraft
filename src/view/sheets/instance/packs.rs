//! The Modpack tab of the new-instance sheet: search Modrinth and pick a modpack to install as a new instance.

use std::collections::HashSet;
use std::time::Duration;

use freya::prelude::*;

use super::{Origin, WIDE, body_max, origins, problem_view};
use crate::core::i18n::{tr, trf};
use crate::core::modrinth::{Filter, Hit, Kind, Page, Search, Sort};
use crate::ui::catalog;
use crate::ui::forms::{self, Problem};
use crate::view::icons::Icon;
use crate::view::install::{self, Source};
use crate::view::kit::button::Button;
use crate::view::kit::face::Face;
use crate::view::kit::field::Field;
use crate::view::kit::group::group;
use crate::view::kit::row::Row;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::para;
use crate::view::kit::{after, off_ui, toast, use_theme};
use crate::view::readme;
use crate::view::store::{Store, use_store};
use crate::view::theme::{control, font, frame, space};

const SEARCH_DELAY: Duration = Duration::from_millis(250);
const ROW_H: f32 = control::L + space::S + space::XS;
const ABOVE_LIST: f32 = control::L + space::M * 2.;

#[derive(Clone)]
enum Status {
    Loading,
    Ready,
    Failed(Problem),
}

fn search_of(query: &str) -> Search {
    let query = query.trim().to_owned();
    let sort = if query.is_empty() { Sort::Downloads } else { Sort::Relevance };
    Search { filter: Filter::any(Kind::Modpack), query, sort, category: None, all: false }
}

fn detail(hit: &Hit) -> String {
    let downloads = trf("%@ Downloads", &[&forms::downloads_short(hit.downloads)]);
    match (hit.author.is_empty(), hit.description.is_empty()) {
        (true, true) => downloads,
        (true, false) => format!("{downloads} · {}", hit.description),
        (false, true) => format!("{} · {downloads}", hit.author),
        (false, false) => format!("{} · {downloads} · {}", hit.author, hit.description),
    }
}

fn source_of(hit: &Hit) -> Source {
    Source::Pack { project: hit.project_id.clone(), title: hit.title.clone(), icon: hit.icon_url.clone(), filter: Filter::any(Kind::Modpack) }
}

#[derive(Clone, Copy)]
struct Find {
    store: Store,
    query: State<String>,
    hits: State<Vec<Hit>>,
    total: State<usize>,
    status: State<Status>,
    more: State<bool>,
    generation: State<u64>,
    chosen: State<Option<String>>,
}

impl Find {
    fn alive(self) -> bool {
        self.query.try_peek().is_some()
    }

    fn run(mut self) {
        let generation = *self.generation.peek() + 1;
        self.generation.set(generation);
        let query = self.query.peek().clone();
        let typed = !query.trim().is_empty();
        if !typed || matches!(*self.status.peek(), Status::Failed(_)) {
            self.status.set(Status::Loading);
        }
        spawn_in_window(async move {
            if typed {
                after(SEARCH_DELAY).await;
                if !self.alive() || *self.generation.peek() != generation {
                    return;
                }
                self.status.set(Status::Loading);
            }
            let search = search_of(&query);
            let result = off_ui(move || catalog::search(&search, 0)).await;
            if !self.alive() || *self.generation.peek() != generation {
                return;
            }
            self.land(result.unwrap_or_else(|| Err(anyhow::anyhow!("the search thread ended early"))), false);
        });
    }

    fn load_more(mut self) {
        if *self.more.peek() || self.hits.peek().len() >= *self.total.peek() {
            return;
        }
        self.more.set(true);
        let generation = *self.generation.peek();
        let offset = self.hits.peek().len();
        let search = search_of(&self.query.peek());
        spawn_in_window(async move {
            let result = off_ui(move || catalog::search(&search, offset)).await;
            if !self.alive() {
                return;
            }
            self.more.set(false);
            if *self.generation.peek() != generation {
                return;
            }
            match result {
                Some(Ok(page)) => self.land(Ok(page), true),
                _ => {
                    toast::show(self.store.ui, tr("Weitere Ergebnisse ließen sich nicht laden."), None);
                }
            }
        });
    }

    fn land(mut self, result: anyhow::Result<Page>, append: bool) {
        match result {
            Ok(page) => {
                if append {
                    let mut known: HashSet<String> = self.hits.peek().iter().map(|h| h.project_id.clone()).collect();
                    let fresh: Vec<Hit> = page.hits.into_iter().filter(|h| known.insert(h.project_id.clone())).collect();
                    self.hits.write().extend(fresh);
                } else {
                    let kept = self.chosen.peek().as_ref().is_some_and(|id| page.hits.iter().any(|h| h.project_id == *id));
                    if !kept {
                        self.chosen.set(None);
                    }
                    self.hits.set(page.hits);
                }
                self.total.set(page.total);
                self.status.set(Status::Ready);
            }
            Err(e) => self.status.set(Status::Failed(forms::explain_as(tr("Modpacks nicht geladen."), &e))),
        }
    }

    fn pick(mut self, id: &str) {
        self.chosen.set(Some(id.to_owned()));
    }

    fn picked(self) -> Option<Hit> {
        let id = self.chosen.peek().clone()?;
        self.hits.peek().iter().find(|h| h.project_id == id).cloned()
    }

    fn install(self, store: Store) {
        let Some(hit) = self.picked() else { return };
        sheet::close(store.ui);
        install::start(store, vec![source_of(&hit)]);
    }
}

/// The Modpack view of the new-instance sheet.
#[derive(PartialEq)]
pub(super) struct Packs {
    pub origin: State<Origin>,
}

impl Component for Packs {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let find = Find {
            store,
            query: use_state(String::new),
            hits: use_state(Vec::new),
            total: use_state(|| 0),
            status: use_state(|| Status::Loading),
            more: use_state(|| false),
            generation: use_state(|| 0),
            chosen: use_state(|| None),
        };
        let details = use_state(HashSet::new);
        let typed = find.query.read().clone();
        use_side_effect_with_deps(&typed, move |_| find.run());
        let back = *store.back.read();
        use_side_effect_with_deps(&back, move |_| {
            if matches!(*find.status.peek(), Status::Failed(_)) {
                find.run();
            }
        });

        let hits = find.hits.read().clone();
        let chosen = find.chosen.read().clone();
        let status = find.status.read().clone();
        let search = Field::new("packs.search", find.query).placeholder(tr("Modpack suchen, z. B. Fabulously Optimized")).glyph(Icon::Search).auto_focus().width(Size::fill());

        let results = match &status {
            Status::Failed(problem) => rect()
                .width(Size::fill())
                .spacing(space::S)
                .child(problem_view(&t, "packs.problem", Icon::CircleAlert, problem, details))
                .child(rect().horizontal().child(Button::new("packs.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| find.run())))
                .into_element(),
            Status::Loading if hits.is_empty() => rect().width(Size::fill()).padding((space::L, 0.)).child(readme::loading(&t)).into_element(),
            Status::Ready if hits.is_empty() => {
                let query = find.query.read().trim().to_owned();
                let text = if query.is_empty() { tr("Modrinth hat keine Modpacks geliefert.") } else { trf("Kein Modpack passt zu „%@“. Suche anders.", &[&query]) };
                rect().width(Size::fill()).padding((space::L, 0.)).center().child(para(&t, text, font::CALLOUT, t.text2).text_align(TextAlign::Center)).into_element()
            }
            _ => {
                let rows: Vec<Element> = hits
                    .iter()
                    .map(|hit| {
                        let id = hit.project_id.clone();
                        Row::new(format!("packs.row.{}", hit.slug), hit.title.clone())
                            .detail(detail(hit))
                            .leading(Face { pid: hit.project_id.clone(), url: hit.icon_url.clone() })
                            .selected(chosen.as_deref() == Some(hit.project_id.as_str()))
                            .height(ROW_H)
                            .on_press(move |_| find.pick(&id))
                            .into_element()
                    })
                    .collect();
                let mut column = rect().width(Size::fill()).spacing(space::S).child(group(&t, None, rows, None));
                if hits.len() < *find.total.read() {
                    column = column.child(rect().width(Size::fill()).horizontal().main_align(Alignment::Center).child(Button::new("packs.more", tr("Mehr laden")).busy(*find.more.read()).on_press(move |_| find.load_more())));
                }
                ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px(body_max(store) - ABOVE_LIST)).child(rect().width(Size::fill()).padding(frame::RING).child(column)).into_element()
            }
        };

        let body = rect().width(Size::fill()).spacing(space::M).child(origins(self.origin, None)).child(search).child(results);
        let reason = find.picked().is_none().then(|| tr("Wähle ein Modpack."));
        Card::new("packs", tr("Instanz erstellen"))
            .width(WIDE)
            .body(body)
            .primary(Primary { label: tr("Modpack installieren"), disabled: reason, danger: false, busy: false, run: EventHandler::new(move |_| find.install(store)) })
            .primary_id("packs.install")
    }
}

#[cfg(test)]
mod tests {
    use super::{detail, search_of, source_of};
    use crate::core::modrinth::{Hit, Kind, Sort};
    use crate::view::install::Source;

    fn hit(author: &str, description: &str) -> Hit {
        Hit { title: "Pack".into(), slug: "pack".into(), project_id: "P1".into(), description: description.into(), downloads: 1200, icon_url: Some("u".into()), author: author.into(), versions: Vec::new(), categories: Vec::new() }
    }

    #[test]
    fn an_empty_search_lists_the_most_downloaded_and_a_typed_one_the_most_relevant() {
        assert_eq!(search_of("  ").sort, Sort::Downloads);
        let typed = search_of(" fabulous ");
        assert_eq!((typed.sort, typed.query.as_str()), (Sort::Relevance, "fabulous"));
        assert_eq!(typed.filter.kind, Kind::Modpack);
        assert!(typed.filter.game_version.is_none() && typed.filter.loaders.is_empty());
    }

    #[test]
    fn the_detail_line_skips_what_a_pack_does_not_have() {
        crate::core::i18n::init("de");
        assert!(detail(&hit("", "")).starts_with("1,2"));
        assert!(detail(&hit("Ann", "")).starts_with("Ann · "));
        assert!(detail(&hit("", "Fast")).ends_with(" · Fast"));
        let both = detail(&hit("Ann", "Fast"));
        assert!(both.starts_with("Ann · ") && both.ends_with(" · Fast"));
    }

    #[test]
    fn installing_a_pick_takes_its_newest_version_for_any_game_version() {
        match source_of(&hit("Ann", "Fast")) {
            Source::Pack { project, title, icon, filter } => {
                assert_eq!((project.as_str(), title.as_str(), icon.as_deref()), ("P1", "Pack", Some("u")));
                assert!(filter.game_version.is_none());
            }
            _ => panic!("not a pack"),
        }
    }
}