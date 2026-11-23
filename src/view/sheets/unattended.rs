//! The batch sheet's part for games nobody watches: how they run, screenshots instead of a window, and what to do with a server's resource pack.

use freya::prelude::*;

use crate::core::batch::{CONNECTION_ONLY_OFFERED, Packs, RunMode, SCREENSHOT_MINUTES, Unattended};
use crate::core::companion;
use crate::core::headless;
use crate::core::i18n::{tr, trf};
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::sheets::batch::{FIELD_W, hint, title_text};
use crate::view::theme::{Theme, space};

/// Whether the section has something set that a closed "Erweitert" would hide.
pub fn set(u: Unattended) -> bool {
    u != Unattended::default()
}

/// The run mode choice, the screenshot interval of hidden games and the resource pack policy, all for `game`'s version;
/// `proxied` counts the picked accounts with a proxy.
pub fn section(t: &Theme, state: State<Unattended>, game: &str, running: bool, lock: &Option<String>, proxied: usize) -> Element {
    let u = *state.read();
    let mut column = rect().width(Size::fill()).spacing(space::M).child(modes(t, state, u, game, running, lock, proxied));
    if u.mode == RunMode::Hidden {
        column = column.child(screenshots(t, state, u, lock));
    }
    if u.mode != RunMode::Connection {
        column = column.child(packs(t, state, u, running, lock));
    }
    column.into_element()
}

fn offered() -> Vec<RunMode> {
    let mut all = vec![RunMode::Windows, RunMode::Hidden];
    if CONNECTION_ONLY_OFFERED {
        all.push(RunMode::Connection);
    }
    all
}

fn unsupported(mode: RunMode, game: &str) -> Option<String> {
    if game.is_empty() {
        return None;
    }
    match mode {
        RunMode::Windows => None,
        RunMode::Hidden => (!companion::available(game)).then(|| trf("Geht erst ab Minecraft %@.", &[&companion::first()])),
        RunMode::Connection => (!headless::supports(game)).then(|| tr("Für diese Minecraft-Version noch nicht möglich.")),
    }
}

fn name(mode: RunMode) -> String {
    match mode {
        RunMode::Windows => tr("Fenster"),
        RunMode::Hidden => tr("Verstecktes Spiel"),
        RunMode::Connection => tr("Nur Verbindung · Beta"),
    }
}

fn sentence(mode: RunMode) -> String {
    match mode {
        RunMode::Windows => tr("Jedes Spiel hat sein echtes Fenster, das du im Manager ein- und ausblendest."),
        RunMode::Hidden => tr("Das echte Spiel läuft unsichtbar und sparsam, ohne Fenster. Mods, Screenshots und ältere Server gehen, es braucht aber den Speicher des Spiels."),
        RunMode::Connection => tr("Kein Spiel und kein Bild, nur Chat und Status: braucht kaum Speicher, aber keine Mods und keine Screenshots. Ob ein Server das erlaubt, entscheidest du."),
    }
}

fn modes(t: &Theme, mut state: State<Unattended>, u: Unattended, game: &str, running: bool, lock: &Option<String>, proxied: usize) -> Element {
    let all = offered();
    let segments = all
        .iter()
        .map(|m| Segment {
            id: format!(
                "batch.mode.{}",
                match m {
                    RunMode::Windows => "windows",
                    RunMode::Hidden => "hidden",
                    RunMode::Connection => "connection",
                }
            ),
            label: name(*m),
            tip: unsupported(*m, game),
        })
        .collect();
    let chosen = all.iter().position(|m| *m == u.mode).unwrap_or(0);
    let game_owned = game.to_owned();
    let pick = all.clone();
    let flags = all.iter().map(|m| *m != u.mode && unsupported(*m, game).is_some()).collect();
    let control = Segmented::new(tr("Wie die Spiele laufen"), segments, chosen).unavailable(flags).disabled(lock.clone()).on_choose(move |i| {
        if let Some(m) = pick.get(i)
            && (*m == state.peek().mode || unsupported(*m, &game_owned).is_none())
        {
            state.write().mode = *m;
        }
    });
    let mut text = sentence(u.mode);
    if running {
        text = format!("{text} {}", tr("Gilt beim nächsten Start."));
    }
    let mut block = block().child(title_text(t, tr("Wie die Spiele laufen"))).child(rect().horizontal().child(control)).child(hint(t, text));
    if let Some(why) = all.iter().find_map(|m| unsupported(*m, game).map(|why| (*m, why))).filter(|(m, _)| *m == RunMode::Connection || u.mode == *m) {
        block = block.child(hint(t, format!("{}: {}", name(why.0), why.1)));
    }
    if u.mode == RunMode::Connection && proxied > 0 {
        let warning = if proxied == 1 { tr("Ein Konto mit Proxy startet in diesem Modus noch nicht. Nimm den Proxy weg oder wähle „Fenster“.") } else { trf("%lld Konten mit Proxy starten in diesem Modus noch nicht. Nimm die Proxys weg oder wähle „Fenster“.", &[&proxied]) };
        block = block.child(hint(t, warning));
    }
    block.into_element()
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