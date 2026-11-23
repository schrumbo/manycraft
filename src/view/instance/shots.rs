use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use freya::prelude::*;
use uuid::Uuid;

use super::shots_parts::{Move, Same, bring_into_view, cell_width, centered, columns, key_move, moved, page, picture_height, row_height};
use super::shots_thumbs::Thumb;
use super::shots_view::Viewer;
use super::{Facts, actions, use_extras};
use crate::core::i18n::{tr, trf};
use crate::core::shots::{self, Shot};
use crate::core::trash;
use crate::ui::forms;
use crate::ui::instance_files::when;
use crate::ui::theme::{files, tile};
use crate::view::commands::{Extra, Shortcut};
use crate::view::drive::use_spot;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::text::{line, secondary};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::native;
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, space};

const DOUBLE: Duration = Duration::from_millis(450);

#[derive(Clone)]
struct List {
    loaded: bool,
    shots: Same<Vec<Shot>>,
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[derive(Clone)]
struct Grid {
    store: Store,
    dir: PathBuf,
    list: State<List>,
    selected: State<Option<PathBuf>>,
    width: State<f32>,
    controller: ScrollController,
    viewing: State<bool>,
    copying: State<bool>,
}

impl Grid {
    fn shots(&self) -> Arc<Vec<Shot>> {
        self.list.peek().shots.0.clone()
    }

    fn at(&self) -> Option<usize> {
        let selected = self.selected.peek();
        let path = selected.as_ref()?;
        self.list.peek().shots.0.iter().position(|s| &s.path == path)
    }

    fn current(&self) -> Option<Shot> {
        let at = self.at()?;
        self.list.peek().shots.0.get(at).cloned()
    }

    fn usable(&self) -> f32 {
        (*self.width.peek() - space::L).max(0.)
    }

    fn columns(&self) -> usize {
        columns(self.usable())
    }

    fn row(&self) -> f32 {
        row_height(cell_width(self.usable(), self.columns()))
    }

    fn viewport(&self) -> f32 {
        self.controller.bounds.peek().map_or(0., |(_, view)| view.height)
    }

    fn reveal_selected(&self) {
        let Some(at) = self.at() else { return };
        let mut controller = self.controller;
        let y = controller.position().y as i32;
        if let Some(y) = bring_into_view(at / self.columns(), self.row(), self.viewport(), y) {
            controller.scroll_to_y(y);
        }
    }

    fn select(&self, at: usize) {
        let shots = self.shots();
        let Some(shot) = shots.get(at) else { return };
        let mut selected = self.selected;
        selected.set(Some(shot.path.clone()));
        self.reveal_selected();
    }

    fn step(&self, by: isize) {
        if let Some(at) = moved(self.at(), by, self.shots().len()) {
            self.select(at);
        }
    }

    fn go(&self, m: Move) {
        let columns = self.columns() as isize;
        match m {
            Move::Left => self.step(-1),
            Move::Right => self.step(1),
            Move::Up => self.step(-columns),
            Move::Down => self.step(columns),
            Move::First => self.select(0),
            Move::Last => self.select(self.shots().len().saturating_sub(1)),
            Move::PageUp => self.step(-page(self.viewport(), self.row(), self.columns())),
            Move::PageDown => self.step(page(self.viewport(), self.row(), self.columns())),
            Move::Open => self.open(),
            Move::Delete => self.delete(),
        }
    }

    fn open(&self) {
        if self.current().is_some() {
            let mut viewing = self.viewing;
            viewing.set(true);
        }
    }

    fn close(&self) {
        let mut viewing = self.viewing;
        viewing.set(false);
        self.reveal_selected();
    }

    fn external(&self) {
        if let Some(shot) = self.current() {
            native::open(&shot.path);
        }
    }

    fn copy_image(&self) {
        let Some(shot) = self.current() else { return };
        let (ui, mut copying) = (self.store.ui, self.copying);
        if *copying.peek() {
            return;
        }
        copying.set(true);
        spawn_in_window(async move {
            let done = off_ui(move || native::copy_image(&shot.path)).await;
            copying.set(false);
            let text = match done {
                Some(Ok(())) => tr("Bild kopiert."),
                Some(Err(e)) => forms::explain_as(tr("Das Bild wurde nicht kopiert."), &e).text,
                None => tr("Das Bild wurde nicht kopiert."),
            };
            toast::show(ui, text, None);
        });
    }

    fn reveal(&self) {
        if let Some(shot) = self.current() {
            native::reveal(&shot.path);
        }
    }

    fn copy(&self) {
        let Some(shot) = self.current() else { return };
        let text = if native::copy_text(shot.path.display().to_string()) { tr("Pfad kopiert.") } else { tr("Der Pfad wurde nicht kopiert.") };
        toast::show(self.store.ui, text, None);
    }

    fn delete(&self) {
        let Some(shot) = self.current() else { return };
        let shots = self.shots();
        let at = self.at().unwrap_or(0);
        let next = shots.get(at + 1).or_else(|| at.checked_sub(1).and_then(|p| shots.get(p))).map(|s| s.path.clone());
        let rest: Vec<Shot> = shots.iter().filter(|s| s.path != shot.path).cloned().collect();
        let (mut list, mut selected) = (self.list, self.selected);
        if next.is_none() {
            let mut viewing = self.viewing;
            viewing.set(false);
        }
        selected.set(next);
        list.set(List { loaded: true, shots: Same(Arc::new(rest)) });
        let (store, dir) = (self.store, self.dir.clone());
        let name = name_of(&shot.path);
        spawn_in_window(async move {
            let (from, path) = (dir.clone(), shot.path.clone());
            match off_ui(move || trash::put(&from, &path)).await {
                Some(Ok(entry)) => offer_undo(store, dir, name, entry),
                Some(Err(e)) => {
                    toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht gelöscht.", &[&name]), &e).text, None);
                }
                None => {
                    toast::show(store.ui, trf("„%@“ wurde nicht gelöscht.", &[&name]), None);
                }
            }
            actions::changed(store);
        });
    }
}

fn offer_undo(store: Store, dir: PathBuf, name: String, entry: String) {
    let text = trf("„%@“ gelöscht.", &[&name]);
    toast::show_kept(store.ui, text, EventHandler::new(move |_| undo(store, dir.clone(), name.clone(), entry.clone())));
}

fn undo(store: Store, dir: PathBuf, name: String, entry: String) {
    spawn_in_window(async move {
        let (from, back) = (dir.clone(), entry.clone());
        match off_ui(move || trash::take_back(&from, &back)).await {
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                toast::show(store.ui, forms::explain_as(trf("„%@“ wurde nicht zurückgeholt.", &[&name]), &e).text, None);
                if !actions::permanent(&e) {
                    offer_undo(store, dir, name, entry);
                }
            }
            None => offer_undo(store, dir, name, entry),
        }
        actions::changed(store);
    });
}

fn menu_items(grid: &Grid) -> Vec<Item> {
    let (open, external, reveal, image, copy, delete) = (grid.clone(), grid.clone(), grid.clone(), grid.clone(), grid.clone(), grid.clone());
    vec![
        Item::new("instance.shots.menu.open", tr("Öffnen"), move |_| open.open()).shortcut(Shortcut::bare(Code::Enter).label()),
        Item::new("instance.shots.menu.external", tr("Mit Standard-App öffnen"), move |_| external.external()),
        Item::new("instance.shots.menu.reveal", tr("Im Ordner zeigen"), move |_| reveal.reveal()),
        Item::Separator,
        Item::new("instance.shots.menu.image", tr("Bild kopieren"), move |_| image.copy_image()).shortcut(Shortcut::primary(Code::KeyC).label()),
        Item::new("instance.shots.menu.copy", tr("Pfad kopieren"), move |_| copy.copy()),
        Item::Separator,
        Item::new("instance.shots.menu.delete", tr("Löschen"), move |_| delete.delete()).danger(),
    ]
}

#[derive(PartialEq)]
struct Cell {
    index: usize,
    shot: Shot,
    caption: String,
    name: String,
    width: f32,
    selected: bool,
    on_pick: EventHandler<usize>,
    on_open: EventHandler<usize>,
    on_menu: EventHandler<(usize, (f32, f32))>,
}

impl Component for Cell {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut last_press = use_state(|| None::<Instant>);
        let spot = use_spot(&format!("instance.shots.cell.{}", self.index), "cell", &self.name, true);
        let height = picture_height(self.width);
        let ring = if self.selected { t.accent } else { Color::TRANSPARENT };
        let (index, pick, open, menu) = (self.index, self.on_pick.clone(), self.on_open.clone(), self.on_menu.clone());
        let wash = if *hovered.read() && !self.selected { t.hover } else { Color::TRANSPARENT };
        rect()
            .width(Size::px(self.width))
            .spacing(space::XS)
            .corner_radius(t.shape.control)
            .background(wash)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(format!("{}, {}", self.name, self.caption))
            .on_sized(spot)
            .cursor(CursorIcon::Pointer)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                let now = Instant::now();
                let double = last_press.peek().is_some_and(|at| now - at < DOUBLE);
                last_press.set(if double { None } else { Some(now) });
                if double {
                    open.call(index);
                } else {
                    pick.call(index);
                }
            })
            .on_secondary_down(move |e: Event<PressEventData>| {
                e.stop_propagation();
                if let PressEventData::Mouse(m) = &*e {
                    menu.call((index, (m.global_location.x as f32, m.global_location.y as f32)));
                }
            })
            .child(
                rect()
                    .width(Size::px(self.width))
                    .height(Size::px(height))
                    .corner_radius(t.shape.control)
                    .overflow(Overflow::Clip)
                    .border(Border::new().fill(ring).width(tile::RING).alignment(BorderAlignment::Inner))
                    .child(Thumb { path: self.shot.path.clone(), stamp: self.shot.taken, width: self.width, height }),
            )
            .child(rect().width(Size::fill()).height(Size::px(files::SHOT_CAPTION - space::XS)).child(line(&t, self.caption.clone(), font::CAPTION, if self.selected { t.text1 } else { t.text2 })))
    }
}

#[derive(PartialEq)]
struct Rows {
    shots: Same<Vec<Shot>>,
    columns: usize,
    cell: f32,
    selected: Option<PathBuf>,
    accounts: Vec<(Uuid, String)>,
    on_pick: EventHandler<usize>,
    on_open: EventHandler<usize>,
    on_menu: EventHandler<(usize, (f32, f32))>,
}

fn row(item: VirtualItem, d: &Rows) -> Element {
    let first = item.index * d.columns;
    let last = (first + d.columns).min(d.shots.0.len());
    let cells = d.shots.0.get(first..last).unwrap_or_default().iter().enumerate().map(|(i, shot)| {
        let who = d.accounts.iter().find(|(id, _)| *id == shot.account).map_or_else(|| tr("Entfernter Account"), |(_, name)| name.clone());
        rect()
            .key(&shot.path)
            .child(Cell {
                index: first + i,
                shot: shot.clone(),
                caption: format!("{who} · {}", when(shot.taken)),
                name: name_of(&shot.path),
                width: d.cell,
                selected: d.selected.as_ref() == Some(&shot.path),
                on_pick: d.on_pick.clone(),
                on_open: d.on_open.clone(),
                on_menu: d.on_menu.clone(),
            })
            .into_element()
    });
    rect().key(item.index).width(Size::fill()).height(Size::px(item.size)).horizontal().spacing(space::S).children(cells).into_element()
}

/// Die Screenshots der Instanz.
#[derive(PartialEq)]
pub struct Shots {
    pub facts: Facts,
}

impl Component for Shots {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let f = &self.facts;
        let dir = f.made.dir();
        let changed = *store.ui.instance.changed.read();
        let mut list = use_state(|| List { loaded: false, shots: Same(Arc::new(Vec::new())) });
        let mut selected = use_state(|| None::<PathBuf>);
        let width = use_state(|| 0f32);
        let mut scan = use_state(|| 0u64);
        let controller = use_scroll_controller(ScrollConfig::default);
        let a11y = use_a11y();
        let viewing = use_state(|| false);
        let copying = use_state(|| false);
        let grid = Grid { store, dir: dir.clone(), list, selected, width, controller, viewing, copying };

        let reload = grid.clone();
        use_side_effect_with_deps(&(dir, changed, f.running), move |(dir, _, _)| {
            let ticket = scan.peek().wrapping_add(1);
            scan.set(ticket);
            let (dir, grid) = (dir.clone(), reload.clone());
            spawn(async move {
                let found = off_ui(move || shots::list(&dir)).await.unwrap_or_default();
                if *scan.peek() != ticket {
                    return;
                }
                let first = found.first().map(|s| s.path.clone());
                let keep = selected.peek().clone().filter(|p| found.iter().any(|s| &s.path == p));
                selected.set(keep.or(first));
                list.set(List { loaded: true, shots: Same(Arc::new(found)) });
                grid.reveal_selected();
            });
        });

        let _ = width.read();
        let chosen = grid.current().is_some();
        let mut extras = Vec::new();
        if chosen {
            let (open, external, image, copy, reveal, delete) = (grid.clone(), grid.clone(), grid.clone(), grid.clone(), grid.clone(), grid.clone());
            extras.push(Extra { id: "instance.shots.open".into(), label: tr("Screenshot öffnen"), glyph: Icon::Fit, shortcut: None, run: EventHandler::new(move |_| open.open()) });
            extras.push(Extra { id: "instance.shots.external".into(), label: tr("Screenshot mit Standard-App öffnen"), glyph: Icon::External, shortcut: None, run: EventHandler::new(move |_| external.external()) });
            extras.push(Extra { id: "instance.shots.image".into(), label: tr("Bild kopieren"), glyph: Icon::Copy, shortcut: Some(Shortcut::primary(Code::KeyC)), run: EventHandler::new(move |_| image.copy_image()) });
            extras.push(Extra { id: "instance.shots.copy".into(), label: tr("Pfad kopieren"), glyph: Icon::Copy, shortcut: None, run: EventHandler::new(move |_| copy.copy()) });
            extras.push(Extra { id: "instance.shots.reveal".into(), label: tr("Screenshot im Ordner zeigen"), glyph: Icon::Folder, shortcut: None, run: EventHandler::new(move |_| reveal.reveal()) });
            extras.push(Extra { id: "instance.shots.delete".into(), label: tr("Screenshot löschen"), glyph: Icon::Close, shortcut: None, run: EventHandler::new(move |_| delete.delete()) });
        }
        use_extras(extras);

        let shown = list.read().clone();
        let count = shown.shots.0.len();
        let mut tools = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
        if chosen {
            let (open, reveal, image, delete) = (grid.clone(), grid.clone(), grid.clone(), grid.clone());
            tools = tools
                .child(Button::new("instance.shots.open", tr("Öffnen")).glyph(Icon::Fit).on_press(move |_| open.open()))
                .child(Button::new("instance.shots.copy", tr("Bild kopieren")).glyph(Icon::Copy).busy(*copying.read()).tip(Shortcut::primary(Code::KeyC).label()).on_press(move |_| image.copy_image()))
                .child(Button::new("instance.shots.reveal", tr("Im Ordner zeigen")).glyph(Icon::Folder).on_press(move |_| reveal.reveal()))
                .child(Button::new("instance.shots.delete", tr("Löschen")).on_press(move |_| delete.delete()));
        }
        let title = if !shown.loaded {
            String::new()
        } else if count == 1 {
            tr("1 Screenshot")
        } else {
            trf("%lld Screenshots", &[&count])
        };
        let toolbar = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .padding((space::M, space::L))
            .child(secondary(&t, title).width(Size::flex(1.)))
            .child(tools)
            .content(Content::flex());

        if *viewing.read() && count > 0 {
            let at = grid.at().unwrap_or(0);
            let who = shown.shots.0.get(at).map_or_else(String::new, |s| f.accounts.iter().find(|(id, _)| *id == s.account).map_or_else(|| tr("Entfernter Account"), |(_, name)| name.clone()));
            let (step, close, external, reveal, delete, image) = (grid.clone(), grid.clone(), grid.clone(), grid.clone(), grid.clone(), grid.clone());
            return Viewer {
                shots: shown.shots.clone(),
                at,
                who,
                copying: *copying.read(),
                on_step: EventHandler::new(move |by| step.step(by)),
                on_close: EventHandler::new(move |_| close.close()),
                on_copy: EventHandler::new(move |_| image.copy_image()),
                on_reveal: EventHandler::new(move |_| reveal.reveal()),
                on_external: EventHandler::new(move |_| external.external()),
                on_delete: EventHandler::new(move |_| delete.delete()),
            }
            .into_element();
        }

        let body = if !shown.loaded {
            rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Screenshots werden gesucht …"))).into_element()
        } else if count == 0 {
            centered(&t, Icon::Fit, tr("Noch keine Screenshots"), tr("Drücke im Spiel F2, um einen aufzunehmen."), super::play::empty_action(store, f, "instance.shots.empty.play"))
        } else {
            let usable = grid.usable();
            let n = columns(usable);
            let cell = cell_width(usable, n);
            let pick = grid.clone();
            let open = grid.clone();
            let context = grid.clone();
            let data = Rows {
                shots: shown.shots.clone(),
                columns: n,
                cell,
                selected: selected.read().clone(),
                accounts: f.accounts.clone(),
                on_pick: EventHandler::new(move |i| {
                    a11y.request_focus();
                    pick.select(i);
                }),
                on_open: EventHandler::new(move |i| {
                    a11y.request_focus();
                    open.select(i);
                    open.open();
                }),
                on_menu: EventHandler::new(move |(i, at): (usize, (f32, f32))| {
                    a11y.request_focus();
                    context.select(i);
                    menu::open(context.store.ui, Menu::at(menu_items(&context), at));
                }),
            };
            let keys = grid.clone();
            let mut width = width;
            rect()
                .a11y_id(a11y)
                .a11y_focusable(true)
                .a11y_role(AccessibilityRole::List)
                .a11y_auto_focus(!store.ui.layered())
                .width(Size::fill())
                .height(Size::fill())
                .padding((0., space::L, space::L, space::L))
                .on_key_down(move |e: Event<KeyboardEventData>| {
                    if let Some(m) = key_move(&e.key, e.modifiers) {
                        e.stop_propagation();
                        keys.go(m);
                    }
                })
                .child(
                    rect()
                        .width(Size::fill())
                        .height(Size::fill())
                        .on_sized(move |e: Event<SizedEventData>| width.set_if_modified(e.area.width()))
                        .child(VirtualScrollView::new_with_data_controlled(data, row, controller).length(count.div_ceil(n)).item_size(row_height(cell)).width(Size::fill()).height(Size::fill())),
                )
                .into_element()
        };
        rect().width(Size::fill()).height(Size::fill()).child(toolbar).child(rect().width(Size::fill()).height(Size::flex(1.)).child(body)).content(Content::flex()).into_element()
    }
}
