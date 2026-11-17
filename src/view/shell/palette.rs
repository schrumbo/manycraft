use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::app::model::Game;
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::view::grid::words;
use crate::view::commands::{self, Command, Want, can_want, want};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::menu::Item;
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::use_theme;
use crate::view::store::{Store, use_store};
use crate::view::theme::{control, font, frame, level, iconsize, space};
use crate::view::ui::{Filter, Page};

#[derive(Clone, PartialEq)]
enum Target {
    Command(Command),
    Page(Page),
    Play(String),
    Extra(EventHandler<()>),
}

#[derive(Clone, PartialEq)]
enum Drill {
    Instance(String),
    Batch(String),
    Game(String),
}

#[derive(Clone, PartialEq)]
struct Entry {
    id: String,
    label: String,
    glyph: Icon,
    shortcut: Option<String>,
    target: Target,
    actions: Option<Drill>,
    off: Option<String>,
    sub: Option<String>,
}

impl Entry {
    fn new(id: String, label: String, glyph: Icon, shortcut: Option<String>, target: Target) -> Self {
        Self { id, label, glyph, shortcut, target, actions: None, off: None, sub: None }
    }

    fn sub(mut self, sub: String) -> Self {
        self.sub = Some(sub);
        self
    }

    fn drills(mut self, drill: Drill) -> Self {
        self.actions = Some(drill);
        self
    }
}

const RECENTS: usize = 5;

const BULK: [Command; 10] = [Command::StartAll, Command::StopAll, Command::CancelStart, Command::StartAnyway, Command::ReconnectDropped, Command::StartSelected, Command::StopSelected, Command::RestartSelected, Command::ReconnectSelected, Command::MessageSelected];

fn filtered(list: Vec<Entry>, q: &str) -> Vec<Entry> {
    let mut scored: Vec<(usize, Entry)> = list.into_iter().filter_map(|e| { let l = e.label.to_lowercase(); l.find(q).map(|at| (usize::from(at != 0) * 100 + at, e)) }).collect();
    scored.sort_by_key(|(s, _)| *s);
    scored.into_iter().map(|(_, e)| e).collect()
}

fn game_items(store: Store, key: &str) -> Vec<Item> {
    let m = store.model.peek();
    let Some(g) = m.games.get(key) else { return Vec::new() };
    let k = key.to_owned();
    let mut items = vec![Item::new("game.play", tr("Spielen"), move |_| store.dispatch(AppEvent::Play(k.clone())))];
    if g.phase.reconnectable() {
        let k = key.to_owned();
        items.push(Item::new("game.reconnect", tr("Neu verbinden"), move |_| store.dispatch(AppEvent::Reconnect(k.clone()))));
    }
    let k = vec![key.to_owned()];
    if g.phase.stoppable() {
        items.push(Item::new("game.stop", tr("Stoppen"), move |_| store.dispatch(AppEvent::Stop(k.clone()))));
    } else if !g.phase.is_active() {
        items.push(Item::new("game.start", tr("Starten"), move |_| store.dispatch(AppEvent::Start(k.clone()))));
    }
    let batch = g.batch_id.clone();
    let mut ui = store.ui;
    items.push(Item::new("game.batch", tr("Im Batch zeigen"), move |_| ui.page.set(Page::Grid(Filter::Batch(batch.clone())))));
    items
}

fn actions(store: Store, drill: &Drill, query: &str) -> Vec<Entry> {
    let q = query.trim().to_lowercase();
    let items = match drill {
        Drill::Instance(id) => super::sidebar::instance_items(store, id),
        Drill::Batch(id) => super::sidebar::batch_items(store, id),
        Drill::Game(key) => game_items(store, key),
    };
    let list = items
        .into_iter()
        .filter_map(|item| match item {
            Item::Action { id, label, shortcut, danger, disabled, run, .. } => {
                let glyph = if danger { Icon::Trash } else { action_glyph(&id) };
                let mut e = Entry::new(format!("palette.{id}"), label, glyph, None, Target::Extra(run));
                if disabled {
                    e.off = Some(shortcut.unwrap_or_default());
                }
                Some(e)
            }
            Item::Separator => None,
        })
        .collect();
    filtered(list, &q)
}

fn action_glyph(id: &str) -> Icon {
    match id.split_once('.').map_or(id, |(_, tail)| tail) {
        "play" | "start" | "start.anyway" => Icon::Play,
        "start.cancel" => Icon::Close,
        "reconnect" => Icon::Wifi,
        "edit" => Icon::Pencil,
        "batch" if id.starts_with("game.") => Icon::Grid,
        "stop" => Icon::Stop,
        "rename" => Icon::Pencil,
        "mods.add" => Icon::Plus,
        "mods.edit" => Icon::Puzzle,
        "loader" => Icon::Refresh,
        "clone" => Icon::Copy,
        "batch" => Icon::Layers,
        "folder" => Icon::Folder,
        "shortcut" => Icon::Link,
        "export" => Icon::Upload,
        "reset" => Icon::RotateCcw,
        _ => Icon::ChevronRight,
    }
}

fn instance_sub(i: &Instance) -> String {
    format!("{} · {} {}", tr("Instanz"), i.game_version, i.loader.name())
}

fn play_instance(store: Store, i: &Instance) -> Entry {
    let m = store.model.peek();
    let id = i.id.clone();
    let expired = crate::app::reduce::solo_pick(&m, i).filter(|a| m.sign_in_expired(*a));
    let run = EventHandler::new(move |_| match expired {
        Some(account) => crate::view::accounts::sign_in(store, vec![account]),
        None => store.dispatch(AppEvent::PlayInstance { instance: id.clone(), account: None }),
    });
    let id = format!("palette.instance.{}.play", i.id);
    match expired.and_then(|a| m.account(a)) {
        Some(account) => {
            let mut e = Entry::new(id, trf("Anmelden als %@", &[&account.name]), Icon::LogIn, None, Target::Extra(run)).sub(format!("{} · {}", i.name, tr("Anmeldung abgelaufen")));
            e.off = commands::sign_in_blocked(store);
            e
        }
        None => Entry::new(id, trf("%@ spielen", &[&i.name]), Icon::Play, None, Target::Extra(run)).sub(instance_sub(i)),
    }
}

fn entries(store: Store, query: &str, recents: &[String]) -> Vec<Entry> {
    let q = query.trim().to_lowercase();
    let mut list: Vec<Entry> = Command::ALL
        .into_iter()
        .filter(|c| *c != Command::OpenPalette && c.available(store))
        .map(|c| Entry::new(format!("palette.{}", c.name()), if c == Command::Undo { commands::undo_label(store).unwrap_or_else(|| c.label()) } else { c.label() }, c.glyph(), c.shortcut().map(|s| s.label()), Target::Command(c)))
        .collect();
    if !Command::SignInWaiting.available(store)
        && !crate::view::accounts::waiting(&store.model.peek()).is_empty()
        && let Some(why) = commands::sign_in_blocked(store)
    {
        let c = Command::SignInWaiting;
        let mut e = Entry::new(format!("palette.{}", c.name()), c.label(), c.glyph(), None, Target::Command(c));
        e.off = Some(why);
        list.push(e);
    }
    list.extend(commands::extras(store).into_iter().map(|x| Entry::new(format!("palette.{}", x.id), x.label, x.glyph, x.shortcut.map(|s| s.label()), Target::Extra(x.run))));
    let m = store.model.peek();
    for b in &m.batches {
        let games = || m.games.values().filter(|g| g.batch_id == b.id);
        let fine = games().filter(|g| g.phase.is_active() && !g.phase.waiting() && !g.phase.needs_attention()).count();
        let sub = format!("{} · {}", tr("Batch"), trf("%lld von %lld laufen", &[&fine, &games().count()]));
        list.push(Entry::new(format!("palette.batch.{}", b.id), b.name.clone(), Icon::Layers, None, Target::Page(Page::Grid(Filter::Batch(b.id.clone())))).sub(sub.clone()).drills(Drill::Batch(b.id.clone())));
        let exporting = m.exporting.contains(&b.instance_id);
        for (suffix, label, glyph, what) in [("edit", trf("%@ bearbeiten", &[&b.name]), Icon::Pencil, Want::EditBatch(b.id.clone())), ("export", trf("%@ als .mrpack exportieren…", &[&b.name]), Icon::Upload, Want::ExportBatch(b.id.clone())), ("delete", trf("%@ löschen…", &[&b.name]), Icon::Trash, Want::DeleteBatch(b.id.clone()))] {
            if can_want(&what) && !(exporting && suffix == "export") {
                list.push(
                    Entry::new(format!("palette.batch.{}.{suffix}", b.id), label, glyph, None, Target::Extra(EventHandler::new(move |_| {
                        want(store, what.clone());
                    })))
                    .sub(sub.clone()),
                );
            }
        }
    }
    list.extend(m.instances.iter().map(|i| Entry::new(format!("palette.instance.{}", i.id), i.name.clone(), Icon::Package, None, Target::Page(Page::Instance(i.id.clone()))).sub(instance_sub(i)).drills(Drill::Instance(i.id.clone()))));
    let game = |key: &String, g: &Game| {
        let batch = m.batches.iter().find(|b| b.id == g.batch_id).map_or_else(String::new, |b| format!(" · {}", b.name));
        Entry::new(format!("palette.play.{key}"), g.name.clone(), Icon::Play, None, Target::Play(key.clone())).sub(format!("{}{batch} · {}", tr("Account"), words::word(g, &m))).drills(Drill::Game(key.clone()))
    };
    if q.is_empty() {
        list.extend(m.games.iter().filter(|(key, g)| !g.alone() && recents.contains(&format!("palette.play.{key}"))).map(|(key, g)| game(key, g)));
        list.extend(m.instances.iter().filter(|i| recents.contains(&format!("palette.instance.{}.play", i.id))).map(|i| play_instance(store, i)));
        return first_view(list, recents);
    }
    list.extend(m.instances.iter().map(|i| play_instance(store, i)));
    let mut games: Vec<Entry> = m.games.iter().filter(|(_, g)| !g.alone()).map(|(key, g)| game(key, g)).collect();
    games.sort_by(|a, b| a.label.cmp(&b.label));
    list.extend(games);
    filtered(list, &q)
}

fn first_view(list: Vec<Entry>, recents: &[String]) -> Vec<Entry> {
    let (bulk, mut rest): (Vec<Entry>, Vec<Entry>) = list.into_iter().partition(|e| matches!(e.target, Target::Command(c) if BULK.contains(&c)));
    let mut head = Vec::new();
    if let Some(at) = rest.iter().position(|e| e.target == Target::Command(Command::NextAttention)) {
        head.push(rest.remove(at));
    }
    for id in recents {
        if let Some(at) = rest.iter().position(|e| &e.id == id) {
            head.push(rest.remove(at));
        }
    }
    rest.retain(|e| !matches!(e.target, Target::Play(_)));
    head.into_iter().chain(rest).chain(bulk).collect()
}

fn remember(mut recents: State<Vec<String>>, id: &str) {
    let mut list = recents.peek().clone();
    list.retain(|x| x != id);
    list.insert(0, id.to_owned());
    list.truncate(RECENTS);
    recents.set(list);
}

fn run(store: Store, entry: &Entry, recents: Option<State<Vec<String>>>) {
    if entry.off.is_some() {
        return;
    }
    if let Some(recents) = recents {
        remember(recents, &entry.id);
    }
    let target = &entry.target;
    let mut ui = store.ui;
    ui.palette.set(false);
    match target {
        Target::Command(c) => c.run(store),
        Target::Page(p) => ui.page.set(p.clone()),
        Target::Play(key) => store.dispatch(AppEvent::Play(key.clone())),
        Target::Extra(run) => run.call(()),
    }
}

/// The command palette: type to find a command, a batch, an instance or an account to play; arrows pick, Enter runs, Esc
/// closes. Tab or → on an instance, a batch or an account lists what can be done to it; ←, Shift+Tab or Esc go back.
/// With nothing typed it leads with the next game that needs you and the last five picks.
#[derive(PartialEq)]
pub struct Palette;

impl Component for Palette {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut ui = store.ui;
        let query = use_state(String::new);
        let mut hot = use_state(|| 0usize);
        let mut drill = use_state(|| None::<(Drill, String)>);
        let recents = use_state(Vec::<String>::new);
        let back = use_state(|| (String::new(), 0usize));
        let open = *ui.palette.read();
        crate::view::kit::use_focus_return(open);
        let mut q = query;
        use_side_effect_with_deps(&open, move |open| {
            if !*open {
                q.set_if_modified(String::new());
                hot.set_if_modified(0);
                drill.set_if_modified(None);
            }
        });
        if !open {
            return rect().into_element();
        }
        let inside = drill.read().clone();
        let list = match &inside {
            Some((d, _)) => actions(store, d, &query.read()),
            None => entries(store, &query.read(), &recents.read()),
        };
        let count = list.len();
        let chosen = (*hot.read()).min(count.saturating_sub(1));
        let first = chosen.saturating_sub(frame::PALETTE_ROWS - 1);
        let keys_list = list.clone();
        let (w, h) = *ui.window.read();
        let remembers = inside.is_none().then_some(recents);
        let enter = move |id: Drill, name: String| {
            let (mut q, mut hot, mut drill, mut back) = (q, hot, drill, back);
            back.set((q.peek().clone(), *hot.peek()));
            q.set(String::new());
            hot.set(0);
            drill.set(Some((id, name)));
        };
        let leave = move || {
            let (mut q, mut hot, mut drill) = (q, hot, drill);
            let (query, row) = back.peek().clone();
            q.set(query);
            hot.set(row);
            drill.set(None);
        };
        let rows = list.iter().enumerate().skip(first).take(frame::PALETTE_ROWS).map(|(i, e)| {
            let entry = e.clone();
            let opens = e.actions.clone().map(|id| {
                let name = e.label.clone();
                EventHandler::new(move |_| enter(id.clone(), name.clone()))
            });
            PaletteRow { entry: e.clone(), hot: i == chosen, on_hover: EventHandler::new(move |_| hot.set(i)), on_press: EventHandler::new(move |_| run(store, &entry, remembers)), on_actions: opens }.into_element()
        });
        let mut body = rect().width(Size::fill()).padding(space::XS).spacing(1.);
        body = if count == 0 { body.padding(space::L).center().child(line(&t, tr("Nichts gefunden"), font::BODY, t.text3)) } else { body.children(rows) };
        let placeholder = match &inside {
            Some((_, name)) => trf("Aktionen für %@", &[name]),
            None => tr("Befehl, Instanz, Account oder Batch"),
        };
        let mut head = rect().width(Size::fill()).padding(space::S).horizontal().cross_align(Alignment::Center).spacing(space::S).content(Content::flex());
        if let Some((_, name)) = &inside {
            head = head
                .child(Button::icon("palette.back", Icon::ChevronLeft, tr("Zurück")).plain().tip(format!("{} (←)", tr("Zurück"))).on_press(move |_| leave()))
                .child(line(&t, name.clone(), font::BODY, t.text1).font_weight(FontWeight::SEMI_BOLD).max_lines(1).max_width(Size::px(frame::PALETTE_CRUMB)).text_overflow(TextOverflow::Ellipsis));
        }
        let empty_query = query.read().is_empty();
        let drilled = inside.is_some();
        let into = list.get(chosen).and_then(|x| x.actions.clone().map(|id| (id, x.label.clone())));
        let claim = move |key: &Key, modifiers: Modifiers| {
            let plain = modifiers.is_empty();
            match key {
                Key::Named(NamedKey::Tab) if modifiers == Modifiers::SHIFT => {
                    if drilled {
                        leave();
                    }
                }
                Key::Named(NamedKey::Tab) if plain => {
                    if let Some((id, name)) = into.clone() {
                        enter(id, name);
                    }
                }
                Key::Named(NamedKey::ArrowRight) if plain && empty_query && into.is_some() => {
                    if let Some((id, name)) = into.clone() {
                        enter(id, name);
                    }
                }
                Key::Named(NamedKey::ArrowLeft | NamedKey::Backspace) if drilled && empty_query && plain => leave(),
                _ => return false,
            }
            true
        };
        let head = head.child(Field::new("palette.query", query).placeholder(placeholder).glyph(Icon::Search).rows_through().claim(claim).width(Size::flex(1.)).auto_focus());
        rect()
            .position(Position::new_global().left(0.).top(frame::CHROME))
            .layer(Layer::OverlayLevel(level::PALETTE))
            .width(Size::px(w))
            .height(Size::px((h - frame::CHROME).max(0.)))
            .background(t.scrim.with_a(t.scrim.a() / 2))
            .cross_align(Alignment::Center)
            .padding((frame::SHEET_TOP - frame::CHROME, space::L, space::L, space::L))
            .on_mouse_down(move |_| ui.palette.set(false))
            .on_wheel(|e: Event<WheelEventData>| e.stop_propagation())
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                let into = keys_list.get(chosen).and_then(|x| x.actions.clone().map(|id| (id, x.label.clone())));
                let shift = e.modifiers.contains(Modifiers::SHIFT);
                let drilled = drill.peek().is_some();
                match &e.key {
                    Key::Named(NamedKey::Escape) => {
                        e.prevent_default();
                        if drilled {
                            leave();
                        } else {
                            ui.palette.set(false);
                        }
                    }
                    Key::Named(NamedKey::ArrowDown) => hot.set((chosen + 1).min(count.saturating_sub(1))),
                    Key::Named(NamedKey::ArrowUp) => hot.set(chosen.saturating_sub(1)),
                    Key::Named(NamedKey::Tab) if shift => {
                        e.prevent_default();
                        if drilled {
                            leave();
                        }
                    }
                    Key::Named(NamedKey::Tab) => {
                        e.prevent_default();
                        if e.modifiers.is_empty()
                            && let Some((id, name)) = into
                        {
                            enter(id, name);
                        }
                    }
                    Key::Named(NamedKey::ArrowRight) if e.modifiers.is_empty() && empty_query && into.is_some() => {
                        e.prevent_default();
                        if let Some((id, name)) = into {
                            enter(id, name);
                        }
                    }
                    Key::Named(NamedKey::ArrowLeft | NamedKey::Backspace) if drilled && empty_query => leave(),
                    Key::Named(NamedKey::Enter) => {
                        e.prevent_default();
                        if let Some(entry) = keys_list.get(chosen) {
                            run(store, entry, remembers);
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
                    .child(head)
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
    on_actions: Option<EventHandler<()>>,
}

impl Component for PaletteRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let off = self.entry.off.is_some();
        let spot = use_spot(&self.entry.id, "option", &self.entry.label, !off);
        let actions_label = tr("Aktionen");
        let actions_spot = use_spot(&format!("{}.actions", self.entry.id), "button", &actions_label, self.on_actions.is_some());
        let (ink, sub) = match (self.hot, off) {
            (_, true) => (t.text3, t.text3),
            (true, false) => (t.on_accent, t.on_accent),
            (false, false) => (t.text1, t.text3),
        };
        let hover = self.on_hover.clone();
        let press = self.on_press.clone();
        let mut r = rect()
            .a11y_role(AccessibilityRole::ListBoxOption)
            .a11y_alt(match (&self.entry.off, &self.entry.sub) {
                (Some(why), _) if !why.is_empty() => format!("{}: {why}", self.entry.label),
                (_, Some(sub)) => format!("{}, {sub}", self.entry.label),
                _ => self.entry.label.clone(),
            })
            .width(Size::fill())
            .height(Size::px(frame::PALETTE_ROW))
            .padding((0., space::XS, 0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .cursor(if off { CursorIcon::NotAllowed } else { CursorIcon::Pointer })
            .on_sized(spot)
            .on_pointer_enter(move |_| hover.call(()))
            .on_press(move |_| press.call(()))
            .child(icon(&t, self.entry.glyph, iconsize::M, sub))
            .content(Content::flex());
        r = match &self.entry.sub {
            Some(sub_text) => r.child(
                rect()
                    .width(Size::flex(1.))
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .content(Content::flex())
                    .child(line(&t, self.entry.label.clone(), font::BODY, ink).max_lines(1).text_overflow(TextOverflow::Ellipsis).max_width(Size::percent(60.)))
                    .child(line(&t, sub_text.clone(), font::CALLOUT, sub).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.))),
            ),
            None => r.child(line(&t, self.entry.label.clone(), font::BODY, ink).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.))),
        };
        match (&self.entry.off, &self.entry.shortcut) {
            (Some(why), _) if !why.is_empty() => r = r.child(line(&t, why.clone(), font::CALLOUT, sub).max_lines(1)),
            (None, Some(k)) => r = r.child(line(&t, k.clone(), font::CALLOUT, sub)),
            _ => {}
        }
        if let Some(open) = self.on_actions.clone() {
            let chevron = rect()
                .a11y_role(AccessibilityRole::Button)
                .a11y_alt(tr("Aktionen"))
                .width(Size::px(control::S))
                .height(Size::px(control::S))
                .center()
                .corner_radius(t.shape.small)
                .cursor(CursorIcon::Pointer)
                .on_sized(actions_spot)
                .on_press(move |e: Event<PressEventData>| {
                    e.stop_propagation();
                    open.call(());
                })
                .child(icon(&t, Icon::ChevronRight, iconsize::S, sub));
            r = r.child(WithTip::new(Some(format!("{actions_label} (⇥)")), chevron));
        } else {
            r = r.child(rect().width(Size::px(space::XS)));
        }
        if self.hot {
            r = r.background(if off { t.hover } else { t.accent });
        }
        r
    }
}
