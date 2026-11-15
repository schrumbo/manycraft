//! Keeps secrets out of the crash files the JVM writes: it prints its whole command line, access token included.

use std::path::Path;

use super::net;

const REDACTED: &str = "<redacted>";
const TOKEN_FLAG: &str = "--accessToken";
const ENV_NAMES: [&str; 2] = ["MANYCRAFT_TOKEN=", "MANYCRAFT_PROXY="];
const JWT_START: &str = "eyJ";
const JWT_MIN: usize = 30;
const MARKERS: [&str; 4] = [TOKEN_FLAG, "MANYCRAFT_", "://", JWT_START];

fn may_hold_secrets(text: &str) -> bool {
    MARKERS.iter().any(|m| text.contains(m))
}

/// Whether `name` is a file the JVM writes when it dies.
pub fn is_crash_file(name: &str) -> bool {
    (name.starts_with("hs_err_pid") || name.starts_with("replay_pid")) && (name.ends_with(".log") || name.ends_with(".mdmp"))
}

/// `text` with the access token after `--accessToken`, JWT-shaped tokens, `MANYCRAFT_*` values and the `user:pass@` of URLs replaced.
pub fn redact(text: &str) -> String {
    if !may_hold_secrets(text) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut after_flag = false;
    let mut rest = text;
    while !rest.is_empty() {
        let (blank, tail) = rest.split_at(rest.len() - rest.trim_start().len());
        out.push_str(blank);
        let (word, remainder) = tail.split_at(tail.find(char::is_whitespace).unwrap_or(tail.len()));
        rest = remainder;
        if word.is_empty() {
            continue;
        }
        if after_flag {
            out.push_str(REDACTED);
        } else {
            out.push_str(&redact_word(word));
        }
        after_flag = word == TOKEN_FLAG;
    }
    out
}

fn redact_word(word: &str) -> String {
    let mut word = word.to_owned();
    for name in ENV_NAMES {
        if let Some(i) = word.find(name) {
            word.truncate(i + name.len());
            word.push_str(REDACTED);
        }
    }
    if let Some((head, tail)) = word.split_once("://")
        && let Some((info, host)) = tail.rsplit_once('@')
        && !info.contains('/')
    {
        word = format!("{head}://{REDACTED}@{host}");
    }
    let mut out = String::with_capacity(word.len());
    let mut rest = word.as_str();
    while let Some((before, from)) = rest.split_once(JWT_START) {
        let (body, tail) = from.split_at(from.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))).unwrap_or(from.len()));
        out.push_str(before);
        if JWT_START.len() + body.len() >= JWT_MIN {
            out.push_str(REDACTED);
        } else {
            out.push_str(JWT_START);
            out.push_str(body);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Rewrites the file with its secrets redacted when it holds any. Returns whether it changed.
pub fn scrub_file(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let text = String::from_utf8_lossy(&bytes);
    if !may_hold_secrets(&text) {
        return false;
    }
    let clean = redact(&text);
    clean != text && net::write_atomic(path, clean.as_bytes()).is_ok()
}

/// Redacts every crash file in `logs`; returns how many it changed.
pub fn scrub_logs(logs: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(logs) else { return 0 };
    entries.filter_map(Result::ok).filter(|e| is_crash_file(&e.file_name().to_string_lossy())).filter(|e| scrub_file(&e.path())).count()
}

/// Redacts the crash files of every game folder under `instances` (`<instance>/games/<account>/logs`).
pub fn scrub_instances(instances: &Path) -> usize {
    let Ok(found) = std::fs::read_dir(instances) else { return 0 };
    found
        .filter_map(Result::ok)
        .filter_map(|instance| std::fs::read_dir(super::instance::games_dir(&instance.path())).ok())
        .flat_map(|accounts| accounts.filter_map(Result::ok))
        .map(|account| scrub_logs(&account.path().join("logs")))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJlLXNpZ25hdHVyZQ";

    #[test]
    fn the_token_after_the_flag_goes() {
        let line = format!("java_command: net.fabricmc.KnotClient --username Alt --accessToken {JWT} --xuid 1");
        let clean = redact(&line);
        assert!(!clean.contains("eyJ"), "{clean}");
        assert_eq!(clean, "java_command: net.fabricmc.KnotClient --username Alt --accessToken <redacted> --xuid 1");
    }

    #[test]
    fn short_or_numeric_tokens_after_the_flag_go_too() {
        assert_eq!(redact("--accessToken 0 --version x"), "--accessToken <redacted> --version x");
    }

    #[test]
    fn jwt_shaped_strings_go_wherever_they_sit() {
        let clean = redact(&format!("env TOKEN={JWT}, other eyJshort ok"));
        assert!(!clean.contains("c2lnbmF0dXJl"), "{clean}");
        assert!(clean.contains("eyJshort"), "{clean}");
    }

    #[test]
    fn only_long_jwt_shaped_strings_go() {
        let body = |n: usize| format!("eyJ{}", "a".repeat(n));
        assert_eq!(redact(&body(10)), body(10));
        assert_eq!(redact(&body(26)), body(26));
        assert_eq!(redact(&body(27)), "<redacted>");
    }

    #[test]
    fn manycraft_variables_lose_their_values() {
        assert_eq!(redact("MANYCRAFT_TOKEN=abc123 PATH=/bin MANYCRAFT_PROXY=socks5://u:p@h:1"), "MANYCRAFT_TOKEN=<redacted> PATH=/bin MANYCRAFT_PROXY=<redacted>");
    }

    #[test]
    fn url_credentials_go_but_plain_urls_stay() {
        assert_eq!(redact("socks5://us%40r:p%3Ass@10.1.1.1:1081"), "socks5://<redacted>@10.1.1.1:1081");
        assert_eq!(redact("https://example.org/a@b"), "https://example.org/a@b");
        assert_eq!(redact("-Dmanycraft.proxy=socks5://u:p@h:1"), "-Dmanycraft.proxy=socks5://<redacted>@h:1");
    }

    #[test]
    fn whitespace_and_everything_else_survive() {
        let text = "#\n# A fatal error\n\n  at x.y(Z.java:1)\t tab\r\n";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn crash_files_are_found_and_rewritten_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("b").join("games").join("a").join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("hs_err_pid42.log"), format!("java_command: X --accessToken {JWT}\n")).unwrap();
        std::fs::write(logs.join("latest.log"), format!("--accessToken {JWT}\n")).unwrap();
        assert_eq!(scrub_instances(dir.path()), 1);
        assert_eq!(std::fs::read_to_string(logs.join("hs_err_pid42.log")).unwrap(), "java_command: X --accessToken <redacted>\n");
        assert!(std::fs::read_to_string(logs.join("latest.log")).unwrap().contains("eyJ"));
        assert_eq!(scrub_instances(dir.path()), 0);
    }

    #[test]
    fn only_jvm_crash_files_count() {
        assert!(is_crash_file("hs_err_pid1234.log"));
        assert!(is_crash_file("replay_pid1234.log"));
        assert!(!is_crash_file("latest.log"));
        assert!(!is_crash_file("hs_err_pid1234.txt"));
    }
}
