//! Clipboard history storage: a fixed-capacity, de-duplicated ring buffer.

use std::collections::VecDeque;
use std::sync::Arc;

/// Newest item is at the front (index 0).
///
/// Entries are `Arc<str>` rather than `String` because the UI snapshots the
/// whole list on every frame it renders, to avoid holding the lock across
/// rendering. Clipboard entries are unbounded in size — copying a log file or a
/// large JSON blob is routine — so deep-copying them at frame rate is not
/// affordable. Sharing makes a snapshot a handful of refcount bumps.
pub struct HistoryStore {
    items: VecDeque<Arc<str>>,
    capacity: usize,
}

impl HistoryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Record a newly copied value. Ignores empties; de-duplicates by moving an
    /// existing identical entry to the front instead of adding a second copy.
    /// Returns `true` if the stored contents/order changed.
    pub fn push(&mut self, value: String) -> bool {
        if value.is_empty() {
            return false;
        }
        if let Some(pos) = self.items.iter().position(|v| v.as_ref() == value.as_str()) {
            // Already present — promote to most-recent (no-op if already first).
            if pos == 0 {
                return false;
            }
            self.items.remove(pos);
        }
        self.items.push_front(Arc::from(value));
        while self.items.len() > self.capacity {
            self.items.pop_back();
        }
        true
    }

    #[allow(dead_code)] // test-only helper; the UI reads items through `iter`
    pub fn get(&self, index: usize) -> Option<&Arc<str>> {
        self.items.get(index)
    }

    #[allow(dead_code)] // test-only helper; the UI reads the length off its snapshot
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[allow(dead_code)] // test-only helper; the UI reads the length off its snapshot
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Newest-first handles to the items. Cloning what this yields is cheap.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<str>> {
        self.items.iter()
    }

    /// Newest-first copy of the items, for persistence. Unlike [`iter`], this
    /// does deep-copy — it runs at most once per persist interval.
    ///
    /// [`iter`]: Self::iter
    pub fn snapshot(&self) -> Vec<String> {
        self.items.iter().map(|s| s.to_string()).collect()
    }

    /// Replace the contents with a previously saved (newest-first) list,
    /// truncated to the current capacity.
    pub fn restore(&mut self, items: Vec<String>) {
        self.items = items
            .into_iter()
            .take(self.capacity)
            .map(Arc::from)
            .collect();
    }

    // Called by the tray's "Clear history", which only exists on Linux so far.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_promotes_to_front() {
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.push("a".into());
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().as_ref(), "a");
        assert_eq!(h.get(1).unwrap().as_ref(), "b");
    }

    #[test]
    fn respects_capacity() {
        let mut h = HistoryStore::new(2);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().as_ref(), "c");
        assert_eq!(h.get(1).unwrap().as_ref(), "b");
        assert!(h.get(2).is_none());
    }

    #[test]
    fn snapshotting_shares_rather_than_copies() {
        // The UI clones this iterator's output on every rendered frame, so it
        // must hand out handles to the stored entries, not fresh copies.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        let cloned: Arc<str> = h.iter().next().unwrap().clone();
        assert!(Arc::ptr_eq(&cloned, h.get(0).unwrap()));
    }

    #[test]
    fn ignores_empty() {
        let mut h = HistoryStore::new(5);
        h.push(String::new());
        assert!(h.is_empty());
    }
}
