//! The Chat page: the chat of every instance in one list, whispers highlighted, and a line that goes to one instance or a whole batch.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use gpui_kit::component::Sizable;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{Broadcast, OpenSettings, SendChat};
use super::drive::Mark;
use super::main_window::{self, MainWindow, Page};
use super::model::{self, ChatLine, Model};
use super::motion::Soft;
use super::sheet::{self, Look};
use super::theme::{Theme, chat, control, font, iconsize, radius, shell, space, tint, with_alpha};
use super::widgets::{KeyNav, TakeFocus, icon};
use crate::core::batch::MAX_CHAT_CHARS;
use crate::core::i18n::{tr, trf};
use crate::core::settings;

const KEPT_HISTORY: usize = 100;
const OVERDRAW: f32 = 400.;

/// Shows the Chat page in the main window.
pub fn show(cx: &mut App) {
    let Some(handle) = cx.windows().into_iter().find(|w| main_window::is_main(w.window_id(), cx)) else { return };
    let _ = handle.update(cx, |view, window, cx| {
        let Ok(root) = view.downcast::<gpui_kit::base::Root>() else { return };
        let content = root.read(cx).view().clone();
        if let Ok(view) = content.downcast::<MainWindow>() {
            view.update(cx, |this, cx| this.go(Page::Chat, window, cx));
        }
    });
}

/// Shows the Chat page and puts the cursor in its input.
pub fn open(cx: &mut App) {
    cx.set_global(FocusInput);
    show(cx);
    if let Some(page) = cx.try_global::<Shared>().map(|s| s.0.clone()) {
        page.update(cx, |_, cx| cx.notify());
    }
}

/// The Chat page the main window shows.
pub fn page(window: &mut Window, cx: &mut App) -> impl IntoElement {
    if let Some(page) = cx.try_global::<Shared>() {
        return page.0.clone();
    }
    let view = cx.new(|cx| ChatPage::new(window, cx));
    cx.set_global(Shared(view.clone()));
    view
}

struct Shared(Entity<ChatPage>);

struct FocusInput;

impl Global for FocusInput {}

impl Global for Shared {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Batch(String),
    Instance(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Scope {
    batch: Option<String>,
    whispers: bool,
}

struct Synced {
    scope: Scope,
    base: u64,
    seq: u64,
}

struct ChatPage {
    list: ListState,
    shown: Rc<RefCell<Vec<u64>>>,
    synced: Option<Synced>,
    scope: Scope,
    away: Rc<Cell<bool>>,
    input: Entity<InputState>,
    batch_select: Entity<SelectState<SearchableVec<String>>>,
    target_select: Entity<SelectState<SearchableVec<String>>>,
    batch_choices: Vec<(String, Option<String>)>,
    target_choices: Vec<(String, Target)>,
    choices_sig: Option<u64>,
    target: Option<Target>,
    target_manual: bool,
    history: Vec<String>,
    recall: Option<usize>,
    draft: String,
    sent: Option<String>,
    rejected: Option<(String, String)>,
    caret_parked: bool,
    _subs: Vec<Subscription>,
}

impl ChatPage {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let away = Rc::new(Cell::new(false));
        let me = cx.entity().downgrade();
        let flag = Rc::clone(&away);
        list.set_scroll_handler(move |event, _, cx| {
            let scrolled_up = event.is_scrolled && !event.is_following_tail;
            if flag.replace(scrolled_up) != scrolled_up {
                let _ = me.update(cx, |_, cx| cx.notify());
            }
        });
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Nachricht oder /Befehl")));
        let batch_select = cx.new(|cx| SelectState::new(SearchableVec::new(vec![tr("Alle Batches")]), Some(gpui_kit::component::IndexPath::new(0)), window, cx).searchable(true));
        let target_select = cx.new(|cx| SelectState::new(SearchableVec::new(Vec::<String>::new()), None, window, cx).searchable(true));
        let subs = vec![
            cx.observe_window_activation(window, |s: &mut Self, window, cx| {
                let focus = s.input.focus_handle(cx);
                if window.is_window_active() {
                    if std::mem::take(&mut s.caret_parked) {
                        window.focus(&focus, cx);
                    }
                } else if focus.is_focused(window) {
                    s.caret_parked = true;
                    window.blur(cx);
                }
            }),
            cx.subscribe_in(&input, window, |s: &mut Self, _, e: &InputEvent, window, cx| match e {
                InputEvent::PressEnter { .. } => s.send(window, cx),
                InputEvent::Change => {
                    let value = s.input.read(cx).value().to_string();
                    if s.recall.is_some_and(|at| s.history.get(at) != Some(&value)) {
                        s.recall = None;
                    }
                    if s.rejected.as_ref().is_some_and(|(text, _)| value.trim() != text) {
                        s.rejected = None;
                    }
                    if !value.is_empty() {
                        s.sent = None;
                    }
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&batch_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<String>>, window, cx| {
                let SelectEvent::Confirm(Some(label)) = e else { return };
                let batch = s.batch_choices.iter().find(|(l, _)| l == label).and_then(|(_, id)| id.clone());
                s.set_batch(batch, window, cx);
            }),
            cx.subscribe_in(&target_select, window, |s: &mut Self, _, e: &SelectEvent<SearchableVec<String>>, window, cx| {
                let SelectEvent::Confirm(Some(label)) = e else { return };
                let target = s.target_choices.iter().find(|(l, _)| l == label).map(|(_, t)| t.clone());
                s.set_target(target, true, window, cx);
            }),
        ];
        Self {
            list,
            shown: Rc::default(),
            synced: None,
            scope: Scope::default(),
            away,
            input,
            batch_select,
            target_select,
            batch_choices: Vec::new(),
            target_choices: Vec::new(),
            choices_sig: None,
            target: None,
            target_manual: false,
            history: Vec::new(),
            recall: None,
            draft: String::new(),
            sent: None,
            rejected: None,
            caret_parked: false,
            _subs: subs,
        }
    }

    fn set_batch(&mut self, batch: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.scope.batch == batch {
            return;
        }
        self.scope.batch = batch.clone();
        if !self.target_manual {
            self.set_target(batch.map(Target::Batch), false, window, cx);
        }
        cx.notify();
    }

    fn set_target(&mut self, target: Option<Target>, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.target = target;
        self.target_manual = manual;
        self.sent = None;
        self.rejected = None;
        let label = self.target.as_ref().and_then(|t| self.target_choices.iter().find(|(_, c)| c == t)).map(|(l, _)| l.clone());
        let select = self.target_select.clone();
        window.on_next_frame(move |window, cx| {
            select.update(cx, |s, cx| match &label {
                Some(label) => s.set_selected_value(label, window, cx),
                None => s.set_selected_index(None, window, cx),
            });
        });
        cx.notify();
    }

    fn choices(&mut self, data: Choices, sig: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.choices_sig = Some(sig);
        let Choices { batches, targets, batch_ids, only_batch } = data;
        self.batch_choices = batches;
        self.target_choices = targets;
        if self.scope.batch.as_ref().is_some_and(|id| !batch_ids.contains(id)) {
            self.scope.batch = None;
        }
        if self.target.as_ref().is_some_and(|t| !self.target_choices.iter().any(|(_, c)| c == t)) {
            self.target = None;
            self.target_manual = false;
        }
        if self.target.is_none() && !self.target_manual {
            self.target = self.scope.batch.clone().map(Target::Batch).or_else(|| only_batch.map(Target::Batch));
        }
        let labels: Vec<String> = self.batch_choices.iter().map(|(l, _)| l.clone()).collect();
        let at = self.batch_choices.iter().position(|(_, id)| *id == self.scope.batch).unwrap_or(0);
        self.batch_select.update(cx, |s, cx| {
            s.set_items(SearchableVec::new(labels), window, cx);
            s.set_selected_index(Some(gpui_kit::component::IndexPath::new(at)), window, cx);
        });
        let labels: Vec<String> = self.target_choices.iter().map(|(l, _)| l.clone()).collect();
        let at = self.target.as_ref().and_then(|t| self.target_choices.iter().position(|(_, c)| c == t));
        self.target_select.update(cx, |s, cx| {
            s.set_items(SearchableVec::new(labels), window, cx);
            s.set_selected_index(at.map(gpui_kit::component::IndexPath::new), window, cx);
        });
    }

    fn sync(&mut self, m: &Model) {
        let (base, seq) = (m.chat_base(), m.chat_seq);
        let same = self.synced.as_ref().is_some_and(|s| s.scope == self.scope && s.base == base);
        if same && self.synced.as_ref().is_some_and(|s| s.seq == seq) {
            return;
        }
        let lines = |ids: std::ops::Range<u64>| ids.filter(|id| m.chat.get((id - base) as usize).is_some_and(|l| in_scope(l, &self.scope))).collect::<Vec<u64>>();
        let trimmed = self.synced.as_ref().is_some_and(|s| s.scope == self.scope && s.base < base);
        if trimmed {
            let dropped = self.shown.borrow().iter().take_while(|id| **id < base).count();
            if dropped > 0 {
                self.list.splice(0..dropped, 0);
                self.shown.borrow_mut().drain(..dropped);
            }
            let from = self.synced.as_ref().map_or(base, |s| s.seq);
            let added = lines(from.max(base)..seq);
            let old = self.shown.borrow().len();
            if !added.is_empty() {
                self.list.splice(old..old, added.len());
                self.shown.borrow_mut().extend(added);
            }
        } else if let Some(synced) = self.synced.as_ref().filter(|_| same) {
            let added = lines(synced.seq..seq);
            let old = self.shown.borrow().len();
            if !added.is_empty() {
                self.list.splice(old..old, added.len());
                self.shown.borrow_mut().extend(added);
            }
        } else {
            let following = self.synced.as_ref().is_none_or(|s| s.scope != self.scope) || self.list.is_scrolled_to_end() != Some(false);
            let all = lines(base..seq);
            self.list.reset(all.len());
            *self.shown.borrow_mut() = all;
            if following {
                self.list.scroll_to_end();
            }
        }
        self.synced = Some(Synced { scope: self.scope.clone(), base, seq });
    }

    fn running(&self, m: &Model) -> usize {
        match &self.target {
            Some(Target::Batch(id)) => m.instances.values().filter(|i| i.batch_id == *id && i.phase.on_server()).count(),
            Some(Target::Instance(key)) => usize::from(m.instances.get(key).is_some_and(|i| i.phase.on_server())),
            None => 0,
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_owned();
        let model = model::get(cx);
        let reached = self.running(model.read(cx));
        if text.is_empty() || !settings::active().broadcast {
            return;
        }
        let problem = line_problem(&text).or_else(|| self.target.is_none().then(|| tr("Wähle, wer die Nachricht bekommt."))).or_else(|| (reached == 0).then(|| tr("Dort läuft gerade keine Instanz.")));
        if let Some(problem) = problem {
            self.rejected = Some((text, problem));
            self.sent = None;
            cx.notify();
            return;
        }
        let Some(target) = self.target.clone() else { return };
        match target {
            Target::Batch(batch) => window.dispatch_action(Box::new(Broadcast { batch, text: text.clone() }), cx),
            Target::Instance(key) => window.dispatch_action(Box::new(SendChat { key, text: text.clone() }), cx),
        }
        if self.history.last() != Some(&text) {
            self.history.push(text);
            if self.history.len() > KEPT_HISTORY {
                self.history.remove(0);
            }
        }
        self.recall = None;
        self.draft.clear();
        self.sent = Some(if reached == 1 { tr("An 1 Instanz gesendet.") } else { trf("An %lld Instanzen gesendet.", &[&reached]) });
        cx.defer_in(window, |this, window, cx| {
            this.input.update(cx, |s, cx| s.set_value("", window, cx));
            cx.notify();
        });
        cx.notify();
    }

    fn step_history(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.input.read(cx).value().to_string();
        let (recall, text) = recall_step(&self.history, self.recall, delta, &current, &self.draft);
        if self.recall.is_none() && recall.is_some() {
            self.draft = current;
        }
        self.recall = recall;
        cx.defer_in(window, move |this, window, cx| {
            this.input.update(cx, |s, cx| s.set_value(text, window, cx));
        });
    }

    fn header(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let toggle = sheet::sidebar_toggle("chat-sidebar", t);
        let whispers = self.scope.whispers;
        let on = settings::active().merged_chat;
        let title = tr("Chat");
        let segments = div()
            .id("chat-scope")
            .p(px(space::XXS))
            .rounded(px(radius::CONTROL))
            .bg(t.raised)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(space::XXS))
            .child(segment("scope-all", tr("Alles"), !whispers, t).on_click(cx.listener(|s, _, _, cx| {
                s.scope.whispers = false;
                cx.notify();
            })))
            .child(segment("scope-whispers", tr("Nur Flüstern"), whispers, t).on_click(cx.listener(|s, _, _, cx| {
                s.scope.whispers = true;
                cx.notify();
            })));
        div()
            .h(px(shell::TOOLBAR))
            .flex_none()
            .px(px(space::M))
            .flex()
            .items_center()
            .gap(px(space::S))
            .border_b_1()
            .border_color(t.separator)
            .child(toggle)
            .child(div().id("chat-title").mark("heading", title.clone()).min_w_0().truncate().text_size(px(font::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
            .child(div().w(px(space::S)).flex_none())
            .children(on.then_some(segments))
            .child(div().flex_1().min_w(px(space::S)))
            .children(on.then(|| div().w(px(chat::PICKER)).flex_none().child(Select::new(&self.batch_select).small().menu_max_h(px(sheet::LIST_MAX)).search_placeholder(tr("Batch suchen")))))
    }

    fn empty(&self, id: &'static str, symbol: &str, title: String, text: String, button: Option<AnyElement>, t: &Theme) -> Stateful<Div> {
        div()
            .id(id)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(space::M))
            .px(px(space::XL))
            .child(icon(symbol, iconsize::HERO, t.tertiary))
            .child(div().id(SharedString::from(format!("{id}-title"))).mark("heading", title.clone()).text_size(px(font::TITLE2)).font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(title))
            .child(div().max_w(px(control::READING)).text_size(px(font::CALLOUT)).text_color(t.secondary).text_center().child(text))
            .children(button.map(|b| div().mt(px(space::S)).child(b)))
    }

    fn body(&mut self, any_lines: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if !settings::active().merged_chat {
            let open = sheet::button("chat-open-settings", tr("Einstellungen öffnen"), Some("settings"), Look::Secondary, false, false, t).on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)).into_any_element();
            return self.empty("chat-off", "messages-square", tr("Der gemeinsame Chat ist aus"), tr("Manycraft sammelt keine Chatzeilen. In den Einstellungen schaltest du ihn wieder ein."), Some(open), t).into_any_element();
        }
        if self.shown.borrow().is_empty() {
            if self.scope.whispers {
                let all = sheet::button("chat-show-all", tr("Alles zeigen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, _, cx| {
                    s.scope.whispers = false;
                    cx.notify();
                })).into_any_element();
                return self.empty("chat-no-whispers", "message-circle", tr("Niemand hat geflüstert"), tr("Flüsternachrichten an deine Accounts erscheinen hier."), Some(all), t).into_any_element();
            }
            if self.scope.batch.is_some() && any_lines {
                let reset = sheet::button("chat-reset-filter", tr("Filter zurücksetzen"), None, Look::Secondary, false, false, t).on_click(cx.listener(|s, _, window, cx| s.set_batch(None, window, cx))).into_any_element();
                return self.empty("chat-no-match", "search", tr("Keine Treffer"), tr("In diesem Batch gibt es noch keine Chatzeilen."), Some(reset), t).into_any_element();
            }
            return self.empty("chat-empty", "messages-square", tr("Noch keine Chatzeilen"), tr("Sobald eine Instanz läuft, erscheint ihr Chat hier."), None, t).into_any_element();
        }
        let rows = Rc::clone(&self.shown);
        let model = model::get(cx);
        let t = *t;
        let list = list(self.list.clone(), move |ix, _, cx| {
            let Some(id) = rows.borrow().get(ix).copied() else { return div().into_any_element() };
            let m = model.read(cx);
            let Some(line) = id.checked_sub(m.chat_base()).and_then(|at| m.chat.get(at as usize)) else { return div().into_any_element() };
            let batch = if m.batches.len() > 1 { m.batch(&line.batch).map(|b| b.name.clone()) } else { None };
            let before = ix.checked_sub(1).and_then(|p| rows.borrow().get(p).copied()).and_then(|p| p.checked_sub(m.chat_base())).and_then(|p| m.chat.get(p as usize)).map(|l| l.at);
            let day = before.is_none_or(|b| !same_day(b, line.at)).then(|| day_title(line.at, now()));
            line_row(id, line, batch, day, &t)
        })
        .size_full()
        .px(px(space::S))
        .pt(px(space::S))
        .pb(px(chat::INSET));
        let jump = (self.list.is_scrolled_to_end() == Some(false)).then(|| {
            div().absolute().bottom(px(chat::INSET)).left_0().right_0().flex().justify_center().child(
                sheet::button("chat-newest", tr("Zu den neuesten"), Some("arrow-down"), Look::Primary, false, false, &t).shadow_md().on_click(cx.listener(|s, _, _, cx| {
                    s.list.scroll_to_end();
                    s.away.set(false);
                    cx.notify();
                })),
            )
        });
        div().id("chat-list").size_full().relative().child(list).children(jump).into_any_element()
    }

    fn composer_off(&self, t: &Theme) -> Div {
        let open = sheet::button("chat-send-off", tr("Einstellungen öffnen"), Some("settings"), Look::Secondary, false, false, t).on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx));
        div()
            .flex_none()
            .px(px(space::L))
            .py(px(space::M))
            .border_t_1()
            .border_color(t.separator)
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space::M))
            .child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(tr("Senden ist in den Einstellungen ausgeschaltet.")))
            .child(open)
    }

    fn composer(&mut self, reached: usize, target_name: String, t: &Theme, cx: &mut Context<Self>) -> Div {
        let text = self.input.read(cx).value().trim().to_owned();
        let can_send = !text.is_empty();
        let hint = match (&self.target, self.sent.clone()) {
            (_, Some(sent)) => sent,
            (None, None) => tr("Wähle, wer die Nachricht bekommt."),
            (Some(Target::Batch(_)), None) => {
                if reached == 0 {
                    trf("In %@ läuft gerade keine Instanz.", &[&target_name])
                } else {
                    let who = if reached == 1 { tr("1 laufende Instanz") } else { trf("%lld laufende Instanzen", &[&reached]) };
                    trf("An %@ in %@ · Enter sendet · ↑ letzte Eingabe", &[&who, &target_name])
                }
            }
            (Some(Target::Instance(_)), None) => trf("An %@ · Enter sendet · ↑ letzte Eingabe", &[&target_name]),
        };
        let look = if can_send { Look::Primary } else { Look::Secondary };
        let send = sheet::button("chat-send", tr("Senden"), Some("send"), look, !can_send, false, t).on_click(cx.listener(|s, _, window, cx| s.send(window, cx)));
        let me = cx.entity().downgrade();
        div()
            .flex_none()
            .px(px(space::L))
            .pt(px(space::M))
            .pb(px(space::S))
            .border_t_1()
            .border_color(t.separator)
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::S))
                    .child(div().w(px(chat::PICKER)).flex_none().child(Select::new(&self.target_select).menu_max_h(px(sheet::LIST_MAX)).placeholder(tr("Empfänger")).search_placeholder(tr("Empfänger suchen"))))
                    .child(
                        div()
                            .key_context("ChatInput")
                            .flex_1()
                            .min_w_0()
                            .capture_key_down(move |e: &KeyDownEvent, window, cx| {
                                let delta = match e.keystroke.key.as_str() {
                                    "up" => -1,
                                    "down" => 1,
                                    _ => return,
                                };
                                if e.keystroke.modifiers.modified() {
                                    return;
                                }
                                cx.stop_propagation();
                                let _ = me.update(cx, |s, cx| s.step_history(delta, window, cx));
                            })
                            .child(Input::new(&self.input)),
                    )
                    .child(send),
            )
            .child(match self.rejected.clone().map(|(_, why)| why) {
                Some(why) => div().min_h(px(font::CAPTION * font::LINE)).text_size(px(font::CAPTION)).text_color(t.primary).child(why),
                None => sheet::note(Some(hint), None, t),
            })
    }
}

impl Render for ChatPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        let model = model::get(cx);
        let broadcast = settings::active().broadcast;
        let sig = choices_signature(model.read(cx));
        if self.choices_sig != Some(sig) {
            let data = Choices::build(model.read(cx));
            self.choices(data, sig, window, cx);
        }
        self.sync(model.read(cx));
        if window.is_window_active() || crate::core::drive::on() {
            let scope = self.scope.batch.clone();
            if model.read(cx).instances.values().any(|i| i.unread_whispers > 0 && scope.as_deref().is_none_or(|b| i.batch_id == b)) {
                cx.defer(move |cx| super::state::clear_whispers(scope.as_deref(), cx));
            }
        }
        let (any_lines, reached, target_name) = {
            let m = model.read(cx);
            let name = match &self.target {
                Some(Target::Batch(id)) => m.batch(id).map(|b| b.name.clone()),
                Some(Target::Instance(key)) => m.instances.get(key).map(|i| i.name.clone()),
                None => None,
            };
            (!m.chat.is_empty(), self.running(m), name.unwrap_or_default())
        };
        let header = self.header(&t, cx);
        let body = self.body(any_lines, &t, cx);
        let merged = settings::active().merged_chat;
        let composer = (merged && broadcast).then(|| self.composer(reached, target_name, &t, cx)).or_else(|| merged.then(|| self.composer_off(&t)));
        if cx.has_global::<FocusInput>() {
            cx.remove_global::<FocusInput>();
            if merged && broadcast {
                let input = self.input.clone();
                window.on_next_frame(move |window, cx| input.update(cx, |s, cx| s.take_focus(window, cx)));
            }
        }
        div()
            .id("chat-page")
            .size_full()
            .flex()
            .flex_col()
            .bg(t.window)
            .text_color(t.primary)
            .font_family(Theme::font_family())
            .text_size(px(font::BODY))
            .line_height(relative(font::LINE))
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .children(composer)
    }
}

fn segment(id: &'static str, label: String, selected: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .mark("segment", label.clone())
        .h(px(control::S))
        .px(px(space::S))
        .rounded(px(radius::INNER))
        .flex()
        .flex_none()
        .items_center()
        .text_size(px(font::CALLOUT))
        .when(selected, |d| d.bg(t.content).shadow_xs().font_weight(FontWeight::MEDIUM).text_color(t.primary))
        .when(!selected, |d| d.bg(t.raised).text_color(t.secondary))
        .cursor_pointer()
        .key_nav()
        .soft(t.ink.opacity(if selected { 0. } else { tint::HOVER }), radius::INNER)
        .child(label)
}

fn line_row(id: u64, line: &ChatLine, batch: Option<String>, day: Option<String>, t: &Theme) -> AnyElement {
    let label = div()
        .w(px(chat::LABEL))
        .flex_none()
        .flex()
        .flex_col()
        .child(
            div()
                .id(SharedString::from(format!("chat-from-{id}")))
                .truncate()
                .text_size(px(font::CALLOUT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.secondary)
                .child(if line.also.is_empty() { line.account.clone() } else { format!("{} +{}", line.account, line.also.len()) })
                .when(!line.also.is_empty(), |d| {
                    let names = std::iter::once(line.account.as_str()).chain(line.also.iter().map(|(_, name, _)| name.as_str())).collect::<Vec<_>>().join(", ");
                    d.tooltip(super::widgets::tip(names))
                }),
        )
        .children(batch.map(|batch| div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(batch)));
    let text = clip(&line.text);
    let (prefix, color) = match (&line.from, line.whisper) {
        (Some(from), true) => (Some(trf("%@ flüstert:", &[from])), t.accent_text),
        (None, true) => (Some(tr("Flüstert:")), t.accent_text),
        (Some(from), false) => (Some(format!("{from}:")), t.secondary),
        (None, false) => (None, t.secondary),
    };
    let body = match prefix {
        Some(prefix) => {
            let styled = format!("{prefix} {text}");
            let mark = HighlightStyle { color: Some(color), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
            StyledText::new(styled).with_highlights([(0..prefix.len(), mark)]).into_any_element()
        }
        None => div().text_color(t.secondary).child(text.clone()).into_any_element(),
    };
    let summary = format!("{}: {}", line.account, line.text);
    let time = div().w(px(chat::TIME)).flex_none().text_size(px(font::CAPTION)).text_color(t.tertiary).pt(px(space::XXS)).child(clock(line.at));
    let row = div()
        .id(SharedString::from(format!("chat-line-{id}")))
        .mark("chat-line", summary)
        .w_full()
        .flex()
        .items_start()
        .gap(px(space::M))
        .px(px(space::M))
        .py(px(space::XS))
        .rounded(px(radius::INNER))
        .border_l(px(chat::BAR))
        .border_color(if line.whisper { t.accent } else { gpui_kit::transparent_black() })
        .when(line.whisper, |d| d.bg(with_alpha(t.accent, chat::WASH)))
        .child(time)
        .child(label)
        .child(div().flex_1().min_w_0().text_color(t.primary).child(body));
    match day {
        Some(day) => div().w_full().child(day_separator(day, t)).child(row).into_any_element(),
        None => row.into_any_element(),
    }
}

fn day_separator(day: String, t: &Theme) -> Div {
    let rule = || div().flex_1().h(px(1.)).bg(t.separator);
    div().w_full().px(px(space::M)).py(px(space::S)).flex().items_center().gap(px(space::S)).child(rule()).child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(day)).child(rule())
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
    if text.chars().count() <= chat::SHOWN_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(chat::SHOWN_CHARS).collect();
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

/// What the two pickers offer, worked out from the model.
struct Choices {
    batches: Vec<(String, Option<String>)>,
    targets: Vec<(String, Target)>,
    batch_ids: Vec<String>,
    only_batch: Option<String>,
}

impl Choices {
    fn build(m: &Model) -> Self {
        let mut batches: Vec<(String, Option<String>)> = vec![(tr("Alle Batches"), None)];
        let names = unique(m.batches.iter().map(|b| b.name.clone()).collect());
        batches.extend(m.batches.iter().zip(names).map(|(b, name)| (name, Some(b.id.clone()))));
        let batch_labels = unique(
            m.batches
                .iter()
                .map(|b| {
                    let total = m.instances.values().filter(|i| i.batch_id == b.id).count();
                    let running = m.instances.values().filter(|i| i.batch_id == b.id && i.phase.on_server()).count();
                    trf("Alle in %@ · %lld von %lld laufen", &[&b.name, &running, &total])
                })
                .collect(),
        );
        let mut targets: Vec<(String, Target)> = m.batches.iter().zip(batch_labels).map(|(b, label)| (label, Target::Batch(b.id.clone()))).collect();
        let mut instances: Vec<_> = m.instances.values().collect();
        instances.sort_by_cached_key(|i| (!i.phase.on_server(), i.name.to_lowercase(), i.key.clone()));
        let instance_labels = unique(
            instances
                .iter()
                .map(|i| {
                    let label = instance_label(m, &i.name, &i.batch_id);
                    if i.phase.on_server() { label } else { format!("{label} · {}", tr("aus")) }
                })
                .collect(),
        );
        targets.extend(instances.iter().zip(instance_labels).map(|(i, label)| (label, Target::Instance(i.key.clone()))));
        Self { batches, targets, batch_ids: m.batches.iter().map(|b| b.id.clone()).collect(), only_batch: (m.batches.len() == 1).then(|| m.batches[0].id.clone()) }
    }
}

fn instance_label(m: &Model, name: &str, batch_id: &str) -> String {
    match m.batch(batch_id) {
        Some(b) => format!("{name} · {}", b.name),
        None => name.to_owned(),
    }
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

fn choices_signature(m: &Model) -> u64 {
    let mut total = 0u64;
    for i in m.instances.values() {
        let mut h = DefaultHasher::new();
        (&i.key, &i.name, &i.batch_id, i.phase.on_server()).hash(&mut h);
        total ^= h.finish();
    }
    let mut h = DefaultHasher::new();
    total.hash(&mut h);
    m.instances.len().hash(&mut h);
    for b in &m.batches {
        (&b.id, &b.name).hash(&mut h);
    }
    h.finish()
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
    use super::{ChatLine, MAX_CHAT_CHARS, Scope, chat, clip, clock, day_title, in_scope, line_problem, now, recall_step, same_day, unique};

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
        let shown = clip(&"b".repeat(chat::SHOWN_CHARS + 50));
        assert_eq!(shown.chars().count(), chat::SHOWN_CHARS + 1);
        assert_eq!(clip("kurz"), "kurz");
    }
}
