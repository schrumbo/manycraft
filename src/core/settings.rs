//! App-wide settings in `settings.json`: chat, notifications, how instances reconnect and restart. Missing keys take their defaults.

use std::path::Path;
use std::sync::{Mutex, OnceLock, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{net, paths};

const FILE: &str = "settings.json";
const BACKUP: &str = "settings.json.bak";
const MAX_ATTEMPTS: u32 = 100;
const MAX_SECS: u64 = 24 * 3600;

/// How a dropped instance comes back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reconnect {
    pub enabled: bool,
    /// Waits in seconds before try 1, 2, …; later tries reuse the last entry.
    pub backoff_secs: Vec<u64>,
    /// Waits for a full server or a kick, which only get worse when hammered.
    pub patient_secs: Vec<u64>,
    /// Random spread of every wait, in percent either side.
    pub jitter_percent: u32,
    pub max_attempts: u32,
}

impl Default for Reconnect {
    fn default() -> Self {
        Self { enabled: true, backoff_secs: vec![10, 30, 90, 300], patient_secs: vec![60, 180, 600, 1800], jitter_percent: 25, max_attempts: 5 }
    }
}

/// Whether a game that exits with an error starts again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrashRestart {
    pub enabled: bool,
    pub backoff_secs: Vec<u64>,
    pub jitter_percent: u32,
    pub max_attempts: u32,
}

impl Default for CrashRestart {
    fn default() -> Self {
        Self { enabled: true, backoff_secs: vec![10, 30, 90, 300], jitter_percent: 25, max_attempts: 3 }
    }
}

/// Which events raise an OS notification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// One per batch when an account is disconnected, banned, crashed or needs to sign in.
    pub attention: bool,
    /// One for whispers that arrive while the window is not in front.
    pub whispers: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self { attention: true, whispers: true }
    }
}

/// Everything configurable app-wide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub reconnect: Reconnect,
    pub crash_restart: CrashRestart,
    /// Whether the chat of all instances is collected into the Chat page.
    pub merged_chat: bool,
    /// Whether the Chat page can send a line to one instance or a whole batch.
    pub broadcast: bool,
    pub notifications: Notifications,
    /// Whether a newer release found by the daily check is downloaded right away and installed when the app quits.
    #[serde(alias = "update_check")]
    pub auto_update: bool,
    /// Whether pre-releases count as updates.
    pub prereleases: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { reconnect: Reconnect::default(), crash_restart: CrashRestart::default(), merged_chat: true, broadcast: true, notifications: Notifications::default(), auto_update: true, prereleases: false }
    }
}

/// The chords the companion mod listens for inside a game, as the mod's defaults have them.
pub const HOTKEYS: [(&str, &str); 4] = [("Nächste Instanz", "ctrl+alt+right"), ("Vorherige Instanz", "ctrl+alt+left"), ("Nächste, die dich braucht", "ctrl+alt+down"), ("Zurück zum Raster", "ctrl+alt+g")];

impl Settings {
    /// The same settings with values a hand-edited file could break put back in range: empty schedules, zero waits, huge counts.
    pub fn sanitized(mut self) -> Self {
        let fix = |secs: &mut Vec<u64>, fallback: &[u64]| {
            if secs.is_empty() {
                *secs = fallback.to_vec();
            }
            for s in secs.iter_mut() {
                *s = (*s).clamp(1, MAX_SECS);
            }
        };
        let (reconnect, crash) = (Reconnect::default(), CrashRestart::default());
        fix(&mut self.reconnect.backoff_secs, &reconnect.backoff_secs);
        fix(&mut self.reconnect.patient_secs, &reconnect.patient_secs);
        fix(&mut self.crash_restart.backoff_secs, &crash.backoff_secs);
        self.reconnect.jitter_percent = self.reconnect.jitter_percent.min(100);
        self.crash_restart.jitter_percent = self.crash_restart.jitter_percent.min(100);
        self.reconnect.max_attempts = self.reconnect.max_attempts.min(MAX_ATTEMPTS);
        self.crash_restart.max_attempts = self.crash_restart.max_attempts.min(MAX_ATTEMPTS);
        self
    }
}

/// Loads `settings.json` from the data folder; an absent file gives the defaults, an unreadable one an error.
pub fn load() -> Result<Settings> {
    load_from(&paths::data_dir())
}

fn load_from(dir: &Path) -> Result<Settings> {
    let path = dir.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let settings: Settings = match serde_json::from_slice(&bytes) {
        Ok(settings) => settings,
        Err(e) => {
            let kept = net::write_atomic(&dir.join(BACKUP), &bytes).is_ok();
            let note = if kept { format!("a copy is in {BACKUP}") } else { "it was left untouched".to_owned() };
            return Err(anyhow::Error::new(e).context(format!("{} is damaged, so the defaults apply; {note}", path.display())));
        }
    };
    Ok(settings.sanitized())
}

fn save_to(dir: &Path, settings: &Settings) -> Result<()> {
    net::write_atomic(&dir.join(FILE), &serde_json::to_vec_pretty(settings)?)
}

static ACTIVE: RwLock<Option<&'static Settings>> = RwLock::new(None);

/// Loads the settings once for the whole run. A damaged file leaves the defaults in force and is returned as the error.
pub fn activate() -> Result<&'static Settings> {
    if let Some(settings) = *ACTIVE.read().unwrap_or_else(|e| e.into_inner()) {
        return Ok(settings);
    }
    let (settings, error) = match load() {
        Ok(settings) => (settings, None),
        Err(e) => (Settings::default(), Some(e)),
    };
    let active = install(settings);
    error.map_or(Ok(active), Err)
}

/// The settings in force: the loaded ones, or the defaults before `activate`.
pub fn active() -> &'static Settings {
    static DEFAULT: OnceLock<Settings> = OnceLock::new();
    let current = *ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    current.unwrap_or_else(|| DEFAULT.get_or_init(Settings::default))
}

fn install(settings: Settings) -> &'static Settings {
    let leaked: &'static Settings = Box::leak(Box::new(settings.sanitized()));
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(leaked);
    leaked
}

/// Puts `settings` in force at once, for every reader. Settings change a handful of times in a run, so each old value is simply kept.
pub fn set(settings: Settings) -> &'static Settings {
    install(settings)
}

/// Saves the settings in force, atomically. Called after every `set`, it always writes the newest value, however saves are ordered.
pub fn persist() -> Result<()> {
    static SAVING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    save_to(&paths::data_dir(), active())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(dir.path()).unwrap(), Settings::default());
    }

    #[test]
    fn partial_files_keep_defaults_for_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"reconnect":{"max_attempts":2}}"#).unwrap();
        let s = load_from(dir.path()).unwrap();
        assert_eq!(s.reconnect.max_attempts, 2);
        assert_eq!(s.reconnect.backoff_secs, Reconnect::default().backoff_secs);
        assert_eq!(s.crash_restart, CrashRestart::default());
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Settings::default();
        s.reconnect.enabled = false;
        s.crash_restart.max_attempts = 1;
        save_to(dir.path(), &s).unwrap();
        assert_eq!(load_from(dir.path()).unwrap(), s);
    }

    #[test]
    fn damaged_file_is_an_error_and_stays() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), b"{").unwrap();
        assert!(load_from(dir.path()).is_err());
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), b"{");
    }

    #[test]
    fn new_keys_default_on_and_old_files_keep_working() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"reconnect":{"enabled":false}}"#).unwrap();
        let s = load_from(dir.path()).unwrap();
        assert!(!s.reconnect.enabled);
        assert!(s.merged_chat && s.broadcast && s.notifications.attention && s.notifications.whispers && s.auto_update && !s.prereleases);
        std::fs::write(dir.path().join(FILE), r#"{"merged_chat":false,"notifications":{"whispers":false}}"#).unwrap();
        let s = load_from(dir.path()).unwrap();
        assert!(!s.merged_chat && !s.notifications.whispers && s.notifications.attention);
        std::fs::write(dir.path().join(FILE), r#"{"update_check":false}"#).unwrap();
        assert!(!load_from(dir.path()).unwrap().auto_update);
    }

    #[test]
    fn a_damaged_file_is_kept_as_a_backup_and_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), br#"{"reconnect": oops"#).unwrap();
        let error = format!("{:#}", load_from(dir.path()).unwrap_err());
        assert!(error.contains(BACKUP) && error.contains("defaults apply"), "{error}");
        assert_eq!(std::fs::read(dir.path().join(BACKUP)).unwrap(), br#"{"reconnect": oops"#);
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), br#"{"reconnect": oops"#);
    }

    #[test]
    fn saving_after_a_damaged_load_keeps_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), b"{ hand edited").unwrap();
        assert!(load_from(dir.path()).is_err());
        let mut changed = Settings::default();
        changed.merged_chat = false;
        save_to(dir.path(), &changed).unwrap();
        assert_eq!(std::fs::read(dir.path().join(BACKUP)).unwrap(), b"{ hand edited");
        assert_eq!(load_from(dir.path()).unwrap(), changed);
    }

    #[test]
    fn a_healthy_file_leaves_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        save_to(dir.path(), &Settings::default()).unwrap();
        load_from(dir.path()).unwrap();
        assert!(!dir.path().join(BACKUP).exists());
    }

    #[test]
    fn broken_values_are_put_back_in_range() {
        let mut s = Settings::default();
        s.reconnect.backoff_secs = Vec::new();
        s.reconnect.patient_secs = vec![0, u64::MAX];
        s.reconnect.jitter_percent = 900;
        s.crash_restart.max_attempts = u32::MAX;
        let s = s.sanitized();
        assert_eq!(s.reconnect.backoff_secs, Reconnect::default().backoff_secs);
        assert_eq!(s.reconnect.patient_secs, vec![1, MAX_SECS]);
        assert_eq!(s.reconnect.jitter_percent, 100);
        assert_eq!(s.crash_restart.max_attempts, MAX_ATTEMPTS);
    }
}
