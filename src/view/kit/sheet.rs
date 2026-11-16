//! Sheets: one short task above a dimmed window. Esc or Abbrechen leaves it, Enter runs the default action.

use freya::animation::*;
use freya::prelude::*;

use super::button::Button;
use super::text::{para, spacer, title};
use super::use_theme;
use crate::core::i18n::tr;
use crate::view::theme::{font, frame, motion, space};
use crate::view::ui::Ui;

/// The open sheet: a component that draws its content inside a [`Card`].
#[derive(Clone, PartialEq)]
pub struct Sheet(pub Element);

/// Opens `content` as the window's sheet; one sheet at a time, a second replaces the first.
pub fn open(mut ui: Ui, content: impl IntoElement) {
    ui.menu.set_if_modified(None);
    ui.palette.set_if_modified(false);
    ui.tip.set_if_modified(None);
    ui.sheet.set(Some(Sheet(content.into_element())));
}

/// Closes the window's sheet.
pub fn close(mut ui: Ui) {
    ui.sheet.set_if_modified(None);
}

/// The default action of a sheet: its label, why it is unavailable, and what it does.
#[derive(Clone, PartialEq)]
pub struct Primary {
    pub label: String,
    pub disabled: Option<String>,
    pub danger: bool,
    pub run: EventHandler<()>,
}

/// The frame of a sheet: title, an optional message, the body, and a footer with Abbrechen, an optional second action and
/// the default action.
#[derive(PartialEq)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub message: Option<String>,
    pub body: Option<Element>,
    pub primary: Option<Primary>,
    pub secondary: Option<Primary>,
    pub width: f32,
    /// Only tells something: no Abbrechen, and Esc also runs the default action.
    pub inform: bool,
}

impl Card {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self { id: id.into(), title: title.into(), message: None, body: None, primary: None, secondary: None, width: frame::SHEET, inform: false }
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_element());
        self
    }

    pub fn primary(mut self, primary: Primary) -> Self {
        self.primary = Some(primary);
        self
    }

    /// A second way to finish, left of the default action; it replaces Abbrechen when it is the other answer.
    pub fn secondary(mut self, secondary: Primary) -> Self {
        self.secondary = Some(secondary);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn inform(mut self) -> Self {
        self.inform = true;
        self
    }
}

impl Component for Card {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ui = use_consume::<Ui>();
        let primary = self.primary.clone();
        let keys = primary.clone();
        let mut footer = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(spacer()).content(Content::flex());
        match &self.secondary {
            Some(s) => {
                let run = s.run.clone();
                let b = Button::new(format!("{}.other", self.id), s.label.clone()).disabled(s.disabled.clone()).on_press(move |_| run.call(()));
                footer = footer.child(if s.danger { b.danger() } else { b });
            }
            None if self.inform => {}
            None => footer = footer.child(Button::new(format!("{}.cancel", self.id), tr("Abbrechen")).on_press(move |_| close(ui))),
        }
        let inform = self.inform;
        if let Some(p) = primary {
            let run = p.run.clone();
            let mut b = Button::new(format!("{}.ok", self.id), p.label.clone()).disabled(p.disabled.clone()).on_press(move |_| run.call(()));
            b = if p.danger { b.danger() } else { b.primary() };
            footer = footer.child(b);
        }
        let mut card = rect()
            .a11y_role(AccessibilityRole::Dialog)
            .a11y_alt(self.title.clone())
            .a11y_builder(|n| n.set_modal())
            .width(Size::px(self.width))
            .padding(space::XL)
            .spacing(space::L)
            .background(t.raised)
            .corner_radius(t.shape.sheet)
            .shadow(t.elevation.float.clone())
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_global_key_down(move |e: Event<KeyboardEventData>| match e.key {
                Key::Named(NamedKey::Escape) => {
                    e.stop_propagation();
                    match &keys {
                        Some(p) if inform => p.run.call(()),
                        _ => close(ui),
                    }
                }
                Key::Named(NamedKey::Enter) if e.modifiers.is_empty() => {
                    if let Some(p) = &keys
                        && p.disabled.is_none()
                    {
                        e.stop_propagation();
                        p.run.call(());
                    }
                }
                _ => {}
            })
            .child(title(&t, self.title.clone()));
        if let Some(rim) = t.elevation.rim {
            card = card.border(Border::new().fill(rim).width(1.).alignment(BorderAlignment::Outer));
        }
        if let Some(m) = &self.message {
            card = card.child(para(&t, m.clone(), font::BODY, t.text2));
        }
        if let Some(b) = &self.body {
            card = card.child(b.clone());
        }
        card.child(footer)
    }
}

/// Draws the window's sheet over a scrim that fades in.
#[derive(PartialEq)]
pub struct Host;

impl Component for Host {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let ui = use_consume::<Ui>();
        let sheet = ui.sheet.read().clone();
        let open = sheet.is_some();
        super::use_focus_return(open);
        let fade = use_animation_with_dependencies(&open, |conf, open| {
            conf.on_creation(OnCreation::Finish);
            conf.on_change(OnChange::Rerun);
            let (from, to) = if *open { (0., 1.) } else { (1., 0.) };
            AnimNum::new(from, to).duration(motion::timed(motion::MEDIUM)).ease(Ease::Out)
        });
        let Some(Sheet(content)) = sheet else {
            return rect().into_element();
        };
        let at = fade.get().value();
        let (w, h) = *ui.window.read();
        rect()
            .position(Position::new_global().left(0.).top(0.))
            .layer(Layer::Overlay)
            .width(Size::px(w))
            .height(Size::px(h))
            .background(t.scrim)
            .opacity(at)
            .cross_align(Alignment::Center)
            .padding((frame::SHEET_TOP, space::L, space::L, space::L))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .child(rect().offset_y((1. - at) * space::S).child(content))
            .into_element()
    }
}

/// A sheet that only asks whether to go ahead.
pub fn confirm(ui: Ui, id: &str, heading: String, message: String, action: String, danger: bool, run: impl FnMut(()) + 'static) {
    let mut run = run;
    let primary = Primary { label: action, disabled: None, danger, run: EventHandler::new(move |_| {
        close(ui);
        run(());
    }) };
    open(ui, Card::new(id, heading).message(message).primary(primary).width(frame::SHEET * 0.8));
}
