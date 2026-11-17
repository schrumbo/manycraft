use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use freya::prelude::*;
use uuid::Uuid;

use super::logs_lines::{Line, capped, first_row, longest, option_label, ordered, scan, search, split};
use super::logs_viewer::{Hits, Viewer, Wrapped, row};
use super::shots_parts::{Same, centered};
use super::{Facts, use_extras};
use crate::core::i18n::{tr, trf};
use crate::core::logs::{self, Kind, LogFile};
use crate::ui::forms;
use crate::ui::theme::{files, tint};
use crate::view::commands::{Extra, Shortcut};
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::Button;
use crate::view::kit::field::{self, Field};
use crate::view::kit::select::Select;
use crate::view::kit::text::{caption, line, secondary, spacer, strong};
use crate::view::kit::{after, off_ui, sheet, toast, use_theme};
use crate::view::native;
use crate::view::store::use_store;
use crate::view::theme::{font, iconsize, space};

const FOLLOW: Duration = Duration::from_secs(1);
const OTHERS: usize = 8;

#[derive(Clone)]
struct Scan {
    loaded: bool,
    accounts: Vec<(Uuid, PathBuf)>,
    account: Option<Uuid>,
    files: Vec<LogFile>,
}

#[derive(Clone, PartialEq)]
enum Status {
    Loading,
    Ready,
    Failed(String),
}

#[derive(Clone)]
struct Doc {
    load: u64,
    status: Status,
    lines: Same<Vec<Line>>,
    version: u64,
    cut: bool,
    longest: usize,
    offset: u64,
    born: Option<SystemTime>,
}

impl Doc {
    fn blank(load: u64) -> Self {
        Self { load, status: Status::Loading, lines: Same(Arc::new(Vec::new())), version: 0, cut: false, longest: 0, offset: 0, born: None }
    }
}

#[derive(Clone, PartialEq)]
enum Upload {
    Sending,
    Sent(String),
    Failed(String),
}

#[derive(Clone)]
struct Found {
    list: Hits,
    at: usize,
    query: String,
}

impl Found {
    fn none() -> Self {
        Self { list: Same(Arc::new(Vec::new())), at: 0, query: String::new() }
    }

    fn line(&self) -> Option<usize> {
        self.list.0.get(self.at).map(|(line, _)| *line)
    }
}

#[derive(Clone, Copy)]
struct Ctx {
    doc: State<Doc>,
    stick: State<bool>,
    found: State<Found>,
    controller: ScrollController,
    wrap: State<bool>,
    cols: State<usize>,
    wrapped: State<Wrapped>,
}

/// Whether the log shows its last line.
pub(super) fn at_end(c: ScrollController) -> bool {
    match *c.bounds.peek() {
        Some((content, view)) => content.height <= view.height || -c.position().y >= content.height - view.height - row(),
        None => true,
    }
}

fn reveal(ctx: Ctx, line: usize) {
    let mut c = ctx.controller;
    let view = c.bounds.peek().map_or(0., |(_, view)| view.height);
    let cols = *ctx.cols.peek();
    let shown = if *ctx.wrap.peek() && cols > 0 { ctx.wrapped.peek().row_of(line, cols).unwrap_or_else(|| first_row(&ctx.doc.peek().lines.0, line, cols)) } else { line };
    let top = (shown as f32 * row() - (view - row()) / 2.).max(0.);
    c.scroll_to_y(-(top as i32));
}

fn scroll_by_key(mut c: ScrollController, key: &Key) {
    let view = c.bounds.peek().map_or(0., |(_, view)| view.height);
    let page = ((view / row()).floor() - 1.).max(1.) * row();
    let y = c.position().y;
    match key {
        Key::Named(NamedKey::PageUp) => {
            c.scroll_to_y((y + page) as i32);
        }
        Key::Named(NamedKey::PageDown) => {
            c.scroll_to_y((y - page) as i32);
        }
        Key::Named(NamedKey::Home) => {
            c.scroll_to_y(0);
        }
        Key::Named(NamedKey::End) => {
            c.scroll_to(ScrollPosition::End, Direction::Vertical);
        }
        _ => {}
    }
}

fn jump(ctx: Ctx, by: isize) {
    let mut found = ctx.found;
    let (n, at) = {
        let f = found.peek();
        (f.list.0.len(), f.at)
    };
    if n == 0 {
        return;
    }
    let next = (at as isize + by).rem_euclid(n as isize) as usize;
    found.write().at = next;
    let line = found.peek().line();
    if let Some(line) = line {
        reveal(ctx, line);
    }
}

fn read_log(path: &Path) -> anyhow::Result<(Vec<Line>, bool, usize, u64, Option<SystemTime>)> {
    let gz = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("gz"));
    let born = logs::born(path);
    let (text, offset) = if gz { (logs::read(path)?, 0) } else { logs::read_from(path, 0).map(|(_, text, next)| (text, next))? };
    let (lines, cut) = capped(split(&logs::redact(&text)), files::LOG_LINES);
    let widest = longest(&lines);
    Ok((lines, cut, widest, offset, born))
}

fn load_doc(ctx: Ctx, path: Option<PathBuf>) {
    let (mut doc, mut stick) = (ctx.doc, ctx.stick);
    let load = doc.peek().load.wrapping_add(1);
    let mut blank = Doc::blank(load);
    let Some(path) = path else {
        blank.status = Status::Ready;
        doc.set(blank);
        return;
    };
    doc.set(blank);
    spawn(async move {
        let result = off_ui(move || read_log(&path)).await;
        if doc.peek().load != load {
            return;
        }
        let what = tr("Das Protokoll lässt sich nicht lesen.");
        match result {
            Some(Ok((lines, cut, longest, offset, born))) => {
                doc.set(Doc { load, status: Status::Ready, lines: Same(Arc::new(lines)), version: 0, cut, longest, offset, born });
                let mut c = ctx.controller;
                c.scroll_to(ScrollPosition::End, Direction::Vertical);
                stick.set(true);
            }
            Some(Err(e)) => {
                let mut failed = Doc::blank(load);
                failed.status = Status::Failed(forms::explain_as(what, &e).text);
                doc.set(failed);
            }
            None => {
                let mut failed = Doc::blank(load);
                failed.status = Status::Failed(what);
                doc.set(failed);
            }
        }
    });
}

async fn catch_up(ctx: Ctx, load: u64, path: PathBuf) -> bool {
    let (mut doc, mut stick) = (ctx.doc, ctx.stick);
    let (offset, known) = {
        let d = doc.peek();
        if d.load != load {
            return false;
        }
        (d.offset, d.born)
    };
    let read = off_ui(move || {
        let born = logs::born(&path);
        let from = if born != known { 0 } else { offset };
        logs::read_from(&path, from).map(|(kept, text, next)| (kept.is_none() || from != offset, split(&logs::redact(&text)), next, born))
    })
    .await;
    if doc.peek().load != load {
        return false;
    }
    let Some(Ok((restarted, lines, next, born))) = read else { return true };
    if lines.is_empty() && !restarted && born == known {
        return true;
    }
    let end = at_end(ctx.controller);
    let widest = longest(&lines);
    let mut d = doc.write();
    d.born = born;
    d.offset = next;
    if restarted {
        d.lines = Same(Arc::new(Vec::new()));
        d.longest = 0;
    }
    let over = {
        let all = Arc::make_mut(&mut d.lines.0);
        all.extend(lines);
        let over = all.len().saturating_sub(files::LOG_LINES);
        if over > 0 {
            all.drain(..over);
        }
        over
    };
    if over > 0 {
        d.cut = true;
    }
    d.longest = d.longest.max(widest);
    d.version += 1;
    drop(d);
    if end {
        stick.set(true);
    }
    true
}

fn all_text(doc: State<Doc>) -> String {
    let d = doc.peek();
    let mut out = String::with_capacity(d.lines.0.iter().map(|l| l.text.len() + 1).sum());
    for line in d.lines.0.iter() {
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

fn name_of(names: &[(Uuid, String)], id: Uuid) -> String {
    names.iter().find(|(a, _)| *a == id).map_or_else(|| tr("Entfernter Account"), |(_, n)| n.clone())
}

/// Die Protokolle der Instanz.
#[derive(PartialEq)]
pub struct Logs {
    pub facts: Facts,
}

impl Component for Logs {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let f = &self.facts;
        let id = f.made.id.clone();
        let dir = f.made.dir();
        let changed = *store.ui.instance.changed.read();
        let playing: Vec<Uuid> = {
            let m = store.model.read();
            let mut now: Vec<Uuid> = m.games.values().filter(|g| g.instance_id == id && g.phase.is_active()).map(|g| g.account_id).collect();
            now.sort_unstable();
            now
        };

        let mut scanned = use_state(|| Scan { loaded: false, accounts: Vec::new(), account: None, files: Vec::new() });
        let mut file = use_state(|| None::<PathBuf>);
        let mut pick = use_state(|| None::<Uuid>);
        let doc = use_state(|| Doc::blank(0));
        let stick = use_state(|| false);
        let found = use_state(Found::none);
        let mut searching = use_state(|| 0u64);
        let mut find_open = use_state(|| false);
        let mut find_epoch = use_state(|| 0u32);
        let query = use_state(String::new);
        let mut copied = use_state(|| false);
        let mut narrow = use_state(|| false);
        let mut link_copied = use_state(|| false);
        let mut upload = use_state(|| None::<Upload>);
        let mut upload_ask = use_state(|| 0u32);
        let mut reload = use_state(|| 0u32);
        let controller = use_scroll_controller(ScrollConfig::default);
        let mut wrap = use_state(|| true);
        let cols = use_state(|| 0usize);
        let wrapped = use_state(Wrapped::default);
        let ctx = Ctx { doc, stick, found, controller, wrap, cols, wrapped };

        let (account_now, pick_now) = (scanned.read().account, *pick.read());
        let _ = account_now;
        use_side_effect_with_deps(&(dir, changed, playing.clone(), pick_now), move |(dir, _, playing, wanted)| {
            let wanted = wanted.or(scanned.peek().account);
            let (dir, playing) = (dir.clone(), playing.clone());
            spawn(async move {
                let result = off_ui(move || scan(&dir, &playing, wanted)).await;
                let (accounts, account, listed) = result.unwrap_or_default();
                let keep = file.peek().clone().filter(|p| listed.iter().any(|x| &x.path == p));
                let first = listed.first().map(|x| x.path.clone());
                scanned.set(Scan { loaded: true, accounts, account, files: listed });
                if keep.is_none() && *file.peek() != first {
                    file.set(first);
                }
            });
        });

        let file_now = file.read().clone();
        use_side_effect_with_deps(&(file_now.clone(), *reload.read()), move |(path, _)| {
            copied.set(false);
            upload.set(None);
            load_doc(ctx, path.clone());
        });

        let shown = scanned.read().clone();
        let running = shown.account.is_some_and(|a| playing.contains(&a));
        let latest = file_now.as_ref().is_some_and(|p| shown.files.iter().any(|x| &x.path == p && x.kind == Kind::Latest));
        let follow = {
            let d = doc.read();
            (running && latest && d.status == Status::Ready).then(|| file_now.clone().map(|p| (d.load, p))).flatten()
        };
        let mut follower = use_state(|| None::<TaskHandle>);
        use_side_effect_with_deps(&follow, move |follow| {
            if let Some(old) = follower.write().take() {
                old.cancel();
            }
            let Some((load, path)) = follow.clone() else { return };
            follower.set(Some(spawn(async move {
                loop {
                    after(FOLLOW).await;
                    if !catch_up(ctx, load, path.clone()).await {
                        break;
                    }
                }
            })));
        });
        let mut was_running = use_state(|| running);
        use_side_effect_with_deps(&running, move |now| {
            let ended = *was_running.peek() && !*now;
            was_running.set(*now);
            if !ended {
                return;
            }
            let Some(path) = file.peek().clone() else { return };
            if !scanned.peek().files.iter().any(|x| x.path == path && x.kind == Kind::Latest) || doc.peek().status != Status::Ready {
                return;
            }
            let load = doc.peek().load;
            spawn(async move {
                catch_up(ctx, load, path).await;
            });
        });

        use_side_effect(move || {
            let _ = controller.bounds.read();
            if *stick.peek() {
                let mut c = controller;
                c.scroll_to(ScrollPosition::End, Direction::Vertical);
                let mut stick = stick;
                stick.set(false);
            }
        });

        let text_now = query.read().clone();
        let (loaded_doc, version) = {
            let d = doc.read();
            (d.load, d.version)
        };
        use_side_effect_with_deps(&(text_now, loaded_doc, version, *find_open.read()), move |(text, _, _, open)| {
            let ticket = searching.peek().wrapping_add(1);
            searching.set(ticket);
            if !*open || text.is_empty() {
                if !found.peek().query.is_empty() {
                    let mut found = found;
                    found.set(Found::none());
                }
                return;
            }
            let (lines, text) = (doc.peek().lines.0.clone(), text.clone());
            spawn(async move {
                let wanted = text.clone();
                let list = off_ui(move || search(&lines, &wanted)).await.unwrap_or_default();
                if *searching.peek() != ticket {
                    return;
                }
                let before = found.peek().clone();
                let fresh = before.query != text;
                let last = list.len().saturating_sub(1);
                let at = if fresh { last } else { before.at.min(last) };
                let mut found = found;
                found.set(Found { list: Same(Arc::new(list)), at, query: text });
                let line = found.peek().line();
                if let (true, Some(line)) = (fresh, line) {
                    reveal(ctx, line);
                }
            });
        });

        use_side_effect_with_deps(&*upload_ask.read(), move |n| {
            if *n == 0 || upload.peek().as_ref() == Some(&Upload::Sending) {
                return;
            }
            let load = doc.peek().load;
            let text = all_text(doc);
            upload.set(Some(Upload::Sending));
            link_copied.set(false);
            spawn(async move {
                let result = off_ui(move || logs::upload(&text)).await;
                if doc.peek().load != load {
                    return;
                }
                upload.set(Some(match result {
                    Some(Ok(url)) => Upload::Sent(url),
                    Some(Err(e)) => Upload::Failed(forms::explain_as(tr("Das Protokoll wurde nicht hochgeladen."), &e).text),
                    None => Upload::Failed(tr("Das Protokoll wurde nicht hochgeladen.")),
                }));
            });
        });

        let state = doc.read().clone();
        let has = !state.lines.0.is_empty();
        let sending = upload.read().as_ref() == Some(&Upload::Sending);
        let hits = found.read().clone();

        let ask = move || {
            let mut upload_ask = upload_ask;
            if upload.peek().as_ref() == Some(&Upload::Sending) {
                return;
            }
            sheet::confirm(
                store.ui,
                "instance.logs.upload",
                tr("Protokoll hochladen?"),
                tr("Manycraft lädt das Protokoll zu mclo.gs hoch, ohne Anmeldedaten, E-Mail-Adressen und Benutzerordner. Jeder mit dem Link kann es lesen."),
                tr("Hochladen"),
                false,
                move |_| {
                    let next = upload_ask.peek().wrapping_add(1);
                    upload_ask.set(next);
                },
            );
        };
        let open_find = move || {
            let (mut find_open, mut find_epoch) = (find_open, find_epoch);
            find_open.set(true);
            let next = find_epoch.peek().wrapping_add(1);
            find_epoch.set(next);
        };
        let copy_all = move || {
            let mut copied = copied;
            if doc.peek().lines.0.is_empty() {
                return;
            }
            if native::copy_text(all_text(doc)) {
                copied.set(true);
            } else {
                toast::show(store.ui, tr("Das Protokoll wurde nicht kopiert."), None);
            }
        };
        let reveal_file = {
            let path = file_now.clone();
            move || {
                if let Some(path) = &path {
                    native::reveal(path);
                }
            }
        };

        let mut extras = Vec::new();
        if file_now.is_some() && has {
            let (find, all, folder, send) = (open_find, copy_all, reveal_file.clone(), ask);
            extras.push(Extra { id: "instance.logs.find".into(), label: tr("Im Protokoll suchen"), glyph: Icon::Search, shortcut: Some(Shortcut::primary(Code::KeyF)), run: EventHandler::new(move |_| find()) });
            extras.push(Extra { id: "instance.logs.copy".into(), label: tr("Ganzes Protokoll kopieren"), glyph: Icon::Copy, shortcut: None, run: EventHandler::new(move |_| all()) });
            extras.push(Extra { id: "instance.logs.reveal".into(), label: tr("Protokoll im Ordner zeigen"), glyph: Icon::Folder, shortcut: None, run: EventHandler::new(move |_| folder()) });
            extras.push(Extra { id: "instance.logs.upload".into(), label: tr("Protokoll hochladen…"), glyph: Icon::Upload, shortcut: None, run: EventHandler::new(move |_| send()) });
            if *find_open.read() && !hits.list.0.is_empty() {
                extras.push(Extra { id: "instance.logs.next".into(), label: tr("Nächster Treffer"), glyph: Icon::ChevronDown, shortcut: Some(Shortcut::bare(Code::F3)), run: EventHandler::new(move |_| jump(ctx, 1)) });
                extras.push(Extra { id: "instance.logs.previous".into(), label: tr("Vorheriger Treffer"), glyph: Icon::ChevronDown, shortcut: Some(Shortcut { code: Code::F3, primary: false, shift: true, alt: false, ctrl: false }), run: EventHandler::new(move |_| jump(ctx, -1)) });
            }
        }
        for other in shown.files.iter().filter(|x| Some(&x.path) != file_now.as_ref()).take(OTHERS) {
            let path = other.path.clone();
            let label = trf("%@ zeigen", &[&super::logs_lines::file_label(other, false)]);
            extras.push(Extra { id: format!("instance.logs.file.{}", other.name()), label, glyph: if other.kind == Kind::Crash { Icon::Alert } else { Icon::Rows }, shortcut: None, run: EventHandler::new(move |_| file.clone().set(Some(path.clone()))) });
        }
        use_extras(extras);

        let on_keys = move |e: Event<KeyboardEventData>| {
            if !field::typing() && store.ui.sheet.peek().is_none() && store.ui.menu.peek().is_none() && e.modifiers.is_empty() {
                scroll_by_key(controller, &e.key);
                return;
            }
            if !field::typing() || !*find_open.peek() {
                return;
            }
            let shift = e.modifiers.contains(Modifiers::SHIFT);
            match &e.key {
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::F3) => jump(ctx, if shift { -1 } else { 1 }),
                Key::Named(NamedKey::Escape) => find_open.set(false),
                _ => {}
            }
        };
        let root = rect().width(Size::fill()).height(Size::fill()).padding((space::M, space::L, space::L, space::L)).spacing(space::S).on_global_key_down(on_keys);

        if !shown.loaded {
            return root.center().child(secondary(&t, tr("Protokolle werden gesucht …"))).into_element();
        }
        let empty = shown.accounts.is_empty() || shown.files.is_empty() && file_now.is_none();

        let mut bar = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .on_sized(move |e: Event<SizedEventData>| narrow.set_if_modified(e.area.width() < files::LOG_BAR_WORDS));
        let words = !*narrow.read();
        if let Some(account) = shown.account {
            let names: Vec<String> = shown.accounts.iter().map(|(a, _)| name_of(&f.accounts, *a)).collect();
            if shown.accounts.len() < 2 {
                bar = bar.child(strong(&t, name_of(&f.accounts, account), font::BODY, t.text1).max_width(Size::px(files::ACCOUNT_PICKER)));
            } else {
                let ids: Vec<Uuid> = shown.accounts.iter().map(|(a, _)| *a).collect();
                let options: Vec<String> = ids.iter().zip(&names).map(|(a, n)| if playing.contains(a) { trf("%@ (läuft)", &[n]) } else { n.clone() }).collect();
                let at = ids.iter().position(|a| *a == account).unwrap_or(0);
                bar = bar.child(Select::new("instance.logs.account", tr("Account"), options, at).width(files::ACCOUNT_PICKER).on_choose(move |i| {
                    if let Some(a) = ids.get(i) {
                        pick.set(Some(*a));
                    }
                }));
            }
        }
        if !shown.files.is_empty() {
            let list = ordered(&shown.files);
            let options: Vec<String> = list.iter().map(|x| option_label(x, running)).collect();
            let paths: Vec<PathBuf> = list.iter().map(|x| x.path.clone()).collect();
            let at = paths.iter().position(|p| Some(p) == file_now.as_ref()).unwrap_or(0);
            bar = bar.child(Select::new("instance.logs.file", tr("Protokoll"), options, at).width(files::LOG_PICKER).on_choose(move |i| {
                if let Some(p) = paths.get(i) {
                    file.set(Some(p.clone()));
                }
            }));
        }
        bar = bar.child(spacer());
        if has {
            let wrapped = *wrap.read();
            bar = bar.child(Button::icon("instance.logs.wrap", Icon::Wrap, tr("Lange Zeilen umbrechen")).on(wrapped).on_press(move |_| {
                let next = !*wrap.peek();
                if at_end(controller) {
                    let mut stick = stick;
                    stick.set(true);
                }
                wrap.set(next);
            }));
            let (find, all) = (open_find, copy_all);
            bar = bar
                .child(Button::new("instance.logs.find", tr("Suchen")).glyph(Icon::Search).tip(Shortcut::primary(Code::KeyF).label()).on_press(move |_| find()))
                .child(if words {
                    Button::new("instance.logs.copy", if *copied.read() { tr("Kopiert") } else { tr("Alles kopieren") }).glyph(if *copied.read() { Icon::Check } else { Icon::Copy })
                } else {
                    Button::icon("instance.logs.copy", if *copied.read() { Icon::Check } else { Icon::Copy }, if *copied.read() { tr("Kopiert") } else { tr("Alles kopieren") })
                }
                .on_press(move |_| all()));
        }
        if file_now.is_some() {
            let folder = reveal_file;
            let reveal = if words { Button::new("instance.logs.reveal", tr("Im Ordner zeigen")).glyph(Icon::Folder) } else { Button::icon("instance.logs.reveal", Icon::Folder, tr("Im Ordner zeigen")) };
            bar = bar.child(reveal.on_press(move |_| folder()));
        }
        if has {
            let send = ask;
            let upload = if words { Button::new("instance.logs.upload", tr("Hochladen…")).glyph(Icon::Upload) } else { Button::icon("instance.logs.upload", Icon::Upload, tr("Hochladen…")) };
            bar = bar.child(upload.disabled(sending.then(|| tr("Wird hochgeladen …"))).on_press(move |_| send()));
        }
        let bar = bar.content(Content::flex());

        if empty {
            let body = centered(&t, Icon::Rows, tr("Noch keine Protokolle"), tr("Sobald ein Account diese Instanz einmal gestartet hat, stehen seine Protokolle hier."), super::play::empty_action(store, f, "instance.logs.empty.play"));
            let mut column = root;
            if shown.accounts.len() > 1 {
                column = column.child(bar);
            }
            return column.child(rect().width(Size::fill()).height(Size::flex(1.)).child(body)).content(Content::flex()).into_element();
        }

        let mut column = root.child(bar);

        if *find_open.read() {
            let at = hits.at;
            let n = hits.list.0.len();
            let counted = match (text_now_is_empty(&query), n) {
                (true, _) => String::new(),
                (false, 0) => tr("Keine Treffer"),
                (false, _) => trf("%1$lld von %2$lld", &[&(at + 1), &n]),
            };
            let none = n == 0;
            let reasons = (!none).then_some(()).map_or_else(|| Some(tr("Keine Treffer")), |_| None);
            column = column.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::XS)
                    .child(rect().key(*find_epoch.read()).width(Size::px(files::FIND_FIELD)).child(Field::new("instance.logs.find.field", query).placeholder(tr("Im Protokoll suchen")).auto_focus().width(Size::fill())))
                    .child(rect().width(Size::px(files::ACCOUNT_PICKER * 0.75)).child(caption(&t, counted)))
                    .child(Button::new("instance.logs.find.previous", tr("Zurück")).plain().tip(tr("Vorheriger Treffer")).disabled(reasons.clone()).on_press(move |_| jump(ctx, -1)))
                    .child(Button::new("instance.logs.find.next", tr("Weiter")).plain().tip(tr("Nächster Treffer")).disabled(reasons).on_press(move |_| jump(ctx, 1)))
                    .child(Button::icon("instance.logs.find.close", Icon::Close, tr("Suche schließen")).on_press(move |_| find_open.set(false))),
            );
        }

        match upload.read().clone() {
            Some(Upload::Sent(url)) => {
                let (copy_url, open_url) = (url.clone(), url.clone());
                column = column.child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(space::S)
                        .child(icon(&t, Icon::Check, iconsize::S, t.success_text))
                        .child(line(&t, url, font::CALLOUT, t.text1).width(Size::flex(1.)))
                        .child(Button::new("instance.logs.link.copy", if *link_copied.read() { tr("Kopiert") } else { tr("Link kopieren") }).plain().on_press(move |_| {
                            if native::copy_text(copy_url.clone()) {
                                link_copied.set(true);
                            } else {
                                toast::show(store.ui, tr("Das Protokoll wurde nicht kopiert."), None);
                            }
                        }))
                        .child(Button::new("instance.logs.link.open", tr("Öffnen")).plain().on_press(move |_| native::open(Path::new(&open_url))))
                        .child(Button::icon("instance.logs.link.close", Icon::Close, tr("Schließen")).on_press(move |_| upload.set(None)))
                        .content(Content::flex()),
                );
            }
            Some(Upload::Failed(text)) => {
                column = column.child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(space::S)
                        .child(icon(&t, Icon::Alert, iconsize::S, t.danger_text))
                        .child(line(&t, text, font::CALLOUT, t.danger_text).width(Size::flex(1.)))
                        .child(Button::icon("instance.logs.upload.close", Icon::Close, tr("Schließen")).on_press(move |_| upload.set(None)))
                        .content(Content::flex()),
                );
            }
            _ => {}
        }

        if let Some(reason) = shown.files.iter().find(|x| Some(&x.path) == file_now.as_ref() && x.kind == Kind::Crash).and_then(|x| x.reason.clone()) {
            column = column.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::S)
                    .padding((space::S, space::M))
                    .corner_radius(t.shape.control)
                    .background(t.attention_text.with_a((tint::ATTENTION * 255.) as u8))
                    .child(icon(&t, Icon::Alert, iconsize::S, t.attention_text))
                    .child(line(&t, reason, font::CALLOUT, t.attention_text).width(Size::flex(1.)))
                    .content(Content::flex()),
            );
        }

        let viewer = match &state.status {
            Status::Loading => rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Protokoll wird gelesen …"))).into_element(),
            Status::Failed(text) => {
                let what = tr("Das Protokoll lässt sich nicht lesen.");
                let reason = text.strip_prefix(what.as_str()).unwrap_or(text).trim().to_owned();
                centered(&t, Icon::Alert, what, reason, None)
            }
            Status::Ready if !has => rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Das Protokoll ist leer."))).into_element(),
            Status::Ready => Viewer {
                lines: state.lines.clone(),
                wrap: *wrap.read(),
                cols,
                stick,
                wrapped,
                version: state.version,
                longest: state.longest,
                hits: if *find_open.read() { hits.list.clone() } else { Same(Arc::new(Vec::new())) },
                current: if *find_open.read() { hits.line() } else { None },
                cut: state.cut,
                controller,
            }
            .into_element(),
        };
        column.child(rect().width(Size::fill()).height(Size::flex(1.)).child(viewer)).content(Content::flex()).into_element()
    }
}

fn text_now_is_empty(query: &State<String>) -> bool {
    query.peek().is_empty()
}
