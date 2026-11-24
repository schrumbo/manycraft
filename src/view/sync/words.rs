//! The plain words of the Sync page: what each item is, what an instance does with it now and when that changes.

use crate::core::i18n::{self, tr, trf};
use crate::core::share::{ITEMS, Item, Sharing, options_travel};

/// The name of `item`.
pub fn label(item: Item) -> String {
    match item {
        Item::Options => tr("Spiel-Optionen"),
        Item::Config => tr("Mod-Einstellungen"),
        Item::Servers => tr("Serverliste"),
    }
}

/// What `item` holds, in a sentence.
pub fn blurb(item: Item) -> String {
    match item {
        Item::Options => tr("Grafik, Steuerung und Ton. Fenster und Ressourcenpakete bleiben pro Instanz."),
        Item::Config => tr("Der Ordner „config“ aller Mods."),
        Item::Servers => tr("Die Liste unter Mehrspieler."),
    }
}

/// The names of `items` as a list: "a", "a und b", "a, b und c".
pub fn names(items: &[Item]) -> String {
    let all: Vec<String> = items.iter().map(|i| label(*i)).collect();
    match all.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => trf("%@ und %@", &[&init.join(", "), last]),
    }
}

/// What is synchronized of `sync`: options only count for a game version whose options can travel.
pub fn effective(sync: Sharing, game_version: &str) -> Sharing {
    Sharing { options: sync.options && options_travel(game_version), ..sync }
}

/// What an instance with `sync` and game `game_version` does now, with a note when a game runs.
pub fn instance_line(sync: Sharing, game_version: &str, running: bool) -> String {
    let now = effective(sync, game_version);
    let on: Vec<Item> = ITEMS.into_iter().filter(|i| now.get(*i)).collect();
    let off: Vec<Item> = ITEMS.into_iter().filter(|i| !now.get(*i) && (*i != Item::Options || options_travel(game_version))).collect();
    let mut line = if on.is_empty() {
        tr("Synchronisiert nichts. Alle Dateien gehören dieser Instanz allein.")
    } else if off.is_empty() {
        trf("Synchronisiert %@.", &[&names(&on)])
    } else {
        trf("Synchronisiert %@. Eigene Dateien: %@.", &[&names(&on), &names(&off)])
    };
    if !options_travel(game_version) {
        line = format!("{line} {}", tr("Spiel-Optionen erst ab Minecraft 1.13."));
    }
    if running {
        line = format!("{line} {}", tr("Ein Spiel läuft: Umstellungen gelten ab dem nächsten Start."));
    }
    line
}

/// A moment in milliseconds since 1970 (UTC) as the person's local date and time.
pub fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| i18n::date_time(d.with_timezone(&chrono::Local).naive_local())).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_read_as_a_list() {
        assert_eq!(names(&[Item::Options]), "Spiel-Optionen");
        assert_eq!(names(&[Item::Options, Item::Servers]), "Spiel-Optionen und Serverliste");
        assert_eq!(names(&ITEMS), "Spiel-Optionen, Mod-Einstellungen und Serverliste");
    }

    #[test]
    fn old_games_never_count_options() {
        assert_eq!(effective(Sharing::ALL, "1.8.9"), Sharing { options: false, config: true, servers: true });
        assert_eq!(effective(Sharing::ALL, "26.3"), Sharing::ALL);
    }

    #[test]
    fn the_line_says_what_is_shared_what_is_not_and_what_waits() {
        assert_eq!(instance_line(Sharing::ALL, "26.3", false), "Synchronisiert Spiel-Optionen, Mod-Einstellungen und Serverliste.");
        assert_eq!(instance_line(Sharing::NONE, "26.3", false), "Synchronisiert nichts. Alle Dateien gehören dieser Instanz allein.");
        let part = Sharing { options: true, config: false, servers: true };
        assert_eq!(instance_line(part, "26.3", false), "Synchronisiert Spiel-Optionen und Serverliste. Eigene Dateien: Mod-Einstellungen.");
        assert!(instance_line(Sharing::ALL, "26.3", true).ends_with("gelten ab dem nächsten Start."));
        assert!(instance_line(Sharing::ALL, "1.8.9", false).contains("erst ab Minecraft 1.13"));
        assert!(!instance_line(Sharing::ALL, "1.8.9", false).contains("Eigene Dateien: Spiel-Optionen"));
    }
}
