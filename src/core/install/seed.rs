//! Game files other launchers on this machine already downloaded, copied instead of fetched when their SHA-1 matches.

use std::path::{Component, Path, PathBuf};

use super::java;
use super::meta::{Arch, Os};

const STORE_PACKAGE: &str = "Microsoft.4297127D64EC6_8wekyb3d8bbwe";

/// Where other launchers keep the files Manycraft installs below its data folder.
#[derive(Clone, Debug, Default)]
pub struct Seeds {
    root: PathBuf,
    launchers: Vec<PathBuf>,
    platforms: &'static [&'static str],
}

impl Seeds {
    /// Seeds for files below the data folder `root` from the launcher data folders `launchers`.
    pub fn new(root: &Path, launchers: Vec<PathBuf>) -> Self {
        Seeds { root: root.to_path_buf(), launchers, platforms: java::platform_keys(Os::current(), Arch::current()) }
    }

    /// Where the launchers would keep the file Manycraft wants at `path`: assets, libraries and versions in the same
    /// layout, Java runtime files below `runtime/<component>/<platform>/<component>`.
    pub fn candidates(&self, path: &Path) -> Vec<PathBuf> {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return Vec::new();
        };
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Vec::new();
        }
        let mut parts = rel.components();
        let Some(first) = parts.next().and_then(|c| c.as_os_str().to_str()) else {
            return Vec::new();
        };
        match first {
            "assets" | "libraries" | "versions" => self.launchers.iter().map(|l| l.join(rel)).collect(),
            "runtimes" => {
                let (Some(component), Some(_hash)) = (parts.next(), parts.next()) else {
                    return Vec::new();
                };
                let file = parts.as_path();
                if file.as_os_str().is_empty() {
                    return Vec::new();
                }
                let component = component.as_os_str();
                self.launchers.iter().flat_map(|l| self.platforms.iter().map(move |p| l.join("runtime").join(component).join(p).join(component).join(file))).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Data folders of the official launcher, its Microsoft Store copy and Prism that exist on this machine.
/// `MANYCRAFT_LAUNCHERS` replaces them with its list of folders; an empty value turns seeding off.
pub fn launchers() -> Vec<PathBuf> {
    if let Some(list) = std::env::var_os("MANYCRAFT_LAUNCHERS") {
        return std::env::split_paths(&list).filter(|p| !p.as_os_str().is_empty()).collect();
    }
    let mut found = official(Os::current(), dirs::config_dir(), dirs::data_dir(), dirs::data_local_dir(), dirs::home_dir());
    found.extend(crate::core::prism::data_dirs());
    found.retain(|d| d.is_dir());
    found
}

fn official(os: Os, roaming: Option<PathBuf>, data: Option<PathBuf>, local: Option<PathBuf>, home: Option<PathBuf>) -> Vec<PathBuf> {
    match os {
        Os::Windows => roaming.map(|d| d.join(".minecraft")).into_iter().chain(local.map(|d| d.join("Packages").join(STORE_PACKAGE).join("LocalCache").join("Local"))).collect(),
        Os::Osx => data.map(|d| d.join("minecraft")).into_iter().collect(),
        Os::Linux => home.map(|d| d.join(".minecraft")).into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeds() -> Seeds {
        Seeds { root: PathBuf::from("data"), launchers: vec![PathBuf::from("mc"), PathBuf::from("prism")], platforms: &["windows-arm64", "windows-x64"] }
    }

    #[test]
    fn shared_folders_map_one_to_one() {
        let s = seeds();
        let object = Path::new("assets").join("objects").join("ab").join("abcd");
        assert_eq!(s.candidates(&Path::new("data").join(&object)), [Path::new("mc").join(&object), Path::new("prism").join(&object)]);
        let jar = Path::new("versions").join("26.3").join("26.3.jar");
        assert_eq!(s.candidates(&Path::new("data").join(&jar))[0], Path::new("mc").join(&jar));
        assert_eq!(s.candidates(&Path::new("data").join("libraries").join("a.jar")).len(), 2);
    }

    #[test]
    fn runtime_files_map_to_the_launcher_layout_for_every_platform_key() {
        let s = Seeds { launchers: vec![PathBuf::from("mc")], ..seeds() };
        let ours = Path::new("data").join("runtimes").join("java-runtime-epsilon").join("0123").join("bin").join("javaw.exe");
        let base = Path::new("mc").join("runtime").join("java-runtime-epsilon");
        assert_eq!(
            s.candidates(&ours),
            [base.join("windows-arm64").join("java-runtime-epsilon").join("bin").join("javaw.exe"), base.join("windows-x64").join("java-runtime-epsilon").join("bin").join("javaw.exe")]
        );
        assert!(s.candidates(&Path::new("data").join("runtimes").join("java-runtime-epsilon").join("0123")).is_empty());
    }

    #[test]
    fn other_paths_have_no_candidates() {
        let s = seeds();
        assert!(s.candidates(Path::new("elsewhere/assets/x")).is_empty());
        assert!(s.candidates(&Path::new("data").join("instances").join("x")).is_empty());
        assert!(s.candidates(&Path::new("data").join("assets").join("..").join("x")).is_empty());
        assert!(Seeds::default().candidates(Path::new("assets/x")).is_empty());
    }

    #[test]
    fn official_folders_per_os() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(official(Os::Windows, p("roam"), None, p("local"), None), [Path::new("roam").join(".minecraft"), Path::new("local").join("Packages").join(STORE_PACKAGE).join("LocalCache").join("Local")]);
        assert_eq!(official(Os::Osx, None, p("support"), None, p("home")), [Path::new("support").join("minecraft")]);
        assert_eq!(official(Os::Linux, None, None, None, p("home")), [Path::new("home").join(".minecraft")]);
        assert!(official(Os::Linux, None, None, None, None).is_empty());
    }
}
