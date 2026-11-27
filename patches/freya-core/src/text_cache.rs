use std::{
    hash::{
        Hash,
        Hasher,
    },
    rc::Rc,
};

use freya_engine::prelude::SkParagraph;
use rustc_hash::{
    FxHashMap,
    FxHasher,
};
use tiny_cache::LRUCache;

use crate::{
    data::TextStyleState,
    node_id::NodeId,
    prelude::Span,
};

pub struct CachedParagraph<'a> {
    pub text_style_state: &'a TextStyleState,
    pub spans: &'a [Span<'a>],
    pub max_lines: Option<usize>,
    pub line_height: Option<f32>,
    pub width: f32,
}

impl Hash for CachedParagraph<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text_style_state.hash(state);
        self.spans.hash(state);
        self.max_lines.hash(state);
        if let Some(v) = self.line_height {
            v.to_bits().hash(state)
        }
        self.width.to_bits().hash(state);
    }
}

const RETAINED_CAPACITY: usize = 2048;

/// Shaped results shared by every label and paragraph, keyed by what shapes them.
/// Each node keeps its latest results, and results of removed nodes stay in a bounded
/// recency list, so a row that is mounted again with the same text is not shaped again.
pub struct ParagraphCache<V, const RETAINED: usize = RETAINED_CAPACITY> {
    live: LRUCache<Rc<V>, NodeId>,
    retained: FxHashMap<u64, (u64, Rc<V>)>,
    tick: u64,
}

impl<V, const RETAINED: usize> Default for ParagraphCache<V, RETAINED> {
    fn default() -> Self {
        Self {
            live: LRUCache::default(),
            retained: FxHashMap::default(),
            tick: 0,
        }
    }
}

impl<V, const RETAINED: usize> ParagraphCache<V, RETAINED> {
    fn hash_of<H: Hash>(key: &H) -> u64 {
        let mut hasher = FxHasher::default();
        key.hash(&mut hasher);
        hasher.finish()
    }

    fn retain(&mut self, hash: u64, value: Rc<V>) {
        self.tick += 1;
        self.retained.insert(hash, (self.tick, value));

        if self.retained.len() > RETAINED {
            let mut ticks: Vec<u64> = self.retained.values().map(|(tick, _)| *tick).collect();
            let cut = ticks.len() / 4;
            let (_, threshold, _) = ticks.select_nth_unstable(cut);
            let threshold = *threshold;
            self.retained.retain(|_, (tick, _)| *tick > threshold);
        }
    }

    /// Returns the saved result for this key, from the node's own results or from the
    /// retained ones. If it returns `None`, create the result and [`insert`](Self::insert) it.
    pub fn utilize<H: Hash>(&mut self, id: NodeId, key: &H) -> Option<Rc<V>> {
        let hash = Self::hash_of(key);

        if let Some(value) = self.live.utilize(id, &hash) {
            let value = (*value).clone();
            self.retain(hash, value.clone());
            return Some(value);
        }

        let value = self.retained.get(&hash)?.1.clone();
        self.live.insert(id, &hash, value.clone());
        self.retain(hash, value.clone());
        Some(value)
    }

    /// Saves a newly created result for this node and keeps it for later nodes.
    pub fn insert<H: Hash>(&mut self, id: NodeId, key: &H, value: V) -> Rc<V> {
        let hash = Self::hash_of(key);
        let value = Rc::new(value);
        self.live.insert(id, &hash, value.clone());
        self.retain(hash, value.clone());
        value
    }

    /// Lets go of what a removed node used; its results stay retained until pushed out.
    pub fn remove(&mut self, id: &NodeId) {
        self.live.remove(id);
    }

    /// Forgets everything, for changes that alter every result such as fonts or scale.
    pub fn reset(&mut self) {
        self.live.reset();
        self.retained.clear();
    }

    pub fn len(&self) -> usize {
        self.retained.len()
    }

    pub fn is_empty(&self) -> bool {
        self.retained.is_empty()
    }

    pub fn users_len(&self) -> usize {
        self.live.users_len()
    }
}

impl<V, const RETAINED: usize> std::fmt::Debug for ParagraphCache<V, RETAINED> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParagraphCache")
            .field("retained", &self.retained.len())
            .field("users", &self.live.users_len())
            .finish()
    }
}

pub type TextCache = ParagraphCache<SkParagraph>;

#[cfg(test)]
mod test {
    use std::num::NonZeroU64;

    use super::ParagraphCache;
    use crate::node_id::NodeId;

    fn node(n: u64) -> NodeId {
        NodeId(NonZeroU64::new(n).unwrap())
    }

    type Cache = ParagraphCache<String, 8>;

    fn shape(cache: &mut Cache, id: NodeId, key: u32, shaped: &mut u32) -> String {
        let value = cache.utilize(id, &key).unwrap_or_else(|| {
            *shaped += 1;
            cache.insert(id, &key, format!("p{key}"))
        });
        (*value).clone()
    }

    #[test]
    fn remounted_node_reuses_result_of_removed_node() {
        let mut cache = Cache::default();
        let mut shaped = 0;
        shape(&mut cache, node(1), 10, &mut shaped);
        cache.remove(&node(1));
        assert_eq!(shape(&mut cache, node(2), 10, &mut shaped), "p10");
        assert_eq!(shaped, 1);
    }

    #[test]
    fn different_keys_do_not_share() {
        let mut cache = Cache::default();
        let mut shaped = 0;
        shape(&mut cache, node(1), 10, &mut shaped);
        assert_eq!(shape(&mut cache, node(2), 11, &mut shaped), "p11");
        assert_eq!(shaped, 2);
    }

    #[test]
    fn retained_results_are_bounded_and_recent_ones_survive() {
        let mut cache = Cache::default();
        let mut shaped = 0;
        for key in 0..100 {
            shape(&mut cache, node(key as u64 + 1), key, &mut shaped);
            cache.remove(&node(key as u64 + 1));
        }
        assert!(cache.len() <= 8);
        let before = shaped;
        shape(&mut cache, node(500), 99, &mut shaped);
        assert_eq!(shaped, before);
        shape(&mut cache, node(501), 0, &mut shaped);
        assert_eq!(shaped, before + 1);
    }

    #[test]
    fn reset_forgets_everything() {
        let mut cache = Cache::default();
        let mut shaped = 0;
        shape(&mut cache, node(1), 10, &mut shaped);
        cache.remove(&node(1));
        cache.reset();
        shape(&mut cache, node(2), 10, &mut shaped);
        assert_eq!(shaped, 2);
        assert_eq!(cache.users_len(), 1);
    }
}
