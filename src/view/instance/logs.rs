use freya::prelude::*;

use super::{Facts, Tab};
use crate::view::kit::use_theme;

/// Die Protokolle der Instanz.
#[derive(PartialEq)]
pub struct Logs {
    pub facts: Facts,
}

impl Component for Logs {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        super::moving(&t, Tab::Logs)
    }
}
