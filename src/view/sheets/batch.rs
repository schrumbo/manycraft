//! The batch sheet: creates or edits a batch (instance, server, accounts, name, background and, opt-in, memory and load pace),
//! and deletes one with an undo.

use std::collections::{HashMap, HashSet};

use freya::prelude::*;
use uuid::Uuid;

use crate::app::event::Event as AppEvent;
use crate::app::model::Model;
use crate::core::account::AccountKind;
use crate::core::batch::{Background, Batch, RunMode, Unattended};
use crate::core::companion;
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Instance, Loader};
use crate::core::options::MEMORY_FLOOR_MB;
use crate::ui::forms::{self, Problem};
use crate::ui::remote::{self, BatchEdit, InstancePick};
use crate::view::commands::{Want, want};
use crate::view::drive::use_spot;
use crate::view::instance::memory::{Memory, Steps};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::check;
use crate::view::kit::group::{Line, group};
use crate::view::kit::field::{Field, typing};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::stepper::Stepper;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::spinner;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::{caption, figure, line, para};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{Live, off_ui, ring, toast, use_arrows, use_theme};
use crate::view::sheets::instance::{Load, WIDE, problem_view};
use crate::view::sheets::unattended;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};
use crate::view::ui::{Filter, Page, SessionUndo, Show, remember};

const FILTER_FROM: usize = 12;
const CHROME: f32 = 280.;
const BODY_MIN: f32 = 160.;
const BODY_MAX: f32 = 620.;
const LIST_MIN: f32 = 96.;
const LIST_MAX: f32 = 220.;
const LIST_SHARE: f32 = 0.5;
const PICK_ROW: f32 = control::M + space::XS;
const BOX_EDGES: f32 = frame::HAIRLINE * 2.;
const FACE: f32 = iconsize::L;
const FACE_INSET: f32 = FACE / 12.;
const LAUNCH_CHOICES: [u32; 5] = [0, 1, 2, 4, 8];
const BACKGROUND_NAMES: [&str; 4] = ["saving", "balanced", "full", "custom"];

/// Opens the sheet for a new batch; with `instance` it plays that instance, with `picked` it starts with those accounts.
pub fn open_new(store: Store, instance: Option<String>, picked: Option<HashSet<Uuid>>) {
    sheet::open(store.ui, BatchSheet { original: None, preselect: instance, picked });
}

/// Opens the sheet for the saved batch `batch_id`.
pub fn open_edit(store: Store, batch_id: &str) {
    let Some(batch) = store.model.peek().batch(batch_id).cloned() else { return };
    sheet::open(store.ui, BatchSheet { original: Some(batch), preselect: None, picked: None });
}

/// Why batch `batch_id` cannot be deleted right now: while any of its games runs.
pub fn delete_blocked(store: Store, batch_id: &str) -> Option<String> {
    is_running(&store.model.peek(), batch_id).then(blocked_text)
}

/// Asks whether to delete batch `batch_id`; deleting can be undone for as long as the window is open. While its games run,
/// a toast says why it cannot be deleted.
pub fn ask_delete(store: Store, batch_id: &str) {
    if let Some(why) = delete_blocked(store, batch_id) {
        toast::show(store.ui, why, None);
        return;
    }
    let Some(batch) = store.model.peek().batch(batch_id).cloned() else { return };
    sheet::confirm(store.ui, "batch.delete", ask_title(&batch), ask_message(), tr("Löschen"), true, move |_| delete(store, batch.clone()));
}

fn blocked_text() -> String {
    tr("Stoppe den Batch, bevor du ihn löschst.")
}

fn ask_title(batch: &Batch) -> String {
    trf("„%@“ löschen?", &[&batch.name])
}

fn ask_message() -> String {
    tr("Der Batch kommt 7 Tage in den Papierkorb. Instanz, Welten und Einstellungen bleiben erhalten.")
}

fn saving_text() -> String {
    tr("Wird gerade gesichert …")
}

fn is_running(model: &Model, id: &str) -> bool {
    model.games.values().any(|g| g.batch_id == id && g.phase.is_active())
}

/// The instance a new batch starts with: one played alone now, else the one played last, else the first by name.
fn recent(model: &Model) -> Option<String> {
    if let Some(g) = model.games.values().find(|g| g.alone() && g.phase.is_active() && model.instance(&g.instance_id).is_some()) {
        return Some(g.instance_id.clone());
    }
    let played = model.instances.iter().filter_map(|i| i.last_played().map(|at| (at, i))).max_by_key(|(at, _)| *at).map(|(_, i)| i.id.clone());
    played.or_else(|| sorted(model).first().map(|i| i.id.clone()))
}

/// The name a new batch takes from its server's host, else from its instance, made unique among the batches; an IP
/// address never becomes a name.
fn name_for(model: &Model, instance: Option<&str>, server: &str) -> String {
    let taken: Vec<&str> = model.batches.iter().map(|b| b.name.as_str()).collect();
    if let Some(host) = forms::server_host(server) {
        return forms::unique_name(&host, &taken);
    }
    match instance.and_then(|id| model.instance(id)) {
        Some(i) => forms::unique_name(&i.name, &taken),
        None => forms::next_batch_name(&taken),
    }
}

/// The accounts a new batch starts with: those in no batch yet, as many as fit into memory at `each_mb`; with how many
/// were free when that is more than fit.
fn free_accounts(model: &Model, each_mb: u32, memory_max: u32) -> (HashSet<Uuid>, Option<usize>) {
    let free: Vec<Uuid> = model.accounts.iter().map(|a| a.id).filter(|id| !model.batches.iter().any(|b| b.accounts.contains(id))).collect();
    let fit = (memory_max / each_mb.max(1)).max(1) as usize;
    let capped = (free.len() > fit).then_some(free.len());
    (free.into_iter().take(fit).collect(), capped)
}

fn sorted(model: &Model) -> Vec<&Instance> {
    let mut listed: Vec<&Instance> = model.instances.iter().collect();
    listed.sort_by_key(|i| (i.name.to_lowercase(), i.id.clone()));
    listed
}

fn companion_hint(game: &str) -> Option<String> {
    (!game.is_empty() && !companion::available(game)).then(|| trf("Hintergrundmodus ab %@", &[&companion::first()]))
}

fn leave(store: Store, id: &str) {
    let mut page = store.ui.page;
    if *page.peek() == Page::Grid(Filter::Batch(id.to_owned())) {
        page.set(Page::Grid(Filter::All));
    }
}

fn delete(store: Store, batch: Batch) {
    if let Some(why) = delete_blocked(store, &batch.id) {
        toast::show(store.ui, why, None);
        return;
    }
    spawn_in_window(async move {
        let doomed = batch.clone();
        match off_ui(move || remote::delete_batch(&doomed)).await {
            Some(Ok(_)) => {
                leave(store, &batch.id);
                offer_restore(store, batch);
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(tr("Nicht gelöscht."), &e).text, None);
            }
            None => {
                toast::show(store.ui, tr("Nicht gelöscht."), None);
            }
        }
        store.reload(true);
    });
}

fn offer_restore(store: Store, batch: Batch) {
    let undo = EventHandler::new({
        let batch = batch.clone();
        move |_| restore(store, batch.clone())
    });
    let mut session = store.ui.session_undo;
    remember(&mut session.write(), SessionUndo { at: toast::stamp(), key: batch.id.clone(), label: trf("Löschen von „%@“ rückgängig", &[&batch.name]), run: undo.clone() });
    toast::show_kept(store.ui, trf("„%@“ gelöscht.", &[&batch.name]), undo);
}

fn forget_restore(store: Store, id: &str) {
    let mut session = store.ui.session_undo;
    if session.peek().iter().any(|u| u.key == id) {
        session.write().retain(|u| u.key != id);
    }
}

fn restore(store: Store, batch: Batch) {
    forget_restore(store, &batch.id);
    spawn_in_window(async move {
        let back = batch.clone();
        match off_ui(move || remote::restore_batch(&back)).await {
            Some(Ok(())) => {
                store.ui.page.settle(Page::Grid(Filter::Batch(batch.id.clone())));
            }
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht zurückgeholt.", &[&batch.name]), &e).text, None);
                if !crate::view::instance::actions::permanent(&e) {
                    offer_restore(store, batch);
                }
            }
            None => offer_restore(store, batch),
        }
        store.reload(true);
    });
}

fn body_max(store: Store) -> f32 {
    let (_, window_h) = *store.ui.window.read();
    (window_h - CHROME).clamp(BODY_MIN, BODY_MAX)
}

fn list_max(store: Store) -> f32 {
    (body_max(store) * LIST_SHARE).clamp(LIST_MIN, LIST_MAX)
}

fn title_text(t: &Theme, text: String) -> Label {
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

#[derive(Clone)]
struct Seed {
    instance: Option<String>,
    name: String,
    server: String,
    accounts: HashSet<Uuid>,
    /// How many accounts were free when fewer of them fit into memory and were picked.
    capped: Option<usize>,
    memory: String,
    inherit: bool,
    background: Background,
    launch_parallel: u32,
    full_speed: bool,
    unattended: Unattended,
}

impl Seed {
    fn new(store: Store, original: Option<&Batch>, preselect: Option<&String>, picked: Option<&HashSet<Uuid>>, memory_max: u32) -> Self {
        let model = store.model.peek();
        let instance = match (original, preselect) {
            (Some(b), _) => b.instance().map(str::to_owned).or_else(|| recent(&model)),
            (None, Some(id)) => Some(id.clone()).filter(|id| model.instance(id).is_some()).or_else(|| recent(&model)),
            (None, None) => recent(&model),
        };
        let own = instance.as_deref().and_then(|id| model.instance(id)).map_or_else(|| crate::core::settings::active().game.memory_mb, |i| i.memory());
        let (accounts, capped) = match (original, picked) {
            (Some(b), _) => (b.accounts.iter().copied().collect(), None),
            (None, Some(picked)) => (picked.clone(), None),
            (None, None) => free_accounts(&model, own, memory_max),
        };
        let name = original.map_or_else(|| name_for(&model, instance.as_deref(), ""), |b| b.name.clone());
        Self {
            instance,
            name,
            server: original.and_then(|b| b.server.clone()).unwrap_or_default(),
            accounts,
            capped,
            memory: original.and_then(|b| b.memory_override).unwrap_or(own).to_string(),
            inherit: original.is_none_or(|b| b.memory_override.is_none()),
            background: original.map_or_else(Background::default, |b| b.background),
            launch_parallel: original.map_or(0, |b| b.launch_parallel),
            full_speed: original.is_some_and(|b| b.full_speed),
            unattended: original.map_or_else(Unattended::default, Batch::unattended),
        }
    }
}

#[derive(Clone, Copy)]
struct Flow {
    store: Store,
    original: State<Option<Batch>>,
    name: State<String>,
    name_auto: State<Option<String>>,
    server: State<String>,
    filter: State<String>,
    instance: State<Option<String>>,
    versions: State<Load<()>>,
    asked: State<bool>,
    list: State<Vec<String>>,
    version: State<String>,
    inherit: State<bool>,
    memory: State<String>,
    background: State<Background>,
    kept_custom: State<Option<Background>>,
    launch_parallel: State<u32>,
    full_speed: State<bool>,
    unattended: State<Unattended>,
    free: State<Option<u64>>,
    accounts: State<HashSet<Uuid>>,
    cursor: State<Option<Uuid>>,
    saving: State<bool>,
    by_primary: State<bool>,
    save_error: State<Option<Problem>>,
    tried: State<bool>,
    asking: State<bool>,
    details: State<HashSet<String>>,
    total_ram: u64,
    memory_max: u32,
}

#[derive(Clone, Copy)]
enum Dial {
    Fps,
    Sight,
}

impl Flow {
    fn alive(self) -> bool {
        self.name.try_peek().is_some()
    }

    fn editing(self) -> bool {
        self.original.peek().is_some()
    }

    fn connection(self) -> bool {
        self.unattended.peek().mode == RunMode::Connection
    }

    fn played(self) -> Option<String> {
        self.instance.peek().clone().filter(|_| !self.connection())
    }

    fn clear_error(mut self) {
        if self.save_error.peek().is_some() {
            self.save_error.set(None);
        }
    }

    fn instance_memory(self) -> u32 {
        let model = self.store.model.peek();
        self.instance.peek().as_deref().and_then(|id| model.instance(id)).map_or_else(|| crate::core::settings::active().game.memory_mb, |i| i.memory())
    }

    fn each_memory(self) -> u32 {
        if *self.inherit.peek() { self.instance_memory() } else { self.typed_memory().unwrap_or_else(|| self.instance_memory()) }
    }

    fn typed_memory(self) -> Option<u32> {
        crate::core::options::parse_memory(&self.memory.peek()).ok().flatten()
    }

    fn memory_problem(self) -> Option<String> {
        if *self.inherit.peek() {
            return None;
        }
        match crate::core::options::parse_memory(&self.memory.peek()) {
            Err(why) => Some(why),
            Ok(None) => Some(tr("Gib den Arbeitsspeicher als Zahl in MB an, zum Beispiel 4096.")),
            Ok(Some(mb)) => crate::core::options::check_memory_max(mb, None, self.total_ram).err(),
        }
    }

    fn picked(self) -> usize {
        let chosen = self.accounts.peek();
        self.store.model.peek().accounts.iter().filter(|a| chosen.contains(&a.id)).count()
    }

    fn problems(self) -> (Option<String>, Option<String>, Option<String>) {
        let name = forms::batch_name_problem(&self.name.peek());
        let server = forms::server_problem(self.server.peek().trim());
        let version = (!self.connection() && self.instance.peek().is_none() && self.version.peek().is_empty()).then(|| tr("Wähle eine Version."));
        (name, server, version)
    }

    fn follow(mut self) {
        if self.name_auto.peek().is_none() {
            return;
        }
        let next = name_for(&self.store.model.peek(), self.played().as_deref(), &self.server.peek());
        self.name_auto.set(Some(next.clone()));
        if *self.name.peek() != next {
            self.name.set(next);
        }
    }

    fn name_typed(mut self, name: &str) {
        self.clear_error();
        if self.name_auto.peek().as_deref().is_some_and(|auto| auto != name) {
            self.name_auto.set(None);
        }
    }

    fn pick_instance(mut self, id: Option<String>) {
        if *self.saving.peek() {
            return;
        }
        self.instance.set(id.clone());
        self.clear_error();
        self.follow();
        if id.is_none() && !*self.asked.peek() {
            self.load_versions(true);
        }
    }

    fn load_versions(mut self, reset: bool) {
        self.asked.set(true);
        if reset {
            self.versions.set(Load::Loading);
        }
        spawn_in_window(async move {
            let result = off_ui(remote::versions).await;
            if !self.alive() {
                return;
            }
            match result {
                Some(Ok(list)) if !list.is_empty() => {
                    if self.version.peek().is_empty() {
                        self.version.set(list[0].clone());
                    }
                    self.list.set(list);
                    self.versions.set(Load::Ready(()));
                }
                Some(Ok(_)) => self.versions.set(Load::Failed(Problem::plain(tr("Mojang hat keine Versionen geliefert. Versuche es später noch einmal.")))),
                Some(Err(e)) => self.versions.set(Load::Failed(forms::explain_as(tr("Versionen nicht geladen."), &e))),
                None => self.versions.set(Load::Failed(Problem::plain(forms::unexplained()))),
            }
        });
    }

    fn retry_failed(self) {
        if self.instance.peek().is_none() && matches!(*self.versions.peek(), Load::Failed(_)) && self.list.peek().is_empty() {
            self.load_versions(true);
        }
    }

    fn read_memory(mut self) {
        spawn_in_window(async move {
            let free = off_ui(|| crate::core::drive::memory().or_else(crate::core::fit::system_memory).map(|m| m.free_mb)).await.flatten();
            if self.alive() {
                self.free.set(free);
            }
        });
    }

    fn toggle_inherit(mut self) {
        if *self.saving.peek() {
            return;
        }
        let inherit = !*self.inherit.peek();
        self.inherit.set(inherit);
        if !inherit {
            let own = self.instance_memory().to_string();
            self.memory.set(own);
        }
    }

    fn lower_memory(mut self, each: u32) {
        if *self.saving.peek() {
            return;
        }
        self.inherit.set(false);
        self.memory.set(each.to_string());
    }

    fn pick_background(mut self, n: usize) {
        if *self.saving.peek() {
            return;
        }
        let (level, kept) = forms::pick_background(*self.background.peek(), *self.kept_custom.peek(), n);
        self.background.set(level);
        self.kept_custom.set(kept);
        self.clear_error();
    }

    fn step_dial(mut self, dial: Dial, up: bool) {
        if *self.saving.peek() {
            return;
        }
        let Background::Custom { fps, sight } = *self.background.peek() else { return };
        let next = match dial {
            Dial::Fps => Background::Custom { fps: forms::ladder_step(fps, up, &forms::FPS_STEPS), sight },
            Dial::Sight => Background::Custom { fps, sight: forms::ladder_step(sight, up, &forms::SIGHT_STEPS) },
        };
        self.background.set(next);
        self.clear_error();
    }

    fn toggle_full_speed(mut self) {
        if *self.saving.peek() {
            return;
        }
        let on = !*self.full_speed.peek();
        self.full_speed.set(on);
        self.clear_error();
    }

    fn pick_pace(mut self, n: usize) {
        if *self.saving.peek() {
            return;
        }
        self.launch_parallel.set(LAUNCH_CHOICES.get(n).copied().unwrap_or(0));
        self.clear_error();
    }

    fn save(mut self, primary: bool) {
        self.tried.set(true);
        let start = primary && !self.editing() && self.picked() > 0;
        let (name, server, version) = self.problems();
        if *self.saving.peek() || name.is_some() || server.is_some() || version.is_some() || self.memory_problem().is_some() {
            return;
        }
        let chosen = self.accounts.peek().clone();
        let edit = BatchEdit {
            name: self.name.peek().trim().to_owned(),
            instance: match self.played() {
                _ if self.connection() => self.original.peek().as_ref().and_then(|b| b.instance().map(str::to_owned)).map_or(InstancePick::None, InstancePick::Existing),
                Some(id) => InstancePick::Existing(id),
                None => InstancePick::New { game_version: self.version.peek().clone(), mods: Vec::new() },
            },
            server: Some(self.server.peek().trim().to_owned()).filter(|s| !s.is_empty()),
            memory_override: if *self.inherit.peek() { None } else { self.typed_memory() },
            accounts: self.store.model.peek().accounts.iter().map(|a| a.id).filter(|id| chosen.contains(id)).collect(),
            background: *self.background.peek(),
            launch_parallel: *self.launch_parallel.peek(),
            full_speed: *self.full_speed.peek(),
            unattended: *self.unattended.peek(),
        };
        let id = self.original.peek().as_ref().map(|b| b.id.clone());
        let store = self.store;
        self.by_primary.set(primary);
        self.saving.set(true);
        self.save_error.set(None);
        spawn_in_window(async move {
            let result = off_ui(move || {
                let mut edit = edit;
                if let InstancePick::New { game_version, .. } = &edit.instance {
                    let game_version = game_version.clone();
                    let defaults = remote::defaults(&game_version, &Loader::default())?;
                    edit.instance = InstancePick::New { game_version, mods: defaults.mods };
                }
                remote::save_batch(id.as_deref(), edit)
            })
            .await;
            match result {
                Some(Ok(batch)) => {
                    store.reload(true);
                    if self.alive() {
                        self.saving.set(false);
                        sheet::close(store.ui);
                    }
                    if start {
                        store.dispatch(AppEvent::StartBatch(batch.id.clone()));
                        let (mut page, mut show) = (store.ui.page, store.ui.show);
                        page.set(Page::Grid(Filter::Batch(batch.id)));
                        show.set_if_modified(Show::All);
                    }
                }
                other => {
                    if !self.alive() {
                        return;
                    }
                    self.saving.set(false);
                    self.save_error.set(Some(match other {
                        Some(Err(e)) => forms::explain_as(tr("Nicht gespeichert."), &e),
                        _ => Problem::plain(forms::unexplained()),
                    }));
                }
            }
        });
    }
}

#[derive(PartialEq)]
struct BatchSheet {
    original: Option<Batch>,
    preselect: Option<String>,
    picked: Option<HashSet<Uuid>>,
}

impl Component for BatchSheet {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let total_ram = use_hook(forms::total_ram_mb);
        let memory_max = forms::memory_max(total_ram);
        let seed = use_hook(|| Seed::new(store, self.original.as_ref(), self.preselect.as_ref(), self.picked.as_ref(), memory_max));
        let flow = Flow {
            store,
            original: use_state(|| self.original.clone()),
            name: use_state(|| seed.name.clone()),
            name_auto: use_state(|| self.original.is_none().then(|| seed.name.clone())),
            server: use_state(|| seed.server.clone()),
            filter: use_state(String::new),
            instance: use_state(|| seed.instance.clone()),
            versions: use_state(|| Load::Loading),
            asked: use_state(|| false),
            list: use_state(Vec::new),
            version: use_state(String::new),
            inherit: use_state(|| seed.inherit),
            memory: use_state(|| seed.memory),
            background: use_state(|| seed.background),
            kept_custom: use_state(|| None),
            launch_parallel: use_state(|| seed.launch_parallel),
            full_speed: use_state(|| seed.full_speed),
            unattended: use_state(|| seed.unattended),
            free: use_state(|| None),
            accounts: use_state(|| seed.accounts.clone()),
            cursor: use_state(|| None),
            saving: use_state(|| false),
            by_primary: use_state(|| false),
            save_error: use_state(|| None),
            tried: use_state(|| false),
            asking: use_state(|| false),
            details: use_state(HashSet::new),
            total_ram,
            memory_max,
        };
        use_hook(move || flow.read_memory());
        let mode = flow.unattended.read().mode;
        use_side_effect_with_deps(&mode, move |mode| {
            if *mode != RunMode::Connection && flow.instance.peek().is_none() && !*flow.asked.peek() {
                flow.load_versions(false);
            }
            flow.follow();
        });
        let back = *store.back.read();
        use_side_effect_with_deps(&back, move |_| flow.retry_failed());
        let server_text = flow.server.read().clone();
        use_side_effect_with_deps(&server_text, move |_| {
            flow.clear_error();
            flow.follow();
        });
        let name_text = flow.name.read().clone();
        use_side_effect_with_deps(&name_text, move |n| flow.name_typed(n));
        let memory_typed = flow.memory.read().clone();
        use_side_effect_with_deps(&memory_typed, move |_| flow.clear_error());

        let editing = self.original.is_some();
        let saving = *flow.saving.read();
        let by_primary = *flow.by_primary.read();
        let tried = *flow.tried.read();
        let instance = flow.instance.read().clone();
        let versions = flow.versions.read().clone();
        let version = flow.version.read().clone();
        let chosen = flow.accounts.read().clone();
        let memory = flow.memory;
        let memory_text = flow.memory.read().clone();
        let background = *flow.background.read();
        let pace = *flow.launch_parallel.read();
        let inherit = *flow.inherit.read();
        let full_speed = *flow.full_speed.read();
        let connection = mode == RunMode::Connection;

        let (choices, candidates, running, picked, instance_memory, moved, own_game) = {
            let model = store.model.read();
            let running = self.original.as_ref().is_some_and(|b| is_running(&model, &b.id));
            let choices: Vec<(Option<String>, String)> = sorted(&model).into_iter().map(|i| (Some(i.id.clone()), i.name.clone())).chain([(None, tr("Neue Instanz…"))]).collect();
            let elsewhere: HashMap<Uuid, String> = model
                .games
                .values()
                .filter(|g| g.phase.is_active() && self.original.as_ref().is_none_or(|b| b.id != g.batch_id))
                .filter_map(|g| model.batch(&g.batch_id).map(|b| (g.account_id, b.name.clone())))
                .collect();
            let candidates: Vec<Candidate> = model
                .accounts
                .iter()
                .map(|a| Candidate { id: a.id, name: a.name.clone(), kind: if a.kind == AccountKind::Microsoft { tr("Microsoft") } else { tr("Offline") }, color: a.color(), elsewhere: elsewhere.get(&a.id).cloned() })
                .collect();
            let picked = candidates.iter().filter(|c| chosen.contains(&c.id)).count();
            let own = instance.as_deref().and_then(|id| model.instance(id));
            let moved = self.original.as_ref().and_then(|b| b.instance()).filter(|id| connection || instance.as_deref() != Some(*id)).and_then(|id| model.instance(id)).map(|i| trf("Welten und Screenshots bleiben bei %@.", &[&i.name]));
            (choices, candidates, running, picked, own.map_or_else(|| crate::core::settings::active().game.memory_mb, |i| i.memory()), moved, own.and_then(|i| companion_hint(&i.game_version)))
        };

        let (name_problem, server_problem, version_problem) = flow.problems();
        let name_shown = name_problem.clone().filter(|_| tried || !flow.name.read().is_empty());
        let lock = saving.then(saving_text);
        let proxied = store.model.read().accounts.iter().filter(|a| a.proxy.is_some() && chosen.contains(&a.id)).count();
        let game = instance.as_deref().and_then(|id| store.model.read().instance(id).map(|i| i.game_version.clone())).unwrap_or_else(|| version.clone());

        if *flow.asking.read() && editing {
            if let Some(batch) = &self.original {
                return confirm_card(flow, batch.clone(), running);
            }
        }

        let instance_lock = if running { Some(tr("Stoppe zuerst die laufenden Spiele.")) } else { lock.clone() };
        let mut rows = vec![unattended::mode_line(&t, flow.unattended, &game, &instance_lock, proxied).into_element()];
        if !connection {
            let at = choices.iter().position(|(id, _)| *id == instance).unwrap_or(0);
            let ids: Vec<Option<String>> = choices.iter().map(|(id, _)| id.clone()).collect();
            let labels: Vec<String> = choices.into_iter().map(|(_, l)| l).collect();
            let picker = Select::new("batch.instance", tr("Instanz"), labels, at).width(frame::PICK_W).disabled(instance_lock.clone()).on_choose(move |i| {
                if let Some(id) = ids.get(i) {
                    flow.pick_instance(id.clone());
                }
            });
            let mut instance_row = Line::new("batch.instance.row", tr("Instanz")).control(picker);
            if let Some(note) = own_game {
                instance_row = instance_row.hint(note);
            }
            rows.push(instance_row.into_element());
            if instance.is_none() {
                rows.push(version_row(&t, flow, &versions, &version, tried.then_some(version_problem.clone()).flatten(), &lock).into_element());
            }
        }
        let mut server_field = Field::new("batch.server", flow.server).placeholder(tr("z. B. play.example.net")).error(server_problem.clone()).disabled(lock.clone());
        let mut name_field = Field::new("batch.name", flow.name).placeholder(tr("z. B. Farm")).error(name_shown).disabled(lock.clone());
        if editing {
            name_field = name_field.auto_focus();
        } else {
            server_field = server_field.auto_focus();
        }
        rows.push(Line::new("batch.server.row", tr("Server")).hint(tr("Optional. Adresse mit oder ohne Port.")).below(server_field).into_element());
        rows.push(Line::new("batch.name.row", tr("Name")).below(name_field).into_element());
        let footer = connection.then(|| {
            let speaks = trf("Headless spricht Minecraft %@ und braucht keine Instanz.", &[&crate::core::batch::HEADLESS_VERSION]);
            match &moved {
                Some(kept) => format!("{speaks} {kept}"),
                None => speaks,
            }
        });
        let mut form = rect().width(Size::fill()).padding(frame::RING).spacing(space::L);
        if running {
            form = form.child(notice(&t, Icon::Info, t.text2, tr("Server und Accounts gelten beim nächsten Start. Die Instanz wechselt nur ein gestoppter Batch.")));
        }
        form = form.child(group(&t, None, rows, footer));
        form = form.child(accounts_section(&t, flow, candidates, picked, &lock, !connection));

        let preset = seed.capped.filter(|_| !editing && chosen == seed.accounts).map(|free| preset_line(&t, flow, picked, free, &lock));
        form = form.children(preset).children(fit_line(&t, flow, picked));

        if !connection {
            let mut tuning = memory_rows(flow, t.danger_text, MemoryView { inherit, memory, typed: flow.typed_memory(), problem: flow.memory_problem().filter(|_| !memory_text.trim().is_empty() || tried), instance_memory, picked }, &lock);
            tuning.push(speed_row(flow, full_speed, running, &lock).into_element());
            tuning.push(background_row(&t, flow, background, running, &lock).into_element());
            form = form.child(group(&t, Some(tr("Leistung")), tuning, None));
        }
        let u = *flow.unattended.read();
        let mut flowing = vec![pace_row(flow, pace, running, &lock).into_element()];
        if u.mode == RunMode::Hidden {
            flowing.push(unattended::screenshots_line(flow.unattended, &lock).into_element());
        }
        flowing.push(unattended::packs_line(flow.unattended, running, &lock).into_element());
        if u.mode == RunMode::Connection {
            flowing.push(unattended::respawn_line(flow.unattended, &lock).into_element());
        }
        form = form.child(group(&t, Some(tr("Ablauf")), flowing, None));

        let mut body = rect().width(Size::fill()).spacing(space::M).child(ScrollView::new().width(Size::fill()).height(Size::auto()).max_height(Size::px(body_max(store))).auto_hide_scrollbars(false).child(form));
        if let Some(problem) = flow.save_error.read().clone() {
            body = body.child(problem_view(&t, "batch.save", Icon::CircleAlert, &problem, flow.details));
        }

        let starts = !editing && picked > 0;
        let invalid = name_problem.or(server_problem).or_else(|| {
            version_problem.map(|p| match &versions {
                Load::Loading => tr("Versionen werden geladen …"),
                Load::Failed(_) => tr("Versionen nicht geladen."),
                Load::Ready(()) => p,
            })
        });
        let (title, subtitle) = if editing {
            (tr("Batch bearbeiten"), None)
        } else {
            (tr("Batch erstellen"), Some(tr("Ein Batch schickt mehrere Accounts gemeinsam los.")))
        };
        let action = if editing { tr("Sichern") } else if starts { tr("Erstellen und starten") } else { tr("Erstellen") };
        let primary = Primary { label: action, disabled: invalid.clone(), danger: false, busy: saving && by_primary, run: EventHandler::new(move |_| flow.save(true)) };
        let mut card = Card::new("batch", title).width(WIDE).body(body).primary(primary).primary_id("batch.save").locked(lock.clone()).on_enter(move |_| {
            if typing() {
                flow.save(true);
            }
        });
        if let Some(s) = subtitle {
            card = card.message(s);
        }
        if editing {
            let why = if running { Some(blocked_text()) } else { lock.clone() };
            let mut asking = flow.asking;
            card = card.leading(Button::new("batch.delete", tr("Batch löschen…")).danger().disabled(why).on_press(move |_| asking.set(true)));
        } else if starts {
            card = card
                .secondary(Primary { label: tr("Nur erstellen"), disabled: invalid, danger: false, busy: saving && !by_primary, run: EventHandler::new(move |_| flow.save(false)) })
                .secondary_id("batch.create-only")
                .keep_cancel();
        }
        card.into_element()
    }
}

fn confirm_card(flow: Flow, batch: Batch, running: bool) -> Element {
    let store = flow.store;
    let mut asking = flow.asking;
    let mut keep = flow.asking;
    let doomed = batch.clone();
    let primary = Primary {
        label: tr("Löschen"),
        disabled: running.then(blocked_text),
        danger: true,
        busy: false,
        run: EventHandler::new(move |_| {
            sheet::close(store.ui);
            delete(store, doomed.clone());
        }),
    };
    let secondary = Primary { label: tr("Behalten"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| keep.set(false)) };
    Card::new("batch", ask_title(&batch))
        .width(frame::SHEET_NARROW)
        .message(ask_message())
        .primary(primary)
        .primary_id("batch.delete.confirm")
        .secondary(secondary)
        .secondary_id("batch.delete.keep")
        .on_escape(move |_| asking.set(false))
        .into_element()
}

fn preset_line(t: &Theme, mut flow: Flow, picked: usize, free: usize, lock: &Option<String>) -> Element {
    let text = trf("%1$lld von %2$lld vorgewählt, mehr passen nicht in den Arbeitsspeicher.", &[&picked, &free]);
    let all = Button::new("batch.preset.all", trf("Alle %lld wählen", &[&free])).plain().disabled(lock.clone()).on_press(move |_| {
        let m = flow.store.model.peek();
        let ids: HashSet<Uuid> = m.accounts.iter().map(|a| a.id).filter(|id| !m.batches.iter().any(|b| b.accounts.contains(id))).collect();
        drop(m);
        flow.accounts.set(ids);
    });
    rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .content(Content::flex())
        .child(rect().width(Size::flex(1.)).child(notice(t, Icon::Info, t.text2, text)))
        .child(all)
        .into_element()
}

fn fit_line(t: &Theme, flow: Flow, count: usize) -> Option<Element> {
    if flow.unattended.read().mode == crate::core::batch::RunMode::Connection {
        return None;
    }
    let (total, budget) = forms::memory_need(count, flow.each_memory(), flow.total_ram)?;
    let lower = forms::memory_fit(count, flow.memory_max);
    let need = trf("%lld Accounts brauchen zusammen bis zu %@. Dieser Rechner hat neben dem System nur %@.", &[&count, &forms::memory_label(total), &forms::memory_label(budget)]);
    let text = if lower.is_some() { need } else { format!("{need} {}", tr("Wähle weniger Accounts.")) };
    let mut column = rect().width(Size::fill()).spacing(space::S).child(notice(t, Icon::CircleAlert, t.attention_text, text));
    if let Some(each) = lower {
        let reason = flow.saving.read().then(saving_text);
        column = column.child(rect().horizontal().child(Button::new("batch.fit.fix", trf("Auf %@ pro Account senken", &[&forms::memory_label(each)])).disabled(reason).on_press(move |_| flow.lower_memory(each))));
    }
    Some(column.into_element())
}

fn version_row(t: &Theme, flow: Flow, versions: &Load<()>, version: &str, problem: Option<String>, lock: &Option<String>) -> Line {
    let row = Line::new("batch.version.row", tr("Minecraft-Version"));
    match versions {
        Load::Failed(why) if flow.list.read().is_empty() => row
            .hint(why.text.clone())
            .hint_tone(t.danger_text)
            .control(Button::new("batch.versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| flow.load_versions(true))),
        Load::Loading if flow.list.read().is_empty() => row.control(spinner::waiting(t, tr("Versionen werden geladen …"))),
        _ => {
            let list = flow.list.read().clone();
            let at = list.iter().position(|v| v == version).unwrap_or(0);
            let pick = list.clone();
            let mut chosen_version = flow.version;
            let select = Select::new("batch.version", tr("Minecraft-Version"), list, at).width(frame::PICK_W).disabled(lock.clone()).on_choose(move |i| {
                if let Some(v) = pick.get(i) {
                    chosen_version.set(v.clone());
                    flow.clear_error();
                }
            });
            let row = row.control(select);
            match (problem, companion_hint(version)) {
                (Some(p), _) => row.hint(p).hint_tone(t.danger_text),
                (None, Some(h)) => row.hint(h),
                (None, None) => row,
            }
        }
    }
}

struct MemoryView {
    inherit: bool,
    memory: State<String>,
    typed: Option<u32>,
    problem: Option<String>,
    instance_memory: u32,
    picked: usize,
}

fn memory_rows(flow: Flow, danger: Color, view: MemoryView, lock: &Option<String>) -> Vec<Element> {
    let MemoryView { inherit, memory, typed, problem, instance_memory, picked } = view;
    let like = trf("Wie die Instanz (%@)", &[&forms::memory_label(instance_memory)]);
    let label = tr("Eigener Arbeitsspeicher");
    let mut own = Line::new("batch.memory.own.row", label.clone())
        .hint(if inherit { like } else { tr("Pro Account, statt dem Wert der Instanz.") })
        .control(Switch::new("batch.memory.own", label, !inherit).disabled(lock.is_some()).on_toggle(move |_| flow.toggle_inherit()));
    if lock.is_none() {
        own = own.on_press(move |_| flow.toggle_inherit());
    }
    let mut rows = vec![own.into_element()];
    if !inherit {
        let count = picked.max(1);
        let total = u64::from(typed.unwrap_or(instance_memory)) * count as u64;
        let note = if count > 1 { trf("Pro Account. %lld Accounts brauchen zusammen bis zu %@.", &[&count, &forms::memory_label(u32::try_from(total).unwrap_or(u32::MAX))]) } else { tr("Pro Account.") };
        let ceiling = u32::try_from(flow.total_ram).unwrap_or(u32::MAX).max(MEMORY_FLOOR_MB);
        let field = Memory::new("batch.memory", tr("Arbeitsspeicher"), memory)
            .placeholder(instance_memory.to_string())
            .steps(Steps { floor: MEMORY_FLOOR_MB, ceiling, from: instance_memory })
            .disabled(lock.clone());
        let mut row = Line::new("batch.memory.row", tr("Arbeitsspeicher")).control(field);
        row = match problem {
            Some(why) => row.hint(why).hint_tone(danger),
            None => row.hint(note),
        };
        rows.push(row.into_element());
    }
    rows
}

fn background_row(t: &Theme, flow: Flow, background: Background, running: bool, lock: &Option<String>) -> Line {
    let name = |level: Background| match level {
        Background::Saving => tr("Sparsam"),
        Background::Balanced => tr("Ausgewogen"),
        Background::Full => tr("Volle Leistung"),
        Background::Custom { .. } => tr("Eigene"),
    };
    let cost = match background {
        Background::Saving => tr("Kostet kaum etwas: Das Spiel zeigt nur wenige Bilder und sieht nicht weit."),
        Background::Balanced => tr("Kostet spürbar mehr: Das Spiel läuft flüssig weiter und sieht so weit wie sonst."),
        Background::Full => tr("Kostet so viel wie ein offenes Spiel."),
        Background::Custom { fps, sight } => {
            let line = match forms::custom_cost(fps) {
                forms::Cost::Little => tr("Kostet wenig: Das Spiel zeichnet im Hintergrund nur für die Vorschau."),
                forms::Cost::Noticeable => tr("Kostet spürbar: Das Spiel zeichnet im Hintergrund weiter."),
                forms::Cost::Much => tr("Kostet viel, fast so viel wie ein offenes Spiel."),
            };
            if sight >= forms::FAR_SIGHT { format!("{line} {}", tr("Die große Sichtweite kostet zusätzlich.")) } else { line }
        }
    };
    let levels: Vec<Background> = Background::PRESETS.into_iter().chain([background.to_custom()]).collect();
    let chosen = levels.iter().position(|l| std::mem::discriminant(l) == std::mem::discriminant(&background)).unwrap_or(0);
    let segments = levels.iter().zip(BACKGROUND_NAMES).map(|(level, id)| Segment { id: format!("batch.background.{id}"), label: name(*level), tip: None }).collect();
    let control = Segmented::new(tr("Im Hintergrund"), segments, chosen).disabled(lock.clone()).on_choose(move |n| flow.pick_background(n));
    let text = if running { format!("{cost} {}", tr("Gilt sofort, auch für die laufenden Spiele.")) } else { cost };
    let mut row = Line::new("batch.background.row", tr("Im Hintergrund")).hint(text).control(control);
    if let Background::Custom { fps, sight } = background {
        row = row.below(rect().horizontal().spacing(space::XL).child(dial(t, flow, Dial::Fps, fps, lock)).child(dial(t, flow, Dial::Sight, sight, lock)));
    }
    row
}

fn dial(t: &Theme, flow: Flow, which: Dial, value: u8, lock: &Option<String>) -> Element {
    let (id, title, less, more, steps): (&str, String, String, String, &[u8]) = match which {
        Dial::Fps => ("fps", tr("Bilder pro Sekunde"), tr("Weniger Bilder pro Sekunde"), tr("Mehr Bilder pro Sekunde"), &forms::FPS_STEPS),
        Dial::Sight => ("sight", tr("Sichtweite"), tr("Weniger Sichtweite"), tr("Mehr Sichtweite"), &forms::SIGHT_STEPS),
    };
    let shown = |v: u8| match which {
        Dial::Fps => v.to_string(),
        Dial::Sight => trf("%lld Chunks", &[&v]),
    };
    let reason = |up: bool| {
        if lock.is_some() {
            return lock.clone();
        }
        if forms::ladder_step(value, up, steps) != value {
            return None;
        }
        let end = shown((if up { steps.last() } else { steps.first() }).copied().unwrap_or(value));
        Some(if up { trf("Mehr geht nicht: höchstens %@.", &[&end]) } else { trf("Weniger geht nicht: mindestens %@.", &[&end]) })
    };
    let stepper = rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::XS)
        .child(Button::icon(format!("batch.background.{id}.less"), Icon::Minus, less).disabled(reason(false)).on_press(move |_| flow.step_dial(which, false)))
        .child(rect().width(Size::px(control::STEPPER_VALUE_WIDE)).center().child(figure(t, shown(value), font::BODY, t.text1).font_weight(FontWeight::MEDIUM)))
        .child(Button::icon(format!("batch.background.{id}.more"), Icon::Plus, more).disabled(reason(true)).on_press(move |_| flow.step_dial(which, true)));
    rect().spacing(space::XS).child(line(t, title, font::CALLOUT, t.text2).font_weight(FontWeight::MEDIUM)).child(stepper).into_element()
}

fn speed_row(flow: Flow, on: bool, running: bool, lock: &Option<String>) -> Line {
    let label = tr("Schneller beim Spielen");
    let text = tr("Spiele im Hintergrund brauchen dann mehr Arbeitsspeicher.");
    let text = if running { format!("{text} {}", tr("Gilt beim nächsten Start.")) } else { text };
    let mut row = Line::new("batch.speed.row", label.clone()).hint(text).control(Switch::new("batch.speed", label, on).disabled(lock.is_some()).on_toggle(move |_| flow.toggle_full_speed()));
    if lock.is_none() {
        row = row.on_press(move |_| flow.toggle_full_speed());
    }
    row
}

fn pace_row(flow: Flow, pace: u32, running: bool, lock: &Option<String>) -> Line {
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let auto = crate::core::stagger::auto_parallel(cores, *flow.free.read());
    let text = match pace {
        0 => trf("Automatisch: höchstens %lld Spiele gleichzeitig, passend zu Prozessor und freiem Arbeitsspeicher. Die übrigen warten.", &[&auto]),
        1 => tr("Ein Spiel startet nach dem anderen. Das schont den Rechner am meisten und dauert am längsten."),
        n => trf("Höchstens %lld Spiele starten gleichzeitig. Mehr geht schneller, der Rechner reagiert dann aber träger. Die übrigen warten.", &[&n]),
    };
    let text = if running { format!("{text} {}", tr("Gilt beim nächsten Start.")) } else { text };
    let segments = LAUNCH_CHOICES
        .iter()
        .map(|n| Segment {
            id: format!("batch.parallel.{n}"),
            label: if *n == 0 { tr("Automatisch") } else { n.to_string() },
            tip: match n {
                0 => None,
                1 => Some(tr("Ein Spiel nach dem anderen")),
                n => Some(trf("%lld Spiele gleichzeitig", &[n])),
            },
        })
        .collect();
    let chosen = LAUNCH_CHOICES.iter().position(|n| *n == pace).unwrap_or(0);
    let control = Segmented::new(tr("Gleichzeitig starten"), segments, chosen).disabled(lock.clone()).on_choose(move |n| flow.pick_pace(n));
    Line::new("batch.parallel.row", tr("Gleichzeitig starten")).hint(text).control(control)
}

#[derive(Clone, PartialEq)]
struct Candidate {
    id: Uuid,
    name: String,
    kind: String,
    color: u8,
    elsewhere: Option<String>,
}

fn accounts_section(t: &Theme, flow: Flow, candidates: Vec<Candidate>, picked: usize, lock: &Option<String>, shared: bool) -> Element {
    let store = flow.store;
    let total = candidates.len();
    let raw = flow.filter.read().trim().to_lowercase();
    let filtered = total > FILTER_FROM && !raw.is_empty();
    let shown: Vec<Candidate> = candidates.into_iter().filter(|c| !filtered || c.name.to_lowercase().contains(&raw)).collect();
    let header = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).content(Content::flex()).child(rect().width(Size::flex(1.)).child(title_text(t, trf("Accounts (%lld von %lld)", &[&picked, &total]))));
    if total == 0 {
        let add = Button::new("batch.accounts.add", tr("Account hinzufügen")).glyph(Icon::Plus).plain().on_press(move |_| {
            sheet::close(store.ui);
            want(store, Want::AddAccount);
        });
        return rect()
            .width(Size::fill())
            .spacing(space::XS)
            .child(header)
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .content(Content::flex())
                    .child(para(t, tr("Noch keine Accounts."), font::CALLOUT, t.text2).width(Size::flex(1.)))
                    .child(add),
            )
            .into_element();
    }
    let chosen = flow.accounts.read().clone();
    let all = !shown.is_empty() && shown.iter().all(|c| chosen.contains(&c.id));
    let (pick, unpick) = if filtered { (tr("Alle gefilterten auswählen"), tr("Gefilterte abwählen")) } else { (tr("Alle auswählen"), tr("Keine auswählen")) };
    let ids: Vec<Uuid> = shown.iter().map(|c| c.id).collect();
    let none = ids.is_empty().then(|| trf("Kein Account passt zu „%@“.", &[&raw]));
    let mut accounts = flow.accounts;
    let toggle_all = Button::new("batch.accounts.all", if all { unpick } else { pick }).plain().disabled(none.clone().or_else(|| lock.clone())).on_press(move |_| {
        let mut set = accounts.peek().clone();
        if all {
            set.retain(|id| !ids.contains(id));
        } else {
            set.extend(ids.iter().copied());
        }
        accounts.set(set);
    });
    let mut column = rect().width(Size::fill()).spacing(space::XS).child(header.child(toggle_all));
    if total > FILTER_FROM {
        column = column.child(Field::new("batch.accounts.filter", flow.filter).placeholder(tr("Accounts filtern")).glyph(Icon::Search));
    }
    let list: Element = if shown.is_empty() {
        rect()
            .width(Size::fill())
            .padding((space::S, space::M))
            .corner_radius(t.shape.control)
            .border(Border::new().fill(t.separator).width(frame::HAIRLINE).alignment(BorderAlignment::Inner))
            .child(para(t, trf("Kein Account passt zu „%@“.", &[&raw]), font::CALLOUT, t.text2))
            .into_element()
    } else {
        let height = (shown.len() as f32 * PICK_ROW + BOX_EDGES).min(list_max(store));
        WithTip::new(lock.clone(), PickList { rows: shown, picked: flow.accounts, cursor: flow.cursor, height, locked: lock.clone() }).fill().into_element()
    };
    column.child(list).children(shared.then(|| para(t, tr("Ändern sich Einstellungen, Ressourcenpakete oder Serverliste in einem Spiel, ziehen alle Spiele dieser Instanz mit, auch in anderen Batches."), font::CAPTION, t.text3))).into_element()
}

#[derive(Clone, Copy)]
enum Step {
    Next,
    Previous,
    First,
    Last,
}

fn moved(at: Option<usize>, count: usize, step: Step) -> usize {
    let last = count.saturating_sub(1);
    match (step, at) {
        (Step::First, _) | (Step::Next | Step::Previous, None) => 0,
        (Step::Last, _) => last,
        (Step::Next, Some(a)) => (a + 1).min(last),
        (Step::Previous, Some(a)) => a.saturating_sub(1),
    }
}

#[derive(PartialEq)]
struct PickList {
    rows: Vec<Candidate>,
    picked: State<HashSet<Uuid>>,
    cursor: State<Option<Uuid>>,
    height: f32,
    locked: Option<String>,
}

impl Component for PickList {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        use_arrows(a11y);
        let focus = use_focus(a11y);
        let mut keyed = use_state(|| false);
        let keyboard = focus() == Focus::Keyboard || focus() != Focus::Not && *keyed.read();
        let scroll = use_scroll_controller(ScrollConfig::default);
        let mut viewport = use_state(|| 0f32);
        let (picked, cursor, locked) = (self.picked, self.cursor, self.locked.is_some());
        let at = cursor.read().and_then(|c| self.rows.iter().position(|r| r.id == c));
        use_side_effect_with_deps(&at, move |at| {
            let Some(at) = *at else { return };
            let mut scroll = scroll;
            let top = at as f32 * PICK_ROW;
            let bottom = top + PICK_ROW;
            let shown = -(scroll.peek_position().y);
            let h = *viewport.peek();
            if top < shown {
                scroll.scroll_to_y(-(top as i32));
            } else if h > 0. && bottom > shown + h {
                scroll.scroll_to_y(-((bottom - h) as i32));
            }
        });
        let ids: Vec<Uuid> = self.rows.iter().map(|r| r.id).collect();
        let n = ids.len();
        let toggle = move |id: Uuid| {
            if locked {
                return;
            }
            let mut set = picked.peek().clone();
            if !set.remove(&id) {
                set.insert(id);
            }
            let mut picked = picked;
            picked.set(set);
        };
        let click = toggle;
        let items = VirtualScrollView::new_with_data_controlled(
            self.rows.clone(),
            move |item: VirtualItem, rows: &Vec<Candidate>| {
                let c = &rows[item.index];
                let id = c.id;
                let press = EventHandler::new(move |_| {
                    keyed.set_if_modified(false);
                    a11y.request_focus();
                    let mut cursor = cursor;
                    cursor.set(Some(id));
                    click(id);
                });
                rect()
                    .key(id)
                    .width(Size::fill())
                    .height(Size::px(item.size))
                    .child(PickRow { index: item.index, candidate: c.clone(), on: picked.read().contains(&id), current: keyboard && cursor.read().is_some_and(|x| x == id), locked, on_press: press })
                    .into_element()
            },
            scroll,
        )
        .length(n)
        .item_size(PICK_ROW)
        .scroll_with_arrows(false)
        .width(Size::fill())
        .height(Size::fill());
        let keys = toggle;
        let mut list = rect()
            .a11y_id(a11y)
            .a11y_focusable(!locked)
            .a11y_role(AccessibilityRole::ListBox)
            .a11y_alt(tr("Accounts"))
            .width(Size::fill())
            .height(Size::px(self.height))
            .corner_radius(t.shape.control)
            .overflow(Overflow::Clip)
            .on_sized(move |e: Event<SizedEventData>| viewport.set_if_modified(e.area.height()))
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !a11y.is_focused() || n == 0 {
                    return;
                }
                keyed.set_if_modified(true);
                let at = cursor.peek().and_then(|c| ids.iter().position(|i| *i == c));
                let go = |to: usize| {
                    let mut cursor = cursor;
                    cursor.set(ids.get(to).copied());
                };
                match &e.key {
                    Key::Named(NamedKey::ArrowDown) => go(moved(at, n, Step::Next)),
                    Key::Named(NamedKey::ArrowUp) => go(moved(at, n, Step::Previous)),
                    Key::Named(NamedKey::Home) => go(moved(at, n, Step::First)),
                    Key::Named(NamedKey::End) => go(moved(at, n, Step::Last)),
                    Key::Named(NamedKey::Enter) => {
                        e.prevent_default();
                        let to = at.unwrap_or(0);
                        go(to);
                        if let Some(id) = ids.get(to) {
                            keys(*id);
                        }
                    }
                    Key::Character(c) if c == " " => {
                        let to = at.unwrap_or(0);
                        go(to);
                        if let Some(id) = ids.get(to) {
                            keys(*id);
                        }
                    }
                    _ => return,
                }
                e.stop_propagation();
            })
            .child(items);
        list = match ring(&t, if keyboard { Focus::Keyboard } else { Focus::Not }) {
            Some(border) => list.border(border),
            None => list.border(Border::new().fill(t.separator).width(frame::HAIRLINE).alignment(BorderAlignment::Inner)),
        };
        list
    }
}

#[derive(PartialEq)]
struct PickRow {
    index: usize,
    candidate: Candidate,
    on: bool,
    current: bool,
    locked: bool,
    on_press: EventHandler<()>,
}

impl Component for PickRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let c = &self.candidate;
        let spot = use_spot(&format!("batch.accounts.{}", self.index), "row", &c.name, !self.locked);
        let press = self.on_press.clone();
        let fill = if self.locked {
            Color::TRANSPARENT
        } else if *pressed.read() {
            t.press
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let side = match &c.elsewhere {
            Some(batch) => format!("{} · {}", c.kind, trf("Läuft in „%@“", &[batch])),
            None => c.kind.clone(),
        };
        let mark = check::mark(&t, self.on);
        let mut row = rect()
            .a11y_alt(format!("{}, {}", c.name, if self.on { tr("ausgewählt") } else { tr("nicht ausgewählt") }))
            .width(Size::fill())
            .height(Size::fill())
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .background(fill)
            .cursor(if self.locked { CursorIcon::NotAllowed } else { CursorIcon::Pointer })
            .content(Content::flex())
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |_| pressed.set(true))
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_press(move |_| press.call(()))
            .child(mark)
            .child(face(&t, store, c))
            .child(line(&t, c.name.clone(), font::BODY, if self.locked { t.text3 } else { t.text1 }).width(Size::flex(1.)))
            .child(caption(&t, side));
        if self.current {
            row = row.border(Border::new().fill(t.focus).width(frame::RING).alignment(BorderAlignment::Inner));
        }
        row
    }
}

fn face(t: &Theme, store: Store, c: &Candidate) -> Element {
    let fill = t.identity(usize::from(c.color));
    let chip = rect().width(Size::px(FACE)).height(Size::px(FACE)).corner_radius(t.shape.small).background(fill).center().overflow(Overflow::Clip);
    match store.faces.read().get(&c.id).cloned() {
        Some(handle) => chip.padding(FACE_INSET).child(image(handle).width(Size::fill()).height(Size::fill()).corner_radius(t.shape.small / 2.)).into_element(),
        None => chip.child(label().text(c.name.chars().next().map(|ch| ch.to_uppercase().collect::<String>()).unwrap_or_default()).font_size(font::CAPTION).font_weight(FontWeight::BOLD).color(t.ink_on(fill))).into_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Step, moved};

    #[test]
    fn the_cursor_starts_on_the_first_row_and_stays_inside_the_list() {
        assert_eq!(moved(None, 5, Step::Next), 0);
        assert_eq!(moved(None, 5, Step::Previous), 0);
        assert_eq!(moved(Some(4), 5, Step::Next), 4);
        assert_eq!(moved(Some(0), 5, Step::Previous), 0);
        assert_eq!(moved(Some(2), 5, Step::Next), 3);
        assert_eq!(moved(Some(2), 5, Step::Previous), 1);
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        assert_eq!(moved(Some(2), 5, Step::First), 0);
        assert_eq!(moved(Some(2), 5, Step::Last), 4);
        assert_eq!(moved(None, 0, Step::Last), 0);
    }
}
