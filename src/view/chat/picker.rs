//! The list of recipients: in a pop-up over the composer, under an `@name` being typed, and as a column beside the
//! chat. Arrows move, Enter picks, Esc closes.

use freya::prelude::*;
use uuid::Uuid;

use super::target::{Choice, Target};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::text::{caption, line, spacer};
use crate::view::kit::use_theme;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, font, frame, iconsize, space};

/// The height of one entry.
pub const ROW: f32 = frame::ROW;
const PAGE: usize = 10;
const HEAD: f32 = 24.;

/// What a key does in a list of `n` entries with `hot` lit.
pub enum Step {
    Move(usize),
    Pick(usize),
    Close,
}

/// The step a key takes in a list of `n` entries.
pub fn step(key: &Key, n: usize, hot: Option<usize>) -> Option<Step> {
    if matches!(key, Key::Named(NamedKey::Escape)) {
        return Some(Step::Close);
    }
    if n == 0 {
        return None;
    }
    let at = hot.unwrap_or(0).min(n - 1);
    Some(match key {
        Key::Named(NamedKey::ArrowDown) => Step::Move(if hot.is_none() { 0 } else { (at + 1) % n }),
        Key::Named(NamedKey::ArrowUp) => Step::Move(if hot.is_none() { n - 1 } else { (at + n - 1) % n }),
        Key::Named(NamedKey::PageDown) => Step::Move((at + PAGE).min(n - 1)),
        Key::Named(NamedKey::PageUp) => Step::Move(at.saturating_sub(PAGE)),
        Key::Named(NamedKey::Home) => Step::Move(0),
        Key::Named(NamedKey::End) => Step::Move(n - 1),
        Key::Named(NamedKey::Enter | NamedKey::Tab) if hot.is_some() => Step::Pick(at),
        _ => return None,
    })
}

/// Where entry `i` starts and how tall the whole list is, with section headings when `sections`.
fn tops(choices: &[Choice], sections: bool) -> (Vec<f32>, f32) {
    let mut y = 0.;
    let mut out = Vec::with_capacity(choices.len());
    let mut last = None;
    for c in choices {
        if sections && c.heading.is_some() && last != Some(&c.heading) {
            y += HEAD;
        }
        last = Some(&c.heading);
        out.push(y);
        y += ROW;
    }
    (out, y)
}

/// The face of the account `id`: its skin on its colour, else its first letter.
pub fn face(t: &Theme, store: Store, id: Uuid, name: &str, side: f32) -> Element {
    let fill = store.model.read().accounts.iter().find(|a| a.id == id).map_or(t.control, |a| t.identity(usize::from(a.color())));
    let chip = rect().width(Size::px(side)).height(Size::px(side)).corner_radius(t.shape.small).background(fill).center().overflow(Overflow::Clip);
    match store.faces.read().get(&id).cloned() {
        Some(handle) => chip.padding(side / 12.).child(image(handle).width(Size::fill()).height(Size::fill()).corner_radius(t.shape.small / 2.)).into_element(),
        None => chip.child(label().text(name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()).font_size(side * 0.5).font_weight(FontWeight::BOLD).color(t.ink_on(fill))).into_element(),
    }
}

/// The glyph or face that stands for `target`.
pub fn mark(t: &Theme, store: Store, target: &Target, account: Option<Uuid>, label: &str, ink: Color) -> Element {
    match (target, account) {
        (Target::Instance(_), Some(id)) => face(t, store, id, label, frame::FACE),
        (Target::Batch(_), _) => icon(t, Icon::Layers, iconsize::S, ink),
        _ => icon(t, Icon::Users, iconsize::S, ink),
    }
}

/// The entries, lit at `hot`, ticked at `current`, scrolled so the lit one shows.
#[derive(Clone, PartialEq)]
pub struct List {
    pub id: &'static str,
    pub choices: Vec<Choice>,
    pub sections: bool,
    pub hot: Option<usize>,
    pub current: Option<Target>,
    /// The tallest the list grows before it scrolls; `None` fills its parent.
    pub max_h: Option<f32>,
    pub on_pick: EventHandler<Target>,
    pub on_hover: EventHandler<usize>,
}

impl List {
    /// How tall the list is with `choices`, up to `max`.
    pub fn height(choices: &[Choice], sections: bool, max: f32) -> f32 {
        tops(choices, sections).1.min(max)
    }
}

impl Component for List {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut scroll = use_scroll_controller(ScrollConfig::default);
        let (tops, total) = tops(&self.choices, self.sections);
        let view_h = self.max_h.map(|m| total.min(m));
        let hot = self.hot;
        use_side_effect_with_deps(&(hot, tops), move |(hot, tops)| {
            let Some(at) = hot.and_then(|i| tops.get(i).copied()) else { return };
            let Some((_, viewport)) = *scroll.bounds.peek() else { return };
            let now = -scroll.peek_position().y;
            if at < now {
                scroll.scroll_to_y(-(at as i32));
            } else if at + ROW > now + viewport.height {
                scroll.scroll_to_y(-((at + ROW - viewport.height).ceil() as i32));
            }
        });
        let mut items: Vec<Element> = Vec::new();
        let mut last = None;
        for (i, c) in self.choices.iter().enumerate() {
            if self.sections && last != Some(&c.heading) {
                if let Some(h) = &c.heading {
                    items.push(rect().width(Size::fill()).height(Size::px(HEAD)).main_align(Alignment::End).padding((0., space::S, space::XXS, space::S)).child(caption(&t, h.clone()).font_weight(FontWeight::MEDIUM)).into_element());
                }
                last = Some(&c.heading);
            }
            let pick = self.on_pick.clone();
            let hover = self.on_hover.clone();
            let target = c.target.clone();
            items.push(
                Entry {
                    id: format!("{}.{}", self.id, i),
                    choice: c.clone(),
                    hot: hot == Some(i),
                    current: self.current.as_ref() == Some(&c.target),
                    on_hover: EventHandler::new(move |_| hover.call(i)),
                    on_press: EventHandler::new(move |_| pick.call(target.clone())),
                }
                .into_element(),
            );
        }
        let height = view_h.map_or(Size::fill(), Size::px);
        ScrollView::new_controlled(scroll).width(Size::fill()).height(height).children(items)
    }
}

#[derive(Clone, PartialEq)]
struct Entry {
    id: String,
    choice: Choice,
    hot: bool,
    current: bool,
    on_hover: EventHandler<()>,
    on_press: EventHandler<()>,
}

impl Component for Entry {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let c = &self.choice;
        let summary = if c.detail.is_empty() { c.label.clone() } else { format!("{} · {}", c.label, c.detail) };
        let spot = use_spot(&self.id, "option", &summary, true);
        let off = c.reached == 0;
        let (ink, soft) = if self.hot { (t.on_accent, t.on_accent) } else if off { (t.text3, t.text3) } else { (t.text1, t.text3) };
        let hover = self.on_hover.clone();
        let press = self.on_press.clone();
        let mut mark = rect().width(Size::px(frame::FACE)).height(Size::px(frame::FACE)).center().child(mark(&t, store, &c.target, c.account, &c.label, soft));
        if off && !self.hot {
            mark = mark.opacity(0.55);
        }
        let mut row = rect()
            .a11y_role(AccessibilityRole::ListBoxOption)
            .a11y_alt(summary)
            .on_sized(spot)
            .width(Size::fill())
            .height(Size::px(ROW))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.small)
            .cursor(CursorIcon::Pointer)
            .on_pointer_enter(move |_| hover.call(()))
            .on_press(move |_| press.call(()))
            .child(mark)
            .child(line(&t, c.label.clone(), font::BODY, ink).font_weight(if c.target == Target::All { FontWeight::MEDIUM } else { FontWeight::NORMAL }));
        if !c.detail.is_empty() {
            row = row.child(line(&t, c.detail.clone(), font::CALLOUT, soft));
        }
        row = row.child(spacer());
        if let Some(s) = &c.state {
            row = row.child(line(&t, s.clone(), font::CAPTION, soft));
        }
        if self.current {
            row = row.child(icon(&t, Icon::Check, iconsize::S, if self.hot { t.on_accent } else { t.accent_text }));
        }
        if self.hot {
            row = row.background(t.accent);
        }
        row.content(Content::flex())
    }
}
