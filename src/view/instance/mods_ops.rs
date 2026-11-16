use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use freya::prelude::*;

use super::list::{self, Row, Sort};
use super::row::ROW;
use super::{Ctx, Fault, Live};
use crate::app::event::Event as Msg;
use crate::core::content::{self, Outcome, Trashed};
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::ui::forms::{self, Problem};
use crate::view::commands::{Extra, Shortcut};
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::sheet;
use crate::view::kit::text::{para, spacer, title};
use crate::view::kit::{held, off_ui, toast, use_theme};
use crate::view::native;
use crate::view::store::Store;
use crate::view::theme::{frame, space};
use crate::view::ui::Ui;

/// Puts `made` into the model when it differs from what the model holds.
pub(super) fn publish(store: Store, made: Instance) {
    if store.model.peek().instance(&made.id) != Some(&made) {
        store.dispatch(Msg::InstanceChanged(made));
    }
}

pub(super) fn changed(store: Store, made: Instance) {
    publish(store, made);
    super::super::actions::changed(store);
}

pub(super) fn note(store: Store, text: impl Into<String>) {
    toast::show(store.ui, text, None);
}

pub(super) fn offer_switched(store: Store, id: String, files: Vec<String>, on: bool, label: String) {
    let text = if on { trf("%@ eingeschaltet.", &[&label]) } else { trf("%@ ausgeschaltet.", &[&label]) };
    toast::show(store.ui, text, Some(EventHandler::new(move |_| undo_switch(store, id.clone(), files.clone(), on, label.clone()))));
}

fn undo_switch(store: Store, id: String, files: Vec<String>, on: bool, label: String) {
    spawn_in_window(async move {
        let (work_id, work_files) = (id.clone(), files.clone());
        match off_ui(move || content::set_enabled(&work_id, &work_files, !on)).await {
            Some(Ok(made)) => changed(store, made),
            Some(Err(e)) => {
                note(store, forms::explain_as(tr("Nicht zurückgestellt."), &e).text);
                if !super::super::actions::permanent(&e) {
                    offer_switched(store, id, files, on, label);
                }
            }
            None => offer_switched(store, id, files, on, label),
        }
    });
}

pub(super) fn offer_trashed(store: Store, id: String, trashed: Trashed, label: String) {
    toast::show_kept(store.ui, trf("%@ gelöscht.", &[&label]), EventHandler::new(move |_| undo_trash(store, id.clone(), trashed.clone(), label.clone())));
}

fn undo_trash(store: Store, id: String, trashed: Trashed, label: String) {
    spawn_in_window(async move {
        let (work_id, work) = (id.clone(), trashed.clone());
        match off_ui(move || content::untrash(&work_id, &work)).await {
            Some(Ok(made)) => changed(store, made),
            Some(Err(e)) => {
                note(store, forms::explain_as(tr("Nicht zurückgeholt."), &e).text);
                if !super::super::actions::permanent(&e) {
                    offer_trashed(store, id, trashed, label);
                }
            }
            None => offer_trashed(store, id, trashed, label),
        }
    });
}

pub(super) fn offer_added(store: Store, id: String, files: Vec<String>, label: String) {
    toast::show(store.ui, trf("%@ hinzugefügt.", &[&label]), Some(EventHandler::new(move |_| undo_add(store, id.clone(), files.clone(), label.clone()))));
}

fn undo_add(store: Store, id: String, files: Vec<String>, label: String) {
    spawn_in_window(async move {
        let (work_id, work) = (id.clone(), files.clone());
        match off_ui(move || content::trash(&work_id, &work)).await {
            Some(Ok((made, _))) => changed(store, made),
            Some(Err(e)) => {
                note(store, forms::explain_as(tr("Nicht entfernt."), &e).text);
                if !super::super::actions::permanent(&e) {
                    offer_added(store, id, files, label);
                }
            }
            None => offer_added(store, id, files, label),
        }
    });
}

struct Ask {
    heading: String,
    message: String,
    only: String,
    all: String,
    on_only: EventHandler<()>,
    on_all: EventHandler<()>,
}

impl PartialEq for Ask {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

impl Component for Ask {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ui = use_consume::<Ui>();
        let (only, all) = (self.on_only.clone(), self.on_all.clone());
        let (key_all, heading) = (self.on_all.clone(), self.heading.clone());
        let footer = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .content(Content::flex())
            .child(spacer())
            .child(Button::new("instance.mods.ask.cancel", tr("Abbrechen")).on_press(move |_| sheet::close(ui)))
            .child(Button::new("instance.mods.ask.only", self.only.clone()).on_press(move |_| {
                sheet::close(ui);
                only.call(());
            }))
            .child(Button::new("instance.mods.ask.all", self.all.clone()).primary().on_press(move |_| {
                sheet::close(ui);
                all.call(());
            }));
        let mut card = rect()
            .a11y_role(AccessibilityRole::Dialog)
            .a11y_alt(heading.clone())
            .width(Size::px(frame::SHEET))
            .padding(space::XL)
            .spacing(space::L)
            .background(t.raised)
            .corner_radius(t.shape.sheet)
            .shadow(t.elevation.float.clone())
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_global_key_down(move |e: Event<KeyboardEventData>| match e.key {
                Key::Named(NamedKey::Escape | NamedKey::Enter) if ui.menu.peek().is_some() || *ui.palette.peek() => {}
                Key::Named(NamedKey::Escape) => {
                    e.stop_propagation();
                    sheet::close(ui);
                }
                Key::Named(NamedKey::Enter) if e.modifiers.is_empty() => {
                    e.stop_propagation();
                    sheet::close(ui);
                    key_all.call(());
                }
                _ => {}
            })
            .child(title(&t, heading))
            .child(para(&t, self.message.clone(), crate::view::theme::font::BODY, t.text2))
            .child(footer);
        if let Some(rim) = t.elevation.rim {
            card = card.border(Border::new().fill(rim).width(1.).alignment(BorderAlignment::Outer));
        }
        card
    }
}

impl Ctx {
    pub(super) fn id(&self) -> String {
        self.id.to_string()
    }

    pub(super) fn note(&self, text: impl Into<String>) {
        note(self.store, text);
    }

    fn running(&self) -> bool {
        self.store.model.peek().games.values().any(|g| g.instance_id == *self.id && g.phase.is_active())
    }

    /// Why the list cannot change now: work on the instance, an unreadable folder, the troubleshooter or running games.
    pub(super) fn locked(&self) -> Option<String> {
        if self.store.ui.instance.busy.read().contains_key(&*self.id) {
            return Some(tr("Warte, bis die Umstellung fertig ist."));
        }
        let scanned = self.data.read().scanned;
        if !scanned && self.fault.read().as_ref().is_some_and(|f| !f.failed) {
            return Some(tr("Die Mods ließen sich nicht lesen."));
        }
        if *self.bisecting.read() || self.bisect.read().as_ref().is_some_and(|b| b.outcome.is_none()) {
            return Some(tr("Beende zuerst die Fehlersuche."));
        }
        self.running().then(|| tr("Stoppe zuerst die laufenden Spiele."))
    }

    fn rows(&self) -> Vec<Row> {
        let query = self.query.peek().trim().to_lowercase();
        list::rows(&self.data.peek(), &query, *self.sort.peek())
    }

    pub(super) fn order(&self) -> Vec<String> {
        list::files(&self.data.peek(), &self.rows())
    }

    pub(super) fn targets(&self) -> Vec<String> {
        list::chosen(&self.order(), &self.selected.peek())
    }

    fn next_ticket(&self) -> Option<u64> {
        let mut at = None;
        self.ticket.soft(|t| {
            *t += 1;
            at = Some(*t);
        });
        at
    }

    pub(super) fn fail(&self, problem: Problem) {
        self.fault.soft(|f| *f = Some(Fault { problem, failed: true }));
    }

    pub(super) fn settle(&self) {
        self.fault.soft(|f| {
            if f.as_ref().is_some_and(|f| f.failed) {
                *f = None;
            }
        });
    }

    pub(super) fn put(&self, made: Instance) {
        self.key.soft(|k| *k = list::key_of(&made));
        publish(self.store, made);
    }

    pub(super) fn rescan(&self) {
        let Some(made) = self.store.model.peek().instance(&self.id).cloned() else { return };
        if !made.loader.takes_mods() {
            return;
        }
        self.key.soft(|k| *k = list::key_of(&made));
        let Some(ticket) = self.next_ticket() else { return };
        let ctx = self.clone();
        spawn_in_window(async move {
            let id = ctx.id();
            let result = off_ui(move || content::scan(&id).map(|(made, scan)| (made, scan, content::load(&id)))).await;
            ctx.scanned(ticket, result);
        });
    }

    fn scanned(&self, ticket: u64, result: Option<anyhow::Result<(Instance, content::Scan, Option<content::Bisect>)>>) {
        if self.ticket.got() != Some(ticket) {
            return;
        }
        match result {
            Some(Ok((made, scan, bisect))) => {
                self.put(made);
                let data = Rc::new(list::Data::new(scan, true));
                self.data.soft(|d| *d = data.clone());
                self.bisect.soft(|b| *b = bisect);
                self.settle_read();
                self.selected.soft(|s| {
                    if s.iter().any(|f| data.entry(f).is_none()) {
                        *s = Rc::new(s.iter().filter(|f| data.entry(f).is_some()).cloned().collect());
                    }
                });
                let wanted = self.reveal.got().flatten();
                self.reveal.soft(|r| *r = None);
                if let Some(file) = wanted.and_then(|name| data.entries().iter().find(|e| e.file.eq_ignore_ascii_case(&name)).map(|e| e.file.clone())) {
                    self.go_to(&file, false);
                }
            }
            Some(Err(e)) => self.unreadable(forms::explain_as(tr("Die Mods ließen sich nicht lesen."), &e)),
            None => self.unreadable(Problem::plain(forms::unexplained())),
        }
    }

    fn settle_read(&self) {
        self.fault.soft(|f| {
            if f.as_ref().is_some_and(|f| !f.failed) {
                *f = None;
            }
        });
    }

    fn unreadable(&self, problem: Problem) {
        self.fault.soft(|f| *f = Some(Fault { problem, failed: false }));
        self.data.soft(|d| {
            if d.scanned {
                *d = Rc::new(list::Data::new(d.scan.clone(), false));
            }
        });
    }

    fn select(&self, files: impl IntoIterator<Item = String>) {
        let set: HashSet<String> = files.into_iter().collect();
        self.selected.soft(|s| *s = Rc::new(set));
    }

    pub(super) fn select_all(&self) {
        let all = self.order();
        self.cursor.soft(|c| *c = all.last().cloned());
        self.anchor.soft(|a| *a = all.first().cloned());
        self.select(all);
    }

    pub(super) fn clear_selection(&self) {
        self.select([]);
    }

    pub(super) fn clear_filter(&self) {
        self.query.soft(String::clear);
        self.list_focus.request_focus();
    }

    pub(super) fn click(&self, file: &str) {
        self.list_focus.request_focus();
        let m = held();
        let toggle = if cfg!(target_os = "macos") { m.contains(Modifiers::META) } else { m.contains(Modifiers::CONTROL) };
        if m.contains(Modifiers::SHIFT) {
            self.extend_to(file);
        } else if toggle {
            let mut set = (**self.selected.peek()).clone();
            if !set.remove(file) {
                set.insert(file.to_owned());
            }
            self.select(set);
            self.anchor.soft(|a| *a = Some(file.to_owned()));
        } else {
            self.select([file.to_owned()]);
            self.anchor.soft(|a| *a = Some(file.to_owned()));
        }
        self.cursor.soft(|c| *c = Some(file.to_owned()));
    }

    fn extend_to(&self, file: &str) {
        let order = self.order();
        let anchor = self.anchor.got().flatten();
        if let Some((files, anchor)) = list::range(&order, anchor.as_deref(), file) {
            self.select(files);
            self.anchor.soft(|a| *a = Some(anchor));
        }
    }

    pub(super) fn step(&self, delta: i64, extend: bool) {
        let order = self.order();
        let cursor = self.cursor.got().flatten();
        if let Some(next) = list::step(&order, cursor.as_deref(), delta) {
            self.go_to(&next, extend);
        }
    }

    fn go_to(&self, file: &str, extend: bool) {
        if extend {
            self.extend_to(file);
        } else {
            self.select([file.to_owned()]);
            self.anchor.soft(|a| *a = Some(file.to_owned()));
        }
        self.cursor.soft(|c| *c = Some(file.to_owned()));
        self.show(file);
    }

    fn show(&self, file: &str) {
        let Some(index) = list::row_of(&self.data.peek(), &self.rows(), file) else { return };
        let top = index as f32 * ROW;
        let bottom = top + ROW;
        let view = *self.viewport.peek();
        let mut scroll = self.scroll;
        let from = -scroll.position().y;
        if top < from {
            scroll.scroll_to_y(-(top as i32));
        } else if view > 0. && bottom > from + view {
            scroll.scroll_to_y(-((bottom - view).ceil() as i32));
        }
    }

    pub(super) fn sort_by(&self, sort: Sort) {
        sort.remember();
        self.sort.soft(|s| *s = sort);
    }

    pub(super) fn key_down(&self, e: &KeyboardEventData, own: bool) -> bool {
        if self.store.ui.layered() || e.modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) {
            return false;
        }
        let shift = e.modifiers.contains(Modifiers::SHIFT);
        match &e.key {
            Key::Named(NamedKey::ArrowUp) => self.step(-1, shift),
            Key::Named(NamedKey::ArrowDown) => self.step(1, shift),
            Key::Named(NamedKey::PageUp) => self.step(-10, shift),
            Key::Named(NamedKey::PageDown) => self.step(10, shift),
            Key::Named(NamedKey::Home) => self.step(i64::MIN / 2, shift),
            Key::Named(NamedKey::End) => self.step(i64::MAX / 2, shift),
            Key::Named(NamedKey::Escape) if !self.selected.peek().is_empty() => self.clear_selection(),
            Key::Named(NamedKey::Escape) if !self.query.peek().is_empty() => self.clear_filter(),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => self.delete(self.targets()),
            Key::Named(NamedKey::Enter) if own => self.open_cursor_menu(),
            Key::Character(c) if own && c == " " => self.toggle_selected(),
            _ => return false,
        }
        true
    }

    fn open_cursor_menu(&self) {
        let Some(file) = self.cursor.got().flatten() else { return };
        let Some(index) = list::row_of(&self.data.peek(), &self.rows(), &file) else { return };
        let bounds = *self.bounds.peek();
        let y = (bounds.y + index as f32 * ROW + scroll_offset(self) + ROW).clamp(bounds.y, bounds.y + bounds.h);
        self.open_row_menu(&file, (bounds.x + space::XL, y));
    }

    fn toggle_selected(&self) {
        let files = self.targets();
        if files.is_empty() {
            return;
        }
        let data = self.data.peek().clone();
        let all_on = files.iter().all(|f| data.entry(f).is_some_and(|e| e.enabled));
        self.switch(files, !all_on);
    }

    pub(super) fn switch(&self, files: Vec<String>, on: bool) {
        let data = self.data.peek().clone();
        let files: Vec<String> = files.into_iter().filter(|f| data.entry(f).is_some_and(|e| e.enabled != on)).collect();
        if files.is_empty() || *self.busy.peek() {
            return;
        }
        if let Some(why) = self.locked() {
            self.note(why);
            return;
        }
        if !data.scanned {
            return;
        }
        if on {
            let missing = content::missing_for(&data.scan, &files);
            if !missing.is_empty() {
                self.ask_with(files, missing);
                return;
            }
        } else {
            let needed = content::needed_by(&data.scan, &files);
            if !needed.is_empty() {
                let shown: Vec<String> = needed.iter().map(|n| if n == "Manycraft" { tr("Manycrafts Mod") } else { n.clone() }).collect();
                let heading = match files.as_slice() {
                    [_] => trf("%@ ausschalten?", &[&data.label_of(&files)]),
                    many => trf("%lld Mods ausschalten?", &[&many.len()]),
                };
                let message = if shown.len() == 1 { trf("Dann startet %@ nicht mehr.", &[&list::listing(&shown)]) } else { trf("Dann starten %@ nicht mehr.", &[&list::listing(&shown)]) };
                let ctx = self.clone();
                sheet::confirm(self.store.ui, "instance.mods.off", heading, message, tr("Ausschalten"), false, move |_| ctx.apply_switch(files.clone(), false));
                return;
            }
        }
        self.apply_switch(files, on);
    }

    fn ask_with(&self, files: Vec<String>, missing: Vec<String>) {
        let data = self.data.peek().clone();
        let needed = data.names(&missing);
        let heading = match needed.as_slice() {
            [one] => trf("Auch „%@“ einschalten?", &[one]),
            many => trf("Auch %lld weitere Mods einschalten?", &[&many.len()]),
        };
        let message = match files.as_slice() {
            [_] => trf("Ohne %@ startet %@ nicht.", &[&list::listing(&needed), &data.label_of(&files)]),
            _ => trf("Ohne %@ starten diese Mods nicht.", &[&list::listing(&needed)]),
        };
        let only = match files.as_slice() {
            [_] => trf("Nur %@", &[&data.label_of(&files)]),
            _ => tr("Nur diese"),
        };
        let all = if files.len() + missing.len() == 2 { tr("Beide einschalten") } else { tr("Alle einschalten") };
        let both: Vec<String> = files.iter().chain(missing.iter()).cloned().collect();
        let (first, second) = (self.clone(), self.clone());
        let ask = Ask {
            heading,
            message,
            only,
            all,
            on_only: EventHandler::new(move |_| first.apply_switch(files.clone(), true)),
            on_all: EventHandler::new(move |_| second.apply_switch(both.clone(), true)),
        };
        sheet::open(self.store.ui, ask);
    }

    pub(super) fn apply_switch(&self, files: Vec<String>, on: bool) {
        if *self.busy.peek() || self.locked().is_some() {
            return;
        }
        let data = self.data.peek().clone();
        let label = data.label_of(&files);
        self.busy.soft(|b| *b = true);
        let _ = self.next_ticket();
        self.settle();
        self.data.soft(|d| *d = Rc::new(d.switched(&files, on)));
        let ctx = self.clone();
        spawn_in_window(async move {
            let id = ctx.id();
            let (work_id, work_files) = (id.clone(), files.clone());
            let result = off_ui(move || content::set_enabled(&work_id, &work_files, on)).await;
            ctx.busy.soft(|b| *b = false);
            match result {
                Some(Ok(made)) => {
                    let culprit = !on && ctx.bisect.got().flatten().is_some_and(|b| matches!(&b.outcome, Some(Outcome::Found(f)) if files.contains(f)));
                    ctx.put(made);
                    offer_switched(ctx.store, id, files, on, label);
                    if culprit {
                        ctx.bisect_step(super::trouble::Step::Dismiss);
                    }
                }
                Some(Err(e)) => ctx.fail(forms::explain_as(tr("Nicht gespeichert."), &e)),
                None => ctx.fail(Problem::plain(forms::unexplained())),
            }
            ctx.rescan();
        });
    }

    /// Asks before deleting `files`, the way the Delete key and the menu do.
    pub(super) fn delete(&self, files: Vec<String>) {
        if files.is_empty() {
            return;
        }
        if let Some(why) = self.locked() {
            self.note(why);
            return;
        }
        let label = self.data.peek().label_of(&files);
        let ctx = self.clone();
        sheet::confirm(self.store.ui, "instance.mods.delete", trf("%@ löschen?", &[&label]), tr("Das lässt sich gleich wieder rückgängig machen."), tr("Löschen"), true, move |_| ctx.trash(files.clone(), None));
    }

    pub(super) fn trash(&self, files: Vec<String>, label: Option<String>) {
        if files.is_empty() || *self.busy.peek() {
            return;
        }
        if let Some(why) = self.locked() {
            self.note(why);
            return;
        }
        let data = self.data.peek().clone();
        if !data.scanned {
            return;
        }
        let label = label.unwrap_or_else(|| data.label_of(&files));
        let next = list::after(&self.order(), &files);
        self.busy.soft(|b| *b = true);
        let _ = self.next_ticket();
        self.settle();
        let ctx = self.clone();
        spawn_in_window(async move {
            let id = ctx.id();
            let (work_id, work_files) = (id.clone(), files.clone());
            let result = off_ui(move || content::trash(&work_id, &work_files)).await;
            ctx.busy.soft(|b| *b = false);
            match result {
                Some(Ok((made, trashed))) => {
                    ctx.data.soft(|d| *d = Rc::new(d.without(&files)));
                    ctx.clear_selection();
                    ctx.cursor.soft(|c| *c = None);
                    if let Some(next) = next {
                        ctx.go_to(&next, false);
                    }
                    ctx.put(made);
                    offer_trashed(ctx.store, id, trashed, label);
                }
                Some(Err(e)) => ctx.fail(forms::explain_as(tr("Nicht gelöscht."), &e)),
                None => ctx.fail(Problem::plain(forms::unexplained())),
            }
            ctx.rescan();
        });
    }

    pub(super) fn fix(&self, file: &str) {
        let data = self.data.peek().clone();
        let Some(older) = data.analysis.older.get(file).cloned() else { return };
        let label = match older.as_slice() {
            [one] => match data.entry(one) {
                Some(e) => list::quoted(format!("{} {}", e.name(), e.version().unwrap_or_default()).trim_end()),
                None => list::quoted(one),
            },
            many => list::count_mods(many.len()),
        };
        self.trash(older, Some(label));
    }

    pub(super) fn fix_all(&self) {
        let mut older: Vec<String> = self.data.peek().analysis.older.values().flatten().cloned().collect();
        older.sort();
        older.dedup();
        self.trash(older, None);
    }

    pub(super) fn copy_list(&self) {
        let data = self.data.peek().clone();
        let Some(text) = list::copy_text(data.entries()) else { return };
        let n = data.entries().len();
        if native::copy_text(text) {
            self.note(if n == 1 { tr("Liste mit 1 Mod kopiert.") } else { trf("Liste mit %lld Mods kopiert.", &[&n]) });
        } else {
            self.note(tr("Die Liste ließ sich nicht kopieren."));
        }
    }

    pub(super) fn dropped(&self, paths: Vec<PathBuf>) {
        let (jars, other): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jar")));
        if !other.is_empty() {
            self.note(tr("Hier lassen sich nur .jar-Dateien ablegen."));
        }
        if jars.is_empty() {
            return;
        }
        self.settle();
        let added: Vec<String> = jars.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        let ctx = self.clone();
        spawn_in_window(async move {
            let id = ctx.id();
            let work_id = id.clone();
            match off_ui(move || content::add_files(&work_id, &jars)).await {
                Some(Ok((made, taken))) => {
                    let fresh: Vec<String> = added.into_iter().filter(|name| !taken.contains(name)).collect();
                    let first = fresh.first().cloned();
                    ctx.reveal.soft(|r| *r = first);
                    ctx.put(made);
                    if !taken.is_empty() {
                        ctx.note(trf("Schon da: %@.", &[&list::listing(&taken)]));
                    }
                    if !fresh.is_empty() {
                        let label = if let [one] = fresh.as_slice() { list::quoted(one) } else { list::count_mods(fresh.len()) };
                        offer_added(ctx.store, id, fresh, label);
                    }
                }
                Some(Err(e)) => ctx.fail(forms::explain_as(tr("Nicht hinzugefügt."), &e)),
                None => ctx.fail(Problem::plain(forms::unexplained())),
            }
            ctx.rescan();
        });
    }

    pub(super) fn open_row_menu(&self, file: &str, at: (f32, f32)) {
        if !self.selected.peek().contains(file) {
            self.select([file.to_owned()]);
            self.anchor.soft(|a| *a = Some(file.to_owned()));
        }
        self.cursor.soft(|c| *c = Some(file.to_owned()));
        self.list_focus.request_focus();
        menu::open(self.store.ui, Menu::at(self.row_menu(), at));
    }

    fn row_menu(&self) -> Vec<Item> {
        let data = self.data.peek().clone();
        let chosen = self.targets();
        let any_off = chosen.iter().any(|f| data.entry(f).is_some_and(|e| !e.enabled));
        let any_on = chosen.iter().any(|f| data.entry(f).is_some_and(|e| e.enabled));
        let dup = chosen.iter().any(|f| data.analysis.older.contains_key(f));
        let locked = self.locked().is_some();
        let mut items = Vec::new();
        if any_off {
            let (ctx, files) = (self.clone(), chosen.clone());
            items.push(Item::new("instance.mods.menu.on", tr("Einschalten"), move |_| ctx.switch(files.clone(), true)).disabled(locked));
        }
        if any_on {
            let (ctx, files) = (self.clone(), chosen.clone());
            items.push(Item::new("instance.mods.menu.off", tr("Ausschalten"), move |_| ctx.switch(files.clone(), false)).disabled(locked));
        }
        if dup {
            let ctx = self.clone();
            items.push(Item::new("instance.mods.menu.fix", tr("Doppelte Mods bereinigen"), move |_| ctx.fix_all()).disabled(locked));
        }
        let (copy, remove, files) = (self.clone(), self.clone(), chosen.clone());
        items.push(Item::Separator);
        items.push(Item::new("instance.mods.menu.copy", tr("Modliste kopieren"), move |_| copy.copy_list()));
        items.push(Item::Separator);
        let label = if chosen.len() > 1 { trf("%lld Mods löschen", &[&chosen.len()]) } else { tr("Löschen") };
        items.push(Item::new("instance.mods.menu.delete", label, move |_| remove.delete(files.clone())).danger().disabled(locked));
        items
    }

    pub(super) fn more_menu(&self, packs: &super::super::actions::Action, shaders: &super::super::actions::Action) -> Vec<Item> {
        let data = self.data.peek().clone();
        let bisecting = self.bisect.peek().is_some();
        let mut items = Vec::new();
        let ctx = self.clone();
        items.push(Item::new("instance.mods.more.copy", tr("Modliste kopieren"), move |_| ctx.copy_list()).disabled(data.entries().is_empty()));
        let ctx = self.clone();
        items.push(Item::new("instance.mods.more.bisect", tr("Fehlerhaften Mod finden…"), move |_| ctx.ask_bisect()).disabled(bisecting));
        if data.analysis.dups > 0 {
            let ctx = self.clone();
            items.push(Item::Separator);
            items.push(Item::new("instance.mods.more.fix", tr("Doppelte Mods bereinigen"), move |_| ctx.fix_all()).disabled(self.locked().is_some()));
        }
        items.push(Item::Separator);
        for (id, action) in [("instance.mods.more.packs", packs), ("instance.mods.more.shaders", shaders)] {
            let run = action.run.clone();
            items.push(Item::new(id, action.label.clone(), move |_| run.call(())).disabled(action.disabled.is_some()));
        }
        items
    }

    fn why_empty(&self) -> Option<String> {
        self.data.peek().entries().is_empty().then(|| tr("Noch keine Mods."))
    }

    fn why_none(&self) -> Option<String> {
        self.targets().is_empty().then(|| tr("Wähle zuerst Mods aus.")).or_else(|| self.locked())
    }

    fn why_updates(&self) -> Option<String> {
        self.store.model.peek().instance(&self.id).is_some_and(|i| i.mods.is_empty()).then(|| tr("Keine Mods von Modrinth."))
    }

    fn why_bisect(&self) -> Option<String> {
        if self.bisect.peek().is_some() { Some(tr("Die Fehlersuche läuft schon.")) } else { self.why_empty() }
    }

    /// The palette entries and shortcuts of the tab; why one is blocked is worked out when it runs.
    pub(super) fn extras(&self) -> Vec<Extra> {
        let dups = self.data.read().analysis.dups;
        let current = *self.sort.read();
        let mut out = Vec::new();
        let mut push = |key: &str, label: String, glyph: Icon, shortcut: Option<Shortcut>, blocked: fn(&Ctx) -> Option<String>, run: Box<dyn Fn(&Ctx)>| {
            let ctx = self.clone();
            out.push(Extra {
                id: format!("instance.mods.{key}"),
                label,
                glyph,
                shortcut,
                run: EventHandler::new(move |_| match blocked(&ctx) {
                    Some(why) => ctx.note(why),
                    None => run(&ctx),
                }),
            });
        };
        let focus = self.filter_focus;
        push("filter", tr("Mods filtern"), Icon::Search, Some(Shortcut::primary(Code::KeyF)), |_| None, Box::new(move |_| focus.request_focus()));
        push("select", tr("Alle Mods auswählen"), Icon::Check, Some(Shortcut::primary(Code::KeyA)), Ctx::why_empty, Box::new(Ctx::select_all));
        push("enable", tr("Ausgewählte Mods einschalten"), Icon::Check, None, Ctx::why_none, Box::new(|c: &Ctx| c.switch(c.targets(), true)));
        push("disable", tr("Ausgewählte Mods ausschalten"), Icon::Minus, None, Ctx::why_none, Box::new(|c: &Ctx| c.switch(c.targets(), false)));
        push("delete", tr("Ausgewählte Mods löschen"), Icon::Close, None, Ctx::why_none, Box::new(|c: &Ctx| c.delete(c.targets())));
        push("updates", tr("Mod-Updates prüfen…"), Icon::Refresh, None, Ctx::why_updates, Box::new(Ctx::check_updates));
        push("copy", tr("Modliste kopieren"), Icon::Copy, None, Ctx::why_empty, Box::new(Ctx::copy_list));
        push("bisect", tr("Fehlerhaften Mod finden…"), Icon::Alert, None, Ctx::why_bisect, Box::new(Ctx::ask_bisect));
        if dups > 0 {
            push("fix", tr("Doppelte Mods bereinigen"), Icon::Layers, None, |c| c.locked(), Box::new(Ctx::fix_all));
        }
        for (sort, label) in [(Sort::Name, tr("Mods nach Name sortieren")), (Sort::Added, tr("Mods nach Hinzufügedatum sortieren")), (Sort::Updated, tr("Mods nach Aktualisierungsdatum sortieren"))] {
            if sort != current {
                push(&format!("sort.{sort:?}").to_lowercase(), label, Icon::Rows, None, |_| None, Box::new(move |c: &Ctx| c.sort_by(sort)));
            }
        }
        out
    }

    pub(super) fn check_updates(&self) {
        super::updates::open(self.store, &self.id);
    }
}

fn scroll_offset(ctx: &Ctx) -> f32 {
    ctx.scroll.position().y
}
