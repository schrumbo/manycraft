use freya::prelude::*;

use super::{Facts, Tab};
use crate::view::kit::use_theme;

/// Die Welten der Instanz und ihre Sicherungen.
#[derive(PartialEq)]
pub struct Worlds {
    pub facts: Facts,
}

impl Component for Worlds {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        super::moving(&t, Tab::Worlds)
    }
}
