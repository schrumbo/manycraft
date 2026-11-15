//! Undo history and its notices: one offer per account removal, deleted or cloned instance and removed mod. A notice
//! hides after a while; its offer stays reachable with Ctrl+Z and the palette until Manycraft quits.

use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;

use super::accounts::{self, Pending};
use super::actions::Undo;
use super::drive::Mark;
use super::remote::InstanceEdit;
use super::sheet::{self, Look};
use super::theme::{Theme, font, radius, shell, space, tint, with_alpha};
use super::{forms, main_window, model, remote, state};
use crate::core::batch::ModRef;
use crate::core::i18n::{tr, trf};

const MAX_TOASTS: usize = 3;
const MAX_OFFERS: usize = 50;

/// What an undo notice brings back.
pub enum Kind {
    Deleted { id: String, name: String, entry: String },
    Cloned { id: String, name: String, source: String },
    ModRemoved { instance_id: String, mod_ref: ModRef, index: usize, base: InstanceEdit },
}

struct Offer {
    kind: Kind,
    failed: bool,
    at: Instant,
    shown: bool,
    hovered: bool,
    round: u64,
}

#[derive(Clone, Copy)]
enum Target {
    Account(u64),
    Instance(u64),
}

struct Line {
    at: Instant,
    target: Target,
    key: String,
    name: String,
    text: String,
    failed: bool,
    shown: bool,
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
}

/// The entity the main window draws.
pub fn toasts(cx: &mut App) -> Entity<UndoToasts> {
    if let Some(toasts) = cx.try_global::<Toasts>() {
        return toasts.0.clone();
    }
    let toasts = cx.new(|_| UndoToasts { items: Pending::new() });
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
    push(kind, false, cx);
}

/// Offers `kind` again after an undo failed, with a retry in its notice.
pub fn retry(kind: Kind, cx: &mut App) {
    push(kind, true, cx);
}

fn push(kind: Kind, failed: bool, cx: &mut App) {
    let id = toasts(cx).update(cx, |t, cx| {
        let id = t.items.push(Offer { kind, failed, at: Instant::now(), shown: true, hovered: false, round: 0 });
        if t.items.items.len() > MAX_OFFERS {
            t.items.items.remove(0);
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
    }
}

fn lines(items: &Pending<Offer>, cx: &App) -> Vec<Line> {
    let mut lines: Vec<Line> = accounts::removals(cx)
        .into_iter()
        .map(|r| {
            let text = if r.failed { trf("„%@“ wurde nicht zurückgeholt.", &[&r.name]) } else { trf("„%@“ entfernt.", &[&r.name]) };
            Line { at: r.at, target: Target::Account(r.id), key: format!("remove-{}", r.id), text, name: r.name, failed: r.failed, shown: r.shown }
        })
        .collect();
    for (id, offer) in &items.items {
        let (name, done, undone) = match &offer.kind {
            Kind::Deleted { name, .. } => (name.clone(), trf("„%@“ gelöscht.", &[name]), trf("„%@“ wurde nicht zurückgeholt.", &[name])),
            Kind::Cloned { name, .. } => (name.clone(), trf("„%@“ geklont.", &[name]), trf("„%@“ wurde nicht gelöscht.", &[name])),
            Kind::ModRemoved { mod_ref, .. } => (mod_ref.title.clone(), trf("„%@“ entfernt.", &[&mod_ref.title]), trf("„%@“ wurde nicht zurückgeholt.", &[&mod_ref.title])),
        };
        let text = if offer.failed { undone } else { done };
        lines.push(Line { at: offer.at, target: Target::Instance(*id), key: format!("offer-{id}"), text, name, failed: offer.failed, shown: offer.shown });
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
    }
}

fn undo_offer(id: u64, cx: &mut App) {
    let taken = toasts(cx).update(cx, |t, cx| {
        cx.notify();
        t.items.take(id)
    });
    let Some(offer) = taken else { return };
    match offer.kind {
        Kind::Deleted { id, name, entry } => restore(id, name, entry, cx),
        Kind::Cloned { id, name, source } => discard(id, name, source, cx),
        Kind::ModRemoved { instance_id, mod_ref, index, base } => super::instance_page::restore_mod(&instance_id, mod_ref, index, base, cx),
    }
}

fn restore(id: String, name: String, entry: String, cx: &mut App) {
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
                    state::tell(forms::explain_as(tr("Nicht zurückgeholt."), &e).text, cx);
                    retry(Kind::Deleted { id, name, entry }, cx);
                }
            }
            state::reload(cx);
        });
    })
    .detach();
}

fn discard(id: String, name: String, source: String, cx: &mut App) {
    let (exists, running) = {
        let m = model::get(cx).read(cx);
        (m.instance(&id).is_some(), m.games.values().any(|g| g.instance_id == id && g.phase.is_active()))
    };
    if !exists {
        state::tell(tr("Diese Instanz gibt es nicht mehr."), cx);
        state::reload(cx);
        return;
    }
    if running {
        state::tell(tr("Gerade laufen Spiele. Stoppe sie zuerst."), cx);
        retry(Kind::Cloned { id, name, source }, cx);
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
                    state::tell(forms::explain_as(tr("Nicht gelöscht."), &e).text, cx);
                    retry(Kind::Cloned { id, name, source }, cx);
                }
            }
            state::reload(cx);
        });
    })
    .detach();
}

/// One notice: its text, the button that undoes it or retries after a failed undo, and a close button.
pub(super) fn toast(key: &str, name: String, text: String, failed: bool, on_undo: impl Fn(&mut App) + 'static, on_close: impl Fn(&mut App) + 'static, on_hover: impl Fn(bool, &mut App) + 'static, t: &Theme) -> Stateful<Div> {
    let label = if failed { tr("Erneut versuchen") } else { tr("Rückgängig") };
    let undo = sheet::button(SharedString::from(format!("undo-{key}")), label, Some("undo-2"), Look::Plain, false, false, t).on_click(move |_, _, cx| on_undo(cx));
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
        .child(undo)
        .child(close)
}

impl Render for UndoToasts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        let shown: Vec<Line> = lines(&self.items, cx).into_iter().filter(|l| l.shown).collect();
        let skip = shown.len().saturating_sub(MAX_TOASTS);
        let toasts: Vec<Stateful<Div>> = shown
            .into_iter()
            .skip(skip)
            .map(|l| {
                let target = l.target;
                toast(&l.key, l.name, l.text, l.failed, move |cx| undo(target, cx), move |cx| close(target, cx), move |on, cx| hover(target, on, cx), &t)
            })
            .collect();
        div().w_full().px(px(space::XL)).flex().flex_col().items_center().gap(px(space::S)).children(toasts)
    }
}
