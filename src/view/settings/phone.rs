//! Aufs Handy: the same notices on the phone through Discord, Telegram or ntfy. Secrets live only in the system keychain.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use super::{toggle, use_prefs};
use crate::app::event::Event;
use crate::core::i18n::{tr, trf};
use crate::core::phone::{self, Secret, Service, Slot};
use crate::core::settings;
use crate::ui::theme::phone_qr;
use crate::view::kit::button::Button;
use crate::view::kit::field::Field;
use crate::view::kit::group::{Line, group, note, status};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::{after, off_thread, scale, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{Theme, space};

const SAVE_AFTER: Duration = Duration::from_millis(400);
const DISCORD_EXAMPLE: &str = "https://discord.com/api/webhooks/…";
const QR_DARK: [u8; 3] = [0, 0, 0];

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
    dirty: State<[bool; 5]>,
    generated: State<bool>,
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

    fn alive(&self) -> bool {
        self.saved.try_peek().is_some()
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
    if value == form.saved.peek()[ix] && !form.dirty.peek()[ix] {
        return;
    }
    let (mut saved, mut errors, mut dirty) = (form.saved, form.errors, form.dirty);
    dirty.write()[ix] = true;
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
            store.dispatch(Event::PhoneOutcome(None));
        }
        if saved.try_peek().is_none() {
            return;
        }
        dirty.write()[ix] = result.is_err();
        if result.is_ok() {
            saved.write()[ix] = value;
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
    let form = form.clone();
    testing.set(true);
    tested.set(None);
    let Some(rx) = off_thread("phone-test", move || phone::Target::new(&current, secret).and_then(|t| phone::send_now(&t, &phone::test_text()))) else {
        testing.set(false);
        return;
    };
    spawn_in_window(async move {
        let result = rx.recv().await.unwrap_or_else(|_| Err(tr("Die Testnachricht ließ sich nicht senden.")));
        if result.is_ok() {
            store.dispatch(Event::PhoneOutcome(None));
        }
        if !form.alive() {
            return;
        }
        testing.set(false);
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
        if !form.alive() {
            return;
        }
        finding.set(false);
        let text = result.map(|c| {
            chat.set(c.id.clone());
            typed(&form, store, Kind::Chat);
            if c.name.is_empty() { tr("Chat gefunden.") } else { trf("Gefunden: %@", &[&c.name]) }
        });
        found.set(Some(text));
    });
}

fn link_key(link: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    link.hash(&mut hasher);
    hasher.finish().to_string()
}

fn qr_pixels(text: &str, target: u32) -> Option<(u32, Vec<u8>)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let modules = code.width();
    let cells = modules + 2 * phone_qr::QUIET;
    let cell = (usize::try_from(target).ok()? / cells).max(1);
    let side = cells * cell;
    let mut rgba = vec![u8::MAX; side * side * 4];
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color != qrcode::Color::Dark {
            continue;
        }
        let (x0, y0) = ((i % modules + phone_qr::QUIET) * cell, (i / modules + phone_qr::QUIET) * cell);
        for y in y0..y0 + cell {
            for x in x0..x0 + cell {
                let at = (y * side + x) * 4;
                rgba[at..at + 3].copy_from_slice(&QR_DARK);
            }
        }
    }
    Some((u32::try_from(side).ok()?, rgba))
}

/// The QR code of an ntfy link, drawn once per link at whole device pixels per module.
#[derive(PartialEq)]
struct Qr {
    link: String,
}

impl Component for Qr {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let link = self.link.clone();
        let device = scale();
        let code = use_hook(move || {
            let target = (phone_qr::SIZE * device).floor() as u32;
            let (side, rgba) = qr_pixels(&link, target)?;
            let handle = ImageHandle::from_rgba(side, side, Bytes::from(rgba), AlphaType::Unpremul)?;
            Some((handle, side as f32 / device))
        });
        let Some((handle, size)) = code else {
            return note(&t, tr("Zu lang für einen QR-Code.")).into_element();
        };
        rect()
            .width(Size::px(phone_qr::SIZE))
            .height(Size::px(phone_qr::SIZE))
            .center()
            .corner_radius(t.shape.small)
            .background(Color::WHITE)
            .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
            .child(image(handle).sampling_mode(SamplingMode::Nearest).aspect_ratio(AspectRatio::None).width(Size::px(size)).height(Size::px(size)))
            .into_element()
    }
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
            dirty: State::create([false; 5]),
            generated: State::create(false),
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
                if !form.alive() {
                    return;
                }
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
                let find = Button::new("settings.phone.find", tr("Chat-ID finden")).disabled(reason).on_press(move |_| find_chat(&finder, store)).into_element();
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
                let mut generated = form.generated;
                let roll = Button::new("settings.phone.topic.new", tr("Zufällig"))
                    .tip(tr("Ein langes zufälliges Thema, das niemand errät"))
                    .on_press(move |_| {
                        generated.set(true);
                        topic.set(phone::new_topic(uuid::Uuid::new_v4().as_u128()).expose().to_owned());
                    })
                    .into_element();
                let hint = if *form.generated.read() { tr("Manycraft hat ein langes, zufälliges Thema gewählt. Wer es kennt, liest mit.") } else { tr("Abonniere dieses Thema in der ntfy-App. Wer es kennt, liest mit, also wähle etwas Langes.") };
                rows.push(field_row(&t, &form, Kind::Topic, tr("Thema"), tr("z. B. farm-alarm-7f3k9q"), Some(roll), note(&t, hint)));
                let named = form.values[Kind::Topic as usize].read().trim().to_owned();
                if !named.is_empty() {
                    let server = form.values[Kind::Server as usize].read().clone();
                    let link = phone::ntfy_link(&server, &named).expose().to_owned();
                    let key = link_key(&link);
                    rows.push(Line::new("settings.phone.qr.row", tr("Handy verbinden")).hint(tr("Mit der Handy-Kamera scannen und das Thema in ntfy abonnieren.")).control(rect().key(key).child(Qr { link })).into_element());
                }
            }
        }
        let testing = *form.testing.read();
        let tester = form.clone();
        let send = Button::new("settings.phone.test", tr("Testnachricht senden")).disabled(testing.then(|| tr("Sendet …"))).on_press(move |_| test(&tester, store));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_pixels_use_whole_modules_inside_a_light_quiet_zone() {
        let (side, rgba) = qr_pixels("https://ntfy.sh/example", 160).expect("a short link fits");
        let side = side as usize;
        assert!(side <= 160 && rgba.len() == side * side * 4);
        assert!(rgba[..4] == [u8::MAX; 4]);
        let first = rgba.chunks(4).position(|p| p[0] == 0).expect("the code has dark modules");
        let (x, y) = (first % side, first / side);
        assert_eq!(x, y);
        assert_eq!(x % phone_qr::QUIET, 0);
        let cell = x / phone_qr::QUIET;
        assert!(cell >= 1 && side % cell == 0);
    }

    #[test]
    fn qr_pixels_refuse_a_link_too_long_for_a_code() {
        assert!(qr_pixels(&"x".repeat(4000), 160).is_none());
    }

    #[test]
    fn link_keys_differ_per_link() {
        assert_ne!(link_key("https://ntfy.sh/a"), link_key("https://ntfy.sh/b"));
    }
}
