//! The Start page: what to play next, what is running and what needs the person, with the way to a first instance.

use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::view::commands::{self, Want, can_want, want};
use crate::view::icons::Icon;
use crate::view::instance::{Hero, InstanceIcon};
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::text::{body, caption, heading, line, secondary, strong};
use crate::view::kit::use_theme;
use crate::view::library::{self, Infos, Wall, ago, use_infos, what};
use crate::view::store::use_store;
use crate::view::theme::{Theme, font, frame, space};
use crate::view::ui::{Filter, Page};

const RECENT_MAX: usize = 8;
const FACE: f32 = 80.;

/// The instances played, newest first, at most `limit`.
pub fn recent<'a>(list: &'a [Instance], infos: &Infos, limit: usize) -> Vec<&'a Instance> {
    let mut played: Vec<(&Instance, std::time::SystemTime)> = list.iter().filter_map(|i| infos.get(&i.id).and_then(|n| n.played).map(|at| (i, at))).collect();
    played.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase())));
    played.into_iter().take(limit).map(|(i, _)| i).collect()
}

/// The instance to continue with: the one played last, else the one made last.
pub fn resume<'a>(list: &'a [Instance], infos: &Infos) -> Option<&'a Instance> {
    recent(list, infos, 1).into_iter().next().or_else(|| list.iter().max_by_key(|i| (infos.get(&i.id).and_then(|n| n.made), i.name.to_lowercase())))
}

fn label(t: &Theme, text: String) -> Element {
    strong(t, text, font::TITLE3, t.text1).into_element()
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
            return scroll(page.child(welcome(&t, store, accounts == 0)));
        }
        if accounts == 0 {
            page = page.child(no_account(&t, store));
        }
        if let Some(i) = resume(&list, &infos) {
            let played = infos.get(&i.id).and_then(|n| n.played).map(|at| trf("Zuletzt %@", &[&ago(at)])).unwrap_or_else(|| tr("Noch nie gespielt"));
            let id = i.id.clone();
            let hero = rect()
                .width(Size::fill())
                .padding(space::L)
                .spacing(space::L)
                .horizontal()
                .cross_align(Alignment::Center)
                .background(t.raised)
                .corner_radius(t.shape.tile)
                .child(InstanceIcon { id: i.id.clone(), name: i.name.clone(), picture: i.icon_path(), side: FACE })
                .child(rect().width(Size::flex(1.)).spacing(space::XXS).child(heading(&t, i.name.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis)).child(secondary(&t, what(i))).child(caption(&t, played)))
                .content(Content::flex())
                .child(Hero { id: id.clone(), room: room - FACE * 2. })
                .child(Button::new("start.open", tr("Instanz öffnen")).tall().on_press(move |_| {
                    let mut page = store.ui.page;
                    page.set(Page::Instance(id.clone()));
                }));
            page = page.child(rect().width(Size::fill()).spacing(space::S).child(label(&t, tr("Weiterspielen"))).child(hero));
        }
        let more: Vec<&Instance> = recent(&list, &infos, RECENT_MAX + 1).into_iter().skip(1).collect();
        if !more.is_empty() {
            let lines = library::lines(store, &more, &infos);
            drop(m);
            page = page.child(rect().width(Size::fill()).spacing(space::S).child(label(&t, tr("Zuletzt gespielt"))).child(Wall { lines }));
        } else {
            drop(m);
        }
        let m = store.model.read();
        if !m.batches.is_empty() {
            let mut rows = rect().width(Size::fill()).spacing(space::XS);
            for b in &m.batches {
                let games = || m.games.values().filter(|g| g.batch_id == b.id);
                let total = games().count();
                let running = games().filter(|g| g.phase.is_active()).count();
                let attention = m.attention_count(Some(&b.id));
                let id = b.id.clone();
                let go = id.clone();
                let toggle = if running == 0 {
                    Button::new(format!("start.batch.{id}.start"), tr("Starten")).on_press(move |_| store.dispatch(AppEvent::StartBatch(id.clone())))
                } else {
                    Button::new(format!("start.batch.{id}.stop"), tr("Stoppen")).on_press(move |_| store.dispatch(AppEvent::StopBatch(id.clone())))
                };
                let mut row = rect()
                    .width(Size::fill())
                    .padding((space::S, space::M))
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::M)
                    .background(t.raised)
                    .corner_radius(t.shape.control)
                    .cursor(CursorIcon::Pointer)
                    .on_press(move |_| {
                        let mut page = store.ui.page;
                        page.set(Page::Grid(Filter::Batch(go.clone())));
                    })
                    .child(rect().width(Size::flex(1.)).child(body(&t, b.name.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis)))
                    .content(Content::flex());
                if attention > 0 {
                    row = row.child(dot(t.attention));
                }
                row = row.child(caption(&t, trf("%lld/%lld laufen", &[&running, &total])).font_tabular()).child(toggle);
                rows = rows.child(rect().key(&b.id).width(Size::fill()).child(row));
            }
            page = page.child(rect().width(Size::fill()).spacing(space::S).child(label(&t, tr("Batches"))).child(rows));
        }
        let needs = m.needs(None);
        if !needs.is_empty() {
            let mut rows = rect().width(Size::fill()).spacing(space::XS);
            for (need, count) in needs {
                rows = rows.child(
                    rect()
                        .key(format!("{need:?}"))
                        .width(Size::fill())
                        .padding((space::S, space::M))
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(space::S)
                        .background(t.attention.with_af32(frame::NOTICE_WASH))
                        .corner_radius(t.shape.control)
                        .child(dot(t.attention))
                        .child(rect().width(Size::flex(1.)).child(line(&t, crate::view::grid::words::need_label(need, count), font::BODY, t.text1)))
                        .content(Content::flex())
                        .child(Button::new(format!("start.need.{need:?}"), tr("Anzeigen")).on_press(move |_| {
                            let mut page = store.ui.page;
                            page.set(Page::Grid(Filter::Attention));
                        })),
                );
            }
            page = page.child(rect().width(Size::fill()).spacing(space::S).child(label(&t, tr("Brauchen dich"))).child(rows));
        }
        drop(m);
        page = page.child(footer(store));
        scroll(page)
    }
}

fn scroll(page: Rect) -> Element {
    ScrollView::new().width(Size::fill()).height(Size::fill()).child(rect().width(Size::fill()).cross_align(Alignment::Center).child(page)).into_element()
}

fn no_account(t: &Theme, store: crate::view::store::Store) -> Element {
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

fn welcome(t: &Theme, store: crate::view::store::Store, bare: bool) -> Element {
    let mut actions = rect().horizontal().spacing(space::S).content(Content::wrap_spacing(space::S));
    actions = actions.child(Button::new("start.welcome.new", tr("Neue Instanz")).glyph(Icon::Plus).primary().tall().disabled(commands::want_blocked(&Want::NewInstance)).on_press(move |_| {
        want(store, Want::NewInstance);
    }));
    actions = actions.child(Button::new("start.welcome.packs", tr("Modpack suchen")).glyph(Icon::Search).tall().on_press(move |_| {
        let mut page = store.ui.page;
        page.set(Page::Discover);
    }));
    if can_want(&Want::Import) {
        actions = actions.child(Button::new("start.welcome.import", tr("Aus anderem Launcher importieren")).glyph(Icon::Download).tall().on_press(move |_| {
            want(store, Want::Import);
        }));
    }
    let mut card = rect()
        .width(Size::fill())
        .max_width(Size::px(frame::WELCOME))
        .padding(space::XL)
        .spacing(space::M)
        .background(t.raised)
        .corner_radius(t.shape.tile)
        .child(heading(t, tr("Willkommen bei Manycraft")))
        .child(secondary(t, tr("Eine Instanz ist ein Minecraft mit eigener Version, eigenen Mods und Einstellungen.")))
        .child(actions);
    if bare {
        card = card.child(no_account(t, store));
    }
    rect().width(Size::fill()).cross_align(Alignment::Center).padding((space::XL, 0.)).child(card).into_element()
}

fn footer(store: crate::view::store::Store) -> Element {
    let mut row = rect().width(Size::fill()).horizontal().spacing(space::S).content(Content::wrap_spacing(space::S));
    row = row.child(Button::new("start.new", tr("Neue Instanz")).glyph(Icon::Plus).disabled(commands::want_blocked(&Want::NewInstance)).on_press(move |_| {
        want(store, Want::NewInstance);
    }));
    if can_want(&Want::Import) {
        row = row.child(Button::new("start.import", tr("Importieren…")).glyph(Icon::Download).on_press(move |_| {
            want(store, Want::Import);
        }));
    }
    row.child(Button::new("start.packs", tr("Modpack suchen")).glyph(Icon::Search).on_press(move |_| {
        let mut page = store.ui.page;
        page.set(Page::Discover);
    }))
    .into_element()
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
