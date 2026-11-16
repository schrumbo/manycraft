use freya::prelude::*;

use super::{Facts, Tab};
use crate::view::kit::use_theme;

/// Die Screenshots der Instanz.
#[derive(PartialEq)]
pub struct Shots {
    pub facts: Facts,
}

impl Component for Shots {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        super::moving(&t, Tab::Shots)
    }
}
