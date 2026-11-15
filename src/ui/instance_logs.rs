//! The Protokolle tab of the instance page: the logs and crash reports of one account's game folder, redacted, with
//! search, a live tail while the game runs, and an upload to mclo.gs on request.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::input::Escape;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu};
use gpui_kit::component::{FocusableExt, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use uuid::Uuid;

use super::drive::Mark;
use super::forms::{self, Problem};
use super::instance_files::{self as files, OpenLogsFolder, PickLog, PickLogAccount, UploadLog};
use super::palette::{Entry, Group, hint};
use super::sheet::{self, Look};
use super::theme::{self, Theme, control, font, iconsize, radius, space, tint};
use super::widgets::{TakeFocus, icon};
use super::model;
use crate::core::i18n::{tr, trf};
use crate::core::logs::{self, Kind, LogFile};
use crate::core::worlds::game_dirs;

/// Key context of the logs tab.
pub const CONTEXT: &str = "InstanceLogs";
const FOLLOW: Duration = Duration::from_millis(500);
const SHOWN_CHARS: usize = 2000;

struct Shown(Entity<Logs>);

impl Global for Shown {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Plain,
    Warn,
    Error,
}

#[derive(Clone)]
struct Line {
    text: SharedString,
    level: Level,
}

enum Text {
    Loading,
    Ready,
    Failed(Problem),
}

enum Upload {
    Sending,
    Sent(String),
    Failed(Problem),
}

struct Find {
    input: Entity<InputState>,
    query: String,
    hits: Arc<Vec<(usize, Range<usize>)>>,
    at: usize,
    searching: u64,
}

struct Logs {
    id: String,
    accounts: Vec<(Uuid, PathBuf)>,
    account: Option<Uuid>,
    files: Vec<LogFile>,
    file: Option<PathBuf>,
    scanned: bool,
    text: Text,
    lines: Vec<Line>,
    cut: bool,
    longest: usize,
    offset: u64,
    load: u64,
    following: bool,
    painted: u64,
    probe: Option<u64>,
    find: Option<Find>,
    upload: Option<Upload>,
    copied: bool,
    link_copied: bool,
    born: Option<std::time::SystemTime>,
    details: bool,
    playing: Vec<Uuid>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _model: Subscription,
}

/// Registers the log commands.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &PickLogAccount, cx| {
        let account = Uuid::parse_str(&a.0).ok();
        with(cx, move |s, cx| {
            if let Some(account) = account {
                s.pick_account(account, cx);
            }
        });
    });
    cx.on_action(|a: &PickLog, cx| {
        let path = PathBuf::from(&a.0);
        with(cx, move |s, cx| s.pick(Some(path), cx));
    });
    cx.on_action(|_: &UploadLog, cx| {
        let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) else { return };
        sheet::in_window(cx, move |window, cx| view.update(cx, |s, cx| s.ask_upload(window, cx)));
    });
    cx.on_action(|_: &OpenLogsFolder, cx| {
        let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) else { return };
        if let Some(path) = view.read(cx).file.clone() {
            files::reveal(path, cx);
        }
    });
}

fn with(cx: &mut App, f: impl FnOnce(&mut Logs, &mut Context<Logs>)) {
    if let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) {
        view.update(cx, f);
    }
}

/// The Protokolle tab of instance `id`; `fresh` lists the files again.
pub fn view(id: &str, fresh: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let view = match cx.try_global::<Shown>() {
        Some(shown) => shown.0.clone(),
        None => {
            let view = cx.new(|cx| Logs::new(id, cx));
            cx.set_global(Shown(view.clone()));
            view
        }
    };
    view.update(cx, |s, cx| {
        if s.id != id {
            s.bind(id, cx);
        } else if fresh {
            s.rescan(cx);
        }
    });
    if fresh && !window.has_active_dialog(cx) {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    }
    view.into_any_element()
}

/// Palette entries for the logs of instance `id`.
pub fn palette(id: &str, cx: &App) -> Vec<Entry> {
    let Some(view) = cx.try_global::<Shown>().map(|s| s.0.clone()) else { return Vec::new() };
    let s = view.read(cx);
    if s.id != id || s.file.is_none() {
        return Vec::new();
    }
    let mut out = vec![
        files::entry(Group::Selection, "search", tr("Im Protokoll suchen"), None, Box::new(files::Find), Some(&files::Find), cx),
        files::entry(Group::Selection, "copy", tr("Ganzes Protokoll kopieren"), None, Box::new(files::CopyAll), Some(&files::CopyAll), cx),
        files::entry(Group::Selection, "folder-open", tr("Protokoll im Ordner zeigen"), None, Box::new(OpenLogsFolder), None, cx),
        files::entry(Group::Selection, "upload", tr("Protokoll hochladen…"), Some("mclo.gs".to_owned()), Box::new(UploadLog), None, cx),
    ];
    for f in s.files.iter().filter(|f| Some(&f.path) != s.file.as_ref()).take(8) {
        out.push(files::entry(Group::Selection, if f.kind == Kind::Crash { "triangle-alert" } else { "scroll-text" }, trf("%@ zeigen", &[&file_label(f, false)]), f.reason.clone(), Box::new(PickLog(f.path.display().to_string())), None, cx));
    }
    out
}

fn file_label(f: &LogFile, running: bool) -> String {
    match f.kind {
        Kind::Latest if running => tr("Aktuell (läuft)"),
        Kind::Latest => tr("Aktuell"),
        Kind::Older => {
            let name = f.name();
            let stem = name.trim_end_matches(".gz").trim_end_matches(".log");
            let date = stem.rsplit_once('-').map_or(stem, |(date, _)| date);
            match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
                Ok(day) => format!("{} ({})", day.format("%d.%m.%Y"), stem.rsplit_once('-').map_or("1", |(_, n)| n)),
                Err(_) => name,
            }
        }
        Kind::Crash => trf("Absturz %@", &[&files::stamp(f.modified)]),
    }
}

fn level(line: &str) -> Level {
    if line.contains("/ERROR]") || line.contains("/FATAL]") || line.contains("Exception") || line.starts_with("\tat ") || line.starts_with("Caused by:") {
        Level::Error
    } else if line.contains("/WARN]") {
        Level::Warn
    } else {
        Level::Plain
    }
}

fn shown(line: &str) -> SharedString {
    let line = line.trim_end_matches('\r');
    if line.len() <= SHOWN_CHARS {
        return SharedString::from(line.to_owned());
    }
    let mut end = SHOWN_CHARS;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    SharedString::from(format!("{}…", line.get(..end).unwrap_or_default()))
}

fn split(text: &str) -> Vec<Line> {
    text.lines().map(|l| Line { text: shown(l), level: level(l) }).collect()
}

impl Logs {
    fn new(id: &str, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&model::get(cx), |s: &mut Self, _, cx| {
            let now = s.active(cx);
            if now != s.playing {
                s.playing = now;
                s.follow(cx);
                cx.notify();
            }
        });
        let mut page = Self {
            id: String::new(),
            accounts: Vec::new(),
            account: None,
            files: Vec::new(),
            file: None,
            scanned: false,
            text: Text::Loading,
            lines: Vec::new(),
            cut: false,
            longest: 0,
            offset: 0,
            load: 0,
            following: false,
            painted: 0,
            probe: None,
            find: None,
            upload: None,
            copied: false,
            link_copied: false,
            born: None,
            details: false,
            playing: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _model: subscription,
        };
        page.bind(id, cx);
        page
    }

    fn active(&self, cx: &App) -> Vec<Uuid> {
        let mut active: Vec<Uuid> = model::get(cx).read(cx).games.values().filter(|g| g.instance_id == self.id && g.phase.is_active()).map(|g| g.account_id).collect();
        active.sort_unstable();
        active
    }

    fn bind(&mut self, id: &str, cx: &mut Context<Self>) {
        self.id = id.to_owned();
        self.account = None;
        self.file = None;
        self.files.clear();
        self.accounts.clear();
        self.lines.clear();
        self.find = None;
        self.upload = None;
        self.playing = self.active(cx);
        self.rescan(cx);
    }

    fn running(&self) -> bool {
        self.account.is_some_and(|a| self.playing.contains(&a))
    }

    fn latest(&self) -> bool {
        self.file.as_ref().is_some_and(|p| self.files.iter().any(|f| &f.path == p && f.kind == Kind::Latest))
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = files::instance_dir(&self.id, cx) else {
            self.scanned = true;
            cx.notify();
            return;
        };
        let (wanted, playing) = (self.account, self.playing.clone());
        let task = cx.background_spawn(async move {
            let mut accounts: Vec<(Uuid, PathBuf, i64)> = game_dirs(&dir)
                .into_iter()
                .map(|(account, game)| {
                    let at = std::fs::metadata(game.join("logs").join("latest.log")).and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
                    (account, game, at)
                })
                .collect();
            accounts.sort_by(|a, b| playing.contains(&b.0).cmp(&playing.contains(&a.0)).then(b.2.cmp(&a.2)));
            let account = wanted.filter(|w| accounts.iter().any(|a| a.0 == *w)).or_else(|| accounts.first().map(|a| a.0));
            let found = account.and_then(|a| accounts.iter().find(|x| x.0 == a)).map(|(_, game, _)| logs::list(game)).unwrap_or_default();
            (accounts.into_iter().map(|(a, g, _)| (a, g)).collect::<Vec<_>>(), account, found)
        });
        cx.spawn(async move |this, cx| {
            let (accounts, account, found) = task.await;
            let _ = this.update(cx, |s, cx| {
                s.accounts = accounts;
                s.account = account;
                s.files = found;
                s.scanned = true;
                let keep = s.file.clone().filter(|f| s.files.iter().any(|x| &x.path == f));
                let pick = keep.clone().or_else(|| s.files.first().map(|f| f.path.clone()));
                if keep.is_none() || pick != s.file {
                    s.pick(pick, cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pick_account(&mut self, account: Uuid, cx: &mut Context<Self>) {
        if self.account == Some(account) {
            return;
        }
        self.account = Some(account);
        self.file = None;
        self.rescan(cx);
    }

    fn pick(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.load += 1;
        self.following = false;
        self.born = None;
        self.file = path.clone();
        self.lines.clear();
        self.cut = false;
        self.longest = 0;
        self.offset = 0;
        self.upload = None;
        self.copied = false;
        self.details = false;
        self.scroll = UniformListScrollHandle::new();
        if let Some(find) = &mut self.find {
            find.hits = Arc::new(Vec::new());
            find.at = 0;
        }
        let Some(path) = path else {
            self.text = Text::Ready;
            cx.notify();
            return;
        };
        self.text = Text::Loading;
        cx.notify();
        let load = self.load;
        let task = cx.background_spawn(async move {
            let gz = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("gz"));
            let born = logs::born(&path);
            let (text, offset) = if gz { (logs::read(&path)?, 0) } else { logs::read_from(&path, 0).map(|(_, text, next)| (text, next))? };
            let mut lines = split(&logs::redact(&text));
            let cut = lines.len() > theme::files::LOG_LINES;
            if cut {
                lines.drain(..lines.len() - theme::files::LOG_LINES);
            }
            let longest = lines.iter().enumerate().max_by_key(|(_, l)| l.text.len()).map_or(0, |(at, _)| at);
            anyhow::Ok((lines, cut, longest, offset, born))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.load != load {
                    return;
                }
                match result {
                    Ok((lines, cut, longest, offset, born)) => {
                        s.born = born;
                        s.lines = lines;
                        s.cut = cut;
                        s.longest = longest;
                        s.offset = offset;
                        s.text = Text::Ready;
                        s.scroll.scroll_to_item(s.lines.len().saturating_sub(1), ScrollStrategy::Bottom);
                        s.search(cx);
                        s.follow(cx);
                    }
                    Err(e) => s.text = Text::Failed(forms::explain_as(tr("Das Protokoll lässt sich nicht lesen."), &e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        if self.following || !self.running() || !self.latest() || !matches!(self.text, Text::Ready) {
            return;
        }
        self.following = true;
        self.probe = None;
        let load = self.load;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOLLOW).await;
                let next = this.update(cx, |s, _| {
                    if s.load != load {
                        return None;
                    }
                    if !s.running() || !s.latest() || s.probe == Some(s.painted) {
                        s.following = false;
                        return None;
                    }
                    s.probe = None;
                    s.file.clone().map(|p| (p, s.offset, s.born))
                });
                let Ok(Some((path, offset, known))) = next else { break };
                let read = cx
                    .background_executor()
                    .spawn(async move {
                        let born = logs::born(&path);
                        let from = if born != known { 0 } else { offset };
                        logs::read_from(&path, from).map(|(kept, text, next)| (kept.is_none() || from != offset, split(&logs::redact(&text)), next, born))
                    })
                    .await;
                let _ = this.update(cx, |s, cx| {
                    if s.load != load {
                        return;
                    }
                    if let Ok((restarted, lines, next, born)) = read {
                        s.born = born;
                        let end = at_end(&s.scroll);
                        if restarted {
                            s.lines.clear();
                            s.longest = 0;
                        }
                        if !lines.is_empty() || restarted {
                            let base = s.lines.len();
                            s.lines.extend(lines);
                            let over = s.lines.len().saturating_sub(theme::files::LOG_LINES);
                            if over > 0 {
                                s.lines.drain(..over);
                                s.cut = true;
                            }
                            if let Some((at, _)) = s.lines.iter().enumerate().skip(base.saturating_sub(over)).max_by_key(|(_, l)| l.text.len())
                                && s.lines.get(s.longest).is_none_or(|l| s.lines.get(at).is_some_and(|n| n.text.len() > l.text.len()))
                            {
                                s.longest = at;
                            }
                            if end {
                                s.scroll.scroll_to_item(s.lines.len().saturating_sub(1), ScrollStrategy::Bottom);
                            }
                            s.search(cx);
                            s.probe = Some(s.painted);
                            cx.notify();
                        }
                        s.offset = next;
                    }
                });
            }
        })
        .detach();
    }

    fn scroll_by(&mut self, rows: f32, cx: &mut Context<Self>) {
        let state = self.scroll.0.borrow();
        let row = state.last_item_size.as_ref().map_or(px(0.), |size| size.item.height);
        let (base, max) = (state.base_handle.clone(), state.base_handle.max_offset().y);
        drop(state);
        let mut offset = base.offset();
        offset.y = (offset.y - row * rows).clamp(-max.max(px(0.)), px(0.));
        base.set_offset(offset);
        cx.notify();
    }

    fn scroll_page(&mut self, pages: f32, cx: &mut Context<Self>) {
        let state = self.scroll.0.borrow();
        let (height, row) = (state.base_handle.bounds().size.height, state.last_item_size.as_ref().map_or(px(0.), |size| size.item.height));
        drop(state);
        if row > px(0.) {
            self.scroll_by(pages * ((height / row).floor() - 1.).max(1.), cx);
        }
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match &self.find {
            Some(find) => find.input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Im Protokoll suchen")));
                cx.subscribe(&input, |s: &mut Self, input, e: &InputEvent, cx| match e {
                    InputEvent::Change => {
                        let query = input.read(cx).value().to_string();
                        if let Some(find) = &mut s.find {
                            if find.query == query {
                                return;
                            }
                            find.query = query;
                            find.at = 0;
                        }
                        s.search(cx);
                    }
                    InputEvent::PressEnter { shift, .. } => s.jump(if *shift { -1 } else { 1 }, cx),
                    _ => {}
                })
                .detach();
                self.find = Some(Find { input: input.clone(), query: String::new(), hits: Arc::new(Vec::new()), at: 0, searching: 0 });
                input
            }
        };
        window.on_next_frame(move |window, cx| {
            input.update(cx, |s, cx| {
                let end = s.value().len();
                s.take_focus(window, cx);
                s.set_selected_range(0..end, cx);
            })
        });
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        find.searching += 1;
        let searching = find.searching;
        let query = find.query.to_ascii_lowercase();
        if query.is_empty() {
            find.hits = Arc::new(Vec::new());
            cx.notify();
            return;
        }
        let lines: Vec<SharedString> = self.lines.iter().map(|l| l.text.clone()).collect();
        let task = cx.background_spawn(async move {
            let mut hits = Vec::new();
            for (at, line) in lines.iter().enumerate() {
                let lower = line.to_ascii_lowercase();
                let mut from = 0;
                while let Some(found) = lower.get(from..).and_then(|rest| rest.find(&query)) {
                    let start = from + found;
                    hits.push((at, start..start + query.len()));
                    from = start + query.len().max(1);
                }
            }
            hits
        });
        cx.spawn(async move |this, cx| {
            let hits = task.await;
            let _ = this.update(cx, |s, cx| {
                let Some(find) = &mut s.find else { return };
                if find.searching != searching {
                    return;
                }
                let fresh = find.hits.is_empty();
                find.at = find.at.min(hits.len().saturating_sub(1));
                if fresh && !hits.is_empty() {
                    find.at = hits.len() - 1;
                }
                find.hits = Arc::new(hits);
                if fresh {
                    s.reveal_hit();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn jump(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        let n = find.hits.len();
        if n == 0 {
            return;
        }
        find.at = (find.at as isize + by).rem_euclid(n as isize) as usize;
        self.reveal_hit();
        cx.notify();
    }

    fn reveal_hit(&mut self) {
        let Some(find) = &self.find else { return };
        if let Some((line, _)) = find.hits.get(find.at) {
            self.scroll.scroll_to_item(*line, ScrollStrategy::Center);
        }
    }

    fn all(&self) -> String {
        let mut out = String::with_capacity(self.lines.iter().map(|l| l.text.len() + 1).sum());
        for line in &self.lines {
            out.push_str(&line.text);
            out.push('\n');
        }
        out
    }

    fn copy_all(&mut self, cx: &mut Context<Self>) {
        if self.lines.is_empty() {
            return;
        }
        let text = self.all();
        if !crate::core::drive::fenced("clipboard", format!("{} lines", self.lines.len())) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.copied = true;
        cx.notify();
    }

    fn ask_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lines.is_empty() || matches!(self.upload, Some(Upload::Sending)) {
            return;
        }
        let view = cx.entity();
        sheet::ask_blocked(window, cx, tr("Protokoll hochladen?"), tr("Manycraft lädt das Protokoll zu mclo.gs hoch, ohne Anmeldedaten, E-Mail-Adressen und Benutzerordner. Jeder mit dem Link kann es lesen."), tr("Hochladen"), |_| None, move |_, cx| view.update(cx, |s, cx| s.send(cx)));
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.all();
        let load = self.load;
        self.upload = Some(Upload::Sending);
        self.link_copied = false;
        cx.notify();
        let task = cx.background_spawn(async move { logs::upload(&text) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.load != load {
                    return;
                }
                s.details = false;
                s.upload = Some(match result {
                    Ok(url) => Upload::Sent(url),
                    Err(e) => Upload::Failed(forms::explain_as(tr("Das Protokoll wurde nicht hochgeladen."), &e)),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn account_picker(&self, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        let name = self.account.map(|a| files::account_name(a, cx)).unwrap_or_default();
        if self.accounts.len() < 2 {
            return div().flex_none().max_w(px(theme::files::ACCOUNT_PICKER)).truncate().text_size(px(font::BODY)).font_weight(FontWeight::MEDIUM).text_color(t.primary).child(name).into_any_element();
        }
        let choices: Vec<(Uuid, String)> = self.accounts.iter().map(|(a, _)| (*a, files::account_name(*a, cx))).collect();
        let playing = self.playing.clone();
        let label = tr("Account");
        let button = Button::new("logs-account-menu")
            .secondary()
            .small()
            .h(px(control::M))
            .px(px(space::M))
            .max_w(px(theme::files::ACCOUNT_PICKER))
            .focus_ring(theme::rings(cx))
            .dropdown_caret(true)
            .child(div().min_w_0().truncate().text_size(px(font::BODY)).child(name))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true);
                for (id, name) in &choices {
                    let text = if playing.contains(id) { trf("%@ (läuft)", &[name]) } else { name.clone() };
                    menu = menu.menu(text, Box::new(PickLogAccount(id.to_string())));
                }
                menu
            });
        div().id("logs-account").mark("button", label).flex_none().child(button).into_any_element()
    }

    fn file_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let running = self.running();
        let current = self.files.iter().find(|f| Some(&f.path) == self.file.as_ref()).map_or_else(|| tr("Kein Protokoll"), |f| file_label(f, running));
        let choices = self.files.clone();
        let label = tr("Protokoll");
        let button = Button::new("logs-file-menu")
            .secondary()
            .small()
            .h(px(control::M))
            .px(px(space::M))
            .max_w(px(theme::files::LOG_PICKER))
            .focus_ring(theme::rings(cx))
            .dropdown_caret(true)
            .child(div().min_w_0().truncate().text_size(px(font::BODY)).child(current))
            .dropdown_menu(move |menu, _, _| file_items(menu.scrollable(true), &choices, running));
        div().id("logs-file").mark("button", label).flex_none().child(button).into_any_element()
    }

    fn toolbar(&self, cx: &mut Context<Self>, t: &Theme) -> Div {
        let has = !self.lines.is_empty();
        let sending = matches!(self.upload, Some(Upload::Sending));
        let mut tools = div().flex().items_center().gap(px(space::XS));
        if has {
            tools = tools
                .child(sheet::icon_button("logs-find", "search", hint(tr("Suchen"), &files::Find, cx), t.secondary, t).on_click(cx.listener(|s, _, window, cx| s.open_find(window, cx))))
                .child(sheet::icon_button("logs-copy", if self.copied { "check" } else { "copy" }, hint(if self.copied { tr("Kopiert") } else { tr("Alles kopieren") }, &files::CopyAll, cx), if self.copied { t.success_text } else { t.secondary }, t).on_click(cx.listener(|s, _, _, cx| s.copy_all(cx))));
        }
        if self.file.is_some() {
            tools = tools.child(sheet::icon_button("logs-folder", "folder-open", tr("Im Ordner zeigen"), t.secondary, t).on_click(|_, window, cx| window.dispatch_action(Box::new(OpenLogsFolder), cx)));
        }
        if has {
            let upload = sheet::button("logs-upload", tr("Hochladen…"), Some("upload"), Look::Secondary, sending, sending, t);
            tools = tools.child(if sending { upload } else { upload.on_click(|_, window, cx| window.dispatch_action(Box::new(UploadLog), cx)) });
        }
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(space::S))
            .child(self.account_picker(cx, t))
            .when(!self.files.is_empty(), |d| d.child(self.file_picker(cx)))
            .child(div().flex_1())
            .child(tools)
    }

    fn find_bar(&self, cx: &mut Context<Self>, t: &Theme) -> Option<Div> {
        let find = self.find.as_ref()?;
        let count = match (find.query.is_empty(), find.hits.len()) {
            (true, _) => String::new(),
            (false, 0) => tr("Keine Treffer"),
            (false, n) => trf("%1$lld von %2$lld", &[&(find.at + 1), &n]),
        };
        let none = find.hits.is_empty();
        let step = |id: &'static str, symbol: &str, label: String, by: isize| {
            if none {
                return sheet::unavailable(div().id(id).size(px(control::M)).flex_none().flex().items_center().justify_center().rounded(px(radius::INNER)), icon(symbol, iconsize::M, t.tertiary), tr("Keine Treffer"));
            }
            sheet::icon_button(id, symbol, label, t.secondary, t).on_click(cx.listener(move |s, _, _, cx| s.jump(by, cx)))
        };
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(space::XS))
                .on_action(cx.listener(|s, _: &Escape, window, cx| {
                    cx.stop_propagation();
                    s.close_find(window, cx);
                }))
                .child(div().w(px(theme::files::FIND_FIELD)).flex_none().child(Input::new(&find.input).small()))
                .child(div().min_w(px(control::L * 2.)).text_size(px(font::CALLOUT)).text_color(t.secondary).child(count))
                .child(step("logs-find-previous", "chevron-up", hint(tr("Vorheriger Treffer"), &files::FindPrevious, cx), -1))
                .child(step("logs-find-next", "chevron-down", hint(tr("Nächster Treffer"), &files::FindNext, cx), 1))
                .child(sheet::icon_button("logs-find-close", "x", tr("Suche schließen"), t.secondary, t).on_click(cx.listener(|s, _, window, cx| s.close_find(window, cx)))),
        )
    }

    fn banner(&self, cx: &mut Context<Self>, t: &Theme) -> Option<AnyElement> {
        match &self.upload {
            Some(Upload::Sent(url)) => {
                let (copy, open) = (url.clone(), url.clone());
                Some(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(space::S))
                        .child(icon("link", iconsize::S, t.success_text))
                        .child(div().min_w_0().flex_1().truncate().text_size(px(font::CALLOUT)).text_color(t.primary).child(url.clone()))
                        .child(sheet::button("logs-link-copy", if self.link_copied { tr("Kopiert") } else { tr("Link kopieren") }, self.link_copied.then_some("check"), Look::Plain, false, false, t).on_click(cx.listener(move |s, _, _, cx| {
                            if !crate::core::drive::fenced("clipboard", &copy) {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }
                            s.link_copied = true;
                            cx.notify();
                        })))
                        .child(sheet::button("logs-link-open", tr("Öffnen"), None, Look::Plain, false, false, t).on_click(move |_, _, cx| {
                            if !crate::core::drive::fenced("open url", &open) {
                                cx.open_url(&open);
                            }
                        }))
                        .child(sheet::icon_button("logs-link-close", "x", tr("Schließen"), t.secondary, t).on_click(cx.listener(|s, _, _, cx| {
                            s.upload = None;
                            cx.notify();
                        })))
                        .into_any_element(),
                )
            }
            Some(Upload::Failed(problem)) => Some(sheet::problem("logs-upload-problem", "circle-alert", t.danger_text, problem, self.details, cx.listener(|s, _, _, cx| {
                s.details = !s.details;
                cx.notify();
            }), t).flex_none().into_any_element()),
            _ => None,
        }
    }

    fn reason(&self, t: &Theme) -> Option<Div> {
        let f = self.files.iter().find(|f| Some(&f.path) == self.file.as_ref())?;
        let reason = f.reason.clone().filter(|_| f.kind == Kind::Crash)?;
        Some(div().flex_none().px(px(space::M)).py(px(space::S)).rounded(px(radius::CONTROL)).bg(t.attention_text.opacity(tint::ATTENTION)).child(sheet::status("triangle-alert", t.attention_text, reason)))
    }

    fn viewer(&self, cx: &mut Context<Self>, t: &Theme) -> AnyElement {
        match &self.text {
            Text::Loading => return div().flex_1().flex().items_center().justify_center().child(sheet::waiting(tr("Protokoll wird gelesen …"), t)).into_any_element(),
            Text::Failed(problem) => {
                let problem = problem.clone();
                return div().flex_1().flex().items_center().justify_center().child(sheet::problem("logs-problem", "circle-alert", t.danger_text, &problem, self.details, cx.listener(|s, _, _, cx| {
                    s.details = !s.details;
                    cx.notify();
                }), t)).into_any_element();
            }
            Text::Ready => {}
        }
        if self.lines.is_empty() {
            return div().flex_1().flex().items_center().justify_center().text_size(px(font::CALLOUT)).text_color(t.secondary).child(tr("Das Protokoll ist leer.")).into_any_element();
        }
        let hits = self.find.as_ref().map(|f| (Arc::clone(&f.hits), f.at));
        let list = uniform_list(
            "logs-lines",
            self.lines.len(),
            cx.processor(move |s: &mut Self, range: Range<usize>, _, cx| {
                let t = Theme::get(cx);
                let hits = hits.clone();
                range
                    .filter_map(|at| s.lines.get(at).map(|l| (at, l.clone())))
                    .map(|(at, line)| {
                        let color = match line.level {
                            Level::Plain => t.primary,
                            Level::Warn => t.attention_text,
                            Level::Error => t.danger_text,
                        };
                        let text = match &hits {
                            Some((hits, current)) => {
                                let first = hits.partition_point(|(l, _)| *l < at);
                                let marks: Vec<(Range<usize>, HighlightStyle)> = hits
                                    .get(first..)
                                    .unwrap_or_default()
                                    .iter()
                                    .enumerate()
                                    .take_while(|(_, (l, _))| *l == at)
                                    .filter(|(_, (_, r))| r.end <= line.text.len())
                                    .map(|(n, (_, r))| {
                                        let strong = first + n == *current;
                                        (r.clone(), HighlightStyle { background_color: Some(if strong { t.accent.opacity(tint::SELECTED * 2.) } else { t.attention_text.opacity(tint::SELECTED) }), ..Default::default() })
                                    })
                                    .collect();
                                StyledText::new(line.text.clone()).with_highlights(marks).into_any_element()
                            }
                            None => line.text.clone().into_any_element(),
                        };
                        div().id(("log-line", at)).whitespace_nowrap().px(px(space::S)).text_color(color).child(text).into_any_element()
                    })
                    .collect()
            }),
        )
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(self.longest))
        .track_scroll(&self.scroll)
        .size_full()
        .py(px(space::XS))
        .font_family(Theme::mono_family())
        .text_size(px(font::CALLOUT));
        let cut = self.cut.then(|| div().flex_none().px(px(space::S)).py(px(space::XS)).border_b_1().border_color(t.separator).text_size(px(font::CAPTION)).text_color(t.tertiary).child(trf("Ältere Zeilen stehen nur in der Datei. Gezeigt werden die letzten %lld.", &[&theme::files::LOG_LINES])));
        div().id("logs-viewer").flex_1().min_h_0().flex().flex_col().rounded(px(radius::CONTROL)).border_1().border_color(t.separator).bg(t.content).overflow_hidden().children(cut).child(div().flex_1().min_h_0().child(list)).into_any_element()
    }
}

fn file_items(mut menu: PopupMenu, choices: &[LogFile], running: bool) -> PopupMenu {
    for f in choices.iter().filter(|f| f.kind != Kind::Crash) {
        menu = menu.menu(file_label(f, running), Box::new(PickLog(f.path.display().to_string())));
    }
    let crashes: Vec<&LogFile> = choices.iter().filter(|f| f.kind == Kind::Crash).collect();
    if !crashes.is_empty() {
        menu = menu.separator().label(tr("Abstürze"));
        for f in crashes {
            let text = match &f.reason {
                Some(reason) => format!("{} · {}", files::stamp(f.modified), clip(reason, 60)),
                None => files::stamp(f.modified),
            };
            menu = menu.menu(text, Box::new(PickLog(f.path.display().to_string())));
        }
    }
    menu
}

fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", text.get(..at).unwrap_or_default()),
        None => text.to_owned(),
    }
}

fn at_end(scroll: &UniformListScrollHandle) -> bool {
    let state = scroll.0.borrow();
    let max = state.base_handle.max_offset().y;
    let slack = state.last_item_size.as_ref().map_or(px(0.), |size| size.item.height);
    max <= px(0.) || -state.base_handle.offset().y >= max - slack
}

impl Render for Logs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::get(cx);
        self.painted += 1;
        if !self.following {
            self.follow(cx);
        }
        let root = div()
            .id("logs")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .gap(px(space::S))
            .on_action(cx.listener(|s, _: &files::Find, window, cx| s.open_find(window, cx)))
            .on_action(cx.listener(|s, _: &files::FindNext, _, cx| s.jump(1, cx)))
            .on_action(cx.listener(|s, _: &files::FindPrevious, _, cx| s.jump(-1, cx)))
            .on_action(cx.listener(|s, _: &files::CopyAll, _, cx| s.copy_all(cx)))
            .on_action(cx.listener(|s, _: &files::Up, _, cx| s.scroll_by(-1., cx)))
            .on_action(cx.listener(|s, _: &files::Down, _, cx| s.scroll_by(1., cx)))
            .on_action(cx.listener(|s, _: &files::PageUp, _, cx| s.scroll_page(-1., cx)))
            .on_action(cx.listener(|s, _: &files::PageDown, _, cx| s.scroll_page(1., cx)))
            .on_action(cx.listener(|s, _: &files::First, _, cx| {
                s.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &files::Last, _, cx| {
                s.scroll.scroll_to_item(s.lines.len().saturating_sub(1), ScrollStrategy::Bottom);
                cx.notify();
            }));
        if !self.scanned {
            return root.child(div().size_full().flex().items_center().justify_center().child(sheet::waiting(tr("Protokolle werden gesucht …"), &t))).into_any_element();
        }
        if self.accounts.is_empty() || self.files.is_empty() && self.file.is_none() {
            let toolbar = (!self.accounts.is_empty()).then(|| self.toolbar(cx, &t));
            return root.children(toolbar).child(files::empty("logs-empty", "scroll-text", tr("Noch keine Protokolle"), tr("Sobald ein Account diese Instanz einmal gestartet hat, stehen seine Protokolle hier."), None, &t)).into_any_element();
        }
        let toolbar = self.toolbar(cx, &t);
        let find = self.find_bar(cx, &t);
        let banner = self.banner(cx, &t);
        let reason = self.reason(&t);
        let viewer = self.viewer(cx, &t);
        root.child(toolbar).children(find).children(banner).children(reason).child(viewer).into_any_element()
    }
}
