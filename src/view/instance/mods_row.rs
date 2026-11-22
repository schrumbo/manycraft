use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use super::Ctx;
use crate::core::i18n::{tr, trf};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::{Tone, badge};
use crate::view::kit::button::Button;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::{line, strong};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::use_theme;
use crate::view::theme::{control, font, iconsize, space};

/// The side of a mod's icon.
pub(in crate::view::instance) const FACE: f32 = control::L + space::S;
/// The height of one row of the list.
pub(super) const ROW: f32 = FACE + space::M;
const THUMB: u32 = 64;
const MAX_ICON_BYTES: u64 = 4 * 1024 * 1024;
const CACHED: usize = 600;

type Pixels = (u32, u32, Vec<u8>);
type Reply = async_channel::Sender<Option<Pixels>>;

thread_local! {
    static FACES: RefCell<HashMap<PathBuf, Option<ImageHandle>>> = RefCell::new(HashMap::new());
}

pub(in crate::view::instance) fn cached(path: &Path) -> Option<Option<ImageHandle>> {
    FACES.with(|f| f.borrow().get(path).cloned())
}

pub(in crate::view::instance) fn remember(path: PathBuf, face: Option<ImageHandle>) {
    FACES.with(|f| {
        let mut f = f.borrow_mut();
        if f.len() >= CACHED {
            f.clear();
        }
        f.insert(path, face);
    });
}

fn decode(path: &Path) -> Option<Pixels> {
    if std::fs::metadata(path).ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let thumb = img.thumbnail(THUMB, THUMB).to_rgba8();
    Some((thumb.width(), thumb.height(), thumb.into_raw()))
}

fn worker() -> &'static async_channel::Sender<(PathBuf, Reply)> {
    static WORKER: OnceLock<async_channel::Sender<(PathBuf, Reply)>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = async_channel::unbounded::<(PathBuf, Reply)>();
        let _ = std::thread::Builder::new().name("mod-icons".into()).spawn(move || {
            while let Ok((path, reply)) = rx.recv_blocking() {
                if reply.is_closed() {
                    continue;
                }
                let _ = reply.send_blocking(decode(&path));
            }
        });
        tx
    })
}

pub(in crate::view::instance) async fn load(path: PathBuf) -> Option<ImageHandle> {
    let (tx, rx) = async_channel::bounded(1);
    worker().send((path, tx)).await.ok()?;
    let (w, h, rgba) = rx.recv().await.ok()??;
    ImageHandle::from_rgba(w, h, Bytes::from(rgba), AlphaType::Unpremul)
}

/// The icon of a mod, decoded off the UI thread and kept; a glyph while it loads or when there is none.
#[derive(PartialEq)]
pub(in crate::view::instance) struct Face {
    pub icon: Option<PathBuf>,
    pub glyph: Icon,
}

impl Component for Face {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut shown = use_state(|| self.icon.as_deref().and_then(cached).flatten());
        use_side_effect_with_deps(&self.icon, move |path| {
            let Some(path) = path.clone() else {
                shown.set(None);
                return;
            };
            match cached(&path) {
                Some(face) => shown.set(face),
                None => {
                    shown.set(None);
                    spawn(async move {
                        let face = load(path.clone()).await;
                        remember(path, face.clone());
                        shown.set(face);
                    });
                }
            }
        });
        let slot = rect().width(Size::px(FACE)).height(Size::px(FACE)).corner_radius(t.shape.tile).overflow(Overflow::Clip).center();
        match shown.read().clone() {
            Some(face) => slot.child(image(face).width(Size::px(FACE)).height(Size::px(FACE)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center)),
            None => slot.background(t.raised).child(icon(&t, self.glyph, iconsize::M, t.text3)),
        }
    }
}

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
    pub project: bool,
    pub update: Option<String>,
}

impl Component for ModRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let lower = self.file.to_lowercase();
        let spot = use_spot(&format!("instance.mods.row.{lower}"), "row", &self.name, true);
        let ctx = self.ctx.clone();
        let file = self.file.clone();
        let locked = self.locked.clone();

        let switch = {
            let (ctx, file) = (ctx.clone(), file.clone());
            WithTip::new(locked.clone(), Switch::new(format!("instance.mods.on.{lower}"), self.name.clone(), self.enabled).disabled(locked.is_some()).on_toggle(move |on| ctx.switch(vec![file.clone()], on)))
        };

        let dim = if self.enabled { 1. } else { t.hidden_dim };
        let title = rect().width(Size::fill()).opacity(dim).child(line(&t, self.name.clone(), font::BODY, t.text1).font_weight(FontWeight::MEDIUM).width(Size::fill()));
        let lines = rect()
            .width(Size::fill())
            .spacing(1.)
            .child(title)
            .child(line(&t, self.caption.clone(), font::CAPTION, if self.warn { t.attention_text } else { t.text3 }).width(Size::fill()));
        let text = rect().width(Size::flex(1.)).child(WithTip::new(Some(self.file.clone()), lines).fill());

        let (press_ctx, press_file) = (ctx.clone(), file.clone());
        let main = rect()
            .width(Size::flex(1.))
            .height(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .cursor(CursorIcon::Pointer)
            .on_sized(spot)
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                press_ctx.click(&press_file);
            })
            .child(rect().opacity(dim).child(Face { icon: self.icon.clone(), glyph: Icon::Layers }))
            .child(text)
            .content(Content::flex());

        let fill = if self.selected {
            t.selection
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let (menu_ctx, menu_file) = (ctx.clone(), file.clone());
        let mut row = rect()
            .width(Size::fill())
            .height(Size::px(ROW))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .corner_radius(t.shape.control)
            .background(fill)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set_if_modified(false))
            .on_secondary_down(move |e: Event<PressEventData>| {
                e.stop_propagation();
                if let PressEventData::Mouse(m) = &*e {
                    menu_ctx.open_row_menu(&menu_file, (m.global_location.x as f32, m.global_location.y as f32));
                }
            })
            .child(switch)
            .child(main);
        if self.cursor && !self.selected {
            row = row.border(Border::new().fill(t.focus).width(1.).alignment(BorderAlignment::Inner));
        }
        if let Some(label) = &self.update {
            let tip = if label.is_empty() { tr("Neue Version verfügbar") } else { trf("Neu: %@", &[label]) };
            row = row.child(WithTip::new(Some(tip), badge(&t, tr("Update"), Tone::Accent)));
        }
        if self.project {
            let shown = *hovered.read() || self.selected || self.cursor;
            let slot = rect().width(Size::px(control::M)).height(Size::px(control::M));
            let (ctx, file) = (ctx.clone(), file.clone());
            row = row.child(if shown {
                slot.child(Button::icon(format!("instance.mods.info.{lower}"), Icon::Info, tr("Readme und Details")).plain().on_press(move |_| ctx.show_project(&file)))
            } else {
                slot
            });
        }
        match &self.trail {
            Trail::None => {}
            Trail::Fix { newest } => {
                let newest = *newest;
                let (ctx, file) = (ctx.clone(), file.clone());
                let label = if newest { tr("Ältere Version löschen") } else { tr("Löschen") };
                row = row.child(Button::new(format!("instance.mods.fix.{lower}"), label).plain().disabled(locked.clone()).on_press(move |_| {
                    if newest {
                        ctx.fix(&file);
                    } else {
                        ctx.trash(vec![file.clone()], None);
                    }
                }));
            }
            Trail::Needs(missing) => {
                let (ctx, missing) = (ctx.clone(), missing.clone());
                row = row.child(Button::new(format!("instance.mods.needs.{lower}"), tr("Einschalten")).plain().disabled(locked.clone()).on_press(move |_| ctx.switch(missing.clone(), true)));
            }
        }
        row.content(Content::flex())
    }
}