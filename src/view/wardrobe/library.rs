use freya::prelude::*;

use super::card::{Card, Removal, pixels, rows};
use super::ctl::{Ctl, keep_text};
use super::desk::Hung;
use super::lane::{Lane, Pending};
use super::rename;
use super::stage::problem_view;
use crate::core::i18n::{tr, trf};
use crate::core::skin_change::Model as Arms;
use crate::core::wardrobe::Origin;
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::spinner;
use crate::view::kit::text::{para, secondary, strong};
use crate::view::theme::{Theme, font, space};

const SCALE: f32 = 4.;
const MIN_CARD: f32 = 128.;
const GAP: f32 = space::M;
const DEFAULT_PER_ROW: usize = 4;

/// How many cards of at least [`MIN_CARD`] fit `width`, and how wide each then is.
fn fit(width: f32) -> (usize, f32) {
    if width < MIN_CARD {
        return (DEFAULT_PER_ROW, MIN_CARD);
    }
    let n = (((width + GAP) / (MIN_CARD + GAP)).floor() as usize).max(1);
    (n, ((width - GAP * (n - 1) as f32) / n as f32).floor())
}

fn origin_text(o: Origin) -> String {
    match o {
        Origin::File => tr("Datei"),
        Origin::Link => tr("Link"),
        Origin::Player => tr("Spieler"),
        Origin::Worn => tr("Vom Account"),
        Origin::Recent => tr("Zuletzt"),
    }
}

fn pending_text(p: Pending) -> String {
    match p {
        Pending::Waiting => tr("Wartet …"),
        Pending::Working(_) => tr("Wird angezogen …"),
    }
}

fn hang(ctl: Ctl, lane: &Lane, hung: &Hung, worn: bool, width: f32) -> Element {
    let file = hung.skin.file.clone();
    let name = hung.skin.name.clone();
    let model = hung.skin.model;
    let press_file = file.clone();
    let pending = lane.skin_pending(&file).map(pending_text);
    let failed = lane.failure.as_ref().filter(|f| f.key == format!("skin:{file}")).map(|f| (tr("Nicht geändert"), f.problem.text.clone()));
    let microsoft = ctl.account().is_some_and(|a| a.kind == crate::core::account::AccountKind::Microsoft);
    let keep = worn.then(keep_text);
    let menu_file = file.clone();
    let menu_name = name.clone();
    let menu_keep = keep.is_some();
    let on_secondary = EventHandler::new(move |at: (f32, f32)| {
        let wear_file = menu_file.clone();
        let rename_file = menu_file.clone();
        let rename_name = menu_name.clone();
        let model_file = menu_file.clone();
        let remove_file = menu_file.clone();
        let items = vec![
            Item::new("wardrobe.menu.wear", tr("Anziehen"), move |_| ctl.wear_skin(&wear_file)).disabled(!microsoft || worn),
            Item::new("wardrobe.menu.rename", tr("Umbenennen…"), move |_| rename::open(ctl, rename_file.clone(), rename_name.clone())),
            Item::new("wardrobe.menu.model", if model == Arms::Slim { tr("Klassische Arme") } else { tr("Schlanke Arme") }, move |_| ctl.set_model(&model_file, if model == Arms::Slim { Arms::Classic } else { Arms::Slim })),
            Item::Separator,
            Item::new("wardrobe.menu.remove", tr("Entfernen"), move |_| ctl.remove(&remove_file)).danger().disabled(menu_keep),
        ];
        menu::open(ctl.store.ui, Menu::at(items, at));
    });
    let remove_file = file.clone();
    Card {
        id: format!("wardrobe.skin.{}", &file[..file.len().min(8)]),
        label: name.clone(),
        caption: Some(name),
        detail: Some(if worn { tr("Getragen") } else { origin_text(hung.skin.origin) }),
        picture: pixels(hung.thumb.clone(), 16. * SCALE, 32. * SCALE),
        worn,
        enabled: true,
        pending,
        failed,
        width,
        on_press: EventHandler::new(move |_| ctl.wear_skin(&press_file)),
        on_secondary: Some(on_secondary),
        remove: Some(Removal { label: tr("Entfernen"), on: EventHandler::new(move |_| ctl.remove(&remove_file)), why_not: keep }),
    }
    .into_element()
}

fn arriving(id: u64, label: String, width: f32) -> Element {
    Card {
        id: format!("wardrobe.adding.{id}"),
        label: label.clone(),
        caption: Some(label),
        detail: Some(tr("Wird geladen …")),
        picture: rect().width(Size::px(16. * SCALE)).height(Size::px(32. * SCALE)).center().child(spinner::Spinner).into_element(),
        worn: false,
        enabled: false,
        pending: None,
        failed: None,
        width,
        on_press: EventHandler::new(|()| {}),
        on_secondary: None,
        remove: None,
    }
    .into_element()
}

fn add_bar(t: &Theme, ctl: Ctl) -> Element {
    let choosing = *ctl.choosing.read();
    let typed = ctl.text.read().trim().is_empty();
    let problem = ctl.add_problem.read().clone();
    let field = Field::new("wardrobe.add.input", ctl.text).placeholder(tr("Link oder Spielername")).glyph(Icon::Link).width(Size::flex(1.)).on_submit(move |_| ctl.add_typed());
    let add = Button::new("wardrobe.add", tr("Hinzufügen")).primary().disabled(typed.then(|| tr("Gib einen Link oder einen Spielernamen ein."))).on_press(move |_| ctl.add_typed());
    let file = Button::new("wardrobe.add.file", tr("Datei…")).glyph(Icon::Upload).disabled(choosing.then(|| tr("Der Dateidialog ist offen."))).on_press(move |_| ctl.choose_file());
    let mut column = rect().width(Size::fill()).spacing(space::S).child(rect().width(Size::fill()).horizontal().content(Content::flex()).cross_align(Alignment::Start).spacing(space::S).child(field).child(add).child(file));
    if let Some(p) = problem {
        column = column.child(problem_view(t, "wardrobe.add", &p, ctl.add_details));
    }
    column.into_element()
}

fn empty(t: &Theme) -> Element {
    rect()
        .width(Size::fill())
        .padding(space::XL)
        .background(t.well)
        .corner_radius(t.shape.control)
        .cross_align(Alignment::Center)
        .spacing(space::XS)
        .child(strong(t, tr("Noch keine Skins"), font::BODY, t.text1))
        .child(para(t, tr("Füg oben einen Link oder Spielernamen hinzu, wähl eine Datei oder zieh ein PNG hierher."), font::CALLOUT, t.text2).width(Size::fill()).text_align(TextAlign::Center))
        .into_element()
}

/// The right side of the page: the skins of the library, to put on and to add to.
pub fn library(t: &Theme, ctl: Ctl, width: Size, dropping: bool) -> Element {
    let grid = *ctl.grid.read();
    let shelf = ctl.shelf();
    let adds = ctl.adds();
    let look = ctl.look();
    let lane = ctl.lane();
    let worn = look.as_ref().and_then(|l| l.worn.clone());
    let (per_row, card) = fit(grid);
    let mut measure = ctl.grid;
    let mut column = rect().width(width).spacing(space::M);
    column = column.child(add_bar(t, ctl));
    let hint = (!look.as_ref().is_some_and(|l| l.microsoft) && look.is_some()).then(|| secondary(t, tr("Zum Anziehen brauchst du einen Microsoft-Account.")));
    let count = if shelf.is_empty() { tr("Skins") } else { trf("Skins · %lld", &[&shelf.len()]) };
    column = column.child(strong(t, count, font::CALLOUT, t.text2));
    if let Some(hint) = hint {
        column = column.child(hint);
    }
    let body = if shelf.is_empty() && adds.is_empty() {
        empty(t)
    } else {
        let mut cards: Vec<Element> = adds.iter().rev().map(|(n, label)| rect().key(format!("adding.{n}")).child(arriving(*n, label.clone(), card)).into_element()).collect();
        cards.extend(shelf.iter().map(|h| rect().key(h.skin.file.clone()).child(hang(ctl, &lane, h, worn.as_deref() == Some(h.skin.file.as_str()), card)).into_element()));
        rows(cards, per_row, GAP)
    };
    let mut zone = rect().width(Size::fill()).on_sized(move |e: Event<SizedEventData>| measure.set_if_modified(e.area.width())).child(body);
    if dropping {
        zone = zone.child(secondary(t, tr("Zum Hinzufügen loslassen.")));
    }
    column.child(zone).into_element()
}
