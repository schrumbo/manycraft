//! Places a menu, a notification or a link opens: a tile, what needs attention, the chat, an instance or a part of the settings.

use gpui_kit::App;
use schemars::JsonSchema;
use serde::Deserialize;

use super::actions::OpenSettingsAt;
use super::model;

/// A part of the settings window a link can open.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema)]
pub enum Section {
    Phone,
    Reconnect,
}

/// A place inside Manycraft.
#[derive(Clone, PartialEq, Debug)]
pub enum Route {
    /// The tile of game `key` in `batch`, selected and scrolled into view; the batch's attention tiles when the game is gone.
    Tile { batch: String, key: String },
    /// The grid of a batch, filtered to what needs attention.
    Attention(String),
    /// The chat, showing only whispers.
    Whispers,
    /// The page of instance `id`.
    Instance(String),
    /// A part of the settings window.
    Settings(Section),
}

/// Registers the action that opens a part of the settings.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &OpenSettingsAt, cx| {
        let section = a.0;
        cx.defer(move |cx| go(Route::Settings(section), cx));
    });
}

/// Shows `route`, bringing the manager back from a played game first when the route lives in it.
pub fn go(route: Route, cx: &mut App) {
    match route {
        Route::Settings(section) => super::settings_window::open_at(section, cx),
        Route::Tile { batch, key } => {
            super::state::show_grid(cx);
            if model::get(cx).read(cx).games.contains_key(&key) {
                super::main_window::reveal_tile(&batch, key, cx);
            } else {
                super::main_window::select_batch(&batch, cx);
            }
        }
        Route::Attention(batch) => {
            super::state::show_grid(cx);
            super::main_window::select_batch(&batch, cx);
        }
        Route::Whispers => {
            super::state::show_grid(cx);
            super::chat::show_whispers(cx);
        }
        Route::Instance(id) => {
            super::state::show_grid(cx);
            super::main_window::show_instance(&id, cx);
        }
    }
}

/// Where a click on the notification `tag` about the games `keys` leads: the one tile, the batch's attention tiles, or the whispers.
pub fn of_notification(tag: &str, keys: &[String]) -> Option<Route> {
    if tag == crate::app::reduce::WHISPER_TAG {
        return Some(Route::Whispers);
    }
    let batch = tag.strip_prefix("attention-")?;
    Some(match keys {
        [key] => Route::Tile { batch: batch.to_owned(), key: key.clone() },
        _ => Route::Attention(batch.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notification_about_one_game_opens_its_tile_and_one_about_many_the_batch() {
        let one = vec!["a".to_owned()];
        let two = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(of_notification("attention-b1", &one), Some(Route::Tile { batch: "b1".into(), key: "a".into() }));
        assert_eq!(of_notification("attention-b1", &two), Some(Route::Attention("b1".into())));
        assert_eq!(of_notification("attention-b1", &[]), Some(Route::Attention("b1".into())));
        assert_eq!(of_notification(crate::app::reduce::WHISPER_TAG, &one), Some(Route::Whispers));
        assert_eq!(of_notification("update", &one), None);
    }
}
