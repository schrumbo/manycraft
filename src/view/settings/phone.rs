//! Aufs Handy: the same notices on the phone through Discord, Telegram or ntfy. Secrets live only in the system keychain.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use freya::prelude::*;

use super::{toggle, use_prefs};
use crate::app::event::Event;
use crate::core::i18n::{tr, trf};
use crate::core::phone::{self, Secret, Service, Slot};
use crate::core::settings;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::group::{Line, group, note, status};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::{after, off_thread, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, space};

const SAVE_AFTER: Duration = Duration::from_millis(400);
const DISCORD_EXAMPLE: &str = "https://discord.com/api/webhooks/…";

/// A field of the phone settings: a keychain secret or a plain setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Discord,
    Token,
    Chat,
    Server,
    Topic,
}

impl Kind {
    const ALL: [Kind; 5] = [Kind::Discord, Kind::Token, Kind::Chat, Kind::Server, Kind::Topic];

    fn slot(self) -> Option<Slot> {
        match self {
            Kind::Discord => Some(Slot::DiscordUrl),
            Kind::Token => Some(Slot::TelegramToken),
            Kind::Topic => Some(Slot::NtfyTopic),
            Kind::Chat | Kind::Server => None,
        }
    }

    fn of(service: Service) -> Self {
        match service {
            Service::Discord => Kind::Discord,
            Service::Telegram => Kind::Token,
            Service::Ntfy => Kind::Topic,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Kind::Discord => "discord",
            Kind::Token => "token",
            Kind::Chat => "chat",
            Kind::Server => "server",
            Kind::Topic => "topic",
        }
    }
}

/// Orders the saves of one field: a slow keychain never lets an older value land after a newer one.
#[derive(Default)]
struct Saver {
    generation: AtomicU64,
    lock: Mutex<()>,
}

impl Saver {
    fn save(&self, kind: Kind, generation: u64, value: String) -> Option<Result<(), String>> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if self.generation.load(Ordering::SeqCst) != generation {
            return None;
        }
        Some(match kind.slot() {
            Some(slot) => phone::store_secret(slot, &Secret::new(value)),
            None => settings::persist().map_err(|e| trf("Die Einstellungen wurden nicht gespeichert: %@", &[&format!("{e:#}")])),
        })
    }
}

#[derive(Clone)]
struct Form {
    values: [State<String>; 5],
    saved: State<[String; 5]>,
    shown: State<[bool; 5]>,
    errors: State<[Option<String>; 5]>,
    savers: Rc<[Arc<Saver>; 5]>,
    testing: State<bool>,
    tested: State<Option<Result<String, String>>>,
    finding: State<bool>,
    found: State<Option<Result<String, String>>>,
}

impl Form {
    fn value(&self, kind: Kind) -> String {
        self.values[kind as usize].peek().trim().to_owned()
    }
}

fn typed(form: &Form, store: Store, kind: Kind) {
    let ix = kind as usize;
    let value = form.value(kind);
    let (mut tested, mut found) = (form.tested, form.found);
    tested.set_if_modified(None);
    if kind == Kind::Token {
        found.set_if_modified(None);
    }
    let saver = Arc::clone(&form.savers[ix]);
    let n = saver.generation.fetch_add(1, Ordering::SeqCst) + 1;
    if value == form.saved.peek()[ix] && form.errors.peek()[ix].is_none() {
        return;
    }
    let (mut saved, mut errors) = (form.saved, form.errors);
    spawn_in_window(async move {
        after(SAVE_AFTER).await;
        if saver.generation.load(Ordering::SeqCst) != n {
            return;
        }
        if kind.slot().is_none() {
            let mut next = settings::active().clone();
            if kind == Kind::Chat {
                next.phone.telegram_chat = value.clone();
            } else {
                next.phone.ntfy_server = value.clone();
            }
            settings::set(next);
        }
        let kept = value.clone();
        let latest = Arc::clone(&saver);
        let Some(rx) = off_thread("phone-save", move || saver.save(kind, n, kept)) else { return };
        let Ok(Some(result)) = rx.recv().await else { return };
        if latest.generation.load(Ordering::SeqCst) != n {
            return;
        }
        if result.is_ok() {
            saved.write()[ix] = value;
            store.dispatch(Event::PhoneOutcome(None));
        }
        errors.write()[ix] = result.err();
    });
}

fn test(form: &Form, store: Store) {
    if *form.testing.peek() {
        return;
    }
    let mut current = settings::active().phone.clone();
    current.telegram_chat = form.value(Kind::Chat);
    current.ntfy_server = form.value(Kind::Server);
    let secret = Secret::new(form.value(Kind::of(current.service)));
    let (mut testing, mut tested) = (form.testing, form.tested);
    testing.set(true);
    tested.set(None);
    let Some(rx) = off_thread("phone-test", move || phone::Target::new(&current, secret).and_then(|t| phone::send_now(&t, &phone::test_text()))) else {
        testing.set(false);
        return;
    };
    spawn_in_window(async move {
        let result = rx.recv().await.unwrap_or_else(|_| Err(tr("Die Testnachricht ließ sich nicht senden.")));
        testing.set(false);
        if result.is_ok() {
            store.dispatch(Event::PhoneOutcome(None));
        }
        tested.set(Some(result.map(|()| tr("Gesendet. Schau aufs Handy."))));
    });
}

fn find_chat(form: &Form, store: Store) {
    if *form.finding.peek() {
        return;
    }
    let token = Secret::new(form.value(Kind::Token));
    let (mut finding, mut found) = (form.finding, form.found);
    let mut chat = form.values[Kind::Chat as usize];
    finding.set(true);
    found.set(None);
    let Some(rx) = off_thread("phone-find", move || phone::find_chat(&token)) else {
        finding.set(false);
        return;
    };
    let form = form.clone();
    spawn_in_window(async move {
        let result = rx.recv().await.unwrap_or_else(|_| Err(tr("Der Chat ließ sich nicht finden.")));
        finding.set(false);
        let text = result.map(|c| {
            chat.set(c.id.clone());
            typed(&form, store, Kind::Chat);
            if c.name.is_empty() { tr("Chat gefunden.") } else { trf("Gefunden: %@", &[&c.name]) }
        });
        found.set(Some(text));
    });
}

fn random_topic() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("manycraft-{}", id.get(..16).unwrap_or(&id))
}

/// The phone group: on or off, the service, its fields, a test message and what to report.
#[derive(PartialEq)]
pub struct PhoneGroup;

impl Component for PhoneGroup {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let p = use_prefs();
        let s = p.read();
        let current = settings::active().phone.clone();
        let form = use_hook(|| Form {
            values: [State::create(String::new()), State::create(String::new()), State::create(current.telegram_chat.clone()), State::create(current.ntfy_server.clone()), State::create(String::new())],
            saved: State::create([String::new(), String::new(), current.telegram_chat.clone(), current.ntfy_server.clone(), String::new()]),
            shown: State::create([false; 5]),
            errors: State::create(Default::default()),
            savers: Rc::new(Kind::ALL.map(|_| Arc::new(Saver::default()))),
            testing: State::create(false),
            tested: State::create(None),
            finding: State::create(false),
            found: State::create(None),
        });
        let loader = form.clone();
        use_hook(move || {
            let Some(rx) = off_thread("phone-read", || [Kind::Discord, Kind::Token, Kind::Topic].map(|k| (k, k.slot().map(phone::read_secret)))) else { return };
            let form = loader;
            spawn_in_window(async move {
                let Ok(secrets) = rx.recv().await else { return };
                let (mut saved, mut errors) = (form.saved, form.errors);
                for (kind, read) in secrets {
                    let ix = kind as usize;
                    match read {
                        Some(Ok(secret)) if form.values[ix].peek().is_empty() => {
                            saved.write()[ix] = secret.expose().to_owned();
                            let mut value = form.values[ix];
                            value.set(secret.expose().to_owned());
                        }
                        Some(Err(e)) => errors.write()[ix] = Some(e),
                        _ => {}
                    }
                }
            });
        });
        for kind in Kind::ALL {
            let watch = form.clone();
            use_side_effect(move || {
                let _ = watch.values[kind as usize].read();
                typed(&watch, store, kind);
            });
        }
        let mut rows = vec![toggle(p, "settings.phone", tr("Aufs Handy schicken"), tr("Schickt dieselben Meldungen über Discord, Telegram oder ntfy, gesammelt pro Batch."), current.enabled, |s, on| s.phone.enabled = on)];
        if !s.phone.enabled {
            return group(&t, Some(tr("Aufs Handy")), rows, None);
        }
        let service = current.service;
        let segments = Service::ALL.iter().map(|sv| Segment { id: format!("settings.phone.{}", sv.name().to_lowercase()), label: sv.name().to_owned(), tip: None }).collect();
        let at = Service::ALL.iter().position(|sv| *sv == service).unwrap_or(0);
        let mut tested = form.tested;
        rows.push(
            Line::new("settings.phone.service", tr("Dienst"))
                .control(Segmented::new(tr("Dienst"), segments, at).on_choose(move |i| {
                    tested.set_if_modified(None);
                    p.change(|s| s.phone.service = Service::ALL.get(i).copied().unwrap_or_default());
                }))
                .into_element(),
        );
        match service {
            Service::Discord => {
                let url = form.saved.read()[Kind::Discord as usize].clone();
                let hint = if !url.is_empty() && !url.contains("/api/webhooks/") {
                    status(&t, tr("Das sieht nicht wie eine Discord-Webhook-URL aus. Die enthält /api/webhooks/."), t.attention_text)
                } else {
                    note(&t, tr("In Discord: Servereinstellungen › Integrationen › Webhooks › Neuer Webhook › Webhook-URL kopieren."))
                };
                rows.push(field_row(&t, &form, Kind::Discord, tr("Webhook-URL"), DISCORD_EXAMPLE.to_owned(), None, hint));
            }
            Service::Telegram => {
                rows.push(field_row(&t, &form, Kind::Token, tr("Bot-Token"), tr("z. B. 123456789:AAF…"), None, note(&t, tr("Schreib @BotFather in Telegram /newbot und kopiere den Token hierher."))));
                let finding = *form.finding.read();
                let no_token = form.values[Kind::Token as usize].read().trim().is_empty();
                let reason = if finding { Some(tr("Sucht …")) } else { no_token.then(|| tr("Erst den Bot-Token eintragen")) };
                let finder = form.clone();
                let find = Button::new("settings.phone.find", if finding { tr("Sucht …") } else { tr("Chat-ID finden") }).disabled(reason).on_press(move |_| find_chat(&finder, store)).into_element();
                let below = match form.found.read().clone() {
                    Some(Ok(found)) => status(&t, found, t.success_text),
                    Some(Err(e)) => status(&t, e, t.danger_text),
                    None => note(&t, tr("Schreib deinem Bot zuerst eine Nachricht, dann „Chat-ID finden“.")),
                };
                rows.push(field_row(&t, &form, Kind::Chat, tr("Chat-ID"), tr("z. B. 123456789"), Some(find), below));
            }
            Service::Ntfy => {
                rows.push(field_row(&t, &form, Kind::Server, tr("Server"), phone::NTFY_DEFAULT.to_owned(), None, note(&t, tr("Nur ändern, wenn du einen eigenen ntfy-Server hast."))));
                let mut topic = form.values[Kind::Topic as usize];
                let roll = Button::new("settings.phone.topic.new", tr("Zufällig")).tip(tr("Ein langes zufälliges Thema, das niemand errät")).on_press(move |_| topic.set(random_topic())).into_element();
                rows.push(field_row(&t, &form, Kind::Topic, tr("Thema"), tr("z. B. farm-alarm-7f3k9q"), Some(roll), note(&t, tr("Abonniere dieses Thema in der ntfy-App. Wer es kennt, liest mit, also wähle etwas Langes."))));
            }
        }
        let testing = *form.testing.read();
        let tester = form.clone();
        let send = Button::new("settings.phone.test", if testing { tr("Sendet …") } else { tr("Testnachricht senden") }).disabled(testing.then(|| tr("Sendet …"))).on_press(move |_| test(&tester, store));
        let trouble = store.model.read().phone_trouble.clone();
        let outcome = match (form.tested.read().clone(), trouble) {
            (Some(Ok(done)), _) => Some(status(&t, done, t.success_text)),
            (Some(Err(e)), _) => Some(status(&t, e, t.danger_text)),
            (None, Some(trouble)) => Some(status(&t, trf("Zuletzt nicht angekommen: %@", &[&trouble]), t.danger_text)),
            (None, None) => None,
        };
        let mut line = Line::new("settings.phone.test.row", tr("Testnachricht")).hint(tr("Prüft, ob Meldungen auf dem Handy ankommen.")).control(send);
        if let Some(o) = outcome {
            line = line.below(o);
        }
        rows.push(line.into_element());
        rows.push(toggle(p, "settings.phone.back", tr("Auch wenn sie zurück sind"), tr("Meldet auch, wenn getrennte Accounts wieder auf dem Server sind."), current.reconnected, |s, on| s.phone.reconnected = on));
        rows.push(toggle(p, "settings.phone.whispers", tr("Auch Flüsternachrichten"), tr("Schickt Flüsternachrichten aufs Handy weiter."), current.whispers, |s, on| s.phone.whispers = on));
        group(&t, Some(tr("Aufs Handy")), rows, Some(tr("Webhook-URL, Token und Thema liegen nur im Passwortspeicher des Systems.")))
    }
}

fn field_row(t: &Theme, form: &Form, kind: Kind, label: String, placeholder: String, trailing: Option<Element>, below: Element) -> Element {
    let ix = kind as usize;
    let secret = kind.slot().is_some();
    let shown = form.shown.read()[ix];
    let mut field = Field::new(format!("settings.phone.{}", kind.id()), form.values[ix]).placeholder(placeholder).width(Size::flex(1.));
    if secret && !shown {
        field = field.secret();
    }
    let mut line = rect().width(Size::fill()).horizontal().cross_align(Alignment::Center).spacing(space::S).child(field).content(Content::flex());
    if secret {
        let mut flags = form.shown;
        line = line.child(Button::new(format!("settings.phone.{}.reveal", kind.id()), if shown { tr("Verbergen") } else { tr("Anzeigen") }).plain().on_press(move |_| {
            let mut f = flags.write();
            f[ix] = !f[ix];
        }));
    }
    if let Some(tr) = trailing {
        line = line.child(tr);
    }
    let below = match form.errors.read()[ix].clone() {
        Some(e) => status(t, e, t.danger_text),
        None => below,
    };
    Line::new(format!("settings.phone.{}.row", kind.id()), label).below(rect().width(Size::fill()).spacing(space::XS).child(line).child(below)).into_element()
}
