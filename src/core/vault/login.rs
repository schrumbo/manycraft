//! How a game's login library (Mojang's authlib) can be pointed at the vault, read from the authlib on its class path.

use std::path::PathBuf;

/// The oldest Minecraft release whose login library can be pointed at the vault (authlib 1.6).
pub const FIRST_VERSION: &str = "1.16";

/// The JVM options a login library reads to find Mojang's services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    /// authlib 10 and later fetch one discovery document that names every service.
    Discovery,
    /// Older authlib takes one base address per service host; all of them must be set.
    Hosts(&'static [&'static str]),
}

impl Login {
    /// The login library on `classpath`; none when there is no authlib or it is too old to be pointed elsewhere.
    pub fn of(classpath: &[PathBuf]) -> Option<Self> {
        classpath.iter().find_map(|p| p.file_name()?.to_str()?.strip_prefix("authlib-")?.strip_suffix(".jar").map(str::to_owned)).and_then(|v| Self::of_version(&v))
    }

    fn of_version(version: &str) -> Option<Self> {
        let mut parts = version.split(['.', '-']).map(str::parse::<u32>);
        let (Some(Ok(major)), Some(Ok(minor))) = (parts.next(), parts.next()) else { return None };
        Some(match major {
            10.. => Self::Discovery,
            7..=9 => Self::Hosts(&["session", "services", "profiles"]),
            6 => Self::Hosts(&["session", "services"]),
            5 => Self::Hosts(&["account", "session", "services"]),
            2..=4 => Self::Hosts(&["auth", "account", "session", "services"]),
            1 if minor >= 6 => Self::Hosts(&["auth", "account", "session", "services"]),
            _ => return None,
        })
    }

    /// The options that send the library to the vault at `base` (`http://127.0.0.1:port`).
    pub fn jvm_args(self, base: &str) -> Vec<String> {
        match self {
            Self::Discovery => vec![format!("-Dminecraft.api.discovery.host={base}{}", super::routes::DISCOVERY_PATH)],
            Self::Hosts(hosts) => hosts.iter().map(|h| format!("-Dminecraft.api.{h}.host={base}")).collect(),
        }
    }
}

/// Whether Minecraft `game_version` can hide its login, judged from the name alone: releases from
/// [`FIRST_VERSION`] on, year-numbered releases and snapshots from 2020 on. Unknown names count as able; the start
/// checks the real library.
pub fn may_hide(game_version: &str) -> bool {
    let lower = game_version.to_ascii_lowercase();
    if ["a", "b", "c", "rd-", "inf-"].iter().any(|p| lower.starts_with(p) && lower[p.len()..].starts_with(|c: char| c.is_ascii_digit())) {
        return false;
    }
    if let Some((year, _)) = lower.split_once('w')
        && year.len() == 2
        && let Ok(year) = year.parse::<u32>()
    {
        return year >= 20;
    }
    let mut parts = lower.split(['.', '-', ' ', '_']).map(str::parse::<u32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(1)), Some(Ok(minor))) => minor >= 16,
        _ => true,
    }
}
