//! The Start page: what to play next, what is running and what needs the person, with the way to a first instance.

mod quick;

use std::time::{Instant, SystemTime};

use freya::prelude::*;

use self::quick::QuickPlay;
use crate::app::event::Event as AppEvent;
use crate::app::model::Need;
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::core::launchers;
use crate::view::commands::{self, Want, can_want, want};
use crate::view::drive::use_spot;
use crate::view::grid::{fix_of, run_fix, words};
use crate::view::icons::Icon;
use crate::view::instance::{self, Facts, Hero, InstanceIcon, facts, use_minute};
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::text::{body, caption, heading, line, secondary, strong};
use crate::view::kit::{off_ui, ring, use_theme};
use crate::view::library::{self, CARD_W, Infos, Wall, ago, use_infos, what};
use crate::view::shell::swatch;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, font, frame, iconsize, space};
use crate::view::ui::{Filter, Page};

const RECENT_MAX: usize = 8;

/// The instances played, newest first, at most `limit`.
pub fn recent<'a>(list: &'a [Instance], infos: &Infos, limit: usize) -> Vec<&'a Instance> {
    let mut played: Vec<(&Instance, SystemTime)> = list.iter().filter_map(|i| infos.get(&i.id).and_then(|n| n.played).map(|at| (i, at))).collect();
    played.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase())));
    played.into_iter().take(limit).map(|(i, _)| i).collect()
}

/// The instance to continue with: the one played last, else the one made last.
pub fn resume<'a>(list: &'a [Instance], infos: &Infos) -> Option<&'a Instance> {
    recent(list, infos, 1).into_iter().next().or_else(|| list.iter().max_by_key(|i| (infos.get(&i.id).and_then(|n| n.made), i.name.to_lowercase())))
}

/// How many instance cards fit side by side in `room`, at most [`RECENT_MAX`].
fn cards_in(room: f32, gap: f32) -> usize {
    (((room + gap) / (CARD_W + gap)).floor().max(1.) as usize).min(RECENT_MAX)
}

fn places(f: &Facts) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, place) in &f.busy {
        if !out.contains(place) {
            out.push(place.clone());
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Calm,
    Live,
    Attention,
}

/// The line under the name of the hero: what stops Play, what runs, or when it was played last.
fn status(f: &Facts, played: Option<SystemTime>, since: Option<Instant>) -> (Tone, String) {
    if f.accounts.is_empty() {
        return (Tone::Attention, tr("Füge zuerst einen Account hinzu."));
    }
    if let Some(text) = instance::running_line(f, since) {
        return (Tone::Live, text);
    }
    if f.stopping.is_some() {
        return (Tone::Calm, tr("Stoppt …"));
    }
    if !f.launching.is_empty() {
        return (Tone::Calm, f.loading.map_or_else(|| tr("Startet …"), |p| trf("Wird installiert · %lld %", &[&p])));
    }
    if f.none_free() {
        let places = places(f);
        return (Tone::Attention, trf("Von Batch %@ genutzt", &[&places.join(", ")]));
    }
    if let Some(who) = f.pick.filter(|a| f.expired.contains(a)).and_then(|a| f.name_of(a)) {
        return (Tone::Attention, trf("Die Anmeldung von %@ ist abgelaufen.", &[&who]));
    }
    if let Some(trouble) = &f.trouble {
        return (Tone::Attention, trouble.clone());
    }
    (Tone::Calm, played.map(|at| trf("Zuletzt %@", &[&ago(at)])).unwrap_or_else(|| tr("Noch nie gespielt")))
}

fn section(t: &Theme, title: String, trailing: Option<Element>, content: impl IntoElement) -> Element {
    let mut head = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(rect().width(Size::flex(1.)).child(strong(t, title, font::TITLE3, t.text1))).content(Content::flex());
    if let Some(trailing) = trailing {
        head = head.child(trailing);
    }
    rect().width(Size::fill()).spacing(space::S).child(head).child(content).into_element()
}

fn go(store: Store, page: Page) {
    let mut now = store.ui.page;
    now.set(page);
}

/// The home page.
#[derive(PartialEq)]
pub struct StartPage;

impl Component for StartPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        use_infos(store);
        let infos = ui.infos.read().clone();
        let m = store.model.read();
        if !m.loaded {
            return rect().into_element();
        }
        let list = m.instances.clone();
        let accounts = m.accounts.len();
        let room = (ui.window.read().0 - ui.sidebar_room()).min(frame::START);
        let mut page = rect().width(Size::fill()).max_width(Size::px(frame::START)).padding(space::XL).spacing(space::XL);
        if list.is_empty() && ui.installs.read().is_empty() {
            drop(m);
            return scroll(page.child(Welcome { bare: accounts == 0 }));
        }
        drop(m);
        if let Some(i) = resume(&list, &infos) {
            let played = infos.get(&i.id).and_then(|n| n.played);
            let title = if played.is_some() { tr("Weiterspielen") } else { tr("Jetzt spielen") };
            page = page.child(section(&t, title, None, ResumeCard { id: i.id.clone(), name: i.name.clone(), picture: i.icon_path(), what: what(i), played, room }));
        }
        let more: Vec<&Instance> = recent(&list, &infos, RECENT_MAX + 1).into_iter().skip(1).take(cards_in(room - space::XL * 2., t.grid_gap)).collect();
        if !more.is_empty() {
            let lines = library::lines(store, &more, &infos);
            let all = Button::new("start.recent.all", tr("Alle Instanzen")).plain().on_press(move |_| go(store, Page::Library)).into_element();
            page = page.child(section(&t, tr("Zuletzt gespielt"), Some(all), Wall { lines }));
        }
        let m = store.model.read();
        if !m.batches.is_empty() {
            let mut rows = rect().width(Size::fill()).spacing(space::XS);
            for b in &m.batches {
                let games = || m.games.values().filter(|g| g.batch_id == b.id);
                let (total, running) = (games().count(), games().filter(|g| g.phase.is_active()).count());
                let attention = m.attention_count(Some(&b.id));
                rows = rows.child(rect().key(&b.id).width(Size::fill()).child(BatchRow { id: b.id.clone(), name: b.name.clone(), total, running, attention }));
            }
            page = page.child(section(&t, tr("Batches"), None, rows));
        }
        let needs = m.needs(None);
        if !needs.is_empty() {
            let mut rows = rect().width(Size::fill()).spacing(space::XS);
            for (need, count) in needs {
                let fix = fix_of(&m, need);
                rows = rows.child(rect().key(format!("{need:?}")).width(Size::fill()).child(NeedRow { need, count, fix }));
            }
            page = page.child(section(&t, tr("Brauchen dich"), None, rows));
        }
        drop(m);
        page = page.child(QuickPlay { infos });
        page = page.child(footer(store));
        scroll(page)
    }
}

fn scroll(page: Rect) -> Element {
    ScrollView::new().width(Size::fill()).height(Size::fill()).child(rect().width(Size::fill()).cross_align(Alignment::Center).child(page)).into_element()
}

#[derive(PartialEq)]
struct ResumeCard {
    id: String,
    name: String,
    picture: Option<std::path::PathBuf>,
    what: String,
    played: Option<SystemTime>,
    room: f32,
}

impl Component for ResumeCard {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let f = facts(&store.model.read(), &self.id);
        use_minute(store, true);
        let since = f.as_ref().and_then(|f| f.stoppable.iter().filter_map(|(key, _)| store.ui.instance.up.read().get(key).copied()).min());
        let (tone, text) = f.as_ref().map(|f| status(f, self.played, since)).unwrap_or((Tone::Calm, String::new()));
        let ink = if tone == Tone::Attention { t.attention_text } else { t.text2 };
        let mut note = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
        match tone {
            Tone::Live => note = note.child(dot(t.success)),
            Tone::Attention => note = note.child(dot(t.attention)),
            Tone::Calm => {}
        }
        note = note.child(line(&t, text, font::CALLOUT, ink).max_lines(2));
        let mut actions = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        if let Some(f) = f.as_ref().filter(|f| f.none_free() && f.stoppable.is_empty()) {
            let batch = places(f).first().and_then(|place| f.batches.iter().find(|(_, name)| name == place)).map(|(id, _)| id.clone());
            if let Some(batch) = batch {
                actions = actions.child(Button::new("start.batch.open", tr("Zum Batch")).tall().on_press(move |_| go(store, Page::Grid(Filter::Batch(batch.clone())))));
            }
        }
        let id = self.id.clone();
        actions = actions.child(Button::new("start.open", tr("Instanz öffnen")).tall().on_press(move |_| go(store, Page::Instance(id.clone())))).child(Hero { id: self.id.clone(), room: self.room - frame::HERO_FACE * 2. });
        rect()
            .width(Size::fill())
            .padding(space::L)
            .spacing(space::L)
            .horizontal()
            .cross_align(Alignment::Center)
            .background(t.raised)
            .corner_radius(t.shape.tile)
            .child(InstanceIcon { id: self.id.clone(), name: self.name.clone(), picture: self.picture.clone(), side: frame::HERO_FACE })
            .child(rect().width(Size::flex(1.)).spacing(space::XXS).child(heading(&t, self.name.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis)).child(secondary(&t, self.what.clone())).child(note))
            .content(Content::flex())
            .child(actions)
    }
}

#[derive(PartialEq)]
struct BatchRow {
    id: String,
    name: String,
    total: usize,
    running: usize,
    attention: usize,
}

impl Component for BatchRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let id = self.id.clone();
        let spot = use_spot(&format!("start.batch.{id}"), "button", &self.name, true);
        let (open, key, act) = (id.clone(), id.clone(), id.clone());
        let counts = trf("%lld/%lld laufen", &[&self.running, &self.total]);
        let toggle = if self.running == 0 {
            Button::new(format!("start.batch.{id}.start"), tr("Starten")).on_press(move |_| store.dispatch(AppEvent::StartBatch(act.clone())))
        } else {
            Button::new(format!("start.batch.{id}.stop"), tr("Stoppen")).on_press(move |_| store.dispatch(AppEvent::StopBatch(act.clone())))
        };
        let mut meta = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        if self.attention > 0 {
            meta = meta.child(dot(t.attention)).child(caption(&t, trf("%lld brauchen dich", &[&self.attention])).color(t.attention_text));
        }
        meta = meta.child(caption(&t, counts.clone()).font_tabular());
        let mut row = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(format!("{}, {counts}", self.name))
            .width(Size::fill())
            .padding((space::S, space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .background(if *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .corner_radius(t.shape.control)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_press(move |_| go(store, Page::Grid(Filter::Batch(open.clone()))))
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if a11y.is_focused() && matches!(&e.key, Key::Named(NamedKey::Enter)) {
                    e.stop_propagation();
                    go(store, Page::Grid(Filter::Batch(key.clone())));
                }
            })
            .child(swatch(&t, &self.id, iconsize::XS))
            .child(rect().width(Size::flex(1.)).child(body(&t, self.name.clone()).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis)))
            .child(meta)
            .child(toggle)
            .content(Content::flex());
        if let Some(border) = ring(&t, focus()) {
            row = row.border(border);
        }
        rect().width(Size::fill()).background(t.raised).corner_radius(t.shape.control).child(row)
    }
}

#[derive(PartialEq)]
struct NeedRow {
    need: Need,
    count: usize,
    fix: Option<String>,
}

impl Component for NeedRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let need = self.need;
        let mut row = rect()
            .width(Size::fill())
            .padding((space::S, space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .background(t.attention.with_af32(frame::NOTICE_WASH))
            .corner_radius(t.shape.control)
            .child(dot(t.attention))
            .child(rect().width(Size::flex(1.)).child(line(&t, words::need_label(need, self.count), font::BODY, t.text1)))
            .content(Content::flex());
        if let Some(fix) = self.fix.clone() {
            row = row.child(Button::new(format!("start.need.{need:?}.fix"), fix).on_press(move |_| run_fix(store, need)));
        }
        row.child(Button::new(format!("start.need.{need:?}"), tr("Anzeigen")).plain().on_press(move |_| go(store, Page::Grid(Filter::Attention))))
    }
}

/// The first run: the ways to a first instance, with the launchers found on this machine.
#[derive(PartialEq)]
struct Welcome {
    bare: bool,
}

impl Component for Welcome {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut found = use_state(Vec::<launchers::Library>::new);
        use_hook(move || {
            spawn_in_window(async move {
                if let Some(libraries) = off_ui(launchers::scan).await
                    && found.try_peek().is_some()
                {
                    found.set(libraries);
                }
            });
        });
        let detected: Vec<String> = found.read().iter().map(|l| trf("%@ mit %lld Instanzen gefunden", &[&l.launcher.name(), &l.instances.len()])).collect();
        let mut actions = rect().horizontal().spacing(space::S).content(Content::wrap_spacing(space::S));
        let new = Button::new("start.welcome.new", tr("Neue Instanz")).glyph(Icon::Plus).tall().disabled(commands::want_blocked(&Want::NewInstance)).on_press(move |_| {
            want(store, Want::NewInstance);
        });
        actions = actions.child(if self.bare { new } else { new.primary() });
        actions = actions.child(Button::new("start.welcome.packs", tr("Modpack suchen")).glyph(Icon::Search).tall().on_press(move |_| go(store, Page::Discover)));
        if can_want(&Want::Import) {
            actions = actions.child(Button::new("start.welcome.import", tr("Aus anderem Launcher importieren")).glyph(Icon::Download).tall().on_press(move |_| {
                want(store, Want::Import);
            }));
        }
        let mut card = rect()
            .width(Size::fill())
            .max_width(Size::px(frame::START_WELCOME))
            .padding(space::XL)
            .spacing(space::M)
            .background(t.raised)
            .corner_radius(t.shape.tile)
            .child(heading(&t, tr("Willkommen bei Manycraft")))
            .child(secondary(&t, tr("Eine Instanz ist ein Minecraft mit eigener Version, eigenen Mods und Einstellungen.")).max_lines(3))
            .child(actions);
        if !detected.is_empty() {
            card = card.child(caption(&t, detected.join(" · ")));
        }
        if self.bare {
            card = card.child(no_account(&t, store));
        }
        rect().width(Size::fill()).cross_align(Alignment::Center).padding((space::XL, 0.)).child(card)
    }
}

fn no_account(t: &Theme, store: Store) -> Element {
    rect()
        .width(Size::fill())
        .padding((space::S, space::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .background(t.attention.with_af32(frame::NOTICE_WASH))
        .corner_radius(t.shape.control)
        .child(rect().width(Size::flex(1.)).child(line(t, tr("Füge zuerst einen Account hinzu."), font::BODY, t.text1)))
        .content(Content::flex())
        .child(Button::new("start.account", tr("Account hinzufügen")).primary().disabled(commands::want_blocked(&Want::AddAccount)).on_press(move |_| {
            want(store, Want::AddAccount);
        }))
        .into_element()
}

fn footer(store: Store) -> Element {
    let mut row = rect().width(Size::fill()).horizontal().spacing(space::S).content(Content::wrap_spacing(space::S));
    row = row.child(Button::new("start.new", tr("Neue Instanz")).glyph(Icon::Plus).disabled(commands::want_blocked(&Want::NewInstance)).on_press(move |_| {
        want(store, Want::NewInstance);
    }));
    if can_want(&Want::Import) {
        row = row.child(Button::new("start.import", tr("Importieren…")).glyph(Icon::Download).on_press(move |_| {
            want(store, Want::Import);
        }));
    }
    row.child(Button::new("start.packs", tr("Modpack suchen")).glyph(Icon::Search).on_press(move |_| go(store, Page::Discover))).into_element()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::core::instance::Loader;
    use crate::view::library::Info;

    fn made(name: &str) -> Instance {
        Instance::new(name, "1.21", Loader::Vanilla)
    }

    fn infos(list: &[(&Instance, Option<u64>, Option<u64>)]) -> Infos {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let map: HashMap<String, Info> = list.iter().map(|(i, p, m)| (i.id.clone(), Info { played: p.map(|s| base + Duration::from_secs(s)), made: m.map(|s| base + Duration::from_secs(s)) })).collect();
        Rc::new(map)
    }

    #[test]
    fn the_newest_played_instance_leads_and_unplayed_ones_stay_out() {
        let (a, b, c) = (made("A"), made("B"), made("C"));
        let infos = infos(&[(&a, Some(10), None), (&b, Some(30), None), (&c, None, None)]);
        let list = vec![a, b, c];
        let names: Vec<&str> = recent(&list, &infos, 8).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["B", "A"]);
        assert_eq!(recent(&list, &infos, 1).len(), 1);
    }

    #[test]
    fn with_nothing_played_the_newest_made_instance_is_resumed() {
        let (a, b) = (made("A"), made("B"));
        let infos = infos(&[(&a, None, Some(5)), (&b, None, Some(9))]);
        let list = vec![a, b];
        assert_eq!(resume(&list, &infos).map(|i| i.name.as_str()), Some("B"));
        assert!(resume(&[], &infos).is_none());
    }
}
