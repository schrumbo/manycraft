use freya::prelude::*;

use super::{Facts, actions, links, use_minute};
use crate::core::companion;
use crate::core::i18n::{tr, trf};
use crate::core::install;
use crate::core::instance::{Instance, Loader};
use crate::ui::remote;
use crate::view::commands::Shortcut;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::{caption, line};
use crate::view::kit::{Bounds, off_ui, ring, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, iconsize, space};
use crate::view::ui::{Filter, Page};


fn status(t: &Theme, store: Store, f: &Facts, id: &str) -> (Color, String) {
    if let Some(b) = store.ui.instance.busy.read().get(id) {
        let text = match b.progress {
            Some(done) => trf("Wird gesichert … %lld %", &[&((done * 100.).round() as u32)]),
            None => b.text.clone(),
        };
        return (t.accent, text);
    }
    let up = store.ui.instance.up.read();
    let since = f.stoppable.iter().filter_map(|(key, _)| up.get(key)).min().copied();
    if let Some(text) = super::running_line(f, since) {
        return (t.success, text);
    }
    if let Some((_, overdue)) = f.stopping {
        return (t.attention, if overdue { tr("Beendet sich nicht") } else { tr("Stoppt …") });
    }
    if !f.launching.is_empty() {
        return match f.loading {
            Some(p) => (t.accent, trf("Wird installiert · %lld %", &[&p])),
            None => (t.accent, tr("Startet …")),
        };
    }
    if let Some(trouble) = f.trouble.as_ref().filter(|_| f.ended == Some(super::Ended::SignIn)) {
        return (t.danger, trouble.clone());
    }
    if f.running {
        return (t.success, f.busy.first().map_or_else(|| tr("Läuft im Batch"), |(_, place)| trf("Von Batch %@ genutzt", &[place])));
    }
    (t.text3, tr("Gestoppt"))
}

fn status_line(t: &Theme, color: Color, text: String) -> impl IntoElement {
    rect().horizontal().cross_align(Alignment::Center).spacing(space::XS + space::XXS).child(dot(color)).child(line(t, text, font::CALLOUT, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis))
}

fn status_row(t: &Theme, color: Color, text: String) -> Element {
    rect().width(Size::fill()).child(status_line(t, color, text)).into_element()
}

fn batch_links(t: &Theme, batches: &[(String, String)]) -> Option<Element> {
    if batches.is_empty() {
        return None;
    }
    let intro = if batches.len() == 1 { tr("In 1 Batch:") } else { trf("In %lld Batches:", &[&batches.len()]) };
    let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS).child(line(t, intro, font::CALLOUT, t.text2));
    for (id, name) in batches {
        row = row.child(Link { id: id.clone(), name: name.clone(), to: Page::Grid(Filter::Batch(id.clone())) });
    }
    Some(row.into_element())
}

#[derive(PartialEq)]
struct Link {
    id: String,
    name: String,
    to: Page,
}

impl Component for Link {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let spot = use_spot(&format!("instance.batch.link.{}", self.id), "link", &self.name, true);
        let mut page = store.ui.page;
        let to = self.to.clone();
        let keys = self.to.clone();
        let mut link = rect()
            .a11y_id(a11y)
            .a11y_role(AccessibilityRole::Link)
            .a11y_focusable(true)
            .a11y_alt(self.name.clone())
            .padding((0., space::XXS))
            .corner_radius(t.shape.small)
            .background(if *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_all_press(move |_| page.set(to.clone()))
            .on_key_down(move |e: Event<KeyboardEventData>| {
                let go = matches!(&e.key, Key::Named(NamedKey::Enter)) || matches!(&e.key, Key::Character(c) if c == " ");
                if a11y.is_focused() && go {
                    e.stop_propagation();
                    page.set(keys.clone());
                }
            })
            .child(line(&t, self.name.clone(), font::CALLOUT, t.accent_text).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        if let Some(border) = ring(&t, focus()) {
            link = link.border(border);
        }
        link
    }
}

fn use_loader_news(made: &Instance) -> Option<String> {
    let mut news = use_state(|| None::<(String, String)>);
    let key = format!("{}|{}|{}|{}", made.id, made.game_version, made.loader.name(), made.loader.version().unwrap_or_default());
    let pinned = made.loader.version().map(str::to_owned).filter(|_| matches!(made.loader, Loader::Fabric(_) | Loader::Quilt(_)));
    let (loader, game) = (made.loader.clone(), made.game_version.clone());
    use_side_effect_with_deps(&key, move |key| {
        let Some(current) = pinned.clone() else { return };
        let (key, loader, game) = (key.clone(), loader.clone(), game.clone());
        spawn(async move {
            if let Some(Ok(Some(newest))) = off_ui(move || remote::newest_loader(&loader, &game)).await
                && install::is_newer(&newest, &current)
            {
                news.set(Some((key, newest)));
            }
        });
    });
    news.read().as_ref().filter(|(k, _)| *k == key).map(|(_, v)| v.clone())
}

/// The page's notices for instance `id`: why its last change was not saved, with a button that hides it.
pub fn notices(t: &Theme, store: Store, id: &str) -> Vec<Element> {
    let nav = store.ui.instance;
    let mut out = Vec::new();
    if super::facts(&store.model.read(), id).is_some_and(|f| matches!(f.ended, Some(super::Ended::Crashed | super::Ended::Failed))) {
        out.push(rect().width(Size::fill()).child(super::crash::Banner { id: id.to_owned() }).into_element());
    }
    if let Some(e) = nav.error.read().get(id).cloned() {
        let mut error = nav.error;
        let gone = id.to_owned();
        out.push(
            rect()
                .width(Size::fill())
                .horizontal()
                .spacing(space::XS)
                .child(icon(t, Icon::Alert, iconsize::S, t.danger_text))
                .child(line(t, e, font::CALLOUT, t.danger_text).width(Size::flex(1.)))
                .content(Content::flex())
                .child(Button::icon("instance.error.close", Icon::Close, tr("Hinweis ausblenden")).plain().on_press(move |_| {
                    error.write().remove(&gone);
                }))
                .into_element(),
        );
    }
    out
}

fn companion_hint(game: &str) -> Option<String> {
    (!game.is_empty() && !companion::available(game)).then(|| trf("Hintergrundmodus ab %@", &[&companion::first()]))
}

/// Every action of the instance's "…" menu, with Rename first when `rename`; Open Folder and Clone too when `all`.
fn more_items(store: Store, f: &Facts, rename: bool, all: bool) -> Vec<Item> {
    let mut items = Vec::new();
    if rename {
        let id = f.made.id.clone();
        items.push(Item::new("instance.more.rename", tr("Umbenennen"), move |_| actions::begin_rename(store, &id)).shortcut(Shortcut::bare(Code::F2).label()));
        items.push(Item::Separator);
    }
    let new = links::new_batch(store, &f.made.id, &f.made.name);
    let run = new.run.clone();
    let mut acts = actions::all(store, f);
    if !all {
        acts.retain(|a| !matches!(a.key, "folder" | "clone"));
    }
    let split = acts.iter().position(|a| a.key == "reset").unwrap_or(acts.len());
    let mut new = Some(because(Item::new("instance.more.batch.new", tr("Neuer Batch…"), move |_| run.call(())), new.disabled));
    for (n, a) in acts.into_iter().enumerate() {
        if n == split {
            items.extend(new.take());
            items.push(Item::Separator);
        }
        let run = a.run.clone();
        let mut item = because(Item::new(format!("instance.more.{}", a.key), a.label, move |_| run.call(())), a.disabled);
        if a.danger {
            item = item.danger();
        }
        items.push(item);
    }
    items.extend(new);
    items
}

fn because(item: Item, why: Option<String>) -> Item {
    match why {
        Some(why) => item.shortcut(why).disabled(true),
        None => item,
    }
}

#[derive(PartialEq)]
struct More {
    facts: Facts,
    rename: bool,
    all: bool,
}

impl Component for More {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut anchor = use_state(Bounds::default);
        let (f, rename, all) = (self.facts.clone(), self.rename, self.all);
        rect().on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e))).child(Button::icon("instance.more", Icon::More, tr("Weitere Aktionen")).plain().on_press(move |_| {
            menu::open(store.ui, Menu::under(more_items(store, &f, rename, all), *anchor.peek()));
        }))
    }
}

/// The line above the tabs: what the instance is doing, the batches it is in, news for its loader and a menu of actions.
#[derive(PartialEq)]
pub struct Strip {
    pub id: String,
}

impl Component for Strip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        use_minute(store, true);
        let f = super::facts(&store.model.read(), &self.id);
        let fallback = Instance::new("", "", Loader::Vanilla);
        let news = use_loader_news(f.as_ref().map_or(&fallback, |f| &f.made));
        let Some(f) = f else { return rect().into_element() };
        let (color, text) = status(&t, store, &f, &self.id);
        let mut left = rect().width(Size::flex(1.)).horizontal().cross_align(Alignment::Center).spacing(space::L).child(status_row(&t, color, text));
        if let Some(links) = batch_links(&t, &f.batches) {
            left = left.child(links);
        }
        if let Some(hint) = companion_hint(&f.made.game_version) {
            left = left.child(caption(&t, hint));
        }
        let mut strip = rect()
            .width(Size::fill())
            .height(Size::px(control::L + space::S * 2.))
            .padding((0., space::L))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .border(Border::new().fill(t.separator).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner))
            .child(left)
            .content(Content::flex());
        if let Some(newest) = news {
            let (id, to) = (f.made.id.clone(), newest.clone());
            strip = strip
                .child(icon(&t, Icon::Download, iconsize::S, t.accent_text))
                .child(line(&t, trf("%@ %@ ist verfügbar.", &[&f.made.loader.name(), &newest]), font::CALLOUT, t.text1).max_lines(1))
                .child(Button::new("instance.loader.update", tr("Aktualisieren")).plain().on_press(move |_| actions::ask_loader(store, &id, &to)));
        }
        strip.child(More { facts: f.clone(), rename: true, all: true }).into_element()
    }
}