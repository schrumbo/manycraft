use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use freya::prelude::*;
use uuid::Uuid;

use super::save::{self, Change, Job, OptionChange, Switch};
use super::memory::{Memory, Steps};
use super::{Facts, snapshots, version_text};
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::core::companion;
use crate::core::install::Versions;
use crate::core::instance::{Instance, Loader};
use crate::core::javas::{Java, mismatch};
use crate::core::options::{self, Gpu, LaunchOptions, Renderer};
use crate::core::snapshot::Reason;
use crate::core::settings;
use crate::core::vault;
use crate::core::versions::{GameVersion, shown};
use crate::ui::forms;
use crate::ui::remote;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::group::{Line, group};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::sheet;
use crate::view::kit::switch::Switch as Toggle;
use crate::view::kit::text::{caption, line, strong};
use crate::view::kit::{after, off_ui, use_theme};
use crate::view::native;
use crate::view::settings::{gpu_label, remember_all_versions};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, iconsize, space};

const TYPING: Duration = Duration::from_millis(800);
const ICON: f32 = 64.;
const MOD_LOADERS: [Loader; 4] = [Loader::Fabric(None), Loader::Quilt(None), Loader::NeoForge(None), Loader::Forge(None)];

#[derive(Clone)]
enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

fn split_args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn java_title(java: &Java) -> String {
    match &java.vendor {
        Some(vendor) => trf("Java %@ · %@", &[&java.major, vendor]),
        None => trf("Java %@", &[&java.major]),
    }
}

fn label_row(t: &Theme, label: String, reset: Option<Element>) -> Element {
    let mut head = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).height(Size::px(control::S)).child(strong(t, label, font::CALLOUT, t.text2));
    if let Some(reset) = reset {
        head = head.child(reset);
    }
    head.into_element()
}

fn note(t: &Theme, hint: Option<String>, problem: Option<String>) -> Option<Element> {
    match (problem, hint) {
        (Some(p), _) => Some(rect().horizontal().spacing(space::XS).child(icon(t, Icon::Alert, iconsize::XS, t.danger_text)).child(line(t, p, font::CAPTION, t.danger_text)).into_element()),
        (None, Some(h)) => Some(caption(t, h).into_element()),
        (None, None) => None,
    }
}

fn setting(t: &Theme, label: String, reset: Option<Element>, control: Element, hint: Option<String>, problem: Option<String>) -> Element {
    let mut col = rect().width(Size::fill()).spacing(space::XS).child(label_row(t, label, reset)).child(control);
    if let Some(n) = note(t, hint, problem) {
        col = col.child(n);
    }
    col.into_element()
}

/// A row of a settings group: `control` on the right with its "Standard" reset before it, and a hint or a problem
/// under the label.
fn row(t: &Theme, key: &str, label: String, reset: Option<Element>, control: impl IntoElement, hint: Option<String>, problem: Option<String>) -> Line {
    let mut right = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
    if let Some(r) = reset {
        right = right.child(r);
    }
    let mut line = Line::new(format!("instance.settings.{key}.row"), label).control(right.child(control));
    match (problem, hint) {
        (Some(p), _) => line = line.hint(p).hint_tone(t.danger_text),
        (None, Some(h)) => line = line.hint(h),
        (None, None) => {}
    }
    line
}

/// A row as [`row`] draws it, with a wide `field` across the row under the label.
fn wide(t: &Theme, key: &str, label: String, reset: Option<Element>, field: Element, hint: Option<String>, problem: Option<String>) -> Element {
    let mut below = rect().width(Size::fill()).spacing(space::XS).child(field);
    if let Some(n) = note(t, hint, problem) {
        below = below.child(n);
    }
    let mut line = Line::new(format!("instance.settings.{key}.row"), label).below(below);
    if let Some(r) = reset {
        line = line.control(r);
    }
    line.into_element()
}

fn reset(key: &str, label: String, run: impl FnMut(()) + 'static) -> Element {
    WithLabel { id: format!("instance.settings.{key}.reset"), label, run: EventHandler::new(run) }.into_element()
}

#[derive(PartialEq)]
struct WithLabel {
    id: String,
    label: String,
    run: EventHandler<()>,
}

impl Component for WithLabel {
    fn render(&self) -> impl IntoElement {
        let run = self.run.clone();
        Button::new(self.id.clone(), tr("Standard")).plain().tip(trf("%@ auf Standard setzen", &[&self.label])).on_press(move |_| run.call(()))
    }
}

type Draft = Rc<RefCell<String>>;

fn use_typed(value: State<String>, saved: String, id: String, problem: State<Option<String>>, draft: Draft, commit: impl Fn(&str, &str) -> Option<String> + 'static) -> EventHandler<String> {
    let commit = use_hook(|| Rc::new(commit) as Rc<dyn Fn(&str, &str) -> Option<String>>);
    let pending = use_hook(|| Rc::new(RefCell::new(None::<(String, String)>)));
    let mut seen = use_state(|| None::<(String, String)>);
    let mut typed = use_state(|| 0u64);
    let key = (id, saved);
    {
        let (commit, pending) = (commit.clone(), pending.clone());
        use_side_effect_with_deps(&key, move |key| {
            let mut value = value;
            if seen.peek().as_ref() != Some(key) {
                let left = pending.borrow_mut().take_if(|(was, _)| *was != key.0);
                if let Some((was, text)) = left {
                    commit(&was, &text);
                }
                seen.set(Some(key.clone()));
                if *value.peek() != key.1 {
                    value.set(key.1.clone());
                }
            }
        });
    }
    {
        let (commit, pending) = (commit.clone(), pending.clone());
        use_drop(move || {
            let left = pending.borrow_mut().take();
            if let Some((was, text)) = left {
                commit(&was, &text);
            }
        });
    }
    let taken = pending.clone();
    let live = move |id: &str, text: &str| {
        taken.borrow_mut().take();
        let mut problem = problem;
        problem.set_if_modified(commit(id, text));
    };
    let later = live.clone();
    use_side_effect(move || {
        let text = value.read().clone();
        draft.borrow_mut().clone_from(&text);
        let Some((id, _)) = seen.peek().clone() else { return };
        *pending.borrow_mut() = Some((id.clone(), text.clone()));
        let n = typed.peek().wrapping_add(1);
        typed.set(n);
        let later = later.clone();
        spawn(async move {
            after(TYPING).await;
            if *typed.peek() == n {
                later(&id, &text);
            }
        });
    });
    EventHandler::new(move |text: String| {
        if let Some((id, _)) = seen.peek().clone() {
            live(&id, &text);
        }
    })
}

fn save_name(store: Store, id: &str, text: &str) -> Option<String> {
    let name = text.trim();
    if name.is_empty() {
        return Some(tr("Der Name darf nicht leer sein."));
    }
    if store.model.peek().instance(id).is_some_and(|m| m.name != name) {
        save::enqueue(store, id, Job::Save(Change::Name(name.to_owned())));
    }
    None
}

fn choose_icon(store: Store, id: String) {
    spawn_in_window(async move {
        let title = tr("Bild wählen");
        let Some(picked) = off_ui(move || native::pick_files(&title, &tr("Bilder"), &["png", "jpg", "jpeg", "webp", "gif"], false)).await else { return };
        let Some(path) = picked.into_iter().next() else { return };
        set_icon(store, id, Some(path)).await;
    });
}

async fn set_icon(store: Store, id: String, path: Option<PathBuf>) {
    let at = id.clone();
    let mut error = store.ui.instance.error;
    match off_ui(move || crate::core::instance::set_icon(&at, path.as_deref())).await {
        Some(Ok(saved)) => {
            error.write().remove(&id);
            store.dispatch(AppEvent::InstanceChanged(saved));
        }
        Some(Err(e)) => {
            error.write().insert(id, forms::explain_as(tr("Das Bild wurde nicht übernommen."), &e).text);
        }
        None => {}
    }
}

/// The name and the picture of the instance, at the top of its settings.
#[derive(PartialEq)]
struct Identity {
    made: Instance,
}

impl Component for Identity {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let made = &self.made;
        let id = made.id.clone();
        let name = use_state(String::new);
        let problem = use_state(|| None::<String>);
        let draft = use_hook(Draft::default);
        let submit = use_typed(name, made.name.clone(), id.clone(), problem, draft, move |id, text| save_name(store, id, text));
        let field = Field::new("instance.settings.name", name).on_submit(move |s| submit.call(s));
        let (pick, clear) = (id.clone(), id.clone());
        let mut buttons = rect().horizontal().spacing(space::XS).child(Button::new("instance.settings.icon.pick", tr("Bild wählen…")).glyph(Icon::Image).on_press(move |_| choose_icon(store, pick.clone())));
        if made.icon.is_some() {
            buttons = buttons.child(Button::new("instance.settings.icon.clear", tr("Bild entfernen")).plain().on_press(move |_| {
                let id = clear.clone();
                spawn_in_window(async move { set_icon(store, id, None).await });
            }));
        }
        let picture = super::InstanceIcon { id: made.id.clone(), name: made.name.clone(), picture: made.icon_path(), side: ICON };
        let side = rect().width(Size::flex(1.)).spacing(space::S).child(setting(&t, tr("Name"), None, field.into_element(), None, problem.read().clone())).child(buttons);
        rect().width(Size::fill()).horizontal().spacing(space::L).child(picture).child(side).content(Content::flex())
    }
}

fn save_jvm(store: Store, id: &str, text: &str) -> Option<String> {
    let args = split_args(text);
    if let Err(problem) = options::check_jvm_args(&args) {
        return Some(problem);
    }
    let saved = store.model.peek().instance(id).map(|m| save::pending_jvm(store, id).unwrap_or_else(|| m.jvm_args.clone()));
    if saved.is_some_and(|s| s != args) {
        save::enqueue(store, id, Job::Save(Change::JvmArgs(args)));
    }
    None
}

fn save_args(store: Store, id: &str, text: &str) -> Option<String> {
    let list = split_args(text);
    if let Err(problem) = options::check_game_args(&list) {
        return Some(problem);
    }
    let saved = store.model.peek().instance(id).map(|m| save::pending_options(store, id, &m.options).game_args);
    if saved.is_some_and(|s| s != list) {
        save::enqueue(store, id, Job::Save(Change::Option(OptionChange::GameArgs(list))));
    }
    None
}

fn save_memory_max(store: Store, id: &str, text: &str) -> Option<String> {
    let typed = match options::parse_memory(text) {
        Ok(typed) => typed,
        Err(why) => return Some(why),
    };
    let made = store.model.peek().instance(id).cloned()?;
    if typed == save::pending_memory(store, id).unwrap_or_else(|| made.own_memory()) {
        return None;
    }
    if let Some(max) = typed {
        let min = save::pending_memory_min(store, id).unwrap_or_else(|| made.own_memory_min()).or(settings::active().game.memory_min_mb);
        if let Err(why) = options::check_memory_max(max, min, forms::total_ram_mb()) {
            return Some(why);
        }
    }
    save::enqueue(store, id, Job::Save(Change::Memory(typed)));
    None
}

fn save_memory_min(store: Store, id: &str, text: &str) -> Option<String> {
    let typed = match options::parse_memory(text) {
        Ok(typed) => typed,
        Err(why) => return Some(why),
    };
    let made = store.model.peek().instance(id).cloned()?;
    if typed == save::pending_memory_min(store, id).unwrap_or_else(|| made.own_memory_min()) {
        return None;
    }
    if let Some(min) = typed {
        let max = save::pending_memory(store, id).unwrap_or_else(|| made.own_memory()).unwrap_or(settings::active().game.memory_mb);
        if let Err(why) = options::check_memory_min(min, max) {
            return Some(why);
        }
    }
    save::enqueue(store, id, Job::Save(Change::MemoryMin(typed)));
    None
}

fn save_window(store: Store, id: &str, width: &str, height: &str) -> Option<String> {
    let size = match options::parse_window(width, height) {
        Ok(size) => size,
        Err(why) => return Some(why),
    };
    let saved = store.model.peek().instance(id).map(|m| save::pending_options(store, id, &m.options).window);
    if saved.is_some_and(|s| s != size) {
        save::enqueue(store, id, Job::Save(Change::Option(OptionChange::Window(size))));
    }
    None
}

/// The Einstellungen tab: version and loader, then the game, and start options under "Erweitert".
#[derive(PartialEq)]
pub struct Settings {
    pub facts: Facts,
}

impl Component for Settings {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let f = &self.facts;
        let made = f.made.clone();
        let id = made.id.clone();
        let nav = store.ui.instance;
        let why = super::blocked(store, &id);
        let others = use_state(|| false);

        let mut catalog = use_state(|| Load::<Vec<GameVersion>>::Loading);
        let mut asked = use_state(|| 0u32);
        use_side_effect(move || {
            let _ = asked.read();
            catalog.set(Load::Loading);
            spawn(async move {
                let result = off_ui(remote::catalog).await;
                catalog.set(match result {
                    Some(Ok(list)) if !list.is_empty() => Load::Ready(list),
                    Some(Ok(_)) => Load::Failed(tr("Mojang hat keine Versionen geliefert. Versuche es später noch einmal.")),
                    Some(Err(e)) => Load::Failed(forms::explain_as(tr("Versionen nicht geladen."), &e).text),
                    None => Load::Failed(forms::unexplained()),
                });
            });
        });
        let mut supported = use_state(|| None::<(String, Option<HashSet<String>>)>);
        let kind = made.loader.name().to_owned();
        let loader_now = made.loader.clone();
        use_side_effect_with_deps(&kind, move |kind| {
            let (kind, loader) = (kind.clone(), loader_now.clone());
            spawn(async move {
                if let Some(found) = off_ui(move || remote::supported_versions(&loader)).await {
                    supported.set(Some((kind, found)));
                }
            });
        });

        let mut col = rect().width(Size::fill()).padding(space::XL).spacing(space::XL).child(Identity { made: made.clone() });

        let all = *store.ui.all_versions.read();
        let mut version_problem = None;
        let version_control: Element = {
            let catalog = catalog.read().clone();
            let list: Option<Vec<GameVersion>> = match &catalog {
                Load::Ready(list) => {
                    let known = supported.read().as_ref().filter(|(k, _)| *k == kind).and_then(|(_, s)| s.clone());
                    Some(shown(list, all, known.as_ref(), &made.game_version))
                }
                _ => None,
            };
            match (list, &catalog) {
                (Some(list), _) => {
                    let labels = list.iter().map(version_text).collect();
                    let at = list.iter().position(|v| v.id == made.game_version).unwrap_or(0);
                    let pick = id.clone();
                    Select::new("instance.settings.version", tr("Minecraft-Version"), labels, at)
                        .width(control::VERSION_MENU)
                        .disabled(why.clone())
                        .on_choose(move |i| {
                            if let Some(v) = list.get(i) {
                                pick_version(store, &pick, v.id.clone());
                            }
                        })
                        .into_element()
                }
                (None, Load::Failed(e)) => {
                    version_problem = Some(e.clone());
                    rect()
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(space::S)
                        .child(line(&t, made.game_version.clone(), font::BODY, t.text1))
                        .child(Button::new("instance.settings.versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).plain().on_press(move |_| {
                            let next = asked.peek().wrapping_add(1);
                            asked.set(next);
                        }))
                        .into_element()
                }
                _ => rect().height(Size::px(control::M)).center().child(caption(&t, tr("Versionen werden geladen …"))).into_element(),
            }
        };
        let mut rows = vec![row(&t, "version", tr("Minecraft-Version"), None, version_control, None, version_problem).into_element()];
        let snapshots = tr("Snapshots zeigen");
        let mut everything = Line::new("instance.settings.everything.row", snapshots.clone())
            .dim(why.is_some())
            .control(Toggle::new("instance.settings.everything", snapshots, all).disabled(why.is_some()).on_toggle(move |on| remember_all_versions(store, on)));
        if why.is_none() {
            everything = everything.on_press(move |_| remember_all_versions(store, !all));
        }
        rows.push(everything.into_element());
        rows.push(loader_row(&t, store, &made, why.clone(), others));
        if made.loader.takes_mods() {
            rows.push(LoaderVersion { made: made.clone(), why: why.clone() }.into_element());
        }
        let mut footer: Vec<String> = Vec::new();
        match (nav.busy.read().get(&id), &why) {
            (Some(b), _) => footer.push(b.text.clone()),
            (None, Some(why)) => footer.push(why.clone()),
            (None, None) => {}
        }
        footer.extend(nav.notes.read().get(&id).into_iter().flatten().cloned());
        col = col.child(group(&t, Some(tr("Version & Loader")), rows, (!footer.is_empty()).then(|| footer.join(" "))));
        col = col.child(Game { facts: f.clone() });
        col = col.child(super::sharing::Sharing { facts: f.clone() });
        col = col.child(Protection { facts: f.clone() });

        let open = *nav.advanced.read();
        let mut adv = nav.advanced;
        let disclosure = Button::new("instance.settings.advanced", tr("Erweitert")).glyph(if open { Icon::ChevronDown } else { Icon::ChevronRight }).plain().on_press(move |_| {
            let now = *adv.peek();
            adv.set(!now);
        });
        let mut advanced = rect().width(Size::fill()).spacing(space::S).child(rect().horizontal().child(disclosure));
        if open {
            advanced = advanced.child(Advanced { facts: f.clone() });
        }
        col = col.child(advanced);
        ScrollView::new().width(Size::fill()).height(Size::fill()).child(col)
    }
}

/// The loader as the create sheet asks for it: with mods or without, and the other mod loaders one click deeper.
fn loader_row(t: &Theme, store: Store, made: &Instance, why: Option<String>, mut others: State<bool>) -> Element {
    let modded = made.loader.takes_mods();
    let mod_name = if modded { made.loader.name() } else { Loader::default().name() };
    let segments = vec![
        Segment { id: "instance.settings.loader.mods".into(), label: trf("Mit Mods (%@)", &[&mod_name]), tip: None },
        Segment { id: "instance.settings.loader.vanilla".into(), label: tr("Ohne Mods"), tip: None },
    ];
    let pick = made.id.clone();
    let with_mods = if modded { made.loader.clone() } else { Loader::default() };
    let control = Segmented::new(tr("Loader"), segments, usize::from(!modded)).disabled(why.clone()).on_choose(move |i| pick_loader(store, &pick, if i == 0 { with_mods.clone() } else { Loader::Vanilla }));
    let mut line = row(t, "loader", tr("Loader"), None, control, None, None);
    if modded {
        let other = !made.loader.same_kind(&Loader::default());
        let open = other || *others.read();
        let mut below = rect().width(Size::fill()).spacing(space::XS);
        if !other {
            let toggle = Button::new("instance.settings.loader.others", tr("Andere Loader")).plain().glyph(if open { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| others.set(!open));
            below = below.child(rect().horizontal().child(toggle));
        }
        if open {
            let chosen = MOD_LOADERS.iter().position(|l| made.loader.same_kind(l)).unwrap_or(0);
            let segments = MOD_LOADERS.iter().map(|l| Segment { id: format!("instance.settings.loader.{}", l.name().to_lowercase()), label: l.name().to_owned(), tip: None }).collect();
            let pick = made.id.clone();
            below = below.child(rect().horizontal().padding((0., 0., 0., space::L)).child(Segmented::new(tr("Mod-Loader"), segments, chosen).disabled(why).on_choose(move |i| {
                if let Some(l) = MOD_LOADERS.get(i) {
                    pick_loader(store, &pick, l.clone());
                }
            })));
        }
        line = line.below(below);
    }
    line.into_element()
}

fn pick_version(store: Store, id: &str, version: String) {
    let Some(made) = store.model.peek().instance(id).cloned() else { return };
    if version == made.game_version || super::blocked(store, id).is_some() {
        return;
    }
    ask_switch(store, &made, Switch::Version(version));
}

fn pick_loader(store: Store, id: &str, loader: Loader) {
    let Some(made) = store.model.peek().instance(id).cloned() else { return };
    if made.loader.same_kind(&loader) || super::blocked(store, id).is_some() {
        return;
    }
    ask_switch(store, &made, Switch::Loader(loader));
}

fn ask_switch(store: Store, made: &Instance, switch: Switch) {
    let id = made.id.clone();
    if made.mods.is_empty() {
        switch_safely(store, id, switch);
        return;
    }
    let heading = match &switch {
        Switch::Version(v) => trf("Auf Minecraft %@ umstellen?", &[v]),
        Switch::Loader(l) => trf("Auf %@ umstellen?", &[&l.name()]),
    };
    sheet::confirm(store.ui, "instance.switch", heading, tr("Mods, die es dafür nicht gibt, werden entfernt. Welten bleiben."), tr("Umstellen"), false, move |_| {
        if super::blocked(store, &id).is_none() {
            switch_safely(store, id.clone(), switch.clone());
        }
    });
}

fn switch_safely(store: Store, id: String, switch: Switch) {
    let mut notes = store.ui.instance.notes;
    notes.write().remove(&id);
    let reason = match &switch {
        Switch::Version(v) => Reason::Version(v.clone()),
        Switch::Loader(l) => Reason::Loader(l.name().to_owned()),
    };
    let at = id.clone();
    snapshots::before(store, &at, reason, move || {
        if super::blocked(store, &id).is_none() {
            save::enqueue(store, &id, Job::Switch(switch));
        }
    });
}

/// The Spiel group: who plays the instance alone, its memory and its window.
#[derive(PartialEq)]
struct Game {
    facts: Facts,
}

impl Component for Game {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let f = &self.facts;
        let made = f.made.clone();
        let id = made.id.clone();
        let now: LaunchOptions = save::pending_options(store, &id, &made.options);
        let busy = store.ui.instance.busy.read().contains_key(&id);
        let standard = settings::active().game.clone();

        let width = use_state(String::new);
        let height = use_state(String::new);
        let mut window_problem = use_state(|| None::<String>);
        let w_draft = use_hook(Draft::default);
        let h_draft = use_hook(Draft::default);
        let other = h_draft.clone();
        let w_submit = use_typed(width, now.window.map(|w| w.width.to_string()).unwrap_or_default(), id.clone(), window_problem, w_draft.clone(), move |id, w| save_window(store, id, w, &other.borrow()));
        let other = w_draft;
        let h_submit = use_typed(height, now.window.map(|w| w.height.to_string()).unwrap_or_default(), id.clone(), window_problem, h_draft, move |id, h| save_window(store, id, &other.borrow(), h));

        let mut rows = vec![account_row(&t, store, f)];

        let ceiling = u32::try_from(forms::total_ram_mb()).unwrap_or(u32::MAX).max(options::MEMORY_FLOOR_MB);
        let own_mb = save::pending_memory(store, &id).unwrap_or_else(|| made.own_memory());
        let own_min = save::pending_memory_min(store, &id).unwrap_or_else(|| made.own_memory_min());
        let wait = || busy.then(|| tr("Warte, bis die Umstellung fertig ist."));

        let max_text = use_state(String::new);
        let max_problem = use_state(|| None::<String>);
        let max_submit = use_typed(max_text, own_mb.map(|mb| mb.to_string()).unwrap_or_default(), id.clone(), max_problem, use_hook(Draft::default), move |id, text| save_memory_max(store, id, text));
        let min_text = use_state(String::new);
        let min_problem = use_state(|| None::<String>);
        let min_submit = use_typed(min_text, own_min.map(|mb| mb.to_string()).unwrap_or_default(), id.clone(), min_problem, use_hook(Draft::default), move |id, text| save_memory_min(store, id, text));

        let max_field = Memory::new("instance.settings.memory", tr("Maximaler Arbeitsspeicher"), max_text)
            .placeholder(standard.memory_mb.to_string())
            .steps(Steps { floor: options::MEMORY_FLOOR_MB, ceiling, from: own_mb.unwrap_or(standard.memory_mb) })
            .disabled(wait())
            .on_submit(move |text| max_submit.call(text));
        let min_field = Memory::new("instance.settings.memory_min", tr("Minimaler Arbeitsspeicher"), min_text)
            .placeholder(standard.memory_min_mb.map(|mb| mb.to_string()).unwrap_or_else(|| tr("Automatisch")))
            .steps(Steps { floor: options::MEMORY_MIN_FLOOR_MB, ceiling: own_mb.unwrap_or(standard.memory_mb), from: own_mb.unwrap_or(standard.memory_mb).min(1024) })
            .disabled(wait())
            .on_submit(move |text| min_submit.call(text));

        let mem_id = id.clone();
        let memory_reset = own_mb.map(|_| reset("memory", tr("Maximaler Arbeitsspeicher"), move |_| save::enqueue(store, &mem_id, Job::Save(Change::Memory(None)))));
        let min_id = id.clone();
        let min_reset = own_min.map(|_| reset("memory_min", tr("Minimaler Arbeitsspeicher"), move |_| save::enqueue(store, &min_id, Job::Save(Change::MemoryMin(None)))));
        let memory_hint = match (own_mb, f.batches.is_empty()) {
            (None, true) => Some(as_standard(forms::memory_label(standard.memory_mb))),
            (None, false) => Some(format!("{} · {}", as_standard(forms::memory_label(standard.memory_mb)), tr("Pro Account in jedem Batch."))),
            (Some(_), false) => Some(tr("Pro Account in jedem Batch.")),
            (Some(_), true) => None,
        };
        let min_hint = match own_min {
            None => Some(as_standard(standard.memory_min_mb.map(forms::memory_label).unwrap_or_else(|| tr("Automatisch")))),
            Some(_) => None,
        };
        rows.push(row(&t, "memory", tr("Maximaler Arbeitsspeicher"), memory_reset, max_field, memory_hint, max_problem.read().clone()).into_element());
        rows.push(row(&t, "memory_min", tr("Minimaler Arbeitsspeicher"), min_reset, min_field, min_hint, min_problem.read().clone()).into_element());

        let wr = id.clone();
        let window_reset = now.window.filter(|w| Some(*w) != standard.window).map(|_| {
            reset("window", tr("Fenstergröße beim Start"), move |_| {
                window_problem.set_if_modified(None);
                save::enqueue(store, &wr, Job::Save(Change::Option(OptionChange::Window(None))));
            })
        });
        let size = rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(Field::new("instance.settings.window.width", width).placeholder(standard.window.map_or_else(|| tr("Breite"), |w| w.width.to_string())).width(Size::px(control::NUMBER)).on_submit(move |s| w_submit.call(s)))
            .child(line(&t, "×", font::BODY, t.text3))
            .child(Field::new("instance.settings.window.height", height).placeholder(standard.window.map_or_else(|| tr("Höhe"), |w| w.height.to_string())).width(Size::px(control::NUMBER)).on_submit(move |s| h_submit.call(s)));
        let window_hint = match standard.window {
            Some(w) if now.window.is_none() => as_standard(format!("{} × {}", w.width, w.height)),
            Some(w) => trf("Leer lassen: %@", &[&as_standard(format!("{} × {}", w.width, w.height))]),
            None => tr("Leer lassen für die Größe, die Minecraft wählt."),
        };
        rows.push(row(&t, "window", tr("Fenstergröße beim Start"), window_reset, size, Some(window_hint), window_problem.read().clone()).into_element());

        let mut footer: Vec<String> = f.running.then(|| tr("Änderungen wirken beim nächsten Start.")).into_iter().collect();
        if !f.batches.is_empty() {
            footer.push(tr("Batches mit dieser Instanz teilen Tastenbelegung und Serverliste."));
        }
        group(&t, Some(tr("Spiel")), rows, (!footer.is_empty()).then(|| footer.join(" ")))
    }
}

/// "Spielen als": always there so the group never jumps; a menu from two accounts on.
fn account_row(t: &Theme, store: Store, f: &Facts) -> Element {
    let label = tr("Spielen als");
    let hint = Some(tr("Gilt, wenn du diese Instanz allein spielst. Batches haben eigene Accounts."));
    match f.accounts.as_slice() {
        [] => {
            let add = Button::new("instance.settings.account.add", tr("Account hinzufügen…")).plain().on_press(move |_| {
                crate::view::commands::want(store, crate::view::commands::Want::AddAccount);
            });
            return row(t, "account", label, None, add, hint, None).into_element();
        }
        [(_, name)] => return row(t, "account", label, None, line(t, name.clone(), font::BODY, t.text2), hint, None).into_element(),
        _ => {}
    }
    let m = store.model.read();
    let active = crate::app::reduce::active_account(&m).and_then(|a| m.account(a)).map(|a| a.name.clone());
    let mut labels = vec![active.map_or_else(|| tr("Standard-Account"), |n| trf("Standard-Account (%@)", &[&n]))];
    labels.extend(f.accounts.iter().map(|(_, n)| n.clone()));
    let own = f.made.default_account.and_then(|d| f.accounts.iter().position(|(a, _)| *a == d));
    let at = own.map_or(0, |n| n + 1);
    let ids: Vec<Uuid> = f.accounts.iter().map(|(a, _)| *a).collect();
    let instance = f.made.id.clone();
    let select = Select::new("instance.settings.account", label.clone(), labels, at).width(control::VERSION_MENU).on_choose(move |i| {
        let account = i.checked_sub(1).and_then(|n| ids.get(n).copied());
        store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account });
    });
    let reset_id = f.made.id.clone();
    let clear = own.map(|_| reset("account", label.clone(), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: reset_id.clone(), account: None })));
    row(t, "account", label, clear, select, hint, None).into_element()
}

/// The Schutz group: what the game and its mods are kept away from, each protection off until it is turned on.
#[derive(PartialEq)]
struct Protection {
    facts: Facts,
}

impl Component for Protection {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let made = &self.facts.made;
        let on = save::pending_options(store, &made.id, &made.options).hide_login;
        let able = vault::may_hide(&made.game_version);
        let (hint, problem) = match (able, on) {
            (true, _) => (Some(tr("Das Spiel bekommt nur einen Platzhalter, deine echte Anmeldung bleibt bei Manycraft. Realms und Mods, die deine Anmeldung selbst brauchen, funktionieren damit nicht.")), None),
            (false, false) => (Some(trf("Geht erst ab Minecraft %@.", &[&vault::FIRST_VERSION])), None),
            (false, true) => (None, Some(trf("Minecraft %@ kann die Anmeldung nicht verbergen. Die Instanz startet so nicht.", &[&made.game_version]))),
        };
        let usable = able || on;
        let label = tr("Anmeldung vor Mods verbergen");
        let (flip, press) = (made.id.clone(), made.id.clone());
        let toggle = Toggle::new("instance.settings.hide_login", label.clone(), on).disabled(!usable).on_toggle(move |v| hide_login(store, &flip, v));
        let mut line = row(&t, "hide_login", label, None, toggle, hint, problem).dim(!usable);
        if usable {
            line = line.on_press(move |_| hide_login(store, &press, !on));
        }
        let running = store.model.read().games.values().any(|g| g.instance_id == made.id && g.phase.is_running());
        group(&t, Some(tr("Schutz")), vec![line.into_element()], running.then(|| tr("Ein Spiel dieser Instanz läuft. Was du umstellst, gilt ab seinem nächsten Start.")))
    }
}

fn hide_login(store: Store, id: &str, on: bool) {
    save::enqueue(store, id, Job::Save(Change::Option(OptionChange::HideLogin(on))));
}

/// The Erweitert group: Java, start arguments and graphics, for the few who need them.
#[derive(PartialEq)]
struct Advanced {
    facts: Facts,
}

impl Component for Advanced {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let made = self.facts.made.clone();
        let id = made.id.clone();
        let now: LaunchOptions = save::pending_options(store, &id, &made.options);

        let mut javas = use_state(|| None::<Vec<Java>>);
        use_hook(move || {
            spawn(async move {
                if let Some(found) = off_ui(remote::detect_javas).await {
                    javas.set(Some(found));
                }
            });
        });
        let mut probe = use_state(|| None::<(PathBuf, String, Result<Java, String>, Option<u32>)>);
        let probe_key = now.java.clone().map(|p| (p, made.game_version.clone()));
        use_side_effect_with_deps(&probe_key, move |key| {
            let Some((path, version)) = key.clone() else { return };
            probe.set(Some((path.clone(), version.clone(), Err(String::new()), None)));
            spawn(async move {
                let (p, v) = (path.clone(), version.clone());
                if let Some((found, expected)) = off_ui(move || (remote::probe_java(&p).map_err(|e| format!("{e:#}")), remote::expected_java(&v))).await
                    && probe.peek().as_ref().is_some_and(|(pp, vv, ..)| *pp == path && *vv == version)
                {
                    probe.set(Some((path, version, found, expected)));
                }
            });
        });
        let nav = store.ui.instance;
        let java_problem = nav.java_problem.read().get(&id).cloned();
        let picking = nav.java_picking.read().contains(&id);

        let jvm = use_state(String::new);
        let mut jvm_problem = use_state(|| None::<String>);
        let jvm_draft = use_hook(Draft::default);
        let jvm_submit = use_typed(jvm, made.jvm_args.join(" "), id.clone(), jvm_problem, jvm_draft, move |id, text| save_jvm(store, id, text));
        let args = use_state(String::new);
        let mut args_problem = use_state(|| None::<String>);
        let args_draft = use_hook(Draft::default);
        let args_submit = use_typed(args, now.game_args.join(" "), id.clone(), args_problem, args_draft, move |id, text| save_args(store, id, text));

        let standard = settings::active().game.clone();
        let mut rows = Vec::new();

        enum Choice {
            Auto,
            Path(PathBuf),
            Pick,
        }
        let standard_java = match &standard.java {
            Some(path) => javas.read().iter().flatten().find(|j| j.path == *path).map_or_else(|| tr("Eigenes Java"), java_title),
            None => tr("Automatisch"),
        };
        let mut choices = vec![(Choice::Auto, as_standard(standard_java))];
        for found in javas.read().iter().flatten() {
            choices.push((Choice::Path(found.path.clone()), java_title(found)));
        }
        if let Some(path) = now.java.as_ref().filter(|p| !choices.iter().any(|(c, _)| matches!(c, Choice::Path(q) if q == *p))) {
            let title = match &*probe.read() {
                Some((probed, _, Ok(found), _)) if probed == path => java_title(found),
                _ => tr("Eigenes Java"),
            };
            choices.push((Choice::Path(path.clone()), title));
        }
        choices.push((Choice::Pick, tr("Datei wählen…")));
        let at = choices.iter().position(|(c, _)| match (c, &now.java) {
            (Choice::Auto, None) => true,
            (Choice::Path(p), Some(q)) => p == q,
            _ => false,
        });
        let labels = choices.iter().map(|(_, l)| l.clone()).collect();
        let paths: Vec<Option<Option<PathBuf>>> = choices
            .iter()
            .map(|(c, _)| match c {
                Choice::Auto => Some(None),
                Choice::Path(p) => Some(Some(p.clone())),
                Choice::Pick => None,
            })
            .collect();
        let java_id = id.clone();
        let java_select = Select::new("instance.settings.java", tr("Java"), labels, at.unwrap_or(0))
            .width(control::JAVA_MENU)
            .disabled(picking.then(|| tr("Datei wird gewählt …")))
            .on_choose(move |i| {
                forget_java_problem(store, &java_id);
                match paths.get(i).cloned() {
                    Some(Some(java)) => save::enqueue(store, &java_id, Job::Save(Change::Option(OptionChange::Java(java)))),
                    Some(None) => choose_java(store, java_id.clone()),
                    None => {}
                }
            });
        let (java_hint, java_problem_now) = match (java_problem, &now.java) {
            (Some(p), _) => (None, Some(p)),
            (None, None) if javas.read().is_none() => (Some(tr("Suche installierte Java-Versionen …")), None),
            (None, None) if standard.java.is_some() => (Some(tr("Passt das Standard-Java nicht zu dieser Version, nimmt Manycraft das passende.")), None),
            (None, None) => (Some(tr("Manycraft lädt das passende Java für diese Version.")), None),
            (None, Some(path)) => match &*probe.read() {
                Some((probed, version, Ok(found), expected)) if probed == path && *version == made.game_version => (Some(path.display().to_string()), expected.and_then(|needed| mismatch(needed, found.major, &made.game_version))),
                Some((probed, _, Err(e), _)) if probed == path && !e.is_empty() => (None, Some(e.clone())),
                Some((probed, _, Err(_), _)) if probed == path => (Some(tr("Java wird geprüft …")), None),
                _ => (Some(path.display().to_string()), None),
            },
        };
        let java_reset_id = id.clone();
        let java_reset = now.java.as_ref().filter(|j| standard.java.as_ref() != Some(*j)).map(|_| {
            reset("java", tr("Java"), move |_| {
                forget_java_problem(store, &java_reset_id);
                save::enqueue(store, &java_reset_id, Job::Save(Change::Option(OptionChange::Java(None))));
            })
        });
        rows.push(row(&t, "java", tr("Java"), java_reset, java_select, java_hint, java_problem_now).into_element());

        let jr = id.clone();
        let jvm_reset = (!made.jvm_args.is_empty()).then(|| {
            reset("jvm_args", tr("Java-Argumente"), move |_| {
                jvm_problem.set_if_modified(None);
                save::enqueue(store, &jr, Job::Save(Change::JvmArgs(Vec::new())));
            })
        });
        let jvm_field = Field::new("instance.settings.jvm_args", jvm).placeholder("-XX:+UseG1GC").on_submit(move |s| jvm_submit.call(s));
        rows.push(wide(&t, "jvm_args", tr("Java-Argumente"), jvm_reset, jvm_field.into_element(), Some(tr("Zum Beispiel -XX:+UseG1GC, getrennt durch Leerzeichen.")), jvm_problem.read().clone()));

        let gr = id.clone();
        let args_reset = (!now.game_args.is_empty()).then(|| {
            reset("game_args", tr("Spielargumente"), move |_| {
                args_problem.set_if_modified(None);
                save::enqueue(store, &gr, Job::Save(Change::Option(OptionChange::GameArgs(Vec::new()))));
            })
        });
        let args_field = Field::new("instance.settings.game_args", args).on_submit(move |s| args_submit.call(s));
        rows.push(wide(&t, "game_args", tr("Spielargumente"), args_reset, args_field.into_element(), Some(tr("Gehen beim Start an Minecraft, zum Beispiel --fullscreen.")), args_problem.read().clone()));

        if cfg!(windows) || now.gpu.is_some() {
            let choices = [(None, as_standard(gpu_label(standard.gpu))), (Some(Gpu::HighPerformance), gpu_label(Some(Gpu::HighPerformance))), (Some(Gpu::PowerSaving), gpu_label(Some(Gpu::PowerSaving)))];
            rows.push(row(&t, "gpu", tr("Grafikkarte"), None, choice_row(store, &id, "gpu", tr("Grafikkarte"), now.gpu, choices.to_vec(), OptionChange::Gpu), Some(tr("Gilt für alle Instanzen, die dasselbe Java nutzen.")), None).into_element());
        }
        if options::renderer_choice(&made.game_version) {
            let choices = [(None, tr("Automatisch")), (Some(Renderer::OpenGl), "OpenGL".to_owned()), (Some(Renderer::Vulkan), "Vulkan".to_owned())];
            rows.push(row(&t, "renderer", tr("Grafikschnittstelle"), None, choice_row(store, &id, "renderer", tr("Grafikschnittstelle"), now.renderer, choices.to_vec(), OptionChange::Renderer), Some(tr("Automatisch nimmt, was im Spiel eingestellt ist.")), None).into_element());
        }
        group(&t, None, rows, None)
    }
}

fn choice_row<T: Copy + PartialEq + 'static>(store: Store, id: &str, key: &str, label: String, now: Option<T>, choices: Vec<(Option<T>, String)>, wrap: fn(Option<T>) -> OptionChange) -> Element {
    let chosen = choices.iter().position(|(v, _)| *v == now).unwrap_or(0);
    let segments = choices.iter().enumerate().map(|(n, (_, l))| Segment { id: format!("instance.settings.{key}.{n}"), label: l.clone(), tip: None }).collect();
    let id = id.to_owned();
    rect()
        .child(Segmented::new(label, segments, chosen).on_choose(move |i| {
            if let Some((value, _)) = choices.get(i) {
                save::enqueue(store, &id, Job::Save(Change::Option(wrap(*value))));
            }
        }))
        .into_element()
}

/// "Wie Standard (4 GB)": a field that follows the launcher default and what that default is.
fn as_standard(value: String) -> String {
    trf("Wie Standard (%@)", &[&value])
}

fn forget_java_problem(store: Store, id: &str) {
    let mut problems = store.ui.instance.java_problem;
    if problems.peek().contains_key(id) {
        problems.write().remove(id);
    }
}

fn choose_java(store: Store, id: String) {
    let nav = store.ui.instance;
    let (mut picking, mut problems) = (nav.java_picking, nav.java_problem);
    if !picking.write().insert(id.clone()) {
        return;
    }
    spawn_in_window(async move {
        let picked = off_ui(|| native::pick_files(&tr("Java wählen"), "Java", &[], false)).await.unwrap_or_default();
        let found = match picked.into_iter().next() {
            Some(path) => off_ui(move || remote::probe_java(&path)).await,
            None => None,
        };
        picking.write().remove(&id);
        match found {
            Some(Ok(java)) => save::enqueue(store, &id, Job::Save(Change::Option(OptionChange::Java(Some(java.path))))),
            Some(Err(e)) => {
                problems.write().insert(id, format!("{e:#}"));
            }
            None => {}
        }
    });
}

#[derive(PartialEq)]
struct LoaderVersion {
    made: Instance,
    why: Option<String>,
}

impl Component for LoaderVersion {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let made = &self.made;
        let mut versions = use_state(|| Load::<Versions>::Loading);
        let mut asked = use_state(|| 0u32);
        let key = (made.loader.with_version(None), made.game_version.clone(), *asked.read());
        use_side_effect_with_deps(&key, move |(kind, game, _)| {
            let (kind, game) = (kind.clone(), game.clone());
            versions.set(Load::Loading);
            spawn(async move {
                let result = off_ui(move || remote::loader_versions(&kind, &game)).await;
                versions.set(match result {
                    Some(Ok(v)) => Load::Ready(v),
                    Some(Err(e)) => Load::Failed(forms::explain_as(tr("Versionen nicht geladen."), &e).text),
                    None => Load::Failed(forms::unexplained()),
                });
            });
        });
        let pinned = made.loader.version().map(str::to_owned);
        let (newest, list) = match &*versions.read() {
            Load::Ready(v) => (v.default.as_ref().map_or_else(|| tr("Immer die neueste"), |d| trf("Immer die neueste (%@)", &[d])), v.list.clone()),
            _ => (tr("Immer die neueste"), Vec::new()),
        };
        let mut items: Vec<Option<String>> = vec![None];
        if let Some(p) = pinned.as_ref().filter(|p| !list.contains(p)) {
            items.push(Some(p.clone()));
        }
        items.extend(list.into_iter().map(Some));
        let labels = items.iter().map(|i| i.clone().unwrap_or_else(|| newest.clone())).collect();
        let at = items.iter().position(|i| *i == pinned).unwrap_or(0);
        let id = made.id.clone();
        let select = Select::new("instance.settings.loader_version", crate::ui::loader_version::label(&made.loader), labels, at).width(control::VERSION_MENU).disabled(self.why.clone()).on_choose(move |i| {
            let Some(want) = items.get(i).cloned() else { return };
            let now = store.model.peek().instance(&id).and_then(|m| m.loader.version().map(str::to_owned));
            let Some(loader) = store.model.peek().instance(&id).map(|m| m.loader.clone()).filter(|_| want != now && super::blocked(store, &id).is_none()) else { return };
            let target = want.as_ref().map_or_else(|| loader.name().to_owned(), |v| format!("{} {v}", loader.name()));
            let id = id.clone();
            snapshots::before(store, &id.clone(), Reason::LoaderUpdate(target), move || save::enqueue(store, &id, Job::Save(Change::LoaderVersion(want))));
        });
        let label = crate::ui::loader_version::label(&made.loader);
        match &*versions.read() {
            Load::Loading => row(&t, "loader_version", label, None, select, Some(tr("Versionen werden geladen …")), None),
            Load::Failed(e) => {
                let retry = Button::new("instance.settings.loader_versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).plain().on_press(move |_| {
                    let next = asked.peek().wrapping_add(1);
                    asked.set(next);
                });
                row(&t, "loader_version", label, Some(retry.into_element()), select, None, Some(e.clone()))
            }
            Load::Ready(_) => row(&t, "loader_version", label, None, select, None, None),
        }
    }
}
