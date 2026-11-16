//! The bar that floats over the grid while two or more games are picked: how many, what can be done to them with how many
//! each action reaches, and a field that sends one chat message to all of them that are on a server.

use std::collections::BTreeSet;

use freya::prelude::*;

use super::words;
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::view::commands::Command;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::text::line;
use crate::view::kit::use_theme;
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, frame, space};

thread_local! {
    static MESSAGE: std::cell::Cell<Option<AccessibilityId>> = const { std::cell::Cell::new(None) };
}

/// True while the message field has the keyboard.
pub fn messaging() -> bool {
    let focused = *Platform::get().current_window().focused_accessibility_id.peek();
    MESSAGE.with(|m| m.get() == Some(focused))
}

/// The selection bar; empty while fewer than two games are picked.
#[derive(PartialEq)]
pub struct SelectionBar {}

fn on_server(store: Store) -> Vec<String> {
    let m = store.model.peek();
    store.ui.selection.peek().iter().filter(|k| m.games.get(*k).is_some_and(|g| g.phase.can_chat())).cloned().collect()
}

impl Component for SelectionBar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut ui = store.ui;
        let text = use_state(String::new);
        let field = use_hook(|| {
            let id = AccessibilityId::new_unique();
            MESSAGE.with(|m| m.set(Some(id)));
            id
        });
        let mut sent = use_state(|| None::<usize>);
        let keys: Vec<String> = ui.selection.read().iter().cloned().collect();
        let open = *ui.message.read();
        let many = keys.len() >= 2;
        use_side_effect_with_deps(&many, move |many| {
            if !many {
                ui.message.set_if_modified(false);
                sent.set_if_modified(None);
            }
        });
        if !many {
            return rect().into_element();
        }
        let m = store.model.read();
        let offers = words::offers(&keys, &m);
        let reach = offers[4].able;
        drop(m);
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS).padding((space::XS, space::XS, space::XS, space::M));
        row = row.child(line(&t, trf("%lld ausgewählt", &[&keys.len()]), font::CALLOUT, t.text1).font_weight(FontWeight::SEMI_BOLD)).child(rect().width(Size::px(space::S)));
        if open {
            let hint = match *sent.read() {
                Some(n) => trf("An %lld Accounts gesendet · noch eine Nachricht…", &[&n]),
                None => trf("Nachricht an %lld Accounts…", &[&reach]),
            };
            let mut text_c = text;
            row = row
                .child(
                    Field::new("selection.message", text)
                        .a11y(field)
                        .placeholder(hint)
                        .glyph(Icon::Message)
                        .width(Size::px(frame::SEARCH * 1.5))
                        .auto_focus()
                        .on_submit(move |s: String| {
                            let s = s.trim().to_owned();
                            if s.is_empty() {
                                return;
                            }
                            let to = on_server(store);
                            for key in &to {
                                store.dispatch(AppEvent::SendChat { key: key.clone(), text: s.clone() });
                            }
                            sent.set(Some(to.len()));
                            text_c.set(String::new());
                        }),
                )
                .child(Button::icon("selection.message.close", Icon::Close, tr("Nachricht schließen")).plain().on_press(move |_| ui.message.set(false)));
        } else {
            for o in offers {
                let id = o.id;
                let label = if o.id == "message" { o.name.clone() } else { o.label.clone() };
                let mut b = Button::new(format!("selection.{id}"), label).plain();
                if o.able == 0 {
                    b = b.disabled(Some(match id {
                        "start" => tr("Alle schon gestartet oder Anmeldung nötig"),
                        "stop" => tr("Keiner ist gestartet"),
                        "restart" => tr("Keiner lässt sich neu starten"),
                        "reconnect" => tr("Keiner ist im Spiel"),
                        _ => tr("Keiner ist auf einem Server"),
                    }));
                }
                if let Some(note) = o.note.clone() {
                    b = b.tip(note);
                }
                row = row.child(b.on_press(move |_| match id {
                    "start" => Command::StartSelected.run(store),
                    "stop" => Command::StopSelected.run(store),
                    "restart" => Command::RestartSelected.run(store),
                    "reconnect" => Command::ReconnectSelected.run(store),
                    _ => Command::MessageSelected.run(store),
                }));
            }
        }
        row = row.child(rect().width(Size::px(1.)).height(Size::px(space::L)).background(t.separator)).child(
            Button::icon("selection.clear", Icon::Close, tr("Auswahl aufheben")).plain().on_press(move |_| ui.selection.set(BTreeSet::new())),
        );
        let card = rect()
            .corner_radius(t.shape.sheet)
            .background(t.raised)
            .shadow(t.elevation.float.clone())
            .layer(Layer::Overlay)
            .on_mouse_down(move |e: Event<MouseEventData>| e.stop_propagation())
            .child(row);
        rect().position(Position::new_absolute().left(0.).bottom(space::L)).width(Size::fill()).main_align(Alignment::Center).cross_align(Alignment::Center).child(card).into_element()
    }
}
