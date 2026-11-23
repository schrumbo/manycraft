//! What needs the user, by kind: a chip per kind of work with its one bulk action, and the line a server outage leaves under the toolbar.

use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::actions::{OpenSettingsAt, ReconnectMany, RespawnMany, SignInAgain, SignInAgainEach, StartMany};
use super::drive::Mark;
use super::main_window::{Filter, MainWindow};
use super::route::Section;
use super::model::{Model, Need, Outage};
use super::motion::Soft;
use super::sheet::{self, Look};
use super::theme::{Theme, control, font, iconsize, radius, shell, space, tile, tint, with_alpha};
use super::widgets::{KeyNav, icon, tip};
use crate::core::account::AccountKind;
use crate::core::i18n::{tr, trf};

/// The fill, text colour and symbol of a kind of work, the same on its tiles and its chip.
pub fn look(need: Need, t: &Theme) -> (Hsla, Hsla, &'static str) {
    match need {
        Need::SignIn => (t.danger, t.danger_text, "log-in"),
        Need::Banned => (t.danger, t.danger_text, "ban"),
        Need::Disconnected | Need::Offer => (t.attention, t.attention_text, "wifi-off"),
        Need::Dead | Need::Crashed | Need::Failed => (t.attention, t.attention_text, "triangle-alert"),
    }
}

/// "7 Anmeldung": how many need this kind of work.
pub fn label(need: Need, count: usize) -> String {
    match need {
        Need::SignIn => trf("%lld Anmeldung", &[&count]),
        Need::Offer => trf("%lld Ressourcenpaket", &[&count]),
        Need::Disconnected => trf("%lld Getrennt", &[&count]),
        Need::Dead => trf("%lld Gestorben", &[&count]),
        Need::Crashed => trf("%lld Abgestürzt", &[&count]),
        Need::Failed => trf("%lld Fehlgeschlagen", &[&count]),
        Need::Banned => trf("%lld Gesperrt", &[&count]),
    }
}

/// The one action that fixes this kind of work for all of `keys` in `m`, with its label; none for banned accounts.
pub fn fix(need: Need, keys: &[String], m: &Model) -> Option<(String, Box<dyn Action>)> {
    let many = keys.len() > 1;
    let pick = |all: &str, one: &str| if many { tr(all) } else { tr(one) };
    Some(match need {
        Need::SignIn => {
            let mut accounts: Vec<Uuid> = keys.iter().filter_map(|k| m.games.get(k)).map(|i| i.account_id).collect();
            accounts.sort_unstable();
            accounts.dedup();
            let accounts: Vec<&crate::core::account::Account> = accounts.into_iter().filter_map(|id| m.account(id)).filter(|a| a.kind == AccountKind::Microsoft).collect();
            match accounts.as_slice() {
                [] => return None,
                [one] => (trf("%@ anmelden", &[&one.name]), Box::new(SignInAgain(one.id.to_string())) as Box<dyn Action>),
                more => (tr("Alle anmelden"), Box::new(SignInAgainEach(more.iter().map(|a| a.id.to_string()).collect()))),
            }
        }
        Need::Disconnected => (pick("Alle verbinden", "Verbinden"), Box::new(ReconnectMany(keys.to_vec()))),
        Need::Dead => (pick("Alle wiederbeleben", "Wiederbeleben"), Box::new(RespawnMany(keys.to_vec()))),
        Need::Crashed | Need::Failed => (pick("Alle starten", "Starten"), Box::new(StartMany(keys.to_vec()))),
        Need::Banned | Need::Offer => return None,
    })
}

/// The games of `batch` (all when `None`) that need `need`, in name order.
pub fn keys_of(need: Need, batch: Option<&str>, m: &Model) -> Vec<String> {
    let mut games: Vec<&super::model::Game> = m.games.values().filter(|i| batch.is_none_or(|b| i.batch_id == b) && i.phase.need() == Some(need)).collect();
    games.sort_by_key(|i| (i.name.to_lowercase(), i.key.clone()));
    games.into_iter().map(|i| i.key.clone()).collect()
}

/// Every kind of work in `batch` with its count and its fix, for the chips and the palette.
pub fn fixes(batch: Option<&str>, m: &Model) -> Vec<(Need, usize, Option<(String, Box<dyn Action>)>)> {
    m.needs(batch)
        .into_iter()
        .map(|(need, count)| {
            let keys = keys_of(need, batch, m);
            (need, count, fix(need, &keys, m))
        })
        .collect()
}

/// The row of chips under the toolbar while the grid shows what needs attention: one per kind, a click narrows the grid to it,
/// its button fixes all of them.
pub fn chips(this: &MainWindow, batch: Option<&str>, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if this.filter.base() != Filter::Attention {
        return None;
    }
    let t = Theme::get(cx);
    let list = fixes(batch, super::model::get(cx).read(cx));
    if list.is_empty() {
        return None;
    }
    let chosen = if let Filter::Need(n) = this.filter { Some(n) } else { None };
    let row = div()
        .id("needs")
        .flex_none()
        .min_h(px(shell::NEEDS))
        .px(px(tile::INSET))
        .py(px(space::S))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(space::S))
        .border_b_1()
        .border_color(t.separator)
        .children(list.into_iter().map(|(need, count, fix)| chip(need, count, fix, chosen == Some(need), &t, cx)))
        .when(!crate::core::settings::active().phone.enabled, |d| d.child(div().flex_1()).child(phone_link(&t)));
    Some(row.into_any_element())
}

fn phone_link(t: &Theme) -> Stateful<Div> {
    let label = tr("Aufs Handy…");
    div()
        .id("needs-phone")
        .mark("button", label.clone())
        .h(px(control::M))
        .px(px(space::S))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::XS))
        .rounded(px(radius::FULL))
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(tint::HOVER), radius::FULL)
        .child(icon("smartphone", iconsize::XS, t.secondary))
        .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(label))
        .tooltip(tip(tr("Meldungen aufs Handy schicken, wenn ein Account dich braucht")))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettingsAt(Section::Phone)), cx))
}

fn chip(need: Need, count: usize, fix: Option<(String, Box<dyn Action>)>, chosen: bool, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    let (fill, text, symbol) = look(need, t);
    let name = label(need, count);
    let id = format!("{need:?}").to_lowercase();
    let hint = if chosen { tr("Wieder alle zeigen, die dich brauchen") } else { tr("Nur diese zeigen") };
    let toggle = div()
        .id(SharedString::from(format!("need-{id}")))
        .mark("chip", name.clone())
        .h(px(control::M))
        .pl(px(space::S))
        .pr(px(if fix.is_some() { space::XS } else { space::M }))
        .flex()
        .items_center()
        .gap(px(space::XS))
        .rounded(px(radius::FULL))
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(tint::HOVER), radius::FULL)
        .child(icon(symbol, iconsize::XS, fill))
        .child(div().text_size(px(font::CALLOUT)).font_weight(FontWeight::MEDIUM).text_color(if chosen { t.accent_text } else { t.primary }).child(name))
        .tooltip(tip(hint))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.filter = if chosen { Filter::Attention } else { Filter::Need(need) };
            this.grid.reset_scroll();
            window.focus(&this.grid.focus, cx);
            cx.notify();
        }));
    let button = fix.map(|(label, action)| {
        div()
            .id(SharedString::from(format!("need-fix-{id}")))
            .mark("button", label.clone())
            .h(px(control::S))
            .px(px(space::S))
            .flex()
            .items_center()
            .rounded(px(radius::FULL))
            .text_size(px(font::CALLOUT))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(text)
            .cursor_pointer()
            .key_nav()
            .soft(t.ink.opacity(tint::HOVER), radius::FULL)
            .child(label)
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
    });
    div()
        .flex()
        .flex_none()
        .items_center()
        .pr(px(space::XXS))
        .rounded(px(radius::FULL))
        .bg(if chosen { with_alpha(t.highlight, tint::SELECTED) } else { with_alpha(fill, tint::ATTENTION) })
        .when(chosen, |d| d.border_1().border_color(t.selection_ring))
        .child(toggle)
        .when_some(button, |d, b| d.child(div().w(px(1.)).h(px(control::S - space::S)).bg(t.separator)).child(b))
        .into_any_element()
}

/// "14:32" today, "Gestern, 14:32" before: when an outage began.
fn since(at: u64) -> String {
    let secs = i64::try_from(at).unwrap_or(i64::MAX);
    let today = chrono::Local::now().date_naive();
    match chrono::DateTime::from_timestamp(secs, 0).map(|d| d.with_timezone(&chrono::Local)) {
        Some(d) if d.date_naive() == today => d.format("%H:%M").to_string(),
        _ => super::instance_files::when(secs),
    }
}

/// What the outage line says: since when the server is gone and how many wait to get back on.
pub fn outage_line(outage: &Outage, batch_name: Option<&str>, m: &Model) -> String {
    let waiting = outage.keys.iter().filter(|k| m.games.get(*k).is_some_and(|i| matches!(i.phase, super::model::Phase::Connecting { .. }))).count();
    let gave_up = outage.keys.len() - waiting;
    let gone = match outage.since {
        Some(at) => trf("Server weg seit %@", &[&since(at)]),
        None => tr("Server weg"),
    };
    let mut parts: Vec<String> = batch_name.map(str::to_owned).into_iter().collect();
    parts.push(gone);
    if waiting > 0 {
        parts.push(if waiting == 1 { tr("1 verbindet sich neu") } else { trf("%lld verbinden sich neu", &[&waiting]) });
    }
    if gave_up > 0 {
        parts.push(if gave_up == 1 { tr("1 hat aufgegeben") } else { trf("%lld haben aufgegeben", &[&gave_up]) });
    }
    parts.join(" · ")
}

/// A quiet line under the toolbar for each batch in view whose server went away, with a button that reconnects them all now,
/// one after another.
pub fn outages(batch: Option<&str>, t: &Theme, cx: &App) -> Vec<AnyElement> {
    let m = super::model::get(cx).read(cx);
    let ids: Vec<&str> = match batch {
        Some(b) => vec![b],
        None => m.batches.iter().map(|b| b.id.as_str()).collect(),
    };
    let named = batch.is_none() && m.batches.len() > 1;
    ids.into_iter()
        .filter_map(|id| {
            let outage = m.outage(id)?;
            let name = named.then(|| m.batch(id).map(|b| b.name.clone())).flatten();
            let text = outage_line(&outage, name.as_deref(), m);
            let keys = outage.keys.clone();
            Some(
                div()
                    .id(SharedString::from(format!("outage-{id}")))
                    .mark("notice", text.clone())
                    .h(px(shell::NOTICE))
                    .flex_none()
                    .pl(px(tile::INSET))
                    .pr(px(space::S))
                    .flex()
                    .items_center()
                    .gap(px(space::S))
                    .border_b_1()
                    .border_color(t.separator)
                    .child(icon("wifi-off", iconsize::S, t.attention_text))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(font::CALLOUT)).text_color(t.secondary).child(text.clone()))
                    .tooltip(tip(text))
                    .child(
                        sheet::button(format!("outage-reconnect-{id}"), tr("Jetzt alle verbinden"), Some("refresh-cw"), Look::Plain, false, false, t)
                            .tooltip(tip(tr("Nacheinander, ohne gesperrte und abgemeldete Accounts")))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                window.dispatch_action(Box::new(ReconnectMany(keys.clone())), cx);
                            }),
                    )
                    .into_any_element(),
            )
        })
        .collect()
}