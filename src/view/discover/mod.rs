//! Entdecken: Modrinth's modpacks, mods, resource packs, shaders and data packs. Modpacks become new instances, every
//! other kind is added to the instance picked as the target, which Entdecken remembers for the session.

use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::core::modrinth::Kind;
use crate::view::commands::{self, Command, Extra, Shortcut, Want};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::chip::Chip;
use crate::view::kit::row::Row;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::sheet::{self, Card};
use crate::view::kit::text::{heading, para};
use crate::view::kit::use_theme;
use crate::view::sheets::browser::{ContentPage, PacksPage};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};

const KINDS: [Kind; 5] = [Kind::Modpack, Kind::Mod, Kind::ResourcePack, Kind::Shader, Kind::DataPack];
const LIST_MAX: f32 = 340.;
const DIMMED: f32 = 0.55;

/// What Entdecken shows and where it adds to, kept for the session.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub target: Option<String>,
    pub kind: Kind,
}

impl Pick {
    pub fn new() -> Self {
        Self { target: None, kind: Kind::Modpack }
    }
}

/// Makes instance `id` the target of Entdecken.
pub fn target(store: Store, id: &str) {
    let mut pick = store.ui.discover;
    pick.write().target = Some(id.to_owned());
}

fn kind_name(kind: Kind) -> String {
    match kind {
        Kind::Modpack => tr("Modpacks"),
        Kind::Mod => tr("Mods"),
        Kind::ResourcePack => tr("Ressourcenpakete"),
        Kind::Shader => tr("Shader"),
        Kind::DataPack => tr("Datenpakete"),
    }
}

/// Why `made` takes no content of `kind`, when it does not.
fn problem(kind: Kind, made: &Instance) -> Option<String> {
    match kind {
        Kind::Mod | Kind::Shader if !made.loader.takes_mods() => Some(tr("Braucht Fabric, Quilt, Forge oder NeoForge")),
        _ => None,
    }
}

fn choose(store: Store) {
    if store.model.peek().instances.is_empty() {
        commands::want(store, Want::NewInstance);
    } else {
        sheet::open(store.ui, Targets);
    }
}

/// The page's commands for the palette: pick the instance content is added to.
pub fn extras(store: Store) -> Vec<Extra> {
    vec![Extra { id: "discover.target".into(), label: tr("Instanz für Entdecken wählen …"), glyph: Icon::Package, shortcut: Some(Shortcut::primary(Code::KeyI)), run: EventHandler::new(move |_| choose(store)) }]
}

/// "Zu welcher Instanz?": the instances, those that take the shown kind first, the others dimmed with the reason.
#[derive(PartialEq)]
struct Targets;

impl Component for Targets {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let (kind, current) = {
            let pick = store.ui.discover.read();
            (pick.kind, pick.target.clone())
        };
        let mut listed: Vec<(Instance, Option<String>)> = store.model.read().instances.iter().map(|i| (i.clone(), problem(kind, i))).collect();
        listed.sort_by_cached_key(|(i, why)| (why.is_some(), i.name.to_lowercase()));
        let rows = listed.into_iter().map(|(made, why)| {
            let id = made.id.clone();
            let detail = why.clone().unwrap_or_else(|| made.version_line());
            let row = Row::new(format!("discover.target.{id}"), made.name.clone()).glyph(Icon::Package).detail(detail).selected(current.as_deref() == Some(id.as_str())).on_press(move |_| {
                target(store, &id);
                sheet::close(store.ui);
            });
            rect().key(&made.id).width(Size::fill()).opacity(if why.is_some() { DIMMED } else { 1. }).child(row).into_element()
        });
        let list = ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px(LIST_MAX)).child(rect().width(Size::fill()).spacing(space::XXS).children(rows));
        Card::new("discover.target", tr("Zu welcher Instanz?")).message(tr("Dorthin kommt, was du hinzufügst.")).width(frame::SHEET).body(list).into_element()
    }
}

/// Entdecken.
#[derive(PartialEq)]
pub struct DiscoverPage;

impl Component for DiscoverPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut pick = store.ui.discover;
        let (kind, wanted) = {
            let p = pick.read();
            (p.kind, p.target.clone())
        };
        let made = wanted.and_then(|id| store.model.read().instance(&id).cloned());
        let segments = KINDS.iter().map(|k| Segment { id: format!("discover.kind.{}", k.project_type()), label: kind_name(*k), tip: None }).collect();
        let at = KINDS.iter().position(|k| *k == kind).unwrap_or(0);
        let mut bar = rect()
            .width(Size::fill())
            .max_width(Size::px(frame::CONTENT_MAX))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .content(Content::flex())
            .child(Segmented::new(tr("Inhalt"), segments, at).on_choose(move |i| {
                if let Some(k) = KINDS.get(i) {
                    pick.write().kind = *k;
                }
            }))
            .child(rect().width(Size::flex(1.)));
        if kind != Kind::Modpack
            && let Some(made) = &made
        {
            bar = bar.child(Chip::new("discover.target.chip", trf("Für: %@", &[&made.name])).on(true).on_press(move |_| choose(store)).on_remove(move |_| pick.write().target = None));
        }
        let body = match (&made, kind) {
            (_, Kind::Modpack) => rect().key("packs").width(Size::fill()).height(Size::fill()).child(PacksPage).into_element(),
            (Some(made), kind) => rect().key(&made.id).width(Size::fill()).height(Size::fill()).child(ContentPage { id: made.id.clone(), kind, outer: true }).into_element(),
            (None, _) => ask(&t, store),
        };
        rect()
            .width(Size::fill())
            .height(Size::fill())
            .content(Content::flex())
            .child(rect().width(Size::fill()).cross_align(Alignment::Center).padding((space::M, space::L, 0., space::L)).child(bar))
            .child(rect().width(Size::fill()).height(Size::flex(1.)).child(body))
    }
}

fn ask(t: &Theme, store: Store) -> Element {
    let none = store.model.read().instances.is_empty();
    let (title, text, action) = if none {
        (
            tr("Noch keine Instanz"),
            tr("Lege zuerst eine Instanz an, dann kannst du Inhalte hinzufügen."),
            Button::new("discover.new", tr("Neue Instanz")).glyph(Icon::Plus).primary().disabled(commands::want_blocked(&Want::NewInstance)).tip(Command::NewInstance.tip()).on_press(move |_| {
                commands::want(store, Want::NewInstance);
            }),
        )
    } else {
        (
            tr("Zu welcher Instanz?"),
            tr("Mods, Ressourcenpakete, Shader und Datenpakete kommen in eine Instanz. Du kannst sie jederzeit wechseln."),
            Button::new("discover.pick", tr("Instanz wählen")).primary().on_press(move |_| choose(store)),
        )
    };
    rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .padding((0., space::XL))
        .child(icon(t, Icon::Compass, iconsize::L * 2., t.text3))
        .child(heading(t, title))
        .child(rect().width(Size::fill()).max_width(Size::px(control::READING)).child(para(t, text, font::CALLOUT, t.text2).text_align(TextAlign::Center)))
        .child(action)
        .into_element()
}
