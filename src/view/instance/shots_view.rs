use std::time::{Duration, Instant};

use freya::prelude::*;

use super::shots_parts::Same;
use super::shots_thumbs::{Source, full, load_full};
use crate::core::i18n::{tr, trf};
use crate::core::shots::Shot;
use crate::view::commands::Shortcut;
use crate::view::icons::{Icon, icon};
use crate::view::kit::button::{Button, Look};
use crate::view::kit::text::{figure, heading, secondary, strong};
use crate::view::kit::{after, use_theme};
use crate::view::theme::{font, iconsize, space};

const SKELETON_SHARE: f32 = 60.;
const DOUBLE: Duration = Duration::from_millis(400);
const SETTLE: Duration = Duration::from_millis(80);
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 8.;
const BUTTON_ZOOM: f32 = 1.25;
const WHEEL_ZOOM: f32 = 0.0022;

/// How a picture sits in its stage: fitted, or at a scale with its center moved off the stage's center.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    zoom: Option<f32>,
    pan: (f32, f32),
}

const FIT: Pose = Pose { zoom: None, pan: (0., 0.) };

fn fit_scale(stage: (f32, f32), size: (f32, f32)) -> f32 {
    if size.0 <= 0. || size.1 <= 0. {
        return 1.;
    }
    (stage.0 / size.0).min(stage.1 / size.1).clamp(MIN_ZOOM, 1.)
}

fn held(pan: (f32, f32), shown: (f32, f32), stage: (f32, f32)) -> (f32, f32) {
    let room = |shown: f32, stage: f32| ((shown - stage) / 2.).max(0.);
    let (x, y) = (room(shown.0, stage.0), room(shown.1, stage.1));
    (pan.0.clamp(-x, x), pan.1.clamp(-y, y))
}

impl Pose {
    fn scale(self, fit: f32) -> f32 {
        self.zoom.unwrap_or(fit)
    }

    fn at_scale(scale: f32, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
        FIT.zoomed(scale, fit, stage, size, at)
    }

    /// The pose after the scale became `scale`, the point `at` (from the stage's center) staying under the pointer.
    fn zoomed(self, scale: f32, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
        let now = self.scale(fit);
        let next = scale.clamp(MIN_ZOOM, MAX_ZOOM);
        if next <= fit {
            return FIT;
        }
        let pan = held(self.pan, (size.0 * now, size.1 * now), stage);
        let point = ((at.0 - pan.0) / now, (at.1 - pan.1) / now);
        let moved = (at.0 - point.0 * next, at.1 - point.1 * next);
        Self { zoom: Some(next), pan: held(moved, (size.0 * next, size.1 * next), stage) }
    }

    fn factor(self, factor: f32, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
        self.zoomed(self.scale(fit) * factor, fit, stage, size, at)
    }

    fn toggled(self, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
        match self.zoom {
            Some(_) => FIT,
            None => Self::at_scale(1., fit, stage, size, at),
        }
    }
}

fn put(pose: State<Pose>, next: Pose) {
    let mut pose = pose;
    pose.set_if_modified(next);
}

fn wheel_factor(delta: f64) -> f32 {
    (delta as f32 * WHEEL_ZOOM).exp()
}

/// One screenshot the viewer shows: where its picture comes from, its stamp and the line above it.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub source: Source,
    pub stamp: i64,
    pub title: String,
}

/// The screenshot files as the viewer shows them, each named by its file.
pub fn files(shots: &[Shot]) -> Vec<Picture> {
    shots.iter().map(|s| Picture { source: Source::File(s.path.clone()), stamp: s.taken, title: s.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }).collect()
}

/// A screenshot shown large in the tab: zoom with the wheel, the buttons or +, − and 0, drag it when it is larger than the
/// stage, step with the arrow keys, leave with Esc.
#[derive(PartialEq)]
pub struct Viewer {
    pub shots: Same<Vec<Picture>>,
    pub at: usize,
    pub who: String,
    pub copying: bool,
    pub on_step: EventHandler<isize>,
    pub on_close: EventHandler<()>,
    pub on_copy: EventHandler<()>,
    /// Offers to save a copy where the user picks; for pictures that are not files.
    pub on_save: Option<EventHandler<()>>,
    pub on_reveal: Option<EventHandler<()>>,
    pub on_external: Option<EventHandler<()>>,
    pub on_delete: Option<EventHandler<()>>,
}

impl Viewer {
    fn shot(&self, at: usize) -> Option<&Picture> {
        self.shots.0.get(at)
    }
}

impl Component for Viewer {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let a11y = use_a11y();
        let mut pose = use_state(|| FIT);
        let mut stage = use_state(|| (0f32, 0f32));
        let mut drag = use_state(|| None::<((f32, f32), (f32, f32))>);
        let mut pressed = use_state(|| None::<Instant>);
        let mut ready = use_state(|| 0u32);
        let mut loading = use_state(|| None::<TaskHandle>);
        let _ = ready.read();
        let Some(shot) = self.shot(self.at) else {
            return rect().width(Size::fill()).height(Size::fill()).center().child(secondary(&t, tr("Der Screenshot ist nicht mehr da."))).into_element();
        };
        let neighbors: Vec<(Source, i64)> = [self.at.checked_add(1), self.at.checked_sub(1)].into_iter().flatten().filter_map(|n| self.shot(n)).map(|s| (s.source.clone(), s.stamp)).collect();
        use_side_effect_with_deps(&(shot.source.clone(), shot.stamp, neighbors), move |(path, stamp, neighbors)| {
            if let Some(old) = loading.write().take() {
                old.cancel();
            }
            let (path, stamp, neighbors) = (path.clone(), *stamp, neighbors.clone());
            loading.set(Some(spawn(async move {
                if full(&path, stamp).is_none() {
                    after(SETTLE).await;
                }
                load_full(path, stamp).await;
                let next = ready.peek().wrapping_add(1);
                ready.set(next);
                for (path, stamp) in neighbors {
                    load_full(path, stamp).await;
                }
            })));
        });

        let picture = full(&shot.source, shot.stamp);
        let size = picture.as_ref().and_then(|p| p.as_ref()).map_or((0., 0.), |f| (f.original.0 as f32, f.original.1 as f32));
        let area = *stage.read();
        let fit = fit_scale(area, size);
        let now = *pose.read();
        let scale = now.scale(fit);
        let shown = (size.0 * scale, size.1 * scale);
        let pan = held(now.pan, shown, area);
        let movable = shown.0 > area.0 + 0.5 || shown.1 > area.1 + 0.5;
        let name = shot.title.clone();
        let count = self.shots.0.len();
        let last = self.at + 1 >= count;

        let zoom_by = move |factor: f32| {
            if size.0 <= 0. {
                return;
            }
            let area = *stage.peek();
            let fit = fit_scale(area, size);
            let next = pose.peek().factor(factor, fit, area, size, (0., 0.));
            put(pose, next);
        };
        let on_step = self.on_step.clone();
        let step = move |by: isize| {
            put(pose, FIT);
            on_step.call(by);
        };
        let delete = {
            let on_delete = self.on_delete.clone();
            move || {
                if let Some(on_delete) = &on_delete {
                    put(pose, FIT);
                    on_delete.call(());
                }
            }
        };
        let (key_step, key_close, key_delete) = (step.clone(), self.on_close.clone(), delete.clone());
        let self_deletes = self.on_delete.is_some();
        let keys = move |e: Event<KeyboardEventData>| {
            if e.modifiers.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META) {
                return;
            }
            let area = *stage.peek();
            let fit = fit_scale(area, size);
            let sized = size.0 > 0.;
            match &e.key {
                Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp) => key_step(-1),
                Key::Named(NamedKey::ArrowRight | NamedKey::PageDown) => key_step(1),
                Key::Named(NamedKey::Home) => key_step(isize::MIN / 2),
                Key::Named(NamedKey::End) => key_step(isize::MAX / 2),
                Key::Named(NamedKey::Escape) => key_close.call(()),
                Key::Named(NamedKey::Delete) if self_deletes => key_delete(),
                Key::Character(c) if sized && (c == "+" || c == "=") => put(pose, pose.peek().factor(BUTTON_ZOOM, fit, area, size, (0., 0.))),
                Key::Character(c) if sized && c == "-" => put(pose, pose.peek().factor(1. / BUTTON_ZOOM, fit, area, size, (0., 0.))),
                Key::Character(c) if c == "0" => put(pose, FIT),
                Key::Character(c) if sized && c == "1" => put(pose, Pose::at_scale(1., fit, area, size, (0., 0.))),
                _ => return,
            }
            e.stop_propagation();
        };

        let (zoom_out, zoom_in, reset) = (zoom_by, zoom_by, pose);
        let percent = format!("{} %", (scale * 100.).round() as i64);
        let mut toolbar = rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(space::S)
            .padding((space::M, space::L))
            .content(Content::flex())
            .child(Button::new("instance.shots.view.back", tr("Alle Screenshots")).plain().glyph(Icon::ChevronLeft).tip(Shortcut::bare(Code::Escape).label()).on_press({
                let close = self.on_close.clone();
                move |_| close.call(())
            }))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(strong(&t, name, font::BODY, t.text1).max_lines(1))
                    .child(secondary(&t, trf("%1$lld von %2$lld · %3$@", &[&(self.at + 1), &count, &self.who])).max_lines(1)),
            )
            .child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(space::XXS)
                    .child(Button::icon("instance.shots.view.out", Icon::Minus, tr("Verkleinern")).tip("-").on_press(move |_| zoom_out(1. / BUTTON_ZOOM)))
                    .child(rect().width(Size::px(52.)).center().child(figure(&t, percent, font::CALLOUT, t.text2)))
                    .child(Button::icon("instance.shots.view.in", Icon::Plus, tr("Vergrößern")).tip("+").on_press(move |_| zoom_in(BUTTON_ZOOM)))
                    .child(Button::new("instance.shots.view.fit", tr("Einpassen")).glyph(Icon::Fit).on(now.zoom.is_none()).tip("0").on_press(move |_| put(reset, FIT))),
            )
            .child(Button::new("instance.shots.view.copy", tr("Bild kopieren")).glyph(Icon::Copy).busy(self.copying).tip(Shortcut::primary(Code::KeyC).label()).on_press({
                let copy = self.on_copy.clone();
                move |_| copy.call(())
            }));
        if let Some(save) = self.on_save.clone() {
            toolbar = toolbar.child(Button::new("instance.shots.view.save", tr("Speichern unter …")).glyph(Icon::Download).tip(Shortcut::primary(Code::KeyS).label()).on_press(move |_| save.call(())));
        }
        if let Some(open) = self.on_external.clone() {
            toolbar = toolbar.child(Button::icon("instance.shots.view.external", Icon::External, tr("Mit Standard-App öffnen")).on_press(move |_| open.call(())));
        }
        if let Some(reveal) = self.on_reveal.clone() {
            toolbar = toolbar.child(Button::icon("instance.shots.view.reveal", Icon::Folder, tr("Im Ordner zeigen")).on_press(move |_| reveal.call(())));
        }
        if self.on_delete.is_some() {
            toolbar = toolbar.child(Button::icon("instance.shots.view.delete", Icon::Trash, tr("Löschen")).tip(Shortcut::bare(Code::Delete).label()).on_press(move |_| delete()));
        }

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
                let fit = fit_scale(area, size);
                let next = pose.peek().factor(wheel_factor(e.delta_y), fit, area, size, at);
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
        surface = match picture {
            None => surface.center().child(rect().width(Size::percent(SKELETON_SHARE)).height(Size::percent(SKELETON_SHARE)).corner_radius(t.shape.tile).background(t.hover)),
            Some(None) => surface.center().spacing(space::M).child(icon(&t, Icon::ImageOff, iconsize::L * 2., t.text3)).child(heading(&t, tr("Das Bild lässt sich nicht öffnen."))),
            Some(Some(f)) => {
                let left = (area.0 - shown.0) / 2. + pan.0;
                let top = (area.1 - shown.1) / 2. + pan.1;
                surface.child(rect().position(Position::new_absolute().left(left).top(top)).child(image(f.handle).width(Size::px(shown.0)).height(Size::px(shown.1)).aspect_ratio(AspectRatio::None)))
            }
        };
        if self.at > 0 {
            let back = step.clone();
            surface = surface.child(rect().position(Position::new_absolute().left(space::M).top((area.1 / 2. - 14.).max(0.))).child(Button::icon("instance.shots.view.previous", Icon::ChevronLeft, tr("Vorheriger Screenshot")).look(Look::Secondary).tip(Shortcut::bare(Code::ArrowLeft).label()).on_press(move |_| back(-1))));
        }
        if !last {
            let forward = step;
            surface = surface.child(rect().position(Position::new_absolute().right(space::M).top((area.1 / 2. - 14.).max(0.))).child(Button::icon("instance.shots.view.next", Icon::ChevronRight, tr("Nächster Screenshot")).look(Look::Secondary).tip(Shortcut::bare(Code::ArrowRight).label()).on_press(move |_| forward(1))));
        }
        rect()
            .a11y_id(a11y)
            .a11y_focusable(true)
            .a11y_auto_focus(true)
            .a11y_role(AccessibilityRole::Group)
            .a11y_alt(tr("Screenshot"))
            .width(Size::fill())
            .height(Size::fill())
            .on_key_down(keys)
            .on_mouse_down(move |_| a11y.request_focus())
            .child(toolbar)
            .child(rect().width(Size::fill()).height(Size::flex(1.)).padding((0., space::L, space::L, space::L)).child(surface))
            .content(Content::flex())
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE: (f32, f32) = (800., 500.);
    const SIZE: (f32, f32) = (1920., 1080.);

    #[test]
    fn a_big_picture_fits_the_stage_and_a_small_one_keeps_its_size() {
        let fit = fit_scale(STAGE, SIZE);
        assert!((fit - 800. / 1920.).abs() < 1e-6);
        assert_eq!(fit_scale(STAGE, (100., 100.)), 1.);
        assert_eq!(fit_scale(STAGE, (0., 0.)), 1.);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_and_never_leaves_the_picture() {
        let fit = fit_scale(STAGE, SIZE);
        let at = (100., 50.);
        let pose = FIT.zoomed(1., fit, STAGE, SIZE, at);
        assert_eq!(pose.zoom, Some(1.));
        let before = (at.0 / fit, at.1 / fit);
        let after = ((at.0 - pose.pan.0) / 1., (at.1 - pose.pan.1) / 1.);
        assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3);
        let room = ((SIZE.0 - STAGE.0) / 2., (SIZE.1 - STAGE.1) / 2.);
        assert!(pose.pan.0.abs() <= room.0 && pose.pan.1.abs() <= room.1);
    }

    #[test]
    fn zooming_out_to_the_fit_returns_to_it_and_zoom_is_bounded() {
        let fit = fit_scale(STAGE, SIZE);
        let zoomed = FIT.zoomed(2., fit, STAGE, SIZE, (0., 0.));
        assert_eq!(zoomed.factor(0.1, fit, STAGE, SIZE, (0., 0.)), FIT);
        assert_eq!(FIT.zoomed(100., fit, STAGE, SIZE, (0., 0.)).zoom, Some(MAX_ZOOM));
    }

    #[test]
    fn a_double_click_toggles_between_the_fit_and_actual_size() {
        let fit = fit_scale(STAGE, SIZE);
        let actual = FIT.toggled(fit, STAGE, SIZE, (0., 0.));
        assert_eq!(actual.zoom, Some(1.));
        assert_eq!(actual.toggled(fit, STAGE, SIZE, (0., 0.)), FIT);
    }

    #[test]
    fn a_double_click_keeps_the_clicked_point_under_the_pointer() {
        let fit = fit_scale(STAGE, SIZE);
        let at = (300., 150.);
        let actual = FIT.toggled(fit, STAGE, SIZE, at);
        assert!((actual.pan.0 - (-420.)).abs() < 1e-3 && (actual.pan.1 - (-210.)).abs() < 1e-3);
    }

    #[test]
    fn a_picture_smaller_than_the_stage_cannot_be_moved() {
        assert_eq!(held((50., -30.), (400., 300.), STAGE), (0., 0.));
        assert_eq!(held((500., 0.), (1000., 300.), STAGE), (100., 0.));
    }

    #[test]
    fn the_wheel_zooms_in_when_it_rolls_up() {
        assert!(wheel_factor(53.) > 1.);
        assert!(wheel_factor(-53.) < 1.);
        assert!(wheel_factor(53.) < BUTTON_ZOOM);
    }
}