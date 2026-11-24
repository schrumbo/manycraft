//! Chat components as the game sends them (JSON at login, NBT later), turned into plain text, styled runs and the
//! translation keys the disconnect classifier reads.

use serde_json::{Map, Value};

use super::v777::TEXTS;
use crate::core::chat_text::Run;

/// The game's NBT nests at most 512 containers; a component walks one level per container.
const MAX_DEPTH: usize = 520;
const MAX_CHARS: usize = 8192;
const MAX_VISITS: usize = 4096;
const MAX_KEYS: usize = 16;

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
    let mut left = MAX_VISITS;
    let mut work = vec![Work::Value(component, Style::default(), 0)];
    while let Some(item) = work.pop() {
        if out.text.len() >= MAX_CHARS {
            break;
        }
        match item {
            Work::Text(text, style) => push(&mut out, text, style),
            Work::Value(value, style, depth) => visit(value, style, depth, &mut out, &mut left, &mut work),
        }
    }
    out
}

/// What a component still has to show, in the order it shows it; a stack of these replaces recursion, so the deepest
/// component the game accepts takes no more stack than a shallow one.
enum Work<'a> {
    Value(&'a Value, Style, usize),
    Text(&'a str, Style),
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

fn visit<'a>(value: &'a Value, style: Style, depth: usize, out: &mut Line, left: &mut usize, work: &mut Vec<Work<'a>>) {
    if depth > MAX_DEPTH || *left == 0 {
        return;
    }
    *left -= 1;
    let mut next: Vec<Work<'a>> = Vec::new();
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
            next.push(Work::Value(first, style, depth + 1));
            next.extend(rest.iter().map(|item| Work::Value(item, inherited, depth + 1)));
        }
        Value::Object(map) => {
            if map.len() == 1
                && let Some(inner) = map.get("")
            {
                work.push(Work::Value(inner, style, depth + 1));
                return;
            }
            let style = styled(map, style);
            if let Some(text) = map.get("text") {
                next.push(Work::Value(text, style, depth + 1));
            } else if let Some(Value::String(key)) = map.get("translate") {
                if out.keys.len() < MAX_KEYS && !out.keys.contains(key) {
                    out.keys.push(key.clone());
                }
                let args = map.get("with").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
                let format = english(key).or_else(|| map.get("fallback").and_then(Value::as_str)).unwrap_or(key);
                format_into(format, args, style, depth, &mut next);
            } else if let Some(Value::String(selector)) = map.get("selector") {
                push(out, selector, style);
            } else if let Some(Value::String(key)) = map.get("keybind") {
                push(out, key, style);
            }
            if let Some(Value::Array(extra)) = map.get("extra") {
                next.extend(extra.iter().map(|item| Work::Value(item, style, depth + 1)));
            }
        }
        Value::Null => {}
    }
    work.extend(next.into_iter().rev());
}

fn format_into<'a>(format: &'a str, args: &'a [Value], style: Style, depth: usize, next: &mut Vec<Work<'a>>) {
    let mut at_arg = 0;
    let mut rest = format;
    while let Some(at) = rest.find('%') {
        next.push(Work::Text(&rest[..at], style));
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('%') {
            next.push(Work::Text("%", style));
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('s') {
            if let Some(arg) = args.get(at_arg) {
                next.push(Work::Value(arg, style, depth + 1));
            }
            at_arg += 1;
            rest = tail;
        } else if let Some((digits, tail)) = after.split_once("$s").filter(|(d, _)| !d.is_empty() && d.len() <= 2 && d.bytes().all(|b| b.is_ascii_digit())) {
            if let Some(arg) = digits.parse::<usize>().ok().and_then(|n| args.get(n.wrapping_sub(1))) {
                next.push(Work::Value(arg, style, depth + 1));
            }
            rest = tail;
        } else {
            next.push(Work::Text("%", style));
            rest = after;
        }
    }
    next.push(Work::Text(rest, style));
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
        let args = [json!("a"), json!("b")];
        let mut next = Vec::new();
        format_into("%2$s then %1$s, 100%%", &args, Style::default(), 0, &mut next);
        let mut out = Line::default();
        let mut work: Vec<Work> = next.into_iter().rev().collect();
        while let Some(item) = work.pop() {
            match item {
                Work::Text(text, style) => push(&mut out, text, style),
                Work::Value(value, style, depth) => visit(value, style, depth, &mut out, &mut MAX_VISITS.to_owned(), &mut work),
            }
        }
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
    fn the_deepest_component_the_game_accepts_is_read_on_a_connection_thread() {
        use super::super::wire::In;
        let levels = 512;
        let mut nbt = vec![10u8];
        for _ in 1..levels {
            nbt.push(10);
            nbt.extend(0u16.to_be_bytes());
        }
        nbt.push(8);
        nbt.extend(0u16.to_be_bytes());
        nbt.extend(2u16.to_be_bytes());
        nbt.extend(b"ok");
        nbt.extend(std::iter::repeat_n(0u8, levels));
        let line = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let value = In::new(&nbt).nbt().unwrap();
                let line = line(&value);
                drop(value);
                line
            })
            .unwrap()
            .join()
            .expect("a 512 KB stack holds the deepest component");
        assert_eq!(line.text, "ok");
    }

    #[test]
    fn deep_or_huge_components_stay_bounded() {
        let mut deep = json!("x");
        for _ in 0..500 {
            deep = json!({"text": "", "extra": [deep]});
        }
        assert_eq!(line(&deep).text, "x", "as deep as the game nests");
        for _ in 0..100 {
            deep = json!({"text": "", "extra": [deep]});
        }
        assert_eq!(line(&deep).text, "", "past the limit nothing is walked");
        let huge = line(&json!({"text": "é".repeat(10_000)}));
        assert!(huge.text.len() <= MAX_CHARS);
    }

    #[test]
    fn repeated_arguments_cannot_blow_up() {
        let mut doubling = json!({"translate": "k0", "fallback": "x"});
        for i in 1..63 {
            doubling = json!({"translate": format!("k{i}"), "fallback": "%1$s%1$s", "with": [doubling]});
        }
        let l = line(&doubling);
        assert!(l.keys.len() <= MAX_KEYS);
        assert!(l.text.len() <= MAX_CHARS);
    }
}