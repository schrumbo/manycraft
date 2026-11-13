#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod core;
mod ui;

use gpui_kit::App;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some(code) = core::cli::run(&args) {
        std::process::exit(code);
    }
    gpui_kit::application().run(|cx: &mut App| {
        gpui_kit::init(cx);
        ui::main_window::open(cx);
    });
}
