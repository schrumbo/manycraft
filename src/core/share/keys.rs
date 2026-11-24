//! `options.txt` as key-value lines: which keys the sync folder holds, merging it into an instance and reading back.

use std::collections::BTreeMap;

use crate::core::sync::PER_INSTANCE_KEYS;

/// Keys the sync folder never holds: the ones every game keeps for itself, the data version and the pack lists, which
/// name files only one instance has.
pub(super) fn excluded(key: &str) -> bool {
    PER_INSTANCE_KEYS.contains(&key) || matches!(key, "version" | "resourcePacks" | "incompatibleResourcePacks")
}

/// Whether `key` and `value` can stand on one line of `options.txt` as the game writes it.
pub(super) fn valid(key: &str, value: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| !c.is_control() && !c.is_whitespace() && c != ':') && !value.contains(['\n', '\r', '\0'])
}

/// The well-formed `key:value` lines of `text`, the last one winning for a key that repeats.
pub(super) fn map(text: &str) -> BTreeMap<String, String> {
    text.lines().filter_map(|l| l.split_once(':')).filter(|(k, v)| valid(k, v)).map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
}

/// Whether `text` reads as an `options.txt`: at least one well-formed line and nothing binary.
pub(super) fn readable(text: &str) -> bool {
    !text.contains('\0') && !map(text).is_empty()
}

/// The lines of `text` the sync folder holds.
pub(super) fn shared(text: &str) -> String {
    lines(map(text).into_iter().filter(|(k, _)| !excluded(k)))
}

/// `key:value` lines, one per entry.
pub(super) fn lines(entries: impl IntoIterator<Item = (String, String)>) -> String {
    entries.into_iter().map(|(k, v)| format!("{k}:{v}\n")).collect()
}

/// The entries of `changes` the sync folder may take and that differ from `now`.
pub(super) fn upward(changes: &BTreeMap<String, String>, now: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    changes.iter().filter(|(k, v)| valid(k, v) && !excluded(k) && now.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// The keys of `after` whose value changed against `before`. A key only one side has is no change: one the game adds
/// after loading is a default it filled in, not a choice.
pub(super) fn changed(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    after.iter().filter(|(k, v)| before.get(*k).is_some_and(|b| b != *v)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// `text` with every key in `changes` set: lines replaced in place, new keys appended.
pub(super) fn apply(text: &str, changes: &BTreeMap<String, String>) -> String {
    let mut seen = Vec::new();
    let mut out: Vec<String> = text
        .lines()
        .map(|l| match l.split_once(':').and_then(|(k, _)| changes.get_key_value(k)) {
            Some((k, v)) => {
                seen.push(k.as_str());
                format!("{k}:{v}")
            }
            None => l.to_owned(),
        })
        .collect();
    out.extend(changes.iter().filter(|(k, _)| !seen.contains(&k.as_str())).map(|(k, v)| format!("{k}:{v}")));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// `instance` with every key the sync folder `mother` holds set to its value; keys only the instance has stay.
pub(super) fn merge(instance: &str, mother: &str) -> String {
    let theirs: BTreeMap<String, String> = map(mother).into_iter().filter(|(k, _)| !excluded(k)).collect();
    let changes = upward(&theirs, &map(instance));
    if changes.is_empty() { instance.to_owned() } else { apply(instance, &changes) }
}

/// Whether every entry of `wanted` is in `text` with the same value.
pub(super) fn covers(text: &str, wanted: &BTreeMap<String, String>) -> bool {
    let now = map(text);
    wanted.iter().all(|(k, v)| now.get(k) == Some(v))
}

/// The data version `text` names, when it is a number.
/// Data version of 17w43a, the first game that names keys the way later ones do.
const FLATTENED: u32 = 1444;

/// Whether `text` is an `options.txt` written before 1.13: an older data version, or keys bound by number.
pub(super) fn legacy(text: &str) -> bool {
    let map = map(text);
    map.get("version").and_then(|v| v.trim().parse::<u32>().ok()).is_some_and(|v| v < FLATTENED) || map.iter().any(|(k, v)| k.starts_with("key_") && v.trim().parse::<i32>().is_ok())
}

pub(super) fn version(text: &str) -> Option<u32> {
    map(text).get("version").and_then(|v| v.trim().parse().ok())
}

/// The game's `options.txt` from the instance `template` over the game's `existing` file: template keys win but for
/// the per-game ones, keys only the game has stay, and the file keeps a data version: its own, the template's, else `fallback`.
pub(super) fn game_file(template: &str, existing: Option<&str>, fallback: Option<u32>) -> String {
    let existing = existing.filter(|t| readable(t)).unwrap_or_default();
    let ours = map(template);
    let theirs = map(existing);
    let mut out: Vec<String> = template
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            valid(k, v).then(|| match theirs.get(k).filter(|_| PER_INSTANCE_KEYS.contains(&k)) {
                Some(own) => format!("{k}:{own}"),
                None => l.to_owned(),
            })
        })
        .collect();
    out.extend(existing.lines().filter_map(|l| l.split_once(':')).filter(|(k, v)| valid(k, v) && !ours.contains_key(*k)).map(|(k, v)| format!("{k}:{v}")));
    out.retain(|l| l.split_once(':').is_none_or(|(k, _)| k != "version"));
    if let Some(known) = version(existing).or_else(|| version(template)).or(fallback) {
        out.insert(0, format!("version:{known}"));
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn the_sync_folder_never_holds_per_game_version_or_pack_keys() {
        let text = "version:4000\nfov:0.5\nfullscreen:true\nresourcePacks:[\"x\"]\nincompatibleResourcePacks:[]\nlastServer:a\nkey_key.jump:key.keyboard.space\n";
        assert_eq!(shared(text), "fov:0.5\nkey_key.jump:key.keyboard.space\n");
    }

    #[test]
    fn merge_sets_mother_keys_and_keeps_the_rest() {
        let out = merge("version:3955\nfov:0.0\nfullscreen:true\nown:1\n", "fov:0.5\nfullscreen:false\nversion:4000\nnew:2\n");
        assert_eq!(out, "version:3955\nfov:0.5\nfullscreen:true\nown:1\nnew:2\n");
        assert_eq!(merge("fov:0.5\n", "fov:0.5\n"), "fov:0.5\n");
    }

    #[test]
    fn changed_ignores_removed_keys() {
        assert_eq!(changed(&m(&[("a", "1"), ("b", "2"), ("gone", "x")]), &m(&[("a", "1"), ("b", "3"), ("new", "4")])), m(&[("b", "3")]));
    }

    #[test]
    fn garbage_is_not_an_options_file() {
        assert!(!readable("\u{0}\u{1}binary"));
        assert!(!readable("no colon here\nnor here"));
        assert!(readable("fov:0.5\n"));
    }

    #[test]
    fn a_rebuilt_game_file_keeps_a_version() {
        assert_eq!(game_file("fov:0.5\n", None, Some(4440)), "version:4440\nfov:0.5\n");
        assert_eq!(game_file("fov:0.5\n", Some("garbage"), Some(4440)), "version:4440\nfov:0.5\n");
        assert_eq!(game_file("fov:0.5\n", Some("version:3955\nfullscreen:true\nfov:0.1\n"), Some(4440)), "version:3955\nfov:0.5\nfullscreen:true\n");
        assert_eq!(game_file("version:3700\nfullscreen:false\nfov:0.5\n", Some("fullscreen:true\n"), None), "version:3700\nfullscreen:true\nfov:0.5\n");
        assert_eq!(game_file("fov:0.5\n", None, None), "fov:0.5\n");
    }
}