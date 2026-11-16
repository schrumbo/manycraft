use freya::prelude::*;

use super::actions;
use crate::core::i18n::tr;
use crate::view::commands::Shortcut;
use crate::view::drive::use_spot;
use crate::view::kit::field::Field;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::text::{caption, line, title};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::use_theme;
use crate::view::store::use_store;
use crate::view::theme::{control, font, space};

/// The instance's name in the toolbar with what it is under it; double-click or F2 edits the name in place.
#[derive(PartialEq)]
pub struct Title {
    pub id: String,
}

impl Component for Title {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let nav = store.ui.instance;
        let made = store.model.read().instance(&self.id).cloned();
        let spot = use_spot("instance.title", "heading", made.as_ref().map_or("", |m| m.name.as_str()), true);
        let Some(made) = made else {
            return rect().child(title(&t, tr("Instanz"))).into_element();
        };
        let renaming = nav.renaming.read().as_deref() == Some(self.id.as_str());
        let error = nav.rename_error.read().clone().filter(|_| renaming);
        let below = match error {
            Some(e) => line(&t, e, font::CALLOUT, t.danger_text).max_lines(1).text_overflow(TextOverflow::Ellipsis).into_element(),
            None => caption(&t, super::subtitle(&made)).max_lines(1).text_overflow(TextOverflow::Ellipsis).into_element(),
        };
        if renaming {
            let typed = nav.rename.read().chars().count() as f32;
            let w = (typed * font::TITLE3 * control::RENAME_GLYPH + space::XL).clamp(control::RENAME_MIN, control::RENAME_MAX);
            return rect()
                .spacing(1.)
                .on_global_key_down(move |e: Event<KeyboardEventData>| {
                    if e.key == Key::Named(NamedKey::Escape) {
                        actions::end_rename(store);
                        e.stop_propagation();
                    }
                })
                .child(Field::new("instance.title.edit", nav.rename).width(Size::px(w)).auto_focus().on_submit(move |_| actions::commit_rename(store)))
                .child(below)
                .into_element();
        }
        let id = self.id.clone();
        let menu_id = self.id.clone();
        let name = rect()
            .on_sized(spot)
            .cursor(CursorIcon::Text)
            .on_mouse_down(move |e: Event<MouseEventData>| {
                if EventsCombos::<()>::pressed(e.global_location).is_double() {
                    actions::begin_rename(store, &id);
                }
            })
            .on_secondary_down(move |e: Event<PressEventData>| {
                let PressEventData::Mouse(at) = &*e else { return };
                let Some(f) = super::facts(&store.model.peek(), &menu_id) else { return };
                let rename_id = menu_id.clone();
                let mut items = vec![Item::new("instance.menu.rename", tr("Umbenennen"), move |_| actions::begin_rename(store, &rename_id)).shortcut(Shortcut::bare(Code::F2).label()), Item::Separator];
                let all = actions::all(store, &f);
                let split = all.iter().position(|a| a.key == "reset").unwrap_or(all.len());
                for (n, a) in all.into_iter().enumerate() {
                    if n == split {
                        items.push(Item::Separator);
                    }
                    let run = a.run.clone();
                    let mut item = Item::new(format!("instance.menu.{}", a.key), a.label, move |_| run.call(())).disabled(a.disabled.is_some());
                    if a.danger {
                        item = item.danger();
                    }
                    items.push(item);
                }
                menu::open(store.ui, Menu::at(items, (at.global_location.x as f32, at.global_location.y as f32)));
            })
            .child(title(&t, made.name.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        rect().spacing(1.).child(WithTip::new(Some(tr("Doppelklicken zum Umbenennen")), name)).child(below).into_element()
    }
}

/// The tabs of the instance page as a segmented control.
#[derive(PartialEq)]
pub struct Tabs;

impl Component for Tabs {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut tab = store.ui.instance.tab;
        let now = *tab.read();
        let chosen = super::Tab::ALL.iter().position(|t| *t == now).unwrap_or(0);
        let digits = [Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4, Code::Digit5];
        let items = super::Tab::ALL.iter().zip(digits).map(|(t, code)| Segment { id: format!("instance.tab.{}", t.key()), label: t.label(), tip: Some(Shortcut::primary(code).label()) }).collect();
        Segmented::new(tr("Bereich"), items, chosen).on_choose(move |i| {
            if let Some(t) = super::Tab::ALL.get(i) {
                tab.set(*t);
            }
        })
    }
}
