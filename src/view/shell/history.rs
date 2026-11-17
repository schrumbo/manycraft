use freya::prelude::*;

use crate::view::store::Store;
use crate::view::ui::{Filter, Page};

const LIMIT: usize = 64;

/// The pages the window showed in order, with the one shown now; Back and Forward walk it.
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    stack: Vec<Page>,
    at: usize,
}

impl History {
    pub fn new(first: Page) -> Self {
        Self { stack: vec![first], at: 0 }
    }

    /// Notes that `page` is shown now; a page that is already the current one changes nothing, and anything that was forward is gone.
    pub fn visit(&mut self, page: &Page) {
        if self.stack[self.at] == *page {
            return;
        }
        self.stack.truncate(self.at + 1);
        self.stack.push(page.clone());
        if self.stack.len() > LIMIT {
            self.stack.remove(0);
        }
        self.at = self.stack.len() - 1;
    }

    pub fn can_back(&self) -> bool {
        self.at > 0
    }

    pub fn can_forward(&self) -> bool {
        self.at + 1 < self.stack.len()
    }

    /// Steps back and returns the page to show, skipping the pages `gone` says no longer exist.
    pub fn back(&mut self, gone: impl Fn(&Page) -> bool) -> Option<Page> {
        let mut at = self.at;
        while at > 0 {
            at -= 1;
            if !gone(&self.stack[at]) {
                self.at = at;
                return Some(self.stack[at].clone());
            }
        }
        None
    }

    /// Steps forward and returns the page to show, skipping the pages `gone` says no longer exist.
    pub fn forward(&mut self, gone: impl Fn(&Page) -> bool) -> Option<Page> {
        let mut at = self.at;
        while at + 1 < self.stack.len() {
            at += 1;
            if !gone(&self.stack[at]) {
                self.at = at;
                return Some(self.stack[at].clone());
            }
        }
        None
    }
}

/// Whether the page no longer exists: an instance or batch that is gone.
pub fn vanished(store: Store, page: &Page) -> bool {
    let m = store.model.peek();
    if !m.loaded {
        return false;
    }
    match page {
        Page::Grid(Filter::Batch(b)) => m.batch(b).is_none(),
        Page::Instance(id) | Page::Browse(id, _) => m.instance(id).is_none(),
        _ => false,
    }
}

/// Shows the page before this one.
pub fn back(store: Store) {
    let ui = store.ui;
    let mut hist = ui.hist;
    let next = hist.write().back(|p| vanished(store, p));
    if let Some(to) = next {
        let mut page = ui.page;
        page.set(to);
    }
}

/// Shows the page after this one.
pub fn forward(store: Store) {
    let ui = store.ui;
    let mut hist = ui.hist;
    let next = hist.write().forward(|p| vanished(store, p));
    if let Some(to) = next {
        let mut page = ui.page;
        page.set(to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walked() -> History {
        let mut h = History::new(Page::Start);
        h.visit(&Page::Library);
        h.visit(&Page::Instance("a".into()));
        h
    }

    #[test]
    fn back_and_forward_walk_the_pages_in_order() {
        let mut h = walked();
        assert_eq!(h.back(|_| false), Some(Page::Library));
        assert_eq!(h.back(|_| false), Some(Page::Start));
        assert_eq!(h.back(|_| false), None);
        assert_eq!(h.forward(|_| false), Some(Page::Library));
        assert!(h.can_forward() && h.can_back());
    }

    #[test]
    fn a_new_page_after_going_back_drops_what_was_forward() {
        let mut h = walked();
        h.back(|_| false);
        h.visit(&Page::Grid(Filter::All));
        assert!(!h.can_forward());
        assert_eq!(h.back(|_| false), Some(Page::Library));
    }

    #[test]
    fn showing_the_current_page_again_adds_nothing() {
        let mut h = History::new(Page::Start);
        h.visit(&Page::Start);
        assert!(!h.can_back());
    }

    #[test]
    fn vanished_pages_are_skipped_and_none_left_stays_put() {
        let mut h = walked();
        assert_eq!(h.back(|p| *p == Page::Library), Some(Page::Start));
        let mut h = walked();
        assert_eq!(h.back(|_| true), None);
        assert_eq!(h.forward(|_| true), None);
        assert!(h.can_back());
    }

    #[test]
    fn the_history_keeps_only_the_last_pages() {
        let mut h = History::new(Page::Start);
        for i in 0..LIMIT + 10 {
            h.visit(&Page::Instance(i.to_string()));
        }
        assert_eq!(h.stack.len(), LIMIT);
        assert_eq!(h.stack[h.at], Page::Instance((LIMIT + 9).to_string()));
    }
}
