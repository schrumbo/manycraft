use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use freya::prelude::*;
use uuid::Uuid;

use super::save::{self, Change, Job, OptionChange, Switch};
use super::{Facts, snapshots};
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::core::install::Versions;
use crate::core::instance::{DEFAULT_MEMORY_MB, Instance, LOADERS, Loader};
use crate::core::javas::{Java, mismatch};
use crate::core::options::{self, Gpu, LaunchOptions, Renderer};
use crate::core::snapshot::Reason;
use crate::core::versions::{GameVersion, Kind, shown};
use crate::ui::forms;
use crate::ui::remote;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::sheet;
use crate::view::kit::switch::Switch as Toggle;
use crate::view::kit::text::{caption, line, strong};
use crate::view::kit::{after, off_ui, use_theme};
use crate::view::native;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, iconsize, space};

const TYPING: Duration = Duration::from_millis(800);

#[derive(Clone)]
enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

fn split_args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn kind_label(kind: Kind) -> Option<String> {
    match kind {
        Kind::Release => None,
        Kind::Snapshot => Some(tr("Snapshot")),
        Kind::PreRelease => Some(tr("Vorabversion")),
        Kind::OldBeta => Some(tr("Alte Beta")),
        Kind::OldAlpha => Some(tr("Alte Alpha")),
    }
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

/// The Einstellungen tab: Minecraft version and loader up top, start options under "Erweitert".
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
        let mut everything = use_state(|| false);

        let mut col = rect().width(Size::fill()).padding(space::XL).spacing(space::XL);

        let version_control: Element = {
            let all = *everything.read();
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
                    let labels = list.iter().map(|v| kind_label(v.kind).map_or_else(|| v.id.clone(), |k| format!("{} · {k}", v.id))).collect();
                    let at = list.iter().position(|v| v.id == made.game_version).unwrap_or(0);
                    let pick = id.clone();
                    rect()
                        .spacing(space::XS)
                        .child(Select::new("instance.settings.version", tr("Minecraft-Version"), labels, at).width(control::VERSION_MENU).disabled(why.clone()).on_choose(move |i| {
                            if let Some(v) = list.get(i) {
                                pick_version(store, &pick, v.id.clone());
                            }
                        }))
                        .child(
                            rect()
                                .horizontal()
                                .cross_align(Alignment::Center)
                                .spacing(space::S)
                                .child(Toggle::new("instance.settings.everything", tr("Snapshots und alte Versionen zeigen"), all).disabled(why.is_some()).on_toggle(move |on| everything.set(on)))
                                .child(line(&t, tr("Snapshots und alte Versionen zeigen"), font::CALLOUT, if why.is_some() { t.text3 } else { t.text2 })),
                        )
                        .into_element()
                }
                (None, Load::Failed(e)) => rect()
                    .spacing(space::XS)
                    .child(line(&t, made.game_version.clone(), font::BODY, t.text1))
                    .child(rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(line(&t, e.clone(), font::CAPTION, t.danger_text)).child(Button::new("instance.settings.versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).plain().on_press(move |_| {
                        let next = asked.peek().wrapping_add(1);
                        asked.set(next);
                    })))
                    .into_element(),
                _ => rect().height(Size::px(control::M)).center().child(caption(&t, tr("Versionen werden geladen …"))).into_element(),
            }
        };
        let chosen = LOADERS.iter().position(|l| made.loader.same_kind(l)).unwrap_or(0);
        let segments = LOADERS.iter().map(|l| Segment { id: format!("instance.settings.loader.{}", l.name().to_lowercase()), label: l.name().to_owned(), tip: None }).collect();
        let pick = id.clone();
        let loader_control = Segmented::new(tr("Loader"), segments, chosen).disabled(why.clone()).on_choose(move |i| {
            if let Some(l) = LOADERS.get(i) {
                pick_loader(store, &pick, l.clone());
            }
        });
        let game = rect()
            .width(Size::fill())
            .horizontal()
            .spacing(space::XL)
            .child(rect().spacing(space::XS).child(label_row(&t, tr("Minecraft-Version"), None)).child(version_control))
            .child(rect().spacing(space::XS).child(label_row(&t, tr("Loader"), None)).child(loader_control));
        let mut top = rect().width(Size::fill()).spacing(space::S).child(game);
        match (nav.busy.read().get(&id), &why) {
            (Some(b), _) => top = top.child(caption(&t, b.text.clone())),
            (None, Some(why)) => top = top.child(caption(&t, why.clone())),
            (None, None) => {}
        }
        for n in nav.notes.read().get(&id).into_iter().flatten() {
            top = top.child(caption(&t, n.clone()));
        }
        col = col.child(top);
        if let Some(account) = account_setting(&t, store, f) {
            col = col.child(account);
        }

        let open = *nav.advanced.read();
        let mut adv = nav.advanced;
        let disclosure = Button::new("instance.settings.advanced", tr("Erweitert")).glyph(if open { Icon::ChevronDown } else { Icon::ChevronRight }).plain().on_press(move |_| {
            let now = *adv.peek();
            adv.set(!now);
        });
        let mut advanced = rect().width(Size::fill()).spacing(space::M).child(disclosure);
        if open {
            advanced = advanced.child(Advanced { facts: f.clone() });
        }
        col = col.child(advanced);
        col = col.child(caption(&t, tr("Batches mit dieser Instanz teilen Tastenbelegung und Serverliste.")));
        ScrollView::new().width(Size::fill()).height(Size::fill()).child(col)
    }
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
        let busy = store.ui.instance.busy.read().contains_key(&id);

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
        let width = use_state(String::new);
        let height = use_state(String::new);
        let mut window_problem = use_state(|| None::<String>);
        let w_draft = use_hook(Draft::default);
        let h_draft = use_hook(Draft::default);
        let other = h_draft.clone();
        let w_submit = use_typed(width, now.window.map(|w| w.width.to_string()).unwrap_or_default(), id.clone(), window_problem, w_draft.clone(), move |id, w| save_window(store, id, w, &other.borrow()));
        let other = w_draft;
        let h_submit = use_typed(height, now.window.map(|w| w.height.to_string()).unwrap_or_default(), id.clone(), window_problem, h_draft, move |id, h| save_window(store, id, &other.borrow(), h));

        let mut col = rect().width(Size::fill()).padding((0., 0., 0., space::L)).spacing(space::L).child(caption(&t, tr("Änderungen wirken beim nächsten Start.")));

        let max = forms::memory_max(forms::total_ram_mb());
        let shown_mb = save::pending_memory(store, &id).unwrap_or(made.memory_mb);
        let wait = || busy.then(|| tr("Warte, bis die Umstellung fertig ist."));
        let less = wait().or_else(|| (shown_mb <= forms::MEMORY_MIN).then(|| trf("Weniger geht nicht: mindestens %@.", &[&forms::memory_label(forms::MEMORY_MIN)])));
        let more = wait().or_else(|| (shown_mb >= max).then(|| trf("Mehr geht nicht: höchstens %@.", &[&forms::memory_label(max)])));
        let step = move |delta: i32, id: String| {
            move |_| {
                let Some(made) = store.model.peek().instance(&id).cloned() else { return };
                let now = save::pending_memory(store, &id).unwrap_or(made.memory_mb);
                let next = forms::memory_step(now, delta, max);
                if next != now {
                    save::enqueue(store, &id, Job::Save(Change::Memory(next)));
                }
            }
        };
        let stepper = rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .child(Button::icon("instance.settings.memory.less", Icon::Minus, tr("Weniger Arbeitsspeicher")).disabled(less).on_press(step(-1, id.clone())))
            .child(rect().width(Size::px(control::STEPPER_VALUE)).center().child(strong(&t, forms::memory_label(shown_mb), font::BODY, t.text1)))
            .child(Button::icon("instance.settings.memory.more", Icon::Plus, tr("Mehr Arbeitsspeicher")).disabled(more).on_press(step(1, id.clone())));
        let mem_id = id.clone();
        let memory_reset = (shown_mb != DEFAULT_MEMORY_MB).then(|| reset("memory", tr("Arbeitsspeicher"), move |_| save::enqueue(store, &mem_id, Job::Save(Change::Memory(DEFAULT_MEMORY_MB)))));
        col = col.child(setting(&t, tr("Arbeitsspeicher"), memory_reset, stepper.into_element(), Some(tr("Pro Account.")), None));

        enum Choice {
            Auto,
            Path(PathBuf),
            Pick,
        }
        let mut choices = vec![(Choice::Auto, tr("Automatisch (empfohlen)"))];
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
            (None, None) => (Some(tr("Manycraft lädt das passende Java für diese Version.")), None),
            (None, Some(path)) => match &*probe.read() {
                Some((probed, version, Ok(found), expected)) if probed == path && *version == made.game_version => (Some(path.display().to_string()), expected.and_then(|needed| mismatch(needed, found.major, &made.game_version))),
                Some((probed, _, Err(e), _)) if probed == path && !e.is_empty() => (None, Some(e.clone())),
                Some((probed, _, Err(_), _)) if probed == path => (Some(tr("Java wird geprüft …")), None),
                _ => (Some(path.display().to_string()), None),
            },
        };
        let java_reset_id = id.clone();
        let java_reset = now.java.is_some().then(|| {
            reset("java", tr("Java"), move |_| {
                forget_java_problem(store, &java_reset_id);
                save::enqueue(store, &java_reset_id, Job::Save(Change::Option(OptionChange::Java(None))));
            })
        });
        col = col.child(setting(&t, tr("Java"), java_reset, java_select.into_element(), java_hint, java_problem_now));

        let jr = id.clone();
        let jvm_reset = (!made.jvm_args.is_empty()).then(|| {
            reset("jvm_args", tr("Java-Argumente"), move |_| {
                jvm_problem.set_if_modified(None);
                save::enqueue(store, &jr, Job::Save(Change::JvmArgs(Vec::new())));
            })
        });
        let jvm_field = Field::new("instance.settings.jvm_args", jvm).placeholder("-XX:+UseG1GC").on_submit(move |s| jvm_submit.call(s));
        col = col.child(setting(&t, tr("Java-Argumente"), jvm_reset, jvm_field.into_element(), Some(tr("Zum Beispiel -XX:+UseG1GC, getrennt durch Leerzeichen.")), jvm_problem.read().clone()));

        let gr = id.clone();
        let args_reset = (!now.game_args.is_empty()).then(|| {
            reset("game_args", tr("Spielargumente"), move |_| {
                args_problem.set_if_modified(None);
                save::enqueue(store, &gr, Job::Save(Change::Option(OptionChange::GameArgs(Vec::new()))));
            })
        });
        let args_field = Field::new("instance.settings.game_args", args).on_submit(move |s| args_submit.call(s));
        col = col.child(setting(&t, tr("Spielargumente"), args_reset, args_field.into_element(), Some(tr("Gehen beim Start an Minecraft, zum Beispiel --fullscreen.")), args_problem.read().clone()));

        let wr = id.clone();
        let window_reset = now.window.is_some().then(|| {
            reset("window", tr("Fenstergröße beim Start"), move |_| {
                window_problem.set_if_modified(None);
                save::enqueue(store, &wr, Job::Save(Change::Option(OptionChange::Window(None))));
            })
        });
        let size = rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(Field::new("instance.settings.window.width", width).placeholder(tr("Breite")).width(Size::px(control::NUMBER)).on_submit(move |s| w_submit.call(s)))
            .child(line(&t, "×", font::BODY, t.text3))
            .child(Field::new("instance.settings.window.height", height).placeholder(tr("Höhe")).width(Size::px(control::NUMBER)).on_submit(move |s| h_submit.call(s)));
        col = col.child(setting(&t, tr("Fenstergröße beim Start"), window_reset, size.into_element(), Some(tr("Leer lassen für die Größe, die Minecraft wählt.")), window_problem.read().clone()));

        if cfg!(windows) || now.gpu.is_some() {
            let choices = [(None, tr("Automatisch")), (Some(Gpu::HighPerformance), tr("Hohe Leistung")), (Some(Gpu::PowerSaving), tr("Energie sparen"))];
            col = col.child(setting(&t, tr("Grafikkarte"), None, choice_row(store, &id, "gpu", tr("Grafikkarte"), now.gpu, choices.to_vec(), OptionChange::Gpu), Some(tr("Gilt für alle Instanzen, die dasselbe Java nutzen.")), None));
        }
        if options::renderer_choice(&made.game_version) {
            let choices = [(None, tr("Automatisch")), (Some(Renderer::OpenGl), "OpenGL".to_owned()), (Some(Renderer::Vulkan), "Vulkan".to_owned())];
            col = col.child(setting(&t, tr("Grafikschnittstelle"), None, choice_row(store, &id, "renderer", tr("Grafikschnittstelle"), now.renderer, choices.to_vec(), OptionChange::Renderer), Some(tr("Automatisch nimmt, was im Spiel eingestellt ist.")), None));
        }
        if made.loader.takes_mods() {
            col = col.child(LoaderVersion { made: made.clone(), why: super::blocked(store, &id) });
        }
        col
    }
}

fn account_setting(t: &Theme, store: Store, f: &Facts) -> Option<Element> {
    if f.accounts.len() < 2 {
        return None;
    }
    let m = store.model.read();
    let active = crate::app::reduce::active_account(&m).and_then(|a| m.account(a)).map(|a| a.name.clone());
    let mut labels = vec![active.map_or_else(|| tr("Aktiver Account"), |n| trf("Aktiver Account (%@)", &[&n]))];
    labels.extend(f.accounts.iter().map(|(_, n)| n.clone()));
    let own = f.made.default_account.and_then(|d| f.accounts.iter().position(|(a, _)| *a == d));
    let at = own.map_or(0, |n| n + 1);
    let ids: Vec<Uuid> = f.accounts.iter().map(|(a, _)| *a).collect();
    let instance = f.made.id.clone();
    let select = Select::new("instance.settings.account", tr("Spielen als"), labels, at).width(control::VERSION_MENU).on_choose(move |i| {
        let account = i.checked_sub(1).and_then(|n| ids.get(n).copied());
        store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account });
    });
    let reset_id = f.made.id.clone();
    let clear = own.map(|_| reset("account", tr("Spielen als"), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: reset_id.clone(), account: None })));
    Some(setting(t, tr("Spielen als"), clear, select.into_element(), Some(tr("Gilt, wenn du diese Instanz allein spielst. Batches haben eigene Accounts.")), None))
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
        let mut col = rect().width(Size::fill()).spacing(space::XS).child(label_row(&t, crate::ui::loader_version::label(&made.loader), None)).child(select);
        match &*versions.read() {
            Load::Loading => col = col.child(caption(&t, tr("Versionen werden geladen …"))),
            Load::Failed(e) => {
                col = col.child(rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(line(&t, e.clone(), font::CAPTION, t.danger_text)).child(Button::new("instance.settings.loader_versions.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).plain().on_press(move |_| {
                    let next = asked.peek().wrapping_add(1);
                    asked.set(next);
                })));
            }
            Load::Ready(_) => {}
        }
        col
    }
}
