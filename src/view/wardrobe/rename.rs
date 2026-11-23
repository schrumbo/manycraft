use freya::prelude::*;

use super::ctl::Ctl;
use crate::core::i18n::tr;
use crate::view::kit::field::Field;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::theme::frame;

const NAME_MAX: usize = 40;

/// The sheet that renames a skin of the library.
#[derive(Clone, PartialEq)]
pub struct Rename {
    pub ctl: Ctl,
    pub file: String,
    pub name: String,
}

impl Component for Rename {
    fn render(&self) -> impl IntoElement {
        let ctl = self.ctl;
        let file = self.file.clone();
        let value = use_state(|| self.name.clone());
        let text = value.read().trim().to_owned();
        let invalid = (text.is_empty() || text.chars().count() > NAME_MAX).then(|| tr("Der Name hat 1 bis 40 Zeichen."));
        let save = {
            let file = file.clone();
            move || {
                let name = value.peek().trim().to_owned();
                if name.is_empty() || name.chars().count() > NAME_MAX {
                    return;
                }
                sheet::close(ctl.store.ui);
                ctl.rename(&file, &name);
            }
        };
        let again = save.clone();
        let field = Field::new("wardrobe.rename.name", value).label(tr("Name")).auto_focus().on_submit(move |_| again());
        let primary = Primary { label: tr("Sichern"), disabled: invalid, danger: false, busy: false, run: EventHandler::new(move |_| save()) };
        Card::new("wardrobe.rename", tr("Skin umbenennen")).width(frame::SHEET_NARROW).body(field.into_element()).primary(primary).primary_id("wardrobe.rename.save")
    }
}

pub fn open(ctl: Ctl, file: String, name: String) {
    sheet::open(ctl.store.ui, Rename { ctl, file, name });
}
