//! Zoom and pan of one picture in its stage: the maths of a viewer, free of drawing.

use freya::prelude::*;

pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 8.;
pub const BUTTON_ZOOM: f32 = 1.25;
const WHEEL_ZOOM: f32 = 0.0022;

/// How a picture sits in its stage: fitted, or at a scale with its center moved off the stage's center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub zoom: Option<f32>,
    pub pan: (f32, f32),
}

pub const FIT: Pose = Pose { zoom: None, pan: (0., 0.) };

pub fn fit_scale(stage: (f32, f32), size: (f32, f32)) -> f32 {
    if size.0 <= 0. || size.1 <= 0. {
        return 1.;
    }
    (stage.0 / size.0).min(stage.1 / size.1).clamp(MIN_ZOOM, 1.)
}

pub fn held(pan: (f32, f32), shown: (f32, f32), stage: (f32, f32)) -> (f32, f32) {
    let room = |shown: f32, stage: f32| ((shown - stage) / 2.).max(0.);
    let (x, y) = (room(shown.0, stage.0), room(shown.1, stage.1));
    (pan.0.clamp(-x, x), pan.1.clamp(-y, y))
}

impl Pose {
    pub fn scale(self, fit: f32) -> f32 {
        self.zoom.unwrap_or(fit)
    }

    pub fn at_scale(scale: f32, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
        FIT.zoomed(scale, fit, stage, size, at)
    }

    /// The pose after the scale became `scale`, the point `at` (from the stage's center) staying under the pointer.
    pub fn zoomed(self, scale: f32, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
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

    pub fn factor(self, factor: f32, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
        self.zoomed(self.scale(fit) * factor, fit, stage, size, at)
    }

    pub fn toggled(self, fit: f32, stage: (f32, f32), size: (f32, f32), at: (f32, f32)) -> Self {
        match self.zoom {
            Some(_) => FIT,
            None => Self::at_scale(1., fit, stage, size, at),
        }
    }
}

pub fn put(pose: State<Pose>, next: Pose) {
    let mut pose = pose;
    pose.set_if_modified(next);
}

/// Moves `pose` to what `next` makes of it. The current pose is copied out before it is written, so no borrow of the
/// state is alive while it changes.
pub fn adjust(pose: State<Pose>, next: impl FnOnce(Pose) -> Pose) {
    let now = *pose.peek();
    put(pose, next(now));
}

pub fn wheel_factor(delta: f64) -> f32 {
    (delta as f32 * WHEEL_ZOOM).exp()
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

    fn view_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                view_sources(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push(path);
            }
        }
    }

    fn writes_while_reading(line: &str) -> Option<String> {
        for opener in ["put(", ".set(", ".set_if_modified("] {
            let mut rest = line;
            while let Some(at) = rest.find(opener) {
                let (before, after) = (&rest[..at], &rest[at + opener.len()..]);
                let name = if opener == "put(" {
                    after.split(',').next().unwrap_or_default().trim().to_owned()
                } else {
                    before.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<Vec<_>>().into_iter().rev().collect()
                };
                let mut depth = 0;
                let args: String = after
                    .chars()
                    .take_while(|c| {
                        depth += i32::from(*c == '(') - i32::from(*c == ')');
                        depth >= 0
                    })
                    .collect();
                let named = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
                if named && (args.contains(&format!("{name}.peek()")) || args.contains(&format!("{name}.read()"))) {
                    return Some(name);
                }
                rest = after;
            }
        }
        None
    }

    #[test]
    fn a_state_is_never_written_while_a_borrow_of_it_is_alive() {
        let mut files = Vec::new();
        view_sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/view"), &mut files);
        let mut found = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            for (n, line) in text.lines().enumerate() {
                if line.contains("writes_while_reading(") {
                    continue;
                }
                if let Some(name) = writes_while_reading(line) {
                    found.push(format!("{}:{} writes {name} inside a borrow of it", file.display(), n + 1));
                }
            }
        }
        assert!(found.is_empty(), "{}", found.join("\n"));
    }

    #[test]
    fn the_scan_sees_a_write_inside_a_borrow() {
        assert_eq!(writes_while_reading("put(pose, pose.peek().factor(1.))"), Some("pose".into()));
        assert_eq!(writes_while_reading("pose.set(pose.peek().toggled())"), Some("pose".into()));
        assert_eq!(writes_while_reading("let next = pose.peek().factor(1.); put(pose, next);"), None);
        assert_eq!(writes_while_reading("size.set(now.larger(*tile.peek()))"), None);
    }
}
