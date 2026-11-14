//! The one place on screen that shows either the grid or one game: its outer frame and whether it fills its monitor.

use serde::{Deserialize, Serialize};

/// A rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn right(self) -> i32 {
        self.x + self.w as i32
    }

    fn bottom(self) -> i32 {
        self.y + self.h as i32
    }

    fn overlap(self, other: Rect) -> u64 {
        let w = (self.right().min(other.right()) - self.x.max(other.x)).max(0) as u64;
        let h = (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0) as u64;
        w * h
    }

    fn center_distance(self, other: Rect) -> i64 {
        let dx = i64::from(self.x) * 2 + i64::from(self.w) - i64::from(other.x) * 2 - i64::from(other.w);
        let dy = i64::from(self.y) * 2 + i64::from(self.h) - i64::from(other.y) * 2 - i64::from(other.h);
        dx * dx + dy * dy
    }

    fn fit(self, area: Rect) -> Rect {
        let w = self.w.min(area.w);
        let h = self.h.min(area.h);
        Rect { x: self.x.clamp(area.x, area.right() - w as i32), y: self.y.clamp(area.y, area.bottom() - h as i32), w, h }
    }

    fn centered(self, area: Rect) -> Rect {
        let w = self.w.min(area.w);
        let h = self.h.min(area.h);
        Rect { x: area.x + (area.w - w) as i32 / 2, y: area.y + (area.h - h) as i32 / 2, w, h }
    }
}

/// How the place fills its monitor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Normal,
    Maximized,
    Fullscreen,
}

impl Mode {
    pub fn is_normal(&self) -> bool {
        *self == Mode::Normal
    }
}

/// Distances from a window's visible frame to its client area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Insets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// The place: the frame it has when it neither is maximized nor fullscreen, and its mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub frame: Rect,
    pub mode: Mode,
}

/// The client area a window with `insets` needs for its frame to cover `frame`.
pub fn client_in(frame: Rect, insets: Insets) -> Rect {
    Rect {
        x: frame.x + insets.left,
        y: frame.y + insets.top,
        w: (frame.w as i32 - insets.left - insets.right).max(1) as u32,
        h: (frame.h as i32 - insets.top - insets.bottom).max(1) as u32,
    }
}

/// The insets of a window whose frame is `frame` and whose client area is `client`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn insets(frame: Rect, client: Rect) -> Insets {
    Insets { left: client.x - frame.x, top: client.y - frame.y, right: frame.right() - client.right(), bottom: frame.bottom() - client.bottom() }
}

/// The work area `frame` is mostly on, or the nearest one.
fn home(frame: Rect, works: &[Rect]) -> Option<Rect> {
    works.iter().copied().max_by_key(|w| (frame.overlap(*w), -frame.center_distance(*w)))
}

/// `frame` as it is when its title bar can be grabbed on some work area, else moved and shrunk onto the work area it is closest to.
pub fn clamp(frame: Rect, works: &[Rect]) -> Rect {
    let bar = Rect { h: frame.h.min(40), ..frame };
    let reachable = works.iter().any(|w| frame.y >= w.y && bar.overlap(*w) >= u64::from(frame.w.min(120)) * u64::from(bar.h));
    match home(frame, works) {
        Some(work) if !reachable => frame.fit(work),
        _ => frame,
    }
}

/// The place after its shown window was seen at `frame` in `mode`. A maximized or fullscreen window keeps the frame it had before, moved onto the work area the window is on.
pub fn follow(before: Option<Place>, frame: Rect, mode: Mode, works: &[Rect]) -> Place {
    if mode.is_normal() {
        return Place { frame, mode };
    }
    let target = home(frame, works).unwrap_or(frame);
    let frame = match before.map(|p| p.frame) {
        Some(base) if home(base, works) == Some(target) && base.overlap(target) > 0 => base.fit(target),
        Some(base) => base.centered(target),
        None => Rect { w: target.w * 3 / 4, h: target.h * 3 / 4, ..target }.centered(target),
    };
    Place { frame, mode }
}

/// Where a hidden window of `size` waits for the place `frame`: at its corner, on the work area the place is on, so that it opens there without costing the place's size while hidden.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn parked(frame: Rect, size: Rect, works: &[Rect]) -> Rect {
    let at = Rect { x: frame.x, y: frame.y, w: size.w, h: size.h };
    match home(frame, works) {
        Some(work) => at.fit(work),
        None => at,
    }
}

/// The mode of a window whose client area is `client` on `monitor`: maximized when the system says so, fullscreen when its client area covers the monitor.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn mode_of(zoomed: bool, client: Rect, monitor: Rect) -> Mode {
    let covers = client.x <= monitor.x + 4 && client.y <= monitor.y + 4 && client.right() >= monitor.right() - 4 && client.bottom() >= monitor.bottom() - 4;
    if zoomed {
        Mode::Maximized
    } else if covers {
        Mode::Fullscreen
    } else {
        Mode::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    const LAPTOP: Rect = r(0, 0, 2560, 1552);
    const LEFT: Rect = r(-1920, 0, 1920, 1032);

    #[test]
    fn client_and_insets_round_trip() {
        let frame = r(100, 50, 1200, 800);
        let client = r(101, 81, 1198, 768);
        let i = insets(frame, client);
        assert_eq!(i, Insets { left: 1, top: 31, right: 1, bottom: 1 });
        assert_eq!(client_in(frame, i), client);
        assert_eq!(client_in(r(-1910, 10, 900, 600), Insets { left: 2, top: 45, right: 2, bottom: 2 }), r(-1908, 55, 896, 553));
    }

    #[test]
    fn client_never_collapses() {
        assert_eq!(client_in(r(0, 0, 10, 20), Insets { left: 6, top: 31, right: 6, bottom: 1 }), r(6, 31, 1, 1));
    }

    #[test]
    fn reachable_frames_stay() {
        let works = [LAPTOP, LEFT];
        assert_eq!(clamp(r(200, 100, 1200, 800), &works), r(200, 100, 1200, 800));
        assert_eq!(clamp(r(-300, 100, 1200, 800), &works), r(-300, 100, 1200, 800));
        assert_eq!(clamp(r(2400, 1400, 1200, 800), &works), r(2400, 1400, 1200, 800));
        assert_eq!(clamp(r(10, 10, 100, 100), &[]), r(10, 10, 100, 100));
    }

    #[test]
    fn frames_on_a_lost_monitor_come_back() {
        assert_eq!(clamp(r(-1753, 120, 1200, 800), &[LAPTOP]), r(0, 120, 1200, 800));
        assert_eq!(clamp(r(5000, 3000, 3000, 2000), &[LAPTOP]), r(0, 0, 2560, 1552));
    }

    #[test]
    fn title_bar_above_the_screen_comes_down() {
        assert_eq!(clamp(r(300, -200, 1200, 800), &[LAPTOP]), r(300, 0, 1200, 800));
        assert_eq!(clamp(r(2530, 300, 1200, 800), &[LAPTOP]), r(1360, 300, 1200, 800));
    }

    #[test]
    fn normal_windows_move_the_place() {
        let before = Place { frame: r(100, 100, 800, 600), mode: Mode::Normal };
        assert_eq!(follow(Some(before), r(400, 200, 1000, 700), Mode::Normal, &[LAPTOP]), Place { frame: r(400, 200, 1000, 700), mode: Mode::Normal });
    }

    #[test]
    fn maximizing_keeps_the_frame_on_the_same_monitor() {
        let before = Place { frame: r(100, 100, 800, 600), mode: Mode::Normal };
        let after = follow(Some(before), r(0, 0, 2560, 1552), Mode::Maximized, &[LAPTOP, LEFT]);
        assert_eq!(after, Place { frame: r(100, 100, 800, 600), mode: Mode::Maximized });
    }

    #[test]
    fn fullscreen_on_another_monitor_moves_the_frame_there() {
        let before = Place { frame: r(100, 100, 800, 600), mode: Mode::Normal };
        let after = follow(Some(before), r(-1920, 0, 1920, 1080), Mode::Fullscreen, &[LAPTOP, LEFT]);
        assert_eq!(after, Place { frame: r(-1360, 216, 800, 600), mode: Mode::Fullscreen });
    }

    #[test]
    fn first_maximized_place_gets_a_frame() {
        assert_eq!(follow(None, r(0, 0, 2560, 1552), Mode::Maximized, &[LAPTOP]), Place { frame: r(320, 194, 1920, 1164), mode: Mode::Maximized });
    }

    #[test]
    fn hidden_windows_park_small_at_the_corner() {
        let size = r(5, 5, 876, 536);
        assert_eq!(parked(r(318, 159, 1924, 1202), size, &[LAPTOP, LEFT]), r(318, 159, 876, 536));
        assert_eq!(parked(r(-500, 600, 480, 400), size, &[LAPTOP, LEFT]), r(-876, 496, 876, 536));
        assert_eq!(parked(r(2400, 100, 100, 100), size, &[LAPTOP]), r(1684, 100, 876, 536));
        assert_eq!(parked(r(10, 10, 100, 100), size, &[]), r(10, 10, 876, 536));
    }

    #[test]
    fn modes_are_read_from_the_window() {
        let monitor = r(0, 0, 2560, 1600);
        assert_eq!(mode_of(true, r(0, 23, 2560, 1529), monitor), Mode::Maximized);
        assert_eq!(mode_of(false, r(-1, -1, 2562, 1602), monitor), Mode::Fullscreen);
        assert_eq!(mode_of(false, r(0, 0, 2561, 1600), monitor), Mode::Fullscreen);
        assert_eq!(mode_of(false, r(2, 45, 2556, 1553), monitor), Mode::Normal);
        assert_eq!(mode_of(false, r(10, 10, 800, 600), monitor), Mode::Normal);
    }
}
