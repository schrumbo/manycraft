/// Runs a developer subcommand when the first argument names one, returning the exit code.
pub fn run(args: &[String]) -> Option<i32> {
    let (cmd, rest) = args.split_first()?;
    let run: fn(&[String]) -> i32 = match cmd.as_str() {
        "login" => super::auth::cli,
        "launch" => super::launch::cli,
        "batch" => super::fleet::cli,
        "mods" => super::modrinth::cli,
        "prism" => super::prism::cli,
        _ => return None,
    };
    attach_console();
    Some(run(rest))
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
