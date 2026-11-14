//! The sidebar: all instances, one row per batch with what needs attention, accounts and chat.

use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{EditBatch, NewBatch, StartBatch, StopBatch};
use super::drive::Mark;
use super::main_window::{MainWindow, Page};
use super::model;
use super::motion::Soft;
use super::theme::{Theme, font, iconsize, radius, shell, space, tint};
use super::widgets::{KeyNav, icon, tip};
use crate::core::i18n::{tr, trf};

struct Row {
    id: String,
    page: Page,
    icon: &'static str,
    label: String,
    trailing: Option<String>,
    attention: usize,
    hint: Option<String>,
}

fn row(this: &MainWindow, r: Row, t: &Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let selected = this.page == r.page;
    let page = r.page.clone();
    let ink = if selected { t.accent_text } else { t.primary };
    div()
        .id(SharedString::from(r.id))
        .mark("row", r.label.clone())
        .h(px(shell::ROW))
        .px(px(space::S))
        .rounded(px(radius::INNER))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::S))
        .bg(if selected { t.accent.opacity(tint::ATTENTION) } else { t.window })
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(tint::HOVER), radius::INNER)
        .child(icon(r.icon, iconsize::M, if selected { t.accent_text } else { t.secondary }))
        .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).text_color(ink).when(selected, |d| d.font_weight(FontWeight::MEDIUM)).child(r.label))
        .when(r.attention > 0, |d| {
            d.child(
                div()
                    .h(px(shell::BADGE))
                    .min_w(px(shell::BADGE))
                    .px(px(space::XS))
                    .rounded(px(radius::FULL))
                    .bg(t.attention)
                    .text_color(t.on_attention)
                    .text_size(px(font::CAPTION2))
                    .font_weight(FontWeight::BOLD)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(r.attention.to_string()),
            )
        })
        .when_some(r.trailing, |d, text| d.child(div().flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).child(text)))
        .when_some(r.hint, |d, hint| d.tooltip(tip(hint)))
        .on_click(cx.listener(move |this, _, window, cx| this.go(page.clone(), window, cx)))
}

fn heading(text: String, t: &Theme) -> Div {
    div().px(px(space::S)).pt(px(space::M)).pb(px(space::XS)).text_size(px(font::CAPTION)).font_weight(FontWeight::SEMIBOLD).text_color(t.tertiary).child(text)
}

/// Draws the sidebar.
pub fn render(this: &MainWindow, _window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = Theme::get(cx);
    let model = model::get(cx);
    let m = model.read(cx);
    let all = Row {
        id: "nav-all".into(),
        page: Page::Grid(None),
        icon: "layout-grid",
        label: tr("Alle"),
        trailing: None,
        attention: m.attention_count(None),
        hint: None,
    };
    let batches: Vec<(Row, String, usize, usize)> = m
        .batches
        .iter()
        .map(|b| {
            let total = m.instances.values().filter(|i| i.batch_id == b.id).count();
            let running = m.instances.values().filter(|i| i.batch_id == b.id && i.phase.is_active()).count();
            let attention = m.attention_count(Some(&b.id));
            let fine = m.instances.values().filter(|i| i.batch_id == b.id && i.phase.is_active() && !i.phase.needs_attention()).count();
            let hint = trf("%lld von %lld laufen", &[&fine, &total]);
            let stopped = total - running;
            (Row { id: format!("nav-batch-{}", b.id), page: Page::Grid(Some(b.id.clone())), icon: "boxes", label: b.name.clone(), trailing: Some(format!("{fine}/{total}")), attention, hint: Some(hint) }, b.id.clone(), running, stopped)
        })
        .collect();
    let accounts = Row { id: "nav-accounts".into(), page: Page::Accounts, icon: "users", label: tr("Accounts"), trailing: Some(m.accounts.len().to_string()), attention: 0, hint: None };
    let chat = Row { id: "nav-chat".into(), page: Page::Chat, icon: "messages-square", label: tr("Chat"), trailing: None, attention: 0, hint: None };
    let mut batch_rows = Vec::new();
    for (r, id, running, stopped) in batches {
        let (start, stop, edit) = (id.clone(), id.clone(), id);
        let element = row(this, r, &t, cx).context_menu(move |menu, _, _| {
            menu.menu_with_disabled(tr("Starten"), Box::new(StartBatch(start.clone())), stopped == 0)
                .menu_with_disabled(tr("Stoppen"), Box::new(StopBatch(stop.clone())), running == 0)
                .separator()
                .menu(tr("Batch bearbeiten"), Box::new(EditBatch(edit.clone())))
        });
        batch_rows.push(element.into_any_element());
    }
    let new_batch = tr("Neuer Batch");
    div()
        .id("sidebar")
        .w(px(shell::SIDEBAR))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(t.separator)
        .bg(t.window)
        .child(
            div()
                .id("sidebar-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(px(space::S))
                .flex()
                .flex_col()
                .gap(px(space::XXS))
                .child(row(this, all, &t, cx))
                .child(heading(tr("Batches"), &t))
                .children(batch_rows)
                .child(
                    div()
                        .id("nav-new-batch")
                        .mark("button", new_batch.clone())
                        .h(px(shell::ROW))
                        .px(px(space::S))
                        .rounded(px(radius::INNER))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(space::S))
                        .cursor_pointer()
                        .key_nav()
                        .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                        .child(icon("plus", iconsize::M, t.tertiary))
                        .child(div().text_size(px(font::BODY)).text_color(t.secondary).child(new_batch))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewBatch), cx)),
                )
                .child(div().h(px(space::M)))
                .child(row(this, accounts, &t, cx))
                .child(row(this, chat, &t, cx)),
        )
        .into_any_element()
}
