use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::drive::Mark;
use super::motion::Soft;
use super::theme::{Theme, control, edge, font, iconsize, radius, ring, space, tint};

/// Makes a clickable element reachable with Tab; Enter and Space then click it, and a ring shows where the focus is.
/// Call it after the element's fill: a control without an opaque one gets its ring inside, since a ring outside would show through.
pub trait KeyNav {
    fn key_nav(self) -> Self;
    /// Like `key_nav`, with the ring always inside, for controls packed so tightly that a ring outside would be covered.
    fn key_nav_inside(self) -> Self;
}

impl KeyNav for Stateful<Div> {
    fn key_nav(mut self) -> Self {
        let filled = self.style().background.as_ref().and_then(Fill::color).and_then(|c| c.as_solid()).is_some_and(|c| c.a >= 1.);
        focus_ring(self, !filled)
    }

    fn key_nav_inside(self) -> Self {
        focus_ring(self, true)
    }
}

fn focus_ring(element: Stateful<Div>, inset: bool) -> Stateful<Div> {
    let color = ring::color();
    let spread = if inset { ring::INSET } else { ring::WIDTH };
    super::drive::marked(element, "clickable")
        .tab_index(0)
        .focus_visible(move |s| s.shadow(vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(spread), inset }]))
}

/// The edge that keeps a small badge readable over a bright preview: a ring in the window colour and a soft shadow.
pub fn badge_edge(t: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow { color: black().opacity(tint::BADGE_SHADOW), offset: point(px(0.), px(0.)), blur_radius: px(edge::BLUR), spread_radius: px(0.), inset: false },
        BoxShadow { color: t.window, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(edge::RING), inset: false },
    ]
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

/// The frame of one segment, raised when selected, with hover and keyboard focus unless `enabled` is false;
/// add its content and wrap it in [`segmented`].
pub fn segment_frame(id: impl Into<ElementId>, label: String, selected: bool, enabled: bool, t: &Theme) -> Stateful<Div> {
    let frame = div()
        .id(id)
        .mark("segment", label)
        .h(px(control::S))
        .px(px(space::S))
        .rounded(px(radius::INNER))
        .flex()
        .flex_none()
        .items_center()
        .text_size(px(font::CALLOUT))
        .when(selected, |d| d.bg(t.segment).shadow_xs().text_color(t.primary))
        .when(!selected, |d| d.bg(t.raised).text_color(t.secondary));
    if !enabled {
        return frame.cursor_not_allowed();
    }
    frame.cursor_pointer().key_nav_inside().soft(if selected { transparent_black() } else { t.segment.opacity(tint::SEGMENT_HOVER) }, radius::INNER)
}

/// One choice of a segmented control with a plain label; the selected one is raised.
pub fn segment(id: impl Into<ElementId>, label: String, selected: bool, enabled: bool, t: &Theme) -> Stateful<Div> {
    segment_frame(id, label.clone(), selected, enabled, t).child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(div().when(selected, |d| d.font_weight(FontWeight::MEDIUM)).child(label.clone()))
                .child(div().h_0().overflow_hidden().font_weight(FontWeight::MEDIUM).text_color(transparent_black()).child(label)),
        )
}

/// A segmented control: a click on a segment or Left and Right on a focused one picks it, `on_pick` gets its index.
pub fn segmented(id: impl Into<ElementId>, segments: impl IntoIterator<Item = Stateful<Div>>, on_pick: impl Fn(usize, &mut Window, &mut App) + 'static, t: &Theme) -> Stateful<Div> {
    let on_pick = std::rc::Rc::new(on_pick);
    let segments: Vec<Stateful<Div>> = segments.into_iter().collect();
    let last = segments.len().saturating_sub(1);
    let items = segments.into_iter().enumerate().map(|(n, s)| {
        let (click, key) = (on_pick.clone(), on_pick.clone());
        s.on_click(move |_, window, cx| click(n, window, cx)).on_key_down(move |e: &KeyDownEvent, window, cx| {
            if e.keystroke.modifiers.modified() {
                return;
            }
            let to = match e.keystroke.key.as_str() {
                "left" if n > 0 => n - 1,
                "right" if n < last => n + 1,
                "left" | "right" => n,
                _ => return,
            };
            cx.stop_propagation();
            if to == n {
                return;
            }
            key(to, window, cx);
            if to < n { window.focus_prev(cx) } else { window.focus_next(cx) }
        })
    });
    div().id(id).p(px(space::XXS)).rounded(px(radius::CONTROL)).bg(t.raised).flex().flex_none().items_center().gap(px(space::XXS)).children(items)
}

struct MenuOpen(Option<SharedString>);

impl Global for MenuOpen {}

/// Whether the context menu of the element `key` is open. Its tooltip steps aside meanwhile, since it would cover the first item.
pub fn menu_open(key: &str, cx: &App) -> bool {
    cx.try_global::<MenuOpen>().is_some_and(|open| open.0.as_deref() == Some(key))
}

/// Marks the context menu being built as the one of the element `key` until it closes. Call it in the menu builder.
pub fn menu_of(key: &str, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let key = SharedString::from(key.to_owned());
    cx.set_global(MenuOpen(Some(key.clone())));
    let entity = cx.entity();
    window
        .subscribe(&entity, cx, move |_, _: &DismissEvent, window, cx| {
            if menu_open(&key, cx) {
                cx.set_global(MenuOpen(None));
                window.refresh();
            }
        })
        .detach();
    menu
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
