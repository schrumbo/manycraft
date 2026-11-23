//! The icon of a Modrinth project, fetched and decoded off the UI thread once and kept by project id.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

use freya::elements::image::ImageHandle;
use freya::engine::prelude::AlphaType;
use freya::prelude::*;

use super::{off_ui, use_theme};
use crate::core::modicons;
use crate::view::icons::{Icon, icon};
use crate::view::theme::{control, iconsize};

const KEPT_ICONS: usize = 48;
const DECODERS: usize = 4;

/// Values by key, the least recently used dropped beyond `cap`.
struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    clock: u64,
    cap: usize,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Lru<K, V> {
    fn new(cap: usize) -> Self {
        Self { map: HashMap::new(), clock: 0, cap }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.map.get_mut(key)?;
        entry.1 = clock;
        Some(entry.0.clone())
    }

    fn put(&mut self, key: K, value: V) {
        self.clock += 1;
        self.map.insert(key, (value, self.clock));
        while self.map.len() > self.cap {
            let Some(oldest) = self.map.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { return };
            self.map.remove(&oldest);
        }
    }
}

thread_local! {
    static ICONS: RefCell<Lru<String, ImageHandle>> = RefCell::new(Lru::new(KEPT_ICONS));
}

static GATE: LazyLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> = LazyLock::new(|| async_channel::bounded(DECODERS));

fn cached(pid: &str) -> Option<ImageHandle> {
    ICONS.with(|i| i.borrow_mut().get(&pid.to_owned()))
}

fn remember(pid: String, handle: ImageHandle) {
    ICONS.with(|i| i.borrow_mut().put(pid, handle));
}

struct Slot;

impl Slot {
    async fn take() -> Self {
        let _ = GATE.0.send(()).await;
        Self
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = GATE.1.try_recv();
    }
}

fn decode(path: &std::path::Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// The icon of project `pid` fetched from `url`, a tile while it loads.
#[derive(PartialEq)]
pub struct Face {
    pub pid: String,
    pub url: Option<String>,
}

impl Component for Face {
    fn render(&self) -> impl IntoElement {
        let t = use_theme();
        let mut shown = use_state(|| cached(&self.pid));
        let mut current = use_state(|| self.pid.clone());
        use_side_effect_with_deps(&(self.pid.clone(), self.url.clone()), move |(pid, url)| {
            current.set(pid.clone());
            if let Some(handle) = cached(pid) {
                shown.set(Some(handle));
                return;
            }
            shown.set(None);
            let Some(url) = url.clone() else { return };
            let pid = pid.clone();
            spawn(async move {
                let slot = Slot::take().await;
                let wanted = vec![(pid.clone(), url)];
                let decoded = off_ui(move || {
                    let _slot = slot;
                    modicons::prepare(&wanted);
                    modicons::icon(&wanted[0].0).and_then(|path| decode(&path))
                })
                .await
                .flatten();
                let Some((w, h, rgba)) = decoded else { return };
                if let Some(handle) = ImageHandle::from_rgba(w, h, Bytes::from(rgba), AlphaType::Unpremul) {
                    remember(pid.clone(), handle.clone());
                    if current.try_peek().is_some_and(|c| *c == pid) {
                        shown.set(Some(handle));
                    }
                }
            });
        });
        let tile = rect().width(Size::px(control::L)).height(Size::px(control::L)).corner_radius(t.shape.control).overflow(Overflow::Clip).center();
        match shown.read().clone() {
            Some(handle) => tile.child(image(handle).width(Size::px(control::L)).height(Size::px(control::L)).aspect_ratio(AspectRatio::Max).image_cover(ImageCover::Center)),
            None => tile.background(t.raised).child(icon(&t, Icon::Package, iconsize::M, t.text3)),
        }
    }
}