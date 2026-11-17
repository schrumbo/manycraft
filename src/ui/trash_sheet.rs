//! "Papierkorb": what was deleted or replaced in the last seven days, across every instance, put back or deleted for good.

use std::collections::HashSet;

use gpui_kit::prelude::*;
use gpui_kit::*;

use gpui_kit::base::actions::Cancel;
use super::actions::OpenTrash;
use super::batch_sheet::Load;
use super::drive::Mark;
use super::instance_files::{self as files, FileUndo};
use super::sheet::{self, Dismissed, Look};
use super::theme::{self, Theme, edge, font, iconsize, radius, space};
use super::undo::{self, Kind};
use super::widgets::icon;
use super::{forms, model};
use crate::core::bin::{self, Item, What};
use crate::core::i18n::{tr, trf};

const RECHECK: std::time::Duration = std::time::Duration::from_millis(600);

/// Registers "Papierkorb öffnen".
pub fn init(cx: &mut App) {
    cx.on_action(|_: &OpenTrash, cx| sheet::in_window(cx, open));
}

/// Opens the Papierkorb sheet.
pub fn open(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let mut s = TrashSheet { load: Load::Loading, taken: HashSet::new(), emptying: false, ticket: 0, root: cx.focus_handle() };
        s.reload(cx);
        s
    });
    sheet::show(view, sheet::WIDE, window, cx);
}

struct TrashSheet {
    load: Load<Vec<Item>>,
    taken: HashSet<String>,
    emptying: bool,
    ticket: u64,
    root: FocusHandle,
}

impl EventEmitter<Dismissed> for TrashSheet {}

impl TrashSheet {
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.ticket += 1;
        let ticket = self.ticket;
        let task = cx.background_spawn(async { bin::list() });
        cx.spawn(async move |this, cx| {
            let items = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.ticket == ticket {
                    s.taken.clear();
                    s.load = match items {
                        Ok(items) => Load::Ready(items),
                        Err(e) => Load::Failed(forms::explain_as(tr("Der Papierkorb lässt sich nicht lesen."), &e)),
                    };
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn put_back(&mut self, item: &Item, cx: &mut Context<Self>) {
        let Some(kind) = undo_kind(item, cx) else { return };
        if matches!(item.what, What::File { taken: true, .. }) && super::snapshots::running(&item.instance, cx) {
            undo::note(tr("Stoppe zuerst die laufenden Spiele."), cx);
            return;
        }
        self.taken.insert(key(item));
        cx.notify();
        undo::take_back(kind, cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RECHECK).await;
            let _ = this.update(cx, |s, cx| s.reload(cx));
        })
        .detach();
    }

    fn ask_empty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Load::Ready(items) = &self.load else { return };
        if items.is_empty() || self.emptying {
            return;
        }
        let size: u64 = items.iter().map(|i| i.size).sum();
        let message = if items.len() == 1 { trf("Ein Eintrag (%@) wird endgültig gelöscht.", &[&files::bytes(size)]) } else { trf("%1$lld Einträge (%2$@) werden endgültig gelöscht.", &[&items.len(), &files::bytes(size)]) };
        let this = cx.entity().downgrade();
        sheet::ask(window, cx, tr("Papierkorb leeren?"), message, tr("Leeren"), move |_, cx| {
            let _ = this.update(cx, |s, cx| s.empty(cx));
        });
    }

    fn empty(&mut self, cx: &mut Context<Self>) {
        self.emptying = true;
        cx.notify();
        let task = cx.background_spawn(async { bin::empty() });
        cx.spawn(async move |this, cx| {
            let (_, result) = task.await;
            let _ = this.update(cx, |s, cx| {
                s.emptying = false;
                if let Err(e) = result {
                    undo::note(forms::explain_as(tr("Nicht alles wurde gelöscht."), &e).text, cx);
                }
                s.reload(cx);
            });
        })
        .detach();
    }

    fn render_row(&self, at: usize, item: &Item, cx: &mut Context<Self>, t: &Theme) -> Stateful<Div> {
        let (symbol, title) = look(item);
        let mut line = Vec::new();
        if !item.name.is_empty() && !matches!(item.what, What::Instance) {
            line.push(item.name.clone());
        }
        line.push(files::when(i64::try_from(item.at / 1000).unwrap_or(i64::MAX)));
        line.push(files::bytes(item.size));
        let can = undo_kind(item, cx).is_some();
        let taken = self.taken.contains(&key(item));
        let button = can.then(|| {
            let item = item.clone();
            sheet::button(SharedString::from(format!("trash-back-{at}")), tr("Zurücklegen"), None, Look::Plain, taken || self.emptying, taken, t).on_click(cx.listener(move |s, _, _, cx| s.put_back(&item, cx)))
        });
        div()
            .id(SharedString::from(format!("trash-row-{at}")))
            .mark("row", title.clone())
            .min_h(px(theme::files::SNAPSHOT_ROW))
            .px(px(space::M))
            .py(px(space::XS))
            .flex()
            .items_center()
            .gap(px(space::M))
            .child(icon(symbol, iconsize::M, t.tertiary))
            .child(div().flex_1().min_w_0().flex().flex_col().child(div().truncate().text_size(px(font::BODY)).child(title)).child(div().truncate().text_size(px(font::CAPTION)).font_features(super::toolbar::tabular()).text_color(t.secondary).child(line.join(" · "))))
            .children(button)
    }
}

fn key(item: &Item) -> String {
    format!("{}/{}", item.instance, item.entry)
}

fn look(item: &Item) -> (&'static str, String) {
    match &item.what {
        What::Instance => ("package", if item.name.is_empty() { tr("Instanz") } else { item.name.clone() }),
        What::Batch => ("layers", if item.name.is_empty() { tr("Batch") } else { item.name.clone() }),
        What::Reset(_) => ("rotate-ccw", tr("Stand vor dem Zurücksetzen")),
        What::Mods(Some(t)) if t.files.len() == 1 => ("puzzle", t.files[0].trim_end_matches(".disabled").to_owned()),
        What::Mods(t) => {
            let n = t.as_ref().map_or(0, |t| t.files.len());
            ("puzzle", if n == 0 { tr("Mods") } else { trf("%lld Mods", &[&n]) })
        }
        What::File { origin, .. } => {
            let parts: Vec<&str> = origin.split('/').collect();
            let last = parts.last().copied().unwrap_or_default().to_owned();
            match parts.as_slice() {
                ["games", _, "saves", _] => ("earth", last),
                ["games", _, "saves"] => ("earth", tr("Welten")),
                ["games", _, "screenshots", _] => ("image", last),
                ["mods"] => ("puzzle", tr("Mods")),
                ["config"] => ("folder", tr("Einstellungen der Mods")),
                ["resourcepacks"] => ("folder", tr("Ressourcenpakete")),
                ["shaderpacks"] => ("folder", tr("Shader")),
                _ => ("file", last),
            }
        }
    }
}

fn undo_kind(item: &Item, cx: &App) -> Option<Kind> {
    let (title, name) = (look(item).1, item.name.clone());
    match &item.what {
        What::Instance => Some(Kind::Deleted { id: item.instance.clone(), name: title, entry: item.entry.clone() }),
        What::Reset(Some(meta)) => {
            model::get(cx).read(cx).instance(&item.instance)?;
            Some(Kind::Reset { instance_id: item.instance.clone(), name, entry: item.entry.clone(), before: meta.before.clone(), after: meta.after.clone() })
        }
        What::Mods(Some(trashed)) => Some(Kind::ModsTrashed { instance_id: item.instance.clone(), trashed: trashed.clone(), label: title }),
        What::File { origin, taken: true } => {
            let world = files::instance_dir(&item.instance, cx)?.join(origin);
            Some(Kind::Files { name: title, undo: FileUndo::Restored { instance: item.instance.clone(), world, replaced: Some(item.entry.clone()) } })
        }
        What::File { .. } => Some(Kind::Files { name: title, undo: FileUndo::Deleted { instance: item.instance.clone(), entry: item.entry.clone() } }),
        What::Reset(None) | What::Mods(None) | What::Batch => None,
    }
}

impl Render for TrashSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            window.focus(&self.root, cx);
        }
        let t = Theme::get(cx);
        let (body, subtitle, any) = match &self.load {
            Load::Loading => (div().py(px(space::L)).child(sheet::waiting(tr("Wird geladen …"), &t)), None, false),
            Load::Failed(problem) => (div().py(px(space::L)).child(sheet::status("circle-alert", t.danger_text, problem.text.clone())), None, false),
            Load::Ready(items) if items.is_empty() => (div().py(px(space::L)).child(sheet::status("trash", t.secondary, tr("Der Papierkorb ist leer."))), Some(tr("Was du löschst oder ersetzt, bleibt hier 7 Tage.")), false),
            Load::Ready(items) => {
                let items = items.clone();
                let size: u64 = items.iter().map(|i| i.size).sum();
                let mut rows = div().id("trash-list").flex().flex_col().max_h(px(theme::files::TRASH_LIST)).overflow_y_scroll().rounded(px(radius::CONTROL)).border_1().border_color(t.separator);
                for (at, item) in items.iter().enumerate() {
                    if at > 0 {
                        rows = rows.child(div().h(px(edge::RING)).mx(px(space::M)).bg(t.separator));
                    }
                    rows = rows.child(self.render_row(at, item, cx, &t));
                }
                (div().child(rows), Some(trf("Bleibt 7 Tage, dann wird es gelöscht · %@", &[&files::bytes(size)])), true)
            }
        };
        let empty = if matches!(self.load, Load::Failed(_)) {
            Some(sheet::button("trash-retry", tr("Erneut versuchen"), Some("refresh-cw"), Look::Secondary, false, false, &t).on_click(cx.listener(|s, _, _, cx| {
                s.load = Load::Loading;
                s.reload(cx);
                cx.notify();
            })).into_any_element())
        } else {
            any.then(|| sheet::button("trash-empty", tr("Papierkorb leeren…"), None, Look::Danger, self.emptying, self.emptying, &t).on_click(cx.listener(|s, _, window, cx| s.ask_empty(window, cx))).into_any_element())
        };
        let done = sheet::button("trash-done", tr("Fertig"), None, Look::Primary, false, false, &t).on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        div()
            .id("trash-sheet")
            .track_focus(&self.root)
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.primary)
            .on_action(cx.listener(|s, _: &Cancel, _, cx| {
                if !s.emptying {
                    cx.propagate();
                }
            }))
            .child(sheet::header(tr("Papierkorb"), subtitle, &t))
            .child(div().px(px(space::XL)).pb(px(space::L)).child(body))
            .child(sheet::footer(empty.into_iter().collect(), vec![done.into_any_element()], &t))
    }
}
