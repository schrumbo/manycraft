use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use crate::core::shots;
use crate::ui::theme::files;
use crate::view::icons::{Icon, icon};
use crate::view::kit::{off_ui, use_theme};
use crate::view::theme::iconsize;

struct Entry<V> {
    stamp: i64,
    value: V,
    used: u64,
}

/// Values by file, each valid for one file stamp, the least recently used dropped beyond `cap`.
struct Kept<V> {
    map: HashMap<PathBuf, Entry<V>>,
    clock: u64,
    cap: usize,
}

impl<V: Clone> Kept<V> {
    fn new(cap: usize) -> Self {
        Self { map: HashMap::new(), clock: 0, cap }
    }

    fn get(&mut self, path: &Path, stamp: i64) -> Option<V> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.map.get_mut(path).filter(|e| e.stamp == stamp)?;
        entry.used = clock;
        Some(entry.value.clone())
    }

    fn put(&mut self, path: PathBuf, stamp: i64, value: V) {
        self.clock += 1;
        self.map.insert(path, Entry { stamp, value, used: self.clock });
        while self.map.len() > self.cap {
            let Some(oldest) = self.map.iter().min_by_key(|(_, e)| e.used).map(|(p, _)| p.clone()) else { return };
            self.map.remove(&oldest);
        }
    }
}

thread_local! {
    static KEPT: RefCell<Kept<Option<ImageHandle>>> = RefCell::new(Kept::new(files::THUMBS_KEPT));
    static GATE: (async_channel::Sender<()>, async_channel::Receiver<()>) = async_channel::bounded(files::DECODERS);
}

fn lookup(path: &Path, stamp: i64) -> Option<Option<ImageHandle>> {
    KEPT.with(|k| k.borrow_mut().get(path, stamp))
}

fn keep(path: PathBuf, stamp: i64, picture: Option<ImageHandle>) {
    KEPT.with(|k| k.borrow_mut().put(path, stamp, picture));
}

/// One of the few places to decode at once; freed when dropped, also when the task is cancelled.
struct Slot;

impl Slot {
    async fn take() -> Self {
        let gate = GATE.with(|g| g.0.clone());
        let _ = gate.send(()).await;
        Self
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        GATE.with(|g| {
            let _ = g.1.try_recv();
        });
    }
}

/// The picture of one screenshot at the cell's size: decoded off the UI thread once per file stamp, kept for a while.
#[derive(Clone, PartialEq)]
pub struct Thumb {
    pub path: PathBuf,
    pub stamp: i64,
    pub width: f32,
    pub height: f32,
}

impl Component for Thumb {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut ready = use_state(|| 0u32);
        let _ = ready.read();
        let key = (self.path.clone(), self.stamp);
        use_side_effect_with_deps(&key, move |(path, stamp)| {
            if lookup(path, *stamp).is_some() {
                return;
            }
            let (path, stamp) = (path.clone(), *stamp);
            spawn(async move {
                let _slot = Slot::take().await;
                if lookup(&path, stamp).is_none() {
                    let source = path.clone();
                    let decoded = off_ui(move || shots::thumbnail(&source, files::THUMB_PX, files::THUMB_PX).ok().map(|p| (p.width(), p.height(), p.into_raw()))).await.flatten();
                    let picture = decoded.and_then(|(w, h, rgba)| ImageHandle::from_rgba(w, h, Bytes::from(rgba), AlphaType::Unpremul));
                    keep(path, stamp, picture);
                }
                let next = ready.peek().wrapping_add(1);
                ready.set(next);
            });
        });
        let frame = rect().width(Size::px(self.width)).height(Size::px(self.height)).background(t.well).corner_radius(t.shape.control).overflow(Overflow::Clip).center();
        match lookup(&self.path, self.stamp) {
            Some(Some(handle)) => frame.child(image(handle).width(Size::px(self.width)).height(Size::px(self.height)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center)),
            Some(None) => frame.child(icon(&t, Icon::Alert, iconsize::L, t.text3)),
            None => frame,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_valid_for_its_file_stamp_only() {
        let mut kept = Kept::new(4);
        kept.put(PathBuf::from("a.png"), 1, 10);
        assert_eq!(kept.get(Path::new("a.png"), 1), Some(10));
        assert_eq!(kept.get(Path::new("a.png"), 2), None);
        assert_eq!(kept.get(Path::new("b.png"), 1), None);
    }

    #[test]
    fn the_least_recently_used_goes_first() {
        let mut kept = Kept::new(2);
        kept.put(PathBuf::from("a"), 1, 1);
        kept.put(PathBuf::from("b"), 1, 2);
        assert_eq!(kept.get(Path::new("a"), 1), Some(1));
        kept.put(PathBuf::from("c"), 1, 3);
        assert_eq!(kept.get(Path::new("b"), 1), None);
        assert_eq!(kept.get(Path::new("a"), 1), Some(1));
        assert_eq!(kept.get(Path::new("c"), 1), Some(3));
    }
}
