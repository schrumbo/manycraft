use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::view::drive::use_spot;
use crate::view::icons::{Icon, icon};
use crate::view::kit::badge::{Tone, badge};
use crate::view::kit::button::Button;
use crate::view::kit::switch::Switch;
use crate::view::kit::text::line;
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::use_theme;
use crate::view::theme::{Theme, control, font, iconsize, space};

/// The side of a mod's icon.
pub(in crate::view::instance) const FACE: f32 = control::L + space::S;
/// The height of one row of the list.
pub(in crate::view::instance) const ROW: f32 = FACE + space::M;
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

/// A button that shows when the pointer rests on a row: an icon with what it does.
#[derive(Clone, PartialEq)]
pub(in crate::view::instance) struct RowAction {
    pub id: &'static str,
    pub icon: Icon,
    pub label: String,
    pub danger: bool,
    pub disabled: Option<String>,
    pub run: EventHandler<()>,
}

/// A text button at the end of a row that stays visible.
#[derive(Clone, PartialEq)]
pub(in crate::view::instance) struct RowText {
    pub id: String,
    pub label: String,
    pub disabled: Option<String>,
    pub run: EventHandler<()>,
}

/// One mod or pack of a list: icon, name and caption, a switch, an update mark and what can be done with it.
#[derive(PartialEq)]
pub(in crate::view::instance) struct EntryRow {
    pub area: &'static str,
    pub key: String,
    pub tip: String,
    pub name: String,
    pub caption: String,
    pub warn: bool,
    pub enabled: bool,
    pub icon: Option<PathBuf>,
    pub glyph: Icon,
    pub selected: bool,
    pub cursor: bool,
    pub locked: Option<String>,
    pub update: Option<String>,
    pub actions: Vec<RowAction>,
    pub trail: Option<RowText>,
    pub on_toggle: EventHandler<bool>,
    pub on_press: Option<EventHandler<()>>,
    pub on_menu: Option<EventHandler<(f32, f32)>>,
}

impl Component for EntryRow {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut hovered = use_state(|| false);
        let (area, key) = (self.area, &self.key);
        let spot = use_spot(&format!("instance.{area}.row.{key}"), "row", &self.name, true);
        let locked = self.locked.clone();
        let toggle = self.on_toggle.clone();
        let switch = WithTip::new(locked.clone(), Switch::new(format!("instance.{area}.on.{key}"), self.name.clone(), self.enabled).disabled(locked.is_some()).on_toggle(move |on| toggle.call(on)));

        let dim = if self.enabled { 1. } else { t.hidden_dim };
        let title = rect().width(Size::fill()).opacity(dim).child(line(&t, self.name.clone(), font::BODY, t.text1).font_weight(FontWeight::MEDIUM).width(Size::fill()));
        let lines = rect()
            .width(Size::fill())
            .spacing(1.)
            .child(title)
            .child(line(&t, self.caption.clone(), font::CAPTION, if self.warn { t.attention_text } else { t.text3 }).width(Size::fill()));
        let text = rect().width(Size::flex(1.)).child(WithTip::new(Some(self.tip.clone()), lines).fill());

        let mut main = rect()
            .width(Size::flex(1.))
            .height(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .on_sized(spot)
            .child(rect().opacity(dim).child(Face { icon: self.icon.clone(), glyph: self.glyph }))
            .child(text)
            .content(Content::flex());
        if let Some(press) = self.on_press.clone() {
            main = main.cursor(CursorIcon::Pointer).on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                press.call(());
            });
        }

        let fill = if self.selected {
            t.selection
        } else if *hovered.read() {
            t.hover
        } else {
            Color::TRANSPARENT
        };
        let mut row = rect()
            .width(Size::fill())
            .height(Size::px(ROW))
            .padding((0., space::M))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::M)
            .corner_radius(t.shape.control)
            .background(fill)
            .on_pointer_over(move |_| hovered.set(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .child(switch)
            .child(main);
        if let Some(menu) = self.on_menu.clone() {
            row = row.on_secondary_down(move |e: Event<PressEventData>| {
                e.stop_propagation();
                if let PressEventData::Mouse(m) = &*e {
                    menu.call((m.global_location.x as f32, m.global_location.y as f32));
                }
            });
        }
        if self.cursor && !self.selected {
            row = row.border(Border::new().fill(t.focus).width(1.).alignment(BorderAlignment::Inner));
        }
        if let Some(label) = &self.update {
            let tip = if label.is_empty() { tr("Neue Version verfügbar") } else { trf("Neu: %@", &[label]) };
            row = row.child(WithTip::new(Some(tip), badge(&t, tr("Update"), Tone::Accent)));
        }
        let shown = *hovered.read() || self.selected || self.cursor;
        for action in &self.actions {
            let slot = rect().width(Size::px(control::M)).height(Size::px(control::M));
            let run = action.run.clone();
            row = row.child(if shown {
                let button = Button::icon(format!("instance.{area}.{}.{key}", action.id), action.icon, action.label.clone()).plain().disabled(action.disabled.clone());
                slot.child(if action.danger { button.danger() } else { button }.on_press(move |_| run.call(())))
            } else {
                slot
            });
        }
        if let Some(trail) = &self.trail {
            let run = trail.run.clone();
            row = row.child(Button::new(trail.id.clone(), trail.label.clone()).plain().disabled(trail.disabled.clone()).on_press(move |_| run.call(())));
        }
        row.content(Content::flex())
    }
}

/// A bar across the page with a glyph and one line of text; buttons are added after it.
pub(in crate::view::instance) fn bar(t: &Theme, glyph: Icon, color: Color, text: String) -> Rect {
    rect()
        .width(Size::fill())
        .height(Size::px(control::L))
        .padding((0., space::S, 0., space::M))
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(space::S)
        .corner_radius(t.shape.control)
        .background(t.raised)
        .child(icon(t, glyph, iconsize::S, color))
        .child(line(t, text, font::CALLOUT, t.text1).max_lines(1).text_overflow(TextOverflow::Ellipsis).width(Size::flex(1.)))
        .content(Content::flex())
}

