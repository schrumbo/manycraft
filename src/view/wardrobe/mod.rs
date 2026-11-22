//! The Wardrobe page: every skin and cape, to switch between and to add to.

use freya::prelude::*;

use crate::core::i18n::tr;
use crate::view::icons::{Icon, icon};
use crate::view::kit::text::{heading, secondary};
use crate::view::kit::{page, use_theme};
use crate::view::theme::{iconsize, space};

/// The Wardrobe page.
#[derive(PartialEq)]
pub struct WardrobePage;

impl Component for WardrobePage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        page::column().height(Size::fill()).center().spacing(space::M).child(icon(&t, Icon::Shirt, iconsize::HERO, t.text3)).child(heading(&t, tr("Garderobe"))).child(page::reading(secondary(&t, tr("Hier liegen bald alle Skins und Umhänge zum Wechseln."))))
    }
}
