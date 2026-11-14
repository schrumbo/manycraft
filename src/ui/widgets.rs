use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::drive::Mark;
use super::motion::Soft;
use super::theme::{Theme, control, font, iconsize, radius, ring, space, tint};

/// Makes a clickable element reachable with Tab; Enter and Space then click it, and a ring shows where the focus is.
pub trait KeyNav {
    fn key_nav(self) -> Self;
}

impl KeyNav for Stateful<Div> {
    fn key_nav(self) -> Self {
        let color = ring::color();
        super::drive::marked(self, "clickable").tab_index(0).focus_visible(move |s| {
            s.shadow(vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(ring::WIDTH), inset: false }])
        })
    }
}

/// Focuses a text field without starting its caret blink while the window is inactive, where nothing would stop it.
pub trait TakeFocus {
    fn take_focus(&self, window: &mut Window, cx: &mut Context<InputState>);
}

impl TakeFocus for InputState {
    fn take_focus(&self, window: &mut Window, cx: &mut Context<InputState>) {
        if window.is_window_active() {
            self.focus(window, cx);
        } else {
            let handle = Focusable::focus_handle(self, cx);
            window.focus(&handle, cx);
        }
    }
}

/// Keeps a wheel turn that moved a list from also scrolling the sheet around it; at the list's end the sheet takes over.
pub trait ContainScroll {
    fn contain_scroll(self, handle: &ScrollHandle) -> Self;
}

impl<E: InteractiveElement> ContainScroll for E {
    fn contain_scroll(self, handle: &ScrollHandle) -> Self {
        let handle = handle.clone();
        let seen = handle.offset().y;
        self.on_scroll(move |_, _, cx| {
            let max = handle.max_offset().y;
            let clamped = |y: Pixels| y.clamp(-max, px(0.));
            if max > px(0.) && clamped(handle.offset().y) != clamped(seen) {
                cx.stop_propagation();
            }
        })
    }
}

/// A Lucide icon, tinted like an SF Symbol.
pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size)).flex_none().text_color(color)
}

/// Tooltip builder for `.tooltip(...)`.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
}

/// The main button of a screen: accent fill, one per view. A disabled one keeps its place but takes no input.
pub fn primary_button(id: impl Into<SharedString>, label: impl Into<SharedString>, icon_name: &str, disabled: bool, t: &Theme) -> Stateful<Div> {
    let (id, label) = (id.into(), label.into());
    let button = div()
        .id(id)
        .mark("button", label.clone())
        .h(px(control::L))
        .px(px(space::L))
        .rounded(px(radius::INNER))
        .bg(t.accent)
        .text_color(t.on_accent)
        .text_size(px(font::BODY))
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .gap(px(space::S));
    if disabled {
        return button.opacity(tint::DISABLED).child(icon(icon_name, iconsize::M, t.on_accent)).child(label);
    }
    button.cursor_pointer().key_nav().soft(t.shade, radius::INNER).child(icon(icon_name, iconsize::M, t.on_accent)).child(label)
}
