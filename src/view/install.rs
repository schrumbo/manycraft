//! Instances on their way in: modpacks, files and other launchers' instances, installed one after another in the
//! background, each with a page whose Play slot shows its progress and a row with a ring in the sidebar.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::core::launchers::{Found, Take};
use crate::core::modrinth::Filter;
use crate::core::mrpack::{self, Refused};
use crate::ui::catalog;
use crate::ui::remote;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::spinner::Spinner;
use crate::view::kit::text::{figure, line, para, strong};
use crate::view::kit::{after, toast, use_theme};
use crate::ui::forms::explain_as;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, motion, space};
use crate::view::ui::{Filter as Shown, Page};

const TILE: f32 = 72.;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Where a new instance comes from.
#[derive(Clone, PartialEq)]
pub enum Source {
    /// A modpack from Modrinth, by project id and title, in its newest version `filter` takes.
    Pack { project: String, title: String, filter: Filter },
    /// A `.mrpack`, a Prism export or a Prism instance folder.
    File(PathBuf),
    /// An instance of another launcher, with what to take along.
    Found(Found, Take),
}

impl Source {
    fn name(&self) -> String {
        match self {
            Source::Pack { title, .. } => title.clone(),
            Source::File(path) => path.file_stem().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| self.label()),
            Source::Found(found, _) => found.name.clone(),
        }
    }

    fn label(&self) -> String {
        match self {
            Source::Pack { title, .. } => title.clone(),
            Source::File(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string()),
            Source::Found(found, _) => found.name.clone(),
        }
    }

    fn gone(&self) -> bool {
        matches!(self, Source::File(path) if !path.exists())
    }
}

/// How far an install is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Waits for the install before it.
    Queued,
    /// Runs; `None` until the first files are counted.
    Running(Option<u8>),
    Failed(String),
}

/// One instance on its way in.
#[derive(Clone)]
pub struct Install {
    pub id: u64,
    pub name: String,
    pub stage: Stage,
    /// Set to stop it; the entry stays until it stopped.
    pub cancel: Arc<AtomicBool>,
    source: Source,
    from: Page,
}

impl PartialEq for Install {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.name == other.name && self.stage == other.stage && Arc::ptr_eq(&self.cancel, &other.cancel) && self.cancel.load(Ordering::SeqCst) == other.cancel.load(Ordering::SeqCst)
    }
}

impl Install {
    /// Whether it was asked to stop.
    pub fn stopping(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// What its Play slot and sidebar row say.
    pub fn state(&self) -> String {
        match &self.stage {
            _ if self.stopping() => tr("Bricht ab …"),
            Stage::Queued => tr("Wartet …"),
            Stage::Running(None) => tr("Wird vorbereitet …"),
            Stage::Running(Some(p)) => trf("Wird installiert · %lld %", &[p]),
            Stage::Failed(_) => tr("Nicht installiert"),
        }
    }
}

enum Report {
    Name(String),
    Percent(u8),
    Done(anyhow::Result<Instance>),
}

/// Installs `sources` one after another and shows the first one's page.
pub fn start(store: Store, sources: Vec<Source>) {
    if let Some(first) = begin(store, sources) {
        let mut page = store.ui.page;
        page.set(Page::Installing(first));
    }
}

/// Installs `sources` one after another where only the sidebar shows them; the id of the first, `None` without sources.
pub fn begin(store: Store, sources: Vec<Source>) -> Option<u64> {
    if sources.is_empty() {
        return None;
    }
    let from = store.ui.page.peek().clone();
    let from = if matches!(from, Page::Installing(_)) { Page::Grid(Shown::All) } else { from };
    let made: Vec<Install> = sources.into_iter().map(|source| Install { id: NEXT.fetch_add(1, Ordering::Relaxed), name: source.name(), stage: Stage::Queued, cancel: Arc::new(AtomicBool::new(false)), source, from: from.clone() }).collect();
    let ids: Vec<u64> = made.iter().map(|i| i.id).collect();
    let mut installs = store.ui.installs;
    installs.write().extend(made);
    let first = ids[0];
    spawn_in_window(async move {
        for id in ids {
            run(store, id).await;
        }
    });
    Some(first)
}

/// Stops install `id`; one that has not begun goes at once.
pub fn cancel(store: Store, id: u64) {
    let Some(entry) = find(store, id) else { return };
    entry.cancel.store(true, Ordering::SeqCst);
    if matches!(entry.stage, Stage::Running(_)) {
        update(store, id, |_| {});
    } else {
        close(store, id);
    }
}

fn find(store: Store, id: u64) -> Option<Install> {
    store.ui.installs.peek().iter().find(|i| i.id == id).cloned()
}

fn update(store: Store, id: u64, change: impl FnOnce(&mut Install)) {
    let mut installs = store.ui.installs;
    let mut list = installs.write();
    if let Some(entry) = list.iter_mut().find(|i| i.id == id) {
        change(entry);
    }
}

fn shown(store: Store, id: u64) -> bool {
    *store.ui.page.peek() == Page::Installing(id)
}

fn close(store: Store, id: u64) {
    let from = find(store, id).map(|i| i.from);
    let mut installs = store.ui.installs;
    installs.write().retain(|i| i.id != id);
    if shown(store, id) {
        let mut page = store.ui.page;
        page.set(from.unwrap_or(Page::Grid(Shown::All)));
    }
}

/// Installs `id` again after it failed.
fn retry(store: Store, id: u64) {
    let Some(entry) = find(store, id) else { return };
    if !matches!(entry.stage, Stage::Failed(_)) {
        return;
    }
    update(store, id, |i| {
        i.stage = Stage::Queued;
        i.cancel = Arc::new(AtomicBool::new(false));
    });
    spawn_in_window(async move { run(store, id).await });
}

async fn run(store: Store, id: u64) {
    let Some(entry) = find(store, id) else { return };
    if entry.stopping() {
        close(store, id);
        return;
    }
    update(store, id, |i| i.stage = Stage::Running(None));
    let (tx, rx) = async_channel::unbounded::<Report>();
    let (source, stop) = (entry.source.clone(), Arc::clone(&entry.cancel));
    let work = std::thread::Builder::new().name("install".into()).spawn(move || {
        let progress = |done: usize, total: usize| {
            let _ = tx.try_send(Report::Percent((done * 100).checked_div(total).unwrap_or(0).min(100) as u8));
        };
        let result = match &source {
            Source::Pack { project, title, filter } => catalog::fetch_modpack(project, title, filter).and_then(|path| {
                if stop.load(Ordering::SeqCst) {
                    anyhow::bail!(Refused::Cancelled);
                }
                remote::import_instance(&path, &stop, &progress)
            }),
            Source::File(path) => {
                if let Ok(name) = mrpack::peek(path) {
                    let _ = tx.try_send(Report::Name(name));
                }
                remote::import_instance(path, &stop, &progress)
            }
            Source::Found(found, take) => remote::import_found(found, *take, &stop, &progress),
        };
        let _ = tx.send_blocking(Report::Done(result));
    });
    if work.is_err() {
        failed(store, id, tr("Die Installation ließ sich nicht starten."));
        return;
    }
    let mut result = None;
    while result.is_none() {
        let Ok(first) = rx.recv().await else { break };
        let mut reports = vec![first];
        while let Ok(next) = rx.try_recv() {
            reports.push(next);
        }
        let (mut name, mut percent) = (None, None);
        for report in reports {
            match report {
                Report::Name(n) => name = Some(n),
                Report::Percent(p) => percent = Some(p),
                Report::Done(done) => result = Some(done),
            }
        }
        let now = find(store, id);
        let renamed = name.filter(|n| now.as_ref().is_some_and(|i| i.name != *n));
        let moved = percent.map(|p| Stage::Running(Some(p))).filter(|s| now.as_ref().is_some_and(|i| i.stage != *s));
        if renamed.is_some() || moved.is_some() {
            update(store, id, |i| {
                if let Some(n) = renamed {
                    i.name = n;
                }
                if let Some(s) = moved {
                    i.stage = s;
                }
            });
        }
        if result.is_none() {
            after(motion::FAST).await;
        }
    }
    let Some(entry) = find(store, id) else { return };
    match result {
        Some(result) => ended(store, &entry, result).await,
        None => failed(store, id, tr("Die Installation ist unerwartet abgebrochen.")),
    }
}

async fn ended(store: Store, entry: &Install, result: anyhow::Result<Instance>) {
    let id = entry.id;
    let stopped = entry.stopping();
    match result {
        Ok(made) if stopped => {
            let label = entry.source.label();
            let dropped = crate::view::kit::off_ui(move || remote::drop_import(&made)).await;
            if !matches!(dropped, Some(Ok(()))) {
                toast::show(store.ui, trf("„%@“ wurde abgebrochen, ließ sich aber nicht ganz entfernen.", &[&label]), None);
                store.reload(true);
            }
            close(store, id);
        }
        Ok(made) => {
            if store.model.peek().instance(&made.id).is_none() {
                store.dispatch(AppEvent::InstanceAdded(made.clone()));
            }
            store.reload(true);
            let here = shown(store, id);
            let mut installs = store.ui.installs;
            installs.write().retain(|i| i.id != id);
            if here {
                let mut page = store.ui.page;
                page.set(Page::Instance(made.id.clone()));
            }
            let instance = made.id.clone();
            toast::show_action(store.ui, trf("„%@“ ist bereit.", &[&made.name]), tr("Spielen"), EventHandler::new(move |_| store.dispatch(AppEvent::PlayInstance { instance: instance.clone(), account: None })));
        }
        Err(e) if stopped || is_cancelled(&e) => close(store, id),
        Err(_) if entry.source.gone() => failed(store, id, trf("„%@“ ist nicht mehr da. Wurde die Datei verschoben?", &[&entry.source.label()])),
        Err(e) => failed(store, id, explain(&entry.source.label(), &e)),
    }
}

fn failed(store: Store, id: u64, why: String) {
    if shown(store, id) {
        update(store, id, |i| i.stage = Stage::Failed(why));
    } else {
        let mut installs = store.ui.installs;
        installs.write().retain(|i| i.id != id);
        toast::show(store.ui, why, None);
    }
}

fn is_cancelled(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<Refused>() == Some(&Refused::Cancelled))
}

fn local_io(e: &anyhow::Error) -> bool {
    let io = e.chain().find_map(|c| c.downcast_ref::<std::io::Error>());
    let network = e.chain().any(|c| c.downcast_ref::<ureq::Error>().is_some()) || ["cannot download", "cannot look the mods up", "cannot find the Fabric loader"].iter().any(|m| format!("{e:#}").contains(m));
    io.is_some_and(|i| !matches!(i.kind(), std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::StorageFull)) && !network
}

fn explain(label: &str, e: &anyhow::Error) -> String {
    let Some(refused) = e.chain().find_map(|c| c.downcast_ref::<Refused>()) else {
        let what = trf("„%@“ wurde nicht installiert.", &[&label]);
        return if local_io(e) { format!("{what} {}", tr("Die Datei ist beschädigt oder lässt sich nicht lesen.")) } else { explain_as(what, e).text };
    };
    match refused {
        Refused::NotAPack => trf("„%@“ ist kein Modpack von Modrinth. Wähle eine .mrpack-Datei.", &[&label]),
        Refused::NoLoader => trf("„%@“ bringt Mods mit, nennt aber keinen Mod-Loader. Nichts wurde installiert.", &[&label]),
        Refused::NoGame => trf("„%@“ nennt keine Minecraft-Version.", &[&label]),
        Refused::Host(_) => trf("„%@“ lädt Dateien von einer Seite, die Manycraft nicht kennt. Nichts wurde installiert.", &[&label]),
        Refused::Hash(_) => trf("Eine Datei aus „%@“ kam verändert an. Nichts wurde installiert. Versuche es noch einmal.", &[&label]),
        Refused::Path(_) => trf("„%@“ enthält Dateien, die außerhalb des Instanzordners landen würden. Nichts wurde installiert.", &[&label]),
        Refused::Prism(_) => trf("„%@“ lässt sich nicht übernehmen.", &[&label]),
        Refused::Unknown => trf("„%@“ ist weder ein Modpack von Modrinth noch etwas aus Prism. Wähle eine .mrpack-Datei, einen Prism-Export oder einen Prism-Instanzordner.", &[&label]),
        Refused::Cancelled => String::new(),
    }
}

/// A ring that fills with `percent`; while nothing is counted yet it turns, and it stands still while it waits.
pub fn ring(t: &Theme, stage: &Stage, size: f32) -> Element {
    match stage {
        Stage::Running(Some(p)) => {
            let r = 9.;
            let full = std::f32::consts::TAU * r;
            let done = full * f32::from((*p).min(100)) / 100.;
            let hex = |c: Color| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke-width="3"><circle cx="12" cy="12" r="{r}" stroke="{}" stroke-opacity="{}"/><circle cx="12" cy="12" r="{r}" stroke="{}" stroke-linecap="round" stroke-dasharray="{done:.2} {full:.2}" transform="rotate(-90 12 12)"/></svg>"#,
                hex(t.text3),
                frame::RING_TRACK,
                hex(t.accent)
            );
            SvgViewer::new(Bytes::from(svg)).width(Size::px(size)).height(Size::px(size)).show_loader(false).into_element()
        }
        Stage::Running(None) => rect().width(Size::px(size)).height(Size::px(size)).center().child(Spinner).into_element(),
        Stage::Queued => icon(t, Icon::Clock, size, t.text3),
        Stage::Failed(_) => icon(t, Icon::CircleAlert, size, t.danger_text),
    }
}

/// The Play slot of an installing instance: the progress as a filling bar with its words, and Abbrechen.
#[derive(PartialEq)]
struct Slot {
    entry: Install,
}

impl Component for Slot {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let entry = &self.entry;
        let id = entry.id;
        let state = entry.state();
        let spot = use_spot(&format!("install.{id}.progress"), "progressbar", &state, false);
        let share = match entry.stage {
            Stage::Running(Some(p)) if !entry.stopping() => f32::from(p.min(100)),
            _ => 0.,
        };
        let bar = rect()
            .a11y_role(AccessibilityRole::ProgressIndicator)
            .a11y_alt(state.clone())
            .width(Size::flex(1.))
            .height(Size::px(control::M))
            .corner_radius(t.shape.control)
            .background(t.control)
            .overflow(Overflow::Clip)
            .on_sized(spot)
            .child(rect().position(Position::new_absolute().left(0.).top(0.)).width(Size::percent(share)).height(Size::fill()).background(t.accent.with_af32(frame::TILE_WASH * 2.)))
            .child(rect().width(Size::fill()).height(Size::fill()).center().child(figure(&t, state, font::BODY, t.text1).font_weight(FontWeight::MEDIUM)));
        let stopping = entry.stopping();
        let stop = Button::new(format!("install.{id}.cancel"), tr("Abbrechen")).disabled(stopping.then(|| tr("Bricht ab …"))).on_press(move |_| cancel(store, id));
        rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(bar).child(stop).into_element()
    }
}

/// The page of an installing instance: what comes in, its progress where Spielen will be, and why it failed when it did.
#[derive(PartialEq)]
pub struct InstallPage {
    pub id: u64,
}

impl Component for InstallPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let id = self.id;
        use_drop(move || {
            let mut installs = store.ui.installs;
            let failed = installs.try_peek().is_some_and(|l| l.iter().any(|i| i.id == id && matches!(i.stage, Stage::Failed(_))));
            if failed && let Some(mut list) = installs.try_write() {
                list.retain(|i| i.id != id);
            }
        });
        let entry = store.ui.installs.read().iter().find(|i| i.id == id).cloned();
        let Some(entry) = entry else { return rect().into_element() };
        let room = store.ui.window.read().0 - if *store.ui.sidebar.read() { frame::SIDEBAR + 1. } else { 0. };
        let wide = room >= frame::INSPECTOR_SPLIT;
        let tile = rect().width(Size::px(TILE)).height(Size::px(TILE)).center().corner_radius(t.shape.tile).background(t.accent.with_af32(frame::TILE_WASH)).child(icon(&t, Icon::Package, iconsize::HERO, t.accent_text));
        let mut hero = rect().width(Size::fill()).max_width(Size::px(frame::SHEET)).cross_align(Alignment::Center).spacing(space::M).child(tile).child(strong(&t, entry.name.clone(), font::TITLE2, t.text1).width(Size::fill()).max_lines(2).text_align(TextAlign::Center));
        let failed = match &entry.stage {
            Stage::Failed(why) => Some(why.clone()),
            _ => None,
        };
        match &failed {
            Some(why) => {
                let again = Button::new(format!("install.{id}.retry"), tr("Erneut versuchen")).glyph(Icon::Refresh).primary().on_press(move |_| retry(store, id));
                let leave = Button::new(format!("install.{id}.close"), tr("Schließen")).on_press(move |_| close(store, id));
                hero = hero.child(para(&t, why.clone(), font::BODY, t.text2).text_align(TextAlign::Center)).child(rect().horizontal().spacing(space::S).child(leave).child(again));
            }
            None => {
                hero = hero.child(para(&t, tr("Wird als Instanz installiert. Du kannst solange weiterarbeiten."), font::BODY, t.text2).text_align(TextAlign::Center));
                if !wide {
                    hero = hero.child(rect().width(Size::px(frame::SHEET / 2.)).child(Slot { entry: entry.clone() }));
                }
            }
        }
        let left = rect().width(Size::flex(1.)).height(Size::fill()).center().padding(space::XL).child(hero);
        let mut page = rect().width(Size::fill()).height(Size::fill()).horizontal().content(Content::flex()).child(left);
        if wide {
            let side = rect().width(Size::fill()).padding(space::L).child(if failed.is_some() {
                rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(ring(&t, &entry.stage, iconsize::M)).child(line(&t, entry.state(), font::CALLOUT, t.danger_text).max_lines(1)).into_element()
            } else {
                Slot { entry }.into_element()
            });
            page = page.child(rect().width(Size::px(1.)).height(Size::fill()).background(t.separator)).child(rect().width(Size::px(frame::INSPECTOR)).height(Size::fill()).background(t.sidebar).child(side));
        }
        page.into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_install_explains_nothing() {
        let e = anyhow::Error::new(Refused::Cancelled);
        assert!(is_cancelled(&e));
        assert!(explain("x", &e).is_empty());
    }

    #[test]
    fn every_refusal_is_explained_with_the_name() {
        for refused in [Refused::NotAPack, Refused::NoLoader, Refused::NoGame, Refused::Unknown] {
            assert!(explain("Farm", &anyhow::Error::new(refused)).contains("Farm"));
        }
    }

    #[test]
    fn a_file_is_named_without_its_extension_and_labelled_with_it() {
        let file = Source::File(PathBuf::from("some/dir/Farm Pack.mrpack"));
        assert_eq!(file.name(), "Farm Pack");
        assert_eq!(file.label(), "Farm Pack.mrpack");
        assert!(file.gone());
    }

    #[test]
    fn the_state_follows_the_stage() {
        let mut entry = Install { id: 1, name: "Farm".into(), stage: Stage::Running(Some(62)), cancel: Arc::new(AtomicBool::new(false)), source: Source::File("x.mrpack".into()), from: Page::Grid(Shown::All) };
        assert!(entry.state().contains("62"));
        entry.stage = Stage::Running(None);
        assert_ne!(entry.state(), Install { stage: Stage::Queued, ..entry.clone() }.state());
        entry.cancel.store(true, Ordering::SeqCst);
        assert_eq!(entry.state(), tr("Bricht ab …"));
    }
}
