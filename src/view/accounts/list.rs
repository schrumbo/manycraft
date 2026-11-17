use freya::prelude::*;
use uuid::Uuid;

use super::work::{Check, save_proxy, test_proxy};
use super::{Line, context, face, pick, tone_color};
use crate::core::i18n::{tr, trf};
use crate::ui::forms::proxy_problem;
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::Tone;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::{self, Menu};
use crate::view::kit::text::{caption, line, para, spacer, title};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{Bounds, ring, sheet, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{Theme, control, font, frame, iconsize, space};

const ROW_H: f32 = control::L + space::L + space::XS;
const EDIT_H: f32 = control::M * 2. + font::CAPTION * 3. + space::L * 2. + space::S * 2.;
const FACE: f32 = frame::FACE * 2.;

/// The accounts as an inset list of rows with everything one account is doing.
#[derive(PartialEq)]
pub struct List {
    pub lines: Vec<Line>,
}

impl Component for List {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let shared = store.ui.accounts;
        let scroll = use_scroll_controller(ScrollConfig::default);
        let mut viewport = use_state(|| 0f32);
        let editing = *shared.editing.read();
        let cursor = *shared.cursor.read();
        let edit_at = editing.and_then(|e| self.lines.iter().position(|l| l.id == e));
        let cursor_at = cursor.and_then(|c| self.lines.iter().position(|l| l.id == c));
        use_side_effect_with_deps(&(cursor_at, edit_at), move |(at, edit)| {
            let Some(at) = at.or(*edit) else { return };
            let edit = *edit;
            let size = move |i: usize| if Some(i) == edit { ROW_H + EDIT_H } else { ROW_H };
            let mut scroll = scroll;
            let top: f32 = (0..at).map(size).sum();
            let bottom = top + size(at);
            let shown = -(scroll.peek_position().y);
            let h = *viewport.peek();
            if top < shown {
                scroll.scroll_to_y(-(top as i32));
            } else if h > 0. && bottom > shown + h {
                scroll.scroll_to_y(-((bottom - h) as i32));
            }
        });
        let n = self.lines.len();
        VirtualScrollView::new_with_data_controlled(self.lines.clone(), move |item: VirtualItem, lines: &Vec<Line>| {
            let l = &lines[item.index];
            rect()
                .key(l.id)
                .width(Size::fill())
                .height(Size::px(item.size))
                .child(Row { line: l.clone(), first: item.index == 0, last: item.index + 1 == n, editing: editing == Some(l.id), cursor: cursor == Some(l.id) })
                .into_element()
        }, scroll)
        .length(n)
        .item_size(move |i: usize| if Some(i) == edit_at { ROW_H + EDIT_H } else { ROW_H })
        .width(Size::fill())
        .height(Size::fill())
        .on_sized(move |e: Event<SizedEventData>| viewport.set_if_modified(e.area.height()))
    }
}

#[derive(PartialEq)]
struct Row {
    line: Line,
    first: bool,
    last: bool,
    editing: bool,
    cursor: bool,
}

impl Component for Row {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let mut face_at = use_state(Bounds::default);
        let l = &self.line;
        let spot = use_spot(&format!("accounts.row.{}", l.name), "row", &l.name, true);
        let picked = store.ui.accounts.picked.read().contains(&l.id);
        let id = l.id;
        let fill = if picked { t.selection } else if *hovered.read() { t.hover } else { Color::TRANSPARENT };
        let radius = t.shape.tile;
        let top = if self.first { radius } else { 0. };
        let bottom = if self.last { radius } else { 0. };
        let corners = CornerRadius::new_symmetric(top, bottom);
        let ink = if l.busy { t.text3 } else { t.text1 };
        let mut name = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(line(&t, l.name.clone(), font::BODY, ink).font_weight(FontWeight::SEMI_BOLD).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        if let Some(p) = &l.proxy {
            name = name.child(WithTip::new(Some(p.clone()), rect().padding((0., space::XS + 1.)).height(Size::px(frame::BADGE)).center().corner_radius(frame::BADGE / 2.).background(t.hover).child(line(&t, tr("Proxy"), font::CAPTION, t.text2))));
        }
        let kind = if l.microsoft { tr("Microsoft") } else { tr("Offline") };
        let status = if l.busy { tr("Wird entfernt …") } else { l.status.clone() };
        let mut second = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS).child(caption(&t, format!("{kind} ·"))).child(line(&t, status, font::CAPTION, if l.busy { t.text3 } else { tone_color(&t, l.tone) }).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        if l.tone == Tone::Quiet && !l.batches.is_empty() && !l.busy {
            second = second.child(caption(&t, format!("· {}", trf("in %@", &[&l.batches.join(", ")]))).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        }
        let mut trailing = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
        if l.sign_in && !l.busy {
            trailing = trailing.child(Button::new(format!("accounts.{}.signin", l.name), tr("Anmelden")).glyph(Icon::LogIn).disabled(crate::view::commands::want_blocked(&crate::view::commands::Want::SignInAgain(Vec::new()))).on_press(move |_| super::sign_in(store, vec![id])));
        }
        if !l.busy {
            trailing = trailing
                .child(Button::icon(format!("accounts.{}.proxy", l.name), Icon::Globe, tr("Proxy bearbeiten")).plain().tip(tr("Proxy bearbeiten (Eingabe)")).on_press(move |_| {
                    let mut editing = store.ui.accounts.editing;
                    editing.set(Some(id));
                }))
                .child(MoreButton { id: l.id, name: l.name.clone() });
        }
        let head = rect()
            .width(Size::fill())
            .height(Size::px(ROW_H))
            .padding((0., space::S, 0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .child(
                rect()
                    .on_sized(move |e: Event<SizedEventData>| face_at.set_if_modified(Bounds::of(&e)))
                    .cursor(CursorIcon::Pointer)
                    .on_press(move |e: Event<PressEventData>| {
                        e.stop_propagation();
                        menu::open(store.ui, Menu::under(super::color_items(store, vec![id]), *face_at.peek()));
                    })
                    .child(WithTip::new(Some(tr("Farbe")), face(&t, store, l, FACE))),
            )
            .child(rect().width(Size::flex(1.)).spacing(space::XXS).child(name).child(second))
            .content(Content::flex())
            .child(trailing);
        let mut r = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(format!("{}, {}", l.name, l.status))
            .width(Size::fill())
            .height(Size::fill())
            .background(t.raised)
            .corner_radius(corners)
            .on_sized(spot)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_press(move |_| {
                a11y.request_focus();
                pick(store, id);
            })
            .on_secondary_down(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e {
                    let at = (m.global_location.x as f32, m.global_location.y as f32);
                    menu::open(store.ui, Menu::at(context(store, id, at), at));
                }
            });
        if let Some(border) = ring(&t, focus()) {
            r = r.border(border);
        } else if self.cursor && !picked {
            r = r.border(Border::new().fill(t.focus.with_a(90)).width(1.).alignment(BorderAlignment::Inner));
        }
        let mut body = rect().width(Size::fill()).height(Size::fill()).background(fill).corner_radius(corners).child(head);
        if self.editing {
            let start = store.model.peek().account(id).and_then(|a| a.proxy.clone()).unwrap_or_default();
            body = body.child(rect().width(Size::fill()).padding((0., space::M, space::M, space::M + FACE + space::M)).child(ProxyEditor { ids: vec![id], start, sheet: false }));
        }
        r = r.child(body);
        let mut wrap = rect().width(Size::fill()).height(Size::fill()).child(r);
        if !self.last {
            wrap = wrap.child(rect().position(Position::new_absolute().left(space::M + FACE + space::M).bottom(0.)).width(Size::fill()).height(Size::px(1.)).background(t.separator));
        }
        wrap
    }
}

#[derive(PartialEq)]
struct MoreButton {
    id: Uuid,
    name: String,
}

impl Component for MoreButton {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut at = use_state(Bounds::default);
        let id = self.id;
        rect().on_sized(move |e: Event<SizedEventData>| at.set_if_modified(Bounds::of(&e))).child(Button::icon(format!("accounts.{}.more", self.name), Icon::More, tr("Weitere Aktionen")).plain().on_press(move |_| {
            let b = *at.peek();
            let anchor = (b.x, b.y + b.h + space::XS);
            menu::open(store.ui, Menu::at(context(store, id, anchor), anchor));
        }))
    }
}

/// Edits the proxy of one or many accounts: the address, a check that it reaches the internet, and saving it.
#[derive(PartialEq)]
pub struct ProxyEditor {
    pub ids: Vec<Uuid>,
    pub start: String,
    pub sheet: bool,
}

impl Component for ProxyEditor {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let value = use_state(|| self.start.clone());
        let mut check = use_state(|| Check::Idle);
        let mut saving = use_state(|| false);
        let mut details = use_state(|| false);
        let typed = value.read().trim().to_owned();
        use_side_effect_with_deps(&typed, move |_| check.set_if_modified(Check::Idle));
        let problem = proxy_problem(&typed);
        let in_sheet = self.sheet;
        let close = move || {
            if in_sheet {
                sheet::close(store.ui);
            } else {
                let mut editing = store.ui.accounts.editing;
                editing.set(None);
            }
        };
        let ids = self.ids.clone();
        let mixed = {
            let m = store.model.peek();
            let mut proxies = m.accounts.iter().filter(|a| self.ids.contains(&a.id)).map(|a| a.proxy.clone());
            let first = proxies.next();
            proxies.any(|p| Some(p) != first)
        };
        let unchanged = typed == self.start.trim();
        let store_proxy = move |typed: String| {
            if *saving.peek() || proxy_problem(&typed).is_some() {
                return;
            }
            saving.set(true);
            save_proxy(store, ids.clone(), typed, move |ok| {
                if let Some(mut busy) = saving.try_write() {
                    *busy = false;
                }
                if ok {
                    close();
                }
            });
        };
        let start = self.start.trim().to_owned();
        let mut clear_all = store_proxy.clone();
        let mut write = store_proxy;
        let mut save = move || {
            let typed = value.peek().trim().to_owned();
            if typed == start {
                return;
            }
            write(typed);
        };
        let mut submit = save.clone();
        let test_value = typed.clone();
        let test_off = if typed.is_empty() {
            Some(tr("Gib zuerst eine Adresse ein."))
        } else if problem.is_some() {
            problem.clone()
        } else if *check.read() == Check::Running {
            Some(tr("Wird geprüft …"))
        } else {
            None
        };
        let save_off = if *saving.read() {
            Some(tr("Wird gesichert …"))
        } else if problem.is_some() {
            problem.clone()
        } else if unchanged {
            Some(tr("Nichts geändert"))
        } else {
            None
        };
        let mut col = rect()
            .width(Size::fill())
            .spacing(space::S)
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if e.key == Key::Named(NamedKey::Escape) && (in_sheet || !store.ui.layered()) {
                    e.stop_propagation();
                    close();
                }
            })
            .child(Field::new("accounts.proxy.field", value).placeholder(tr("z. B. socks5://host:1080")).auto_focus().error(problem.filter(|_| !typed.is_empty())).hint(tr("Gilt für Anmeldung und Spiel. Leer lassen für keinen Proxy.")).on_submit(move |_| submit()));
        let check_now = check.read().clone();
        match &check_now {
            Check::Idle => {}
            Check::Running => col = col.child(status(&t, Icon::Refresh, t.text2, tr("Proxy wird geprüft …"))),
            Check::Passed => col = col.child(status(&t, Icon::Check, t.success_text, tr("Der Proxy erreicht das Internet."))),
            Check::Failed(why) => {
                let mut row = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(icon(&t, Icon::Alert, iconsize::S, t.danger_text)).child(para(&t, why.text.clone(), font::CALLOUT, t.danger_text).width(Size::flex(1.))).content(Content::flex());
                if why.detail.is_some() {
                    row = row.child(Button::new("accounts.proxy.details", if *details.read() { tr("Details ausblenden") } else { tr("Details") }).plain().on_press(move |_| details.toggle()));
                }
                col = col.child(row);
                if *details.read()
                    && let Some(d) = &why.detail
                {
                    col = col.child(rect().width(Size::fill()).padding((space::XS, space::S)).corner_radius(t.shape.small).background(t.well).child(crate::view::kit::text::mono(&t, d.clone(), font::CAPTION, t.text2)));
                }
            }
        }
        let mut buttons = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S);
        buttons = buttons
            .child(Button::new("accounts.proxy.test", tr("Testen")).glyph(Icon::Wifi).disabled(test_off).on_press(move |_| test_proxy(test_value.clone(), check, value)))
            .child(spacer())
            .content(Content::flex())
            .child(Button::new("accounts.proxy.cancel", tr("Abbrechen")).on_press(move |_| close()));
        if mixed {
            buttons = buttons.child(Button::new("accounts.proxy.clear", tr("Alle Proxys entfernen")).plain().disabled((*saving.read()).then(|| tr("Wird gesichert …"))).on_press(move |_| clear_all(String::new())));
        }
        buttons = buttons
            .child(Button::new("accounts.proxy.save", tr("Sichern")).primary().disabled(save_off).on_press(move |_| save()));
        col.child(buttons)
    }
}

fn status(t: &Theme, glyph: Icon, ink: Color, text: String) -> Element {
    rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(icon(t, glyph, iconsize::S, ink)).child(line(t, text, font::CALLOUT, ink)).into_element()
}

/// The proxy of several accounts, or of one in the table, in a sheet.
#[derive(PartialEq)]
pub struct ProxySheet {
    pub ids: Vec<Uuid>,
    pub start: String,
}

impl Component for ProxySheet {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let m = store.model.peek();
        let heading = match self.ids.as_slice() {
            [one] => trf("Proxy für „%@“", &[&m.account(*one).map(|a| a.name.clone()).unwrap_or_default()]),
            many => trf("Proxy für %lld Accounts", &[&many.len()]),
        };
        let mixed = self.ids.len() > 1 && self.start.is_empty() && {
            let proxies: std::collections::BTreeSet<Option<&str>> = m.accounts.iter().filter(|a| self.ids.contains(&a.id)).map(|a| a.proxy.as_deref()).collect();
            proxies.len() > 1
        };
        let mut card = rect()
            .a11y_role(AccessibilityRole::Dialog)
            .a11y_alt(heading.clone())
            .width(Size::px(frame::SHEET))
            .padding(space::XL)
            .spacing(space::L)
            .background(t.raised)
            .corner_radius(t.shape.sheet)
            .shadow(t.elevation.float.clone())
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .child(title(&t, heading));
        if let Some(rim) = t.elevation.rim {
            card = card.border(Border::new().fill(rim).width(1.).alignment(BorderAlignment::Outer));
        }
        if mixed {
            card = card.child(para(&t, tr("Die Accounts haben gerade verschiedene Proxys. Was du hier sicherst, gilt danach für alle."), font::BODY, t.text2));
        }
        card.child(ProxyEditor { ids: self.ids.clone(), start: self.start.clone(), sheet: true })
    }
}
