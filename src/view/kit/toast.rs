//! Toasts: a short note at the bottom of the window, with Rückgängig for what can be undone. At most three at once; a toast
//! under the pointer stays.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use freya::prelude::*;

use super::button::Button;
use super::text::{line, spacer};
use super::{after, use_theme};
use crate::core::i18n::tr;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{font, frame, level, iconsize, space};
use crate::view::ui::Ui;

const PLAIN: Duration = Duration::from_secs(4);
const UNDOABLE: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_secs(3);
const HELD: Duration = Duration::from_millis(500);

static NEXT: AtomicU64 = AtomicU64::new(1);

/// One toast: what happened, and how to take it back.
#[derive(Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub undo: Option<EventHandler<()>>,
    /// A button that goes on from here, such as Spielen once an instance is ready; Strg+Z never reaches it.
    pub action: Option<(String, EventHandler<()>)>,
    /// Stays when newer toasts push out older ones, for undoing what is hard to get back.
    pub keep: bool,
    held: bool,
    until: Instant,
}

/// Shows `text`; with `undo`, Rückgängig and Strg+Z take the change back while the toast is up.
pub fn show(ui: Ui, text: impl Into<String>, undo: Option<EventHandler<()>>) -> u64 {
    push(ui, text.into(), undo, None, false)
}

/// Shows `text` with an `undo` that newer toasts never push out, for deletions and resets.
pub fn show_kept(ui: Ui, text: impl Into<String>, undo: EventHandler<()>) -> u64 {
    push(ui, text.into(), Some(undo), None, true)
}

/// Shows `text` with a button `label` that runs `run` and closes the toast.
pub fn show_action(ui: Ui, text: impl Into<String>, label: impl Into<String>, run: EventHandler<()>) -> u64 {
    push(ui, text.into(), None, Some((label.into(), run)), false)
}

fn push(mut ui: Ui, text: String, undo: Option<EventHandler<()>>, action: Option<(String, EventHandler<()>)>, keep: bool) -> u64 {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let until = Instant::now() + if undo.is_some() || action.is_some() { UNDOABLE } else { PLAIN };
    {
        let mut toasts = ui.toasts.write();
        toasts.push(Toast { id, text, undo, action, keep, held: false, until });
        while toasts.len() > frame::TOASTS {
            let Some(oldest) = toasts.iter().position(|t| !t.keep) else { break };
            toasts.remove(oldest);
        }
    }
    spawn_in_window(async move {
        loop {
            let Some((held, until)) = ui.toasts.peek().iter().find(|t| t.id == id).map(|t| (t.held, t.until)) else { return };
            let now = Instant::now();
            if !held && now >= until {
                dismiss(ui, id);
                return;
            }
            after(if held { HELD } else { until - now }).await;
        }
    });
    id
}

fn hold(mut ui: Ui, id: u64, held: bool) {
    if !ui.toasts.peek().iter().any(|t| t.id == id && t.held != held) {
        return;
    }
    let mut toasts = ui.toasts.write();
    if let Some(t) = toasts.iter_mut().find(|t| t.id == id) {
        t.held = held;
        if !held {
            t.until = t.until.max(Instant::now() + GRACE);
        }
    }
}

/// Removes toast `id`.
pub fn dismiss(mut ui: Ui, id: u64) {
    if ui.toasts.peek().iter().any(|t| t.id == id) {
        ui.toasts.write().retain(|t| t.id != id);
    }
}

/// Undoes the newest toast that can be undone; false when there is none.
pub fn undo_last(ui: Ui) -> bool {
    let last = ui.toasts.peek().iter().rev().find_map(|t| t.undo.clone().map(|u| (t.id, u)));
    let Some((id, undo)) = last else { return false };
    dismiss(ui, id);
    undo.call(());
    true
}

/// Draws the toasts of the window, newest at the bottom.
#[derive(PartialEq)]
pub struct Host;

impl Component for Host {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ui = use_consume::<Ui>();
        let toasts = ui.toasts.read().clone();
        if toasts.is_empty() {
            return rect().into_element();
        }
        let (w, h) = *ui.window.read();
        let width = frame::TOAST.min(w - space::L * 2.);
        let cards = toasts.into_iter().map(|toast| {
            let id = toast.id;
            let mut card = rect()
                .a11y_role(AccessibilityRole::Alert)
                .a11y_alt(toast.text.clone())
                .width(Size::fill())
                .padding((space::XS, space::XS, space::XS, space::M))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::S)
                .background(t.raised)
                .corner_radius(t.shape.control + 2.)
                .shadow(t.elevation.float.clone())
                .on_pointer_enter(move |_| hold(ui, id, true))
                .on_pointer_leave(move |_| hold(ui, id, false))
                .child(super::at_least(40. - space::XS * 2., line(&t, toast.text.clone(), font::BODY, t.text1)))
                .child(spacer())
                .content(Content::flex());
            if let Some(rim) = t.elevation.rim {
                card = card.border(Border::new().fill(rim).width(1.).alignment(BorderAlignment::Outer));
            }
            if let Some(undo) = toast.undo.clone() {
                card = card.child(Button::new(format!("toast.{id}.undo"), tr("Rückgängig")).plain().glyph(Icon::Undo).on_press(move |_| {
                    dismiss(ui, id);
                    undo.call(());
                }));
            }
            if let Some((label, run)) = toast.action.clone() {
                card = card.child(Button::new(format!("toast.{id}.action"), label).plain().on_press(move |_| {
                    dismiss(ui, id);
                    run.call(());
                }));
            }
            card.child(
                rect()
                    .padding(space::XS + 2.)
                    .corner_radius(t.shape.small)
                    .cursor(CursorIcon::Pointer)
                    .a11y_role(AccessibilityRole::Button)
                    .a11y_alt(tr("Schließen"))
                    .on_press(move |_| dismiss(ui, id))
                    .child(icon(&t, Icon::Close, iconsize::XS, t.text3)),
            )
            .into_element()
        });
        rect()
            .position(Position::new_global().left((w - width) / 2.).top(0.))
            .layer(Layer::OverlayLevel(level::TOAST))
            .width(Size::px(width))
            .height(Size::px(h - space::L - *ui.toast_lift.read()))
            .main_align(Alignment::End)
            .spacing(space::S)
            .children(cards)
            .into_element()
    }
}
