use freya::prelude::*;

use super::card::{Card, pixels, rows};
use super::ctl::Ctl;
use super::desk::Look;
use super::lane::{Lane, Pending};
use super::text::step_text;
use crate::app::model::Model;
use crate::core::account::{Account, AccountKind};
use crate::core::i18n::{tr, trf};
use crate::core::skin_change::Model as Arms;
use crate::view::accounts::{head, sign_in};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Face, Item, Menu};
use crate::view::kit::row::Row;
use crate::view::kit::spinner;
use crate::view::kit::text::{caption, para, secondary, strong};
use crate::view::kit::Bounds;
use crate::view::theme::{Theme, font, iconsize, space};

const PREVIEW: f32 = 7.;
const PREVIEW_NARROW: f32 = 5.;
const CAPE: f32 = 3.;
const CHOICE: f32 = 88.;
const CAPES_PER_ROW: usize = 3;
const DETAIL_LINES: usize = 8;
const CHOOSER: f32 = 52.;
const FACE: f32 = 32.;

fn kind_text(a: &Account) -> String {
    if a.kind == AccountKind::Microsoft { tr("Microsoft") } else { tr("Offline") }
}

/// The account whose outfit the stage shows, as a button that opens the list of accounts.
fn chooser(t: &Theme, ctl: Ctl, m: &Model, account: Option<&Account>) -> Element {
    let store = ctl.store;
    let mut anchor = ctl.anchor;
    let chosen = account.map(|a| a.id);
    let accounts: Vec<(uuid::Uuid, String, u8, String)> = m.accounts.iter().map(|a| (a.id, a.name.clone(), a.color(), kind_text(a))).collect();
    let open = move |_| {
        let Some(at) = *anchor.peek() else { return };
        let items: Vec<Item> = accounts
            .iter()
            .map(|(id, name, color, kind)| {
                let id = *id;
                Item::new(format!("wardrobe.account.{id}"), name.clone(), move |_| ctl.choose(id)).face(Face { id, name: name.clone(), color: *color }).detail(kind.clone()).checked(chosen == Some(id))
            })
            .collect();
        menu::open(store.ui, Menu::under(items, at));
    };
    let row = match account {
        Some(a) => Row::new("wardrobe.account", a.name.clone()).leading(head(t, store, a.id, &a.name, a.color(), FACE)).detail(kind_text(a)),
        None => Row::new("wardrobe.account", tr("Account wählen")).glyph(Icon::User),
    };
    let row = row.height(CHOOSER).trailing(icon(t, Icon::ChevronsUpDown, iconsize::S, t.text3)).on_press(open);
    rect().width(Size::fill()).background(t.raised).corner_radius(t.shape.control).on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Some(Bounds::of(&e)))).child(row).into_element()
}

fn notice(t: &Theme, color: Color, text: String) -> Element {
    rect()
        .width(Size::fill())
        .horizontal()
        .spacing(space::XS)
        .content(Content::flex())
        .child(rect().padding((space::XXS, 0., 0., 0.)).child(icon(t, Icon::CircleAlert, iconsize::S, color)))
        .child(para(t, text, font::CALLOUT, color).width(Size::flex(1.)))
        .into_element()
}

/// A problem in words, with its technical detail one click away.
pub fn problem_view(t: &Theme, id: &str, problem: &crate::ui::forms::Problem, mut open: State<bool>) -> Element {
    let mut column = rect().width(Size::fill()).spacing(space::XS).child(notice(t, t.danger_text, problem.text.clone()));
    if let Some(detail) = &problem.detail {
        let shown = *open.read();
        let toggle = Button::new(format!("{id}.details"), if shown { tr("Details ausblenden") } else { tr("Details") }).plain().glyph(if shown { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| open.toggle());
        column = column.child(rect().horizontal().child(toggle));
        if shown {
            column = column.child(rect().width(Size::fill()).padding((space::XS, space::S)).corner_radius(t.shape.small).background(t.well).child(crate::view::kit::text::mono(t, detail.clone(), font::CAPTION, t.text2).width(Size::fill()).max_lines(DETAIL_LINES)));
        }
    }
    column.into_element()
}

fn arms_text(model: Arms) -> String {
    match model {
        Arms::Classic => tr("Klassische Arme"),
        Arms::Slim => tr("Schlanke Arme"),
    }
}

fn preview(t: &Theme, ctl: Ctl, account: &Account, look: Option<&Look>, scale: f32) -> Element {
    let mut card = rect().width(Size::fill()).padding(space::L).background(t.well).corner_radius(t.shape.control).cross_align(Alignment::Center).spacing(space::M);
    let Some(look) = look.filter(|l| l.known || l.skin.is_some()) else {
        return card.height(Size::px(32. * scale + space::L * 2.)).center().child(spinner::waiting(t, tr("Lädt …"))).into_element();
    };
    match ctl.views() {
        Some((front, back)) => {
            card = card.child(rect().horizontal().spacing(space::XL).child(pixels(front, 16. * scale, 32. * scale)).child(pixels(back, 16. * scale, 32. * scale)));
            let name = look.worn.as_deref().and_then(|file| ctl.shelf().iter().find(|h| h.skin.file == file).map(|h| h.skin.name.clone()));
            let mut text = rect().cross_align(Alignment::Center).spacing(2.);
            text = text.child(strong(t, name.unwrap_or_else(|| trf("Skin von %@", &[&account.name])), font::BODY, t.text1));
            text = text.child(caption(t, arms_text(look.model)));
            card = card.child(text);
        }
        None if look.microsoft => {
            card = card.height(Size::px(32. * scale + space::L * 2.)).center().child(secondary(t, tr("Kein Skin gefunden.")));
        }
        None => {}
    }
    if !look.microsoft {
        card = card.child(para(t, tr("Offline-Accounts tragen den Standard-Skin des Spiels. Einen eigenen Skin ziehst du mit einem Microsoft-Account an."), font::CALLOUT, t.text2).width(Size::fill()).text_align(TextAlign::Center));
    }
    card.into_element()
}

fn pending_text(p: Pending) -> String {
    match p {
        Pending::Waiting => tr("Wartet …"),
        Pending::Working(_) => tr("Wird angezogen …"),
    }
}

fn capes(t: &Theme, ctl: Ctl, look: &Look, lane: &Lane) -> Element {
    let failed = |key: String| lane.failed(&key).map(|f| (tr("Nicht geändert"), f.problem.text.clone()));
    let mut column = rect().width(Size::fill()).spacing(space::S).child(strong(t, tr("Umhang"), font::CALLOUT, t.text2));
    if look.capes.is_empty() {
        return column.child(secondary(t, tr("Dieser Account besitzt noch keinen Umhang."))).into_element();
    }
    let none = Card {
        id: "wardrobe.cape.none".into(),
        label: tr("Kein Umhang"),
        caption: Some(tr("Kein Umhang")),
        detail: None,
        picture: rect().width(Size::px(10. * CAPE)).height(Size::px(16. * CAPE)).center().child(icon(t, Icon::CircleOff, iconsize::M, t.text3)).into_element(),
        worn: look.cape.is_none(),
        enabled: true,
        pending: lane.cape_pending(None).map(pending_text),
        failed: failed("cape:".into()),
        width: CHOICE,
        on_press: EventHandler::new(move |_| ctl.wear_cape(None)),
        on_secondary: None,
        remove: None,
    };
    let mut cards = vec![rect().key("none").child(none).into_element()];
    for (i, cape) in look.capes.iter().enumerate() {
        let id = cape.id.clone();
        let picture = match &cape.thumb {
            Some(h) => pixels(h.clone(), 10. * CAPE, 16. * CAPE),
            None => rect().width(Size::px(10. * CAPE)).height(Size::px(16. * CAPE)).into_element(),
        };
        let card = Card {
            id: format!("wardrobe.cape.{i}"),
            label: cape.alias.clone(),
            caption: Some(cape.alias.clone()),
            detail: None,
            picture,
            worn: look.cape.as_deref() == Some(cape.id.as_str()),
            enabled: true,
            pending: lane.cape_pending(Some(&cape.id)).map(pending_text),
            failed: failed(format!("cape:{}", cape.id)),
            width: CHOICE,
            on_press: EventHandler::new(move |_| ctl.wear_cape(Some(id.clone()))),
            on_secondary: None,
            remove: None,
        };
        cards.push(rect().key(cape.id.clone()).child(card).into_element());
    }
    column = column.child(rows(cards, CAPES_PER_ROW, space::S));
    column.into_element()
}

fn empty(t: &Theme, ctl: Ctl) -> Element {
    let store = ctl.store;
    rect()
        .width(Size::fill())
        .padding(space::XL)
        .background(t.well)
        .corner_radius(t.shape.control)
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .child(icon(t, Icon::Shirt, iconsize::HERO, t.text3))
        .child(strong(t, tr("Noch kein Account"), font::BODY, t.text1))
        .child(para(t, tr("Mit einem Microsoft-Account ziehst du hier Skins an und wählst Umhänge."), font::CALLOUT, t.text2).width(Size::fill()).text_align(TextAlign::Center))
        .child(Button::new("wardrobe.add-account", tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().on_press(move |_| {
            crate::view::commands::want(store, crate::view::commands::Want::AddAccount);
        }))
        .into_element()
}

/// The left side of the page: who is dressed, how they look now and which cape they wear.
pub fn stage(t: &Theme, ctl: Ctl, m: &Model, width: Size, wide: bool) -> Element {
    let account = ctl.account();
    let mut column = rect().width(width).spacing(space::M);
    column = column.child(chooser(t, ctl, m, account.as_ref()));
    let Some(account) = account else {
        return column.child(empty(t, ctl)).into_element();
    };
    let store = ctl.store;
    let id = account.id;
    let lane = ctl.lane();
    if let Some(failure) = lane.failure() {
        let mut strip = rect().width(Size::fill()).spacing(space::S).child(problem_view(t, "wardrobe.error", &failure.problem, ctl.details));
        if failure.signin {
            strip = strip.child(rect().horizontal().child(Button::new("wardrobe.signin", tr("Erneut anmelden")).primary().on_press(move |_| sign_in(store, vec![id]))));
        }
        column = column.child(strip);
    }
    if let Some((problem, signin)) = &lane.load {
        let mut strip = rect().width(Size::fill()).spacing(space::S).child(problem_view(t, "wardrobe.load", problem, ctl.details));
        let button = if *signin {
            Button::new("wardrobe.signin", tr("Erneut anmelden")).primary().on_press(move |_| sign_in(store, vec![id]))
        } else {
            Button::new("wardrobe.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| ctl.reload())
        };
        strip = strip.child(rect().horizontal().child(button));
        column = column.child(strip);
    }
    let look = ctl.look();
    let unknown = look.as_ref().is_some_and(|l| l.microsoft && !l.known) && lane.load.is_some();
    if !unknown {
        column = column.child(preview(t, ctl, &account, look.as_ref(), if wide { PREVIEW } else { PREVIEW_NARROW }));
    }
    if let Some((step, waiting)) = lane.working() {
        let text = step.map_or_else(|| tr("Wird verarbeitet …"), step_text);
        let text = if waiting > 0 { trf("%@ · %lld warten", &[&text, &waiting]) } else { text };
        column = column.child(spinner::beside(caption(t, text)));
    }
    if let Some(look) = &look
        && look.microsoft
        && look.known
    {
        column = column.child(capes(t, ctl, look, &lane));
    }
    column.into_element()
}
