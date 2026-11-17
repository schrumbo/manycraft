#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod core;
mod runner;
#[cfg_attr(feature = "freya", allow(dead_code))]
mod ui;
#[cfg(feature = "freya")]
mod view;

use std::borrow::Cow;

use gpui_kit::{App, AssetSource, SharedString};
use rust_embed::RustEmbed;

#[cfg_attr(feature = "freya", allow(dead_code))]
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
    if args.first().is_none_or(|a| a != "apply-update")
        && let Err(e) = core::backup::before_first_start(env!("CARGO_PKG_VERSION"))
    {
        if !core::updater::install::boot(env!("CARGO_PKG_VERSION")) {
            refuse_start(&e, !args.first().is_some_and(|a| core::cli::is_command(a)));
        }
        std::process::exit(1);
    }
    if let Some(code) = core::cli::run(&args) {
        std::process::exit(code);
    }
    log_panics();
    if core::updater::install::boot(env!("CARGO_PKG_VERSION")) {
        return;
    }
    match core::adopt::claim() {
        Ok(core::adopt::Claim::Manager) => {}
        Ok(core::adopt::Claim::Taken) => {
            let handed = match core::shortcut::Play::parse(&args) {
                Some(play) => core::adopt::play_first(&play),
                None => core::adopt::reopen_first(),
            };
            if !handed {
                eprintln!("another Manycraft runs on this data folder and does not answer");
            }
            return;
        }
        Err(e) => eprintln!("manager lock: {e:#}"),
    }
    let lang = std::env::var("MANYCRAFT_LANG").ok().filter(|l| !l.is_empty()).or_else(|| core::settings::loaded().language.clone());
    core::i18n::init(lang.as_deref().unwrap_or_default());
    #[cfg(feature = "freya")]
    view::run(core::shortcut::Play::parse(&args));
    #[cfg(not(feature = "freya"))]
    gpui(&args);
}

#[cfg(not(feature = "freya"))]
fn gpui(args: &[String]) {
    ui::theme::start_with(ui::theme::Choice::parse(&std::env::var("MANYCRAFT_THEME").unwrap_or_default()));
    let play = core::shortcut::Play::parse(args);
    let app = gpui_kit::application().with_assets(Assets);
    app.on_reopen(ui::state::show_grid);
    app.run(|cx: &mut App| {
        #[cfg(windows)]
        cx.set_app_identity(core::APP_ID, "Manycraft");
        gpui_kit::init(cx);
        ui::theme::apply(ui::theme::Theme::plain(false), None, cx);
        ui::theme::watch_focus(cx);
        ui::model::init(cx);
        ui::state::init(cx);
        ui::accounts::init(cx);
        ui::undo::init(cx);
        ui::skin_sheet::init(cx);
        ui::batch_sheet::init(cx);
        ui::instance_sheet::init(cx);
        ui::import_sheet::init(cx);
        ui::instance_page::init(cx);
        ui::mod_list::init(cx);
        ui::instance_files::init(cx);
        ui::maintenance::init(cx);
        ui::trash_sheet::init(cx);
        ui::transfer::init(cx);
        ui::settings_window::init(cx);
        ui::route::init(cx);
        ui::main_window::open(cx);
        ui::drive::init(cx);
        ui::updates::init(cx);
        ui::tray::init(cx);
        if let Some(play) = play {
            ui::state::request_play(play.instance, play.account, cx);
        }
    });
}

fn refuse_start(error: &anyhow::Error, window: bool) {
    core::cli::attach_console();
    eprintln!("backup before the first start: {error:#}");
    #[cfg(feature = "freya")]
    if window {
        core::i18n::init(&std::env::var("MANYCRAFT_LANG").unwrap_or_default());
        let text = core::i18n::trf("Vor dem ersten Start dieser Version sichert Manycraft deine Accounts, Instanzen, Batches und Einstellungen. Das hat nicht geklappt, deshalb wurde nichts verändert. Prüfe, ob genug Speicherplatz frei ist, und starte Manycraft dann erneut.\n\nDetails: %@", &[&format!("{error:#}")]);
        rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title(core::i18n::tr("Manycraft kann nicht starten")).set_description(text).set_buttons(rfd::MessageButtons::Ok).show();
    }
    #[cfg(not(feature = "freya"))]
    let _ = window;
}

fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = core::paths::data_dir().join("logs");
        let text = format!("{info}
{}
", std::backtrace::Backtrace::force_capture());
        if std::fs::create_dir_all(&dir).is_ok() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("manycraft-panic.log")) {
                let _ = f.write_all(text.as_bytes());
            }
        }
        previous(info);
    }));
}
