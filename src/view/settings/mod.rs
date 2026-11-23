//! The settings page: the few options that rarely change, saved the moment they are touched.

mod game;
mod look;
mod phone;
mod share;

use freya::prelude::*;

pub(crate) use self::game::gpu_label;
use self::game::GameGroup;
use self::look::LookGroup;
use self::phone::PhoneGroup;
use self::share::ShareGroup;
use crate::app::event::Event;
use crate::app::model::UpdateStatus;
use crate::core::i18n::{tr, trf};
use crate::core::settings::{self, HOTKEYS, PreviewRate, Settings};
use crate::view::kit::Bounds;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, group};
use crate::view::kit::row::Row;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::stepper::Stepper;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::line;
use crate::view::kit::{off_thread, open_external, use_theme};
use crate::view::sheets;
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, control, font, level, space};
use crate::view::updates;

const PAGE: f32 = 600.;
const JUMPS: f32 = 180.;
const SECTIONS: usize = 11;
const SPY: f32 = space::XL * 2.;
const ATTEMPTS: (i64, i64) = (1, 100);
const RATES: [PreviewRate; 3] = [PreviewRate::Saving, PreviewRate::Normal, PreviewRate::Smooth];

/// Changes settings and saves them; `rev` makes the page draw the new values, since the settings are not a state.
#[derive(Clone, Copy, PartialEq)]
pub struct Prefs {
    store: Store,
    rev: State<u64>,
}

impl Prefs {
    /// Whether the page that owns these settings is still there.
    pub fn alive(self) -> bool {
        self.rev.try_peek().is_some()
    }

    /// The settings now, read so the caller draws again when they change.
    pub fn read(self) -> Settings {
        let _ = *self.rev.read();
        settings::active().clone()
    }

    /// Applies `edit`, tells the games and the updater what concerns them, and saves in the background.
    pub fn change(mut self, edit: impl FnOnce(&mut Settings)) {
        let (before, next) = settings::update(edit);
        if std::ptr::eq(before, next) {
            return;
        }
        let badge = next.game_badge != before.game_badge;
        let sleep = next.prevent_sleep != before.prevent_sleep;
        let rate = next.preview_rate != before.preview_rate;
        let recheck = (next.auto_update && !before.auto_update) || next.prereleases != before.prereleases;
        if badge {
            self.store.dispatch(Event::TellGames);
        }
        if rate {
            self.store.replan();
        }
        if sleep {
            self.store.keep_awake();
        }
        if recheck {
            updates::check(self.store, false);
        }
        save(self.store);
        *self.rev.write() += 1;
    }
}

/// Writes the settings file off the UI thread; a failure shows in the notice strip.
pub fn save(store: Store) {
    let Some(rx) = off_thread("settings-save", settings::persist) else { return };
    spawn_in_window(async move {
        if let Ok(Err(e)) = rx.recv().await {
            store.dispatch(Event::Error(trf("Die Einstellungen wurden nicht gespeichert: %@", &[&format!("{e:#}")])));
        }
    });
}

/// Remembers whether version pickers also offer snapshots, pre-releases and old versions, and saves it.
pub(crate) fn remember_all_versions(store: Store, on: bool) {
    let mut shown = store.ui.all_versions;
    shown.set(on);
    settings::update(|s| s.all_versions = on);
    save(store);
}

fn use_prefs() -> Prefs {
    use_consume::<Prefs>()
}

/// A row with a switch; a click anywhere on the row flips it too.
fn toggle(p: Prefs, id: &str, label: String, hint: String, on: bool, apply: fn(&mut Settings, bool)) -> Element {
    Line::new(format!("{id}.row"), label.clone())
        .hint(hint)
        .control(Switch::new(id, label, on).on_toggle(move |v| p.change(|s| apply(s, v))))
        .on_press(move |_| p.change(|s| apply(s, !on)))
        .into_element()
}

fn attempts(p: Prefs, id: &str, label: String, value: u32, requires: Option<&str>, apply: fn(&mut Settings, u32)) -> Element {
    let enabled = requires.is_none();
    let reason = requires.map(|switch| trf("Erst „%@“ einschalten.", &[&tr(switch)]));
    let stepper = Stepper::new(id, label.clone(), i64::from(value), ATTEMPTS.0, ATTEMPTS.1).disabled(reason).on_change(move |v: i64| p.change(|s| apply(s, u32::try_from(v).unwrap_or(1))));
    Line::new(format!("{id}.row"), label).dim(!enabled).control(stepper).into_element()
}

pub(crate) fn keycap(t: &Theme, text: String) -> Element {
    rect().height(Size::px(control::S)).min_width(Size::px(control::S)).padding((0., space::S)).center().corner_radius(t.shape.small).background(t.control).child(line(t, text, font::CALLOUT, t.text1)).into_element()
}

/// The keys of a chord like `ctrl+alt+right` as this system's keyboard prints them.
pub(crate) fn chord_keys(chord: &str) -> Vec<String> {
    let mac = cfg!(target_os = "macos");
    chord
        .split('+')
        .map(|key| match key {
            "ctrl" if mac => "⌃".to_owned(),
            "ctrl" => tr("Strg"),
            "alt" if mac => "⌥".to_owned(),
            "alt" => "Alt".to_owned(),
            "shift" if mac => "⇧".to_owned(),
            "shift" => tr("Umschalt"),
            "cmd" | "meta" | "super" => if mac { "⌘".to_owned() } else { "Win".to_owned() },
            "left" => "←".to_owned(),
            "right" => "→".to_owned(),
            "up" => "↑".to_owned(),
            "down" => "↓".to_owned(),
            other => other.to_uppercase(),
        })
        .collect()
}

/// The whole settings page.
#[derive(PartialEq)]
pub struct SettingsPage;

impl Component for SettingsPage {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let rev = use_state(|| 0u64);
        let p = use_provide_context(|| Prefs { store, rev });
        let scroll = use_scroll_controller(ScrollConfig::default);
        let heights = use_state(|| [0f32; SECTIONS]);
        let picked = use_state(|| None::<usize>);
        let mut width = use_state(|| f32::INFINITY);
        let s = p.read();
        let ingame = group(
            &t,
            Some(tr("Im Spiel")),
            vec![
                toggle(p, "settings.sleep", tr("Computer wach halten, solange Spiele laufen"), tr("Der Bildschirm darf trotzdem ausgehen."), s.prevent_sleep, |s, on| s.prevent_sleep = on),
                toggle(p, "settings.badge", tr("Namensschild im Spiel"), tr("Zeigt oben links im Spiel, welcher Account es ist. F1 blendet es mit aus."), s.game_badge, |s, on| s.game_badge = on),
            ],
            None,
        );
        let connection = group(
            &t,
            Some(tr("Verbindung")),
            vec![
                toggle(p, "settings.reconnect", tr("Automatisch neu verbinden"), tr("Geht nach einem Verbindungsabbruch wieder auf den Server."), s.reconnect.enabled, |s, on| s.reconnect.enabled = on),
                attempts(p, "settings.reconnect.attempts", tr("Versuche beim Verbinden"), s.reconnect.max_attempts, (!s.reconnect.enabled).then_some("Automatisch neu verbinden"), |s, n| s.reconnect.max_attempts = n),
                toggle(p, "settings.restart", tr("Nach einem Absturz neu starten"), tr("Startet eine Instanz, deren Spiel unerwartet beendet wurde."), s.crash_restart.enabled, |s, on| s.crash_restart.enabled = on),
                attempts(p, "settings.restart.attempts", tr("Versuche beim Neustart"), s.crash_restart.max_attempts, (!s.crash_restart.enabled).then_some("Nach einem Absturz neu starten"), |s, n| s.crash_restart.max_attempts = n),
            ],
            Some(tr("Nach den letzten Versuchen zeigt die Kachel, dass der Account dich braucht. Gesperrte Accounts verbinden sich nie von selbst neu.")),
        );
        let notices = group(
            &t,
            Some(tr("Benachrichtigungen")),
            vec![
                toggle(p, "settings.notify.attention", tr("Bei Problemen"), tr("Eine Meldung pro Batch, wenn ein Account getrennt, gesperrt oder abgestürzt ist."), s.notifications.attention, |s, on| s.notifications.attention = on),
            ],
            Some(tr("Meldungen kommen gesammelt und nie mit Ton.")),
        );
        let chat = group(
            &t,
            Some(tr("Chat")),
            vec![
                toggle(p, "settings.chat.merged", tr("Gemeinsamer Chat"), tr("Sammelt die Chatzeilen aller Instanzen auf der Seite Chat."), s.merged_chat, |s, on| s.merged_chat = on),
                toggle(p, "settings.chat.send", tr("Nachrichten senden"), tr("Erlaubt auf der Seite Chat, eine Zeile an eine Instanz oder einen ganzen Batch zu schicken."), s.broadcast, |s, on| s.broadcast = on),
            ],
            None,
        );
        let hotkeys = group(
            &t,
            Some(tr("Tastenkürzel im Spiel")),
            HOTKEYS
                .iter()
                .enumerate()
                .map(|(i, (label, chord))| Line::new(format!("settings.hotkey.{i}"), tr(label)).control(rect().horizontal().spacing(space::XS).children(chord_keys(chord).into_iter().map(|k| keycap(&t, k)))).into_element())
                .collect(),
            Some(tr("Diese Kürzel gelten in jedem Spiel.")),
        );
        let rate = RATES.iter().position(|r| *r == s.preview_rate).unwrap_or(1);
        let rates = vec![
            Segment { id: "settings.rate.saving".into(), label: tr("Sparsam"), tip: None },
            Segment { id: "settings.rate.normal".into(), label: tr("Normal"), tip: None },
            Segment { id: "settings.rate.smooth".into(), label: tr("Flüssig"), tip: None },
        ];
        let advanced = group(
            &t,
            Some(tr("Erweitert")),
            vec![
                Line::new("settings.rate.row", tr("Vorschau-Rate"))
                    .hint(tr("Wie oft sich die Kacheln erneuern. Flüssig kostet die Spiele mehr Rechenzeit, Sparsam weniger."))
                    .control(Segmented::new(tr("Vorschau-Rate"), rates, rate).on_choose(move |i| p.change(|s| s.preview_rate = RATES[i.min(2)])))
                    .into_element(),
            ],
            None,
        );
        let sections: [Element; SECTIONS] = [LookGroup.into_element(), GameGroup.into_element(), ShareGroup.into_element(), ingame, connection, notices, PhoneGroup.into_element(), chat, hotkeys, UpdatesGroup.into_element(), advanced];
        let wide = *width.read() >= PAGE + (JUMPS + space::XL) * 2.;
        let mut column = rect().width(Size::fill()).max_width(Size::px(PAGE)).spacing(space::XL);
        for (i, section) in sections.into_iter().enumerate() {
            column = column.child(rect().width(Size::fill()).on_sized(move |e| measure(heights, i, Bounds::of(&e).h)).child(section));
        }
        let mut page = rect().width(Size::fill()).height(Size::fill()).on_sized(move |e| width.set_if_modified(Bounds::of(&e).w));
        if wide {
            page = page.child(rect().position(Position::new_absolute().left(space::XL).top(space::XL)).layer(Layer::Relative(level::FLOAT)).width(Size::px(JUMPS)).child(Jumps { scroll, heights, picked }));
        }
        page.child(ScrollView::new_controlled(scroll).width(Size::fill()).height(Size::fill()).child(rect().width(Size::fill()).padding(space::XL).cross_align(Alignment::Center).child(column)))
    }
}

fn measure(mut heights: State<[f32; SECTIONS]>, i: usize, h: f32) {
    let mut next = *heights.peek();
    if next[i] != h {
        next[i] = h;
        heights.set(next);
    }
}

fn titles() -> [String; SECTIONS] {
    ["Allgemein", "Spiel", "Teilen", "Im Spiel", "Verbindung", "Benachrichtigungen", "Aufs Handy", "Chat", "Tastenkürzel", "Updates", "Erweitert"].map(tr)
}

/// Where section `i` starts in the scrolled page.
fn top_of(heights: &[f32; SECTIONS], i: usize) -> f32 {
    space::XL + heights[..i].iter().map(|h| h + space::XL).sum::<f32>()
}

/// Which section the reader is in: the last one whose title reached the top, the one just picked while it is still
/// there, or at the end of the page the last one in view.
fn current(heights: &[f32; SECTIONS], y: f32, viewport: f32, end: bool, picked: Option<usize>) -> usize {
    let reading = y + space::XL + SPY;
    let seen = |i: usize| {
        let top = top_of(heights, i);
        top < if end { y + viewport } else { reading } && top + 1. >= y
    };
    if let Some(p) = picked.filter(|p| seen(*p)) {
        return p;
    }
    if end && let Some(last) = (0..SECTIONS).rev().find(|i| seen(*i)) {
        return last;
    }
    (0..SECTIONS).rev().find(|i| top_of(heights, *i) <= reading).unwrap_or(0)
}

/// The sections of the page as a list beside it; a window too narrow for it has no navigation at all, the page just scrolls.
#[derive(PartialEq)]
struct Jumps {
    scroll: ScrollController,
    heights: State<[f32; SECTIONS]>,
    picked: State<Option<usize>>,
}

impl Component for Jumps {
    fn render(&self) -> impl IntoElement {
        let mut scroll = self.scroll;
        let heights = *self.heights.read();
        let mut picked = self.picked;
        let y = -scroll.position().y;
        let (content, viewport) = scroll.bounds.read().map_or((0., 0.), |(c, v)| (c.height, v.height));
        let end = content > viewport && y + viewport >= content - 1.;
        let on = current(&heights, y, viewport, end, *picked.read());
        let mut jump = move |i: usize| {
            picked.set(Some(i));
            scroll.scroll_to_y(-(top_of(&heights, i) - space::XL) as i32);
        };
        rect()
            .width(Size::fill())
            .a11y_role(AccessibilityRole::Navigation)
            .a11y_alt(tr("Abschnitte"))
            .children(titles().into_iter().enumerate().map(|(i, title)| Row::new(format!("settings.jump.{i}"), title).selected(i == on).on_press(move |_| jump(i)).into_element()))
            .into_element()
    }
}

#[derive(PartialEq)]
struct UpdatesGroup;

impl Component for UpdatesGroup {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let p = use_prefs();
        let s = p.read();
        updates_group(&t, store, p, &s)
    }
}

fn updates_group(t: &Theme, store: Store, p: Prefs, s: &Settings) -> Element {
    let u = store.model.read().updates.clone();
    let blocked = updates::blocked(store);
    let action = match &u.status {
        UpdateStatus::Ready { .. } => Button::new("settings.update.restart", tr("Neu starten")).primary().disabled(blocked.clone()).on_press(move |_| sheets::updates::restart(store)),
        UpdateStatus::Downloading { .. } if u.cancelling => Button::new("settings.update.cancel", tr("Abbrechen")).disabled(Some(tr("Bricht ab …"))),
        UpdateStatus::Downloading { .. } => Button::new("settings.update.cancel", tr("Abbrechen")).on_press(move |_| updates::cancel(store)),
        UpdateStatus::Available { page, installs: false, .. } => {
            let page = page.clone();
            Button::new("settings.update.page", tr("Download-Seite öffnen")).on_press(move |_| open_external("open url", page.clone()))
        }
        UpdateStatus::Checking => Button::new("settings.update.check", tr("Nach Updates suchen")).disabled(Some(tr("Sucht …"))),
        _ => Button::new("settings.update.check", tr("Nach Updates suchen")).on_press(move |_| updates::check(store, true)),
    };
    let mut status = updates::status_line(&u);
    if blocked.is_some() && matches!(u.status, UpdateStatus::Ready { .. }) {
        status = format!("{status} {}", updates::launching_line());
    }
    let mut rows = vec![Line::new("settings.update.row", trf("Manycraft %@", &[&updates::version()])).hint(status).control(action).into_element()];
    if let Some((version, notes)) = u.news.clone() {
        rows.push(
            Line::new("settings.update.news", trf("Neu in %@", &[&version]))
                .hint(notes)
                .control(Button::new("settings.update.news.ok", tr("Ausblenden")).plain().on_press(move |_| updates::dismiss_news(store)))
                .into_element(),
        );
    }
    rows.push(toggle(p, "settings.update.auto", tr("Updates automatisch laden"), tr("Lädt neue Versionen im Hintergrund und installiert sie beim Beenden."), s.auto_update, |s, on| s.auto_update = on));
    rows.push(toggle(p, "settings.update.pre", tr("Vorabversionen"), tr("Bietet auch Testversionen an, bevor sie veröffentlicht sind."), s.prereleases, |s, on| s.prereleases = on));
    if let Some(previous) = u.previous.clone() {
        let mut note = trf("Version %@ ist noch da, falls die neue Probleme macht.", &[&previous]);
        if blocked.is_some() {
            note = format!("{note} {}", updates::launching_line());
        }
        rows.push(
            Line::new("settings.update.previous", tr("Vorherige Version"))
                .hint(note)
                .control(Button::new("settings.update.back", tr("Zurückkehren")).disabled(blocked).on_press(move |_| sheets::updates::go_back(store)))
                .into_element(),
        );
    }
    group(t, Some(tr("Updates")), rows, updates::trouble_line(&u))
}

#[cfg(test)]
mod tests {
    use super::{HOTKEYS, SECTIONS, chord_keys, current, space, top_of};

    #[test]
    fn chords_print_as_keys() {
        let keys = chord_keys("ctrl+alt+right");
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[2], "→");
        assert_eq!(chord_keys("ctrl+alt+g").last().map(String::as_str), Some("G"));
    }

    #[test]
    fn every_hotkey_has_a_label_and_a_chord() {
        for (label, chord) in HOTKEYS {
            assert!(!label.is_empty() && chord.contains('+'));
        }
    }

    #[test]
    fn the_spy_follows_the_title_at_the_top() {
        let mut heights = [300f32; SECTIONS];
        heights[0] = 220.;
        let viewport = 820.;
        assert_eq!(current(&heights, 0., viewport, false, None), 0);
        let second = top_of(&heights, 1);
        assert_eq!(current(&heights, second - space::XL * 4., viewport, false, None), 0);
        assert_eq!(current(&heights, second, viewport, false, None), 1);
        let content = top_of(&heights, SECTIONS - 1) + 300.;
        assert_eq!(current(&heights, content - viewport, viewport, true, None), SECTIONS - 1);
        assert_eq!(current(&heights, content - viewport, viewport, true, Some(SECTIONS - 2)), SECTIONS - 2);
    }
}