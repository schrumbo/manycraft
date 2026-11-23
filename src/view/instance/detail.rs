//! The detail pane of an installed mod or pack: the readme of its Modrinth project, or what the file says about itself.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use freya::prelude::*;

use super::cursor::{Cursor, Live};
use super::mods::problem_view;
use crate::core::i18n::{tr, trf};
use crate::core::modmeta::{self, About};
use crate::core::modrinth::Kind;
use crate::ui::forms::{Problem, downloads_short};
use crate::view::commands::Shortcut;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::text::{caption, para};
use crate::view::kit::{after, off_ui, open_external, page, toast, use_theme};
use crate::view::readme::{self, Body};
use crate::view::store::Store;
use crate::view::theme::{font, space};

const SETTLE: Duration = Duration::from_millis(120);
const REFOCUS: Duration = Duration::from_millis(50);

/// The key that shows or hides the detail pane of the row under the cursor, in every list that has one.
pub(super) const TOGGLE: Shortcut = Shortcut::primary(Code::KeyI);

/// `label` with the key of [`TOGGLE`] for a tooltip.
pub(super) fn with_key(label: String) -> String {
    format!("{label} · {}", TOGGLE.label())
}

/// What the pane shows.
#[derive(Clone, PartialEq)]
pub(super) enum Subject {
    /// A project on Modrinth: its readme.
    Project { id: String, title: String, kind: Kind },
    /// A jar that is not from Modrinth: what its metadata says.
    Local { dir: PathBuf, file: String },
    /// A row without details, with the reason.
    Nothing(String),
}

impl Subject {
    /// A key that changes whenever the pane has to start over.
    pub fn key(&self) -> String {
        match self {
            Self::Project { id, .. } => format!("project:{id}"),
            Self::Local { dir, file } => format!("local:{}", dir.join(file).display()),
            Self::Nothing(_) => "nothing".to_owned(),
        }
    }
}

/// Whether a list shows the detail pane, and the keyboard focus that goes with it.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Pane {
    open: State<bool>,
    list: Cursor,
    store: Store,
}

pub(super) fn use_pane(store: Store, list: Cursor) -> Pane {
    Pane { open: use_state(|| false), list, store }
}

impl Pane {
    /// For a render: subscribes to the pane opening and closing.
    pub fn shown(&self) -> bool {
        *self.open.read()
    }

    /// For an event handler.
    pub fn is_open(&self) -> bool {
        self.open.got().unwrap_or(false)
    }

    /// Opens the pane on `file`; in a wide window the list keeps the keyboard, in a narrow one the pane takes it.
    pub fn show(&self, file: &str) {
        self.list.stand(file);
        self.open.soft(|o| *o = true);
        if self.store.ui.window.peek().0 >= page::SPLIT_AT {
            self.list.focus.request_focus();
        }
    }

    /// What the palette calls the command of [`Pane::toggle`].
    pub fn label(&self) -> String {
        if self.shown() { tr("Details ausblenden") } else { tr("Details anzeigen") }
    }

    /// Closes the pane, or opens it on the row under the cursor; with no such row it says so.
    pub fn toggle(&self) {
        if self.is_open() {
            self.close();
            return;
        }
        match self.list.current() {
            Some(file) => self.show(&file),
            None => {
                toast::show(self.store.ui, tr("Wähle zuerst einen Eintrag in der Liste."), None);
            }
        }
    }

    /// Closes the pane and gives the keyboard back to the list.
    pub fn close(&self) {
        self.open.soft(|o| *o = false);
        let focus = self.list.focus;
        spawn_in_window(async move {
            after(REFOCUS).await;
            focus.request_focus();
        });
    }
}

enum Fetch {
    Loading,
    Project(Rc<Body>),
    Local(Rc<Option<About>>),
    Failed(Problem),
}

/// The pane of the selected row, to the right of the list or over it; `id` names its controls for the drive tool.
#[derive(Clone, PartialEq)]
pub(super) struct Detail {
    pub id: &'static str,
    pub subject: Subject,
    pub narrow: bool,
    pub back: String,
    pub close: EventHandler<()>,
    /// Opens the project in the browser with its versions and gallery.
    pub versions: Option<EventHandler<()>>,
}

#[derive(Clone)]
struct Pending {
    subject: Subject,
    fetch: State<Fetch>,
}

impl Pending {
    fn run(&self) {
        let (subject, mut fetch) = (self.subject.clone(), self.fetch);
        let _ = fetch.try_write().map(|mut f| *f = Fetch::Loading);
        spawn_in_window(async move {
            after(SETTLE).await;
            if fetch.try_peek().is_none() {
                return;
            }
            let loaded = match subject {
                Subject::Project { id, .. } => match off_ui(move || readme::fetch(&id)).await {
                    Some(Ok(body)) => Fetch::Project(Rc::new(body)),
                    Some(Err(e)) => Fetch::Failed(readme::problem_of(&e)),
                    None => Fetch::Failed(Problem::plain(crate::ui::forms::unexplained())),
                },
                Subject::Local { dir, file } => match off_ui(move || modmeta::about(&crate::core::content::jar_path(&dir, &file))).await {
                    Some(Ok(about)) => Fetch::Local(Rc::new(about)),
                    Some(Err(e)) => Fetch::Failed(crate::ui::forms::explain_as(tr("Die Datei ließ sich nicht lesen."), &e)),
                    None => Fetch::Failed(Problem::plain(crate::ui::forms::unexplained())),
                },
                Subject::Nothing(_) => return,
            };
            if let Some(mut f) = fetch.try_write() {
                *f = loaded;
            }
        });
    }
}

impl Component for Detail {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let id = self.id;
        let fetch = use_state(|| Fetch::Loading);
        let details = use_state(|| false);
        let pending = Pending { subject: self.subject.clone(), fetch };
        let start = pending.clone();
        use_hook(move || start.run());

        let close = self.close.clone();
        let esc = Shortcut::bare(Code::Escape).label();
        let button = if self.narrow {
            let tip = trf("Zurück · %@", &[&esc]);
            Button::new(format!("{id}.back"), self.back.clone()).plain().glyph(Icon::ChevronLeft).tip(tip).on_press(move |_| close.call(())).into_element()
        } else {
            let tip = trf("Schließen · %@", &[&esc]);
            Button::icon(format!("{id}.close"), Icon::Close, tr("Schließen")).plain().tip(tip).on_press(move |_| close.call(())).into_element()
        };

        let (title, facts) = match (&self.subject, &*fetch.read()) {
            (Subject::Project { title, .. }, Fetch::Project(body)) => (title.clone(), vec![trf("%@ Downloads", &[&downloads_short(body.details.downloads)])]),
            (Subject::Project { title, .. }, _) => (title.clone(), Vec::new()),
            (Subject::Local { file, .. }, Fetch::Local(about)) => match about.as_ref() {
                Some(a) => {
                    let mut facts = Vec::new();
                    if !a.version.is_empty() {
                        facts.push(a.version.clone());
                    }
                    if !a.authors.is_empty() {
                        facts.push(trf("von %@", &[&a.authors.join(", ")]));
                    }
                    (a.name.clone(), facts)
                }
                None => (file.clone(), Vec::new()),
            },
            (Subject::Local { file, .. }, _) => (file.clone(), Vec::new()),
            (Subject::Nothing(_), _) => (String::new(), Vec::new()),
        };
        let mut heading = rect().width(Size::flex(1.)).spacing(space::XXS);
        if !title.is_empty() {
            heading = heading.child(crate::view::kit::text::strong(&t, title, font::TITLE3, t.text1).width(Size::fill()).max_lines(2));
        }
        if !facts.is_empty() {
            heading = heading.child(caption(&t, facts.join(" · ")).width(Size::fill()).max_lines(2));
        }
        let header = if self.narrow {
            rect()
                .width(Size::fill())
                .child(rect().width(Size::fill()).padding((space::XS, space::XS, 0., space::XS)).horizontal().child(button))
                .child(rect().width(Size::fill()).padding((space::S, space::M)).horizontal().content(Content::flex()).child(heading))
        } else {
            rect().width(Size::fill()).padding((space::S, space::XS, space::S, space::M)).horizontal().content(Content::flex()).spacing(space::S).child(heading).child(button)
        };

        let content: Element = match (&self.subject, &*fetch.read()) {
            (Subject::Nothing(text), _) => readme::quiet(&t, text.clone()),
            (_, Fetch::Loading) => readme::loading(&t),
            (_, Fetch::Failed(why)) => {
                let retry = pending.clone();
                rect()
                    .width(Size::fill())
                    .spacing(space::S)
                    .child(problem_view(&t, &format!("{id}.error"), Icon::WifiOff, why, details))
                    .child(Button::new(format!("{id}.retry"), tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| retry.run()))
                    .into_element()
            }
            (Subject::Project { kind, .. }, Fetch::Project(body)) => {
                let first = self.versions.clone().map(|open| Button::new(format!("{id}.versions"), tr("Versionen und Galerie")).glyph(Icon::Layers).on_press(move |_| open.call(())).into_element());
                readme::view(&t, body, &readme::page_url(*kind, &body.details.slug), id, first)
            }
            (Subject::Local { file, .. }, Fetch::Local(about)) => local(&t, id, file, about.as_ref().as_ref()),
            _ => readme::loading(&t),
        };

        let focus = use_a11y();
        let esc_close = self.close.clone();
        rect()
            .a11y_id(focus)
            .a11y_focusable(true)
            .a11y_auto_focus(self.narrow)
            .a11y_role(AccessibilityRole::Group)
            .a11y_alt(tr("Details"))
            .width(Size::fill())
            .height(Size::fill())
            .content(Content::flex())
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if matches!(e.key, Key::Named(NamedKey::Escape)) && e.modifiers.is_empty() {
                    e.stop_propagation();
                    esc_close.call(());
                }
            })
            .child(header)
            .child(ScrollView::new().width(Size::fill()).height(Size::flex(1.)).scroll_with_arrows(false).child(rect().width(Size::fill()).padding((0., space::M, space::M, space::M)).child(page::reading(content))))
    }
}

fn local(t: &crate::view::theme::Theme, id: &str, file: &str, about: Option<&About>) -> Element {
    let mut column = rect().width(Size::fill()).spacing(space::M).child(caption(t, tr("Nicht von Modrinth. Die Angaben stehen in der Datei.")).width(Size::fill()));
    let Some(about) = about else {
        return column.child(readme::quiet(t, tr("Die Datei enthält keine Beschreibung."))).into_element();
    };
    if about.description.is_empty() {
        column = column.child(readme::quiet(t, tr("Die Datei enthält keine Beschreibung.")));
    } else {
        column = column.child(para(t, about.description.clone(), font::BODY, t.text1));
    }
    if let Some(url) = about.homepage.clone().filter(|u| u.starts_with("https://")) {
        column = column.child(rect().horizontal().child(Button::new(format!("{id}.link.homepage"), tr("Webseite")).glyph(Icon::External).on_press(move |_| open_external("homepage", url.clone()))));
    }
    column.child(caption(t, file.to_owned()).width(Size::fill())).into_element()
}

/// `list` with `pane` to its right in a wide window, or `pane` alone over it in a narrow one; without a pane the list keeps the same place in the tree, so opening one does not rebuild it.
pub(super) fn split(t: &crate::view::theme::Theme, window: f32, list: impl IntoElement, pane: Option<Detail>) -> Element {
    use crate::view::theme::frame;
    let card = pane.map(|pane| {
        let pane = rect().key(pane.subject.key()).width(Size::fill()).height(Size::fill()).child(pane);
        rect().width(Size::fill()).height(Size::fill()).background(t.raised).corner_radius(t.shape.tile).overflow(Overflow::Clip).child(pane)
    });
    if window < page::SPLIT_AT
        && let Some(card) = card
    {
        return rect().width(Size::fill()).height(Size::fill()).padding(frame::GUTTER).child(card).into_element();
    }
    rect()
        .width(Size::fill())
        .height(Size::fill())
        .horizontal()
        .content(Content::flex())
        .child(rect().width(Size::flex(1.)).height(Size::fill()).content(Content::flex()).child(list))
        .maybe_child(card.map(|card| rect().width(Size::flex(1.)).height(Size::fill()).padding((frame::GUTTER, frame::GUTTER, frame::GUTTER, 0.)).child(card)))
        .into_element()
}