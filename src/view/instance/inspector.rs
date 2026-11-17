use freya::prelude::*;
use uuid::Uuid;

use super::{Facts, actions, links, uptime_text, use_minute};
use crate::app::event::Event as AppEvent;
use crate::core::companion;
use crate::core::i18n::{tr, trf};
use crate::core::install;
use crate::core::instance::{Instance, Loader};
use crate::ui::forms;
use crate::ui::remote;
use crate::view::commands::{Shortcut, Want, want_blocked};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Face, Item, Menu};
use crate::view::kit::text::{caption, hairline, line, spacer};
use crate::view::kit::{Bounds, after, off_ui, ring, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};
use crate::view::ui::{Filter, Page};


#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Worlds {
    count: usize,
    last: Option<i64>,
}

fn played(ms: i64) -> Option<String> {
    Some(crate::ui::instance_files::when(ms / 1000)).filter(|s| !s.is_empty())
}

fn status(t: &Theme, store: Store, f: &Facts, id: &str) -> (Color, String) {
    if let Some(b) = store.ui.instance.busy.read().get(id) {
        let text = match b.progress {
            Some(done) => trf("Wird gesichert … %lld %", &[&((done * 100.).round() as u32)]),
            None => b.text.clone(),
        };
        return (t.accent, text);
    }
    if let Some((_, overdue)) = f.stopping {
        return (t.attention, if overdue { tr("Beendet sich nicht") } else { tr("Wird gestoppt…") });
    }
    if let Some(trouble) = &f.trouble {
        return (t.danger, trouble.clone());
    }
    let up = store.ui.instance.up.read();
    let since = f.stoppable.iter().filter_map(|(key, _)| up.get(key)).min().copied();
    match (f.stoppable.len(), since.and_then(uptime_text)) {
        (0, _) if f.running => (t.success, f.busy.first().map_or_else(|| tr("Läuft im Batch"), |(_, place)| trf("Läuft in %@", &[place]))),
        (0, _) => (t.text3, tr("Gestoppt")),
        (1, None) => (t.success, tr("Läuft")),
        (1, Some(up)) => (t.success, trf("Läuft · %@", &[&up])),
        (n, None) => (t.success, trf("%lld laufen", &[&n])),
        (n, Some(up)) => (t.success, trf("%1$lld laufen · %2$@", &[&n, &up])),
    }
}

fn status_line(t: &Theme, color: Color, text: String) -> impl IntoElement {
    rect().horizontal().cross_align(Alignment::Center).spacing(space::XS + space::XXS).child(dot(color)).child(line(t, text, font::CALLOUT, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis))
}

fn status_row(t: &Theme, store: Store, f: &Facts, color: Color, text: String) -> Element {
    let row = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(rect().width(Size::flex(1.)).child(status_line(t, color, text))).content(Content::flex());
    if f.trouble.is_none() {
        return row.into_element();
    }
    let mut tab = store.ui.instance.tab;
    row.child(Button::new("instance.logs", tr("Protokoll ansehen")).plain().on_press(move |_| tab.set(super::Tab::Logs))).into_element()
}

fn batch_links(t: &Theme, batches: &[(String, String)]) -> Option<Element> {
    if batches.is_empty() {
        return None;
    }
    let intro = if batches.len() == 1 { tr("In 1 Batch:") } else { trf("In %lld Batches:", &[&batches.len()]) };
    let mut row = rect().horizontal().width(Size::fill()).cross_align(Alignment::Center).spacing(space::XS).child(line(t, intro, font::CALLOUT, t.text2));
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

fn fact(t: &Theme, label: String, value: String) -> Element {
    rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .child(line(t, label, font::CALLOUT, t.text2))
        .child(spacer())
        .child(line(t, value, font::CALLOUT, t.text1).max_lines(1).text_overflow(TextOverflow::Ellipsis))
        .content(Content::flex())
        .into_element()
}

fn loader_value(made: &Instance) -> String {
    match (&made.loader, made.loader.version()) {
        (Loader::Vanilla, _) => tr("Vanilla"),
        (l, Some(v)) => format!("{} {v}", l.name()),
        (l, None) => trf("%@ · neueste", &[&l.name()]),
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

fn use_worlds(store: Store, made: &Instance) -> Option<Worlds> {
    let mut found = use_state(|| None::<(String, Worlds)>);
    let changed = *store.ui.instance.changed.read();
    let deps = (made.id.clone(), changed);
    let dir = made.dir();
    use_side_effect_with_deps(&deps, move |(id, _)| {
        let (id, dir) = (id.clone(), dir.clone());
        spawn(async move {
            let read = off_ui(move || {
                let list = crate::core::worlds::list(&dir);
                Worlds { count: list.len(), last: list.iter().filter_map(|w| w.level.last_played).max() }
            })
            .await;
            if let Some(w) = read {
                found.set(Some((id, w)));
            }
        });
    });
    found.read().as_ref().filter(|(id, _)| *id == made.id).map(|(_, w)| *w)
}

/// The page's notices for instance `id`: why its last change was not saved, with a button that hides it.
pub fn notices(t: &Theme, store: Store, id: &str) -> Vec<Element> {
    let nav = store.ui.instance;
    let mut out = Vec::new();
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

/// The column beside the tabs: what runs, the facts and what to do with the instance.
#[derive(PartialEq)]
pub struct Inspector {
    pub id: String,
}

impl Component for Inspector {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let f = super::facts(&store.model.read(), &self.id);
        use_minute(store, true);
        let fallback = Instance::new("", "", Loader::Vanilla);
        let made = f.as_ref().map_or(&fallback, |f| &f.made).clone();
        let news = use_loader_news(&made);
        let worlds = use_worlds(store, &made);
        let Some(f) = f else { return rect().into_element() };
        let (color, text) = status(&t, store, &f, &self.id);

        let mut col = rect().width(Size::fill()).padding(space::L).spacing(space::M);
        col = col.child(status_row(&t, store, &f, color, text));
        if let Some(links) = batch_links(&t, &f.batches) {
            col = col.child(links);
        }
        for n in notices(&t, store, &self.id) {
            col = col.child(n);
        }
        col = col.child(hairline(&t));
        let mut list = rect().width(Size::fill()).spacing(space::S);
        list = list.child(fact(&t, tr("Minecraft"), f.made.game_version.clone())).child(fact(&t, tr("Loader"), loader_value(&f.made)));
        if f.made.loader.takes_mods() {
            list = list.child(fact(&t, tr("Mods"), (f.made.mods.len() + f.made.local_mods.len()).to_string()));
        }
        if let Some(w) = worlds {
            list = list.child(fact(&t, tr("Welten"), w.count.to_string()));
            if let Some(text) = w.last.and_then(played) {
                list = list.child(fact(&t, tr("Zuletzt gespielt"), text));
            }
        }
        list = list.child(fact(&t, tr("Arbeitsspeicher"), forms::memory_label(f.made.memory())));
        col = col.child(list);
        if let Some(newest) = news {
            let (id, to) = (f.made.id.clone(), newest.clone());
            col = col.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::XS)
                    .child(icon(&t, Icon::Download, iconsize::S, t.accent_text))
                    .child(line(&t, trf("%@ %@ ist verfügbar.", &[&f.made.loader.name(), &newest]), font::CALLOUT, t.text1).width(Size::flex(1.)))
                    .content(Content::flex())
                    .child(Button::new("instance.loader.update", tr("Aktualisieren")).plain().on_press(move |_| actions::ask_loader(store, &id, &to))),
            );
        }
        if let Some(hint) = companion_hint(&f.made.game_version) {
            col = col.child(caption(&t, hint));
        }
        col = col.child(hairline(&t));
        let mut acts = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::XS);
        for a in actions::all(store, &f).into_iter().filter(|a| matches!(a.key, "folder" | "clone")) {
            let run = a.run.clone();
            acts = acts.child(Button::new(format!("instance.{}", a.key), a.label).glyph(a.glyph).plain().disabled(a.disabled).on_press(move |_| run.call(())));
        }
        acts = acts.child(More { facts: f.clone(), rename: false, all: false });
        col = col.child(acts);
        ScrollView::new().width(Size::fill()).height(Size::fill()).drag_scrolling(false).child(col).into_element()
    }
}

/// The inspector folded into one line above the tabs, for narrow windows: status and a menu of actions.
#[derive(PartialEq)]
pub struct Strip {
    pub id: String,
}

impl Component for Strip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        use_minute(store, true);
        let Some(f) = super::facts(&store.model.read(), &self.id) else { return rect().into_element() };
        let (color, text) = status(&t, store, &f, &self.id);
        rect()
            .width(Size::fill())
            .height(Size::px(control::L + space::S * 2.))
            .padding((0., space::L))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .border(Border::new().fill(t.separator).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner))
            .child(rect().width(Size::flex(1.)).child(status_row(&t, store, &f, color, text)))
            .content(Content::flex())
            .child(More { facts: f.clone(), rename: true, all: true })
            .into_element()
    }
}
