//! Logs and crash reports in a game folder: listing, reading, following, redacting and uploading to mclo.gs.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

const MAX_LOG: u64 = 256 << 20;
const REASON_BYTES: u64 = 64 << 10;
const UPLOAD_URL: &str = "https://api.mclo.gs/1/log";
const UPLOAD_LINES: usize = 25_000;
const UPLOAD_BYTES: usize = 10 << 20;
const HIDDEN: &str = "***";
const SECRET_KEYS: [&str; 9] = ["accesstoken", "access_token", "clienttoken", "refreshtoken", "refresh_token", "xuid", "clientid", "sessionid", "session_id"];
const USER_VARIABLES: [&str; 3] = ["username=", "user=", "logname="];

/// What a file in the logs list is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// logs/latest.log, written by the game that runs or ran last.
    Latest,
    /// An earlier log, often gzipped.
    Older,
    /// A crash report of the game or of Java.
    Crash,
}

/// One log or crash report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFile {
    pub path: PathBuf,
    pub kind: Kind,
    /// Unix seconds.
    pub modified: i64,
    pub size: u64,
    /// For a crash: the line that says why, readable first.
    pub reason: Option<String>,
}

impl LogFile {
    /// The file name.
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// The logs of the game folder `game_dir`: latest.log first, then older logs and crash reports, newest first.
pub fn list(game_dir: &Path) -> Vec<LogFile> {
    let mut files = Vec::new();
    let mut add = |path: PathBuf, kind: Kind| {
        let Ok(meta) = std::fs::metadata(&path) else { return };
        if !meta.is_file() {
            return;
        }
        let modified = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_secs()).ok()).unwrap_or(0);
        let reason = (kind == Kind::Crash).then(|| head(&path).ok().and_then(|text| crash_reason(&redact(&text)))).flatten();
        files.push(LogFile { path, kind, modified, size: meta.len(), reason });
    };
    for (dir, crash) in [(game_dir.join("logs"), false), (game_dir.join("crash-reports"), true), (game_dir.to_path_buf(), true)] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let path = entry.path();
            if dir == game_dir {
                if name.starts_with("hs_err_pid") && name.ends_with(".log") {
                    add(path, Kind::Crash);
                }
            } else if crash {
                if name.ends_with(".txt") {
                    add(path, Kind::Crash);
                }
            } else if name == "latest.log" {
                add(path, Kind::Latest);
            } else if name.ends_with(".log") || name.ends_with(".log.gz") {
                add(path, Kind::Older);
            }
        }
    }
    files.sort_by(|a, b| (a.kind != Kind::Latest).cmp(&(b.kind != Kind::Latest)).then_with(|| b.modified.cmp(&a.modified)).then_with(|| b.path.cmp(&a.path)));
    files
}

fn head(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(REASON_BYTES).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The whole text of the log at `path`, ungzipped, invalid UTF-8 replaced.
pub fn read(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut bytes = Vec::new();
    if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("gz")) {
        flate2::read::GzDecoder::new(std::io::BufReader::new(file)).take(MAX_LOG + 1).read_to_end(&mut bytes).with_context(|| format!("cannot unpack {}", path.display()))?;
    } else {
        file.take(MAX_LOG + 1).read_to_end(&mut bytes).with_context(|| format!("cannot read {}", path.display()))?;
    }
    if bytes.len() as u64 > MAX_LOG {
        bail!("{} is larger than 256 MB", path.display());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// When `path` was created, to tell a log that started over from one that only grew.
pub fn born(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.created()).ok()
}

/// The complete lines written to `path` since byte `offset`, and the offset after them. A file that got shorter
/// started over, so it is read from the start; `None` then tells the caller to drop what it has.
pub fn read_from(path: &Path, offset: u64) -> Result<(Option<u64>, String, u64)> {
    let mut file = std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let len = file.metadata()?.len();
    let (restart, from) = if len < offset { (true, 0) } else { (false, offset) };
    if len == from {
        return Ok((Some(from).filter(|_| !restart), String::new(), from));
    }
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.take((len - from).min(MAX_LOG)).read_to_end(&mut bytes)?;
    let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |at| at + 1);
    bytes.truncate(end);
    let next = from + end as u64;
    Ok((Some(from).filter(|_| !restart), String::from_utf8_lossy(&bytes).into_owned(), next))
}

/// The readable reason of a crash report: Minecraft's description with its exception, or what Java says killed it.
pub fn crash_reason(text: &str) -> Option<String> {
    if text.contains("---- Minecraft Crash Report ----") {
        let mut lines = text.lines().map(str::trim);
        let description = lines.by_ref().find_map(|l| l.strip_prefix("Description:")).map(str::trim).filter(|d| !d.is_empty());
        let cause = lines.find(|l| !l.is_empty()).filter(|l| !l.starts_with("at ") && (l.contains("Exception") || l.contains("Error") || l.contains("Throwable")));
        return match (description, cause) {
            (Some(d), Some(c)) => Some(format!("{d}: {c}")),
            (Some(d), None) => Some(d.to_owned()),
            (None, Some(c)) => Some(c.to_owned()),
            (None, None) => None,
        };
    }
    if text.contains("insufficient memory for the Java Runtime Environment") {
        return Some(super::i18n::tr("Java hatte zu wenig Arbeitsspeicher."));
    }
    if !text.contains("A fatal error has been detected by the Java Runtime Environment") {
        return None;
    }
    let lines: Vec<&str> = text.lines().map(|l| l.trim_start_matches('#').trim()).collect();
    let start = lines.iter().position(|l| l.contains("A fatal error has been detected"))?;
    let error = lines.iter().skip(start + 1).find(|l| !l.is_empty())?;
    let error = error.split(" at pc=").next().unwrap_or(error).trim();
    let frame = lines.iter().position(|l| l.starts_with("Problematic frame:")).and_then(|at| lines.get(at + 1)).and_then(|l| {
        let inside = l.split_once('[')?.1.split(']').next()?;
        Some(inside.split('+').next().unwrap_or(inside).trim().to_owned())
    });
    Some(match frame.filter(|f| !f.is_empty()) {
        Some(frame) => format!("{error} in {frame}"),
        None => error.to_owned(),
    })
}

/// `text` with access tokens, session ids, other secrets, user names in home paths and email addresses replaced by ***.
pub fn redact(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let b = text.as_bytes();
    let mut spans = Vec::new();
    secrets(&lower, b, &mut spans);
    user_variables(&lower, b, &mut spans);
    tokens(b, &mut spans);
    emails(b, &mut spans);
    homes(&lower, b, &mut spans);
    if let Some(home) = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).filter(|h| h.len() > 3) {
        for variant in [home.clone(), home.replace('\\', "/")] {
            let at_lower = variant.to_ascii_lowercase();
            spans.extend(lower.match_indices(&at_lower).map(|(at, m)| {
                let name = m.rfind(['\\', '/']).map_or(0, |s| s + 1);
                at + name..at + m.len()
            }));
        }
    }
    splice(text, spans)
}

fn splice(text: &str, mut spans: Vec<Range<usize>>) -> String {
    if spans.is_empty() {
        return text.to_owned();
    }
    spans.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for span in spans {
        if span.end <= at || span.is_empty() {
            continue;
        }
        let start = span.start.max(at);
        let (Some(before), true) = (text.get(at..start), text.is_char_boundary(span.end)) else { continue };
        out.push_str(before);
        out.push_str(HIDDEN);
        at = span.end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    out
}

fn value_end(b: &[u8], from: usize) -> usize {
    b.iter().skip(from).position(|c| c.is_ascii_whitespace() || matches!(c, b'"' | b'\'' | b',' | b'}' | b')' | b']' | b';' | b'&')).map_or(b.len(), |n| from + n)
}

fn secrets(lower: &str, b: &[u8], spans: &mut Vec<Range<usize>>) {
    for key in SECRET_KEYS {
        for (at, _) in lower.match_indices(key) {
            let mut from = at + key.len();
            while matches!(b.get(from), Some(b'"' | b'\'')) {
                from += 1;
            }
            let gap = from;
            while matches!(b.get(from), Some(b' ' | b'\t')) {
                from += 1;
            }
            if matches!(b.get(from), Some(b':' | b'=')) {
                from += 1;
            } else if from == gap {
                continue;
            }
            while matches!(b.get(from), Some(b' ' | b'\t' | b'"' | b'\'')) {
                from += 1;
            }
            let end = value_end(b, from);
            if end > from {
                spans.push(from..end);
            }
        }
    }
}

fn user_variables(lower: &str, b: &[u8], spans: &mut Vec<Range<usize>>) {
    for key in USER_VARIABLES {
        for (at, _) in lower.match_indices(key) {
            if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') {
                continue;
            }
            let from = at + key.len();
            let end = value_end(b, from);
            if end > from {
                spans.push(from..end);
            }
        }
    }
}

fn tokens(b: &[u8], spans: &mut Vec<Range<usize>>) {
    let base64 = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'+' | b'/' | b'=');
    let mut at = 0;
    while at + 6 < b.len() {
        let rest = &b[at..];
        if rest.starts_with(b"token:") {
            let from = at + 6;
            let end = value_end(b, from);
            if end > from {
                spans.push(from..end);
            }
            at = end.max(at + 1);
            continue;
        }
        let starts_word = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || matches!(b[at - 1], b'-' | b'_'));
        if starts_word && rest.starts_with(b"eyJ") {
            let end = at + rest.iter().position(|c| !base64(*c)).unwrap_or(rest.len());
            let dots = b[at..end].iter().filter(|c| **c == b'.').count();
            if dots >= 2 && end - at >= 30 {
                spans.push(at..end);
            }
            at = end.max(at + 1);
            continue;
        }
        at += 1;
    }
}

fn emails(b: &[u8], spans: &mut Vec<Range<usize>>) {
    let local = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-');
    let domain = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-');
    for (at, _) in b.iter().enumerate().filter(|(_, c)| **c == b'@') {
        let start = b[..at].iter().rposition(|c| !local(*c)).map_or(0, |p| p + 1);
        let mut end = at + 1 + b[at + 1..].iter().position(|c| !domain(*c)).unwrap_or(b.len() - at - 1);
        while end > at + 1 && b[end - 1] == b'.' {
            end -= 1;
        }
        let host = &b[at + 1..end];
        let tld = host.rsplit(|c| *c == b'.').next().unwrap_or_default();
        if start < at && host.contains(&b'.') && tld.len() >= 2 && tld.iter().all(u8::is_ascii_alphabetic) && !host.starts_with(b".") {
            spans.push(start..end);
        }
    }
}

fn homes(lower: &str, b: &[u8], spans: &mut Vec<Range<usize>>) {
    for marker in ["\\users\\", "/users/", "/home/", "\\\\users\\\\"] {
        for (at, _) in lower.match_indices(marker) {
            let from = at + marker.len();
            let stop = |c: &u8| matches!(c, b'\\' | b'/' | b'\n' | b'\r' | b'"' | b'\'' | b'<' | b'>' | b'|' | b':' | b'*' | b'?');
            let end = b.iter().skip(from).position(stop).map_or(b.len(), |n| from + n);
            let end = match b.get(end) {
                Some(b'\\' | b'/') => end,
                _ => b.iter().skip(from).take(end - from).position(u8::is_ascii_whitespace).map_or(end, |n| from + n),
            };
            if end > from && !matches!(lower.get(from..end), Some("public" | "shared") | None) {
                spans.push(from..end);
            }
        }
    }
}

/// Uploads `text` to mclo.gs, its last 25 000 lines and 10 MB at most, and returns the link. Callers redact first.
pub fn upload(text: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Answer {
        success: bool,
        url: Option<String>,
        error: Option<String>,
    }
    let shown = tail(text, UPLOAD_LINES, UPLOAD_BYTES);
    if super::drive::on() {
        return Ok("https://mclo.gs/drive".to_owned());
    }
    let agent = super::net::agent_within(Duration::from_secs(60));
    let mut res = agent.post(UPLOAD_URL).send_form([("content", shown)]).context("mclo.gs is not reachable")?;
    let answer: Answer = res.body_mut().read_json().context("mclo.gs answered something unexpected")?;
    match (answer.success, answer.url) {
        (true, Some(url)) if url.starts_with("https://") => Ok(url),
        _ => bail!("mclo.gs refused the log: {}", answer.error.unwrap_or_default()),
    }
}

fn tail(text: &str, lines: usize, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut cut = text.get(start..).unwrap_or_default();
    if start > 0 {
        cut = cut.split_once('\n').map_or(cut, |(_, rest)| rest);
    }
    let ends: Vec<usize> = cut.match_indices('\n').map(|(at, _)| at).collect();
    let lines_in = ends.len() + usize::from(!cut.ends_with('\n') && !cut.is_empty());
    if lines_in <= lines {
        return cut;
    }
    let drop = lines_in - lines;
    cut.get(ends[drop - 1] + 1..).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_session_ids_are_hidden() {
        let line = "[main/INFO]: --username Dev --accessToken eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlc2ln --uuid 1234";
        assert_eq!(redact(line), "[main/INFO]: --username Dev --accessToken *** --uuid 1234");
        assert_eq!(redact("(Session ID is token:abc123:def456)"), "(Session ID is token:***)");
        assert_eq!(redact(r#"{"accessToken":"secret","name":"Dev"}"#), r#"{"accessToken":"***","name":"Dev"}"#);
        assert_eq!(redact("access_token=abc&x=1 xuid: 2535 clientId=xyz"), "access_token=***&x=1 xuid: *** clientId=***");
        assert_eq!(redact("got eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl here"), "got *** here");
        assert_eq!(redact("eyJ short.a.b"), "eyJ short.a.b");
        assert_eq!(redact("cb?id_token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl"), "cb?id_token=***");
        assert_eq!(redact("GET /session/eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl"), "GET /session/***");
        assert_eq!(redact(r#""refresh_token" : "M.C5_secret""#), r#""refresh_token" : "***""#);
        assert_eq!(redact("USERNAME=someone\nUSER=other\nMAX_USER=1"), "USERNAME=***\nUSER=***\nMAX_USER=1");
    }

    #[test]
    fn emails_are_hidden_but_object_ids_stay() {
        assert_eq!(redact("Signed in as some.one+mc@example.co.uk."), "Signed in as ***.");
        assert_eq!(redact("net.minecraft.class_310@6a2b3c4d and a@b"), "net.minecraft.class_310@6a2b3c4d and a@b");
        assert_eq!(redact("@Inject failed"), "@Inject failed");
    }

    #[test]
    fn user_names_in_home_paths_are_hidden() {
        assert_eq!(redact(r"C:\Users\Some One\AppData\Local\x.jar"), r"C:\Users\***\AppData\Local\x.jar");
        assert_eq!(redact("/home/someone/.local/share and /Users/other/Library"), "/home/***/.local/share and /Users/***/Library");
        assert_eq!(redact(r"C:/Users/someone/x and C:\Users\Public\y"), r"C:/Users/***/x and C:\Users\Public\y");
        assert_eq!(redact("path /home/someone ends"), "path /home/*** ends");
        assert_eq!(redact(r#"{"dir":"C:\\Users\\someone\\x"}"#), r#"{"dir":"C:\\Users\\***\\x"}"#);
    }

    #[test]
    fn plain_text_and_umlauts_pass_unchanged() {
        let text = "[12:00:00] [Render thread/INFO]: Grüße aus dem Spiel, ✓ token\nzweite Zeile";
        assert_eq!(redact(text), text);
        assert_eq!(redact(""), "");
    }

    #[test]
    fn minecraft_crash_reasons_read_description_and_exception() {
        let report = "---- Minecraft Crash Report ----\n// Oops.\n\nTime: 2026-11-15\nDescription: Ticking entity\n\njava.lang.NullPointerException: Cannot invoke \"x\"\n\tat a.b.c(d.java:1)\n";
        assert_eq!(crash_reason(report).as_deref(), Some("Ticking entity: java.lang.NullPointerException: Cannot invoke \"x\""));
        let bare = "---- Minecraft Crash Report ----\nDescription: Initializing game\n\n\tat x.y\n";
        assert_eq!(crash_reason(bare).as_deref(), Some("Initializing game"));
        assert_eq!(crash_reason("just a log"), None);
    }

    #[test]
    fn java_crash_reasons_name_the_error_and_the_library() {
        let hs = "#\n# A fatal error has been detected by the Java Runtime Environment:\n#\n#  EXCEPTION_ACCESS_VIOLATION (0xc0000005) at pc=0x00007ffb, pid=1234, tid=5678\n#\n# JRE version: 21\n# Problematic frame:\n# C  [atio6axx.dll+0x1234]\n";
        assert_eq!(crash_reason(hs).as_deref(), Some("EXCEPTION_ACCESS_VIOLATION (0xc0000005) in atio6axx.dll"));
        let oom = "#\n# There is insufficient memory for the Java Runtime Environment to continue.\n";
        assert!(crash_reason(oom).is_some());
    }

    #[test]
    fn logs_are_listed_latest_first_and_crashes_with_their_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        std::fs::create_dir_all(dir.path().join("crash-reports")).unwrap();
        std::fs::write(dir.path().join("logs").join("latest.log"), "now").unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, b"old line\n").unwrap();
        std::fs::write(dir.path().join("logs").join("2026-11-14-1.log.gz"), gz.finish().unwrap()).unwrap();
        std::fs::write(dir.path().join("crash-reports").join("crash-1.txt"), "---- Minecraft Crash Report ----\nDescription: Boom\n").unwrap();
        std::fs::write(dir.path().join("hs_err_pid1.log"), "# A fatal error has been detected by the Java Runtime Environment:\n#  SIGSEGV (0xb)\n").unwrap();
        std::fs::write(dir.path().join("options.txt"), "x").unwrap();
        let files = list(dir.path());
        assert_eq!(files[0].kind, Kind::Latest);
        assert_eq!(files.iter().filter(|f| f.kind == Kind::Older).count(), 1);
        let reasons: Vec<_> = files.iter().filter(|f| f.kind == Kind::Crash).filter_map(|f| f.reason.clone()).collect();
        assert_eq!(reasons.len(), 2);
        assert!(reasons.contains(&"Boom".to_owned()) && reasons.contains(&"SIGSEGV (0xb)".to_owned()));
        let older = files.iter().find(|f| f.kind == Kind::Older).unwrap();
        assert_eq!(read(&older.path).unwrap(), "old line\n");
    }

    #[test]
    fn following_returns_whole_new_lines_and_starts_over_on_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.log");
        std::fs::write(&path, "a\nb\npart").unwrap();
        let (same, text, at) = read_from(&path, 0).unwrap();
        assert_eq!((same, text.as_str(), at), (Some(0), "a\nb\n", 4));
        std::fs::write(&path, "a\nb\npartial\nc\n").unwrap();
        let (same, text, at) = read_from(&path, at).unwrap();
        assert_eq!((same, text.as_str(), at), (Some(4), "partial\nc\n", 14));
        std::fs::write(&path, "new\n").unwrap();
        let (same, text, _) = read_from(&path, at).unwrap();
        assert_eq!((same, text.as_str()), (None, "new\n"));
    }

    #[test]
    fn uploads_keep_the_end_of_long_logs() {
        assert_eq!(tail("a\nb\nc\n", 2, 100), "b\nc\n");
        assert_eq!(tail("a\nb\nc", 2, 100), "b\nc");
        assert_eq!(tail("a\nb\n", 5, 100), "a\nb\n");
        assert_eq!(tail("aaaa\nbb\n", 9, 4), "bb\n");
        assert_eq!(tail("ü\nü", 9, 3), "ü");
    }
}
