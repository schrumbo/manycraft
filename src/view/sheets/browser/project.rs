//! The page of one project in the content browser: description, gallery, versions and what it needs.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::SystemTime;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use super::{Face, Facts, Flow, act_of, fit_name, run_hint, saved_when, status_line, stepped, trail_of, trail_view, version_blocked};
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
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::spinner;
use crate::view::kit::{off_ui, open_external, ring, use_arrows, use_theme};
use crate::view::sheets::instance::{Load, problem_view};
use crate::view::theme::{Theme, control, faces, font, frame, iconsize, space};

const SHOT_BOX: (u32, u32) = (1024, 576);
const SHOTS_AT_ONCE: usize = 2;
const MAX_VERSIONS: usize = 60;
const GAME_VERSIONS_SHOWN: usize = 4;
const SKELETON_LINES: usize = 4;
const VERSION_STEP: f32 = frame::VERSION_ROW + frame::HAIRLINE;
const ACTION_LABEL_CHARS: usize = 28;

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

#[derive(Clone, PartialEq, Debug)]
enum Tail {
    Busy,
    Added,
    Add { replacing: bool },
    Blocked(String),
    Mark(Mark),
    Nothing,
}

struct Side<'a> {
    id: &'a str,
    interactive: bool,
    have: Option<&'a str>,
    chosen: Option<&'a str>,
    busy: bool,
    pointed: bool,
    fits: bool,
    blocked: Option<String>,
}

fn tail_of(side: &Side, fit: &str) -> Tail {
    let mark = || mark_of(side.have, side.chosen, side.id).map_or(Tail::Nothing, Tail::Mark);
    if !side.interactive {
        mark()
    } else if side.busy {
        Tail::Busy
    } else if side.have == Some(side.id) {
        Tail::Added
    } else if side.pointed {
        if !side.fits {
            Tail::Blocked(trf("Passt nicht zu %@.", &[&fit]))
        } else if let Some(why) = &side.blocked {
            Tail::Blocked(why.clone())
        } else {
            Tail::Add { replacing: side.have.is_some() }
        }
    } else {
        mark()
    }
}

fn start_row(ids: &[String], have: Option<&str>, chosen: Option<&str>) -> usize {
    [have, chosen].into_iter().flatten().find_map(|id| ids.iter().position(|i| i == id)).unwrap_or(0)
}

fn detail_of(tail: &Tail, detail: String, replaces: Option<&str>) -> String {
    match (tail, replaces) {
        (Tail::Add { replacing: true }, Some(old)) => trf("Ersetzt Version %@.", &[&old]),
        _ => detail,
    }
}

fn tip_of(label: &str, hint: Option<String>) -> Option<String> {
    if label.chars().count() > ACTION_LABEL_CHARS {
        Some(match hint {
            Some(hint) => format!("{label} · {hint}"),
            None => label.to_owned(),
        })
    } else {
        hint
    }
}

fn shown_number(v: &Version) -> &str {
    [v.version_number.as_str(), v.name.as_str()].into_iter().find(|n| !n.trim().is_empty()).unwrap_or(&v.id)
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
    let img = fitted(img).to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

fn fitted(img: image::DynamicImage) -> image::DynamicImage {
    if img.width() > SHOT_BOX.0 || img.height() > SHOT_BOX.1 { img.thumbnail(SHOT_BOX.0, SHOT_BOX.1) } else { img }
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
        self.load_details(generation);
        self.load_versions(generation);
    }

    fn load_details(&self, generation: u64) {
        let asked = self.hit.project_id.clone();
        let page = self.clone();
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
    }

    fn load_versions(&self, generation: u64) {
        let version = self.flow.live.peek().version.clone();
        let (asked, kind) = (self.hit.project_id.clone(), self.kind);
        let page = self.clone();
        spawn_in_window(async move {
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
        let generation = *self.epoch.peek();
        let (mut details, mut versions, mut titles, mut shots) = (self.details, self.versions, self.titles, self.shots);
        if matches!(*details.peek(), Load::Failed(_)) {
            details.set(Load::Loading);
            shots.set(HashMap::new());
            self.load_details(generation);
        }
        if matches!(*versions.peek(), Load::Failed(_)) {
            versions.set(Load::Loading);
            titles.set(Rc::new(HashMap::new()));
            self.load_versions(generation);
        }
    }

    fn show(&self, section: Section) {
        let mut state = self.section;
        state.set(section);
        let mut body = self.flow.body;
        body.scroll_to_y(0);
        if section == Section::Gallery {
            self.shots();
        }
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
    /// Shown beside the results instead of over them.
    pub pane: bool,
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
            let row = rect().width(Size::fill()).padding((space::XS, space::XS, 0., space::XS)).horizontal();
            if self.pane {
                let tip = trf("Schließen · %@", &[&Shortcut::bare(Code::Escape).label()]);
                row.main_align(Alignment::End).child(Button::icon("browser.project.close", Icon::Close, tr("Schließen")).plain().tip(tip).on_press(move |_| flow.close_page()))
            } else {
                let tip = trf("Zurück zu den Ergebnissen · %@", &[&Shortcut::bare(Code::Escape).label()]);
                row.child(Button::new("browser.project.back", tr("Ergebnisse")).plain().glyph(Icon::ChevronLeft).tip(tip).on_press(move |_| flow.close_page()))
            }
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
        let hint = Some(run_hint(act_of(hit, &self.facts), self.kind == Kind::Modpack, false)).filter(|h| !h.is_empty());
        let header = rect()
            .width(Size::fill())
            .padding((space::S, space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .content(Content::flex())
            .child(Face { pid: hit.project_id.clone(), url: hit.icon_url.clone() })
            .child(heading)
            .child(trail_view(&t, &self.flow, hit, &trail, None, hint));

        let items = SECTIONS.iter().map(|s| Segment { id: format!("browser.project.section.{}", s.id()), label: s.label(), tip: None }).collect();
        let at = SECTIONS.iter().position(|s| *s == current).unwrap_or(0);
        let choose = pg.clone();
        let sections = Segmented::new(tr("Abschnitt"), items, at).on_choose(move |i| {
            if let Some(section) = SECTIONS.get(i) {
                choose.show(*section);
            }
        });

        let mut content_top = use_state(|| 0f32);
        let content = match current {
            Section::About => about(&t, &pg),
            Section::Gallery => gallery(&t, &pg),
            Section::Versions => versions(&t, self, &pg, content_top),
            Section::Needs => needs(&t, self, &pg),
        };
        let body = ScrollView::new_controlled(self.flow.body).width(Size::fill()).height(Size::flex(1.)).scroll_with_arrows(false).child(
            rect().width(Size::fill()).padding(space::M).on_sized(move |e: Event<SizedEventData>| content_top.set_if_modified(e.area.min_y())).child(content),
        );

        rect()
            .width(Size::fill())
            .height(Size::fill())
            .content(Content::flex())
            .on_sized(spot)
            .child(back)
            .child(header)
            .child(rect().padding((0., space::M, space::S, space::M)).horizontal().child(sections))
            .child(hairline(&t))
            .child(body)
    }
}

fn quiet(t: &Theme, text: String) -> Element {
    rect().width(Size::fill()).padding((space::L, 0.)).child(para(t, text, font::CALLOUT, t.text2)).into_element()
}

fn web_page(pg: &Pg, versions: bool) -> String {
    let base = format!("https://modrinth.com/{}/{}", pg.kind.project_type(), pg.hit.slug);
    if versions { format!("{base}/versions") } else { base }
}

fn nothing(t: &Theme, pg: &Pg, text: String, id: &str, versions: bool) -> Element {
    let url = web_page(pg, versions);
    rect()
        .width(Size::fill())
        .padding((space::L, 0.))
        .spacing(space::M)
        .child(para(t, text, font::CALLOUT, t.text2))
        .child(rect().horizontal().child(Button::new(id, tr("Auf Modrinth ansehen")).glyph(Icon::External).on_press(move |_| open_external("modrinth", url.clone()))))
        .into_element()
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
        return nothing(t, pg, tr("Zu diesem Projekt gibt es keine Bilder."), "browser.project.gallery.web", false);
    }
    let mut shots = body.details.gallery.clone();
    shots.sort_by_key(|s| (!s.featured, s.ordering));
    let pics = pg.shots.read();
    rect()
        .width(Size::fill())
        .spacing(space::L)
        .children(shots.into_iter().map(|shot| {
            let frame = rect().width(Size::fill()).height(Size::px(frame::SHOT_HEIGHT)).corner_radius(t.shape.control).overflow(Overflow::Clip).center();
            let frame = match pics.get(&shot.url) {
                Some(Pic::Ready(handle)) => frame.background(t.well).child(image(handle.clone()).width(Size::fill()).height(Size::px(frame::SHOT_HEIGHT)).aspect_ratio(AspectRatio::Min).image_cover(ImageCover::Center)).into_element(),
                Some(Pic::Failed) => WithTip::new(Some(tr("Bild ließ sich nicht laden.")), frame.background(t.well).child(icon(t, Icon::ImageOff, iconsize::L, t.text3))).fill().into_element(),
                _ => frame.background(t.hover).into_element(),
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

fn versions(t: &Theme, page: &ProjectPage, pg: &Pg, content_top: State<f32>) -> Element {
    let loaded = pg.versions.read().clone();
    let list = match loaded {
        Load::Loading => return loading(t),
        Load::Failed(why) => return failed(t, pg, "browser.project.versions.error", &why),
        Load::Ready(list) => list,
    };
    if list.is_empty() {
        return nothing(t, pg, tr("Von diesem Projekt gibt es keine Versionen."), "browser.project.versions.web", true);
    }
    let rest = list.len().saturating_sub(MAX_VERSIONS);
    let mut column = rect().width(Size::fill()).child(VersionList { flow: page.flow.clone(), hit: page.hit.clone(), kind: page.kind, facts: page.facts.clone(), have: page.have.clone(), list, content_top });
    if rest > 0 {
        let link = web_page(pg, true);
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

#[derive(PartialEq)]
struct VersionList {
    flow: Flow,
    hit: Hit,
    kind: Kind,
    facts: Rc<Facts>,
    have: Option<String>,
    list: Rc<Vec<Version>>,
    content_top: State<f32>,
}

impl Component for VersionList {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        use_arrows(a11y);
        let focus = use_focus(a11y);
        let mut keyed = use_state(|| false);
        let keyboard = focus() == Focus::Keyboard || focus() != Focus::Not && *keyed.read();
        let mut cursor = use_state(|| None::<String>);
        let interactive = self.kind != Kind::Modpack;
        let label = tr("Versionen");
        let spot = use_spot("browser.project.versions", "list", &label, interactive);
        let ids: Vec<String> = self.list.iter().take(MAX_VERSIONS).map(|v| v.id.clone()).collect();
        let n = ids.len();
        let at = cursor.read().as_ref().and_then(|c| ids.iter().position(|i| i == c));
        let body = self.flow.body;
        let content_top = self.content_top;
        let mut list_top = use_state(|| 0f32);
        use_side_effect_with_deps(&at, move |at| {
            let Some(at) = *at else { return };
            let mut body = body;
            let top = (*list_top.peek() - *content_top.peek()) + frame::RING_GAP + at as f32 * VERSION_STEP;
            let bottom = top + frame::VERSION_ROW;
            let shown = -body.peek_position().y;
            let h = body.bounds.peek().map_or(0., |(_, viewport)| viewport.height);
            if top < shown {
                body.scroll_to_y(-((top - space::M).max(0.) as i32));
            } else if h > 0. && bottom + space::M > shown + h {
                body.scroll_to_y(-((bottom + space::M - h) as i32));
            }
        });

        let pid = &self.hit.project_id;
        let chosen = modrinth::newest_for(&self.list, &self.facts.filter).map(|v| v.id.clone());
        let adding = self.facts.adding.contains(pid);
        let picked = self.flow.versioning.read().get(pid).cloned();
        let busy_id = if adding && !self.facts.restoring.contains(pid) { picked.or_else(|| chosen.clone()) } else { None };
        let blocked = if adding { Some(tr("Wird hinzugefügt …")) } else { version_blocked(&self.facts) };
        let fit = fit_name(&self.facts);

        let mut column = rect().width(Size::fill());
        for (i, v) in self.list.iter().take(MAX_VERSIONS).enumerate() {
            let (id, ix) = (v.id.clone(), i);
            let press = EventHandler::new({
                let id = id.clone();
                move |_| {
                    keyed.set_if_modified(false);
                    a11y.request_focus();
                    let mut cursor = cursor;
                    cursor.set(Some(id.clone()));
                }
            });
            let add = EventHandler::new({
                let (flow, hit, list, id) = (self.flow.clone(), self.hit.clone(), self.list.clone(), id.clone());
                move |_| {
                    keyed.set_if_modified(false);
                    let mut cursor = cursor;
                    cursor.set(Some(id.clone()));
                    if let Some(version) = list.get(ix) {
                        flow.add_version(&hit, version);
                    }
                    a11y.request_focus();
                }
            });
            let row = VersionRow {
                list: self.list.clone(),
                ix,
                interactive,
                have: self.have.clone(),
                chosen: chosen.clone(),
                busy: busy_id.as_deref() == Some(v.id.as_str()),
                fits: modrinth::version_fits(v, &self.facts.filter),
                blocked: blocked.clone(),
                fit: fit.clone(),
                current: keyboard && at == Some(i),
                on_press: press,
                on_add: add,
            };
            let mut keyed_row = rect().key(id).width(Size::fill());
            if i > 0 {
                keyed_row = keyed_row.child(hairline(&t));
            }
            column = column.child(keyed_row.child(row));
        }

        let (flow, hit, list) = (self.flow.clone(), self.hit.clone(), self.list.clone());
        let (have_key, chosen_key) = (self.have.clone(), chosen.clone());
        let mut spot = spot;
        let mut list_box = rect()
            .a11y_id(a11y)
            .a11y_focusable(interactive)
            .a11y_role(AccessibilityRole::ListBox)
            .a11y_alt(label)
            .width(Size::fill())
            .padding(frame::RING_GAP)
            .corner_radius(t.shape.control)
            .on_sized(move |e: Event<SizedEventData>| {
                list_top.set_if_modified(e.area.min_y());
                spot(e);
            })
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !interactive || !a11y.is_focused() || n == 0 || !e.modifiers.is_empty() {
                    return;
                }
                keyed.set_if_modified(true);
                let at = cursor.peek().as_ref().and_then(|c| ids.iter().position(|i| i == c));
                let page = body.bounds.peek().map_or(1, |(_, viewport)| (viewport.height / VERSION_STEP) as i32).max(1);
                let go = |to: usize| {
                    let mut cursor = cursor;
                    cursor.set(ids.get(to).cloned());
                };
                match &e.key {
                    Key::Named(NamedKey::ArrowDown) => go(stepped(at, n, 1)),
                    Key::Named(NamedKey::ArrowUp) => go(stepped(at, n, -1)),
                    Key::Named(NamedKey::PageDown) => go(stepped(at, n, page)),
                    Key::Named(NamedKey::PageUp) => go(stepped(at, n, -page)),
                    Key::Named(NamedKey::Home) => go(stepped(at, n, i32::MIN)),
                    Key::Named(NamedKey::End) => go(stepped(at, n, i32::MAX)),
                    Key::Named(NamedKey::Enter) => {
                        e.prevent_default();
                        match at {
                            None => go(start_row(&ids, have_key.as_deref(), chosen_key.as_deref())),
                            Some(to) => {
                                if let Some(version) = list.get(to) {
                                    flow.add_version(&hit, version);
                                }
                            }
                        }
                    }
                    _ => return,
                }
                e.stop_propagation();
            });
        if let Some(border) = ring(&t, if keyboard { Focus::Keyboard } else { Focus::Not }) {
            list_box = list_box.border(border);
        }
        list_box.child(column)
    }
}

#[derive(PartialEq)]
struct VersionRow {
    list: Rc<Vec<Version>>,
    ix: usize,
    interactive: bool,
    have: Option<String>,
    chosen: Option<String>,
    busy: bool,
    fits: bool,
    blocked: Option<String>,
    fit: String,
    current: bool,
    on_press: EventHandler<()>,
    on_add: EventHandler<()>,
}

impl Component for VersionRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let Some(v) = self.list.get(self.ix) else { return rect().into_element() };
        let number = shown_number(v);
        let mut name = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(line(&t, v.version_number.clone(), font::BODY, t.text1).font_weight(FontWeight::MEDIUM));
        match v.version_type.as_str() {
            "beta" => name = name.child(badge(&t, tr("Beta"), Tone::Quiet)),
            "alpha" => name = name.child(badge(&t, tr("Alpha"), Tone::Quiet)),
            _ => {}
        }
        let mut games: Vec<String> = v.game_versions.iter().rev().take(GAME_VERSIONS_SHOWN).cloned().collect();
        if v.game_versions.len() > GAME_VERSIONS_SHOWN {
            games.push("…".to_owned());
        }
        let detail = format!("{} · {} · {}", day(&v.date_published), games.join(", "), v.loaders.join(", "));
        let side = Side {
            id: &v.id,
            interactive: self.interactive,
            have: self.have.as_deref(),
            chosen: self.chosen.as_deref(),
            busy: self.busy,
            pointed: *hovered.read() || self.current,
            fits: self.fits,
            blocked: self.blocked.clone(),
        };
        let tail = tail_of(&side, &self.fit);
        let replaces = self.have.as_deref().and_then(|have| self.list.iter().find(|o| o.id == have)).map(|o| shown_number(o).to_owned());
        let detail = detail_of(&tail, detail, replaces.as_deref());
        let fill = if self.interactive && *pressed.read() { t.press } else { Color::TRANSPARENT };
        let mut row = rect()
            .width(Size::fill())
            .height(Size::px(frame::VERSION_ROW))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .background(fill)
            .content(Content::flex())
            .child(rect().width(Size::flex(1.)).child(name).child(caption(&t, detail).width(Size::fill())));
        if self.interactive {
            let press = self.on_press.clone();
            row = row
                .cursor(CursorIcon::Pointer)
                .on_pointer_enter(move |_| hovered.set(true))
                .on_pointer_leave(move |_| {
                    hovered.set_if_modified(false);
                    pressed.set_if_modified(false);
                })
                .on_mouse_down(move |_| pressed.set(true))
                .on_mouse_up(move |_| pressed.set_if_modified(false))
                .on_press(move |_| press.call(()));
        }
        if self.current {
            row = row.border(Border::new().fill(t.focus).width(frame::RING).alignment(BorderAlignment::Inner));
        }
        match tail_view(&t, tail, &v.id, number, replaces, &self.on_add) {
            Some(tail) => row.child(tail).into_element(),
            None => row.into_element(),
        }
    }
}

fn tail_view(t: &Theme, tail: Tail, id: &str, number: &str, replaces: Option<String>, add: &EventHandler<()>) -> Option<Element> {
    let label = trf("Version %@ hinzufügen", &[&number]);
    let button = || Button::new(format!("browser.project.version.{id}.add"), label.clone()).glyph(Icon::Plus).not_focusable().label_max(frame::VERSION_ACTION_LABEL);
    match tail {
        Tail::Busy => Some(spinner::waiting(t, tr("Wird hinzugefügt …"))),
        Tail::Added => Some(status_line(t, Icon::Check, t.success_text, tr("Hinzugefügt"))),
        Tail::Add { .. } => {
            let add = add.clone();
            let mut b = button().on_press(move |_| add.call(()));
            if let Some(tip) = tip_of(&label, replaces.map(|old| trf("Ersetzt Version %@.", &[&old]))) {
                b = b.tip(tip);
            }
            Some(b.into_element())
        }
        Tail::Blocked(why) => {
            let tip = tip_of(&label, Some(why.clone())).unwrap_or(why);
            Some(button().disabled(Some(tip)).into_element())
        }
        Tail::Mark(mark) => Some(mark_view(t, mark)),
        Tail::Nothing => None,
    }
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
                .height(Size::px(frame::VERSION_ROW))
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

    fn side<'a>(id: &'a str) -> Side<'a> {
        Side { id, interactive: true, have: None, chosen: Some("new"), busy: false, pointed: false, fits: true, blocked: None }
    }

    #[test]
    fn a_row_at_rest_shows_the_same_marks_as_before() {
        assert_eq!(tail_of(&side("new"), "x"), Tail::Mark(Mark::Adding));
        assert_eq!(tail_of(&side("old"), "x"), Tail::Nothing);
        assert_eq!(tail_of(&Side { have: Some("old"), ..side("new") }, "x"), Tail::Mark(Mark::Newest));
    }

    #[test]
    fn the_installed_row_says_added_even_under_the_pointer_and_busy_beats_everything() {
        assert_eq!(tail_of(&Side { have: Some("old"), ..side("old") }, "x"), Tail::Added);
        assert_eq!(tail_of(&Side { have: Some("old"), pointed: true, ..side("old") }, "x"), Tail::Added);
        assert_eq!(tail_of(&Side { have: Some("old"), busy: true, pointed: true, ..side("old") }, "x"), Tail::Busy);
        assert_eq!(tail_of(&Side { busy: true, ..side("new") }, "x"), Tail::Busy);
    }

    #[test]
    fn the_pointer_or_cursor_brings_the_button_for_a_version_that_fits() {
        assert_eq!(tail_of(&Side { pointed: true, ..side("old") }, "x"), Tail::Add { replacing: false });
        assert_eq!(tail_of(&Side { pointed: true, have: Some("other"), ..side("old") }, "x"), Tail::Add { replacing: true });
        assert_eq!(tail_of(&Side { pointed: true, ..side("new") }, "x"), Tail::Add { replacing: false });
    }

    #[test]
    fn a_version_that_does_not_fit_or_is_blocked_shows_the_reason_on_a_disabled_button() {
        let misfit = Side { pointed: true, fits: false, blocked: Some("busy".into()), ..side("old") };
        assert_eq!(tail_of(&misfit, "Minecraft 1.21 · Fabric"), Tail::Blocked(trf("Passt nicht zu %@.", &[&"Minecraft 1.21 · Fabric"])));
        let blocked = Side { pointed: true, blocked: Some("busy".into()), ..side("old") };
        assert_eq!(tail_of(&blocked, "x"), Tail::Blocked("busy".into()));
        assert_eq!(tail_of(&Side { fits: false, ..side("old") }, "x"), Tail::Nothing);
    }

    #[test]
    fn the_first_enter_moves_the_cursor_to_the_installed_row_then_the_default_then_the_top() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(start_row(&ids, Some("c"), Some("b")), 2);
        assert_eq!(start_row(&ids, None, Some("b")), 1);
        assert_eq!(start_row(&ids, Some("gone"), Some("b")), 1);
        assert_eq!(start_row(&ids, Some("gone"), None), 0);
        assert_eq!(start_row(&[], None, None), 0);
    }

    #[test]
    fn a_row_with_a_replacing_button_says_what_it_replaces() {
        let replacing = Tail::Add { replacing: true };
        assert_eq!(detail_of(&replacing, "d".into(), Some("1.0")), trf("Ersetzt Version %@.", &[&"1.0"]));
        assert_eq!(detail_of(&replacing, "d".into(), None), "d");
        assert_eq!(detail_of(&Tail::Add { replacing: false }, "d".into(), Some("1.0")), "d");
        assert_eq!(detail_of(&Tail::Blocked("x".into()), "d".into(), Some("1.0")), "d");
        assert_eq!(detail_of(&Tail::Mark(Mark::Newest), "d".into(), Some("1.0")), "d");
    }

    #[test]
    fn a_long_button_label_goes_whole_into_the_tooltip() {
        assert_eq!(tip_of("Version 1 hinzufügen", None), None);
        assert_eq!(tip_of("Version 1 hinzufügen", Some("hint".into())), Some("hint".into()));
        let long = "Version 1.21.1-fabric-0.5.11+mc1.21.1 hinzufügen";
        assert_eq!(tip_of(long, None), Some(long.into()));
        assert_eq!(tip_of(long, Some("hint".into())), Some(format!("{long} · hint")));
    }

    #[test]
    fn modpack_rows_stay_read_only() {
        assert_eq!(tail_of(&Side { interactive: false, pointed: true, busy: true, ..side("old") }, "x"), Tail::Nothing);
        assert_eq!(tail_of(&Side { interactive: false, pointed: true, ..side("new") }, "x"), Tail::Mark(Mark::Adding));
    }

    #[test]
    fn a_version_is_named_by_its_number_then_its_name_then_its_id() {
        let mut v = canned_version();
        assert_eq!(shown_number(&v), "1.0");
        v.version_number.clear();
        assert_eq!(shown_number(&v), "Name");
        v.name = " ".into();
        assert_eq!(shown_number(&v), "id");
    }

    fn canned_version() -> Version {
        Version { id: "id".into(), project_id: "p".into(), name: "Name".into(), version_number: "1.0".into(), version_type: "release".into(), date_published: String::new(), game_versions: Vec::new(), loaders: Vec::new(), files: Vec::new(), dependencies: Vec::new(), changelog: None }
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
    fn a_big_shot_shrinks_into_the_box_and_a_small_one_stays() {
        let wide = fitted(image::DynamicImage::new_rgba8(4000, 1000));
        assert_eq!((wide.width(), wide.height()), (1024, 256));
        let tall = fitted(image::DynamicImage::new_rgba8(1000, 4000));
        assert_eq!((tall.width(), tall.height()), (144, 576));
        let small = fitted(image::DynamicImage::new_rgba8(300, 200));
        assert_eq!((small.width(), small.height()), (300, 200));
    }

    #[test]
    fn a_day_reads_as_a_date_and_an_unreadable_one_stays_short() {
        assert_eq!(day("not a date at all"), "not a date");
        assert!(!day("2024-05-06T07:08:09Z").is_empty());
    }
}
