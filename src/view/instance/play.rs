//! The primary Play control of an instance as a state machine: Play, the progress of an install or start with Cancel,
//! the way back to the running game with Stop, a stop that can be forced, signing in again and trying again after a crash.

use std::time::{Duration, Instant};

use freya::prelude::*;
use uuid::Uuid;

use super::{Ended, Facts, facts};
use crate::app::event::Event as AppEvent;
use crate::app::meter::Meter;
use crate::core::account::AccountKind;
use crate::core::game_window::CAN_RAISE;
use crate::core::i18n::{tr, trf};
use crate::view::commands::{Shortcut, Want, want, want_blocked};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::{Button, Look};
use crate::view::kit::menu::{self, Face, Item, Menu};
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::{Bounds, after, ring, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::{control, font, frame, iconsize, space};

const LOADING_W: f32 = 320.;

const GUARD: Duration = Duration::from_secs(2);

const NAME_MAX: f32 = 240.;

const ACCOUNTS_SEARCH_FROM: usize = 6;

fn tip(label: String, shortcut: Shortcut) -> String {
    format!("{label} ({})", shortcut.label())
}

fn account_items(store: Store, f: &Facts) -> Vec<Item> {
    let m = store.model.peek();
    let mut items = Vec::new();
    for (account, name) in &f.accounts {
        let (instance, id) = (f.made.id.clone(), *account);
        let kind = m.account(id).map(|a| a.kind);
        let mut item = if let Some(place) = f.busy_in(id) {
            Item::new(format!("instance.play.as.{id}"), name.clone(), |_| {}).shortcut(trf("läuft in %@", &[&place])).disabled(true)
        } else if f.playing.contains(account) {
            Item::new(format!("instance.play.as.{id}"), name.clone(), |_| {}).shortcut(tr("Läuft")).disabled(true)
        } else if f.expired.contains(account) {
            let blocked = crate::view::commands::sign_in_blocked(store);
            Item::new(format!("instance.play.as.{id}"), name.clone(), move |_| crate::view::accounts::sign_in(store, vec![id])).disabled(blocked.is_some()).warn().shortcut(blocked.unwrap_or_else(|| format!("{} · {}", tr("Anmeldung abgelaufen"), tr("Anmelden"))))
        } else {
            let tag = if kind == Some(AccountKind::Microsoft) { tr("Microsoft") } else { tr("Offline") };
            Item::new(format!("instance.play.as.{id}"), name.clone(), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account: Some(id) })).shortcut(tag)
        };
        if let Some(a) = m.account(id) {
            item = item.face(Face { id, name: a.name.clone(), color: a.color() });
        }
        items.push(item.checked(f.pick == Some(id)));
    }
    if f.pinned {
        let instance = f.made.id.clone();
        items.push(Item::Separator);
        items.push(Item::new("instance.play.default.clear", tr("Standard-Account verwenden"), move |_| store.dispatch(AppEvent::SetDefaultAccount { instance: instance.clone(), account: None })));
    }
    items
}

/// The account Play starts with, as a pull-down that picks the instance's own account.
#[derive(PartialEq)]
struct AccountChip {
    facts: Facts,
    compact: bool,
}

impl Component for AccountChip {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut anchor = use_state(Bounds::default);
        let mut hovered = use_state(|| false);
        let mut pressed = use_state(|| false);
        let f = &self.facts;
        let pick = f.pick.or(f.solo);
        let name = pick.and_then(|a| f.name_of(a)).unwrap_or_default().to_owned();
        let label = trf("als %@", &[&name]);
        let spot = use_spot("instance.play.account", "button", &label, true);
        let menu_facts = f.clone();
        let open = move || {
            let items = account_items(store, &menu_facts);
            let menu = Menu::under(items, *anchor.peek());
            menu::open(store.ui, if menu_facts.accounts.len() >= ACCOUNTS_SEARCH_FROM { menu.searchable(tr("Account suchen")) } else { menu });
        };
        let keys_open = open.clone();
        let mut press_open = open;
        let instead = f.solo.filter(|s| Some(*s) != f.pick).and_then(|s| {
            let who = f.name_of(s)?.to_owned();
            match f.busy_in(s) {
                Some(place) => Some(trf("statt %1$@ (läuft in %2$@)", &[&who, &place])),
                None => f.expired.contains(&s).then(|| trf("statt %@ (Anmeldung abgelaufen)", &[&who])),
            }
        });
        let tip = match instead {
            Some(why) => why,
            None if self.compact => label.clone(),
            None => tr("Account wählen"),
        };
        let fill = if *pressed.read() {
            t.press
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let m = store.model.read();
        let face = pick.and_then(|a| m.account(a)).map(|a| crate::view::accounts::head(&t, store, a.id, &a.name, a.color(), frame::FACE));
        drop(m);
        let mut chip = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(format!("{label}, {}", tr("Account wählen")))
            .height(Size::px(control::L))
            .padding((0., space::S, 0., space::XS + space::XXS))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::XS + space::XXS)
            .corner_radius(t.shape.control)
            .background(fill)
            .cursor(CursorIcon::Pointer)
            .on_sized(move |e: Event<SizedEventData>| anchor.set_if_modified(Bounds::of(&e)))
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| {
                hovered.set_if_modified(false);
                pressed.set_if_modified(false);
            })
            .on_mouse_down(move |e: Event<MouseEventData>| {
                if e.button == Some(MouseButton::Left) {
                    pressed.set(true);
                }
            })
            .on_mouse_up(move |_| pressed.set_if_modified(false))
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                a11y.request_focus();
                press_open();
            })
            .on_key_down(move |e: Event<KeyboardEventData>| {
                let go = matches!(&e.key, Key::Named(NamedKey::Enter | NamedKey::ArrowDown)) || matches!(&e.key, Key::Character(c) if c == " ");
                if a11y.is_focused() && go {
                    e.stop_propagation();
                    keys_open.clone()();
                }
            });
        if let Some(face) = face {
            chip = chip.child(face);
        }
        if !self.compact {
            chip = chip.child(line(&t, label, font::BODY, t.text1).font_weight(FontWeight::MEDIUM).max_lines(1).text_overflow(TextOverflow::Ellipsis).max_width(Size::px(frame::CHIP_NAME)));
        }
        chip = chip.child(icon(&t, Icon::ChevronDown, iconsize::S, t.text2));
        if let Some(border) = ring(&t, focus()) {
            chip = chip.border(border);
        }
        let chip = rect().corner_radius(t.shape.control).background(t.control).on_sized(spot).child(chip);
        WithTip::new(Some(tip), chip)
    }
}

/// What the primary control of an instance shows and does now.
#[derive(Clone, Debug, PartialEq)]
pub enum Play {
    /// No account exists to play with.
    NoAccount,
    /// Every account plays this instance in a batch.
    Taken,
    /// The account Play would use has to sign in again.
    SignIn { account: Uuid, name: String },
    Idle,
    /// The last game crashed or failed to start.
    Retry,
    /// Minecraft's files are being installed for the game on its way, to `percent`.
    Installing(u8),
    /// A game is on its way, with the keys Cancel stops; none while Play was pressed and the model has not caught up.
    Starting { keys: Vec<String> },
    /// A game runs: `raise` is the one to switch to, `keys` all that Stop ends.
    Running { raise: String, keys: Vec<String> },
    /// A game is being stopped, and it can be ended at once when `overdue`.
    Stopping { key: String, overdue: bool },
}

/// The state of the primary control from the facts of the instance; `pending` while Play was pressed just now.
pub fn play(f: &Facts, pending: bool) -> Play {
    if f.accounts.is_empty() {
        return Play::NoAccount;
    }
    let keys: Vec<String> = f.stoppable.iter().map(|(k, _)| k.clone()).collect();
    if let Some(raise) = keys.iter().find(|k| !f.launching.contains(k)).cloned() {
        return Play::Running { raise, keys };
    }
    if !f.launching.is_empty() {
        return match f.loading {
            Some(percent) => Play::Installing(percent.min(100)),
            None => Play::Starting { keys: f.launching.clone() },
        };
    }
    if let Some((key, overdue)) = f.stopping.clone() {
        return Play::Stopping { key, overdue };
    }
    if f.none_free() {
        return Play::Taken;
    }
    if let Some(account) = f.pick.filter(|a| f.expired.contains(a)) {
        return Play::SignIn { account, name: f.name_of(account).unwrap_or_default().to_owned() };
    }
    if pending {
        return Play::Starting { keys: Vec::new() };
    }
    if matches!(f.ended, Some(Ended::Crashed | Ended::Failed)) { Play::Retry } else { Play::Idle }
}

fn pressed(store: Store, id: &str) -> bool {
    store.ui.instance.played.read().get(id).is_some_and(|at| at.elapsed() < GUARD)
}

/// Starts the instance with `account`, or with the one Play would use; a second press within two seconds does nothing.
pub fn play_now(store: Store, id: &str, account: Option<Uuid>) {
    let mut played = store.ui.instance.played;
    if played.peek().get(id).is_some_and(|at| at.elapsed() < GUARD) {
        return;
    }
    played.write().insert(id.to_owned(), Instant::now());
    store.dispatch(AppEvent::PlayInstance { instance: id.to_owned(), account });
    spawn_in_window(async move {
        after(GUARD + Duration::from_millis(50)).await;
        if let Some(mut played) = played.try_write() {
            played.retain(|_, at| at.elapsed() < GUARD);
        }
    });
}

fn stop_keys(store: Store, keys: Vec<String>) {
    store.dispatch(AppEvent::Stop(keys));
}

fn sign_in_button(store: Store, account: Uuid, name: &str) -> Button {
    Button::new("instance.play", trf("Anmelden als %@", &[&name]))
        .glyph(Icon::LogIn)
        .primary()
        .tall()
        .label_max(NAME_MAX)
        .tip(trf("Die Anmeldung von %@ ist abgelaufen.", &[&name]))
        .disabled(want_blocked(&Want::SignInAgain(Vec::new())))
        .on_press(move |_| crate::view::accounts::sign_in(store, vec![account]))
}

fn batch_taken() -> String {
    tr("Jeder Account spielt diese Instanz gerade in einem Batch.")
}

fn starting() -> String {
    tr("Das Spiel startet.")
}

fn saving() -> String {
    tr("Das Spiel speichert und beendet sich.")
}

/// Play, Stop and the way back to the running game, at the trailing end of the instance's toolbar.
#[derive(PartialEq)]
pub struct Hero {
    pub id: String,
    /// The width the toolbar has, so the account chip folds to its face when it runs short.
    pub room: f32,
}

impl Component for Hero {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let mut ui = store.ui;
        let Some(f) = facts(&store.model.read(), &self.id) else { return rect().into_element() };
        let state = play(&f, pressed(store, &self.id));
        let row = rect().horizontal().cross_align(Alignment::Center).spacing(space::S);
        let id = self.id.clone();
        let with_chip = |row: Rect| if f.accounts.len() > 1 { row.child(AccountChip { facts: f.clone(), compact: self.room < frame::TOOLBAR_SEARCH }) } else { row };
        match state {
            Play::NoAccount => row
                .child(Button::new("instance.play", tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().tall().on_press(move |_| {
                    want(store, Want::AddAccount);
                }))
                .into_element(),
            Play::Taken => with_chip(row).child(Button::new("instance.play", tr("Spielen")).glyph(Icon::Play).primary().tall().disabled(Some(batch_taken()))).into_element(),
            Play::SignIn { account, name } => with_chip(row).child(sign_in_button(store, account, &name)).into_element(),
            Play::Idle | Play::Retry => {
                let (label, glyph) = if state == Play::Retry { (tr("Erneut spielen"), Icon::Refresh) } else { (tr("Spielen"), Icon::Play) };
                let pick = f.pick;
                with_chip(row)
                    .child(Button::new("instance.play", label.clone()).glyph(glyph).primary().tall().tip(tip(label, Shortcut::primary(Code::Enter))).on_press(move |_| {
                        ui.menu.set_if_modified(None);
                        play_now(store, &id, pick);
                    }))
                    .into_element()
            }
            Play::Installing(_) => {
                let keys = f.launching.clone();
                let meter = crate::view::install::game_meter(&store.model.peek(), &id).unwrap_or_else(|| Meter::begin(tr("Wird vorbereitet …"), Instant::now()));
                rect()
                    .width(Size::px(LOADING_W))
                    .child(crate::view::install::Progress {
                        id: "instance.loading".into(),
                        meter,
                        stopping: false,
                        on_cancel: EventHandler::new(move |_| stop_keys(store, keys.clone())),
                    })
                    .into_element()
            }
            Play::Starting { keys } => {
                let waiting = keys.is_empty();
                row.child(Button::new("instance.play", tr("Startet …")).busy(true).tall().disabled(Some(starting())))
                    .child(Button::new("instance.cancel", tr("Abbrechen")).tall().disabled(waiting.then(|| starting())).on_press(move |_| stop_keys(store, keys.clone())))
                    .into_element()
            }
            Play::Running { raise, keys } => {
                let label = if keys.len() == 1 { tr("Stoppen") } else { tr("Alle stoppen") };
                let stop_tip = tip(label.clone(), Shortcut::primary(Code::Period));
                if !CAN_RAISE {
                    return row.child(Button::new("instance.stop", label).glyph(Icon::Stop).tall().tip(stop_tip).on_press(move |_| stop_keys(store, keys.clone()))).into_element();
                }
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::XXS)
                    .child(Button::new("instance.play", tr("Zum Spiel")).glyph(Icon::External).primary().tall().tip(tip(tr("Zum Spiel"), Shortcut::primary(Code::Enter))).on_press(move |_| store.dispatch(AppEvent::Raise(raise.clone()))))
                    .child(Button::icon("instance.stop", Icon::Stop, stop_tip).look(Look::Secondary).tall().on_press(move |_| stop_keys(store, keys.clone())))
                    .into_element()
            }
            Play::Stopping { key, overdue: true } => row
                .child(Button::new("instance.play", tr("Sofort beenden")).glyph(Icon::Stop).danger().tall().tip(trf("Nicht Gespeichertes geht verloren (%@)", &[&Shortcut::primary(Code::Period).label()])).on_press(move |_| stop_keys(store, vec![key.clone()])))
                .into_element(),
            Play::Stopping { .. } => row.child(Button::new("instance.play", tr("Stoppt …")).busy(true).tall().disabled(Some(saving()))).into_element(),
        }
    }
}

/// Play of an instance as one icon, for cards and rows that show it while they are hovered.
#[derive(PartialEq)]
pub struct Mini {
    pub id: String,
    pub name: String,
    pub spot: String,
    pub plain: bool,
}

impl Component for Mini {
    fn render(&self) -> impl IntoElement {
        let store = use_store();
        let Some(f) = facts(&store.model.read(), &self.id) else { return rect().into_element() };
        let state = play(&f, pressed(store, &self.id));
        let (id, name, key) = (self.id.clone(), self.name.clone(), format!("{}.{}", self.spot, self.id));
        let pick = f.pick;
        let button = match state {
            Play::NoAccount => Button::icon(format!("{key}.play"), Icon::UserPlus, tr("Account hinzufügen")).primary().on_press(move |_| {
                want(store, Want::AddAccount);
            }),
            Play::Taken => Button::icon(format!("{key}.play"), Icon::Play, trf("„%@“ spielen", &[&name])).disabled(Some(batch_taken())),
            Play::SignIn { account, name: who } => Button::icon(format!("{key}.play"), Icon::LogIn, trf("Anmelden als %@", &[&who])).primary().disabled(want_blocked(&Want::SignInAgain(Vec::new()))).on_press(move |_| crate::view::accounts::sign_in(store, vec![account])),
            Play::Idle => Button::icon(format!("{key}.play"), Icon::Play, trf("„%@“ spielen", &[&name])).primary().on_press(move |_| play_now(store, &id, pick)),
            Play::Retry => Button::icon(format!("{key}.play"), Icon::Refresh, trf("„%@“ erneut spielen", &[&name])).primary().on_press(move |_| play_now(store, &id, pick)),
            Play::Installing(_) | Play::Starting { .. } => {
                let keys = f.launching.clone();
                Button::icon(format!("{key}.cancel"), Icon::Stop, trf("„%@“ abbrechen", &[&name])).disabled(keys.is_empty().then(|| starting())).on_press(move |_| stop_keys(store, keys.clone()))
            }
            Play::Running { keys, .. } => Button::icon(format!("{key}.stop"), Icon::Stop, trf("„%@“ stoppen", &[&name])).on_press(move |_| stop_keys(store, keys.clone())),
            Play::Stopping { key: game, overdue: true } => Button::icon(format!("{key}.stop"), Icon::Stop, trf("„%@“ sofort beenden · Nicht Gespeichertes geht verloren", &[&name])).danger().on_press(move |_| stop_keys(store, vec![game.clone()])),
            Play::Stopping { .. } => Button::icon(format!("{key}.stop"), Icon::Stop, tr("Stoppt …")).busy(true).disabled(Some(saving())),
        };
        let button = if self.plain { button.plain() } else { button };
        button.not_focusable().into_element()
    }
}

/// The account Play starts with now, for the palette and the shortcut.
pub fn pick(f: &Facts) -> Option<Uuid> {
    if f.none_free() { None } else { f.pick }
}

/// The one action of an empty tab that fills when the instance has been played: Play, or the way back to its game.
pub fn empty_action(store: Store, f: &Facts, id: &str) -> Option<Button> {
    let instance = f.made.id.clone();
    let id = id.to_owned();
    Some(match play(f, pressed(store, &instance)) {
        Play::NoAccount => Button::new(id, tr("Account hinzufügen")).glyph(Icon::UserPlus).primary().on_press(move |_| {
            want(store, Want::AddAccount);
        }),
        Play::Taken => Button::new(id, tr("Spielen")).glyph(Icon::Play).primary().disabled(Some(batch_taken())),
        Play::SignIn { account, name } => sign_in_button(store, account, &name),
        Play::Idle | Play::Retry => {
            let pick = f.pick;
            Button::new(id, tr("Spielen")).glyph(Icon::Play).primary().on_press(move |_| play_now(store, &instance, pick))
        }
        Play::Installing(_) | Play::Starting { .. } => Button::new(id, tr("Startet …")).busy(true).primary().disabled(Some(starting())),
        Play::Running { keys, .. } if !CAN_RAISE => Button::new(id, tr("Stoppen")).glyph(Icon::Stop).on_press(move |_| stop_keys(store, keys.clone())),
        Play::Running { raise, .. } => Button::new(id, tr("Zum Spiel")).glyph(Icon::External).primary().on_press(move |_| store.dispatch(AppEvent::Raise(raise.clone()))),
        Play::Stopping { .. } => Button::new(id, tr("Stoppt …")).busy(true).primary().disabled(Some(saving())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instance::{Instance, Loader};

    fn facts() -> Facts {
        let account = Uuid::from_u128(1);
        Facts {
            made: Instance::new("a", "A", Loader::Vanilla),
            batches: Vec::new(),
            accounts: vec![(account, "Max".into())],
            solo: Some(account),
            pinned: false,
            running: false,
            playing: Vec::new(),
            stoppable: Vec::new(),
            stopping: None,
            trouble: None,
            ended: None,
            launching: Vec::new(),
            loading: None,
            memory_mb: 0,
            expired: Vec::new(),
            busy: Vec::new(),
            pick: Some(account),
        }
    }

    fn running(f: &mut Facts, keys: &[&str], launching: &[&str]) {
        f.running = true;
        f.playing = vec![f.accounts[0].0];
        f.stoppable = keys.iter().map(|k| ((*k).to_owned(), "Max".to_owned())).collect();
        f.launching = launching.iter().map(|k| (*k).to_owned()).collect();
    }

    #[test]
    fn a_fresh_instance_plays_and_a_press_just_made_shows_starting_at_once() {
        let f = facts();
        assert_eq!(play(&f, false), Play::Idle);
        assert_eq!(play(&f, true), Play::Starting { keys: Vec::new() });
    }

    #[test]
    fn without_an_account_it_asks_for_one() {
        let mut f = facts();
        f.accounts.clear();
        f.pick = None;
        assert_eq!(play(&f, false), Play::NoAccount);
    }

    #[test]
    fn a_game_on_its_way_installs_while_files_load_and_starts_after() {
        let mut f = facts();
        running(&mut f, &["k"], &["k"]);
        assert_eq!(play(&f, false), Play::Starting { keys: vec!["k".into()] });
        f.loading = Some(43);
        assert_eq!(play(&f, true), Play::Installing(43));
        f.loading = Some(250);
        assert_eq!(play(&f, false), Play::Installing(100));
    }

    #[test]
    fn a_game_with_a_window_runs_and_stop_ends_every_game_of_it() {
        let mut f = facts();
        running(&mut f, &["a", "b"], &["b"]);
        assert_eq!(play(&f, false), Play::Running { raise: "a".into(), keys: vec!["a".into(), "b".into()] });
    }

    #[test]
    fn a_stop_can_be_forced_once_it_is_overdue() {
        let mut f = facts();
        f.stopping = Some(("k".into(), false));
        assert_eq!(play(&f, false), Play::Stopping { key: "k".into(), overdue: false });
        f.stopping = Some(("k".into(), true));
        assert_eq!(play(&f, false), Play::Stopping { key: "k".into(), overdue: true });
    }

    #[test]
    fn a_game_that_runs_stays_on_top_while_another_one_stops() {
        let mut f = facts();
        running(&mut f, &["a"], &[]);
        f.stopping = Some(("b".into(), true));
        assert!(matches!(play(&f, false), Play::Running { .. }));
    }

    #[test]
    fn an_expired_account_signs_in_before_it_plays() {
        let mut f = facts();
        f.expired = vec![f.accounts[0].0];
        assert_eq!(play(&f, true), Play::SignIn { account: f.accounts[0].0, name: "Max".into() });
    }

    #[test]
    fn a_crash_or_failed_start_offers_to_play_again_but_a_sign_in_does_not() {
        let mut f = facts();
        f.ended = Some(Ended::Crashed);
        assert_eq!(play(&f, false), Play::Retry);
        f.ended = Some(Ended::Failed);
        assert_eq!(play(&f, false), Play::Retry);
        f.ended = Some(Ended::SignIn);
        assert_eq!(play(&f, false), Play::Idle);
    }

    #[test]
    fn an_instance_every_account_plays_in_a_batch_cannot_start() {
        let mut f = facts();
        f.busy = vec![(f.accounts[0].0, "Farm".into())];
        assert_eq!(play(&f, false), Play::Taken);
    }

    #[test]
    fn a_crash_never_shows_while_a_new_game_is_on_its_way() {
        let mut f = facts();
        f.ended = Some(Ended::Crashed);
        running(&mut f, &["k"], &["k"]);
        assert!(matches!(play(&f, false), Play::Starting { .. }));
    }
}
