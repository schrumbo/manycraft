use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use serde::Deserialize;

/// A translation: one string, or plural forms keyed by CLDR category.
#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    One(String),
    Plural(HashMap<String, String>),
}

static EN: LazyLock<HashMap<String, Entry>> = LazyLock::new(|| serde_json::from_str(include_str!("../../assets/i18n/en.json")).unwrap_or_default());
static LANG: RwLock<Lang> = RwLock::new(Lang::De);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

/// Picks the UI language: `setting` (`de` or `en`), else the system language, falling back to German.
pub fn init(setting: &str) {
    let code = if setting.is_empty() { sys_locale::get_locale().unwrap_or_default() } else { setting.to_string() };
    *LANG.write().unwrap_or_else(|e| e.into_inner()) = if code.starts_with("en") { Lang::En } else { Lang::De };
}

/// The language the UI speaks.
pub fn lang() -> Lang {
    *LANG.read().unwrap_or_else(|e| e.into_inner())
}

/// Translates a German source string; unknown keys and German stay as written.
pub fn tr(key: &str) -> String {
    trf(key, &[])
}

/// Translates a German source string whose English text depends on `context`, kept in the English file as `key (context)`.
pub fn trc(context: &str, key: &str) -> String {
    match lang() {
        Lang::En => tr(&format!("{key} ({context})")),
        Lang::De => key.to_string(),
    }
}

/// Translates and fills `%@` / `%lld` / `%1$@` placeholders in order.
pub fn trf(key: &str, args: &[&dyn std::fmt::Display]) -> String {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let template = match (lang(), EN.get(key)) {
        (Lang::En, Some(Entry::One(s))) => s.clone(),
        (Lang::En, Some(Entry::Plural(forms))) => {
            let one = args.iter().find_map(|a| a.parse::<i64>().ok()) == Some(1);
            forms.get(if one { "one" } else { "other" }).or_else(|| forms.get("other")).cloned().unwrap_or_else(|| key.to_string())
        }
        _ => key.to_string(),
    };
    fill(&template, &args)
}

fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut next = 0;
    let mut rest = template;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('%') {
            out.push('%');
            rest = r;
            continue;
        }
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let positional = rest[digits.len()..].strip_prefix('$').filter(|_| !digits.is_empty());
        let after = positional.unwrap_or(rest);
        let Some(spec) = ["@", "lld", "ld", "d", ".1f", ".2f", "f"].iter().find(|s| after.starts_with(**s)) else {
            out.push('%');
            continue;
        };
        let index = if positional.is_some() {
            digits.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
        } else {
            next += 1;
            Some(next - 1)
        };
        out.push_str(index.and_then(|i| args.get(i)).map_or("", String::as_str));
        rest = &after[spec.len()..];
    }
    out + rest
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn literal_after(source: &str, start: usize) -> Option<(String, usize)> {
        let mut out = String::new();
        let mut chars = source[start..].char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return Some((out, start + i + 1)),
                '\\' => match chars.next()?.1 {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    }

    fn keys_in(source: &str, found: &mut Vec<String>) {
        let mut from = 0;
        while let Some(at) = source[from..].find("trc(\"") {
            from += at + "trc(\"".len();
            let Some((context, end)) = literal_after(source, from) else { continue };
            from = end;
            let Some(open) = source[from..].find('"').filter(|i| source[from..from + i].trim_start_matches(',').trim().is_empty()) else { continue };
            if let Some((key, end)) = literal_after(source, from + open + 1) {
                found.push(format!("{key} ({context})"));
                from = end;
            }
        }
        for call in ["tr(\"", "trf(\""] {
            let mut from = 0;
            while let Some(at) = source[from..].find(call) {
                let start = from + at;
                from = start + call.len();
                let ident = source[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
                if ident {
                    continue;
                }
                if let Some((key, end)) = literal_after(source, from) {
                    found.push(key);
                    from = end;
                }
            }
        }
    }

    fn collect(dir: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") && path.file_name().is_some_and(|n| n != "i18n.rs") {
                keys_in(&std::fs::read_to_string(&path).unwrap(), found);
            }
        }
    }

    #[test]
    fn every_german_key_has_an_english_text() {
        let mut keys = Vec::new();
        collect(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut keys);
        let missing: Vec<&String> = keys.iter().filter(|k| !EN.contains_key(k.as_str())).collect();
        assert!(missing.is_empty(), "add to assets/i18n/en.json: {missing:?}");
    }

    #[test]
    fn english_file_parses_and_has_no_empty_texts() {
        assert!(!EN.is_empty());
        for (key, entry) in EN.iter() {
            let empty = match entry {
                Entry::One(s) => s.is_empty(),
                Entry::Plural(forms) => forms.values().any(String::is_empty),
            };
            assert!(!empty, "empty English text for {key:?}");
        }
    }

    #[test]
    fn scanner_reads_escaped_keys() {
        let mut found = Vec::new();
        keys_in("a(tr(\"Sag \\\"Hallo\\\"\")); mytr(\"nein\"); trf(\"%@ Konten\", &[&1]); trc(\"Ort\", \"Neu\")", &mut found);
        assert_eq!(found, ["Neu (Ort)", "Sag \"Hallo\"", "%@ Konten"]);
    }

    #[test]
    fn keeps_plain_percent_signs() {
        assert_eq!(fill("100%", &[]), "100%");
        assert_eq!(fill("%%", &[]), "%");
        assert_eq!(fill("%@ von 60 %", &["5".into()]), "5 von 60 %");
    }

    #[test]
    fn fills_placeholders() {
        assert_eq!(fill("in %@", &["2 Tagen".into()]), "in 2 Tagen");
        assert_eq!(fill("%2$@ / %1$@", &["a".into(), "b".into()]), "b / a");
        assert_eq!(fill("%lld von %lld", &["1".into(), "3".into()]), "1 von 3");
    }
}
