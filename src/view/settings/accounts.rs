//! Accounts: signing every account out of this computer at once.

use freya::prelude::*;

use crate::app::event::{Event, Report};
use crate::core::account::{Account, AccountKind};
use crate::core::auth;
use crate::core::i18n::{tr, trf};
use crate::view::icons::Icon;
use crate::view::kit::button::Button;
use crate::view::kit::group::{Line, status};
use crate::view::kit::{off_ui, open_external, sheet, toast, use_theme};
use crate::view::store::{Store, use_store};
use crate::view::theme::space;

const SECURITY_PAGE: &str = "https://account.microsoft.com/security";

/// How the last sign-out of all accounts went.
#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Idle,
    Busy,
    Done,
    Failed(usize),
}

/// The row that signs every account out of this computer, the last of the data and updates group.
#[derive(PartialEq)]
pub struct SignOutRow;

impl Component for SignOutRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let outcome = use_state(|| Outcome::Idle);
        let any = store.model.read().accounts.iter().any(|a| a.kind == AccountKind::Microsoft);
        let now = *outcome.read();
        let button = match now {
            Outcome::Failed(_) => Button::new("settings.accounts.signout", tr("Erneut versuchen")).glyph(Icon::Refresh).on_press(move |_| sign_out(store, Some(outcome))),
            _ => {
                let why = if now == Outcome::Busy { Some(tr("Wird abgemeldet …")) } else { (!any).then(|| tr("Kein Account ist angemeldet.")) };
                Button::new("settings.accounts.signout", tr("Abmelden…")).disabled(why).on_press(move |_| confirm(store, Some(outcome)))
            }
        };
        let mut line = Line::new("settings.accounts.signout.row", tr("Alle Accounts abmelden")).control(button);
        line = match now {
            Outcome::Done => line.below(
                rect()
                    .width(Size::fill())
                    .spacing(space::XS)
                    .child(status(&t, done_text(), t.text2))
                    .child(rect().horizontal().child(Button::new("settings.accounts.security", tr("Microsoft-Kontoseite öffnen")).glyph(Icon::External).plain().on_press(|_| open_security_page()))),
            ),
            Outcome::Failed(n) => line.hint(failed_text(n)).hint_tone(t.danger_text),
            Outcome::Idle | Outcome::Busy => line.hint(tr("Entfernt alle gespeicherten Anmeldungen von diesem Computer. Sinnvoll, wenn du einer Mod nicht mehr traust.")),
        };
        line.into_element()
    }
}

/// Asks before signing every Microsoft account out of this computer, and says in a notice how it went.
pub fn sign_out_all(store: Store) {
    confirm(store, None);
}

fn confirm(store: Store, outcome: Option<State<Outcome>>) {
    let message = tr("Die Accounts bleiben in der Liste, aber jeder muss sich vor dem nächsten Spielen neu anmelden. Laufende Spiele laufen weiter, bis du sie beendest.");
    sheet::confirm(store.ui, "settings.accounts.signout.confirm", tr("Alle Accounts abmelden?"), message, tr("Alle abmelden"), true, move |_| sign_out(store, outcome));
}

fn sign_out(store: Store, outcome: Option<State<Outcome>>) {
    let accounts: Vec<Account> = store.model.peek().accounts.iter().filter(|a| a.kind == AccountKind::Microsoft).cloned().collect();
    if let Some(mut o) = outcome {
        o.set(Outcome::Busy);
    }
    spawn_in_window(async move {
        let asked = accounts.clone();
        let Some(failed) = off_ui(move || auth::sign_out_all(&asked)).await else { return };
        let gone: Vec<Report> = accounts.iter().filter(|a| !failed.contains(&a.id)).map(|a| Report::SignIn { account: a.id, valid: false }).collect();
        if !gone.is_empty() {
            store.dispatch(Event::Reports(gone));
        }
        let shown = match outcome {
            Some(mut o) if o.try_peek().is_some() => {
                o.set(if failed.is_empty() { Outcome::Done } else { Outcome::Failed(failed.len()) });
                true
            }
            _ => false,
        };
        if shown {
            return;
        }
        if failed.is_empty() {
            toast::show_action(store.ui, done_text(), tr("Microsoft-Kontoseite öffnen"), EventHandler::new(|_| open_security_page()));
        } else {
            toast::show_action(store.ui, failed_text(failed.len()), tr("Erneut versuchen"), EventHandler::new(move |_| sign_out(store, None)));
        }
    });
}

fn done_text() -> String {
    tr("Alle Accounts abgemeldet. Hat jemand deine Anmeldung schon kopiert, gilt sie weiter, bis du dich bei Microsoft überall abmeldest.")
}

fn failed_text(count: usize) -> String {
    trf("%@ Accounts konnten nicht abgemeldet werden.", &[&count])
}

fn open_security_page() {
    open_external("open url", SECURITY_PAGE.to_owned());
}