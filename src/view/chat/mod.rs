//! The Chat page: the chat of every game as one stream, and a composer that always says who sends and switches that
//! with one key.

mod picker;
mod target;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use freya::prelude::*;

use self::picker::{List, Step, face, mark, step};
pub use self::target::Target;
use self::target::{Choice, at_query, at_word, batches_of, choices, exists, filter, name, reached, remember, resolve_at};
use super::commands::{Command, Extra, Shortcut};
use super::drive::use_spot;
use super::icons::{Icon, icon};
use super::kit::button::Button;
use super::kit::field::Field;
use super::kit::select::Select;
use super::kit::text::{caption, heading, line, para, secondary};
use super::kit::tooltip::WithTip;
use super::kit::menu::{self, Item, Menu};
use super::kit::{Bounds, open_external, ring, toast, use_theme};
use super::native;
use super::store::{Store, use_store};
use super::theme::{Theme, color, control, font, frame, iconsize, level, space};
use crate::app::event::Event as AppEvent;
use crate::app::model::{ChatLine, Model};
use crate::core::batch::MAX_CHAT_CHARS;
use crate::core::chat_text::{self, Run};
use crate::core::i18n::{tr, trf};
use crate::core::settings;
use crate::core::themes::Rgba;

const KEPT_HISTORY: usize = 100;
const SHOWN_CHARS: usize = 600;
const PAGE: usize = 200;
const TIME_W: f32 = 44.;
const PICKER_W: f32 = 200.;
const PICKER_NARROW: f32 = 150.;
const CHIP_MAX: f32 = 220.;
const POPUP_W: f32 = 320.;
const POPUP_ROWS: f32 = 10.;
const TAG_MAX: f32 = 180.;
const MENTION_MIN: usize = 3;
const HEARD_MAX: usize = 3;
const LINK_SHOWN: usize = 40;
const GLYPH: f32 = 0.5;
const READING: f32 = control::READING;

/// The page's state, kept while other pages show so the draft, the history and the recipient survive.
#[derive(Clone, Copy, PartialEq)]
pub struct Shared {
    pub batch: State<Option<String>>,
    /// The recipient the person picked; `None` follows the batch shown, else everyone.
    pub target: State<Option<Target>>,
    pub recent: State<Vec<Target>>,
    pub draft: State<String>,
    pub history: State<Vec<String>>,
    pub recall: State<Option<usize>>,
    /// What was being typed before the arrows went back in the history.
    pub typed: State<String>,
    pub sent: State<Option<String>>,
    pub rejected: State<Option<(String, String)>>,
    /// How many of the newest lines the list holds.
    pub shown: State<usize>,
    pub focus: State<Option<Instant>>,
    /// Whether the recipient pop-up is open, its filter and the entry lit in it.
    pub picker: State<bool>,
    pub query: State<String>,
    pub hot: State<Option<usize>>,
    /// The entry lit under an `@name`, and the draft whose suggestions Esc closed.
    pub at_hot: State<Option<usize>>,
    pub at_closed: State<Option<String>>,
    /// Where the recipient control and the message field are, for the pop-ups above them.
    pub chip_at: State<Bounds>,
    pub field_at: State<Bounds>,
    /// The sequence number of the first line that arrived after the page was left.
    pub left_at: State<Option<u64>>,
}

impl Shared {
    /// Empty state: everything shown, everyone gets the next line.
    pub fn create() -> Self {
        Self {
            batch: State::create(None),
            target: State::create(None),
            recent: State::create(Vec::new()),
            draft: State::create(String::new()),
            history: State::create(Vec::new()),
            recall: State::create(None),
            typed: State::create(String::new()),
            sent: State::create(None),
            rejected: State::create(None),
            shown: State::create(PAGE),
            focus: State::create(None),
            picker: State::create(false),
            query: State::create(String::new()),
            hot: State::create(None),
            at_hot: State::create(None),
            at_closed: State::create(None),
            chip_at: State::create(Bounds::default()),
            field_at: State::create(Bounds::default()),
            left_at: State::create(None),
        }
    }
}

/// The batch picker's entries: all batches, then each by name.
fn batch_choices(m: &Model) -> Vec<(String, Option<String>)> {
    let mut out = vec![(tr("Alle Batches"), None)];
    let names = unique(m.batches.iter().map(|b| b.name.clone()).collect());
    out.extend(m.batches.iter().zip(names).map(|(b, name)| (name, Some(b.id.clone()))));
    out
}

/// The recipient in use: the one picked while it still exists, else the batch shown, else everyone. A picked one that
/// is gone gives nobody, so a line never goes to more games than the person chose.
fn target_of(m: &Model, shared: Shared) -> Option<Target> {
    match shared.target.read().clone() {
        Some(t) if exists(m, &t) => Some(t),
        Some(_) => None,
        None => Some(shared.batch.read().clone().filter(|b| m.batch(b).is_some()).map_or(Target::All, Target::Batch)),
    }
}

/// Makes `target` the recipient and gives the message field the keyboard.
fn pick(store: Store, target: Target) {
    let mut shared = store.ui.chat;
    shared.target.set(Some(target));
    shared.picker.set_if_modified(false);
    shared.sent.set(None);
    shared.rejected.set(None);
    shared.focus.set(Some(Instant::now()));
}

/// Opens the recipient pop-up with an empty filter.
fn open_picker(store: Store) {
    let mut shared = store.ui.chat;
    shared.query.set(String::new());
    shared.hot.set(None);
    shared.picker.set(true);
}

fn send(store: Store) {
    let mut shared = store.ui.chat;
    let text = shared.draft.peek().trim().to_owned();
    if text.is_empty() || !settings::active().broadcast {
        return;
    }
    let named = {
        let m = store.model.peek();
        resolve_at(&text, &choices(&m, &shared.recent.peek()))
    };
    let text = match named {
        Some((target, rest)) => {
            shared.target.set(Some(target));
            rest
        }
        None => text,
    };
    if let Some((word, _)) = at_word(&text) {
        let problem = trf("„@%@“ passt nicht zu genau einem Account. Wähle den Absender links.", &[&word]);
        shared.rejected.set(Some((text.clone(), problem)));
        shared.sent.set(None);
        return;
    }
    if text.is_empty() {
        return;
    }
    let (target, reach, batches) = {
        let m = store.model.peek();
        let target = target_of(&m, shared);
        let reach = target.as_ref().map_or(0, |t| reached(&m, t));
        let batches = target.as_ref().map(|t| batches_of(&m, t)).unwrap_or_default();
        (target, reach, batches)
    };
    let problem = line_problem(&text).or_else(|| target.is_none().then(|| tr("Wähle, wer sendet."))).or_else(|| (reach == 0).then(|| tr("Dort ist gerade niemand im Spiel.")));
    if let Some(problem) = problem {
        shared.rejected.set(Some((text, problem)));
        shared.sent.set(None);
        return;
    }
    let Some(target) = target else { return };
    match &target {
        Target::Instance(key) => store.dispatch(AppEvent::SendChat { key: key.clone(), text: text.clone() }),
        _ => {
            for batch in batches {
                store.dispatch(AppEvent::Broadcast { batch, text: text.clone() });
            }
        }
    }
    if target != Target::All {
        let recent = remember(&shared.recent.peek(), &target);
        shared.recent.set(recent);
    }
    let mut history = shared.history.peek().clone();
    if history.last() != Some(&text) {
        history.push(text);
        if history.len() > KEPT_HISTORY {
            history.remove(0);
        }
        shared.history.set(history);
    }
    shared.recall.set(None);
    shared.typed.set(String::new());
    shared.rejected.set(None);
    shared.sent.set(Some(if reach == 1 { tr("An 1 Account gesendet.") } else { trf("An %lld Accounts gesendet.", &[&reach]) }));
    shared.draft.set(String::new());
}

fn step_history(store: Store, delta: isize) {
    let mut shared = store.ui.chat;
    let current = shared.draft.peek().clone();
    let at = *shared.recall.peek();
    let (recall, text) = recall_step(&shared.history.peek(), at, delta, &current, &shared.typed.peek());
    if at.is_none() && recall.is_some() {
        shared.typed.set(current);
    }
    shared.recall.set(recall);
    shared.draft.set(text);
}

/// Puts the newest sent line back into the message field, as the up arrow in the field does.
fn recall_last(store: Store) {
    let mut shared = store.ui.chat;
    let newest = shared.history.peek().last().cloned();
    let recalling = shared.recall.peek().is_some();
    match newest.filter(|_| recalling) {
        Some(text) => {
            let last = shared.history.peek().len() - 1;
            shared.recall.set(Some(last));
            shared.draft.set(text);
        }
        None => step_history(store, -1),
    }
    shared.focus.set(Some(Instant::now()));
}

/// Picks the batch the page shows; a recipient the person picked stays.
fn set_batch(store: Store, batch: Option<String>) {
    let mut shared = store.ui.chat;
    if *shared.batch.peek() == batch {
        return;
    }
    shared.batch.set(batch);
    shared.shown.set(PAGE);
}

/// Opens the page with the message field focused.
pub fn focus(store: Store) {
    let mut ui = store.ui;
    if *ui.page.peek() != super::ui::Page::Chat {
        ui.page.set(super::ui::Page::Chat);
    }
    let mut at = ui.chat.focus;
    at.set(Some(Instant::now()));
}

fn can_send() -> bool {
    let s = settings::active();
    s.merged_chat && s.broadcast
}

/// The page's commands for the palette: pick who sends, or send to everyone again.
pub fn extras(store: Store) -> Vec<Extra> {
    if !can_send() {
        return Vec::new();
    }
    let mut out = vec![Extra { id: "chat.sendas".into(), label: tr("Senden als …"), glyph: Icon::Users, shortcut: Some(Shortcut::primary(Code::KeyE)), run: EventHandler::new(move |_| open_picker(store)) }];
    if !store.ui.chat.history.peek().is_empty() {
        out.push(Extra { id: "chat.recall".into(), label: tr("Letzte Nachricht zurückholen"), glyph: Icon::Message, shortcut: None, run: EventHandler::new(move |_| recall_last(store)) });
    }
    if store.ui.chat.target.peek().as_ref().is_some_and(|t| *t != Target::All) {
        out.push(Extra { id: "chat.toall".into(), label: tr("An alle senden"), glyph: Icon::Send, shortcut: None, run: EventHandler::new(move |_| pick(store, Target::All)) });
    }
    out
}

/// The page's control in the toolbar: the batch shown, when there are several.
#[derive(PartialEq)]
pub struct Tools {
    pub room: f32,
}

impl Component for Tools {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let shared = store.ui.chat;
        let m = store.model.read();
        if !settings::active().merged_chat || m.batches.len() < 2 {
            return rect().into_element();
        }
        let choices = batch_choices(&m);
        let chosen = choices.iter().position(|(_, id)| *id == *shared.batch.read()).unwrap_or(0);
        let ids: Vec<Option<String>> = choices.iter().map(|(_, id)| id.clone()).collect();
        let width = if self.room >= frame::TOOLBAR_SEARCH { PICKER_W } else { PICKER_NARROW };
        Select::new("chat.batch", tr("Batch"), choices.into_iter().map(|(l, _)| l).collect(), chosen).width(width).on_choose(move |i| set_batch(store, ids.get(i).cloned().flatten())).into_element()
    }
}

/// The Chat page.
#[derive(PartialEq)]
pub struct ChatPage;

impl Component for ChatPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let shared = store.ui.chat;
        let mark_at = use_hook(|| *shared.left_at.peek());
        use_drop(move || {
            let m = store.model.peek();
            let mut left = store.ui.chat.left_at;
            left.set(Some(m.chat_base() + m.chat.len() as u64));
        });
        let _ = store.chat.read();
        let m = store.model.read();
        let batch = shared.batch.read().clone().filter(|b| m.batches.len() > 1 && m.batch(b).is_some());
        let merged = settings::active().merged_chat;
        let broadcast = settings::active().broadcast;
        let page = rect().width(Size::fill()).height(Size::fill());
        if !m.loaded {
            return page.center().child(secondary(&t, tr("Lädt …"))).into_element();
        }
        if !merged {
            return page.child(empty(&t, Icon::Message, tr("Der gemeinsame Chat ist aus"), tr("Manycraft sammelt keine Chatzeilen. In den Einstellungen schaltest du ihn wieder ein."), Some(Button::new("chat.off.settings", tr("Einstellungen öffnen")).glyph(Icon::Settings).tall().on_press(move |_| Command::OpenSettings.run(store)).into_element()))).into_element();
        }
        let base = m.chat_base();
        let ids: Vec<u64> = m.chat.iter().enumerate().filter(|(_, l)| in_scope(l, batch.as_deref())).map(|(i, _)| base + i as u64).collect();
        let body = if ids.is_empty() {
            if batch.is_some() && !m.chat.is_empty() {
                empty(&t, Icon::Search, tr("Keine Treffer"), tr("In diesem Batch gibt es noch keine Chatzeilen."), Some(Button::new("chat.filter.reset", tr("Alle Batches zeigen")).tall().on_press(move |_| set_batch(store, None)).into_element()))
            } else {
                empty(&t, Icon::Message, tr("Noch redet keiner"), tr("Sobald ein Account auf einem Server ist, erscheint der Chat hier."), None)
            }
        } else {
            let shown = (*shared.shown.read()).min(ids.len());
            let older = ids.len() - shown;
            let window: Vec<ChatLine> = ids[older..].iter().filter_map(|id| m.chat.get((id - base) as usize).cloned()).collect();
            let names = if m.batches.len() > 1 && batch.is_none() { m.batches.iter().map(|b| (b.id.clone(), b.name.clone())).collect() } else { Vec::new() };
            let running = running_by_batch(&m);
            let own = own_names(&m);
            let rows: Vec<Line> = window
                .into_iter()
                .zip(ids[older..].iter().copied())
                .map(|(l, id)| {
                    let tag = names.iter().find(|(b, _)| *b == l.batch).map(|(_, n)| n.clone());
                    let mentions = marks(&l, &own);
                    let about_own = l.from.is_none() && !mentions.is_empty();
                    let heard = if about_own { None } else { heard_by(&m, &l, &running) };
                    let seen = 1 + l.also.len();
                    Line { line: l, id, batch: tag, heard, seen, mentions }
                })
                .collect();
            let mark = mark_at.and_then(|at| ids[older..].iter().skip(1).find(|id| **id >= at).copied());
            Lines { rows, older, mark }.into_element()
        };
        let mut main = rect().width(Size::fill()).height(Size::fill()).child(rect().width(Size::fill()).height(Size::flex(1.)).child(body)).content(Content::flex());
        if broadcast {
            main = main.child(Composer);
        } else {
            main = main.child(
                rect()
                    .width(Size::fill())
                    .padding((space::M, space::L))
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::M)
                    .border(Border::new().fill(t.separator).width(BorderWidth { top: 1., right: 0., bottom: 0., left: 0. }).alignment(BorderAlignment::Inner))
                    .child(line(&t, tr("Senden ist in den Einstellungen ausgeschaltet."), font::CALLOUT, t.text3).width(Size::flex(1.)))
                    .content(Content::flex())
                    .child(Button::new("chat.send.off", tr("Einstellungen öffnen")).glyph(Icon::Settings).on_press(move |_| Command::OpenSettings.run(store))),
            );
        }
        main.into_element()
    }
}

fn running_by_batch(m: &Model) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for g in m.games.values().filter(|g| g.phase.can_chat()) {
        *out.entry(g.batch_id.clone()).or_insert(0) += 1;
    }
    out
}

/// Which of the person's accounts got `l`, for a line only a few of those on its server got: their names
/// and the game to answer from.
fn heard_by(m: &Model, l: &ChatLine, running: &HashMap<String, usize>) -> Option<Heard> {
    let got = 1 + l.also.iter().filter(|(_, _, b)| *b == l.batch).count();
    let on = running.get(&l.batch).copied().unwrap_or(0);
    if got >= on || 1 + l.also.len() > HEARD_MAX {
        return None;
    }
    let names: Vec<&str> = std::iter::once(l.account.as_str()).chain(l.also.iter().map(|(_, n, _)| n.as_str())).collect();
    Some(Heard { names: names.join(", ") })
}

fn own_names(m: &Model) -> HashSet<String> {
    m.accounts.iter().map(|a| a.name.to_lowercase()).filter(|n| n.chars().count() >= MENTION_MIN).collect()
}

/// The text a line shows: the whole line as the game styled it, else its words.
fn shown(l: &ChatLine) -> Cow<'_, str> {
    if l.runs.is_empty() { Cow::Borrowed(l.text.as_str()) } else { Cow::Owned(chat_text::plain(&l.runs)) }
}

/// Where the person's account names stand in what `l` shows, leaving out the sender's own name.
fn marks(l: &ChatLine, own: &HashSet<String>) -> Vec<(usize, usize)> {
    let text = shown(l);
    let mut out = mentions(&text, own);
    if !l.runs.is_empty()
        && let Some(from) = l.from.as_deref()
        && let Some(i) = out.iter().position(|&(s, e)| text[s..e].eq_ignore_ascii_case(from))
    {
        out.remove(i);
    }
    out
}

/// Byte ranges of the person's account names in `text`, as whole words.
fn mentions(text: &str, own: &HashSet<String>) -> Vec<(usize, usize)> {
    if own.is_empty() {
        return Vec::new();
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (start, word(c)) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                if text.get(s..i).is_some_and(|w| own.contains(&w.to_lowercase())) {
                    out.push((s, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

fn empty(t: &Theme, glyph: Icon, title: String, text: String, action: Option<Element>) -> Element {
    let mut col = rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .spacing(space::M)
        .padding(space::XL)
        .child(icon(t, glyph, iconsize::L * 2., t.text3))
        .child(heading(t, title))
        .child(rect().max_width(Size::px(READING)).child(para(t, text, font::CALLOUT, t.text2).text_align(TextAlign::Center)));
    if let Some(a) = action {
        col = col.child(a);
    }
    col.into_element()
}

#[derive(Clone, Debug, PartialEq)]
struct Heard {
    names: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Line {
    line: ChatLine,
    id: u64,
    batch: Option<String>,
    heard: Option<Heard>,
    /// How many of the person's accounts got the line.
    seen: usize,
    mentions: Vec<(usize, usize)>,
}

/// The newest lines in scope, following the newest while the list is at its end.
#[derive(PartialEq)]
struct Lines {
    rows: Vec<Line>,
    older: usize,
    /// The first line that came while the page was away, marked as new.
    mark: Option<u64>,
}

impl Component for Lines {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut scroll = use_scroll_controller(|| ScrollConfig { default_vertical_position: ScrollPosition::End, ..Default::default() });
        let mut follow = use_state(|| true);
        let mut away = use_state(|| None::<u64>);
        let mut tall = use_state(|| (0f32, 0f32));
        let mut known = use_state(HashMap::<u64, f32>::new);
        let mut measured = use_state(|| 0f32);
        use_side_effect(move || {
            let Some((content, viewport)) = *scroll.bounds.read() else { return };
            let y = scroll.position().y;
            if (content.height, viewport.height) != *tall.peek() {
                tall.set((content.height, viewport.height));
                if *follow.peek() {
                    scroll.scroll_to(ScrollPosition::End, Direction::Vertical);
                }
                return;
            }
            let at_end = -y >= content.height - viewport.height - space::S;
            if at_end != *follow.peek() {
                follow.set(at_end);
                let newest = {
                    let m = store.model.peek();
                    (m.chat_base() + m.chat.len() as u64).checked_sub(1)
                };
                away.set(if at_end { None } else { newest });
            }
        });
        let at_end = *follow.read();
        let fresh = away.read().map_or(0, |from| self.rows.iter().filter(|r| r.id > from).count());
        let now = now();
        let mut entries = vec![Entry::Edge(space::S)];
        if self.older > 0 {
            entries.push(Entry::Older(self.older.min(PAGE)));
        }
        let mut before: Option<i64> = None;
        for r in &self.rows {
            let mut stamp = before.is_none_or(|b| b.div_euclid(60) != r.line.at.div_euclid(60));
            if before.is_none_or(|b| !same_day(b, r.line.at)) {
                entries.push(Entry::Day(r.id, day_title(r.line.at, now)));
                stamp = true;
            }
            if self.mark == Some(r.id) {
                entries.push(Entry::Fresh(r.id));
                stamp = true;
            }
            before = Some(r.line.at);
            entries.push(Entry::Line(r.clone(), stamp));
        }
        entries.push(Entry::Edge(space::M));
        let width = *measured.read();
        let sizes = entries.iter().map(|e| known.read().get(&e.key()).copied().unwrap_or_else(|| e.guess(width))).collect::<Vec<f32>>();
        let n = entries.len();
        let theme = t.clone();
        let shown = store.ui.chat.shown;
        let list = VirtualScrollView::new_with_data_controlled(
            entries,
            move |item: VirtualItem, entries: &Vec<Entry>| {
                let entry = &entries[item.index];
                let key = entry.key();
                rect().key(key).width(Size::fill()).padding((0., space::S)).on_sized(move |e: Event<SizedEventData>| remember_height(known, key, Bounds::of(&e).h)).child(entry.draw(&theme, shown)).into_element()
            },
            scroll,
        )
        .length(n)
        .item_size(move |i: usize| sizes.get(i).copied().unwrap_or(0.))
        .width(Size::fill())
        .height(Size::fill());
        let mut wrap = rect()
            .width(Size::fill())
            .height(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| {
                let w = Bounds::of(&e).w;
                if (w - *measured.peek()).abs() >= 1. {
                    measured.set(w);
                    known.write().clear();
                }
            })
            .child(list);
        if !at_end {
            let label = match fresh {
                0 => tr("Zu den neuesten"),
                1 => tr("1 neue Zeile"),
                n => trf("%lld neue Zeilen", &[&n]),
            };
            wrap = wrap.child(
                rect()
                    .position(Position::new_absolute().left(0.).bottom(space::M))
                    .width(Size::fill())
                    .main_align(Alignment::Center)
                    .horizontal()
                    .child(Button::new("chat.newest", label).glyph(Icon::ArrowDown).primary().on_press(move |_| {
                        scroll.scroll_to(ScrollPosition::End, Direction::Vertical);
                        follow.set(true);
                        away.set(None);
                    })),
            );
        }
        wrap
    }
}

/// One item of the virtual list: a chat line, a day or new-lines divider, the button for older lines, or room at an end.
#[derive(Clone, PartialEq)]
enum Entry {
    Edge(f32),
    Older(usize),
    Day(u64, String),
    Fresh(u64),
    Line(Line, bool),
}

impl Entry {
    fn key(&self) -> u64 {
        match self {
            Self::Edge(h) => u64::MAX - u64::from(*h > space::S),
            Self::Older(_) => u64::MAX - 2,
            Self::Day(id, _) => id * 4 + 1,
            Self::Fresh(id) => id * 4 + 2,
            Self::Line(l, _) => l.id * 4,
        }
    }

    /// The height before the item was laid out once: one line per row, more for long text in a narrow list.
    fn guess(&self, width: f32) -> f32 {
        match self {
            Self::Edge(h) => *h,
            Self::Older(_) => control::M + space::S * 2.,
            Self::Day(..) | Self::Fresh(_) => space::S * 2. + font::CAPTION * font::LINE,
            Self::Line(l, _) => {
                let text = (width - TIME_W - space::M * 3. - space::S * 2.).max(font::BODY * 8.);
                let per_line = (text / (font::BODY * GLYPH)).max(1.);
                let lines = (shown(&l.line).chars().count() as f32 / per_line).ceil().max(1.);
                (space::XXS + 1.) * 2. + lines * font::BODY * frame::PARA_LINE
            }
        }
    }

    fn draw(&self, t: &Theme, mut shown: State<usize>) -> Element {
        match self {
            Self::Edge(h) => rect().height(Size::px(*h)).into_element(),
            Self::Older(more) => rect()
                .width(Size::fill())
                .padding(space::S)
                .center()
                .child(Button::new("chat.older", if *more == 1 { tr("1 ältere Zeile zeigen") } else { trf("%lld ältere Zeilen zeigen", &[more]) }).plain().on_press(move |_| {
                    let now = *shown.peek();
                    shown.set(now + PAGE);
                }))
                .into_element(),
            Self::Day(_, title) => separator(t, title.clone(), t.text3, t.separator),
            Self::Fresh(_) => separator(t, tr("Neue Zeilen"), t.accent_text, t.accent.with_af32(0.5)),
            Self::Line(row, stamp) => Row { row: row.clone(), stamp: *stamp }.into_element(),
        }
    }
}

fn remember_height(mut known: State<HashMap<u64, f32>>, key: u64, h: f32) {
    if known.peek().get(&key).is_none_or(|k| (k - h).abs() >= 0.5) {
        known.write().insert(key, h);
    }
}

#[derive(PartialEq)]
struct Row {
    row: Line,
    /// Whether the line opens its minute and shows the time.
    stamp: bool,
}

impl Component for Row {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let r = &self.row;
        let l = &r.line;
        let base = if l.from.is_none() { t.text2 } else { t.text1 };
        let mut body = paragraph().width(Size::flex(1.)).font_size(font::BODY).color(base).line_height(frame::PARA_LINE);
        let plain;
        let runs = if l.runs.is_empty() {
            if let Some(from) = &l.from {
                body = body.span(Span::new(format!("{from}: ")).color(t.text2).font_weight(FontWeight::SEMI_BOLD));
            }
            plain = [Run { text: l.text.clone(), ..Run::default() }];
            &plain[..]
        } else {
            &l.runs[..]
        };
        body = body.spans_iter(spans(&t, runs, &r.mentions, base).into_iter());
        let text = shown(l);
        let summary = match &l.from {
            Some(_) if !l.runs.is_empty() => text.into_owned(),
            from => format!("{}: {text}", from.as_deref().unwrap_or(&l.account)),
        };
        let spot = use_spot(&format!("chat.line.{}", r.id), "chat-line", &summary, true);
        let store = use_store();
        let found = links(&l.text);
        let copied = summary.clone();
        let mut row = rect()
            .on_sized(spot)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(summary)
            .width(Size::fill())
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_secondary_down(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e {
                    let at = (m.global_location.x as f32, m.global_location.y as f32);
                    menu::open(store.ui, Menu::at(line_menu(store, &found, copied.clone()), at));
                }
            })
            .padding((space::XXS + 1., space::M))
            .horizontal()
            .spacing(space::M)
            .child(rect().width(Size::px(TIME_W)).padding((space::XXS, 0., 0., 0.)).maybe_child(self.stamp.then(|| caption(&t, clock(l.at)).font_tabular())))
            .child(body);
        let seen = (*hovered.read() && r.heard.is_none() && r.seen > 1).then(|| trf("Gesehen von %lld Accounts", &[&r.seen]));
        if r.heard.is_some() || r.batch.is_some() || seen.is_some() {
            let mut tags = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).max_width(Size::px(TAG_MAX * 2.));
            if let Some(b) = &r.batch {
                tags = tags.child(rect().max_width(Size::px(TAG_MAX)).child(caption(&t, b.clone())));
            }
            if let Some(h) = &r.heard {
                let tip = trf("Nur bei %@ angekommen.", &[&h.names]);
                let names = rect().max_width(Size::px(TAG_MAX)).a11y_alt(tip.clone()).child(caption(&t, h.names.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis));
                tags = tags.child(WithTip::new(Some(tip), names));
            }
            if let Some(tip) = seen {
                tags = tags.child(WithTip::new(Some(tip.clone()), rect().a11y_alt(tip).child(caption(&t, format!("{}×", r.seen)).font_tabular())));
            }
            row = row.child(tags);
        }
        row.content(Content::flex())
    }
}

/// The web addresses in `text`, in order and once each, without the punctuation that closes a sentence.
fn links(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = word.trim_start_matches(['(', '<', '[', '"', '\'']);
        if !(word.starts_with("https://") || word.starts_with("http://")) {
            continue;
        }
        let url = word.trim_end_matches(['.', ',', '!', '?', ':', ';', ')', '>', ']', '"', '\'']);
        if url.len() > "https://".len() && !found.iter().any(|f| f == url) {
            found.push(url.to_owned());
        }
    }
    found
}

fn line_menu(store: Store, links: &[String], text: String) -> Vec<Item> {
    let mut items: Vec<Item> = links
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let label = if links.len() == 1 { tr("Link öffnen") } else { trf("%@ öffnen", &[&clip_link(url)]) };
            let url = url.clone();
            Item::new(format!("chat.line.link.{i}"), label, move |_| open_external("open url", url.clone()))
        })
        .collect();
    if !items.is_empty() {
        items.push(Item::Separator);
    }
    items.push(Item::new("chat.line.copy", tr("Text kopieren"), move |_| {
        if !native::copy_text(text.clone()) {
            toast::show(store.ui, tr("Der Text wurde nicht kopiert."), None);
        }
    }));
    items
}

fn clip_link(url: &str) -> String {
    let bare = url.trim_start_matches("https://").trim_start_matches("http://");
    if bare.chars().count() > LINK_SHOWN { format!("{}…", bare.chars().take(LINK_SHOWN).collect::<String>()) } else { bare.to_owned() }
}

/// The spans for `runs` in their colours, each made readable on the page, with the person's names in `marks` in accent,
/// cut after `SHOWN_CHARS` characters.
fn spans(t: &Theme, runs: &[Run], marks: &[(usize, usize)], base: Color) -> Vec<Span<'static>> {
    let back = Rgba { r: t.surface.r(), g: t.surface.g(), b: t.surface.b(), a: 255 };
    let mut out = Vec::new();
    let mut left = SHOWN_CHARS;
    let mut start = 0;
    for run in runs {
        let end = start + run.text.len();
        let ink = run.color.map_or(base, |c| color(Rgba::rgb(c).readable_on(back, 4.5)));
        let mut cuts: Vec<usize> = marks.iter().flat_map(|&(s, e)| [s, e]).filter(|&b| b > start && b < end).map(|b| b - start).collect();
        cuts.push(run.text.len());
        let mut from = 0;
        for to in cuts {
            let Some(piece) = run.text.get(from..to).filter(|p| !p.is_empty()) else { continue };
            let marked = marks.iter().any(|&(s, e)| s <= start + from && start + to <= e);
            let n = piece.chars().count();
            let cut = n > left;
            let mut text: String = if cut { piece.chars().take(left).collect() } else { piece.to_owned() };
            if cut {
                text.push('…');
            }
            left = left.saturating_sub(n);
            let mut span = Span::new(text).color(if marked { t.accent_text } else { ink });
            if run.bold {
                span = span.font_weight(FontWeight::BOLD);
            } else if marked {
                span = span.font_weight(FontWeight::SEMI_BOLD);
            }
            if run.italic {
                span = span.font_slant(FontSlant::Italic);
            }
            match (run.strike, run.underline) {
                (true, true) => span = span.text_decoration(TextDecoration::UnderlineLineThrough),
                (true, false) => span = span.text_decoration(TextDecoration::LineThrough),
                (false, true) => span = span.text_decoration(TextDecoration::Underline),
                (false, false) => {}
            }
            out.push(span);
            if cut {
                return out;
            }
            from = to;
        }
        start = end;
    }
    out
}

fn separator(t: &Theme, text: String, ink: Color, rule: Color) -> Element {
    let rule = move || rect().width(Size::flex(1.)).height(Size::px(1.)).background(rule);
    rect().width(Size::fill()).padding((space::S, space::M)).horizontal().cross_align(Alignment::Center).spacing(space::S).child(rule()).child(line(t, text, font::CAPTION, ink).font_weight(FontWeight::MEDIUM)).child(rule()).content(Content::flex()).into_element()
}

/// The control that says who gets the next line; a click or Ctrl+E opens the list.
#[derive(PartialEq)]
struct SendAs;

impl Component for SendAs {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut shared = store.ui.chat;
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let m = store.model.read();
        let target = target_of(&m, shared);
        let reach = target.as_ref().map_or(0, |tg| reached(&m, tg));
        let label = target.as_ref().map_or_else(|| tr("Absender wählen"), |tg| name(&m, tg));
        let account = match &target {
            Some(Target::Instance(key)) => m.games.get(key).map(|g| g.account_id),
            _ => None,
        };
        let count = match &target {
            Some(Target::All | Target::Batch(_)) => Some(reach.to_string()),
            Some(Target::Instance(key)) => m.games.get(key).and_then(|g| target::state(&g.phase)),
            None => None,
        };
        let summary = match &count {
            Some(c) => format!("{}: {label} · {c}", tr("Senden als")),
            None => format!("{}: {label}", tr("Senden als")),
        };
        let mut spot = use_spot("chat.sendas", "button", &summary, true);
        let open = *shared.picker.read();
        let fill = if open { t.selection } else if *hovered.read() { t.hover } else { t.control };
        let ink = if target.is_none() { t.text3 } else { t.text1 };
        let mut chip = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(summary)
            .on_sized(move |e: Event<SizedEventData>| {
                shared.chip_at.set_if_modified(Bounds::of(&e));
                spot(e);
            })
            .height(Size::px(control::M))
            .max_width(Size::px(CHIP_MAX))
            .padding((0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS + 2.)
            .corner_radius(t.shape.field)
            .background(fill)
            .cursor(CursorIcon::Pointer)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_press(move |_| if *store.ui.chat.picker.peek() { shared.picker.set(false) } else { open_picker(store) })
            .on_key_down(move |e: Event<KeyboardEventData>| {
                let opens = matches!(&e.key, Key::Named(NamedKey::Enter | NamedKey::ArrowUp | NamedKey::ArrowDown)) || matches!(&e.key, Key::Character(c) if c == " ");
                if a11y.is_focused() && opens {
                    e.stop_propagation();
                    open_picker(store);
                }
            });
        if let Some(tg) = &target {
            chip = chip.child(mark(&t, store, tg, account, &label, t.text2));
        }
        chip = chip.child(line(&t, label, font::BODY, ink).font_weight(FontWeight::MEDIUM));
        if let Some(c) = count {
            chip = chip.child(line(&t, c, font::CALLOUT, t.text2).font_tabular());
        }
        chip = chip.child(icon(&t, Icon::ChevronUp, iconsize::XS, t.text3));
        if let Some(border) = ring(&t, focus()) {
            chip = chip.border(border);
        }
        let tip = Shortcut::primary(Code::KeyE).label();
        WithTip::new(Some(trf("Wer sendet · %@ oder @Name", &[&tip])), chip)
    }
}

/// The line under the list: who gets the message, the message, and what happened to the last one.
#[derive(PartialEq)]
struct Composer;

impl Component for Composer {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut shared = store.ui.chat;
        let mut lift = store.ui.toast_lift;
        use_drop(move || lift.set(0.));
        let field = use_a11y();
        let focus_at = *shared.focus.read();
        use_side_effect_with_deps(&focus_at, move |at| {
            if at.is_some() {
                field.request_focus();
                let mut asked = store.ui.chat.focus;
                asked.set(None);
            }
        });
        let draft = shared.draft.read().clone();
        use_side_effect_with_deps(&draft, move |value| {
            let mut shared = store.ui.chat;
            if shared.recall.peek().is_some_and(|at| shared.history.peek().get(at) != Some(value)) {
                shared.recall.set(None);
            }
            if shared.rejected.peek().as_ref().is_some_and(|(text, _)| value.trim() != text) {
                shared.rejected.set(None);
            }
            if !value.is_empty() && shared.sent.peek().is_some() {
                shared.sent.set(None);
            }
            shared.at_hot.set_if_modified(None);
            if shared.at_closed.peek().as_ref().is_some_and(|c| c != value) && at_query(value).is_none() {
                shared.at_closed.set(None);
            }
            let all = choices(&store.model.peek(), &shared.recent.peek());
            if let Some((target, rest)) = resolve_at(value, &all) {
                shared.target.set(Some(target));
                shared.draft.set(rest);
            }
        });
        let m = store.model.read();
        let target = target_of(&m, shared);
        let reach = target.as_ref().map_or(0, |tg| reached(&m, tg));
        let all = choices(&m, &shared.recent.read());
        let suggest = at_query(&draft).filter(|_| shared.at_closed.read().as_deref() != Some(draft.as_str())).map(|q| (q.is_empty(), filter(&all, q)));
        let absent = match &target {
            None => Some(tr("Der gewählte Absender ist weg. Wähle, wer sendet.")),
            Some(tg @ Target::Instance(_)) if reach == 0 => Some(trf("%@ ist gerade nicht im Spiel.", &[&name(&m, tg)])),
            Some(tg) if reach == 0 => Some(trf("%@: gerade ist niemand im Spiel.", &[&name(&m, tg)])),
            Some(_) => None,
        };
        let hint = shared.sent.read().clone().or_else(|| absent.clone());
        let joins: Vec<String> = match &target {
            Some(Target::Batch(id)) => m.batch(id).map(|b| b.join_lines()).unwrap_or_default(),
            _ => Vec::new(),
        };
        let blocked = if absent.is_some() {
            absent
        } else if draft.trim().is_empty() {
            Some(tr("Schreib zuerst eine Nachricht."))
        } else {
            None
        };
        let can_send = blocked.is_none();
        let mut send_button = Button::new("chat.send", tr("Senden")).glyph(Icon::Send).disabled(blocked);
        if can_send {
            send_button = send_button.primary();
        }
        let submit_list = suggest.as_ref().map(|(_, l)| l.clone());
        let row = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(SendAs)
            .child(rect().width(Size::flex(1.)).on_sized(move |e: Event<SizedEventData>| shared.field_at.set_if_modified(Bounds::of(&e))).child(Field::new("chat.message", shared.draft).placeholder(tr("Nachricht oder /Befehl · @Name wechselt den Absender")).a11y(field).rows_through().on_submit(move |_| {
                if let Some(list) = submit_list.as_ref() {
                    if let Some(at) = list.len().checked_sub(1).map(|end| store.ui.chat.at_hot.peek().unwrap_or(0).min(end)) {
                        pick_at(store, list[at].target.clone());
                    }
                    return;
                }
                send(store);
            })))
            .content(Content::flex())
            .child(send_button.on_press(move |_| send(store)));
        let note = match shared.rejected.read().clone() {
            Some((_, why)) => Some(line(&t, why, font::CAPTION, t.danger_text)),
            None => hint.map(|h| caption(&t, h).max_lines(1).text_overflow(TextOverflow::Ellipsis)),
        };
        let note = rect().width(Size::fill()).height(Size::px((font::CAPTION * font::LINE).ceil())).maybe_child(note);
        let keys_list = suggest.as_ref().map(|(_, l)| l.clone());
        let mut col = rect()
            .width(Size::fill())
            .padding((space::M, space::L, space::S, space::L))
            .spacing(space::XS)
            .border(Border::new().fill(t.separator).width(BorderWidth { top: 1., right: 0., bottom: 0., left: 0. }).alignment(BorderAlignment::Inner))
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if *Platform::get().current_window().focused_accessibility_id.peek() != field {
                    return;
                }
                if Shortcut::primary(Code::KeyE).matches(&e) {
                    e.stop_propagation();
                    open_picker(store);
                    return;
                }
                if !e.modifiers.is_empty() {
                    return;
                }
                if let Some(list) = &keys_list {
                    let mut shared = store.ui.chat;
                    if matches!(e.key, Key::Named(NamedKey::Enter)) {
                        return;
                    }
                    let hot = shared.at_hot.peek().or(Some(0));
                    match step(&e.key, list.len(), hot) {
                        Some(Step::Move(i)) => shared.at_hot.set(Some(i)),
                        Some(Step::Pick(i)) => pick_at(store, list[i].target.clone()),
                        Some(Step::Close) => {
                            let draft = shared.draft.peek().clone();
                            shared.at_closed.set(Some(draft));
                        }
                        None => return,
                    }
                    e.stop_propagation();
                    e.prevent_default();
                    return;
                }
                let delta = match e.key {
                    Key::Named(NamedKey::ArrowUp) => -1,
                    Key::Named(NamedKey::ArrowDown) => 1,
                    _ => return,
                };
                e.stop_propagation();
                e.prevent_default();
                step_history(store, delta);
            })
            .on_sized(move |e: Event<SizedEventData>| lift.set_if_modified(Bounds::of(&e).h))
            .child(row)
            .child(note);
        if !joins.is_empty() {
            let names: Vec<&str> = joins.iter().map(|j| j.split_whitespace().next().unwrap_or_default()).collect();
            col = col.child(caption(&t, trf("Beim Betreten sendet jeder Account: %@", &[&names.join(", ")])).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        }
        if *shared.picker.read() {
            col = col.child(Popup { anchor: *shared.chip_at.read(), current: target.clone() });
        } else if let Some((sections, list)) = suggest {
            col = col.child(Suggestions { anchor: *shared.field_at.read(), list, sections, hot: *shared.at_hot.read(), current: target.clone() });
        }
        col
    }
}

/// Picks the recipient an `@name` pointed at and takes the name out of the message.
fn pick_at(store: Store, target: Target) {
    let mut shared = store.ui.chat;
    shared.draft.set(String::new());
    shared.at_hot.set(None);
    pick(store, target);
}

/// The recipient pop-up over the composer: a filter and the list.
#[derive(PartialEq)]
struct Popup {
    anchor: Bounds,
    current: Option<Target>,
}

impl Component for Popup {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut shared = store.ui.chat;
        let (w, h) = *store.ui.window.read();
        let m = store.model.read();
        let q = shared.query.read().clone();
        use_side_effect_with_deps(&q, move |_| shared.hot.set_if_modified(None));
        let list = filter(&choices(&m, &shared.recent.read()), &q);
        let sections = q.trim().is_empty();
        let hot = (*shared.hot.read()).or_else(|| (!sections && !list.is_empty()).then_some(0)).or_else(|| self.current.as_ref().and_then(|c| list.iter().position(|x| x.target == *c)));
        let list_h = List::height(&list, sections, picker::ROW * POPUP_ROWS).max(picker::ROW);
        let panel_h = control::M + space::XS + list_h + space::XS * 2.;
        let width = POPUP_W.min(w - space::S * 2.);
        let x = self.anchor.x.min(w - width - space::S).max(space::S);
        let y = (self.anchor.y - panel_h - space::XS).max(space::S);
        let keys = list.clone();
        let mut panel = rect()
            .position(Position::new_absolute().left(x).top(y))
            .a11y_role(AccessibilityRole::Dialog)
            .a11y_alt(tr("Senden als"))
            .width(Size::px(width))
            .padding(space::XS)
            .spacing(space::XS)
            .background(t.raised)
            .corner_radius(t.shape.control + 2.)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Outer))
            .shadow(t.elevation.float.clone())
            .on_mouse_down(|e: Event<MouseEventData>| e.stop_propagation())
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                let mut shared = store.ui.chat;
                match step(&e.key, keys.len(), hot) {
                    Some(Step::Move(i)) => shared.hot.set(Some(i)),
                    Some(Step::Pick(i)) => pick(store, keys[i].target.clone()),
                    Some(Step::Close) => {
                        shared.picker.set(false);
                        shared.focus.set(Some(Instant::now()));
                    }
                    None => return,
                }
                e.stop_propagation();
                e.prevent_default();
            })
            .child(Field::new("chat.sendas.search", shared.query).placeholder(tr("Account oder Batch suchen")).glyph(Icon::Search).auto_focus().rows_through());
        panel = if list.is_empty() {
            panel.child(rect().width(Size::fill()).height(Size::px(list_h)).center().child(line(&t, tr("Nichts gefunden"), font::CALLOUT, t.text3)))
        } else {
            panel.child(List {
                id: "chat.sendas.option",
                choices: list,
                sections,
                hot,
                current: self.current.clone(),
                max_h: Some(list_h),
                on_pick: EventHandler::new(move |tg| pick(store, tg)),
                on_hover: EventHandler::new(move |i| shared.hot.set(Some(i))),
            })
        };
        rect()
            .position(Position::new_global().left(0.).top(0.))
            .layer(Layer::OverlayLevel(level::MENU))
            .width(Size::px(w))
            .height(Size::px(h))
            .on_mouse_down(move |_| {
                shared.picker.set(false);
                shared.focus.set(Some(Instant::now()));
            })
            .child(panel)
    }
}

/// The recipients that fit an `@name` being typed, over the message field.
#[derive(PartialEq)]
struct Suggestions {
    anchor: Bounds,
    list: Vec<Choice>,
    sections: bool,
    hot: Option<usize>,
    current: Option<Target>,
}

impl Component for Suggestions {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut shared = store.ui.chat;
        let (w, _) = *store.ui.window.read();
        let list_h = List::height(&self.list, self.sections, picker::ROW * POPUP_ROWS).max(picker::ROW);
        let panel_h = list_h + space::XS * 2. + font::CAPTION * 1.4 + space::XS;
        let width = POPUP_W.min(self.anchor.w).max(POPUP_W.min(w - space::S * 2.));
        let x = self.anchor.x.min(w - width - space::S).max(space::S);
        let y = (self.anchor.y - panel_h - space::XS).max(space::S);
        let hot = self.hot.or_else(|| (!self.list.is_empty()).then_some(0));
        let mut panel = rect()
            .position(Position::new_global().left(x).top(y))
            .layer(Layer::OverlayLevel(level::MENU))
            .a11y_role(AccessibilityRole::ListBox)
            .a11y_alt(tr("Senden als"))
            .width(Size::px(width))
            .padding(space::XS)
            .spacing(space::XS)
            .background(t.raised)
            .corner_radius(t.shape.control + 2.)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Outer))
            .shadow(t.elevation.float.clone())
            .child(rect().padding((0., space::S)).child(caption(&t, tr("Senden als · Enter wählt · Esc schließt"))));
        panel = if self.list.is_empty() {
            panel.child(rect().width(Size::fill()).height(Size::px(list_h)).center().child(line(&t, tr("Nichts gefunden"), font::CALLOUT, t.text3)))
        } else {
            panel.child(List {
                id: "chat.at.option",
                choices: self.list.clone(),
                sections: self.sections,
                hot,
                current: self.current.clone(),
                max_h: Some(list_h),
                on_pick: EventHandler::new(move |tg| pick_at(store, tg)),
                on_hover: EventHandler::new(move |i| shared.at_hot.set(Some(i))),
            })
        };
        panel
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn local(at: i64) -> Option<chrono::DateTime<chrono::Local>> {
    chrono::DateTime::from_timestamp(at, 0).map(|d| d.with_timezone(&chrono::Local))
}

fn clock(at: i64) -> String {
    local(at).map_or_else(String::new, |d| d.format("%H:%M").to_string())
}

fn same_day(a: i64, b: i64) -> bool {
    local(a).map(|d| d.date_naive()) == local(b).map(|d| d.date_naive())
}

fn day_title(at: i64, now: i64) -> String {
    let Some(day) = local(at).map(|d| d.date_naive()) else { return String::new() };
    let today = local(now).map(|d| d.date_naive());
    if Some(day) == today {
        tr("Heute")
    } else if today.and_then(|t| t.pred_opt()) == Some(day) {
        tr("Gestern")
    } else {
        day.format("%d.%m.%Y").to_string()
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= SHOWN_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(SHOWN_CHARS).collect();
    cut.push('…');
    cut
}

fn in_scope(line: &ChatLine, batch: Option<&str>) -> bool {
    batch.is_none_or(|b| line.batch == b || line.also.iter().any(|(_, _, other)| other == b))
}

fn line_problem(text: &str) -> Option<String> {
    let n = text.chars().count();
    (n > MAX_CHAT_CHARS).then(|| trf("Zu lang: %lld von höchstens %lld Zeichen.", &[&n, &MAX_CHAT_CHARS]))
}

/// `labels` with later repeats numbered, so every one picks exactly one thing.
fn unique(labels: Vec<String>) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut taken: HashSet<String> = labels.iter().cloned().collect();
    labels
        .into_iter()
        .map(|l| {
            let n = seen.entry(l.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                return l;
            }
            let mut k = *n;
            loop {
                let candidate = format!("{l} ({k})");
                if taken.insert(candidate.clone()) {
                    return candidate;
                }
                k += 1;
            }
        })
        .collect()
}

/// Where the arrow keys move in the history of sent lines: the new position and the text to show.
/// Up from the field goes to the newest entry, down past it brings back what was being typed.
fn recall_step(history: &[String], at: Option<usize>, delta: isize, current: &str, draft: &str) -> (Option<usize>, String) {
    if history.is_empty() {
        return (None, current.to_owned());
    }
    match (at, delta < 0) {
        (None, true) => (Some(history.len() - 1), history[history.len() - 1].clone()),
        (None, false) => (None, current.to_owned()),
        (Some(at), true) => {
            let at = at.saturating_sub(1);
            (Some(at), history[at].clone())
        }
        (Some(at), false) if at + 1 < history.len() => (Some(at + 1), history[at + 1].clone()),
        (Some(_), false) => (None, draft.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{ChatLine, MAX_CHAT_CHARS, Model, SHOWN_CHARS, clip, clip_link, clock, day_title, heard_by, in_scope, line_problem, links, mentions, now, recall_step, same_day, unique};

    fn line(batch: &str) -> ChatLine {
        ChatLine { instance: "k".into(), account: "Alex".into(), batch: batch.into(), also: Vec::new(), at: 0, from: None, text: "x".into(), runs: Vec::new() }
    }

    #[test]
    fn links_are_found_once_without_closing_punctuation() {
        assert_eq!(links("see https://example.org/a, and (http://x.io/b). again https://example.org/a"), ["https://example.org/a", "http://x.io/b"]);
        assert!(links("no link here, https:// alone or ftp://x").is_empty());
        assert_eq!(clip_link("https://example.org/"), "example.org/");
        assert!(clip_link(&format!("https://example.org/{}", "a".repeat(80))).ends_with('…'));
    }

    #[test]
    fn scope_filters_by_batch() {
        assert!(in_scope(&line("a"), None));
        assert!(in_scope(&line("farm"), Some("farm")) && !in_scope(&line("a"), Some("farm")));
        let mut shared = line("a");
        shared.also.push(("k".into(), "n".into(), "farm".into()));
        assert!(in_scope(&shared, Some("farm")));
    }

    #[test]
    fn lines_only_some_accounts_saw_name_them() {
        let m = Model::default();
        let running: HashMap<String, usize> = [("farm".to_owned(), 3)].into();
        let mut l = line("farm");
        assert_eq!(heard_by(&m, &l, &running).map(|h| h.names), Some("Alex".into()));
        l.also.push(("k2".into(), "Bert".into(), "farm".into()));
        assert_eq!(heard_by(&m, &l, &running).map(|h| h.names), Some("Alex, Bert".into()));
        l.also.push(("k3".into(), "Cara".into(), "farm".into()));
        assert_eq!(heard_by(&m, &l, &running), None);
        let crowd: HashMap<String, usize> = [("farm".to_owned(), 30)].into();
        assert_eq!(heard_by(&m, &l, &crowd).map(|h| h.names), Some("Alex, Bert, Cara".into()));
        l.also.push(("k4".into(), "Dora".into(), "farm".into()));
        assert_eq!(heard_by(&m, &l, &crowd), None);
        let alone: HashMap<String, usize> = [("farm".to_owned(), 1)].into();
        assert_eq!(heard_by(&m, &line("farm"), &alone), None);
    }

    #[test]
    fn own_names_are_found_as_whole_words() {
        let own: HashSet<String> = ["alex".to_owned(), "bert_2".to_owned()].into();
        assert_eq!(mentions("hi Alex, und alexander", &own), [(3, 7)]);
        assert_eq!(mentions("bert_2!", &own), [(0, 6)]);
        assert_eq!(mentions("Grüße alex", &own), [(8, 12)]);
        assert!(mentions("nothing", &HashSet::new()).is_empty());
    }

    #[test]
    fn lines_know_their_day_and_clock() {
        let at = now();
        assert!(same_day(at, at));
        assert!(!same_day(at, at - 3 * 24 * 3600));
        assert_eq!(clock(at).len(), 5);
        assert_eq!(day_title(at, at), crate::core::i18n::tr("Heute"));
        assert_eq!(day_title(at - 24 * 3600, at), crate::core::i18n::tr("Gestern"));
        assert_ne!(day_title(at - 5 * 24 * 3600, at), crate::core::i18n::tr("Heute"));
    }

    #[test]
    fn repeated_labels_get_numbers() {
        let labels = unique(vec!["Farm".into(), "Alex".into(), "Farm".into(), "Farm".into()]);
        assert_eq!(labels, ["Farm", "Alex", "Farm (2)", "Farm (3)"]);
        assert_eq!(unique(vec!["A".into(), "A".into(), "A (2)".into()]), ["A", "A (3)", "A (2)"]);
        let many = unique(vec!["Farm".into(), "Farm".into(), "Farm".into(), "Farm (2)".into()]);
        let distinct: std::collections::HashSet<_> = many.iter().collect();
        assert_eq!(distinct.len(), many.len());
    }

    #[test]
    fn arrow_up_walks_back_through_sent_lines_and_down_returns_the_draft() {
        let history = vec!["one".to_owned(), "two".to_owned()];
        assert_eq!(recall_step(&history, None, -1, "typing", ""), (Some(1), "two".into()));
        assert_eq!(recall_step(&history, Some(1), -1, "two", "typing"), (Some(0), "one".into()));
        assert_eq!(recall_step(&history, Some(0), -1, "one", "typing"), (Some(0), "one".into()));
        assert_eq!(recall_step(&history, Some(0), 1, "one", "typing"), (Some(1), "two".into()));
        assert_eq!(recall_step(&history, Some(1), 1, "two", "typing"), (None, "typing".into()));
        assert_eq!(recall_step(&history, None, 1, "typing", ""), (None, "typing".into()));
        assert_eq!(recall_step(&[], None, -1, "typing", ""), (None, "typing".into()));
    }

    #[test]
    fn long_lines_are_refused_and_long_text_is_cut_for_display() {
        assert!(line_problem(&"a".repeat(MAX_CHAT_CHARS)).is_none());
        assert!(line_problem(&"ä".repeat(MAX_CHAT_CHARS + 1)).is_some());
        let shown = clip(&"b".repeat(SHOWN_CHARS + 50));
        assert_eq!(shown.chars().count(), SHOWN_CHARS + 1);
        assert_eq!(clip("kurz"), "kurz");
    }
}
