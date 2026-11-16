//! Text fields: a label above, a hint or the error below, and the focus ring around the box.

use freya::prelude::*;

use super::text::{caption, line};
use super::{ring, use_theme};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{Theme, control, font, iconsize, space};

thread_local! {
    static FIELDS: std::cell::RefCell<std::collections::HashSet<AccessibilityId>> = std::cell::RefCell::default();
}

/// True while a text field has the keyboard, so app shortcuts leave its editing keys alone.
pub fn typing() -> bool {
    let focused = *Platform::get().current_window().focused_accessibility_id.peek();
    FIELDS.with(|f| f.borrow().contains(&focused))
}

/// A one-line text field bound to `value`.
#[derive(Clone, PartialEq)]
pub struct Field {
    id: String,
    value: State<String>,
    label: Option<String>,
    placeholder: String,
    hint: Option<String>,
    error: Option<String>,
    glyph: Option<Icon>,
    width: Size,
    auto_focus: bool,
    secret: bool,
    lines: usize,
    on_submit: Option<EventHandler<String>>,
    claim: Option<Callback<(Key, Modifiers), bool>>,
    rows_through: bool,
    a11y: Option<AccessibilityId>,
}

impl Field {
    /// A field named `id` for the drive tool, editing `value`.
    pub fn new(id: impl Into<String>, value: State<String>) -> Self {
        Self {
            id: id.into(),
            value,
            label: None,
            placeholder: String::new(),
            hint: None,
            error: None,
            glyph: None,
            width: Size::fill(),
            auto_focus: false,
            secret: false,
            lines: 1,
            on_submit: None,
            claim: None,
            rows_through: false,
            a11y: None,
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// A line below the field that says what belongs in it.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// What is wrong with the value, shown in place of the hint.
    pub fn error(mut self, error: Option<String>) -> Self {
        self.error = error;
        self
    }

    pub fn glyph(mut self, glyph: Icon) -> Self {
        self.glyph = Some(glyph);
        self
    }

    pub fn width(mut self, width: Size) -> Self {
        self.width = width;
        self
    }

    pub fn auto_focus(mut self) -> Self {
        self.auto_focus = true;
        self
    }

    /// Hides what is typed.
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Shows `lines` rows and lets Enter break the line; `on_submit` then runs on Ctrl or Cmd with Enter.
    pub fn lines(mut self, lines: usize) -> Self {
        self.lines = lines.max(1);
        self
    }

    pub fn on_submit(mut self, handler: impl FnMut(String) + 'static) -> Self {
        self.on_submit = Some(EventHandler::new(handler));
        self
    }

    /// Lets `claim` see each key first; a key it returns true for never reaches the text or the app's shortcuts.
    pub fn claim(mut self, claim: impl Fn(&Key, Modifiers) -> bool + 'static) -> Self {
        self.claim = Some(Callback::new(move |(key, modifiers): (Key, Modifiers)| claim(&key, modifiers)));
        self
    }

    /// Lets Up, Down, Page Up and Page Down pass on to the list this field searches.
    pub fn rows_through(mut self) -> Self {
        self.rows_through = true;
        self
    }

    /// Uses `a11y` as the field's focus id, so its owner can focus it.
    pub fn a11y(mut self, a11y: AccessibilityId) -> Self {
        self.a11y = Some(a11y);
        self
    }
}

fn shortcut_key(key: &Key, modifiers: Modifiers) -> bool {
    let Key::Character(c) = key else { return false };
    modifiers.intersects(Modifiers::CONTROL | Modifiers::META) && !modifiers.contains(Modifiers::ALT) && !["a", "c", "v", "x", "y", "z"].iter().any(|k| c.eq_ignore_ascii_case(k))
}

fn colors(t: &Theme, error: bool) -> InputColorsThemePartial {
    let border = if error { t.danger } else { t.separator };
    InputColorsThemePartial {
        background: Some(t.well.into()),
        focus_background: Some(t.well.into()),
        border_fill: Some(border.into()),
        focus_border_fill: Some(if error { t.danger } else { t.focus }.into()),
        color: Some(t.text1.into()),
        placeholder_color: Some(t.text3.into()),
    }
}

impl Component for Field {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_hook(|| self.a11y.unwrap_or_else(AccessibilityId::new_unique));
        use_hook(|| FIELDS.with(|f| f.borrow_mut().insert(a11y)));
        use_drop(move || {
            FIELDS.with(|f| f.borrow_mut().remove(&a11y));
        });
        let focus = use_focus(a11y);
        let spot = use_spot(&self.id, "field", self.label.as_deref().unwrap_or(&self.placeholder), true);
        let mut input = Input::new(self.value)
            .a11y_id(a11y)
            .placeholder(self.placeholder.clone())
            .auto_focus(self.auto_focus)
            .width(Size::fill())
            .theme_colors(colors(&t, self.error.is_some()))
            .theme_layout(InputLayoutThemePartial {
                corner_radius: Some(CornerRadius::new_all(t.shape.field).into()),
                padding: Some(Gaps::new((control::M - font::BODY * font::LINE) / 2., space::S + 2., (control::M - font::BODY * font::LINE) / 2., space::S + 2.).into()),
            });
        if self.secret {
            input = input.mode(InputMode::new_password());
        }
        if let Some(g) = self.glyph {
            input = input.leading(rect().padding((0., 0., 0., space::S)).child(icon(&t, g, iconsize::S, t.text3)));
        }
        if self.lines > 1 {
            let (submit, value) = (self.on_submit.clone(), self.value);
            input = input.multiline(true).height(Size::px(control::M + (self.lines - 1) as f32 * font::BODY * font::LINE)).on_pre_key_down(Callback::new(move |e: Event<KeyboardEventData>| match &e.key {
                Key::Named(NamedKey::Enter) if e.modifiers.intersects(Modifiers::CONTROL | Modifiers::META) => {
                    e.stop_propagation();
                    if let Some(h) = &submit {
                        h.call(value.peek().clone());
                    }
                    false
                }
                Key::Named(NamedKey::Escape | NamedKey::Shift) => true,
                Key::Named(NamedKey::Tab) => false,
                _ => {
                    if !e.modifiers.intersects(Modifiers::CONTROL | Modifiers::META) {
                        e.stop_propagation();
                    }
                    true
                }
            }));
        } else {
            if let Some(h) = self.on_submit.clone() {
                input = input.on_submit(move |s| h.call(s));
            }
            let (claim, rows_through) = (self.claim.clone(), self.rows_through);
            input = input.on_pre_key_down(Callback::new(move |e: Event<KeyboardEventData>| {
                let key = e.key.clone();
                if claim.as_ref().is_some_and(|c| c.call((key.clone(), e.modifiers))) {
                    e.stop_propagation();
                    e.prevent_default();
                    return false;
                }
                if shortcut_key(&key, e.modifiers) || rows_through && matches!(key, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::PageUp | NamedKey::PageDown)) {
                    return false;
                }
                match key {
                    Key::Named(NamedKey::Enter | NamedKey::Escape | NamedKey::Shift) => true,
                    Key::Named(NamedKey::Tab) => false,
                    _ => {
                        e.stop_propagation();
                        e.prevent_default();
                        true
                    }
                }
            }));
        }
        let mut boxed = rect().width(Size::fill()).corner_radius(t.shape.field).on_sized(spot).child(input);
        if let Some(border) = ring(&t, focus()) {
            boxed = boxed.border(border);
        }
        let mut column = rect().width(self.width.clone()).spacing(space::XS);
        if let Some(l) = &self.label {
            column = column.child(line(&t, l.clone(), font::CALLOUT, t.text2).font_weight(FontWeight::MEDIUM));
        }
        column = column.child(boxed);
        if let Some(e) = &self.error {
            column = column.child(line(&t, e.clone(), font::CAPTION, t.danger_text));
        } else if let Some(h) = &self.hint {
            column = column.child(caption(&t, h.clone()));
        }
        column
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_letters_leave_the_field_and_editing_keys_stay() {
        let ctrl = Modifiers::CONTROL;
        assert!(shortcut_key(&Key::Character("k".into()), ctrl));
        assert!(shortcut_key(&Key::Character("M".into()), ctrl | Modifiers::SHIFT));
        assert!(shortcut_key(&Key::Character("q".into()), Modifiers::META));
        for k in ["a", "c", "v", "x", "y", "z", "Z"] {
            assert!(!shortcut_key(&Key::Character(k.into()), ctrl));
        }
        assert!(!shortcut_key(&Key::Character("k".into()), Modifiers::empty()));
        assert!(!shortcut_key(&Key::Character("@".into()), ctrl | Modifiers::ALT));
        assert!(!shortcut_key(&Key::Named(NamedKey::Home), ctrl));
    }
}
