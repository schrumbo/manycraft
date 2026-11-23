//! The batch sheet's rows for games nobody watches: how they run, screenshots instead of a window, and what to do with a
//! server's resource pack.

use freya::prelude::*;

use crate::core::batch::{CONNECTION_ONLY_OFFERED, Packs, RunMode, SCREENSHOT_MINUTES, Unattended};
use crate::core::companion;
use crate::core::i18n::{tr, trf};
use crate::view::kit::group::Line;
use crate::view::kit::segmented::{Segment, Segmented};
use crate::view::kit::select::Select;
use crate::view::theme::{Theme, frame};

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
        RunMode::Windows | RunMode::Connection => None,
        RunMode::Hidden => (!companion::available(game)).then(|| trf("Geht erst ab Minecraft %@.", &[&companion::first()])),
    }
}

fn name(mode: RunMode) -> String {
    match mode {
        RunMode::Windows => tr("Fenster"),
        RunMode::Hidden => tr("Versteckt"),
        RunMode::Connection => tr("Headless"),
    }
}

fn sentence(mode: RunMode) -> String {
    match mode {
        RunMode::Windows => tr("Jedes Spiel hat sein echtes Fenster, das du im Manager ein- und ausblendest."),
        RunMode::Hidden => tr("Das echte Spiel läuft unsichtbar und sparsam, ohne Fenster. Mods, Screenshots und ältere Server gehen, es braucht aber den Speicher des Spiels."),
        RunMode::Connection => tr("Kein Spiel und kein Bild, nur Chat und Status: braucht kaum Speicher, aber keine Mods und keine Screenshots. Ob ein Server das erlaubt, entscheidest du."),
    }
}

/// The first row of the sheet: how the games run. `game` is the Minecraft version of the chosen instance, `proxied` counts
/// the picked accounts with a proxy, `lock` says why nothing can change.
pub fn mode_line(t: &Theme, mut state: State<Unattended>, game: &str, lock: &Option<String>, proxied: usize) -> Line {
    let mode = state.read().mode;
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
    let chosen = all.iter().position(|m| *m == mode).unwrap_or(0);
    let game_owned = game.to_owned();
    let pick = all.clone();
    let flags = all.iter().map(|m| *m != mode && unsupported(*m, game).is_some()).collect();
    let control = Segmented::new(tr("Wie die Spiele laufen"), segments, chosen).unavailable(flags).disabled(lock.clone()).on_choose(move |i| {
        if let Some(m) = pick.get(i)
            && (*m == state.peek().mode || unsupported(*m, &game_owned).is_none())
        {
            state.write().mode = *m;
        }
    });
    let line = Line::new("batch.mode.row", tr("Wie die Spiele laufen")).control(control);
    if let Some(why) = unsupported(mode, game) {
        return line.hint(why).hint_tone(t.danger_text);
    }
    if mode == RunMode::Connection && proxied > 0 {
        let warning = if proxied == 1 { tr("Ein Konto mit Proxy startet in diesem Modus noch nicht. Nimm den Proxy weg oder wähle „Fenster“.") } else { trf("%lld Konten mit Proxy starten in diesem Modus noch nicht. Nimm die Proxys weg oder wähle „Fenster“.", &[&proxied]) };
        return line.hint(warning).hint_tone(t.attention_text);
    }
    line.hint(sentence(mode))
}

/// The screenshot interval of hidden games.
pub fn screenshots_line(mut state: State<Unattended>, lock: &Option<String>) -> Line {
    let u = *state.read();
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
    let picker = Select::new("batch.screenshots", tr("Screenshots"), labels, at).width(frame::PICK_W).disabled(lock.clone()).on_choose(move |i| {
        if let Some(c) = choices.get(i) {
            state.write().screenshots = *c;
        }
    });
    Line::new("batch.screenshots.row", tr("Screenshots"))
        .hint(tr("Beim Betreten des Servers kommt einer von selbst, weitere über die Kachel. Jeder Screenshot kostet kurz Leistung."))
        .control(picker)
}

fn packs_offered(mode: RunMode) -> Vec<Packs> {
    Packs::ALL.iter().copied().filter(|p| mode != RunMode::Connection || *p != Packs::Ask).collect()
}

/// What the games answer when a server offers a resource pack. Headless games cannot ask, so they only accept or decline.
pub fn packs_line(mut state: State<Unattended>, running: bool, lock: &Option<String>) -> Line {
    let u = *state.read();
    let offered = packs_offered(u.mode);
    let shown = if offered.contains(&u.packs) { u.packs } else { Packs::Accept };
    let name = |p: Packs| match p {
        Packs::Ask => tr("Fragen"),
        Packs::Accept => tr("Annehmen"),
        Packs::Decline => tr("Ablehnen"),
    };
    let segments = offered
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
    let chosen = offered.iter().position(|p| *p == shown).unwrap_or(0);
    let pick = offered.clone();
    let control = Segmented::new(tr("Ressourcenpakete vom Server"), segments, chosen).disabled(lock.clone()).on_choose(move |n| {
        if let Some(p) = pick.get(n) {
            state.write().packs = *p;
        }
    });
    let text = match shown {
        Packs::Ask => tr("Ein offenes Spiel fragt wie sonst. Ein Spiel im Hintergrund wartet, bis du in seiner Kachel antwortest."),
        Packs::Accept => tr("Jedes Paket, das ein Server anbietet, wird geladen."),
        Packs::Decline => tr("Jedes Paket wird abgelehnt. Verlangt der Server eins, trennt er die Verbindung."),
    };
    let text = if running { format!("{text} {}", tr("Gilt sofort.")) } else { text };
    Line::new("batch.packs.row", tr("Ressourcenpakete vom Server")).hint(text).control(control)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_games_never_offer_to_ask_about_packs() {
        assert_eq!(packs_offered(RunMode::Connection), vec![Packs::Accept, Packs::Decline]);
        assert_eq!(packs_offered(RunMode::Windows), Packs::ALL.to_vec());
        assert_eq!(packs_offered(RunMode::Hidden), Packs::ALL.to_vec());
    }
}
