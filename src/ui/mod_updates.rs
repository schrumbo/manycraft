//! "Updates prüfen": the Modrinth updates of an instance's mods with what changed, chosen and applied in one step, undoable.

use std::collections::HashSet;

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::prelude::*;
use gpui_kit::*;

use gpui_kit::base::actions::{Cancel, Confirm};
use super::batch_sheet::Load;
use super::drive::Mark;
use super::forms;
use super::model;
use super::motion::Soft;
use super::{remote, snapshots};
use super::sheet::{self, Dismissed, Look};
use super::theme::{Theme, control, edge, font, iconsize, radius, space, tint};
use super::widgets::{KeyNav, icon};
use crate::core::i18n::{tr, trf};
use crate::core::modrinth::Update;
use crate::core::snapshot::Reason;

const NOTES_MAX: usize = 40;
const LIST_MAX: f32 = 360.;

/// Opens the updates sheet for instance `id`.
pub fn open(id: &str, window: &mut Window, cx: &mut App) {
    let id = id.to_owned();
    let view = cx.new(|cx| {
        let mut s = ModUpdates { id, load: Load::Loading, chosen: HashSet::new(), open: HashSet::new(), applying: false, error: None, details: false, ticket: 0, root: cx.focus_handle() };
        s.reload(cx);
        s
    });
    sheet::show(view, sheet::WIDE, window, cx);
}

struct ModUpdates {
    id: String,
    load: Load<Vec<Update>>,
    chosen: HashSet<String>,
    open: HashSet<String>,
    applying: bool,
    error: Option<forms::Problem>,
    details: bool,
    ticket: u64,
    root: FocusHandle,
}

impl EventEmitter<Dismissed> for ModUpdates {}

impl ModUpdates {
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(made) = model::get(cx).read(cx).instance(&self.id).cloned() else { return };
        self.load = Load::Loading;
        self.ticket += 1;
        let ticket = self.ticket;
        let task = cx.background_spawn(async move { remote::mod_updates(&made) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.ticket != ticket {
                    return;
                }
                s.load = match result {
                    Ok(list) => {
                        s.chosen = list.iter().map(|u| u.current.project_id.clone()).collect();
                        Load::Ready(list)
                    }
                    Err(e) => Load::Failed(forms::explain_as(tr("Updates nicht geladen."), &e)),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn selection(&self) -> Vec<Update> {
        match &self.load {
            Load::Ready(list) => list.iter().filter(|u| self.chosen.contains(&u.current.project_id)).cloned().collect(),
            _ => Vec::new(),
        }
    }

    fn running(&self, cx: &App) -> bool {
        model::get(cx).read(cx).games.values().any(|g| g.instance_id == self.id && g.phase.is_active())
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let n = self.selection().len();
        if n == 0 || self.applying {
            return;
        }
        let sheet = cx.entity().downgrade();
        snapshots::before(&self.id.clone(), Reason::ModUpdates(n), cx, move |cx| {
            let _ = sheet.update(cx, |s, cx| s.apply_now(cx));
        });
    }

    fn apply_now(&mut self, cx: &mut Context<Self>) {
        let selection = self.selection();
        if selection.is_empty() || self.applying {
            return;
        }
        self.applying = true;
        self.error = None;
        cx.notify();
        let (id, running) = (self.id.clone(), self.running(cx));
        let sent = selection.clone();
        let task = cx.background_spawn(async move { remote::apply_updates(&id, &sent, running) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.applying = false;
                match result {
                    Ok(made) => {
                        applied(made, selection, cx);
                        cx.emit(Dismissed);
                    }
                    Err(e) => s.error = Some(forms::explain_as(tr("Nicht aktualisiert."), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let Load::Ready(list) = &self.load else { return };
        if self.chosen.len() == list.len() {
            self.chosen.clear();
        } else {
            self.chosen = list.iter().map(|u| u.current.project_id.clone()).collect();
        }
        cx.notify();
    }

    fn render_row(&self, at: usize, u: &Update, cx: &mut Context<Self>, t: &Theme) -> Div {
        let pid = u.current.project_id.clone();
        let on = self.chosen.contains(&pid);
        let open = self.open.contains(&pid);
        let has_notes = u.changelog.iter().any(|(_, text)| !text.trim().is_empty());
        let versions = format!("{} → {}", u.current.file_name, u.latest.file_name);
        let pick = pid.clone();
        let check = Checkbox::new(SharedString::from(format!("update-on-{at}"))).checked(on).tab_stop(true).on_click(cx.listener(move |s, on: &bool, _, cx| {
            if *on {
                s.chosen.insert(pick.clone());
            } else {
                s.chosen.remove(&pick);
            }
            cx.notify();
        }));
        let fold = pid.clone();
        let disclosure = has_notes.then(|| {
            div()
                .id(SharedString::from(format!("update-notes-{at}")))
                .mark("button", tr("Änderungen"))
                .flex_none()
                .h(px(control::S))
                .px(px(space::S))
                .flex()
                .items_center()
                .gap(px(space::XXS))
                .rounded(px(radius::SMALL))
                .cursor_pointer()
                .key_nav()
                .soft(t.ink.opacity(tint::HOVER), radius::SMALL)
                .text_size(px(font::CALLOUT))
                .text_color(t.secondary)
                .child(tr("Änderungen"))
                .child(icon(if open { "chevron-down" } else { "chevron-right" }, iconsize::XS, t.tertiary))
                .on_click(cx.listener(move |s, _, _, cx| {
                    if !s.open.remove(&fold) {
                        s.open.insert(fold.clone());
                    }
                    cx.notify();
                }))
        });
        let flip = pid.clone();
        let head = div()
            .flex()
            .items_center()
            .gap(px(space::M))
            .child(div().id(SharedString::from(format!("update-check-{at}"))).mark("checkbox", u.current.title.clone()).flex_none().child(check))
            .child(div().id(SharedString::from(format!("update-head-{at}"))).flex_1().min_w_0().px(px(space::XS)).cursor_pointer().soft(t.ink.opacity(tint::HOVER), radius::SMALL).on_click(cx.listener(move |s, _, _, cx| {
                if s.applying {
                    return;
                }
                if !s.chosen.remove(&flip) {
                    s.chosen.insert(flip.clone());
                }
                cx.notify();
            })).flex().flex_col().child(div().truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).child(u.current.title.clone())).child(div().truncate().text_size(px(font::CAPTION)).text_color(t.tertiary).child(versions)))
            .children(disclosure);
        let notes = (open && has_notes).then(|| {
            let mut col = div().ml(px(control::S + space::M)).mt(px(space::XS)).p(px(space::M)).rounded(px(radius::INNER)).bg(t.raised).flex().flex_col().gap(px(space::XS));
            let mut shown = 0;
            for (version, text) in &u.changelog {
                if shown >= NOTES_MAX {
                    break;
                }
                col = col.child(div().text_size(px(font::CALLOUT)).font_weight(FontWeight::SEMIBOLD).child(version.clone()));
                shown += 1;
                let blocks = parse(text);
                if blocks.is_empty() {
                    col = col.child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(tr("Keine Angaben.")));
                }
                for block in blocks {
                    if shown >= NOTES_MAX {
                        col = col.child(div().text_size(px(font::CALLOUT)).text_color(t.tertiary).child(tr("Mehr auf Modrinth.")));
                        break;
                    }
                    shown += 1;
                    col = col.child(render_block(block, t));
                }
            }
            col
        });
        div().py(px(space::S)).px(px(space::M)).flex().flex_col().child(head).children(notes)
    }
}

fn render_block(block: Block, t: &Theme) -> Div {
    let line = |spans: Vec<Span>| {
        let mut text = div().flex().flex_wrap().text_size(px(font::CALLOUT)).text_color(t.secondary);
        for span in spans {
            text = text.child(match span {
                Span::Plain(s) => div().child(s),
                Span::Bold(s) => div().font_weight(FontWeight::SEMIBOLD).text_color(t.primary).child(s),
                Span::Code(s) => div().px(px(space::XXS)).rounded(px(radius::SMALL)).bg(t.separator).font_family(Theme::mono_family()).child(s),
            });
        }
        text
    };
    match block {
        Block::Heading(spans) => line(spans).font_weight(FontWeight::SEMIBOLD).text_color(t.primary),
        Block::Bullet(depth, spans) => div().flex().gap(px(space::XS)).pl(px(space::M * depth as f32)).child(div().flex_none().text_size(px(font::CALLOUT)).text_color(t.tertiary).child("•")).child(div().flex_1().min_w_0().child(line(spans))),
        Block::Text(spans) => line(spans),
    }
}

impl Render for ModUpdates {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            window.focus(&self.root, cx);
        }
        let t = Theme::get(cx);
        let name = model::get(cx).read(cx).instance(&self.id).map(|i| i.name.clone());
        let running = self.running(cx);
        let body = match &self.load {
            Load::Loading => div().py(px(space::L)).child(sheet::waiting(tr("Updates werden gesucht …"), &t)),
            Load::Failed(problem) => {
                let problem = problem.clone();
                div().child(sheet::problem("updates-load", "wifi-off", t.danger_text, &problem, self.details, cx.listener(|s, _, _, cx| {
                    s.details = !s.details;
                    cx.notify();
                }), &t))
            }
            Load::Ready(list) if list.is_empty() => div().py(px(space::L)).child(sheet::status("circle-check", t.success_text, tr("Alle Mods sind aktuell."))),
            Load::Ready(list) => {
                let list = list.clone();
                let all = self.chosen.len() == list.len();
                let toggle = sheet::button("updates-toggle", if all { tr("Keine auswählen") } else { tr("Alle auswählen") }, None, Look::Plain, self.applying, false, &t).on_click(cx.listener(|s, _, _, cx| s.toggle_all(cx)));
                let count = if list.len() == 1 { tr("1 Update") } else { trf("%lld Updates", &[&list.len()]) };
                let mut rows = div().id("updates-list").flex().flex_col().max_h(px(LIST_MAX)).overflow_y_scroll().rounded(px(radius::CONTROL)).border_1().border_color(t.separator);
                for (at, u) in list.iter().enumerate() {
                    if at > 0 {
                        rows = rows.child(div().h(px(edge::RING)).mx(px(space::M)).bg(t.separator));
                    }
                    rows = rows.child(self.render_row(at, u, cx, &t));
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::S))
                    .child(div().flex().items_center().child(div().flex_1().child(sheet::label(count, &t))).child(toggle))
                    .child(rows)
                    .when(running, |d| d.child(div().text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Gilt beim nächsten Start."))))
            }
        };
        let error = self.error.clone().map(|e| {
            div().px(px(space::XL)).pb(px(space::M)).child(sheet::problem("updates-apply", "circle-alert", t.danger_text, &e, self.details, cx.listener(|s, _, _, cx| {
                s.details = !s.details;
                cx.notify();
            }), &t))
        });
        let n = self.selection().len();
        let cancel = sheet::button("updates-cancel", tr("Abbrechen"), None, Look::Secondary, self.applying, false, &t).on_click(cx.listener(|s, _, _, cx| {
            if !s.applying {
                cx.emit(Dismissed);
            }
        }));
        let main = match &self.load {
            Load::Failed(_) => sheet::button("updates-retry", tr("Erneut versuchen"), None, Look::Primary, false, false, &t).on_click(cx.listener(|s, _, _, cx| s.reload(cx))),
            Load::Ready(list) if list.is_empty() => sheet::button("updates-done", tr("Fertig"), None, Look::Primary, false, false, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
            _ => {
                let saving = snapshots::progress(&self.id, cx).is_some();
                sheet::button("updates-apply", if n == 0 { tr("Aktualisieren") } else if n == 1 { tr("1 Mod aktualisieren") } else { trf("%lld Mods aktualisieren", &[&n]) }, None, Look::Primary, n == 0 || saving, self.applying || saving, &t).when(!saving, |b| b.on_click(cx.listener(|s, _, _, cx| s.apply(cx))))
            }
        };
        div()
            .id("updates-sheet")
            .track_focus(&self.root)
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .on_action(cx.listener(|s, _: &Confirm, window, cx| {
                if s.root.is_focused(window) && !s.selection().is_empty() {
                    s.apply(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|s, _: &Cancel, _, cx| {
                if !s.applying {
                    cx.propagate();
                }
            }))
            .child(sheet::header(tr("Mod-Updates"), name, &t))
            .child(div().px(px(space::XL)).pb(px(space::L)).child(body))
            .children(error)
            .child(sheet::footer(Vec::new(), vec![cancel.into_any_element(), main.into_any_element()], &t))
    }
}

/// Puts the mods of instance `id` back on the files they had before `updates`; says why when it cannot.
/// Shows the instance `made` after `selection` was applied to it everywhere and offers to undo the updates.
pub fn applied(made: crate::core::instance::Instance, selection: Vec<Update>, cx: &mut App) {
    let label = match selection.as_slice() {
        [one] => trf("„%@“", &[&one.current.title]),
        many => trf("%lld Mods", &[&many.len()]),
    };
    let id = made.id.clone();
    super::mod_list::put(made, cx);
    super::undo::offer(super::undo::Kind::ModsUpdated { instance_id: id, updates: selection, label }, cx);
}

pub fn undo_updates(id: String, updates: Vec<Update>, label: String, at: std::time::Instant, cx: &mut App) {
    let back: Vec<Update> = updates.iter().map(|u| Update { current: u.latest.clone(), latest: u.current.clone(), changelog: Vec::new() }).collect();
    let running = model::get(cx).read(cx).games.values().any(|g| g.instance_id == id && g.phase.is_active());
    let task = cx.background_spawn({
        let id = id.clone();
        async move { remote::apply_updates(&id, &back, running) }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(made) => super::mod_list::put(made, cx),
            Err(e) => {
                super::undo::note(forms::explain_as(tr("Nicht zurückgestellt."), &e).text, cx);
                if !super::undo::permanent(&e) {
                    super::undo::retry(super::undo::Kind::ModsUpdated { instance_id: id, updates, label }, at, cx);
                }
            }
        });
    })
    .detach();
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
        } else if let Some((n, rest)) = line.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
            let _ = n;
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
            && let Some(end) = rest[m.len()..].find(m)
            && end > 0
        {
            if !plain.is_empty() {
                out.push(Span::Plain(std::mem::take(&mut plain)));
            }
            let inner = rest[m.len()..m.len() + end].to_owned();
            out.push(if m == "`" { Span::Code(inner) } else { Span::Bold(inner) });
            rest = &rest[m.len() * 2 + end..];
            continue;
        }
        let c = rest.chars().next().unwrap_or_default();
        let opens = (c == '*' || c == '_') && !plain.chars().last().is_some_and(char::is_alphanumeric) && rest[1..].chars().next().is_some_and(|n| !n.is_whitespace());
        let close = opens.then(|| rest[1..].match_indices(c).map(|(at, _)| at + 1).find(|&at| at > 1 && !rest[..at].chars().next_back().is_some_and(char::is_whitespace) && !rest[at + 1..].chars().next().is_some_and(char::is_alphanumeric))).flatten();
        if let Some(close) = close {
            plain.push_str(&rest[1..close]);
            rest = &rest[close + 1..];
            continue;
        }
        plain.push(c);
        rest = &rest[c.len_utf8()..];
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
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|close| (mid, mid + 2 + close))) {
            Some((mid, close)) if !after[..mid].contains('[') => {
                out.push_str(rest[..open].trim_end_matches('!'));
                out.push_str(&after[..mid]);
                rest = &after[close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
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
        assert_eq!(got, vec![
            Block::Heading(vec![Span::Plain("Fixes".into())]),
            Block::Bullet(0, vec![Span::Plain("crash on join".into())]),
            Block::Bullet(1, vec![Span::Plain("nested".into())]),
            Block::Bullet(0, vec![Span::Plain("first".into())]),
            Block::Text(vec![Span::Plain("plain line".into())]),
            Block::Text(vec![Span::Plain("quoted".into())]),
        ]);
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
}
