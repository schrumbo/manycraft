//! Pipe tables: a header row, a delimiter row that sets each column's alignment, then the body rows.

use freya::prelude::*;

use super::{Block, Prose, Run, inline};
use crate::view::kit::use_theme;
use crate::view::theme::{Theme, font, space};

const LONGEST: (usize, usize) = (8, 48);
const CELL_PADDING: f32 = 8.;

/// How the text of a table column lines up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

fn split(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut ticks = false;
    let mut chars = line.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '`' => {
                ticks = !ticks;
                cell.push(c);
            }
            '|' if !ticks => cells.push(std::mem::take(&mut cell)),
            _ => cell.push(c),
        }
    }
    cells.push(cell);
    if line.trim_start().starts_with('|') && cells.first().is_some_and(|c| c.trim().is_empty()) {
        cells.remove(0);
    }
    if line.trim_end().ends_with('|') && !line.trim_end().ends_with("\\|") && cells.last().is_some_and(|c| c.trim().is_empty()) {
        cells.pop();
    }
    cells.into_iter().map(|c| c.trim().to_owned()).collect()
}

fn delimiter(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') {
        return None;
    }
    split(line)
        .iter()
        .map(|cell| {
            let dashes = cell.trim_matches(':');
            if dashes.is_empty() || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
        })
        .collect()
}

pub(super) const MAX_COLUMNS: usize = 32;
pub(super) const MAX_ROWS: usize = 500;

/// The table that starts at `lines[from]` and the line after it, when there is one.
pub(super) fn parse(lines: &[&str], from: usize) -> Option<(Block, usize)> {
    let head_line = lines.get(from)?;
    if !head_line.contains('|') {
        return None;
    }
    let align = delimiter(lines.get(from + 1)?)?;
    let head = split(head_line);
    if head.len() != align.len() || align.len() > MAX_COLUMNS {
        return None;
    }
    let mut rows = Vec::new();
    let mut at = from + 2;
    while let Some(line) = lines.get(at).filter(|l| !l.trim().is_empty() && l.contains('|')) {
        if rows.len() >= MAX_ROWS {
            return None;
        }
        let mut cells = split(line);
        cells.resize(align.len(), String::new());
        rows.push(cells.iter().map(|c| inline(c)).collect());
        at += 1;
    }
    Some((Block::Table { align, head: head.iter().map(|c| inline(c)).collect(), rows }, at))
}

fn width_of(runs: &[Run]) -> usize {
    runs.iter().map(|r| r.text.chars().count()).sum()
}

fn longest_word(runs: &[Run]) -> usize {
    let text: String = runs.iter().map(|r| r.text.as_str()).collect();
    text.split_whitespace().map(|w| w.chars().count()).max().unwrap_or(0)
}

#[derive(PartialEq)]
struct Row {
    cells: Vec<Vec<Run>>,
    align: Vec<Align>,
    weights: Vec<f32>,
    header: bool,
    last: bool,
    color: Color,
}

impl Component for Row {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut row = rect().width(Size::fill()).horizontal().content(Content::flex());
        if self.header {
            row = row.background(t.raised);
        }
        if !self.last {
            row = row.border(Border::new().fill(t.separator).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner));
        }
        for (c, runs) in self.cells.iter().enumerate() {
            let align = match self.align.get(c) {
                Some(Align::Center) => TextAlign::Center,
                Some(Align::Right) => TextAlign::Right,
                _ => TextAlign::Left,
            };
            let (weight, ink) = if self.header { (FontWeight::SEMI_BOLD, t.text1) } else { (FontWeight::NORMAL, self.color) };
            row = row.child(rect().width(Size::flex(self.weights.get(c).copied().unwrap_or(1.))).padding((space::S, space::M)).child(Prose { runs: runs.clone(), size: font::CALLOUT, weight, color: ink, align }));
        }
        row
    }
}

/// The table as rows in a rounded frame, each column as wide as its longest cell up to a limit.
pub(super) fn view(t: &Theme, align: &[Align], head: &[Vec<Run>], rows: &[Vec<Vec<Run>>], color: Color) -> Element {
    let weights: Vec<f32> = (0..align.len())
        .map(|c| {
            let cells = || std::iter::once(head.get(c)).chain(rows.iter().map(|r| r.get(c))).flatten();
            let longest = cells().map(|runs| width_of(runs)).max().unwrap_or(0).clamp(LONGEST.0, LONGEST.1);
            let word = cells().map(|runs| longest_word(runs)).max().unwrap_or(0).min(LONGEST.1);
            longest.max(word) as f32 + CELL_PADDING
        })
        .collect();
    let row = |cells: &[Vec<Run>], header: bool, last: bool| Row { cells: cells.to_vec(), align: align.to_vec(), weights: weights.clone(), header, last, color }.into_element();
    rect()
        .width(Size::fill())
        .corner_radius(t.shape.control)
        .overflow(Overflow::Clip)
        .border(Border::new().fill(t.separator).width(1.).alignment(BorderAlignment::Inner))
        .child(row(head, true, rows.is_empty()))
        .children(rows.iter().enumerate().map(|(i, cells)| row(cells, false, i + 1 == rows.len())))
        .into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_word_of_the_runs_counts() {
        let runs = inline("short **extraordinary** tail");
        assert_eq!(longest_word(&runs), "extraordinary".len());
        assert_eq!(longest_word(&[]), 0);
    }

    #[test]
    fn a_table_beyond_the_limits_stays_text() {
        let wide = vec!["a"; MAX_COLUMNS + 1].join("|");
        let rule = vec!["-"; MAX_COLUMNS + 1].join("|");
        assert!(parse(&[&wide, &rule], 0).is_none());
        let mut tall = vec!["a|b", "-|-"];
        tall.extend(std::iter::repeat_n("c|d", MAX_ROWS + 1));
        assert!(parse(&tall, 0).is_none());
    }

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    fn texts(cells: &[Vec<Run>]) -> Vec<String> {
        cells.iter().map(|c| c.iter().map(|r| r.text.as_str()).collect()).collect()
    }

    #[test]
    fn a_table_has_a_header_a_delimiter_and_rows() {
        let src = lines("| Mod | Use |\n| --- | :-: |\n| Sodium | Speed |\n| Iris | Shaders |\n\nafter");
        let (Block::Table { align, head, rows }, next) = parse(&src, 0).unwrap() else { panic!("not a table") };
        assert_eq!(align, [Align::Left, Align::Center]);
        assert_eq!(texts(&head), ["Mod", "Use"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(texts(&rows[1]), ["Iris", "Shaders"]);
        assert_eq!(next, 4);
    }

    #[test]
    fn pipes_at_the_edges_are_optional() {
        let src = lines("a | b\n--- | ---:\n1 | 2");
        let (Block::Table { align, rows, .. }, _) = parse(&src, 0).unwrap() else { panic!("not a table") };
        assert_eq!(align, [Align::Left, Align::Right]);
        assert_eq!(texts(&rows[0]), ["1", "2"]);
    }

    #[test]
    fn short_rows_are_padded_and_long_rows_cut() {
        let src = lines("| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |");
        let (Block::Table { rows, .. }, _) = parse(&src, 0).unwrap() else { panic!("not a table") };
        assert_eq!(texts(&rows[0]), ["1", ""]);
        assert_eq!(texts(&rows[1]), ["1", "2"]);
    }

    #[test]
    fn an_escaped_pipe_and_code_keep_their_pipe() {
        assert_eq!(split("| a \\| b | `c | d` |"), ["a | b", "`c | d`"]);
    }

    #[test]
    fn text_without_a_delimiter_row_is_no_table() {
        assert!(parse(&lines("a | b\nnot a delimiter"), 0).is_none());
        assert!(parse(&lines("| a | b |\n| --- |"), 0).is_none());
        assert!(parse(&lines("just text\n---"), 0).is_none());
    }
}