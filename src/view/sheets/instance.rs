//! The new-instance sheet, which sets up an instance with version, loader, mods and memory.

use std::collections::HashSet;

use freya::prelude::*;
use uuid::Uuid;

use crate::app::event::Event as AppEvent;
use crate::core::batch::ModRef;
use crate::core::companion;
use crate::core::fit::{self, Fit, Memory};
use crate::core::i18n::{tr, trc, trf};
use crate::core::install::Versions;
use crate::core::instance::{Instance, Loader};
use crate::core::modrinth::{Kind as ModKind, Update};
use crate::core::settings;
use crate::core::snapshot::Reason;
use crate::core::versions::{GameVersion, Kind, shown};
use crate::ui::forms::{self, Problem};
use crate::ui::remote::{self, InstanceEdit};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::instance::{Busy, snapshots, version_text};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::spinner;
use crate::view::kit::switch::SwitchLabel;
use crate::view::kit::text::{caption, line, mono, para, strong};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{at_least, off_ui, ring, toast, use_theme};
use crate::view::settings::remember_all_versions;
use crate::view::sheets::browser::{Browser, Scope};
use crate::view::sheets::import;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};
use crate::view::ui::Page;

const WIDE: f32 = frame::SHEET;
const BODY_MIN: f32 = 160.;
const BODY_MAX: f32 = 620.;
const CHROME: f32 = 280.;
const LIST_MAX: f32 = 220.;
const ROW: f32 = control::M + space::XS;
const DEFAULT_ROWS: usize = 4;
const SNUG_PERCENT: u64 = 80;
const DETAIL_LINES: usize = 8;
const BOX_EDGES: f32 = 2.;
const MOD_LOADERS: [Loader; 4] = [Loader::Fabric(None), Loader::Quilt(None), Loader::NeoForge(None), Loader::Forge(None)];

/// Opens the sheet that sets up a new instance; with `play`, it also starts the instance with that account (or the
/// first one) once it is made.
pub fn create(store: Store, play: Option<Option<String>>) {
    sheet::open(store.ui, Create { play, origin: Origin::New });
}

/// Opens the new-instance sheet on Importieren.
pub fn import(store: Store) {
    if store.ui.sheet.peek().is_none() {
        sheet::open(store.ui, Create { play: None, origin: Origin::Import });
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

/// Shows the Mods tab of instance `id`.
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

fn build_edit(name: &str, version: &str, loader: &Loader, memory_mb: Option<u32>, args: &str, mods: Vec<ModRef>, with_mods: bool) -> InstanceEdit {
    InstanceEdit {
        name: name.trim().to_owned(),
        game_version: version.to_owned(),
        loader: loader.clone(),
        memory_mb,
        jvm_args: args.split_whitespace().map(str::to_owned).collect(),
        options: Default::default(),
        mods: if loader.takes_mods() && with_mods { mods } else { Vec::new() },
    }
}

fn default_gaps(found: usize, notes: &[String], version: &str) -> Option<String> {
    let skipped = notes.iter().filter(|n| n.starts_with("skipped ")).count();
    (skipped > 0).then(|| trf("%1$lld von %2$lld Performance-Mods für %3$@ verfügbar", &[&found, &(found + skipped), &version]))
}

fn waiting_reason(saving: bool, held: Option<String>, updating: bool) -> Option<String> {
    if saving {
        Some(tr("Wird gerade gesichert …"))
    } else if held.is_some() {
        held
    } else if updating {
        Some(tr("Warte, bis die Updates fertig sind."))
    } else {
        None
    }
}

fn form_dirty(touched: bool, name: &str, auto: &str) -> bool {
    touched || (!name.trim().is_empty() && name.trim() != auto)
}

fn adding_wait() -> String {
    tr("Einen Moment, die Mods werden hinzugefügt …")
}

fn unsaved_reason() -> String {
    tr("Sichere zuerst die Änderungen.")
}

fn adding_reason(version: &str, lock: Option<String>) -> Option<String> {
    if version.is_empty() { Some(tr("Wähle zuerst eine Version.")) } else { lock }
}

fn add_button(reason: Option<String>) -> Button {
    Button::new("instance.sheet.mods.add", tr("Mods hinzufügen…")).glyph(Icon::Plus).disabled(reason)
}

fn mods_header(t: &Theme, count: usize, add: Button) -> Element {
    rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).content(Content::flex()).child(rect().width(Size::flex(1.)).child(label(t, trf("Mods (%lld)", &[&count])))).child(add).into_element()
}

fn mods_title(count: usize, touched: bool) -> String {
    if touched { trf("Mods (%lld)", &[&count]) } else { trf("Performance-Mods (%lld)", &[&count]) }
}

pub(super) fn body_max(store: Store) -> f32 {
    let (_, window_h) = *store.ui.window.read();
    (window_h - CHROME).clamp(BODY_MIN, BODY_MAX)
}

fn rows_height(rows: usize) -> f32 {
    (rows as f32 * ROW + space::XXS * 2. + BOX_EDGES).min(LIST_MAX)
}

fn label(t: &Theme, text: String) -> Label {
    line(t, text, font::CALLOUT, t.text2).font_weight(FontWeight::MEDIUM)
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

fn waiting(t: &Theme, text: String, rows: usize) -> Element {
    rect().width(Size::fill()).height(Size::px(rows_height(rows))).main_align(Alignment::Center).child(spinner::waiting(t, text)).into_element()
}

fn version_height() -> f32 {
    control::M + space::XS + frame::SWITCH_H.max(font::CALLOUT * font::LINE)
}

fn rows_box(t: &Theme, rows: Vec<Element>, height: f32) -> Element {
    rect()
        .width(Size::fill())
        .corner_radius(t.shape.control)
        .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
        .overflow(Overflow::Clip)
        .child(ScrollView::new().width(Size::fill()).height(Size::px(height)).child(rect().width(Size::fill()).padding((space::XXS, 0.)).children(rows)))
        .into_element()
}

fn file_row(t: &Theme, name: String) -> Element {
    rect()
        .width(Size::fill())
        .height(Size::px(ROW))
        .padding((0., space::XS, 0., space::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .content(Content::flex())
        .child(icon(t, Icon::Package, iconsize::S, t.text3))
        .child(line(t, name, font::BODY, t.text1).width(Size::flex(1.)))
        .into_element()
}

fn local_files(t: &Theme, names: &[String]) -> Element {
    let rows = names.iter().map(|n| file_row(t, n.clone())).collect();
    rows_box(t, rows, rows_height(names.len()))
}

fn mod_list(t: &Theme, mods: &[ModRef], locked: Option<String>, remove: impl Fn(String) + Clone + 'static) -> Element {
    let rows = mods
        .iter()
        .map(|m| {
            let pid = m.project_id.clone();
            let remove = remove.clone();
            let button = Button::icon(format!("instance.mod.{pid}.remove"), Icon::Close, trf("„%@“ entfernen", &[&m.title])).disabled(locked.clone()).on_press({
                let pid = pid.clone();
                move |_| remove(pid.clone())
            });
            rect()
                .key(pid)
                .width(Size::fill())
                .height(Size::px(ROW))
                .padding((0., space::XS, 0., space::M))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::S)
                .content(Content::flex())
                .child(icon(t, Icon::Package, iconsize::S, t.text3))
                .child(line(t, m.title.clone(), font::BODY, t.text1).width(Size::flex(1.)))
                .child(button)
                .into_element()
        })
        .collect();
    rows_box(t, rows, rows_height(mods.len()))
}

fn running_notice(t: &Theme) -> Element {
    notice(t, Icon::CircleAlert, t.accent_text, tr("Diese Instanz läuft. Änderungen an den Mods gelten beim nächsten Start."))
}

fn finish(store: Store, id: String, play: Option<Option<String>>) {
    sheet::close(store.ui);
    let mut page = store.ui.page;
    page.set(Page::Instance(id.clone()));
    if let Some(account) = play {
        store.dispatch(AppEvent::PlayInstance { instance: id, account: account.and_then(|a| Uuid::parse_str(&a).ok()) });
    }
}

#[derive(Clone, Copy)]
struct Make {
    store: Store,
    play: State<Option<Option<String>>>,
    name: State<String>,
    name_auto: State<String>,
    version: State<String>,
    loader: State<Loader>,
    list: State<Vec<GameVersion>>,
    versions: State<Load<()>>,
    supported: State<Option<(String, Option<HashSet<String>>)>>,
    memory: State<u32>,
    advanced: State<bool>,
    args: State<String>,
    free: State<Option<u64>>,
    mods: State<Vec<ModRef>>,
    touched: State<bool>,
    busy: State<Option<String>>,
    mods_error: State<Option<Problem>>,
    notes: State<Vec<String>>,
    ticket: State<u64>,
    saving: State<bool>,
    save_error: State<Option<Problem>>,
    tried: State<bool>,
    made: State<Option<Instance>>,
    details: State<HashSet<String>>,
    searching: State<bool>,
    adding: State<HashSet<String>>,
    with_mods: State<bool>,
    show_mods: State<bool>,
    others: State<bool>,
    mod_loader: State<Loader>,
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
        if self.mods_error.peek().is_some() && self.busy.peek().is_none() && !*self.saving.peek() && self.mods.peek().is_empty() && self.adding.peek().is_empty() {
            let version = self.version.peek().clone();
            self.load_mods(version);
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

    fn takes_mods(self) -> bool {
        self.loader.peek().takes_mods()
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
        if version == *self.version.peek() || self.busy.peek().is_some() || !self.adding.peek().is_empty() {
            return;
        }
        let previous = self.version.peek().clone();
        self.version.set(version);
        let unpinned = self.loader.peek().with_version(None);
        self.loader.set(unpinned);
        self.follow_name();
        self.load_mods(previous);
    }

    fn load_mods(mut self, previous: String) {
        self.mods_error.set(None);
        let version = self.version.peek().clone();
        if version.is_empty() {
            return;
        }
        let ticket = *self.ticket.peek() + 1;
        self.ticket.set(ticket);
        if !self.takes_mods() {
            self.mods.set(Vec::new());
            self.notes.set(Vec::new());
            self.touched.set(false);
            return;
        }
        self.notes.set(Vec::new());
        let touched = *self.touched.peek();
        let current = self.mods.peek().clone();
        let loader = self.loader.peek().clone();
        self.busy.set(Some(if touched { trf("Mods werden für %@ angepasst …", &[&version]) } else { tr("Standard-Mods werden geladen …") }));
        let (game, wanted) = (version.clone(), loader);
        spawn_in_window(async move {
            let result = off_ui(move || {
                if touched {
                    remote::move_mods(&current, &game, &wanted).map(|(mods, missing)| (mods, missing.into_iter().map(|m| trf("„%@“ gibt es nicht für %@ und wurde entfernt.", &[&m, &game])).collect::<Vec<_>>()))
                } else {
                    remote::defaults(&game, &wanted).map(|d| {
                        let note = default_gaps(d.mods.len(), &d.notes, &game);
                        (d.mods, note.into_iter().collect::<Vec<_>>())
                    })
                }
            })
            .await;
            if !self.alive() || *self.ticket.peek() != ticket {
                return;
            }
            self.busy.set(None);
            if !self.takes_mods() {
                return;
            }
            match result {
                Some(Ok((mods, notes))) => {
                    self.mods.set(mods);
                    self.notes.set(notes);
                }
                Some(Err(e)) if previous.is_empty() || previous == version => {
                    self.mods.set(Vec::new());
                    self.mods_error.set(Some(forms::explain_as(tr("Mods nicht geladen."), &e)));
                }
                Some(Err(e)) => {
                    self.mods_error.set(Some(forms::explain_as(trf("Mods für %@ nicht geladen, die Version bleibt %@.", &[&version, &previous]), &e)));
                    self.version.set(previous);
                    self.follow_name();
                }
                None => self.mods_error.set(Some(Problem::plain(forms::unexplained()))),
            }
        });
    }

    fn pick_loader(mut self, next: Loader) {
        if *self.saving.peek() || self.busy.peek().is_some() || !self.adding.peek().is_empty() || self.loader.peek().same_kind(&next) {
            return;
        }
        if next.takes_mods() {
            self.mod_loader.set(next.clone());
        }
        self.loader.set(next.clone());
        self.save_error.set(None);
        self.notes.set(Vec::new());
        if next.takes_mods() {
            let version = self.version.peek().clone();
            self.load_mods(version);
        }
    }

    fn search(mut self) {
        self.searching.set(true);
    }

    fn leave_search(mut self) {
        self.searching.set(false);
    }

    fn remove_mod(mut self, project_id: &str) {
        self.mods.write().retain(|m| m.project_id != project_id);
        self.touched.set(true);
    }

    fn step_memory(mut self, delta: i32, max: u32) {
        let next = forms::memory_step(*self.memory.peek(), delta, max);
        self.memory.set(next);
        self.save_error.set(None);
    }

    fn name_problem(self) -> Option<String> {
        let model = self.store.model.peek();
        let taken: Vec<&str> = model.instances.iter().map(|i| i.name.as_str()).collect();
        name_problem(&self.name.peek(), &taken)
    }

    fn edit(self) -> InstanceEdit {
        let loader = self.loader.peek().clone();
        build_edit(&self.name.peek(), &self.version.peek(), &loader, Some(*self.memory.peek()).filter(|mb| *mb != crate::core::settings::active().game.memory_mb), &self.args.peek(), self.mods.peek().clone(), *self.with_mods.peek())
    }

    fn create(mut self) {
        self.tried.set(true);
        if *self.saving.peek() || self.busy.peek().is_some() || !self.adding.peek().is_empty() || self.name_problem().is_some() || self.version.peek().is_empty() {
            return;
        }
        let edit = self.edit();
        let store = self.store;
        let play = self.play.peek().clone();
        self.saving.set(true);
        self.save_error.set(None);
        spawn_in_window(async move {
            let result = off_ui(move || remote::create_instance(edit)).await;
            match result {
                Some(Ok(made)) => {
                    if store.model.peek().instance(&made.id).is_none() {
                        store.dispatch(AppEvent::InstanceAdded(made.clone()));
                    }
                    store.reload(true);
                    if !self.alive() {
                        return;
                    }
                    if made.local_mods.is_empty() {
                        finish(store, made.id, play);
                    } else {
                        self.saving.set(false);
                        self.made.set(Some(made));
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
    play: Option<Option<String>>,
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
            play: use_state(|| self.play.clone()),
            name: use_state(String::new),
            name_auto: use_state(String::new),
            version: use_state(String::new),
            loader: use_state(Loader::default),
            list: use_state(Vec::new),
            versions: use_state(|| Load::Loading),
            supported: use_state(|| None),
            memory: use_state(move || forms::memory_step(crate::core::settings::active().game.memory_mb, 0, memory_max)),
            advanced: use_state(|| false),
            args: use_state(String::new),
            free: use_state(|| None),
            mods: use_state(Vec::new),
            touched: use_state(|| false),
            busy: use_state(|| None),
            mods_error: use_state(|| None),
            notes: use_state(Vec::new),
            ticket: use_state(|| 0),
            saving: use_state(|| false),
            save_error: use_state(|| None),
            tried: use_state(|| false),
            made: use_state(|| None),
            details: use_state(HashSet::new),
            searching: use_state(|| false),
            adding: use_state(HashSet::new),
            with_mods: use_state(|| true),
            show_mods: use_state(|| false),
            others: use_state(|| false),
            mod_loader: use_state(Loader::default),
        };
        use_hook(move || {
            flow.load_versions(false);
            flow.read_memory();
        });
        let kind = flow.loader.read().name().to_owned();
        use_side_effect_with_deps(&kind, move |kind| flow.check_supported(kind.clone()));
        let back = *store.back.read();
        use_side_effect_with_deps(&back, move |_| flow.retry_failed());

        if let Some(made) = flow.made.read().clone() {
            return created(&t, flow, &made).into_element();
        }

        let version = flow.version.read().clone();
        let loader = flow.loader.read().clone();
        let versions = flow.versions.read().clone();
        let list = flow.list.read().clone();
        let everything = *store.ui.all_versions.read();
        let busy = flow.busy.read().clone();
        let saving = *flow.saving.read();
        let tried = *flow.tried.read();
        let name_text = flow.name.read().clone();
        let play = flow.play.read().is_some();
        let adding = !flow.adding.read().is_empty();
        let blocked = busy.clone().or_else(|| saving.then(|| tr("Wird gerade gesichert …"))).or_else(|| adding.then(adding_wait));

        match *origin.read() {
            Origin::Import => return import::Import { origin }.into_element(),
            Origin::Pack => {
                let primary = Primary { label: tr("Modpacks durchsuchen"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| crate::view::sheets::browser::find_modpacks(store)) };
                let body = rect().width(Size::fill()).spacing(space::M).child(origins(origin, None)).child(PackDrop);
                return Card::new("instance", tr("Instanz erstellen")).width(WIDE).body(body).primary(primary).primary_id("instance.pack.find").into_element();
            }
            Origin::New => {}
        }

        if *flow.searching.read() {
            let scope = Scope {
                create: true,
                page: false,
                made: None,
                version: version.clone(),
                loader: loader.clone(),
                mods: flow.mods,
                touched: Some(flow.touched),
                adding: flow.adding,
                updatable: HashSet::new(),
                updating: HashSet::new(),
                saving,
                held: busy.as_ref().map(|_| tr("Einen Moment, die Mods werden geladen …")),
                pending: false,
                dirty: form_dirty(*flow.touched.read(), &name_text, &flow.name_auto.read()),
                follow: Some((flow.version, flow.loader)),
                kind: ModKind::Mod,
                filters: None,
                on_update: EventHandler::new(|_| {}),
                back: EventHandler::new(move |_| flow.leave_search()),
                done: EventHandler::new(move |_| flow.leave_search()),
            };
            return Browser { scope }.into_element();
        }

        let version_control: Element = match &versions {
            Load::Failed(why) if list.is_empty() => rect()
                .width(Size::fill())
                .spacing(space::XS)
                .child(problem_view(&t, "instance.versions", Icon::CircleAlert, why, flow.details))
                .child(rect().horizontal().child(Button::new("instance.versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| flow.load_versions(true))))
                .into_element(),
            Load::Loading if list.is_empty() => rect().height(Size::px(version_height())).child(rect().height(Size::px(control::M)).main_align(Alignment::Center).child(spinner::waiting(&t, tr("Versionen werden geladen …")))).into_element(),
            _ => {
                let known = flow.supported.read().as_ref().filter(|(k, _)| k == loader.name()).and_then(|(_, s)| s.clone());
                let offered = shown(&list, everything, known.as_ref(), &version);
                let labels = offered.iter().map(version_text).collect();
                let at = offered.iter().position(|v| v.id == version).unwrap_or(0);
                let select = Select::new("instance.version", tr("Minecraft-Version"), labels, at).width(frame::PICK_W).disabled(blocked.clone()).on_choose(move |i| {
                    if let Some(v) = offered.get(i) {
                        flow.change_version(v.id.clone());
                    }
                });
                let row = SwitchLabel {
                    id: "instance.everything",
                    text: tr("Snapshots zeigen"),
                    on: everything,
                    disabled: blocked.clone(),
                    on_toggle: EventHandler::new(move |on: bool| remember_all_versions(store, on)),
                };
                rect().width(Size::fill()).spacing(space::XS).child(select).child(row).into_element()
            }
        };
        let version_note: Option<Element> = if tried && version.is_empty() {
            Some(line(&t, tr("Wähle eine Version."), font::CAPTION, t.danger_text).into_element())
        } else {
            companion_hint(&version).map(|h| WithTip::new(Some(tr("Ab dieser Version spart Manycraft Rechenzeit, solange die Instanz im Hintergrund läuft.")), caption(&t, h)).fill().into_element())
        };
        let version_column = rect().width(Size::px(frame::PICK_W)).spacing(space::XS).child(label(&t, tr("Minecraft-Version"))).child(version_control).children(version_note);
        let name_error = flow.name_problem().filter(|_| tried || !name_text.is_empty());
        let name_column = rect().width(Size::flex(1.)).child(Field::new("instance.name", flow.name).label(tr("Name")).placeholder(tr("z. B. Technik")).error(name_error).auto_focus());
        let top = rect().width(Size::fill()).horizontal().spacing(space::M).content(Content::flex()).child(name_column).child(version_column);

        let loader_column = loader_section(flow, &loader, blocked.clone());

        let press = pressure(*flow.memory.read(), total, *flow.free.read(), memory_max);
        let mut form = rect().width(Size::fill()).padding(frame::RING).spacing(space::M).child(top);
        if !*flow.advanced.read()
            && let Some(text) = strain_text(&press, *flow.memory.read(), total)
        {
            form = form.child(strain_row(&t, flow, &press, text, t.attention_text));
        }
        form = form.child(loader_column);
        if loader.takes_mods() {
            form = form.child(mods_section(&t, flow, &loader));
        }
        form = form.child(advanced(&t, flow, &loader, &version, blocked.clone(), total, memory_max, &press));

        let mut body = rect().width(Size::fill()).spacing(space::M).child(origins(origin, blocked.clone().or_else(|| saving.then(|| tr("Wird gerade gesichert …"))))).child(ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px(body_max(store))).child(form));
        if let Some(problem) = flow.save_error.read().clone() {
            body = body.child(problem_view(&t, "instance.save", Icon::CircleAlert, &problem, flow.details));
        }
        let version_reason = version.is_empty().then(|| match &versions {
            Load::Loading => tr("Versionen werden geladen …"),
            Load::Failed(_) => tr("Versionen nicht geladen."),
            Load::Ready(()) => tr("Wähle eine Version."),
        });
        let reason = version_reason.or_else(|| flow.name_problem()).or_else(|| blocked.clone());
        let primary = Primary { label: if play { tr("Erstellen und spielen") } else { tr("Erstellen") }, disabled: reason, danger: false, busy: saving, run: EventHandler::new(move |_| flow.create()) };
        Card::new("instance", tr("Instanz erstellen")).width(WIDE).body(body).primary(primary).primary_id("instance.save").locked(saving.then(|| tr("Wird gerade gesichert …"))).into_element()
    }
}

fn created(t: &Theme, flow: Make, made: &Instance) -> Card {
    let store = flow.store;
    let (id, play) = (made.id.clone(), flow.play.peek().clone());
    let body = rect()
        .width(Size::fill())
        .spacing(space::M)
        .child(notice(t, Icon::CircleCheck, t.success_text, trf("„%@“ ist angelegt: %lld Mods von Modrinth, %lld eigene Dateien.", &[&made.name, &made.mods.len(), &made.local_mods.len()])))
        .child(label(t, tr("Nicht auf Modrinth")))
        .child(local_files(t, &made.local_mods))
        .child(para(t, tr("Diese Dateien bleiben unverändert in der Instanz. Sie werden nicht automatisch aktualisiert und beim Synchronisieren nie gelöscht."), font::CALLOUT, t.text2));
    let done = Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| finish(store, id.clone(), play.clone())) };
    Card::new("instance", tr("Instanz erstellt")).width(WIDE).body(body).primary(done).primary_id("instance.done").inform()
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pressure {
    over: bool,
    tight: Option<Fit>,
    snug: Option<Fit>,
    lower: Option<u32>,
}

impl Pressure {
    fn strained(&self) -> bool {
        self.over || self.tight.is_some()
    }
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
        let mut memory = flow.memory;
        let mut error = flow.save_error;
        row = row.child(Button::new("instance.memory.fix", tr("Arbeitsspeicher senken")).plain().on_press(move |_| {
            memory.set(to);
            error.set(None);
        }));
    }
    row.into_element()
}

fn advanced(t: &Theme, flow: Make, loader: &Loader, version: &str, locked: Option<String>, total: u64, memory_max: u32, press: &Pressure) -> Element {
    let memory = *flow.memory.read();
    let open = *flow.advanced.read();
    let mut toggle = flow.advanced;
    let disclosure = Button::new("instance.advanced", tr("Erweitert")).plain().glyph(if open { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| toggle.set(!open));
    let mut column = rect().width(Size::fill()).spacing(space::S).child(rect().horizontal().child(disclosure));
    if !open {
        return column.into_element();
    }
    let less = (memory <= forms::MEMORY_MIN).then(|| trf("Weniger geht nicht: mindestens %@.", &[&forms::memory_label(forms::MEMORY_MIN)]));
    let more = (memory >= memory_max).then(|| trf("Mehr geht nicht: höchstens %@.", &[&forms::memory_label(memory_max)]));
    let stepper = rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::XS)
        .child(Button::icon("instance.memory.less", Icon::Minus, tr("Weniger Arbeitsspeicher")).disabled(less).on_press(move |_| flow.step_memory(-1, memory_max)))
        .child(rect().width(Size::px(control::STEPPER_VALUE)).center().child(strong(t, forms::memory_label(memory), font::BODY, t.text1)))
        .child(Button::icon("instance.memory.more", Icon::Plus, tr("Mehr Arbeitsspeicher")).disabled(more).on_press(move |_| flow.step_memory(1, memory_max)));
    let note: Element = if let Some(text) = strain_text(press, memory, total) {
        strain_row(t, flow, press, text, t.danger_text)
    } else if let Some(fit) = press.snug {
        para(t, trf("%@. Das passt, wird aber knapp.", &[&crate::ui::model::fit_line(1, memory, &fit)]), font::CAPTION, t.text3).into_element()
    } else {
        para(t, tr("Pro Account."), font::CAPTION, t.text3).into_element()
    };
    let memory_block = rect().width(Size::fill()).spacing(space::XS).child(label(t, tr("Arbeitsspeicher"))).child(stepper).child(note);
    let mut inner = rect().width(Size::fill()).padding((0., 0., 0., space::L)).spacing(space::M);
    if loader.takes_mods() {
        let mut pin = flow.loader;
        let unpinned = loader.with_version(None);
        inner = inner.child(LoaderVersion { loader: loader.clone(), game: version.to_owned(), locked, on_pick: EventHandler::new(move |v: Option<String>| pin.set(unpinned.with_version(v))) });
    }
    column = column.child(inner.child(memory_block).child(Field::new("instance.args", flow.args).label(tr("Java-Argumente"))));
    column.into_element()
}

fn loader_section(flow: Make, loader: &Loader, blocked: Option<String>) -> Element {
    let modded = loader.takes_mods();
    let remembered = flow.mod_loader.read().clone();
    let mod_name = if modded { loader.name() } else { remembered.name() };
    let segments = vec![
        Segment { id: "instance.loader.mods".into(), label: trf("Mit Mods (%@)", &[&mod_name]), tip: None },
        Segment { id: "instance.loader.vanilla".into(), label: tr("Ohne Mods"), tip: None },
    ];
    let control = Segmented::new(tr("Mods"), segments, usize::from(!modded)).disabled(blocked.clone()).on_choose(move |i| flow.pick_loader(if i == 0 { flow.mod_loader.peek().clone() } else { Loader::Vanilla }));
    let mut column = rect().width(Size::fill()).spacing(space::XS).child(rect().horizontal().child(control));
    if !modded {
        return column.into_element();
    }
    let open = *flow.others.read() || !loader.same_kind(&Loader::default());
    let mut others = flow.others;
    let toggle = Button::new("instance.loader.others", tr("Andere Loader")).plain().glyph(if open { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| others.set(!open));
    column = column.child(rect().horizontal().child(toggle));
    if open {
        let chosen = MOD_LOADERS.iter().position(|l| loader.same_kind(l)).unwrap_or(0);
        let segments = MOD_LOADERS.iter().map(|l| Segment { id: format!("instance.loader.{}", l.name().to_lowercase()), label: l.name().to_owned(), tip: None }).collect();
        let pick = Segmented::new(tr("Loader"), segments, chosen).disabled(blocked).on_choose(move |i| {
            if let Some(next) = MOD_LOADERS.get(i) {
                flow.pick_loader(next.clone());
            }
        });
        column = column.child(rect().padding((0., 0., 0., space::L)).horizontal().child(pick));
    }
    column.into_element()
}

fn mods_section(t: &Theme, flow: Make, loader: &Loader) -> Element {
    let mods = flow.mods.read().clone();
    let busy = flow.busy.read().clone();
    let saving = *flow.saving.read();
    let version = flow.version.read().clone();
    let on = *flow.with_mods.read();
    let failed = flow.mods_error.read().clone();
    let open = (*flow.show_mods.read() || failed.is_some()) && on;
    let lock = busy.clone().or_else(|| saving.then(|| tr("Wird gerade gesichert …")));
    let mut with = flow.with_mods;
    let title = if busy.is_some() { tr("Performance-Mods") } else { mods_title(mods.len(), *flow.touched.read()) };
    let switch = SwitchLabel { id: "instance.mods.on", text: title, on, disabled: lock.clone(), on_toggle: EventHandler::new(move |next: bool| with.set(next)) };
    let mut head = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(rect().width(Size::flex(1.)).child(switch));
    if let Some(text) = busy.clone().filter(|_| on) {
        head = head.child(spinner::beside(line(t, text, font::CALLOUT, t.text2)));
    } else if on {
        let mut show = flow.show_mods;
        let (label, glyph) = if open { (tr("Mods ausblenden"), Icon::ChevronDown) } else { (tr("Mods zeigen"), Icon::ChevronRight) };
        head = head.child(Button::new("instance.mods.show", label).plain().glyph(glyph).on_press(move |_| show.set(!open)));
    }
    let mut column = rect().width(Size::fill()).spacing(space::XS).child(head);
    if !on {
        return column.child(para(t, trf("Die Instanz startet mit %@ allein.", &[&loader.name()]), font::CAPTION, t.text3)).into_element();
    }
    if !open || busy.is_some() {
        return column.into_element();
    }
    let mut inner = rect().width(Size::fill()).padding((0., 0., 0., space::L)).spacing(space::XS);
    if let Some(problem) = failed {
        let retry = Button::new("instance.mods.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| {
            let version = flow.version.peek().clone();
            flow.load_mods(version);
        });
        inner = inner.child(problem_view(t, "instance.mods", Icon::CircleAlert, &problem, flow.details)).child(rect().horizontal().child(retry));
    }
    if mods.is_empty() {
        if flow.mods_error.read().is_none() {
            inner = inner.child(line(t, trf("Keine Mods. Die Instanz startet mit %@ allein.", &[&loader.name()]), font::CALLOUT, t.text2));
        }
    } else {
        inner = inner.child(mod_list(t, &mods, saving.then(|| tr("Wird gerade gesichert …")), move |pid| flow.remove_mod(&pid)));
    }
    for note in flow.notes.read().iter() {
        inner = inner.child(para(t, note.clone(), font::CAPTION, t.text3));
    }
    let more = Button::new("instance.sheet.mods.add", tr("Weitere Mods…")).glyph(Icon::Plus).plain().disabled(adding_reason(&version, lock)).on_press(move |_| flow.search());
    column.child(inner.child(rect().horizontal().child(more))).into_element()
}

const DROP_H: f32 = 180.;
const PACK_FILES: [&str; 2] = ["mrpack", "zip"];

fn pack_files(paths: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    paths.into_iter().filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| PACK_FILES.iter().any(|x| e.eq_ignore_ascii_case(x)))).collect()
}

fn install_packs(store: Store, paths: Vec<std::path::PathBuf>) {
    sheet::close(store.ui);
    crate::view::install::start(store, paths.into_iter().map(crate::view::install::Source::File).collect());
}

#[derive(PartialEq)]
struct PackDrop;

impl Component for PackDrop {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut dropping = use_state(|| false);
        let mut wrong = use_state(|| false);
        let mut picking = use_state(|| false);
        let spot = use_spot("instance.pack.drop", "group", &tr("Modpack-Datei hierher ziehen"), true);
        let pick = move |_| {
            if *picking.peek() {
                return;
            }
            picking.set(true);
            wrong.set_if_modified(false);
            spawn_in_window(async move {
                let (title, filter) = (tr("Modpack wählen"), tr("Modpack"));
                let paths = off_ui(move || crate::view::native::pick_files(&title, &filter, &PACK_FILES, true)).await.unwrap_or_default();
                if picking.try_peek().is_none() {
                    return;
                }
                picking.set(false);
                let packs = pack_files(paths);
                if !packs.is_empty() {
                    install_packs(store, packs);
                }
            });
        };
        let (note, tint) = if *wrong.read() { (tr("Nur .mrpack- und .zip-Dateien lassen sich installieren."), t.danger_text) } else { (tr(".mrpack von Modrinth oder ein Export als .zip"), t.text3) };
        let mut zone = rect()
            .width(Size::fill())
            .height(Size::px(DROP_H))
            .center()
            .spacing(space::S)
            .corner_radius(t.shape.tile)
            .background(t.well)
            .border(Border::new().fill(t.separator).width(frame::HAIRLINE).alignment(BorderAlignment::Inner))
            .on_sized(spot)
            .on_global_file_hover(move |_| {
                dropping.set_if_modified(true);
                wrong.set_if_modified(false);
            })
            .on_global_file_hover_cancelled(move |_| dropping.set_if_modified(false))
            .on_file_drop(move |e: Event<FileEventData>| {
                dropping.set_if_modified(false);
                let packs = pack_files(e.file_paths.clone());
                if packs.is_empty() {
                    wrong.set(true);
                } else {
                    install_packs(store, packs);
                }
            })
            .child(icon(&t, Icon::Package, iconsize::L, t.text3))
            .child(strong(&t, tr("Modpack-Datei hierher ziehen"), font::BODY, t.text1))
            .child(line(&t, note, font::CAPTION, tint))
            .child(Button::new("instance.pack.file", tr("Datei wählen…")).glyph(Icon::Folder).busy(*picking.read()).on_press(pick));
        if *dropping.read() {
            zone = zone.child(
                rect()
                    .position(Position::new_absolute().left(0.).top(0.))
                    .width(Size::fill())
                    .height(Size::fill())
                    .corner_radius(t.shape.tile)
                    .background(t.accent.with_a((frame::NOTICE_WASH * 255.) as u8))
                    .center()
                    .child(strong(&t, tr("Zum Installieren loslassen"), font::BODY, t.accent_text)),
            );
        }
        zone
    }
}

#[derive(PartialEq)]
struct LoaderVersion {
    loader: Loader,
    game: String,
    locked: Option<String>,
    on_pick: EventHandler<Option<String>>,
}

impl Component for LoaderVersion {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut versions = use_state(|| Load::<Versions>::Loading);
        let mut asked = use_state(|| 0u32);
        let mut ticket = use_state(|| 0u64);
        let back = *use_store().back.read();
        use_side_effect_with_deps(&back, move |_| {
            if matches!(*versions.peek(), Load::Failed(_)) {
                let next = asked.peek().wrapping_add(1);
                asked.set(next);
            }
        });
        let key = (self.loader.with_version(None), self.game.clone(), *asked.read());
        use_side_effect_with_deps(&key, move |(kind, game, _)| {
            let (kind, game) = (kind.clone(), game.clone());
            let n = *ticket.peek() + 1;
            ticket.set(n);
            if !kind.takes_mods() || game.is_empty() {
                versions.set(Load::Ready(Versions::default()));
                return;
            }
            versions.set(Load::Loading);
            spawn_in_window(async move {
                let result = off_ui(move || remote::loader_versions(&kind, &game)).await;
                if ticket.try_peek().is_none_or(|t| *t != n) {
                    return;
                }
                versions.set(match result {
                    Some(Ok(found)) => Load::Ready(found),
                    Some(Err(e)) => Load::Failed(forms::explain_as(tr("Versionen nicht geladen."), &e)),
                    None => Load::Failed(Problem::plain(forms::unexplained())),
                });
            });
        });
        let pinned = self.loader.version().map(str::to_owned);
        let (newest, found) = match &*versions.read() {
            Load::Ready(v) => (v.default.as_ref().map_or_else(|| tr("Immer die neueste"), |d| trf("Immer die neueste (%@)", &[d])), v.list.clone()),
            _ => (tr("Immer die neueste"), Vec::new()),
        };
        let mut items: Vec<Option<String>> = vec![None];
        if let Some(p) = pinned.as_ref().filter(|p| !found.contains(p)) {
            items.push(Some(p.clone()));
        }
        items.extend(found.into_iter().map(Some));
        let labels = items.iter().map(|i| i.clone().unwrap_or_else(|| newest.clone())).collect();
        let at = items.iter().position(|i| *i == pinned).unwrap_or(0);
        let heading = crate::ui::loader_version::label(&self.loader);
        let pick = self.on_pick.clone();
        let select = Select::new("instance.loader.version", heading.clone(), labels, at).width(control::VERSION_MENU).disabled(self.locked.clone()).on_choose(move |i| {
            if let Some(want) = items.get(i).cloned() {
                pick.call(want);
            }
        });
        let mut column = rect().width(Size::fill()).spacing(space::XS).child(label(&t, heading)).child(select);
        match &*versions.read() {
            Load::Loading => column = column.child(spinner::waiting(&t, tr("Versionen werden geladen …"))),
            Load::Failed(why) => {
                column = column.child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(space::S)
                        .content(Content::flex())
                        .child(para(&t, why.text.clone(), font::CAPTION, t.danger_text).width(Size::flex(1.)))
                        .child(Button::new("instance.loader.versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).plain().on_press(move |_| {
                            let next = asked.peek().wrapping_add(1);
                            asked.set(next);
                        })),
                );
            }
            Load::Ready(_) => {}
        }
        column
    }
}

#[cfg(test)]
mod tests {
    use super::{build_edit, default_gaps, followed_name, form_dirty, name_problem, pressure, waiting_reason};
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
    fn every_mod_loader_keeps_the_mods() {
        let fabric = build_edit(" Farm ", "26.3", &Loader::Fabric(None), Some(2048), "", vec![a_mod()], true);
        assert_eq!(fabric.name, "Farm");
        assert_eq!(fabric.mods.len(), 1);
        for loader in [Loader::Quilt(None), Loader::Forge(None), Loader::NeoForge(None)] {
            assert_eq!(build_edit("Farm", "26.3", &loader, Some(2048), "", vec![a_mod()], true).mods.len(), 1);
        }
        assert!(build_edit("Farm", "26.3", &Loader::Vanilla, Some(2048), "", vec![a_mod()], true).mods.is_empty());
        assert!(build_edit("Farm", "26.3", &Loader::Fabric(None), Some(2048), "", vec![a_mod()], false).mods.is_empty());
    }

    #[test]
    fn the_picked_loader_version_is_saved_and_vanilla_takes_no_mods() {
        assert_eq!(build_edit("A", "26.3", &Loader::Fabric(Some("0.17.1".into())), Some(2048), "", Vec::new(), true).loader, Loader::Fabric(Some("0.17.1".into())));
        assert_eq!(build_edit("A", "26.3", &Loader::Fabric(None), Some(2048), "", Vec::new(), true).loader, Loader::Fabric(None));
        assert!(build_edit("A", "26.3", &Loader::Vanilla, Some(2048), "", vec![a_mod()], true).mods.is_empty());
    }

    #[test]
    fn java_arguments_split_on_whitespace() {
        assert_eq!(build_edit("A", "26.3", &Loader::Fabric(None), Some(2048), " -Xss1M  -Dx=1 ", Vec::new(), true).jvm_args, ["-Xss1M", "-Dx=1"]);
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
    fn skipped_mods_are_counted_in_the_note() {
        assert!(default_gaps(3, &[], "26.3").is_none());
        assert!(default_gaps(3, &["skipped lithium: none".into()], "26.3").is_some());
    }

    #[test]
    fn memory_that_fits_is_left_alone() {
        let p = pressure(2048, 16_384, Some(8192), 14_336);
        assert!(!p.strained());
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
        assert!(!pressure(3000, 16_384, None, 14_336).strained());
    }

    #[test]
    fn a_form_is_dirty_with_mods_or_a_typed_name() {
        assert!(!form_dirty(false, "", ""));
        assert!(!form_dirty(false, "Minecraft 26.3", "Minecraft 26.3"));
        assert!(!form_dirty(false, "  ", "Minecraft 26.3"));
        assert!(form_dirty(false, "Farm", "Minecraft 26.3"));
        assert!(form_dirty(true, "", ""));
    }

    #[test]
    fn a_save_outranks_a_held_instance_which_outranks_updates() {
        assert!(waiting_reason(false, None, false).is_none());
        assert_eq!(waiting_reason(false, Some("held".into()), true).as_deref(), Some("held"));
        assert_ne!(waiting_reason(true, Some("held".into()), true).as_deref(), Some("held"));
        assert!(waiting_reason(false, None, true).is_some());
    }
}
