//! Markdown for project descriptions: a small parser into blocks and styled runs, and a renderer in the theme's fonts.
//! Links open in the browser; text is not selectable.

use std::ops::Range;

use freya::engine::prelude::{RectHeightStyle, RectWidthStyle};
use freya::prelude::*;
use freya_core::elements::paragraph::ParagraphHolderInner;

use super::text::{hairline, line};
use super::{open_external, use_theme};
use crate::view::theme::{Theme, faces, font, frame, space};

const MAX_QUOTE_DEPTH: usize = 4;
const MAX_NESTING: usize = 8;
const MAX_INDENT: usize = 4;
const MARKER_W: f32 = 22.;
const QUOTE_BAR: f32 = 2.;
const TAB_WIDTH: usize = 4;

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
    while j < c.len() {
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
    let mut j = start;
    while j < c.len() {
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
    let mut j = at + 1;
    while j < c.len() {
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
                if let Some((_, end)) = code_span(c, j) {
                    j = end - 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if j >= c.len() || c.get(j + 1) != Some(&'(') {
        return None;
    }
    let mut parens = 1;
    let mut k = j + 2;
    while k < c.len() {
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
    if k >= c.len() {
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
        out.push(Block::Paragraph(inline(&para.join(" "))));
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
        } else if item_of(line).is_some() {
            flush(&mut out, &mut para);
            let (list, next) = list_from(lines, i, depth);
            out.push(list);
            i = next.max(i + 1);
        } else {
            para.push(line.trim());
            i += 1;
        }
    }
    flush(&mut out, &mut para);
    out
}

/// The blocks of `text`: headings, paragraphs joined across line breaks, lists, quotes, code fences and rules.
pub fn blocks(text: &str) -> Vec<Block> {
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

fn url_at(holder: &ParagraphHolder, links: &[(Range<usize>, String)], at: CursorPoint) -> Option<String> {
    let at = holder.visible_location(at);
    let inner = holder.0.borrow();
    let ParagraphHolderInner { paragraph, scale_factor, .. } = inner.as_ref()?;
    let (x, y) = ((at.x * scale_factor) as f32, (at.y * scale_factor) as f32);
    links
        .iter()
        .find(|(range, _)| paragraph.get_rects_for_range(range.clone(), RectHeightStyle::Tight, RectWidthStyle::Tight).iter().any(|b| b.rect.left <= x && x <= b.rect.right && b.rect.top <= y && y <= b.rect.bottom))
        .map(|(_, url)| url.clone())
}

#[derive(PartialEq)]
struct Prose {
    runs: Vec<Run>,
    size: f32,
    weight: FontWeight,
    color: Color,
}

impl Component for Prose {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let holder = use_hook(ParagraphHolder::default);
        let mut hovering = use_state(|| false);
        let links = link_ranges(&self.runs);
        let mut p = paragraph().width(Size::fill()).line_height(frame::PARA_LINE).font_size(self.size).color(self.color).font_weight(self.weight).holder(holder.clone());
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
        let (over, press, moved) = (links.clone(), links, holder.clone());
        p.cursor(if *hovering.read() { CursorIcon::Pointer } else { CursorIcon::Default })
            .on_pointer_move(move |e: Event<PointerEventData>| hovering.set_if_modified(url_at(&moved, &over, e.element_location()).is_some()))
            .on_pointer_leave(move |_| hovering.set_if_modified(false))
            .on_pointer_press(move |e: Event<PointerEventData>| {
                if !e.data().is_primary() {
                    return;
                }
                if let Some(url) = url_at(&holder, &press, e.element_location()) {
                    e.stop_propagation();
                    open_external("project link", url);
                }
            })
    }
}

fn prose(runs: &[Run], size: f32, weight: FontWeight, color: Color) -> Element {
    Prose { runs: runs.to_vec(), size, weight, color }.into_element()
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
                    .child(rect().width(Size::px(MARKER_W)).child(line(t, marker_text(item.marker), font::BODY, t.text2).font_tabular()))
                    .child(rect().width(Size::flex(1.)).child(prose(&item.runs, font::BODY, FontWeight::NORMAL, color)))
                    .into_element()
            }))
            .into_element(),
        Block::Quote(inner) => rect()
            .width(Size::fill())
            .horizontal()
            .spacing(space::S)
            .content(Content::flex())
            .child(rect().width(Size::px(QUOTE_BAR)).height(Size::fill()).corner_radius(QUOTE_BAR / 2.).background(t.separator))
            .child(rect().width(Size::flex(1.)).child(view(t, inner, t.text2)))
            .into_element(),
        Block::Code(text) => code_block(t, text),
        Block::Rule => hairline(t),
    }
}

/// `blocks` as a column, body text in `color`.
pub fn view(t: &Theme, blocks: &[Block], color: Color) -> Element {
    rect().width(Size::fill()).spacing(space::M).children(blocks.iter().map(|b| view_block(t, b, color))).into_element()
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
}
