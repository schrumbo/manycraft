//! Checks and formats for the account and batch forms, free of UI so they are tested on their own.

use std::fmt;
use std::io::ErrorKind;

use crate::core::account::Account;
use crate::core::i18n::{tr, trf};

/// Smallest memory a batch can get, in MB.
pub const MEMORY_MIN: u32 = 1024;
/// Memory changes in steps of this many MB.
pub const MEMORY_STEP: u32 = 512;
const FALLBACK_RAM_MB: u64 = 16 * 1024;
const KEPT_FOR_SYSTEM_MB: u64 = 2048;
const NAME_MAX: usize = 40;

/// A failure as the person reads it: plain German with a next step, and the technical cause apart for those who want it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub text: String,
    pub detail: Option<String>,
}

impl Problem {
    /// A problem that needs no technical detail.
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), detail: None }
    }
}

/// An error whose text is already plain German with a next step, so [`explain`] shows it as it is.
#[derive(Debug)]
pub struct Plain(pub String);

impl fmt::Display for Plain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Plain {}

/// Turns any failure into a [`Problem`]: what happened in everyday German and what to do next; the raw cause only as detail.
pub fn explain(e: &anyhow::Error) -> Problem {
    if let Some(Plain(text)) = e.chain().find_map(|c| c.downcast_ref::<Plain>()) {
        return Problem::plain(text.clone());
    }
    let raw = format!("{e:#}");
    let io = e.chain().find_map(|c| c.downcast_ref::<std::io::Error>());
    let text = if io.is_some_and(|i| i.kind() == ErrorKind::PermissionDenied) {
        tr("Manycraft darf diese Datei nicht schreiben. Prüfe die Rechte des Ordners und versuche es noch einmal.")
    } else if io.is_some_and(|i| i.kind() == ErrorKind::StorageFull) {
        tr("Der Speicherplatz ist voll. Schaffe Platz und versuche es noch einmal.")
    } else if super::remote::is_network(e) {
        tr("Keine Verbindung. Prüfe Internet und Proxy und versuche es noch einmal.")
    } else if raw.contains("changed by another Manycraft") {
        tr("Ein anderes Manycraft-Fenster hat das gerade geändert. Versuche es noch einmal.")
    } else if raw.contains("is damaged") || raw.contains("is unreadable") {
        tr("Eine Datei von Manycraft ist beschädigt. Manycraft hat sie nicht verändert. Sichere oder verschiebe sie und versuche es noch einmal.")
    } else if raw.contains("keychain") {
        tr("Der Passwortspeicher deines Systems antwortet nicht. Entsperre ihn und versuche es noch einmal.")
    } else {
        tr("Das hat nicht geklappt. Versuche es noch einmal. Bleibt es so, helfen die Details bei der Fehlersuche.")
    };
    Problem { text, detail: Some(raw) }
}

/// Like [`explain`], with what was being attempted in front of the reason.
pub fn explain_as(what: String, e: &anyhow::Error) -> Problem {
    let mut problem = explain(e);
    problem.text = format!("{what} {}", problem.text);
    problem
}

/// What is wrong with an offline account name, or `None` when vanilla and the saved accounts accept it.
pub fn offline_name_problem(name: &str, accounts: &[Account]) -> Option<String> {
    if name.is_empty() {
        return Some(tr("Gib einen Namen ein."));
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Some(tr("Nur Buchstaben ohne Umlaute, Ziffern und _."));
    }
    if name.len() < 3 {
        return Some(tr("Mindestens 3 Zeichen."));
    }
    if name.len() > 16 {
        return Some(tr("Höchstens 16 Zeichen."));
    }
    if accounts.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
        return Some(tr("Diesen Namen gibt es schon."));
    }
    None
}

/// What is wrong with a proxy address (`socks5://` or `http://`, optional `user:pass@`, then `host:port`); empty means no proxy.
pub fn proxy_problem(proxy: &str) -> Option<String> {
    if proxy.is_empty() {
        return None;
    }
    let rest = ["socks5://", "http://"].iter().find_map(|scheme| proxy.strip_prefix(scheme));
    let Some(rest) = rest else {
        return Some(tr("Beginnt mit socks5:// oder http://"));
    };
    let address = match rest.rsplit_once('@') {
        Some((user, address)) => {
            let name = user.split_once(':').map_or(user, |(name, _)| name);
            if name.is_empty() {
                return Some(tr("Vor dem @ fehlt der Benutzername."));
            }
            address
        }
        None => rest,
    };
    address_problem(address.strip_suffix('/').unwrap_or(address), true)
}

/// What is wrong with a server address (`host` or `host:port`); empty means no server.
pub fn server_problem(server: &str) -> Option<String> {
    if server.is_empty() {
        return None;
    }
    address_problem(server, false)
}

fn address_problem(address: &str, port_needed: bool) -> Option<String> {
    let (host, port) = match address.strip_prefix('[') {
        Some(v6) => match v6.split_once(']') {
            Some((host, rest)) => (host, rest.strip_prefix(':')),
            None => return Some(tr("Die Adresse ist unvollständig.")),
        },
        None => match address.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (address, None),
        },
    };
    let v6 = address.starts_with('[');
    if host.is_empty() {
        return Some(tr("Es fehlt die Adresse."));
    }
    if !v6 && !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_') {
        return Some(tr("Die Adresse enthält ungültige Zeichen."));
    }
    if v6 && !host.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.') {
        return Some(tr("Die Adresse enthält ungültige Zeichen."));
    }
    match port {
        None if port_needed => Some(tr("Es fehlt der Port, z. B. :1080")),
        None => None,
        Some(port) if port.parse::<u16>().is_ok_and(|p| p > 0) => None,
        Some(_) => Some(tr("Der Port ist eine Zahl von 1 bis 65535.")),
    }
}

/// A proxy address without its password, safe to show.
pub fn proxy_display(proxy: &str) -> String {
    let Some((scheme, rest)) = proxy.split_once("://") else { return proxy.to_owned() };
    match rest.rsplit_once('@') {
        Some((user, address)) => {
            let name = user.split_once(':').map_or(user, |(name, _)| name);
            format!("{scheme}://{name}@{address}")
        }
        None => proxy.to_owned(),
    }
}

/// What is wrong with a batch name.
pub fn batch_name_problem(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        Some(tr("Gib dem Batch einen Namen."))
    } else if name.trim().chars().count() > NAME_MAX {
        Some(trf("Höchstens %lld Zeichen.", &[&NAME_MAX]))
    } else {
        None
    }
}

/// `base`, or `base (2)`, `base (3)` … when a batch of that name exists.
pub fn unique_name(base: &str, taken: &[&str]) -> String {
    let free = |name: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(name));
    if free(base) {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base} ({n})")).find(|name| free(name)).unwrap_or_default()
}

/// The first `Batch N` that no batch uses yet.
pub fn next_batch_name(taken: &[&str]) -> String {
    (1..).map(|n| trf("Batch %lld", &[&n])).find(|name| !taken.iter().any(|t| t.eq_ignore_ascii_case(name))).unwrap_or_default()
}

/// Memory in MB as people read it: `1 GB`, `2,5 GB`.
pub fn memory_label(mb: u32) -> String {
    let tenths = (u64::from(mb) * 10 + 512) / 1024;
    let text = if tenths % 10 == 0 { format!("{}", tenths / 10) } else { format!("{}.{}", tenths / 10, tenths % 10) };
    let text = if crate::core::i18n::lang() == crate::core::i18n::Lang::De { text.replace('.', ",") } else { text };
    format!("{text} GB")
}

/// The most memory one instance may get on a machine with `total_mb`: all but 2 GB for the system, in whole steps.
pub fn memory_max(total_mb: u64) -> u32 {
    let usable = total_mb.saturating_sub(KEPT_FOR_SYSTEM_MB).min(u64::from(u32::MAX));
    let stepped = usable / u64::from(MEMORY_STEP) * u64::from(MEMORY_STEP);
    (stepped as u32).max(MEMORY_MIN)
}

/// `mb` moved by `steps` memory steps, kept between the minimum and `max`, on a step.
pub fn memory_step(mb: u32, steps: i32, max: u32) -> u32 {
    let snapped = (mb / MEMORY_STEP) as i64 + i64::from(steps);
    let value = (snapped * i64::from(MEMORY_STEP)).clamp(i64::from(MEMORY_MIN), i64::from(max.max(MEMORY_MIN)));
    value as u32
}

/// Downloads as a short count: `950`, `12.400`, `1,2 Mio.`.
pub fn downloads_label(n: u64) -> String {
    let de = crate::core::i18n::lang() == crate::core::i18n::Lang::De;
    if n >= 1_000_000 {
        let tenths = (n + 50_000) / 100_000;
        let number = if tenths % 10 == 0 { format!("{}", tenths / 10) } else { format!("{}.{}", tenths / 10, tenths % 10) };
        let number = if de { number.replace('.', ",") } else { number };
        return trf("%@ Mio. Downloads", &[&number]);
    }
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(if de { '.' } else { ',' });
        }
        grouped.push(c);
    }
    trf("%@ Downloads", &[&grouped])
}

/// Physical memory of this machine in MB, or 16 GB when the system does not say.
pub fn total_ram_mb() -> u64 {
    system_ram_mb().filter(|mb| *mb > 0).unwrap_or(FALLBACK_RAM_MB)
}

#[cfg(windows)]
fn system_ram_mb() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys / (1024 * 1024))
}

#[cfg(target_os = "macos")]
fn system_ram_mb() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    let ok = unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut bytes).cast(), &mut size, std::ptr::null_mut(), 0) } == 0;
    ok.then_some(bytes / (1024 * 1024))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_ram_mb() -> Option<u64> {
    meminfo_mb(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn meminfo_mb(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::account::Account;

    #[test]
    fn failures_read_as_plain_german_and_keep_the_cause_apart() {
        crate::core::i18n::init("de");
        let wrapped = anyhow::Error::new(Plain("Diesen Batch gibt es nicht mehr.".into())).context("saving");
        assert_eq!(explain(&wrapped), Problem::plain("Diesen Batch gibt es nicht mehr."));
        let net = anyhow::Error::from(std::io::Error::other("reset")).context("cannot reach Modrinth");
        let p = explain(&net);
        assert!(p.text.starts_with("Keine Verbindung.") && p.detail.as_deref().is_some_and(|d| d.contains("cannot reach")), "{p:?}");
        let stale = explain(&anyhow::anyhow!("accounts.json was changed by another Manycraft since it was read; reload and try again"));
        assert!(stale.text.contains("anderes Manycraft-Fenster") && !stale.text.contains("accounts.json"), "{stale:?}");
        let keychain = explain(&anyhow::anyhow!("system keychain is not available"));
        assert!(keychain.text.contains("Passwortspeicher"), "{keychain:?}");
        let denied = explain(&anyhow::Error::from(std::io::Error::from(ErrorKind::PermissionDenied)).context("cannot write x"));
        assert!(denied.text.contains("nicht schreiben"), "{denied:?}");
        let other = explain(&anyhow::anyhow!("something odd"));
        assert!(other.text.starts_with("Das hat nicht geklappt.") && other.detail.as_deref() == Some("something odd"), "{other:?}");
        for p in [p, stale, keychain, denied, other] {
            assert!(p.text.contains("noch einmal"), "{p:?}");
        }
    }

    #[test]
    fn offline_names_follow_vanilla_and_refuse_duplicates() {
        let saved = vec![Account::offline("Steve").unwrap()];
        for ok in ["Alex", "a_b", "Player_1234567_"] {
            assert_eq!(offline_name_problem(ok, &saved), None, "{ok}");
        }
        for bad in ["", "ab", "a_b_c_d_e_f_g_h_i", "has space", "näme", "a-b", "steve", "STEVE"] {
            assert!(offline_name_problem(bad, &saved).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn proxies_need_scheme_host_and_port() {
        for ok in ["", "socks5://127.0.0.1:1080", "http://proxy.example.net:8080", "socks5://user:pass@host:1", "socks5://user@host:65535", "http://[::1]:3128", "http://h:8080/"] {
            assert_eq!(proxy_problem(ok), None, "{ok}");
        }
        for bad in ["127.0.0.1:1080", "https://h:1", "socks4://h:1", "socks5://h", "socks5://:1080", "socks5://h:0", "socks5://h:70000", "socks5://h:port", "socks5://@h:1", "socks5://:pw@h:1", "http://bad host:1", "http://[::1:3128"] {
            assert!(proxy_problem(bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn servers_may_omit_the_port() {
        for ok in ["", "play.example.net", "1.2.3.4:25565", "[2001:db8::1]:25565", "[::1]", "my_server"] {
            assert_eq!(server_problem(ok), None, "{ok}");
        }
        for bad in [":25565", "host:", "host:0", "host:99999", "two words", "host/path", "ü.de"] {
            assert!(server_problem(bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn shown_proxies_hide_the_password() {
        assert_eq!(proxy_display("socks5://me:secret@h:1080"), "socks5://me@h:1080");
        assert_eq!(proxy_display("socks5://me:se@cret@h:1080"), "socks5://me@h:1080");
        assert_eq!(proxy_display("http://h:1"), "http://h:1");
        assert!(!proxy_display("http://u:p4ss@h:1").contains("p4ss"));
    }

    #[test]
    fn batch_names_are_needed_and_numbered() {
        assert!(batch_name_problem("  ").is_some());
        assert!(batch_name_problem(&"x".repeat(41)).is_some());
        assert_eq!(batch_name_problem("Farm"), None);
        crate::core::i18n::init("de");
        assert_eq!(next_batch_name(&[]), "Batch 1");
        assert_eq!(next_batch_name(&["batch 1", "Batch 2", "Farm"]), "Batch 3");
    }

    #[test]
    fn unique_name_counts_up_only_when_taken() {
        assert_eq!(unique_name("Farm", &["Other"]), "Farm");
        assert_eq!(unique_name("Farm", &["farm"]), "Farm (2)");
        assert_eq!(unique_name("Farm", &["Farm", "Farm (2)"]), "Farm (3)");
    }

    #[test]
    fn memory_steps_stay_in_range() {
        assert_eq!(memory_max(16 * 1024), 14 * 1024);
        assert_eq!(memory_max(2048), MEMORY_MIN);
        assert_eq!(memory_max(0), MEMORY_MIN);
        assert_eq!(memory_max(8000), 5632);
        assert_eq!(memory_step(2048, 1, 8192), 2560);
        assert_eq!(memory_step(2048, -1, 8192), 1536);
        assert_eq!(memory_step(1024, -1, 8192), 1024);
        assert_eq!(memory_step(8192, 1, 8192), 8192);
        assert_eq!(memory_step(2100, 0, 8192), 2048);
        assert_eq!(memory_step(9000, 0, 4096), 4096);
    }

    #[test]
    fn labels_read_like_people_talk() {
        crate::core::i18n::init("de");
        assert_eq!(memory_label(1024), "1 GB");
        assert_eq!(memory_label(2560), "2,5 GB");
        assert_eq!(downloads_label(950), "950 Downloads");
        assert_eq!(downloads_label(12_400), "12.400 Downloads");
        assert_eq!(downloads_label(1_250_000), "1,3 Mio. Downloads");
        assert_eq!(downloads_label(3_000_000), "3 Mio. Downloads");
    }

    #[test]
    fn meminfo_gives_total_memory() {
        assert_eq!(meminfo_mb("MemFree: 1 kB\nMemTotal:       16303128 kB\n"), Some(15921));
        assert_eq!(meminfo_mb("nothing"), None);
        assert!(total_ram_mb() > 0);
    }
}
