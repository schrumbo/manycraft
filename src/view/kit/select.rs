//! A pop-up button that picks one value from a list.

use freya::prelude::*;

use super::menu::{self, Item, Menu};
use super::text::line;
use super::tooltip::WithTip;
use super::{Bounds, ring, use_theme};
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{control, font, frame, iconsize, space};
use crate::view::ui::Ui;

/// Shows the chosen option; a click, Space, Enter or Down opens the list with the choice checked, filterable when long.
#[derive(Clone, PartialEq)]
pub struct Select {
    id: String,
    label: String,
    options: Vec<String>,
    chosen: usize,
    width: f32,
    disabled: Option<String>,
    on_choose: Option<EventHandler<usize>>,
}

impl Select {
    pub fn new(id: impl Into<String>, label: impl Into<String>, options: Vec<String>, chosen: usize) -> Self {
        Self { id: id.into(), label: label.into(), options, chosen, width: 200., disabled: None, on_choose: None }
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Greys the control out and shows `reason` as its tooltip.
    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.disabled = reason;
        self
    }

    pub fn on_choose(mut self, handler: impl FnMut(usize) + 'static) -> Self {
        self.on_choose = Some(EventHandler::new(handler));
        self
    }
}

impl Component for Select {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ui = use_consume::<Ui>();
        let a11y = use_a11y();
        super::use_arrows(a11y);
        let focus = use_focus(a11y);
        let mut bounds = use_state(Bounds::default);
        let mut hovered = use_state(|| false);
        let enabled = self.disabled.is_none();
        let mut spot = use_spot(&self.id, "select", &self.label, enabled);
        let options = self.options.clone();
        let chosen = self.chosen;
        let id = self.id.clone();
        let choose = self.on_choose.clone();
        let open = move || {
            let items = options
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let choose = choose.clone();
                    Item::new(format!("{id}.{i}"), o.clone(), move |_| {
                        a11y.request_focus();
                        if let Some(h) = &choose {
                            h.call(i);
                        }
                    })
                    .checked(i == chosen)
                })
                .collect();
            let menu = Menu::under(items, *bounds.peek());
            menu::open(ui, if options.len() > frame::MENU_ROWS { menu.searchable(tr("Suchen")) } else { menu });
        };
        let mut click = open.clone();
        let mut keys = open;
        let value = self.options.get(chosen).cloned().unwrap_or_default();
        let mut s = rect()
            .a11y_id(a11y)
            .a11y_focusable(enabled)
            .a11y_role(AccessibilityRole::ComboBox)
            .a11y_alt(format!("{}: {value}", self.label))
            .width(Size::px(self.width))
            .height(Size::px(control::M))
            .padding((0., space::S, 0., space::S + 2.))
            .horizontal()
            .cross_align(Alignment::Center)
            .background(if *hovered.read() && enabled { t.hover } else { t.control })
            .corner_radius(t.shape.control)
            .cursor(if enabled { CursorIcon::Pointer } else { CursorIcon::NotAllowed })
            .on_sized(move |e: Event<SizedEventData>| {
                bounds.set_if_modified(Bounds::of(&e));
                spot(e);
            })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_press(move |_| {
                if enabled {
                    a11y.request_focus();
                    click();
                }
            })
            .on_key_down(move |e: Event<KeyboardEventData>| {
                if !a11y.is_focused() || !enabled {
                    return;
                }
                let opens = match &e.key {
                    Key::Named(NamedKey::Enter) | Key::Named(NamedKey::ArrowDown) => true,
                    Key::Character(c) => c == " ",
                    _ => false,
                };
                if opens {
                    e.stop_propagation();
                    keys();
                }
            })
            .child(line(&t, value, font::BODY, if enabled { t.text1 } else { t.text3 }).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.)))
            .child(icon(&t, Icon::ChevronDown, iconsize::S, if enabled { t.text2 } else { t.text3 }))
            .content(Content::flex());
        if let Some(border) = ring(&t, focus()) {
            s = s.border(border);
        }
        WithTip::new(self.disabled.clone(), s)
    }
}
