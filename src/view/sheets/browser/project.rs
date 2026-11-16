//! The page of one project in the content browser: description, gallery, versions and what it needs.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::SystemTime;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use super::{AUTHOR_MAX, Face, Facts, Flow, saved_when, status_line, trail_of, trail_view};
use crate::core::i18n::{Lang, lang, tr, trf};
use crate::core::modrinth::{self, Details, Hit, Kind, Version};
use crate::core::packs;
use crate::ui::catalog;
use crate::ui::forms::{self, Problem, downloads_short};
use crate::ui::remote;
use crate::view::commands::Shortcut;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::{Tone, badge};
use crate::view::kit::button::Button;
use crate::view::kit::markdown::{self, Block};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::text::{caption, hairline, line, para, strong};
use crate::view::kit::{off_ui, open_external, use_theme};
use crate::view::sheets::instance::{Load, problem_view};
use crate::view::theme::{Theme, control, faces, font, frame, iconsize, space};

pub(super) const LINE_STEP: f32 = 40.;
const SHOT_HEIGHT: f32 = 270.;
const SHOT_EDGE: u32 = 1280;
const SHOTS_AT_ONCE: usize = 2;
const MAX_VERSIONS: usize = 60;
const GAME_VERSIONS_SHOWN: usize = 4;
const VERSION_ROW: f32 = 44.;
const SKELETON_LINES: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    About,
    Gallery,
    Versions,
    Needs,
}

const SECTIONS: [Section; 4] = [Section::About, Section::Gallery, Section::Versions, Section::Needs];

impl Section {
    fn id(self) -> &'static str {
        match self {
            Section::About => "about",
            Section::Gallery => "gallery",
            Section::Versions => "versions",
            Section::Needs => "needs",
        }
    }

    fn label(self) -> String {
        match self {
            Section::About => tr("Beschreibung"),
            Section::Gallery => tr("Galerie"),
            Section::Versions => tr("Versionen"),
            Section::Needs => tr("Abhängigkeiten"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mark {
    Added,
    Newest,
    Adding,
}

fn mark_of(have: Option<&str>, chosen: Option<&str>, id: &str) -> Option<Mark> {
    if have == Some(id) {
        Some(Mark::Added)
    } else if chosen == Some(id) {
        Some(if have.is_some() { Mark::Newest } else { Mark::Adding })
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Need {
    Added,
    WithIt,
    Required,
    Conflict,
    Optional,
}

fn need_of(have: bool, dependency_type: &str, mods: bool) -> Need {
    match dependency_type {
        _ if have => Need::Added,
        "required" if mods => Need::WithIt,
        "required" => Need::Required,
        "incompatible" => Need::Conflict,
        _ => Need::Optional,
    }
}

fn day(iso: &str) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(iso) else { return iso.chars().take(10).collect() };
    let at = at.with_timezone(&chrono::Local);
    if lang() == Lang::En { at.format("%b %-d, %Y").to_string() } else { at.format("%-d.%-m.%Y").to_string() }
}

fn problem_of(e: &anyhow::Error) -> Problem {
    if remote::is_network(e) {
        Problem { text: tr("Keine Verbindung zu Modrinth. Prüf dein Internet und versuch es nochmal."), detail: Some(format!("{e:#}")) }
    } else {
        forms::explain_as(tr("Die Seite ließ sich nicht laden."), e)
    }
}

struct Body {
    details: Details,
    blocks: Vec<Block>,
    saved: Option<SystemTime>,
}

#[derive(Clone)]
enum Pic {
    Loading,
    Ready(ImageHandle),
    Failed,
}

static SHOT_GATE: LazyLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> = LazyLock::new(|| async_channel::bounded(SHOTS_AT_ONCE));

struct ShotSlot;

impl ShotSlot {
    async fn take() -> Self {
        let _ = SHOT_GATE.0.send(()).await;
        Self
    }
}

impl Drop for ShotSlot {
    fn drop(&mut self) {
        let _ = SHOT_GATE.1.try_recv();
    }
}

fn decode_shot(path: &std::path::Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let img = if img.width().max(img.height()) > SHOT_EDGE { img.thumbnail(SHOT_EDGE, SHOT_EDGE) } else { img };
    let img = img.to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

#[derive(Clone)]
struct Pg {
    hit: Hit,
    kind: Kind,
    flow: Flow,
    details: State<Load<Rc<Body>>>,
    versions: State<Load<Rc<Vec<Version>>>>,
    titles: State<Rc<HashMap<String, String>>>,
    shots: State<HashMap<String, Pic>>,
    section: State<Section>,
    epoch: State<u64>,
}

impl Pg {
    fn current(&self, generation: u64) -> bool {
        self.epoch.try_peek().is_some_and(|e| *e == generation)
    }

    fn fetch(&self) {
        let generation = {
            let mut epoch = self.epoch;
            let next = *epoch.peek() + 1;
            epoch.set(next);
            next
        };
        let version = self.flow.live.peek().version.clone();
        let (id, kind) = (self.hit.project_id.clone(), self.kind);
        let page = self.clone();
        let asked = id.clone();
        spawn_in_window(async move {
            let result = off_ui(move || {
                catalog::details(&asked).map(|(details, saved)| {
                    let blocks = markdown::blocks(&packs::readable(&details.body));
                    (details, blocks, saved)
                })
            })
            .await;
            if !page.current(generation) {
                return;
            }
            let loaded = match result {
                Some(Ok((details, blocks, saved))) => Load::Ready(Rc::new(Body { details, blocks, saved })),
                Some(Err(e)) => Load::Failed(problem_of(&e)),
                None => Load::Failed(Problem::plain(forms::unexplained())),
            };
            let mut details = page.details;
            details.set(loaded);
            if *page.section.peek() == Section::Gallery {
                page.shots();
            }
        });
        let page = self.clone();
        spawn_in_window(async move {
            let asked = id.clone();
            let result = off_ui(move || catalog::versions(&asked, kind, &version)).await;
            if !page.current(generation) {
                return;
            }
            let mut wanted: Vec<String> = Vec::new();
            let loaded = match result {
                Some(Ok((list, _))) => {
                    wanted = list.iter().flat_map(|v| v.dependencies.iter().filter_map(|d| d.project_id.clone())).collect();
                    wanted.sort();
                    wanted.dedup();
                    Load::Ready(Rc::new(list))
                }
                Some(Err(e)) => Load::Failed(problem_of(&e)),
                None => Load::Failed(Problem::plain(forms::unexplained())),
            };
            let mut versions = page.versions;
            versions.set(loaded);
            if wanted.is_empty() {
                return;
            }
            let titles = off_ui(move || catalog::titles(&wanted).unwrap_or_default()).await.unwrap_or_default();
            if !page.current(generation) {
                return;
            }
            let mut state = page.titles;
            state.set(Rc::new(titles));
        });
    }

    fn retry(&self) {
        let (mut details, mut versions, mut titles) = (self.details, self.versions, self.titles);
        details.set(Load::Loading);
        versions.set(Load::Loading);
        titles.set(Rc::new(HashMap::new()));
        self.fetch();
        self.flow.focus();
    }

    fn show(&self, section: Section) {
        let mut state = self.section;
        state.set(section);
        let mut body = self.flow.body;
        body.scroll_to_y(0);
        if section == Section::Gallery {
            self.shots();
        }
        self.flow.focus();
    }

    fn shots(&self) {
        let urls: Vec<String> = match &*self.details.peek() {
            Load::Ready(body) => body.details.gallery.iter().map(|s| s.url.clone()).collect(),
            _ => return,
        };
        let mut shots = self.shots;
        let mut fresh: Vec<String> = Vec::new();
        {
            let known = shots.peek();
            for url in urls {
                if !known.contains_key(&url) && !fresh.contains(&url) {
                    fresh.push(url);
                }
            }
        }
        if fresh.is_empty() {
            return;
        }
        {
            let mut map = shots.write();
            for url in &fresh {
                map.insert(url.clone(), Pic::Loading);
            }
        }
        let generation = *self.epoch.peek();
        for url in fresh {
            let page = self.clone();
            spawn_in_window(async move {
                let slot = ShotSlot::take().await;
                if !page.current(generation) {
                    return;
                }
                let asked = url.clone();
                let decoded = off_ui(move || {
                    let _slot = slot;
                    catalog::shot(&asked).ok().and_then(|path| decode_shot(&path))
                })
                .await
                .flatten();
                if !page.current(generation) {
                    return;
                }
                let pic = decoded.and_then(|(w, h, rgba)| ImageHandle::from_rgba(w, h, Bytes::from(rgba), AlphaType::Unpremul)).map_or(Pic::Failed, Pic::Ready);
                let mut shots = page.shots;
                shots.write().insert(url, pic);
            });
        }
    }
}

/// The page of `hit` over the results of the browser: what it is, what is in the gallery, which versions there are and what it needs.
#[derive(PartialEq)]
pub(super) struct ProjectPage {
    pub flow: Flow,
    pub hit: Hit,
    pub kind: Kind,
    pub facts: Rc<Facts>,
    pub have: Option<String>,
    pub mods: Rc<HashSet<String>>,
}

impl Component for ProjectPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let hit = &self.hit;
        let spot = use_spot("browser.project", "page", &hit.title, true);
        let pg = Pg {
            hit: hit.clone(),
            kind: self.kind,
            flow: self.flow.clone(),
            details: use_state(|| Load::Loading),
            versions: use_state(|| Load::Loading),
            titles: use_state(|| Rc::new(HashMap::new())),
            shots: use_state(HashMap::new),
            section: use_state(|| Section::About),
            epoch: use_state(|| 0),
        };
        let start = pg.clone();
        use_hook(move || start.fetch());
        let current = *pg.section.read();

        let back = {
            let flow = self.flow.clone();
            let tip = trf("Zurück zu den Ergebnissen · %@", &[&Shortcut::bare(Code::Escape).label()]);
            Button::new("browser.project.back", tr("Ergebnisse")).plain().glyph(Icon::ChevronLeft).tip(tip).on_press(move |_| flow.close_page())
        };

        let (downloads, updated, saved) = match &*pg.details.read() {
            Load::Ready(body) => (body.details.downloads, Some(day(&body.details.updated)), body.saved),
            _ => (hit.downloads, None, None),
        };
        let mut facts = vec![trf("%@ Downloads", &[&downloads_short(downloads)])];
        if let Some(when) = updated {
            facts.push(trf("Aktualisiert am %@", &[&when]));
        }
        if let Some(at) = saved {
            facts.push(trf("Gespeichert um %@", &[&saved_when(at)]));
        }
        let mut title_row = paragraph().width(Size::fill()).max_lines(1).text_overflow(TextOverflow::Ellipsis).font_size(font::TITLE3).color(t.text1).font_weight(FontWeight::SEMI_BOLD);
        if let Some(f) = &t.font {
            title_row = title_row.font_family(f.clone());
        }
        for family in faces::BODY {
            title_row = title_row.font_family(family);
        }
        title_row = title_row.span(Span::new(hit.title.clone()));
        if !hit.author.is_empty() {
            title_row = title_row.span(Span::new(format!("  {}", trf("von %@", &[&hit.author]))).font_size(font::CALLOUT).color(t.text2).font_weight(FontWeight::NORMAL));
        }
        let heading = rect().width(Size::flex(1.)).spacing(space::XXS).child(title_row).child(caption(&t, facts.join(" · ")).width(Size::fill()));
        let trail = trail_of(hit, &self.facts);
        let header = rect()
            .width(Size::fill())
            .padding((space::S, space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .content(Content::flex())
            .child(Face { pid: hit.project_id.clone(), url: hit.icon_url.clone() })
            .child(heading)
            .child(trail_view(&t, &self.flow, hit, &trail, None));

        let items = SECTIONS.iter().map(|s| Segment { id: format!("browser.project.section.{}", s.id()), label: s.label(), tip: None }).collect();
        let at = SECTIONS.iter().position(|s| *s == current).unwrap_or(0);
        let choose = pg.clone();
        let sections = Segmented::new(tr("Abschnitt"), items, at).on_choose(move |i| {
            if let Some(section) = SECTIONS.get(i) {
                choose.show(*section);
            }
        });

        let content = match current {
            Section::About => about(&t, &pg),
            Section::Gallery => gallery(&t, &pg),
            Section::Versions => versions(&t, self, &pg),
            Section::Needs => needs(&t, self, &pg),
        };
        let body = ScrollView::new_controlled(self.flow.body).width(Size::fill()).height(Size::flex(1.)).scroll_with_arrows(false).child(rect().width(Size::fill()).padding(space::M).child(content));

        rect()
            .width(Size::fill())
            .height(Size::fill())
            .content(Content::flex())
            .on_sized(spot)
            .child(rect().padding((space::XS, space::XS, 0., space::XS)).horizontal().child(back))
            .child(header)
            .child(rect().padding((0., space::M, space::S, space::M)).horizontal().child(sections))
            .child(hairline(&t))
            .child(body)
    }
}

fn quiet(t: &Theme, text: String) -> Element {
    rect().width(Size::fill()).padding((space::L, 0.)).child(para(t, text, font::CALLOUT, t.text2)).into_element()
}

fn loading(t: &Theme) -> Element {
    let bar = |w: f32| rect().width(Size::px(w)).height(Size::px(frame::BAR_H)).corner_radius(t.shape.small).background(t.hover);
    let one = || {
        rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .child(rect().width(Size::px(control::L)).height(Size::px(control::L)).corner_radius(t.shape.control).background(t.hover))
            .child(rect().spacing(space::S).child(bar(frame::BAR_NAME)).child(bar(frame::BAR_TEXT)))
    };
    rect().width(Size::fill()).spacing(space::M).children((0..SKELETON_LINES).map(|_| one())).into_element()
}

fn failed(t: &Theme, pg: &Pg, key: &str, why: &Problem) -> Element {
    let retry = pg.clone();
    rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .content(Content::flex())
        .child(rect().width(Size::flex(1.)).child(problem_view(t, key, Icon::WifiOff, why, pg.flow.details)))
        .child(Button::new("browser.project.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| retry.retry()))
        .into_element()
}

fn about(t: &Theme, pg: &Pg) -> Element {
    let loaded = pg.details.read().clone();
    match loaded {
        Load::Loading => loading(t),
        Load::Failed(why) => failed(t, pg, "browser.project.error", &why),
        Load::Ready(body) if body.blocks.is_empty() => quiet(t, if body.details.description.is_empty() { tr("Keine Beschreibung.") } else { body.details.description.clone() }),
        Load::Ready(body) => {
            let mut column = rect().width(Size::fill()).spacing(space::M);
            if !body.details.description.is_empty() {
                column = column.child(para(t, body.details.description.clone(), font::BODY, t.text2));
            }
            column.child(markdown::view(t, &body.blocks, t.text1)).into_element()
        }
    }
}

fn gallery(t: &Theme, pg: &Pg) -> Element {
    let loaded = pg.details.read().clone();
    let body = match loaded {
        Load::Loading => return loading(t),
        Load::Failed(why) => return failed(t, pg, "browser.project.error", &why),
        Load::Ready(body) => body,
    };
    if body.details.gallery.is_empty() {
        return quiet(t, tr("Zu diesem Projekt gibt es keine Bilder."));
    }
    let mut shots = body.details.gallery.clone();
    shots.sort_by_key(|s| (!s.featured, s.ordering));
    let pics = pg.shots.read();
    rect()
        .width(Size::fill())
        .spacing(space::L)
        .children(shots.into_iter().map(|shot| {
            let frame = rect().width(Size::fill()).height(Size::px(SHOT_HEIGHT)).corner_radius(t.shape.control).overflow(Overflow::Clip).center();
            let frame = match pics.get(&shot.url) {
                Some(Pic::Ready(handle)) => frame.background(t.well).child(image(handle.clone()).width(Size::fill()).height(Size::px(SHOT_HEIGHT)).aspect_ratio(AspectRatio::Min).image_cover(ImageCover::Center)),
                Some(Pic::Failed) => frame.background(t.well).child(icon(t, Icon::ImageOff, iconsize::L, t.text3)),
                _ => frame.background(t.hover),
            };
            let mut column = rect().key(shot.url.clone()).width(Size::fill()).spacing(space::XS).child(frame);
            if let Some(text) = shot.title.filter(|c| !c.trim().is_empty()) {
                column = column.child(para(t, text, font::CALLOUT, t.text2));
            }
            column
        }))
        .into_element()
}

fn mark_view(t: &Theme, mark: Mark) -> Element {
    match mark {
        Mark::Added => status_line(t, Icon::Check, t.success_text, tr("Hinzugefügt")),
        Mark::Newest => status_line(t, Icon::ArrowDown, t.text2, tr("Neueste passende")),
        Mark::Adding => status_line(t, Icon::ArrowDown, t.text2, tr("Wird hinzugefügt")),
    }
}

fn versions(t: &Theme, page: &ProjectPage, pg: &Pg) -> Element {
    let loaded = pg.versions.read().clone();
    let list = match loaded {
        Load::Loading => return loading(t),
        Load::Failed(why) => return failed(t, pg, "browser.project.versions.error", &why),
        Load::Ready(list) => list,
    };
    if list.is_empty() {
        return quiet(t, tr("Von diesem Projekt gibt es keine Versionen."));
    }
    let chosen = modrinth::newest_for(&list, &page.facts.filter).map(|v| v.id.clone());
    let rest = list.len().saturating_sub(MAX_VERSIONS);
    let mut column = rect().width(Size::fill());
    for (i, v) in list.iter().take(MAX_VERSIONS).enumerate() {
        if i > 0 {
            column = column.child(hairline(t));
        }
        let mut name = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(line(t, v.version_number.clone(), font::BODY, t.text1).font_weight(FontWeight::MEDIUM));
        match v.version_type.as_str() {
            "beta" => name = name.child(badge(t, tr("Beta"), Tone::Quiet)),
            "alpha" => name = name.child(badge(t, tr("Alpha"), Tone::Quiet)),
            _ => {}
        }
        let mut games: Vec<String> = v.game_versions.iter().rev().take(GAME_VERSIONS_SHOWN).cloned().collect();
        if v.game_versions.len() > GAME_VERSIONS_SHOWN {
            games.push("…".to_owned());
        }
        let detail = format!("{} · {} · {}", day(&v.date_published), games.join(", "), v.loaders.join(", "));
        let mut row = rect()
            .key(v.id.clone())
            .width(Size::fill())
            .height(Size::px(VERSION_ROW))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .content(Content::flex())
            .child(rect().width(Size::flex(1.)).child(name).child(caption(t, detail).width(Size::fill())));
        if let Some(mark) = mark_of(page.have.as_deref(), chosen.as_deref(), &v.id) {
            row = row.child(mark_view(t, mark));
        }
        column = column.child(row);
    }
    if rest > 0 {
        let link = format!("https://modrinth.com/{}/{}/versions", page.kind.project_type(), page.hit.slug);
        column = column.child(
            rect()
                .width(Size::fill())
                .padding((space::S, 0.))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::M)
                .content(Content::flex())
                .child(rect().width(Size::flex(1.)).child(para(t, trf("und %lld ältere", &[&rest]), font::CALLOUT, t.text2)))
                .child(Button::new("browser.project.versions.all", tr("Alle auf Modrinth")).glyph(Icon::External).on_press(move |_| open_external("modrinth", link.clone()))),
        );
    }
    column.into_element()
}

fn needs(t: &Theme, page: &ProjectPage, pg: &Pg) -> Element {
    let loaded = pg.versions.read().clone();
    let list = match loaded {
        Load::Loading => return loading(t),
        Load::Failed(why) => return failed(t, pg, "browser.project.versions.error", &why),
        Load::Ready(list) => list,
    };
    let Some(version) = modrinth::newest_for(&list, &page.facts.filter).or_else(|| list.first()) else { return quiet(t, tr("Braucht nichts weiter.")) };
    let mut seen: HashSet<&str> = HashSet::new();
    let wanted: Vec<(&str, &str)> = version
        .dependencies
        .iter()
        .filter(|d| d.dependency_type != "embedded")
        .filter_map(|d| Some((d.project_id.as_deref()?, d.dependency_type.as_str())))
        .filter(|(id, _)| seen.insert(id))
        .collect();
    if wanted.is_empty() {
        return quiet(t, tr("Braucht nichts weiter."));
    }
    let titles = pg.titles.read().clone();
    let mods = page.kind == Kind::Mod;
    let mut column = rect().width(Size::fill());
    for (i, (id, dependency_type)) in wanted.into_iter().enumerate() {
        if i > 0 {
            column = column.child(hairline(t));
        }
        let title = titles.get(id).cloned().unwrap_or_else(|| id.to_owned());
        let (state, color) = match need_of(page.mods.contains(id), dependency_type, mods) {
            Need::Added => (tr("Hinzugefügt"), t.success_text),
            Need::WithIt => (tr("Wird mit hinzugefügt"), t.text2),
            Need::Required => (tr("Benötigt"), t.text2),
            Need::Conflict => (tr("Verträgt sich nicht"), t.danger_text),
            Need::Optional => (tr("Optional"), t.text3),
        };
        column = column.child(
            rect()
                .key(id.to_owned())
                .width(Size::fill())
                .height(Size::px(VERSION_ROW))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::M)
                .content(Content::flex())
                .child(Face { pid: id.to_owned(), url: None })
                .child(line(t, title, font::BODY, t.text1).width(Size::flex(1.)))
                .child(line(t, state, font::CALLOUT, color)),
        );
    }
    column.into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_version_is_marked_added_and_the_newest_one_depends_on_it() {
        assert_eq!(mark_of(Some("a"), Some("b"), "a"), Some(Mark::Added));
        assert_eq!(mark_of(Some("a"), Some("b"), "b"), Some(Mark::Newest));
        assert_eq!(mark_of(None, Some("b"), "b"), Some(Mark::Adding));
        assert_eq!(mark_of(Some("a"), Some("b"), "c"), None);
        assert_eq!(mark_of(None, None, "a"), None);
    }

    #[test]
    fn the_newest_version_that_is_also_installed_is_added() {
        assert_eq!(mark_of(Some("b"), Some("b"), "b"), Some(Mark::Added));
    }

    #[test]
    fn a_dependency_says_what_it_is_to_the_instance() {
        assert_eq!(need_of(true, "required", true), Need::Added);
        assert_eq!(need_of(true, "incompatible", false), Need::Added);
        assert_eq!(need_of(false, "required", true), Need::WithIt);
        assert_eq!(need_of(false, "required", false), Need::Required);
        assert_eq!(need_of(false, "incompatible", true), Need::Conflict);
        assert_eq!(need_of(false, "optional", true), Need::Optional);
        assert_eq!(need_of(false, "anything", false), Need::Optional);
    }

    #[test]
    fn a_day_reads_as_a_date_and_an_unreadable_one_stays_short() {
        assert_eq!(day("not a date at all"), "not a date");
        assert!(!day("2024-05-06T07:08:09Z").is_empty());
    }
}
