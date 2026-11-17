//! Which line of builds this is. A build whose version carries a pre-release tag is a beta: it keeps its own data folder,
//! keychain entries and system identity, so it never touches the data or the install of a stable Manycraft.

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn beta_version(version: &str) -> bool {
    version.contains('-')
}

fn name_of(beta: bool) -> &'static str {
    if beta { "Manycraft Beta" } else { "Manycraft" }
}

fn app_id_of(beta: bool) -> &'static str {
    if beta { "io.github.schrumbo.manycraft.beta" } else { "io.github.schrumbo.manycraft" }
}

/// Whether this build is a beta.
pub fn is_beta() -> bool {
    beta_version(VERSION)
}

/// The name people see for this build.
pub fn name() -> &'static str {
    name_of(is_beta())
}

/// The folder name under the local data folder.
pub fn data_folder() -> &'static str {
    name()
}

/// The keychain service every secret of this build is stored under.
pub fn keychain_service() -> &'static str {
    name()
}

/// The identity of this build to the operating system: window class, Windows AppUserModelID, macOS bundle identifier and the Linux desktop entry.
pub fn app_id() -> &'static str {
    app_id_of(is_beta())
}

/// The registry key of the Windows install entry.
#[cfg(windows)]
pub fn uninstall_key() -> &'static str {
    if is_beta() {
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ManycraftBeta"
    } else {
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Manycraft"
    }
}

/// The ending an installer of this channel carries before its extension, which keeps the installers of the other channel out of reach.
pub fn asset_tag() -> &'static str {
    asset_tag_of(is_beta())
}

fn asset_tag_of(beta: bool) -> &'static str {
    if beta { "-beta" } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_pre_release_tag_makes_a_beta() {
        assert!(beta_version("0.10.0-beta.1"));
        assert!(!beta_version("0.9.3"));
        assert!(!beta_version("0.10.0"));
    }

    #[test]
    fn the_channels_share_no_name_or_identity() {
        assert_ne!(name_of(true), name_of(false));
        assert_ne!(app_id_of(true), app_id_of(false));
        assert_ne!(asset_tag_of(true), asset_tag_of(false));
        assert_eq!(app_id_of(false), "io.github.schrumbo.manycraft");
        assert_eq!(name_of(false), "Manycraft");
    }
}
