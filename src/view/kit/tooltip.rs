//! Tooltips: shown after the pointer rests on a control, at once while moving from one tooltip to the next.

use std::time::{Duration, Instant};

use freya::prelude::*;

use super::text::para;
use super::{Bounds, after, use_theme};
use crate::view::theme::{font, frame, space};
use crate::view::ui::Ui;

const REST: Duration = Duration::from_millis(500);
const NEIGHBOUR: Duration = Duration::from_millis(800);

/// A tooltip on screen: its text and the control it explains.
#[derive(Clone, Debug, PartialEq)]
pub struct Tip {
    pub text: String,
    pub anchor: Bounds,
}

/// Wraps `child` so that resting the pointer on it shows `tip`.
#[derive(PartialEq)]
pub struct WithTip {
    pub tip: Option<String>,
    pub child: Element,
}

impl WithTip {
    pub fn new(tip: Option<String>, child: impl IntoElement) -> Self {
        Self { tip, child: child.into_element() }
    }
}

impl Component for WithTip {
    fn render(&self) -> impl IntoElement {
        let ui = use_consume::<Ui>();
        let mut bounds = use_state(Bounds::default);
        let mut hovered = use_state(|| false);
        let tip = self.tip.clone();
        let enter_tip = tip.clone();
        rect()
            .on_sized(move |e: Event<SizedEventData>| bounds.set_if_modified(Bounds::of(&e)))
            .on_pointer_enter(move |_| {
                hovered.set(true);
                let Some(text) = enter_tip.clone() else { return };
                let mut ui = ui;
                let recent = ui.tip_seen.peek().is_some_and(|t: Instant| t.elapsed() < NEIGHBOUR);
                spawn(async move {
                    if !recent {
                        after(REST).await;
                    }
                    if *hovered.peek() {
                        ui.tip.set(Some(Tip { text, anchor: *bounds.peek() }));
                    }
                });
            })
            .on_pointer_leave(move |_| {
                hovered.set(false);
                let mut ui = ui;
                if ui.tip.peek().is_some() {
                    ui.tip_seen.set(Some(Instant::now()));
                    ui.tip.set(None);
                }
            })
            .on_mouse_down(move |_| {
                let mut ui = ui;
                ui.tip.set_if_modified(None);
            })
            .child(self.child.clone())
    }
}

/// Draws the tooltip of the window, below its control, kept inside the window.
pub fn host(ui: Ui) -> Element {
    let t = use_theme();
    let Some(tip) = ui.tip.read().clone() else {
        return rect().into_element();
    };
    let (w, h) = *ui.window.read();
    let width = frame::TOOLTIP.min(w - space::L * 2.);
    let x = (tip.anchor.center().0 - width / 2.).clamp(space::S, (w - width - space::S).max(space::S));
    let below = tip.anchor.y + tip.anchor.h + space::XS;
    let y = if below + 40. > h { (tip.anchor.y - 40.).max(space::S) } else { below };
    rect()
        .position(Position::new_global().left(x).top(y))
        .layer(Layer::Overlay)
        .width(Size::px(width))
        .cross_align(Alignment::Center)
        .child(
            rect()
                .padding((space::XS + 1., space::S))
                .max_width(Size::px(width))
                .background(if t.dark { t.control } else { t.text1 })
                .corner_radius(t.shape.small)
                .shadow(t.elevation.float.clone())
                .child(para(&t, tip.text, font::CALLOUT, if t.dark { t.text1 } else { t.surface }).width(Size::auto())),
        )
        .into_element()
}
