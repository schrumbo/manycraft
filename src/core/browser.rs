//! The default web browser and how to open a page in its private mode, so a sign-in there starts without a saved Microsoft session.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// Browser families that differ in how they are asked for a private window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Edge,
    Chromium,
    Firefox,
    Opera,
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    Safari,
    Unknown,
}

impl Kind {
    /// The command-line flag that opens a private window, when this browser has one.
    pub fn private_flag(self) -> Option<&'static str> {
        match self {
            Self::Edge => Some("--inprivate"),
            Self::Chromium => Some("--incognito"),
            Self::Firefox => Some("-private-window"),
            Self::Opera => Some("--private"),
            Self::Safari | Self::Unknown => None,
        }
    }
}

/// The default browser: its family and the command that starts it.
#[derive(Debug, PartialEq, Eq)]
pub struct Browser {
    pub kind: Kind,
    program: PathBuf,
    args: Vec<String>,
}

impl Browser {
    /// Whether this browser can be asked for a private window.
    pub fn private(&self) -> bool {
        self.kind.private_flag().is_some()
    }
}

/// Opens `url` in a private window of the default browser. False when that browser has no private flag Manycraft knows;
/// the caller then opens the page the normal way.
pub fn open_private(url: &str) -> Result<bool> {
    let Some(browser) = default() else { return Ok(false) };
    let Some(flag) = browser.kind.private_flag() else { return Ok(false) };
    let mut command = Command::new(&browser.program);
    command.args(&browser.args).arg(flag).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = command.spawn().with_context(|| format!("cannot start {}", browser.program.display()))?;
    std::thread::spawn(move || child.wait());
    Ok(true)
}

/// The default browser for https links, if it can be found.
#[cfg(windows)]
pub fn default() -> Option<Browser> {
    let key = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https";
    let prog_id = registry::string(registry::CURRENT_USER, &format!(r"{key}\UserChoiceLatest\ProgId"), "ProgId").or_else(|| registry::string(registry::CURRENT_USER, &format!(r"{key}\UserChoice"), "ProgId"))?;
    let command = registry::string(registry::CLASSES_ROOT, &format!(r"{prog_id}\shell\open\command"), "")?;
    Some(Browser { kind: kind_from_prog_id(&prog_id), program: command_program(&command)?, args: Vec::new() })
}

/// The default browser for https links, if it can be found.
#[cfg(target_os = "macos")]
pub fn default() -> Option<Browser> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSBundle, NSString, NSURL};
    let url = NSURL::URLWithString(&NSString::from_str("https://www.microsoft.com/"))?;
    let app = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&url)?;
    let bundle = NSBundle::bundleWithURL(&app)?;
    let id = bundle.bundleIdentifier()?.to_string();
    let program = bundle.executableURL()?.path()?.to_string();
    Some(Browser { kind: kind_from_bundle(&id), program: program.into(), args: Vec::new() })
}

/// The default browser for https links, if it can be found.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn default() -> Option<Browser> {
    let out = Command::new("xdg-settings").args(["get", "default-web-browser"]).stderr(Stdio::null()).output().ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() {
        return None;
    }
    let exec = desktop_dirs().into_iter().find_map(|dir| std::fs::read_to_string(dir.join(&id)).ok()).and_then(|text| desktop_exec(&text))?;
    let mut parts = exec_args(&exec);
    if parts.is_empty() {
        return None;
    }
    let program = parts.remove(0);
    Some(Browser { kind: kind_from_desktop(&id), program: program.into(), args: parts })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn desktop_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter().chain(system.split(':').map(PathBuf::from)).chain(dirs::home_dir().map(|h| h.join(".local/share/flatpak/exports/share"))).chain([PathBuf::from("/var/lib/flatpak/exports/share")]).map(|d| d.join("applications")).collect()
}

#[cfg(any(windows, test))]
fn kind_from_prog_id(prog_id: &str) -> Kind {
    let id = prog_id.to_ascii_lowercase();
    if id.starts_with("msedge") {
        Kind::Edge
    } else if ["chrome", "brave", "vivaldi", "chromium"].iter().any(|p| id.starts_with(p)) {
        Kind::Chromium
    } else if id.starts_with("firefox") || id.starts_with("librewolf") {
        Kind::Firefox
    } else if id.starts_with("opera") {
        Kind::Opera
    } else {
        Kind::Unknown
    }
}

#[cfg(any(windows, test))]
fn command_program(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => command.split(' ').next()?,
    };
    (!program.is_empty()).then(|| PathBuf::from(program))
}

#[cfg(any(target_os = "macos", test))]
fn kind_from_bundle(id: &str) -> Kind {
    match id.to_ascii_lowercase().as_str() {
        id if id.starts_with("com.microsoft.edgemac") => Kind::Edge,
        id if ["com.google.chrome", "com.brave.browser", "com.vivaldi.vivaldi", "org.chromium.chromium"].iter().any(|p| id.starts_with(p)) => Kind::Chromium,
        id if id.starts_with("org.mozilla.firefox") || id.starts_with("org.mozilla.nightly") || id.starts_with("io.gitlab.librewolf") => Kind::Firefox,
        id if id.starts_with("com.operasoftware.opera") => Kind::Opera,
        "com.apple.safari" | "com.apple.safaritechnologypreview" => Kind::Safari,
        _ => Kind::Unknown,
    }
}

#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn kind_from_desktop(id: &str) -> Kind {
    let id = id.to_ascii_lowercase();
    if id.contains("edge") {
        Kind::Edge
    } else if ["chrome", "chromium", "brave", "vivaldi"].iter().any(|p| id.contains(p)) {
        Kind::Chromium
    } else if id.contains("firefox") || id.contains("librewolf") {
        Kind::Firefox
    } else if id.contains("opera") {
        Kind::Opera
    } else {
        Kind::Unknown
    }
}

#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn desktop_exec(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some(value) = line.strip_prefix("Exec=") {
            return Some(value.trim().to_string());
        }
    }
    None
}

#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn exec_args(exec: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => current.extend(chars.next()),
            ' ' | '\t' if !quoted => {
                if started {
                    parts.push(std::mem::take(&mut current));
                }
                started = false;
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        parts.push(current);
    }
    parts.into_iter().filter(|p| !(p.len() == 2 && p.starts_with('%')) && !p.starts_with("@@")).map(|p| p.replace("%%", "%")).collect()
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

    pub const CURRENT_USER: HKEY = HKEY_CURRENT_USER;
    pub const CLASSES_ROOT: HKEY = HKEY_CLASSES_ROOT;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    pub fn string(root: HKEY, key: &str, value: &str) -> Option<String> {
        let (key, value) = (wide(key), wide(value));
        let mut size = 0u32;
        let found = unsafe { RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        if found != ERROR_SUCCESS || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let read = unsafe { RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
        if read != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_flags_per_browser() {
        assert_eq!(Kind::Edge.private_flag(), Some("--inprivate"));
        assert_eq!(Kind::Chromium.private_flag(), Some("--incognito"));
        assert_eq!(Kind::Firefox.private_flag(), Some("-private-window"));
        assert_eq!(Kind::Opera.private_flag(), Some("--private"));
        assert_eq!(Kind::Safari.private_flag(), None);
        assert_eq!(Kind::Unknown.private_flag(), None);
        let browser = |kind| Browser { kind, program: PathBuf::from("browser"), args: Vec::new() };
        assert!(browser(Kind::Firefox).private());
        assert!(!browser(Kind::Unknown).private());
    }

    #[test]
    fn windows_prog_ids() {
        for (id, kind) in [
            ("MSEdgeHTM", Kind::Edge),
            ("MSEdgeBHTML", Kind::Edge),
            ("ChromeHTML", Kind::Chromium),
            ("BraveHTML", Kind::Chromium),
            ("VivaldiHTM.KMHYDZ2W3BSXAMLHXMRTMDA4IU", Kind::Chromium),
            ("ChromiumHTM.QJK5BUZBMLNWMNA4SHVHWB3VKE", Kind::Chromium),
            ("FirefoxURL-308046B0AF4A39CB", Kind::Firefox),
            ("OperaStable", Kind::Opera),
            ("Opera GXStable", Kind::Opera),
            ("IE.HTTP", Kind::Unknown),
            ("AppXq0fevzme2pys62n3e0fbqa7peapykr8v", Kind::Unknown),
        ] {
            assert_eq!(kind_from_prog_id(id), kind, "{id}");
        }
    }

    #[test]
    fn windows_open_commands() {
        assert_eq!(command_program(r#""C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" --single-argument %1"#), Some(PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")));
        assert_eq!(command_program(r#""C:\Program Files\Mozilla Firefox\firefox.exe" -osint -url "%1""#), Some(PathBuf::from(r"C:\Program Files\Mozilla Firefox\firefox.exe")));
        assert_eq!(command_program(r"C:\Tools\browser.exe %1"), Some(PathBuf::from(r"C:\Tools\browser.exe")));
        assert_eq!(command_program("  "), None);
        assert_eq!(command_program(r#""""#), None);
    }

    #[test]
    fn macos_bundle_ids() {
        for (id, kind) in [
            ("com.microsoft.edgemac", Kind::Edge),
            ("com.google.Chrome", Kind::Chromium),
            ("com.google.Chrome.canary", Kind::Chromium),
            ("com.brave.Browser", Kind::Chromium),
            ("com.vivaldi.Vivaldi", Kind::Chromium),
            ("org.chromium.Chromium", Kind::Chromium),
            ("org.mozilla.firefox", Kind::Firefox),
            ("org.mozilla.firefoxdeveloperedition", Kind::Firefox),
            ("com.operasoftware.Opera", Kind::Opera),
            ("com.apple.Safari", Kind::Safari),
            ("company.thebrowser.Browser", Kind::Unknown),
        ] {
            assert_eq!(kind_from_bundle(id), kind, "{id}");
        }
    }

    #[test]
    fn linux_desktop_ids() {
        for (id, kind) in [
            ("microsoft-edge.desktop", Kind::Edge),
            ("google-chrome.desktop", Kind::Chromium),
            ("com.google.Chrome.desktop", Kind::Chromium),
            ("chromium-browser.desktop", Kind::Chromium),
            ("brave-browser.desktop", Kind::Chromium),
            ("vivaldi-stable.desktop", Kind::Chromium),
            ("firefox.desktop", Kind::Firefox),
            ("org.mozilla.firefox.desktop", Kind::Firefox),
            ("firefox_firefox.desktop", Kind::Firefox),
            ("opera.desktop", Kind::Opera),
            ("org.gnome.Epiphany.desktop", Kind::Unknown),
        ] {
            assert_eq!(kind_from_desktop(id), kind, "{id}");
        }
    }

    #[test]
    fn linux_exec_line_skips_actions_and_field_codes() {
        let file = "[Desktop Entry]\nName=Firefox\nExec=firefox %u\n\n[Desktop Action new-private-window]\nExec=firefox --private-window %u\n";
        assert_eq!(desktop_exec(file).as_deref(), Some("firefox %u"));
        assert_eq!(exec_args("firefox %u"), ["firefox"]);
        let action_first = "[Desktop Action new-window]\nExec=chrome --new-window\n[Desktop Entry]\nExec=/usr/bin/google-chrome-stable %U\n";
        assert_eq!(desktop_exec(action_first).as_deref(), Some("/usr/bin/google-chrome-stable %U"));
        assert_eq!(desktop_exec("[Desktop Entry]\nName=x\n"), None);
    }

    #[test]
    fn linux_exec_handles_flatpak_and_quotes() {
        assert_eq!(exec_args("/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=/app/bin/chromium --file-forwarding org.chromium.Chromium @@u %U @@"), ["/usr/bin/flatpak", "run", "--branch=stable", "--arch=x86_64", "--command=/app/bin/chromium", "--file-forwarding", "org.chromium.Chromium"]);
        assert_eq!(exec_args(r#""/opt/My Browser/browser" --flag=100%% %U"#), ["/opt/My Browser/browser", "--flag=100%"]);
        assert_eq!(exec_args(r#"env A="" b"#), ["env", "A=", "b"]);
        assert_eq!(exec_args(r#""/opt/a \"b\"/x" c\d"#), [r#"/opt/a "b"/x"#, r"c\d"]);
    }
}
