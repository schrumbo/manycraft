/// Runs a developer subcommand when the first argument names one, returning the exit code.
pub fn run(args: &[String]) -> Option<i32> {
    let (cmd, rest) = args.split_first()?;
    #[cfg(any(windows, target_os = "macos"))]
    if cmd == super::webauth::SUBCOMMAND {
        return Some(super::signin_window::cli(rest));
    }
    let run: fn(&[String]) -> i32 = match cmd.as_str() {
        "login" => super::auth::cli,
        "launch" => super::launch::cli,
        "batch" => super::fleet::cli,
        "instance" => super::fleet::instance_cli,
        "mods" => super::modrinth::cli,
        "prism" => super::prism::cli,
        "launchers" => super::launchers::cli,
        "mrpack" => super::mrpack::cli,
        "clean" => super::install::clean::cli,
        "smoke" => super::smoke::cli,
        "apply-update" => super::updater::install::cli,
        "help" | "--help" | "-h" => help,
        _ => return None,
    };
    attach_console();
    if !matches!(cmd.as_str(), "apply-update" | "help" | "--help" | "-h") {
        for problem in super::migrate::run().problems {
            eprintln!("{problem}");
        }
        super::settings::loaded();
    }
    Some(run(rest))
}

fn help(_: &[String]) -> i32 {
    println!("manycraft [--play <instance> [--account <id>]] [file.mrpack]

commands: login, launch, batch, instance, mods, prism, launchers, mrpack, clean, smoke, help");
    0
}

#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
