//! What the views show: the application model plus the images only the GPU draws, held in one entity.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use gpui_kit::{App, AppContext, Entity, EventEmitter, Global, RenderImage};
use uuid::Uuid;

use crate::app;
use crate::core::fit::Fit;
use crate::core::i18n::{tr, trf};

pub use crate::app::model::{ChatLine, Game, Import, Need, Outage, Phase, Run, UpdateStatus, Updates, banned_status, link_status, proxy_status, restart_status};

use super::forms;

/// Sent instead of a notify when only chat lines arrived, which nothing but the chat page shows.
pub struct ChatArrived;

impl EventEmitter<ChatArrived> for Model {}

/// The application model with the images of its games and accounts. Only `ui::state` changes it.
#[derive(Default)]
pub struct Model {
    pub app: app::model::Model,
    /// The frame each tile shows, by game key: the running game's newest, or the still of an earlier run.
    pub previews: HashMap<String, Arc<RenderImage>>,
    /// Skin faces by account id, for tiles of instances without a still.
    pub faces: HashMap<Uuid, Arc<RenderImage>>,
}

impl Deref for Model {
    type Target = app::model::Model;

    fn deref(&self) -> &Self::Target {
        &self.app
    }
}

impl DerefMut for Model {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.app
    }
}

impl Model {
    /// The frame tile `key` shows, if any.
    pub fn preview(&self, key: &str) -> Option<&Arc<RenderImage>> {
        self.previews.get(key)
    }

    /// Whether the frame of tile `key` came from the running game.
    pub fn live(&self, key: &str) -> bool {
        self.app.frames.get(key).is_some_and(|f| f.live)
    }
}

/// What a bulk start under way says: how far it is and how many wait for free memory.
pub fn run_line(run: &Run) -> String {
    let line = trf("Startet %lld/%lld", &[&run.launched, &run.total]);
    if run.memory == 0 {
        return line;
    }
    let waiting = if run.memory == 1 { tr("1 wartet auf Arbeitsspeicher") } else { trf("%lld warten auf Arbeitsspeicher", &[&run.memory]) };
    format!("{line} · {waiting}")
}

/// What `count` instances of `each_mb` need against the memory that is free, as one German line.
pub fn fit_line(count: u32, each_mb: u32, fit: &Fit) -> String {
    let label = |mb: u64| forms::memory_label(u32::try_from(mb).unwrap_or(u32::MAX));
    let free = forms::free_memory_label(fit.free_mb);
    if count == 1 {
        return trf("Ein Account braucht %@, dein PC hat %@ frei", &[&label(fit.needed_mb), &free]);
    }
    trf("%lld Accounts × %@ = %@, dein PC hat %@ frei", &[&count, &label(u64::from(each_mb)), &label(fit.needed_mb), &free])
}

struct Shared(Entity<Model>);

impl Global for Shared {}

/// Creates the model entity once at startup.
pub fn init(cx: &mut App) {
    let model = cx.new(|_| Model::default());
    cx.set_global(Shared(model));
}

/// The model entity; views observe it and re-render on change.
pub fn get(cx: &App) -> Entity<Model> {
    cx.global::<Shared>().0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fit_line_names_what_is_needed_and_what_is_free() {
        crate::core::i18n::init("de");
        let fit = |needed_mb, free_mb| Fit { needed_mb, free_mb, suggested_count: 0 };
        assert_eq!(fit_line(50, 2048, &fit(102_400, 32_768)), "50 Accounts × 2 GB = 100 GB, dein PC hat 32 GB frei");
        assert_eq!(fit_line(3, 2560, &fit(7680, 4096)), "3 Accounts × 2,5 GB = 7,5 GB, dein PC hat 4 GB frei");
        assert_eq!(fit_line(1, 8192, &fit(8192, 4096)), "Ein Account braucht 8 GB, dein PC hat 4 GB frei");
        assert_eq!(fit_line(1, 2048, &fit(2048, 2040)), "Ein Account braucht 2 GB, dein PC hat 1,9 GB frei");
        assert_eq!(fit_line(2, 2048, &fit(4096, 2047)), "2 Accounts × 2 GB = 4 GB, dein PC hat 1,9 GB frei");
    }
}
