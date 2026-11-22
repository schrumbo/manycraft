use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use freya::prelude::Span as Piece;
use freya::prelude::*;

use super::ops::{changed, note};
use super::{Live, problem_view, snapshots};
use crate::core::i18n::{tr, trf};
use crate::core::instance::Instance;
use crate::core::modrinth::Update;
use crate::core::snapshot::Reason;
use crate::ui::forms::{self, Problem};
use crate::ui::remote;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::switch::Switch;
use crate::view::kit::text::{caption, line, secondary, strong};
use crate::view::kit::{off_ui, toast, use_theme};
use crate::view::store::Store;
use crate::view::theme::{Theme, control, faces, font, frame, iconsize, space};

const NOTES_MAX: usize = 40;
const FRESH: Duration = Duration::from_secs(600);

thread_local! {
    static LOOKED: RefCell<HashMap<String, (Instant, Vec<Update>)>> = RefCell::new(HashMap::new());
}

fn signature(made: &Instance) -> String {
    format!("{}|{}|{}|{}", made.id, made.game_version, made.loader.name(), super::list::key_of(made))
}

fn looked(signature: &str) -> Option<Vec<Update>> {
    LOOKED.with(|l| l.borrow().get(signature).filter(|(at, _)| at.elapsed() < FRESH).map(|(_, list)| list.clone()))
}

fn remember(signature: String, list: Vec<Update>) {
    LOOKED.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|_, (at, _)| at.elapsed() < FRESH);
        l.insert(signature, (Instant::now(), list));
    });
}
const LIST_MAX: f32 = frame::PALETTE_ROW * 10.;

/// What the tab knows about updates of its mods.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::view::instance) enum Found {
    #[default]
    Idle,
    /// `quiet` when nobody asked: the tab looked on its own and says nothing unless it finds something.
    Checking { quiet: bool },
    Ready(Vec<Update>),
    Failed(String),
    Applying(Vec<Update>),
}

/// The new version number of every mod with an update, by project; empty when the update does not state one.
pub(super) fn marks(found: &Found) -> HashMap<String, String> {
    match found {
        Found::Ready(list) | Found::Applying(list) => list.iter().map(|u| (u.current.project_id.clone(), u.changelog.first().map(|(number, _)| number.clone()).unwrap_or_default())).collect(),
        _ => HashMap::new(),
    }
}

/// Looks for updates of the tab's mods and shows what it found above the list. A `quiet` look answers from the last one
/// while it is fresh, and neither reports that all is current nor that it could not look.
pub(super) fn check(ctx: &super::Ctx, quiet: bool) {
    let mut found = ctx.found;
    if matches!(*found.peek(), Found::Checking { .. } | Found::Applying(_)) {
        return;
    }
    let Some(made) = ctx.store.model.peek().instance(&ctx.id).cloned() else { return };
    let key = signature(&made);
    if quiet {
        if made.mods.is_empty() {
            return;
        }
        if let Some(list) = looked(&key) {
            found.set(if list.is_empty() { Found::Idle } else { Found::Ready(list) });
            return;
        }
    }
    found.set(Found::Checking { quiet });
    spawn_in_window(async move {
        let result = off_ui(move || remote::mod_updates(&made)).await;
        if found.try_peek().is_none() {
            return;
        }
        found.set(match result {
            Some(Ok(list)) => {
                remember(key, list.clone());
                if list.is_empty() && quiet { Found::Idle } else { Found::Ready(list) }
            }
            _ if quiet => Found::Idle,
            Some(Err(e)) => Found::Failed(forms::explain_as(tr("Updates nicht geladen."), &e).text),
            None => Found::Failed(forms::unexplained()),
        });
    });
}

fn apply_all(store: Store, id: String, list: Vec<Update>, mut found: State<Found>) {
    found.set(Found::Applying(list.clone()));
    spawn_in_window(async move {
        let running = is_running(store, &id);
        let (work_id, sent) = (id.clone(), list.clone());
        let result = off_ui(move || remote::apply_updates(&work_id, &sent, running)).await;
        let alive = found.try_peek().is_some();
        match result {
            Some(Ok(made)) => {
                changed(store, made);
                offer_updated(store, id, list.clone(), label_of(&list));
                if alive {
                    found.set(Found::Idle);
                }
            }
            Some(Err(e)) if alive => found.set(Found::Failed(forms::explain_as(tr("Nicht aktualisiert."), &e).text)),
            Some(Err(e)) => note(store, forms::explain_as(tr("Nicht aktualisiert."), &e).text),
            None if alive => found.set(Found::Ready(list)),
            None => {}
        }
    });
}

/// The line above the list with what the last look for updates found; `locked` says why nothing can change now.
pub(super) fn inline(t: &Theme, ctx: &super::Ctx, locked: Option<String>) -> Option<Element> {
    let state = ctx.found.read().clone();
    let (store, id) = (ctx.store, ctx.id.to_string());
    let mut found = ctx.found;
    let bar = |glyph: Icon, color: Color, text: String| {
        rect()
            .width(Size::fill())
            .height(Size::px(control::L))
            .padding((0., space::S, 0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .corner_radius(t.shape.control)
            .background(t.raised)
            .child(icon(t, glyph, iconsize::S, color))
            .child(line(t, text, font::CALLOUT, t.text1).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.)))
            .content(Content::flex())
    };
    let close = Button::icon("instance.mods.updates.close", Icon::Close, tr("Hinweis ausblenden")).plain().on_press(move |_| found.set(Found::Idle));
    let row = match state {
        Found::Idle | Found::Checking { quiet: true } => return None,
        Found::Checking { quiet: false } => bar(Icon::Refresh, t.text2, tr("Suche nach Updates …")),
        Found::Ready(list) if list.is_empty() => bar(Icon::Check, t.success_text, tr("Alle Mods sind aktuell.")).child(close),
        Found::Failed(text) => {
            let again = ctx.clone();
            bar(Icon::Alert, t.danger_text, text).child(Button::new("instance.mods.updates.retry", tr("Erneut versuchen")).plain().on_press(move |_| again.check_updates())).child(close)
        }
        Found::Ready(list) => {
            let count = if list.len() == 1 { trf("Update für „%@“", &[&list[0].current.title]) } else { trf("%lld Updates verfügbar", &[&list.len()]) };
            let pick_id = id.clone();
            bar(Icon::Download, t.accent_text, count)
                .child(Button::new("instance.mods.updates.pick", tr("Auswählen…")).plain().on_press(move |_| {
                    found.set(Found::Idle);
                    open(store, &pick_id);
                }))
                .child(Button::new("instance.mods.updates.all", tr("Alle aktualisieren")).primary().disabled(locked).on_press(move |_| apply_all(store, id.clone(), list.clone(), found)))
                .child(close)
        }
        Found::Applying(list) => bar(Icon::Download, t.accent_text, trf("%@ wird aktualisiert …", &[&label_of(&list)])).child(Button::new("instance.mods.updates.all", tr("Alle aktualisieren")).primary().busy(true).disabled(Some(tr("Wird aktualisiert …")))),
    };
    Some(row.into_element())
}

/// Opens the sheet that looks for updates of the mods of instance `id`.
pub(super) fn open(store: Store, id: &str) {
    sheet::open(store.ui, Updates { store, id: id.to_owned() });
}

#[derive(Clone)]
enum Load {
    Loading,
    Ready(Vec<Update>),
    Failed(Problem),
}

fn chosen_of(load: &Load, chosen: &HashSet<String>) -> Vec<Update> {
    match load {
        Load::Ready(list) => list.iter().filter(|u| chosen.contains(&u.current.project_id)).cloned().collect(),
        _ => Vec::new(),
    }
}

fn label_of(selection: &[Update]) -> String {
    match selection {
        [one] => trf("„%@“", &[&one.current.title]),
        many => trf("%lld Mods", &[&many.len()]),
    }
}

fn offer_updated(store: Store, id: String, updates: Vec<Update>, label: String) {
    toast::show(store.ui, trf("%@ aktualisiert.", &[&label]), Some(EventHandler::new(move |_| undo_updates(store, id.clone(), updates.clone(), label.clone()))));
}

fn is_running(store: Store, id: &str) -> bool {
    store.model.peek().games.values().any(|g| g.instance_id == id && g.phase.is_active())
}

fn undo_updates(store: Store, id: String, updates: Vec<Update>, label: String) {
    let back: Vec<Update> = updates.iter().map(|u| Update { current: u.latest.clone(), latest: u.current.clone(), changelog: Vec::new() }).collect();
    spawn_in_window(async move {
        let running = is_running(store, &id);
        let work_id = id.clone();
        match off_ui(move || remote::apply_updates(&work_id, &back, running)).await {
            Some(Ok(made)) => changed(store, made),
            Some(Err(e)) => {
                note(store, forms::explain_as(tr("Nicht zurückgestellt."), &e).text);
                if !super::super::actions::permanent(&e) {
                    offer_updated(store, id, updates, label);
                }
            }
            None => offer_updated(store, id, updates, label),
        }
    });
}

fn apply_now(store: Store, id: String, selection: Vec<Update>, applying: State<bool>, error: State<Option<Problem>>) {
    if selection.is_empty() || applying.got() != Some(false) {
        return;
    }
    applying.soft(|a| *a = true);
    error.soft(|e| *e = None);
    spawn_in_window(async move {
        let running = is_running(store, &id);
        let (work_id, sent) = (id.clone(), selection.clone());
        let result = off_ui(move || remote::apply_updates(&work_id, &sent, running)).await;
        let alive = applying.try_peek().is_some();
        applying.soft(|a| *a = false);
        match result {
            Some(Ok(made)) => {
                changed(store, made);
                offer_updated(store, id, selection.clone(), label_of(&selection));
                if alive {
                    sheet::close(store.ui);
                }
            }
            Some(Err(e)) => failed(store, alive, error, forms::explain_as(tr("Nicht aktualisiert."), &e)),
            None => failed(store, alive, error, Problem::plain(forms::unexplained())),
        }
    });
}

fn failed(store: Store, alive: bool, error: State<Option<Problem>>, problem: Problem) {
    if alive {
        error.soft(|e| *e = Some(problem));
    } else {
        note(store, problem.text);
    }
}

#[derive(PartialEq)]
struct Updates {
    store: Store,
    id: String,
}

impl Component for Updates {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = self.store;
        let ui = store.ui;
        let mut load = use_state(|| Load::Loading);
        let mut chosen = use_state(HashSet::<String>::new);
        let mut open = use_state(HashSet::<String>::new);
        let applying = use_state(|| false);
        let error = use_state(|| None::<Problem>);
        let mut details = use_state(|| false);
        let mut asked = use_state(|| 0u32);

        let source = self.id.clone();
        use_side_effect(move || {
            let _ = asked.read();
            load.set(Load::Loading);
            let id = source.clone();
            spawn(async move {
                let Some(made) = store.model.peek().instance(&id).cloned() else {
                    load.set(Load::Failed(Problem::plain(tr("Diese Instanz gibt es nicht mehr."))));
                    return;
                };
                match off_ui(move || remote::mod_updates(&made)).await {
                    Some(Ok(list)) => {
                        chosen.set(list.iter().map(|u| u.current.project_id.clone()).collect());
                        load.set(Load::Ready(list));
                    }
                    Some(Err(e)) => load.set(Load::Failed(forms::explain_as(tr("Updates nicht geladen."), &e))),
                    None => load.set(Load::Failed(Problem::plain(forms::unexplained()))),
                }
            });
        });

        let name = store.model.read().instance(&self.id).map(|i| i.name.clone()).unwrap_or_default();
        let running = is_running(store, &self.id);
        let saving = store.ui.instance.busy.read().contains_key(&self.id);
        let state = load.read().clone();
        let picked = chosen.read().clone();
        let open_now = open.read().clone();
        let busy = *applying.read();
        let n = chosen_of(&state, &picked).len();

        let body: Element = match &state {
            Load::Loading => rect().width(Size::fill()).padding((space::L, 0.)).center().child(secondary(&t, tr("Updates werden gesucht …"))).into_element(),
            Load::Failed(problem) => problem_view(&t, "instance.mods.updates.load", Icon::Wifi, problem, details),
            Load::Ready(list) if list.is_empty() => rect()
                .width(Size::fill())
                .padding((space::L, 0.))
                .horizontal()
                .center()
                .spacing(space::S)
                .child(icon(&t, Icon::Check, iconsize::M, t.success_text))
                .child(line(&t, tr("Alle Mods sind aktuell."), font::BODY, t.text1))
                .into_element(),
            Load::Ready(list) => {
                let all = picked.len() == list.len();
                let every: HashSet<String> = list.iter().map(|u| u.current.project_id.clone()).collect();
                let count = if list.len() == 1 { tr("1 Update") } else { trf("%lld Updates", &[&list.len()]) };
                let toggle = Button::new("instance.mods.updates.all", if all { tr("Keine auswählen") } else { tr("Alle auswählen") }).plain().disabled(busy.then(|| tr("Wird aktualisiert …"))).on_press(move |_| {
                    chosen.set(if all { HashSet::new() } else { every.clone() });
                });
                let mut rows = rect().width(Size::fill());
                for (at, u) in list.iter().enumerate() {
                    if at > 0 {
                        rows = rows.child(rect().width(Size::fill()).height(Size::px(1.)).background(t.separator));
                    }
                    rows = rows.child(update_row(&t, at, u, picked.contains(&u.current.project_id), open_now.contains(&u.current.project_id), busy, chosen, open));
                }
                let mut column = rect()
                    .width(Size::fill())
                    .spacing(space::S)
                    .child(rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).content(Content::flex()).child(strong(&t, count, font::CALLOUT, t.text2).width(Size::flex(1.))).child(toggle))
                    .child(
                        rect().width(Size::fill()).corner_radius(t.shape.tile).border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner)).overflow(Overflow::Clip).child(ScrollView::new().max_height(Size::px(LIST_MAX)).height(Size::auto()).child(rows)),
                    );
                if running {
                    column = column.child(caption(&t, tr("Gilt beim nächsten Start.")));
                }
                column.into_element()
            }
        };

        let mut content = rect().width(Size::fill()).spacing(space::M).child(body);
        if let Some(problem) = error.read().clone() {
            content = content.child(problem_view(&t, "instance.mods.updates.apply", Icon::Alert, &problem, details));
        }

        let selection = chosen_of(&state, &picked);
        let apply = {
            let id = self.id.clone();
            move || {
                if selection.is_empty() || *applying.peek() {
                    return;
                }
                let (job, work) = (selection.clone(), id.clone());
                snapshots::before(store, &id, Reason::ModUpdates(job.len()), move || apply_now(store, work, job, applying, error));
            }
        };
        let mut card = Card::new("instance.mods.updates", tr("Mod-Updates")).message(name).body(content).width(frame::SHEET * 1.25);
        card = match &state {
            Load::Failed(_) => card.primary(Primary { label: tr("Erneut versuchen"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| {
                let next = asked.peek().wrapping_add(1);
                asked.set(next);
            }) }),
            Load::Ready(list) if list.is_empty() => card.primary(Primary { label: tr("Fertig"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(ui)) }),
            Load::Loading => card,
            Load::Ready(_) => {
                let label = match n {
                    0 => tr("Aktualisieren"),
                    1 => tr("1 Mod aktualisieren"),
                    n => trf("%lld Mods aktualisieren", &[&n]),
                };
                let why = if saving {
                    Some(tr("Wird gerade gesichert …"))
                } else if busy {
                    Some(tr("Wird aktualisiert …"))
                } else if n == 0 {
                    Some(tr("Wähle zuerst Mods aus."))
                } else {
                    None
                };
                card.secondary(Primary { label: tr("Abbrechen"), disabled: busy.then(|| tr("Wird aktualisiert …")), danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(ui)) })
                    .primary(Primary { label, disabled: why, danger: false, busy, run: EventHandler::new(move |_| apply()) })
            }
        };
        card
    }
}

#[allow(clippy::too_many_arguments)]
fn update_row(t: &Theme, at: usize, u: &Update, on: bool, open_now: bool, busy: bool, mut chosen: State<HashSet<String>>, mut open: State<HashSet<String>>) -> Element {
    let pid = u.current.project_id.clone();
    let has_notes = u.changelog.iter().any(|(_, text)| !text.trim().is_empty());
    let (flip, fold, pick) = (pid.clone(), pid.clone(), pid.clone());
    let head = rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::M)
        .content(Content::flex())
        .child(Switch::new(format!("instance.mods.updates.on.{at}"), u.current.title.clone(), on).disabled(busy).on_toggle(move |now| {
            let mut set = chosen.peek().clone();
            if now {
                set.insert(pick.clone());
            } else {
                set.remove(&pick);
            }
            chosen.set(set);
        }))
        .child(
            rect()
                .width(Size::flex(1.))
                .cursor(CursorIcon::Pointer)
                .spacing(1.)
                .on_press(move |e: Event<PressEventData>| {
                    e.stop_propagation();
                    if busy {
                        return;
                    }
                    let mut set = chosen.peek().clone();
                    if !set.remove(&flip) {
                        set.insert(flip.clone());
                    }
                    chosen.set(set);
                })
                .child(line(t, u.current.title.clone(), font::BODY, t.text1).font_weight(FontWeight::MEDIUM).width(Size::fill()))
                .child(caption(t, format!("{} → {}", u.current.file_name, u.latest.file_name)).width(Size::fill())),
        );
    let head = if has_notes {
        head.child(
            Button::new(format!("instance.mods.updates.notes.{at}"), tr("Änderungen")).plain().glyph(if open_now { Icon::ChevronDown } else { Icon::ChevronRight }).on_press(move |_| {
                let mut set = open.peek().clone();
                if !set.remove(&fold) {
                    set.insert(fold.clone());
                }
                open.set(set);
            }),
        )
    } else {
        head
    };
    let mut row = rect().width(Size::fill()).padding((space::S, space::M)).child(head);
    if open_now && has_notes {
        row = row.child(notes(t, u));
    }
    row.into_element()
}

fn notes(t: &Theme, u: &Update) -> Element {
    let mut col = rect().width(Size::fill()).margin((space::XS, 0., 0., control::S + space::M)).padding(space::M).corner_radius(t.shape.control).background(t.raised).spacing(space::XS);
    let mut shown = 0;
    for (version, text) in &u.changelog {
        if shown >= NOTES_MAX {
            break;
        }
        col = col.child(strong(t, version.clone(), font::CALLOUT, t.text1).width(Size::fill()));
        shown += 1;
        let blocks = parse(text);
        if blocks.is_empty() {
            col = col.child(caption(t, tr("Keine Angaben.")));
        }
        for block in blocks {
            if shown >= NOTES_MAX {
                col = col.child(caption(t, tr("Mehr auf Modrinth.")));
                break;
            }
            shown += 1;
            col = col.child(block_view(t, block));
        }
    }
    col.into_element()
}

fn runs(t: &Theme, spans: Vec<Span>, color: Color, weight: FontWeight) -> Element {
    let ink = t.text1;
    paragraph()
        .width(Size::fill())
        .font_size(font::CALLOUT)
        .color(color)
        .font_weight(weight)
        .line_height(font::LINE)
        .spans_iter(spans.into_iter().map(move |s| match s {
            Span::Plain(x) => Piece::new(x),
            Span::Bold(x) => Piece::new(x).font_weight(FontWeight::SEMI_BOLD).color(ink),
            Span::Code(x) => faces::MONO.iter().fold(Piece::new(x), |p, f| p.font_family(*f)),
        }))
        .into_element()
}

fn block_view(t: &Theme, block: Block) -> Element {
    match block {
        Block::Heading(spans) => runs(t, spans, t.text1, FontWeight::SEMI_BOLD),
        Block::Bullet(depth, spans) => rect()
            .width(Size::fill())
            .horizontal()
            .spacing(space::XS)
            .padding((0., 0., 0., space::M * depth as f32))
            .content(Content::flex())
            .child(line(t, "•", font::CALLOUT, t.text3))
            .child(rect().width(Size::flex(1.)).child(runs(t, spans, t.text2, FontWeight::NORMAL)))
            .into_element(),
        Block::Text(spans) => runs(t, spans, t.text2, FontWeight::NORMAL),
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum Span {
    Plain(String),
    Bold(String),
    Code(String),
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum Block {
    Heading(Vec<Span>),
    Bullet(usize, Vec<Span>),
    Text(Vec<Span>),
}

fn from(s: &str, at: usize) -> &str {
    s.get(at..).unwrap_or_default()
}

fn upto(s: &str, to: usize) -> &str {
    s.get(..to).unwrap_or_default()
}

fn between(s: &str, a: usize, b: usize) -> &str {
    s.get(a..b).unwrap_or_default()
}

fn parse(markdown: &str) -> Vec<Block> {
    let mut out = Vec::new();
    for raw in markdown.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.is_empty() || line.chars().all(|c| matches!(c, '-' | '*' | '_' | '=')) || line.starts_with("<!--") || line.starts_with("```") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            out.push(Block::Heading(spans(rest.trim_start_matches('#').trim())));
        } else if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).or_else(|| line.strip_prefix("+ ")) {
            out.push(Block::Bullet(indent / 2, spans(rest)));
        } else if let Some((_, rest)) = line.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
            out.push(Block::Bullet(indent / 2, spans(rest)));
        } else {
            out.push(Block::Text(spans(line.trim_start_matches('>').trim())));
        }
    }
    out
}

fn spans(text: &str) -> Vec<Span> {
    let text = links(text);
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let marker = ["**", "__", "`"].into_iter().find(|m| rest.starts_with(m));
        if let Some(m) = marker
            && let Some(end) = from(rest, m.len()).find(m)
            && end > 0
        {
            if !plain.is_empty() {
                out.push(Span::Plain(std::mem::take(&mut plain)));
            }
            let inner = between(rest, m.len(), m.len() + end).to_owned();
            out.push(if m == "`" { Span::Code(inner) } else { Span::Bold(inner) });
            rest = from(rest, m.len() * 2 + end);
            continue;
        }
        let c = rest.chars().next().unwrap_or_default();
        let next = from(rest, c.len_utf8());
        let opens = (c == '*' || c == '_') && !plain.chars().last().is_some_and(char::is_alphanumeric) && next.chars().next().is_some_and(|n| !n.is_whitespace());
        let close = opens
            .then(|| next.match_indices(c).map(|(at, _)| at + 1).find(|&at| at > 1 && !upto(rest, at).chars().next_back().is_some_and(char::is_whitespace) && !from(rest, at + 1).chars().next().is_some_and(char::is_alphanumeric)))
            .flatten();
        if let Some(close) = close {
            plain.push_str(between(rest, 1, close));
            rest = from(rest, close + 1);
            continue;
        }
        plain.push(c);
        rest = next;
    }
    if !plain.is_empty() {
        out.push(Span::Plain(plain));
    }
    out
}

fn links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = from(rest, open + 1);
        match after.find("](").and_then(|mid| from(after, mid + 2).find(')').map(|close| (mid, mid + 2 + close))) {
            Some((mid, close)) if !upto(after, mid).contains('[') => {
                out.push_str(upto(rest, open).trim_end_matches('!'));
                out.push_str(upto(after, mid));
                rest = from(after, close + 1);
            }
            _ => {
                out.push_str(upto(rest, open + 1));
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{Block, Span, links, parse, spans};

    #[test]
    fn headings_bullets_and_text_become_blocks() {
        let got = parse("## Fixes\n\n- crash on join\n  - nested\n1. first\nplain line\n---\n> quoted");
        assert_eq!(
            got,
            vec![
                Block::Heading(vec![Span::Plain("Fixes".into())]),
                Block::Bullet(0, vec![Span::Plain("crash on join".into())]),
                Block::Bullet(1, vec![Span::Plain("nested".into())]),
                Block::Bullet(0, vec![Span::Plain("first".into())]),
                Block::Text(vec![Span::Plain("plain line".into())]),
                Block::Text(vec![Span::Plain("quoted".into())]),
            ]
        );
    }

    #[test]
    fn bold_code_and_links_render_as_spans() {
        assert_eq!(spans("**Fix** `foo()` in [Sodium](https://x.y/z) now"), vec![Span::Bold("Fix".into()), Span::Plain(" ".into()), Span::Code("foo()".into()), Span::Plain(" in Sodium now".into())]);
        assert_eq!(links("![img](a.png) and [x]"), "img and [x]");
        assert_eq!(spans("a * b and unclosed **bold"), vec![Span::Plain("a * b and unclosed **bold".into())]);
        for odd in ["2 ** 3", "note **", "__ init"] {
            assert_eq!(spans(odd), vec![Span::Plain(odd.into())]);
        }
        assert_eq!(spans("fixed mod_menu and snake_case_name, 2*3, *new* and _über_ ü*"), vec![Span::Plain("fixed mod_menu and snake_case_name, 2*3, new and über ü*".into())]);
    }

    #[test]
    fn empty_and_rule_only_notes_give_nothing() {
        assert!(parse("\n---\n\n").is_empty());
        assert!(parse("```\n```").is_empty());
    }

    #[test]
    fn odd_text_never_panics() {
        for odd in ["[ü](", "**ü**", "_ü_x_", "[[a](b)", "`", "*", "ü[", "[a](ü)"] {
            let _ = spans(odd);
            let _ = links(odd);
        }
    }
}