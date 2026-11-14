//! The settings window: the few options that rarely change, saved the moment they are touched.

use gpui_kit::base::{Scrollbar, ScrollbarMode};
use gpui_kit::component::switch::Switch;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{OpenSettings, PreviousVersion};
use super::drive::Mark;
use super::motion::Soft;
use super::model::{self, UpdateStatus};
use super::sheet::{self, Look};
use super::theme::{self, Theme, control, font, iconsize, radius, space, tint};
use super::widgets::{KeyNav, icon, tip};
use crate::core::i18n::{tr, trf};
use crate::core::settings::{self, HOTKEYS, Settings};

const WIDTH: f32 = 560.;
const HEIGHT: f32 = 700.;
const TALL_HEIGHT: f32 = 880.;
const TALL_SCREEN: f32 = 1000.;
const MIN_WIDTH: f32 = 440.;
const MIN_HEIGHT: f32 = 360.;
const PAGE_MAX: f32 = 520.;
const ATTEMPTS: std::ops::RangeInclusive<u32> = 1..=100;

actions!(settings_window, [Close]);

struct Open(AnyWindowHandle);

impl Global for Open {}

/// Registers the shortcut, the macOS app menu entry and the action that opens the window.
pub fn init(cx: &mut App) {
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    cx.bind_keys([KeyBinding::new(Box::leak(format!("{m}-,").into_boxed_str()), OpenSettings, None), KeyBinding::new("escape", Close, Some("Settings"))]);
    cx.on_action(|_: &OpenSettings, cx| cx.defer(open));
    #[cfg(target_os = "macos")]
    cx.set_menus([Menu::new("Manycraft").items([MenuItem::action(tr("Einstellungen …"), OpenSettings)])]);
}

fn existing(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<Open>().map(|o| o.0).filter(|h| cx.windows().contains(h))
}

/// Opens the settings window, or brings the open one to the front.
pub fn open(cx: &mut App) {
    if let Some(handle) = existing(cx) {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let roomy = cx.primary_display().is_some_and(|d| f32::from(d.bounds().size.height) >= TALL_SCREEN);
    let bounds = Bounds::centered(None, size(px(WIDTH), px(if roomy { TALL_HEIGHT } else { HEIGHT })), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
        app_id: Some("io.github.schrumbo.manycraft".into()),
        ..Default::default()
    };
    let options = super::chrome::window_options(options, title());
    if let Ok((handle, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::refresh(window, cx);
        cx.new(|cx| SettingsView::new(window, cx))
    }) {
        cx.set_global(Open(handle));
    }
}

fn title() -> String {
    format!("{} – Manycraft", tr("Einstellungen"))
}

struct SettingsView {
    focus: FocusHandle,
    scroll: ScrollHandle,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::refresh(window, cx);
                cx.notify();
            }),
            cx.observe(&model::get(cx), |_, _, cx| cx.notify()),
        ];
        let focus = cx.focus_handle();
        let first = focus.clone();
        window.on_next_frame(move |window, cx| window.focus(&first, cx));
        Self { focus, scroll: ScrollHandle::new(), error: None, _subs: subs }
    }

    fn change(&mut self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let before = settings::active().clone();
        let mut next = before.clone();
        edit(&mut next);
        let recheck = (next.auto_update && !before.auto_update) || next.prereleases != before.prereleases;
        settings::set(next);
        if recheck {
            super::updates::check(false, cx);
        }
        let task = cx.background_executor().spawn(async { settings::persist() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.error = result.err().map(|e| trf("Die Einstellungen wurden nicht gespeichert: %@", &[&format!("{e:#}")]));
                cx.notify();
            });
        })
        .detach();
        cx.refresh_windows();
        cx.notify();
    }

    fn toggle(&self, id: &'static str, label: String, hint: String, on: bool, apply: impl Fn(&mut Settings, bool) + Clone + 'static, t: &Theme, cx: &mut Context<Self>) -> Div {
        let flip = apply.clone();
        let control = Switch::new(id).checked(on).accessibility_label(label.clone()).on_click(cx.listener(move |s, on: &bool, _, cx| {
            let on = *on;
            s.change(|st| apply(st, on), cx);
        }));
        let press = cx.listener(move |s, _: &ClickEvent, _, cx| s.change(|st| flip(st, !on), cx));
        row_with(label, Some(hint), control, Some((SharedString::from(format!("{id}-label")), Box::new(press))), t)
    }

    fn attempts(&self, id: &'static str, label: String, value: u32, enabled: bool, apply: impl Fn(&mut Settings, u32) + Clone + 'static, t: &Theme, cx: &mut Context<Self>) -> Div {
        let (less, more) = (apply.clone(), apply);
        let can_less = enabled && value > *ATTEMPTS.start();
        let can_more = enabled && value < *ATTEMPTS.end();
        let minus = step_button(SharedString::from(format!("{id}-less")), "minus", tr("Weniger"), can_less, t).when(can_less, |b| {
            b.on_click(cx.listener(move |s, _, _, cx| {
                s.change(|st| less(st, value.saturating_sub(1).max(*ATTEMPTS.start())), cx);
            }))
        });
        let plus = step_button(SharedString::from(format!("{id}-more")), "plus", tr("Mehr"), can_more, t).when(can_more, |b| {
            b.on_click(cx.listener(move |s, _, _, cx| {
                s.change(|st| more(st, (value + 1).min(*ATTEMPTS.end())), cx);
            }))
        });
        let control = div().flex().items_center().gap(px(space::XS)).child(minus).child(div().id(SharedString::from(format!("{id}-value"))).mark("value", value.to_string()).min_w(px(control::M)).text_center().text_color(if enabled { t.primary } else { t.tertiary }).child(value.to_string())).child(plus);
        row(label, None, control, t)
    }
}

fn step_button(id: SharedString, name: &str, label: String, enabled: bool, t: &Theme) -> Stateful<Div> {
    let button = div().id(id).mark("button", label.clone()).size(px(control::S)).flex_none().rounded(px(radius::INNER)).bg(t.raised).flex().items_center().justify_center();
    if !enabled {
        return button.opacity(tint::DISABLED).cursor_not_allowed().tooltip(tip(label)).child(icon(name, iconsize::S, t.secondary));
    }
    button.cursor_pointer().key_nav().soft(t.ink.opacity(tint::HOVER), radius::INNER).tooltip(tip(label)).child(icon(name, iconsize::S, t.primary))
}

type Press = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

fn row(label: String, hint: Option<String>, control: impl IntoElement, t: &Theme) -> Div {
    row_with(label, hint, control, None, t)
}

fn row_with(label: String, hint: Option<String>, control: impl IntoElement, press: Option<(SharedString, Press)>, t: &Theme) -> Div {
    let lines = (div().text_size(px(font::BODY)).text_color(t.primary).child(label), hint.map(|h| div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(h)));
    let text = match press {
        Some((id, press)) => div()
            .id(id)
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .cursor_pointer()
            .soft(t.ink.opacity(tint::HOVER), radius::INNER)
            .on_click(press)
            .child(lines.0)
            .children(lines.1)
            .into_any_element(),
        None => div().flex_1().min_w_0().flex().flex_col().child(lines.0).children(lines.1).into_any_element(),
    };
    div()
        .flex()
        .items_center()
        .gap(px(space::L))
        .px(px(space::L))
        .py(px(space::M))
        .min_h(px(control::L + space::M))
        .child(text)
        .child(div().flex_none().child(control))
}

fn group(title: String, rows: Vec<Div>, footer: Option<String>, t: &Theme) -> Div {
    let last = rows.len().saturating_sub(1);
    let rows = rows.into_iter().enumerate().map(|(i, r)| if i < last { r.border_b_1().border_color(t.separator) } else { r });
    div()
        .flex()
        .flex_col()
        .gap(px(space::S))
        .child(div().px(px(space::XS)).text_size(px(font::CALLOUT)).font_weight(FontWeight::SEMIBOLD).text_color(t.secondary).child(title))
        .child(div().flex().flex_col().rounded(px(radius::CARD)).bg(t.content).border_1().border_color(t.separator).overflow_hidden().children(rows))
        .children(footer.map(|f| div().px(px(space::XS)).text_size(px(font::CAPTION)).text_color(t.tertiary).child(f)))
}

fn keycap(text: String, t: &Theme) -> Div {
    div()
        .h(px(control::S))
        .min_w(px(control::S))
        .px(px(space::S))
        .rounded(px(radius::SMALL))
        .bg(t.raised)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(font::CALLOUT))
        .font_family(Theme::font_family())
        .text_color(t.primary)
        .child(text)
}

/// The keys of a chord like `ctrl+alt+right` as they are printed on this system's keyboard.
fn chord_keys(chord: &str) -> Vec<String> {
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

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        super::perf::frame_start();
        super::theme::follow_focus(window, cx);
        let t = Theme::get(cx);
        let s = settings::active().clone();
        let updates = model::get(cx).read(cx).updates.clone();
        let searching = matches!(updates.status, UpdateStatus::Checking) || (updates.asked && matches!(updates.status, UpdateStatus::Downloading { .. }));
        let search = sheet::button("set-update-now", tr("Nach Updates suchen"), None, Look::Secondary, false, searching, &t).when(!searching, |b| b.on_click(|_, _, cx| super::updates::check(true, cx)));
        let mut rows = vec![
            row(trf("Manycraft %@", &[&env!("CARGO_PKG_VERSION")]), Some(super::updates::status_line(&updates)), search, &t),
            self.toggle("set-auto-update", tr("Updates automatisch laden"), tr("Lädt neue Versionen im Hintergrund und installiert sie beim Beenden."), s.auto_update, |st, on| st.auto_update = on, &t, cx),
            self.toggle("set-prereleases", tr("Vorabversionen"), tr("Bietet auch Testversionen an, bevor sie veröffentlicht sind."), s.prereleases, |st, on| st.prereleases = on, &t, cx),
        ];
        if let Some(previous) = updates.previous.clone() {
            let back = sheet::button("set-previous", tr("Zurückkehren"), Some("undo-2"), Look::Secondary, false, false, &t).on_click(|_, window, cx| window.dispatch_action(Box::new(PreviousVersion), cx));
            rows.push(row(tr("Vorherige Version"), Some(trf("Version %@ ist noch da, falls die neue Probleme macht.", &[&previous])), back, &t));
        }
        let general = group(tr("Updates"), rows, super::updates::trouble_line(&updates), &t);
        let chat = group(
            tr("Chat"),
            vec![
                self.toggle("set-merged-chat", tr("Gemeinsamer Chat"), tr("Sammelt die Chatzeilen aller Instanzen auf der Seite Chat."), s.merged_chat, |st, on| st.merged_chat = on, &t, cx),
                self.toggle("set-broadcast", tr("Nachrichten senden"), tr("Erlaubt auf der Seite Chat, eine Zeile an eine Instanz oder einen ganzen Batch zu schicken."), s.broadcast, |st, on| st.broadcast = on, &t, cx),
            ],
            None,
            &t,
        );
        let notices = group(
            tr("Benachrichtigungen"),
            vec![
                self.toggle("set-notify-attention", tr("Bei Problemen"), tr("Eine Meldung pro Batch, wenn ein Account getrennt, gesperrt oder abgestürzt ist."), s.notifications.attention, |st, on| st.notifications.attention = on, &t, cx),
                self.toggle("set-notify-whispers", tr("Bei Flüstern"), tr("Eine Meldung, wenn dir jemand flüstert und Manycraft nicht im Vordergrund ist."), s.notifications.whispers, |st, on| st.notifications.whispers = on, &t, cx),
            ],
            Some(tr("Meldungen kommen gesammelt und nie mit Ton.")),
            &t,
        );
        let connection = group(
            tr("Verbindung"),
            vec![
                self.toggle("set-reconnect", tr("Automatisch neu verbinden"), tr("Geht nach einem Verbindungsabbruch wieder auf den Server."), s.reconnect.enabled, |st, on| st.reconnect.enabled = on, &t, cx),
                self.attempts("set-reconnect-attempts", tr("Versuche beim Verbinden"), s.reconnect.max_attempts, s.reconnect.enabled, |st, n| st.reconnect.max_attempts = n, &t, cx),
                self.toggle("set-restart", tr("Nach einem Absturz neu starten"), tr("Startet eine Instanz, deren Spiel unerwartet beendet wurde."), s.crash_restart.enabled, |st, on| st.crash_restart.enabled = on, &t, cx),
                self.attempts("set-restart-attempts", tr("Versuche beim Neustart"), s.crash_restart.max_attempts, s.crash_restart.enabled, |st, n| st.crash_restart.max_attempts = n, &t, cx),
            ],
            Some(tr("Nach den letzten Versuchen zeigt die Kachel, dass der Account dich braucht. Gesperrte Accounts verbinden sich nie von selbst neu.")),
            &t,
        );
        let hotkeys = group(
            tr("Tastenkürzel im Spiel"),
            HOTKEYS
                .iter()
                .map(|(label, chord)| row(tr(label), None, div().flex().items_center().gap(px(space::XS)).children(chord_keys(chord).into_iter().map(|k| keycap(k, &t))), &t))
                .collect(),
            Some(tr("Diese Kürzel gelten in jedem Spiel.")),
            &t,
        );
        let follows = div().px(px(space::XS)).text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Sprache und Erscheinungsbild folgen dem System."));
        let error = self.error.clone().map(|e| sheet::notice("circle-alert", t.danger, t.danger_text, e));
        let page = div()
            .id("settings-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .max_w(px(PAGE_MAX + space::XL * 2.))
                    .px(px(space::XL))
                    .py(px(space::XL))
                    .flex()
                    .flex_col()
                    .gap(px(space::XL))
                    .children(error)
                    .child(general)
                    .child(chat)
                    .child(notices)
                    .child(connection)
                    .child(hotkeys)
                    .child(follows),
            );
        let root = div()
            .id("settings")
            .key_context("Settings")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Close, window, _| window.remove_window()))
            .size_full()
            .bg(t.window)
            .text_color(t.primary)
            .font_family(Theme::font_family())
            .text_size(px(font::BODY))
            .line_height(relative(font::LINE))
            .relative()
            .child(page)
            .child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Scrolling));
        super::chrome::frame(title(), super::perf::frame_end(root), &t)
    }
}

#[cfg(test)]
mod tests {
    use super::{HOTKEYS, chord_keys};

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
}
