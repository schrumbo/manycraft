use std::ops::Range;

use super::logs_lines::Line;

/// A place in a log: a line and a byte offset in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Spot {
    pub line: usize,
    pub at: usize,
}

/// The text between two spots; `anchor` is where it began and `head` where it ends now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Spot,
    pub head: Spot,
}

#[derive(PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// The byte offset in `text` of the UTF-16 offset `units`, which is how the text engine counts; held to the end.
pub fn byte_at(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (at, c) in text.char_indices() {
        if seen >= units {
            return at;
        }
        seen += c.len_utf16();
    }
    text.len()
}

fn word_around(text: &str, at: usize) -> Range<usize> {
    let at = at.min(text.len());
    let probe = text.get(at..).and_then(|rest| rest.chars().next()).or_else(|| text.get(..at).and_then(|before| before.chars().next_back()));
    let Some(kind) = probe.map(class) else { return at..at };
    let mut start = at;
    while let Some(c) = text.get(..start).and_then(|b| b.chars().next_back()).filter(|c| class(*c) == kind) {
        start -= c.len_utf8();
    }
    let mut end = at;
    while let Some(c) = text.get(end..).and_then(|r| r.chars().next()).filter(|c| class(*c) == kind) {
        end += c.len_utf8();
    }
    start..end
}

impl Selection {
    /// A selection that holds nothing yet, from which a drag begins.
    pub fn caret(at: Spot) -> Self {
        Self { anchor: at, head: at }
    }

    /// The word, run of spaces or run of signs around `at`.
    pub fn word(lines: &[Line], at: Spot) -> Self {
        let text = lines.get(at.line).map_or("", |l| &l.text);
        let range = word_around(text, at.at);
        Self { anchor: Spot { line: at.line, at: range.start }, head: Spot { line: at.line, at: range.end } }
    }

    /// The whole of line `line`.
    pub fn line(lines: &[Line], line: usize) -> Self {
        let len = lines.get(line).map_or(0, |l| l.text.len());
        Self { anchor: Spot { line, at: 0 }, head: Spot { line, at: len } }
    }

    /// Every line; `None` for an empty log.
    pub fn all(lines: &[Line]) -> Option<Self> {
        let last = lines.len().checked_sub(1)?;
        Some(Self { anchor: Spot { line: 0, at: 0 }, head: Spot { line: last, at: lines[last].text.len() } })
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    fn bounds(&self) -> (Spot, Spot) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    /// Whether `spot` lies inside the selected text.
    pub fn covers(&self, spot: Spot) -> bool {
        let (start, end) = self.bounds();
        start <= spot && spot < end
    }

    /// The selected part of a row that shows the bytes `part` of line `line`, as byte offsets in that row.
    pub fn on_row(&self, line: usize, part: &Range<usize>) -> Option<Range<usize>> {
        let (start, end) = self.bounds();
        if line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line { start.at } else { 0 }.max(part.start);
        let to = if line == end.line { end.at } else { usize::MAX }.min(part.end);
        (from < to).then(|| from - part.start..to - part.start)
    }

    /// The selected text, the lines it spans joined by line breaks.
    pub fn text(&self, lines: &[Line]) -> String {
        let (start, end) = self.bounds();
        let mut out = String::new();
        for at in start.line..=end.line.min(lines.len().saturating_sub(1)) {
            let Some(line) = lines.get(at) else { break };
            let from = if at == start.line { start.at } else { 0 };
            let to = if at == end.line { end.at.min(line.text.len()) } else { line.text.len() };
            out.push_str(line.text.get(from..to).unwrap_or_default());
            if at < end.line {
                out.push('\n');
            }
        }
        out
    }

    /// The selection after `over` lines left the front of the log; `None` when it lay in them.
    pub fn shifted(self, over: usize) -> Option<Self> {
        let moved = |s: Spot| if s.line < over { Spot { line: 0, at: 0 } } else { Spot { line: s.line - over, at: s.at } };
        let (start, end) = self.bounds();
        (end.line >= over).then(|| {
            let (a, b) = (moved(start), moved(end));
            if self.anchor <= self.head { Self { anchor: a, head: b } } else { Self { anchor: b, head: a } }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::logs_lines::split;
    use super::*;

    fn spot(line: usize, at: usize) -> Spot {
        Spot { line, at }
    }

    fn range(a: Spot, b: Spot) -> Selection {
        Selection { anchor: a, head: b }
    }

    #[test]
    fn text_joins_the_spanned_lines_and_cuts_the_ends_whichever_way_it_was_dragged() {
        let log = split("alpha beta\ngamma\ndelta epsilon");
        let forward = range(spot(0, 6), spot(2, 5));
        assert_eq!(forward.text(&log), "beta\ngamma\ndelta");
        assert_eq!(range(spot(2, 5), spot(0, 6)).text(&log), forward.text(&log));
        assert_eq!(range(spot(1, 1), spot(1, 3)).text(&log), "am");
        assert_eq!(Selection::all(&log).map(|s| s.text(&log)), Some("alpha beta\ngamma\ndelta epsilon".into()));
        assert_eq!(Selection::all(&[]), None);
    }

    #[test]
    fn a_wrapped_row_shows_only_its_own_part_of_the_selection() {
        let selection = range(spot(1, 4), spot(3, 2));
        assert_eq!(selection.on_row(0, &(0..10)), None);
        assert_eq!(selection.on_row(1, &(0..6)), Some(4..6));
        assert_eq!(selection.on_row(1, &(6..12)), Some(0..6));
        assert_eq!(selection.on_row(2, &(0..8)), Some(0..8));
        assert_eq!(selection.on_row(3, &(0..9)), Some(0..2));
        assert_eq!(selection.on_row(3, &(2..9)), None);
        assert_eq!(selection.on_row(4, &(0..9)), None);
        assert_eq!(Selection::caret(spot(1, 1)).on_row(1, &(0..5)), None);
    }

    #[test]
    fn a_double_click_takes_the_word_a_run_of_signs_or_a_run_of_spaces() {
        let log = split("[12:00:01] Loaded  chunk_7: ok\nüber alles");
        let at = |line, at| Selection::word(&log, spot(line, at)).text(&log);
        assert_eq!(at(0, 2), "12");
        assert_eq!(at(0, 0), "[");
        assert_eq!(at(0, 15), "Loaded");
        assert_eq!(at(0, 17), "  ");
        assert_eq!(at(0, 22), "chunk_7");
        assert_eq!(at(0, 29), "ok");
        assert_eq!(at(0, 30), "ok");
        assert_eq!(at(1, 2), "über");
        assert_eq!(Selection::line(&log, 1).text(&log), "über alles");
    }

    #[test]
    fn utf16_offsets_become_byte_offsets() {
        assert_eq!(byte_at("aüb", 0), 0);
        assert_eq!(byte_at("aüb", 2), 3);
        assert_eq!(byte_at("a😀b", 3), 5);
        assert_eq!(byte_at("ab", 9), 2);
    }

    #[test]
    fn lines_leaving_the_front_move_the_selection_or_drop_it() {
        let selection = range(spot(5, 2), spot(8, 4));
        assert_eq!(selection.shifted(3), Some(range(spot(2, 2), spot(5, 4))));
        assert_eq!(range(spot(8, 4), spot(5, 2)).shifted(3), Some(range(spot(5, 4), spot(2, 2))));
        assert_eq!(selection.shifted(6), Some(range(spot(0, 0), spot(2, 4))));
        assert_eq!(selection.shifted(9), None);
    }
}
