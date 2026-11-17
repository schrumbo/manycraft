//! App-wide settings in `settings.json`: chat, notifications, how instances reconnect and restart. Missing keys take their defaults.

use std::path::Path;
use std::sync::{Mutex, OnceLock, RwLock};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{paths, phone, store};

const FILE: &str = "settings.json";
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

/// How often the tiles on screen refresh their previews, against what it costs the games.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewRate {
    /// Half as often.
    Saving,
    #[default]
    Normal,
    /// Twice as often.
    Smooth,
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
    /// Whether attention also goes to the phone, and how.
    pub phone: phone::Phone,
    /// Whether a newer release found by the daily check is downloaded right away and installed when the app quits.
    #[serde(alias = "update_check")]
    pub auto_update: bool,
    /// Whether pre-releases count as updates.
    pub prereleases: bool,
    /// Whether each game shows a small badge with the account's name and colour in its corner.
    pub game_badge: bool,
    /// Whether the computer is kept from sleeping on its own while a game runs.
    pub prevent_sleep: bool,
    /// Whether the hint that closing the window keeps Manycraft in the notification area was seen.
    pub tray_explained: bool,
    /// Whether the first game shown in the manager's place showed how to close it and get back.
    pub swap_explained: bool,
    pub preview_rate: PreviewRate,
    /// Fields a newer version wrote that this one does not know, written back unchanged.
    #[serde(flatten)]
    pub kept: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { reconnect: Reconnect::default(), crash_restart: CrashRestart::default(), merged_chat: true, broadcast: true, notifications: Notifications::default(), phone: phone::Phone::default(), auto_update: true, prereleases: false, game_badge: true, prevent_sleep: true, tray_explained: false, swap_explained: false, preview_rate: PreviewRate::Normal, kept: serde_json::Map::new() }
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

/// Loads `settings.json` from the data folder; an absent file gives the defaults, a damaged one its backup or the defaults.
pub fn load() -> Result<Settings> {
    load_from(&paths::data_dir())
}

fn load_from(dir: &Path) -> Result<Settings> {
    Ok(store::load::<Settings>(&dir.join(FILE))?.unwrap_or_default().sanitized())
}

fn save_to(dir: &Path, settings: &Settings) -> Result<()> {
    store::replace::<Settings>(&dir.join(FILE), &serde_json::to_vec_pretty(settings)?)
}

static ACTIVE: RwLock<Option<&'static Settings>> = RwLock::new(None);

/// Loads the settings once for the whole run. An unreadable file leaves the defaults in force and is returned as the error.
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
    fn a_damaged_file_without_backup_gives_the_defaults_and_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, br#"{"reconnect": oops"#).unwrap();
        assert_eq!(load_from(dir.path()).unwrap(), Settings::default());
        assert_eq!(std::fs::read(store::damaged_path(&path)).unwrap(), br#"{"reconnect": oops"#);
    }

    #[test]
    fn new_keys_default_on_and_old_files_keep_working() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"reconnect":{"enabled":false}}"#).unwrap();
        let s = load_from(dir.path()).unwrap();
        assert!(!s.reconnect.enabled);
        assert!(s.merged_chat && s.broadcast && s.notifications.attention && s.notifications.whispers && s.auto_update && !s.prereleases && s.prevent_sleep);
        std::fs::write(dir.path().join(FILE), r#"{"merged_chat":false,"notifications":{"whispers":false}}"#).unwrap();
        let s = load_from(dir.path()).unwrap();
        assert!(!s.merged_chat && !s.notifications.whispers && s.notifications.attention);
        std::fs::write(dir.path().join(FILE), r#"{"update_check":false}"#).unwrap();
        assert!(!load_from(dir.path()).unwrap().auto_update);
    }

    #[test]
    fn a_file_of_another_shape_is_an_error_and_stays() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), br#"{"reconnect":5}"#).unwrap();
        assert!(load_from(dir.path()).unwrap_err().downcast_ref::<store::Untouched>().is_some());
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), br#"{"reconnect":5}"#);
        save_to(dir.path(), &Settings::default()).unwrap();
        assert_eq!(std::fs::read(store::damaged_path(&dir.path().join(FILE))).unwrap(), br#"{"reconnect":5}"#);
    }

    #[test]
    fn a_damaged_file_comes_back_from_the_previous_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = Settings::default();
        first.merged_chat = false;
        save_to(dir.path(), &first).unwrap();
        save_to(dir.path(), &Settings::default()).unwrap();
        std::fs::write(dir.path().join(FILE), b"{ hand edited").unwrap();
        assert_eq!(load_from(dir.path()).unwrap(), first);
        assert_eq!(load_from(dir.path()).unwrap(), first);
    }

    #[test]
    fn a_healthy_file_leaves_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        save_to(dir.path(), &Settings::default()).unwrap();
        load_from(dir.path()).unwrap();
        assert!(!store::backup_path(&dir.path().join(FILE)).exists());
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
