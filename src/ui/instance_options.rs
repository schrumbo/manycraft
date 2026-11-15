//! The advanced part of an instance's settings: memory, Java, arguments, window size, graphics card and renderer.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::IndexPath;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::{Change, InstancePage, Job};
use crate::core::drive;
use crate::core::i18n::{tr, trf};
use crate::core::instance::{DEFAULT_MEMORY_MB, Instance};
use crate::core::javas::{Java, mismatch};
use crate::core::options::{self, Gpu, LaunchOptions, Renderer, WindowSize};
use crate::ui::drive::Mark;
use crate::ui::remote;
use crate::ui::sheet::{self, Look};
use crate::ui::theme::{Theme, control, font, space};
use crate::ui::widgets::{TakeFocus, segment, segmented};

/// Settings a palette entry can open.
pub(super) const SETTINGS: [&str; 7] = ["memory", "java", "jvm_args", "game_args", "window", "gpu", "renderer"];
const REVEAL_FRAMES: u8 = 3;

/// One change of the start options, applied onto the instance as it is when the save runs.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum OptionChange {
    Java(Option<PathBuf>),
    GameArgs(Vec<String>),
    Window(Option<WindowSize>),
    Gpu(Option<Gpu>),
    Renderer(Option<Renderer>),
}

impl OptionChange {
    pub(super) fn apply(&self, options: &mut LaunchOptions) {
        match self {
            Self::Java(java) => options.java.clone_from(java),
            Self::GameArgs(args) => options.game_args.clone_from(args),
            Self::Window(size) => options.window = *size,
            Self::Gpu(gpu) => options.gpu = *gpu,
            Self::Renderer(renderer) => options.renderer = *renderer,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum JavaChoice {
    Auto,
    Path(PathBuf),
    Pick,
}

#[derive(Clone)]
struct JavaItem {
    choice: JavaChoice,
    title: String,
    line: String,
}

impl SearchableListItem for JavaItem {
    type Value = JavaChoice;

    fn title(&self) -> SharedString {
        SharedString::from(self.title.clone())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = Theme::get(cx);
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(div().truncate().child(self.title.clone()))
            .when(!self.line.is_empty(), |d| d.child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(self.line.clone())))
    }

    fn value(&self) -> &Self::Value {
        &self.choice
    }
}

fn java_title(java: &Java) -> String {
    match &java.vendor {
        Some(vendor) => trf("Java %@ · %@", &[&java.major, vendor]),
        None => trf("Java %@", &[&java.major]),
    }
}

/// What the advanced part shows and edits besides the inputs the page already owns.
pub(super) struct Options {
    java_select: Entity<SelectState<SearchableVec<JavaItem>>>,
    javas: Option<Vec<Java>>,
    detecting: bool,
    probe: Option<(PathBuf, String, Result<Java, String>, Option<u32>)>,
    java_problem: Option<String>,
    picking: bool,
    game_args: Entity<InputState>,
    width: Entity<InputState>,
    height: Entity<InputState>,
    pub(super) jvm_problem: Option<String>,
    game_problem: Option<String>,
    window_problem: Option<String>,
    synced: Option<(String, LaunchOptions)>,
    java_items: Option<(Option<PathBuf>, usize)>,
    gpu_focus: FocusHandle,
    renderer_focus: FocusHandle,
    pub(super) memory_focus: FocusHandle,
    reveal: Option<&'static str>,
    revealed: Rc<Cell<Option<Bounds<Pixels>>>>,
    waited: u8,
}

impl Options {
    pub(super) fn new(window: &mut Window, cx: &mut Context<InstancePage>) -> (Self, Vec<Subscription>) {
        let java_select = cx.new(|cx| SelectState::new(SearchableVec::new(Vec::<JavaItem>::new()), None, window, cx));
        let game_args = cx.new(|cx| InputState::new(window, cx));
        let width = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Breite")));
        let height = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Höhe")));
        let committed = |s: &mut InstancePage, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<InstancePage>| {
            if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                s.commit_options(cx);
            }
        };
        let subs = vec![
            cx.subscribe(&game_args, committed),
            cx.subscribe(&width, committed),
            cx.subscribe(&height, committed),
            cx.subscribe_in(&java_select, window, |s: &mut InstancePage, _, e: &SelectEvent<SearchableVec<JavaItem>>, window, cx| {
                let SelectEvent::Confirm(Some(choice)) = e else { return };
                s.pick_java(choice.clone(), window, cx);
            }),
        ];
        let options = Self {
            java_select,
            javas: None,
            detecting: false,
            probe: None,
            java_problem: None,
            picking: false,
            game_args,
            width,
            height,
            jvm_problem: None,
            game_problem: None,
            window_problem: None,
            synced: None,
            java_items: None,
            gpu_focus: cx.focus_handle().tab_stop(true),
            renderer_focus: cx.focus_handle().tab_stop(true),
            memory_focus: cx.focus_handle(),
            reveal: None,
            revealed: Rc::new(Cell::new(None)),
            waited: 0,
        };
        (options, subs)
    }

    /// Forgets what belonged to the instance shown before.
    pub(super) fn rebind(&mut self) {
        self.jvm_problem = None;
        self.game_problem = None;
        self.window_problem = None;
        self.java_problem = None;
        self.synced = None;
        self.java_items = None;
        self.reveal = None;
    }

    pub(super) fn revealing(&self) -> bool {
        self.reveal.is_some()
    }

    /// Opens `setting` once the advanced part shows.
    pub(super) fn reveal(&mut self, setting: &str) {
        self.reveal = SETTINGS.into_iter().find(|s| *s == setting);
        self.revealed.set(None);
        self.waited = 0;
    }
}

impl InstancePage {
    fn options_now(&self, made: &Instance) -> LaunchOptions {
        let mut now = made.options.clone();
        for (_, job) in self.in_flight.iter().chain(self.queue.iter()).filter(|(id, _)| *id == self.id) {
            if let Job::Save(Change::Option(change)) = job {
                change.apply(&mut now);
            }
        }
        now
    }

    fn change_option(&mut self, change: OptionChange, cx: &mut Context<Self>) {
        self.enqueue(Job::Save(Change::Option(change)), cx);
    }

    pub(super) fn sync_options(&mut self, made: &Instance, window: &mut Window, cx: &mut Context<Self>) {
        if self.advanced && self.opts.javas.is_none() && !self.opts.detecting {
            self.detect_javas(cx);
        }
        let options = &made.options;
        let current = self.opts.synced.as_ref().is_some_and(|(id, saved)| *id == made.id && saved.game_args == options.game_args && saved.window == options.window);
        if !current {
            let (args, width, height) = (options.game_args.join(" "), options.window.map(|w| w.width.to_string()).unwrap_or_default(), options.window.map(|w| w.height.to_string()).unwrap_or_default());
            self.opts.synced = Some((made.id.clone(), options.clone()));
            self.opts.game_args.update(cx, |s, cx| s.set_value(args, window, cx));
            self.opts.width.update(cx, |s, cx| s.set_value(width, window, cx));
            self.opts.height.update(cx, |s, cx| s.set_value(height, window, cx));
        }
        let java = made.options.java.clone();
        let items_key = (java.clone(), self.opts.javas.as_ref().map_or(0, Vec::len));
        if self.opts.java_items.as_ref() != Some(&items_key) {
            self.opts.java_items = Some(items_key);
            let mut items = vec![JavaItem { choice: JavaChoice::Auto, title: tr("Automatisch (empfohlen)"), line: String::new() }];
            for found in self.opts.javas.iter().flatten() {
                items.push(JavaItem { choice: JavaChoice::Path(found.path.clone()), title: java_title(found), line: found.path.display().to_string() });
            }
            if let Some(path) = java.as_ref().filter(|p| !items.iter().any(|i| i.choice == JavaChoice::Path((*p).clone()))) {
                let title = match &self.opts.probe {
                    Some((probed, _, Ok(found), _)) if probed == path => java_title(found),
                    _ => tr("Eigenes Java"),
                };
                items.push(JavaItem { choice: JavaChoice::Path(path.clone()), title, line: path.display().to_string() });
            }
            items.push(JavaItem { choice: JavaChoice::Pick, title: tr("Datei wählen…"), line: String::new() });
            let want = java.map_or(JavaChoice::Auto, JavaChoice::Path);
            let at = items.iter().position(|i| i.choice == want);
            self.opts.java_select.update(cx, |s, cx| {
                s.set_items(SearchableVec::new(items), window, cx);
                s.set_selected_index(at.map(IndexPath::new), window, cx);
            });
        }
        let probe_key = made.options.java.clone().map(|p| (p, made.game_version.clone()));
        let probed = self.opts.probe.as_ref().map(|(p, v, ..)| (p.clone(), v.clone()));
        if let Some((path, version)) = probe_key.filter(|k| probed.as_ref() != Some(k)) {
            self.probe_java(path, version, cx);
        }
    }

    fn detect_javas(&mut self, cx: &mut Context<Self>) {
        self.opts.detecting = true;
        let task = cx.background_executor().spawn(async { remote::detect_javas() });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                s.opts.detecting = false;
                s.opts.javas = Some(found);
                cx.notify();
            });
        })
        .detach();
    }

    fn probe_java(&mut self, path: PathBuf, version: String, cx: &mut Context<Self>) {
        self.opts.probe = Some((path.clone(), version.clone(), Err(String::new()), None));
        let task = cx.background_executor().spawn({
            let (path, version) = (path.clone(), version.clone());
            async move { (remote::probe_java(&path).map_err(|e| format!("{e:#}")), remote::expected_java(&version)) }
        });
        cx.spawn(async move |this, cx| {
            let (found, expected) = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.opts.probe.as_ref().is_some_and(|(p, v, ..)| *p == path && *v == version) {
                    s.opts.probe = Some((path, version, found, expected));
                    s.opts.java_items = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pick_java(&mut self, choice: JavaChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.opts.java_problem = None;
        match choice {
            JavaChoice::Auto => self.change_option(OptionChange::Java(None), cx),
            JavaChoice::Path(path) => self.change_option(OptionChange::Java(Some(path)), cx),
            JavaChoice::Pick => {
                self.opts.java_items = None;
                self.choose_java_file(window, cx);
            }
        }
        cx.notify();
    }

    fn choose_java_file(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.opts.picking {
            return;
        }
        self.opts.picking = true;
        let prompt = (!drive::on()).then(|| cx.prompt_for_paths(PathPromptOptions { files: true, directories: true, multiple: false, prompt: Some(tr("Java wählen").into()) }));
        let id = self.id.clone();
        cx.spawn(async move |this, cx| {
            let paths: Option<Vec<PathBuf>> = match prompt {
                Some(prompt) => prompt.await.ok().and_then(Result::ok).flatten(),
                None => drive::answer("java file"),
            };
            let Some(path) = paths.and_then(|p| p.into_iter().next()) else {
                let _ = this.update(cx, |s, cx| {
                    s.opts.picking = false;
                    cx.notify();
                });
                return;
            };
            let found = cx.background_executor().spawn(async move { remote::probe_java(&path) }).await;
            let _ = this.update(cx, |s, cx| {
                s.opts.picking = false;
                if s.id != id {
                    return;
                }
                match found {
                    Ok(java) => s.change_option(OptionChange::Java(Some(java.path)), cx),
                    Err(e) => s.opts.java_problem = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn commit_options(&mut self, cx: &mut Context<Self>) {
        let Some((id, saved)) = self.opts.synced.clone() else { return };
        if id != self.id {
            return;
        }
        let args = super::split_args(&self.opts.game_args.read(cx).value());
        self.opts.game_problem = options::check_game_args(&args).err();
        if self.opts.game_problem.is_none() && args != saved.game_args {
            self.change_option(OptionChange::GameArgs(args), cx);
        }
        match options::parse_window(&self.opts.width.read(cx).value(), &self.opts.height.read(cx).value()) {
            Ok(size) => {
                self.opts.window_problem = None;
                if size != saved.window {
                    self.change_option(OptionChange::Window(size), cx);
                }
            }
            Err(why) => self.opts.window_problem = Some(why),
        }
        cx.notify();
    }

    fn java_note(&self, made: &Instance, java: Option<&PathBuf>) -> (Option<String>, Option<String>) {
        if let Some(problem) = &self.opts.java_problem {
            return (None, Some(problem.clone()));
        }
        let Some(path) = java else { return (Some(tr("Manycraft lädt das passende Java für diese Version.")), None) };
        match &self.opts.probe {
            Some((probed, version, Ok(found), expected)) if probed == path && *version == made.game_version => {
                let problem = expected.and_then(|needed| mismatch(needed, found.major, &made.game_version));
                (Some(path.display().to_string()), problem)
            }
            Some((probed, _, Err(e), _)) if probed == path && !e.is_empty() => (None, Some(e.clone())),
            Some((probed, _, Err(_), _)) if probed == path => (Some(tr("Java wird geprüft …")), None),
            _ => (Some(path.display().to_string()), None),
        }
    }

    fn reset_button(&self, id: &'static str, label: String, cx: &mut Context<Self>, act: impl Fn(&mut Self, &mut Context<Self>) + 'static, t: &Theme) -> Stateful<Div> {
        sheet::button(id, tr("Standard"), None, Look::Plain, false, false, t).mark("reset", label).on_click(cx.listener(move |s, _, _, cx| act(s, cx)))
    }

    fn setting(&self, key: &'static str, label: String, reset: Option<Stateful<Div>>, control: impl IntoElement, hint: Option<String>, problem: Option<String>, t: &Theme) -> Div {
        let head = div().flex().items_center().gap(px(space::S)).min_h(px(control::S)).child(sheet::label(label, t)).children(reset);
        let probe = (self.opts.reveal == Some(key)).then(|| {
            let cell = self.opts.revealed.clone();
            canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {}).absolute().size_full()
        });
        div().relative().flex().flex_col().gap(px(space::XS)).child(head).child(control).child(sheet::note(hint, problem, t)).children(probe)
    }

    fn choice_row<T: Copy + PartialEq + 'static>(&self, id: &'static str, focus: &FocusHandle, now: Option<T>, choices: &[(Option<T>, String)], cx: &mut Context<Self>, wrap: fn(Option<T>) -> OptionChange, t: &Theme) -> Div {
        let segments = choices.iter().enumerate().map(|(n, (value, label))| {
            let selected = *value == now;
            segment((id, n), label.clone(), selected, true, t).when(selected, |d| d.track_focus(focus))
        });
        let values: Vec<Option<T>> = choices.iter().map(|(v, _)| *v).collect();
        let pick = cx.listener(move |s, n: &usize, _, cx| {
            if let Some(value) = values.get(*n) {
                s.change_option(wrap(*value), cx);
            }
        });
        div().flex().child(segmented(id, segments, move |n, window, cx| pick(&n, window, cx), t))
    }

    /// The settings inside "Erweitert".
    pub(super) fn render_options(&mut self, made: &Instance, memory: Div, cx: &mut Context<Self>, t: &Theme) -> Div {
        let now = self.options_now(made);
        let memory_reset = (self.pending_memory().unwrap_or(made.memory_mb) != DEFAULT_MEMORY_MB).then(|| self.reset_button("memory-reset", tr("Arbeitsspeicher"), cx, |s, cx| s.enqueue(Job::Save(Change::Memory(DEFAULT_MEMORY_MB)), cx), t));
        let memory = self.setting("memory", tr("Arbeitsspeicher"), memory_reset, memory, Some(tr("Pro Account.")), None, t);

        let (java_hint, java_problem) = self.java_note(made, now.java.as_ref());
        let java_reset = now.java.is_some().then(|| {
            self.reset_button("java-reset", tr("Java"), cx, |s, cx| {
                s.opts.java_problem = None;
                s.change_option(OptionChange::Java(None), cx);
            }, t)
        });
        let java_control = div().w(px(control::JAVA_MENU)).max_w_full().child(Select::new(&self.opts.java_select).menu_width(px(control::JAVA_MENU)).menu_max_h(px(sheet::LIST_MAX)).disabled(self.opts.picking));
        let java_hint = if self.opts.detecting && now.java.is_none() { Some(tr("Suche installierte Java-Versionen …")) } else { java_hint };
        let java = self.setting("java", tr("Java"), java_reset, java_control, java_hint, java_problem, t);

        let jvm_reset = (!made.jvm_args.is_empty()).then(|| {
            self.reset_button("jvm-reset", tr("Java-Argumente"), cx, |s, cx| {
                s.opts.jvm_problem = None;
                s.enqueue(Job::Save(Change::JvmArgs(Vec::new())), cx);
            }, t)
        });
        let jvm = self.setting("jvm_args", tr("Java-Argumente"), jvm_reset, Input::new(&self.jvm), Some(tr("Zum Beispiel -XX:+UseG1GC, getrennt durch Leerzeichen.")), self.opts.jvm_problem.clone(), t);

        let game_reset = (!now.game_args.is_empty()).then(|| {
            self.reset_button("game-args-reset", tr("Spielargumente"), cx, |s, cx| {
                s.opts.game_problem = None;
                s.change_option(OptionChange::GameArgs(Vec::new()), cx);
            }, t)
        });
        let game = self.setting("game_args", tr("Spielargumente"), game_reset, Input::new(&self.opts.game_args), Some(tr("Gehen beim Start an Minecraft, zum Beispiel --fullscreen.")), self.opts.game_problem.clone(), t);

        let window_reset = now.window.is_some().then(|| {
            self.reset_button("window-reset", tr("Fenstergröße beim Start"), cx, |s, cx| {
                s.opts.window_problem = None;
                s.change_option(OptionChange::Window(None), cx);
            }, t)
        });
        let size = div()
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(div().w(px(control::NUMBER)).child(Input::new(&self.opts.width)))
            .child(div().text_color(t.tertiary).child("×"))
            .child(div().w(px(control::NUMBER)).child(Input::new(&self.opts.height)));
        let window_size = self.setting("window", tr("Fenstergröße beim Start"), window_reset, size, Some(tr("Leer lassen für die Größe, die Minecraft wählt.")), self.opts.window_problem.clone(), t);

        let mut col = div().flex().flex_col().gap(px(space::M)).child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Änderungen wirken beim nächsten Start."))).child(memory).child(java).child(jvm).child(game).child(window_size);
        if cfg!(windows) || now.gpu.is_some() {
            let choices = [(None, tr("Automatisch")), (Some(Gpu::HighPerformance), tr("Hohe Leistung")), (Some(Gpu::PowerSaving), tr("Energie sparen"))];
            let row = self.choice_row("instance-gpu", &self.opts.gpu_focus.clone(), now.gpu, &choices, cx, OptionChange::Gpu, t);
            col = col.child(self.setting("gpu", tr("Grafikkarte"), None, row, Some(tr("Gilt für alle Instanzen, die dasselbe Java nutzen.")), None, t));
        }
        if options::renderer_choice(&made.game_version) {
            let choices = [(None, tr("Automatisch")), (Some(Renderer::OpenGl), "OpenGL".to_owned()), (Some(Renderer::Vulkan), "Vulkan".to_owned())];
            let row = self.choice_row("instance-renderer", &self.opts.renderer_focus.clone(), now.renderer, &choices, cx, OptionChange::Renderer, t);
            col = col.child(self.setting("renderer", tr("Grafikschnittstelle"), None, row, Some(tr("Automatisch nimmt, was im Spiel eingestellt ist.")), None, t));
        }
        if made.loader.takes_mods() {
            let label = crate::ui::loader_version::label(&made.loader);
            col = col.child(div().flex().flex_col().gap(px(space::XS)).child(sheet::label(label, t)).child(self.loader_version.clone()));
        }
        col
    }

    /// Scrolls the setting a palette entry asked for into view and focuses it, once it was laid out.
    pub(super) fn finish_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.opts.reveal else { return };
        let Some(bounds) = self.opts.revealed.get() else {
            self.opts.waited += 1;
            if self.opts.waited > REVEAL_FRAMES {
                self.opts.reveal = None;
            }
            cx.notify();
            return;
        };
        self.opts.reveal = None;
        let viewport = self.scroll.bounds();
        let offset = self.scroll.offset();
        let top = bounds.origin.y - viewport.origin.y - offset.y;
        let max = self.scroll.max_offset().y.max(px(0.));
        let y = (top - px(space::L)).max(px(0.)).min(max);
        self.scroll.set_offset(point(offset.x, -y));
        match key {
            "java" => window.focus(&self.opts.java_select.focus_handle(cx), cx),
            "jvm_args" => self.jvm.update(cx, |s, cx| s.take_focus(window, cx)),
            "game_args" => self.opts.game_args.update(cx, |s, cx| s.take_focus(window, cx)),
            "window" => self.opts.width.update(cx, |s, cx| s.take_focus(window, cx)),
            "gpu" => window.focus(&self.opts.gpu_focus, cx),
            "renderer" => window.focus(&self.opts.renderer_focus, cx),
            _ => window.focus(&self.opts.memory_focus, cx),
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Gpu, LaunchOptions, OptionChange, Renderer, SETTINGS, WindowSize};

    #[test]
    fn option_changes_touch_only_their_field() {
        let mut options = LaunchOptions { game_args: vec!["--demo".into()], ..LaunchOptions::default() };
        OptionChange::Gpu(Some(Gpu::PowerSaving)).apply(&mut options);
        OptionChange::Renderer(Some(Renderer::Vulkan)).apply(&mut options);
        OptionChange::Window(Some(WindowSize { width: 800, height: 600 })).apply(&mut options);
        assert_eq!(options.game_args, ["--demo"]);
        assert_eq!((options.gpu, options.renderer), (Some(Gpu::PowerSaving), Some(Renderer::Vulkan)));
        OptionChange::GameArgs(Vec::new()).apply(&mut options);
        OptionChange::Java(Some(PathBuf::from("java"))).apply(&mut options);
        assert!(options.game_args.is_empty());
        assert_eq!(options.window, Some(WindowSize { width: 800, height: 600 }));
        assert_eq!(options.java, Some(PathBuf::from("java")));
    }

    #[test]
    fn every_palette_setting_is_known() {
        assert!(SETTINGS.contains(&"java") && SETTINGS.contains(&"renderer"));
    }
}
