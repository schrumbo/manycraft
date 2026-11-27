//! The quiet lines between the toolbar and the games: starts still on their way, servers that went away, and on
//! "Brauchen dich" one chip per kind of work with the action that fixes it.

use freya::prelude::*;

use super::words::{self, Fix};
use crate::app::event::Event as AppEvent;
use crate::app::model::{Model, Need};
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{ring, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, frame, iconsize, space};
use crate::view::ui::{Filter, Show};

/// Everything the grid page shows above its games for `filter`.
#[derive(PartialEq)]
pub struct Strips {
    pub filter: Filter,
}

impl Component for Strips {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let m = store.model.read();
        let batch = match &self.filter {
            Filter::Batch(b) => Some(b.clone()),
            _ => None,
        };
        let mut col = rect().width(Size::fill());
        if let Some(run) = m.run(batch.as_deref()) {
            let (cancel, anyway) = (batch.clone(), batch.clone());
            let mut s = strip(&t, Icon::Play, t.text2, words::run_line(&run), t.text1);
            if run.memory > 0 {
                s = s.child(Button::new("grid.run.anyway", tr("Trotzdem alle starten")).plain().tip(tr("Startet auch, was nicht in den freien Arbeitsspeicher passt")).on_press(move |_| store.dispatch(AppEvent::StartAnyway(anyway.clone()))));
            }
            s = s.child(Button::new("grid.run.cancel", tr("Abbrechen")).plain().tip(tr("Was schon läuft, läuft weiter")).on_press(move |_| store.dispatch(AppEvent::CancelStart(cancel.clone()))));
            let share = if run.total == 0 { 0. } else { run.launched as f32 / run.total as f32 };
            col = col.child(rect().width(Size::fill()).child(s).child(rect().position(Position::new_absolute().left(0.).bottom(0.)).width(Size::percent(share * 100.)).height(Size::px(frame::PROGRESS)).background(t.accent)));
        }
        let ids: Vec<String> = match &batch {
            Some(b) => vec![b.clone()],
            None => m.batches.iter().map(|b| b.id.clone()).collect(),
        };
        let named = batch.is_none() && m.batches.len() > 1;
        for id in ids {
            let Some(outage) = m.outage(&id) else { continue };
            let keys = outage.keys.clone();
            col = col.child(strip(&t, Icon::Wifi, t.attention_text, words::outage_line(&m, &id, &outage, named), t.text1).child(
                Button::new(format!("grid.outage.{id}.reconnect"), tr("Jetzt alle verbinden")).plain().tip(tr("Nacheinander, ohne gesperrte und abgemeldete Accounts")).on_press(move |_| store.dispatch(AppEvent::ReconnectMany(keys.clone()))),
            ));
        }
        if self.filter == Filter::Attention {
            let needs = m.needs(None);
            if !needs.is_empty() {
                let chosen = match *store.ui.show.read() {
                    Show::Need(n) => Some(n),
                    _ => None,
                };
                let chips = needs.into_iter().map(|(need, count)| NeedChip { need, count, chosen: chosen == Some(need), fix: fix_of(&m, need) }.into_element());
                col = col.child(rect().width(Size::fill()).horizontal().spacing(space::S).padding((space::S, space::L)).cross_align(Alignment::Center).border(hairline(&t)).children(chips));
            }
        }
        col
    }
}

/// The label of the one action that fixes `need` for everyone who has it.
pub fn fix_of(m: &Model, need: Need) -> Option<String> {
    words::fix(need, &words::keys_of(need, None, m), m).map(|(label, _)| label)
}

fn hairline(t: &Theme) -> Border {
    Border::new().fill(t.separator).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner)
}

fn strip(t: &Theme, glyph: Icon, tone: Color, text: String, ink: Color) -> Rect {
    rect()
        .width(Size::fill())
        .height(Size::px(frame::NOTICE))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .padding((0., space::S, 0., space::L))
        .border(hairline(t))
        .child(icon(t, glyph, iconsize::S, tone))
        .child(line(t, text, font::CALLOUT, ink).width(Size::flex(1.)).max_lines(1).text_overflow(TextOverflow::Ellipsis))
        .content(Content::flex())
}

/// Carries out the fix of `need` for everyone who needs it.
pub fn run_fix(store: Store, need: Need) {
    let fix = {
        let m = store.model.peek();
        words::fix(need, &words::keys_of(need, None, &m), &m)
    };
    match fix {
        Some((_, Fix::Events(events))) => {
            for e in events {
                store.dispatch(e);
            }
        }
        Some((_, Fix::SignIn(ids))) => crate::view::accounts::sign_in(store, ids),
        Some((_, Fix::Leave(keys))) => {
            let groups = {
                let m = store.model.peek();
                let mut groups: Vec<(String, Vec<uuid::Uuid>)> = Vec::new();
                for g in keys.iter().filter_map(|k| m.games.get(k)) {
                    match groups.iter_mut().find(|(b, _)| *b == g.batch_id) {
                        Some((_, ids)) => ids.push(g.account_id),
                        None => groups.push((g.batch_id.clone(), vec![g.account_id])),
                    }
                }
                groups
            };
            store.dispatch(AppEvent::Stop(keys));
            crate::view::accounts::take_out(store, groups);
        }
        None => {}
    }
}

#[derive(PartialEq)]
struct NeedChip {
    need: Need,
    count: usize,
    chosen: bool,
    fix: Option<String>,
}

impl Component for NeedChip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let label = words::need_label(self.need, self.count);
        let id = format!("grid.need.{}", format!("{:?}", self.need).to_lowercase());
        let spot = use_spot(&id, "chip", &label, true);
        let (tone, glyph) = match self.need {
            Need::SignIn => (t.danger_text, Icon::LogIn),
            Need::Banned => (t.danger_text, Icon::Close),
            Need::Disconnected => (t.attention_text, Icon::Wifi),
            Need::Offer | Need::Conduct => (t.attention_text, Icon::Package),
            Need::Screen => (t.attention_text, Icon::Image),
            Need::Nearby => (t.attention_text, Icon::Users),
            Need::Dead | Need::Crashed | Need::Failed => (t.attention_text, Icon::Alert),
        };
        let (need, chosen) = (self.need, self.chosen);
        let fill = if chosen { t.selection } else if *hovered.read() { t.hover } else { t.control };
        let mut show = store.ui.show;
        let mut toggle = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(label.clone())
            .height(Size::px(control::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .padding((0., space::S))
            .corner_radius(t.shape.chip)
            .background(fill)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_all_press(move |_| {
                a11y.request_focus();
                show.set(if chosen { Show::All } else { Show::Need(need) });
            })
            .child(icon(&t, glyph, iconsize::XS, tone))
            .child(line(&t, label, font::CALLOUT, if chosen { t.accent_text } else { t.text1 }).font_weight(FontWeight::MEDIUM));
        if let Some(border) = ring(&t, focus()) {
            toggle = toggle.border(border);
        }
        let hint = if chosen { tr("Wieder alle zeigen, die dich brauchen") } else { tr("Nur diese zeigen") };
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::XXS).child(WithTip::new(Some(hint), toggle));
        if let Some(fix) = &self.fix {
            row = row.child(Button::new(format!("{id}.fix"), fix.clone()).plain().on_press(move |_| run_fix(store, need)));
        }
        row
    }
}