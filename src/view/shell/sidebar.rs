use freya::prelude::*;

use crate::view::icons::Icon;
use crate::view::kit::badge::{Tone, badge};
use crate::view::kit::row::Row;
use crate::view::kit::text::{hairline, line, spacer};
use crate::view::kit::use_theme;
use crate::view::store::use_store;
use crate::view::theme::{font, frame, space};
use crate::view::ui::{Filter, Page};
use crate::core::i18n::tr;

fn section(t: &crate::view::theme::Theme, title: String) -> impl IntoElement {
    rect().padding((space::M, space::S, space::XS, space::S)).child(line(t, title, font::CAPTION, t.text3).font_weight(FontWeight::SEMI_BOLD))
}

/// The sidebar: the grid of all games and of those that need you, accounts and chat, then every batch and instance.
#[derive(PartialEq)]
pub struct Sidebar;

impl Component for Sidebar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut page = store.ui.page;
        let now = page.read().clone();
        let m = store.model.read();
        let attention = m.attention_count(None);
        let whispers: u32 = m.games.values().map(|g| g.unread_whispers).sum();
        let go = move |to: Page| move |_| page.set(to.clone());

        let mut top = rect().width(Size::fill()).spacing(1.);
        top = top.child(Row::new("nav.all", tr("Alle")).glyph(Icon::Grid).height(frame::ROW).selected(now == Page::Grid(Filter::All)).trailing(badge(&t, m.games.len().to_string(), Tone::Quiet)).on_press(go(Page::Grid(Filter::All))));
        let mut need = Row::new("nav.attention", tr("Brauchen dich")).glyph(Icon::Alert).height(frame::ROW).selected(now == Page::Grid(Filter::Attention)).on_press(go(Page::Grid(Filter::Attention)));
        if attention > 0 {
            need = need.trailing(badge(&t, attention.to_string(), Tone::Attention));
        }
        top = top.child(need);
        top = top.child(Row::new("nav.accounts", tr("Accounts")).glyph(Icon::Users).height(frame::ROW).selected(now == Page::Accounts).on_press(go(Page::Accounts)));
        let mut chat = Row::new("nav.chat", tr("Chat")).glyph(Icon::Message).height(frame::ROW).selected(now == Page::Chat).on_press(go(Page::Chat));
        if whispers > 0 {
            chat = chat.trailing(badge(&t, whispers.to_string(), Tone::Accent));
        }
        top = top.child(chat);

        let mut list = ScrollView::new().width(Size::fill()).height(Size::flex(1.)).spacing(1.).drag_scrolling(false);
        if !m.batches.is_empty() {
            list = list.child(section(&t, tr("Batches")));
            for b in &m.batches {
                let n = m.attention_count(Some(&b.id));
                let to = Page::Grid(Filter::Batch(b.id.clone()));
                let mut row = Row::new(format!("nav.batch.{}", b.id), b.name.clone()).glyph(Icon::Layers).height(frame::ROW).selected(now == to).on_press(go(to));
                if n > 0 {
                    row = row.trailing(badge(&t, n.to_string(), Tone::Attention));
                }
                list = list.child(row);
            }
        }
        if !m.instances.is_empty() {
            list = list.child(section(&t, tr("Instanzen")));
            for i in &m.instances {
                let to = Page::Instance(i.id.clone());
                list = list.child(Row::new(format!("nav.instance.{}", i.id), i.name.clone()).glyph(Icon::Folder).height(frame::ROW).selected(now == to).on_press(go(to)));
            }
        }

        rect()
            .a11y_role(AccessibilityRole::Navigation)
            .width(Size::px(frame::SIDEBAR))
            .height(Size::fill())
            .background(t.sidebar)
            .padding((space::M, space::S))
            .spacing(space::XS)
            .child(top)
            .child(list)
            .content(Content::flex())
            .child(hairline(&t))
            .child(Row::new("nav.settings", tr("Einstellungen")).glyph(Icon::Settings).height(frame::ROW).selected(now == Page::Settings).on_press(go(Page::Settings)))
            .child(spacer())
    }
}
