//! The settings window: the few options that rarely change, saved the moment they are touched.

use std::sync::{Arc, Mutex, PoisonError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::base::{Scrollbar, ScrollbarMode};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::actions::{CancelUpdate, OpenSettings, PreviousVersion, RestartToUpdate};
use super::drive::Mark;
use super::motion::Soft;
use super::model::{self, UpdateStatus};
use super::sheet::{self, Look};
use super::route::Section;
use super::theme::{self, Theme, control, font, iconsize, phone_qr, radius, space, tint};
use super::widgets::{KeyNav, icon, segment, segmented, tip};
use crate::core::i18n::{tr, trf};
use crate::core::phone::{self, Secret, Service, Slot};
use crate::core::settings::{self, HOTKEYS, PreviewRate, Settings};

const WIDTH: f32 = 560.;
const HEIGHT: f32 = 700.;
const TALL_HEIGHT: f32 = 880.;
const TALL_SCREEN: f32 = 1000.;
const MIN_WIDTH: f32 = 440.;
const MIN_HEIGHT: f32 = 360.;
const PAGE_MAX: f32 = 520.;
const ATTEMPTS: std::ops::RangeInclusive<u32> = 1..=100;
const DISCORD_EXAMPLE: &str = "https://discord.com/api/webhooks/…";
const SAVE_AFTER: Duration = Duration::from_millis(400);

actions!(settings_window, [Close]);

struct Open(AnyWindowHandle);

impl Global for Open {}

/// Registers the shortcut, the macOS menu bar and the action that opens the window.
pub fn init(cx: &mut App) {
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    cx.bind_keys([KeyBinding::new(Box::leak(format!("{m}-,").into_boxed_str()), OpenSettings, None), KeyBinding::new("escape", Close, Some("Settings"))]);
    cx.on_action(|_: &OpenSettings, cx| cx.defer(open));
    #[cfg(target_os = "macos")]
    cx.set_menus([Menu::new("Manycraft").items([MenuItem::action(tr("Einstellungen …"), OpenSettings), MenuItem::action(tr("Papierkorb öffnen"), super::actions::OpenTrash), MenuItem::separator(), MenuItem::action(tr("Manycraft beenden"), super::actions::Quit)]), Menu::new(tr("Darstellung")).items([MenuItem::action(tr("Vollbild"), super::actions::ToggleFullscreen)])]);
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
    let display = super::main_window::display(cx);
    let roomy = display.and_then(|id| cx.find_display(id)).or_else(|| cx.primary_display()).is_some_and(|d| f32::from(d.bounds().size.height) >= TALL_SCREEN);
    let bounds = Bounds::centered(display, size(px(WIDTH), px(if roomy { TALL_HEIGHT } else { HEIGHT })), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display,
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
        app_id: Some(crate::core::APP_ID.into()),
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

/// Opens the settings window, or brings it to the front, scrolled to `section` and outlining it for a moment.
pub fn open_at(section: Section, cx: &mut App) {
    open(cx);
    let Some(handle) = existing(cx) else { return };
    let _ = handle.update(cx, |view, window, cx| {
        let Ok(root) = view.downcast::<gpui_kit::base::Root>() else { return };
        let content = root.read(cx).view().clone();
        if let Ok(view) = content.downcast::<SettingsView>() {
            view.update(cx, |s, cx| {
                let generation = s.pointed.map_or(0, |(_, g)| g + 1);
                s.jump = Some(section);
                s.pointed = Some((section, generation));
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(super::motion::POINT_OUT).await;
                    let _ = this.update(cx, |s, cx| {
                        if s.pointed.is_some_and(|(_, g)| g == generation) {
                            s.pointed = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
                cx.notify();
            });
        }
    });
}

fn title() -> String {
    format!("{} – Manycraft", tr("Einstellungen"))
}

struct SettingsView {
    focus: FocusHandle,
    scroll: ScrollHandle,
    error: Option<String>,
    phone: PhoneForm,
    jump: Option<Section>,
    pointed: Option<(Section, u64)>,
    _subs: Vec<Subscription>,
}

/// A typed field of the phone settings: a keychain secret or a plain setting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Discord,
    Token,
    Chat,
    Server,
    Topic,
}

impl Field {
    const ALL: [Field; 5] = [Field::Discord, Field::Token, Field::Chat, Field::Server, Field::Topic];

    fn slot(self) -> Option<Slot> {
        match self {
            Field::Discord => Some(Slot::DiscordUrl),
            Field::Token => Some(Slot::TelegramToken),
            Field::Topic => Some(Slot::NtfyTopic),
            Field::Chat | Field::Server => None,
        }
    }

    fn of(service: Service) -> Self {
        match service {
            Service::Discord => Field::Discord,
            Service::Telegram => Field::Token,
            Service::Ntfy => Field::Topic,
        }
    }

    fn set(self, value: String) {
        settings::update(|next| {
            if self == Field::Chat {
                next.phone.telegram_chat = value;
            } else {
                next.phone.ntfy_server = value;
            }
        });
    }
}

/// Orders the saves of one field: a slow keychain never lets an older value land after a newer one.
#[derive(Default)]
struct Saver {
    generation: AtomicU64,
    lock: Mutex<()>,
}

impl Saver {
    fn save(&self, field: Field, generation: u64, value: String) -> Option<Result<(), String>> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if self.generation.load(Ordering::SeqCst) != generation {
            return None;
        }
        Some(match field.slot() {
            Some(slot) => phone::store_secret(slot, &Secret::new(value)),
            None => settings::persist().map_err(|e| trf("Die Einstellungen wurden nicht gespeichert: %@", &[&format!("{e:#}")])),
        })
    }
}

struct PhoneForm {
    inputs: [Entity<InputState>; 5],
    saved: [String; 5],
    savers: [Arc<Saver>; 5],
    shown: [bool; 5],
    dirty: [bool; 5],
    errors: [Option<String>; 5],
    testing: bool,
    tested: Option<Result<String, String>>,
    finding: bool,
    found: Option<Result<String, String>>,
    loaded: bool,
    offered: bool,
    generated: bool,
}

impl PhoneForm {
    fn input(&self, field: Field) -> &Entity<InputState> {
        &self.inputs[field as usize]
    }

    fn value(&self, field: Field, cx: &App) -> String {
        self.input(field).read(cx).value().trim().to_owned()
    }
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
        let current = settings::active().phone.clone();
        let inputs = Field::ALL.map(|field| {
            cx.new(|cx| {
                let input = InputState::new(window, cx);
                match field {
                    Field::Discord => input.masked(true).placeholder(DISCORD_EXAMPLE),
                    Field::Token => input.masked(true).placeholder(tr("z. B. 123456789:AAF…")),
                    Field::Chat => input.placeholder(tr("z. B. 123456789")).default_value(current.telegram_chat.clone()),
                    Field::Server => input.placeholder(phone::NTFY_DEFAULT).default_value(current.ntfy_server.clone()),
                    Field::Topic => input.masked(true).placeholder(tr("z. B. farm-alarm-7f3k9q")),
                }
            })
        });
        let mut subs = subs;
        for field in Field::ALL {
            subs.push(cx.subscribe(&inputs[field as usize], move |s: &mut Self, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    s.typed(field, cx);
                }
            }));
        }
        let phone = PhoneForm {
            inputs,
            saved: [String::new(), String::new(), current.telegram_chat.clone(), current.ntfy_server.clone(), String::new()],
            savers: Field::ALL.map(|_| Arc::new(Saver::default())),
            shown: [false; 5],
            dirty: [false; 5],
            errors: Default::default(),
            testing: false,
            tested: None,
            finding: false,
            found: None,
            loaded: false,
            offered: false,
            generated: false,
        };
        cx.spawn_in(window, async move |this, cx| {
            let secrets = cx.background_spawn(async { [Field::Discord, Field::Token, Field::Topic].map(|f| (f, f.slot().map(phone::read_secret))) }).await;
            let _ = this.update_in(cx, |s, window, cx| {
                for (field, read) in secrets {
                    match read {
                        Some(Ok(secret)) => {
                            if !s.phone.value(field, cx).is_empty() {
                                continue;
                            }
                            s.phone.saved[field as usize] = secret.expose().to_owned();
                            s.phone.input(field).update(cx, |i, cx| i.set_value(secret.expose().to_owned(), window, cx));
                        }
                        Some(Err(e)) => s.phone.errors[field as usize] = Some(e),
                        None => {}
                    }
                }
                s.phone.loaded = true;
                cx.notify();
            });
        })
        .detach();
        Self { focus, scroll: ScrollHandle::new(), error: None, phone, jump: None, pointed: None, _subs: subs }
    }

    fn fill_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = &settings::active().phone;
        let topic = Field::Topic as usize;
        if !current.enabled || current.service != Service::Ntfy || !self.phone.loaded || self.phone.offered || self.phone.errors[topic].is_some() {
            return;
        }
        self.phone.offered = true;
        if !self.phone.value(Field::Topic, cx).is_empty() {
            return;
        }
        self.phone.generated = true;
        let fresh = phone::new_topic(uuid::Uuid::new_v4().as_u128());
        self.phone.input(Field::Topic).update(cx, |i, cx| i.set_value(fresh.expose().to_owned(), window, cx));
        self.typed(Field::Topic, cx);
    }

    fn typed(&mut self, field: Field, cx: &mut Context<Self>) {
        let ix = field as usize;
        let value = self.phone.value(field, cx);
        if field == Field::Token {
            self.phone.found = None;
        }
        self.phone.tested = None;
        cx.notify();
        let saver = Arc::clone(&self.phone.savers[ix]);
        let n = saver.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if value == self.phone.saved[ix] && !self.phone.dirty[ix] {
            return;
        }
        self.phone.dirty[ix] = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            if saver.generation.load(Ordering::SeqCst) != n {
                return;
            }
            if field.slot().is_none() {
                let kept = value.clone();
                let _ = cx.update(|_| field.set(kept));
            }
            let kept = value.clone();
            let latest = Arc::clone(&saver);
            let Some(result) = cx.background_spawn(async move { saver.save(field, n, kept) }).await else { return };
            let _ = this.update(cx, |s, cx| {
                if latest.generation.load(Ordering::SeqCst) != n {
                    return;
                }
                s.phone.dirty[ix] = result.is_err();
                if result.is_ok() {
                    s.phone.saved[ix] = value;
                    super::state::set_phone_trouble(None, cx);
                }
                s.phone.errors[ix] = result.err();
                cx.notify();
            });
        })
        .detach();
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        if self.phone.testing {
            return;
        }
        let mut current = settings::active().phone.clone();
        current.telegram_chat = self.phone.value(Field::Chat, cx);
        current.ntfy_server = self.phone.value(Field::Server, cx);
        let secret = Secret::new(self.phone.value(Field::of(current.service), cx));
        self.phone.testing = true;
        self.phone.tested = None;
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { phone::Target::new(&current, secret).and_then(|t| phone::send_now(&t, &phone::test_text())) }).await;
            let _ = this.update(cx, |s, cx| {
                s.phone.testing = false;
                if result.is_ok() {
                    super::state::set_phone_trouble(None, cx);
                }
                s.phone.tested = Some(result.map(|()| tr("Gesendet. Schau aufs Handy.")));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn find_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.phone.finding {
            return;
        }
        let token = Secret::new(self.phone.value(Field::Token, cx));
        self.phone.finding = true;
        self.phone.found = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { phone::find_chat(&token) }).await;
            let _ = this.update_in(cx, |s, window, cx| {
                s.phone.finding = false;
                let found = result.map(|chat| {
                    s.phone.input(Field::Chat).update(cx, |i, cx| i.set_value(chat.id.clone(), window, cx));
                    s.typed(Field::Chat, cx);
                    if chat.name.is_empty() { tr("Chat gefunden.") } else { trf("Gefunden: %@", &[&chat.name]) }
                });
                s.phone.found = Some(found);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reveal(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let ix = field as usize;
        self.phone.shown[ix] = !self.phone.shown[ix];
        let masked = !self.phone.shown[ix];
        self.phone.input(field).update(cx, |i, cx| i.set_masked(masked, window, cx));
        cx.notify();
    }

    fn field_row(&self, field: Field, label: String, trailing: Option<Stateful<Div>>, note: Div, t: &Theme, cx: &mut Context<Self>) -> Row {
        let ix = field as usize;
        let reveal = field.slot().map(|_| {
            let shown = self.phone.shown[ix];
            sheet::button(SharedString::from(format!("set-phone-reveal-{ix}")), if shown { tr("Verbergen") } else { tr("Anzeigen") }, None, Look::Plain, false, false, t)
                .on_click(cx.listener(move |s, _, window, cx| s.reveal(field, window, cx)))
        });
        let note = match &self.phone.errors[ix] {
            Some(e) => sheet::note(None, Some(e.clone()), t),
            None => note,
        };
        let line = div().flex().items_center().gap(px(space::S)).child(div().flex_1().min_w_0().child(Input::new(self.phone.input(field)))).children(reveal).children(trailing);
        let below = div().flex().flex_col().gap(px(space::XS)).child(line).child(note);
        Row { below: Some(below.into_any_element()), ..row(label, None, div(), t) }
    }

    fn phone_group(&self, outlined: bool, t: &Theme, cx: &mut Context<Self>) -> Div {
        let current = settings::active().phone.clone();
        let mut rows = vec![self.toggle("set-phone", tr("Aufs Handy schicken"), tr("Schickt dieselben Meldungen über Discord, Telegram oder ntfy, gesammelt pro Batch."), current.enabled, |st, on| st.phone.enabled = on, t, cx)];
        if !current.enabled {
            return outlined_group(tr("Aufs Handy"), rows, None, outlined, t);
        }
        let service = current.service;
        let segments = Service::ALL.map(|sv| segment(SharedString::from(format!("set-phone-{}", sv.name().to_lowercase())), sv.name().to_owned(), sv == service, true, t));
        let pick = cx.listener(|s, n: &usize, _, cx| {
            let next = Service::ALL.get(*n).copied().unwrap_or_default();
            s.phone.tested = None;
            s.change(|st| st.phone.service = next, cx);
        });
        rows.push(row(tr("Dienst"), None, segmented("set-phone-service", segments, move |n, window, cx| pick(&n, window, cx), t), t));
        match service {
            Service::Discord => {
                let url = &self.phone.saved[Field::Discord as usize];
                let hint = tr("In Discord: Servereinstellungen › Integrationen › Webhooks › Neuer Webhook › Webhook-URL kopieren.");
                let note = if !url.is_empty() && !url.contains("/api/webhooks/") {
                    sheet::status("triangle-alert", t.attention_text, tr("Das sieht nicht wie eine Discord-Webhook-URL aus. Die enthält /api/webhooks/.")).text_size(px(font::CAPTION))
                } else {
                    sheet::note(Some(hint), None, t)
                };
                rows.push(self.field_row(Field::Discord, tr("Webhook-URL"), None, note, t, cx));
            }
            Service::Telegram => {
                rows.push(self.field_row(Field::Token, tr("Bot-Token"), None, sheet::note(Some(tr("Schreib @BotFather in Telegram /newbot und kopiere den Token hierher.")), None, t), t, cx));
                let finding = self.phone.finding;
                let no_token = self.phone.value(Field::Token, cx).is_empty();
                let find = sheet::button("set-phone-find", tr("Chat-ID finden"), None, Look::Secondary, no_token, finding, t).when(!finding && !no_token, |b| b.on_click(cx.listener(|s, _, window, cx| s.find_chat(window, cx))));
                let note = match &self.phone.found {
                    Some(Ok(found)) => sheet::status("circle-check", t.success_text, found.clone()).text_size(px(font::CAPTION)),
                    Some(Err(e)) => sheet::note(None, Some(e.clone()), t),
                    None => sheet::note(Some(tr("Schreib deinem Bot zuerst eine Nachricht, dann „Chat-ID finden“.")), None, t),
                };
                rows.push(self.field_row(Field::Chat, tr("Chat-ID"), Some(find), note, t, cx));
            }
            Service::Ntfy => {
                rows.push(self.field_row(Field::Server, tr("Server"), None, sheet::note(Some(tr("Nur ändern, wenn du einen eigenen ntfy-Server hast.")), None, t), t, cx));
                rows.push(self.field_row(Field::Topic, tr("Thema"), None, sheet::note(Some(if self.phone.generated { tr("Manycraft hat ein langes, zufälliges Thema gewählt. Wer es kennt, liest mit.") } else { tr("Abonniere dieses Thema in der ntfy-App. Wer es kennt, liest mit, also wähle etwas Langes.") }), None, t), t, cx));
                let topic = self.phone.value(Field::Topic, cx);
                if let Some(code) = (!topic.is_empty()).then(|| qr(phone::ntfy_link(&self.phone.value(Field::Server, cx), &topic).expose(), t)).flatten() {
                    rows.push(row(tr("Handy verbinden"), Some(tr("Mit der Handy-Kamera scannen und das Thema in ntfy abonnieren.")), code, t));
                }
            }
        }
        let testing = self.phone.testing;
        let send = sheet::button("set-phone-test", tr("Testnachricht senden"), None, Look::Secondary, false, testing, t).when(!testing, |b| b.on_click(cx.listener(|s, _, _, cx| s.test(cx))));
        let trouble = model::get(cx).read(cx).phone_trouble.clone();
        let outcome = match (&self.phone.tested, trouble) {
            (Some(Ok(done)), _) => Some(sheet::status("circle-check", t.success_text, done.clone())),
            (Some(Err(e)), _) => Some(sheet::status("circle-alert", t.danger_text, e.clone())),
            (None, Some(trouble)) => Some(sheet::status("circle-alert", t.danger_text, trf("Zuletzt nicht angekommen: %@", &[&trouble]))),
            (None, None) => None,
        };
        rows.push(Row { below: outcome.map(IntoElement::into_any_element), ..row(tr("Testnachricht"), Some(tr("Prüft, ob Meldungen auf dem Handy ankommen.")), send, t) });
        rows.push(self.toggle("set-phone-back", tr("Auch wenn sie zurück sind"), tr("Meldet auch, wenn getrennte Accounts wieder auf dem Server sind."), current.reconnected, |st, on| st.phone.reconnected = on, t, cx));
        rows.push(self.toggle("set-phone-whispers", tr("Auch Flüsternachrichten"), tr("Schickt Flüsternachrichten aufs Handy weiter."), current.whispers, |st, on| st.phone.whispers = on, t, cx));
        outlined_group(tr("Aufs Handy"), rows, Some(tr("Webhook-URL, Token und Thema liegen nur im Passwortspeicher des Systems.")), outlined, t)
    }

    fn change(&mut self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let (before, next) = settings::update(edit);
        let badge_changed = next.game_badge != before.game_badge;
        let sleep_changed = next.prevent_sleep != before.prevent_sleep;
        let rate_changed = next.preview_rate != before.preview_rate;
        let recheck = (next.auto_update && !before.auto_update) || next.prereleases != before.prereleases;
        if badge_changed {
            super::state::tell_games(cx);
        }
        if sleep_changed {
            super::state::keep_awake(cx);
        }
        if rate_changed {
            super::state::replan(cx);
        }
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

    fn toggle(&self, id: &'static str, label: String, hint: String, on: bool, apply: impl Fn(&mut Settings, bool) + Clone + 'static, t: &Theme, cx: &mut Context<Self>) -> Row {
        let flip = apply.clone();
        let control = Switch::new(id).checked(on).accessibility_label(label.clone()).on_click(cx.listener(move |s, on: &bool, _, cx| {
            let on = *on;
            s.change(|st| apply(st, on), cx);
        }));
        let press = cx.listener(move |s, _: &ClickEvent, _, cx| s.change(|st| flip(st, !on), cx));
        Row { press: Some((SharedString::from(format!("{id}-row")), Box::new(press))), ..row(label, Some(hint), control, t) }
    }

    fn attempts(&self, id: &'static str, label: String, value: u32, enabled: bool, apply: impl Fn(&mut Settings, u32) + Clone + 'static, t: &Theme, cx: &mut Context<Self>) -> Row {
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

struct Row {
    text: Div,
    control: AnyElement,
    press: Option<(SharedString, Press)>,
    below: Option<AnyElement>,
}

fn row(label: String, hint: Option<String>, control: impl IntoElement, t: &Theme) -> Row {
    let text = div().flex_1().min_w_0().flex().flex_col().child(div().text_size(px(font::BODY)).text_color(t.primary).child(label)).children(hint.map(|h| div().text_size(px(font::CALLOUT)).text_color(t.secondary).child(h)));
    Row { text, control: control.into_any_element(), press: None, below: None }
}

fn group(title: String, rows: Vec<Row>, footer: Option<String>, t: &Theme) -> Div {
    outlined_group(title, rows, footer, false, t)
}

fn outlined_group(title: String, rows: Vec<Row>, footer: Option<String>, outlined: bool, t: &Theme) -> Div {
    let last = rows.len().saturating_sub(1);
    let inner = radius::CARD - 1.;
    let rows = rows.into_iter().enumerate().map(|(i, r)| {
        let (top, bottom) = (if i == 0 { inner } else { 0. }, if i == last { inner } else { 0. });
        let control = match &r.press {
            Some((id, _)) => div().id(SharedString::from(format!("{id}-control"))).flex_none().on_click(|_, _, cx| cx.stop_propagation()).child(r.control).into_any_element(),
            None => div().flex_none().child(r.control).into_any_element(),
        };
        let press = r.press.map(|(id, press)| {
            div().id(id).absolute().inset_0().cursor_pointer().soft_corners(t.ink.opacity(tint::HOVER), Corners { top_left: top, top_right: top, bottom_right: bottom, bottom_left: bottom }).on_click(press)
        });
        div()
            .relative()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(space::S))
            .px(px(space::L))
            .py(px(space::M))
            .min_h(px(control::L + space::M))
            .when(i < last, |d| d.border_b_1().border_color(t.separator))
            .children(press)
            .child(div().flex().items_center().gap(px(space::L)).child(r.text).child(control))
            .children(r.below)
    });
    div()
        .flex()
        .flex_col()
        .gap(px(space::S))
        .child(div().px(px(space::XS)).text_size(px(font::CALLOUT)).font_weight(FontWeight::SEMIBOLD).text_color(t.secondary).child(title))
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(radius::CARD))
                .bg(t.content)
                .border_1()
                .border_color(t.separator)
                .relative()
                .overflow_hidden()
                .children(rows)
                .when(outlined, |d| d.child(div().absolute().inset_0().rounded(px(radius::CARD - 1.)).border_2().border_color(theme::ring::color()))),
        )
        .children(footer.map(|f| div().px(px(space::XS)).text_size(px(font::CAPTION)).text_color(t.tertiary).child(f)))
}

fn qr(text: &str, t: &Theme) -> Option<Div> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let n = code.width();
    let dark: Vec<bool> = code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
    let paint = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let scale = window.scale_factor();
            let cells = (n + 2 * phone_qr::QUIET) as f32;
            let cell = (f32::from(bounds.size.width) * scale / cells).floor().max(1.) / scale;
            let inset = (f32::from(bounds.size.width) - cell * n as f32) / 2.;
            for (i, on) in dark.iter().enumerate() {
                if *on {
                    let at = point(bounds.origin.x + px(inset + (i % n) as f32 * cell), bounds.origin.y + px(inset + (i / n) as f32 * cell));
                    window.paint_quad(fill(Bounds::new(at, size(px(cell), px(cell))), phone_qr::DARK));
                }
            }
        },
    )
    .size_full();
    Some(div().size(px(phone_qr::SIZE)).flex_none().rounded(px(radius::SMALL)).border_1().border_color(t.separator).bg(phone_qr::LIGHT).child(paint))
}

/// One key as a cap, as the keyboard prints it.
pub(super) fn keycap(text: String, t: &Theme) -> Div {
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

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.fill_topic(window, cx);
        super::perf::frame_start();
        super::theme::follow_focus(window, cx);
        let t = Theme::get(cx);
        let s = settings::active().clone();
        let updates = model::get(cx).read(cx).updates.clone();
        let searching = matches!(updates.status, UpdateStatus::Checking);
        let launching = super::updates::counts(cx).0 > 0;
        let waits = super::updates::launching_line();
        let search = if matches!(updates.status, UpdateStatus::Ready { .. }) {
            let restart = sheet::button("set-update-restart", tr("Neu starten"), None, Look::Primary, launching, false, &t);
            if launching { restart } else { restart.on_click(|_, window, cx| window.dispatch_action(Box::new(RestartToUpdate), cx)) }
        } else if matches!(updates.status, UpdateStatus::Downloading { .. }) {
            let stopping = updates.cancelling;
            let cancel = sheet::button("set-update-cancel", if stopping { tr("Bricht ab …") } else { tr("Abbrechen") }, None, Look::Secondary, false, stopping, &t);
            if stopping { cancel } else { cancel.on_click(|_, window, cx| window.dispatch_action(Box::new(CancelUpdate), cx)) }
        } else {
            sheet::button("set-update-now", tr("Nach Updates suchen"), None, Look::Secondary, false, searching, &t).when(!searching, |b| b.on_click(|_, _, cx| super::updates::check(true, cx)))
        };
        let status = super::updates::status_line(&updates);
        let status = if launching && matches!(updates.status, UpdateStatus::Ready { .. }) { format!("{status} {waits}") } else { status };
        let mut rows = vec![
            row(trf("Manycraft %@", &[&env!("CARGO_PKG_VERSION")]), Some(status), search, &t),
            self.toggle("set-auto-update", tr("Updates automatisch laden"), tr("Lädt neue Versionen im Hintergrund und installiert sie beim Beenden."), s.auto_update, |st, on| st.auto_update = on, &t, cx),
            self.toggle("set-prereleases", tr("Vorabversionen"), tr("Bietet auch Testversionen an, bevor sie veröffentlicht sind."), s.prereleases, |st, on| st.prereleases = on, &t, cx),
        ];
        if let Some(previous) = updates.previous.clone() {
            let back = sheet::button("set-previous", tr("Zurückkehren"), Some("rotate-ccw-clock"), Look::Secondary, launching, false, &t);
            let back = if launching { back } else { back.on_click(|_, window, cx| window.dispatch_action(Box::new(PreviousVersion), cx)) };
            let note = trf("Version %@ ist noch da, falls die neue Probleme macht.", &[&previous]);
            rows.push(row(tr("Vorherige Version"), Some(if launching { format!("{note} {waits}") } else { note }), back, &t));
        }
        let updates_group = group(tr("Updates"), rows, super::updates::trouble_line(&updates), &t);
        let general = group(
            tr("Im Spiel"),
            vec![
                self.toggle("set-prevent-sleep", tr("Computer wach halten, solange Spiele laufen"), tr("Der Bildschirm darf trotzdem ausgehen."), s.prevent_sleep, |st, on| st.prevent_sleep = on, &t, cx),
                self.toggle("set-game-badge", tr("Namensschild im Spiel"), tr("Zeigt oben links im Spiel, welcher Account es ist. F1 blendet es mit aus."), s.game_badge, |st, on| st.game_badge = on, &t, cx),
            ],
            None,
            &t,
        );
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
        let pointed = self.pointed.map(|(section, _)| section);
        let phone = self.phone_group(pointed == Some(Section::Phone), &t, cx);
        let connection = outlined_group(
            tr("Verbindung"),
            vec![
                self.toggle("set-reconnect", tr("Automatisch neu verbinden"), tr("Geht nach einem Verbindungsabbruch wieder auf den Server."), s.reconnect.enabled, |st, on| st.reconnect.enabled = on, &t, cx),
                self.attempts("set-reconnect-attempts", tr("Versuche beim Verbinden"), s.reconnect.max_attempts, s.reconnect.enabled, |st, n| st.reconnect.max_attempts = n, &t, cx),
                self.toggle("set-restart", tr("Nach einem Absturz neu starten"), tr("Startet eine Instanz, deren Spiel unerwartet beendet wurde."), s.crash_restart.enabled, |st, on| st.crash_restart.enabled = on, &t, cx),
                self.attempts("set-restart-attempts", tr("Versuche beim Neustart"), s.crash_restart.max_attempts, s.crash_restart.enabled, |st, n| st.crash_restart.max_attempts = n, &t, cx),
            ],
            Some(tr("Nach den letzten Versuchen zeigt die Kachel, dass der Account dich braucht. Gesperrte Accounts verbinden sich nie von selbst neu.")),
            pointed == Some(Section::Reconnect),
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
        let rates = [(PreviewRate::Saving, tr("Sparsam")), (PreviewRate::Normal, tr("Normal")), (PreviewRate::Smooth, tr("Flüssig"))];
        let picked = s.preview_rate;
        let segments = rates.iter().enumerate().map(|(n, (rate, label))| segment(SharedString::from(format!("set-preview-rate-{n}")), label.clone(), *rate == picked, true, &t)).collect::<Vec<_>>();
        let pick = cx.listener(move |s, n: &usize, _, cx| {
            let next = rates.get(*n).map_or(PreviewRate::Normal, |(rate, _)| *rate);
            s.change(|st| st.preview_rate = next, cx);
        });
        let advanced = group(
            tr("Erweitert"),
            vec![row(tr("Vorschau-Rate"), Some(tr("Wie oft sich die Kacheln erneuern. Flüssig kostet die Spiele mehr Rechenzeit, Sparsam weniger.")), segmented("set-preview-rate", segments, move |n, window, cx| pick(&n, window, cx), &t), &t)],
            None,
            &t,
        );
        let follows = div().px(px(space::XS)).text_size(px(font::CAPTION)).text_color(t.tertiary).child(tr("Sprache und Erscheinungsbild folgen dem System."));
        let error = self.error.clone().map(|e| sheet::notice("circle-alert", t.danger, t.danger_text, e));
        let parts: Vec<(Option<Section>, AnyElement)> = error
            .map(|e| (None, e.into_any_element()))
            .into_iter()
            .chain([
                (None, updates_group.into_any_element()),
                (None, general.into_any_element()),
                (None, chat.into_any_element()),
                (None, notices.into_any_element()),
                (Some(Section::Phone), phone.into_any_element()),
                (Some(Section::Reconnect), connection.into_any_element()),
                (None, hotkeys.into_any_element()),
                (None, advanced.into_any_element()),
                (None, follows.into_any_element()),
            ])
            .collect();
        if let Some(ix) = self.jump.take().and_then(|jump| parts.iter().position(|(section, _)| *section == Some(jump))) {
            self.scroll.scroll_to_top_of_item(ix);
        }
        let page = div()
            .id("settings-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .items_center()
            .px(px(space::XL))
            .py(px(space::XL))
            .gap(px(space::XL))
            .children(parts.into_iter().map(|(_, part)| div().w_full().max_w(px(PAGE_MAX)).flex_none().child(part)));
        let root = div()
            .id("settings")
            .key_context("Settings")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Close, window, _| window.remove_window()))
            .on_action(|_: &super::actions::ToggleFullscreen, window, _| window.toggle_fullscreen())
            .size_full()
            .bg(t.window)
            .text_color(t.primary)
            .font_family(Theme::font_family())
            .text_size(px(font::BODY))
            .line_height(relative(font::LINE))
            .relative()
            .child(page)
            .child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Scrolling));
        super::chrome::frame(title(), super::perf::frame_end(root), window.is_fullscreen(), &t)
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
