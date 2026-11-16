//! A small arc that turns while something is waited for; it only animates while it is on screen.

use std::time::Duration;

use freya::animation::*;
use freya::prelude::*;

use super::text::caption;
use super::use_theme;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{Theme, frame, iconsize, motion, space};

const SIZE: f32 = iconsize::S;
const FULL_TURN: f32 = 360.;

/// The turning arc; it stands still when the person asked for less motion.
#[derive(PartialEq)]
pub struct Spinner;

impl Component for Spinner {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let turn = use_animation(|conf| {
            conf.on_creation(if motion::reduced() { OnCreation::Nothing } else { OnCreation::Run });
            conf.on_finish(OnFinish::restart());
            AnimNum::new(0., FULL_TURN).duration(Duration::from_millis(frame::SPIN_MS))
        });
        rect().width(Size::px(SIZE)).height(Size::px(SIZE)).rotate(turn.get().value()).child(icon(&t, Icon::Spinner, SIZE, t.text3))
    }
}

/// `text` as a caption beside the turning arc.
pub fn waiting(t: &Theme, text: impl Into<String>) -> Element {
    beside(caption(t, text))
}

/// `text` beside the turning arc, as the caller styled it.
pub fn beside(text: Label) -> Element {
    rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(Spinner).child(text).into_element()
}
