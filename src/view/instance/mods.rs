use freya::prelude::*;

use super::{Facts, Tab};
use crate::view::kit::use_theme;

/// Die Mods-Liste mit Updates und Fehlersuche.
#[derive(PartialEq)]
pub struct Mods {
    pub facts: Facts,
}

impl Component for Mods {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        super::moving(&t, Tab::Mods)
    }
}
