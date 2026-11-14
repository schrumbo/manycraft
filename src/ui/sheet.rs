//! The frame every sheet shares: presentation over the main window, header, footer, buttons, field parts and the confirmation alert.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogEntrance, DialogFooter};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::drive::Mark;
use super::forms::Problem;
use super::motion::Soft;
use super::theme::{Theme, control, font, iconsize, radius, space, tint, with_alpha};
use super::widgets::{KeyNav, icon, tip};
use crate::core::i18n::tr;

/// Width of a sheet with a few fields.
pub const NARROW: f32 = 440.;
/// Width of a sheet with lists.
pub const WIDE: f32 = 560.;
/// The tallest a scrolling list inside a sheet gets.
pub const LIST_MAX: f32 = 220.;
/// Room kept between a sheet and the window edges.
pub const MARGIN: f32 = 48.;

/// Emitted by a sheet that is done, saved or cancelled.
pub struct Dismissed;

/// Presents `view` as a sheet over the window; it closes when the view emits `Dismissed`, on Escape and on Abbrechen.
pub fn show<V: Render + EventEmitter<Dismissed>>(view: Entity<V>, width: f32, window: &mut Window, cx: &mut App) {
    if busy(window, cx) {
        return;
    }
    let handle = window.window_handle();
    cx.subscribe(&view, move |_, _: &Dismissed, cx| {
        let _ = handle.update(cx, |_, window, cx| window.close_dialog(cx));
    })
    .detach();
    window.open_dialog(cx, move |d, _, _| d.entrance(DialogEntrance::Fade).w(px(width)).p_0().close_button(false).on_ok(pass_enter).child(view.clone()));
}

/// Whether a sheet or alert is already up, so a second one must not open.
pub fn busy(window: &mut Window, cx: &mut App) -> bool {
    window.has_active_dialog(cx)
}

/// Runs `f` in the window an app-level action acts in, the active one or else the main one, once the current update is over.
pub fn in_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    cx.defer(move |cx| {
        let target = cx.active_window().or_else(|| cx.windows().into_iter().find(|w| super::main_window::is_main(w.window_id(), cx))).or_else(|| cx.windows().into_iter().next());
        if let Some(handle) = target {
            let _ = handle.update(cx, |_, window, cx| f(window, cx));
        }
    });
}

/// Lets Enter reach the focused field or button instead of closing the sheet around it.
pub fn pass_enter(_: &ClickEvent, _: &mut Window, cx: &mut App) -> bool {
    cx.propagate();
    false
}

/// Whether a text field has focus, so Enter belongs to the sheet's main action rather than to a focused button.
pub fn typing(window: &Window) -> bool {
    window.context_stack().iter().any(|c| c.contains("Input"))
}

/// Asks before a change that cannot be taken back. Focus starts on Abbrechen, so Enter alone never runs `ok`.
pub fn ask(window: &mut Window, cx: &mut App, title: String, message: String, ok_text: String, ok: impl Fn(&mut Window, &mut App) + 'static) {
    let ok = std::rc::Rc::new(ok);
    let cancel = cx.focus_handle();
    let first = cancel.clone();
    window.open_alert_dialog(cx, move |alert, _, _| {
        let ok = ok.clone();
        let confirm = Button::new("alert-ok").label(ok_text.clone()).danger().on_click(move |_, window, cx| {
            window.close_dialog(cx);
            ok(window, cx);
        });
        alert.title(title.clone()).description(message.clone()).on_ok(pass_enter).footer(
            DialogFooter::new().child(Button::new("alert-cancel").label(tr("Abbrechen")).track_focus(&cancel).on_click(|_, window, cx| window.close_dialog(cx))).child(confirm),
        )
    });
    first.focus(window, cx);
}

/// How a sheet button looks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// The sheet's main action: accent fill, trailing.
    Primary,
    /// Cancel, back and side actions.
    Secondary,
    /// Removing something: red text, no fill.
    Danger,
    /// A quiet text action inside the content.
    Plain,
}

/// A labelled button. A disabled one keeps its place and says why in `reason`; a busy one keeps its label beside a spinner.
pub fn button(id: impl Into<SharedString>, label: impl Into<SharedString>, icon_name: Option<&str>, look: Look, disabled: bool, busy: bool, t: &Theme) -> Stateful<Div> {
    let (id, label) = (id.into(), label.into());
    let (bg, fg, shade) = match look {
        Look::Primary => (Some(t.accent), t.on_accent, t.shade),
        Look::Secondary => (Some(t.ink.opacity(if t.dark { 0.1 } else { 0.05 })), t.primary, t.ink.opacity(tint::HOVER)),
        Look::Danger => (None, t.danger_text, with_alpha(t.danger, tint::HOVER)),
        Look::Plain => (None, t.accent_text, t.ink.opacity(tint::HOVER)),
    };
    let base = div()
        .id(id)
        .mark("button", label.clone())
        .h(px(control::M))
        .px(px(if matches!(look, Look::Plain) { space::S } else { space::M }))
        .rounded(px(radius::INNER))
        .when_some(bg, |d, bg| d.bg(bg))
        .text_color(fg)
        .text_size(px(font::BODY))
        .when(look == Look::Primary, |d| d.font_weight(FontWeight::SEMIBOLD))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(space::XS));
    let inner = |d: Stateful<Div>| {
        d.when(busy, |d| d.child(Spinner::new().color(fg).with_size(px(iconsize::S))))
            .when_some(icon_name.filter(|_| !busy), |d, name| d.child(icon(name, iconsize::S, fg)))
            .child(label.clone())
    };
    if disabled || busy {
        return inner(base.when(disabled && !busy, |d| d.opacity(tint::DISABLED)));
    }
    inner(base.cursor_pointer().key_nav().soft(shade, radius::INNER))
}

/// An icon-only button with a tooltip naming what it does.
pub fn icon_button(id: impl Into<SharedString>, icon_name: &str, label: impl Into<SharedString>, color: Hsla, t: &Theme) -> Stateful<Div> {
    let label: SharedString = label.into();
    div()
        .id(id.into())
        .mark("button", label.clone())
        .size(px(control::M))
        .flex_none()
        .rounded(px(radius::INNER))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(tint::HOVER), radius::INNER)
        .tooltip(tip(label))
        .child(icon(icon_name, iconsize::M, color))
}

/// Title and an optional line under it, at the top of a sheet.
pub fn header(title: impl Into<SharedString>, subtitle: Option<String>, t: &Theme) -> Div {
    let title: SharedString = title.into();
    div()
        .px(px(space::XL))
        .pt(px(space::XL))
        .pb(px(space::M))
        .flex()
        .flex_col()
        .gap(px(space::XS))
        .child(div().id("sheet-title").mark("heading", title.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
        .children(subtitle.map(|s| div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(s)))
}

/// The bottom row: side actions leading, Abbrechen and the main action trailing.
pub fn footer(leading: Vec<AnyElement>, trailing: Vec<AnyElement>, t: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(space::S))
        .px(px(space::XL))
        .py(px(space::L))
        .border_t_1()
        .border_color(t.separator)
        .children(leading)
        .child(div().flex_1())
        .children(trailing)
}

/// A field's label above it.
pub fn label(text: impl Into<SharedString>, t: &Theme) -> Div {
    div().text_size(px(font::CALLOUT)).font_weight(FontWeight::MEDIUM).text_color(t.primary).child(text.into())
}

/// The line under a field: the hint, or the problem in red. It always takes its height, so nothing moves when it changes.
pub fn note(hint: Option<String>, problem: Option<String>, t: &Theme) -> Div {
    let line = div().min_h(px(font::CAPTION * font::LINE)).text_size(px(font::CAPTION));
    match problem {
        Some(p) => line.flex().items_start().gap(px(space::XS)).text_color(t.danger_text).child(div().pt(px(space::XXS)).child(icon("circle-alert", iconsize::XS, t.danger_text))).child(div().min_w_0().child(p)),
        None => line.text_color(t.tertiary).children(hint),
    }
}

/// A field: label, control and the line under it.
pub fn field(label_text: impl Into<SharedString>, control: impl IntoElement, hint: Option<String>, problem: Option<String>, t: &Theme) -> Div {
    div().flex().flex_col().gap(px(space::XS)).child(label(label_text, t)).child(control).child(note(hint, problem, t))
}

/// A status line with an icon: progress, success or failure inside the content.
pub fn status(icon_name: &str, color: Hsla, text: impl Into<SharedString>) -> Div {
    div().flex().items_start().gap(px(space::XS)).text_size(px(font::CALLOUT)).text_color(color).child(div().pt(px(space::XXS)).child(icon(icon_name, iconsize::S, color))).child(div().min_w_0().child(text.into()))
}

/// A spinner beside a quiet line, for work that takes a moment.
pub fn waiting(text: impl Into<SharedString>, t: &Theme) -> Div {
    div().flex().items_center().gap(px(space::S)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(Spinner::new().color(t.secondary).with_size(px(iconsize::S))).child(text.into())
}

/// A tinted box for something the person should know before saving.
pub fn notice(icon_name: &str, color: Hsla, text_color: Hsla, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(space::S))
        .px(px(space::M))
        .py(px(space::S))
        .rounded(px(radius::CONTROL))
        .bg(with_alpha(color, tint::ATTENTION))
        .text_size(px(font::CALLOUT))
        .text_color(text_color)
        .child(div().pt(px(space::XXS)).child(icon(icon_name, iconsize::S, text_color)))
        .child(div().min_w_0().child(text.into()))
}

/// A failure in the content: its plain text with an icon, and a quiet Details control for the technical cause.
/// Keep `open` and the toggle in the owner so the disclosure survives a redraw.
pub fn problem(id: impl Into<SharedString>, icon_name: &str, color: Hsla, problem: &Problem, open: bool, toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, t: &Theme) -> Div {
    let line = status(icon_name, color, problem.text.clone());
    match problem.detail.clone() {
        Some(detail) => div().min_w_0().flex().flex_col().child(line).child(details(id, detail, open, toggle, t)),
        None => div().min_w_0().child(line),
    }
}

/// The Details control and, when open, the technical cause under it.
pub fn details(id: impl Into<SharedString>, detail: String, open: bool, toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, t: &Theme) -> Div {
    let id = id.into();
    let label = if open { tr("Details ausblenden") } else { tr("Details") };
    let more = button(format!("{id}-details"), label, Some(if open { "chevron-down" } else { "chevron-right" }), Look::Plain, false, false, t).on_click(toggle);
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(space::XXS))
        .child(div().flex().child(more))
        .when(open, |d| d.child(div().id(SharedString::from(format!("{id}-detail"))).px(px(space::S)).py(px(space::XS)).rounded(px(radius::SMALL)).bg(t.raised).font_family(Theme::mono_family()).text_size(px(font::CAPTION)).text_color(t.secondary).child(detail)))
}

const HEAD_BATCH: usize = 8;

/// Fetches missing skin heads off the UI thread in small chunks and redraws `V` as they arrive.
pub fn warm_heads<V: 'static>(accounts: Vec<crate::core::account::Account>, cx: &mut Context<V>) {
    cx.spawn(async move |this, cx| {
        for chunk in accounts.chunks(HEAD_BATCH) {
            let chunk = chunk.to_vec();
            let changed = cx.background_executor().spawn(async move { crate::core::skins::prepare(&chunk) }).await;
            if changed && this.update(cx, |_, cx| cx.notify()).is_err() {
                return;
            }
        }
    })
    .detach();
}
