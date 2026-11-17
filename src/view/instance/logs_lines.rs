use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;

use crate::core::i18n::{tr, trf};
use crate::core::logs::{self, Kind, LogFile};
use crate::core::worlds::game_dirs;
use crate::ui::instance_files::stamp;

/// The longest a line gets before it is cut for display.
pub const SHOWN_CHARS: usize = 2000;

/// How a line reads: plain, a warning or an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Plain,
    Warn,
    Error,
}

/// One line of a log as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: Arc<str>,
    pub level: Level,
}

/// What the file picker and the palette call `f`.
pub fn file_label(f: &LogFile, running: bool) -> String {
    match f.kind {
        Kind::Latest if running => tr("Aktuell (läuft)"),
        Kind::Latest => tr("Aktuell"),
        Kind::Older => {
            let name = f.name();
            let stem = name.trim_end_matches(".gz").trim_end_matches(".log");
            let date = stem.rsplit_once('-').map_or(stem, |(date, _)| date);
            match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
                Ok(day) => format!("{} ({})", day.format("%d.%m.%Y"), stem.rsplit_once('-').map_or("1", |(_, n)| n)),
                Err(_) => name,
            }
        }
        Kind::Crash => trf("Absturz %@", &[&stamp(f.modified)]),
    }
}

/// What the file picker shows for `f`: crash reports with the reason in a few words.
pub fn option_label(f: &LogFile, running: bool) -> String {
    match (&f.kind, &f.reason) {
        (Kind::Crash, Some(reason)) => format!("{} · {}", file_label(f, running), clip(reason, 60)),
        _ => file_label(f, running),
    }
}

/// The files in the order the picker lists them: logs first, then crash reports.
pub fn ordered(files: &[LogFile]) -> Vec<&LogFile> {
    let (crashes, logs): (Vec<&LogFile>, Vec<&LogFile>) = files.iter().partition(|f| f.kind == Kind::Crash);
    logs.into_iter().chain(crashes).collect()
}

/// How serious `line` reads.
pub fn level(line: &str) -> Level {
    if line.contains("/ERROR]") || line.contains("/FATAL]") || line.contains("Exception") || line.starts_with("\tat ") || line.starts_with("Caused by:") {
        Level::Error
    } else if line.contains("/WARN]") {
        Level::Warn
    } else {
        Level::Plain
    }
}

fn shown(line: &str) -> Arc<str> {
    let line = line.trim_end_matches('\r');
    if line.len() <= SHOWN_CHARS {
        return Arc::from(line);
    }
    let mut end = SHOWN_CHARS;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    Arc::from(format!("{}…", line.get(..end).unwrap_or_default()))
}

/// The lines of `text`, long ones cut, each with its level.
pub fn split(text: &str) -> Vec<Line> {
    text.lines().map(|l| Line { text: shown(l), level: level(l) }).collect()
}

/// The characters of the longest line.
pub fn longest(lines: &[Line]) -> usize {
    lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0)
}

/// The rows `lines` take wrapped at `cols` characters, in order: the line each row shows and the byte range of it.
pub fn wrap_rows(lines: &[Line], cols: usize) -> Vec<(usize, Range<usize>)> {
    let cols = cols.max(1);
    let mut rows = Vec::with_capacity(lines.len());
    for (at, line) in lines.iter().enumerate() {
        let text = &*line.text;
        let mut start = 0;
        let mut cuts = text.char_indices().map(|(i, _)| i).skip(cols).step_by(cols).peekable();
        while let Some(end) = cuts.next() {
            rows.push((at, start..end));
            start = end;
        }
        rows.push((at, start..text.len()));
    }
    rows
}

/// The first row of line `at` when every line before it wraps at `cols` characters.
pub fn first_row(lines: &[Line], at: usize, cols: usize) -> usize {
    let cols = cols.max(1);
    lines.iter().take(at).map(|l| l.text.chars().count().div_ceil(cols).max(1)).sum()
}

/// `text` cut to `chars` characters with an ellipsis.
pub fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", text.get(..at).unwrap_or_default()),
        None => text.to_owned(),
    }
}

/// Every place `query` occurs in `lines`, ignoring ASCII case: the line and the byte range in it.
pub fn search(lines: &[Line], query: &str) -> Vec<(usize, Range<usize>)> {
    let query = query.to_ascii_lowercase();
    let mut hits = Vec::new();
    if query.is_empty() {
        return hits;
    }
    for (at, line) in lines.iter().enumerate() {
        let lower = line.text.to_ascii_lowercase();
        let mut from = 0;
        while let Some(found) = lower.get(from..).and_then(|rest| rest.find(&query)) {
            let start = from + found;
            hits.push((at, start..start + query.len()));
            from = start + query.len().max(1);
        }
    }
    hits
}

/// The hits of line `at`, which `hits` lists in line order.
pub fn on_line(hits: &[(usize, Range<usize>)], at: usize) -> &[(usize, Range<usize>)] {
    let first = hits.partition_point(|(l, _)| *l < at);
    let rest = hits.get(first..).unwrap_or_default();
    let count = rest.iter().take_while(|(l, _)| *l == at).count();
    rest.get(..count).unwrap_or_default()
}

/// A byte range of `text` as UTF-16 offsets, which the paragraph highlights by; `None` when it cuts a character.
pub fn utf16(text: &str, range: &Range<usize>) -> Option<(usize, usize)> {
    let before = text.get(..range.start)?;
    let inside = text.get(range.clone())?;
    let start = before.encode_utf16().count();
    Some((start, start + inside.encode_utf16().count()))
}

/// The accounts that have a game folder in the instance at `dir`, those playing first and then the most recently
/// written; the account to show (`wanted` when it exists, else the first); and its files.
pub fn scan(dir: &Path, playing: &[Uuid], wanted: Option<Uuid>) -> (Vec<(Uuid, PathBuf)>, Option<Uuid>, Vec<LogFile>) {
    let mut accounts: Vec<(Uuid, PathBuf, i64)> = game_dirs(dir)
        .into_iter()
        .map(|(account, game)| {
            let at = std::fs::metadata(game.join("logs").join("latest.log")).and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_secs()).ok()).unwrap_or(0);
            (account, game, at)
        })
        .collect();
    accounts.sort_by(|a, b| playing.contains(&b.0).cmp(&playing.contains(&a.0)).then(b.2.cmp(&a.2)));
    let account = wanted.filter(|w| accounts.iter().any(|a| a.0 == *w)).or_else(|| accounts.first().map(|a| a.0));
    let found = account.and_then(|a| accounts.iter().find(|x| x.0 == a)).map(|(_, game, _)| logs::list(game)).unwrap_or_default();
    (accounts.into_iter().map(|(a, g, _)| (a, g)).collect(), account, found)
}

/// The newest log lines of `all` that fit the cap, and whether older ones were left out.
pub fn capped(mut all: Vec<Line>, cap: usize) -> (Vec<Line>, bool) {
    let cut = all.len() > cap;
    if cut {
        all.drain(..all.len() - cap);
    }
    (all, cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Line> {
        split(text)
    }

    #[test]
    fn levels_follow_the_log_format() {
        assert_eq!(level("[12:00:00] [main/ERROR]: boom"), Level::Error);
        assert_eq!(level("[12:00:00] [main/WARN]: hmm"), Level::Warn);
        assert_eq!(level("\tat net.minecraft.Foo.bar(Foo.java:1)"), Level::Error);
        assert_eq!(level("Caused by: java.lang.Oops"), Level::Error);
        assert_eq!(level("[12:00:00] [main/INFO]: fine"), Level::Plain);
    }

    #[test]
    fn long_lines_are_cut_on_a_character_boundary() {
        let long = "ä".repeat(SHOWN_CHARS);
        let split = lines(&long);
        assert!(split[0].text.ends_with('…'));
        assert!(split[0].text.len() <= SHOWN_CHARS + '…'.len_utf8());
        assert_eq!(lines("a\r\nb")[0].text.as_ref(), "a");
    }

    #[test]
    fn search_finds_every_place_ignoring_ascii_case() {
        let found = search(&lines("Foo foo\nbar\nFOO"), "foo");
        assert_eq!(found, vec![(0, 0..3), (0, 4..7), (2, 0..3)]);
        assert!(search(&lines("abc"), "").is_empty());
        assert_eq!(search(&lines("aaaa"), "aa"), vec![(0, 0..2), (0, 2..4)]);
    }

    #[test]
    fn search_survives_non_ascii_text() {
        let found = search(&lines("Ünï fo\u{1F600}o foo"), "foo");
        assert_eq!(found.len(), 1);
        assert_eq!(&"Ünï fo\u{1F600}o foo"[found[0].1.clone()], "foo");
        assert_eq!(search(&lines("日本語 日本"), "日本").len(), 2);
    }

    #[test]
    fn hits_of_one_line_are_found_by_bisecting() {
        let hits = search(&lines("a\nfoo foo\nb\nfoo"), "foo");
        assert_eq!(on_line(&hits, 1).len(), 2);
        assert_eq!(on_line(&hits, 2).len(), 0);
        assert_eq!(on_line(&hits, 3).len(), 1);
        assert_eq!(on_line(&hits, 99).len(), 0);
    }

    #[test]
    fn highlight_offsets_count_utf16_units() {
        let text = "a\u{1F600}b foo";
        let at = text.find("foo").unwrap_or(0);
        assert_eq!(utf16(text, &(at..at + 3)), Some((5, 8)));
        assert_eq!(utf16("é", &(1..2)), None);
        assert_eq!(utf16("abc", &(2..9)), None);
    }

    #[test]
    fn clipping_counts_characters() {
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("äöü", 3), "äöü");
        assert_eq!(clip("äöüx", 3), "äöü…");
    }

    fn account_logs(dir: &Path, account: Uuid, age: u64) {
        let logs = crate::core::instance::games_dir(dir).join(account.simple().to_string()).join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let path = logs.join("latest.log");
        std::fs::write(&path, "x\n").unwrap();
        let at = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(at).unwrap();
    }

    #[test]
    fn the_account_that_plays_comes_first_and_a_wish_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (Uuid::from_u128(1), Uuid::from_u128(2));
        account_logs(dir.path(), old, 500);
        account_logs(dir.path(), new, 5);
        let (accounts, shown, files) = scan(dir.path(), &[], None);
        assert_eq!(accounts.iter().map(|a| a.0).collect::<Vec<_>>(), vec![new, old]);
        assert_eq!((shown, files.len()), (Some(new), 1));
        assert_eq!(scan(dir.path(), &[old], None).1, Some(old));
        assert_eq!(scan(dir.path(), &[old], Some(new)).1, Some(new));
        assert_eq!(scan(dir.path(), &[], Some(Uuid::from_u128(9))).1, Some(new));
        assert_eq!(scan(&dir.path().join("none"), &[], None).1, None);
    }

    #[test]
    fn logs_come_before_crash_reports_in_the_picker() {
        let file = |kind, name: &str| LogFile { path: PathBuf::from(name), kind, modified: 0, size: 0, reason: None };
        let files = vec![file(Kind::Latest, "latest.log"), file(Kind::Crash, "crash.txt"), file(Kind::Older, "2026-01-01-1.log.gz")];
        let names: Vec<String> = ordered(&files).iter().map(|f| f.name()).collect();
        assert_eq!(names, ["latest.log", "2026-01-01-1.log.gz", "crash.txt"]);
        assert_eq!(file_label(&files[2], false), "01.01.2026 (1)");
    }

    #[test]
    fn only_the_newest_lines_stay() {
        let (kept, cut) = capped(lines("1\n2\n3\n4"), 2);
        assert!(cut);
        assert_eq!(kept.iter().map(|l| l.text.as_ref()).collect::<Vec<_>>(), ["3", "4"]);
        assert!(!capped(lines("1"), 2).1);
        assert_eq!(longest(&lines("ab\nabcd\nä")), 4);
    }
}
