//! The first run on the grid: three steps from an empty app to the first game, the done ones ticked and only the next
//! one offered.

use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::app::model::Model;
use crate::core::i18n::{tr, trf};
use crate::view::commands::{self, Command, Want};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::text::{caption, heading, line, para, secondary};
use crate::view::kit::{at_least, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{Theme, control, font, frame, iconsize, space};

/// Whether the grid of all still greets: before the first account, the first instance or the first game played alone.
pub fn due(m: &Model) -> bool {
    let played = m.games.values().any(|g| g.alone()) || m.instances.iter().any(|i| i.last_account.is_some());
    m.loaded && m.batches.is_empty() && (m.accounts.is_empty() || m.instances.is_empty() || !played)
}

/// The welcome card with the steps Account hinzufügen, Instanz erstellen and Spielen.
#[derive(PartialEq)]
pub struct Welcome;

impl Component for Welcome {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        use_hook(move || crate::view::accounts::look_for_prism(store));
        let m = store.model.read();
        let has_account = !m.accounts.is_empty();
        let has_instance = !m.instances.is_empty();
        let first = m.instances.iter().min_by_key(|i| i.name.to_lowercase()).map(|i| (i.id.clone(), i.name.clone()));
        let next = if !has_account { 0 } else if !has_instance { 1 } else { 2 };
        let prism = Command::ImportPrism.available(store);

        let add = Button::new("welcome.account", tr("Hinzufügen…")).primary().disabled(commands::want_blocked(&Want::AddAccount)).tip(Command::AddAccount.tip()).on_press(move |_| {
            commands::want(store, Want::AddAccount);
        });
        let create = Button::new("welcome.instance", tr("Erstellen…")).primary().disabled(commands::want_blocked(&Want::NewInstance)).tip(Command::NewInstance.tip()).on_press(move |_| {
            commands::want(store, Want::NewInstance);
        });
        let play = first.clone().map(|(id, _)| Button::new("welcome.play", tr("Spielen")).glyph(Icon::Play).primary().on_press(move |_| store.dispatch(AppEvent::PlayInstance { instance: id.clone(), account: None })));
        let play_label = first.as_ref().map_or_else(|| tr("Spielen"), |(_, name)| trf("%@ spielen", &[name]));

        let account_action = (next == 0).then(|| {
            let row = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
            let row = if prism { row.child(Button::new("welcome.prism", tr("Aus Prism")).plain().glyph(Icon::Download).tip(Command::ImportPrism.label()).on_press(move |_| Command::ImportPrism.run(store))) } else { row };
            row.child(add).into_element()
        });
        let mut steps = rect().width(Size::fill()).spacing(space::XS);
        steps = steps.child(step(&t, 0, (has_account, next == 0), tr("Account hinzufügen"), account_action));
        steps = steps.child(step(&t, 1, (has_instance, next == 1), tr("Instanz erstellen"), (next == 1).then(|| create.into_element())));
        steps = steps.child(step(&t, 2, (false, next == 2), play_label, play.filter(|_| next == 2).map(IntoElement::into_element)));

        let mut extra = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
        if next < 2 && commands::can_want(&Want::Import) {
            let label = tr("Aus anderem Launcher übernehmen…");
            let tip = Command::Import.shortcut().map_or_else(|| label.clone(), |s| format!("{label} ({})", s.label()));
            extra = extra.child(Button::new("welcome.import", label).plain().glyph(Icon::Download).tip(tip).on_press(move |_| {
                commands::want(store, Want::Import);
            }));
        } else if next >= 2 && commands::can_want(&Want::NewBatch) {
            extra = extra.child(Button::new("welcome.batch", tr("Mehrere Accounts als Batch…")).plain().glyph(Icon::Layers).tip(Command::NewBatch.tip()).on_press(move |_| {
                commands::want(store, Want::NewBatch);
            }));
        }

        let mut card = rect()
            .a11y_role(AccessibilityRole::Group)
            .a11y_alt(tr("Willkommen bei Manycraft"))
            .width(Size::px(frame::WELCOME))
            .padding(space::XL)
            .spacing(space::L)
            .corner_radius(t.shape.sheet)
            .background(t.raised)
            .border(Border::new().fill(t.separator).width(frame::HAIRLINE).alignment(BorderAlignment::Inner))
            .child(rect().width(Size::fill()).spacing(space::XS).child(heading(&t, tr("Willkommen bei Manycraft"))).child(para(&t, tr("Drei Schritte bis zum ersten Spiel."), font::CALLOUT, t.text2)))
            .child(steps);
        if let Some(e) = t.elevation.card.clone() {
            card = card.shadow(e);
        }
        card = card.child(at_least(control::M, extra));
        let mut page = rect().width(Size::fill()).height(Size::fill()).center().padding(space::L).spacing(space::M).child(card);
        if next == 2 {
            page = page.child(caption(&t, tr("Prüfe die Regeln deines Servers, bevor du mehrere Accounts nutzt.")));
        }
        page
    }
}

fn step(t: &Theme, index: usize, (done, now): (bool, bool), label: String, action: Option<Element>) -> Element {
    let mark = rect().width(Size::px(control::S)).height(Size::px(control::S)).center().corner_radius(control::S / 2.);
    let mark = if done {
        mark.background(t.success.with_af32(frame::TILE_WASH)).child(icon(t, Icon::Check, iconsize::S, t.success_text))
    } else if now {
        mark.background(t.accent).child(line(t, (index + 1).to_string(), font::CALLOUT, t.on_accent).font_weight(FontWeight::SEMI_BOLD))
    } else {
        mark.border(Border::new().fill(t.separator).width(frame::BOX_EDGE).alignment(BorderAlignment::Inner)).child(line(t, (index + 1).to_string(), font::CALLOUT, t.text3))
    };
    let state = if done { tr("Erledigt") } else if now { tr("Als Nächstes") } else { tr("Später") };
    let alt = format!("{label} · {state}");
    let text = if done {
        secondary(t, label).width(Size::flex(1.))
    } else if now {
        line(t, label, font::BODY, t.text1).font_weight(FontWeight::SEMI_BOLD).width(Size::flex(1.))
    } else {
        line(t, label, font::BODY, t.text3).width(Size::flex(1.))
    };
    let mut row = rect()
        .a11y_role(AccessibilityRole::ListItem)
        .a11y_alt(alt)
        .width(Size::fill())
        .height(Size::px(control::L))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .child(mark)
        .child(text)
        .content(Content::flex());
    if let Some(a) = action {
        row = row.child(a);
    }
    row.into_element()
}
