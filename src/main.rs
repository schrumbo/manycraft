#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod core;
mod ui;

use std::borrow::Cow;

use gpui_kit::{App, AssetSource, SharedString};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets"]
#[exclude = "i18n/*"]
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some(f) = Self::get(path) {
            return Ok(Some(f.data));
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = Self::iter().filter(|p| p.starts_with(path)).map(SharedString::from).collect();
        out.extend(gpui_kit::assets::AllAssets.list(path)?);
        Ok(out)
    }
}

fn main() {
    core::drive::check();
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some(code) = core::cli::run(&args) {
        std::process::exit(code);
    }
    core::i18n::init(&std::env::var("MANYCRAFT_LANG").unwrap_or_default());
    ui::theme::start_with(ui::theme::Choice::parse(&std::env::var("MANYCRAFT_THEME").unwrap_or_default()));
    gpui_kit::application().with_assets(Assets).run(|cx: &mut App| {
        gpui_kit::init(cx);
        ui::theme::apply(ui::theme::Theme::plain(false), None, cx);
        ui::main_window::open(cx);
        ui::drive::init(cx);
    });
}
