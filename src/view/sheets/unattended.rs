//! The batch sheet's part for games nobody watches: no window, screenshots instead, and what to do with a server's resource pack.

use freya::prelude::*;

use crate::core::batch::{Packs, SCREENSHOT_MINUTES, Unattended};
use crate::core::companion;
use crate::core::i18n::{tr, trf};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::sheets::batch::{FIELD_W, hint, title_text};
use crate::view::theme::{Theme, font, space};

/// Whether the section has something set that a closed "Erweitert" would hide.
pub fn set(u: Unattended) -> bool {
    u != Unattended::default()
}

/// The switch for windowless games, their screenshot interval and the resource pack policy, all for `game`'s version.
pub fn section(t: &Theme, state: State<Unattended>, game: &str, running: bool, lock: &Option<String>) -> Element {
    let u = *state.read();
    let mut column = rect().width(Size::fill()).spacing(space::M).child(windowless(t, state, u, game, running, lock));
    if u.headless {
        column = column.child(screenshots(t, state, u, lock));
    }
    column.child(packs(t, state, u, running, lock)).into_element()
}

fn windowless(t: &Theme, mut state: State<Unattended>, u: Unattended, game: &str, running: bool, lock: &Option<String>) -> Element {
    let label = tr("Ohne Spielfenster");
    let unsupported = (!game.is_empty() && !companion::available(game) && !u.headless).then(|| trf("Geht erst ab Minecraft %@.", &[&companion::first()]));
    let reason = lock.clone().or(unsupported);
    let text = tr("Die Spiele laufen unsichtbar und sparsam, ohne Fenster und ohne Platz in der Taskleiste. Du siehst sie über Screenshots; Chat und Befehle gehen wie sonst.");
    let text = if running { format!("{text} {}", tr("Gilt beim nächsten Start.")) } else { text };
    let blocked = reason.is_some();
    let mut toggle = move || {
        if !blocked {
            state.write().headless ^= true;
        }
    };
    let row = rect()
        .width(Size::fill())
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .cursor(if blocked { CursorIcon::NotAllowed } else { CursorIcon::Pointer })
        .on_pointer_press(move |e: Event<PointerEventData>| {
            if let PointerEventData::Mouse(m) = &*e
                && m.button == Some(MouseButton::Left)
            {
                toggle();
            }
        })
        .child(rect().on_pointer_press(|e: Event<PointerEventData>| e.stop_propagation()).child(Switch::new("batch.headless", label.clone(), u.headless).disabled(blocked).on_toggle(move |_| toggle())))
        .child(line(t, label, font::CALLOUT, t.text2));
    block().child(WithTip::new(reason, row).fill()).child(hint(t, text)).into_element()
}

fn screenshots(t: &Theme, mut state: State<Unattended>, u: Unattended, lock: &Option<String>) -> Element {
    let choices: Vec<Option<u32>> = std::iter::once(None).chain(SCREENSHOT_MINUTES.into_iter().map(Some)).collect();
    let labels = choices
        .iter()
        .map(|c| match c {
            None => tr("Nur auf Wunsch"),
            Some(1) => tr("Jede Minute"),
            Some(n) => trf("Alle %lld Minuten", &[n]),
        })
        .collect();
    let at = choices.iter().position(|c| *c == u.screenshots).unwrap_or(0);
    let picker = Select::new("batch.screenshots", tr("Screenshots"), labels, at).width(FIELD_W - space::L).disabled(lock.clone()).on_choose(move |i| {
        if let Some(c) = choices.get(i) {
            state.write().screenshots = *c;
        }
    });
    let text = tr("Beim Betreten des Servers kommt einer von selbst, weitere über die Kachel. Jeder Screenshot kostet kurz Leistung.");
    block().child(title_text(t, tr("Screenshots"))).child(picker).child(hint(t, text)).into_element()
}

fn packs(t: &Theme, mut state: State<Unattended>, u: Unattended, running: bool, lock: &Option<String>) -> Element {
    let name = |p: Packs| match p {
        Packs::Ask => tr("Fragen"),
        Packs::Accept => tr("Annehmen"),
        Packs::Decline => tr("Ablehnen"),
    };
    let segments = Packs::ALL
        .iter()
        .map(|p| Segment {
            id: format!(
                "batch.packs.{}",
                match p {
                    Packs::Ask => "ask",
                    Packs::Accept => "accept",
                    Packs::Decline => "decline",
                }
            ),
            label: name(*p),
            tip: None,
        })
        .collect();
    let chosen = Packs::ALL.iter().position(|p| *p == u.packs).unwrap_or(0);
    let control = Segmented::new(tr("Ressourcenpakete vom Server"), segments, chosen).disabled(lock.clone()).on_choose(move |n| {
        if let Some(p) = Packs::ALL.get(n) {
            state.write().packs = *p;
        }
    });
    let text = match u.packs {
        Packs::Ask => tr("Ein offenes Spiel fragt wie sonst. Ein Spiel im Hintergrund wartet, bis du in seiner Kachel antwortest."),
        Packs::Accept => tr("Jedes Paket, das ein Server anbietet, wird geladen."),
        Packs::Decline => tr("Jedes Paket wird abgelehnt. Verlangt der Server eins, trennt er die Verbindung."),
    };
    let text = if running { format!("{text} {}", tr("Gilt sofort.")) } else { text };
    block().child(title_text(t, tr("Ressourcenpakete vom Server"))).child(rect().horizontal().child(control)).child(hint(t, text)).into_element()
}

fn block() -> Rect {
    rect().width(Size::fill()).padding((0., 0., 0., space::L)).spacing(space::XS)
}