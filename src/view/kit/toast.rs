//! Toasts: a short note at the bottom of the window, with Rückgängig for what can be undone. At most three at once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use freya::prelude::*;

use super::button::Button;
use super::text::{line, spacer};
use super::{after, use_theme};
use crate::core::i18n::tr;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{font, frame, level, iconsize, space};
use crate::view::ui::Ui;

const PLAIN: Duration = Duration::from_secs(4);
const UNDOABLE: Duration = Duration::from_secs(8);

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
}

/// Shows `text`; with `undo`, Rückgängig and Strg+Z take the change back while the toast is up.
pub fn show(ui: Ui, text: impl Into<String>, undo: Option<EventHandler<()>>) -> u64 {
    add(ui, text.into(), undo, false)
}

/// Shows `text` with an `undo` that newer toasts never push out, for deletions and resets.
pub fn show_kept(ui: Ui, text: impl Into<String>, undo: EventHandler<()>) -> u64 {
    add(ui, text.into(), Some(undo), true)
}

/// Shows `text` with a button `label` that runs `run` and closes the toast.
pub fn show_action(ui: Ui, text: impl Into<String>, label: impl Into<String>, run: EventHandler<()>) -> u64 {
    push(ui, Toast { id: NEXT.fetch_add(1, Ordering::Relaxed), text: text.into(), undo: None, action: Some((label.into(), run)), keep: false })
}

fn add(ui: Ui, text: String, undo: Option<EventHandler<()>>, keep: bool) -> u64 {
    push(ui, Toast { id: NEXT.fetch_add(1, Ordering::Relaxed), text, undo, action: None, keep })
}

fn push(mut ui: Ui, toast: Toast) -> u64 {
    let id = toast.id;
    let wait = if toast.undo.is_some() || toast.action.is_some() { UNDOABLE } else { PLAIN };
    {
        let mut toasts = ui.toasts.write();
        toasts.push(toast);
        while toasts.len() > frame::TOASTS {
            let Some(oldest) = toasts.iter().position(|t| !t.keep) else { break };
            toasts.remove(oldest);
        }
    }
    spawn_in_window(async move {
        after(wait).await;
        dismiss(ui, id);
    });
    id
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
