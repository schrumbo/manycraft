use freya::prelude::*;

use crate::core::i18n::tr;
use crate::view::commands::{self, Command};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::field::Field;
use crate::view::kit::text::{line, spacer};
use crate::view::kit::use_theme;
use crate::view::store::{Store, use_store};
use crate::view::theme::{font, frame, iconsize, space};
use crate::view::ui::{Filter, Page};

#[derive(Clone, PartialEq)]
enum Target {
    Command(Command),
    Page(Page),
    Play(String),
    Extra(EventHandler<()>),
}

#[derive(Clone, PartialEq)]
struct Entry {
    id: String,
    label: String,
    glyph: Icon,
    shortcut: Option<String>,
    target: Target,
}

fn entries(store: Store, query: &str) -> Vec<Entry> {
    let q = query.trim().to_lowercase();
    let mut list: Vec<Entry> = Command::ALL
        .into_iter()
        .filter(|c| *c != Command::OpenPalette && c.available(store))
        .map(|c| Entry { id: format!("palette.{}", c.name()), label: c.label(), glyph: c.glyph(), shortcut: c.shortcut().map(|s| s.label()), target: Target::Command(c) })
        .collect();
    list.extend(commands::extras(store).into_iter().map(|x| Entry { id: format!("palette.{}", x.id), label: x.label, glyph: x.glyph, shortcut: x.shortcut.map(|s| s.label()), target: Target::Extra(x.run) }));
    let m = store.model.peek();
    list.extend(m.batches.iter().map(|b| Entry { id: format!("palette.batch.{}", b.id), label: b.name.clone(), glyph: Icon::Layers, shortcut: None, target: Target::Page(Page::Grid(Filter::Batch(b.id.clone()))) }));
    list.extend(m.instances.iter().map(|i| Entry { id: format!("palette.instance.{}", i.id), label: i.name.clone(), glyph: Icon::Folder, shortcut: None, target: Target::Page(Page::Instance(i.id.clone())) }));
    if q.is_empty() {
        let (bulk, rest): (Vec<Entry>, Vec<Entry>) = list.into_iter().partition(|e| matches!(e.target, Target::Command(Command::StartAll | Command::StopAll | Command::CancelStart | Command::StartAnyway | Command::ReconnectDropped | Command::StartSelected | Command::StopSelected | Command::RestartSelected | Command::ReconnectSelected | Command::MessageSelected)));
        return rest.into_iter().chain(bulk).collect();
    }
    let mut games: Vec<Entry> = m.games.iter().map(|(key, g)| Entry { id: format!("palette.play.{key}"), label: g.name.clone(), glyph: Icon::Play, shortcut: None, target: Target::Play(key.clone()) }).collect();
    games.sort_by(|a, b| a.label.cmp(&b.label));
    list.extend(games);
    let mut scored: Vec<(usize, Entry)> = list.into_iter().filter_map(|e| { let l = e.label.to_lowercase(); l.find(&q).map(|at| (usize::from(at != 0) * 100 + at, e)) }).collect();
    scored.sort_by_key(|(s, _)| *s);
    scored.into_iter().map(|(_, e)| e).collect()
}

fn run(store: Store, target: &Target) {
    let mut ui = store.ui;
    ui.palette.set(false);
    match target {
        Target::Command(c) => c.run(store),
        Target::Page(p) => ui.page.set(p.clone()),
        Target::Play(key) => store.dispatch(crate::app::event::Event::Play(key.clone())),
        Target::Extra(run) => run.call(()),
    }
}

/// The command palette: type to find a command, a batch, an instance or an account to play; arrows pick, Enter runs, Esc closes.
#[derive(PartialEq)]
pub struct Palette;

impl Component for Palette {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut ui = store.ui;
        let query = use_state(String::new);
        let mut hot = use_state(|| 0usize);
        let open = *ui.palette.read();
        crate::view::kit::use_focus_return(open);
        let mut q = query;
        use_side_effect_with_deps(&open, move |open| {
            if !*open {
                q.set_if_modified(String::new());
                hot.set_if_modified(0);
            }
        });
        if !open {
            return rect().into_element();
        }
        let list = entries(store, &query.read());
        let count = list.len();
        let chosen = (*hot.read()).min(count.saturating_sub(1));
        let first = chosen.saturating_sub(frame::PALETTE_ROWS - 1);
        let keys_list = list.clone();
        let (w, h) = *ui.window.read();
        let rows = list.iter().enumerate().skip(first).take(frame::PALETTE_ROWS).map(|(i, e)| {
            let target = e.target.clone();
            PaletteRow { entry: e.clone(), hot: i == chosen, on_hover: EventHandler::new(move |_| hot.set(i)), on_press: EventHandler::new(move |_| run(store, &target)) }.into_element()
        });
        let mut body = rect().width(Size::fill()).padding(space::XS).spacing(1.);
        body = if count == 0 { body.padding(space::L).center().child(line(&t, tr("Nichts gefunden"), font::BODY, t.text3)) } else { body.children(rows) };
        rect()
            .position(Position::new_global().left(0.).top(0.))
            .layer(Layer::Overlay)
            .width(Size::px(w))
            .height(Size::px(h))
            .background(t.scrim.with_a(t.scrim.a() / 2))
            .cross_align(Alignment::Center)
            .padding((frame::SHEET_TOP, space::L, space::L, space::L))
            .on_mouse_down(move |_| ui.palette.set(false))
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                match &e.key {
                    Key::Named(NamedKey::Escape) => ui.palette.set(false),
                    Key::Named(NamedKey::ArrowDown) => hot.set((chosen + 1).min(count.saturating_sub(1))),
                    Key::Named(NamedKey::ArrowUp) => hot.set(chosen.saturating_sub(1)),
                    Key::Named(NamedKey::Tab) if e.modifiers.contains(Modifiers::SHIFT) => hot.set(chosen.saturating_sub(1)),
                    Key::Named(NamedKey::Tab) => hot.set((chosen + 1).min(count.saturating_sub(1))),
                    Key::Named(NamedKey::Enter) => {
                        e.prevent_default();
                        if let Some(entry) = keys_list.get(chosen) {
                            run(store, &entry.target);
                        }
                    }
                    _ => return,
                }
                e.stop_propagation();
            })
            .child(
                rect()
                    .a11y_role(AccessibilityRole::Dialog)
                    .a11y_alt(tr("Befehle"))
                    .a11y_builder(|n| n.set_modal())
                    .width(Size::px(frame::PALETTE.min(w - space::L * 2.)))
                    .background(t.raised)
                    .corner_radius(t.shape.sheet)
                    .shadow(t.elevation.float.clone())
                    .overflow(Overflow::Clip)
                    .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
                    .child(rect().width(Size::fill()).padding(space::S).child(Field::new("palette.query", query).placeholder(tr("Befehl, Account oder Batch suchen")).glyph(Icon::Search).width(Size::fill()).auto_focus()))
                    .child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator))
                    .child(body),
            )
            .into_element()
    }
}

#[derive(Clone, PartialEq)]
struct PaletteRow {
    entry: Entry,
    hot: bool,
    on_hover: EventHandler<()>,
    on_press: EventHandler<()>,
}

impl Component for PaletteRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let spot = use_spot(&self.entry.id, "option", &self.entry.label, true);
        let (ink, sub) = if self.hot { (t.on_accent, t.on_accent) } else { (t.text1, t.text3) };
        let hover = self.on_hover.clone();
        let press = self.on_press.clone();
        let mut r = rect()
            .a11y_role(AccessibilityRole::ListBoxOption)
            .a11y_alt(self.entry.label.clone())
            .width(Size::fill())
            .height(Size::px(frame::PALETTE_ROW))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .on_sized(spot)
            .on_pointer_enter(move |_| hover.call(()))
            .on_press(move |_| press.call(()))
            .child(icon(&t, self.entry.glyph, iconsize::M, sub))
            .child(line(&t, self.entry.label.clone(), font::BODY, ink))
            .child(spacer())
            .content(Content::flex());
        if let Some(k) = &self.entry.shortcut {
            r = r.child(line(&t, k.clone(), font::CALLOUT, sub));
        }
        if self.hot {
            r = r.background(t.accent);
        }
        r
    }
}
