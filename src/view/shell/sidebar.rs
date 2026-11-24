use std::path::PathBuf;

use freya::prelude::*;

use crate::app::event::Event as AppEvent;
use crate::app::model::{Model, Phase, UpdateStatus, Updates};
use crate::core::i18n::{tr, trf};
use crate::core::instance::{Instance, Loader};
use crate::view::accounts::off;
use crate::view::commands::{self, Command, Want, can_want, want, want_blocked};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::dot;
use crate::view::kit::button::Button;
use crate::view::kit::menu::{self, Item, Menu};
use crate::view::kit::row::Row;
use crate::view::kit::text::{hairline, line, spacer};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{toast, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, font, frame, iconsize, level, space};
use crate::view::ui::{Filter, Page};

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
    if stopped == 0 {
        start = start.shortcut(tr("Kein Spiel zum Starten"));
    } else if let Some(k) = keys(Command::StartSelected) {
        start = start.shortcut(k);
    }
    items.push(start);
    let b = id.to_owned();
    let mut stop = Item::new("batch.stop", tr("Stoppen"), move |_| store.dispatch(AppEvent::StopBatch(b.clone()))).disabled(stoppable == 0);
    if stoppable == 0 {
        stop = stop.shortcut(tr("Nichts läuft"));
    } else if let Some(k) = keys(Command::StopSelected) {
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
        let who = crate::app::reduce::solo_pick(&m, made).and_then(|a| m.account(a)).map(|a| (a.id, a.name.clone()));
        match who {
            Some((account, name)) if m.sign_in_expired(account) => {
                let blocked = commands::sign_in_blocked(store);
                let item = Item::new("instance.play", trf("Anmelden als %@", &[&name]), move |_| crate::view::accounts::sign_in(store, vec![account])).disabled(blocked.is_some());
                match blocked {
                    Some(why) => item.shortcut(why),
                    None => item,
                }
            }
            who => {
                let label = who.map_or_else(|| tr("Spielen"), |(_, n)| trf("Spielen als %@", &[&n]));
                Item::new("instance.play", label, move |_| store.dispatch(AppEvent::PlayInstance { instance: i.clone(), account: None }))
            }
        }
    } else {
        Item::new("instance.stop", tr("Stoppen"), move |_| store.dispatch(AppEvent::Stop(alone.clone())))
    }];
    items.extend(wanted("instance.rename", tr("Umbenennen…"), Want::RenameInstance(id.to_owned()), store));
    let g = id.to_owned();
    items.push(Item::new("instance.group", tr("Gruppe…"), move |_| crate::view::library::assign(store, &g)));
    if made.loader.takes_mods() {
        items.extend(wanted("instance.mods.add", tr("Inhalte hinzufügen…"), Want::AddMods(id.to_owned()), store));
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
    let mut lines: Vec<BatchLine> = m
        .batches
        .iter()
        .map(|b| {
            let games = || m.games.values().filter(|g| g.batch_id == b.id);
            let total = games().count();
            let running = games().filter(|g| g.phase.is_active()).count();
            let fine = games().filter(|g| g.counts_as_running()).count();
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
                Some(run) => trf("Startet %lld/%lld", &[&run.launched, &run.total]),
                None => format!("{fine}/{total}"),
            };
            BatchLine { id: b.id.clone(), name: b.name.clone(), trailing, hint, attention, cancel: run.is_some() }
        })
        .collect();
    lines.sort_by_cached_key(|l| l.name.to_lowercase());
    lines
}

/// The colour chip of batch `id`, as the sidebar shows it.
pub fn swatch(t: &Theme, id: &str, side: f32) -> Element {
    let at = id.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(usize::from(b)));
    rect().width(Size::px(side)).height(Size::px(side)).corner_radius(t.shape.small).background(t.identity(at)).into_element()
}

/// The sidebar: Start, the library and Entdecken, then the batches and chat, the settings and the account that plays.
#[derive(PartialEq)]
pub struct Sidebar {
    /// Shown above the page in a narrow window, so never folded to the rail.
    pub overlay: bool,
}

impl Component for Sidebar {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let mut page = ui.page;
        let now = page.read().clone();
        let rail = *ui.rail.read() && !self.overlay;
        let width = if rail { frame::RAIL } else { *ui.sidebar_w.read() };
        let m = store.model.read();
        let mut drawer = ui.drawer;
        let go = move |to: Page| {
            move |_| {
                drawer.set_if_modified(false);
                page.set(to.clone());
            }
        };
        let row = |id: &str, label: String, glyph: Icon, selected: bool, tip: Option<Command>| {
            let r = Row::new(id, label.clone()).glyph(glyph).height(frame::ROW).selected(selected).compact(rail, None);
            (r, tip.map(Command::tip).or_else(|| rail.then_some(label)))
        };
        let attention = m.attention_count(None);
        let batches = batch_lines(&m);
        let games = m.games.values().filter(|g| !g.alone()).count();
        let running = m.games.values().filter(|g| !g.alone() && g.phase.is_active()).count();

        let mut top = rect().width(Size::fill()).spacing(1.);
        let (r, tip) = row("nav.start", tr("Start"), Icon::Home, now == Page::Start, Some(Command::ShowStart));
        top = top.child(WithTip::new(tip, r.on_press(go(Page::Start))));
        let (r, tip) = row("nav.library", tr("Instanzen"), Icon::Package, matches!(now, Page::Library | Page::Instance(_) | Page::Browse(..)), Some(Command::ShowLibrary));
        let r = if rail { r } else { r.trailing(count(&t, m.instances.len().to_string(), matches!(now, Page::Library | Page::Instance(_) | Page::Browse(..)))) };
        top = top.child(WithTip::new(tip, r.on_press(go(Page::Library))));
        let (r, tip) = row("nav.discover", tr("Entdecken"), Icon::Compass, now == Page::Discover, Some(Command::ShowDiscover));
        top = top.child(WithTip::new(tip, r.on_press(go(Page::Discover))));
        let (r, tip) = row("nav.wardrobe", tr("Garderobe"), Icon::Shirt, now == Page::Wardrobe, Some(Command::ShowWardrobe));
        top = top.child(WithTip::new(tip, r.on_press(go(Page::Wardrobe))));

        let can_batch = can_want(&Want::NewBatch);
        let on_batches = matches!(now, Page::Grid(Filter::All | Filter::Attention));
        let _ = store.chat.read();
        let new_chat = crate::view::chat::unread(&m, *ui.chat.left_at.read(), None);
        let mut end = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
        if attention > 0 {
            end = end.child(dot(t.attention));
        }
        if new_chat > 0 {
            end = end.child(dot(t.accent));
        }
        if !batches.is_empty() {
            end = end.child(count(&t, format!("{running}/{games}"), on_batches));
        }
        if !rail && can_batch {
            end = end.child(Button::icon("nav.batches.add", Icon::Plus, tr("Neuer Batch")).plain().tip(Command::NewBatch.tip()).disabled(want_blocked(&Want::NewBatch)).on_press(move |_| {
                want(store, Want::NewBatch);
            }));
        }
        let mark = (attention > 0).then(|| dot(t.attention)).or_else(|| (new_chat > 0).then(|| dot(t.accent)));
        let r = Row::new("nav.batches", tr("Batches")).glyph(Icon::Layers).height(frame::ROW).selected(on_batches).trailing(end).compact(rail, mark).on_press(go(Page::Grid(Filter::All)));
        let tip = match (rail, new_chat) {
            (true, _) => tr("Batches"),
            (false, 0) => Command::ShowAll.tip(),
            (false, n) => format!("{} · {}", Command::ShowAll.tip(), trf("%lld ungelesen", &[&n])),
        };
        let group = rect().width(Size::fill()).spacing(1.).child(WithTip::new(Some(tip), r));

        let mut list = ScrollView::new().width(Size::fill()).height(Size::flex(1.)).spacing(1.).drag_scrolling(false);
        for b in batches {
            let to = Page::Grid(Filter::Batch(b.id.clone()));
            let selected = now == to;
            let mut end = rect().horizontal().cross_align(Alignment::Center).spacing(space::XS);
            if b.attention > 0 {
                end = end.child(dot(t.attention));
            }
            end = end.child(count(&t, b.trailing.clone(), selected));
            if b.cancel {
                let id = b.id.clone();
                end = end.child(Button::icon(format!("nav.batch.{}.cancel", b.id), Icon::Close, tr("Start abbrechen")).plain().on_press(move |_| store.dispatch(AppEvent::CancelStart(Some(id.clone())))));
            }
            let id = b.id.clone();
            let mark = (b.attention > 0).then(|| dot(t.attention));
            let r = Row::new(format!("nav.batch.{}", b.id), b.name.clone())
                .leading(swatch(&t, &b.id, if rail { iconsize::S } else { iconsize::XS }))
                .height(frame::ROW)
                .selected(selected)
                .trailing(end)
                .compact(rail, mark)
                .on_press(go(to))
                .on_secondary(move |at| menu::open(store.ui, Menu::at(batch_items(store, &id), at)));
            let tip = if rail { format!("{} · {}", b.name, b.hint) } else { b.hint.clone() };
            let indent = if rail { 0. } else { space::S };
            list = list.child(rect().key(&b.id).width(Size::fill()).padding((0., 0., 0., indent)).child(WithTip::new(Some(tip), r)));
        }

        let mut foot = rect().width(Size::fill()).spacing(1.);
        if !rail {
            if let Some(news) = news_line(&t, store, &m.updates) {
                foot = foot.child(news);
            }
            let launching = m.games.values().any(|g| matches!(g.phase, Phase::Starting { .. }));
            if let Some(update) = update_row(&t, store, &m.updates, launching) {
                foot = foot.child(update);
            }
        }
        let (r, tip) = row("nav.settings", tr("Einstellungen"), Icon::Settings, matches!(now, Page::Settings(_)), Some(Command::OpenSettings));
        foot = foot.child(WithTip::new(tip, r.on_press(go(Page::Settings(Default::default())))));
        foot = foot.child(super::account::AccountSwitch { rail });

        let mut bar = rect()
            .a11y_role(AccessibilityRole::Navigation)
            .width(Size::px(width))
            .height(Size::fill())
            .background(t.sidebar)
            .padding((space::S, space::S))
            .spacing(space::XS)
            .child(top)
            .child(rect().width(Size::fill()).padding((space::XS, 0.)).child(hairline(&t)))
            .child(group)
            .child(list)
            .content(Content::flex())
            .child(hairline(&t))
            .child(foot);
        if rail {
            bar = bar.cross_align(Alignment::Center);
        }
        bar.child(spacer())
    }
}

/// How wide the sidebar is after its edge was dragged from `from` to `to` while it was `start` wide: a rail once pulled
/// well under the narrowest width, else that width clamped to its range.
pub fn resized(start: f32, from: f32, to: f32) -> Option<f32> {
    let raw = start + (to - from);
    (raw >= frame::SIDEBAR_MIN - frame::SIDEBAR_SNAP).then(|| raw.clamp(frame::SIDEBAR_MIN, frame::SIDEBAR_MAX))
}

/// The divider between the sidebar and the page, wide enough to take hold of: dragging it sets the sidebar's width, and
/// pulling it in far enough folds the sidebar to its rail.
#[derive(PartialEq)]
pub struct Grip;

impl Component for Grip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let ui = store.ui;
        let mut start = use_state(|| None::<(f32, f32)>);
        let spot = use_spot("sidebar.grip", "separator", &tr("Breite der Seitenleiste"), true);
        let handle = rect()
            .position(Position::new_absolute().left(-frame::SIDEBAR_GRIP / 2.).top(0.))
            .width(Size::px(frame::SIDEBAR_GRIP))
            .height(Size::fill())
            .layer(level::FLOAT)
            .cursor(CursorIcon::ColResize)
            .on_sized(spot)
            .on_pointer_down(move |e: Event<PointerEventData>| {
                if !e.data().is_primary() {
                    return;
                }
                let now = if *ui.rail.peek() { frame::RAIL } else { *ui.sidebar_w.peek() };
                let mut dragging = ui.sidebar_drag;
                dragging.set(true);
                start.set(Some((e.global_location().x as f32, now)));
                e.stop_propagation();
            })
            .on_global_pointer_move(move |e: Event<PointerEventData>| {
                let Some((from, width)) = *start.peek() else { return };
                let (mut rail, mut wide) = (ui.rail, ui.sidebar_w);
                match resized(width, from, e.global_location().x as f32) {
                    Some(w) => {
                        rail.set_if_modified(false);
                        wide.set_if_modified(w);
                    }
                    None => rail.set_if_modified(true),
                }
                e.stop_propagation();
            })
            .on_capture_global_pointer_up(move |e: Event<PointerEventData>| {
                if start.peek().is_some() {
                    start.set(None);
                    let mut dragging = ui.sidebar_drag;
                    dragging.set(false);
                    e.stop_propagation();
                }
            });
        rect().width(Size::px(1.)).height(Size::fill()).background(t.separator).child(handle)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_the_edge_clamps_the_width_to_its_range() {
        assert_eq!(resized(232., 232., 300.), Some(300.));
        assert_eq!(resized(232., 232., 900.), Some(frame::SIDEBAR_MAX));
        assert_eq!(resized(232., 232., 190.), Some(frame::SIDEBAR_MIN));
    }

    #[test]
    fn pulling_the_edge_far_in_folds_the_sidebar_and_pulling_out_of_the_rail_opens_it() {
        assert_eq!(resized(232., 232., 100.), None);
        assert_eq!(resized(frame::RAIL, frame::RAIL, frame::RAIL + 40.), None);
        assert_eq!(resized(frame::RAIL, frame::RAIL, frame::RAIL + 144.), Some(frame::SIDEBAR_MIN));
    }
}