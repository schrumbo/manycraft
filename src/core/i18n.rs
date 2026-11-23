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

impl Lang {
    /// Every language the UI speaks; each one except German needs its own file in `assets/i18n`.
    pub const ALL: [Lang; 2] = [Lang::De, Lang::En];

    /// The language code used in settings and as the catalog file name.
    pub const fn code(self) -> &'static str {
        match self {
            Lang::De => "de",
            Lang::En => "en",
        }
    }
}

/// Whether `code` names a language the UI speaks.
pub fn supported(code: &str) -> bool {
    Lang::ALL.iter().any(|l| l.code() == code)
}

/// Picks the UI language: `setting` (a language code), else the system language, falling back to German.
pub fn init(setting: &str) {
    let code = if setting.is_empty() { sys_locale::get_locale().unwrap_or_default() } else { setting.to_string() };
    *LANG.write().unwrap_or_else(|e| e.into_inner()) = Lang::ALL.into_iter().find(|l| code.starts_with(l.code())).unwrap_or(Lang::De);
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
    render(lang(), key, args)
}

fn render(lang: Lang, key: &str, args: &[&dyn std::fmt::Display]) -> String {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let template = match (lang, EN.get(key)) {
        (Lang::En, Some(Entry::One(s))) => s.clone(),
        (Lang::En, Some(Entry::Plural(forms))) => {
            let one = args.iter().find_map(|a| a.parse::<i64>().ok()) == Some(1);
            forms.get(if one { "one" } else { "other" }).or_else(|| forms.get("other")).cloned().unwrap_or_else(|| key.to_string())
        }
        _ => key.to_string(),
    };
    fill(&template, &args)
}

/// A number written with a point as decimal mark, in the UI language's mark.
pub fn decimal(text: String) -> String {
    decimal_in(lang(), text)
}

fn decimal_in(lang: Lang, text: String) -> String {
    if lang == Lang::De { text.replace('.', ",") } else { text }
}

/// A time of day: `14:05`, or `2:05 PM` in English.
pub fn clock(at: chrono::NaiveTime) -> String {
    clock_in(lang(), at)
}

fn clock_in(lang: Lang, at: chrono::NaiveTime) -> String {
    at.format(if lang == Lang::En { "%-I:%M %p" } else { "%H:%M" }).to_string()
}

/// A calendar day: `05.03.2026`, or `Mar 5, 2026` in English; without the year `05.03.` and `Mar 5`.
pub fn date(at: chrono::NaiveDate, with_year: bool) -> String {
    date_in(lang(), at, with_year)
}

fn date_in(lang: Lang, at: chrono::NaiveDate, with_year: bool) -> String {
    let pattern = match (lang, with_year) {
        (Lang::En, true) => "%b %-d, %Y",
        (Lang::En, false) => "%b %-d",
        (Lang::De, true) => "%d.%m.%Y",
        (Lang::De, false) => "%d.%m.",
    };
    at.format(pattern).to_string()
}

/// A day and time of day without the year: `05.03. 14:05`, or `Mar 5, 2:05 PM` in English.
pub fn date_time(at: chrono::NaiveDateTime) -> String {
    date_time_in(lang(), at)
}

fn date_time_in(lang: Lang, at: chrono::NaiveDateTime) -> String {
    let joint = if lang == Lang::En { ", " } else { " " };
    format!("{}{joint}{}", date_in(lang, at.date(), false), clock_in(lang, at.time()))
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
#[path = "i18n/audit.rs"]
mod audit;

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, NaiveTime};

    use super::*;

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

    #[test]
    fn dates_numbers_and_plurals_follow_the_language() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap_or_default();
        let time = NaiveTime::from_hms_opt(14, 5, 0).unwrap_or_default();
        let at = day.and_time(time);
        assert_eq!((date_in(Lang::De, day, true), date_in(Lang::De, day, false), clock_in(Lang::De, time), decimal_in(Lang::De, "2.5".into())), ("05.03.2026".into(), "05.03.".into(), "14:05".into(), "2,5".into()));
        assert_eq!(date_time_in(Lang::De, at), "05.03. 14:05");
        assert_eq!((date_in(Lang::En, day, true), date_in(Lang::En, day, false), clock_in(Lang::En, time), decimal_in(Lang::En, "2.5".into())), ("Mar 5, 2026".into(), "Mar 5".into(), "2:05 PM".into(), "2.5".into()));
        assert_eq!(date_time_in(Lang::En, at), "Mar 5, 2:05 PM");
        assert_eq!(render(Lang::En, "%lld Mods", &[&1]), "1 mod");
        assert_eq!(render(Lang::En, "%lld Mods", &[&3]), "3 mods");
        assert_eq!(render(Lang::De, "%lld Mods", &[&3]), "3 Mods");
    }
}