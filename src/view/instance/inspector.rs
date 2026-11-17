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

const ACCOUNTS_SEARCH_FROM: usize = 6;
const PREVIEW_W: f32 = super::INSPECTOR - space::L * 2.;

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
        (0, _) if f.running => (t.success, tr("Läuft")),
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

fn face_of(m: &crate::app::model::Model, id: Uuid) -> Option<Face> {
    m.account(id).map(|a| Face { id, name: a.name.clone(), color: a.color() })
}

fn play_menu(store: Store, f: &Facts) -> Vec<Item> {
    let m = store.model.peek();
    let several = f.accounts.len() > 1;
    let mut items = Vec::new();
    for (account, name) in f.accounts.iter().filter(|_| several) {
        let (instance, account_id) = (f.made.id.clone(), *account);
        let mut item = if f.playing.contains(account) {
            Item::new(format!("instance.play.as.{account}"), name.clone(), |_| {}).shortcut(tr("Läuft")).disabled(true)
        } else if f.expired.contains(account) {
            let blocked = crate::view::commands::sign_in_blocked(store);
            Item::new(format!("instance.play.as.{account}"), name.clone(), move |_| crate::view::accounts::sign_in(store, vec![account_id])).disabled(blocked.is_some()).shortcut(blocked.unwrap_or_else(|| tr("Anmelden")))
        } else {
            let item = Item::new(format!("instance.play.as.{account}"), name.clone(), move |_| store.dispatch(AppEvent::PlayInstance { instance: instance.clone(), account: Some(account_id) }));
            if f.pinned && f.solo == Some(*account) { item.shortcut(tr("Standard")) } else { item }
        };
        if let Some(face) = face_of(&m, *account) {
            item = item.face(face);
        }
        items.push(item.checked(f.solo == Some(*account)));
    }
    if let Some(solo) = f.solo.filter(|_| several) {
        let instance = f.made.id.clone();
        items.push(Item::Separator);
        items.push(if f.pinned {
            Item::new("instance.play.default.clear", tr("Standard-Account entfernen"), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account: None }))
        } else {
            Item::new("instance.play.default.set", tr("Als Standard für diese Instanz"), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account: Some(solo) }))
        });
    }
    if f.stoppable.len() > 1 {
        items.push(Item::Separator);
        for (key, name) in &f.stoppable {
            let key = key.clone();
            items.push(Item::new(format!("instance.play.stop.{key}"), trf("„%@“ stoppen", &[name]), move |_| store.dispatch(AppEvent::Stop(vec![key.clone()]))));
        }
    }
    items
}

#[derive(PartialEq)]
struct PlayControl {
    facts: Facts,
}

impl Component for PlayControl {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut anchor = use_state(Bounds::default);
        let f = &self.facts;
        let mut ui = store.ui;
        if f.accounts.is_empty() {
            return rect().child(Button::new("instance.play", tr("Account hinzufügen")).glyph(Icon::Plus).primary().on_press(move |_| ui.page.set(Page::Accounts))).into_element();
        }
        let main = if let Some((key, overdue)) = f.stopping.clone() {
            if overdue {
                Button::new("instance.play", tr("Sofort beenden")).glyph(Icon::Stop).danger().tip(trf("Nicht Gespeichertes geht verloren (%@)", &[&Shortcut::primary(Code::Period).label()])).on_press(move |_| store.dispatch(AppEvent::Stop(vec![key.clone()])))
            } else {
                Button::new("instance.play", tr("Wird gestoppt…")).glyph(Icon::Stop).disabled(Some(tr("Das Spiel speichert und beendet sich.")))
            }
        } else if !f.stoppable.is_empty() {
            let keys: Vec<String> = f.stoppable.iter().map(|(k, _)| k.clone()).collect();
            let label = if keys.len() == 1 { tr("Stoppen") } else { tr("Alle stoppen") };
            Button::new("instance.play", label).glyph(Icon::Stop).tip(Shortcut::primary(Code::Period).label()).on_press(move |_| store.dispatch(AppEvent::Stop(keys.clone())))
        } else {
            let name = f.solo.and_then(|a| f.accounts.iter().find(|(id, _)| *id == a)).map(|(_, n)| n.clone());
            let (instance, solo) = (f.made.id.clone(), f.solo);
            match (name, solo.filter(|a| f.expired.contains(a))) {
                (Some(n), Some(account)) => Button::new("instance.play", trf("Anmelden als %@", &[&n]))
                    .glyph(Icon::LogIn)
                    .primary()
                    .tip(tr("Die Anmeldung ist abgelaufen."))
                    .disabled(want_blocked(&Want::SignInAgain(Vec::new())))
                    .on_press(move |_| crate::view::accounts::sign_in(store, vec![account])),
                (name, _) => {
                    let label = name.map_or_else(|| tr("Spielen"), |n| trf("Spielen als %@", &[&n]));
                    Button::new("instance.play", label).glyph(Icon::Play).primary().tip(Shortcut::primary(Code::KeyR).label()).on_press(move |_| store.dispatch(AppEvent::PlayInstance { instance: instance.clone(), account: solo }))
                }
            }
        };
        let menu_facts = f.clone();
        let items = play_menu(store, f);
        if items.is_empty() {
            return rect().child(main).into_element();
        }
        let label = tr("Anderer Account");
        let mut more = Button::icon("instance.play.more", Icon::ChevronDown, label.clone()).tip(label);
        if f.stoppable.is_empty() && f.stopping.is_none() {
            more = more.primary();
        }
        rect()
            .horizontal()
            .spacing(1.)
            .on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e)))
            .child(main)
            .child(more.on_press(move |_| {
                let menu = Menu::under(play_menu(store, &menu_facts), *anchor.peek());
                menu::open(store.ui, if menu_facts.accounts.len() >= ACCOUNTS_SEARCH_FROM { menu.searchable(tr("Account suchen")) } else { menu });
            }))
            .into_element()
    }
}

#[derive(PartialEq)]
struct BatchControl {
    id: String,
    name: String,
    batches: Vec<(String, String)>,
}

impl Component for BatchControl {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut anchor = use_state(Bounds::default);
        let new = links::new_batch(store, &self.id, &self.name);
        if self.batches.is_empty() {
            let run = new.run.clone();
            return rect().child(Button::new("instance.batch.new", tr("Neuer Batch…")).disabled(new.disabled).on_press(move |_| run.call(()))).into_element();
        }
        let batches = self.batches.clone();
        let (new_label, new_off, new_run) = (new.label, new.disabled.is_some(), new.run);
        rect()
            .on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e)))
            .child(Button::new("instance.batch.start", tr("Batch starten")).glyph(Icon::ChevronDown).on_press(move |_| {
                let mut items: Vec<Item> = batches.iter().map(|(id, name)| {
                    let id = id.clone();
                    Item::new(format!("instance.batch.start.{id}"), name.clone(), move |_| store.dispatch(AppEvent::StartBatch(id.clone())))
                }).collect();
                let run = new_run.clone();
                items.push(Item::Separator);
                items.push(Item::new("instance.batch.new", new_label.clone(), move |_| run.call(())).disabled(new_off));
                menu::open(store.ui, Menu::under(items, *anchor.peek()));
            }))
            .into_element()
    }
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

#[derive(PartialEq)]
struct Preview {
    facts: Facts,
    busy: Option<f32>,
}

impl Component for Preview {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let f = &self.facts;
        let h = PREVIEW_W * 9. / 16.;
        let (glyph, text) = if !f.playing.is_empty() {
            (Icon::Play, tr("Läuft in eigenem Fenster"))
        } else if f.running {
            (Icon::Layers, tr("Läuft im Batch"))
        } else {
            (Icon::Moon, tr("Gestoppt"))
        };
        let mut pic = rect().width(Size::px(PREVIEW_W)).height(Size::px(h)).background(t.well).corner_radius(t.shape.tile).overflow(Overflow::Clip).center().spacing(space::XS).child(icon(&t, glyph, iconsize::L, t.text3)).child(caption(&t, text));
        if let Some(done) = self.busy {
            pic = pic.child(rect().position(Position::new_absolute().left(0.).bottom(0.)).width(Size::px(PREVIEW_W * done)).height(Size::px(frame::PROGRESS)).background(t.accent));
        }
        pic
    }
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

/// The column beside the tabs: the live picture, what runs, Play, the facts and what to do with the instance.
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
        let busy = store.ui.instance.busy.read().get(&self.id).and_then(|b| b.progress);

        let mut col = rect().width(Size::fill()).padding(space::L).spacing(space::M);
        col = col.child(Preview { facts: f.clone(), busy }).child(status_row(&t, store, &f, color, text));
        col = col.child(PlayControl { facts: f.clone() }).child(BatchControl { id: f.made.id.clone(), name: f.made.name.clone(), batches: f.batches.clone() });
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
        list = list.child(fact(&t, tr("Arbeitsspeicher"), forms::memory_label(f.made.memory_mb)));
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
        let mut acts = rect().width(Size::fill()).spacing(space::XXS);
        let all = actions::all(store, &f);
        let split = all.iter().position(|a| a.key == "reset").unwrap_or(all.len());
        for (n, a) in all.into_iter().enumerate() {
            if n == split {
                acts = acts.child(rect().height(Size::px(space::S)));
            }
            let run = a.run.clone();
            let mut b = Button::new(format!("instance.{}", a.key), a.label).glyph(a.glyph).plain().disabled(a.disabled);
            if a.danger {
                b = b.danger();
            }
            acts = acts.child(b.on_press(move |_| run.call(())));
        }
        col = col.child(acts);
        ScrollView::new().width(Size::fill()).height(Size::fill()).drag_scrolling(false).child(col).into_element()
    }
}

/// The inspector folded into one line above the tabs, for narrow windows: status, Batch, Play and a menu of actions.
#[derive(PartialEq)]
pub struct Strip {
    pub id: String,
}

impl Component for Strip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut anchor = use_state(Bounds::default);
        use_minute(store, true);
        let Some(f) = super::facts(&store.model.read(), &self.id) else { return rect().into_element() };
        let (color, text) = status(&t, store, &f, &self.id);
        let menu_facts = f.clone();
        let mut bar = rect()
            .width(Size::fill())
            .height(Size::px(control::L + space::S * 2.))
            .padding((0., space::L))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .border(Border::new().fill(t.separator).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner))
            .child(rect().width(Size::flex(1.)).child(status_row(&t, store, &f, color, text)))
            .content(Content::flex())
            .child(BatchControl { id: f.made.id.clone(), name: f.made.name.clone(), batches: f.batches.clone() })
            .child(PlayControl { facts: f.clone() });
        bar = bar.child(rect().on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e))).child(Button::icon("instance.more", Icon::More, tr("Weitere Aktionen")).plain().tip(tr("Weitere Aktionen")).on_press(move |_| {
            let rename_id = menu_facts.made.id.clone();
            let mut items = vec![Item::new("instance.more.rename", tr("Umbenennen"), move |_| actions::begin_rename(store, &rename_id)).shortcut(Shortcut::bare(Code::F2).label()), Item::Separator];
            let all = actions::all(store, &menu_facts);
            let split = all.iter().position(|a| a.key == "reset").unwrap_or(all.len());
            for (n, a) in all.into_iter().enumerate() {
                if n == split {
                    items.push(Item::Separator);
                }
                let run = a.run.clone();
                let mut item = Item::new(format!("instance.more.{}", a.key), a.label, move |_| run.call(())).disabled(a.disabled.is_some());
                if a.danger {
                    item = item.danger();
                }
                items.push(item);
            }
            menu::open(store.ui, Menu::under(items, *anchor.peek()));
        })));
        bar.into_element()
    }
}
