//! Feasibility prototype of the manager on Freya: a grid of tiles, a real game window swapped over a tile, live
//! previews from the shared preview files, and frame counters for idle measurements. Windows only.

#[cfg(windows)]
#[path = "../../core/preview.rs"]
#[allow(dead_code)]
mod preview;
#[cfg(windows)]
mod app;
#[cfg(windows)]
mod stand_in;
#[cfg(windows)]
mod swap;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("stand-in") {
        stand_in::run(&args[1..]);
        return;
    }
    app::run(app::Options::parse(&args));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the Freya prototype runs on Windows only");
}
