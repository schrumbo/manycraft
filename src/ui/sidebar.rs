//! The sidebar: all instances, one row per batch with what needs attention, accounts and chat.

use gpui_kit::component::menu::{ContextMenuExt, PopupMenu};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{CancelImport, DismissNews, EditBatch, ExportBatch, ImportBatch, NewBatch, OpenBatchFolder, OpenSettings, RestartToUpdate, StartBatch, StopBatch};
use super::drive::Mark;
use super::main_window::{MainWindow, Page};
use super::model::{self, UpdateStatus, Updates};
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
    unread: u32,
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
        .bg(if selected { t.highlight.opacity(tint::SELECTED) } else { t.window })
        .cursor_pointer()
        .key_nav_inside()
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
        .when(r.unread > 0, |d| {
            d.child(
                div()
                    .h(px(shell::BADGE))
                    .min_w(px(shell::BADGE))
                    .px(px(space::XS))
                    .rounded(px(radius::FULL))
                    .bg(t.accent)
                    .text_color(t.on_accent)
                    .text_size(px(font::CAPTION2))
                    .font_weight(FontWeight::BOLD)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(r.unread.to_string()),
            )
        })
        .when_some(r.trailing, |d, text| d.child(div().flex_none().text_size(px(font::CAPTION)).text_color(if selected { t.accent_text } else { t.tertiary }).child(text)))
        .when_some(r.hint, |d, hint| d.tooltip(tip(hint)))
        .on_click(cx.listener(move |this, _, window, cx| this.go(page.clone(), window, cx)))
}

fn news_line(updates: &Updates, t: &Theme) -> Option<Stateful<Div>> {
    let (version, notes) = updates.news.clone()?;
    let text = if notes.is_empty() { trf("Neu in %@", &[&version]) } else { trf("Neu in %@: %@", &[&version, &notes]) };
    let close = tr("Ausblenden");
    Some(
        div()
            .id("nav-news")
            .mark("note", text.clone())
            .min_h(px(shell::ROW))
            .pl(px(space::S))
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(icon("sparkles", iconsize::S, t.tertiary))
            .child(div().id("nav-news-text").flex_1().min_w_0().truncate().text_size(px(font::CAPTION)).text_color(t.secondary).child(text.clone()).tooltip(tip(text)))
            .child(
                div()
                    .id("nav-news-close")
                    .mark("button", close.clone())
                    .size(px(shell::PILL))
                    .flex_none()
                    .rounded(px(radius::INNER))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .key_nav()
                    .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                    .child(icon("x", iconsize::S, t.tertiary))
                    .tooltip(tip(close))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(DismissNews), cx)),
            ),
    )
}

fn update_row(updates: &Updates, launching: bool, t: &Theme) -> Option<Stateful<Div>> {
    let base = |label: String, icon_name: &str| {
        div()
            .id("nav-update")
            .mark("button", label.clone())
            .h(px(shell::ROW))
            .px(px(space::S))
            .rounded(px(radius::INNER))
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(icon(icon_name, iconsize::M, t.accent_text))
            .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).text_color(t.accent_text).child(label))
    };
    let trailing = |text: String| div().flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).child(text);
    match updates.status.clone() {
        UpdateStatus::Ready { version } => {
            let label = tr("Neu starten");
            let pill = div()
                .id("nav-update-restart")
                .mark("button", label.clone())
                .h(px(shell::PILL))
                .px(px(space::S))
                .flex_none()
                .rounded(px(radius::FULL))
                .bg(t.accent.opacity(tint::SELECTED))
                .flex()
                .items_center()
                .text_size(px(font::CAPTION))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.accent_text)
                .child(label);
            let row = base(tr("Update bereit"), "download");
            if launching {
                return Some(row.opacity(tint::DISABLED).cursor_not_allowed().tooltip(tip(tr("Geht, sobald keine Instanz mehr startet."))).child(pill));
            }
            Some(
                row.cursor_pointer()
                    .key_nav()
                    .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                    .tooltip(tip(trf("Version %@ ist geladen und wird beim Beenden installiert.", &[&version])))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(RestartToUpdate), cx))
                    .child(pill),
            )
        }
        UpdateStatus::Available { version, page, installs: false } if page.starts_with("https://github.com/") => Some(
            base(tr("Update verfügbar"), "download")
                .cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                .child(trailing(version.clone()))
                .tooltip(tip(trf("Version %@ auf GitHub ansehen", &[&version])))
                .on_click(move |_, _, cx| cx.open_url(&page)),
        ),
        UpdateStatus::Available { version, installs: true, .. } => Some(
            base(tr("Update verfügbar"), "download")
                .cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                .child(trailing(version.clone()))
                .tooltip(tip(trf("Version %@ laden", &[&version])))
                .on_click(|_, _, cx| super::updates::fetch(cx)),
        ),
        UpdateStatus::Checking if updates.asked => Some(base(tr("Sucht …"), "refresh-cw")),
        UpdateStatus::Downloading { version, percent } if updates.asked => {
            let done = percent.map_or_else(|| version.clone(), |p| format!("{p} %"));
            Some(base(tr("Update wird geladen"), "download").child(trailing(done)).tooltip(tip(trf("Version %@ wird geladen.", &[&version]))))
        }
        _ => None,
    }
}

/// The menu of a batch, in the sidebar and over its tiles.
pub fn batch_menu(menu: PopupMenu, id: &str, running: usize, stopped: usize, exporting: bool) -> PopupMenu {
    menu.menu_with_disabled(tr("Starten"), Box::new(StartBatch(id.to_owned())), stopped == 0)
        .menu_with_disabled(tr("Stoppen"), Box::new(StopBatch(id.to_owned())), running == 0)
        .separator()
        .menu(tr("Batch bearbeiten"), Box::new(EditBatch(id.to_owned())))
        .menu(tr("Batch-Ordner öffnen"), Box::new(OpenBatchFolder(id.to_owned())))
        .when(!exporting, |menu| menu.menu(tr("Als .mrpack exportieren…"), Box::new(ExportBatch(id.to_owned()))))
        .separator()
        .menu(tr("Batch importieren…"), Box::new(ImportBatch))
}

fn import_row(import: &model::Import, t: &Theme) -> Stateful<Div> {
    let stopping = import.cancel.load(std::sync::atomic::Ordering::SeqCst);
    let state = if stopping {
        tr("Bricht ab …")
    } else {
        import.percent.map_or_else(|| tr("Prüft …"), |p| format!("{p} %"))
    };
    let cancel = tr("Import abbrechen");
    let id = import.id;
    div()
        .id(SharedString::from(format!("nav-import-{id}")))
        .mark("row", import.name.clone())
        .h(px(shell::ROW))
        .px(px(space::S))
        .rounded(px(radius::INNER))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::S))
        .child(icon("download", iconsize::M, t.tertiary))
        .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).text_color(t.secondary).child(import.name.clone()))
        .child(div().flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).child(state))
        .tooltip(tip(trf("„%@“ wird als Batch importiert.", &[&import.name])))
        .when(!stopping, |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("nav-import-cancel-{id}")))
                    .mark("button", cancel.clone())
                    .size(px(shell::PILL))
                    .flex_none()
                    .rounded(px(radius::INNER))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .key_nav()
                    .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                    .child(icon("x", iconsize::S, t.tertiary))
                    .tooltip(tip(cancel))
                    .on_click(move |_, window, cx| window.dispatch_action(Box::new(CancelImport(id)), cx)),
            )
        })
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
        unread: 0,
        hint: None,
    };
    let batches: Vec<(Row, String, usize, usize, bool)> = m
        .batches
        .iter()
        .map(|b| {
            let total = m.instances.values().filter(|i| i.batch_id == b.id).count();
            let running = m.instances.values().filter(|i| i.batch_id == b.id && i.phase.is_active()).count();
            let attention = m.attention_count(Some(&b.id));
            let fine = m.instances.values().filter(|i| i.batch_id == b.id && i.phase.is_active() && !i.phase.needs_attention()).count();
            let hint = match (total, running) {
                (0, _) => tr("Noch keiner da."),
                (1, 0) if attention == 0 => tr("Schläft noch."),
                (_, 0) if attention == 0 => trf("Alle %lld schlummern.", &[&total]),
                _ => trf("%lld von %lld laufen", &[&fine, &total]),
            };
            let stopped = total - running;
            let exporting = m.exporting.contains(&b.id);
            let trailing = if exporting { tr("Exportiert …") } else { format!("{fine}/{total}") };
            (Row { id: format!("nav-batch-{}", b.id), page: Page::Grid(Some(b.id.clone())), icon: "boxes", label: b.name.clone(), trailing: Some(trailing), attention, unread: 0, hint: Some(hint) }, b.id.clone(), running, stopped, exporting)
        })
        .collect();
    let accounts = Row { id: "nav-accounts".into(), page: Page::Accounts, icon: "users", label: tr("Accounts"), trailing: Some(m.accounts.len().to_string()), attention: 0, unread: 0, hint: None };
    let updates = m.updates.clone();
    let chat = Row { id: "nav-chat".into(), page: Page::Chat, icon: "messages-square", label: tr("Chat"), trailing: None, attention: 0, unread: m.unread_whispers_total(), hint: None };
    let imports: Vec<AnyElement> = m.imports.iter().map(|import| {
        let id = import.id;
        import_row(import, &t).context_menu(move |menu, _, _| menu.menu(tr("Import abbrechen"), Box::new(CancelImport(id)))).into_any_element()
    }).collect();
    let mut batch_rows = Vec::new();
    for (r, id, running, stopped, exporting) in batches {
        let element = row(this, r, &t, cx).context_menu(move |menu, _, _| batch_menu(menu, &id, running, stopped, exporting));
        batch_rows.push(element.into_any_element());
    }
    batch_rows.extend(imports);
    let new_batch = tr("Neuer Batch");
    let settings = tr("Einstellungen");
    let shortcut = super::palette::shortcut(&OpenSettings, cx).unwrap_or_default();
    let launching = super::updates::counts(cx).0 > 0;
    let news = news_line(&updates, &t);
    let update = update_row(&updates, launching, &t);
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
        .child(
            div().flex_none().px(px(space::S)).pb(px(space::S)).children(news).children(update).child(
                div()
                    .id("nav-settings")
                    .mark("button", settings.clone())
                    .h(px(shell::ROW))
                    .px(px(space::S))
                    .rounded(px(radius::INNER))
                    .flex()
                    .items_center()
                    .gap(px(space::S))
                    .cursor_pointer()
                    .key_nav()
                    .soft(t.ink.opacity(tint::HOVER), radius::INNER)
                    .child(icon("settings", iconsize::M, t.secondary))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(font::BODY)).text_color(t.primary).child(settings))
                    .tooltip(tip(shortcut))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            ),
        )
        .into_any_element()
}
