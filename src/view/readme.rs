//! A project's readme as the pages beside lists and in the browser show it: the short description, the project's own links and the long text.

use freya::prelude::*;

use crate::core::i18n::tr;
use crate::core::modrinth::{Details, Kind};
use crate::ui::catalog;
use crate::ui::forms::{self, Problem};
use crate::ui::remote;
use crate::view::kit::button::Button;
use crate::view::kit::markdown::{self, Block};
use crate::view::kit::open_external;
use crate::view::kit::text::para;
use crate::view::theme::{Theme, control, font, frame, space};

const SKELETON_LINES: usize = 4;

/// A project's page as fetched: its details and the long description cut into blocks.
pub struct Body {
    pub details: Details,
    pub blocks: Vec<Block>,
    pub saved: Option<std::time::SystemTime>,
}

/// Fetches and parses the page of `project_id`; slow, so call it off the UI thread.
pub fn fetch(project_id: &str) -> anyhow::Result<Body> {
    catalog::details(project_id).map(|(details, saved)| {
        let blocks = markdown::blocks(&details.body);
        Body { details, blocks, saved }
    })
}

/// What went wrong with a fetch, in words for the user.
pub fn problem_of(e: &anyhow::Error) -> Problem {
    if remote::is_network(e) {
        Problem { text: tr("Keine Verbindung zu Modrinth. Prüf dein Internet und versuch es nochmal."), detail: Some(format!("{e:#}")) }
    } else {
        forms::explain_as(tr("Die Seite ließ sich nicht laden."), e)
    }
}

/// The address of a project's page on Modrinth.
pub fn page_url(kind: Kind, slug: &str) -> String {
    format!("https://modrinth.com/{}/{slug}", kind.project_type())
}

/// A quiet line for a page with nothing to show.
pub fn quiet(t: &Theme, text: String) -> Element {
    rect().width(Size::fill()).padding((space::L, 0.)).child(para(t, text, font::CALLOUT, t.text2)).into_element()
}

/// Placeholder lines while a page loads.
pub fn loading(t: &Theme) -> Element {
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

/// The description, the project's links and the long text of `body`; `page` is its address on Modrinth, `id` names the link buttons for the drive tool and `first` stands above the description.
pub fn view(t: &Theme, body: &Body, page: &str, id: &str, first: Option<Element>) -> Element {
    if body.blocks.is_empty() {
        let text = quiet(t, if body.details.description.is_empty() { tr("Keine Beschreibung.") } else { body.details.description.clone() });
        return match first {
            Some(first) => rect().width(Size::fill()).child(first).child(text).into_element(),
            None => text,
        };
    }
    let mut column = rect().width(Size::fill()).spacing(space::M);
    if let Some(first) = first {
        column = column.child(rect().width(Size::fill()).horizontal().child(first));
    }
    if !body.details.description.is_empty() {
        column = column.child(para(t, body.details.description.clone(), font::BODY, t.text2));
    }
    column.child(links(&body.details, page, id)).child(markdown::view(t, &body.blocks, t.text1)).into_element()
}

fn web_url(url: &Option<String>) -> Option<String> {
    url.as_deref().map(str::trim).filter(|u| u.starts_with("https://")).map(str::to_owned)
}

/// The project's own pages next to its page on Modrinth.
pub fn links(details: &Details, page: &str, id: &str) -> Element {
    let mut row = rect().width(Size::fill()).horizontal().spacing(space::XS).content(Content::wrap_spacing(space::XS));
    let pages = [
        ("source", tr("Quellcode"), web_url(&details.source_url)),
        ("wiki", tr("Wiki"), web_url(&details.wiki_url)),
        ("issues", tr("Fehler melden"), web_url(&details.issues_url)),
        ("discord", tr("Discord"), web_url(&details.discord_url)),
    ];
    for (key, label, url) in pages {
        if let Some(url) = url {
            row = row.child(Button::new(format!("{id}.link.{key}"), label).glyph(crate::view::icons::Icon::External).on_press(move |_| open_external(key, url.clone())));
        }
    }
    let url = page.to_owned();
    row.child(Button::new(format!("{id}.link.modrinth"), tr("Auf Modrinth")).glyph(crate::view::icons::Icon::External).on_press(move |_| open_external("modrinth", url.clone()))).into_element()
}