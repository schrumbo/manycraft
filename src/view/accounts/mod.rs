//! The Accounts page: every account with what it is doing now, as a calm list or, for many accounts, a sortable table;
//! pick several to add them to a batch, set a proxy, recolour or remove them at once.

mod list;
mod table;
mod work;

use std::collections::BTreeSet;
use std::time::Instant;

use freya::prelude::*;
use uuid::Uuid;

use super::commands::{Want, can_want, want, want_blocked};
use super::icons::{Icon, icon};
use super::kit::badge::Tone;
use super::kit::button::Button;
use super::kit::field::Field;
use super::kit::menu::{self, Item, Menu};
use super::kit::segmented::{Segment, Segmented};
use super::kit::text::{caption, heading, line, para, secondary, spacer, strong};
use super::kit::{Bounds, held, use_theme};
use super::store::{Store, use_store};
use super::theme::{Theme, font, frame, iconsize, space};
use super::ui::Page;
use crate::app::model::{Game, Model, Phase, rank, status, stopped_ban};
use crate::core::account::{Account, AccountKind, COLORS};
use crate::core::history::Kind;
use crate::core::i18n::{tr, trf};
use crate::core::prism::AccountImport;
use crate::ui::forms::{Problem, offline_name_problem, proxy_display};

pub use work::{add_to_batch, ask_remove, import_prism, look_for_prism, off};

const PAGE_MAX: f32 = 760.;
/// From this many accounts on the page offers a filter.
pub const FILTER_FROM: usize = 12;
const TABLE_FROM: usize = 30;

/// How the page lays the accounts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    List,
    Table,
}

/// The table columns, also what it sorts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Kind,
    Batches,
    Status,
    Proxy,
    Online,
}

/// What the page learned about Prism Launcher's accounts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prism {
    pub found: usize,
    pub busy: bool,
    pub done: Option<AccountImport>,
}

/// The page's state, kept while other pages show so the picks and the view survive a visit to the grid.
#[derive(Clone, Copy, PartialEq)]
pub struct Shared {
    pub picked: State<BTreeSet<Uuid>>,
    /// The account the keyboard is on, and where Shift extends a range from.
    pub cursor: State<Option<Uuid>>,
    /// The layout the person chose; `None` follows the number of accounts.
    pub view: State<Option<View>>,
    pub filter: State<String>,
    pub focus_filter: State<Option<Instant>>,
    pub sort: State<(Column, bool)>,
    /// The account whose proxy is being edited in the list.
    pub editing: State<Option<Uuid>>,
    /// Accounts being removed right now.
    pub removing: State<BTreeSet<Uuid>>,
    pub problem: State<Option<Problem>>,
    pub prism: State<Prism>,
}

impl Shared {
    /// Empty state: nothing picked, the layout follows the count.
    pub fn create() -> Self {
        Self {
            picked: State::create(BTreeSet::new()),
            cursor: State::create(None),
            view: State::create(None),
            filter: State::create(String::new()),
            focus_filter: State::create(None),
            sort: State::create((Column::Name, true)),
            editing: State::create(None),
            removing: State::create(BTreeSet::new()),
            problem: State::create(None),
            prism: State::create(Prism::default()),
        }
    }
}

/// One account as the page shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: Uuid,
    pub name: String,
    pub microsoft: bool,
    pub proxy: Option<String>,
    pub color: u8,
    pub batches: Vec<String>,
    pub status: String,
    pub tone: Tone,
    pub sign_in: bool,
    /// Unix seconds of the last join; `Some(u64::MAX)` while on a server.
    pub online: Option<u64>,
    pub busy: bool,
}

fn describe(m: &Model, a: &Account, removing: &BTreeSet<Uuid>) -> Line {
    let games: Vec<&Game> = m.games.values().filter(|g| g.account_id == a.id).collect();
    let owner = |g: &Game| m.batch(&g.batch_id).map(|b| b.name.clone()).or_else(|| m.instance(&g.instance_id).map(|i| i.name.clone())).unwrap_or_default();
    let with_owner = |g: &Game, text: String| if games.len() > 1 { format!("{text} · {}", owner(g)) } else { text };
    let preparing = |g: &Game| m.preparing.get(&g.batch_id).copied();
    let batches: Vec<String> = m.batches.iter().filter(|b| b.accounts.contains(&a.id)).map(|b| b.name.clone()).collect();
    let attention = games.iter().filter(|g| g.phase.needs_attention()).min_by_key(|g| rank(&g.phase));
    let active = games.iter().filter(|g| g.phase.is_active()).min_by_key(|g| rank(&g.phase));
    let banned = games.iter().find_map(|g| stopped_ban(g, m).map(|s| (*g, s)));
    let (status, tone) = if let Some(g) = attention {
        (with_owner(g, status(g, preparing(g))), if matches!(g.phase, Phase::Banned { .. }) { Tone::Danger } else { Tone::Attention })
    } else if let Some(g) = active {
        (with_owner(g, status(g, preparing(g))), if g.phase.on_server() { Tone::Accent } else { Tone::Quiet })
    } else if let Some((g, text)) = banned {
        (with_owner(g, text), Tone::Danger)
    } else if batches.is_empty() && games.is_empty() {
        (tr("In keinem Batch"), Tone::Quiet)
    } else {
        (tr("Gestoppt"), Tone::Quiet)
    };
    let online = if games.iter().any(|g| g.phase.on_server()) {
        Some(u64::MAX)
    } else {
        games.iter().filter_map(|g| m.history.get(&g.key)).flat_map(|log| log.iter()).filter(|e| e.kind == Kind::Connected).map(|e| e.at).max()
    };
    Line {
        id: a.id,
        name: a.name.clone(),
        microsoft: a.kind == AccountKind::Microsoft,
        proxy: a.proxy.as_deref().map(proxy_display),
        color: a.color(),
        batches,
        status,
        tone,
        sign_in: a.kind == AccountKind::Microsoft && games.iter().any(|g| g.phase == Phase::LoginNeeded),
        online,
        busy: removing.contains(&a.id),
    }
}

fn matches(line: &Line, q: &str) -> bool {
    q.is_empty() || line.name.to_lowercase().contains(q) || line.batches.iter().any(|b| b.to_lowercase().contains(q)) || line.proxy.as_deref().is_some_and(|p| p.to_lowercase().contains(q))
}

/// The accounts the page shows, filtered and in the table's order.
pub fn lines(m: &Model, shared: Shared) -> Vec<Line> {
    let q = shared.filter.peek().trim().to_lowercase();
    let removing = shared.removing.peek();
    let mut out: Vec<Line> = m.accounts.iter().map(|a| describe(m, a, &removing)).filter(|l| matches(l, &q)).collect();
    if view_of(m, shared) == View::Table {
        let (column, up) = *shared.sort.peek();
        sort(&mut out, column, up);
    }
    out
}

fn sort(out: &mut [Line], column: Column, up: bool) {
    out.sort_by(|a, b| {
        let order = match column {
            Column::Name => std::cmp::Ordering::Equal,
            Column::Kind => a.microsoft.cmp(&b.microsoft).reverse(),
            Column::Batches => a.batches.join(", ").to_lowercase().cmp(&b.batches.join(", ").to_lowercase()),
            Column::Status => tone_rank(a.tone).cmp(&tone_rank(b.tone)).then_with(|| a.status.cmp(&b.status)),
            Column::Proxy => a.proxy.is_none().cmp(&b.proxy.is_none()).then_with(|| a.proxy.cmp(&b.proxy)),
            Column::Online => b.online.cmp(&a.online),
        };
        let order = order.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        if up { order } else { order.reverse() }
    });
}

fn tone_rank(tone: Tone) -> u8 {
    match tone {
        Tone::Danger => 0,
        Tone::Attention => 1,
        Tone::Accent => 2,
        Tone::Quiet => 3,
    }
}

/// The layout in use: the one chosen, else the table from [`TABLE_FROM`] accounts on.
pub fn view_of(m: &Model, shared: Shared) -> View {
    shared.view.peek().unwrap_or(if m.accounts.len() >= TABLE_FROM { View::Table } else { View::List })
}

/// The ids of the accounts on the page, in order.
pub fn shown_ids(store: Store) -> Vec<Uuid> {
    lines(&store.model.peek(), store.ui.accounts).into_iter().map(|l| l.id).collect()
}

/// Picks `id` as a click does: alone, toggled with Ctrl or Cmd, or as a range from the cursor with Shift.
pub fn pick(store: Store, id: Uuid) {
    let mut shared = store.ui.accounts;
    let mods = held();
    let toggle = if cfg!(target_os = "macos") { mods.contains(Modifiers::META) } else { mods.contains(Modifiers::CONTROL) };
    let mut picked = shared.picked.peek().clone();
    if mods.contains(Modifiers::SHIFT)
        && let Some(anchor) = *shared.cursor.peek()
    {
        let ids = shown_ids(store);
        if let (Some(a), Some(b)) = (ids.iter().position(|i| *i == anchor), ids.iter().position(|i| *i == id)) {
            if !toggle {
                picked.clear();
            }
            picked.extend(ids[a.min(b)..=a.max(b)].iter().copied());
            shared.picked.set(picked);
            return;
        }
    }
    if toggle {
        if !picked.remove(&id) {
            picked.insert(id);
        }
    } else {
        picked = BTreeSet::from([id]);
    }
    shared.picked.set(picked);
    shared.cursor.set(Some(id));
}

/// Moves the cursor by `delta` rows; with `extend` the picks grow from the anchor as Shift+arrow does.
pub fn step(store: Store, delta: isize, extend: bool) {
    let mut shared = store.ui.accounts;
    let ids = shown_ids(store);
    if ids.is_empty() {
        return;
    }
    let now = shared.cursor.peek().and_then(|c| ids.iter().position(|i| *i == c));
    let next = match now {
        Some(at) => at.saturating_add_signed(delta).min(ids.len() - 1),
        None if delta < 0 => ids.len() - 1,
        None => 0,
    };
    let id = ids[next];
    if extend {
        shared.picked.write().insert(id);
    } else {
        shared.picked.set(BTreeSet::from([id]));
    }
    shared.cursor.set(Some(id));
}

/// The accounts an action applies to: the picks, else the account the keyboard is on.
pub fn targets(store: Store) -> Vec<Uuid> {
    let shared = store.ui.accounts;
    let picked = shared.picked.peek();
    let ids = shown_ids(store);
    if picked.is_empty() {
        return shared.cursor.peek().filter(|c| ids.contains(c)).into_iter().collect();
    }
    ids.into_iter().filter(|i| picked.contains(i)).collect()
}

/// Selects every account on the page.
pub fn pick_all(store: Store) {
    let mut picked = store.ui.accounts.picked;
    picked.set(shown_ids(store).into_iter().collect());
}

/// Clears the picks; false when nothing was picked.
pub fn clear(store: Store) -> bool {
    let mut picked = store.ui.accounts.picked;
    if picked.peek().is_empty() {
        return false;
    }
    picked.set(BTreeSet::new());
    true
}

/// The Microsoft accounts that wait for a new sign-in.
pub fn waiting(m: &Model) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = m.accounts.iter().filter(|a| a.kind == AccountKind::Microsoft && m.games.values().any(|g| g.account_id == a.id && g.phase == Phase::LoginNeeded)).map(|a| a.id).collect();
    ids.dedup();
    ids
}

/// Asks the sheets to sign `ids` in again, one after another.
pub fn sign_in(store: Store, ids: Vec<Uuid>) {
    if !ids.is_empty() {
        want(store, Want::SignInAgain(ids));
    }
}

/// The colour menu for `ids`, with the colour they share checked.
pub fn color_items(store: Store, ids: Vec<Uuid>) -> Vec<Item> {
    let m = store.model.peek();
    let colors: BTreeSet<u8> = m.accounts.iter().filter(|a| ids.contains(&a.id)).map(Account::color).collect();
    let current = (colors.len() == 1).then(|| colors.first().copied()).flatten();
    (0..COLORS.len() as u8)
        .map(|n| {
            let ids = ids.clone();
            Item::new(format!("accounts.color.{n}"), crate::ui::identity::name(n), move |_| store.dispatch(crate::app::event::Event::Recolor { ids: ids.clone(), color: n })).checked(current == Some(n))
        })
        .collect()
}

/// The batch menu for `ids`: every batch, those that already hold all of them checked and off.
pub fn batch_items(store: Store, ids: Vec<Uuid>) -> Vec<Item> {
    let m = store.model.peek();
    let mut batches: Vec<(String, String, bool)> = m.batches.iter().map(|b| (b.id.clone(), b.name.clone(), ids.iter().all(|i| b.accounts.contains(i)))).collect();
    batches.sort_by_key(|(_, name, _)| name.to_lowercase());
    let mut items: Vec<Item> = batches
        .into_iter()
        .map(|(id, name, all)| {
            let ids = ids.clone();
            let batch = id.clone();
            Item::new(format!("accounts.batch.{id}"), name, move |_| add_to_batch(store, batch.clone(), ids.clone())).checked(all).disabled(all)
        })
        .collect();
    if can_want(&Want::NewBatch) {
        if !items.is_empty() {
            items.push(Item::Separator);
        }
        items.push(Item::new("accounts.batch.new", tr("Neuer Batch…"), move |_| {
            want(store, Want::NewBatch);
        }));
    }
    items
}

/// The context menu for the picks, or for `id` alone when it is not picked.
pub fn context(store: Store, id: Uuid, at: (f32, f32)) -> Vec<Item> {
    let mut shared = store.ui.accounts;
    if !shared.picked.peek().contains(&id) {
        shared.picked.set(BTreeSet::from([id]));
        shared.cursor.set(Some(id));
    }
    let ids = targets(store);
    let m = store.model.peek();
    let chosen: Vec<&Account> = m.accounts.iter().filter(|a| ids.contains(&a.id)).collect();
    let microsoft: Vec<Uuid> = chosen.iter().filter(|a| a.kind == AccountKind::Microsoft).map(|a| a.id).collect();
    let one = chosen.len() == 1;
    let mut items = Vec::new();
    if !microsoft.is_empty() {
        let again = microsoft.clone();
        items.push(Item::new("accounts.menu.signin", if microsoft.len() == 1 { tr("Erneut anmelden") } else { tr("Nacheinander neu anmelden") }, move |_| sign_in(store, again.clone())).disabled(!can_want(&Want::SignInAgain(Vec::new()))));
    }
    if one && !microsoft.is_empty() {
        items.push(Item::new("accounts.menu.skin", tr("Skin ändern…"), move |_| {
            want(store, Want::ChangeSkin(id));
        }).disabled(!can_want(&Want::ChangeSkin(id))));
    }
    if one {
        items.push(Item::new("accounts.menu.proxy", tr("Proxy bearbeiten"), move |_| work::edit_proxy(store, vec![id])));
    } else {
        let many = ids.clone();
        items.push(Item::new("accounts.menu.proxy", tr("Proxy für alle setzen…"), move |_| work::edit_proxy(store, many.clone())));
    }
    items.push(Item::Separator);
    let colors = ids.clone();
    items.push(Item::new("accounts.menu.color", tr("Farbe…"), move |_| {
        menu::open(store.ui, Menu::at(color_items(store, colors.clone()), at));
    }));
    let joins = ids.clone();
    items.push(Item::new("accounts.menu.batch", tr("Zu Batch hinzufügen…"), move |_| {
        menu::open(store.ui, Menu::at(batch_items(store, joins.clone()), at));
    }).disabled(m.batches.is_empty()));
    items.push(Item::Separator);
    let gone = ids;
    items.push(Item::new("accounts.menu.remove", if one { tr("Entfernen…") } else { trf("%lld Accounts entfernen…", &[&chosen.len()]) }, move |_| ask_remove(store, gone.clone())).shortcut(super::commands::Command::RemoveAccounts.keys()).danger());
    items
}

/// The face chip of an account: its skin face on its colour, else the first letter.
pub fn face(t: &Theme, store: Store, l: &Line, side: f32) -> Element {
    let fill = t.identity(usize::from(l.color));
    let chip = rect().width(Size::px(side)).height(Size::px(side)).corner_radius(t.shape.small).background(fill).center().overflow(Overflow::Clip);
    match store.faces.read().get(&l.id).cloned() {
        Some(handle) => chip.padding(side / 12.).child(image(handle).width(Size::fill()).height(Size::fill()).corner_radius(t.shape.small / 2.)).into_element(),
        None => chip.child(label().text(l.name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()).font_size(side * 0.5).font_weight(FontWeight::BOLD).color(t.ink_on(fill))).into_element(),
    }
}

/// The colour of a status line.
pub fn tone_color(t: &Theme, tone: Tone) -> Color {
    match tone {
        Tone::Quiet => t.text2,
        Tone::Accent => t.success_text,
        Tone::Attention => t.attention_text,
        Tone::Danger => t.danger_text,
    }
}

/// When an account was last on a server, in a few words.
pub fn online_text(online: Option<u64>) -> String {
    match online {
        Some(u64::MAX) => tr("Jetzt"),
        Some(at) => crate::ui::instance_files::when(i64::try_from(at).unwrap_or(i64::MAX)),
        None => "—".to_owned(),
    }
}

/// The count line under the page title.
pub fn count_line(m: &Model) -> String {
    let n = m.accounts.len();
    let mut parts = vec![if n == 1 { tr("1 Account") } else { trf("%lld Accounts", &[&n]) }];
    let playing = m.accounts.iter().filter(|a| m.games.values().any(|g| g.account_id == a.id && g.phase.on_server())).count();
    if playing > 0 {
        parts.push(if playing == 1 { tr("1 spielt") } else { trf("%lld spielen", &[&playing]) });
    }
    let waiting = waiting(m).len();
    if waiting > 0 {
        parts.push(if waiting == 1 { tr("1 braucht eine Anmeldung") } else { trf("%lld brauchen eine Anmeldung", &[&waiting]) });
    }
    parts.join(" · ")
}

/// The page's controls in the toolbar: the layout, the filter from a dozen accounts on, Prism and adding an account.
#[derive(PartialEq)]
pub struct Tools {
    pub room: f32,
}

impl Component for Tools {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut shared = store.ui.accounts;
        let filter_a11y = use_a11y();
        let focus_at = *shared.focus_filter.read();
        use_side_effect_with_deps(&focus_at, move |at| {
            if at.is_some() {
                filter_a11y.request_focus();
                let mut asked = store.ui.accounts.focus_filter;
                asked.set(None);
            }
        });
        use_hook(move || look_for_prism(store));
        let m = store.model.read();
        let count = m.accounts.len();
        let _ = shared.view.read();
        let view = view_of(&m, shared);
        let prism = shared.prism.read().clone();
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        if count >= FILTER_FROM || !shared.filter.read().is_empty() {
            let w = if self.room >= frame::TOOLBAR_SEARCH { frame::SEARCH } else { frame::SEARCH_NARROW };
            row = row.child(Field::new("accounts.filter", shared.filter).placeholder(tr("Accounts filtern")).glyph(Icon::Search).width(Size::px(w)).a11y(filter_a11y));
        }
        if count > 0 && self.room >= frame::TOOLBAR_FULL {
            let items = vec![
                Segment { id: "accounts.view.list".into(), label: tr("Liste"), tip: Some(tr("Karten mit allem, was ein Account gerade tut")) },
                Segment { id: "accounts.view.table".into(), label: tr("Tabelle"), tip: Some(tr("Kompakte Zeilen zum Sortieren und Auswählen")) },
            ];
            row = row.child(Segmented::new(tr("Ansicht"), items, usize::from(view == View::Table)).on_choose(move |i| shared.view.set(Some(if i == 1 { View::Table } else { View::List }))));
        }
        if prism.found > 0 && count > 0 {
            row = row.child(Button::new("accounts.prism", tr("Aus Prism übernehmen")).glyph(Icon::Download).disabled(prism.busy.then(|| tr("Wird übernommen …"))).on_press(move |_| import_prism(store)));
        }
        row.child(Button::new("accounts.add", tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().disabled(want_blocked(&Want::AddAccount)).tip(super::commands::Command::AddAccount.tip()).on_press(move |_| {
            want(store, Want::AddAccount);
        }))
    }
}

/// The Accounts page.
#[derive(PartialEq)]
pub struct AccountsPage;

impl Component for AccountsPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let shared = store.ui.accounts;
        let m = store.model.read();
        let _ = shared.removing.read();
        let _ = shared.filter.read();
        let _ = shared.sort.read();
        let _ = shared.view.read();
        let shown = lines(&m, shared);
        let known: BTreeSet<Uuid> = m.accounts.iter().map(|a| a.id).collect();
        let listed = view_of(&m, shared) == View::List;
        use_side_effect_with_deps(&(known, listed), move |(known, listed)| {
            let mut s = store.ui.accounts;
            if s.editing.peek().is_some_and(|e| !*listed || !known.contains(&e)) {
                s.editing.set(None);
            }
            if s.picked.peek().iter().any(|i| !known.contains(i)) {
                s.picked.write().retain(|i| known.contains(i));
            }
            if s.cursor.peek().is_some_and(|c| !known.contains(&c)) {
                s.cursor.set(None);
            }
        });
        let mut page = rect().width(Size::fill()).height(Size::fill()).on_global_key_down(move |e: Event<KeyboardEventData>| keys(store, &e));
        if !m.loaded {
            return match m.error.clone() {
                None => page.center().child(secondary(&t, tr("Lädt …"))).into_element(),
                Some(why) => page
                    .center()
                    .spacing(space::M)
                    .child(rect().max_width(Size::px(PAGE_MAX * 0.6)).child(para(&t, why, font::BODY, t.text2).text_align(TextAlign::Center)))
                    .child(Button::new("accounts.retry", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| store.retry()))
                    .into_element(),
            };
        }
        let view = view_of(&m, shared);
        let wide = view == View::Table;
        let mut strips = rect().width(Size::fill()).spacing(space::S);
        let mut any = false;
        if let Some(p) = shared.problem.read().clone() {
            strips = strips.child(ProblemStrip { problem: p });
            any = true;
        }
        if let Some(card) = prism_card(&t, store, &m) {
            strips = strips.child(card);
            any = true;
        }
        let waiting = waiting(&m);
        if !waiting.is_empty() {
            strips = strips.child(sign_in_strip(&t, store, waiting));
            any = true;
        }
        let picked = {
            let p = shared.picked.read();
            shown.iter().filter(|l| p.contains(&l.id)).count()
        };
        if picked > 1 {
            strips = strips.child(SelectionBar { count: picked, all: shown.len() });
            any = true;
        }
        let mut column = rect().width(Size::fill()).height(Size::fill()).cross_align(Alignment::Center);
        if m.accounts.is_empty() {
            return page.child(empty(&t, store)).into_element();
        }
        let pad = if wide { space::L } else { space::XL };
        let mut inner = rect().width(Size::fill()).height(Size::fill()).padding((space::L, pad, 0., pad)).spacing(space::M);
        if !wide {
            inner = inner.max_width(Size::px(PAGE_MAX + pad * 2.));
        }
        if any {
            inner = inner.child(strips);
        }
        if shown.is_empty() {
            inner = inner.child(no_match(&t, store));
        } else if wide {
            inner = inner.child(table::Table { lines: shown });
        } else {
            inner = inner.child(list::List { lines: shown });
        }
        column = column.child(inner);
        page = page.child(column);
        page.into_element()
    }
}

fn keys(store: Store, e: &KeyboardEventData) {
    if store.ui.layered() || super::kit::field::typing() || store.ui.accounts.editing.peek().is_some() {
        return;
    }
    let shift = e.modifiers.contains(Modifiers::SHIFT);
    match &e.key {
        Key::Named(NamedKey::ArrowDown) => step(store, 1, shift),
        Key::Named(NamedKey::ArrowUp) => step(store, -1, shift),
        Key::Named(NamedKey::Home) => step(store, isize::MIN / 2, shift),
        Key::Named(NamedKey::End) => step(store, isize::MAX / 2, shift),
        Key::Named(NamedKey::Enter) if e.modifiers.is_empty() => {
            let ids = targets(store);
            if !ids.is_empty() {
                work::edit_proxy(store, ids);
            }
        }
        Key::Named(NamedKey::ContextMenu) => {
            if let Some(id) = *store.ui.accounts.cursor.peek() {
                let (w, h) = *store.ui.window.peek();
                let at = (w / 2., h / 3.);
                menu::open(store.ui, Menu::at(context(store, id, at), at));
            }
        }
        _ => {}
    }
}

fn empty(t: &Theme, store: Store) -> Element {
    let prism = store.ui.accounts.prism.read().clone();
    let mut actions = rect().horizontal().spacing(space::S).child(Button::new("accounts.empty.add", tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().tall().disabled(want_blocked(&Want::AddAccount)).on_press(move |_| {
        want(store, Want::AddAccount);
    }));
    if prism.found > 0 {
        actions = actions.child(Button::new("accounts.empty.prism", tr("Aus Prism übernehmen")).glyph(Icon::Download).tall().disabled(prism.busy.then(|| tr("Wird übernommen …"))).on_press(move |_| import_prism(store)));
    }
    let mut col = rect().width(Size::fill()).height(Size::fill()).center().spacing(space::M).padding(space::XL);
    if let Some(p) = store.ui.accounts.problem.read().clone() {
        col = col.child(rect().width(Size::px(PAGE_MAX * 0.7)).child(ProblemStrip { problem: p }));
    }
    if let Some(card) = prism_card(t, store, &store.model.read()) {
        col = col.child(rect().width(Size::px(PAGE_MAX * 0.7)).child(card));
    }
    col.child(icon(t, Icon::Users, iconsize::L * 2., t.text3))
        .child(heading(t, tr("Hier ist es noch still")))
        .child(rect().max_width(Size::px(PAGE_MAX * 0.6)).child(para(t, tr("Füge ein Microsoft-Konto hinzu oder ein Offline-Konto für Server ohne Anmeldeprüfung."), font::BODY, t.text2).text_align(TextAlign::Center)))
        .child(actions)
        .into_element()
}

fn no_match(t: &Theme, store: Store) -> Element {
    let mut filter = store.ui.accounts.filter;
    let q = filter.read().trim().to_owned();
    rect()
        .width(Size::fill())
        .padding((space::XXL, 0.))
        .center()
        .spacing(space::M)
        .child(icon(t, Icon::Search, iconsize::L * 2., t.text3))
        .child(heading(t, tr("Kein Treffer")))
        .child(secondary(t, trf("Kein Account passt zu „%@“.", &[&q])))
        .child(Button::new("accounts.filter.clear", tr("Filter löschen")).tall().on_press(move |_| filter.set(String::new())))
        .into_element()
}

fn sign_in_strip(t: &Theme, store: Store, ids: Vec<Uuid>) -> Element {
    let text = if ids.len() == 1 {
        let m = store.model.peek();
        let name = m.account(ids[0]).map(|a| a.name.clone()).unwrap_or_default();
        trf("„%@“ braucht eine neue Anmeldung.", &[&name])
    } else {
        trf("%lld Accounts brauchen eine neue Anmeldung.", &[&ids.len()])
    };
    let label = if ids.len() == 1 { tr("Anmelden") } else { tr("Alle nacheinander anmelden") };
    strip(t, Icon::Key, t.attention_text, t.attention.with_af32(frame::NOTICE_WASH))
        .child(line(t, text, font::CALLOUT, t.text1).width(Size::flex(1.)).max_lines(1).text_overflow(TextOverflow::Ellipsis))
        .content(Content::flex())
        .child(Button::new("accounts.signin.all", label).glyph(Icon::LogIn).disabled(want_blocked(&Want::SignInAgain(Vec::new()))).on_press(move |_| sign_in(store, ids.clone())))
        .into_element()
}

fn strip(t: &Theme, glyph: Icon, ink: Color, wash: Color) -> Rect {
    rect()
        .width(Size::fill())
        .min_height(Size::px(frame::NOTICE + space::S))
        .padding((space::XS, space::XS, space::XS, space::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .corner_radius(t.shape.control)
        .background(wash)
        .child(icon(t, glyph, iconsize::S, ink))
}

/// A failure of the page with the cause behind Details.
#[derive(PartialEq)]
struct ProblemStrip {
    problem: Problem,
}

impl Component for ProblemStrip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut open = use_state(|| false);
        let mut problem = store.ui.accounts.problem;
        let mut head = strip(&t, Icon::Alert, t.danger_text, t.danger.with_af32(frame::NOTICE_WASH))
            .child(para(&t, self.problem.text.clone(), font::CALLOUT, t.text1).width(Size::flex(1.)))
            .content(Content::flex());
        if self.problem.detail.is_some() {
            head = head.child(Button::new("accounts.problem.details", if *open.read() { tr("Details ausblenden") } else { tr("Details") }).plain().on_press(move |_| open.toggle()));
        }
        head = head.child(Button::icon("accounts.problem.close", Icon::Close, tr("Ausblenden")).plain().on_press(move |_| problem.set(None)));
        let mut col = rect().width(Size::fill()).spacing(space::XS).child(head);
        if *open.read()
            && let Some(d) = &self.problem.detail
        {
            col = col.child(rect().width(Size::fill()).padding((space::XS, space::S)).corner_radius(t.shape.small).background(t.well).child(super::kit::text::mono(&t, d.clone(), font::CAPTION, t.text2)));
        }
        col
    }
}

fn prism_card(t: &Theme, store: Store, m: &Model) -> Option<Element> {
    let done = store.ui.accounts.prism.read().done.clone()?;
    let waiting: Vec<String> = done.needs_sign_in.iter().filter(|n| !m.accounts.iter().any(|a| a.name.eq_ignore_ascii_case(n))).cloned().collect();
    let existing = done.skipped.iter().filter(|(_, why)| why == "already added").count();
    let refused: Vec<String> = done
        .skipped
        .iter()
        .filter(|(_, why)| why != "already added")
        .map(|(name, _)| trf("„%@“ übersprungen: %@", &[name, &offline_name_problem(name, &[]).unwrap_or_else(|| tr("Der Name ist ungültig."))]))
        .collect();
    let added = done.added.len();
    let summary = match (added, waiting.len()) {
        (0, 0) if refused.is_empty() => tr("Aus Prism gibt es nichts Neues. Alle Namen sind schon hier."),
        (0, _) => tr("Keine Offline-Accounts aus Prism übernommen."),
        (1, _) => tr("1 Offline-Account aus Prism übernommen."),
        _ => trf("%lld Offline-Accounts aus Prism übernommen.", &[&added]),
    };
    let mut prism = store.ui.accounts.prism;
    let mut card = rect().width(Size::fill()).padding((space::M, space::L)).spacing(space::S).corner_radius(t.shape.tile).background(t.raised).border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner));
    card = card.child(
        rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(icon(t, if added > 0 { Icon::Check } else { Icon::Help }, iconsize::S, if added > 0 { t.success_text } else { t.text2 }))
            .child(line(t, summary, font::BODY, t.text1).width(Size::flex(1.)))
            .content(Content::flex())
            .child(Button::icon("accounts.prism.close", Icon::Close, tr("Ausblenden")).plain().on_press(move |_| prism.write().done = None)),
    );
    if !waiting.is_empty() {
        let all = waiting.clone();
        let mut intro = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(para(t, tr("Diese Microsoft-Accounts brauchen eine neue Anmeldung. Anmeldungen aus Prism werden nicht übernommen."), font::CALLOUT, t.text2).width(Size::flex(1.))).content(Content::flex());
        if waiting.len() > 1 {
            intro = intro.child(Button::new("accounts.prism.signin.all", tr("Alle nacheinander anmelden")).glyph(Icon::LogIn).disabled(want_blocked(&Want::SignInNames(Vec::new()))).on_press(move |_| {
                want(store, Want::SignInNames(all.clone()));
            }));
        }
        card = card.child(intro);
        for (at, name) in waiting.iter().enumerate() {
            let mut order = waiting.clone();
            order.rotate_left(at);
            card = card.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::M)
                    .child(icon(t, Icon::Key, iconsize::S, t.text2))
                    .child(strong(t, name.clone(), font::BODY, t.text1).width(Size::flex(1.)).max_lines(1).text_overflow(TextOverflow::Ellipsis))
                    .content(Content::flex())
                    .child(Button::new(format!("accounts.prism.signin.{name}"), tr("Anmelden")).glyph(Icon::LogIn).disabled(want_blocked(&Want::SignInNames(Vec::new()))).on_press(move |_| {
                        want(store, Want::SignInNames(order.clone()));
                    })),
            );
        }
    }
    if existing > 0 && (added > 0 || !done.needs_sign_in.is_empty()) {
        card = card.child(caption(t, if existing == 1 { tr("1 Name war schon vorhanden.") } else { trf("%lld Namen waren schon vorhanden.", &[&existing]) }));
    }
    for text in refused {
        card = card.child(rect().horizontal().spacing(space::S).cross_align(Alignment::Center).child(icon(t, Icon::Alert, iconsize::S, t.danger_text)).child(line(t, text, font::CALLOUT, t.danger_text)));
    }
    Some(card.into_element())
}

/// The bar over the accounts while several are picked: what can be done to all of them at once.
#[derive(PartialEq)]
struct SelectionBar {
    count: usize,
    all: usize,
}

impl Component for SelectionBar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut batch_at = use_state(Bounds::default);
        let mut color_at = use_state(Bounds::default);
        let no_batch = store.model.read().batches.is_empty();
        let mut bar = rect()
            .width(Size::fill())
            .height(Size::px(frame::NOTICE + space::S))
            .padding((0., space::XS, 0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .background(t.selection)
            .child(strong(&t, trf("%lld ausgewählt", &[&self.count]), font::CALLOUT, t.accent_text))
            .content(Content::flex());
        if self.count < self.all {
            bar = bar.child(Button::new("accounts.bulk.all", trf("Alle %lld auswählen", &[&self.all])).plain().tip(super::commands::Command::SelectAll.tip()).on_press(move |_| pick_all(store)));
        }
        bar = bar.child(spacer());
        bar = bar
            .child(rect().on_sized(move |e: Event<SizedEventData>| batch_at.set_if_modified(Bounds::of(&e))).child(Button::new("accounts.bulk.batch", tr("Zu Batch hinzufügen…")).glyph(Icon::Layers).disabled(no_batch.then(|| tr("Leg zuerst einen Batch an."))).on_press(move |_| {
                let ids = targets(store);
                menu::open(store.ui, Menu::under(batch_items(store, ids), *batch_at.peek()));
            })))
            .child(Button::new("accounts.bulk.proxy", tr("Proxy…")).glyph(Icon::Globe).on_press(move |_| work::edit_proxy(store, targets(store))))
            .child(rect().on_sized(move |e: Event<SizedEventData>| color_at.set_if_modified(Bounds::of(&e))).child(Button::new("accounts.bulk.color", tr("Farbe…")).glyph(Icon::Palette).on_press(move |_| {
                let ids = targets(store);
                menu::open(store.ui, Menu::under(color_items(store, ids), *color_at.peek()));
            })))
            .child(Button::new("accounts.bulk.remove", tr("Entfernen…")).glyph(Icon::Trash).danger().on_press(move |_| ask_remove(store, targets(store))))
            .child(Button::icon("accounts.bulk.clear", Icon::Close, tr("Auswahl aufheben")).plain().tip(super::commands::Command::ClearSelection.tip()).on_press(move |_| {
                clear(store);
            }));
        bar
    }
}

/// Opens the Accounts page with the filter field focused.
pub fn focus_filter(store: Store) {
    let mut ui = store.ui;
    if *ui.page.peek() != Page::Accounts {
        ui.page.set(Page::Accounts);
    }
    let mut at = ui.accounts.focus_filter;
    at.set(Some(Instant::now()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(name: &str, tone: Tone, online: Option<u64>, proxy: Option<&str>) -> Line {
        Line { id: Uuid::new_v4(), name: name.into(), microsoft: true, proxy: proxy.map(str::to_owned), color: 0, batches: Vec::new(), status: String::new(), tone, sign_in: false, online, busy: false }
    }

    #[test]
    fn status_sorts_trouble_first_and_names_break_ties() {
        let mut v = vec![line("b", Tone::Quiet, None, None), line("a", Tone::Quiet, None, None), line("c", Tone::Attention, None, None), line("d", Tone::Danger, None, None)];
        sort(&mut v, Column::Status, true);
        assert_eq!(v.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["d", "c", "a", "b"]);
        sort(&mut v, Column::Status, false);
        assert_eq!(v[0].name, "b");
    }

    #[test]
    fn online_sorts_playing_then_newest_then_never() {
        let mut v = vec![line("never", Tone::Quiet, None, None), line("old", Tone::Quiet, Some(10), None), line("now", Tone::Quiet, Some(u64::MAX), None), line("new", Tone::Quiet, Some(20), None)];
        sort(&mut v, Column::Online, true);
        assert_eq!(v.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["now", "new", "old", "never"]);
    }

    #[test]
    fn proxies_sort_before_none() {
        let mut v = vec![line("x", Tone::Quiet, None, None), line("y", Tone::Quiet, None, Some("socks5://b:1")), line("z", Tone::Quiet, None, Some("socks5://a:1"))];
        sort(&mut v, Column::Proxy, true);
        assert_eq!(v.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["z", "y", "x"]);
    }

    #[test]
    fn filter_looks_at_name_batches_and_proxy() {
        let mut l = line("Steve", Tone::Quiet, None, Some("socks5://eu.example:1080"));
        l.batches = vec!["Farm".into()];
        assert!(matches(&l, "ste"));
        assert!(matches(&l, "farm"));
        assert!(matches(&l, "eu.example"));
        assert!(!matches(&l, "alex"));
        assert!(matches(&l, ""));
    }
}
