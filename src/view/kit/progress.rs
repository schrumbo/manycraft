//! Progress of work the person waits for, drawn: a bar with the step it is on, a count or byte figure and, once it can be
//! told, how long is left. Work that cannot be measured shows a sliding bar with the step name and the seconds it has run.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use freya::animation::*;
use freya::prelude::*;

pub use crate::app::meter::{Meter, Unit};

use super::text::{caption, line};
use super::{after, use_theme};
use crate::core::i18n::trf;
use crate::view::drive::use_spot;
use crate::view::theme::{Theme, font, frame, motion, space};

const SWEEP_MS: u64 = 1300;
const SEGMENT: f32 = 32.;

/// Re-renders its component once a second while `live`, and gives the time of the last tick; nothing redraws otherwise.
pub fn use_second(live: bool) -> Instant {
    let mut tick = use_state(Instant::now);
    let flag = use_hook(|| Rc::new(Cell::new(live)));
    flag.set(live);
    let reads = Rc::clone(&flag);
    use_hook(move || {
        spawn(async move {
            loop {
                after(Duration::from_secs(1)).await;
                if reads.get() {
                    let Some(mut t) = tick.try_write() else { break };
                    *t = Instant::now();
                }
            }
        })
    });
    *tick.read()
}

/// A thin bar: filled to `share`, or a segment that slides while nothing is measurable (it stands still when the person asked for less motion).
#[derive(PartialEq)]
pub struct Bar {
    share: Option<f32>,
    label: String,
    still: bool,
}

impl Bar {
    pub fn new(share: Option<f32>) -> Self {
        Bar { share, label: String::new(), still: false }
    }

    /// Draws it without animation, for waits that may last minutes.
    pub fn still(mut self) -> Self {
        self.still = true;
        self
    }

    /// What a screen reader says about it.
    pub fn named(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

impl Component for Bar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let moving = self.share.is_none() && !self.still && !motion::reduced();
        let mut slide = use_animation(|conf| {
            conf.on_creation(OnCreation::Nothing);
            conf.on_finish(OnFinish::reverse());
            AnimNum::new(0., 100. - SEGMENT).duration(Duration::from_millis(SWEEP_MS)).ease(Ease::InOut).function(Function::Sine)
        });
        use_side_effect_with_deps(&moving, move |moving| {
            if *moving {
                slide.start();
            } else {
                slide.reset();
            }
        });
        let track = rect()
            .a11y_role(AccessibilityRole::ProgressIndicator)
            .a11y_alt(self.label.clone())
            .width(Size::fill())
            .height(Size::px(frame::BAR))
            .corner_radius(frame::BAR / 2.)
            .background(t.control)
            .overflow(Overflow::Clip)
            .horizontal();
        match self.share {
            Some(share) => track.child(rect().width(Size::percent(share.clamp(0., 1.) * 100.)).height(Size::fill()).corner_radius(frame::BAR / 2.).background(t.accent)),
            None => {
                let at = if moving { slide.get().value() } else { (100. - SEGMENT) / 2. };
                track.child(rect().width(Size::percent(at)).height(Size::fill())).child(rect().width(Size::percent(SEGMENT)).height(Size::fill()).corner_radius(frame::BAR / 2.).background(t.accent))
            }
        }
    }
}

/// A bar with its step above and its figures beside the step: what the person looks at while work runs.
#[derive(PartialEq)]
pub struct Wait {
    id: String,
    how: How,
}

#[derive(PartialEq)]
enum How {
    Measured(Meter),
    Unmeasured(String),
    Stepped(String, usize, usize),
}

impl Wait {
    /// Work that reports `meter`; `id` names it for the drive tool.
    pub fn new(id: impl Into<String>, meter: Meter) -> Self {
        Wait { id: id.into(), how: How::Measured(meter) }
    }

    /// Work that cannot be measured: a sliding bar with `step` and, after two seconds, how long it has run.
    pub fn working(id: impl Into<String>, step: impl Into<String>) -> Self {
        Wait { id: id.into(), how: How::Unmeasured(step.into()) }
    }

    /// A wait for the person that has `of` fixed steps, now at `at`: a bar that stands still at that step, with no animation.
    pub fn stepped(id: impl Into<String>, step: impl Into<String>, at: usize, of: usize) -> Self {
        Wait { id: id.into(), how: How::Stepped(step.into(), at, of) }
    }
}

impl Component for Wait {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let born = use_hook(Instant::now);
        let ticking = !matches!(self.how, How::Stepped(..));
        let tick = use_second(ticking);
        let now = tick.max(Instant::now());
        let (share, step, detail) = match &self.how {
            How::Measured(meter) => (meter.share(), meter.step().to_owned(), meter.detail(now)),
            How::Unmeasured(step) => {
                let meter = Meter::begin(step.clone(), born);
                (None, step.clone(), meter.detail(now))
            }
            How::Stepped(step, at, of) => (Some(*at as f32 / (*of).max(1) as f32), step.clone(), trf("Schritt %lld von %lld", &[at, of])),
        };
        let said = if detail.is_empty() { step.clone() } else { format!("{step} · {detail}") };
        let spot = use_spot(&self.id, "progressbar", &said, false);
        let bar = if ticking { Bar::new(share) } else { Bar::new(share).still() };
        rect()
            .width(Size::fill())
            .spacing(space::XS)
            .on_sized(spot)
            .child(bar.named(said))
            .child(line(&t, step, font::CALLOUT, t.text2).width(Size::fill()))
            .maybe_child((!detail.is_empty()).then(|| caption(&t, detail).width(Size::fill())))
    }
}

/// A grey bar standing where text will be, for lists and fields that are loading; `width` is its length.
pub fn placeholder(t: &Theme, width: f32) -> Rect {
    rect().width(Size::px(width)).height(Size::px(frame::BAR_H)).corner_radius(t.shape.small).background(t.hover)
}