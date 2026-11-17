use freya::prelude::*;

use super::group_names;
use crate::app::event::Event as AppEvent;
use crate::core::i18n::{tr, trf};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::{caption, strong};
use crate::view::kit::{ring, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, frame, iconsize, space};

/// The header of a group: a chevron, its name and how many instances it holds; pressing folds the group.
#[derive(PartialEq)]
pub struct Header {
    pub key: String,
    pub title: String,
    pub count: Option<usize>,
    pub open: bool,
}

impl Component for Header {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut folded = store.ui.library.folded;
        let key = self.key.clone();
        let id = format!("library.group.{}", if self.key.is_empty() { "none" } else { self.key.trim_start_matches('\u{1}') });
        let spot = use_spot(&id, "button", &self.title, true);
        let glyph = if self.open { Icon::ChevronDown } else { Icon::ChevronRight };
        let mut head = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(self.title.clone())
            .width(Size::fill())
            .padding((space::XS, space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_press(move |_| {
                let mut set = folded.write();
                if !set.remove(&key) {
                    set.insert(key.clone());
                }
            })
            .child(icon(&t, glyph, iconsize::XS, t.text3))
            .child(strong(&t, self.title.clone(), font::BODY, t.text1));
        if let Some(border) = ring(&t, focus()) {
            head = head.border(border);
        }
        if let Some(n) = self.count {
            head = head.child(caption(&t, n.to_string()).font_tabular());
        }
        head
    }
}

/// Asks for the group instance `id` belongs to.
pub fn assign(store: Store, id: &str) {
    if store.model.peek().instance(id).is_none() {
        return;
    }
    sheet::open(store.ui, GroupSheet { id: id.to_owned() });
}

#[derive(PartialEq)]
struct GroupSheet {
    id: String,
}

impl Component for GroupSheet {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let id = self.id.clone();
        let made = store.model.read().instance(&id).cloned();
        let value = use_state(|| made.as_ref().and_then(|i| i.group.clone()).unwrap_or_default());
        let Some(made) = made else { return Card::new("library.group.sheet", tr("Gruppe")).inform().primary(Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(store.ui)) }).width(frame::SHEET_NARROW).into_element() };
        let existing: Vec<String> = group_names(&store.model.read().instances);
        let typed = value.read().clone();
        let save = {
            let id = id.clone();
            move |group: Option<String>| {
                store.dispatch(AppEvent::SetGroup { instance: id.clone(), group });
                sheet::close(store.ui);
            }
        };
        let mut chips = rect().width(Size::fill()).horizontal().spacing(space::XS).content(Content::wrap_spacing(space::XS));
        for name in existing {
            let mut value = value;
            let label = name.clone();
            chips = chips.child(Button::new(format!("library.group.pick.{name}"), label).on(typed.trim() == name).on_press(move |_| value.set(name.clone())));
        }
        let body = rect()
            .width(Size::fill())
            .spacing(space::M)
            .child(Field::new("library.group.name", value).label(tr("Name der Gruppe")).placeholder(tr("Neue Gruppe")).auto_focus().hint(tr("Eine Gruppe, die es noch nicht gibt, entsteht damit.")))
            .child(chips);
        let empty = typed.trim().is_empty();
        let (on_save, on_clear) = (save.clone(), save);
        let mut card = Card::new("library.group.sheet", trf("Gruppe von „%@“", &[&made.name])).body(body).width(frame::SHEET_NARROW).primary(Primary { label: tr("Sichern"), disabled: empty.then(|| tr("Gib der Gruppe einen Namen.")), danger: false, busy: false, run: EventHandler::new({
            let value = value;
            move |_| on_save(Some(value.peek().trim().to_owned()))
        }) });
        if made.group.is_some() {
            card = card.secondary(Primary { label: tr("Aus Gruppe nehmen"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| on_clear(None)) }).keep_cancel();
        }
        card.into_element()
    }
}
