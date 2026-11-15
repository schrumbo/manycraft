//! What the manager window remembers across restarts in `window.json`: where it was, how big, on which monitor, whether maximized, which page it showed and whether the sidebar was open.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{net, paths};

const FILE: &str = "window.json";

/// The page the window showed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    #[default]
    Grid,
    Accounts,
    Chat,
}

/// A window's frame in the units of the window system: logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Frame {
    fn sound(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite() && v.abs() < 1e6) && self.w >= 1. && self.h >= 1.
    }
}

/// Everything remembered about the manager window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    /// The frame the window has when it is not maximized; `None` before the first save.
    pub frame: Option<Frame>,
    pub maximized: bool,
    /// The monitor the window was on.
    pub display: Option<String>,
    pub page: Page,
    /// The batch of the grid page; the grid of all when `None`.
    pub batch: Option<String>,
    pub sidebar: bool,
}

impl Default for Saved {
    fn default() -> Self {
        Self { frame: None, maximized: false, display: None, page: Page::Grid, batch: None, sidebar: true }
    }
}

fn path() -> PathBuf {
    paths::data_dir().join(FILE)
}

/// What was remembered; the defaults when nothing was saved or the file is unreadable.
pub fn load() -> Saved {
    load_from(&path())
}

/// Remembers `saved`.
pub fn save(saved: &Saved) -> Result<()> {
    save_to(&path(), saved)
}

fn load_from(path: &Path) -> Saved {
    let mut saved: Saved = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    saved.frame = saved.frame.filter(Frame::sound);
    saved
}

fn save_to(path: &Path, saved: &Saved) -> Result<()> {
    net::write_atomic(path, &serde_json::to_vec_pretty(saved).context("cannot encode the window state")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> Saved {
        Saved { frame: Some(Frame { x: -1500.5, y: 139., w: 1280., h: 800. }), maximized: true, display: Some("d".into()), page: Page::Chat, batch: Some("b".into()), sidebar: false }
    }

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        save_to(&file, &saved()).unwrap();
        assert_eq!(load_from(&file), saved());
    }

    #[test]
    fn missing_or_broken_files_give_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        assert_eq!(load_from(&file), Saved::default());
        std::fs::write(&file, b"{ not json").unwrap();
        assert_eq!(load_from(&file), Saved::default());
        std::fs::write(&file, b"{\"page\":\"somewhere\"}").unwrap();
        assert_eq!(load_from(&file), Saved::default());
    }

    #[test]
    fn missing_keys_take_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        std::fs::write(&file, b"{\"maximized\":true}").unwrap();
        assert_eq!(load_from(&file), Saved { maximized: true, ..Saved::default() });
    }

    #[test]
    fn absurd_frames_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        for frame in ["{\"x\":0,\"y\":0,\"w\":0,\"h\":800}", "{\"x\":1e30,\"y\":0,\"w\":800,\"h\":800}", "{\"x\":0,\"y\":0,\"w\":800,\"h\":-5}"] {
            std::fs::write(&file, format!("{{\"frame\":{frame}}}")).unwrap();
            assert_eq!(load_from(&file).frame, None, "{frame}");
        }
    }
}
