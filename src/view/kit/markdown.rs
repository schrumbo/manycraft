//! Markdown for project descriptions: a small parser into blocks and styled runs, and a renderer in the theme's fonts.
//! Links open in the browser; text is not selectable.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use freya::engine::prelude::{RectHeightStyle, RectWidthStyle};
use freya::prelude::*;
use freya_core::elements::paragraph::ParagraphHolderInner;

use super::text::{hairline, line};
use super::{open_external, use_theme};
use crate::view::theme::{Theme, faces, font, frame, space};

mod html;
mod picture;
mod table;

pub use picture::Pic;
pub use table::Align;

const MAX_QUOTE_DEPTH: usize = 4;
const MAX_NESTING: usize = 8;
const MAX_INDENT: usize = 4;
const TAB_WIDTH: usize = 4;
const MAX_CLOSER: usize = 2048;

/// A piece of inline text with one style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}

/// How a list item is marked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    Bullet,
    Number(usize),
}

/// One item of a list; `depth` counts how far it is nested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub depth: usize,
    pub marker: Marker,
    pub runs: Vec<Run>,
}

/// One block of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading { level: u8, runs: Vec<Run> },
    Paragraph(Vec<Run>),
    List(Vec<Item>),
    Quote(Vec<Block>),
    Code(String),
    Rule,
    Table { align: Vec<Align>, head: Vec<Vec<Run>>, rows: Vec<Vec<Vec<Run>>> },
    Images(Vec<Pic>),
}

#[derive(Clone, Default)]
struct Look {
    bold: bool,
    italic: bool,
    code: bool,
    link: Option<String>,
}

fn push(out: &mut Vec<Run>, text: &str, look: &Look) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.bold == look.bold
        && last.italic == look.italic
        && last.code == look.code
        && last.link == look.link
    {
        last.text.push_str(text);
        return;
    }
    out.push(Run { text: text.to_owned(), bold: look.bold, italic: look.italic, code: look.code, link: look.link.clone() });
}

fn run_len(c: &[char], at: usize, marker: char) -> usize {
    c.iter().skip(at).take_while(|ch| **ch == marker).count()
}

fn code_span(c: &[char], at: usize) -> Option<(String, usize)> {
    let open = run_len(c, at, '`');
    let mut j = at + open;
    let limit = c.len().min(j + MAX_CLOSER);
    while j < limit {
        if c[j] == '`' {
            let close = run_len(c, j, '`');
            if close == open {
                let inner: String = c[at + open..j].iter().collect();
                let inner = match inner.strip_prefix(' ').and_then(|s| s.strip_suffix(' ')) {
                    Some(trimmed) if !trimmed.trim().is_empty() => trimmed.to_owned(),
                    _ => inner,
                };
                return Some((inner, j + close));
            }
            j += close;
        } else {
            j += 1;
        }
    }
    None
}

fn emphasis(c: &[char], at: usize) -> Option<(Range<usize>, usize, usize)> {
    let marker = c[at];
    let open = run_len(c, at, marker).min(3);
    let start = at + open;
    if c.get(start).is_none_or(|ch| ch.is_whitespace()) {
        return None;
    }
    if marker == '_' && at > 0 && c[at - 1].is_alphanumeric() {
        return None;
    }
    let limit = c.len().min(start + MAX_CLOSER);
    let mut j = start;
    while j < limit {
        match c[j] {
            '\\' => j += 2,
            '`' => match code_span(c, j) {
                Some((_, end)) => j = end,
                None => j += run_len(c, j, '`'),
            },
            ch if ch == marker => {
                let run = run_len(c, j, marker);
                let closes = run == open && !c[j - 1].is_whitespace() && (marker != '_' || !c.get(j + run).is_some_and(|n| n.is_alphanumeric()));
                if closes {
                    return Some((start..j, j + run, open));
                }
                j += run;
            }
            _ => j += 1,
        }
    }
    None
}

fn safe_link(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then(|| url.to_owned())
}

fn link(c: &[char], at: usize) -> Option<(Range<usize>, String, usize)> {
    let mut depth = 1;
    let limit = c.len().min(at + 1 + MAX_CLOSER);
    let mut j = at + 1;
    while j < limit {
        match c[j] {
            '\\' => j += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            '`' => {
                j = match code_span(c, j) {
                    Some((_, end)) => end,
                    None => j + run_len(c, j, '`'),
                } - 1;
            }
            _ => {}
        }
        j += 1;
    }
    if j >= limit || c.get(j + 1) != Some(&'(') {
        return None;
    }
    let mut parens = 1;
    let reach = c.len().min(j + 2 + MAX_CLOSER);
    let mut k = j + 2;
    while k < reach {
        match c[k] {
            '(' => parens += 1,
            ')' => {
                parens -= 1;
                if parens == 0 {
                    break;
                }
            }
            _ => {}
        }
        k += 1;
    }
    if k >= reach {
        return None;
    }
    let raw: String = c[j + 2..k].iter().collect();
    let target = raw.split_whitespace().next()?;
    let target = target.strip_prefix('<').and_then(|t| t.strip_suffix('>')).unwrap_or(target);
    Some((at + 1..j, target.to_owned(), k + 1))
}

fn walk(c: &[char], look: &Look, out: &mut Vec<Run>, depth: usize) {
    let mut plain = String::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '\\'
            && let Some(next) = c.get(i + 1)
            && next.is_ascii_punctuation()
        {
            plain.push(*next);
            i += 2;
            continue;
        }
        if depth < MAX_NESTING {
            match ch {
                '`' => {
                    if let Some((code, end)) = code_span(c, i) {
                        push(out, &plain, look);
                        plain.clear();
                        push(out, &code, &Look { code: true, ..look.clone() });
                        i = end;
                    } else {
                        let run = run_len(c, i, '`');
                        plain.extend(std::iter::repeat_n('`', run));
                        i += run;
                    }
                    continue;
                }
                '*' | '_' => {
                    if let Some((inner, end, open)) = emphasis(c, i) {
                        push(out, &plain, look);
                        plain.clear();
                        let mut next = look.clone();
                        next.bold |= open >= 2;
                        next.italic |= open != 2;
                        walk(&c[inner], &next, out, depth + 1);
                        i = end;
                    } else {
                        let run = run_len(c, i, ch);
                        plain.extend(std::iter::repeat_n(ch, run));
                        i += run;
                    }
                    continue;
                }
                '[' if look.link.is_none() => {
                    if let Some((text, url, end)) = link(c, i) {
                        push(out, &plain, look);
                        plain.clear();
                        walk(&c[text], &Look { link: safe_link(&url), ..look.clone() }, out, depth + 1);
                        i = end;
                        continue;
                    }
                }
                _ => {}
            }
        }
        plain.push(ch);
        i += 1;
    }
    push(out, &plain, look);
}

/// The runs of one line or paragraph of text: bold, italic, code and links; markers that never close stay as written.
pub fn inline(text: &str) -> Vec<Run> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    walk(&chars, &Look::default(), &mut out, 0);
    out
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { TAB_WIDTH } else { 1 }).sum()
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(first) = compact.chars().next() else { return false };
    compact.chars().count() >= 3 && matches!(first, '-' | '*' | '_') && compact.chars().all(|c| c == first)
}

fn fence_of(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    let first = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = t.chars().take_while(|c| *c == first).count();
    (run >= 3).then_some((first, run))
}

fn heading_of(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = t.get(hashes..)?;
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim_end();
    Some((hashes.min(3) as u8, text))
}

fn quote_line(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn item_of(line: &str) -> Option<(usize, Marker, &str)> {
    let indent = indent_of(line);
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix(['-', '*', '+']).and_then(|r| r.strip_prefix(' ')) {
        return Some((indent, Marker::Bullet, rest));
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let number = t.get(..digits)?.parse().ok()?;
        let rest = t.get(digits..)?.strip_prefix(['.', ')'])?.strip_prefix(' ')?;
        return Some((indent, Marker::Number(number), rest));
    }
    None
}

fn starts_block(line: &str, depth: usize) -> bool {
    is_rule(line) || fence_of(line).is_some() || heading_of(line).is_some() || (depth < MAX_QUOTE_DEPTH && quote_line(line).is_some())
}

fn flush(out: &mut Vec<Block>, para: &mut Vec<&str>) {
    if !para.is_empty() {
        out.push(Block::Paragraph(inline(&picture::without_pictures(&para.join(" ")))));
        para.clear();
    }
}

fn list_from(lines: &[&str], from: usize, depth: usize) -> (Block, usize) {
    let mut raw: Vec<(usize, Marker, String)> = Vec::new();
    let mut indents: Vec<usize> = Vec::new();
    let mut counters: Vec<Option<usize>> = Vec::new();
    let mut at = from;
    while at < lines.len() {
        let line = lines[at];
        if line.trim().is_empty() || starts_block(line, depth) {
            break;
        }
        if let Some((indent, marker, rest)) = item_of(line) {
            while indents.last().is_some_and(|top| *top > indent) {
                indents.pop();
            }
            if indents.last().is_none_or(|top| *top < indent) {
                indents.push(indent);
            }
            let level = indents.len() - 1;
            counters.truncate(level + 1);
            counters.resize(level + 1, None);
            let marker = match marker {
                Marker::Number(written) => {
                    let n = counters[level].map_or(written, |previous| previous + 1);
                    counters[level] = Some(n);
                    Marker::Number(n)
                }
                Marker::Bullet => {
                    counters[level] = None;
                    Marker::Bullet
                }
            };
            raw.push((level, marker, rest.trim().to_owned()));
        } else if let Some(last) = raw.last_mut() {
            if !last.2.is_empty() {
                last.2.push(' ');
            }
            last.2.push_str(line.trim());
        }
        at += 1;
    }
    let items = raw.into_iter().map(|(level, marker, text)| Item { depth: level, marker, runs: inline(&text) }).collect();
    (Block::List(items), at)
}

fn parse(lines: &[&str], depth: usize) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            flush(&mut out, &mut para);
            i += 1;
        } else if let Some((mark, run)) = fence_of(line) {
            flush(&mut out, &mut para);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() {
                let closes = fence_of(lines[i]).is_some_and(|(m, r)| m == mark && r >= run) && lines[i].trim().chars().all(|c| c == mark);
                if closes {
                    i += 1;
                    break;
                }
                code.push(lines[i]);
                i += 1;
            }
            out.push(Block::Code(code.join("\n")));
        } else if is_rule(line) {
            flush(&mut out, &mut para);
            out.push(Block::Rule);
            i += 1;
        } else if let Some((level, text)) = heading_of(line) {
            flush(&mut out, &mut para);
            out.push(Block::Heading { level, runs: inline(text) });
            i += 1;
        } else if depth < MAX_QUOTE_DEPTH && quote_line(line).is_some() {
            flush(&mut out, &mut para);
            let mut inner = Vec::new();
            while i < lines.len() {
                let Some(rest) = quote_line(lines[i]) else { break };
                inner.push(rest);
                i += 1;
            }
            out.push(Block::Quote(parse(&inner, depth + 1)));
        } else if let Some((table, next)) = table::parse(lines, i) {
            flush(&mut out, &mut para);
            out.push(table);
            i = next;
        } else if item_of(line).is_some() {
            flush(&mut out, &mut para);
            let (list, next) = list_from(lines, i, depth);
            out.push(list);
            i = next.max(i + 1);
        } else if let Some(pics) = picture::only_pictures(line.trim()) {
            flush(&mut out, &mut para);
            let continues = i > 0 && !lines[i - 1].trim().is_empty();
            match out.last_mut() {
                Some(Block::Images(row)) if continues => row.extend(pics),
                _ => out.push(Block::Images(pics)),
            }
            i += 1;
        } else {
            para.push(line.trim());
            i += 1;
        }
    }
    flush(&mut out, &mut para);
    out
}

fn label_key(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn definition(line: &str) -> Option<(String, String)> {
    if indent_of(line) >= MAX_INDENT {
        return None;
    }
    let rest = line.trim_start().strip_prefix('[')?;
    let (label, url) = rest.split_once("]:")?;
    let target = url.split_whitespace().next()?;
    let target = target.strip_prefix('<').and_then(|t| t.strip_suffix('>')).unwrap_or(target);
    (!label.trim().is_empty() && !label.contains('[') && !label.starts_with('^')).then(|| (label_key(label), target.to_owned()))
}

fn closing(c: &[char], at: usize) -> Option<usize> {
    let mut depth = 0;
    let mut j = at;
    while j < c.len().min(at + MAX_CLOSER) {
        match c[j] {
            '\\' => j += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

fn resolve(text: &str, defs: &HashMap<String, String>, depth: usize) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => {
                out.extend(c[i..c.len().min(i + 2)].iter());
                i += 2;
                continue;
            }
            '`' => {
                let end = code_span(&c, i).map_or(i + run_len(&c, i, '`'), |(_, end)| end);
                out.extend(c[i..end].iter());
                i = end;
                continue;
            }
            '[' if depth < MAX_NESTING => {}
            other => {
                out.push(other);
                i += 1;
                continue;
            }
        }
        let Some(close) = closing(&c, i) else {
            out.push('[');
            i += 1;
            continue;
        };
        let inner: String = c[i + 1..close].iter().collect();
        let shown = resolve(&inner, defs, depth + 1);
        let after = close + 1;
        let full = (c.get(after) == Some(&'[')).then(|| closing(&c, after)).flatten().map(|end| {
            let label: String = c[after + 1..end].iter().collect();
            (if label.trim().is_empty() { label_key(&inner) } else { label_key(&label) }, end + 1)
        });
        let target = match full {
            Some((key, next)) => defs.get(&key).map(|url| (url, next)),
            None if c.get(after) != Some(&'(') => defs.get(&label_key(&inner)).map(|url| (url, after)),
            None => None,
        };
        match target {
            Some((url, next)) => {
                out.push_str(&format!("[{shown}]({url})"));
                i = next;
            }
            None => {
                out.push_str(&format!("[{shown}]"));
                i = after;
            }
        }
    }
    out
}

fn references(text: &str) -> Cow<'_, str> {
    let mut defs = HashMap::new();
    let mut fence: Option<(char, usize)> = None;
    let mut kept = Vec::new();
    for line in text.lines() {
        if let Some((mark, run)) = fence {
            if fence_of(line).is_some_and(|(m, r)| m == mark && r >= run) && line.trim().chars().all(|c| c == mark) {
                fence = None;
            }
            kept.push((line, true));
            continue;
        }
        if let Some(open) = fence_of(line) {
            fence = Some(open);
            kept.push((line, true));
            continue;
        }
        match definition(line) {
            Some((key, url)) => {
                defs.entry(key).or_insert(url);
            }
            None => kept.push((line, false)),
        }
    }
    if defs.is_empty() {
        return Cow::Borrowed(text);
    }
    Cow::Owned(kept.into_iter().map(|(line, code)| if code { line.to_owned() } else { resolve(line, &defs, 0) }).collect::<Vec<_>>().join("\n"))
}

/// The blocks of `text`: headings, paragraphs joined across line breaks, lists, quotes, code fences and rules.
/// Reference links (`[text][ref]`, `[ref]`) are resolved against their `[ref]: url` lines, which are not shown.
pub fn blocks(text: &str) -> Vec<Block> {
    let text = html::normalize(text);
    let text = references(&text);
    let lines: Vec<&str> = text.lines().collect();
    parse(&lines, 0)
}

/// Where each link lies in the text of `runs`, in UTF-16 units as the text layout counts.
pub fn link_ranges(runs: &[Run]) -> Vec<(Range<usize>, String)> {
    let mut at = 0;
    let mut out = Vec::new();
    for run in runs {
        let len = run.text.encode_utf16().count();
        if let Some(url) = &run.link {
            out.push((at..at + len, url.clone()));
        }
        at += len;
    }
    out
}

fn link_at(holder: &ParagraphHolder, links: &[(Range<usize>, String)], at: CursorPoint) -> Option<(Range<usize>, String)> {
    let at = holder.visible_location(at);
    let inner = holder.0.borrow();
    let ParagraphHolderInner { paragraph, scale_factor, .. } = inner.as_ref()?;
    let (x, y) = ((at.x * scale_factor) as f32, (at.y * scale_factor) as f32);
    links
        .iter()
        .find(|(range, _)| paragraph.get_rects_for_range(range.clone(), RectHeightStyle::Tight, RectWidthStyle::Tight).iter().any(|b| b.rect.left <= x && x <= b.rect.right && b.rect.top <= y && y <= b.rect.bottom))
        .cloned()
}

#[derive(PartialEq)]
struct Prose {
    runs: Vec<Run>,
    size: f32,
    weight: FontWeight,
    color: Color,
    align: TextAlign,
}

impl Component for Prose {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let holder = use_hook(ParagraphHolder::default);
        let mut hovering = use_state(|| false);
        let mut pressed = use_state(|| None::<Range<usize>>);
        let links = link_ranges(&self.runs);
        let mut p = paragraph().width(Size::fill()).line_height(frame::PARA_LINE).font_size(self.size).color(self.color).font_weight(self.weight).text_align(self.align).holder(holder.clone());
        if let Some(f) = &t.font {
            p = p.font_family(f.clone());
        }
        for family in faces::BODY {
            p = p.font_family(family);
        }
        for run in &self.runs {
            let mut s = Span::new(run.text.clone());
            if run.bold {
                s = s.font_weight(FontWeight::BOLD);
            }
            if run.italic {
                s = s.font_slant(FontSlant::Italic);
            }
            if run.code {
                for family in faces::MONO {
                    s = s.font_family(family);
                }
            }
            if run.link.is_some() {
                s = s.color(t.accent_text).text_decoration(TextDecoration::Underline);
            }
            p = p.span(s);
        }
        if links.is_empty() {
            return p;
        }
        let (over, down, release, moved, grabbed) = (links.clone(), links.clone(), links, holder.clone(), holder.clone());
        p.cursor(if *hovering.read() { CursorIcon::Pointer } else { CursorIcon::Default })
            .on_pointer_move(move |e: Event<PointerEventData>| hovering.set_if_modified(link_at(&moved, &over, e.element_location()).is_some()))
            .on_pointer_leave(move |_| {
                hovering.set_if_modified(false);
                pressed.set_if_modified(None);
            })
            .on_pointer_down(move |e: Event<PointerEventData>| {
                if e.data().is_primary() {
                    pressed.set_if_modified(link_at(&grabbed, &down, e.element_location()).map(|(range, _)| range));
                }
            })
            .on_pointer_press(move |e: Event<PointerEventData>| {
                if !e.data().is_primary() {
                    return;
                }
                let began = pressed.peek().clone();
                pressed.set_if_modified(None);
                if let Some((range, url)) = link_at(&holder, &release, e.element_location())
                    && began == Some(range)
                {
                    e.stop_propagation();
                    open_external("project link", url);
                }
            })
    }
}

fn prose(runs: &[Run], size: f32, weight: FontWeight, color: Color) -> Element {
    Prose { runs: runs.to_vec(), size, weight, color, align: TextAlign::Left }.into_element()
}

fn code_block(t: &Theme, text: &str) -> Element {
    let mut p = paragraph().width(Size::fill()).line_height(frame::PARA_LINE).font_size(font::CALLOUT).color(t.text1);
    for family in faces::MONO {
        p = p.font_family(family);
    }
    rect().width(Size::fill()).padding(space::S).corner_radius(t.shape.small).background(t.well).child(p.span(Span::new(text.to_owned()))).into_element()
}

fn marker_text(marker: Marker) -> String {
    match marker {
        Marker::Bullet => "•".to_owned(),
        Marker::Number(n) => format!("{n}."),
    }
}

fn view_block(t: &Theme, block: &Block, color: Color) -> Element {
    match block {
        Block::Heading { level, runs } => {
            let size = match level {
                1 => font::TITLE2,
                2 => font::TITLE3,
                _ => font::BODY,
            };
            prose(runs, size, FontWeight::SEMI_BOLD, t.text1)
        }
        Block::Paragraph(runs) => prose(runs, font::BODY, FontWeight::NORMAL, color),
        Block::List(items) => rect()
            .width(Size::fill())
            .spacing(space::XS)
            .children(items.iter().map(|item| {
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .padding((0., 0., 0., item.depth.min(MAX_INDENT) as f32 * space::L))
                    .content(Content::flex())
                    .child(rect().width(Size::px(frame::MARKER_W)).child(line(t, marker_text(item.marker), font::BODY, t.text2).font_tabular()))
                    .child(rect().width(Size::flex(1.)).child(prose(&item.runs, font::BODY, FontWeight::NORMAL, color)))
                    .into_element()
            }))
            .into_element(),
        Block::Quote(inner) => rect()
            .width(Size::fill())
            .horizontal()
            .spacing(space::S)
            .content(Content::flex())
            .child(rect().width(Size::px(frame::QUOTE_BAR)).height(Size::fill()).corner_radius(frame::QUOTE_BAR / 2.).background(t.separator))
            .child(rect().width(Size::flex(1.)).child(view(t, inner, t.text2)))
            .into_element(),
        Block::Code(text) => code_block(t, text),
        Block::Rule => hairline(t),
        Block::Table { align, head, rows } => table::view(t, align, head, rows, color),
        Block::Images(pics) => picture::view(pics),
    }
}

/// `blocks` as a column, body text in `color`; prose keeps a readable measure while tables, pictures and code use the width.
pub fn view(t: &Theme, blocks: &[Block], color: Color) -> Element {
    let column = blocks.iter().map(|b| {
        let block = view_block(t, b, color);
        match b {
            Block::Table { .. } | Block::Images(_) | Block::Code(_) | Block::Rule => block,
            _ => rect().width(Size::fill()).max_width(Size::px(frame::PROSE_MAX)).child(block).into_element(),
        }
    });
    rect().width(Size::fill()).spacing(space::M).children(column).into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Run {
        Run { text: text.to_owned(), ..Run::default() }
    }

    fn bold(text: &str) -> Run {
        Run { bold: true, ..plain(text) }
    }

    fn italic(text: &str) -> Run {
        Run { italic: true, ..plain(text) }
    }

    fn code(text: &str) -> Run {
        Run { code: true, ..plain(text) }
    }

    fn linked(text: &str, url: &str) -> Run {
        Run { link: Some(url.to_owned()), ..plain(text) }
    }

    fn item(depth: usize, marker: Marker, runs: Vec<Run>) -> Item {
        Item { depth, marker, runs }
    }

    #[test]
    fn headings_by_level_and_deeper_ones_stay_headings() {
        assert_eq!(blocks("# One\n## Two\n### Three\n#### Four"), vec![
            Block::Heading { level: 1, runs: vec![plain("One")] },
            Block::Heading { level: 2, runs: vec![plain("Two")] },
            Block::Heading { level: 3, runs: vec![plain("Three")] },
            Block::Heading { level: 3, runs: vec![plain("Four")] },
        ]);
    }

    #[test]
    fn a_hash_without_a_space_is_text() {
        assert_eq!(blocks("#nope"), vec![Block::Paragraph(vec![plain("#nope")])]);
    }

    #[test]
    fn closing_hashes_leave_a_heading() {
        assert_eq!(blocks("## Title ##"), vec![Block::Heading { level: 2, runs: vec![plain("Title")] }]);
    }

    #[test]
    fn lines_join_into_a_paragraph_and_a_blank_line_ends_it() {
        assert_eq!(blocks("one\ntwo\n\nthree"), vec![Block::Paragraph(vec![plain("one two")]), Block::Paragraph(vec![plain("three")])]);
    }

    #[test]
    fn crlf_text_parses_like_lf() {
        assert_eq!(blocks("one\r\ntwo\r\n\r\nthree"), blocks("one\ntwo\n\nthree"));
    }

    #[test]
    fn bullets_take_every_marker() {
        assert_eq!(blocks("- a\n* b\n+ c"), vec![Block::List(vec![item(0, Marker::Bullet, vec![plain("a")]), item(0, Marker::Bullet, vec![plain("b")]), item(0, Marker::Bullet, vec![plain("c")])])]);
    }

    #[test]
    fn numbered_lists_count_on_from_the_first_number() {
        assert_eq!(blocks("3. a\n1. b\n2) c"), vec![Block::List(vec![item(0, Marker::Number(3), vec![plain("a")]), item(0, Marker::Number(4), vec![plain("b")]), item(0, Marker::Number(5), vec![plain("c")])])]);
    }

    #[test]
    fn a_nested_item_with_bold_and_a_link() {
        let got = blocks("- top\n  - **fast** see [docs](https://example.org/a_b)\n- next");
        assert_eq!(got, vec![Block::List(vec![
            item(0, Marker::Bullet, vec![plain("top")]),
            item(1, Marker::Bullet, vec![bold("fast"), plain(" see "), linked("docs", "https://example.org/a_b")]),
            item(0, Marker::Bullet, vec![plain("next")]),
        ])]);
    }

    #[test]
    fn a_wrapped_item_continues_on_the_next_line() {
        assert_eq!(blocks("- one\n  and more\n- two"), vec![Block::List(vec![item(0, Marker::Bullet, vec![plain("one and more")]), item(0, Marker::Bullet, vec![plain("two")])])]);
    }

    #[test]
    fn a_list_ends_at_a_blank_line_and_a_heading() {
        assert_eq!(blocks("- a\n\n- b\n# H"), vec![
            Block::List(vec![item(0, Marker::Bullet, vec![plain("a")])]),
            Block::List(vec![item(0, Marker::Bullet, vec![plain("b")])]),
            Block::Heading { level: 1, runs: vec![plain("H")] },
        ]);
    }

    #[test]
    fn quotes_hold_blocks() {
        assert_eq!(blocks("> one\n> two\n>\n> - x"), vec![Block::Quote(vec![Block::Paragraph(vec![plain("one two")]), Block::List(vec![item(0, Marker::Bullet, vec![plain("x")])])])]);
    }

    #[test]
    fn endless_quote_marks_stop_nesting() {
        let got = blocks(">>>>>>>> deep");
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn a_code_fence_keeps_its_text_as_written() {
        let got = blocks("```rust\n  let x = **1**;\n\n# not a heading\n- not a list\n```\nafter");
        assert_eq!(got, vec![Block::Code("  let x = **1**;\n\n# not a heading\n- not a list".to_owned()), Block::Paragraph(vec![plain("after")])]);
    }

    #[test]
    fn reference_links_resolve_and_their_definitions_hide() {
        let got = blocks("See [the wiki][w], [Docs] and [Site][].\n\n[w]: https://example.com/wiki\n[docs]: <https://example.com/docs> \"Docs\"\n[site]: https://example.com");
        assert_eq!(got, vec![Block::Paragraph(vec![plain("See "), linked("the wiki", "https://example.com/wiki"), plain(", "), linked("Docs", "https://example.com/docs"), plain(" and "), linked("Site", "https://example.com"), plain(".")])]);
    }

    #[test]
    fn unknown_references_and_code_stay_as_written() {
        assert_eq!(blocks("[x] and `[w]`\n\n[w]: https://example.com"), vec![Block::Paragraph(vec![plain("[x] and "), code("[w]")])]);
        assert_eq!(blocks("```\n[w]: https://example.com\n```"), vec![Block::Code("[w]: https://example.com".to_owned())]);
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        assert_eq!(blocks("```\na\nb"), vec![Block::Code("a\nb".to_owned())]);
    }

    #[test]
    fn rules_are_three_or_more_marks() {
        assert_eq!(blocks("a\n---\nb\n* * *\n___"), vec![Block::Paragraph(vec![plain("a")]), Block::Rule, Block::Paragraph(vec![plain("b")]), Block::Rule, Block::Rule]);
        assert_eq!(blocks("--"), vec![Block::Paragraph(vec![plain("--")])]);
    }

    #[test]
    fn spans_of_every_kind() {
        assert_eq!(inline("a **b** *c* _d_ `e` [f](https://x.y)"), vec![plain("a "), bold("b"), plain(" "), italic("c"), plain(" "), italic("d"), plain(" "), code("e"), plain(" "), linked("f", "https://x.y")]);
    }

    #[test]
    fn styles_nest() {
        assert_eq!(inline("**a *b* c**"), vec![bold("a "), Run { bold: true, italic: true, ..plain("b") }, bold(" c")]);
        assert_eq!(inline("[**a**](https://x.y)"), vec![Run { bold: true, ..linked("a", "https://x.y") }]);
    }

    #[test]
    fn code_keeps_its_markers() {
        assert_eq!(inline("`a **b** [c](d)`"), vec![code("a **b** [c](d)")]);
    }

    #[test]
    fn unclosed_markers_stay_literal() {
        for text in ["**bold", "*one", "_one", "`code", "[a](https://x.y", "[a]", "a ** b", "2 * 3 * 4", "snake_case_name", "***"] {
            assert_eq!(inline(text), vec![plain(text)], "{text}");
        }
    }

    #[test]
    fn a_long_run_of_unclosed_markers_stays_literal() {
        for unit in ["*a ", "_a ", "[a ", "[a](b "] {
            let text = unit.repeat(20000);
            assert_eq!(blocks(&text), vec![Block::Paragraph(vec![plain(text.trim())])], "{unit}");
        }
    }

    #[test]
    fn an_escape_keeps_the_mark_as_text() {
        assert_eq!(inline(r"\*a\* \[b\]"), vec![plain("*a* [b]")]);
    }

    #[test]
    fn only_web_links_are_links() {
        assert_eq!(inline("[a](javascript:alert(1)) [b](file:///c) [c](rel/path) [d](mailto:x@y.z)"), vec![plain("a b c d")]);
        assert_eq!(inline("[a](HTTPS://X.Y \"title\")"), vec![linked("a", "HTTPS://X.Y")]);
    }

    #[test]
    fn a_link_may_hold_brackets_and_parentheses() {
        assert_eq!(inline("[a [b]](https://x.y/(z))"), vec![linked("a [b]", "https://x.y/(z)")]);
    }

    #[test]
    fn non_ascii_text_survives_every_parser() {
        assert_eq!(inline("**Ärger** 日本語 `é` [ü](https://x.y)"), vec![bold("Ärger"), plain(" 日本語 "), code("é"), plain(" "), linked("ü", "https://x.y")]);
        assert_eq!(blocks("# Überschrift\n- ✓ fertig"), vec![Block::Heading { level: 1, runs: vec![plain("Überschrift")] }, Block::List(vec![item(0, Marker::Bullet, vec![plain("✓ fertig")])])]);
    }

    #[test]
    fn link_ranges_count_utf16_units() {
        let runs = inline("é😀 [ab](https://x.y) z");
        let ranges = link_ranges(&runs);
        assert_eq!(ranges, vec![(4..6, "https://x.y".to_owned())]);
    }

    #[test]
    fn empty_input_has_no_blocks() {
        assert!(blocks("").is_empty());
        assert!(blocks("\n  \n").is_empty());
        assert!(inline("").is_empty());
    }

    #[test]
    fn a_pipe_table_is_one_block_between_paragraphs() {
        let got = blocks("Intro\n| a | b |\n|---|---|\n| 1 | 2 |\n\nOutro");
        assert!(matches!(got[0], Block::Paragraph(_)));
        assert!(matches!(&got[1], Block::Table { rows, .. } if rows.len() == 1));
        assert!(matches!(got[2], Block::Paragraph(_)));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn badge_lines_form_one_row_of_pictures() {
        let got = blocks("![a](https://x.org/a.svg)\n![b](https://x.org/b.svg)\n\n![c](https://x.org/c.png)");
        assert!(matches!(&got[0], Block::Images(row) if row.len() == 2));
        assert!(matches!(&got[1], Block::Images(row) if row.len() == 1));
    }

    #[test]
    fn a_picture_among_words_is_its_alt_text() {
        assert_eq!(blocks("See ![the logo](https://x.org/a.png) here"), vec![Block::Paragraph(vec![plain("See the logo here")])]);
    }

    #[test]
    fn html_descriptions_read_like_markdown() {
        let got = blocks("<h1 align=\"center\">Sodium</h1>\n<p align=\"center\"><a href=\"https://x.org\"><img src=\"https://x.org/a.png\" width=\"300\"></a></p>\n<ul><li>Fast</li><li>Free</li></ul>");
        assert_eq!(got.len(), 3);
        assert!(matches!(&got[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&got[1], Block::Images(row) if row[0].width == Some(300) && row[0].link.as_deref() == Some("https://x.org")));
        assert!(matches!(&got[2], Block::List(items) if items.len() == 2));
    }

    #[test]
    fn a_reference_style_picture_is_resolved() {
        let got = blocks("![logo][l]\n\n[l]: https://x.org/a.png");
        assert!(matches!(&got[0], Block::Images(row) if row[0].url == "https://x.org/a.png"));
    }
}
