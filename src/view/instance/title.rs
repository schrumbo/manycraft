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

const ICON: f32 = control::L;

/// The instance's icon and name in the toolbar with what it is under it; a click or F2 edits the name in place.
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
        let field = use_hook(AccessibilityId::new_unique);
        let mut held = use_state(|| false);
        let mut hovered = use_state(|| false);
        let editing = self.id.clone();
        use_side_effect(move || {
            let focused = *Platform::get().current_window().focused_accessibility_id.read();
            if nav.renaming.read().as_deref() != Some(editing.as_str()) {
                held.set_if_modified(false);
            } else if focused == field {
                held.set_if_modified(true);
            } else if *held.peek() {
                held.set(false);
                actions::commit_rename(store);
            }
        });
        let Some(made) = made else {
            return rect().child(title(&t, tr("Instanz"))).into_element();
        };
        let picture = super::InstanceIcon { id: made.id.clone(), name: made.name.clone(), picture: made.icon_path(), side: ICON };
        let with_icon = |col: Element| rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(picture).child(col).into_element();
        let renaming = nav.renaming.read().as_deref() == Some(self.id.as_str());
        let error = nav.rename_error.read().clone().filter(|_| renaming);
        let below = match error {
            Some(e) => line(&t, e, font::CALLOUT, t.danger_text).max_lines(1).text_overflow(TextOverflow::Ellipsis).into_element(),
            None => caption(&t, super::subtitle(&made)).max_lines(1).text_overflow(TextOverflow::Ellipsis).into_element(),
        };
        if renaming {
            let typed = nav.rename.read().chars().count() as f32;
            let w = (typed * font::TITLE3 * control::RENAME_GLYPH + space::XL).clamp(control::RENAME_MIN, control::RENAME_MAX);
            return with_icon(rect()
                .spacing(1.)
                .on_global_key_down(move |e: Event<KeyboardEventData>| {
                    if e.key == Key::Named(NamedKey::Escape) {
                        actions::end_rename(store);
                        e.stop_propagation();
                    }
                })
                .child(Field::new("instance.title.edit", nav.rename).a11y(field).width(Size::px(w)).auto_focus().on_submit(move |_| actions::commit_rename(store)))
                .child(below)
                .into_element());
        }
        let id = self.id.clone();
        let menu_id = self.id.clone();
        let name = rect()
            .on_sized(spot)
            .padding((0., space::XS))
            .corner_radius(t.shape.small)
            .background(if *hovered.read() { t.hover } else { Color::TRANSPARENT })
            .cursor(CursorIcon::Text)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_press(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
                actions::begin_rename(store, &id);
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
        with_icon(rect().spacing(1.).child(WithTip::new(Some(tr("Klicken zum Umbenennen")), name)).child(below).into_element())
    }
}

/// The tabs of the instance page as a segmented control.
#[derive(PartialEq)]
pub struct Tabs;

impl Component for Tabs {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut tab = store.ui.instance.tab;
        let id = super::current(store).unwrap_or_default();
        let takes_mods = store.model.read().instance(&id).is_some_and(|i| i.loader.takes_mods());
        let now = super::shown_tab(store, &id, takes_mods);
        let tabs = super::Tab::shown(takes_mods);
        let chosen = tabs.iter().position(|t| *t == now).unwrap_or(0);
        let items = tabs.iter().zip(super::TAB_DIGITS).map(|(t, code)| Segment { id: format!("instance.tab.{}", t.key()), label: t.label(), tip: Some(Shortcut::alt(code).label()) }).collect();
        Segmented::new(tr("Bereich"), items, chosen).on_choose(move |i| {
            if let Some(t) = tabs.get(i) {
                tab.set(*t);
            }
        })
    }
}
