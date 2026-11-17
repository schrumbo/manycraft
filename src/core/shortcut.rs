//! Desktop shortcuts that start Manycraft with `--play <instance> [--account <id>]`, and reading those arguments back.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const PLAY_FLAG: &str = "--play";
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn desktop_marker() -> &'static str {
    if super::channel::is_beta() { "X-Manycraft-Beta-Shortcut=true" } else { "X-Manycraft-Shortcut=true" }
}
const ACCOUNT_FLAG: &str = "--account";
const NAME_MAX: usize = 80;
const FALLBACK_NAME: &str = "Manycraft";
const FREE_NAMES: usize = 99;
#[cfg(any(test, target_os = "macos"))]
fn bundle_id() -> String {
    format!("{}.shortcut", super::channel::app_id())
}

/// An instance to play and, when the shortcut names one, the account to play it with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Play {
    pub instance: String,
    pub account: Option<String>,
}

fn plain(id: &str) -> bool {
    id.len() <= 64 && id.starts_with(|c: char| c.is_ascii_alphanumeric()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Play {
    /// The command line arguments that ask for this play.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![PLAY_FLAG.to_owned(), self.instance.clone()];
        if let Some(account) = &self.account {
            args.push(ACCOUNT_FLAG.to_owned());
            args.push(account.clone());
        }
        args
    }

    /// The play a command line asks for; none when `--play` is missing or an id is not a plain id.
    pub fn parse(args: &[String]) -> Option<Self> {
        let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|at| args.get(at + 1)).cloned();
        let instance = value(PLAY_FLAG).filter(|id| plain(id))?;
        let account = match value(ACCOUNT_FLAG) {
            Some(id) if plain(&id) => Some(id),
            Some(_) => return None,
            None => None,
        };
        Some(Self { instance, account })
    }

    fn checked(&self) -> Result<()> {
        if !plain(&self.instance) || self.account.as_deref().is_some_and(|a| !plain(a)) {
            bail!("not a valid instance or account id");
        }
        Ok(())
    }
}

/// The folder desktop shortcuts go in.
pub fn desktop_dir() -> Option<PathBuf> {
    dirs::desktop_dir().filter(|d| d.is_dir())
}

fn file_stem(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { ' ' } else { c }).collect();
    let clean: String = clean.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(NAME_MAX).collect();
    let clean = clean.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    if clean.is_empty() { FALLBACK_NAME.to_owned() } else { clean }
}

#[cfg(windows)]
fn quoted(arg: &str) -> String {
    if arg.chars().any(|c| c.is_whitespace() || c == '"') { format!("\"{}\"", arg.replace('"', "\\\"")) } else { arg.to_owned() }
}

/// The path `stem.ext` in `dir`, or `stem (2).ext` and on while a file of the person's own is there; a shortcut of ours (`ours`) is replaced.
fn free_path(dir: &Path, stem: &str, ext: &str, ours: &dyn Fn(&Path) -> bool) -> Result<PathBuf> {
    for n in 1..=FREE_NAMES {
        let path = dir.join(if n == 1 { format!("{stem}.{ext}") } else { format!("{stem} ({n}).{ext}") });
        if std::fs::symlink_metadata(&path).is_err() || ours(&path) {
            return Ok(path);
        }
    }
    bail!("the desktop has too many files named {stem}")
}

/// Writes a shortcut named `name` into `dir` that starts `exe` with `play`'s arguments, replacing an older one of that name. Returns what it wrote.
pub fn create(play: &Play, name: &str, exe: &Path, dir: &Path) -> Result<PathBuf> {
    play.checked()?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let stem = file_stem(name);
    let args = play.args();
    make(&stem, name, exe, &args, dir)
}

#[cfg(windows)]
fn make(stem: &str, name: &str, exe: &Path, args: &[String], dir: &Path) -> Result<PathBuf> {
    let line = args.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(" ");
    let (target, dir, stem, description) = (exe.to_path_buf(), dir.to_path_buf(), stem.to_owned(), name.to_owned());
    let own = exe.file_name().map(|n| n.to_owned());
    std::thread::spawn(move || {
        let path = free_path(&dir, &stem, "lnk", &|p| windows_link::read(p).is_ok_and(|(target, args)| Path::new(&target).file_name() == own.as_deref() && args.split(' ').any(|a| a == PLAY_FLAG)))?;
        windows_link::write(&path, &target, &line, &description)?;
        Ok(path)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("the shortcut writer stopped"))?
}

#[cfg(windows)]
mod windows_link {
    use std::path::Path;

    use anyhow::{Result, anyhow};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    pub fn write(path: &Path, exe: &Path, args: &str, description: &str) -> Result<()> {
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let result = (|| -> windows::core::Result<()> {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&HSTRING::from(exe.as_os_str()))?;
                link.SetArguments(&HSTRING::from(args))?;
                link.SetDescription(&HSTRING::from(description))?;
                link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
                if let Some(parent) = exe.parent() {
                    link.SetWorkingDirectory(&HSTRING::from(parent.as_os_str()))?;
                }
                let file: IPersistFile = link.cast()?;
                file.Save(&HSTRING::from(path.as_os_str()), true)
            })();
            if initialized {
                CoUninitialize();
            }
            result.map_err(|e| anyhow!("cannot write {}: {e}", path.display()))
        }
    }

    pub fn read(path: &Path) -> Result<(String, String)> {
        use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
        use windows::Win32::System::Com::STGM_READ;
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let read = || -> Result<(String, String)> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| anyhow!("{e}"))?;
            let file: IPersistFile = link.cast().map_err(|e| anyhow!("{e}"))?;
            file.Load(&HSTRING::from(path.as_os_str()), STGM_READ).map_err(|e| anyhow!("{e}"))?;
            let mut target = [0u16; 1024];
            let mut find = WIN32_FIND_DATAW::default();
            link.GetPath(&mut target, &mut find, 0).map_err(|e| anyhow!("{e}"))?;
            let mut args = [0u16; 1024];
            link.GetArguments(&mut args).map_err(|e| anyhow!("{e}"))?;
            let text = |buf: &[u16]| String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(buf.len())]);
            Ok((text(&target), text(&args)))
            };
            let result = read();
            if initialized {
                CoUninitialize();
            }
            result
        }
    }
}

#[cfg(target_os = "macos")]
fn make(stem: &str, name: &str, exe: &Path, args: &[String], dir: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let app = free_path(dir, stem, "app", &|p| std::fs::read_to_string(p.join("Contents").join("Info.plist")).is_ok_and(|t| t.contains(&bundle_id())))?;
    let building = dir.join(format!(".{stem}.shortcut-building"));
    let _ = std::fs::remove_dir_all(&building);
    let (macos, contents) = (building.join("Contents").join("MacOS"), building.join("Contents"));
    std::fs::create_dir_all(&macos)?;
    std::fs::write(contents.join("Info.plist"), bundle_plist(name))?;
    let launcher = macos.join("launcher");
    std::fs::write(&launcher, launcher_script(exe, args))?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    if app.exists() {
        std::fs::remove_dir_all(&app)?;
    }
    std::fs::rename(&building, &app)?;
    Ok(app)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn make(stem: &str, name: &str, exe: &Path, args: &[String], dir: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = free_path(dir, stem, "desktop", &|p| std::fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| l == desktop_marker())))?;
    std::fs::write(&path, desktop_entry(name, exe, args))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn desktop_entry(name: &str, exe: &Path, args: &[String]) -> String {
    let field = |text: &str| text.chars().filter(|c| !c.is_control()).flat_map(|c| if c == '\\' { vec!['\\', '\\'] } else { vec![c] }).collect::<String>();
    let escape = |text: &str| {
        let inner: String = text
            .chars()
            .flat_map(|c| match c {
                '"' | '`' | '$' => vec!['\\', '\\', c],
                '\\' => vec!['\\'; 4],
                '%' => vec!['%', '%'],
                c => vec![c],
            })
            .collect();
        format!("\"{inner}\"")
    };
    let exec = std::iter::once(escape(&exe.to_string_lossy())).chain(args.iter().map(|a| escape(a))).collect::<Vec<_>>().join(" ");
    format!("[Desktop Entry]\nType=Application\nName={}\nComment={}\nExec={exec}\nTerminal=false\nCategories=Game;\n{}\n", field(name), field(name), desktop_marker())
}

#[cfg(any(test, target_os = "macos"))]
fn launcher_script(exe: &Path, args: &[String]) -> String {
    let single = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let line = std::iter::once(single(&exe.to_string_lossy())).chain(args.iter().map(|a| single(a))).collect::<Vec<_>>().join(" ");
    format!("#!/bin/sh\nexec {line}\n")
}

#[cfg(any(test, target_os = "macos"))]
fn bundle_plist(name: &str) -> String {
    let xml: String = name.chars().filter(|c| !c.is_control()).flat_map(|c| match c {
        '&' => "&amp;".chars().collect::<Vec<_>>(),
        '<' => "&lt;".chars().collect(),
        '>' => "&gt;".chars().collect(),
        c => vec![c],
    }).collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n<key>CFBundleExecutable</key><string>launcher</string>\n<key>CFBundleIdentifier</key><string>{}</string>\n<key>CFBundleName</key><string>{xml}</string>\n<key>CFBundlePackageType</key><string>APPL</string>\n<key>LSUIElement</key><true/>\n</dict>\n</plist>\n", bundle_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(account: Option<&str>) -> Play {
        Play { instance: "0a1b2c3d4e5f60718293a4b5c6d7e8f9".into(), account: account.map(str::to_owned) }
    }

    #[test]
    fn the_arguments_of_a_shortcut_parse_back_to_the_same_play() {
        for made in [play(None), play(Some("5f1d2c3b-aaaa-4bbb-8ccc-0123456789ab"))] {
            assert_eq!(Play::parse(&made.args()), Some(made));
        }
    }

    #[test]
    fn a_command_line_without_a_valid_play_asks_for_nothing() {
        let line = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert_eq!(Play::parse(&line(&[])), None);
        assert_eq!(Play::parse(&line(&["--play"])), None);
        assert_eq!(Play::parse(&line(&["--play", "--account", "x"])), None);
        assert_eq!(Play::parse(&line(&["--play", "a b"])), None);
        assert_eq!(Play::parse(&line(&["--play", "abc", "--account", "../x"])), None);
        assert_eq!(Play::parse(&line(&["--account", "abc"])), None);
        assert_eq!(Play::parse(&line(&["--other", "--play", "abc"])), Some(Play { instance: "abc".into(), account: None }));
    }

    #[test]
    fn file_names_lose_what_a_file_system_refuses() {
        assert_eq!(file_stem("Farm: A/B?"), "Farm A B");
        assert_eq!(file_stem("  ... "), FALLBACK_NAME);
        assert_eq!(file_stem(&"x".repeat(200)).chars().count(), NAME_MAX);
    }

    #[test]
    fn a_desktop_entry_quotes_the_program_and_every_argument() {
        let entry = desktop_entry("My Farm", Path::new("/opt/many craft/manycraft"), &play(Some("acc-1")).args());
        assert!(entry.contains("Name=My Farm\n"));
        assert!(entry.contains("Exec=\"/opt/many craft/manycraft\" \"--play\" \"0a1b2c3d4e5f60718293a4b5c6d7e8f9\" \"--account\" \"acc-1\"\n"));
        let tricky = desktop_entry("x", Path::new("/a\"b$c"), &[]);
        assert!(tricky.contains("Exec=\"/a\\\\\"b\\\\$c\""));
        assert!(desktop_entry("x", Path::new("/x"), &[]).lines().any(|l| l == desktop_marker()));
        assert!(desktop_entry("A\\B", Path::new("/x"), &[]).contains("Name=A\\\\B\n"));
    }

    #[test]
    fn a_launcher_script_single_quotes_what_it_runs() {
        let script = launcher_script(Path::new("/Apps/It's/manycraft"), &play(None).args());
        assert_eq!(script, "#!/bin/sh\nexec '/Apps/It'\\''s/manycraft' '--play' '0a1b2c3d4e5f60718293a4b5c6d7e8f9'\n");
        assert!(bundle_plist("A & <B>").contains("<string>A &amp; &lt;B&gt;</string>"));
    }

    #[test]
    fn a_file_of_the_person_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let ext = if cfg!(windows) { "lnk" } else if cfg!(target_os = "macos") { "app" } else { "desktop" };
        let theirs = dir.path().join(format!("Farm.{ext}"));
        std::fs::create_dir_all(&theirs).ok();
        if !theirs.is_dir() || ext != "app" {
            let _ = std::fs::remove_dir_all(&theirs);
            std::fs::write(&theirs, "mine").unwrap();
        }
        let exe = std::env::current_exe().unwrap();
        let first = create(&play(None), "Farm", &exe, dir.path()).unwrap();
        assert_eq!(first.file_name().unwrap().to_string_lossy(), format!("Farm (2).{ext}"));
        assert_eq!(create(&play(None), "Farm", &exe, dir.path()).unwrap(), first);
        assert!(theirs.exists());
        if ext != "app" {
            assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "mine");
        }
    }

    #[test]
    fn an_invalid_id_makes_no_shortcut() {
        let dir = tempfile::tempdir().unwrap();
        let bad = Play { instance: "../x".into(), account: None };
        assert!(create(&bad, "x", Path::new("manycraft"), dir.path()).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_shortcut_starts_the_program_with_the_play_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let made = play(Some("5f1d2c3b-aaaa-4bbb-8ccc-0123456789ab"));
        let path = create(&made, "Test: Farm", &exe, dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), "Test Farm.lnk");
        let (target, args) = windows_link::read(&path).unwrap();
        assert!(Path::new(&target).ends_with(exe.file_name().unwrap()));
        let parts: Vec<String> = args.split(' ').map(str::to_owned).collect();
        assert_eq!(Play::parse(&parts), Some(made));
        let again = create(&play(None), "Test: Farm", &exe, dir.path()).unwrap();
        assert_eq!(again, path);
        assert_eq!(Play::parse(&windows_link::read(&again).unwrap().1.split(' ').map(str::to_owned).collect::<Vec<_>>()), Some(play(None)));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_desktop_shortcut_is_an_executable_entry() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = create(&play(None), "Farm", Path::new("/usr/bin/manycraft"), dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), "Farm.desktop");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o111, 0o111);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_mac_shortcut_is_an_app_that_runs_the_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let path = create(&play(None), "Farm", Path::new("/Applications/manycraft"), dir.path()).unwrap();
        assert!(path.join("Contents/MacOS/launcher").is_file());
        assert!(path.join("Contents/Info.plist").is_file());
    }
}
