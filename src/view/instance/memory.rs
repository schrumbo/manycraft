//! The memory fields: a number of MB typed in a field, with a pair of buttons that move it by half a GB.

use freya::prelude::*;

use crate::core::i18n::tr;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::text::line;
use crate::view::kit::use_theme;
use crate::view::theme::{control, font, space};

const STEP: u32 = 512;
const FIELD: f32 = control::NUMBER * 1.5;

/// One memory field in MB; empty means "follows the standard", which `placeholder` names.
#[derive(Clone, PartialEq)]
pub(crate) struct Memory {
    id: String,
    label: String,
    text: State<String>,
    placeholder: String,
    steps: Option<Steps>,
    disabled: Option<String>,
    on_submit: Option<EventHandler<String>>,
}

/// What the buttons step between, and where they start from while the field is empty.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Steps {
    pub floor: u32,
    pub ceiling: u32,
    pub from: u32,
}

impl Memory {
    /// A field named `id` for the drive tool, editing `text`; `label` is what it is for.
    pub fn new(id: impl Into<String>, label: impl Into<String>, text: State<String>) -> Self {
        Self { id: id.into(), label: label.into(), text, placeholder: String::new(), steps: None, disabled: None, on_submit: None }
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Adds the minus and plus buttons.
    pub fn steps(mut self, steps: Steps) -> Self {
        self.steps = Some(steps);
        self
    }

    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.disabled = reason;
        self
    }

    pub fn on_submit(mut self, handler: impl FnMut(String) + 'static) -> Self {
        self.on_submit = Some(EventHandler::new(handler));
        self
    }
}

fn moved(typed: &str, steps: Steps, up: bool) -> u32 {
    let now = crate::core::options::parse_memory(typed).ok().flatten().unwrap_or(steps.from);
    let next = if up { (now / STEP).saturating_add(1).saturating_mul(STEP) } else { now.div_ceil(STEP).saturating_sub(1) * STEP };
    next.clamp(steps.floor, steps.ceiling.max(steps.floor))
}

impl Component for Memory {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut text = self.text;
        let field = Field::new(self.id.clone(), text).placeholder(self.placeholder.clone()).width(Size::px(FIELD)).disabled(self.disabled.clone());
        let field = match self.on_submit.clone() {
            Some(submit) => field.on_submit(move |s| submit.call(s)),
            None => field,
        };
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        if let Some(steps) = self.steps {
            let mut text = text;
            row = row.child(Button::icon(format!("{}.less", self.id), Icon::Minus, tr("Weniger Arbeitsspeicher")).disabled(self.disabled.clone()).on_press(move |_| {
                let next = moved(&text.peek(), steps, false).to_string();
                text.set(next);
            }));
        }
        row = row.child(field);
        if let Some(steps) = self.steps {
            let mut text = text;
            row = row.child(Button::icon(format!("{}.more", self.id), Icon::Plus, tr("Mehr Arbeitsspeicher")).disabled(self.disabled.clone()).on_press(move |_| {
                let next = moved(&text.peek(), steps, true).to_string();
                text.set(next);
            }));
        }
        row.child(line(&t, tr("MB"), font::BODY, t.text3))
    }
}