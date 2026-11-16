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
    on_submit: Option<EventHandler<String>>,
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
            on_submit: None,
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

    pub fn on_submit(mut self, handler: impl FnMut(String) + 'static) -> Self {
        self.on_submit = Some(EventHandler::new(handler));
        self
    }

    /// Uses `a11y` as the field's focus id, so its owner can focus it.
    pub fn a11y(mut self, a11y: AccessibilityId) -> Self {
        self.a11y = Some(a11y);
        self
    }
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
                padding: Some(Gaps::new((control::M - font::BODY * 1.3) / 2., space::S + 2., (control::M - font::BODY * 1.3) / 2., space::S + 2.).into()),
            });
        if self.secret {
            input = input.mode(InputMode::new_password());
        }
        if let Some(g) = self.glyph {
            input = input.leading(rect().padding((0., 0., 0., space::S)).child(icon(&t, g, iconsize::S, t.text3)));
        }
        if let Some(h) = self.on_submit.clone() {
            input = input.on_submit(move |s| h.call(s));
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
