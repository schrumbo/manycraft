//! The new-instance sheet: one page with name, version, loader, memory and the performance mods. Its other tabs pick a
//! modpack from Modrinth and import instances of other launchers.

mod packs;

use std::collections::HashSet;

use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::core::batch::ModRef;
use crate::core::companion;
use crate::core::fit::{self, Fit, Memory};
use crate::core::i18n::{tr, trc, trf};
use crate::core::instance::{Instance, Loader};
use crate::core::modrinth;
use crate::core::versions::{GameVersion, Kind, shown};
use crate::ui::forms::{self, Problem};
use crate::ui::remote::{self, InstanceEdit};
use crate::view::icons::{Icon, icon};
use crate::view::instance::version_text;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::group::{Line, group};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::progress::placeholder;
use crate::view::kit::stepper::{Stepper, Typing};
use crate::view::kit::switch::{Switch, SwitchLabel};
use crate::view::kit::text::{line, mono, para};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::settings::remember_all_versions;
use crate::view::sheets::import;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};
use crate::view::ui::Page;

/// The width of the new-instance sheet on all its tabs.
pub(super) const WIDE: f32 = 680.;
const BODY_MIN: f32 = 160.;
const BODY_MAX: f32 = 620.;
const CHROME: f32 = 280.;
const SNUG_PERCENT: u64 = 80;
const DETAIL_LINES: usize = 8;
const LOADERS: [Loader; 5] = [Loader::Fabric(None), Loader::Quilt(None), Loader::NeoForge(None), Loader::Forge(None), Loader::Vanilla];

/// Opens the sheet that sets up a new instance.
pub fn create(store: Store) {
    sheet::open(store.ui, Create { origin: Origin::New });
}

/// Opens the new-instance sheet on Importieren.
pub fn import(store: Store) {
    if store.ui.sheet.peek().is_none() {
        sheet::open(store.ui, Create { origin: Origin::Import });
    }
}

/// Where a new instance comes from: set up by hand, a modpack, or another launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    New,
    Pack,
    Import,
}

const ORIGINS: [Origin; 3] = [Origin::New, Origin::Pack, Origin::Import];

/// The segmented control that switches the new-instance sheet between its origins.
pub(super) fn origins(mut origin: State<Origin>, disabled: Option<String>) -> Element {
    let segments = ORIGINS
        .iter()
        .map(|o| {
            let (id, label) = match o {
                Origin::New => ("instance.origin.new", trc("Herkunft", "Neu")),
                Origin::Pack => ("instance.origin.pack", tr("Modpack")),
                Origin::Import => ("instance.origin.import", tr("Importieren")),
            };
            Segment { id: id.into(), label, tip: None }
        })
        .collect();
    let at = ORIGINS.iter().position(|o| *o == *origin.read()).unwrap_or(0);
    let choose = Segmented::new(tr("Herkunft"), segments, at).disabled(disabled).on_choose(move |i| {
        if let Some(o) = ORIGINS.get(i) {
            origin.set(*o);
        }
    });
    rect().width(Size::fill()).horizontal().main_align(Alignment::Center).child(choose).into_element()
}

/// Shows the Mods tab of instance ${id}.
pub fn mods(store: Store, id: String) {
    if store.model.peek().instance(&id).is_some() {
        crate::view::instance::show_on(store, &id, crate::view::instance::Tab::Mods);
    }
}

#[derive(Clone)]
pub(super) enum Load<T> {
    Loading,
    Ready(T),
    Failed(Problem),
}

fn name_problem(name: &str, taken: &[&str]) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some(tr("Gib der Instanz einen Namen."));
    }
    if let Some(problem) = forms::batch_name_problem(name) {
        return Some(problem);
    }
    let lower = name.to_lowercase();
    taken.iter().any(|t| t.trim().to_lowercase() == lower).then(|| trf("Es gibt schon eine Instanz „%@“. Wähle einen anderen Namen.", &[&name]))
}

fn companion_hint(game: &str) -> Option<String> {
    (!game.is_empty() && !companion::available(game)).then(|| trf("Hintergrundmodus ab %@", &[&companion::first()]))
}

fn followed_name(typed: &str, auto: &str, version: &str, taken: &[&str]) -> Option<String> {
    (!version.is_empty() && typed.trim() == auto).then(|| forms::unique_name(&trf("Minecraft %@", &[&version]), taken))
}

fn build_edit(name: &str, version: &str, loader: &Loader, memory_mb: Option<u32>, mods: Vec<ModRef>) -> InstanceEdit {
    InstanceEdit {
        name: name.trim().to_owned(),
        game_version: version.to_owned(),
        loader: loader.clone(),
        memory_mb,
        jvm_args: Vec::new(),
        options: Default::default(),
        mods: if loader.takes_mods() { mods } else { Vec::new() },
    }
}

fn skipped_note(skipped: &[String]) -> Option<String> {
    (!skipped.is_empty()).then(|| trf("Nicht dabei: %@.", &[&skipped.join(", ")]))
}

fn mods_hint(loader: &Loader) -> String {
    let names = modrinth::default_names(loader);
    let more = names.len().saturating_sub(2);
    match names.as_slice() {
        [first, second, ..] => trf("%1$@, %2$@ und %3$lld weitere für mehr Bilder pro Sekunde. Was es für diese Version oder diesen Loader nicht gibt, bleibt weg.", &[first, second, &more]),
        _ => tr("Ohne Loader nicht möglich."),
    }
}

/// What creating an instance brought: the instance, the preset mods that were left out, and whether the preset could
/// not be looked up at all.
struct Made {
    instance: Instance,
    skipped: Vec<String>,
    offline: bool,
}

pub(super) fn body_max(store: Store) -> f32 {
    let (_, window_h) = *store.ui.window.read();
    (window_h - CHROME).clamp(BODY_MIN, BODY_MAX)
}

fn notice(t: &Theme, glyph: Icon, color: Color, text: String) -> Element {
    rect()
        .width(Size::fill())
        .horizontal()
        .spacing(space::XS)
        .content(Content::flex())
        .child(rect().padding((space::XXS, 0., 0., 0.)).child(icon(t, glyph, iconsize::S, color)))
        .child(para(t, text, font::CALLOUT, color).width(Size::flex(1.)))
        .into_element()
}

pub(super) fn problem_view(t: &Theme, key: &str, glyph: Icon, problem: &Problem, mut open: State<HashSet<String>>) -> Element {
    let shown = open.read().contains(key);
    let mut column = rect().width(Size::fill()).spacing(space::XXS).child(notice(t, glyph, t.danger_text, problem.text.clone()));
    if let Some(detail) = &problem.detail {
        let flip = key.to_owned();
        let toggle = Button::new(format!("{key}.details"), if shown { tr("Details ausblenden") } else { tr("Details") }).plain().glyph(if shown { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| {
            let mut set = open.peek().clone();
            if !set.remove(&flip) {
                set.insert(flip.clone());
            }
            open.set(set);
        });
        column = column.child(rect().horizontal().child(toggle));
        if shown {
            column = column.child(rect().width(Size::fill()).padding((space::XS, space::S)).corner_radius(t.shape.small).background(t.well).child(mono(t, detail.clone(), font::CAPTION, t.text2).width(Size::fill()).max_lines(DETAIL_LINES)));
        }
    }
    column.into_element()
}

fn finish(store: Store, id: String) {
    sheet::close(store.ui);
    let mut page = store.ui.page;
    page.set(Page::Instance(id));
}

#[derive(Clone, Copy)]
struct Make {
    store: Store,
    name: State<String>,
    name_auto: State<String>,
    version: State<String>,
    loader: State<Loader>,
    list: State<Vec<GameVersion>>,
    versions: State<Load<()>>,
    supported: State<Option<(String, Option<HashSet<String>>)>>,
    memory: State<u32>,
    free: State<Option<u64>>,
    saving: State<bool>,
    save_error: State<Option<Problem>>,
    tried: State<bool>,
    details: State<HashSet<String>>,
    with_mods: State<bool>,
}

impl Make {
    fn alive(self) -> bool {
        self.name.try_peek().is_some()
    }

    fn load_versions(mut self, reset: bool) {
        if reset {
            self.versions.set(Load::Loading);
        }
        spawn_in_window(async move {
            let result = off_ui(remote::catalog).await;
            if !self.alive() {
                return;
            }
            match result {
                Some(Ok(list)) if !list.is_empty() => {
                    let newest = list.iter().find(|v| v.kind == Kind::Release).or_else(|| list.first()).map(|v| v.id.clone());
                    self.list.set(list);
                    self.versions.set(Load::Ready(()));
                    if let Some(newest) = newest
                        && self.version.peek().is_empty()
                    {
                        self.change_version(newest);
                    }
                }
                Some(Ok(_)) => self.versions.set(Load::Failed(Problem::plain(tr("Mojang hat keine Versionen geliefert. Versuche es später noch einmal.")))),
                Some(Err(e)) => self.versions.set(Load::Failed(forms::explain_as(tr("Versionen nicht geladen."), &e))),
                None => self.versions.set(Load::Failed(Problem::plain(forms::unexplained()))),
            }
        });
    }

    fn retry_failed(self) {
        if matches!(*self.versions.peek(), Load::Failed(_)) && self.list.peek().is_empty() {
            self.load_versions(true);
        }
    }

    fn read_memory(mut self) {
        spawn_in_window(async move {
            let free = off_ui(|| crate::core::drive::memory().or_else(fit::system_memory).map(|m| m.free_mb)).await.flatten();
            if self.alive() {
                self.free.set(free);
            }
        });
    }

    fn check_supported(mut self, kind: String) {
        let loader = self.loader.peek().clone();
        spawn_in_window(async move {
            let found = off_ui(move || remote::supported_versions(&loader)).await;
            if !self.alive() {
                return;
            }
            if let Some(found) = found
                && self.loader.peek().name() == kind
            {
                self.supported.set(Some((kind, found)));
            }
        });
    }

    fn follow_name(mut self) {
        let version = self.version.peek().clone();
        let typed = self.name.peek().clone();
        let auto = self.name_auto.peek().clone();
        let model = self.store.model.peek();
        let taken: Vec<&str> = model.instances.iter().map(|i| i.name.as_str()).collect();
        if let Some(name) = followed_name(&typed, &auto, &version, &taken) {
            self.name.set(name.clone());
            self.name_auto.set(name);
        }
    }

    fn change_version(mut self, version: String) {
        if version == *self.version.peek() || *self.saving.peek() {
            return;
        }
        self.version.set(version);
        let unpinned = self.loader.peek().with_version(None);
        self.loader.set(unpinned);
        self.follow_name();
    }

    fn pick_loader(mut self, next: Loader) {
        if *self.saving.peek() || self.loader.peek().same_kind(&next) {
            return;
        }
        self.loader.set(next);
        self.save_error.set(None);
    }

    fn set_memory(mut self, mb: u32) {
        self.memory.set(mb);
        self.save_error.set(None);
    }

    fn name_problem(self) -> Option<String> {
        let model = self.store.model.peek();
        let taken: Vec<&str> = model.instances.iter().map(|i| i.name.as_str()).collect();
        name_problem(&self.name.peek(), &taken)
    }

    fn create(mut self) {
        self.tried.set(true);
        if *self.saving.peek() || self.name_problem().is_some() || self.version.peek().is_empty() {
            return;
        }
        let (name, version, loader) = (self.name.peek().clone(), self.version.peek().clone(), self.loader.peek().clone());
        let memory = Some(*self.memory.peek()).filter(|mb| *mb != crate::core::settings::active().game.memory_mb);
        let with_mods = *self.with_mods.peek() && loader.takes_mods();
        let store = self.store;
        self.saving.set(true);
        self.save_error.set(None);
        spawn_in_window(async move {
            let result = off_ui(move || -> anyhow::Result<Made> {
                let (mods, skipped, offline) = if with_mods {
                    match remote::defaults(&version, &loader) {
                        Ok(d) => (d.mods, d.skipped, false),
                        Err(_) => (Vec::new(), Vec::new(), true),
                    }
                } else {
                    (Vec::new(), Vec::new(), false)
                };
                let instance = remote::create_instance(build_edit(&name, &version, &loader, memory, mods))?;
                Ok(Made { instance, skipped, offline })
            })
            .await;
            match result {
                Some(Ok(made)) => {
                    let Made { instance, skipped, offline } = made;
                    let id = instance.id.clone();
                    let note = if offline { Some(tr("Die Performance-Mods ließen sich nicht laden. Du findest sie später unter Mods.")) } else { skipped_note(&skipped) };
                    if store.model.peek().instance(&id).is_none() {
                        store.dispatch(AppEvent::InstanceAdded(instance));
                    }
                    store.reload(true);
                    if let Some(note) = note {
                        toast::show(store.ui, note, None);
                    }
                    if self.alive() {
                        finish(store, id);
                    }
                }
                other => {
                    if !self.alive() {
                        return;
                    }
                    self.saving.set(false);
                    self.save_error.set(Some(match other {
                        Some(Err(e)) => forms::explain_as(tr("Nicht angelegt."), &e),
                        _ => Problem::plain(forms::unexplained()),
                    }));
                }
            }
        });
    }
}

#[derive(PartialEq)]
struct Create {
    origin: Origin,
}

impl Component for Create {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let total = use_hook(forms::total_ram_mb);
        let memory_max = forms::memory_max(total);
        let start = self.origin;
        let origin = use_state(move || start);
        let flow = Make {
            store,
            name: use_state(String::new),
            name_auto: use_state(String::new),
            version: use_state(String::new),
            loader: use_state(Loader::default),
            list: use_state(Vec::new),
            versions: use_state(|| Load::Loading),
            supported: use_state(|| None),
            memory: use_state(|| crate::core::settings::active().game.memory_mb),
            free: use_state(|| None),
            saving: use_state(|| false),
            save_error: use_state(|| None),
            tried: use_state(|| false),
            details: use_state(HashSet::new),
            with_mods: use_state(|| true),
        };
        use_hook(move || {
            flow.load_versions(false);
            flow.read_memory();
        });
        let kind = flow.loader.read().name().to_owned();
        use_side_effect_with_deps(&kind, move |kind| flow.check_supported(kind.clone()));
        let back = *store.back.read();
        use_side_effect_with_deps(&back, move |_| flow.retry_failed());

        match *origin.read() {
            Origin::Import => return import::Import { origin }.into_element(),
            Origin::Pack => return packs::Packs { origin }.into_element(),
            Origin::New => {}
        }

        let version = flow.version.read().clone();
        let loader = flow.loader.read().clone();
        let versions = flow.versions.read().clone();
        let list = flow.list.read().clone();
        let everything = *store.ui.all_versions.read();
        let saving = *flow.saving.read();
        let tried = *flow.tried.read();
        let name_text = flow.name.read().clone();
        let locked = saving.then(|| tr("Wird gerade gesichert …"));
        let memory = *flow.memory.read();

        let name_error = flow.name_problem().filter(|_| tried || !name_text.is_empty());
        let name_row = Line::new("instance.name.row", tr("Name")).below(Field::new("instance.name", flow.name).placeholder(tr("z. B. Technik")).error(name_error).disabled(locked.clone()).auto_focus());

        let version_row = match &versions {
            Load::Failed(why) if list.is_empty() => Line::new("instance.version.row", tr("Minecraft-Version"))
                .hint(why.text.clone())
                .hint_tone(t.danger_text)
                .control(Button::new("instance.versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| flow.load_versions(true))),
            Load::Loading if list.is_empty() => Line::new("instance.version.row", tr("Minecraft-Version")).control(placeholder(&t, frame::PICK_W)),
            _ => {
                let known = flow.supported.read().as_ref().filter(|(k, _)| k == loader.name()).and_then(|(_, s)| s.clone());
                let offered = shown(&list, everything, known.as_ref(), &version);
                let labels = offered.iter().map(version_text).collect();
                let at = offered.iter().position(|v| v.id == version).unwrap_or(0);
                let select = Select::new("instance.version", tr("Minecraft-Version"), labels, at).width(frame::PICK_W).disabled(locked.clone()).on_choose(move |i| {
                    if let Some(v) = offered.get(i) {
                        flow.change_version(v.id.clone());
                    }
                });
                let snapshots = SwitchLabel { id: "instance.everything", text: tr("Snapshots zeigen"), on: everything, disabled: locked.clone(), on_toggle: EventHandler::new(move |on: bool| remember_all_versions(store, on)) };
                let mut row = Line::new("instance.version.row", tr("Minecraft-Version")).control(select).below(snapshots);
                if tried && version.is_empty() {
                    row = row.hint(tr("Wähle eine Version.")).hint_tone(t.danger_text);
                } else if let Some(hint) = companion_hint(&version) {
                    row = row.hint(hint);
                }
                row
            }
        };

        let chosen = LOADERS.iter().position(|l| loader.same_kind(l)).unwrap_or(0);
        let segments = LOADERS
            .iter()
            .map(|l| match l {
                Loader::Vanilla => Segment { id: "instance.loader.vanilla".into(), label: tr("Ohne Mods"), tip: None },
                other => Segment { id: format!("instance.loader.{}", other.name().to_lowercase()), label: other.name().to_owned(), tip: None },
            })
            .collect();
        let loader_pick = Segmented::new(tr("Loader"), segments, chosen).disabled(locked.clone()).on_choose(move |i| {
            if let Some(next) = LOADERS.get(i) {
                flow.pick_loader(next.clone());
            }
        });
        let loader_row = Line::new("instance.loader.row", tr("Loader")).control(loader_pick);

        let press = pressure(memory, total, *flow.free.read(), memory_max);
        let stepper = Stepper::new("instance.memory", tr("Arbeitsspeicher"), i64::from(memory), i64::from(forms::MEMORY_MIN), i64::from(memory_max.max(forms::MEMORY_MIN)))
            .step(i64::from(forms::MEMORY_STEP))
            .unit(tr("GB"))
            .typing(Typing {
                parse: |text| forms::parse_memory_gb(text).map(i64::from),
                show: |mb| forms::memory_gb(u32::try_from(mb).unwrap_or(forms::MEMORY_MIN)),
                hint: trf("%@ bis %@", &[&forms::memory_gb(forms::MEMORY_MIN), &forms::memory_label(memory_max.max(forms::MEMORY_MIN))]),
            })
            .disabled(locked.clone())
            .on_change(move |mb| flow.set_memory(u32::try_from(mb).unwrap_or(forms::MEMORY_MIN)));
        let mut memory_row = Line::new("instance.memory.row", tr("Arbeitsspeicher")).control(stepper);
        if let Some(text) = strain_text(&press, memory, total) {
            memory_row = memory_row.below(strain_row(&t, flow, &press, text, t.danger_text));
        } else if let Some(fit) = press.snug {
            memory_row = memory_row.hint(trf("%@. Das passt, wird aber knapp.", &[&crate::ui::model::fit_line(1, memory, &fit)]));
        } else {
            memory_row = memory_row.hint(tr("Pro Account."));
        }

        let modded = loader.takes_mods();
        let on = *flow.with_mods.read() && modded;
        let mut with = flow.with_mods;
        let mut flip = flow.with_mods;
        let mut mods_row = Line::new("instance.mods.row", tr("Performance-Mods"))
            .hint(if modded { mods_hint(&loader) } else { tr("Ohne Loader nicht möglich.") })
            .dim(!modded)
            .control(Switch::new("instance.mods.on", tr("Performance-Mods"), on).disabled(!modded || saving).on_toggle(move |next| with.set(next)));
        if modded && !saving {
            mods_row = mods_row.on_press(move |_| {
                let next = !*flip.peek();
                flip.set(next);
            });
        }

        let rows = vec![name_row.into_element(), version_row.into_element(), loader_row.into_element(), memory_row.into_element(), mods_row.into_element()];
        let form = rect().width(Size::fill()).padding(frame::RING).child(group(&t, None, rows, None));
        let mut body = rect()
            .width(Size::fill())
            .spacing(space::M)
            .child(origins(origin, locked.clone()))
            .child(ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px(body_max(store))).child(form));
        if let Some(problem) = flow.save_error.read().clone() {
            body = body.child(problem_view(&t, "instance.save", Icon::CircleAlert, &problem, flow.details));
        }
        let version_reason = version.is_empty().then(|| match &versions {
            Load::Loading => tr("Versionen werden geladen …"),
            Load::Failed(_) => tr("Versionen nicht geladen."),
            Load::Ready(()) => tr("Wähle eine Version."),
        });
        let reason = version_reason.or_else(|| flow.name_problem()).or_else(|| locked.clone());
        let primary = Primary { label: tr("Erstellen"), disabled: reason, danger: false, busy: saving, run: EventHandler::new(move |_| flow.create()) };
        Card::new("instance", tr("Instanz erstellen")).width(WIDE).body(body).primary(primary).primary_id("instance.save").locked(locked).into_element()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pressure {
    over: bool,
    tight: Option<Fit>,
    snug: Option<Fit>,
    lower: Option<u32>,
}

fn pressure(memory: u32, total: u64, free: Option<u64>, memory_max: u32) -> Pressure {
    let over = u64::from(memory) > total;
    let measured = free.map(|free| fit::fit_in(1, memory, Memory { total_mb: total, free_mb: free }));
    let snug = measured.filter(|f| f.fits() && f.needed_mb * 100 > f.free_mb * SNUG_PERCENT);
    let tight = measured.filter(|f| !f.fits());
    let target = if over { Some(memory_max) } else { tight.map(|f| forms::memory_step(u32::try_from(f.free_mb).unwrap_or(u32::MAX), 0, memory_max)) };
    Pressure { over, tight, snug, lower: target.filter(|to| *to < memory) }
}

fn strain_text(p: &Pressure, memory: u32, total: u64) -> Option<String> {
    if p.over {
        Some(trf("Das ist mehr, als dieser Rechner hat (%@).", &[&forms::memory_label(u32::try_from(total).unwrap_or(u32::MAX))]))
    } else {
        p.tight.map(|fit| crate::ui::model::fit_line(1, memory, &fit))
    }
}

fn strain_row(t: &Theme, flow: Make, p: &Pressure, text: String, color: Color) -> Element {
    let mut row = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(rect().width(Size::flex(1.)).child(notice(t, Icon::CircleAlert, color, text)));
    if let Some(to) = p.lower {
        row = row.child(Button::new("instance.memory.fix", tr("Arbeitsspeicher senken")).plain().on_press(move |_| flow.set_memory(to)));
    }
    row.into_element()
}

#[cfg(test)]
mod tests {
    use super::{build_edit, followed_name, mods_hint, name_problem, pressure, skipped_note};
    use crate::core::batch::ModRef;
    use crate::core::instance::Loader;

    fn a_mod() -> ModRef {
        ModRef { project_id: "p".into(), version_id: "v".into(), title: "Mod".into(), file_name: "mod.jar".into(), url: String::new(), sha1: String::new() }
    }

    #[test]
    fn an_empty_name_is_a_problem() {
        assert!(name_problem("  ", &[]).is_some());
    }

    #[test]
    fn a_taken_name_is_a_problem_in_any_case() {
        assert!(name_problem("farm", &["Farm"]).is_some());
        assert!(name_problem(" ÄRGER ", &["ärger"]).is_some());
        assert!(name_problem("Farm 2", &["Farm"]).is_none());
    }

    #[test]
    fn a_long_name_is_a_problem() {
        assert!(name_problem(&"x".repeat(200), &[]).is_some());
    }

    #[test]
    fn every_mod_loader_keeps_the_mods_and_vanilla_takes_none() {
        let fabric = build_edit(" Farm ", "26.3", &Loader::Fabric(None), Some(2048), vec![a_mod()]);
        assert_eq!(fabric.name, "Farm");
        assert_eq!(fabric.mods.len(), 1);
        for loader in [Loader::Quilt(None), Loader::Forge(None), Loader::NeoForge(None)] {
            assert_eq!(build_edit("Farm", "26.3", &loader, Some(2048), vec![a_mod()]).mods.len(), 1);
        }
        assert!(build_edit("Farm", "26.3", &Loader::Vanilla, Some(2048), vec![a_mod()]).mods.is_empty());
    }

    #[test]
    fn a_new_instance_starts_with_the_loader_unpinned_and_no_java_arguments() {
        let edit = build_edit("A", "26.3", &Loader::Fabric(None), None, Vec::new());
        assert_eq!(edit.loader, Loader::Fabric(None));
        assert!(edit.jvm_args.is_empty());
        assert_eq!(edit.memory_mb, None);
    }

    #[test]
    fn the_name_follows_the_version_until_the_person_types() {
        let first = followed_name("", "", "26.3", &[]);
        assert!(first.as_deref().is_some_and(|n| n.contains("26.3")));
        let auto = first.unwrap_or_default();
        assert!(followed_name(&auto, &auto, "26.2", &[]).is_some_and(|n| n.contains("26.2")));
        assert!(followed_name("Farm", &auto, "26.2", &[]).is_none());
        assert!(followed_name("", &auto, "26.2", &[]).is_none());
        assert!(followed_name("", "", "", &[]).is_none());
    }

    #[test]
    fn the_followed_name_skips_names_already_taken() {
        crate::core::i18n::init("de");
        assert_eq!(followed_name("", "", "26.3", &["minecraft 26.3"]).as_deref(), Some("Minecraft 26.3 (2)"));
        assert_eq!(followed_name("", "", "26.3", &["Minecraft 26.3", "Minecraft 26.3 (2)"]).as_deref(), Some("Minecraft 26.3 (3)"));
    }

    #[test]
    fn left_out_mods_are_named_and_a_complete_preset_says_nothing() {
        crate::core::i18n::init("de");
        assert!(skipped_note(&[]).is_none());
        let note = skipped_note(&["Gnetum".to_owned(), "Ixeris".to_owned()]).unwrap();
        assert!(note.contains("Gnetum, Ixeris"), "{note}");
    }

    #[test]
    fn the_hint_names_the_first_two_of_the_preset_and_counts_the_rest() {
        crate::core::i18n::init("de");
        let names = crate::core::modrinth::default_names(&Loader::Fabric(None));
        let hint = mods_hint(&Loader::Fabric(None));
        assert!(hint.starts_with(&format!("{}, {} und {} weitere", names[0], names[1], names.len() - 2)), "{hint}");
    }

    #[test]
    fn memory_that_fits_is_left_alone() {
        let p = pressure(2048, 16_384, Some(8192), 14_336);
        assert!(p.tight.is_none() && !p.over);
        assert!(p.snug.is_none());
        assert!(p.lower.is_none());
    }

    #[test]
    fn memory_over_the_machine_is_lowered_to_the_most_allowed() {
        let p = pressure(20_480, 16_384, Some(8192), 14_336);
        assert!(p.over);
        assert_eq!(p.lower, Some(14_336));
    }

    #[test]
    fn memory_over_what_is_free_is_lowered_to_what_fits() {
        let p = pressure(6144, 16_384, Some(3000), 14_336);
        assert!(p.tight.is_some());
        assert_eq!(p.lower, Some(2560));
    }

    #[test]
    fn nothing_fits_below_the_minimum_so_there_is_no_fix_at_the_minimum() {
        let p = pressure(1024, 16_384, Some(500), 14_336);
        assert!(p.tight.is_some());
        assert!(p.lower.is_none());
        assert_eq!(pressure(2048, 16_384, Some(500), 14_336).lower, Some(1024));
    }

    #[test]
    fn memory_that_barely_fits_is_snug_and_unmeasured_memory_is_never_strained() {
        assert!(pressure(3000, 16_384, Some(3500), 14_336).snug.is_some());
        let p = pressure(3000, 16_384, None, 14_336);
        assert!(p.tight.is_none() && !p.over);
    }
}
