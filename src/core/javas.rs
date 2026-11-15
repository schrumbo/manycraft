//! Java installations on this computer, for an instance that starts with its own Java instead of Mojang's runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::i18n::trf;

/// A Java installation and the version it reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Java {
    /// The launcher a game starts with: `javaw.exe` on Windows, `java` elsewhere.
    pub path: PathBuf,
    pub version: String,
    pub major: u32,
    pub vendor: Option<String>,
}

/// The Java version and vendor in the `release` file of a Java home.
pub fn parse_release(text: &str) -> Option<(String, Option<String>)> {
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == name).then(|| value.trim().trim_matches('"').to_owned()).filter(|v| !v.is_empty())
        })
    };
    Some((field("JAVA_VERSION")?, field("IMPLEMENTOR")))
}

/// The version in the output of `java -version`, such as `17.0.2` from `openjdk version "17.0.2" 2022-01-18`.
pub fn parse_version_output(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains(" version \""))?;
    line.split('"').nth(1).filter(|v| !v.is_empty()).map(str::to_owned)
}

/// `path` without the `\\?\` prefix `canonicalize` puts before a drive path on Windows, as the file picker and Windows'
/// own graphics settings write it.
pub fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with(r"UNC\") => PathBuf::from(rest),
        _ => path,
    }
}

/// The major version of a Java version string: 8 for `1.8.0_402`, 21 for `21.0.1`, 25 for `25`.
pub fn major(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 { parts.next()?.parse().ok() } else { Some(first) }
}

/// Why Java `major` cannot start a game that needs Java `required`; games that need Java before 16 run only on their own
/// Java, newer ones on that Java or later.
pub fn mismatch(required: u32, major: u32, game: &str) -> Option<String> {
    let fits = if required < 16 { major == required } else { major >= required };
    (!fits).then(|| trf("Minecraft %@ braucht Java %@, gewählt ist Java %@.", &[&game, &required, &major]))
}

/// The Java a game needs: the major version its metadata names, else what its runtime component stands for.
pub fn required_major(major_version: Option<u32>, component: Option<&str>) -> u32 {
    major_version.unwrap_or(match component {
        Some("java-runtime-alpha") => 16,
        Some("java-runtime-beta" | "java-runtime-gamma" | "java-runtime-gamma-snapshot") => 17,
        Some("java-runtime-delta") => 21,
        Some("java-runtime-epsilon") => 25,
        _ => 8,
    })
}

/// The Java `game_version` needs: what its downloaded metadata says, else what Mojang ships for that release line, else
/// `None` for a version not known yet.
pub fn expected_major(game_version: &str) -> Option<u32> {
    let saved = super::paths::data_dir().join("versions").join(game_version).join(format!("{game_version}.json"));
    let declared = std::fs::read(saved).ok().and_then(|bytes| serde_json::from_slice::<super::install::meta::VersionJson>(&bytes).ok()).and_then(|v| v.java_version);
    match declared {
        Some(java) => Some(required_major(java.major_version, Some(&java.component))),
        None => release_major(game_version),
    }
}

fn release_major(game_version: &str) -> Option<u32> {
    if game_version.starts_with(['a', 'b', 'c', 'r', 'i']) {
        return Some(8);
    }
    let parts: Vec<u32> = game_version.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
    Some(match parts.as_slice() {
        [1, minor, ..] if *minor <= 16 => 8,
        [1, 17, ..] => 16,
        [1, 18 | 19, ..] | [1, 20] | [1, 20, 0..=4] => 17,
        [1, 20, _] | [1, 21, ..] => 21,
        [year, ..] if *year >= 26 => 25,
        _ => return None,
    })
}

fn launcher_name() -> &'static str {
    if cfg!(windows) { "javaw.exe" } else { "java" }
}

fn console_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

/// The Java home a picked file or folder belongs to: the folder itself, the folder above `bin`, or a macOS bundle's `Contents/Home`.
pub fn home_of(picked: &Path) -> Option<PathBuf> {
    let candidates = if picked.is_dir() {
        vec![picked.to_path_buf(), picked.join("Contents").join("Home"), picked.join("jre.bundle").join("Contents").join("Home")]
    } else {
        vec![picked.parent()?.parent()?.to_path_buf()]
    };
    candidates.into_iter().find(|home| home.join("bin").join(console_name()).is_file() || home.join("bin").join(launcher_name()).is_file())
}

/// Reads the Java at `home`: the `release` file when it has one, else what `java -version` prints.
pub fn probe(home: &Path) -> Result<Java> {
    let bin = home.join("bin");
    let launcher = [launcher_name(), console_name()].into_iter().map(|n| bin.join(n)).find(|p| p.is_file()).with_context(|| trf("In %@ ist kein Java.", &[&home.display()]))?;
    let (version, vendor) = match std::fs::read_to_string(home.join("release")).ok().as_deref().and_then(parse_release) {
        Some(found) => found,
        None => (ask_version(&bin.join(console_name()))?, None),
    };
    let Some(major) = major(&version) else { bail!(trf("Die Java-Version „%@“ ist unbekannt.", &[&version])) };
    Ok(Java { path: launcher, version, major, vendor })
}

fn ask_version(java: &Path) -> Result<String> {
    let mut command = Command::new(java);
    command.arg("-version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().with_context(|| format!("cannot run {}", java.display()))?;
    parse_version_output(&String::from_utf8_lossy(&output.stderr)).with_context(|| format!("{} prints no version", java.display()))
}

/// Java installations found in the usual places of this system, newest first; Manycraft's own runtimes are left out.
pub fn detect() -> Vec<Java> {
    let mut homes = candidate_homes();
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        homes.push(PathBuf::from(home));
    }
    let own = super::paths::data_dir();
    let mut found: BTreeMap<PathBuf, Java> = BTreeMap::new();
    for home in homes {
        let Ok(home) = home.canonicalize().map(plain) else { continue };
        if home.starts_with(&own) || found.contains_key(&home) {
            continue;
        }
        if let Ok(java) = probe(&home) {
            found.insert(home, java);
        }
    }
    let mut list: Vec<Java> = found.into_values().collect();
    list.sort_by(|a, b| b.major.cmp(&a.major).then_with(|| a.path.cmp(&b.path)));
    list
}

fn children(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
}

#[cfg(windows)]
fn candidate_homes() -> Vec<PathBuf> {
    let mut homes = registry_homes();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        let Some(base) = std::env::var_os(var).map(PathBuf::from) else { continue };
        for vendor in ["Java", "Eclipse Adoptium", "Eclipse Foundation", "AdoptOpenJDK", "Microsoft", "Zulu", "BellSoft", "Amazon Corretto", "Semeru", "OpenJDK"] {
            homes.extend(children(&base.join(vendor)));
        }
    }
    homes
}

#[cfg(windows)]
fn registry_homes() -> Vec<PathBuf> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn subkeys(path: &str) -> Vec<String> {
        let mut key: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide(path).as_ptr(), 0, KEY_READ | KEY_WOW64_64KEY, &mut key) } != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut out = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let status = unsafe { RegEnumKeyExW(key, index, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
            if status != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        unsafe { RegCloseKey(key) };
        out
    }

    fn string(path: &str, value: &str) -> Option<String> {
        let (path, value) = (wide(path), wide(value));
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let status = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, path.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
        if status != ERROR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    let mut homes = Vec::new();
    for root in [r"SOFTWARE\JavaSoft\Java Runtime Environment", r"SOFTWARE\JavaSoft\Java Development Kit", r"SOFTWARE\JavaSoft\JRE", r"SOFTWARE\JavaSoft\JDK"] {
        for version in subkeys(root) {
            homes.extend(string(&format!(r"{root}\{version}"), "JavaHome"));
        }
    }
    for root in [r"SOFTWARE\Eclipse Adoptium\JDK", r"SOFTWARE\Eclipse Adoptium\JRE", r"SOFTWARE\Eclipse Foundation\JDK", r"SOFTWARE\AdoptOpenJDK\JDK", r"SOFTWARE\AdoptOpenJDK\JRE", r"SOFTWARE\Microsoft\JDK"] {
        for version in subkeys(root) {
            homes.extend(string(&format!(r"{root}\{version}\hotspot\MSI"), "Path"));
        }
    }
    for flavor in subkeys(r"SOFTWARE\Azul Systems\Zulu") {
        homes.extend(string(&format!(r"SOFTWARE\Azul Systems\Zulu\{flavor}"), "InstallationPath"));
    }
    homes.into_iter().map(PathBuf::from).collect()
}

#[cfg(target_os = "macos")]
fn candidate_homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let mut bundles = children(Path::new("/Library/Java/JavaVirtualMachines"));
    if let Some(home) = dirs::home_dir() {
        bundles.extend(children(&home.join("Library/Java/JavaVirtualMachines")));
        homes.extend(children(&home.join(".sdkman/candidates/java")));
    }
    homes.extend(bundles.iter().map(|b| b.join("Contents/Home")));
    for prefix in ["/opt/homebrew/opt", "/usr/local/opt"] {
        for formula in children(Path::new(prefix)).into_iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("openjdk"))) {
            homes.push(formula.join("libexec/openjdk.jdk/Contents/Home"));
        }
    }
    homes
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidate_homes() -> Vec<PathBuf> {
    let mut homes = children(Path::new("/usr/lib/jvm"));
    homes.extend(children(Path::new("/usr/lib64/jvm")));
    homes.extend(children(Path::new("/opt/java")));
    if let Some(home) = dirs::home_dir() {
        homes.extend(children(&home.join(".sdkman/candidates/java")));
        homes.extend(children(&home.join(".jdks")));
    }
    homes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_file_names_version_and_vendor() {
        let text = "IMPLEMENTOR=\"Eclipse Adoptium\"\nJAVA_RUNTIME_VERSION=\"17.0.2+8\"\nJAVA_VERSION=\"17.0.2\"\nJAVA_VERSION_DATE=\"2022-01-18\"\n";
        assert_eq!(parse_release(text), Some(("17.0.2".to_owned(), Some("Eclipse Adoptium".to_owned()))));
        assert_eq!(parse_release("JAVA_VERSION=\"1.8.0_402\""), Some(("1.8.0_402".to_owned(), None)));
        assert_eq!(parse_release("IMPLEMENTOR=\"x\""), None);
    }

    #[test]
    fn java_version_output_is_read_from_both_spellings() {
        let openjdk = "openjdk version \"17.0.2\" 2022-01-18\nOpenJDK Runtime Environment Temurin-17.0.2+8 (build 17.0.2+8)\n";
        assert_eq!(parse_version_output(openjdk).as_deref(), Some("17.0.2"));
        let oracle = "Picked up _JAVA_OPTIONS: -Xmx1g\njava version \"1.8.0_402\"\nJava(TM) SE Runtime Environment (build 1.8.0_402-b08)\n";
        assert_eq!(parse_version_output(oracle).as_deref(), Some("1.8.0_402"));
        assert_eq!(parse_version_output("Error: could not find java.dll"), None);
    }

    #[test]
    fn major_versions_follow_both_numbering_schemes() {
        assert_eq!(major("1.8.0_402"), Some(8));
        assert_eq!(major("17.0.2"), Some(17));
        assert_eq!(major("21"), Some(21));
        assert_eq!(major("25-ea"), Some(25));
        assert_eq!(major("abc"), None);
    }

    #[test]
    fn a_canonical_windows_path_loses_its_verbatim_prefix() {
        assert_eq!(plain(PathBuf::from(r"\\?\C:\Java\jdk-21")), PathBuf::from(r"C:\Java\jdk-21"));
        assert_eq!(plain(PathBuf::from(r"\\?\UNC\host\share")), PathBuf::from(r"\\?\UNC\host\share"));
        assert_eq!(plain(PathBuf::from("/usr/lib/jvm/java-21")), PathBuf::from("/usr/lib/jvm/java-21"));
    }

    #[test]
    fn old_games_need_their_own_java_and_new_ones_at_least_theirs() {
        assert!(mismatch(8, 8, "1.8.9").is_none());
        assert!(mismatch(8, 17, "1.8.9").unwrap().contains("Java 8"));
        assert!(mismatch(21, 25, "1.21.5").is_none());
        assert!(mismatch(21, 17, "1.21.5").is_some());
        assert!(mismatch(16, 17, "1.17.1").is_none());
        assert!(mismatch(16, 8, "1.17.1").is_some());
        assert_eq!(required_major(Some(17), Some("java-runtime-delta")), 17);
        assert_eq!(required_major(None, Some("jre-legacy")), 8);
        assert_eq!(required_major(None, None), 8);
    }

    #[test]
    fn a_java_home_is_found_from_its_launcher_or_folder() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("jdk-17");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin").join(launcher_name()), b"").unwrap();
        std::fs::write(home.join("release"), "JAVA_VERSION=\"17.0.2\"\nIMPLEMENTOR=\"Vendor\"\n").unwrap();
        assert_eq!(home_of(&home.join("bin").join(launcher_name())).as_deref(), Some(home.as_path()));
        assert_eq!(home_of(&home).as_deref(), Some(home.as_path()));
        assert_eq!(home_of(dir.path()), None);
        let java = probe(&home).unwrap();
        assert_eq!((java.major, java.version.as_str(), java.vendor.as_deref()), (17, "17.0.2", Some("Vendor")));
        assert!(java.path.ends_with(launcher_name()));
    }
}
