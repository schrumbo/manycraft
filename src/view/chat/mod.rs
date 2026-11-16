//! The Chat page: the chat of every game in one list, whispers marked, and a line that goes to one game or a whole
//! batch.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use freya::prelude::*;

use super::commands::Command;
use super::icons::{Icon, icon};
use super::kit::button::Button;
use super::kit::field::Field;
use super::kit::segmented::{Segment, Segmented};
use super::kit::select::Select;
use super::kit::text::{caption, heading, line, para};
use super::kit::tooltip::WithTip;
use super::kit::use_theme;
use super::drive::use_spot;
use super::store::{Store, use_store};
use super::theme::{Theme, control, font, iconsize, space};
use crate::app::event::Event as AppEvent;
use crate::app::model::{ChatLine, Model};
use crate::core::batch::MAX_CHAT_CHARS;
use crate::core::i18n::{tr, trf};
use crate::core::settings;

const KEPT_HISTORY: usize = 100;
const SHOWN_CHARS: usize = 600;
const PAGE: usize = 200;
const TIME_W: f32 = 44.;
const LABEL_W: f32 = 148.;
const PICKER_W: f32 = 200.;
const TARGET_W: f32 = 240.;
const BAR: f32 = 2.;
const WASH: f32 = 0.1;
const READING: f32 = control::READING;

/// Who a line goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Batch(String),
    Instance(String),
}

/// The page's state, kept while other pages show so the draft and the history survive.
#[derive(Clone, Copy, PartialEq)]
pub struct Shared {
    pub whispers: State<bool>,
    pub batch: State<Option<String>>,
    pub target: State<Option<Target>>,
    /// Whether the person picked the recipient, so changing the batch shown leaves it alone.
    pub manual: State<bool>,
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
}

impl Shared {
    /// Empty state: everything shown, nobody picked.
    pub fn create() -> Self {
        Self {
            whispers: State::create(false),
            batch: State::create(None),
            target: State::create(None),
            manual: State::create(false),
            draft: State::create(String::new()),
            history: State::create(Vec::new()),
            recall: State::create(None),
            typed: State::create(String::new()),
            sent: State::create(None),
            rejected: State::create(None),
            shown: State::create(PAGE),
            focus: State::create(None),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Scope {
    batch: Option<String>,
    whispers: bool,
}

fn scope(shared: Shared) -> Scope {
    Scope { batch: shared.batch.read().clone(), whispers: *shared.whispers.read() }
}

/// The batch picker's entries: all batches, then each by name.
fn batch_choices(m: &Model) -> Vec<(String, Option<String>)> {
    let mut out = vec![(tr("Alle Batches"), None)];
    let names = unique(m.batches.iter().map(|b| b.name.clone()).collect());
    out.extend(m.batches.iter().zip(names).map(|(b, name)| (name, Some(b.id.clone()))));
    out
}

/// The recipient picker's entries: every batch with how many of it run, then every game, running ones first.
fn target_choices(m: &Model) -> Vec<(String, Target)> {
    let labels = unique(
        m.batches
            .iter()
            .map(|b| {
                let total = m.games.values().filter(|i| i.batch_id == b.id).count();
                let running = m.games.values().filter(|i| i.batch_id == b.id && i.phase.on_server()).count();
                trf("Alle in %@ · %lld von %lld laufen", &[&b.name, &running, &total])
            })
            .collect(),
    );
    let mut out: Vec<(String, Target)> = m.batches.iter().zip(labels).map(|(b, label)| (label, Target::Batch(b.id.clone()))).collect();
    let mut games: Vec<_> = m.games.values().collect();
    games.sort_by_cached_key(|i| (!i.phase.on_server(), i.name.to_lowercase(), i.key.clone()));
    let labels = unique(
        games
            .iter()
            .map(|i| {
                let label = match m.batch(&i.batch_id) {
                    Some(b) => format!("{} · {}", i.name, b.name),
                    None => i.name.clone(),
                };
                if i.phase.on_server() { label } else { format!("{label} · {}", tr("aus")) }
            })
            .collect(),
    );
    out.extend(games.iter().zip(labels).map(|(i, label)| (label, Target::Instance(i.key.clone()))));
    out
}

/// The recipient in use: the one picked while it still exists, else the batch shown, else the only batch.
fn target_of(m: &Model, shared: Shared) -> Option<Target> {
    let valid = |t: &Target| match t {
        Target::Batch(id) => m.batch(id).is_some(),
        Target::Instance(key) => m.games.contains_key(key),
    };
    if let Some(t) = shared.target.read().clone().filter(valid) {
        return Some(t);
    }
    if *shared.manual.read() && shared.target.read().is_some() {
        return None;
    }
    shared.batch.read().clone().filter(|b| m.batch(b).is_some()).or_else(|| (m.batches.len() == 1).then(|| m.batches[0].id.clone())).map(Target::Batch)
}

fn reached(m: &Model, target: Option<&Target>) -> usize {
    match target {
        Some(Target::Batch(id)) => m.games.values().filter(|i| i.batch_id == *id && i.phase.on_server()).count(),
        Some(Target::Instance(key)) => usize::from(m.games.get(key).is_some_and(|i| i.phase.on_server())),
        None => 0,
    }
}

fn send(store: Store) {
    let mut shared = store.ui.chat;
    let text = shared.draft.peek().trim().to_owned();
    if text.is_empty() || !settings::active().broadcast {
        return;
    }
    let (target, reach) = {
        let m = store.model.peek();
        let target = target_of(&m, shared);
        let reach = reached(&m, target.as_ref());
        (target, reach)
    };
    let problem = line_problem(&text).or_else(|| target.is_none().then(|| tr("Wähle, wer die Nachricht bekommt."))).or_else(|| (reach == 0).then(|| tr("Dort läuft gerade keine Instanz.")));
    if let Some(problem) = problem {
        shared.rejected.set(Some((text, problem)));
        shared.sent.set(None);
        return;
    }
    let Some(target) = target else { return };
    match target {
        Target::Batch(batch) => store.dispatch(AppEvent::Broadcast { batch, text: text.clone() }),
        Target::Instance(key) => store.dispatch(AppEvent::SendChat { key, text: text.clone() }),
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
    shared.sent.set(Some(if reach == 1 { tr("An 1 Instanz gesendet.") } else { trf("An %lld Instanzen gesendet.", &[&reach]) }));
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

/// Picks the batch the page shows; the recipient follows unless the person picked one.
fn set_batch(store: Store, batch: Option<String>) {
    let mut shared = store.ui.chat;
    if *shared.batch.peek() == batch {
        return;
    }
    shared.batch.set(batch.clone());
    shared.shown.set(PAGE);
    if !*shared.manual.peek() {
        shared.target.set(batch.map(Target::Batch));
        shared.sent.set(None);
        shared.rejected.set(None);
    }
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

/// The page's controls in the toolbar: everything or only whispers, and the batch shown.
#[derive(PartialEq)]
pub struct Tools {
    pub room: f32,
}

impl Component for Tools {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let shared = store.ui.chat;
        let mut whispers = shared.whispers;
        if !settings::active().merged_chat {
            return rect().into_element();
        }
        let m = store.model.read();
        let choices = batch_choices(&m);
        let chosen = choices.iter().position(|(_, id)| *id == *shared.batch.read()).unwrap_or(0);
        let items = vec![Segment { id: "chat.scope.all".into(), label: tr("Alles"), tip: None }, Segment { id: "chat.scope.whispers".into(), label: tr("Nur Flüstern"), tip: Some(tr("Nur Nachrichten, die jemand deinen Accounts zuflüstert")) }];
        let mut row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S).child(Segmented::new(tr("Zeigen"), items, usize::from(*whispers.read())).on_choose(move |i| whispers.set(i == 1)));
        if m.batches.len() > 1 && self.room >= super::theme::frame::TOOLBAR_SEARCH {
            let ids: Vec<Option<String>> = choices.iter().map(|(_, id)| id.clone()).collect();
            row = row.child(Select::new("chat.batch", tr("Batch"), choices.into_iter().map(|(l, _)| l).collect(), chosen).width(PICKER_W).on_choose(move |i| set_batch(store, ids.get(i).cloned().flatten())));
        }
        row.into_element()
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
        let m = store.model.read();
        let mut s = scope(shared);
        s.batch = s.batch.filter(|b| m.batches.len() > 1 && m.batch(b).is_some());
        let unread = m.games.values().any(|i| i.unread_whispers > 0 && s.batch.as_deref().is_none_or(|b| i.batch_id == b));
        use_side_effect_with_deps(&(unread, s.batch.clone()), move |(unread, batch)| {
            if *unread {
                store.dispatch(AppEvent::ClearWhispers(batch.clone()));
            }
        });
        let merged = settings::active().merged_chat;
        let broadcast = settings::active().broadcast;
        let mut page = rect().width(Size::fill()).height(Size::fill());
        if !merged {
            return page.child(empty(&t, Icon::Message, tr("Der gemeinsame Chat ist aus"), tr("Manycraft sammelt keine Chatzeilen. In den Einstellungen schaltest du ihn wieder ein."), Some(Button::new("chat.off.settings", tr("Einstellungen öffnen")).glyph(Icon::Settings).tall().on_press(move |_| Command::OpenSettings.run(store)).into_element()))).into_element();
        }
        let base = m.chat_base();
        let ids: Vec<u64> = m.chat.iter().enumerate().filter(|(_, l)| in_scope(l, &s)).map(|(i, _)| base + i as u64).collect();
        let body = if ids.is_empty() {
            let mut whispers = shared.whispers;
            if s.whispers {
                empty(&t, Icon::Message, tr("Niemand hat geflüstert"), tr("Flüsternachrichten an deine Accounts erscheinen hier."), Some(Button::new("chat.show.all", tr("Alles zeigen")).tall().on_press(move |_| whispers.set(false)).into_element()))
            } else if s.batch.is_some() && !m.chat.is_empty() {
                empty(&t, Icon::Search, tr("Keine Treffer"), tr("In diesem Batch gibt es noch keine Chatzeilen."), Some(Button::new("chat.filter.reset", tr("Filter zurücksetzen")).tall().on_press(move |_| set_batch(store, None)).into_element()))
            } else {
                empty(&t, Icon::Message, tr("Noch redet keiner"), tr("Sobald eine Instanz läuft, erscheint ihr Chat hier."), None)
            }
        } else {
            let shown = (*shared.shown.read()).min(ids.len());
            let older = ids.len() - shown;
            let window: Vec<ChatLine> = ids[older..].iter().filter_map(|id| m.chat.get((id - base) as usize).cloned()).collect();
            let names = if m.batches.len() > 1 { m.batches.iter().map(|b| (b.id.clone(), b.name.clone())).collect() } else { Vec::new() };
            Lines { lines: window, ids: ids[older..].to_vec(), older, names }.into_element()
        };
        page = page.child(rect().width(Size::fill()).height(Size::flex(1.)).child(body)).content(Content::flex());
        if broadcast {
            page = page.child(Composer);
        } else {
            page = page.child(
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
        page.into_element()
    }
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

/// The newest lines in scope, following the newest while the list is at its end.
#[derive(PartialEq)]
struct Lines {
    lines: Vec<ChatLine>,
    ids: Vec<u64>,
    older: usize,
    names: Vec<(String, String)>,
}

impl Component for Lines {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut scroll = use_scroll_controller(|| ScrollConfig { default_vertical_position: ScrollPosition::End, ..Default::default() });
        let mut follow = use_state(|| true);
        let mut tall = use_state(|| 0f32);
        use_side_effect(move || {
            let Some((content, viewport)) = *scroll.bounds.read() else { return };
            let y = scroll.position().y;
            if content.height != *tall.peek() {
                tall.set(content.height);
                if *follow.peek() {
                    scroll.scroll_to(ScrollPosition::End, Direction::Vertical);
                }
                return;
            }
            let at_end = -y >= content.height - viewport.height - space::S;
            if at_end != *follow.peek() {
                follow.set(at_end);
            }
        });
        let at_end = *follow.read();
        let now = now();
        let mut list = rect().width(Size::fill()).padding((space::S, space::S, space::M, space::S));
        if self.older > 0 {
            let mut shown = store.ui.chat.shown;
            let more = self.older.min(PAGE);
            list = list.child(rect().width(Size::fill()).padding(space::S).center().child(Button::new("chat.older", if more == 1 { tr("1 ältere Zeile zeigen") } else { trf("%lld ältere Zeilen zeigen", &[&more]) }).plain().on_press(move |_| {
                let now = *shown.peek();
                shown.set(now + PAGE);
            })));
        }
        let mut before: Option<i64> = None;
        for (i, l) in self.lines.iter().enumerate() {
            if before.is_none_or(|b| !same_day(b, l.at)) {
                list = list.child(day_separator(&t, day_title(l.at, now)));
            }
            before = Some(l.at);
            let batch = self.names.iter().find(|(id, _)| *id == l.batch).map(|(_, n)| n.clone());
            list = list.child(Row { line: l.clone(), id: self.ids.get(i).copied().unwrap_or_default(), batch }.into_element());
        }
        let mut wrap = rect().width(Size::fill()).height(Size::fill()).child(ScrollView::new_controlled(scroll).width(Size::fill()).height(Size::fill()).child(list));
        if !at_end {
            wrap = wrap.child(
                rect()
                    .position(Position::new_absolute().left(0.).bottom(space::M))
                    .width(Size::fill())
                    .main_align(Alignment::Center)
                    .horizontal()
                    .child(Button::new("chat.newest", tr("Zu den neuesten")).glyph(Icon::ArrowDown).primary().on_press(move |_| {
                        scroll.scroll_to(ScrollPosition::End, Direction::Vertical);
                        follow.set(true);
                    })),
            );
        }
        wrap
    }
}

#[derive(PartialEq)]
struct Row {
    line: ChatLine,
    id: u64,
    batch: Option<String>,
}

impl Component for Row {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let l = &self.line;
        let who = if l.also.is_empty() { l.account.clone() } else { format!("{} +{}", l.account, l.also.len()) };
        let names = (!l.also.is_empty()).then(|| std::iter::once(l.account.as_str()).chain(l.also.iter().map(|(_, n, _)| n.as_str())).collect::<Vec<_>>().join(", "));
        let mut label = rect().width(Size::px(LABEL_W)).child(WithTip::new(names, line(&t, who, font::CALLOUT, t.text2).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis)));
        if let Some(b) = &self.batch {
            label = label.child(caption(&t, b.clone()).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        }
        let text = clip(&l.text);
        let (prefix, color) = match (&l.from, l.whisper) {
            (Some(from), true) => (Some(trf("%@ flüstert:", &[from])), t.accent_text),
            (None, true) => (Some(tr("Flüstert:")), t.accent_text),
            (Some(from), false) => (Some(format!("{from}:")), t.text2),
            (None, false) => (None, t.text2),
        };
        let mut body = paragraph().width(Size::flex(1.)).font_size(font::BODY).color(t.text1);
        if let Some(p) = prefix {
            body = body.span(Span::new(format!("{p} ")).color(color).font_weight(FontWeight::SEMI_BOLD));
        }
        body = body.span(Span::new(text));
        let wash = if l.whisper { t.accent.with_af32(WASH) } else { Color::TRANSPARENT };
        let summary = format!("{}: {}", l.account, l.text);
        let spot = use_spot(&format!("chat.line.{}", self.id), "chat-line", &summary, true);
        rect()
            .on_sized(spot)
            .a11y_role(AccessibilityRole::ListItem)
            .a11y_alt(summary)
            .width(Size::fill())
            .padding((space::XS, space::M))
            .horizontal()
            .spacing(space::M)
            .corner_radius(super::theme::radius::INNER)
            .background(wash)
            .border(Border::new().fill(if l.whisper { t.accent } else { Color::TRANSPARENT }).width(BorderWidth { top: 0., right: 0., bottom: 0., left: BAR }).alignment(BorderAlignment::Inner))
            .child(rect().width(Size::px(TIME_W)).padding((space::XXS, 0., 0., 0.)).child(caption(&t, clock(l.at))))
            .child(label)
            .child(body)
            .content(Content::flex())
    }
}

fn day_separator(t: &Theme, day: String) -> Element {
    let rule = || rect().width(Size::flex(1.)).height(Size::px(1.)).background(t.separator);
    rect().width(Size::fill()).padding((space::S, space::M)).horizontal().cross_align(Alignment::Center).spacing(space::S).child(rule()).child(caption(t, day)).child(rule()).content(Content::flex()).into_element()
}

/// The line under the list: who gets the message, the message, and what happened to the last one.
#[derive(PartialEq)]
struct Composer;

impl Component for Composer {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let shared = store.ui.chat;
        let field = use_a11y();
        let focus_at = *shared.focus.read();
        use_side_effect_with_deps(&focus_at, move |at| {
            if at.is_some() {
                field.request_focus();
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
        });
        let m = store.model.read();
        let target = target_of(&m, shared);
        let reach = reached(&m, target.as_ref());
        let choices = target_choices(&m);
        let chosen = target.as_ref().and_then(|tg| choices.iter().position(|(_, c)| c == tg));
        let name = match &target {
            Some(Target::Batch(id)) => m.batch(id).map(|b| b.name.clone()),
            Some(Target::Instance(key)) => m.games.get(key).map(|i| i.name.clone()),
            None => None,
        }
        .unwrap_or_default();
        let hint = match (&target, shared.sent.read().clone()) {
            (_, Some(sent)) => sent,
            (None, None) => tr("Wähle, wer die Nachricht bekommt."),
            (Some(Target::Batch(_)), None) if reach == 0 => trf("In %@ läuft gerade keine Instanz.", &[&name]),
            (Some(Target::Batch(_)), None) => {
                let who = if reach == 1 { tr("1 laufende Instanz") } else { trf("%lld laufende Instanzen", &[&reach]) };
                trf("An %@ in %@ · Enter sendet · ↑ letzte Eingabe", &[&who, &name])
            }
            (Some(Target::Instance(_)), None) => trf("An %@ · Enter sendet · ↑ letzte Eingabe", &[&name]),
        };
        let joins: Vec<String> = match &target {
            Some(Target::Batch(id)) => m.batch(id).map(|b| b.join_lines()).unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut labels: Vec<String> = choices.iter().map(|(l, _)| l.clone()).collect();
        let targets: Vec<Target> = choices.into_iter().map(|(_, c)| c).collect();
        let picker_at = match chosen {
            Some(i) => i,
            None => {
                labels.insert(0, tr("Empfänger wählen"));
                0
            }
        };
        let offset = usize::from(chosen.is_none());
        let can_send = !draft.trim().is_empty();
        let mut send = Button::new("chat.send", tr("Senden")).glyph(Icon::Send).disabled((!can_send).then(|| tr("Schreib zuerst eine Nachricht.")));
        if can_send {
            send = send.primary();
        }
        let row = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(Select::new("chat.target", tr("Empfänger"), labels, picker_at).width(TARGET_W).on_choose(move |i| {
                let Some(t) = i.checked_sub(offset).and_then(|i| targets.get(i)).cloned() else { return };
                let mut shared = store.ui.chat;
                shared.target.set(Some(t));
                shared.manual.set(true);
                shared.sent.set(None);
                shared.rejected.set(None);
            }))
            .child(rect().width(Size::flex(1.)).child(Field::new("chat.message", shared.draft).placeholder(tr("Nachricht oder /Befehl")).a11y(field).on_submit(move |_| send_now(store))))
            .content(Content::flex())
            .child(send.on_press(move |_| send_now(store)));
        let note = match shared.rejected.read().clone() {
            Some((_, why)) => line(&t, why, font::CAPTION, t.danger_text),
            None => caption(&t, hint),
        };
        let mut col = rect()
            .width(Size::fill())
            .padding((space::M, space::L, space::S, space::L))
            .spacing(space::XS)
            .border(Border::new().fill(t.separator).width(BorderWidth { top: 1., right: 0., bottom: 0., left: 0. }).alignment(BorderAlignment::Inner))
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if *Platform::get().current_window().focused_accessibility_id.peek() != field || !e.modifiers.is_empty() {
                    return;
                }
                let delta = match e.key {
                    Key::Named(NamedKey::ArrowUp) => -1,
                    Key::Named(NamedKey::ArrowDown) => 1,
                    _ => return,
                };
                e.stop_propagation();
                step_history(store, delta);
            })
            .child(row)
            .child(note);
        if !joins.is_empty() {
            let names: Vec<&str> = joins.iter().map(|j| j.split_whitespace().next().unwrap_or_default()).collect();
            col = col.child(caption(&t, trf("Beim Betreten sendet jede Instanz: %@", &[&names.join(", ")])).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        }
        col
    }
}

fn send_now(store: Store) {
    send(store);
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

fn in_scope(line: &ChatLine, scope: &Scope) -> bool {
    scope.batch.as_ref().is_none_or(|b| line.batch == *b || line.also.iter().any(|(_, _, other)| other == b)) && (!scope.whispers || line.whisper)
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
    use super::{ChatLine, MAX_CHAT_CHARS, SHOWN_CHARS, Scope, clip, clock, day_title, in_scope, line_problem, now, recall_step, same_day, unique};

    fn line(batch: &str, whisper: bool) -> ChatLine {
        ChatLine { instance: "k".into(), account: "Alex".into(), batch: batch.into(), also: Vec::new(), at: 0, whisper, from: None, text: "x".into() }
    }

    #[test]
    fn scope_filters_by_batch_and_whispers() {
        let all = Scope::default();
        let farm = Scope { batch: Some("farm".into()), whispers: false };
        let whispers = Scope { batch: None, whispers: true };
        let farm_whispers = Scope { batch: Some("farm".into()), whispers: true };
        assert!(in_scope(&line("a", false), &all));
        assert!(in_scope(&line("farm", false), &farm) && !in_scope(&line("a", false), &farm));
        let mut shared = line("a", false);
        shared.also.push(("k".into(), "n".into(), "farm".into()));
        assert!(in_scope(&shared, &farm));
        assert!(in_scope(&line("a", true), &whispers) && !in_scope(&line("a", false), &whispers));
        assert!(in_scope(&line("farm", true), &farm_whispers) && !in_scope(&line("farm", false), &farm_whispers) && !in_scope(&line("a", true), &farm_whispers));
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
