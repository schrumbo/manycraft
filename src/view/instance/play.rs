//! Play in the toolbar of an instance: the account it starts with, and while its own game runs the way back to it and Stop.

use freya::prelude::*;
use uuid::Uuid;

use super::{Facts, facts};
use crate::app::event::Event as AppEvent;
use crate::core::account::AccountKind;
use crate::core::i18n::{tr, trf};
use crate::view::commands::{Shortcut, Want, want, want_blocked};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Face, Item, Menu};
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{Bounds, ring, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{control, font, frame, iconsize, space};

const ACCOUNTS_SEARCH_FROM: usize = 6;

fn tip(label: String, shortcut: Shortcut) -> String {
    format!("{label} ({})", shortcut.label())
}

fn account_items(store: Store, f: &Facts) -> Vec<Item> {
    let m = store.model.peek();
    let mut items = Vec::new();
    for (account, name) in &f.accounts {
        let (instance, id) = (f.made.id.clone(), *account);
        let kind = m.account(id).map(|a| a.kind);
        let mut item = if let Some(place) = f.busy_in(id) {
            Item::new(format!("instance.play.as.{id}"), name.clone(), |_| {}).shortcut(trf("läuft in %@", &[&place])).disabled(true)
        } else if f.playing.contains(account) {
            Item::new(format!("instance.play.as.{id}"), name.clone(), |_| {}).shortcut(tr("Läuft")).disabled(true)
        } else if f.expired.contains(account) {
            Item::new(format!("instance.play.as.{id}"), name.clone(), move |_| crate::view::accounts::sign_in(store, vec![id])).shortcut(tr("Anmelden"))
        } else {
            let tag = if kind == Some(AccountKind::Microsoft) { tr("Microsoft") } else { tr("Offline") };
            Item::new(format!("instance.play.as.{id}"), name.clone(), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account: Some(id) })).shortcut(tag)
        };
        if let Some(a) = m.account(id) {
            item = item.face(Face { id, name: a.name.clone(), color: a.color() });
        }
        items.push(item.checked(f.pick == Some(id)));
    }
    if f.pinned {
        let instance = f.made.id.clone();
        items.push(Item::Separator);
        items.push(Item::new("instance.play.default.clear", tr("Standard-Account verwenden"), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account: None })));
    }
    items
}

/// The account Play starts with, as a pull-down that picks the instance's own account.
#[derive(PartialEq)]
struct AccountChip {
    facts: Facts,
    compact: bool,
}

impl Component for AccountChip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut anchor = use_state(Bounds::default);
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let f = &self.facts;
        let pick = f.pick.or(f.solo);
        let name = pick.and_then(|a| f.name_of(a)).unwrap_or_default().to_owned();
        let label = trf("als %@", &[&name]);
        let spot = use_spot("instance.play.account", "button", &label, true);
        let menu_facts = f.clone();
        let open = move || {
            let items = account_items(store, &menu_facts);
            let menu = Menu::under(items, *anchor.peek());
            menu::open(store.ui, if menu_facts.accounts.len() >= ACCOUNTS_SEARCH_FROM { menu.searchable(tr("Account suchen")) } else { menu });
        };
        let keys_open = open.clone();
        let mut press_open = open;
        let instead = f.solo.filter(|s| Some(*s) != f.pick).and_then(|s| Some((f.name_of(s)?.to_owned(), f.busy_in(s)?.to_owned())));
        let tip = match instead {
            Some((who, place)) => trf("statt %1$@ (läuft in %2$@)", &[&who, &place]),
            None => tr("Account wählen"),
        };
        let wash = if *pressed.read() {
            t.press
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let m = store.model.read();
        let face = pick.and_then(|a| m.account(a)).map(|a| crate::view::accounts::head(&t, store, a.id, &a.name, a.color(), frame::FACE));
        drop(m);
        let mut chip = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(format!("{label}, {}", tr("Account wählen")))
            .height(Size::px(control::L))
            .padding((0., space::S, 0., space::XS + space::XXS))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS + space::XXS)
            .corner_radius(t.shape.control)
            .background(t.control)
            .cursor(CursorIcon::Pointer)
            .on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e)))
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |e: Event<MouseEventData>| {
                if e.button == Some(MouseButton::Left) {
                    pressed.set(true);
                }
            })
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                a11y.request_focus();
                press_open();
            })
            .on_key_down(move |e: Event<KeyboardEventData>| {
                let go = matches!(&e.key, Key::Named(NamedKey::Enter | NamedKey::ArrowDown)) || matches!(&e.key, Key::Character(c) if c == " ");
                if a11y.is_focused() && go {
                    e.stop_propagation();
                    keys_open.clone()();
                }
            });
        if let Some(face) = face {
            chip = chip.child(face);
        }
        if !self.compact {
            chip = chip.child(line(&t, label, font::BODY, t.text1).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis).max_width(Size::px(frame::CHIP_NAME)));
        }
        chip = chip.child(icon(&t, Icon::ChevronDown, iconsize::S, t.text2));
        if let Some(border) = ring(&t, focus()) {
            chip = chip.border(border);
        }
        let chip = rect().corner_radius(t.shape.control).background(wash).on_sized(spot).child(chip);
        WithTip::new(Some(tip), chip)
    }
}

/// Play, Stop and the way back to the running game, at the trailing end of the instance's toolbar.
#[derive(PartialEq)]
pub struct Hero {
    pub id: String,
    /// The width the toolbar has, so the account chip folds to its face when it runs short.
    pub room: f32,
}

impl Component for Hero {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut ui = store.ui;
        let Some(f) = facts(&store.model.read(), &self.id) else { return rect().into_element() };
        let row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        if f.accounts.is_empty() {
            return row.child(Button::new("instance.play", tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().tall().on_press(move |_| {
                want(store, Want::AddAccount);
            })).into_element();
        }
        if let Some((key, overdue)) = f.stopping.clone() {
            let b = if overdue {
                Button::new("instance.play", tr("Sofort beenden")).glyph(Icon::Stop).danger().tall().tip(trf("Nicht Gespeichertes geht verloren (%@)", &[&Shortcut::primary(Code::Period).label()])).on_press(move |_| store.dispatch(AppEvent::Stop(vec![key.clone()])))
            } else {
                Button::new("instance.play", tr("Wird gestoppt…")).glyph(Icon::Stop).tall().disabled(Some(tr("Das Spiel speichert und beendet sich.")))
            };
            return row.child(b).into_element();
        }
        if let Some((first, _)) = f.stoppable.first().cloned() {
            let keys: Vec<String> = f.stoppable.iter().map(|(k, _)| k.clone()).collect();
            let label = if keys.len() == 1 { tr("Stoppen") } else { tr("Alle stoppen") };
            return row
                .child(Button::new("instance.stop", label.clone()).glyph(Icon::Stop).tall().tip(tip(label, Shortcut::primary(Code::Period))).on_press(move |_| store.dispatch(AppEvent::Stop(keys.clone()))))
                .child(Button::new("instance.play", tr("Zum Spiel wechseln")).glyph(Icon::External).primary().tall().on_press(move |_| store.dispatch(AppEvent::Raise(first.clone()))))
                .into_element();
        }
        if f.none_free() {
            return row
                .child(Button::new("instance.play.add", tr("Account hinzufügen…")).plain().on_press(move |_| {
                want(store, Want::AddAccount);
            }))
                .child(Button::new("instance.play", tr("Alle Accounts beschäftigt")).glyph(Icon::Play).primary().tall().disabled(Some(tr("Jeder Account spielt diese Instanz gerade in einem Batch."))))
                .into_element();
        }
        let mut row = row;
        if f.accounts.len() > 1 {
            row = row.child(AccountChip { facts: f.clone(), compact: self.room < frame::TOOLBAR_SEARCH });
        }
        let (instance, pick) = (f.made.id.clone(), f.pick);
        let main = match pick.filter(|a| f.expired.contains(a)) {
            Some(account) => Button::new("instance.play", trf("Anmelden als %@", &[&f.name_of(account).unwrap_or_default()]))
                .glyph(Icon::LogIn)
                .primary()
                .tall()
                .tip(tr("Die Anmeldung ist abgelaufen."))
                .disabled(want_blocked(&Want::SignInAgain(Vec::new())))
                .on_press(move |_| crate::view::accounts::sign_in(store, vec![account])),
            None => Button::new("instance.play", tr("Spielen")).glyph(Icon::Play).primary().tall().tip(tip(tr("Spielen"), Shortcut::primary(Code::KeyR))).on_press(move |_| {
                ui.menu.set_if_modified(None);
                store.dispatch(AppEvent::PlayInstance { instance: instance.clone(), account: pick });
            }),
        };
        row.child(main).into_element()
    }
}

/// The account Play starts with now, for the palette and the shortcut.
pub fn pick(f: &Facts) -> Option<Uuid> {
    if f.none_free() { None } else { f.pick }
}

/// The one action of an empty tab that fills when the instance has been played: Play, or the way back to its game.
pub fn empty_action(store: Store, f: &Facts, id: &str) -> Option<Button> {
    if f.accounts.is_empty() {
        return Some(Button::new(id.to_owned(), tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().on_press(move |_| {
            want(store, Want::AddAccount);
        }));
    }
    if let Some((first, _)) = f.stoppable.first().cloned() {
        return Some(Button::new(id.to_owned(), tr("Zum Spiel wechseln")).glyph(Icon::External).primary().on_press(move |_| store.dispatch(AppEvent::Raise(first.clone()))));
    }
    if f.stopping.is_some() {
        return None;
    }
    let Some(account) = pick(f).filter(|a| !f.expired.contains(a)) else {
        return Some(Button::new(id.to_owned(), tr("Alle Accounts beschäftigt")).glyph(Icon::Play).primary().disabled(Some(tr("Jeder Account spielt diese Instanz gerade in einem Batch."))));
    };
    let instance = f.made.id.clone();
    Some(Button::new(id.to_owned(), tr("Spielen")).glyph(Icon::Play).primary().on_press(move |_| store.dispatch(AppEvent::PlayInstance { instance: instance.clone(), account: Some(account) })))
}
