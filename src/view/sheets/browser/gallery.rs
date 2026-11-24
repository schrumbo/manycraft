//! The gallery of a project: thumbnails in a grid, a click opens the picture large with fit, zoom and arrows.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Instant;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use crate::core::i18n::{tr, trf};
use crate::ui::catalog;
use crate::view::commands::Shortcut;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::{Button, Look};
use crate::view::kit::sheet::{self, Card, Primary};
use crate::view::kit::text::{caption, figure, line, secondary, strong};
use crate::view::kit::tooltip::WithTip;
use crate::view::kit::zoom::{BUTTON_ZOOM, FIT, Pose, fit_scale, held, put, wheel_factor};
use crate::view::kit::{off_ui, ring, use_theme};
use crate::view::store::use_store;
use crate::view::theme::{Theme, font, iconsize, space};

const THUMB_BOX: (u32, u32) = (1024, 576);
const LARGE_BOX: (u32, u32) = (2560, 4096);
const AT_ONCE: usize = 2;
const AT_ONCE_LARGE: usize = 1;
const TILE_MIN: f32 = 200.;
const TILE_MAX: f32 = 360.;
const GAP: f32 = space::M;
const ASPECT: f32 = 16. / 9.;
const FIRST_ROOM: f32 = 520.;
const STAGE_CHROME: f32 = 260.;
const STAGE_MIN: f32 = 280.;
const STAGE_MAX: f32 = 900.;
const CARD_MARGIN: f32 = 120.;
const CARD_MAX: f32 = 1400.;
const DOUBLE: std::time::Duration = std::time::Duration::from_millis(400);

/// One picture of the gallery: its small copy for the grid and its large one for the viewer.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Item {
    pub title: Option<String>,
    pub thumb: String,
    pub large: String,
}

/// The thumbnails of a gallery by url, kept while the project page is open so a regrid or a section switch costs no decoding.
pub(super) type Thumbs = State<HashMap<String, Pic>>;

#[derive(Clone, PartialEq)]
pub(super) enum Pic {
    Loading,
    Ready { handle: ImageHandle, size: (u32, u32) },
    Failed,
}

type Gate = (async_channel::Sender<()>, async_channel::Receiver<()>);

static THUMBS: LazyLock<Gate> = LazyLock::new(|| async_channel::bounded(AT_ONCE));
static LARGE: LazyLock<Gate> = LazyLock::new(|| async_channel::bounded(AT_ONCE_LARGE));

struct Slot(&'static Gate);

impl Slot {
    async fn take(large: bool) -> Self {
        let gate: &'static Gate = if large { &LARGE } else { &THUMBS };
        let _ = gate.0.send(()).await;
        Self(gate)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = self.0.1.try_recv();
    }
}

fn decode(path: &std::path::Path, bound: (u32, u32)) -> Option<(u32, u32, Vec<u8>)> {
    let img = bounded(image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?, bound).to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

fn bounded(img: image::DynamicImage, bound: (u32, u32)) -> image::DynamicImage {
    if img.width() > bound.0 || img.height() > bound.1 { img.thumbnail(bound.0, bound.1) } else { img }
}

/// The picture at `url`, or `None` when `wanted` says nobody waits for it any more by the time its turn comes.
/// A large picture that cannot be read falls back to `fallback`, the small copy.
async fn fetch(url: String, fallback: Option<String>, wanted: impl Fn() -> bool) -> Option<Pic> {
    let large = fallback.is_some();
    let slot = Slot::take(large).await;
    if !wanted() {
        return None;
    }
    let decoded = off_ui(move || {
        let _slot = slot;
        let first = if large { catalog::shot_large(&url) } else { catalog::shot(&url) };
        first.ok().and_then(|p| decode(&p, if large { LARGE_BOX } else { THUMB_BOX })).or_else(|| {
            let small = catalog::shot(fallback.as_deref()?).ok()?;
            decode(&small, THUMB_BOX)
        })
    })
    .await
    .flatten();
    Some(decoded.and_then(|(w, h, rgba)| ImageHandle::from_rgba(w, h, Bytes::from(rgba), AlphaType::Unpremul).map(|handle| Pic::Ready { handle, size: (w, h) })).unwrap_or(Pic::Failed))
}

fn columns(room: f32) -> usize {
    (((room + GAP) / (TILE_MIN + GAP)).floor() as usize).max(1)
}

fn tile_width(room: f32, columns: usize) -> f32 {
    ((room - GAP * (columns - 1) as f32) / columns as f32).min(TILE_MAX)
}

/// The pictures of `items` as a grid of thumbnails with their titles; `room` follows the grid's width.
pub(super) fn grid(t: &Theme, items: &[Item], room: State<f32>, thumbs: Thumbs, on_open: EventHandler<usize>) -> Element {
    let mut measured = room;
    let width = *room.read();
    let cols = columns(width);
    let w = tile_width(width, cols);
    let titled = items.iter().any(|i| i.title.as_ref().is_some_and(|s| !s.trim().is_empty()));
    let _ = t;
    let rows = items.chunks(cols).enumerate().map(|(r, chunk)| {
        rect()
            .key(r)
            .width(Size::fill())
            .horizontal()
            .spacing(GAP)
            .children(chunk.iter().enumerate().map(|(c, item)| {
                Tile { index: r * cols + c, url: item.thumb.clone(), thumbs, label: item.title.clone().filter(|s| !s.trim().is_empty()), titled, w, on_open: on_open.clone() }.into_element()
            }))
    });
    rect()
        .width(Size::fill())
        .spacing(space::L)
        .on_sized(move |e: Event<SizedEventData>| measured.set_if_modified(e.area.width()))
        .children(rows)
        .into_element()
}

#[derive(PartialEq)]
struct Tile {
    index: usize,
    url: String,
    thumbs: Thumbs,
    label: Option<String>,
    titled: bool,
    w: f32,
    on_open: EventHandler<usize>,
}

impl Component for Tile {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let focus = use_focus(a11y);
        let mut hovered = use_state(|| false);
        let thumbs = self.thumbs;
        use_side_effect_with_deps(&self.url, move |url| {
            let mut thumbs = thumbs;
            if thumbs.peek().contains_key(url) {
                return;
            }
            thumbs.write().insert(url.clone(), Pic::Loading);
            let url = url.clone();
            spawn_in_window(async move {
                if let Some(next) = fetch(url.clone(), None, move || thumbs.try_peek().is_some()).await
                    && let Some(mut map) = thumbs.try_write()
                {
                    map.insert(url, next);
                }
            });
        });
        let pic = thumbs.read().get(&self.url).cloned().unwrap_or(Pic::Loading);
        let h = (self.w / ASPECT).round();
        let frame = rect().width(Size::px(self.w)).height(Size::px(h)).corner_radius(t.shape.control).overflow(Overflow::Clip).center();
        let frame = match pic {
            Pic::Ready { handle, .. } => frame.background(t.well).child(image(handle).width(Size::px(self.w)).height(Size::px(h)).aspect_ratio(AspectRatio::Min).image_cover(ImageCover::Center)).into_element(),
            Pic::Failed => WithTip::new(Some(tr("Bild ließ sich nicht laden.")), frame.background(t.well).child(icon(&t, Icon::ImageOff, iconsize::L, t.text3))).fill().into_element(),
            Pic::Loading => frame.background(t.hover).into_element(),
        };
        let mut outline = rect().width(Size::px(self.w)).height(Size::px(h)).corner_radius(t.shape.control);
        if *hovered.read() {
            outline = outline.border(Border::new().fill(t.accent).width(2.).alignment(BorderAlignment::Inner));
        } else if let Some(border) = ring(&t, focus()) {
            outline = outline.border(border);
        }
        let index = self.index;
        let open = self.on_open.clone();
        let alt = self.label.clone().unwrap_or_else(|| trf("Bild %lld", &[&(index + 1)]));
        let mut column = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_role(AccessibilityRole::Button)
            .a11y_alt(alt)
            .width(Size::px(self.w))
            .spacing(space::XS)
            .cursor(CursorIcon::Pointer)
            .on_pointer_over(move |_| hovered.set_if_modified(true))
            .on_pointer_out(move |_| hovered.set_if_modified(false))
            .on_key_down({
                let open = open.clone();
                move |e: Event<KeyboardEventData>| {
                    if matches!(&e.key, Key::Named(NamedKey::Enter)) || matches!(&e.key, Key::Character(c) if c == " ") {
                        e.stop_propagation();
                        open.call(index);
                    }
                }
            })
            .on_all_press(move |e: Event<PressEventData>| {
                if let PressEventData::Mouse(m) = &*e
                    && m.button != Some(MouseButton::Left)
                {
                    return;
                }
                a11y.request_focus();
                open.call(index);
            })
            .child(rect().width(Size::px(self.w)).height(Size::px(h)).child(frame).child(rect().position(Position::new_absolute().left(0.).top(0.)).child(outline)));
        if let Some(label) = &self.label {
            column = column.child(line(&t, label.clone(), font::CALLOUT, t.text2).max_lines(1).text_overflow(TextOverflow::Ellipsis));
        } else if self.titled {
            column = column.child(rect().height(Size::px(font::CALLOUT * font::LINE)));
        }
        column
    }
}

/// Opens the picture `at` of `items` in the viewer, a sheet over the window.
pub(super) fn open(ui: crate::view::ui::Ui, project: String, items: Vec<Item>, at: usize) {
    sheet::open(ui, Lightbox { project, items: Rc::new(items), at });
}

#[derive(PartialEq)]
struct Lightbox {
    project: String,
    items: Rc<Vec<Item>>,
    at: usize,
}

impl Component for Lightbox {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let store = use_store();
        let a11y = use_a11y();
        let mut at = use_state(|| self.at.min(self.items.len().saturating_sub(1)));
        let mut pics = use_state(HashMap::<usize, Pic>::new);
        let mut pose = use_state(|| FIT);
        let mut stage = use_state(|| (0f32, 0f32));
        let mut drag = use_state(|| None::<((f32, f32), (f32, f32))>);
        let mut pressed = use_state(|| None::<Instant>);
        let (win_w, win_h) = *store.ui.window.read();
        let count = self.items.len();
        let current = *at.read();

        let items = self.items.clone();
        use_side_effect_with_deps(&current, move |at| {
            let wanted: Vec<usize> = [at.checked_sub(1), Some(*at), at.checked_add(1)].into_iter().flatten().filter(|i| *i < items.len()).collect();
            let missing: Vec<usize> = {
                let mut map = pics.write();
                map.retain(|k, _| wanted.contains(k));
                wanted.into_iter().filter(|i| !map.contains_key(i)).collect()
            };
            if missing.is_empty() {
                return;
            }
            {
                let mut map = pics.write();
                for i in &missing {
                    map.insert(*i, Pic::Loading);
                }
            }
            for i in missing {
                let (url, thumb) = (items[i].large.clone(), items[i].thumb.clone());
                spawn_in_window(async move {
                    let wanted = move || pics.try_peek().is_some_and(|map| map.contains_key(&i));
                    if let Some(next) = fetch(url, Some(thumb), wanted).await
                        && let Some(mut map) = pics.try_write()
                        && map.contains_key(&i)
                    {
                        map.insert(i, next);
                    }
                });
            }
        });

        let shown = pics.read().get(&current).cloned().unwrap_or(Pic::Loading);
        let size = match &shown {
            Pic::Ready { size, .. } => (size.0 as f32, size.1 as f32),
            _ => (0., 0.),
        };
        let area = *stage.read();
        let fit = fit_scale(area, size);
        let now = *pose.read();
        let scale = now.scale(fit);
        let drawn = (size.0 * scale, size.1 * scale);
        let pan = held(now.pan, drawn, area);
        let movable = drawn.0 > area.0 + 0.5 || drawn.1 > area.1 + 0.5;
        let last = current + 1 >= count;

        let zoom_by = move |factor: f32| {
            if size.0 <= 0. {
                return;
            }
            let area = *stage.peek();
            let next = pose.peek().factor(factor, fit_scale(area, size), area, size, (0., 0.));
            put(pose, next);
        };
        let mut step = move |by: isize| {
            let next = (current as isize).saturating_add(by).clamp(0, count as isize - 1) as usize;
            if next != current {
                put(pose, FIT);
                pressed.set(None);
                drag.set_if_modified(None);
                at.set(next);
            }
        };
        let keys = move |e: Event<KeyboardEventData>| {
            if e.modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) {
                return;
            }
            let area = *stage.peek();
            let fit = fit_scale(area, size);
            let sized = size.0 > 0.;
            match &e.key {
                Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp) => step(-1),
                Key::Named(NamedKey::ArrowRight | NamedKey::PageDown) => step(1),
                Key::Named(NamedKey::Home) => step(isize::MIN / 2),
                Key::Named(NamedKey::End) => step(isize::MAX / 2),
                Key::Character(c) if sized && (c == "+" || c == "=") => zoom_by(BUTTON_ZOOM),
                Key::Character(c) if sized && c == "-" => zoom_by(1. / BUTTON_ZOOM),
                Key::Character(c) if c == "0" => put(pose, FIT),
                Key::Character(c) if sized && c == "1" => put(pose, Pose::at_scale(1., fit, area, size, (0., 0.))),
                _ => return,
            }
            e.stop_propagation();
        };

        let (zoom_out, zoom_in, reset) = (zoom_by, zoom_by, pose);
        let toolbar = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .content(Content::flex())
            .child(rect().width(Size::flex(1.)).child(secondary(&t, trf("%1$lld von %2$lld", &[&(current + 1), &count])).max_lines(1)))
            .child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::XXS)
                    .child(Button::icon("browser.gallery.out", Icon::Minus, tr("Verkleinern")).tip("-").on_press(move |_| zoom_out(1. / BUTTON_ZOOM)))
                    .child(rect().width(Size::px(52.)).center().child(figure(&t, format!("{} %", (scale * 100.).round() as i64), font::CALLOUT, t.text2)))
                    .child(Button::icon("browser.gallery.in", Icon::Plus, tr("Vergrößern")).tip("+").on_press(move |_| zoom_in(BUTTON_ZOOM)))
                    .child(Button::new("browser.gallery.fit", tr("Einpassen")).glyph(Icon::Fit).on(now.zoom.is_none()).tip("0").on_press(move |_| put(reset, FIT))),
            );

        let mut surface = rect()
            .width(Size::fill())
            .height(Size::fill())
            .background(t.well)
            .corner_radius(t.shape.control)
            .overflow(Overflow::Clip)
            .cursor(if movable { if drag.read().is_some() { CursorIcon::Grabbing } else { CursorIcon::Grab } } else { CursorIcon::Default })
            .on_sized(move |e: Event<SizedEventData>| stage.set_if_modified((e.area.width(), e.area.height())))
            .on_wheel(move |e: Event<WheelEventData>| {
                e.stop_propagation();
                if size.0 <= 0. {
                    return;
                }
                let area = *stage.peek();
                let at = (e.element_location.x as f32 - area.0 / 2., e.element_location.y as f32 - area.1 / 2.);
                let next = pose.peek().factor(wheel_factor(e.delta_y), fit_scale(area, size), area, size, at);
                pose.set_if_modified(next);
            })
            .on_pointer_down(move |e: Event<PointerEventData>| {
                if !e.data().is_primary() || size.0 <= 0. {
                    return;
                }
                let area = *stage.peek();
                let fit = fit_scale(area, size);
                let twice = pressed.peek().is_some_and(|at| at.elapsed() < DOUBLE);
                pressed.set(if twice { None } else { Some(Instant::now()) });
                if twice {
                    let at = (e.element_location().x as f32 - area.0 / 2., e.element_location().y as f32 - area.1 / 2.);
                    put(pose, pose.peek().toggled(fit, area, size, at));
                    return;
                }
                let current = *pose.peek();
                let place = held(current.pan, (size.0 * current.scale(fit), size.1 * current.scale(fit)), area);
                drag.set(Some(((e.global_location().x as f32, e.global_location().y as f32), place)));
            })
            .on_global_pointer_move(move |e: Event<PointerEventData>| {
                let Some((from, start)) = *drag.peek() else { return };
                let area = *stage.peek();
                let fit = fit_scale(area, size);
                let current = *pose.peek();
                let at = e.global_location();
                let pan = held((start.0 + at.x as f32 - from.0, start.1 + at.y as f32 - from.1), (size.0 * current.scale(fit), size.1 * current.scale(fit)), area);
                pose.set_if_modified(Pose { zoom: current.zoom, pan });
            })
            .on_global_pointer_up(move |_: Event<PointerEventData>| drag.set_if_modified(None));
        surface = match shown {
            Pic::Loading => surface.center().child(rect().width(Size::percent(60.)).height(Size::percent(60.)).corner_radius(t.shape.tile).background(t.hover)),
            Pic::Failed => surface.center().spacing(space::M).child(icon(&t, Icon::ImageOff, iconsize::L * 2., t.text3)).child(strong(&t, tr("Das Bild lässt sich nicht öffnen."), font::BODY, t.text1)).child(secondary(&t, tr("Mit den Pfeilen geht es zu den anderen Bildern."))),
            Pic::Ready { handle, .. } => {
                let left = (area.0 - drawn.0) / 2. + pan.0;
                let top = (area.1 - drawn.1) / 2. + pan.1;
                surface.child(rect().position(Position::new_absolute().left(left).top(top)).child(image(handle).width(Size::px(drawn.0)).height(Size::px(drawn.1)).aspect_ratio(AspectRatio::None)))
            }
        };
        let middle = (area.1 / 2. - 14.).max(0.);
        if current > 0 {
            surface = surface.child(rect().position(Position::new_absolute().left(space::M).top(middle)).on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation()).child(Button::icon("browser.gallery.previous", Icon::ChevronLeft, tr("Vorheriges Bild")).look(Look::Secondary).tip(Shortcut::bare(Code::ArrowLeft).label()).on_press(move |_| step(-1))));
        }
        if !last {
            surface = surface.child(rect().position(Position::new_absolute().right(space::M).top(middle)).on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation()).child(Button::icon("browser.gallery.next", Icon::ChevronRight, tr("Nächstes Bild")).look(Look::Secondary).tip(Shortcut::bare(Code::ArrowRight).label()).on_press(move |_| step(1))));
        }
        let caption_line = self.items.get(current).and_then(|i| i.title.clone()).filter(|s| !s.trim().is_empty());
        let stage_h = (win_h - STAGE_CHROME).clamp(STAGE_MIN, STAGE_MAX);
        let body = rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_auto_focus(true)
            .a11y_role(AccessibilityRole::Group)
            .a11y_alt(tr("Galerie"))
            .width(Size::fill())
            .spacing(space::S)
            .on_global_key_down(keys)
            .child(toolbar)
            .child(rect().width(Size::fill()).height(Size::px(stage_h)).child(surface))
            .child(rect().width(Size::fill()).height(Size::px(font::CAPTION * font::LINE)).child(caption(&t, caption_line.unwrap_or_default())));
        let close = Primary { label: tr("Schließen"), disabled: None, danger: false, busy: false, run: EventHandler::new(move |_| sheet::close(store.ui)) };
        Card::new("browser.gallery", self.project.clone()).width((win_w - CARD_MARGIN).clamp(crate::view::theme::frame::SHEET, CARD_MAX)).body(body).primary(close).primary_id("browser.gallery.close").inform()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_picture_shrinks_into_the_box_and_a_small_one_stays() {
        let wide = bounded(image::DynamicImage::new_rgba8(4000, 1000), THUMB_BOX);
        assert_eq!((wide.width(), wide.height()), (1024, 256));
        let tall = bounded(image::DynamicImage::new_rgba8(1000, 4000), THUMB_BOX);
        assert_eq!((tall.width(), tall.height()), (144, 576));
        let small = bounded(image::DynamicImage::new_rgba8(300, 200), THUMB_BOX);
        assert_eq!((small.width(), small.height()), (300, 200));
    }

    #[test]
    fn the_grid_fills_its_width_with_tiles_of_a_readable_size() {
        assert_eq!(columns(0.), 1);
        assert_eq!(columns(TILE_MIN), 1);
        assert_eq!(columns(TILE_MIN * 2. + GAP), 2);
        assert_eq!(columns(TILE_MIN * 3. + GAP * 2. - 1.), 2);
        let room = 900.;
        let cols = columns(room);
        assert!(tile_width(room, cols) >= TILE_MIN);
        assert!(tile_width(room, cols) * cols as f32 + GAP * (cols - 1) as f32 <= room + 0.01);
        assert_eq!(tile_width(3000., 1), TILE_MAX);
    }
}