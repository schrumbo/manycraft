//! Opt-in start options of an instance: own Java, game arguments, window size, graphics card and renderer.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::i18n::trf;

/// Window sizes Minecraft accepts at start.
pub const WINDOW_MIN: u32 = 320;
pub const WINDOW_MAX: u32 = 16384;

/// Game arguments Manycraft sets itself; a game argument of the user may not repeat them.
const MANAGED_GAME: [&str; 15] = [
    "--username",
    "--uuid",
    "--accessToken",
    "--userType",
    "--userProperties",
    "--version",
    "--versionType",
    "--gameDir",
    "--assetsDir",
    "--assetIndex",
    "--quickPlayMultiplayer",
    "--quickPlayPath",
    "--width",
    "--height",
    "--graphicsBackend",
];

/// Window size the game opens with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

/// Which graphics card Windows gives the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gpu {
    HighPerformance,
    PowerSaving,
}

/// Which graphics interface the game draws with, on versions that offer a choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Renderer {
    OpenGl,
    Vulkan,
}

impl Renderer {
    /// The value of the game's `--graphicsBackend` argument.
    pub fn argument(self) -> &'static str {
        match self {
            Self::OpenGl => "opengl",
            Self::Vulkan => "vulkan",
        }
    }
}

/// Start options that are all off until the user sets them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOptions {
    /// Java launcher the user picked instead of the runtime Mojang names for the version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<Gpu>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Renderer>,
}

impl LaunchOptions {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Arguments these options add to the game on `game_version`.
    pub fn game_arguments(&self, game_version: &str) -> Vec<String> {
        let mut out = self.game_args.clone();
        if let Some(size) = self.window {
            out.extend(["--width".to_owned(), size.width.to_string(), "--height".to_owned(), size.height.to_string()]);
        }
        if let Some(renderer) = self.renderer.filter(|_| renderer_choice(game_version)) {
            out.extend(["--graphicsBackend".to_owned(), renderer.argument().to_owned()]);
        }
        out
    }
}

/// Whether `game_version` lets the player choose between OpenGL and Vulkan: 26.2 and newer, with their snapshots.
pub fn renderer_choice(game_version: &str) -> bool {
    let mut parts = game_version.split(['.', '-', ' ']);
    let (Some(Ok(year)), Some(Ok(drop))) = (parts.next().map(str::parse::<u32>), parts.next().map(str::parse::<u32>)) else { return false };
    year > 26 || (year == 26 && drop >= 2)
}

/// Why `args` cannot be Java arguments: every one starts with "-" and the memory has its own setting.
pub fn check_jvm_args(args: &[String]) -> Result<(), String> {
    if let Some(arg) = args.iter().find(|a| !a.starts_with('-')) {
        return Err(trf("Jedes Java-Argument beginnt mit „-“, „%@“ nicht.", &[arg]));
    }
    if args.iter().any(|a| a.starts_with("-Xmx") || a.starts_with("-Xms")) {
        return Err(super::i18n::tr("Den Arbeitsspeicher stellst du unter „Arbeitsspeicher“ ein."));
    }
    Ok(())
}

/// Why `args` cannot be game arguments: Manycraft sets the account, folders, window size and renderer itself.
pub fn check_game_args(args: &[String]) -> Result<(), String> {
    match args.iter().find(|a| MANAGED_GAME.iter().any(|m| a.split('=').next() == Some(*m))) {
        Some(arg) => Err(trf("„%@“ setzt Manycraft selbst.", &[&arg.split('=').next().unwrap_or(arg)])),
        None => Ok(()),
    }
}

/// The window size typed as `width` and `height`; both empty means the game's own size.
pub fn parse_window(width: &str, height: &str) -> Result<Option<WindowSize>, String> {
    let (width, height) = (width.trim(), height.trim());
    if width.is_empty() && height.is_empty() {
        return Ok(None);
    }
    let number = |text: &str| text.parse::<u32>().ok().filter(|n| (WINDOW_MIN..=WINDOW_MAX).contains(n));
    match (number(width), number(height)) {
        (Some(width), Some(height)) => Ok(Some(WindowSize { width, height })),
        _ if width.is_empty() || height.is_empty() => Err(super::i18n::tr("Gib Breite und Höhe an.")),
        _ => Err(trf("Breite und Höhe sind ganze Zahlen von %@ bis %@.", &[&WINDOW_MIN, &WINDOW_MAX])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_renderer_choice_starts_with_26_2() {
        for yes in ["26.2", "26.2.1", "26.3", "26.2-snapshot-4", "26.3-pre1", "27.1"] {
            assert!(renderer_choice(yes), "{yes}");
        }
        for no in ["26.1", "26.1.2", "1.21.5", "1.8.9", "b1.7.3", "25w14a", ""] {
            assert!(!renderer_choice(no), "{no}");
        }
    }

    #[test]
    fn options_add_window_size_and_renderer_only_where_known() {
        let options = LaunchOptions { game_args: args(&["--demo"]), window: Some(WindowSize { width: 1280, height: 720 }), renderer: Some(Renderer::Vulkan), ..LaunchOptions::default() };
        assert_eq!(options.game_arguments("26.3"), args(&["--demo", "--width", "1280", "--height", "720", "--graphicsBackend", "vulkan"]));
        assert_eq!(options.game_arguments("1.8.9"), args(&["--demo", "--width", "1280", "--height", "720"]));
        assert!(LaunchOptions::default().game_arguments("26.3").is_empty());
    }

    #[test]
    fn default_options_are_not_written() {
        assert_eq!(serde_json::to_string(&LaunchOptions::default()).unwrap(), "{}");
        let back: LaunchOptions = serde_json::from_str(r#"{"gpu":"high_performance","renderer":"open_gl"}"#).unwrap();
        assert_eq!((back.gpu, back.renderer), (Some(Gpu::HighPerformance), Some(Renderer::OpenGl)));
    }

    #[test]
    fn java_arguments_start_with_a_dash_and_leave_the_memory_alone() {
        assert!(check_jvm_args(&args(&["-XX:+UseG1GC", "-Dfoo=bar"])).is_ok());
        assert!(check_jvm_args(&args(&["-XX:+UseG1GC", "G1"])).unwrap_err().contains("G1"));
        assert!(check_jvm_args(&args(&["-Xmx8G"])).is_err());
    }

    #[test]
    fn game_arguments_may_not_repeat_what_manycraft_sets() {
        assert!(check_game_args(&args(&["--demo", "--fullscreen"])).is_ok());
        assert!(check_game_args(&args(&["--gameDir", "x"])).unwrap_err().contains("--gameDir"));
        assert!(check_game_args(&args(&["--accessToken=x"])).unwrap_err().contains("--accessToken"));
    }

    #[test]
    fn a_window_size_needs_both_numbers_in_range() {
        assert_eq!(parse_window(" ", ""), Ok(None));
        assert_eq!(parse_window("1280", " 720 "), Ok(Some(WindowSize { width: 1280, height: 720 })));
        assert!(parse_window("1280", "").is_err());
        assert!(parse_window("12", "720").is_err());
        assert!(parse_window("wide", "720").is_err());
    }
}
