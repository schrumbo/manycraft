use std::path::PathBuf;

use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::app::model::{Import, Model, Phase, UpdateStatus, Updates};
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Instance, Loader};
use crate::view::accounts::off;
use crate::view::commands::{Command, Want, can_want, want, want_blocked};
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::{Tone, badge, dot};
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::row::Row;
use crate::view::kit::text::{caption, hairline, line, spacer};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{toast, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, font, frame, iconsize, space};
use crate::view::ui::{Filter, Page};

const IMPORT_ROWS_MAX: usize = 4;

fn section(t: &Theme, title: String) -> impl IntoElement {
    rect().padding((space::M, space::S, space::XS, space::S)).child(line(t, title, font::CAPTION, t.text3).font_weight(FontWeight::SEMI_BOLD))
}

fn count(t: &Theme, text: String, selected: bool) -> Element {
    line(t, text, font::CAPTION, if selected { t.accent_text } else { t.text3 }).into_element()
}

fn open_with_system(target: &std::ffi::OsStr) -> std::io::Result<()> {
    let opener = if cfg!(windows) { "explorer" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener).arg(target).spawn()?.wait().map(drop)
}

/// Opens `dir` in the system's file manager, creating it first.
pub fn open_folder(store: Store, dir: PathBuf) {
    let fenced = crate::core::drive::fenced("open folder", dir.display());
    off("open-folder", move || std::fs::create_dir_all(&dir).and_then(|()| if fenced { Ok(()) } else { open_with_system(dir.as_os_str()) }), move |result: std::io::Result<()>| {
        if result.is_err() {
            toast::show(store.ui, tr("Der Ordner lässt sich nicht öffnen."), None);
        }
    });
}

fn wanted(id: &str, label: String, w: Want, store: Store) -> Option<Item> {
    can_want(&w).then(|| {
        Item::new(id, label, move |_| {
            want(store, w.clone());
        })
    })
}

/// The menu of a batch, in the sidebar and over its tiles; shortcuts show when its page is open, where they act on it.
pub fn batch_items(store: Store, id: &str) -> Vec<Item> {
    let m = store.model.peek();
    let shown = *store.ui.page.peek() == Page::Grid(Filter::Batch(id.to_owned()));
    let keys = |c: Command| if shown { c.shortcut().map(|s| s.label()) } else { None };
    let games = || m.games.values().filter(|g| g.batch_id == id);
    let stopped = games().filter(|g| !g.phase.is_active()).count();
    let stoppable = games().filter(|g| g.phase.stoppable()).count();
    let busy = games().any(|g| g.phase.is_active());
    let exporting = m.instance_of(id).is_some_and(|i| m.exporting.contains(&i.id));
    let dir = m.instance_of(id).map(Instance::dir);
    let mut items = Vec::new();
    if let Some(run) = m.run(Some(id)) {
        if run.memory > 0 {
            let b = id.to_owned();
            items.push(Item::new("batch.start.anyway", tr("Trotzdem alle starten"), move |_| store.dispatch(AppEvent::StartAnyway(Some(b.clone())))));
        }
        let b = id.to_owned();
        items.push(Item::new("batch.start.cancel", tr("Start abbrechen"), move |_| store.dispatch(AppEvent::CancelStart(Some(b.clone())))));
        items.push(Item::Separator);
    }
    let b = id.to_owned();
    let mut start = Item::new("batch.start", tr("Starten"), move |_| store.dispatch(AppEvent::StartBatch(b.clone()))).disabled(stopped == 0);
    if let Some(k) = keys(Command::StartSelected) {
        start = start.shortcut(k);
    }
    items.push(start);
    let b = id.to_owned();
    let mut stop = Item::new("batch.stop", tr("Stoppen"), move |_| store.dispatch(AppEvent::StopBatch(b.clone()))).disabled(stoppable == 0);
    if let Some(k) = keys(Command::StopSelected) {
        stop = stop.shortcut(k);
    }
    items.push(stop);
    items.push(Item::Separator);
    items.extend(wanted("batch.edit", tr("Bearbeiten…"), Want::EditBatch(id.to_owned()), store));
    if let Some(dir) = dir {
        items.push(Item::new("batch.folder", tr("Ordner öffnen"), move |_| open_folder(store, dir.clone())));
    }
    if !exporting {
        items.extend(wanted("batch.export", tr("Als .mrpack exportieren…"), Want::ExportBatch(id.to_owned()), store));
    }
    if can_want(&Want::DeleteBatch(id.to_owned())) {
        items.push(Item::Separator);
        let b = id.to_owned();
        let mut delete = Item::new("batch.delete", tr("Löschen…"), move |_| {
            want(store, Want::DeleteBatch(b.clone()));
        })
        .danger()
        .disabled(busy);
        if busy {
            delete = delete.shortcut(tr("Erst stoppen"));
        }
        items.push(delete);
    }
    items
}

/// The menu of an instance, in the sidebar and over the tiles it plays alone.
pub fn instance_items(store: Store, id: &str) -> Vec<Item> {
    let m = store.model.peek();
    let Some(made) = m.instance(id) else { return Vec::new() };
    let users: Vec<String> = m.batches.iter().filter(|b| b.instance_id == id).map(|b| b.name.clone()).collect();
    let running = m.games.values().any(|g| g.instance_id == id && g.phase.is_active());
    let pinned = matches!(made.loader, Loader::Fabric(Some(_)) | Loader::Quilt(Some(_)));
    let dir = made.dir();
    let i = id.to_owned();
    let alone: Vec<String> = m.solo_games(id).filter(|g| g.phase.is_active()).map(|g| g.key.clone()).collect();
    let mut items = vec![if alone.is_empty() {
        let who = crate::app::reduce::solo_account(&m, made).and_then(|a| m.account(a)).map(|a| (a.id, a.name.clone()));
        match who {
            Some((account, name)) if m.sign_in_expired(account) => Item::new("instance.play", trf("Anmelden als %@", &[&name]), move |_| crate::view::accounts::sign_in(store, vec![account])),
            who => {
                let label = who.map_or_else(|| tr("Spielen"), |(_, n)| trf("Spielen als %@", &[&n]));
                Item::new("instance.play", label, move |_| store.dispatch(AppEvent::PlayInstance { instance: i.clone(), account: None }))
            }
        }
    } else {
        Item::new("instance.stop", tr("Stoppen"), move |_| store.dispatch(AppEvent::Stop(alone.clone())))
    }];
    items.extend(wanted("instance.rename", tr("Umbenennen…"), Want::RenameInstance(id.to_owned()), store));
    if made.loader.takes_mods() {
        items.extend(wanted("instance.mods.add", tr("Mods hinzufügen…"), Want::AddMods(id.to_owned()), store));
        items.extend(wanted("instance.mods.edit", tr("Mods verwalten…"), Want::EditMods(id.to_owned()), store));
    }
    if pinned {
        items.extend(wanted("instance.loader", tr("Loader aktualisieren…"), Want::UpdateLoader(id.to_owned()), store));
    }
    items.extend(wanted("instance.clone", tr("Klonen"), Want::CloneInstance(id.to_owned()), store));
    items.extend(wanted("instance.batch", tr("Neuer Batch mit dieser Instanz…"), Want::NewBatchFor(id.to_owned()), store));
    items.push(Item::new("instance.folder", tr("Ordner öffnen"), move |_| open_folder(store, dir.clone())));
    items.extend(wanted("instance.shortcut", tr("Verknüpfung erstellen"), Want::CreateShortcut(id.to_owned()), store));
    items.extend(wanted("instance.export", tr("Als .mrpack exportieren…"), Want::ExportInstance(id.to_owned()), store));
    let mut tail = Vec::new();
    if can_want(&Want::ResetInstance(id.to_owned())) {
        let i = id.to_owned();
        let mut reset = Item::new("instance.reset", tr("Zurücksetzen…"), move |_| {
            want(store, Want::ResetInstance(i.clone()));
        })
        .disabled(running);
        if running {
            reset = reset.shortcut(tr("Erst stoppen"));
        }
        tail.push(reset);
    }
    if can_want(&Want::DeleteInstance(id.to_owned())) {
        let i = id.to_owned();
        let mut delete = Item::new("instance.delete", tr("Löschen…"), move |_| {
            want(store, Want::DeleteInstance(i.clone()));
        })
        .danger()
        .disabled(!users.is_empty());
        if !users.is_empty() {
            delete = delete.shortcut(crate::core::fleet::used_by(&users));
        }
        tail.push(delete);
    }
    if !tail.is_empty() {
        items.push(Item::Separator);
        items.extend(tail);
    }
    items
}

struct BatchLine {
    id: String,
    name: String,
    trailing: String,
    hint: String,
    attention: usize,
    cancel: bool,
}

fn batch_lines(m: &Model) -> Vec<BatchLine> {
    m.batches
        .iter()
        .map(|b| {
            let games = || m.games.values().filter(|g| g.batch_id == b.id);
            let total = games().count();
            let running = games().filter(|g| g.phase.is_active()).count();
            let fine = games().filter(|g| g.phase.is_active() && !g.phase.waiting() && !g.phase.needs_attention()).count();
            let attention = m.attention_count(Some(&b.id));
            let run = m.run(Some(&b.id));
            let hint = match (total, running) {
                _ if run.is_some() => run.as_ref().map(crate::ui::model::run_line).unwrap_or_default(),
                (0, _) => tr("Noch keiner da."),
                (1, 0) if attention == 0 => tr("Schläft noch."),
                (_, 0) if attention == 0 => trf("Alle %lld schlummern.", &[&total]),
                _ => trf("%lld von %lld laufen", &[&fine, &total]),
            };
            let trailing = match &run {
                _ if m.exporting.contains(&b.instance_id) => tr("Wird exportiert …"),
                Some(run) => format!("{}/{}", run.launched, run.total),
                None => format!("{fine}/{total}"),
            };
            BatchLine { id: b.id.clone(), name: b.name.clone(), trailing, hint, attention, cancel: run.is_some() }
        })
        .collect()
}

/// The sidebar: the grid of all games and of those that need you, accounts and chat, then every batch and instance.
#[derive(PartialEq)]
pub struct Sidebar;

impl Component for Sidebar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let mut page = store.ui.page;
        let now = page.read().clone();
        crate::view::instance::use_minute(store, true);
        let up = store.ui.instance.up.read().clone();
        let m = store.model.read();
        let attention = m.attention_count(None);
        let go = move |to: Page| move |_| page.set(to.clone());

        let mut top = rect().width(Size::fill()).spacing(1.);
        top = top.child(Row::new("nav.all", tr("Alle")).glyph(Icon::Grid).height(frame::ROW).selected(now == Page::Grid(Filter::All)).trailing(count(&t, m.games.values().filter(|g| !g.alone()).count().to_string(), now == Page::Grid(Filter::All))).on_press(go(Page::Grid(Filter::All))));
        let mut need = Row::new("nav.attention", tr("Brauchen dich")).glyph(Icon::Alert).height(frame::ROW).selected(now == Page::Grid(Filter::Attention)).on_press(go(Page::Grid(Filter::Attention)));
        if attention > 0 {
            need = need.trailing(badge(&t, attention.to_string(), Tone::Attention));
        }
        top = top.child(need);
        top = top.child(Row::new("nav.accounts", tr("Accounts")).glyph(Icon::Users).height(frame::ROW).selected(now == Page::Accounts).trailing(count(&t, m.accounts.len().to_string(), now == Page::Accounts)).on_press(go(Page::Accounts)));
        top = top.child(Row::new("nav.chat", tr("Chat")).glyph(Icon::Message).height(frame::ROW).selected(now == Page::Chat).on_press(go(Page::Chat)));

        let mut list = ScrollView::new().width(Size::fill()).height(Size::flex(1.)).spacing(1.).drag_scrolling(false);
        if !m.batches.is_empty() || !m.imports.is_empty() || can_want(&Want::NewBatch) {
            list = list.child(section(&t, tr("Batches")));
            for b in batch_lines(&m) {
                let to = Page::Grid(Filter::Batch(b.id.clone()));
                let selected = now == to;
                let mut end = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
                if b.attention > 0 {
                    end = end.child(badge(&t, b.attention.to_string(), Tone::Attention));
                }
                end = end.child(count(&t, b.trailing, selected));
                if b.cancel {
                    let id = b.id.clone();
                    end = end.child(Button::icon(format!("nav.batch.{}.cancel", b.id), Icon::Close, tr("Start abbrechen")).plain().on_press(move |_| store.dispatch(AppEvent::CancelStart(Some(id.clone())))));
                }
                let id = b.id.clone();
                let row = Row::new(format!("nav.batch.{}", b.id), b.name).glyph(Icon::Layers).height(frame::ROW).selected(selected).trailing(end).on_press(go(to)).on_secondary(move |at| menu::open(store.ui, Menu::at(batch_items(store, &id), at)));
                list = list.child(WithTip::new(Some(b.hint), row));
            }
            for import in m.imports.iter().take(IMPORT_ROWS_MAX) {
                list = list.child(ImportRow { import: import.clone() });
            }
            let more = m.imports.len().saturating_sub(IMPORT_ROWS_MAX);
            if more > 0 {
                list = list.child(rect().height(Size::px(frame::ROW)).padding((0., space::S)).main_align(Alignment::Center).child(caption(&t, trf("und %lld weitere", &[&more]))));
            }
            if can_want(&Want::NewBatch) {
                list = list.child(new_row(&t, "nav.batch.new", tr("Neuer Batch"), move || {
                    want(store, Want::NewBatch);
                }));
            }
        }
        if !m.instances.is_empty() || can_want(&Want::NewInstance) {
            list = list.child(section(&t, tr("Instanzen")));
            let mut made: Vec<&Instance> = m.instances.iter().collect();
            made.sort_by_cached_key(|i| i.name.to_lowercase());
            for i in made {
                let to = Page::Instance(i.id.clone());
                let selected = now == to;
                let running = m.games.values().any(|g| g.instance_id == i.id && g.phase.is_active());
                let since = m.solo_games(&i.id).filter(|g| g.phase.is_active()).filter_map(|g| up.get(&g.key)).min().copied();
                let mut end = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
                if running {
                    end = end.child(dot(t.success));
                }
                let alone = m.solo_games(&i.id).any(|g| g.phase.is_active());
                match since.and_then(crate::view::instance::uptime_text) {
                    Some(up) => end = end.child(count(&t, up, selected)),
                    None if alone => {}
                    None => end = end.child(count(&t, i.loader.name().to_owned(), selected)),
                }
                let tip = match since.and_then(crate::view::instance::uptime_text) {
                    Some(up) => Some(trf("Läuft · %@", &[&up])),
                    None => running.then(|| tr("Läuft gerade")),
                };
                let id = i.id.clone();
                let row = Row::new(format!("nav.instance.{}", i.id), i.name.clone()).glyph(Icon::Package).height(frame::ROW).selected(selected).trailing(end).on_press(go(to)).on_secondary(move |at| menu::open(store.ui, Menu::at(instance_items(store, &id), at)));
                list = list.child(WithTip::new(tip, row));
            }
            if can_want(&Want::NewInstance) {
                list = list.child(new_row(&t, "nav.instance.new", tr("Neue Instanz"), move || {
                    want(store, Want::NewInstance);
                }));
            }
        }

        let mut foot = rect().width(Size::fill()).spacing(1.);
        if let Some(news) = news_line(&t, store, &m.updates) {
            foot = foot.child(news);
        }
        let launching = m.games.values().any(|g| matches!(g.phase, Phase::Starting { .. }));
        if let Some(update) = update_row(&t, store, &m.updates, launching) {
            foot = foot.child(update);
        }
        if can_want(&Want::OpenTrash) {
            foot = foot.child(Row::new("nav.trash", tr("Papierkorb")).glyph(Icon::Trash).height(frame::ROW).on_press(move |_| {
                want(store, Want::OpenTrash);
            }));
        }
        foot = foot.child(WithTip::new(Some(Command::OpenSettings.tip()), Row::new("nav.settings", tr("Einstellungen")).glyph(Icon::Settings).height(frame::ROW).selected(now == Page::Settings).on_press(go(Page::Settings))));
        foot = foot.child(super::account::AccountSwitch);

        rect()
            .a11y_role(AccessibilityRole::Navigation)
            .width(Size::px(frame::SIDEBAR))
            .height(Size::fill())
            .background(t.sidebar)
            .padding((space::M, space::S))
            .spacing(space::XS)
            .child(top)
            .child(list)
            .content(Content::flex())
            .child(hairline(&t))
            .child(foot)
            .child(spacer())
    }
}

fn new_row(t: &Theme, id: &str, label: String, mut run: impl FnMut() + 'static) -> Element {
    Row::new(id, label).leading(icon(t, Icon::Plus, iconsize::S, t.text3)).height(frame::ROW).on_press(move |_| run()).into_element()
}

struct ImportRow {
    import: Import,
}

impl PartialEq for ImportRow {
    fn eq(&self, other: &Self) -> bool {
        self.import.id == other.import.id && self.import.name == other.import.name && self.import.percent == other.import.percent && std::sync::Arc::ptr_eq(&self.import.cancel, &other.import.cancel)
    }
}

impl Component for ImportRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut asked = use_state(|| false);
        let flag = self.import.cancel.clone();
        let stopping = *asked.read() || flag.load(std::sync::atomic::Ordering::SeqCst);
        let state = if stopping { tr("Bricht ab …") } else { self.import.percent.map_or_else(|| tr("Prüft …"), |p| format!("{p} %")) };
        let stop = move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(mut a) = asked.try_write() {
                *a = true;
            }
        };
        let mut press = stop.clone();
        let end = rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS)
            .child(count(&t, state, false))
            .child(Button::icon(format!("nav.import.{}.cancel", self.import.id), Icon::Close, tr("Import abbrechen")).plain().disabled(stopping.then(|| tr("Bricht ab …"))).on_press(move |_| press()));
        let store = use_store();
        let row = Row::new(format!("nav.import.{}", self.import.id), self.import.name.clone()).leading(icon(&t, Icon::Download, iconsize::S, t.text3)).height(frame::ROW).trailing(end).on_secondary(move |at| {
            let mut run = stop.clone();
            menu::open(store.ui, Menu::at(vec![Item::new("import.cancel", tr("Import abbrechen"), move |_| run()).disabled(stopping)], at));
        });
        WithTip::new(Some(trf("„%@“ wird als Instanz importiert.", &[&self.import.name])), row)
    }
}

fn news_line(t: &Theme, store: Store, updates: &Updates) -> Option<Element> {
    let (version, notes) = updates.news.clone()?;
    let text = if notes.is_empty() { trf("Neu in %@", &[&version]) } else { trf("Neu in %@: %@", &[&version, &notes]) };
    let close = Button::icon("nav.news.close", Icon::Close, tr("Ausblenden")).plain().disabled(want_blocked(&Want::DismissNews)).on_press(move |_| {
        want(store, Want::DismissNews);
    });
    Some(
        rect()
            .width(Size::fill())
            .height(Size::px(frame::ROW))
            .padding((0., 0., 0., space::S))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .child(icon(t, Icon::Sparkles, iconsize::S, t.text3))
            .child(rect().width(Size::flex(1.)).child(WithTip::new(Some(text.clone()), line(t, text, font::CAPTION, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis))))
            .content(Content::flex())
            .child(close)
            .into_element(),
    )
}

fn update_row(t: &Theme, store: Store, updates: &Updates, launching: bool) -> Option<Element> {
    let row = |id: &str, label: String, glyph: Icon| Row::new(id, label).leading(icon(t, glyph, iconsize::S, t.accent_text)).height(frame::ROW);
    let ask = move |w: Want| {
        move |_| {
            want(store, w.clone());
        }
    };
    let el = match updates.status.clone() {
        UpdateStatus::Ready { version } => {
            let blocked = if launching { Some(crate::ui::updates::launching_line()) } else { want_blocked(&Want::RestartToUpdate) };
            let restart = Button::new("nav.update.restart", tr("Neu starten")).primary().disabled(blocked.clone()).on_press(ask(Want::RestartToUpdate));
            let tip = blocked.unwrap_or_else(|| trf("Version %@ ist geladen und wird beim Beenden installiert.", &[&version]));
            WithTip::new(Some(tip), row("nav.update.ready", tr("Update bereit"), Icon::Download).trailing(restart)).into_element()
        }
        UpdateStatus::Available { version, installs: true, .. } if updates.cancelled => row("nav.update.cancelled", tr("Abgebrochen"), Icon::Download).trailing(count(t, version, false)).into_element(),
        UpdateStatus::Available { version, page, installs: false } if page.starts_with("https://github.com/") => {
            let url = page.clone();
            WithTip::new(
                Some(trf("Version %@ auf GitHub ansehen", &[&version])),
                row("nav.update.page", tr("Update verfügbar"), Icon::Download).trailing(count(t, version, false)).on_press(move |_| {
                    if !crate::core::drive::fenced("open url", &url) {
                        let url = url.clone();
                        off("open-url", move || open_with_system(std::ffi::OsStr::new(&url)), |_| {});
                    }
                }),
            )
            .into_element()
        }
        UpdateStatus::Available { version, installs: true, .. } => {
            let tip = want_blocked(&Want::FetchUpdate).unwrap_or_else(|| trf("Version %@ laden", &[&version]));
            WithTip::new(Some(tip), row("nav.update.fetch", tr("Update verfügbar"), Icon::Download).trailing(count(t, version, false)).on_press(ask(Want::FetchUpdate))).into_element()
        }
        UpdateStatus::Checking if updates.asked => row("nav.update.checking", tr("Sucht …"), Icon::Refresh).into_element(),
        UpdateStatus::Downloading { version, percent } => {
            let stopping = updates.cancelling;
            let done = if stopping { tr("Bricht ab …") } else { percent.map_or_else(|| version.clone(), |p| format!("{p} %")) };
            let blocked = if stopping { Some(tr("Bricht ab …")) } else { want_blocked(&Want::CancelUpdate) };
            let cancel = Button::icon("nav.update.cancel", Icon::Close, tr("Laden abbrechen")).plain().disabled(blocked).on_press(ask(Want::CancelUpdate));
            let end = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS).child(count(t, done, false)).child(cancel);
            let tip = if stopping { trf("Laden von Version %@ wird abgebrochen …", &[&version]) } else { trf("Version %@ wird geladen.", &[&version]) };
            WithTip::new(Some(tip), row("nav.update.downloading", tr("Wird geladen"), Icon::Download).trailing(end)).into_element()
        }
        _ => return None,
    };
    Some(el)
}
