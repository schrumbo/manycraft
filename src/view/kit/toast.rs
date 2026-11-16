//! Toasts: a short note at the bottom of the window, with Rückgängig for what can be undone. At most three at once.

use std::time::Duration;

use freya::prelude::*;

use super::button::Button;
use super::text::{line, spacer};
use super::{after, use_theme};
use crate::core::i18n::tr;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{font, frame, iconsize, space};
use crate::view::ui::Ui;

const PLAIN: Duration = Duration::from_secs(4);
const UNDOABLE: Duration = Duration::from_secs(8);

/// One toast: what happened, and how to take it back.
#[derive(Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub undo: Option<EventHandler<()>>,
}

/// Shows `text`; with `undo`, Rückgängig and Strg+Z take the change back while the toast is up.
pub fn show(mut ui: Ui, text: impl Into<String>, undo: Option<EventHandler<()>>) -> u64 {
    let id = ui.toasts.peek().iter().map(|t| t.id).max().unwrap_or(0) + 1;
    let wait = if undo.is_some() { UNDOABLE } else { PLAIN };
    {
        let mut toasts = ui.toasts.write();
        toasts.push(Toast { id, text: text.into(), undo });
        let extra = toasts.len().saturating_sub(frame::TOASTS);
        toasts.drain(..extra);
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
                .min_height(Size::px(40.))
                .padding((space::XS, space::XS, space::XS, space::M))
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(space::S)
                .background(t.raised)
                .corner_radius(t.shape.control + 2.)
                .shadow(t.elevation.float.clone())
                .child(line(&t, toast.text.clone(), font::BODY, t.text1))
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
            .layer(Layer::Overlay)
            .width(Size::px(width))
            .height(Size::px(h - space::L))
            .main_align(Alignment::End)
            .spacing(space::S)
            .children(cards)
            .into_element()
    }
}
