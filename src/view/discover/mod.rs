//! Entdecken: Modrinth's modpacks, mods, resource packs, shaders and data packs. Modpacks become new instances, every
//! other kind browses freely and asks for a target instance when adding or via the target chip.

use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::core::modrinth::{self, Filter, Hit, Kind, Support, Version};
use crate::view::commands::{self, Command, Extra, Shortcut, Want};
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::chip::Chip;
use crate::view::kit::row::Row;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::sheet::{self, Card};
use crate::view::kit::text::caption;
use crate::view::kit::use_theme;
use crate::view::sheets::browser::{ContentPage, DiscoverBrowsePage, PacksPage};
use crate::view::store::{Store, use_store};
use crate::view::theme::{frame, space};

const KINDS: [Kind; 5] = [Kind::Modpack, Kind::Mod, Kind::ResourcePack, Kind::Shader, Kind::DataPack];
const LIST_MAX: f32 = 340.;

/// A content addition waiting for the newly chosen instance's browser to mount.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub instance: String,
    pub kind: Kind,
    pub hit: Hit,
    pub version: Option<Version>,
}

/// What Entdecken shows and where it adds to, kept for the session.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub target: Option<String>,
    pub kind: Kind,
    pub pending: Option<Pending>,
}

impl Pick {
    pub fn new() -> Self {
        Self { target: None, kind: Kind::Modpack, pending: None }
    }
}

/// Makes instance `id` the target of Entdecken.
pub fn target(store: Store, id: &str) {
    let mut pick = store.ui.discover;
    let mut p = pick.write();
    p.target = Some(id.to_owned());
    p.pending = None;
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

/// Why `made` cannot take `kind` (and optional `hit` / `version`), when it cannot.
fn problem(kind: Kind, made: &Instance, hit: Option<&Hit>, version: Option<&Version>) -> Option<String> {
    if matches!(kind, Kind::Mod | Kind::Shader) && !made.loader.takes_mods() {
        return Some(tr("Braucht Fabric, Quilt, Forge oder NeoForge"));
    }
    let filter = Filter::of(kind, &made.game_version, &made.loader, None);
    if let Some(v) = version {
        if !modrinth::version_fits(v, &filter) {
            return Some(trf("Passt nicht zu %@.", &[&format!("Minecraft {} · {}", made.game_version, made.loader.name())]));
        }
        return None;
    }
    if let Some(hit) = hit {
        return match hit.fits(&filter) {
            Support::Fits => None,
            Support::OtherVersion => Some(trf("Nicht für Minecraft %@", &[&made.game_version])),
            Support::OtherLoader if kind == Kind::Shader => Some(tr("Nur für OptiFine")),
            Support::OtherLoader => Some(trf("Nicht für %@", &[&made.loader.name()])),
        };
    }
    None
}

fn choose(store: Store, pending: Option<(Hit, Option<Version>)>) {
    if store.model.peek().instances.is_empty() {
        commands::want(store, Want::NewInstance);
    } else {
        sheet::open(store.ui, Targets { pending });
    }
}

/// The page's commands for the palette: pick the instance content is added to.
pub fn extras(store: Store) -> Vec<Extra> {
    vec![Extra { id: "discover.target".into(), label: tr("Instanz für Entdecken wählen …"), glyph: Icon::Package, shortcut: Some(Shortcut::primary(Code::KeyI)), run: EventHandler::new(move |_| choose(store, None)) }]
}

/// "Zu welcher Instanz?": compatible instances first, incompatible ones disabled with the reason.
#[derive(PartialEq)]
struct Targets {
    pending: Option<(Hit, Option<Version>)>,
}

impl Component for Targets {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let (kind, current) = {
            let pick = store.ui.discover.read();
            (pick.kind, pick.target.clone())
        };
        let hit = self.pending.as_ref().map(|(h, _)| h);
        let version = self.pending.as_ref().and_then(|(_, v)| v.as_ref());
        let mut listed: Vec<(Instance, Option<String>)> = store.model.read().instances.iter().map(|i| (i.clone(), problem(kind, i, hit, version))).collect();
        listed.sort_by_cached_key(|(i, why)| (why.is_some(), i.name.to_lowercase()));
        let any_fit = listed.iter().any(|(_, why)| why.is_none());
        let pending = self.pending.clone();
        let rows = listed.into_iter().map(|(made, why)| {
            let id = made.id.clone();
            let blocked = why.is_some();
            let detail = why.unwrap_or_else(|| made.version_line());
            let mut row = Row::new(format!("discover.target.{id}"), made.name.clone()).glyph(Icon::Package).detail(detail).selected(current.as_deref() == Some(id.as_str())).disabled(blocked);
            if !blocked {
                let pending = pending.clone();
                row = row.on_press(move |_| {
                    let mut pick = store.ui.discover;
                    {
                        let mut p = pick.write();
                        p.target = Some(id.clone());
                        p.pending = pending.as_ref().map(|(h, v)| Pending { instance: id.clone(), kind, hit: h.clone(), version: v.clone() });
                    }
                    sheet::close(store.ui);
                });
            }
            rect().key(&made.id).width(Size::fill()).child(row).into_element()
        });
        let list = ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px(LIST_MAX)).child(rect().width(Size::fill()).spacing(space::XXS).children(rows));
        let mut body = rect().width(Size::fill()).spacing(space::S).child(list);
        if !any_fit {
            body = body.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .content(Content::flex())
                    .child(rect().width(Size::flex(1.)).child(caption(&t, tr("Keine passende Instanz vorhanden."))))
                    .child(Button::new("discover.target.new", tr("Neue Instanz")).glyph(Icon::Plus).disabled(commands::want_blocked(&Want::NewInstance)).tip(Command::NewInstance.tip()).on_press(move |_| {
                        sheet::close(store.ui);
                        commands::want(store, Want::NewInstance);
                    })),
            );
        }
        Card::new("discover.target", tr("Zu welcher Instanz?")).message(tr("Dorthin kommt, was du hinzufügst.")).width(frame::SHEET).body(body).into_element()
    }
}

/// Entdecken.
#[derive(PartialEq)]
pub struct DiscoverPage;

impl Component for DiscoverPage {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut pick = store.ui.discover;
        use_drop(move || {
            if let Some(mut p) = pick.try_write() {
                p.pending = None;
            }
        });
        let (kind, wanted) = {
            let p = pick.read();
            (p.kind, p.target.clone())
        };
        let made = wanted.and_then(|id| store.model.read().instance(&id).cloned());
        let segments = KINDS.iter().map(|k| Segment { id: format!("discover.kind.{}", k.project_type()), label: kind_name(*k), tip: None }).collect();
        let at = KINDS.iter().position(|k| *k == kind).unwrap_or(0);
        let mut bar = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .content(Content::flex())
            .child(Segmented::new(tr("Inhalt"), segments, at).on_choose(move |i| {
                if let Some(k) = KINDS.get(i) {
                    let mut p = pick.write();
                    p.kind = *k;
                    p.pending = None;
                }
            }))
            .child(rect().width(Size::flex(1.)));
        if kind != Kind::Modpack {
            match &made {
                Some(made) => {
                    bar = bar.child(Chip::new("discover.target.chip", trf("Für: %@", &[&made.name])).on(true).on_press(move |_| choose(store, None)).on_remove(move |_| {
                        let mut p = pick.write();
                        p.target = None;
                        p.pending = None;
                    }));
                }
                None => {
                    bar = bar.child(Chip::new("discover.target.chip", tr("Instanz wählen")).on(false).on_press(move |_| choose(store, None)));
                }
            }
        }
        let body = match (&made, kind) {
            (_, Kind::Modpack) => rect().key("packs").width(Size::fill()).height(Size::fill()).child(PacksPage).into_element(),
            (Some(made), kind) => rect().key(&made.id).width(Size::fill()).height(Size::fill()).child(ContentPage { id: made.id.clone(), kind, outer: true }).into_element(),
            (None, kind) => rect()
                .key("browse")
                .width(Size::fill())
                .height(Size::fill())
                .child(DiscoverBrowsePage { kind, on_pick: EventHandler::new(move |want: (Hit, Option<Version>)| choose(store, Some(want))) })
                .into_element(),
        };
        rect()
            .width(Size::fill())
            .height(Size::fill())
            .content(Content::flex())
            .child(rect().width(Size::fill()).padding((space::M, frame::GUTTER, 0., frame::GUTTER)).child(bar))
            .child(rect().width(Size::fill()).height(Size::flex(1.)).child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instance::Loader;

    fn sample_hit(versions: &[&str], loaders: &[&str]) -> Hit {
        Hit {
            project_id: "p".into(),
            slug: "p".into(),
            title: "P".into(),
            description: String::new(),
            author: String::new(),
            icon_url: None,
            downloads: 0,
            versions: versions.iter().map(|s| (*s).to_owned()).collect(),
            categories: loaders.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn picker_rejects_vanilla_for_mods_and_shaders_and_checks_hit_support() {
        let vanilla = Instance::new("V", "1.21.4", Loader::Vanilla);
        let fabric_old = Instance::new("Old", "1.20.1", Loader::Fabric(None));
        let fabric_new = Instance::new("New", "1.21.4", Loader::Fabric(None));
        let forge = Instance::new("Forge", "1.21.4", Loader::Forge(None));
        let mod_hit = sample_hit(&["1.21.4"], &["fabric"]);
        assert!(problem(Kind::Mod, &vanilla, None, None).is_some());
        assert!(problem(Kind::Shader, &vanilla, None, None).is_some());
        assert!(problem(Kind::ResourcePack, &vanilla, None, None).is_none());
        assert!(problem(Kind::Mod, &fabric_old, Some(&mod_hit), None).is_some());
        assert!(problem(Kind::Mod, &forge, Some(&mod_hit), None).is_some());
        assert!(problem(Kind::Mod, &fabric_new, Some(&mod_hit), None).is_none());
    }
}