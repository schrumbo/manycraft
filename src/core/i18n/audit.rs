//! Source audit: every UI text reaches the catalogs, and nothing in the catalogs is dead.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::{Entry, Lang};

const TEXT_CALLS: &[&str] = &[
    "line", "body", "secondary", "caption", "strong", "title", "heading", "para", "figure", "mono", "badge", "waiting", "status", "group", "titled", "show", "show_kept", "show_action", "confirm", "tip", "detail", "shortcut", "searchable", "message", "hint",
    "placeholder", "label", "unit", "shown", "error", "with_tooltip", "with_title", "tooltip", "toast", "sub", "a11y_alt", "text", "disabled", "locked", "pick_files", "save_file", "add_filter", "set_title",
];
const TEXT_TYPES: &[&str] = &["Button", "Chip", "Switch", "Stepper", "Select", "Segmented", "Sheet", "Row", "Item", "Line", "Field", "Drop", "Segment", "Tooltip"];
const NOT_TRANSLATED: &[&str] = &[
    "Manycraft", "Modrinth", "Minecraft", "Fabric", "Quilt", "Forge", "NeoForge", "Iris", "Prism", "Telegram", "Discord", "OpenGL", "Vulkan", "Deutsch", "English", "Alt", "Win", "Microsoft", "Java", "-XX:+UseG1GC",
];
const GERMAN_WORDS: &[&str] = &[
    "der", "die", "das", "und", "nicht", "ist", "wird", "für", "mit", "kein", "keine", "oder", "noch", "bitte", "wurde", "werden", "sind", "bei", "nach", "über", "zum", "zur", "dem", "den", "ein", "eine", "auf",
];
const LOCALE_PATTERN_PARTS: &[&str] = &["%H", "%I", "%b", "%B", "%a", "%A", "%d", "%e", "%-d", "%-m", "%p", "%x", "%c", "%r"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Str,
    Char,
    Ident,
    Punct,
}

#[derive(Clone, Debug)]
struct Tok {
    kind: Kind,
    text: String,
    line: usize,
}

impl Tok {
    fn is(&self, kind: Kind, text: &str) -> bool {
        self.kind == kind && self.text == text
    }
}

fn starts(c: &[char], at: usize, text: &str) -> bool {
    text.chars().enumerate().all(|(i, t)| c.get(at + i) == Some(&t))
}

fn read_string(c: &[char], mut i: usize, line: &mut usize) -> (String, usize) {
    let mut out = String::new();
    while i < c.len() && c[i] != '"' {
        if c[i] != '\\' {
            if c[i] == '\n' {
                *line += 1;
            }
            out.push(c[i]);
            i += 1;
            continue;
        }
        let next = c.get(i + 1).copied().unwrap_or('"');
        i += 2;
        match next {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '\n' => {
                *line += 1;
                while c.get(i).is_some_and(|w| w.is_whitespace()) {
                    if c[i] == '\n' {
                        *line += 1;
                    }
                    i += 1;
                }
            }
            'u' => {
                let end = c[i..].iter().position(|&x| x == '}').map_or(c.len(), |p| i + p);
                let digits: String = c[(i + 1).min(end)..end].iter().collect();
                out.extend(u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32));
                i = end + 1;
            }
            'x' => {
                let digits: String = c[i..(i + 2).min(c.len())].iter().collect();
                out.extend(u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32));
                i += 2;
            }
            other => out.push(other),
        }
    }
    (out, i + 1)
}

fn lex(source: &str) -> Vec<Tok> {
    let c: Vec<char> = source.chars().collect();
    let (mut i, mut line, mut out) = (0, 1, Vec::new());
    while i < c.len() {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if starts(&c, i, "//") {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if starts(&c, i, "/*") {
            let mut depth = 0;
            while i < c.len() {
                if starts(&c, i, "/*") {
                    depth += 1;
                    i += 2;
                } else if starts(&c, i, "*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if c[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if let Some((hashes, open)) = raw_open(&c, i) {
            let close: String = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
            let end = (open..c.len()).find(|&j| starts(&c, j, &close)).unwrap_or(c.len());
            let text: String = c[open..end].iter().collect();
            out.push(Tok { kind: Kind::Str, text: text.clone(), line });
            line += text.matches('\n').count();
            i = end + close.len();
        } else if ch == '"' || (ch == 'b' && c.get(i + 1) == Some(&'"')) {
            let start_line = line;
            let (text, next) = read_string(&c, i + if ch == 'b' { 2 } else { 1 }, &mut line);
            out.push(Tok { kind: Kind::Str, text, line: start_line });
            i = next;
        } else if ch == '\'' {
            let end = if c.get(i + 1) == Some(&'\\') {
                (i + 2..c.len()).find(|&j| c[j] == '\'')
            } else {
                Some(i + 2).filter(|&j| c.get(j) == Some(&'\''))
            };
            if let Some(end) = end {
                out.push(Tok { kind: Kind::Char, text: c[i + 1..end].iter().collect(), line });
                i = end + 1;
            } else {
                i += 1;
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let end = (i..c.len()).find(|&j| !(c[j].is_alphanumeric() || c[j] == '_')).unwrap_or(c.len());
            out.push(Tok { kind: Kind::Ident, text: c[i..end].iter().collect(), line });
            i = end;
        } else {
            out.push(Tok { kind: Kind::Punct, text: ch.to_string(), line });
            i += 1;
        }
    }
    out
}

fn raw_open(c: &[char], i: usize) -> Option<(usize, usize)> {
    let mut at = i;
    if c.get(at) == Some(&'b') {
        at += 1;
    }
    if c.get(at) != Some(&'r') {
        return None;
    }
    at += 1;
    let hashes = c[at..].iter().take_while(|&&x| x == '#').count();
    (c.get(at + hashes) == Some(&'"')).then_some((hashes, at + hashes + 1))
}

fn production(source: &str) -> &str {
    let mut from = 0;
    while let Some(at) = source[from..].find("#[cfg(test)]") {
        let start = from + at;
        let rest = source[start + "#[cfg(test)]".len()..].trim_start();
        if rest.starts_with("mod ") {
            return &source[..start];
        }
        from = start + 1;
    }
    source
}

struct Source {
    path: String,
    toks: Vec<Tok>,
}

fn sources(dir: &Path, out: &mut Vec<Source>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.is_dir() {
            if name != "i18n" && name != "testdata" {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "i18n.rs" && name != "tests.rs" {
            let text = std::fs::read_to_string(&path).unwrap();
            let rel = path.strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push(Source { path: rel, toks: lex(production(&text)) });
        }
    }
}

fn all_sources() -> Vec<Source> {
    let mut out = Vec::new();
    sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

fn call_args(toks: &[Tok], open: usize) -> Vec<&[Tok]> {
    let (mut depth, mut args, mut from) = (0, Vec::new(), open + 1);
    for (j, t) in toks.iter().enumerate().skip(open) {
        if t.kind != Kind::Punct {
            continue;
        }
        match t.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if depth == 0 {
                    args.push(&toks[from..j]);
                    return args;
                }
            }
            "," if depth == 1 => {
                args.push(&toks[from..j]);
                from = j + 1;
            }
            _ => {}
        }
    }
    args
}

#[derive(Debug, PartialEq, Eq)]
struct Use {
    path: String,
    line: usize,
    key: String,
}

fn translation_keys(sources: &[Source]) -> Vec<Use> {
    let mut found = Vec::new();
    for source in sources {
        let t = &source.toks;
        for (i, tok) in t.iter().enumerate() {
            let call = tok.kind == Kind::Ident && matches!(tok.text.as_str(), "tr" | "trf" | "trc") && t.get(i + 1).is_some_and(|n| n.is(Kind::Punct, "("));
            let declared = i > 0 && (t[i - 1].is(Kind::Ident, "fn") || t[i - 1].is(Kind::Punct, "."));
            if !call || declared {
                continue;
            }
            let args = call_args(t, i + 1);
            let strings = |arg: &[Tok]| -> Vec<(usize, String)> {
                if arg.iter().any(|a| a.is(Kind::Ident, "format")) {
                    return Vec::new();
                }
                arg.iter().filter(|a| a.kind == Kind::Str).map(|a| (a.line, a.text.clone())).collect()
            };
            if tok.text == "trc" {
                for (line, key) in args.get(1).map(|a| strings(a)).unwrap_or_default() {
                    for (_, context) in args.first().map(|a| strings(a)).unwrap_or_default() {
                        found.push(Use { path: source.path.clone(), line, key: format!("{key} ({context})") });
                    }
                }
            } else if let Some(first) = args.first() {
                found.extend(strings(first).into_iter().map(|(line, key)| Use { path: source.path.clone(), line, key }));
            }
        }
    }
    found
}

struct Pairs(Vec<(String, Entry)>);

impl<'de> Deserialize<'de> for Pairs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Walk;
        impl<'de> Visitor<'de> for Walk {
            type Value = Pairs;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map of texts")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs, A::Error> {
                let mut out = Vec::new();
                while let Some(pair) = map.next_entry::<String, Entry>()? {
                    out.push(pair);
                }
                Ok(Pairs(out))
            }
        }
        deserializer.deserialize_map(Walk)
    }
}

struct Catalog {
    code: String,
    entries: HashMap<String, Entry>,
    duplicates: Vec<String>,
}

fn catalogs() -> Vec<Catalog> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/i18n");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let Pairs(pairs) = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let mut entries = HashMap::new();
            let mut duplicates = Vec::new();
            for (key, entry) in pairs {
                if entries.insert(key.clone(), entry).is_some() {
                    duplicates.push(key);
                }
            }
            Catalog { code: path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned(), entries, duplicates }
        })
        .collect()
}

fn placeholders(text: &str) -> Vec<(usize, &'static str)> {
    let (mut out, mut next, mut rest) = (Vec::new(), 1, text);
    while let Some(i) = rest.find('%') {
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('%') {
            rest = r;
            continue;
        }
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let positional = rest[digits.len()..].strip_prefix('$').filter(|_| !digits.is_empty());
        let after = positional.unwrap_or(rest);
        let Some((spec, kind)) = [("@", "text"), ("lld", "int"), ("ld", "int"), ("d", "int"), (".1f", "real"), (".2f", "real"), ("f", "real")].into_iter().find(|(s, _)| after.starts_with(s)) else { continue };
        let index = positional.map_or_else(
            || {
                next += 1;
                next - 1
            },
            |_| digits.parse().unwrap_or(0),
        );
        out.push((index, kind));
        rest = &after[spec.len()..];
    }
    out.sort_unstable();
    out
}

fn prose(text: &str) -> bool {
    let mut letters = false;
    let mut depth = 0;
    for ch in text.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            c if depth == 0 && c.is_alphabetic() => letters = true,
            _ => {}
        }
    }
    let mut bare = String::new();
    depth = 0;
    for ch in text.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            c if depth == 0 => bare.push(c),
            _ => {}
        }
    }
    let machine = bare.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.:/-+*#$[]=<>@%&,;".contains(c));
    letters && !machine && !NOT_TRANSLATED.contains(&text)
}

fn german(text: &str) -> bool {
    text.chars().any(|c| "äöüßÄÖÜ".contains(c)) || text.to_lowercase().split(|c: char| !c.is_alphabetic()).any(|w| GERMAN_WORDS.contains(&w))
}

fn leading_literal(arg: &[Tok]) -> Option<&Tok> {
    let mut a = arg;
    loop {
        match a {
            [t, rest @ ..] if t.is(Kind::Punct, "&") => a = rest,
            [t, p, rest @ ..] if t.is(Kind::Ident, "Some") && p.is(Kind::Punct, "(") => a = rest,
            [t, b, p, rest @ ..] if t.is(Kind::Ident, "format") && b.is(Kind::Punct, "!") && p.is(Kind::Punct, "(") => a = rest,
            [t, c1, c2, f, p, rest @ ..] if t.is(Kind::Ident, "String") && c1.is(Kind::Punct, ":") && c2.is(Kind::Punct, ":") && f.is(Kind::Ident, "from") && p.is(Kind::Punct, "(") => a = rest,
            [t, ..] if t.kind == Kind::Str => return Some(t),
            _ => return None,
        }
    }
}

fn live_ui(path: &str) -> bool {
    path.starts_with("src/view/") || path.starts_with("src/app/")
}

fn missing(uses: &[Use], catalog: &Catalog) -> Vec<String> {
    let mut out: Vec<String> = uses.iter().filter(|u| !catalog.entries.contains_key(&u.key)).map(|u| format!("{}.json lacks {:?} ({}:{})", catalog.code, u.key, u.path, u.line)).collect();
    out.dedup();
    out
}

fn unused(sources: &[Source], uses: &[Use], catalog: &Catalog) -> Vec<String> {
    let mut seen: HashSet<&str> = uses.iter().map(|u| u.key.as_str()).collect();
    seen.extend(sources.iter().flat_map(|s| s.toks.iter()).filter(|t| t.kind == Kind::Str).map(|t| t.text.as_str()));
    let mut out: Vec<String> = catalog.entries.keys().filter(|k| !seen.contains(k.as_str())).map(|k| format!("{}.json has an unused entry {k:?}", catalog.code)).collect();
    out.sort();
    out
}

fn mismatched(catalog: &Catalog) -> Vec<String> {
    let mut out = Vec::new();
    for (key, entry) in &catalog.entries {
        let wanted = placeholders(key);
        match entry {
            Entry::One(text) => {
                if placeholders(text) != wanted {
                    out.push(format!("{}.json: placeholders differ in {key:?} -> {text:?}", catalog.code));
                }
            }
            Entry::Plural(forms) => {
                for (form, text) in forms {
                    let fine = match form.as_str() {
                        "other" => placeholders(text) == wanted,
                        "one" => placeholders(text).iter().all(|p| wanted.contains(p)),
                        _ => false,
                    };
                    if !fine {
                        out.push(format!("{}.json: bad {form:?} form of {key:?} -> {text:?}", catalog.code));
                    }
                }
                if !forms.contains_key("other") {
                    out.push(format!("{}.json: {key:?} has no \"other\" form", catalog.code));
                }
            }
        }
    }
    out.sort();
    out
}

fn untranslated_calls(sources: &[Source]) -> Vec<String> {
    let mut out = Vec::new();
    for source in sources.iter().filter(|s| live_ui(&s.path)) {
        let t = &source.toks;
        for (i, tok) in t.iter().enumerate() {
            if tok.kind != Kind::Ident || !t.get(i + 1).is_some_and(|n| n.is(Kind::Punct, "(")) || (i > 0 && t[i - 1].is(Kind::Ident, "fn")) {
                continue;
            }
            let typed = i >= 3 && t[i - 1].is(Kind::Punct, ":") && t[i - 2].is(Kind::Punct, ":") && TEXT_TYPES.contains(&t[i - 3].text.as_str()) && matches!(tok.text.as_str(), "new" | "icon");
            if !typed && !TEXT_CALLS.contains(&tok.text.as_str()) {
                continue;
            }
            for arg in call_args(t, i + 1) {
                if let Some(lit) = leading_literal(arg).filter(|l| prose(&l.text)) {
                    out.push(format!("{}:{} {}({:?}) is shown without tr()", source.path, lit.line, tok.text, lit.text));
                }
            }
        }
    }
    out
}

fn german_strays(sources: &[Source], keys: &HashSet<&str>) -> Vec<String> {
    let mut out = Vec::new();
    for source in sources.iter().filter(|s| live_ui(&s.path)) {
        for tok in source.toks.iter().filter(|t| t.kind == Kind::Str && german(&t.text) && !keys.contains(t.text.as_str()) && prose(&t.text)) {
            out.push(format!("{}:{} German text without an English entry: {:?}", source.path, tok.line, tok.text));
        }
    }
    out
}

fn local_formats(sources: &[Source]) -> Vec<String> {
    let mut out = Vec::new();
    for source in sources.iter().filter(|s| !s.path.starts_with("src/ui/") && !s.path.starts_with("src/bin/")) {
        let t = &source.toks;
        for (i, tok) in t.iter().enumerate() {
            if tok.is(Kind::Ident, "format") && t.get(i + 1).is_some_and(|n| n.is(Kind::Punct, "(")) && i > 0 && t[i - 1].is(Kind::Punct, ".") {
                if let Some(pattern) = t.get(i + 2).filter(|p| p.kind == Kind::Str && LOCALE_PATTERN_PARTS.iter().any(|part| p.text.contains(part)) && !p.text.contains("%Y-%m")) {
                    out.push(format!("{}:{} date or time written with {:?}: use i18n::date, clock or date_time", source.path, pattern.line, pattern.text));
                }
            }
            let comma = tok.is(Kind::Ident, "replace") && t.get(i + 2).is_some_and(|p| p.is(Kind::Char, ".")) && t.get(i + 4).is_some_and(|p| p.is(Kind::Str, ","));
            if comma || (tok.is(Kind::Ident, "lang") && t.get(i + 1).is_some_and(|n| n.is(Kind::Punct, "(")) && t.get(i + 2).is_some_and(|n| n.is(Kind::Punct, ")")) && !(i > 0 && t[i - 1].is(Kind::Ident, "fn"))) {
                out.push(format!("{}:{} formats by language itself: use the helpers in i18n", source.path, tok.line));
            }
        }
    }
    out
}

fn report(what: &str, problems: &[String]) {
    assert!(problems.is_empty(), "{what}:\n{}", problems.join("\n"));
}

#[test]
fn every_catalog_is_a_language_the_app_speaks() {
    let codes: Vec<String> = catalogs().into_iter().map(|c| c.code).collect();
    let wanted: Vec<&str> = Lang::ALL.iter().filter(|l| **l != Lang::De).map(|l| l.code()).collect();
    assert_eq!(codes, wanted, "assets/i18n needs one file per language except German, and every file a Lang");
}

#[test]
fn every_tr_literal_has_a_text_in_every_language() {
    let (sources, catalogs) = (all_sources(), catalogs());
    let uses = translation_keys(&sources);
    assert!(uses.len() > 1000, "the scanner found only {} calls", uses.len());
    let problems: Vec<String> = catalogs.iter().flat_map(|c| missing(&uses, c)).collect();
    report("add these to assets/i18n", &problems);
}

#[test]
fn no_catalog_entry_is_dead() {
    let (sources, catalogs) = (all_sources(), catalogs());
    let uses = translation_keys(&sources);
    let problems: Vec<String> = catalogs.iter().flat_map(|c| unused(&sources, &uses, c)).collect();
    report("delete these from assets/i18n", &problems);
}

#[test]
fn catalogs_are_clean() {
    let mut problems = Vec::new();
    for c in catalogs() {
        problems.extend(c.duplicates.iter().map(|k| format!("{}.json repeats {k:?}", c.code)));
        problems.extend(mismatched(&c));
        for (key, entry) in &c.entries {
            let empty = match entry {
                Entry::One(s) => s.trim().is_empty(),
                Entry::Plural(forms) => forms.values().any(|s| s.trim().is_empty()),
            };
            if empty {
                problems.push(format!("{}.json has an empty text for {key:?}", c.code));
            }
            let texts: Vec<&String> = match entry {
                Entry::One(s) => vec![s],
                Entry::Plural(forms) => forms.values().collect(),
            };
            problems.extend(texts.into_iter().filter(|s| german(s)).map(|s| format!("{}.json still has German in {key:?} -> {s:?}", c.code)));
        }
    }
    problems.sort();
    report("fix assets/i18n", &problems);
}

#[test]
fn texts_in_the_live_ui_go_through_tr() {
    let sources = all_sources();
    let catalogs = catalogs();
    let keys: HashSet<&str> = catalogs.iter().flat_map(|c| c.entries.keys().map(String::as_str)).collect();
    let mut problems = untranslated_calls(&sources);
    problems.extend(german_strays(&sources, &keys));
    report("wrap in tr() and add to assets/i18n", &problems);
}

#[test]
fn dates_and_numbers_are_formatted_by_the_i18n_layer() {
    report("use the helpers in core::i18n", &local_formats(&all_sources()));
}

fn fake(path: &str, code: &str) -> Source {
    Source { path: path.into(), toks: lex(code) }
}

fn fake_catalog(pairs: &[(&str, &str)]) -> Catalog {
    Catalog { code: "xx".into(), entries: pairs.iter().map(|(k, v)| ((*k).to_owned(), Entry::One((*v).to_owned()))).collect(), duplicates: Vec::new() }
}

#[test]
fn a_missing_key_is_reported() {
    let sources = [fake("src/view/a.rs", "fn f() { tr(\"Neu\"); trf(\"%@ Konten\", &[&1]); trc(\"Ort\", \"Neu\"); trf(if n == 1 { \"Ein Konto\" } else { \"Konten\" }, &[]); }")];
    let uses = translation_keys(&sources);
    let keys: Vec<&str> = uses.iter().map(|u| u.key.as_str()).collect();
    assert_eq!(keys, ["Neu", "%@ Konten", "Neu (Ort)", "Ein Konto", "Konten"]);
    let found = missing(&uses, &fake_catalog(&[("Neu", "New"), ("%@ Konten", "%@ accounts")]));
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found[0].contains("Neu (Ort)"));
}

#[test]
fn a_dead_entry_is_reported() {
    let sources = [fake("src/view/a.rs", "fn f() { tr(\"Neu\"); const NAMES: [&str; 1] = [\"Dynamisch\"]; }")];
    let uses = translation_keys(&sources);
    let found = unused(&sources, &uses, &fake_catalog(&[("Neu", "New"), ("Dynamisch", "Dynamic"), ("Tot", "Dead")]));
    assert_eq!(found.len(), 1);
    assert!(found[0].contains("Tot"));
}

#[test]
fn placeholders_must_survive_translation() {
    assert!(mismatched(&fake_catalog(&[("%@ von %lld", "%@ of %lld"), ("%1$@ und %2$@", "%2$@ and %1$@")])).is_empty());
    assert_eq!(mismatched(&fake_catalog(&[("%@ von %lld", "%@ of"), ("%1$@ und %2$lld", "%1$lld and %2$@")])).len(), 2);
    assert!(mismatched(&fake_catalog(&[("100 %", "100 %"), ("%% sicher", "%% sure")])).is_empty());
}

#[test]
fn untranslated_texts_are_flagged() {
    let sources = [fake("src/view/a.rs", "fn f() { body(t, \"Hallo Welt\"); caption(t, tr(\"Gut\")); Button::new(\"a.b\", \"Speichern\"); show(ui, format!(\"{x}\")); label(\"Manycraft\"); sheet.hint(Some(\"Ein Hinweis\")); }")];
    let found = untranslated_calls(&sources);
    assert_eq!(found.len(), 3, "{found:?}");
    let keys = HashSet::new();
    assert_eq!(german_strays(&[fake("src/view/a.rs", "let a = \"Größe wählen\"; let b = \"ok\"; let c = \"Das ist nicht gut\";")], &keys).len(), 2);
}

#[test]
fn local_formatting_is_flagged() {
    let bad = [fake("src/view/a.rs", "fn f() { at.format(\"%H:%M\"); x.replace('.', \",\"); if lang() == Lang::De {} at.format(\"%Y-%m-%d %H-%M\"); }")];
    assert_eq!(local_formats(&bad).len(), 3);
}

#[test]
fn the_lexer_skips_comments_chars_and_test_modules() {
    let code = "// tr(\"c\")\nfn f<'a>(x: &'a str) { let q = '\"'; tr(\"a\\u{e4}\\n\"); let r = r#\"tr(\"raw\")\"#; }\n#[cfg(test)]\nmod tests { fn t() { tr(\"in tests\"); } }";
    let sources = [fake("src/x.rs", production(code))];
    let keys: Vec<String> = translation_keys(&sources).into_iter().map(|u| u.key).collect();
    assert_eq!(keys, ["a\u{e4}\n"]);
}