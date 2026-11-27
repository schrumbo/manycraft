//! A stepper: a number with minus and plus, changed by clicks or the arrow keys, ten steps at a time with Shift.

use std::cell::Cell;
use std::rc::Rc;

use freya::prelude::*;

use super::field::Field;
use super::text::{figure, line};
use super::tooltip::WithTip;
use super::{after, ring, use_theme};
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{control, font, iconsize, space};

/// How a stepper takes typed input: `parse` reads the text as a value, `show` writes a value as text, and `hint` says what
/// belongs in the field when the text was not accepted.
#[derive(Clone)]
pub struct Typing {
    pub parse: fn(&str) -> Option<i64>,
    pub show: fn(i64) -> String,
    pub hint: String,
}

impl PartialEq for Typing {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.parse, other.parse) && std::ptr::fn_addr_eq(self.show, other.show) && self.hint == other.hint
    }
}

/// A whole number between `min` and `max`, shown with `unit`.
#[derive(Clone, PartialEq)]
pub struct Stepper {
    id: String,
    label: String,
    value: i64,
    min: i64,
    max: i64,
    step: i64,
    unit: String,
    shown: Option<String>,
    typing: Option<Typing>,
    disabled: Option<String>,
    on_change: Option<EventHandler<i64>>,
}

impl Stepper {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: i64, min: i64, max: i64) -> Self {
        Self { id: id.into(), label: label.into(), value, min, max, step: 1, unit: String::new(), shown: None, typing: None, disabled: None, on_change: None }
    }

    pub fn step(mut self, step: i64) -> Self {
        self.step = step.max(1);
        self
    }

    /// Text after the number, such as "GB".
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    /// Writes `text` in place of the number and its unit, such as "4 GB" for 4096.
    pub fn shown(mut self, text: impl Into<String>) -> Self {
        self.shown = Some(text.into());
        self
    }

    /// Turns the number into a field the value can be typed in; Enter or leaving the field commits it, and text outside `min`
    /// and `max` or that `typing` cannot read goes back to the current value with its hint.
    pub fn typing(mut self, typing: Typing) -> Self {
        self.typing = Some(typing);
        self
    }

    /// Greys the stepper out and ignores clicks and keys when `reason` is set; the reason shows as its tooltip.
    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.disabled = reason;
        self
    }

    pub fn on_change(mut self, handler: impl FnMut(i64) + 'static) -> Self {
        self.on_change = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Stepper {
    fn render(&self) -> impl IntoElement {
        if let Some(typing) = self.typing.clone() {
            return Typed { stepper: self.clone(), typing }.into_element();
        }
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let spot = use_spot(&self.id, "stepper", &self.label, self.disabled.is_none());
        let (value, min, max, step) = (self.value, self.min, self.max, self.step);
        let live = self.disabled.is_none();
        let change = self.on_change.clone();
        let set = move |v: i64| {
            let v = v.clamp(min, max);
            if v != value
                && let Some(h) = &change
            {
                h.call(v);
            }
        };
        let side = |glyph: Icon, enabled: bool, to: i64, tip: String| {
            let mut set = set.clone();
            Side { glyph, enabled, tip, on_press: EventHandler::new(move |_| {
                a11y.request_focus();
                set(to);
            }) }
        };
        let text = match self.shown.clone() {
            Some(shown) => shown,
            None if self.unit.is_empty() => value.to_string(),
            None => format!("{value} {}", self.unit),
        };
        let mut keys = set.clone();
        let mut s = rect()
            .a11y_id(a11y)
            .a11y_focusable(live)
            .a11y_role(AccessibilityRole::SpinButton)
            .a11y_alt(format!("{}: {text}", self.label))
            .horizontal()
            .cross_align(Alignment::Center)
            .height(Size::px(control::M))
            .padding((0., space::XXS))
            .spacing(space::XXS)
            .background(t.control)
            .corner_radius(t.shape.control)
            .on_sized(spot)
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !live || !a11y.is_focused() {
                    return;
                }
                let jump = if e.modifiers.contains(Modifiers::SHIFT) { step * 10 } else { step };
                match e.key {
                    Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowRight) => keys(value + jump),
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowLeft) => keys(value - jump),
                    _ => return,
                }
                e.stop_propagation();
            })
            .child(side(Icon::Minus, live && value > min, value - step, tr("Weniger")))
            .child(rect().width(Size::px(control::STEPPER_VALUE)).center().child(figure(&t, text, font::BODY, if live { t.text1 } else { t.text3 })))
            .child(side(Icon::Plus, live && value < max, value + step, tr("Mehr")));
        if let Some(border) = ring(&t, focus()) {
            s = s.border(border);
        }
        WithTip::new(self.disabled.clone(), s).into_element()
    }
}

const HINT_FOR: std::time::Duration = std::time::Duration::from_millis(2500);

/// What typed `text` becomes: the value to commit, or `None` when it is unreadable or outside `min` and `max`.
fn committed(typing: &Typing, text: &str, min: i64, max: i64) -> Option<i64> {
    (typing.parse)(text).filter(|v| (min..=max).contains(v))
}

#[derive(PartialEq)]
struct Typed {
    stepper: Stepper,
    typing: Typing,
}

impl Component for Typed {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let s = &self.stepper;
        let a11y = use_hook(AccessibilityId::new_unique);
        let focus = use_focus(a11y);
        let (value, min, max, step) = (s.value, s.min, s.max, s.step);
        let show = self.typing.show;
        let mut text = use_state(|| show(value));
        let mut bad = use_state(|| false);
        let mut epoch = use_state(|| 0u64);
        let mut editing = use_state(|| false);
        let latest = use_hook(|| Rc::new(Cell::new(value)));
        latest.set(value);
        use_side_effect_with_deps(&value, move |v| {
            if !*editing.peek() {
                text.set(show(*v));
            }
        });
        let live = s.disabled.is_none();
        let (typing, change) = (self.typing.clone(), s.on_change.clone());
        let mut commit = {
            let (typing, change, latest) = (typing.clone(), change.clone(), latest.clone());
            move |typed: &str| match committed(&typing, typed, min, max) {
                Some(v) => {
                    if v != latest.get() {
                        if let Some(h) = &change {
                            h.call(v);
                        }
                        bad.set(false);
                    }
                    text.set(show(v));
                }
                None => {
                    text.set(show(latest.get()));
                    let mine = epoch.peek().wrapping_add(1);
                    epoch.set(mine);
                    bad.set(true);
                    spawn(async move {
                        after(HINT_FOR).await;
                        if let (Some(now), Some(mut shown)) = (epoch.try_peek().map(|e| *e), bad.try_write())
                            && now == mine
                        {
                            *shown = false;
                        }
                    });
                }
            }
        };
        let focused = focus() != Focus::Not;
        let mut blur = commit.clone();
        use_side_effect_with_deps(&focused, move |now| {
            let was = *editing.peek();
            editing.set(*now);
            if was && !*now {
                let typed = text.peek().clone();
                blur(&typed);
            }
        });
        let step_by = {
            let change = change.clone();
            move |to: i64| {
                let to = to.clamp(min, max);
                if to != value
                    && let Some(h) = &change
                {
                    h.call(to);
                }
            }
        };
        let side = |glyph: Icon, enabled: bool, to: i64, tip: String| {
            let mut set = step_by.clone();
            Side { glyph, enabled, tip, on_press: EventHandler::new(move |_| set(to)) }
        };
        let (up, down) = (step_by.clone(), step_by.clone());
        let field = Field::new(format!("{}.field", s.id), text)
            .a11y(a11y)
            .owns_enter()
            .width(Size::px(control::STEPPER_VALUE_WIDE))
            .disabled(s.disabled.clone())
            .claim(move |key, _| match key {
                Key::Named(NamedKey::ArrowUp) => {
                    up(value + step);
                    true
                }
                Key::Named(NamedKey::ArrowDown) => {
                    down(value - step);
                    true
                }
                _ => false,
            })
            .on_submit(move |typed| commit(&typed));
        rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(Hint { shown: bad, text: typing.hint.clone() })
            .child(side(Icon::Minus, live && value > min, value - step, tr("Weniger")))
            .child(field)
            .child(side(Icon::Plus, live && value < max, value + step, tr("Mehr")))
            .child(line(&t, s.unit.clone(), font::BODY, t.text3))
    }
}

#[derive(PartialEq)]
struct Hint {
    shown: State<bool>,
    text: String,
}

impl Component for Hint {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hint = rect();
        if *self.shown.read() {
            hint = hint.child(line(&t, self.text.clone(), font::CAPTION, t.danger_text));
        }
        hint
    }
}

#[derive(PartialEq)]
struct Side {
    glyph: Icon,
    enabled: bool,
    tip: String,
    on_press: EventHandler<()>,
}

impl Component for Side {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let press = self.on_press.clone();
        let mut b = rect()
            .width(Size::px(control::S))
            .height(Size::px(control::S))
            .center()
            .corner_radius(t.shape.small)
            .child(icon(&t, self.glyph, iconsize::S, if self.enabled { t.text1 } else { t.text3 }));
        if self.enabled {
            b = b
                .background(if *pressed.read() { t.press } else if *hovered.read() { t.hover } else { Color::TRANSPARENT })
                .cursor(CursorIcon::Pointer)
                .on_pointer_enter(move |_| hovered.set(true))
                .on_pointer_leave(move |_| {
                    hovered.set_if_modified(false);
                    pressed.set_if_modified(false);
                })
                .on_mouse_down(move |_| pressed.set(true))
                .on_mouse_up(move |_| pressed.set_if_modified(false))
                .on_press(move |_| press.call(()));
        }
        WithTip::new(self.enabled.then(|| self.tip.clone()), b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typing() -> Typing {
        Typing { parse: |s| s.trim().parse().ok(), show: |v| v.to_string(), hint: String::new() }
    }

    #[test]
    fn typed_text_commits_only_inside_the_range() {
        let t = typing();
        assert_eq!(committed(&t, "5", 1, 8), Some(5));
        assert_eq!(committed(&t, "0", 1, 8), None);
        assert_eq!(committed(&t, "9", 1, 8), None);
        assert_eq!(committed(&t, "abc", 1, 8), None);
    }
}
