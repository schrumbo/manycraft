//! Tooltips: shown after the pointer rests on a control, at once while moving from one tooltip to the next.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use freya::prelude::*;

use super::text::para;
use super::{Bounds, after, use_theme};
use crate::core::chat_text::{self, Run};
use crate::core::themes::Rgba;
use crate::view::theme::{color, font, frame, level, space};
use crate::view::ui::Ui;

const REST: Duration = Duration::from_millis(500);
const NEIGHBOUR: Duration = Duration::from_millis(800);
const LINE: f32 = 17.;
const WRAP: usize = 38;

/// A tooltip on screen: its text, its styled lines when it has them, and the control it explains.
#[derive(Clone, Debug, PartialEq)]
pub struct Tip {
    pub text: String,
    pub lines: Vec<Vec<Run>>,
    pub anchor: Bounds,
}

/// Wraps `child` so that resting the pointer on it shows `tip`.
#[derive(PartialEq)]
pub struct WithTip {
    pub tip: Option<String>,
    pub lines: Vec<Vec<Run>>,
    pub child: Element,
    pub fill: bool,
}

impl WithTip {
    pub fn new(tip: Option<String>, child: impl IntoElement) -> Self {
        Self { tip, lines: Vec::new(), child: child.into_element(), fill: false }
    }

    /// Shows `lines`, one paragraph each in their own colours and styles, in place of the plain text, which stays for
    /// screen readers and the like.
    pub fn styled(mut self, lines: Vec<Vec<Run>>) -> Self {
        self.lines = lines;
        self
    }

    /// Takes the full width, for children that wrap text.
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }
}

impl Component for WithTip {
    fn render(&self) -> impl IntoElement {
        let ui = use_consume::<Ui>();
        let mut bounds = use_state(Bounds::default);
        let mut hovered = use_state(|| false);
        let shown = use_hook(|| Rc::new(Cell::new(false)));
        let gone = shown.clone();
        use_drop(move || {
            if gone.get() {
                let mut ui = ui;
                ui.tip.set(None);
            }
        });
        let (enter_shown, leave_shown, down_shown) = (shown.clone(), shown.clone(), shown);
        let tip = self.tip.clone();
        let enter_tip = tip.clone();
        let lines = self.lines.clone();
        rect()
            .width(if self.fill { Size::fill() } else { Size::auto() })
            .on_sized(move |e: Event<SizedEventData>| bounds.set_if_modified(Bounds::of(&e)))
            .on_pointer_enter(move |_| {
                hovered.set(true);
                let Some(text) = enter_tip.clone() else { return };
                let mut ui = ui;
                let shown = enter_shown.clone();
                let lines = lines.clone();
                let recent = ui.tip_seen.peek().is_some_and(|t: Instant| t.elapsed() < NEIGHBOUR);
                spawn(async move {
                    if !recent {
                        after(REST).await;
                    }
                    if *hovered.peek() {
                        ui.tip.set(Some(Tip { text, lines, anchor: *bounds.peek() }));
                        shown.set(true);
                    }
                });
            })
            .on_pointer_leave(move |_| {
                hovered.set(false);
                leave_shown.set(false);
                let mut ui = ui;
                if ui.tip.peek().is_some() {
                    ui.tip_seen.set(Some(Instant::now()));
                    ui.tip.set(None);
                }
            })
            .on_mouse_down(move |_| {
                down_shown.set(false);
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
    let rows: usize = tip.lines.iter().map(|l| chat_text::plain(l).chars().count().div_ceil(WRAP).max(1)).sum();
    let tall = if tip.lines.is_empty() { 40. } else { 24. + rows as f32 * LINE };
    let y = if below + tall > h { (tip.anchor.y - tall).max(space::S) } else { below };
    let (ink, back) = if t.dark { (t.text1, t.control) } else { (t.surface, t.text1) };
    let body = if tip.lines.is_empty() {
        para(&t, tip.text, font::CALLOUT, ink).width(Size::auto()).into_element()
    } else {
        rect().spacing(0.).children(tip.lines.iter().map(|runs| paragraph_of(runs, ink, back))).into_element()
    };
    rect()
        .position(Position::new_global().left(x).top(y))
        .layer(Layer::OverlayLevel(level::TIP))
        .width(Size::px(width))
        .cross_align(Alignment::Center)
        .child(
            rect()
                .padding((space::XS + 1., space::S))
                .max_width(Size::px(width))
                .background(back)
                .corner_radius(t.shape.small)
                .shadow(t.elevation.float.clone())
                .child(body),
        )
        .into_element()
}

fn paragraph_of(runs: &[Run], ink: Color, back: Color) -> Element {
    let runs = chat_text::styled(runs, "");
    if chat_text::plain(&runs).trim().is_empty() {
        return rect().height(Size::px(space::XS)).into_element();
    }
    let back = Rgba { r: back.r(), g: back.g(), b: back.b(), a: 255 };
    let mut body = paragraph().font_size(font::CALLOUT).color(ink).line_height(frame::PARA_LINE).width(Size::auto());
    for run in &runs {
        let text = chat_text::printable(&run.text);
        if text.is_empty() {
            continue;
        }
        let mut span = Span::new(text.into_owned()).color(run.color.map_or(ink, |c| color(Rgba::rgb(c).readable_on(back, 4.5))));
        if run.bold {
            span = span.font_weight(FontWeight::BOLD);
        }
        if run.italic {
            span = span.font_slant(FontSlant::Italic);
        }
        match (run.strike, run.underline) {
            (true, true) => span = span.text_decoration(TextDecoration::UnderlineLineThrough),
            (true, false) => span = span.text_decoration(TextDecoration::LineThrough),
            (false, true) => span = span.text_decoration(TextDecoration::Underline),
            (false, false) => {}
        }
        body = body.span(span);
    }
    body.into_element()
}
