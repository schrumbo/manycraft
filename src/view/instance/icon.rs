//! The picture of an instance: the one the user or its pack gave it, else its initial on a colour of its own.

use std::path::PathBuf;

use freya::prelude::*;

use super::mods::row::{cached, load, remember};
use crate::view::kit::use_theme;

fn initial(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default()
}

fn hue(id: &str) -> usize {
    id.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(usize::from(b)))
}

/// The icon of instance `id` named `name`, `side` points square; `picture` is its image file, if it has one.
#[derive(PartialEq)]
pub struct InstanceIcon {
    pub id: String,
    pub name: String,
    pub picture: Option<PathBuf>,
    pub side: f32,
}

impl Component for InstanceIcon {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut shown = use_state(|| self.picture.as_deref().and_then(cached).flatten());
        use_side_effect_with_deps(&self.picture, move |path| {
            let Some(path) = path.clone() else {
                shown.set_if_modified(None);
                return;
            };
            match cached(&path) {
                Some(face) => shown.set(face),
                None => {
                    shown.set(None);
                    spawn(async move {
                        let face = load(path.clone()).await;
                        remember(path, face.clone());
                        shown.set(face);
                    });
                }
            }
        });
        let side = self.side;
        let radius = (side * 0.22).round();
        let slot = rect().width(Size::px(side)).height(Size::px(side)).corner_radius(radius).overflow(Overflow::Clip).center();
        match shown.read().clone().filter(|_| self.picture.is_some()) {
            Some(face) => slot.child(image(face).width(Size::px(side)).height(Size::px(side)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center)),
            None => {
                let fill = t.identity(hue(&self.id));
                slot.background(fill).child(label().text(initial(&self.name)).font_size(side * 0.5).font_weight(FontWeight::BOLD).color(t.ink_on(fill)))
            }
        }
    }
}
