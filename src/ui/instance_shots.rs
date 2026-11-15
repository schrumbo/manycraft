//! The Screenshots tab of the instance page: every screenshot in the instance's game folders, newest first.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::menu::{ContextMenuExt, PopupMenu};
use gpui_kit::component::{FocusableExt, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::drive::Mark;
use super::forms::{self, Problem};
use super::instance_files::{self as files, CopyShot, DeleteShot, FileUndo, OpenShot, RevealShot};
use super::motion::Soft;
use super::palette::{Entry, Group, hint};
use super::sheet;
use super::theme::{self, Theme, font, radius, space, tile, tint};
use super::widgets::{icon, menu_of};
use super::{model, undo};
use crate::core::i18n::{tr, trf};
use crate::core::shots::{self, Shot};

/// Key context of the screenshot grid.
pub const CONTEXT: &str = "InstanceShots";

struct Shown(Entity<Shots>);

impl Global for Shown {}

enum Outcome {
    Done(String),
    Failed(Problem),
}

#[derive(Default)]
struct Thumbs {
    kept: HashMap<PathBuf, (Arc<RenderImage>, u64)>,
    failed: HashSet<PathBuf>,
    running: HashSet<PathBuf>,
    wanted: Vec<PathBuf>,
    clock: u64,
}

struct Shots {
    id: String,
    dir: Option<PathBuf>,
    loaded: bool,
    shots: Vec<Shot>,
    selected: Option<PathBuf>,
    thumbs: Thumbs,
    width: Rc<Cell<f32>>,
    outcome: Option<Outcome>,
    details: bool,
    scan: u64,
    stale: bool,
    playing: Vec<Uuid>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _model: Subscription,
}

/// Registers the screenshot commands.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &OpenShot, cx| files::open(PathBuf::from(&a.0), cx));
    cx.on_action(|a: &RevealShot, cx| files::reveal(PathBuf::from(&a.0), cx));
    cx.on_action(|a: &CopyShot, cx| {
        let path = PathBuf::from(&a.0);
        with(cx, move |s, cx| s.copy(path, cx));
    });
    cx.on_action(|a: &DeleteShot, cx| {
        let path = PathBuf::from(&a.0);
        with(cx, move |s, cx| s.delete(path, cx));
    });
}

fn with(cx: &mut App, f: impl FnOnce(&mut Shots, &mut Context<Shots>)) {
    if let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) {
        view.update(cx, f);
    }
}

/// The Screenshots tab of instance `id`; `fresh` reads the folders again.
pub fn view(id: &str, fresh: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let view = match cx.try_global::<Shown>() {
        Some(shown) => shown.0.clone(),
        None => {
            let view = cx.new(|cx| Shots::new(id, cx));
            cx.set_global(Shown(view.clone()));
            view
        }
    };
    view.update(cx, |s, cx| {
        if s.id != id {
            s.bind(id, cx);
        } else if fresh {
            s.rescan(cx);
        }
    });
    if fresh && !window.has_active_dialog(cx) {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    }
    view.into_any_element()
}

/// Reads the screenshots of instance `id` again if they are shown, and selects `back` when it came back.
pub fn changed(id: &str, back: Option<PathBuf>, cx: &mut App) {
    with(cx, |s, cx| {
        if s.id == id {
            if let Some(back) = back.filter(|b| b.parent().and_then(Path::file_name).is_some_and(|n| n == "screenshots")) {
                s.selected = Some(back);
            }
            s.rescan(cx);
        }
    });
}

/// Palette entries for the selected screenshot of instance `id`.
pub fn palette(id: &str, cx: &App) -> Vec<Entry> {
    let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) else { return Vec::new() };
    let s = view.read(cx);
    let Some(shot) = s.current().filter(|_| s.id == id) else { return Vec::new() };
    let path = shot.path.display().to_string();
    let name = shot.path.file_name().map(|n| n.to_string_lossy().into_owned());
    vec![
        files::entry(Group::Selection, "external-link", tr("Screenshot öffnen"), name.clone(), Box::new(OpenShot(path.clone())), Some(&files::Open), cx),
        files::entry(Group::Selection, "copy", tr("Bild kopieren"), name.clone(), Box::new(CopyShot(path.clone())), Some(&files::Copy), cx),
        files::entry(Group::Selection, "folder-open", tr("Screenshot im Ordner zeigen"), name.clone(), Box::new(RevealShot(path.clone())), None, cx),
        files::entry(Group::Selection, "trash", tr("Screenshot löschen"), name, Box::new(DeleteShot(path)), Some(&files::Delete), cx),
    ]
}

impl Shots {
    fn new(id: &str, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&model::get(cx), |s: &mut Self, _, cx| {
            let now = s.active(cx);
            if now != s.playing {
                if s.playing.iter().any(|a| !now.contains(a)) {
                    s.stale = true;
                    cx.notify();
                }
                s.playing = now;
            }
        });
        let mut page = Self {
            id: String::new(),
            dir: None,
            loaded: false,
            shots: Vec::new(),
            selected: None,
            thumbs: Thumbs::default(),
            width: Rc::new(Cell::new(0.)),
            outcome: None,
            details: false,
            scan: 0,
            stale: false,
            playing: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _model: subscription,
        };
        page.bind(id, cx);
        page
    }

    fn active(&self, cx: &App) -> Vec<Uuid> {
        let mut active: Vec<Uuid> = model::get(cx).read(cx).games.values().filter(|g| g.instance_id == self.id && g.phase.is_active()).map(|g| g.account_id).collect();
        active.sort_unstable();
        active
    }

    fn bind(&mut self, id: &str, cx: &mut Context<Self>) {
        self.id = id.to_owned();
        self.loaded = false;
        self.shots.clear();
        self.selected = None;
        self.outcome = None;
        self.playing = self.active(cx);
        self.scroll = UniformListScrollHandle::new();
        for (_, (image, _)) in self.thumbs.kept.drain() {
            cx.drop_image(image, None);
        }
        self.thumbs.failed.clear();
        self.thumbs.wanted.clear();
        self.rescan(cx);
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.dir = files::instance_dir(&self.id, cx);
        let Some(dir) = self.dir.clone() else {
            self.loaded = true;
            cx.notify();
            return;
        };
        self.scan += 1;
        let scan = self.scan;
        let task = cx.background_spawn(async move { shots::list(&dir) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.scan != scan {
                    return;
                }
                s.thumbs.failed.clear();
                let present: HashSet<&Path> = found.iter().map(|f| f.path.as_path()).collect();
                let gone: Vec<PathBuf> = s.thumbs.kept.keys().filter(|k| !present.contains(k.as_path())).cloned().collect();
                for path in gone {
                    if let Some((image, _)) = s.thumbs.kept.remove(&path) {
                        cx.drop_image(image, None);
                    }
                }
                s.shots = found;
                s.loaded = true;
                if s.selected.as_ref().is_none_or(|k| !s.shots.iter().any(|f| &f.path == k)) {
                    s.selected = s.shots.first().map(|f| f.path.clone());
                }
                if let Some(at) = s.at() {
                    let columns = s.columns();
                    s.scroll.scroll_to_item(at / columns, ScrollStrategy::Nearest);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn columns(&self) -> usize {
        let width = self.width.get();
        ((width + space::S) / (theme::files::SHOT_MIN + space::S)).floor().max(1.) as usize
    }

    fn at(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.shots.iter().position(|s| &s.path == selected)
    }

    fn current(&self) -> Option<&Shot> {
        self.at().and_then(|at| self.shots.get(at))
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.shots.is_empty() {
            return;
        }
        let at = self.at().map_or(0, |at| at.saturating_add_signed(by).min(self.shots.len() - 1));
        self.select(at, cx);
    }

    fn page(&self) -> isize {
        let state = self.scroll.0.borrow();
        let (height, row) = (state.base_handle.bounds().size.height, state.last_item_size.as_ref().map_or(px(0.), |size| size.item.height));
        let rows = if row > px(0.) { ((height / row).floor() as isize).max(1) } else { 1 };
        rows * self.columns() as isize
    }

    fn select(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(shot) = self.shots.get(at) else { return };
        self.selected = Some(shot.path.clone());
        let columns = self.columns();
        self.scroll.scroll_to_item(at / columns, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn dispatch_selected(&self, what: fn(String) -> Box<dyn Action>, window: &mut Window, cx: &mut App) {
        if let Some(shot) = self.current() {
            window.dispatch_action(what(shot.path.display().to_string()), cx);
        }
    }

    fn want(&mut self, visible: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.thumbs.clock += 1;
        let clock = self.thumbs.clock;
        let mut wanted = Vec::new();
        for path in visible {
            match self.thumbs.kept.get_mut(&path) {
                Some(kept) => kept.1 = clock,
                None if !self.thumbs.failed.contains(&path) && !self.thumbs.running.contains(&path) => wanted.push(path),
                None => {}
            }
        }
        self.thumbs.wanted = wanted;
        self.decode(cx);
    }

    fn decode(&mut self, cx: &mut Context<Self>) {
        while self.thumbs.running.len() < theme::files::DECODERS {
            let Some(path) = (!self.thumbs.wanted.is_empty()).then(|| self.thumbs.wanted.remove(0)) else { return };
            self.thumbs.running.insert(path.clone());
            let source = path.clone();
            let task = cx.background_spawn(async move { shots::thumbnail(&source, theme::files::THUMB_PX, theme::files::THUMB_PX).map(|p| (p.width(), p.height(), p.into_raw())) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |s, cx| {
                    s.thumbs.running.remove(&path);
                    match result.ok().and_then(|(w, h, rgba)| super::state::render_image(w, h, rgba)) {
                        Some(image) if s.shots.iter().any(|f| f.path == path) => {
                            s.thumbs.kept.insert(path, (image, s.thumbs.clock));
                            s.evict(cx);
                        }
                        Some(image) => cx.drop_image(image, None),
                        None => {
                            s.thumbs.failed.insert(path);
                        }
                    }
                    s.decode(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn evict(&mut self, cx: &mut Context<Self>) {
        while self.thumbs.kept.len() > theme::files::THUMBS_KEPT {
            let Some(oldest) = self.thumbs.kept.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { return };
            if let Some((image, _)) = self.thumbs.kept.remove(&oldest) {
                cx.drop_image(image, None);
            }
        }
    }

    fn copy(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let task = cx.background_spawn({
            let path = path.clone();
            async move { shots::png(&path) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.details = false;
                s.outcome = Some(match result {
                    Ok(bytes) => {
                        if !crate::core::drive::fenced("clipboard image", path.display()) {
                            cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes)));
                        }
                        Outcome::Done(tr("Bild kopiert."))
                    }
                    Err(e) => Outcome::Failed(forms::explain_as(tr("Das Bild wurde nicht kopiert."), &e)),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn delete(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else { return };
        if !self.shots.iter().any(|f| f.path == path) {
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if self.selected.as_ref() == Some(&path)
            && let Some(at) = self.at()
        {
            let next = self.shots.get(at + 1).or_else(|| at.checked_sub(1).and_then(|p| self.shots.get(p)));
            self.selected = next.map(|f| f.path.clone());
        }
        self.shots.retain(|f| f.path != path);
        self.outcome = None;
        cx.notify();
        let id = self.id.clone();
        let task = cx.background_spawn(async move { crate::core::trash::put(&dir, &path) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                match result {
                    Ok(entry) => undo::offer(undo::Kind::Files { name: name.clone(), undo: FileUndo::Deleted { instance: id.clone(), entry } }, cx),
                    Err(e) => {
                        s.details = false;
                        s.outcome = Some(Outcome::Failed(forms::explain_as(trf("„%@“ wurde nicht gelöscht.", &[&name]), &e)));
                    }
                }
                s.rescan(cx);
            });
        })
        .detach();
    }

    fn cell(&self, at: usize, shot: &Shot, width: f32, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let selected = self.selected.as_ref() == Some(&shot.path);
        let key = format!("shot-{at}");
        let height = width / tile::ASPECT;
        let picture = match self.thumbs.kept.get(&shot.path) {
            Some((image, _)) => img(ImageSource::Render(image.clone())).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            None if self.thumbs.failed.contains(&shot.path) => icon("image-off", theme::iconsize::L, t.tertiary).into_any_element(),
            None => div().into_any_element(),
        };
        let caption = format!("{} · {}", files::account_name(shot.account, cx), files::when(shot.taken));
        let name = shot.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let path = shot.path.display().to_string();
        let menu_key = key.clone();
        div()
            .id(SharedString::from(key))
            .mark("cell", name)
            .w(px(width))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .rounded(px(radius::CONTROL))
            .cursor_pointer()
            .when(!selected, |d| d.soft(t.ink.opacity(tint::HOVER), radius::CONTROL))
            .on_mouse_down(MouseButton::Left, cx.listener(move |s, _, window, cx| {
                window.focus(&s.focus, cx);
                s.select(at, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |s, _, _, cx| s.select(at, cx)))
            .on_click(cx.listener(|s, e: &ClickEvent, window, cx| {
                if e.click_count() == 2 {
                    s.dispatch_selected(|p| Box::new(OpenShot(p)), window, cx);
                }
            }))
            .child(
                div()
                    .id(SharedString::from(format!("{menu_key}-picture")))
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(radius::CONTROL))
                    .overflow_hidden()
                    .bg(t.well)
                    .flex()
                    .items_center()
                    .justify_center()
                    .border(px(tile::RING))
                    .border_color(if selected { t.accent } else { gpui_kit::transparent_black() })
                    .child(picture),
            )
            .child(div().h(px(theme::files::SHOT_CAPTION - space::XS)).truncate().text_size(px(font::CAPTION)).text_color(if selected { t.primary } else { t.secondary }).child(caption))
            .context_menu(move |menu, window, cx| menu_of(&menu_key, shot_menu(menu, &path), window, cx))
            .into_any_element()
    }

    fn toolbar(&self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let count = if self.shots.len() == 1 { tr("1 Screenshot") } else { trf("%lld Screenshots", &[&self.shots.len()]) };
        let path = self.current().map(|s| s.path.display().to_string());
        let button = |id: &'static str, symbol: &str, label: String, color: Hsla, make: fn(String) -> Box<dyn Action>| {
            let path = path.clone();
            sheet::icon_button(id, symbol, label, color, t).when_some(path, |b, path| b.on_click(move |_, window, cx| window.dispatch_action(make(path.clone()), cx)))
        };
        let actions = path.is_some().then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(space::XS))
                .child(button("shots-open", "external-link", hint(tr("Öffnen"), &files::Open, cx), t.secondary, |p| Box::new(OpenShot(p))))
                .child(button("shots-copy", "copy", hint(tr("Bild kopieren"), &files::Copy, cx), t.secondary, |p| Box::new(CopyShot(p))))
                .child(button("shots-reveal", "folder-open", tr("Im Ordner zeigen"), t.secondary, |p| Box::new(RevealShot(p))))
                .child(button("shots-delete", "trash", hint(tr("Löschen"), &files::Delete, cx), t.danger_text, |p| Box::new(DeleteShot(p))))
        });
        div().flex_none().flex().items_center().justify_between().gap(px(space::S)).child(div().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(count)).children(actions)
    }
}

fn shot_menu(menu: PopupMenu, path: &str) -> PopupMenu {
    menu.menu(tr("Öffnen"), Box::new(OpenShot(path.to_owned())))
        .menu(tr("Im Ordner zeigen"), Box::new(RevealShot(path.to_owned())))
        .menu(tr("Bild kopieren"), Box::new(CopyShot(path.to_owned())))
        .separator()
        .menu(tr("Löschen"), Box::new(DeleteShot(path.to_owned())))
}

impl Render for Shots {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        if std::mem::take(&mut self.stale) {
            self.rescan(cx);
        }
        let root = div()
            .id("shots")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .on_action(cx.listener(|s, _: &files::Left, _, cx| s.step(-1, cx)))
            .on_action(cx.listener(|s, _: &files::Right, _, cx| s.step(1, cx)))
            .on_action(cx.listener(|s, _: &files::Up, _, cx| s.step(-(s.columns() as isize), cx)))
            .on_action(cx.listener(|s, _: &files::Down, _, cx| s.step(s.columns() as isize, cx)))
            .on_action(cx.listener(|s, _: &files::First, _, cx| s.select(0, cx)))
            .on_action(cx.listener(|s, _: &files::Last, _, cx| s.select(s.shots.len().saturating_sub(1), cx)))
            .on_action(cx.listener(|s, _: &files::PageUp, _, cx| s.step(-s.page(), cx)))
            .on_action(cx.listener(|s, _: &files::PageDown, _, cx| s.step(s.page(), cx)))
            .on_action(cx.listener(|s, _: &files::Open, window, cx| s.dispatch_selected(|p| Box::new(OpenShot(p)), window, cx)))
            .on_action(cx.listener(|s, _: &files::Copy, window, cx| s.dispatch_selected(|p| Box::new(CopyShot(p)), window, cx)))
            .on_action(cx.listener(|s, _: &files::Delete, window, cx| s.dispatch_selected(|p| Box::new(DeleteShot(p)), window, cx)));
        if !self.loaded {
            return root.child(div().size_full().flex().items_center().justify_center().child(sheet::waiting(tr("Screenshots werden gesucht …"), &t))).into_any_element();
        }
        let outcome = match &self.outcome {
            Some(Outcome::Done(text)) => Some(sheet::status("circle-check", t.success_text, text.clone()).flex_none()),
            Some(Outcome::Failed(problem)) => Some(sheet::problem("shots-problem", "circle-alert", t.danger_text, problem, self.details, cx.listener(|s, _, _, cx| {
                s.details = !s.details;
                cx.notify();
            }), &t).flex_none()),
            None => None,
        };
        if self.shots.is_empty() {
            return root.children(outcome).child(files::empty("shots-empty", "image", tr("Noch keine Screenshots"), tr("Drücke im Spiel F2, um einen aufzunehmen."), None, &t)).into_any_element();
        }
        let columns = self.columns();
        let rows = self.shots.len().div_ceil(columns);
        let width = self.width.get();
        let cell = ((width - space::S * (columns - 1) as f32) / columns as f32).floor().max(1.);
        let list = uniform_list(
            "shots-grid",
            rows,
            cx.processor(move |s: &mut Self, range: Range<usize>, _, cx| {
                let t = Theme::get(cx);
                let first = range.start * columns;
                let last = (range.end * columns).min(s.shots.len());
                let shown: Vec<Shot> = s.shots.get(first..last).map(<[Shot]>::to_vec).unwrap_or_default();
                s.want(shown.iter().map(|f| f.path.clone()).collect(), cx);
                shown
                    .chunks(columns)
                    .enumerate()
                    .map(|(n, chunk)| {
                        let cells: Vec<AnyElement> = chunk.iter().enumerate().map(|(i, shot)| s.cell(first + n * columns + i, shot, cell, cx, &t)).collect();
                        div().w_full().pb(px(space::S)).flex().gap(px(space::S)).children(cells).into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full();
        let measure = {
            let width = Rc::clone(&self.width);
            let view = cx.entity().downgrade();
            canvas(
                move |bounds, window, _| {
                    let now = f32::from(bounds.size.width);
                    if (width.get() - now).abs() > 0.5 {
                        width.set(now);
                        let view = view.clone();
                        window.on_next_frame(move |_, cx| {
                            view.update(cx, |_, cx| cx.notify()).ok();
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
        };
        let grid = div().id("shots-area").flex_1().min_h_0().relative().child(measure).when(width > 0., |d| d.child(list));
        root.child(self.toolbar(cx, &t)).children(outcome).child(grid).into_any_element()
    }
}
