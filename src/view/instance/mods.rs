use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use freya::prelude::*;

use super::{Facts, links, snapshots};
use crate::core::content::Bisect;
use crate::core::i18n::{tr, trf};
use crate::ui::forms::Problem;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::{self, Menu};
use crate::view::kit::select::Select;
use crate::view::kit::text::{caption, heading, line, mono, secondary, spacer};
use crate::view::kit::{Bounds, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{Theme, control, font, frame, iconsize, space};
use list::{Data, Row, Sort};
use row::{INSET, ModRow, ROW, Section, Trail};

#[path = "mods_list.rs"]
mod list;
#[path = "mods_ops.rs"]
mod ops;
#[path = "mods_row.rs"]
mod row;
#[path = "mods_trouble.rs"]
mod trouble;
#[path = "mods_updates.rs"]
mod updates;

pub(super) trait Live<T: 'static> {
    fn soft(self, f: impl FnOnce(&mut T));
    fn got(self) -> Option<T>
    where
        T: Clone;
}

impl<T: 'static> Live<T> for State<T> {
    fn soft(mut self, f: impl FnOnce(&mut T)) {
        if let Some(mut w) = self.try_write() {
            f(&mut w);
        }
    }

    fn got(self) -> Option<T>
    where
        T: Clone,
    {
        self.try_peek().map(|r| r.clone())
    }
}

/// A problem shown above the list; `failed` when it came from a change, not from reading the files.
#[derive(Clone, PartialEq)]
pub(super) struct Fault {
    pub problem: Problem,
    pub failed: bool,
}

/// Everything the rows and the operations of the tab share.
#[derive(Clone, PartialEq)]
pub(super) struct Ctx {
    pub store: crate::view::store::Store,
    pub id: Rc<str>,
    pub data: State<Rc<Data>>,
    pub fault: State<Option<Fault>>,
    pub query: State<String>,
    pub sort: State<Sort>,
    pub selected: State<Rc<HashSet<String>>>,
    pub anchor: State<Option<String>>,
    pub cursor: State<Option<String>>,
    pub bisect: State<Option<Bisect>>,
    pub busy: State<bool>,
    pub bisecting: State<bool>,
    pub reveal: State<Option<String>>,
    pub ticket: State<u64>,
    pub key: State<String>,
    pub scroll: ScrollController,
    pub viewport: State<f32>,
    pub bounds: State<Bounds>,
    pub list_focus: AccessibilityId,
    pub filter_focus: AccessibilityId,
}

/// A problem with its cause one click away.
pub(super) fn problem_view(t: &Theme, id: &str, glyph: Icon, problem: &Problem, mut details: State<bool>) -> Element {
    let open = *details.read();
    let mut col = rect().width(Size::fill()).spacing(space::XS).child(
        rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Start)
            .spacing(space::S)
            .content(Content::flex())
            .child(icon(t, glyph, iconsize::M, t.danger_text))
            .child(line(t, problem.text.clone(), font::CALLOUT, t.danger_text).max_lines(4).width(Size::flex(1.))),
    );
    if let Some(detail) = &problem.detail {
        let label = if open { tr("Details ausblenden") } else { tr("Details") };
        col = col.child(Button::new(format!("{id}.details"), label).plain().on_press(move |_| details.set(!open)));
        if open {
            col = col.child(mono(t, detail.clone(), font::CAPTION, t.text2).width(Size::fill()));
        }
    }
    col.into_element()
}

#[derive(Default)]
struct Cache {
    data: Option<Rc<Data>>,
    query: String,
    sort: Sort,
    rows: Rc<Vec<Row>>,
}

struct View {
    data: Rc<Data>,
    rows: Rc<Vec<Row>>,
    selected: Rc<HashSet<String>>,
    cursor: Option<String>,
    locked: Option<String>,
    ctx: Ctx,
}

impl PartialEq for View {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.data, &other.data) && Rc::ptr_eq(&self.rows, &other.rows) && Rc::ptr_eq(&self.selected, &other.selected) && self.cursor == other.cursor && self.locked == other.locked
    }
}

impl View {
    fn item(&self, index: usize) -> Element {
        match self.rows.get(index) {
            Some(Row::Own) => rect().key("own").width(Size::fill()).height(Size::px(ROW)).child(Section).into_element(),
            Some(Row::Mod(at)) => {
                let Some(e) = self.data.entries().get(*at) else { return rect().height(Size::px(ROW)).into_element() };
                let (caption, warn) = list::caption(&self.data, e);
                let trail = if self.data.analysis.older.contains_key(&e.file) {
                    Trail::Fix { newest: true }
                } else if self.data.analysis.twin.contains_key(&e.file) {
                    Trail::Fix { newest: false }
                } else {
                    self.data.analysis.needs.get(&e.file).map_or(Trail::None, |m| Trail::Needs(m.clone()))
                };
                let selected = self.selected.contains(&e.file);
                let cursor = self.cursor.as_deref() == Some(e.file.as_str());
                rect()
                    .key(&e.file)
                    .width(Size::fill())
                    .height(Size::px(ROW))
                    .child(ModRow {
                        ctx: self.ctx.clone(),
                        file: e.file.clone(),
                        name: e.name(),
                        version: e.version().map(str::to_owned),
                        caption,
                        warn,
                        enabled: e.enabled,
                        icon: e.icon.clone(),
                        selected,
                        cursor,
                        locked: self.locked.clone(),
                        trail,
                    })
                    .into_element()
            }
            None => rect().height(Size::px(ROW)).into_element(),
        }
    }
}

fn empty(t: &Theme, glyph: Icon, title: String, hint: String, action: Option<Element>) -> Element {
    let mut col = rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .child(icon(t, glyph, iconsize::HERO, t.text3))
        .child(heading(t, title))
        .child(secondary(t, hint).max_lines(3));
    if let Some(a) = action {
        col = col.child(a);
    }
    col.into_element()
}

/// Die Mods-Liste mit Updates und Fehlersuche.
#[derive(PartialEq)]
pub struct Mods {
    pub facts: Facts,
}

impl Component for Mods {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let made = &self.facts.made;
        let id: Rc<str> = use_hook(|| Rc::from(made.id.as_str()));
        let data = use_state(|| Rc::new(Data::fallback(made)));
        let fault = use_state(|| None::<Fault>);
        let query = use_state(String::new);
        let sort = use_state(Sort::current);
        let selected = use_state(|| Rc::new(HashSet::<String>::new()));
        let anchor = use_state(|| None::<String>);
        let cursor = use_state(|| None::<String>);
        let bisect = use_state(|| None::<Bisect>);
        let busy = use_state(|| false);
        let bisecting = use_state(|| false);
        let reveal = use_state(|| None::<String>);
        let ticket = use_state(|| 0u64);
        let key = use_state(String::new);
        let viewport = use_state(|| 0f32);
        let bounds = use_state(Bounds::default);
        let scroll = use_hook(|| ScrollController::new(0, 0));
        let list_focus = use_a11y();
        let filter_focus = use_a11y();
        let details = use_state(|| false);
        let mut dropping = use_state(|| false);
        let seen = use_state(|| u64::MAX);
        let cache = use_hook(|| Rc::new(RefCell::new(Cache::default())));

        let ctx = Ctx { store, id, data, fault, query, sort, selected, anchor, cursor, bisect, busy, bisecting, reveal, ticket, key, scroll, viewport, bounds, list_focus, filter_focus };

        let changed = *ui.instance.changed.read();
        let rescan = ctx.clone();
        use_side_effect_with_deps(&(changed, list::key_of(made)), move |(changed, key)| {
            let mut seen = seen;
            if *seen.peek() != *changed || rescan.key.got().as_deref() != Some(key.as_str()) {
                seen.set(*changed);
                rescan.rescan();
            }
        });

        let scan = ctx.data.read().clone();
        let text = ctx.query.read().clone();
        let needle = text.trim().to_lowercase();
        let order = *ctx.sort.read();
        let rows = {
            let mut c = cache.borrow_mut();
            let fresh = c.data.as_ref().is_some_and(|d| Rc::ptr_eq(d, &scan)) && c.query == needle && c.sort == order;
            if !fresh {
                c.rows = Rc::new(list::rows(&scan, &needle, order));
                c.data = Some(scan.clone());
                c.query = needle.clone();
                c.sort = order;
            }
            c.rows.clone()
        };
        let chosen = ctx.selected.read().clone();
        let focused = list_focus.is_focused();
        let at = ctx.cursor.read().clone().filter(|_| focused);
        let current = ctx.bisect.read().clone();
        let fault_now = ctx.fault.read().clone();

        let add = links::add_mods(store, &made.id, &made.name);
        let packs = links::resource_packs(store, &made.id);
        let shaders = links::shaders(store, &made.id);
        super::use_extras(ctx.extras());

        let loading = !scan.scanned && fault_now.is_none();
        let lock = ctx.locked().or_else(|| loading.then(|| tr("Mods werden gelesen …")));
        let mods_shown = rows.iter().filter(|r| matches!(r, Row::Mod(_))).count();
        let filtering = !needle.is_empty();
        let picked = list::chosen(&list::files(&scan, &rows), &chosen).len();

        let filter = {
            let on_keys = ctx.clone();
            rect()
                .width(Size::flex(1.))
                .on_key_down(move |e: Event<KeyboardEventData>| match e.key {
                    Key::Named(NamedKey::Escape) => {
                        e.stop_propagation();
                        if on_keys.query.peek().is_empty() {
                            on_keys.list_focus.request_focus();
                        } else {
                            on_keys.query.soft(String::clear);
                        }
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        e.stop_propagation();
                        on_keys.list_focus.request_focus();
                        if on_keys.cursor.peek().is_none() {
                            on_keys.step(1, false);
                        }
                    }
                    _ => {}
                })
                .child(
                    Field::new("instance.mods.filter", ctx.query)
                        .placeholder(tr("Mods filtern"))
                        .glyph(Icon::Search)
                        .a11y(filter_focus)
                        .on_submit({
                            let ctx = ctx.clone();
                            move |_| {
                                ctx.list_focus.request_focus();
                                if ctx.cursor.peek().is_none() {
                                    ctx.step(1, false);
                                }
                            }
                        }),
                )
        };

        let sort_menu = {
            let ctx = ctx.clone();
            Select::new("instance.mods.sort", tr("Sortieren"), Sort::ALL.iter().map(|s| s.label()).collect(), Sort::ALL.iter().position(|s| *s == order).unwrap_or(0))
                .width(frame::SEARCH_NARROW)
                .on_choose(move |i| {
                    if let Some(s) = Sort::ALL.get(i) {
                        ctx.sort_by(*s);
                    }
                })
        };

        let updates_why = made.mods.is_empty().then(|| tr("Keine Mods von Modrinth."));
        let check = {
            let ctx = ctx.clone();
            Button::new("instance.mods.updates", tr("Updates prüfen…")).glyph(Icon::Refresh).disabled(updates_why).on_press(move |_| ctx.check_updates())
        };

        let mut more_bounds = use_state(Bounds::default);
        let more = {
            let ctx = ctx.clone();
            rect().on_sized(move |e: Event<SizedEventData>| more_bounds.set_if_modified(Bounds::of(&e))).child(Button::icon("instance.mods.more", Icon::More, tr("Mehr")).plain().on_press(move |_| {
                menu::open(ui, Menu::under(ctx.more_menu(&packs, &shaders), *more_bounds.peek()));
            }))
        };

        let mut bar = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex()).child(filter).child(sort_menu).child(check).child(more);
        let add_button = {
            let run = add.run.clone();
            Button::new("instance.mods.add", add.label.clone()).primary().glyph(add.glyph).disabled(add.disabled.clone()).on_press(move |_| run.call(()))
        };
        bar = bar.child(add_button);

        let mut counts = rect().width(Size::fill()).height(Size::px(control::S)).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex());
        counts = counts.child(caption(&t, list::count_text(&scan, mods_shown, filtering)).width(Size::flex(1.)).max_lines(1));
        if picked > 1 {
            let (on, off, del) = (ctx.clone(), ctx.clone(), ctx.clone());
            counts = counts
                .child(caption(&t, trf("%lld ausgewählt", &[&picked])))
                .child(Button::new("instance.mods.bulk.on", tr("Einschalten")).plain().disabled(lock.clone()).on_press(move |_| on.switch(on.targets(), true)))
                .child(Button::new("instance.mods.bulk.off", tr("Ausschalten")).plain().disabled(lock.clone()).on_press(move |_| off.switch(off.targets(), false)))
                .child(Button::new("instance.mods.bulk.delete", tr("Löschen")).plain().danger().disabled(lock.clone()).on_press(move |_| del.delete(del.targets())));
        }

        let body: Element = if scan.entries().is_empty() && loading {
            rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Mods werden gelesen …"))).into_element()
        } else if scan.entries().is_empty() {
            let run = add.run.clone();
            let button = Button::new("instance.mods.empty.add", add.label.clone()).primary().glyph(add.glyph).disabled(add.disabled.clone()).on_press(move |_| run.call(())).into_element();
            empty(&t, Icon::Layers, tr("Noch keine Mods"), tr("Füge Mods hinzu oder ziehe .jar-Dateien hierher."), Some(button))
        } else if mods_shown == 0 {
            let clear = ctx.clone();
            let button = Button::new("instance.mods.empty.clear", tr("Filter löschen")).on_press(move |_| clear.clear_filter()).into_element();
            empty(&t, Icon::Search, trf("Kein Mod passt zu „%@“.", &[&text.trim()]), String::new(), Some(button))
        } else {
            let view = View { data: scan.clone(), rows: rows.clone(), selected: chosen.clone(), cursor: at, locked: lock.clone(), ctx: ctx.clone() };
            let (view_h, view_b) = (ctx.viewport, ctx.bounds);
            VirtualScrollView::new_with_data_controlled(view, |item, view: &View| view.item(item.index), ctx.scroll)
                .length(rows.len())
                .item_size(ROW)
                .scroll_with_arrows(false)
                .on_sized(move |e: Event<SizedEventData>| {
                    view_h.soft(|h| *h = e.area.height());
                    view_b.soft(|b| *b = Bounds::of(&e));
                })
                .into_element()
        };

        let keys = ctx.clone();
        let list = rect()
            .a11y_id(list_focus)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::List)
            .a11y_alt(tr("Mods"))
            .width(Size::fill())
            .height(Size::flex(1.))
            .padding(INSET)
            .corner_radius(t.shape.tile)
            .overflow(Overflow::Clip)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if keys.key_down(&e, keys.list_focus.is_focused()) {
                    e.stop_propagation();
                }
            })
            .child(body);

        let drop_ctx = ctx.clone();
        let mut root = rect()
            .width(Size::fill())
            .height(Size::fill())
            .padding(space::L)
            .spacing(space::S)
            .on_global_file_hover(move |_| dropping.set_if_modified(true))
            .on_global_file_hover_cancelled(move |_| dropping.set_if_modified(false))
            .on_file_drop(move |e: Event<FileEventData>| {
                dropping.set_if_modified(false);
                match drop_ctx.locked() {
                    Some(why) => drop_ctx.note(why),
                    None => drop_ctx.dropped(e.file_paths.clone()),
                }
            });
        if let Some(b) = &current {
            root = root.child(trouble::card(&ctx, &t, b, &scan, self.facts.running));
        }
        if let Some(f) = &fault_now {
            let mut col = rect().width(Size::fill()).spacing(space::XS).child(problem_view(&t, "instance.mods.problem", Icon::Alert, &f.problem, details));
            if !f.failed {
                let retry = ctx.clone();
                col = col.child(Button::new("instance.mods.retry", tr("Erneut versuchen")).on_press(move |_| retry.rescan()));
            }
            root = root.child(col);
        }
        root = root.child(bar).child(counts).child(list);
        if *dropping.read() {
            root = root.child(
                rect()
                    .position(Position::new_absolute().left(0.).top(0.))
                    .width(Size::fill())
                    .height(Size::fill())
                    .corner_radius(t.shape.tile)
                    .background(t.accent.with_a((frame::NOTICE_WASH * 255.) as u8))
                    .child(spacer()),
            );
        }
        root.content(Content::flex())
    }
}
