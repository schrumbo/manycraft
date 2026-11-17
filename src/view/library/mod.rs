//! The Instanzen library: every instance as a card or a row, in groups the user names, found by typing, ordered as asked.

mod card;
mod groups;
mod info;
mod recent;
mod rows;

use std::collections::BTreeSet;

use freya::prelude::*;

pub use self::card::Wall;
pub use self::groups::assign;
pub use self::info::{Info, Infos, ago, use_infos};
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Instance, Loader};
use crate::view::commands::{self, Command, Want};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::text::{caption, heading, line, secondary};
use crate::view::kit::{Bounds, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, font, frame, iconsize, space};
use crate::view::ui::Page;

/// The group of the instance the app keeps for Schnell spielen.
pub const QUICK: &str = "Schnellstart";

/// The key of the group of deleted instances at the end of the library.
pub const RECENT: &str = "\u{1}recent";

/// How the library orders instances inside a group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    Played,
    Made,
    Version,
    Loader,
}

impl Sort {
    pub const ALL: [Sort; 5] = [Sort::Name, Sort::Played, Sort::Made, Sort::Version, Sort::Loader];

    pub fn label(self) -> String {
        match self {
            Sort::Name => tr("Name"),
            Sort::Played => tr("Zuletzt gespielt"),
            Sort::Made => tr("Erstellt"),
            Sort::Version => tr("Version"),
            Sort::Loader => tr("Loader"),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Played => "played",
            Sort::Made => "made",
            Sort::Version => "version",
            Sort::Loader => "loader",
        }
    }
}

/// Cards or rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Grid,
    List,
}

/// What the library page keeps while it is not shown: the filter, the order, the layout and which groups are folded.
#[derive(Clone, Copy, PartialEq)]
pub struct Shared {
    pub query: State<String>,
    pub sort: State<Sort>,
    pub grouped: State<bool>,
    pub mode: State<Mode>,
    pub folded: State<BTreeSet<String>>,
    /// Set to scroll the library to its end, where the deleted instances are.
    pub reveal: State<bool>,
}

impl Shared {
    pub fn create() -> Self {
        Self {
            query: State::create(String::new()),
            sort: State::create(Sort::Name),
            grouped: State::create(true),
            mode: State::create(Mode::Grid),
            folded: State::create(BTreeSet::from([RECENT.to_owned(), QUICK.to_owned()])),
            reveal: State::create(false),
        }
    }
}

/// A run of instances under one header.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub key: String,
    pub title: String,
    pub ids: Vec<String>,
}

fn number_keys(version: &str) -> Vec<u64> {
    version.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect()
}

fn group_of(i: &Instance) -> String {
    i.group.clone().filter(|g| !g.trim().is_empty()).unwrap_or_default()
}

/// The instances of `list` that match every word of `query` in name, version, loader or group.
pub fn found<'a>(list: &'a [Instance], query: &str) -> Vec<&'a Instance> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    list.iter()
        .filter(|i| {
            let hay = format!("{} {} {} {}", i.name, i.game_version, i.loader.name(), group_of(i)).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect()
}

/// `items` in the order `sort` asks for; ties fall back to the name.
pub fn ordered<'a>(mut items: Vec<&'a Instance>, sort: Sort, infos: &Infos) -> Vec<&'a Instance> {
    let played = |i: &Instance| infos.get(&i.id).and_then(|n| n.played);
    let made = |i: &Instance| infos.get(&i.id).and_then(|n| n.made);
    items.sort_by(|a, b| {
        let first = match sort {
            Sort::Name => std::cmp::Ordering::Equal,
            Sort::Played => played(b).cmp(&played(a)),
            Sort::Made => made(b).cmp(&made(a)),
            Sort::Version => number_keys(&b.game_version).cmp(&number_keys(&a.game_version)),
            Sort::Loader => a.loader.name().cmp(b.loader.name()),
        };
        first.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.id.cmp(&b.id))
    });
    items
}

/// The title a group shows.
pub fn title_of(key: &str) -> String {
    match key {
        "" => tr("Ohne Gruppe"),
        QUICK => tr("Schnellstart"),
        RECENT => tr("Zuletzt gelöscht"),
        other => other.to_owned(),
    }
}

/// `items` under their group headers: named groups alphabetically, then those without one, then Schnellstart. One
/// headerless run while grouping is off or no instance has a group.
pub fn sections(items: &[&Instance], grouped: bool) -> Vec<Section> {
    if !grouped || items.iter().all(|i| group_of(i).is_empty()) {
        return vec![Section { key: String::new(), title: String::new(), ids: items.iter().map(|i| i.id.clone()).collect() }];
    }
    let mut names: Vec<String> = items.iter().map(|i| group_of(i)).collect::<BTreeSet<_>>().into_iter().collect();
    let rank = |k: &str| match k {
        "" => 1,
        QUICK => 2,
        _ => 0,
    };
    names.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.to_lowercase().cmp(&b.to_lowercase())));
    names.into_iter().map(|k| Section { title: title_of(&k), ids: items.iter().filter(|i| group_of(i) == k).map(|i| i.id.clone()).collect(), key: k }).collect()
}

/// The names of the groups instances are in now, alphabetically, without the group of Schnellstart.
pub fn group_names(list: &[Instance]) -> Vec<String> {
    let mut names: Vec<String> = list.iter().map(group_of).filter(|g| !g.is_empty() && g != QUICK).collect::<BTreeSet<_>>().into_iter().collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// What the library shows of one instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: String,
    pub name: String,
    pub picture: Option<std::path::PathBuf>,
    pub what: String,
    pub game_version: String,
    pub loader: String,
    pub group: String,
    pub played: Option<std::time::SystemTime>,
    pub running: Option<Vec<String>>,
    pub loading: Option<u8>,
    pub trouble: bool,
}

/// "Fabric · 1.21.4", or the version alone for the plain game.
pub fn what(i: &Instance) -> String {
    if i.loader == Loader::Vanilla { i.game_version.clone() } else { format!("{} · {}", i.loader.name(), i.game_version) }
}

pub fn lines(store: Store, list: &[&Instance], infos: &Infos) -> Vec<Line> {
    let m = store.model.read();
    list.iter()
        .map(|i| {
            let keys: Vec<String> = m.solo_games(&i.id).filter(|g| g.phase.is_active()).map(|g| g.key.clone()).collect();
            Line {
                id: i.id.clone(),
                name: i.name.clone(),
                picture: i.icon_path(),
                what: what(i),
                game_version: i.game_version.clone(),
                loader: i.loader.name().to_owned(),
                group: group_of(i),
                played: infos.get(&i.id).and_then(|n| n.played),
                running: (!keys.is_empty()).then_some(keys),
                loading: crate::view::instance::loading(&m, &i.id),
                trouble: m.solo_games(&i.id).any(|g| g.phase.needs_attention()),
            }
        })
        .collect()
}

/// The library's header tools: the filter, the order, grouping, the layout, the more menu and Neue Instanz.
#[derive(PartialEq)]
pub struct Tools {
    pub room: f32,
}

impl Component for Tools {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let shared = store.ui.library;
        let sort = shared.sort;
        let mut grouped = shared.grouped;
        let mut mode = shared.mode;
        let mut sort_at = use_state(Bounds::default);
        let mut more_at = use_state(Bounds::default);
        let search_a11y = use_a11y();
        let focus_at = *store.ui.focus_search.read();
        use_side_effect_with_deps(&focus_at, move |at| {
            if at.is_some() {
                search_a11y.request_focus();
            }
        });
        let any = !store.model.read().instances.is_empty();
        let compact = self.room < frame::TOOLBAR_FULL;
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        if any {
            row = row.child(Field::new("library.filter", shared.query).glyph(Icon::Search).placeholder(tr("Instanzen filtern")).width(Size::px(if compact { frame::SEARCH_NARROW } else { frame::SEARCH })).a11y(search_a11y));
            let now = *sort.read();
            let label = trf("Sortieren: %@", &[&now.label()]);
            let open = move |_| {
                let items = Sort::ALL
                    .into_iter()
                    .map(|s| {
                        let mut sort = sort;
                        Item::new(format!("library.sort.{}", s.key()), s.label(), move |_| sort.set(s)).checked(*sort.peek() == s)
                    })
                    .collect();
                menu::open(store.ui, Menu::under(items, *sort_at.peek()));
            };
            let button = if compact { Button::icon("library.sort", Icon::ChevronsUpDown, label.clone()) } else { Button::new("library.sort", label.clone()).glyph(Icon::ChevronsUpDown) };
            row = row.child(rect().on_sized(move |e: Event<SizedEventData>| sort_at.set_if_modified(Bounds::of(&e))).child(button.tip(label).on_press(open)));
            row = row.child(Button::icon("library.group", Icon::Layers, tr("Gruppieren")).on(*grouped.read()).tip(tr("Nach Gruppen ordnen")).on_press(move |_| {
                let on = *grouped.peek();
                grouped.set(!on);
            }));
            let items = vec![Segment { id: "library.mode.grid".into(), label: tr("Raster"), tip: Some(Command::LibraryAsGrid.tip()) }, Segment { id: "library.mode.list".into(), label: tr("Liste"), tip: Some(Command::LibraryAsList.tip()) }];
            let chosen = usize::from(*mode.read() == Mode::List);
            row = row.child(Segmented::new(tr("Ansicht"), items, chosen).on_choose(move |i| mode.set(if i == 1 { Mode::List } else { Mode::Grid })));
        }
        row = row.child(rect().on_sized(move |e: Event<SizedEventData>| more_at.set_if_modified(Bounds::of(&e))).child(Button::icon("library.more", Icon::More, tr("Mehr")).tip(tr("Mehr")).on_press(move |_| {
            let items = vec![
                Item::new("library.recent", tr("Zuletzt gelöscht"), move |_| show_recent(store)),
                Item::new("library.folder", tr("Instanzordner öffnen"), move |_| crate::view::shell::open_folder(store, crate::core::instance::instances_dir())),
            ];
            menu::open(store.ui, Menu::under(items, *more_at.peek()));
        })));
        row.child(Button::new("library.new", tr("Neue Instanz")).glyph(Icon::Plus).primary().disabled(commands::want_blocked(&Want::NewInstance)).tip(Command::NewInstance.tip()).on_press(move |_| {
            commands::want(store, Want::NewInstance);
        }))
    }
}

/// Shows the library with the group of deleted instances opened and scrolled to.
pub fn show_recent(store: Store) {
    let mut ui = store.ui;
    if *ui.page.peek() != Page::Library {
        ui.page.set(Page::Library);
    }
    let mut folded = ui.library.folded;
    folded.write().remove(RECENT);
    let mut reveal = ui.library.reveal;
    reveal.set(true);
}

/// The library page.
#[derive(PartialEq)]
pub struct LibraryPage;

impl Component for LibraryPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let shared = ui.library;
        let mut scroll = use_scroll_controller(ScrollConfig::default);
        use_infos(store);
        let reveal = *shared.reveal.read();
        use_side_effect_with_deps(&reveal, move |on| {
            if *on {
                let mut reveal = shared.reveal;
                reveal.set(false);
                scroll.scroll_to(ScrollPosition::End, Direction::Vertical);
            }
        });
        let m = store.model.read();
        let infos = ui.infos.read().clone();
        let query = shared.query.read().clone();
        let (sort, grouped, mode) = (*shared.sort.read(), *shared.grouped.read(), *shared.mode.read());
        let list = m.instances.clone();
        let loaded = m.loaded;
        drop(m);
        let installs = ui.installs.read().clone();
        if loaded && list.is_empty() && installs.is_empty() {
            return rect().width(Size::fill()).height(Size::fill()).child(empty(&t, store)).into_element();
        }
        let searching = !query.trim().is_empty();
        let hits = ordered(found(&list, &query), sort, &infos);
        let on_screen = lines(store, &hits, &infos);
        let parts = sections(&hits, grouped);
        let folded = shared.folded.read().clone();
        let mut body = rect().width(Size::fill()).padding((space::L, space::XL)).spacing(space::L);
        if !installs.is_empty() && !searching {
            body = body.child(card::Arriving { entries: installs });
        }
        if hits.is_empty() && searching {
            body = body.child(nothing(&t, &query, shared));
        }
        let headed = parts.len() > 1 || parts.first().is_some_and(|p| !p.key.is_empty());
        for part in parts {
            let open = !headed || searching || !folded.contains(&part.key);
            let mut block = rect().key(&part.key).width(Size::fill()).spacing(space::S);
            if headed {
                block = block.child(groups::Header { key: part.key.clone(), title: part.title.clone(), count: Some(part.ids.len()), open });
            }
            if open {
                let pick: Vec<Line> = on_screen.iter().filter(|l| part.ids.contains(&l.id)).cloned().collect();
                block = block.child(match mode {
                    Mode::Grid => card::Wall { lines: pick }.into_element(),
                    Mode::List => rows::Table { lines: pick }.into_element(),
                });
            }
            body = body.child(block);
        }
        if !searching {
            body = body.child(recent::Recent);
        }
        ScrollView::new_controlled(scroll).width(Size::fill()).height(Size::fill()).child(body).into_element()
    }
}

fn nothing(t: &Theme, query: &str, shared: Shared) -> Element {
    let mut typed = shared.query;
    rect()
        .width(Size::fill())
        .padding((space::XL * 2., 0.))
        .center()
        .spacing(space::M)
        .child(icon(t, Icon::Search, iconsize::HERO, t.text3))
        .child(heading(t, tr("Nichts gefunden")))
        .child(secondary(t, trf("Zu „%@“ passt keine Instanz.", &[&query.trim()])))
        .child(Button::new("library.filter.clear", tr("Filter löschen")).tall().on_press(move |_| typed.set(String::new())))
        .into_element()
}

fn empty(t: &Theme, store: Store) -> Element {
    let create = Button::new("library.empty.new", tr("Neue Instanz")).glyph(Icon::Plus).primary().tall().disabled(commands::want_blocked(&Want::NewInstance)).tip(Command::NewInstance.tip()).on_press(move |_| {
        commands::want(store, Want::NewInstance);
    });
    let packs = Button::new("library.empty.packs", tr("Modpack suchen")).glyph(Icon::Search).tall().on_press(move |_| {
        let mut page = store.ui.page;
        page.set(Page::Discover);
    });
    let mut actions = rect().horizontal().spacing(space::S).child(create).child(packs);
    if commands::can_want(&Want::Import) {
        actions = actions.child(Button::new("library.empty.import", tr("Importieren…")).glyph(Icon::Download).tall().on_press(move |_| {
            commands::want(store, Want::Import);
        }));
    }
    rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .child(icon(t, Icon::Package, iconsize::HERO, t.text3))
        .child(heading(t, tr("Noch keine Instanzen")))
        .child(rect().max_width(Size::px(frame::WELCOME)).child(line(t, tr("Eine Instanz ist ein Minecraft mit eigener Version, eigenen Mods und Einstellungen."), font::BODY, t.text2).text_align(TextAlign::Center).max_lines(3)))
        .child(actions)
        .child(caption(t, tr("Ein Modpack oder eine Instanz aus einem anderen Launcher wird hier auch zur Instanz.")))
        .into_element()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    use super::*;

    fn made(name: &str, version: &str, group: Option<&str>) -> Instance {
        let mut i = Instance::new(name, version, Loader::Vanilla);
        i.group = group.map(str::to_owned);
        i
    }

    fn names(items: &[&Instance]) -> Vec<String> {
        items.iter().map(|i| i.name.clone()).collect()
    }

    #[test]
    fn the_filter_matches_every_word_in_name_version_loader_and_group() {
        let list = vec![made("Farm", "1.21.4", Some("Technik")), made("Bau", "1.20.1", None)];
        assert_eq!(names(&found(&list, "farm")), ["Farm"]);
        assert_eq!(names(&found(&list, "1.20 bau")), ["Bau"]);
        assert_eq!(names(&found(&list, "technik 1.21")), ["Farm"]);
        assert_eq!(found(&list, "  ").len(), 2);
        assert!(found(&list, "farm 1.20").is_empty());
    }

    #[test]
    fn sorting_by_the_time_played_puts_the_newest_first_and_unplayed_last() {
        let list = vec![made("A", "1.21", None), made("B", "1.21", None), made("C", "1.21", None)];
        let now = SystemTime::now();
        let mut infos = HashMap::new();
        infos.insert(list[0].id.clone(), Info { played: Some(now - Duration::from_secs(900)), made: None });
        infos.insert(list[1].id.clone(), Info { played: Some(now), made: None });
        let infos: Infos = Rc::new(infos);
        assert_eq!(names(&ordered(list.iter().collect(), Sort::Played, &infos)), ["B", "A", "C"]);
        assert_eq!(names(&ordered(list.iter().collect(), Sort::Name, &infos)), ["A", "B", "C"]);
    }

    #[test]
    fn versions_sort_by_their_numbers_newest_first() {
        let list = vec![made("A", "1.9", None), made("B", "1.21.4", None), made("C", "1.21", None)];
        let infos: Infos = Rc::default();
        assert_eq!(names(&ordered(list.iter().collect(), Sort::Version, &infos)), ["B", "C", "A"]);
    }

    #[test]
    fn groups_run_alphabetically_then_ungrouped_then_the_quick_start() {
        let list = vec![made("1", "1.21", None), made("2", "1.21", Some(QUICK)), made("3", "1.21", Some("technik")), made("4", "1.21", Some("Bau"))];
        let refs: Vec<&Instance> = list.iter().collect();
        let keys: Vec<String> = sections(&refs, true).into_iter().map(|s| s.key).collect();
        assert_eq!(keys, ["Bau", "technik", "", QUICK]);
    }

    #[test]
    fn without_groups_or_with_grouping_off_the_library_is_one_run() {
        let list = vec![made("1", "1.21", None), made("2", "1.21", None)];
        let refs: Vec<&Instance> = list.iter().collect();
        assert_eq!(sections(&refs, true).len(), 1);
        let grouped = vec![made("1", "1.21", Some("x"))];
        let refs: Vec<&Instance> = grouped.iter().collect();
        assert_eq!(sections(&refs, false).len(), 1);
        assert_eq!(sections(&refs, true)[0].key, "x");
    }

    #[test]
    fn blank_groups_count_as_none_and_the_quick_start_is_not_offered_as_a_name() {
        let list = vec![made("1", "1.21", Some("  ")), made("2", "1.21", Some(QUICK)), made("3", "1.21", Some("Bau"))];
        assert_eq!(group_names(&list), ["Bau"]);
        let refs: Vec<&Instance> = list.iter().collect();
        assert_eq!(sections(&refs, true).iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), ["Bau", "", QUICK]);
    }
}
