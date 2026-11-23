use std::path::PathBuf;

use freya::prelude::*;

use super::Ctx;
use crate::core::i18n::tr;
use crate::view::icons::Icon;
use crate::view::instance::entry::{EntryRow, ROW, RowAction, RowText};
use crate::view::kit::text::strong;
use crate::view::kit::use_theme;
use crate::view::theme::{font, space};

/// The header above the mods that are not from Modrinth.
#[derive(PartialEq)]
pub(super) struct Section;

impl Component for Section {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        rect().width(Size::fill()).height(Size::px(ROW)).padding((0., space::M, space::XS, space::M)).main_align(Alignment::End).child(strong(&t, tr("Eigene Dateien"), font::CALLOUT, t.text2))
    }
}

/// What a row offers at its end.
#[derive(Clone, PartialEq)]
pub(super) enum Trail {
    None,
    Fix { newest: bool },
    Needs(Vec<String>),
}

/// One mod of the list.
#[derive(PartialEq)]
pub(super) struct ModRow {
    pub ctx: Ctx,
    pub file: String,
    pub name: String,
    pub caption: String,
    pub warn: bool,
    pub enabled: bool,
    pub icon: Option<PathBuf>,
    pub selected: bool,
    pub cursor: bool,
    pub locked: Option<String>,
    pub trail: Trail,
    pub update: Option<String>,
}

impl Component for ModRow {
    fn render(&self) -> impl IntoElement {
        let lower = self.file.to_lowercase();
        let (ctx, file, locked) = (self.ctx.clone(), self.file.clone(), self.locked.clone());
        let (toggle_ctx, toggle_file) = (ctx.clone(), file.clone());
        let (press_ctx, press_file) = (ctx.clone(), file.clone());
        let (menu_ctx, menu_file) = (ctx.clone(), file.clone());
        let mut actions = Vec::new();
        let (info_ctx, info_file) = (ctx.clone(), file.clone());
        actions.push(RowAction { id: "info", icon: Icon::Info, label: tr("Readme und Details"), danger: false, disabled: None, run: EventHandler::new(move |_| info_ctx.pane.show(&info_file)) });
        let trail = match &self.trail {
            Trail::None => None,
            Trail::Fix { newest } => {
                let (newest, ctx, file) = (*newest, ctx.clone(), file.clone());
                let label = if newest { tr("Ältere Version löschen") } else { tr("Löschen") };
                let run = EventHandler::new(move |_| {
                    if newest {
                        ctx.fix(&file);
                    } else {
                        ctx.trash(vec![file.clone()], None);
                    }
                });
                Some(RowText { id: format!("instance.mods.fix.{lower}"), label, disabled: locked.clone(), run })
            }
            Trail::Needs(missing) => {
                let (ctx, missing) = (ctx.clone(), missing.clone());
                Some(RowText { id: format!("instance.mods.needs.{lower}"), label: tr("Einschalten"), disabled: locked.clone(), run: EventHandler::new(move |_| ctx.switch(missing.clone(), true)) })
            }
        };
        EntryRow {
            area: "mods",
            key: lower,
            tip: self.file.clone(),
            name: self.name.clone(),
            caption: self.caption.clone(),
            warn: self.warn,
            enabled: self.enabled,
            icon: self.icon.clone(),
            glyph: Icon::Layers,
            selected: self.selected,
            cursor: self.cursor,
            locked,
            update: self.update.clone(),
            actions,
            trail,
            on_toggle: EventHandler::new(move |on| toggle_ctx.switch(vec![toggle_file.clone()], on)),
            on_press: Some(EventHandler::new(move |_| press_ctx.click(&press_file))),
            on_menu: Some(EventHandler::new(move |at| menu_ctx.open_row_menu(&menu_file, at))),
        }
    }
}