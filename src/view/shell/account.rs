//! The active account at the foot of the sidebar: who plays an instance that names no account of its own, switched in
//! one click or from the keyboard.

use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::core::account::AccountKind;
use crate::core::i18n::{tr, trf};
use crate::view::commands::{Command, Want, can_want, want, want_blocked};
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::dot;
use crate::view::kit::menu::{self, Face, Item, Menu};
use crate::view::kit::row::Row;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{Bounds, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{frame, iconsize, space};
use crate::view::ui::Page;

fn items(store: Store) -> (Vec<Item>, Vec<Item>) {
    let m = store.model.peek();
    let active = crate::app::reduce::active_account(&m);
    let items: Vec<Item> = m
        .accounts
        .iter()
        .map(|a| {
            let id = a.id;
            let expired = m.sign_in_expired(id);
            let detail = if expired {
                tr("Anmeldung abgelaufen")
            } else if a.kind == AccountKind::Microsoft {
                tr("Microsoft")
            } else {
                tr("Offline")
            };
            let item = Item::new(format!("account.switch.{id}"), a.name.clone(), move |_| store.dispatch(AppEvent::ChooseAccount(id))).face(Face { id, name: a.name.clone(), color: a.color() }).detail(detail).checked(active == Some(id));
            if expired { item.warn() } else { item }
        })
        .collect();
    let mut footer = vec![
        Item::new("account.switch.add", tr("Account hinzufügen…"), move |_| {
            want(store, Want::AddAccount);
        })
        .glyph(Icon::UserPlus)
        .disabled(!can_want(&Want::AddAccount)),
    ];
    let mut page = store.ui.page;
    footer.push(Item::new("account.switch.manage", tr("Accounts verwalten…"), move |_| page.set(Page::ACCOUNTS)).glyph(Icon::Users));
    (items, footer)
}

/// Opens the account switcher above `anchor` when it sits in the lower half of the window, under it otherwise.
pub fn open(store: Store, anchor: Bounds) {
    let low = anchor.y > store.ui.window.peek().1 / 2.;
    let (items, footer) = items(store);
    menu::open(store.ui, if low { Menu::above(items, anchor) } else { Menu::under(items, anchor) }.pinned(footer));
}

/// Opens the account switcher where the sidebar or its drawer shows the active account, or under the toolbar while
/// neither is shown or laid out yet.
pub fn open_from_keys(store: Store) {
    let at = *store.ui.account_at.peek();
    let anchor = match at.filter(|_| *store.ui.sidebar.peek() || *store.ui.drawer.peek()) {
        Some(b) => b,
        None => {
            let w = store.ui.window.peek().0;
            Bounds { x: (w - frame::MENU) / 2., y: 0., w: frame::MENU, h: frame::TOOLBAR }
        }
    };
    open(store, anchor);
}

/// The active account in the sidebar's foot, folded to its face on the rail; adds the first account while there is none.
#[derive(PartialEq)]
pub struct AccountSwitch {
    pub rail: bool,
}

impl Component for AccountSwitch {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut at = store.ui.account_at;
        let m = store.model.read();
        if !m.loaded {
            return rect().into_element();
        }
        let Some(account) = crate::app::reduce::active_account(&m).and_then(|id| m.account(id)) else {
            let blocked = want_blocked(&Want::AddAccount);
            let row = Row::new("nav.account.add", tr("Account hinzufügen…")).glyph(Icon::UserPlus).height(frame::ROW).compact(self.rail, None).on_press(move |_| {
                want(store, Want::AddAccount);
            });
            return WithTip::new(blocked.or_else(|| self.rail.then(|| tr("Account hinzufügen…"))), row).into_element();
        };
        let head = crate::view::accounts::head(&t, store, account.id, &account.name, account.color(), frame::FACE);
        let expired = m.sign_in_expired(account.id);
        let mut tip = trf("Spielt Instanzen ohne eigenen Account · %@", &[&Command::SwitchAccount.keys()]);
        let mut end = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        let mut row = Row::new("nav.account", account.name.clone()).leading(head).height(frame::ROW).compact(self.rail, expired.then(|| dot(t.attention)));
        if expired {
            tip = format!("{} · {tip}", tr("Anmeldung nötig"));
            end = end.child(dot(t.attention));
            row = row.detail(tr("Anmeldung nötig"));
        }
        if self.rail {
            tip = format!("{} · {tip}", account.name);
        }
        let row = row.trailing(end.child(icon(&t, Icon::ChevronDown, iconsize::S, t.text3))).on_press(move |_| open_from_keys(store));
        rect().width(Size::fill()).on_sized(move |e: Event<SizedEventData>| at.set_if_modified(Some(Bounds::of(&e)))).child(WithTip::new(Some(tip), row)).into_element()
    }
}
