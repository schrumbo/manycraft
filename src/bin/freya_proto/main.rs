//! Feasibility prototype of the manager on Freya: a grid of tiles, a real game window swapped over a tile, live
//! previews from the shared preview files, and frame counters for idle measurements. With `--theme` or `--screen` it
//! draws the theme screens instead. Windows only.

#[cfg(windows)]
#[path = "../../core/preview.rs"]
#[allow(dead_code)]
mod preview;
#[cfg(windows)]
mod app;
#[cfg(windows)]
mod art;
#[cfg(windows)]
#[path = "../../core/channel.rs"]
#[allow(dead_code)]
mod channel;
#[cfg(windows)]
mod gallery;
#[cfg(windows)]
#[path = "../../core/i18n.rs"]
#[allow(dead_code)]
mod i18n;
#[cfg(windows)]
mod icons;
#[cfg(windows)]
#[path = "../../core/paths.rs"]
#[allow(dead_code)]
mod paths;
#[cfg(windows)]
mod stand_in;
#[cfg(windows)]
mod swap;
#[cfg(windows)]
mod theme;
#[cfg(windows)]
#[path = "../../core/themes.rs"]
#[allow(dead_code)]
mod themes;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("stand-in") {
        stand_in::run(&args[1..]);
        return;
    }
    if let Some(options) = gallery::Options::parse(&args) {
        gallery::run(options);
        return;
    }
    app::run(app::Options::parse(&args));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the Freya prototype runs on Windows only");
}
