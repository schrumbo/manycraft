pub mod account;
pub mod auth;
#[allow(dead_code)]
pub mod batch;
pub mod browser;
pub mod cli;
#[allow(dead_code)]
pub mod companion;
#[allow(dead_code)]
pub mod drive;
#[allow(dead_code)]
pub mod fit;
pub mod fleet;
pub mod game_icon;
pub mod game_window;
#[allow(dead_code)]
pub mod history;
pub mod i18n;
pub mod install;
#[allow(dead_code)]
pub mod ipc;
pub mod launch;
#[allow(dead_code)]
pub mod lock;
pub mod modicons;
pub mod modrinth;
pub mod net;
pub mod paths;
pub mod prism;
pub mod proxy;
pub mod reconnect;
pub mod scrub;
pub mod skins;
#[allow(dead_code)]
pub mod settings;
#[cfg(any(windows, target_os = "macos"))]
pub mod signin_window;
pub mod skin;
pub mod still;
pub mod store;
pub mod updater;
pub mod webauth;
