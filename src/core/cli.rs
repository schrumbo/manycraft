/// Runs a developer subcommand when the first argument names one, returning the exit code.
pub fn run(args: &[String]) -> Option<i32> {
    let (cmd, rest) = args.split_first()?;
    #[cfg(any(windows, target_os = "macos"))]
    if cmd == super::webauth::SUBCOMMAND {
        return Some(super::signin_window::cli(rest));
    }
    let run = COMMANDS.iter().find(|(name, _)| name == cmd)?.1;
    attach_console();
    if !matches!(cmd.as_str(), "apply-update" | "folders" | "help" | "--help" | "-h") {
        for problem in super::migrate::run().problems {
            eprintln!("{problem}");
        }
        super::settings::loaded();
    }
    Some(run(rest))
}

const COMMANDS: [(&str, fn(&[String]) -> i32); 15] = [
    ("login", super::auth::cli),
    ("launch", super::launch::cli),
    ("batch", super::fleet::cli),
    ("instance", super::fleet::instance_cli),
    ("mods", super::modrinth::cli),
    ("prism", super::prism::cli),
    ("launchers", super::launchers::cli),
    ("mrpack", super::mrpack::cli),
    ("clean", super::install::clean::cli),
    ("smoke", super::smoke::cli),
    ("folders", super::folders::cli),
    ("apply-update", super::updater::install::cli),
    ("help", help),
    ("--help", help),
    ("-h", help),
];

/// Whether `first` names a subcommand that runs without a window.
pub fn is_command(first: &str) -> bool {
    #[cfg(any(windows, target_os = "macos"))]
    if first == super::webauth::SUBCOMMAND {
        return true;
    }
    COMMANDS.iter().any(|(name, _)| *name == first)
}

fn help(_: &[String]) -> i32 {
    println!("manycraft [--play <instance> [--account <id>]] [file.mrpack]

commands: login, launch, batch, instance, mods, prism, launchers, mrpack, clean, smoke, folders, help");
    0
}

/// Lets output reach the console this process was started from.
#[cfg(windows)]
pub fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Lets output reach the console this process was started from.
#[cfg(not(windows))]
pub fn attach_console() {}
