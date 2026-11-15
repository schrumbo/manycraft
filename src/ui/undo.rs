//! Undo history and its notices: one offer per account removal, deleted, cloned, reset or changed instance and removed mod. A notice
//! hides after a while; its offer stays reachable with Ctrl+Z and the palette until Manycraft quits.

use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;

use super::accounts::{self, Pending};
use super::actions::Undo;
use super::drive::Mark;
use super::remote::InstanceEdit;
use crate::core::instance::Instance;
use super::sheet::{self, Look};
use super::theme::{Theme, font, radius, shell, space, tint, with_alpha};
use super::{forms, main_window, model, remote, state};
use crate::core::content::Trashed;
use crate::core::modrinth::Update;
use crate::core::i18n::{tr, trf};

const MAX_TOASTS: usize = 3;
const MAX_OFFERS: usize = 50;

/// What a change of an instance's name, version or loader was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Renamed,
    /// Moved to another game version or loader; carries what it moved to, like "Minecraft 26.3" or "Quilt".
    Switched(String),
    /// Moved to a newer loader; carries it, like "Fabric 0.18.1".
    Updated(String),
}

/// What an undo notice brings back.
pub enum Kind {
    Deleted { id: String, name: String, entry: String },
    Cloned { id: String, name: String, source: String },
    ModsSwitched { instance_id: String, files: Vec<String>, on: bool, label: String },
    ModsTrashed { instance_id: String, trashed: Trashed, label: String },
    ModsUpdated { instance_id: String, updates: Vec<Update>, label: String },
    Edited { instance_id: String, name: String, what: Edit, before: InstanceEdit, after: InstanceEdit },
    Reset { instance_id: String, name: String, entry: String, before: Instance, after: Instance },
}

struct Offer {
    kind: Kind,
    failed: bool,
    at: Instant,
    shown: bool,
    hovered: bool,
    round: u64,
}

struct Note {
    id: u64,
    text: String,
    at: Instant,
    hovered: bool,
    round: u64,
}

#[derive(Clone, Copy)]
enum Target {
    Account(u64),
    Instance(u64),
    Note(u64),
}

struct Line {
    at: Instant,
    target: Target,
    key: String,
    name: String,
    text: String,
    failed: bool,
    shown: bool,
    hovered: bool,
}

struct Toasts(Entity<UndoToasts>);

impl Global for Toasts {}

/// Registers Ctrl+Z, which undoes the newest offer of any kind.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &Undo, cx| undo_newest(cx));
}

/// The undo notices: a stack above the content, bottom centre, that never moves the content under it.
pub struct UndoToasts {
    items: Pending<Offer>,
    notes: Vec<Note>,
    next_note: u64,
}

/// The entity the main window draws.
pub fn toasts(cx: &mut App) -> Entity<UndoToasts> {
    if let Some(toasts) = cx.try_global::<Toasts>() {
        return toasts.0.clone();
    }
    let toasts = cx.new(|_| UndoToasts { items: Pending::new(), notes: Vec::new(), next_note: 0 });
    cx.set_global(Toasts(toasts.clone()));
    toasts
}

/// Draws the notices again, after the account removals changed.
pub fn changed(cx: &mut App) {
    if let Some(toasts) = cx.try_global::<Toasts>().map(|t| t.0.clone()) {
        toasts.update(cx, |_, cx| cx.notify());
    }
}

/// Offers to undo `kind` until Manycraft quits; its notice hides after a while.
pub fn offer(kind: Kind, cx: &mut App) {
    push(kind, false, Instant::now(), cx);
}

/// Offers `kind` again after an undo failed, with a retry in its notice; it keeps its place in the history, which `at` was.
pub fn retry(kind: Kind, at: Instant, cx: &mut App) {
    push(kind, true, at, cx);
}

/// Shows `text` in the notice stack for a while, without an undo: why an undo or a disabled control did not work.
pub fn note(text: String, cx: &mut App) {
    let id = toasts(cx).update(cx, |t, cx| {
        t.next_note += 1;
        let id = t.next_note;
        t.notes.push(Note { id, text, at: Instant::now(), hovered: false, round: 0 });
        cx.notify();
        id
    });
    arm_note(id, cx);
}

fn arm_note(id: u64, cx: &mut App) {
    let round = toasts(cx).update(cx, |t, _| {
        t.notes.iter_mut().find(|n| n.id == id).map(|n| {
            n.round += 1;
            n.round
        })
    });
    let Some(round) = round else { return };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(accounts::UNDO_FOR).await;
        cx.update(|cx| {
            toasts(cx).update(cx, |t, cx| {
                let before = t.notes.len();
                t.notes.retain(|n| n.id != id || n.round != round || n.hovered);
                if t.notes.len() != before {
                    cx.notify();
                }
            })
        });
    })
    .detach();
}

/// Whether `e` is a refusal no retry changes: the thing is gone, taken, damaged or changed since.
pub(super) fn permanent(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<forms::Plain>() || c.is::<crate::core::instance::Final>())
}

fn slot(ats: impl Iterator<Item = Instant>, at: Instant) -> usize {
    ats.take_while(|a| *a <= at).count()
}

fn evict(items: &[(u64, Offer)]) -> Option<usize> {
    let capped = |o: &Offer| !matches!(o.kind, Kind::Deleted { .. } | Kind::Reset { .. });
    if items.iter().filter(|(_, o)| capped(o)).count() <= MAX_OFFERS {
        return None;
    }
    items.iter().position(|(_, o)| capped(o))
}

fn push(kind: Kind, failed: bool, at: Instant, cx: &mut App) {
    let id = toasts(cx).update(cx, |t, cx| {
        let place = slot(t.items.items.iter().map(|(_, o)| o.at), at);
        let id = t.items.insert(place, Offer { kind, failed, at, shown: true, hovered: false, round: 0 });
        if let Some(oldest) = evict(&t.items.items) {
            t.items.items.remove(oldest);
        }
        cx.notify();
        id
    });
    arm(id, cx);
}

fn arm(id: u64, cx: &mut App) {
    let round = toasts(cx).update(cx, |t, _| {
        t.items.items.iter_mut().find(|(i, _)| *i == id).map(|(_, o)| {
            o.round += 1;
            o.round
        })
    });
    let Some(round) = round else { return };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(accounts::UNDO_FOR).await;
        cx.update(|cx| hide_later(id, round, cx));
    })
    .detach();
}

fn hide_later(id: u64, round: u64, cx: &mut App) {
    toasts(cx).update(cx, |t, cx| {
        if let Some((_, o)) = t.items.items.iter_mut().find(|(i, o)| *i == id && o.round == round && !o.hovered) {
            o.shown = false;
            cx.notify();
        }
    });
}

fn hover(target: Target, on: bool, cx: &mut App) {
    match target {
        Target::Account(id) => accounts::hover_removal(id, on, cx),
        Target::Instance(id) => {
            let changed = toasts(cx).update(cx, |t, _| match t.items.items.iter_mut().find(|(i, _)| *i == id) {
                Some((_, o)) if o.hovered != on => {
                    o.hovered = on;
                    o.round += 1;
                    true
                }
                _ => false,
            });
            if changed && !on {
                arm(id, cx);
            }
        }
        Target::Note(id) => {
            let changed = toasts(cx).update(cx, |t, _| match t.notes.iter_mut().find(|n| n.id == id) {
                Some(n) if n.hovered != on => {
                    n.hovered = on;
                    n.round += 1;
                    true
                }
                _ => false,
            });
            if changed && !on {
                arm_note(id, cx);
            }
        }
    }
}

fn close(target: Target, cx: &mut App) {
    match target {
        Target::Account(id) => accounts::hide_removal(id, cx),
        Target::Instance(id) => toasts(cx).update(cx, |t, cx| {
            if let Some((_, o)) = t.items.items.iter_mut().find(|(i, _)| *i == id) {
                o.shown = false;
                o.hovered = false;
                cx.notify();
            }
        }),
        Target::Note(id) => toasts(cx).update(cx, |t, cx| {
            t.notes.retain(|n| n.id != id);
            cx.notify();
        }),
    }
}

fn lines(items: &Pending<Offer>, cx: &App) -> Vec<Line> {
    let mut lines: Vec<Line> = accounts::removals(cx)
        .into_iter()
        .map(|r| {
            let text = if r.failed { trf("„%@“ wurde nicht zurückgeholt.", &[&r.name]) } else { trf("„%@“ entfernt.", &[&r.name]) };
            Line { at: r.at, target: Target::Account(r.id), key: format!("remove-{}", r.id), text, name: r.name, failed: r.failed, shown: r.shown, hovered: r.hovered }
        })
        .collect();
    for (id, offer) in &items.items {
        let (name, done, undone) = match &offer.kind {
            Kind::Deleted { name, .. } => (name.clone(), trf("„%@“ gelöscht.", &[name]), trf("„%@“ wurde nicht zurückgeholt.", &[name])),
            Kind::Cloned { name, .. } => (name.clone(), trf("„%@“ geklont.", &[name]), trf("„%@“ wurde nicht gelöscht.", &[name])),
            Kind::ModsSwitched { label, on: true, .. } => (label.clone(), trf("%@ eingeschaltet.", &[label]), trf("%@ wurde nicht zurückgestellt.", &[label])),
            Kind::ModsSwitched { label, on: false, .. } => (label.clone(), trf("%@ ausgeschaltet.", &[label]), trf("%@ wurde nicht zurückgestellt.", &[label])),
            Kind::ModsTrashed { label, .. } => (label.clone(), trf("%@ gelöscht.", &[label]), trf("%@ wurde nicht zurückgeholt.", &[label])),
            Kind::ModsUpdated { label, .. } => (label.clone(), trf("%@ aktualisiert.", &[label]), trf("%@ wurde nicht zurückgestellt.", &[label])),
            Kind::Reset { name, .. } => (name.clone(), trf("„%@“ zurückgesetzt.", &[name]), trf("„%@“ wurde nicht zurückgeholt.", &[name])),
            Kind::Edited { name, what, .. } => {
                let done = match what {
                    Edit::Renamed => trf("Umbenannt in „%@“.", &[name]),
                    Edit::Switched(to) => trf("„%@“ auf %@ umgestellt.", &[name, to]),
                    Edit::Updated(to) => trf("„%@“ auf %@ aktualisiert.", &[name, to]),
                };
                let undone = if *what == Edit::Renamed { trf("„%@“ wurde nicht zurückbenannt.", &[name]) } else { trf("„%@“ wurde nicht zurückgestellt.", &[name]) };
                (name.clone(), done, undone)
            }
        };
        let text = if offer.failed { undone } else { done };
        lines.push(Line { at: offer.at, target: Target::Instance(*id), key: format!("offer-{id}"), text, name, failed: offer.failed, shown: offer.shown, hovered: offer.hovered });
    }
    lines.sort_by_key(|l| l.at);
    lines
}

fn current(cx: &App) -> Vec<Line> {
    match cx.try_global::<Toasts>() {
        Some(toasts) => lines(&toasts.0.read(cx).items, cx),
        None => lines(&Pending::new(), cx),
    }
}

/// The text of the offer Ctrl+Z would undo now.
pub fn newest(cx: &App) -> Option<String> {
    current(cx).pop().map(|l| l.text)
}

fn undo_newest(cx: &mut App) {
    if let Some(line) = current(cx).pop() {
        undo(line.target, cx);
    }
}

fn undo(target: Target, cx: &mut App) {
    match target {
        Target::Account(id) => accounts::undo_removal(id, cx),
        Target::Instance(id) => undo_offer(id, cx),
        Target::Note(_) => {}
    }
}

fn undo_offer(id: u64, cx: &mut App) {
    let taken = toasts(cx).update(cx, |t, cx| {
        cx.notify();
        t.items.take(id)
    });
    let Some(offer) = taken else { return };
    let at = offer.at;
    match offer.kind {
        Kind::Deleted { id, name, entry } => restore(id, name, entry, at, cx),
        Kind::Cloned { id, name, source } => discard(id, name, source, at, cx),
        Kind::ModsSwitched { instance_id, files, on, label } => super::mod_list::undo_switch(instance_id, files, on, label, at, cx),
        Kind::ModsTrashed { instance_id, trashed, label } => super::mod_list::undo_trash(instance_id, trashed, label, at, cx),
        Kind::ModsUpdated { instance_id, updates, label } => super::mod_updates::undo_updates(instance_id, updates, label, at, cx),
        Kind::Edited { instance_id, name, what, before, after } => super::maintenance::restore_edit(&instance_id, name, what, before, after, at, cx),
        Kind::Reset { instance_id, name, entry, before, after } => super::maintenance::restore_reset(&instance_id, name, entry, before, after, at, cx),
    }
}

fn restore(id: String, name: String, entry: String, at: Instant, cx: &mut App) {
    let task = cx.background_spawn({
        let entry = entry.clone();
        async move { remote::restore_instance(&entry) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(made) => {
                    let id = made.id.clone();
                    model::get(cx).update(cx, |m, cx| {
                        if !m.instances.iter().any(|i| i.id == made.id) {
                            m.instances.push(made);
                            m.instances.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
                        }
                        cx.notify();
                    });
                    main_window::show_instance(&id, cx);
                }
                Err(e) => {
                    note(forms::explain_as(tr("Nicht zurückgeholt."), &e).text, cx);
                    if !permanent(&e) {
                        retry(Kind::Deleted { id, name, entry }, at, cx);
                    }
                }
            }
            state::reload(cx);
        });
    })
    .detach();
}

fn discard(id: String, name: String, source: String, at: Instant, cx: &mut App) {
    let (exists, running) = {
        let m = model::get(cx).read(cx);
        (m.instance(&id).is_some(), m.games.values().any(|g| g.instance_id == id && g.phase.is_active()))
    };
    if !exists {
        note(tr("Diese Instanz gibt es nicht mehr."), cx);
        state::reload(cx);
        return;
    }
    if running {
        note(tr("Gerade laufen Spiele. Stoppe sie zuerst."), cx);
        retry(Kind::Cloned { id, name, source }, at, cx);
        state::reload(cx);
        return;
    }
    let task = cx.background_spawn({
        let id = id.clone();
        async move { remote::discard_instance(&id) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(_) => {
                    model::get(cx).update(cx, |m, cx| {
                        m.instances.retain(|made| made.id != id);
                        cx.notify();
                    });
                    main_window::leave_instance(&id, &source, cx);
                }
                Err(e) => {
                    note(forms::explain_as(tr("Nicht gelöscht."), &e).text, cx);
                    if !permanent(&e) {
                        retry(Kind::Cloned { id, name, source }, at, cx);
                    }
                }
            }
            state::reload(cx);
        });
    })
    .detach();
}

/// One notice: its text, the button that undoes it or retries after a failed undo, and a close button.
pub(super) fn toast(key: &str, name: String, text: String, failed: bool, on_undo: Option<Box<dyn Fn(&mut App)>>, on_close: impl Fn(&mut App) + 'static, on_hover: impl Fn(bool, &mut App) + 'static, t: &Theme) -> Stateful<Div> {
    let label = if failed { tr("Erneut versuchen") } else { tr("Rückgängig") };
    let undo = on_undo.map(|on_undo| sheet::button(SharedString::from(format!("undo-{key}")), label, Some("undo-2"), Look::Plain, false, false, t).on_click(move |_, _, cx| on_undo(cx)));
    let close = sheet::icon_button(SharedString::from(format!("undo-close-{key}")), "x", tr("Schließen"), t.tertiary, t).on_click(move |_, _, cx| on_close(cx));
    div()
        .id(SharedString::from(format!("undo-note-{key}")))
        .mark("status", name)
        .occlude()
        .max_w(px(shell::TOAST))
        .flex()
        .items_center()
        .gap(px(space::S))
        .pl(px(space::M))
        .pr(px(space::XS))
        .py(px(space::XS))
        .rounded(px(radius::CONTROL))
        .bg(t.content)
        .border_1()
        .border_color(if failed { with_alpha(t.danger, tint::ATTENTION) } else { t.separator })
        .shadow_md()
        .on_hover(move |on, _, cx| on_hover(*on, cx))
        .child(div().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(if failed { t.danger_text } else { t.primary }).child(text))
        .children(undo)
        .child(close)
}

impl Render for UndoToasts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        let mut shown: Vec<Line> = lines(&self.items, cx).into_iter().filter(|l| l.shown).collect();
        shown.extend(self.notes.iter().map(|n| Line { at: n.at, target: Target::Note(n.id), key: format!("note-{}", n.id), name: n.text.clone(), text: n.text.clone(), failed: true, shown: true, hovered: n.hovered }));
        shown.sort_by_key(|l| l.at);
        let skip = shown.len().saturating_sub(MAX_TOASTS);
        let released: Vec<Target> = shown.iter().take(skip).filter(|l| l.hovered).map(|l| l.target).collect();
        if !released.is_empty() {
            cx.defer(move |cx| {
                for target in released {
                    hover(target, false, cx);
                }
            });
        }
        let toasts: Vec<Stateful<Div>> = shown
            .into_iter()
            .skip(skip)
            .map(|l| {
                let target = l.target;
                let on_undo: Option<Box<dyn Fn(&mut App)>> = match target {
                    Target::Note(_) => None,
                    _ => Some(Box::new(move |cx| undo(target, cx))),
                };
                toast(&l.key, l.name, l.text, l.failed, on_undo, move |cx| close(target, cx), move |on, cx| hover(target, on, cx), &t)
            })
            .collect();
        div().w_full().px(px(space::XL)).flex().flex_col().items_center().gap(px(space::S)).children(toasts)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Kind, MAX_OFFERS, Offer, evict, forms, permanent, slot};

    fn offer_of(kind: Kind) -> (u64, Offer) {
        (0, Offer { kind, failed: false, at: Instant::now(), shown: true, hovered: false, round: 0 })
    }

    fn cloned() -> Offer {
        offer_of(Kind::Cloned { id: "a".into(), name: "A".into(), source: "b".into() }).1
    }

    fn deleted() -> Offer {
        offer_of(Kind::Deleted { id: "a".into(), name: "A".into(), entry: "a-1".into() }).1
    }

    #[test]
    fn a_retried_offer_goes_back_to_its_place_by_time() {
        let base = Instant::now();
        let ats: Vec<Instant> = [1, 2, 4, 5].iter().map(|s| base + Duration::from_secs(*s)).collect();
        assert_eq!(slot(ats.iter().copied(), base + Duration::from_secs(3)), 2);
        assert_eq!(slot(ats.iter().copied(), base), 0);
        assert_eq!(slot(ats.iter().copied(), base + Duration::from_secs(9)), 4);
        assert_eq!(slot(ats.iter().copied(), ats[1]), 2);
    }

    #[test]
    fn the_cap_evicts_the_oldest_offer_that_is_not_a_deletion() {
        let mut items: Vec<(u64, Offer)> = vec![(0, deleted()), (1, deleted())];
        items.extend((0..MAX_OFFERS).map(|i| (i as u64 + 2, cloned())));
        assert_eq!(evict(&items), None);
        items.push((99, cloned()));
        assert_eq!(evict(&items), Some(2));
        items.retain(|(_, o)| matches!(o.kind, Kind::Deleted { .. }));
        items.extend((0..MAX_OFFERS * 2).map(|_| (0, deleted())));
        assert_eq!(evict(&items), None);
    }

    fn reset() -> Offer {
        let made = crate::core::instance::Instance::new("A", "26.2", crate::core::instance::Loader::Vanilla);
        offer_of(Kind::Reset { instance_id: "a".into(), name: "A".into(), entry: "a-1.reset".into(), before: made.clone(), after: made }).1
    }

    #[test]
    fn a_reset_is_kept_like_a_deletion_while_edits_count_against_the_cap() {
        let mut items: Vec<(u64, Offer)> = vec![(0, reset())];
        items.extend((0..MAX_OFFERS).map(|i| (i as u64 + 1, cloned())));
        items.push((99, cloned()));
        assert_eq!(evict(&items), Some(1));
        items.retain(|(_, o)| matches!(o.kind, Kind::Reset { .. }));
        items.extend((0..MAX_OFFERS * 2).map(|_| (0, reset())));
        assert_eq!(evict(&items), None);
    }

    #[test]
    fn only_refusals_that_no_retry_changes_are_permanent() {
        let plain = anyhow::Error::new(forms::Plain("gone".into())).context("saving");
        let fixed = anyhow::Error::new(crate::core::instance::Final("taken".into()));
        let io = anyhow::Error::new(std::io::Error::other("disk")).context("cannot move");
        let busy = anyhow::anyhow!("another Manycraft is using it");
        assert!(permanent(&plain));
        assert!(permanent(&fixed));
        assert!(!permanent(&io));
        assert!(!permanent(&busy));
    }
}
