//! The Minecraft version picker: releases by default, every kind on request, limited to what the loader supports.

use std::collections::HashSet;

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::drive::Mark;
use super::motion::Soft;
use super::theme::{Theme, control, font, radius, space, tint};
use super::widgets::KeyNav;
use crate::core::i18n::tr;
use crate::core::versions::{GameVersion, Kind, shown};

/// One row of the version picker.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionItem {
    id: String,
    kind: Kind,
}

/// What a kind of version is called, or `None` for a release.
pub fn kind_label(kind: Kind) -> Option<String> {
    match kind {
        Kind::Release => None,
        Kind::Snapshot => Some(tr("Testversion")),
        Kind::PreRelease => Some(tr("Vorabversion")),
        Kind::OldBeta => Some(tr("Alte Beta")),
        Kind::OldAlpha => Some(tr("Alte Alpha")),
    }
}

impl SearchableListItem for VersionItem {
    type Value = String;

    fn title(&self) -> SharedString {
        SharedString::from(self.id.clone())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = Theme::get(cx);
        div()
            .flex()
            .items_baseline()
            .gap(px(space::S))
            .child(div().min_w_0().truncate().child(self.id.clone()))
            .children(kind_label(self.kind).map(|label| div().flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).child(label)))
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

/// The rows to offer; `keep` stays whatever it is.
pub fn items(catalog: &[GameVersion], everything: bool, supported: Option<&HashSet<String>>, keep: &str) -> Vec<VersionItem> {
    shown(catalog, everything, supported, keep).into_iter().map(|v| VersionItem { id: v.id, kind: v.kind }).collect()
}

/// The checkbox under the picker that adds snapshots and old versions.
pub fn everything_toggle(id: &'static str, on: bool, toggle: impl Fn(&mut Window, &mut App) + Clone + 'static, t: &Theme) -> Stateful<Div> {
    let label = tr("Snapshots und alte Versionen zeigen");
    let check = toggle.clone();
    div()
        .id(id)
        .mark("checkbox", label.clone())
        .self_start()
        .h(px(control::S))
        .px(px(space::XS))
        .ml(px(-space::XS))
        .flex()
        .items_center()
        .gap(px(space::XS))
        .rounded(px(radius::INNER))
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(tint::HOVER), radius::INNER)
        .on_click(move |_, window, cx| toggle(window, cx))
        .child(Checkbox::new(SharedString::from(format!("{id}-check"))).checked(on).tab_stop(false).on_click(move |_: &bool, window, cx| {
            check(window, cx);
            cx.stop_propagation();
        }))
        .child(div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(label))
}

#[cfg(test)]
mod tests {
    use super::{GameVersion, Kind, VersionItem, items, kind_label};

    fn catalog() -> Vec<GameVersion> {
        [("26.4-snapshot-1", Kind::Snapshot), ("26.3", Kind::Release), ("b1.7.3", Kind::OldBeta)].map(|(id, kind)| GameVersion { id: id.into(), kind }).into()
    }

    #[test]
    fn rows_carry_the_kind_and_keep_the_current_version() {
        let ids = |rows: Vec<VersionItem>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(items(&catalog(), false, None, "")), ["26.3"]);
        assert_eq!(ids(items(&catalog(), false, None, "b1.7.3")), ["26.3", "b1.7.3"]);
        assert_eq!(ids(items(&catalog(), true, None, "")), ["26.4-snapshot-1", "26.3", "b1.7.3"]);
        assert!(kind_label(Kind::Release).is_none());
        assert!(kind_label(Kind::OldAlpha).is_some());
    }
}
