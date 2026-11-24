//! Turns the HTML that project descriptions mix into their Markdown into Markdown, so one parser reads both.

use std::borrow::Cow;

use super::table::{MAX_COLUMNS, MAX_ROWS};

const MAX_TAG: usize = 2048;
const MAX_DEPTH: usize = 6;

const SKIPPED: [&str; 7] = ["script", "style", "svg", "iframe", "video", "object", "noscript"];
const BLOCKS: [&str; 14] = ["p", "div", "section", "article", "header", "footer", "main", "nav", "center", "details", "ul", "ol", "blockquote", "figure"];

struct Tag {
    name: String,
    closing: bool,
    attrs: Vec<(String, String)>,
    len: usize,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn parse_tag(s: &str) -> Option<Tag> {
    let rest = s.strip_prefix('<')?;
    let (closing, body) = match rest.strip_prefix('/') {
        Some(b) => (true, b),
        None => (false, rest),
    };
    let name_len = body.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(body.len());
    if name_len == 0 || !body.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    if !body[name_len..].starts_with(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>') {
        return None;
    }
    let name = body[..name_len].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut i = name_len;
    let bytes = body.as_bytes();
    let mut limit = body.len().min(MAX_TAG);
    while !body.is_char_boundary(limit) {
        limit -= 1;
    }
    loop {
        while i < limit && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= limit {
            return None;
        }
        if bytes[i] == b'>' {
            let len = 1 + usize::from(closing) + i + 1;
            return Some(Tag { name, closing, attrs, len });
        }
        let start = i;
        while i < limit && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == start {
            return None;
        }
        let key = body[start..i].to_ascii_lowercase();
        while i < limit && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < limit && bytes[i] == b'=' {
            i += 1;
            while i < limit && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < limit && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                let from = i + 1;
                let to = body[from..limit].find(quote as char)? + from;
                value = body[from..to].to_owned();
                i = to + 1;
            } else {
                let from = i;
                while i < limit && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = body[from..i].to_owned();
            }
        }
        attrs.push((key, decode(&value)));
    }
}

fn entity(s: &str) -> Option<(char, usize)> {
    let end = s.bytes().take(11).position(|b| b == b';')?;
    let name = &s[1..end];
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "copy" => '©',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code).filter(|c| !c.is_control())?
        }
    };
    Some((c, end + 1))
}

fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap_or(' ');
        if c == '&'
            && let Some((decoded, len)) = entity(&s[i..])
        {
            out.push(decoded);
            i += len;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

fn find_close(s: &str, name: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let at = (0..bytes.len().saturating_sub(name.len() + 1)).find(|&i| bytes[i] == b'<' && bytes[i + 1] == b'/' && bytes[i + 2..i + 2 + name.len()].eq_ignore_ascii_case(name.as_bytes()))?;
    let end = s[at..].find('>')? + at + 1;
    Some((at, end))
}

fn image(tag: &Tag) -> String {
    let alt = tag.attr("alt").unwrap_or("").trim().replace(['[', ']', '\n'], " ");
    let Some(src) = tag.attr("src").map(str::trim).filter(|s| !s.is_empty() && !s.contains([' ', ')', '(']) && !s.starts_with("data:")) else {
        return alt;
    };
    let width = tag.attr("width").and_then(|w| w.trim().trim_end_matches("px").parse::<u32>().ok()).filter(|w| *w > 0);
    match width {
        Some(w) => format!("![{alt}]({src}){{width={w}}}"),
        None => format!("![{alt}]({src})"),
    }
}

struct Converter {
    out: String,
    links: Vec<Option<String>>,
    ordered: Vec<bool>,
    cells: usize,
    summary: Option<usize>,
}

impl Converter {
    fn blank(&mut self) {
        while !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn open(&mut self, tag: &Tag) {
        match tag.name.as_str() {
            "br" => self.out.push_str(if self.cells > 0 { " " } else { "\n" }),
            "hr" => {
                self.blank();
                self.out.push_str("---\n\n");
            }
            "img" => self.out.push_str(&image(tag)),
            "a" => {
                let href = tag.attr("href").map(str::trim).filter(|h| !h.is_empty() && !h.contains([' ', ')', '(']) && !h.starts_with('#')).map(str::to_owned);
                if href.is_some() {
                    self.out.push('[');
                }
                self.links.push(href);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.blank();
                let level = usize::from(tag.name.as_bytes()[1] - b'0');
                self.out.push_str(&"#".repeat(level));
                self.out.push(' ');
            }
            "b" | "strong" => self.out.push_str("**"),
            "i" | "em" => self.out.push('*'),
            "code" | "kbd" => self.out.push('`'),
            "summary" => {
                self.blank();
                self.summary = Some(self.out.len());
            }
            "li" => {
                if !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
                self.out.push_str(if self.ordered.last().copied().unwrap_or(false) { "1. " } else { "- " });
            }
            "ul" | "ol" => {
                self.blank();
                self.ordered.push(tag.name == "ol");
            }
            "td" | "th" => self.cells += 1,
            name if BLOCKS.contains(&name) => self.blank(),
            _ => {}
        }
    }

    fn close(&mut self, tag: &Tag) {
        match tag.name.as_str() {
            "a" => {
                if let Some(Some(href)) = self.links.pop() {
                    self.out.push_str(&format!("]({href})"));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.blank(),
            "b" | "strong" => self.out.push_str("**"),
            "i" | "em" => self.out.push('*'),
            "code" | "kbd" => self.out.push('`'),
            "summary" => {
                if let Some(at) = self.summary.take() {
                    let body = self.out.split_off(at);
                    let text = body.trim();
                    if text.is_empty() {
                    } else if text.contains('\n') || text.starts_with('#') {
                        self.out.push_str(text);
                    } else {
                        self.out.push_str(&format!("**{text}**"));
                    }
                }
                self.blank();
            }
            "ul" | "ol" => {
                self.ordered.pop();
                self.blank();
            }
            "td" | "th" => self.cells = self.cells.saturating_sub(1),
            name if BLOCKS.contains(&name) => self.blank(),
            _ => {}
        }
    }
}

fn plain_cell(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "\\|")
}

fn is_picture(cell: &str) -> bool {
    let c = cell.trim();
    !c.is_empty() && (c.starts_with("![") || c.starts_with("[!["))
}

fn table(inner: &str, depth: usize) -> String {
    let lower = inner.to_ascii_lowercase();
    let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<tr") {
        let from = at + start;
        let Some(open) = lower[from..].find('>') else { break };
        let body_from = from + open + 1;
        let (body_to, next) = match lower[body_from..].find("</tr") {
            Some(e) => (body_from + e, lower[body_from + e..].find('>').map_or(lower.len(), |g| body_from + e + g + 1)),
            None => (lower.len(), lower.len()),
        };
        let body = &inner[body_from..body_to];
        let body_lower = &lower[body_from..body_to];
        let mut cells = Vec::new();
        let mut head = false;
        let mut c = 0;
        while let Some(s) = body_lower[c..].find("<t").map(|s| s + c) {
            let rest = &body_lower[s..];
            let is_th = rest.starts_with("<th") && !rest[3..].starts_with(|ch: char| ch.is_ascii_alphabetic());
            let is_td = rest.starts_with("<td") && !rest[3..].starts_with(|ch: char| ch.is_ascii_alphabetic());
            if !is_th && !is_td {
                c = s + 2;
                continue;
            }
            head |= is_th;
            let Some(gt) = rest.find('>') else { break };
            let cell_from = s + gt + 1;
            let name = if is_th { "</th" } else { "</td" };
            let cell_to = body_lower[cell_from..].find(name).map_or(body_lower.len(), |e| cell_from + e);
            cells.push(plain_cell(&convert(&body[cell_from..cell_to], depth + 1)));
            c = body_lower[cell_to..].find('>').map_or(body_lower.len(), |g| cell_to + g + 1);
        }
        if !cells.is_empty() {
            rows.push((head, cells));
        }
        at = next;
    }
    if rows.is_empty() {
        return String::new();
    }
    rows.truncate(MAX_ROWS);
    let width = rows.iter().map(|(_, c)| c.len()).max().unwrap_or(1);
    if width > MAX_COLUMNS || rows.iter().all(|(_, cells)| cells.iter().all(|c| is_picture(c) || c.is_empty())) {
        return rows.iter().map(|(_, cells)| cells.iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n\n");
    }
    let line = |cells: &[String]| format!("| {} |", (0..width).map(|i| cells.get(i).map_or("", String::as_str)).collect::<Vec<_>>().join(" | "));
    let mut out = String::new();
    let (first, rest) = rows.split_first().map_or((&[][..], &[][..]), |(f, r)| (&f.1[..], r));
    out.push_str(&line(first));
    out.push('\n');
    out.push_str(&format!("|{}|\n", vec![" --- "; width].join("|")));
    for (_, cells) in rest {
        out.push_str(&line(cells));
        out.push('\n');
    }
    out
}

fn convert(s: &str, depth: usize) -> String {
    let mut conv = Converter { out: String::with_capacity(s.len()), links: Vec::new(), ordered: Vec::new(), cells: 0, summary: None };
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let c = rest.chars().next().unwrap_or(' ');
        match c {
            '`' => {
                let run = rest.chars().take_while(|c| *c == '`').count();
                let ticks = "`".repeat(run);
                match rest[run..].find(&ticks) {
                    Some(end) => {
                        let to = run + end + run;
                        conv.out.push_str(&rest[..to]);
                        i += to;
                    }
                    None => {
                        conv.out.push_str(&ticks);
                        i += run;
                    }
                }
            }
            '<' if rest.starts_with("<!--") => i += rest.find("-->").map_or(rest.len(), |e| e + 3),
            '<' => match parse_tag(rest) {
                Some(tag) if SKIPPED.contains(&tag.name.as_str()) && !tag.closing => {
                    i += match find_close(&rest[tag.len..], &tag.name) {
                        Some((_, end)) => tag.len + end,
                        None => rest.len(),
                    };
                }
                Some(tag) if tag.name == "pre" && !tag.closing => {
                    let (inner, used) = match find_close(&rest[tag.len..], "pre") {
                        Some((at, end)) => (&rest[tag.len..tag.len + at], tag.len + end),
                        None => (&rest[tag.len..], rest.len()),
                    };
                    let mut code = String::new();
                    let mut j = 0;
                    while j < inner.len() {
                        match parse_tag(&inner[j..]) {
                            Some(t) => j += t.len,
                            None => {
                                let ch = inner[j..].chars().next().unwrap_or(' ');
                                code.push(ch);
                                j += ch.len_utf8();
                            }
                        }
                    }
                    conv.blank();
                    conv.out.push_str("```\n");
                    conv.out.push_str(decode(code.trim_matches('\n')).as_str());
                    conv.out.push_str("\n```\n\n");
                    i += used;
                }
                Some(tag) if tag.name == "table" && !tag.closing && depth < MAX_DEPTH => {
                    let (inner, used) = match find_close(&rest[tag.len..], "table") {
                        Some((at, end)) => (&rest[tag.len..tag.len + at], tag.len + end),
                        None => (&rest[tag.len..], rest.len()),
                    };
                    conv.blank();
                    conv.out.push_str(&table(inner, depth));
                    conv.blank();
                    i += used;
                }
                Some(tag) => {
                    if tag.closing {
                        conv.close(&tag);
                    } else {
                        conv.open(&tag);
                    }
                    i += tag.len;
                }
                None => {
                    conv.out.push('<');
                    i += 1;
                }
            },
            '&' => match entity(rest) {
                Some((decoded, len)) => {
                    conv.out.push(decoded);
                    i += len;
                }
                None => {
                    conv.out.push('&');
                    i += 1;
                }
            },
            _ => {
                conv.out.push(c);
                i += c.len_utf8();
            }
        }
    }
    conv.out
}

/// The marker of a code fence on `line`: its character, its length and whether nothing but an info string follows.
pub(super) fn fence(line: &str) -> Option<(char, usize, bool)> {
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = t.chars().take_while(|x| *x == c).count();
    if run < 3 {
        return None;
    }
    let rest = &t[run..];
    if c == '`' && rest.contains('`') {
        return None;
    }
    Some((c, run, rest.trim().is_empty()))
}

/// `text` with its HTML as the Markdown it stands for: links, pictures, headings, emphasis, lists, tables and line breaks.
/// Code stays as written; scripts, styles and anything else that cannot be shown are dropped.
pub fn normalize(text: &str) -> Cow<'_, str> {
    if !text.contains('<') && !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chunk = String::new();
    let mut inside: Option<(char, usize)> = None;
    for line in text.lines() {
        let marker = fence(line);
        let closes = matches!((inside, marker), (Some((c, n)), Some((mc, mn, true))) if c == mc && mn >= n);
        if closes || (inside.is_none() && marker.is_some()) {
            if inside.is_none() {
                out.push_str(&convert(&chunk, 0));
                chunk.clear();
            }
            inside = if closes { None } else { marker.map(|(c, n, _)| (c, n)) };
            out.push_str(line);
            out.push('\n');
        } else if inside.is_some() {
            out.push_str(line);
            out.push('\n');
        } else {
            chunk.push_str(line);
            chunk.push('\n');
        }
    }
    out.push_str(&convert(&chunk, 0));
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(s: &str) -> String {
        normalize(s).trim().to_owned()
    }

    #[test]
    fn a_long_multibyte_attribute_does_not_panic() {
        let long = format!("<a title=\"{}\">x</a>", "ä".repeat(1100));
        let _ = md(&long);
        let _ = md(&format!("<a  {}>", "é".repeat(1100)));
    }

    #[test]
    fn closing_tags_are_found_in_any_case() {
        assert_eq!(find_close("a <PRE>x</PrE> b", "pre"), Some((8, 14)));
        assert!(find_close("a <PRE>x</PrE> b", "pre").is_some());
        assert!(find_close("a <PRE>x", "pre").is_none());
    }

    #[test]
    fn a_wide_table_is_flattened() {
        let cells = "<td>x</td>".repeat(MAX_COLUMNS + 1);
        assert!(!md(&format!("<table><tr>{cells}</tr><tr><td>y</td></tr></table>")).contains("---"));
    }

    #[test]
    fn plain_markdown_is_untouched() {
        assert!(matches!(normalize("# Hi\n\n- a\n- b"), Cow::Borrowed(_)));
    }

    #[test]
    fn inline_tags_become_their_markdown() {
        assert_eq!(md("<b>bold</b> and <i>soft</i> and <code>x</code>"), "**bold** and *soft* and `x`");
    }

    #[test]
    fn a_linked_picture_keeps_both() {
        assert_eq!(md(r#"<a href="https://x.org"><img src="https://x.org/a.png" alt="Logo" width="200"></a>"#), "[![Logo](https://x.org/a.png){width=200}](https://x.org)");
    }

    #[test]
    fn headings_and_paragraphs_get_blank_lines() {
        assert_eq!(md("<h2>Title</h2><p>One</p><p>Two</p>"), "## Title\n\nOne\n\nTwo");
    }

    #[test]
    fn centered_blocks_lose_the_tag_and_keep_the_text() {
        assert_eq!(md("<div align=\"center\"><h1>Sodium</h1></div>"), "# Sodium");
    }

    #[test]
    fn lists_become_markdown_lists() {
        assert_eq!(md("<ul><li>a</li><li>b</li></ul>"), "- a\n- b");
        assert_eq!(md("<ol><li>a</li><li>b</li></ol>"), "1. a\n1. b");
    }

    #[test]
    fn entities_decode_and_stay_out_of_code() {
        assert_eq!(md("fish &amp; chips &#169; &lt;3"), "fish & chips © <3");
        assert_eq!(md("x `a &amp; <b>` y"), "x `a &amp; <b>` y");
    }

    #[test]
    fn fenced_code_is_left_alone() {
        let src = "<b>a</b>\n```html\n<b>b</b>\n```\n<i>c</i>";
        assert_eq!(md(src), "**a**\n```html\n<b>b</b>\n```\n*c*");
    }

    #[test]
    fn pre_blocks_become_fences() {
        assert_eq!(md("<pre><code>a &lt; b\nc</code></pre>"), "```\na < b\nc\n```");
    }

    #[test]
    fn scripts_comments_and_unknown_tags_vanish() {
        assert_eq!(md("a<script>alert(1)</script><!-- note -->b<blink>c</blink>"), "abc");
    }

    #[test]
    fn a_lone_angle_bracket_and_autolinks_stay() {
        assert_eq!(md("1 < 2 and <https://x.org> and a<b"), "1 < 2 and <https://x.org> and a<b");
    }

    #[test]
    fn a_table_becomes_a_pipe_table() {
        let got = md("<table><tr><th>Mod</th><th>Use</th></tr><tr><td>Sodium</td><td>Speed | fps</td></tr></table>");
        assert_eq!(got, "| Mod | Use |\n| --- | --- |\n| Sodium | Speed \\| fps |");
    }

    #[test]
    fn one_line_backtick_spans_are_not_fences() {
        let src = "<center>\n\n```Early access```<br>\n[<img src=\"https://x.org/a.png\" alt=\"A\"/>](https://x.org)\n\n</center>\n\n<details>\n<summary>list</summary>\n\n```\nA, B\n```\n</details>\n\n<b>after</b>";
        let got = md(src);
        assert!(got.contains("[![A](https://x.org/a.png)](https://x.org)"), "{got}");
        assert!(got.contains("**list**") && got.contains("**after**"), "{got}");
        assert!(!got.contains("<"), "{got}");
    }

    #[test]
    fn a_summary_that_is_a_heading_stays_a_heading() {
        let got = md("<details><summary><h2>Features</h2></summary>\n\n- one\n</details>");
        assert!(got.contains("## Features") && !got.contains("**"), "{got}");
    }

    #[test]
    fn a_longer_fence_is_closed_only_by_an_equal_or_longer_one() {
        let src = "````\n```\n<b>x</b>\n````\n<b>y</b>";
        assert_eq!(md(src), "````\n```\n<b>x</b>\n````\n**y**");
    }

    #[test]
    fn a_summary_with_a_heading_inside_does_not_leave_loose_stars() {
        let got = md("<details><summary> <img src=\"https://x.org/a.png\" width=\"64\"> <br>\n\n### All <br> <a href=\"https://x.org\">go</a>\n\n</summary>\n\n- one\n</details>");
        assert!(!got.contains("**"), "{got}");
        assert!(got.contains("### All"), "{got}");
    }

    #[test]
    fn a_table_of_pictures_is_a_row_of_pictures() {
        let got = md(r#"<table><tr><td><img src="https://x.org/a.png"></td><td><img src="https://x.org/b.png"></td></tr></table>"#);
        assert_eq!(got, "![](https://x.org/a.png) ![](https://x.org/b.png)");
    }

    #[test]
    fn a_picture_without_a_usable_source_is_its_alt_text() {
        assert_eq!(md(r#"<img src="data:image/png;base64,AA" alt="x">"#), "x");
    }

    #[test]
    fn summary_becomes_a_bold_line_and_details_stay_open() {
        assert_eq!(md("<details><summary>More</summary>Text</details>"), "**More**\n\nText");
    }

    #[test]
    fn unclosed_and_malformed_tags_do_not_loop() {
        assert_eq!(md("<a href=\"x"), "<a href=\"x");
        let _ = md(&"<div>".repeat(500));
        let _ = md(&"<table><tr><td>".repeat(50));
    }

    #[test]
    fn non_ascii_text_survives() {
        assert_eq!(md("<p>Größe — ñ 日本</p>"), "Größe — ñ 日本");
    }
}