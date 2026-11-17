//! The page of all games grouped by batch: one section per batch with a sticky header that folds it, "Farm · 15 von 17
//! laufen · 2 brauchen dich", the games that need you first within each.

use std::collections::HashSet;

use freya::prelude::*;

use crate::app::model::Model;
use crate::core::i18n::tr;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::text::line;
use crate::view::kit::{ring, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{font, iconsize, space};
use crate::view::ui::{Filter, View};

/// The height of a group's header.
pub const HEADER: f32 = 32.;

/// A batch's games on the grouped page, in the page's order.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub keys: Vec<String>,
    pub folded: bool,
}

/// The groups the page shows `keys` in, batches by name; None when it shows them flat: on a batch's page, in the table,
/// with fewer than two batches or when the user chose the flat order.
pub fn of(m: &Model, filter: &Filter, view: View, grouped: bool, folded: &HashSet<String>, keys: &[String]) -> Option<Vec<Group>> {
    if *filter != Filter::All || view != View::Tiles || !grouped || m.batches.len() < 2 {
        return None;
    }
    let mut batches: Vec<_> = m.batches.iter().collect();
    batches.sort_by_key(|b| (b.name.to_lowercase(), b.id.clone()));
    let groups = batches
        .into_iter()
        .map(|b| Group { id: b.id.clone(), name: b.name.clone(), keys: keys.iter().filter(|k| m.games.get(*k).is_some_and(|g| g.batch_id == b.id)).cloned().collect(), folded: folded.contains(&b.id) })
        .filter(|g| !g.keys.is_empty())
        .collect();
    Some(groups)
}

/// The keys of the open groups, in order: what the page lays out and the keyboard walks.
pub fn open_keys(groups: &[Group]) -> Vec<String> {
    groups.iter().filter(|g| !g.folded).flat_map(|g| g.keys.iter().cloned()).collect()
}

/// A group's header: the disclosure chevron, the batch and its count; folds and unfolds the group on press.
#[derive(PartialEq)]
pub struct Header {
    pub id: String,
    pub name: String,
    pub line: Option<String>,
    pub folded: bool,
    pub width: f32,
    /// How far the header's content starts in, so a stuck header spanning the page lines up with the tiles.
    pub inset: f32,
    pub stuck: bool,
}

impl Component for Header {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let spot = use_spot(&format!("grid.group.{}", self.id), "button", &self.name, true);
        let id = self.id.clone();
        let mut toggle = move || {
            let mut folded = store.ui.grid_view.folded;
            let mut set = folded.peek().clone();
            if !set.remove(&id) {
                set.insert(id.clone());
            }
            folded.set(set);
        };
        let state = if self.folded { tr("eingeklappt") } else { tr("ausgeklappt") };
        let label = match &self.line {
            Some(l) => format!("{} · {l}", self.name),
            None => self.name.clone(),
        };
        let mut r = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(format!("{label}, {state}"))
            .width(Size::px(self.width))
            .height(Size::px(HEADER))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .padding((0., space::XS, 0., space::XS + self.inset))
            .corner_radius(if self.stuck { 0. } else { t.shape.small + 2. })
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_all_press(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
                a11y.request_focus();
                toggle();
            });
        r = r.background(if *hovered.read() { t.hover } else if self.stuck { t.surface } else { Color::TRANSPARENT });
        if let Some(border) = ring(&t, focus()) {
            r = r.border(border);
        }
        let glyph = if self.folded { Icon::ChevronRight } else { Icon::ChevronDown };
        r = r
            .child(icon(&t, glyph, iconsize::XS, t.text2))
            .child(line(&t, self.name.clone(), font::CALLOUT, t.text1).font_weight(FontWeight::SEMI_BOLD).max_lines(1));
        if let Some(l) = &self.line {
            r = r.child(line(&t, format!("· {l}"), font::CALLOUT, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.))).content(Content::flex());
        }
        r
    }
}
