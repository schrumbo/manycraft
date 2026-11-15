//! Undo notices over the main window: one per account removal, deleted or cloned instance and removed mod, each with its own timer.

use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;

use super::accounts::{self, Pending};
use super::actions::Undo;
use super::drive::Mark;
use super::sheet::{self, Look};
use super::theme::{Theme, font, radius, shell, space, tint, with_alpha};
use super::{forms, main_window, model, remote, state};
use crate::core::batch::ModRef;
use crate::core::i18n::{tr, trf};

const MAX_TOASTS: usize = 3;

/// What an undo notice brings back.
pub enum Kind {
    Deleted { id: String, name: String },
    Cloned { id: String, name: String, source: String },
    ModRemoved { instance_id: String, mod_ref: ModRef, index: usize },
}

struct Offer {
    kind: Kind,
    failed: bool,
    at: Instant,
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
}

struct Toasts(Entity<UndoToasts>);

impl Global for Toasts {}

/// Registers Ctrl+Z, which undoes the newest notice of any kind.
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

/// Offers to undo `kind` for a while.
pub fn offer(kind: Kind, cx: &mut App) {
    push(kind, false, cx);
}

fn push(kind: Kind, failed: bool, cx: &mut App) {
    let id = toasts(cx).update(cx, |t, cx| {
        let id = t.items.push(Offer { kind, failed, at: Instant::now() });
        cx.notify();
        id
    });
    cx.spawn(async move |cx| {
        cx.background_executor().timer(accounts::UNDO_FOR).await;
        cx.update(|cx| expire(id, cx));
    })
    .detach();
}

fn expire(id: u64, cx: &mut App) {
    let gone = toasts(cx).update(cx, |t, cx| {
        let gone = t.items.take(id);
        if gone.is_some() {
            cx.notify();
        }
        gone
    });
    if let Some(Offer { kind: Kind::Deleted { id, .. }, .. }) = gone {
        cx.background_spawn(async move {
            if let Err(e) = remote::purge_instance(&id) {
                eprintln!("trash: {e:#}");
            }
        })
        .detach();
    }
}

fn text(name: &str, failed: bool, done: String) -> String {
    if failed { trf("„%@“ wurde nicht zurückgeholt.", &[&name]) } else { done }
}

fn lines(items: &Pending<Offer>, cx: &App) -> Vec<Line> {
    let mut lines: Vec<Line> = accounts::removals(cx)
        .into_iter()
        .map(|r| Line { at: r.at, target: Target::Account(r.id), key: format!("remove-{}", r.id), text: text(&r.name, r.failed, trf("„%@“ entfernt.", &[&r.name])), name: r.name, failed: r.failed })
        .collect();
    for (id, offer) in &items.items {
        let (name, done) = match &offer.kind {
            Kind::Deleted { name, .. } => (name.clone(), trf("„%@“ gelöscht.", &[name])),
            Kind::Cloned { name, .. } => (name.clone(), trf("„%@“ angelegt.", &[name])),
            Kind::ModRemoved { mod_ref, .. } => (mod_ref.title.clone(), trf("„%@“ entfernt.", &[&mod_ref.title])),
        };
        lines.push(Line { at: offer.at, target: Target::Instance(*id), key: format!("offer-{id}"), text: text(&name, offer.failed, done), name, failed: offer.failed });
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

/// The text of the notice Ctrl+Z would undo now.
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
        Kind::Deleted { id, name } => restore(id, name, cx),
        Kind::Cloned { id, source, .. } => discard(id, source, cx),
        Kind::ModRemoved { instance_id, mod_ref, index } => super::instance_page::restore_mod(&instance_id, mod_ref, index, cx),
    }
}

fn restore(id: String, name: String, cx: &mut App) {
    let task = cx.background_spawn({
        let id = id.clone();
        async move { remote::restore_instance(&id) }
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
                    push(Kind::Deleted { id, name }, true, cx);
                }
            }
            state::reload(cx);
        });
    })
    .detach();
}

fn discard(id: String, source: String, cx: &mut App) {
    let task = cx.background_spawn({
        let id = id.clone();
        async move { remote::discard_instance(&id) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            match result {
                Ok(()) => {
                    model::get(cx).update(cx, |m, cx| {
                        m.instances.retain(|made| made.id != id);
                        cx.notify();
                    });
                    main_window::leave_instance(&id, &source, cx);
                }
                Err(e) => state::tell(forms::explain_as(tr("Nicht gelöscht."), &e).text, cx),
            }
            state::reload(cx);
        });
    })
    .detach();
}

/// One notice: its text and the button that undoes it, or retries after a failed undo.
pub(super) fn toast(key: &str, name: String, text: String, failed: bool, on_undo: impl Fn(&mut App) + 'static, t: &Theme) -> Stateful<Div> {
    let label = if failed { tr("Erneut versuchen") } else { tr("Rückgängig") };
    let undo = sheet::button(SharedString::from(format!("undo-{key}")), label, Some("undo-2"), Look::Plain, false, false, t).on_click(move |_, _, cx| on_undo(cx));
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
        .child(div().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(if failed { t.danger_text } else { t.primary }).child(text))
        .child(undo)
}

impl Render for UndoToasts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        let all = lines(&self.items, cx);
        let hidden = all.len().saturating_sub(MAX_TOASTS);
        let toasts: Vec<Stateful<Div>> = all
            .into_iter()
            .rev()
            .take(MAX_TOASTS)
            .rev()
            .map(|l| {
                let target = l.target;
                toast(&l.key, l.name, l.text, l.failed, move |cx| undo(target, cx), &t)
            })
            .collect();
        div()
            .w_full()
            .px(px(space::XL))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::S))
            .when(hidden > 0, |d| d.child(div().text_size(px(font::CAPTION)).text_color(t.secondary).child(trf("+%lld weitere", &[&hidden]))))
            .children(toasts)
    }
}
