//! Chat components as the game sends them (JSON at login, NBT later), turned into plain text, styled runs and the
//! translation keys the disconnect classifier reads.

use serde_json::{Map, Value};

use super::v777::TEXTS;
use crate::core::chat_text::Run;

const MAX_DEPTH: usize = 64;
const MAX_CHARS: usize = 8192;

/// A component read for the manager: its text, its styled runs and every translation key in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub runs: Vec<Run>,
    pub keys: Vec<String>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    color: Option<u32>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// Reads `component`.
pub fn line(component: &Value) -> Line {
    let mut out = Line::default();
    walk(component, Style::default(), &mut out, 0);
    out
}

/// The English text of translation `key` with `args` filled in, as the game would show it.
pub fn translate(key: &str, args: &[Value]) -> Line {
    let mut map = Map::new();
    map.insert("translate".into(), Value::String(key.into()));
    map.insert("with".into(), Value::Array(args.to_vec()));
    line(&Value::Object(map))
}

/// The English text of `key`, if the client knows it.
pub fn english(key: &str) -> Option<&'static str> {
    TEXTS.binary_search_by(|(k, _)| (*k).cmp(key)).ok().map(|i| TEXTS[i].1)
}

fn push(out: &mut Line, text: &str, style: Style) {
    if text.is_empty() || out.text.len() >= MAX_CHARS {
        return;
    }
    let room = MAX_CHARS - out.text.len();
    let text = if text.len() > room { &text[..text.floor_char_boundary(room)] } else { text };
    out.text.push_str(text);
    if let Some(last) = out.runs.last_mut()
        && style_of(last) == style
    {
        last.text.push_str(text);
        return;
    }
    out.runs.push(Run { text: text.to_owned(), color: style.color, bold: style.bold, italic: style.italic, underline: style.underline, strike: style.strike });
}

fn style_of(run: &Run) -> Style {
    Style { color: run.color, bold: run.bold, italic: run.italic, underline: run.underline, strike: run.strike }
}

fn walk(value: &Value, style: Style, out: &mut Line, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::String(s) => push(out, s, style),
        Value::Number(n) => push(out, &n.to_string(), style),
        Value::Bool(b) => push(out, &b.to_string(), style),
        Value::Array(items) => {
            let Some((first, rest)) = items.split_first() else { return };
            let inherited = match first {
                Value::Object(map) => styled(map, style),
                _ => style,
            };
            walk(first, style, out, depth + 1);
            for item in rest {
                walk(item, inherited, out, depth + 1);
            }
        }
        Value::Object(map) => {
            if map.len() == 1
                && let Some(inner) = map.get("")
            {
                walk(inner, style, out, depth + 1);
                return;
            }
            let style = styled(map, style);
            if let Some(text) = map.get("text") {
                walk(text, style, out, depth + 1);
            } else if let Some(Value::String(key)) = map.get("translate") {
                out.keys.push(key.clone());
                let args = map.get("with").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
                let format = english(key).or_else(|| map.get("fallback").and_then(Value::as_str)).unwrap_or(key);
                format_into(format, args, style, out, depth);
            } else if let Some(Value::String(selector)) = map.get("selector") {
                push(out, selector, style);
            } else if let Some(Value::String(key)) = map.get("keybind") {
                push(out, key, style);
            }
            if let Some(Value::Array(extra)) = map.get("extra") {
                for item in extra {
                    walk(item, style, out, depth + 1);
                }
            }
        }
        Value::Null => {}
    }
}

fn format_into(format: &str, args: &[Value], style: Style, out: &mut Line, depth: usize) {
    let mut next = 0;
    let mut rest = format;
    while let Some(at) = rest.find('%') {
        push(out, &rest[..at], style);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('%') {
            push(out, "%", style);
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('s') {
            if let Some(arg) = args.get(next) {
                walk(arg, style, out, depth + 1);
            }
            next += 1;
            rest = tail;
        } else if let Some((digits, tail)) = after.split_once("$s").filter(|(d, _)| !d.is_empty() && d.len() <= 2 && d.bytes().all(|b| b.is_ascii_digit())) {
            if let Some(arg) = digits.parse::<usize>().ok().and_then(|n| args.get(n.wrapping_sub(1))) {
                walk(arg, style, out, depth + 1);
            }
            rest = tail;
        } else {
            push(out, "%", style);
            rest = after;
        }
    }
    push(out, rest, style);
}

fn styled(map: &Map<String, Value>, mut style: Style) -> Style {
    if let Some(color) = map.get("color").and_then(Value::as_str).and_then(color) {
        style.color = Some(color);
    }
    for (key, flag) in [("bold", &mut style.bold), ("italic", &mut style.italic), ("underlined", &mut style.underline), ("strikethrough", &mut style.strike)] {
        match map.get(key) {
            Some(Value::Bool(b)) => *flag = *b,
            Some(Value::Number(n)) => *flag = n.as_i64() != Some(0),
            _ => {}
        }
    }
    style
}

fn color(name: &str) -> Option<u32> {
    if let Some(hex) = name.strip_prefix('#') {
        return u32::from_str_radix(hex, 16).ok().filter(|c| *c <= 0xFF_FFFF);
    }
    Some(match name {
        "black" => 0x000000,
        "dark_blue" => 0x0000AA,
        "dark_green" => 0x00AA00,
        "dark_aqua" => 0x00AAAA,
        "dark_red" => 0xAA0000,
        "dark_purple" => 0xAA00AA,
        "gold" => 0xFFAA00,
        "gray" => 0xAAAAAA,
        "dark_gray" => 0x555555,
        "blue" => 0x5555FF,
        "green" => 0x55FF55,
        "aqua" => 0x55FFFF,
        "red" => 0xFF5555,
        "light_purple" => 0xFF55FF,
        "yellow" => 0xFFFF55,
        "white" => 0xFFFFFF,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_text_table_is_sorted_for_lookup() {
        assert!(TEXTS.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(english("chat.type.text"), Some("<%s> %s"));
        assert_eq!(english("no.such.key"), None);
    }

    #[test]
    fn translations_fill_in_their_arguments_with_styles() {
        let joined = line(&json!({"translate": "multiplayer.player.joined", "with": [{"text": "Dev", "color": "yellow"}], "color": "yellow"}));
        assert_eq!(joined.text, "Dev joined the game");
        assert_eq!(joined.keys, ["multiplayer.player.joined"]);
        assert_eq!(joined.runs.len(), 1);
        assert_eq!(joined.runs[0].color, Some(0xFFFF55));
        let say = line(&json!({"translate": "chat.type.announcement", "with": ["Server", "hi"]}));
        assert_eq!(say.text, "[Server] hi");
        let positional = translate("x", &[]);
        assert_eq!(positional.text, "x", "an unknown key shows itself");
        let mut out = Line::default();
        format_into("%2$s then %1$s, 100%%", &[json!("a"), json!("b")], Style::default(), &mut out, 0);
        assert_eq!(out.text, "b then a, 100%");
    }

    #[test]
    fn nbt_shaped_components_read_like_json_ones() {
        let nbt = json!({"text": "", "extra": [{"text": "A", "bold": 1}, {"": "B"}, {"text": "C", "color": "#123456", "bold": 0}]});
        let l = line(&nbt);
        assert_eq!(l.text, "ABC");
        assert!(l.runs[0].bold && !l.runs[1].bold);
        assert_eq!(l.runs.last().unwrap().color, Some(0x123456));
        assert_eq!(line(&json!(["a", {"text": "b"}, 3])).text, "ab3");
        assert_eq!(line(&json!({"translate": "custom.key", "fallback": "Hallo %s", "with": ["Welt"]})).text, "Hallo Welt");
    }

    #[test]
    fn deep_or_huge_components_stay_bounded() {
        let mut deep = json!("x");
        for _ in 0..500 {
            deep = json!({"text": "", "extra": [deep]});
        }
        assert_eq!(line(&deep).text, "");
        let huge = line(&json!({"text": "é".repeat(10_000)}));
        assert!(huge.text.len() <= MAX_CHARS);
    }
}
