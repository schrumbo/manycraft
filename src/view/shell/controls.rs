//! The window buttons the app draws itself: the caption buttons of Windows at the right of the title bar, and on a
//! Linux desktop without the system's frame the desktop's buttons on its side, with edges that resize the window.

use freya::prelude::*;
use freya::winit::window::ResizeDirection;
use freya_winit::WinitPlatformExt;

use crate::app::event::Event as AppEvent;
use crate::core::drive;
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::use_theme;
use crate::view::native::frame::{self, Caption, Chrome, Zone};
use crate::view::store::{Store, use_store};
use crate::view::theme::{CLOSE_HOVER, CLOSE_INK, CLOSE_PRESS, frame as size, iconsize, level, space};
use crate::view::ui::Ui;

/// The window buttons, in the order and on the side the desktop wants them.
#[derive(PartialEq)]
pub struct Controls {
    pub chrome: State<Chrome>,
}

impl Component for Controls {
    fn render(&self) -> impl IntoElement {
        let chrome = self.chrome;
        if cfg!(target_os = "windows") {
            return rect()
                .horizontal()
                .child(WinButton { caption: Caption::Minimize, chrome })
                .child(WinButton { caption: Caption::Maximize, chrome })
                .child(WinButton { caption: Caption::Close, chrome })
                .into_element();
        }
        let (order, left) = frame::buttons();
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(size::CAPTION_GAP).padding(if left { (0., space::S, 0., space::M) } else { (0., space::M, 0., space::S) });
        for caption in order {
            row = row.child(RoundButton { caption, chrome });
        }
        row.into_element()
    }
}

fn name_of(caption: Caption, maximized: bool) -> String {
    match caption {
        Caption::Minimize => tr("Minimieren"),
        Caption::Maximize if maximized => tr("Wiederherstellen"),
        Caption::Maximize => tr("Maximieren"),
        Caption::Close => tr("Schließen"),
    }
}

fn spot_id(caption: Caption) -> &'static str {
    match caption {
        Caption::Minimize => "titlebar.minimize",
        Caption::Maximize => "titlebar.maximize",
        Caption::Close => "titlebar.close",
    }
}

fn act(store: Store, caption: Caption) {
    match caption {
        Caption::Minimize => frame::minimize(),
        Caption::Maximize => frame::toggle_maximize(),
        Caption::Close => store.dispatch(AppEvent::CloseWindow),
    }
}

#[derive(PartialEq)]
struct WinButton {
    caption: Caption,
    chrome: State<Chrome>,
}

impl Component for WinButton {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let caption = self.caption;
        let c = *self.chrome.read();
        let mut hovered = use_state(|| false);
        let mut held = use_state(|| false);
        let name = name_of(caption, c.maximized);
        let mut zone = frame::use_zone(Zone::Button(caption));
        let mut spot = use_spot(spot_id(caption), "button", &name, true);
        let hover = c.hover == Some(caption) || *hovered.read();
        let pressed = hover && (c.pressed == Some(caption) || *held.read());
        let close = caption == Caption::Close;
        let fill = match (close, hover, pressed) {
            (true, _, true) => CLOSE_PRESS,
            (true, true, false) => CLOSE_HOVER,
            (false, _, true) => t.press,
            (false, true, false) => t.hover,
            _ => Color::TRANSPARENT,
        };
        let ink = if close && hover { CLOSE_INK } else if c.inactive { t.text3 } else { t.text1 };
        rect()
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(name)
            .width(Size::px(size::CAPTION_W))
            .height(Size::px(size::TITLEBAR))
            .center()
            .background(fill)
            .on_sized(move |e: Event<SizedEventData>| {
                zone(&e);
                spot(e);
            })
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| {
                hovered.set_if_modified(false);
                held.set_if_modified(false);
            })
            .on_pointer_down(move |e: Event<PointerEventData>| {
                e.stop_propagation();
                held.set(true);
            })
            .on_mouse_up(move |_| held.set_if_modified(false))
            .on_press(move |_| act(store, caption))
            .child(label().text(frame::glyph(caption, c.maximized)).font_family(frame::glyph_font()).font_size(size::CAPTION_GLYPH).color(ink))
    }
}

#[derive(PartialEq)]
struct RoundButton {
    caption: Caption,
    chrome: State<Chrome>,
}

impl Component for RoundButton {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let caption = self.caption;
        let c = *self.chrome.read();
        let mut hovered = use_state(|| false);
        let mut held = use_state(|| false);
        let name = name_of(caption, c.maximized);
        let mut zone = frame::use_zone(Zone::Button(caption));
        let mut spot = use_spot(spot_id(caption), "button", &name, true);
        let glyph = match caption {
            Caption::Minimize => Icon::Minus,
            Caption::Maximize if c.maximized => Icon::Copy,
            Caption::Maximize => Icon::Square,
            Caption::Close => Icon::Close,
        };
        let fill = if *held.read() && *hovered.read() { t.press } else if *hovered.read() { t.hover } else { t.control };
        let ink = if c.inactive { t.text3 } else { t.text1 };
        let button = rect()
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(name.clone())
            .width(Size::px(size::CAPTION_ROUND))
            .height(Size::px(size::CAPTION_ROUND))
            .corner_radius(size::CAPTION_ROUND / 2.)
            .cursor(CursorIcon::Pointer)
            .center()
            .background(fill)
            .on_sized(move |e: Event<SizedEventData>| {
                zone(&e);
                spot(e);
            })
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| {
                hovered.set_if_modified(false);
                held.set_if_modified(false);
            })
            .on_pointer_down(move |e: Event<PointerEventData>| {
                e.stop_propagation();
                held.set(true);
            })
            .on_mouse_up(move |_| held.set_if_modified(false))
            .on_press(move |_| act(store, caption))
            .child(icon(&t, glyph, iconsize::S, ink));
        WithTip::new(Some(name), button)
    }
}

/// The edges and corners that resize a window without the system's frame; gone while it is maximized.
#[derive(PartialEq)]
pub struct Edges {
    pub chrome: State<Chrome>,
}

impl Component for Edges {
    fn render(&self) -> impl IntoElement {
        let ui = use_consume::<Ui>();
        let c = *self.chrome.read();
        if !cfg!(target_os = "linux") || c.framed || c.maximized {
            return rect().into_element();
        }
        let (w, h) = *ui.window.read();
        let (e, c) = (size::EDGE, size::EDGE * 2.);
        let bands = [
            (0., 0., c, c, ResizeDirection::NorthWest, CursorIcon::NwResize),
            (c, 0., w - 2. * c, e, ResizeDirection::North, CursorIcon::NResize),
            (w - c, 0., c, c, ResizeDirection::NorthEast, CursorIcon::NeResize),
            (w - e, c, e, h - 2. * c, ResizeDirection::East, CursorIcon::EResize),
            (w - c, h - c, c, c, ResizeDirection::SouthEast, CursorIcon::SeResize),
            (c, h - e, w - 2. * c, e, ResizeDirection::South, CursorIcon::SResize),
            (0., h - c, c, c, ResizeDirection::SouthWest, CursorIcon::SwResize),
            (0., c, e, h - 2. * c, ResizeDirection::West, CursorIcon::WResize),
        ];
        let mut layer = rect();
        for (x, y, bw, bh, direction, cursor) in bands {
            layer = layer.child(
                rect()
                    .position(Position::new_global().left(x).top(y))
                    .layer(Layer::OverlayLevel(level::EDGE))
                    .width(Size::px(bw.max(0.)))
                    .height(Size::px(bh.max(0.)))
                    .cursor(cursor)
                    .on_pointer_down(move |ev: Event<PointerEventData>| {
                        if ev.button() != Some(MouseButton::Left) {
                            return;
                        }
                        ev.stop_propagation();
                        if drive::fenced("resize", "main window") {
                            return;
                        }
                        Platform::get().with_window(Platform::window_id(), move |window| {
                            let _ = window.drag_resize_window(direction);
                        });
                    }),
            );
        }
        layer.into_element()
    }
}
